"""Kill an owned Taskloom process during actual zero-fee native generation.

This checks process/budget recovery, not answer quality or uncertain tool
effects. Run without other traffic on the supplied local model log.
"""
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import time
from urllib.parse import urlsplit

from native_retrieval import host, save


def request_count(log):
    return log.read_bytes().count(b"[Request received] Model:")


def wait_idle(api, identity):
    for _ in range(1500):
        observed = api("/runs/" + identity)
        if not observed["job"]["active"]:
            return observed
        time.sleep(0.1)
    raise RuntimeError("recovery worker exceeded harness deadline")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("out", type=Path)
    parser.add_argument("--config", type=Path, required=True)
    parser.add_argument("--taskloom", type=Path, required=True)
    parser.add_argument("--model-log", type=Path, required=True,
                        help="official Transformers server log; exclude concurrent traffic")
    args = parser.parse_args()
    config = json.loads(args.config.read_text(encoding="utf-8"))
    if (urlsplit(config["provider"]["base_url"]).hostname not in {"localhost", "127.0.0.1", "::1"}
            or not config.get("prices")
            or any(p["input_per_million"] or p["output_per_million"] for p in config["prices"])):
        raise ValueError("this example only supports explicitly zero-fee local native APIs")
    root = args.out.resolve()
    root.mkdir(parents=True, exist_ok=False)
    executable = args.taskloom.resolve()
    subprocess.run([str(executable), "init", str(root / "defaults.json")], check=True, capture_output=True)
    defaults = json.loads((root / "defaults.json").read_text(encoding="utf-8"))
    # A long enough first generation to observe admission and kill the caller.
    # No tools are offered; uncertain-effect recovery remains a separate check.
    config.update(state_dir=str(root / "state"), knowledge=False, roles=[], mcp=[], research={},
                  retrieval={"mode": "lexical"}, sandbox=None, sandbox_top_level=False,
                  final_output=None, output_tokens=1024,
                  system="Write a detailed numbered explanation of 100 distinct considerations for reliable task execution. Continue until the list is complete. Do not call tools.",
                  limits=dict(defaults["limits"], max_cost_micros=0, max_steps=2, max_tool_calls=0))
    config_path = root / "taskloom.json"
    save(config_path, config)
    shutil.copy2(__file__, root / "harness.py")
    shutil.copy2(Path(__file__).with_name("native_retrieval.py"), root / "native_retrieval.py")
    result = {"version": 1, "evidence_kind": "actual native API plus OS-process kill and restart",
              "failures": [], "completed": False, "configuration": config,
              "framework_binary_sha256": hashlib.sha256(executable.read_bytes()).hexdigest(),
              "harness_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
              "currency": "USD", "max_total_cost_micros": 0, "input_per_million": 0, "output_per_million": 0,
              "limitations": ["requires exclusive traffic on supplied local Transformers log",
                              "generation recovery can issue a new request; it is not exactly once",
                              "no tools/effects/approval or multi-agent process death exercised",
                              "answer quality is not scored; local hardware/electricity unmetered"]}
    identity = "native-process-death"
    started = time.perf_counter()
    try:
        baseline = request_count(args.model_log)
        with host(executable, config_path, root) as api:
            api("/runs", {"id": identity, "prompt": "Explain the considerations in order with a paragraph for each."})
            for _ in range(1000):
                state = api("/runs/" + identity)["state"]
                if state["cursor"]["type"] == "model_in_flight" and request_count(args.model_log) > baseline:
                    break
                if state["status"] == "finished":
                    raise RuntimeError("model completed before process interruption; not exercised")
                time.sleep(0.01)
            else:
                raise RuntimeError("native dispatch/in-flight checkpoint observation timed out")
            save(root / "before-kill.json", state)
            result["killed_process_id"] = api.host_process.pid
            api.host_process.kill()
            api.host_process.wait(timeout=10)
            result["process_exit_code"] = api.host_process.returncode
        # Snapshot the actual crash-time spending journal before loading can
        # reconcile it. The checkpoint itself precedes HTTP reservation.
        journals = list((root / "state" / "runs").glob("*.budget.json"))
        if len(journals) != 1:
            raise RuntimeError("expected one isolated crash-time budget journal")
        crash_budget = json.loads(journals[0].read_text())
        save(root / "crash-budget.json", crash_budget)
        result["crash_budget"] = crash_budget
        with host(executable, config_path, root) as api:
            result["after_restart_before_resume"] = api("/runs/" + identity)
            api("/runs/" + identity + "/resume", {})
            observed = wait_idle(api, identity)
            save(root / "after-recovery.json", observed)
            final = observed["state"]
            events = [e["kind"]["type"] for e in final["report"]["events"]]
            if events.count("recovered_model") != 1:
                result["failures"].append("expected one explicit interrupted-model recovery event")
            if final["report"]["budget"]["uncertain_attempts"] < 1:
                result["failures"].append("crash-time attempt uncertainty was lost")
            if final["report"]["tool_calls"] or final["tool_steps"] or final["config"]["tools"]:
                result["failures"].append("unexpected tool capability or execution")
            if final["status"] != "finished":
                result["failures"].append("recovered run did not terminate within its frozen limits")
            attempts = request_count(args.model_log)
            api("/runs/" + identity + "/resume", {})
            replay = wait_idle(api, identity)
            result["terminal_reentry_unchanged"] = replay["state"] == final and request_count(args.model_log) == attempts
            if not result["terminal_reentry_unchanged"]:
                result["failures"].append("terminal reentry changed state or sent another request")
            result["final_stop_reason"] = final["report"]["stop_reason"]
            result["final_budget"] = final["report"]["budget"]
        result["observed_native_requests"] = request_count(args.model_log) - baseline
        result["completed"] = True
    except Exception as error:
        result["failures"].append(str(error))
    result["status"] = "failed" if result["failures"] else "passed"
    result["wall_ms"] = round((time.perf_counter() - started) * 1000)
    save(root / "report.json", result)
    print(json.dumps({k: result[k] for k in ("status", "failures", "completed", "wall_ms")}), flush=True)
    raise SystemExit(1 if result["failures"] else 0)


if __name__ == "__main__":
    main()
