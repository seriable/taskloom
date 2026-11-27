"""Bounded loopback-only OpenAI embedding API over verified local Qwen weights.

Requires locally installed torch and transformers. No network or remote code
is used after startup. This is an evaluation/example server, not a production
multi-user host. No query instruction/prefix or synthetic vectors are added.
"""
import argparse
import hmac
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import threading
import time

from download import digest, validate


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("root", type=Path)
    parser.add_argument("--port", type=int, default=18083)
    parser.add_argument("--key-env", default="TASKLOOM_LOCAL_API_KEY")
    parser.add_argument("--max-requests", type=int, default=256)
    parser.add_argument("--max-tokens", type=int, default=512)
    args = parser.parse_args()
    key = os.environ[args.key_env]
    if not key or not 1 <= args.max_requests <= 10000 or not 16 <= args.max_tokens <= 4096:
        raise ValueError("invalid local server limits")
    root = args.root.resolve()
    manifest = json.loads((root / "models.lock.json").read_text(encoding="utf-8"))
    validate(manifest)
    model_id = "Qwen/Qwen3-Embedding-0.6B"
    folder = root / model_id.split("/")[1]
    for item in manifest["models"][model_id]:
        file = folder / item["path"]
        if file.stat().st_size != item["bytes"] or digest(file) != item["sha256"]:
            raise ValueError("local embedding artifact differs from locked identity")
    import torch
    from transformers import AutoModel, AutoTokenizer
    torch.set_num_threads(4)
    device = "cuda:0" if torch.cuda.is_available() else "cpu"
    tokenizer = AutoTokenizer.from_pretrained(folder, padding_side="left", local_files_only=True, trust_remote_code=False)
    model = AutoModel.from_pretrained(folder, local_files_only=True, trust_remote_code=False,
                                      dtype=torch.bfloat16 if device.startswith("cuda") else torch.float32)
    model.to(device).eval()
    gate = threading.Lock()
    count = 0

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass  # Never log authorization headers or request text.

        def reply(self, code, value):
            body = json.dumps(value).encode()
            self.send_response(code)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def do_GET(self):
            if self.path != "/health":
                return self.reply(404, {"error": "not found"})
            self.reply(200, {"model": model_id, "requests": count, "max_requests": args.max_requests,
                             "device": device, "price_per_million_input_tokens": 0,
                             "currency": "USD", "cost_boundary": "local API fee only; hardware unmetered"})

        def do_POST(self):
            nonlocal count
            if not hmac.compare_digest(self.headers.get("Authorization", ""), "Bearer " + key):
                return self.reply(401, {"error": "local evaluation bearer key required"})
            if self.path != "/v1/embeddings":
                return self.reply(404, {"error": "not found"})
            if not gate.acquire(blocking=False):
                return self.reply(429, {"error": "one local inference request at a time"})
            try:
                size = int(self.headers.get("Content-Length", "0"))
                if not 0 < size <= 512 * 1024:
                    return self.reply(413, {"error": "bounded Content-Length required"})
                body = json.loads(self.rfile.read(size))
                texts = body.get("input")
                if body.get("model") != model_id or not isinstance(texts, list) or not 1 <= len(texts) <= 32 or any(not isinstance(t, str) or not t.strip() for t in texts):
                    return self.reply(400, {"error": "invalid model or input"})
                if count >= args.max_requests:
                    return self.reply(429, {"error": "local evaluation request cap exhausted"})
                encoded = tokenizer(texts, padding=True, truncation=False, return_tensors="pt")
                if encoded.input_ids.shape[1] > args.max_tokens:
                    return self.reply(400, {"error": "token limit exceeded; no silent truncation"})
                count += 1
                started = time.perf_counter()
                with torch.inference_mode():
                    output = model(**encoded.to(device))
                    # Left padding puts every sequence's last nonpadding token
                    # at the final position, as prescribed by the model card.
                    vectors = torch.nn.functional.normalize(output.last_hidden_state[:, -1].float(), p=2, dim=1).cpu().tolist()
                tokens = int(encoded.attention_mask.sum().item())
                self.reply(200, {"model": model_id, "object": "list", "data": [
                    {"index": i, "object": "embedding", "embedding": v} for i, v in enumerate(vectors)],
                    "usage": {"prompt_tokens": tokens, "total_tokens": tokens},
                    "local_inference_ms": int((time.perf_counter() - started) * 1000)})
            except (ValueError, TypeError, KeyError) as error:
                self.reply(400, {"error": str(error)})
            except Exception as error:
                print(f"embedding inference failed: {type(error).__name__}", flush=True)
                self.reply(500, {"error": "local inference failed; inspect server"})
            finally:
                gate.release()

    print(f"ready {model_id} at http://127.0.0.1:{args.port}; {device}; API fee USD 0; hardware unmetered", flush=True)
    server = ThreadingHTTPServer(("127.0.0.1", args.port), Handler)
    server.daemon_threads = True
    server.serve_forever()


if __name__ == "__main__":
    main()
