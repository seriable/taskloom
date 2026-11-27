#![cfg(feature = "sandbox")]
use futures_util::future::BoxFuture;
use serde_json::json;
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use std::time::Duration;
use taskloom::runtime::sandbox::*;
use taskloom::runtime::*;

#[tokio::test]
#[ignore = "requires explicit Docker CLI path, intentionally uses an absent local engine endpoint"]
async fn actual_docker_cli_missing_engine_fails_before_code_without_host_fallback() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = config();
    cfg.executable = PathBuf::from(
        std::env::var("TASKLOOM_SANDBOX_DOCKER").expect("set explicit Docker CLI path"),
    );
    cfg.endpoint = if cfg!(windows) {
        format!(
            "npipe:////./pipe/taskloom-absent-{}-{}",
            std::process::id(),
            dir.path().file_name().unwrap().to_string_lossy()
        )
    } else {
        format!("unix://{}", dir.path().join("absent.sock").display())
    };
    let sandbox = DockerSandbox::new(cfg).unwrap();
    let result = sandbox
        .execute("print('must not execute')", &context())
        .await;
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("engine unavailable"));
}

fn config() -> DockerSandboxConfig {
    DockerSandboxConfig {
        executable: std::env::current_exe().unwrap(),
        endpoint: "unix:///var/run/docker.sock".into(),
        image: format!("sha256:{}", "a".repeat(64)),
        platform: "linux/amd64".into(),
        memory_mib: 128,
        cpu_millis: 500,
        timeout_ms: 1000,
        max_output_bytes: 4096,
    }
}
fn context() -> ToolContext {
    ToolContext {
        run_id: "sandbox-test".into(),
        call_id: "python-1".into(),
    }
}
#[test]
fn command_contract_grants_no_host_mount_network_shell_or_mutable_image() {
    let sandbox = DockerSandbox::new(config()).unwrap();
    let code = "print('$(arbitrary host shell); --privileged --volume /:/host')";
    let spec = sandbox.command_spec(code, &context()).unwrap();
    for flag in [
        "--pull=never",
        "--init=false",
        "--network=none",
        "--read-only",
        "--cap-drop=ALL",
        "--security-opt=no-new-privileges",
        "--user=0:0",
        "--pids-limit=32",
        "--ipc=none",
        "--cgroupns=private",
        "--log-driver=none",
        "--entrypoint=/usr/local/bin/python3",
    ] {
        assert!(spec.arguments.iter().any(|v| v == flag), "{flag}");
    }
    for flag in [
        "--privileged",
        "--volume",
        "--mount",
        "--device",
        "--env",
        "--env-file",
        "--pid=host",
        "--network=host",
        "sh",
        "bash",
        "cmd",
        "powershell",
    ] {
        assert!(!spec.arguments.iter().any(|v| v == flag), "{flag}");
    }
    assert!(!spec.arguments.iter().any(|v| v.contains(code)));
    assert!(spec
        .arguments
        .iter()
        .any(|v| v.contains("noexec,size=16m,uid=65534,gid=65534")));
    assert_eq!(
        sandbox
            .command_spec(code, &context())
            .unwrap()
            .container_name,
        spec.container_name
    );
    assert_ne!(
        sandbox
            .command_spec("print(2)", &context())
            .unwrap()
            .container_name,
        spec.container_name
    );
}
#[test]
fn configuration_rejects_remote_engines_mutable_images_and_unbounded_execution() {
    let mut option_like = config();
    option_like.image = format!("-repo@sha256:{}", "a".repeat(64));
    assert!(DockerSandbox::new(option_like).is_err());
    for image in [
        "python:latest",
        "python:3.12",
        "sha256:abc",
        "--privileged",
        "repo@sha256:zzzz",
    ] {
        let mut cfg = config();
        cfg.image = image.into();
        assert!(DockerSandbox::new(cfg).is_err());
    }
    for endpoint in [
        "tcp://localhost:2375",
        "ssh://server",
        "unix://relative",
        "npipe://",
        "unix:///socket?param=1",
    ] {
        let mut cfg = config();
        cfg.endpoint = endpoint.into();
        assert!(DockerSandbox::new(cfg).is_err());
    }
    let mut cfg = config();
    cfg.timeout_ms = 30001;
    assert!(DockerSandbox::new(cfg).is_err());
    let mut cfg = config();
    cfg.platform = "windows/amd64".into();
    assert!(DockerSandbox::new(cfg).is_err());
    let sandbox = DockerSandbox::new(config()).unwrap();
    assert!(sandbox
        .command_spec(&"x".repeat(65537), &context())
        .is_err());
}

struct FixtureSandbox {
    calls: Arc<AtomicUsize>,
    version: &'static str,
}
impl CodeSandbox for FixtureSandbox {
    fn identity(&self) -> String {
        self.version.into()
    }
    fn execute<'a>(
        &'a self,
        code: &'a str,
        _: &'a ToolContext,
    ) -> BoxFuture<'a, Result<SandboxOutcome, RuntimeError>> {
        Box::pin(async move {
            assert_eq!(code, "print(2+2)");
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(SandboxOutcome {
                image: "fixture-not-a-container".into(),
                container_name: "fixture".into(),
                exit_code: 0,
                stdout: "4\n".into(),
                stderr: String::new(),
                timed_out: false,
                output_limited: false,
                cleanup_confirmed: true,
                elapsed_ms: 0,
            })
        })
    }
}
struct CodeModel;
impl Model for CodeModel {
    fn generate<'a>(
        &'a self,
        request: &'a ModelRequest,
        budget: &'a RunBudget,
    ) -> BoxFuture<'a, Result<ModelOutput, RuntimeError>> {
        Box::pin(async move {
            budget.reserve_before_dispatch(0).await?.settle(0);
            Ok(ModelOutput {
                model: request.model.clone(),
                turn: if request
                    .messages
                    .iter()
                    .any(|m| matches!(m, Message::Tool { .. }))
                {
                    AssistantTurn::text("fixture done")
                } else {
                    AssistantTurn {
                        content: String::new(),
                        tool_calls: vec![ToolCall {
                            id: "python".into(),
                            name: "python_sandbox".into(),
                            arguments: json!({"code":"print(2+2)"}),
                        }],
                    }
                },
            })
        })
    }
}
fn registry(calls: Arc<AtomicUsize>, version: &'static str) -> ToolRegistry {
    let mut tools = ToolRegistry::new();
    tools
        .register(SandboxTool(Arc::new(FixtureSandbox { calls, version })))
        .unwrap();
    tools
}
#[tokio::test]
async fn sandbox_tool_requires_exact_code_approval_and_freezes_worker_identity() {
    let dir = tempfile::tempdir().unwrap();
    let store = FileRunStore::new(dir.path());
    let model = CodeModel;
    let calls = Arc::new(AtomicUsize::new(0));
    let tools = registry(calls.clone(), "fixture-v1");
    let policy = ToolPolicy::default()
        .allow("python_sandbox")
        .grant("sandbox:python");
    let agent = AgentLoop::new(&model, &tools, "fixture").with_policy(policy.clone());
    let durable = DurableAgent::new(&agent, &store);
    let state = durable
        .start_or_resume("sandbox", vec![Message::user("compute")])
        .await
        .unwrap();
    assert_eq!(state.status, RunStatus::WaitingApproval);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let ticket = state.pending_approval().unwrap();
    assert_eq!(ticket.call.arguments, json!({"code":"print(2+2)"}));
    let changed = registry(calls.clone(), "fixture-v2");
    let other = AgentLoop::new(&model, &changed, "fixture").with_policy(policy);
    assert!(DurableAgent::new(&other, &store)
        .decide(
            "sandbox",
            &ticket.request_id,
            ApprovalDecision {
                approved: true,
                actor: "operator".into(),
                note: "reviewed".into()
            }
        )
        .await
        .is_err());
    durable
        .decide(
            "sandbox",
            &ticket.request_id,
            ApprovalDecision {
                approved: true,
                actor: "operator".into(),
                note: "reviewed exact code and worker".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        durable.resume("sandbox").await.unwrap().report.stop_reason,
        StopReason::Completed
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

fn real_sandbox(timeout_ms: u64, max_output_bytes: usize) -> DockerSandbox {
    let cfg = DockerSandboxConfig {
        executable: PathBuf::from(
            std::env::var("TASKLOOM_SANDBOX_DOCKER").expect("set explicit Docker CLI path"),
        ),
        endpoint: std::env::var("TASKLOOM_SANDBOX_ENDPOINT")
            .expect("set local Linux engine endpoint"),
        image: std::env::var("TASKLOOM_SANDBOX_IMAGE")
            .expect("set audited supervisor image digest"),
        platform: std::env::var("TASKLOOM_SANDBOX_PLATFORM")
            .unwrap_or_else(|_| "linux/amd64".into()),
        memory_mib: 128,
        cpu_millis: 500,
        timeout_ms,
        max_output_bytes,
    };
    DockerSandbox::new(cfg).unwrap()
}
#[tokio::test]
#[ignore = "requires running local Linux Docker engine and pinned audited supervisor image"]
async fn actual_container_denies_root_monitor_signals_host_writes_network_and_secrets() {
    std::env::set_var("TASKLOOM_SANDBOX_TEST_SECRET", "host-fixture-only");
    let sandbox = real_sandbox(3000, 8192);
    let result = sandbox
        .execute(
            r#"
import os, socket, signal, resource
assert os.getuid() == 65534 and os.getgid() == 65534
assert 'TASKLOOM_SANDBOX_TEST_SECRET' not in os.environ
assert not os.path.exists('/var/run/docker.sock')
assert int(open('/sys/fs/cgroup/memory.max').read()) == 128 * 1024 * 1024
assert open('/sys/fs/cgroup/memory.swap.max').read().strip() == '0'
assert open('/sys/fs/cgroup/pids.max').read().strip() == '32'
quota, period = map(int, open('/sys/fs/cgroup/cpu.max').read().split())
assert quota / period == .5
assert resource.getrlimit(resource.RLIMIT_NOFILE) == (64, 64)
assert resource.getrlimit(resource.RLIMIT_FSIZE) == (1048576, 1048576)
try:
    os.kill(1, signal.SIGTERM)
    raise AssertionError('worker signalled root monitor')
except PermissionError:
    pass
try:
    open('/host-marker', 'w').write('escape')
    raise AssertionError('root filesystem writable')
except OSError:
    pass
try:
    socket.create_connection(('1.1.1.1', 443), timeout=.2)
    raise AssertionError('network accessible')
except OSError:
    pass
print('isolation checks passed')
"#,
            &context(),
        )
        .await
        .unwrap();
    std::env::remove_var("TASKLOOM_SANDBOX_TEST_SECRET");
    assert_eq!(result.exit_code, 0, "{}", result.stderr);
    assert!(result.cleanup_confirmed);
    assert!(result.stdout.contains("isolation checks passed"));
}
#[tokio::test]
#[ignore = "requires running local Linux Docker engine and pinned audited supervisor image"]
async fn actual_supervisor_caps_output_and_kills_detached_children_at_deadline() {
    let sandbox = real_sandbox(500, 1024);
    let mut ctx = context();
    ctx.call_id = "deadline".into();
    let result=sandbox.execute("import os,time,signal\nif os.fork()==0:\n os.setsid()\n signal.signal(signal.SIGTERM,signal.SIG_IGN)\nwhile True: time.sleep(.01)",&ctx).await.unwrap();
    assert!(result.timed_out, "{result:?}");
    assert_eq!(result.exit_code, 124);
    assert!(result.cleanup_confirmed);
    ctx.call_id = "output".into();
    let result = sandbox
        .execute("import os\nwhile True: os.write(1,b'x'*4096)", &ctx)
        .await
        .unwrap();
    assert!(result.output_limited, "{result:?}");
    assert_eq!(result.exit_code, 125);
    assert!(result.stdout.len() + result.stderr.len() <= 1024);
    assert!(result.cleanup_confirmed);
}
#[tokio::test]
#[ignore = "requires running local Linux Docker engine and pinned audited supervisor image"]
async fn actual_cancelled_controller_leaves_no_named_container() {
    let sandbox = real_sandbox(10000, 1024);
    let ctx = context();
    let spec = sandbox
        .command_spec("import time;time.sleep(100)", &ctx)
        .unwrap();
    let worker = sandbox.clone();
    let handle =
        tokio::spawn(async move { worker.execute("import time;time.sleep(100)", &ctx).await });
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let probe_config = tempfile::tempdir().unwrap();
            let output = docker_command(&spec.program, probe_config.path())
                .args([
                    "--host",
                    &sandbox_config_endpoint(),
                    "inspect",
                    "--type=container",
                    "--format={{.State.Running}}",
                    &spec.container_name,
                ])
                .kill_on_drop(true)
                .output()
                .await
                .unwrap();
            if output.status.success() && String::from_utf8_lossy(&output.stdout).trim() == "true" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("container must be observed running before controller cancellation");
    handle.abort();
    let _ = handle.await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    let probe_config = tempfile::tempdir().unwrap();
    let output = docker_command(&spec.program, probe_config.path())
        .args([
            "--host",
            &sandbox_config_endpoint(),
            "inspect",
            "--type=container",
            &spec.container_name,
        ])
        .output()
        .await
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("No such"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
fn sandbox_config_endpoint() -> String {
    std::env::var("TASKLOOM_SANDBOX_ENDPOINT").unwrap()
}

#[tokio::test]
#[ignore = "requires running local Linux Docker engine and pinned audited supervisor image"]
async fn actual_supervisor_deadline_covers_open_stdin_without_code_or_eof() {
    use tokio::io::AsyncReadExt;
    let sandbox = real_sandbox(500, 1024);
    let spec = sandbox.command_spec("print(1)", &context()).unwrap();
    let probe_config = tempfile::tempdir().unwrap();
    let mut child = docker_command(&spec.program, probe_config.path())
        .args(&spec.arguments)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let held_stdin = child.stdin.take().unwrap();
    let mut stdout = child.stdout.take().unwrap();
    let mut bytes = Vec::new();
    let (_, status) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::try_join!(stdout.read_to_end(&mut bytes), child.wait())
    })
    .await
    .expect("PID 1 must exit while stdin remains open")
    .unwrap();
    drop(held_stdin);
    assert!(status.success());
    let response: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(response["timed_out"], true);
    assert_eq!(response["exit_code"], 124);
}
fn docker_command(program: &std::path::Path, dir: &std::path::Path) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(program);
    command
        .env_clear()
        .env("DOCKER_CONFIG", dir)
        .env("HOME", dir)
        .env("LANG", "C");
    #[cfg(windows)]
    {
        command.creation_flags(0x08000000);
        if let Some(root) = std::env::var_os("SystemRoot") {
            command.env("SystemRoot", root);
        }
    }
    command
}
