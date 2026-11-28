"""Audit actual model conditioning and resource parity for a saved comparison.

Only opaque tool-call control IDs are normalized. Prompts, tool definitions,
retrieval values, results, model, temperature and output ceilings must match.
This audit cannot establish factual quality or generalize to other adapters.
"""
import argparse
import hashlib
import json
from pathlib import Path


def compact(value):
    return json.dumps(value, sort_keys=True, ensure_ascii=False, separators=(",", ":"))


def messages(source):
    out = []
    for message in source:
        role = message["role"]
        if role in {"system", "user"}:
            out.append({"role": role, "content": message["content"]})
        elif role == "assistant":
            turn = message["turn"]
            item = {"role": role, "content": turn["content"]}
            if turn["tool_calls"]:
                item["tool_calls"] = [{"id": c["id"], "type": "function", "function": {
                    "name": c["name"], "arguments": compact(c["arguments"])}} for c in turn["tool_calls"]]
            out.append(item)
        elif role == "tool":
            result = message["result"]
            out.append({"role": role, "tool_call_id": result["call_id"], "content": compact({
                "output": result["output"], "is_error": result["is_error"]})})
    return out


def normalized(body):
    body = json.loads(json.dumps(body))
    ids = {}
    for message in body["messages"]:
        for call in message.get("tool_calls", []):
            original = call["id"]
            ids.setdefault(original, f"opaque-call-{len(ids)}")
            call["id"] = ids[original]
        if "tool_call_id" in message:
            message["tool_call_id"] = ids[message["tool_call_id"]]
    return body


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("taskloom", type=Path)
    parser.add_argument("comparison", type=Path)
    parser.add_argument("out", type=Path)
    parser.add_argument('--require-applied-contract', action='store_true',
                        help='require fresh native adapter execution-contract evidence; historical runs may lack it')
    args = parser.parse_args()
    baseline = json.loads((args.taskloom / "report.json").read_text(encoding="utf-8"))
    comparison = json.loads((args.comparison / "report.json").read_text(encoding="utf-8"))
    cases = []
    expected_ids = [c['id'] for c in baseline['cases']]
    actual = [c['id'] for c in comparison['cases']]
    if not expected_ids or len(set(expected_ids)) != len(expected_ids) or expected_ids != actual:
        raise ValueError('nonempty ordered complete case denominator must match; no missing/duplicate/reordered cases')
    suite = json.loads((args.taskloom/'suite.json').read_text(encoding='utf-8'))
    if [c['id'] for c in suite['cases']] != expected_ids:
        raise ValueError('frozen suite case denominator differs from recorded execution')
    if baseline['not_run'] or comparison['not_run'] or baseline['requested'] != len(expected_ids) or comparison['requested'] != len(actual):
        raise ValueError('incomplete campaign remains unexecuted; not comparable')
    for case in baseline["cases"]:
        native = json.loads((args.taskloom / case["trajectory_path"]).read_text(encoding="utf-8"))
        other = json.loads((args.comparison / case["id"] / "model-api.json").read_text(encoding="utf-8"))
        outcomes = [c for c in comparison["cases"] if c["id"] == case["id"]]
        contract = outcomes[0].get('applied_contract')
        fields = ['model', 'temperature', 'max_output_tokens', 'limits', 'policy', 'approvals', 'context', 'tools']
        contract_matches = contract is not None and all(contract.get(key) == native['config'][key] for key in fields)
        config = native["config"]
        tools = [{"type": "function", "function": {"name": t["name"], "description": t["description"],
                 "parameters": t["input_schema"]}} for t in config["tools"]]
        checks = []
        for context, recorded in zip(native["report"]["contexts"], other):
            if context["summary"] is not None:
                checks.append({"step": context["step"], "matched": False, "reason": "compressed context needs explicit reconstruction"})
                continue
            selected = [native["report"]["messages"][i] for i in context["retained_indices"]]
            expected = {"model": config["model"], "temperature": config["temperature"],
                        "max_tokens": config["max_output_tokens"], "tools": tools, "messages": messages(selected)}
            actual = recorded["request"]
            checks.append({"step": context["step"], "matched": normalized(expected) == normalized(actual)})
        native_attempts = case["usage"]["requests"] + native["report"]["budget"]["uncertain_attempts"]
        other_attempts = sum(len(o.get("http_attempts", [])) for o in other)
        cases.append({"id": case["id"], "model_decisions_match": len(other) == len(native["report"]["contexts"]),
            'applied_contract_present': contract is not None, 'applied_contract_matches': contract_matches,
            'actual_context_compression_absent': all(c.get('summary') is None for c in native['report']['contexts']),
            "conditioning": checks, "native_http_attempts_known_or_uncertain": native_attempts,
            "comparison_http_attempts": other_attempts, "http_attempt_budget_match": native_attempts == other_attempts,
            "same_outcome": bool(outcomes) and case["status"] == outcomes[0]["status"]})
    report = {"version": 1, "normalized": "opaque tool-call IDs only", "cases": cases,
        'case_denominator': len(expected_ids), 'case_ids': expected_ids,
        'applied_contract_required': args.require_applied_contract,
        'all_applied_contracts_match': all(c['applied_contract_matches'] and c['actual_context_compression_absent'] for c in cases),
        "all_conditioning_matches": all(c["model_decisions_match"] and all(s["matched"] for s in c["conditioning"]) for c in cases),
        "all_http_attempt_counts_match": all(c["http_attempt_budget_match"] for c in cases),
        "same_suite_fingerprint": baseline["suite_fingerprint"] == comparison["suite_fingerprint"],
        "same_total_api_fee_cap": baseline["configuration"]["max_total_cost_micros"] == comparison["max_total_cost_micros"],
        "limitations": ["native HTTP attempt count is inferred from answered requests and durable uncertainty journals",
            "no latency superiority or factual-quality conclusion follows from parity"]}
    # Bind the actual recordings and adapter copy used in this run, not the
    # mutable current adapter source or a newly written conditions template.
    evidence = [args.taskloom/'report.json', args.taskloom/'suite.json', args.comparison/'report.json',
                args.comparison/'execution-provenance.json', args.comparison/'harness.py']
    for case in baseline['cases']:
        evidence += [args.taskloom/case['trajectory_path'], args.comparison/case['id']/'model-api.json',
                     args.comparison/case['id']/'result.json']
    report['evidence'] = [{'kind': 'private local original recording', 'path': str(p.resolve()),
                           'sha256': hashlib.sha256(p.read_bytes()).hexdigest()} for p in evidence]
    with args.out.open('x', encoding='utf-8') as stream:
        stream.write(json.dumps(report, ensure_ascii=False, indent=2) + '\n')
    print(json.dumps({k:v for k,v in report.items() if k != "cases"}, indent=2))
    if not all(report[k] for k in ["all_conditioning_matches", "all_http_attempt_counts_match", "same_suite_fingerprint", "same_total_api_fee_cap"]):
        raise SystemExit(1)
    if args.require_applied_contract and not report['all_applied_contracts_match']:
        raise SystemExit(1)


if __name__ == "__main__":
    main()
