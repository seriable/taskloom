"""Run the same labelled corpus through actual Taskloom lexical/hybrid APIs.

This example only calls a zero-fee loopback embedding endpoint. No model
generation or competitors are evaluated. All returned rankings, errors and
separate embedding charges are retained, including negative-query failures.
"""
import argparse
import contextlib
import hashlib
import json
import os
from pathlib import Path
import socket
import subprocess
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid


def save(path, value):
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")


def source_matches(path, expected):
    # Git's Windows checkout may use CRLF. Bind the same source text on either
    # platform without permitting JSON/key/label changes.
    text = path.read_text(encoding="utf-8")
    return expected in {
        hashlib.sha256(text.encode("utf-8")).hexdigest(),
        hashlib.sha256(text.replace("\n", "\r\n").encode("utf-8")).hexdigest()}


def request_json(request):
    """Retry bounded safe observations only; POST uncertainty is never replayed."""
    read_only = request.get_method() == 'GET'
    attempts = 3 if read_only else 1
    for attempt in range(attempts):
        try:
            with urllib.request.urlopen(request, timeout=10 if read_only else 120) as response:
                return json.loads(response.read(4 * 1024 * 1024))
        except urllib.error.HTTPError as error:
            with error:
                detail = error.read(8192).decode(errors='replace')
            raise RuntimeError(f'HTTP {error.code}: {detail}') from error
        except (urllib.error.URLError, TimeoutError, ConnectionError):
            if attempt + 1 == attempts:
                raise
            time.sleep(.2 * (attempt + 1))


@contextlib.contextmanager
def host(executable, config, root):
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        port = sock.getsockname()[1]
    token = uuid.uuid4().hex
    env = dict(os.environ, TASKLOOM_API_TOKEN=token)
    log = (root / f"host-{port}.log").open("wb")
    process = subprocess.Popen([str(executable), "--config", str(config), "serve", f"127.0.0.1:{port}"],
                               env=env, stdout=log, stderr=subprocess.STDOUT,
                               creationflags=subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0)

    def api(path, body=None):
        request = urllib.request.Request(f"http://127.0.0.1:{port}/api{path}",
            data=None if body is None else json.dumps(body).encode(),
            headers={"Authorization": "Bearer " + token, "Content-Type": "application/json"})
        return request_json(request)

    # Owned process handle for explicit OS-process interruption campaigns.
    # This never refers to an independently running user process.
    api.host_process = process

    try:
        for _ in range(150):
            if process.poll() is not None:
                raise RuntimeError("local Taskloom host exited; inspect log")
            try:
                api("/retrieval")
                break
            except urllib.error.URLError:
                time.sleep(0.1)
        else:
            raise RuntimeError("local Taskloom host readiness timed out")
        yield api
    finally:
        process.terminate()
        try:
            process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=10)
        log.close()


def evaluate(api, dataset):
    cases = []
    recall = mrr = 0.0
    positives = negatives = negative_correct = 0
    for query in dataset["queries"]:
        case = {"id": query["id"], "query": query["text"], "relevant": query["relevant"]}
        if "split" in query:
            case["split"] = query["split"]
        started = time.perf_counter()
        try:
            result = api("/knowledge/search", {"text": query["text"], "top_k": 5, "max_bytes": 24576})
            case["result"] = result
            ranked = list(dict.fromkeys(hit["chunk"]["document_id"] for hit in result["hits"]))
            relevant = set(query["relevant"])
            matches = len(set(ranked) & relevant)
            rr = next((1 / (i + 1) for i, value in enumerate(ranked) if value in relevant), 0)
            if relevant:
                positives += 1
                recall += matches / len(relevant)
                mrr += rr
            else:
                negatives += 1
                negative_correct += int(not ranked)
            case.update(ranked=ranked, reciprocal_rank=rr)
        except Exception as error:
            case["error"] = str(error)
            if query["relevant"]:
                positives += 1
            else:
                negatives += 1
        case["wall_ms"] = round((time.perf_counter() - started) * 1000)
        cases.append(case)
        print(f"evaluated {query['id']}: {case.get('ranked', case.get('error'))}", flush=True)
    result = {"positive_queries": positives, "recall_at_5": recall / positives,
            "mrr_at_5": mrr / positives, "negative_queries": negatives,
            "negative_correct": negative_correct, "errors": sum("error" in c for c in cases), "cases": cases}
    result["splits"] = {}
    for split in sorted({q["split"] for q in cases if "split" in q}):
        selected = [q for q in cases if q.get("split") == split]
        positive = [q for q in selected if q["relevant"]]
        negative = [q for q in selected if not q["relevant"]]
        result["splits"][split] = {
            "positive_queries": len(positive), "negative_queries": len(negative),
            "recall_at_5": sum(len(set(q.get("ranked", [])) & set(q["relevant"])) / len(q["relevant"]) for q in positive) / len(positive) if positive else None,
            "mrr_at_5": sum(q.get("reciprocal_rank", 0) for q in positive) / len(positive) if positive else None,
            "negative_correct": sum("error" not in q and not q["ranked"] for q in negative),
            "errors": sum("error" in q for q in selected)}
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("out", type=Path)
    parser.add_argument("--taskloom", type=Path, required=True)
    parser.add_argument("--model-root", type=Path, required=True)
    parser.add_argument("--dataset", type=Path, default=Path("tests/data/retrieval-quality-v1.json"))
    parser.add_argument("--embedding-url", default="http://127.0.0.1:18083/v1")
    parser.add_argument("--key-env", default="TASKLOOM_LOCAL_API_KEY")
    parser.add_argument("--semantic-min-score", type=float, default=0.0)
    parser.add_argument("--lexical-weight", type=float, default=1.0)
    parser.add_argument("--profile", type=Path, help="explicit retrieval JSON: lexical settings, semantic cutoff and fusion; services supplied separately")
    parser.add_argument("--protocol", type=Path, help="fixed dataset/profile hashes and split quality gates; failure remains visible")
    parser.add_argument("--database",type=Path,help="explicit AppDatabase JSON; migrate first and set only its named URL environment")
    args = parser.parse_args()
    protocol = None
    if args.protocol:
        protocol = json.loads(args.protocol.read_text(encoding="utf-8"))
        if not args.profile or not source_matches(args.dataset, protocol["dataset_sha256"]) or not source_matches(args.profile, protocol["profile_sha256"]):
            raise ValueError("fixed protocol dataset/profile hash mismatch; refuse before dispatch")
    url = urllib.parse.urlsplit(args.embedding_url)
    if url.hostname not in {"127.0.0.1", "localhost", "::1"} or url.scheme != "http" or url.query or url.username:
        raise ValueError("this zero-fee example permits only a loopback embedding service")
    if not os.environ.get(args.key_env):
        raise ValueError("configure the local service's key environment first")
    root = args.out.resolve()
    root.mkdir(parents=True, exist_ok=False)
    dataset = json.loads(args.dataset.read_text(encoding="utf-8"))
    manifest = json.loads((args.model_root / "models.lock.json").read_text(encoding="utf-8"))
    identity = hashlib.sha256(json.dumps(manifest["models"]["Qwen/Qwen3-Embedding-0.6B"], sort_keys=True).encode()).hexdigest()
    config = {"state_dir": str(root / "state"), "provider": {"type": "offline"}, "retrieval": {
        "mode": "lexical", "semantic_min_score": args.semantic_min_score,
        "lexical_weight": args.lexical_weight, "embedding": {"provider": {"type": "openai", "base_url": args.embedding_url,
        "key_env": args.key_env, "namespace": "qwen3-embedding-files-" + identity},
        "model": "Qwen/Qwen3-Embedding-0.6B", "currency": "USD", "input_micros_per_million_tokens": 0,
        "max_request_cost_micros": 0, "max_total_cost_micros": 0, "max_input_bytes": 131072, "timeout_ms": 60000}}}
    if args.profile:
        profile = json.loads(args.profile.read_text(encoding="utf-8"))
        allowed = {"lexical", "semantic_min_score", "lexical_weight", "semantic_weight", "rank_constant", "candidate_k", "exact_identifiers"}
        if not isinstance(profile, dict) or set(profile) - allowed:
            raise ValueError("profile may only set lexical/fusion/relevance settings; no authority or service changes")
        config["retrieval"].update(profile)
    if args.database:
        config['database']=json.loads(args.database.read_text(encoding='utf-8'))
    config_path = root / "taskloom.json"
    save(config_path, config)
    save(root / "dataset.json", dataset)
    save(root / "models.lock.json", manifest)
    report = {"version": 1, "evidence_kind": "actual local learned embedding API, no generation",
        "taskloom_sha256": hashlib.sha256(args.taskloom.resolve().read_bytes()).hexdigest(),
        "model": "Qwen/Qwen3-Embedding-0.6B", "model_files_identity": identity, "configuration": config,
        "limitations": ["small visible synthetic relevance set, not held out", "no semantic claim-support or factual-accuracy scoring",
                        "no competitor evaluation", "no query instruction/prefix", "API fee USD 0; hardware/electricity unmetered"]}
    report["dataset_sha256"] = hashlib.sha256(args.dataset.read_bytes()).hexdigest()
    report["protocol"] = protocol
    report["phase_configurations"] = {"lexical": json.loads(json.dumps(config))}
    started = time.perf_counter()
    try:
        with host(args.taskloom.resolve(), config_path, root) as api:
            for i, document in enumerate(dataset["documents"]):
                api("/knowledge", {"document": document, "expected_revision": i})
            report["lexical"] = evaluate(api, dataset)
            status = api("/retrieval")
            report["build"] = api("/retrieval/index", {"expected_revision": status["corpus_revision"],
                                                      "expected_binding": status["configuration_binding"]})
        config["retrieval"]["mode"] = "hybrid"
        report["phase_configurations"]["hybrid"] = json.loads(json.dumps(config))
        save(config_path, config)
        with host(args.taskloom.resolve(), config_path, root) as api:
            report["hybrid"] = evaluate(api, dataset)
            report["index_status"] = api("/retrieval")
        if protocol:
            gates = protocol["gates"]
            measured = report["hybrid"]["splits"][gates["split"]]
            report["quality_checks"] = {
                "fixed_positive_count": measured["positive_queries"] == gates["positive_queries"],
                "fixed_negative_count": measured["negative_queries"] == gates["negative_queries"],
                "recall_at_5": measured["recall_at_5"] >= gates["min_recall_at_5"],
                "mrr_at_5": measured["mrr_at_5"] >= gates["min_mrr_at_5"],
                "negative_accuracy": measured["negative_correct"] / measured["negative_queries"] >= gates["required_negative_accuracy"],
                "errors": measured["errors"] <= gates["max_errors"]}
            report["quality_status"] = "passed" if all(report["quality_checks"].values()) else "failed"
    except Exception as error:
        report["campaign_error"] = str(error)
        raise
    finally:
        report["wall_ms"] = round((time.perf_counter() - started) * 1000)
        save(root / "report.json", report)
    print(json.dumps({name: {k: v for k, v in report[name].items() if k != "cases"}
                      for name in ["lexical", "hybrid"]}, indent=2), flush=True)
    if report.get("quality_status") == "failed":
        raise SystemExit(1)


if __name__ == "__main__":
    main()
