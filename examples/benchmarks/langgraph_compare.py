"""Actual LangGraph StateGraph comparison against a frozen Taskloom campaign.

Same raw native model API, prompts, advertised tools, knowledge engine, output
checks and resource ceilings. Model/graph control is real LangGraph. Local tool
implementations and isolated approval/reconciliation are harness code. This
adapter exercises controller interruption, not OS-process crash recovery.
"""
import argparse
import hashlib
import importlib.metadata
import json
import os
from pathlib import Path
import shutil
import subprocess
import time
import urllib.error
import urllib.request
from typing import TypedDict

os.environ["LANGSMITH_TRACING"] = "false"
os.environ["LANGCHAIN_TRACING"] = "false"
os.environ["LANGCHAIN_TRACING_V2"] = "false"

from langgraph.graph import StateGraph, START, END
from langgraph.checkpoint.memory import InMemorySaver
from langgraph.types import Command, interrupt
from native_retrieval import host, save


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *unused):
        return None


def compact(value):
    # Match the sorted object serialization of Taskloom's serde_json::Value.
    return json.dumps(value, ensure_ascii=False, separators=(",", ":"), sort_keys=True)


def framework_source_identity():
    """Hash the installed public distribution inputs, excluding generated caches."""
    distribution = importlib.metadata.distribution('langgraph')
    records = []
    for relative in sorted(distribution.files or [], key=str):
        if str(relative).endswith('.py') or str(relative).endswith('.dist-info/METADATA'):
            path = Path(distribution.locate_file(relative))
            records.append({'path': str(relative).replace('\\', '/'), 'sha256': hashlib.sha256(path.read_bytes()).hexdigest()})
    if not records:
        raise ValueError('installed LangGraph public source files unavailable')
    return {'distribution': 'langgraph', 'version': distribution.version,
            'sha256': hashlib.sha256(compact(records).encode()).hexdigest(), 'files': len(records)}


class State(TypedDict):
    messages: list
    steps: int
    calls: int


class ObservedEffect(Exception):
    pass


def pointer(value, path):
    for part in path.split("/")[1:]:
        key = part.replace("~1", "/").replace("~0", "~")
        value = value[int(key)] if isinstance(value, list) else value[key]
    return value


def check(case, result, verifier):
    failures = []
    text = result.get("final_text") or ""
    for rule in case["checks"]:
        try:
            kind = rule["type"]
            if kind == "final_contains":
                assert rule["text"] in text, "missing required final phrase"
            elif kind == "json_equals":
                assert compact(pointer(json.loads(text), rule["pointer"])) == compact(rule["value"]), "exact JSON assertion failed"
            elif kind == "tool_calls":
                n = sum(t["name"] == rule["name"] for t in result["tool_attempts"])
                assert rule["min"] <= n <= rule["max"], f"{rule['name']} attempts {n} outside required bounds"
            elif kind == "tool_arguments":
                assert any(t["name"] == rule["name"] and t["arguments"] == rule["arguments"] for t in result["tool_attempts"]), "missing executed exact arguments"
            elif kind == "citations":
                payload = {"report": json.loads(text), "evidence": result["evidence"]}
                verified = subprocess.run([str(verifier)], input=compact(payload).encode(), capture_output=True, timeout=10)
                assert verified.returncode == 0, "shared Rust citation validation failed: " + verified.stderr.decode(errors="replace")[:1000]
                sources = json.loads(verified.stdout)["sources"]
                assert len(sources) >= rule["min_sources"] and set(rule["required_documents"]) <= {s["document_id"] for s in sources}, "missing required sources"
        except Exception as error:
            failures.append(f"{rule['type']}: {error}")
    return failures


def run_case(case, reference, root, args):
    started = time.perf_counter()
    output = root / case["id"]
    output.mkdir()
    frozen = json.loads((args.reference / reference["trajectory_path"]).read_text(encoding="utf-8"))
    if any(context.get('summary') is not None for context in frozen['report']['contexts']):
        raise ValueError('this adapter does not implement the frozen context compressor; refuse before model dispatch')
    source = args.reference / "cases" / case["id"] / "knowledge"
    if source.exists():
        shutil.copytree(source, output / "state" / "knowledge")
    config = {"state_dir": str(output / "state")}
    config_file = output / "knowledge.json"
    save(config_file, config)
    result = {"id": case["id"], "category": case["category"], "failures": [], "tool_attempts": [],
        "evidence": [], "approval_exercised": False, "controller_interruption_exercised": False,
        "effects": 0, "models_served": [], "usage": {"requests": 0, "input_tokens": 0, "output_tokens": 0},
        "charged_model_cost_micros": 0, "cost_unknown": False}
    tools = frozen["config"]["tools"]
    advertised = [{"type": "function", "function": {"name": t["name"], "description": t["description"],
                    "parameters": t["input_schema"]}} for t in tools]
    names = {t["name"] for t in tools}
    # Save the contract actually used to build this graph and its tool preflight,
    # independently of a later caller-supplied comparability manifest.
    result['applied_contract'] = {key: frozen['config'][key] for key in
        ['model', 'temperature', 'max_output_tokens', 'limits', 'policy', 'approvals', 'context', 'tools']}
    result['applied_contract']['context_support'] = 'uncompressed recorded contexts only'
    result['applied_contract']['transport'] = {'attempts_per_decision': 2, 'timeout_seconds': 120,
        'ambient_proxy': False, 'redirect': False, 'paid_api_supported': False}
    if args.configuration['model'] != frozen['config']['model'] or frozen['config']['temperature'] != 0:
        raise ValueError('frozen model or temperature differs from implemented adapter contract')
    messages = [{"role": "system", "content": frozen["report"]["messages"][0]["content"]},
                {"role": "user", "content": case["prompt"]}]
    snapshots = []
    write_result = None
    write_note = None

    with host(args.taskloom, config_file, output) as api:
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect())
        def agent(state):
            duration = frozen['config']['limits']['max_duration']
            if state["steps"] >= frozen["config"]["limits"]["max_steps"] or time.perf_counter() - started > duration['secs'] + duration['nanos']/1_000_000_000:
                raise RuntimeError("model-step or duration ceiling reached")
            body = {"model": args.configuration["model"], "messages": state["messages"], "temperature": 0.0,
                    "max_tokens": args.configuration["output_tokens"], "tools": advertised}
            # Inputs and raw native outputs are retained for parity inspection.
            record = {"request": body}
            snapshots.append(record)
            save(output / "model-api.json", snapshots)
            request = urllib.request.Request(args.configuration["provider"]["base_url"] + "/chat/completions",
                data=compact(body).encode(), headers={"Content-Type": "application/json",
                "Authorization": "Bearer " + os.environ[args.configuration["provider"]["key_env"]]})
            record["http_attempts"] = []
            for attempt in range(2):
                result["usage"]["requests"] += 1
                try:
                    with opener.open(request, timeout=120) as response:
                        data = response.read(frozen["config"]["limits"]["max_model_result_bytes"] + 1)
                        if len(data) > frozen["config"]["limits"]["max_model_result_bytes"]:
                            raise ValueError("native model response exceeds frozen byte bound")
                        raw = json.loads(data)
                    record["http_attempts"].append({"attempt": attempt + 1, "status": 200})
                    break
                except urllib.error.HTTPError as error:
                    record["http_attempts"].append({"attempt": attempt + 1, "status": error.code,
                                                   "body": error.read(8192).decode(errors="replace")})
                    save(output / "model-api.json", snapshots)
                    # Match Taskloom GatewayTransport's two transient HTTP
                    # attempts. No tool is retried. Zero API fees are explicit;
                    # unknown usage on failed attempts remains visible below.
                    if attempt == 1 or error.code not in {408, 429, 500, 502, 503, 504}:
                        raise
                    time.sleep(30 if error.code == 429 else 1)
                except urllib.error.URLError as error:
                    record["http_attempts"].append({"attempt": attempt + 1, "error": str(error)})
                    save(output / "model-api.json", snapshots)
                    if attempt == 1:
                        raise
                    time.sleep(1)
                except ValueError as error:
                    record["http_attempts"].append({"attempt": attempt + 1, "error": "invalid native JSON response: " + str(error)})
                    save(output / "model-api.json", snapshots)
                    raise
                except (TimeoutError, ConnectionError, OSError):
                    record["http_attempts"].append({"attempt": attempt + 1, "error": "transport outcome unknown; not replayed"})
                    save(output / "model-api.json", snapshots)
                    raise
            record["response"] = raw
            save(output / "model-api.json", snapshots)
            usage = raw.get("usage") or {}
            if any(type(usage.get(key)) is not int or not 0 <= usage[key] <= 2**64-1 for key in ['prompt_tokens', 'completion_tokens']):
                record['usage_unknown'] = True
                save(output / 'model-api.json', snapshots)
                raise ValueError('native token usage absent or invalid; retained as unknown')
            result["usage"]["input_tokens"] += usage.get("prompt_tokens", 0)
            result["usage"]["output_tokens"] += usage.get("completion_tokens", 0)
            result["models_served"].append(raw.get("model"))
            choice = raw["choices"][0]
            message = choice["message"]
            # The Taskloom native adapter also rejects truncated completions.
            if choice["finish_reason"] not in {"stop", "tool_calls"}:
                raise RuntimeError("native completion truncated or refused")
            calls = json.loads(json.dumps(message.get("tool_calls") or []))
            for call in calls:
                # Taskloom parses arguments into Value and emits compact,
                # sorted JSON in later model history. Match that conditioning
                # while retaining the untouched raw response above.
                call["function"]["arguments"] = compact(json.loads(call["function"]["arguments"]))
            if bool(calls) != (choice["finish_reason"] == "tool_calls"):
                raise RuntimeError("native finish reason/tool calls mismatch")
            clean = {"role": "assistant", "content": message.get("content") or ""}
            if calls:
                clean["tool_calls"] = calls
            return {"messages": state["messages"] + [clean], "steps": state["steps"] + 1}

        def tools_node(state):
            nonlocal write_result, write_note
            updates = []
            calls = state["messages"][-1].get("tool_calls") or []
            if state["calls"] + len(calls) > frozen["config"]["limits"]["max_tool_calls"]:
                raise RuntimeError("tool-attempt ceiling reached")
            # Match the runtime's whole-batch preflight: an invalid or denied
            # call must not let an earlier call in that batch execute first.
            for call in calls:
                name = call["function"]["name"]
                arguments = json.loads(call["function"]["arguments"])
                if name not in names:
                    raise RuntimeError("tool is not permitted: " + name)
                if name == "knowledge_search":
                    if set(arguments) - {"query", "top_k"} or not isinstance(arguments.get("query"), str) or not 1 <= len(arguments["query"]) <= 4096 or type(arguments.get("top_k", 5)) is not int or not 1 <= arguments.get("top_k", 5) <= 20:
                        raise RuntimeError("invalid knowledge search arguments")
                elif name == "record_note":
                    if set(arguments) != {"note"} or not isinstance(arguments.get("note"), str) or not 1 <= len(arguments["note"]) <= 1000:
                        raise RuntimeError("invalid bounded note")
                elif name == "calculate":
                    if set(arguments) != {"a", "b", "operation"} or any(type(arguments[k]) is not int or not -1000000 <= arguments[k] <= 1000000 for k in ["a", "b"]) or arguments["operation"] not in {"add", "subtract", "multiply"}:
                        raise RuntimeError("invalid bounded arithmetic")
            for call in calls:
                name = call["function"]["name"]
                arguments = json.loads(call["function"]["arguments"])
                if name not in names:
                    raise RuntimeError("tool is not permitted: " + name)
                if name == "record_note":
                    approved = interrupt({"call": call, "binding": hashlib.sha256(compact(call).encode()).hexdigest()})
                    if approved != hashlib.sha256(compact(call).encode()).hexdigest():
                        raise RuntimeError("approval no longer matches exact input")
                    note = arguments.get("note")
                    if set(arguments) != {"note"} or not isinstance(note, str) or not 1 <= len(note) <= 1000:
                        raise RuntimeError("invalid bounded note")
                    result["tool_attempts"].append({"name": name, "arguments": arguments})
                    # One create-only fixture file; model text cannot name paths.
                    with (output / "effect.txt").open("xb") as stream:
                        stream.write(note.encode())
                        stream.flush()
                        os.fsync(stream.fileno())
                    result["effects"] += 1
                    value = {"saved": True}
                    write_note = note
                    write_result = {"role": "tool", "tool_call_id": call["id"],
                                    "content": compact({"output": value, "is_error": False})}
                    if case["category"] == "recovery":
                        raise ObservedEffect("controller interrupted after flush, before graph checkpoint")
                elif name == "calculate":
                    a, b = arguments["a"], arguments["b"]
                    if set(arguments) != {"a", "b", "operation"} or type(a) is not int or type(b) is not int or not -1000000 <= a <= 1000000 or not -1000000 <= b <= 1000000:
                        raise RuntimeError("invalid bounded arithmetic")
                    operation = arguments["operation"]
                    if operation not in {"add", "subtract", "multiply"}:
                        raise RuntimeError("invalid operation")
                    value = {"value": {"add": a+b, "subtract": a-b, "multiply": a*b}[operation]}
                    result["tool_attempts"].append({"name": name, "arguments": arguments})
                elif name == "knowledge_search":
                    value = api("/knowledge/search", {"text": arguments["query"], "top_k": arguments.get("top_k", 5), "max_bytes": 24576})
                    result["evidence"].append(value)
                    result["tool_attempts"].append({"name": name, "arguments": arguments})
                else:
                    raise RuntimeError("unsupported benchmark tool")
                updates.append({"role": "tool", "tool_call_id": call["id"],
                                "content": compact({"output": value, "is_error": False})})
            return {"messages": state["messages"] + updates, "calls": state["calls"] + len(calls)}

        builder = StateGraph(State)
        builder.add_node("agent", agent)
        builder.add_node("tools", tools_node)
        builder.add_edge(START, "agent")
        builder.add_conditional_edges("agent", lambda state: "tools" if state["messages"][-1].get("tool_calls") else END)
        builder.add_edge("tools", "agent")
        graph = builder.compile(checkpointer=InMemorySaver())
        run_config = {"configurable": {"thread_id": case["id"]}, "recursion_limit": 64}
        try:
            state = graph.invoke({"messages": messages, "steps": 0, "calls": 0}, run_config)
            if state.get("__interrupt__"):
                pending = state["__interrupt__"][0].value
                if pending["call"]["function"]["name"] != "record_note" or case["category"] not in {"approval", "recovery"}:
                    raise RuntimeError("harness refuses non-fixture approval")
                result["approval_exercised"] = True
                try:
                    state = graph.invoke(Command(resume=pending["binding"]), run_config)
                except ObservedEffect:
                    # Explicit observed-result reconciliation, NOT silent rerun
                    # or a claim of exactly-once external operations.
                    saved = (output / "effect.txt").read_bytes()
                    if write_result is None or saved.decode() != write_note:
                        raise RuntimeError("cannot reconcile original note")
                    checkpoint = graph.get_state(run_config)
                    values = checkpoint.values
                    graph.update_state(run_config, {"messages": values["messages"] + [write_result],
                                       "calls": values["calls"] + 1}, as_node="tools")
                    result["controller_interruption_exercised"] = True
                    state = graph.invoke(None, run_config)
            if state.get("__interrupt__") or state["messages"][-1]["role"] != "assistant" or state["messages"][-1].get("tool_calls"):
                raise RuntimeError("graph did not finish with an answer")
            result["final_text"] = state["messages"][-1]["content"]
            result["messages"] = state["messages"]
            before = compact(state)
            requests_before = result["usage"]["requests"]
            effects_before = result["effects"]
            # None reenters the completed graph, without injecting a new turn.
            replay = graph.invoke(None, run_config)
            result["replay_unchanged"] = (compact(replay) == before and
                result["usage"]["requests"] == requests_before and result["effects"] == effects_before)
            if not result["replay_unchanged"]:
                result["failures"].append("completed graph reentry changed output, requests or effects")
        except Exception as error:
            result["failures"].append(str(error))
            result["messages"] = graph.get_state(run_config).values.get("messages", [])
        if case["category"] in {"approval", "recovery"}:
            if not result["approval_exercised"]:
                result["failures"].append("did not exercise fixture approval")
            if result["effects"] != 1:
                result["failures"].append("expected one observed fixture effect")
        if case["category"] == "recovery" and not result["controller_interruption_exercised"]:
            result["failures"].append("did not exercise controller recovery")
        result["failures"].extend(check(case, result, args.verifier))
    result["wall_elapsed_ms"] = round((time.perf_counter() - started) * 1000)
    result["model_decisions"] = len(snapshots)
    result["http_attempts_without_usage"] = sum(a.get("status") != 200 for s in snapshots for a in s.get("http_attempts", []))
    result['cost_unknown'] = result['http_attempts_without_usage'] > 0 or any(s.get('usage_unknown') for s in snapshots)
    result["status"] = "failed" if result["failures"] else "passed"
    save(output / "result.json", result)
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("reference", type=Path)
    parser.add_argument("out", type=Path)
    parser.add_argument("--config", type=Path, required=True)
    parser.add_argument("--taskloom", type=Path, required=True)
    parser.add_argument("--verifier", type=Path, required=True)
    args = parser.parse_args()
    args.reference = args.reference.resolve()
    args.taskloom = args.taskloom.resolve()
    args.verifier = args.verifier.resolve()
    args.configuration = json.loads(args.config.read_text(encoding="utf-8"))
    baseline = json.loads((args.reference / "report.json").read_text(encoding="utf-8"))
    if args.configuration["model"] != baseline["origin"]["model"] or args.configuration["output_tokens"] != baseline["configuration"]["output_tokens"]:
        raise ValueError("model/output budget differs from the frozen Taskloom run")
    if any(p["input_per_million"] != 0 or p["output_per_million"] != 0 for p in args.configuration["prices"]) or not args.configuration["prices"]:
        raise ValueError("this local adapter requires explicit zero API fee; paid comparison admission is not implemented")
    from urllib.parse import urlsplit
    if urlsplit(args.configuration["provider"]["base_url"]).hostname not in {"127.0.0.1", "localhost", "::1"}:
        raise ValueError("this zero-fee comparison permits only a local model API")
    suite = json.loads((args.reference / "suite.json").read_text(encoding="utf-8"))
    ids = [case["id"] for case in suite["cases"]]
    if ids != [case["id"] for case in baseline["cases"]] or len(set(ids)) != len(ids) or any(not 1 <= len(value) <= 64 or any(c not in "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789_-" for c in value) for value in ids):
        raise ValueError("invalid, reordered or duplicate isolated case identity")
    if any(not (args.reference / case["trajectory_path"]).resolve().is_relative_to(args.reference) for case in baseline["cases"]):
        raise ValueError("trajectory escapes the frozen campaign directory")
    root = args.out.resolve()
    root.mkdir(parents=True, exist_ok=False)
    shutil.copy2(__file__, root / "harness.py")
    shutil.copy2(Path(__file__).with_name("native_retrieval.py"), root / "native_retrieval.py")
    save(root / "execution-provenance.json", {"framework_binary_sha256": hashlib.sha256(args.taskloom.read_bytes()).hexdigest(),
        'native_framework_source': framework_source_identity(),
        "verifier_binary_sha256": hashlib.sha256(args.verifier.read_bytes()).hexdigest(),
        "harness_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest()})
    report = {"version": 1, "framework": "LangGraph", "framework_version": importlib.metadata.version("langgraph"),
        "evidence_kind": "actual local native model API", "model": args.configuration["model"],
        "currency": "USD", "input_price_per_million": 0, "output_price_per_million": 0,
        "max_total_cost_micros": baseline["configuration"]["max_total_cost_micros"], "credential_environment": args.configuration["provider"]["key_env"],
        "suite_fingerprint": baseline["suite_fingerprint"], "cases": [],
        "generation_retry_policy": "two unchanged attempts on transient transport/HTTP errors, matching the Taskloom default; no tool retry",
        "limitations": ["custom documented StateGraph adapter, not all LangGraph applications",
            "in-memory native checkpoint; OS-process recovery not evaluated", "RAG uses the same Taskloom engine over copied corpus via local API; IPC affects latency",
            "hardware/electricity not metered",
            "same checks do not establish semantic claim support or general superiority"]}
    for case, reference in zip(suite["cases"], baseline["cases"]):
        result = run_case(case, reference, root, args)
        report["cases"].append(result)
        report["passed"] = sum(c["status"] == "passed" for c in report["cases"])
        report["failed"] = sum(c["status"] == "failed" for c in report["cases"])
        report["requested"] = len(suite["cases"])
        report["not_run"] = report["requested"] - len(report["cases"])
        save(root / "report.json", report)
        print(f"{case['id']}: {result['status']}: {result['failures']}", flush=True)
        if result['cost_unknown']:
            report['stopped_reason'] = 'unknown attempt retained; remaining cases are not dispatched'
            save(root / 'report.json', report)
            break
    raise SystemExit(1 if report["failed"] or report["not_run"] else 0)


if __name__ == "__main__":
    main()
