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
        try:
            with urllib.request.urlopen(request, timeout=120) as response:
                return json.loads(response.read(4 * 1024 * 1024))
        except urllib.error.HTTPError as error:
            raise RuntimeError(f"HTTP {error.code}: {error.read(8192).decode()}") from error

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
    return {"positive_queries": positives, "recall_at_5": recall / positives,
            "mrr_at_5": mrr / positives, "negative_queries": negatives,
            "negative_correct": negative_correct, "errors": sum("error" in c for c in cases), "cases": cases}


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
    parser.add_argument("--database",type=Path,help="explicit AppDatabase JSON; migrate first and set only its named URL environment")
    args = parser.parse_args()
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
    if args.database:
        config['database']=json.loads(args.database.read_text(encoding='utf-8'))
    config_path = root / "taskloom.json"
    save(config_path, config)
    save(root / "dataset.json", dataset)
    save(root / "models.lock.json", manifest)
    report = {"version": 1, "evidence_kind": "actual local learned embedding API, no generation",
        "model": "Qwen/Qwen3-Embedding-0.6B", "model_files_identity": identity, "configuration": config,
        "limitations": ["small visible synthetic relevance set, not held out", "no semantic claim-support or factual-accuracy scoring",
                        "no competitor evaluation", "no query instruction/prefix", "API fee USD 0; hardware/electricity unmetered"]}
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
    except Exception as error:
        report["campaign_error"] = str(error)
        raise
    finally:
        report["wall_ms"] = round((time.perf_counter() - started) * 1000)
        save(root / "report.json", report)
    print(json.dumps({name: {k: v for k, v in report[name].items() if k != "cases"}
                      for name in ["lexical", "hybrid"]}, indent=2), flush=True)


if __name__ == "__main__":
    main()
