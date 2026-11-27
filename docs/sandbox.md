# Explicit Linux-container Python execution

The optional `sandbox` feature exposes `CodeSandbox`, `DockerSandbox` and
`SandboxTool`. `app` includes the feature but registers no execution tool until
the host supplies `sandbox` configuration. A missing Docker engine, missing
image or failed container never falls back to host Python or a host shell.

## Isolation contract

Only code is model-controlled. The host fixes the Docker CLI, local engine
socket/pipe, immutable audited image, platform, CPU/memory/output/time limits.
The model cannot select an executable, image, mount, network, package, device,
environment variable or Docker option. Code is sent through stdin, not shell
command text or host argv interpolation.
Before dispatch, a bounded engine capability query requires Linux, memory/swap/
PID/CPU limits, private cgroup namespaces and Docker's built-in seccomp profile.
Unsupported or unverified capabilities fail instead of relying on warnings
about ignored resource flags.

Each call runs a fresh Linux container with no network, no host bind mounts,
read-only root filesystem, a 16 MiB non-executable `/tmp`, 32-PID ceiling,
bounded CPU/memory/swap, file-descriptor/file-size/core limits, private IPC/
cgroup namespaces, Docker's normal seccomp policy and no-new-privileges.
There is no Docker socket, host credential, artifact export or package install
capability. Docker CLI configuration/environment is replaced with an empty
private config so host proxy variables/context credentials are not injected.

The supplied image's Python supervisor runs as **container PID 1 and uid 0**,
with only SETUID/SETGID capabilities. Its untrusted child runs as uid/gid 65534,
without those privileges, and cannot signal the root monitor. The monitor owns
the wall deadline and combined stdout/stderr byte ceiling. It returns a bounded
JSON packet and exits container PID 1 on timeout, excessive output or completion,
causing the container runtime to kill remaining namespace processes even if
they detached from the original process group. This deadline survives loss of
the host controller; it does not depend solely on killing `docker run`.
Ingress is also nonblocking and deadline-bound if the controller dies before
stdin EOF. Final response writing has a 500 ms grace before unconditional PID 1
exit, so an abandoned/nonreading output connection cannot keep code running.

`--rm` removes the container after completion. The host additionally requests
forced removal on normal return, timeout and dropped futures. Cleanup is bounded
and best-effort if the daemon/runtime is unavailable. Results expose
`container_name` and `cleanup_confirmed`; inspect an unconfirmed cleanup instead
of assuming resources were removed. A process/daemon crash can prevent host
cleanup; the audited container monitor remains the execution time boundary
while its Linux runtime is functioning. Resource limits are per call, not a
global Docker billing/concurrency account.

A container shares its Linux kernel with the worker host. This is not VM or
hardware isolation, a guarantee against kernel/runtime exploits, or support for
arbitrary hostile multitenant workloads. Use a dedicated VM/worker or implement
`CodeSandbox` against an appropriately isolated service for that threat model.
The Docker daemon/socket is a powerful host capability and must remain
operator-owned. The tool never exposes the socket to submitted code.

## Build and pin the trusted supervisor image

Use a running local **Linux** Docker engine. Resolve a trusted official Python
base image to an immutable digest, review `sandbox/python/supervisor.py`, build
the supplied Dockerfile, then pin the resulting image ID/digest. For Linux:

```sh
DOCKER=/usr/bin/docker
ENGINE=unix:///var/run/docker.sock
$DOCKER --host "$ENGINE" pull python:3.12-slim
BASE_IMAGE=$($DOCKER --host "$ENGINE" image inspect --format '{{index .RepoDigests 0}}' python:3.12-slim)
$DOCKER --host "$ENGINE" build --platform linux/amd64 --build-arg BASE_IMAGE="$BASE_IMAGE" -t taskloom-sandbox:local sandbox/python
IMAGE=$($DOCKER --host "$ENGINE" image inspect --format '{{.Id}}' taskloom-sandbox:local)
```

Retain the resolved base and final image digests with your deployment evidence;
repeat production builds using those exact digests. The mutable tag above is
only an explicit base discovery step. Runtime execution accepts only
`sha256:64-lowercase-hex` or `repository@sha256:64-lowercase-hex`, and always uses
`--pull=never`. A digest must identify the **audited supplied supervisor image**;
an arbitrary image digest does not establish its safety. The entrypoint is
fixed to `/usr/local/bin/python3 -I -B /opt/taskloom/supervisor.py`.

On Windows select the installed `docker.exe` and the Linux engine's named pipe,
for example `npipe:////./pipe/dockerDesktopLinuxEngine`. TCP, SSH, Windows
containers and context/engine fallback are rejected. Choose `linux/arm64` only
with a matching audited image/worker; there is no platform auto-detection.

## Application configuration and approvals

Merge into your host config after replacing paths and the digest:

```json
{
  "sandbox": {
    "executable": "/usr/bin/docker",
    "endpoint": "unix:///var/run/docker.sock",
    "image": "sha256:REPLACE_WITH_64_LOWERCASE_HEX_IMAGE_ID",
    "platform": "linux/amd64",
    "memory_mib": 128,
    "cpu_millis": 500,
    "timeout_ms": 5000,
    "max_output_bytes": 16384
  }
}
```

The placeholder deliberately fails validation. Maximum bounds are 30 seconds,
64 KiB code, 64 KiB combined raw output, 1 GiB memory and two CPUs per call.
`cpu_millis: 500` means half a CPU. The container result reports exit code,
stdout/stderr, timeout/output-limit flags, image, named container, elapsed time
and cleanup confirmation. Non-UTF-8 output is displayed lossily after the raw
byte limit is enforced. Container/infrastructure charges are not generation
`RunBudget` amounts.

The registered tool is `python_sandbox`, permission `sandbox:python`, with a
single argument `{"code":"..."}`. A workflow executor role must explicitly
allow this name. The tool is conservatively NonIdempotent, so default durable
approval requires review of the exact code and worker binding before dispatch.
A decision never implicitly executes code: resume the run/parent separately.
Changed image/limits/engine contracts invalidate pending reentry. PEER planning,
writing and reviewing cannot acquire this effectful tool. Generic AgentTask
execution and default app configuration remain unchanged.

For a library host:

```rust,no_run
use std::sync::Arc;
use taskloom::runtime::{ToolRegistry, ToolPolicy};
use taskloom::runtime::sandbox::{DockerSandbox, DockerSandboxConfig, SandboxTool};
# fn example(config: DockerSandboxConfig) -> Result<(), taskloom::runtime::RuntimeError> {
let mut tools = ToolRegistry::new();
tools.register(SandboxTool(Arc::new(DockerSandbox::new(config)?)))?;
let policy = ToolPolicy::default().allow("python_sandbox").grant("sandbox:python");
// Pass tools/policy to the existing AgentLoop + DurableAgent controller.
# let _ = policy; Ok(()) }
```

## Acceptance and current limits

Default tests verify the security-relevant command contract, bounded config,
exact-code approval and frozen worker identity. These are configuration/control
tests, **not proof that an engine actually isolated a workload**.

The separate real-container suite observes uid/gid, denied root-monitor signals,
read-only filesystem, no socket/network/host secret, output capping, detached
child timeout and cleanup after an observed running controller is cancelled.
It also holds stdin open without code/EOF and requires the supervisor to exit.
It is ignored by default and requires the audited image and running engine:

```sh
export TASKLOOM_SANDBOX_DOCKER=/usr/bin/docker
export TASKLOOM_SANDBOX_ENDPOINT=unix:///var/run/docker.sock
export TASKLOOM_SANDBOX_IMAGE="$IMAGE"
cargo test --locked --features sandbox --test agent_sandbox -- --ignored --test-threads=1
```

A separate ignored test uses the real Docker CLI against an intentionally
absent local endpoint and requires failure before code dispatch. This test
passed on Windows on 2026-11-27. It proves the unavailable-engine failure path,
not container isolation. Run only that case without a built image using
`cargo test --features sandbox --test agent_sandbox actual_docker_cli_missing_engine -- --ignored`
after setting `TASKLOOM_SANDBOX_DOCKER`.

The hosted CI workflow includes explicit image-build/isolation acceptance and
retains resolved base/final image provenance. Its configuration is not evidence
of a successful run; GitHub account billing currently prevents hosted jobs.

2026-11-27: after the owner manually started Docker Desktop, all five separately
invoked environment tests passed on its Linux/amd64 engine. This includes actual
cgroup memory/swap/PID/CPU values and file limits, uid/gid, denied monitor signals,
read-only root, no network/host secret/socket, detached-child deadlines, output
limits, open stdin without EOF and cancellation cleanup. No labelled test
containers remained after the suite. The first deadline attempt exposed a
cross-UID `killpg` permission failure; the repaired monitor uses bounded response
writing and unconditional PID 1 exit without adding CAP_KILL.

Docker Hub download was unavailable. The build used the existing cached official
Python 3.11 slim-bookworm image at a fixed digest. The built image, supervisor
source hash and actual test evidence are recorded in
[sandbox acceptance](acceptance/sandbox-20261127.json). This proves these cases
on this local engine; hosted CI remains blocked by the GitHub account issue.
An earlier automatic rejection of starting Docker was resolved by the owner's
manual startup; no rejected startup command was retried through another route.
