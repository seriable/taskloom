"""Download only declared public Qwen safetensors/configs; verify every byte.

No account/token, remote Python code, pickle weights or model invocation.
The manifest pins each file's revision, size and SHA-256 from ModelScope.
Run with --prepare to create a lock before downloading the larger artifacts.
"""
import argparse
import concurrent.futures
import hashlib
import json
from pathlib import Path
import urllib.parse
import urllib.request

PROFILES = {
    "standard": (("Qwen/Qwen3-1.7B", "Qwen/Qwen3-Embedding-0.6B"), 7 * 1024**3),
    "generation-4b": (("Qwen/Qwen3-4B",), 10 * 1024**3),
}
SMALL_FILES = {"config.json", "generation_config.json", "tokenizer.json",
               "tokenizer_config.json", "vocab.json", "merges.txt",
               "model.safetensors.index.json", "LICENSE"}


def digest(path):
    h = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            h.update(block)
    return h.hexdigest()


def prepare(root, profile="standard"):
    models, _ = PROFILES[profile]
    manifest = {"version": 1, "source": "https://modelscope.cn", "models": {}}
    for model in models:
        url = f"https://modelscope.cn/api/v1/models/{model}/repo/files?Revision=master&Recursive=true"
        with urllib.request.urlopen(url, timeout=30) as response:
            data = json.loads(response.read(1024 * 1024))
        if not data.get("Success"):
            raise ValueError("model metadata request failed")
        files = []
        for item in data["Data"]["Files"]:
            name = item["Path"]
            if item["Type"] == "blob" and (name in SMALL_FILES or
                    ("/" not in name and name.endswith(".safetensors"))):
                files.append({"path": name, "revision": item["Revision"],
                              "bytes": item["Size"], "sha256": item["Sha256"]})
        manifest["models"][model] = files
    validate(manifest, profile)
    root.mkdir(parents=True, exist_ok=True)
    # Never overwrite an established identity when a source's master moves.
    path = root / "models.lock.json"
    with path.open("x", encoding="utf-8") as stream:
        json.dump(manifest, stream, indent=2)
        stream.write("\n")
    return manifest


def validate(manifest, profile="standard"):
    models, max_total_bytes = PROFILES[profile]
    if manifest["version"] != 1 or manifest["source"] != "https://modelscope.cn" or set(manifest["models"]) != set(models):
        raise ValueError("unsupported model manifest")
    total = 0
    for model, files in manifest["models"].items():
        if not 3 <= len(files) <= 16 or len({f["path"] for f in files}) != len(files):
            raise ValueError("invalid file count")
        for item in files:
            name = item["path"]
            if "/" in name or "\\" in name or not (name in SMALL_FILES or name.endswith(".safetensors")):
                raise ValueError("unsupported model artifact")
            for key, size in [("revision", 40), ("sha256", 64)]:
                value = item[key]
                if len(value) != size or any(c not in "0123456789abcdef" for c in value):
                    raise ValueError("unbound revision or hash")
            if not 0 < item["bytes"] <= max_total_bytes:
                raise ValueError("invalid model file size")
            total += item["bytes"]
    if total > max_total_bytes:
        raise ValueError("model artifacts exceed download limit")


def download(root, model, item):
    folder = root / model.split("/")[1]
    folder.mkdir(parents=True, exist_ok=True)
    path = folder / item["path"]
    if path.exists():
        if path.stat().st_size == item["bytes"] and digest(path) == item["sha256"]:
            print(f"verified existing {model}/{item['path']}", flush=True)
            return
        raise ValueError(f"existing artifact differs: {path}; retain and investigate")
    url = f"https://modelscope.cn/models/{model}/resolve/{item['revision']}/{urllib.parse.quote(item['path'])}"
    temporary = path.with_name(path.name + ".partial")
    count = 0
    checksum = hashlib.sha256()
    with urllib.request.urlopen(url, timeout=60) as response, temporary.open("wb") as stream:
        host = urllib.parse.urlsplit(response.url).hostname or ""
        if not (host == "modelscope.cn" or host.endswith((".modelscope.cn", ".aliyuncs.com", ".aliyun.com"))):
            raise ValueError("model artifact redirected outside the configured publisher storage")
        while block := response.read(1024 * 1024):
            count += len(block)
            if count > item["bytes"]:
                raise ValueError("model artifact exceeds pinned size")
            stream.write(block)
            checksum.update(block)
            if count % (64 * 1024 * 1024) == 0:
                print(f"downloading {model}/{item['path']}: {count * 100 // item['bytes']}%", flush=True)
    if count != item["bytes"] or checksum.hexdigest() != item["sha256"]:
        raise ValueError("artifact size/hash mismatch; partial retained")
    temporary.replace(path)
    print(f"verified {model}/{item['path']} ({count} bytes)", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("root", type=Path)
    parser.add_argument("--prepare", action="store_true")
    parser.add_argument("--profile", choices=sorted(PROFILES), default="standard",
                        help="explicit model set and download byte ceiling; use a separate root per profile")
    args = parser.parse_args()
    root = args.root.resolve()
    path = root / "models.lock.json"
    manifest = json.loads(path.read_text(encoding="utf-8")) if path.exists() else prepare(root, args.profile)
    validate(manifest, args.profile)
    total = sum(f["bytes"] for files in manifest["models"].values() for f in files)
    print(f"pinned {total} bytes across {len(manifest['models'])} public models", flush=True)
    if args.prepare:
        return
    with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
        jobs = [pool.submit(download, root, model, item) for model, files in manifest["models"].items() for item in files]
        for job in jobs:
            job.result()


if __name__ == "__main__":
    main()
