"""Exercise actual Taskloom DAG/PEER roles through a configured local model.

No scripted model answers. Retain failed planning, role outputs, citation and
review checks, rather than calling an incomplete workflow successful.
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
from native_subscription import metadata_for


def node(name, task, dependencies=(), params=None, select=None):
    return {"id": name, "task": task, "depends_on": list(dependencies), "params": params,
            "condition": {"type": "always"}, "allow_skipped_dependencies": False, "select": select or {}}


def run_case(case, api, root, verifier):
    started = time.perf_counter()
    writer_role = case.get("writer_role", "writer")
    request = {"id": case["id"], "input": {"goal": case["goal"]}, "definition": None, "peer": None,
        "limits": {"max_nodes": 32, "max_parallel": case.get("max_parallel", 2), "max_dispatches": 64, "max_duration": {"secs": case.get("duration_seconds", 300), "nanos": 0},
                   "shutdown_grace": {"secs": 5, "nanos": 0}, "max_output_bytes": 131072, "max_cost_micros": 1}}
    if "input_context" in case:
        request["input"]["review_context"] = case["input_context"]
    if case["kind"] == "peer":
        request["peer"] = {"planner": "planner", "executor": "analyst", "writer": writer_role, "reviewer": "reviewer",
                           "max_tasks": case.get("max_tasks", 2), "max_revisions": 1}
        final_node = "result"
    else:
        branches = [node(b["id"], "analyst", params={"instructions": b["instructions"]}) for b in case["branches"]]
        dependencies = [b["id"] for b in case["branches"]]
        if case.get("initial_draft"):
            branches.append(node("review-initial", "reviewer", params={"candidate": case["initial_draft"]}))
            dependencies.append("review-initial")
        request["definition"] = {"nodes": branches + [node("draft", writer_role, dependencies),
            node("review", "reviewer", ["draft"], select={"draft": {"node": "draft", "pointer": ""}})]}
        final_node = "draft"
    result = {"id": case["id"], "request": request, "failures": [], "children": [], "model_steps": 0, "tool_attempts": 0,
              "requested_tool_calls": 0, "tool_errors": 0,
              "native_usage": {"requests": 0, "input_tokens": 0, "output_tokens": 0, "cost_micros": 0},
              "output_rejections": 0, "output_repairs_admitted": 0, "output_validations": 0}
    directory = root / case["id"]
    directory.mkdir()
    try:
        api("/workflows", request)
        for _ in range(5 * (case.get("duration_seconds", 300) + 20)):
            observed = api("/workflows/" + case["id"])
            if not observed["job"]["active"]:
                break
            time.sleep(0.2)
        else:
            raise RuntimeError("workflow controller exceeded harness deadline")
        result["observation"] = observed
        state = observed["state"]
        if state["status"] != "completed":
            result["failures"].append("workflow did not complete: " + state["status"])
        evidence = []
        for item in state["nodes"]:
            if item["status"] == "failed" and item.get("detail"):
                result["failures"].append(item["id"] + ": " + item["detail"])
            if item.get("child"):
                child = api("/runs/" + item["child"]["run_id"])["state"]
                result["children"].append(child)
                result["model_steps"] += child["report"]["steps"]
                result["requested_tool_calls"] += child["report"]["tool_calls"]
                result["tool_attempts"] += sum(t["attempts"] for t in child["tool_steps"])
                result["tool_errors"] += sum(t["progress"].get("result", {}).get("is_error") is True for t in child["tool_steps"])
                for key in result["native_usage"]:
                    result["native_usage"][key] += child["report"]["usage"][key]
                for event in child["report"]["events"]:
                    kind = event["kind"]
                    if kind["type"] == "output_rejected":
                        result["output_rejections"] += 1
                        result["output_repairs_admitted"] += int(kind["repair"])
                    elif kind["type"] == "output_validated":
                        result["output_validations"] += 1
                search_ids = {t["call"]["id"] for t in child["tool_steps"] if t["call"]["name"] == "knowledge_search"}
                for message in child["report"]["messages"]:
                    if message["role"] == "tool" and message["result"]["call_id"] in search_ids and not message["result"]["is_error"]:
                        evidence.append(message["result"]["output"])
        # Only work-role nodes count toward the parallelism observation;
        # pure plan/review validators are excluded.
        work_ids = {n["id"] for n in state["definition"]["nodes"] if n["task"] == "analyst"}
        active = set()
        maximum = 0
        for event in state["events"]:
            kind = event["kind"]
            if kind.get("node") in work_ids:
                if kind["type"] == "node_started":
                    active.add(kind["node"])
                else:
                    active.discard(kind["node"])
                maximum = max(maximum, len(active))
        result["maximum_concurrent_analysts"] = maximum
        result["completed_analyst_roles"] = sum(item["status"] == "complete" and item["id"] in work_ids for item in state["nodes"])
        if result["completed_analyst_roles"] < case.get("min_completed_analysts", 0):
            result["failures"].append("minimum completed analyst role count was not reached")
        if result["native_usage"]["requests"] < case.get("min_native_requests", 0):
            result["failures"].append("minimum completed native request count was not reached")
        result["review_rework_exercised"] = any(n["id"] == "draft-1" and n["status"] == "complete" for n in state["nodes"])
        if case.get("initial_draft"):
            initial_review = next((n.get("output") for n in state["nodes"] if n["id"] == "review-initial"), None)
            result["initial_review"] = initial_review
            result["review_rework_exercised"] = (isinstance(initial_review, dict) and initial_review.get("approved") is False
                and bool(initial_review.get("feedback", "").strip())
                and any(n["id"] == "draft" and n["status"] == "complete" for n in state["nodes"]))
        if case["require_parallel"] and maximum < 2:
            result["failures"].append("two actual analyst executions did not overlap")
        if case["require_rework"] and not result["review_rework_exercised"]:
            result["failures"].append("required reviewer rejection/rewrite not exercised")
        if case["kind"] != "peer":
            review = next((n.get("output") for n in state["nodes"] if n["id"] == "review"), None)
            if not isinstance(review, dict) or review.get("approved") is not True:
                result["failures"].append("reviewer did not approve the report")
        final = next((n.get("output") for n in state["nodes"] if n["id"] == final_node), None)
        # PEER result contains the approved draft under answer; inspect and
        # retain it without replacing malformed/missing output with a fixture.
        if case["kind"] == "peer" and isinstance(final, dict):
            final = final.get("answer")
        result["final_output"] = final
        if not isinstance(final, (str, dict)):
            result["failures"].append("missing final report text")
        else:
            try:
                # Validated role output is a typed JSON object. Legacy Text
                # output still requires exact JSON; never strip fences/IDs.
                report = json.loads(final) if isinstance(final, str) else final
                for phrase in case["required_phrases"]:
                    if phrase.lower() not in report["text"].lower():
                        result["failures"].append("missing requested topic: " + phrase)
                checked = subprocess.run([str(verifier)], input=json.dumps({"report": report, "evidence": evidence}).encode(),
                                         capture_output=True, timeout=10)
                if checked.returncode:
                    raise ValueError(checked.stderr.decode(errors="replace")[:1000])
                documents = {s["document_id"] for s in json.loads(checked.stdout)["sources"]}
                if not set(case["required_documents"]) <= documents:
                    result["failures"].append("required source coverage missing")
            except Exception as error:
                result["failures"].append("exact citation/output check: " + str(error))
        # A model reviewer judgment is evidence, not an independent truth label.
        result["claim_support_assessment"] = "model reviewer output retained; independent factual/entailment scoring still required"
        result["charged_model_cost_micros"] = observed["budget"]["committed_micros"]
        result["uncertain_model_cost_micros"] = observed["budget"]["uncertain_micros"]
        if state["status"] == "completed":
            api("/workflows/" + case["id"] + "/resume", {})
            for _ in range(50):
                replay = api("/workflows/" + case["id"])
                if not replay["job"]["active"]:
                    break
                time.sleep(0.1)
            result["replay_unchanged"] = replay["state"] == state
            if not result["replay_unchanged"]:
                result["failures"].append("completed workflow reentry changed state")
    except Exception as error:
        result["failures"].append(str(error))
    result["wall_elapsed_ms"] = round((time.perf_counter() - started) * 1000)
    result["status"] = "failed" if result["failures"] else "passed"
    save(directory / "result.json", result)
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("out", type=Path)
    parser.add_argument("--config", type=Path, required=True)
    parser.add_argument("--taskloom", type=Path, required=True)
    parser.add_argument("--verifier", type=Path, required=True)
    parser.add_argument("--suite", type=Path, default=Path("examples/benchmarks/collaboration-v1.json"))
    args = parser.parse_args()
    config = json.loads(args.config.read_text(encoding="utf-8"))
    if not config.get("prices") or urlsplit(config["provider"]["base_url"]).hostname not in {"localhost", "127.0.0.1", "::1"} or any(p["input_per_million"] or p["output_per_million"] for p in config["prices"]):
        raise ValueError("this example only supports explicitly zero-fee local native APIs")
    suite = json.loads(args.suite.read_text(encoding="utf-8"))
    if suite["version"] not in {1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17} or not suite["cases"]:
        raise ValueError("unsupported suite")
    for case in suite["cases"]:
        for field, low, high, default in [("duration_seconds", 1, 1800, 300), ("max_tasks", 1, 8, 2), ("min_completed_analysts", 0, 8, 0), ("min_native_requests", 0, 256, 0)]:
            value = case.get(field, default)
            if type(value) is not int or not low <= value <= high:
                raise ValueError("invalid bounded case setting: " + field)
    # An optional real-repository suite pins complete sources and exact
    # contiguous excerpts. Refuse drift before starting a host/model request.
    for pin in suite.get("source_pins", []):
        source_root = Path(__file__).resolve().parents[2]
        relative = Path(pin["path"])
        source = (source_root / relative).resolve()
        if relative.is_absolute() or not source.is_relative_to(source_root):
            raise ValueError("source pin must stay within the repository")
        text = source.read_text(encoding="utf-8")
        excerpt = text[pin["character_start"]:pin["character_end"]]
        documents = [doc for doc in suite["documents"] if doc["id"] == pin["document_id"]]
        if (hashlib.sha256(text.encode()).hexdigest() != pin["sha256_utf8_lf"]
                or hashlib.sha256(excerpt.encode()).hexdigest() != pin["excerpt_sha256_utf8_lf"]
                or len(documents) != 1 or documents[0]["text"] != excerpt):
            raise ValueError("repository source or exact excerpt changed: " + pin["path"])
    ids = [case["id"] for case in suite["cases"]]
    if len(set(ids)) != len(ids) or any(not 1 <= len(value) <= 64 or any(c not in "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789_-" for c in value) for value in ids):
        raise ValueError("invalid or duplicate isolated case identity")
    root = args.out.resolve()
    root.mkdir(parents=True, exist_ok=False)
    shutil.copy2(__file__, root / "harness.py")
    shutil.copy2(Path(__file__).with_name("native_retrieval.py"), root / "native_retrieval.py")
    save(root / "execution-provenance.json", {"framework_binary_sha256": hashlib.sha256(args.taskloom.read_bytes()).hexdigest(),
        "verifier_binary_sha256": hashlib.sha256(args.verifier.read_bytes()).hexdigest(),
        "harness_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest()})
    # A model-only config is reused, never its production tool grants/services.
    config.update(state_dir=str(root / "state"), roles=suite["roles"], knowledge=True,
                  mcp=[], skills=[], memory=None, final_output=None, research={}, sandbox=None, sandbox_top_level=False,
                  retrieval={"mode": "lexical"}, database=None, observability=None)
    # Every native role has a separate durable run. Reserve zero API fees per
    # role under the campaign's explicit one-micro-unit admission cap.
    subprocess.run([str(args.taskloom.resolve()), "init", str(root / "defaults.json")], capture_output=True, check=True)
    defaults = json.loads((root / "defaults.json").read_text(encoding="utf-8"))
    config["limits"] = dict(defaults["limits"], max_cost_micros=0)
    config_path = root / "taskloom.json"
    save(config_path, config)
    save(root / "suite.json", suite)
    report = {"version": 1, "evidence_kind": "actual local native model API, multiple durable agent roles",
        "configuration": config, "suite_sha256": hashlib.sha256(args.suite.read_bytes()).hexdigest(),
        "cases": [], "uncovered": suite["uncovered"], "currency": "USD", "max_total_cost_micros": 1,
        "limitations": ["visible synthetic operations tasks, no competitor campaign", "API fee zero; hardware unmetered",
            "native provider concurrency is subject to the local inference server scheduler", "reviewer verdict is not independent factual scoring"]}
    report.update(completed=False, active_case=None, passed=0, failed=0, requested=len(suite["cases"]), not_run=len(suite["cases"]))
    subscription = metadata_for(config)
    if subscription:
        report.update(evidence_kind=subscription['evidence_kind'], subscription=subscription,
                      currency=None, max_total_cost_micros=None)
        report['limitations'][1] = subscription['fees']
        report['limitations'][2] = 'Remote provider scheduling/weights are not controlled; role overlap is not proof of simultaneous inference.'
    save(root / "report.json", report)
    with host(args.taskloom.resolve(), config_path, root) as api:
        for i, document in enumerate(suite["documents"]):
            api("/knowledge", {"document": document, "expected_revision": i})
        for case in suite["cases"]:
            report["active_case"] = case["id"]
            save(root / "report.json", report)
            result = run_case(case, api, root, args.verifier.resolve())
            report["cases"].append(result)
            report["passed"] = sum(c["status"] == "passed" for c in report["cases"])
            report["failed"] = sum(c["status"] == "failed" for c in report["cases"])
            report["requested"] = len(suite["cases"])
            report["not_run"] = report["requested"] - len(report["cases"])
            report["active_case"] = None
            save(root / "report.json", report)
            print(f"{case['id']}: {result['status']}: {result['failures']}", flush=True)
    # Failed HTTP observation can leave a child working until host shutdown.
    # Preserve its cold files instead of calling missing usage zero. A budget
    # journal may have a newer generation than its saved run checkpoint.
    report['cold_saved_state'] = {str(path.relative_to(root)): json.loads(path.read_text(encoding='utf-8'))
        for path in sorted((root / 'state' / 'runs').glob('*.json'))}
    report['usage_qualification'] = ('Case snapshots can precede late child completion. Cold run/budget '
        'files after owned host shutdown and subscription HTTP reservations/observations are separate; '
        'newer-generation budgets take precedence. Missing responses are not zero usage.')
    report["completed"] = True
    save(root / "report.json", report)
    raise SystemExit(1 if report["failed"] or report["not_run"] else 0)


if __name__ == "__main__":
    main()
