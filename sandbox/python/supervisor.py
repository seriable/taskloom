"""Container PID 1 only. Never execute this supervisor as a host-code fallback.

The root monitor owns the deadline and output ceiling; untrusted Python runs
as uid/gid 65534 and cannot signal/modify the monitor. Exiting PID 1 kills all
remaining namespace processes, including children that detached their session.
"""
import argparse
import base64
import json
import os
import resource
import selectors
import subprocess
import sys
import time


def emit(exit_code, stdout=b"", stderr=b"", timed_out=False, output_limited=False):
    packet = (json.dumps({
        "version": 1,
        "exit_code": exit_code,
        "stdout_base64": base64.b64encode(stdout).decode("ascii"),
        "stderr_base64": base64.b64encode(stderr).decode("ascii"),
        "timed_out": timed_out,
        "output_limited": output_limited,
    }) + "\n").encode("utf-8")
    # A disconnected/nonreading client must not keep container PID 1 alive
    # by blocking the final response. Kernel/container teardown takes priority.
    os.set_blocking(1, False)
    until = time.monotonic() + 0.5
    with selectors.DefaultSelector() as selector:
        selector.register(1, selectors.EVENT_WRITE)
        try:
            while packet and time.monotonic() < until:
                if selector.select(max(0, until - time.monotonic())):
                    packet = packet[os.write(1, packet):]
        except (BrokenPipeError, BlockingIOError):
            pass
    os._exit(0)


def read_code(deadline):
    # Cover ingress too: a controller dying before EOF cannot leave a root
    # monitor blocked forever waiting for code on an open Docker stdin.
    code = bytearray()
    os.set_blocking(0, False)
    with selectors.DefaultSelector() as selector:
        selector.register(0, selectors.EVENT_READ)
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                emit(124, timed_out=True)
            if selector.select(remaining):
                block = os.read(0, 8192)
                if not block:
                    break
                code.extend(block)
                if len(code) > 65536:
                    raise ValueError("code exceeds input limit")
    if not code:
        raise ValueError("empty code")
    return code.decode("utf-8")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--timeout-ms", type=int, required=True)
    parser.add_argument("--max-output-bytes", type=int, required=True)
    args = parser.parse_args()
    if os.getpid() != 1 or os.getuid() != 0:
        raise RuntimeError("supervisor requires container PID 1 and root monitor")
    if not 1 <= args.timeout_ms <= 30000 or not 1024 <= args.max_output_bytes <= 65536:
        raise ValueError("invalid supervisor limits")
    deadline = time.monotonic() + args.timeout_ms / 1000
    code = read_code(deadline)
    resource.setrlimit(resource.RLIMIT_CORE, (0, 0))
    child = subprocess.Popen(
        [sys.executable, "-I", "-B", "-c", code],
        stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        start_new_session=True, user=65534, group=65534, extra_groups=[],
        env={"PATH": "/usr/local/bin:/usr/bin:/bin", "HOME": "/tmp", "LANG": "C.UTF-8"},
    )
    selector = selectors.DefaultSelector()
    buffers = {"stdout": bytearray(), "stderr": bytearray()}
    for name, stream in (("stdout", child.stdout), ("stderr", child.stderr)):
        os.set_blocking(stream.fileno(), False)
        selector.register(stream, selectors.EVENT_READ, name)
    timed_out = False
    output_limited = False
    while selector.get_map():
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            timed_out = True
            break
        for key, _ in selector.select(min(remaining, 0.05)):
            data = os.read(key.fileobj.fileno(), 4096)
            if not data:
                selector.unregister(key.fileobj)
                key.fileobj.close()
                continue
            capacity = args.max_output_bytes - sum(map(len, buffers.values()))
            buffers[key.data].extend(data[:capacity])
            if len(data) > capacity:
                output_limited = True
                break
        if output_limited:
            break
    if timed_out or output_limited:
        # Exiting PID 1 ends the namespace, including detached descendants.
        # The monitor intentionally has no CAP_KILL across the worker UID;
        # do not make reporting the deadline depend on killpg permission.
        emit(124 if timed_out else 125, bytes(buffers["stdout"]),
             bytes(buffers["stderr"]), timed_out, output_limited)
        return
    try:
        code = child.wait(timeout=max(0.001, deadline - time.monotonic()))
    except subprocess.TimeoutExpired:
        emit(124, bytes(buffers["stdout"]), bytes(buffers["stderr"]), True)
        return
    emit(code, bytes(buffers["stdout"]), bytes(buffers["stderr"]))


if __name__ == "__main__":
    try:
        main()
    except Exception as error:
        emit(126, stderr=("sandbox supervisor failed: " + type(error).__name__
                          + "; verify the pinned image contract").encode("utf-8"))
