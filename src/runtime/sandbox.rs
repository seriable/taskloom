//! Host-configured isolated execution. A Linux container shares its kernel;
//! use a dedicated VM/worker for adversarial workloads. Never falls back to a
//! host Python interpreter or shell when the configured engine/image is absent.
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine;
use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::{
    state::fingerprint, RuntimeError, Tool, ToolContext, ToolDefinition, ToolEffect, ToolError,
};

/// Implementors must enforce isolated execution, host-owned resource bounds
/// and accurate cleanup reporting, without granting host files, network or
/// credentials. Never implement a fallback that runs submitted code on the host.
pub trait CodeSandbox: Send + Sync {
    fn identity(&self) -> String;
    fn execute<'a>(
        &'a self,
        code: &'a str,
        context: &'a ToolContext,
    ) -> BoxFuture<'a, Result<SandboxOutcome, RuntimeError>>;
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxOutcome {
    pub image: String,
    pub container_name: String,
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
    pub output_limited: bool,
    pub cleanup_confirmed: bool,
    pub elapsed_ms: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DockerSandboxConfig {
    pub executable: PathBuf,
    /// Explicit local Linux engine socket/pipe; TCP/SSH/context fallbacks denied.
    pub endpoint: String,
    /// Audited supervisor image: sha256:ID or repository@sha256:DIGEST.
    pub image: String,
    pub platform: String,
    pub memory_mib: u32,
    pub cpu_millis: u32,
    pub timeout_ms: u64,
    pub max_output_bytes: usize,
}
impl DockerSandboxConfig {
    pub fn validate(&self) -> Result<(), RuntimeError> {
        let digest = self
            .image
            .rsplit_once("sha256:")
            .filter(|(prefix, _)| prefix.is_empty() || prefix.ends_with('@'));
        let image_ok = digest.is_some_and(|(prefix, hash)| {
            hash.len() == 64
                && hash
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
                && prefix.len() <= 256
                && (prefix.is_empty()
                    || prefix.strip_suffix('@').is_some_and(|repo| {
                        repo.as_bytes()
                            .first()
                            .is_some_and(u8::is_ascii_alphanumeric)
                            && !repo.contains('@')
                    }))
                && prefix.bytes().all(|b| {
                    b.is_ascii_alphanumeric()
                        || matches!(b, b'/' | b'_' | b'.' | b':' | b'-' | b'@')
                })
        });
        let local = self
            .endpoint
            .strip_prefix("unix://")
            .is_some_and(|p| p.starts_with('/') && p.len() > 1)
            || self
                .endpoint
                .strip_prefix("npipe://")
                .is_some_and(|p| p.starts_with("//./pipe/") && p.len() > 9);
        if !self.executable.is_absolute()
            || !image_ok
            || !local
            || self.endpoint.len() > 1024
            || self
                .endpoint
                .chars()
                .any(|c| c.is_control() || c.is_whitespace() || matches!(c, '?' | '#'))
            || !matches!(self.platform.as_str(), "linux/amd64" | "linux/arm64")
            || !(64..=1024).contains(&self.memory_mib)
            || !(100..=2000).contains(&self.cpu_millis)
            || !(1..=30000).contains(&self.timeout_ms)
            || !(1024..=65536).contains(&self.max_output_bytes)
        {
            return Err(config_error("invalid isolated execution configuration"));
        }
        Ok(())
    }
}
#[derive(Debug, Clone)]
pub struct SandboxCommand {
    pub program: PathBuf,
    pub arguments: Vec<String>,
    pub container_name: String,
}
#[derive(Clone)]
pub struct DockerSandbox {
    config: DockerSandboxConfig,
    binding: String,
}
impl DockerSandbox {
    pub fn new(mut config: DockerSandboxConfig) -> Result<Self, RuntimeError> {
        config.validate()?;
        config.executable =
            std::fs::canonicalize(&config.executable).map_err(super::store::storage)?;
        if !config.executable.is_file() {
            return Err(config_error("Docker CLI must be a regular executable"));
        }
        let binding = format!("docker-python-supervisor-v1:{}", fingerprint(&config)?);
        Ok(Self { config, binding })
    }
    /// Reviewable argv: code is sent only over stdin, never interpolated into
    /// host command text. No model-selected image, path, mount, grant or flag.
    pub fn command_spec(
        &self,
        code: &str,
        context: &ToolContext,
    ) -> Result<SandboxCommand, RuntimeError> {
        if code.trim().is_empty()
            || code.len() > 65536
            || context.run_id.is_empty()
            || context.run_id.len() > 1024
            || context.call_id.is_empty()
            || context.call_id.len() > 1024
        {
            return Err(config_error("invalid sandbox code or context identity"));
        }
        let name = format!(
            "taskloom-{}",
            fingerprint(&(&self.binding, &context.run_id, &context.call_id, code))?
        );
        let memory = format!("{}m", self.config.memory_mib);
        let cpu = format!(
            "{}.{:03}",
            self.config.cpu_millis / 1000,
            self.config.cpu_millis % 1000
        );
        let args = vec![
            "--host".into(),
            self.config.endpoint.clone(),
            "run".into(),
            "--rm".into(),
            "--pull=never".into(),
            "--init=false".into(),
            "--name".into(),
            name.clone(),
            "--platform".into(),
            self.config.platform.clone(),
            "--network=none".into(),
            "--read-only".into(),
            "--cap-drop=ALL".into(),
            "--cap-add=SETUID".into(),
            "--cap-add=SETGID".into(),
            "--security-opt=no-new-privileges".into(),
            "--user=0:0".into(),
            "--pids-limit=32".into(),
            "--memory".into(),
            memory.clone(),
            "--memory-swap".into(),
            memory,
            "--cpus".into(),
            cpu,
            "--ulimit=nofile=64:64".into(),
            "--ulimit=fsize=1048576:1048576".into(),
            "--ulimit=core=0:0".into(),
            "--tmpfs=/tmp:rw,nosuid,nodev,noexec,size=16m,uid=65534,gid=65534,mode=0711".into(),
            "--workdir=/tmp".into(),
            "--ipc=none".into(),
            "--cgroupns=private".into(),
            "--log-driver=none".into(),
            "--label=org.taskloom.sandbox=python-supervisor-v1".into(),
            "--interactive".into(),
            "--entrypoint=/usr/local/bin/python3".into(),
            self.config.image.clone(),
            "-I".into(),
            "-B".into(),
            "/opt/taskloom/supervisor.py".into(),
            "--timeout-ms".into(),
            self.config.timeout_ms.to_string(),
            "--max-output-bytes".into(),
            self.config.max_output_bytes.to_string(),
            // Code travels over stdin only; its size and digest let the
            // supervisor refuse a truncated (interrupted) or altered write.
            "--code-bytes".into(),
            code.len().to_string(),
            "--code-sha256".into(),
            hex::encode(Sha256::digest(code.as_bytes())),
        ];
        Ok(SandboxCommand {
            program: self.config.executable.clone(),
            arguments: args,
            container_name: name,
        })
    }
    fn command(&self, config_dir: &std::path::Path) -> tokio::process::Command {
        let mut command = tokio::process::Command::new(&self.config.executable);
        // Docker config can inject host proxy variables into containers. Use
        // an empty private config and no inherited environment/context/auth.
        command
            .env_clear()
            .env("DOCKER_CONFIG", config_dir)
            .env("HOME", config_dir)
            .env("LANG", "C")
            .kill_on_drop(true);
        #[cfg(windows)]
        {
            command.creation_flags(0x08000000);
            if let Some(root) = std::env::var_os("SystemRoot") {
                command.env("SystemRoot", root);
            }
        }
        command
    }
    async fn cleanup(&self, name: &str, dir: &std::path::Path) -> bool {
        let mut command = self.command(dir);
        command
            .args(["--host", &self.config.endpoint, "rm", "--force", name])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let Ok(mut child) = command.spawn() else {
            return false;
        };
        let stderr = child.stderr.take().unwrap();
        match tokio::time::timeout(Duration::from_secs(5), async {
            tokio::try_join!(read_limited(stderr, 8192), async {
                child.wait().await.map_err(super::store::storage)
            })
        })
        .await
        {
            Ok(Ok((error, status))) => {
                status.success() || String::from_utf8_lossy(&error).contains("No such container")
            }
            _ => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                false
            }
        }
    }
    async fn validate_engine(&self, dir: &std::path::Path) -> Result<(), RuntimeError> {
        let mut command = self.command(dir);
        command
            .args([
                "--host",
                &self.config.endpoint,
                "info",
                "--format={{json .}}",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = command
            .spawn()
            .map_err(|_| config_error("cannot query configured Docker engine; no host fallback"))?;
        let stdout = child.stdout.take().unwrap();
        let output = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::try_join!(read_limited(stdout, 64 * 1024), async {
                child.wait().await.map_err(super::store::storage)
            })
        })
        .await;
        match output {
            Ok(Ok((bytes, status))) if status.success() => {
                let info: Value = serde_json::from_slice(&bytes)
                    .map_err(|_| config_error("invalid Docker engine capability response"))?;
                validate_engine_info(&info)
            }
            _ => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                Err(config_error(
                    "Docker Linux engine unavailable or capability query failed; no host fallback",
                ))
            }
        }
    }
    async fn run(&self, code: &str, context: &ToolContext) -> Result<SandboxOutcome, RuntimeError> {
        let spec = self.command_spec(code, context)?;
        let started = Instant::now();
        let dir = Arc::new(tempfile::tempdir().map_err(super::store::storage)?);
        self.validate_engine(dir.path()).await?;
        let mut cleanup = Cleanup {
            sandbox: self.clone(),
            name: spec.container_name.clone(),
            dir: dir.clone(),
            armed: true,
        };
        let mut command = self.command(dir.path());
        command
            .args(&spec.arguments)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command
            .spawn()
            .map_err(|_| config_error("cannot start configured Docker CLI; no host fallback"))?;
        let output = tokio::time::timeout(
            Duration::from_millis(self.config.timeout_ms + 10_000),
            exchange(
                &mut child,
                code.as_bytes(),
                self.config.max_output_bytes * 2 + 4096,
            ),
        )
        .await;
        let output = match output {
            Ok(Ok(output)) => Ok(output),
            error => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                Err(match error {
                    Ok(Err(error)) => error,
                    _ => config_error(
                        "Docker response deadline exceeded; inspect named container and supervisor",
                    ),
                })
            }
        };
        let removed = self.cleanup(&spec.container_name, dir.path()).await;
        cleanup.armed = !removed;
        let (written, stdout, stderr, status) = output?;
        check_exchange(&written, &stderr, status)?;
        let response: SupervisorOutput = serde_json::from_slice(&stdout)
            .map_err(|_| config_error("invalid isolated supervisor response"))?;
        if response.version != 1 {
            return Err(config_error("unsupported isolated supervisor contract"));
        }
        let decode = |value: &str| {
            base64::engine::general_purpose::STANDARD
                .decode(value)
                .map_err(|_| config_error("invalid supervisor output encoding"))
        };
        let stdout = decode(&response.stdout_base64)?;
        let stderr = decode(&response.stderr_base64)?;
        if stdout.len().saturating_add(stderr.len()) > self.config.max_output_bytes {
            return Err(config_error("supervisor violated output ceiling"));
        }
        Ok(SandboxOutcome {
            image: self.config.image.clone(),
            container_name: spec.container_name,
            exit_code: response.exit_code,
            stdout: String::from_utf8_lossy(&stdout).into_owned(),
            stderr: String::from_utf8_lossy(&stderr).into_owned(),
            timed_out: response.timed_out,
            output_limited: response.output_limited,
            cleanup_confirmed: removed,
            elapsed_ms: started.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
        })
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SupervisorOutput {
    version: u32,
    exit_code: i32,
    stdout_base64: String,
    stderr_base64: String,
    timed_out: bool,
    output_limited: bool,
}
impl CodeSandbox for DockerSandbox {
    fn identity(&self) -> String {
        self.binding.clone()
    }
    fn execute<'a>(
        &'a self,
        code: &'a str,
        context: &'a ToolContext,
    ) -> BoxFuture<'a, Result<SandboxOutcome, RuntimeError>> {
        Box::pin(self.run(code, context))
    }
}
struct Cleanup {
    sandbox: DockerSandbox,
    name: String,
    dir: Arc<tempfile::TempDir>,
    armed: bool,
}
impl Drop for Cleanup {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            let sandbox = self.sandbox.clone();
            let name = self.name.clone();
            let dir = self.dir.clone();
            runtime.spawn(async move {
                let _ = sandbox.cleanup(&name, dir.path()).await;
            });
        }
    }
}
pub struct SandboxTool(pub Arc<dyn CodeSandbox>);
impl Tool for SandboxTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {name:"python_sandbox".into(),description:format!("Execute explicitly approved Python in the configured isolated worker. No host files, network, packages or credentials are granted. Inspect exit_code, output and cleanup_confirmed. Binding {}",self.0.identity()),effect:ToolEffect::NonIdempotent,permissions:BTreeSet::from(["sandbox:python".into()]),input_schema:json!({"type":"object","properties":{"code":{"type":"string","minLength":1,"maxLength":65536}},"required":["code"],"additionalProperties":false})}
    }
    fn execute<'a>(
        &'a self,
        args: Value,
        context: ToolContext,
    ) -> BoxFuture<'a, Result<Value, ToolError>> {
        Box::pin(async move {
            let code = args["code"]
                .as_str()
                .ok_or_else(|| ToolError("missing Python code".into()))?;
            let outcome = self
                .0
                .execute(code, &context)
                .await
                .map_err(|e| ToolError(e.to_string()))?;
            serde_json::to_value(outcome).map_err(|e| ToolError(e.to_string()))
        })
    }
}
async fn read_limited(
    reader: impl tokio::io::AsyncRead + Unpin,
    max: usize,
) -> Result<Vec<u8>, RuntimeError> {
    let mut bytes = Vec::new();
    reader
        .take((max + 1) as u64)
        .read_to_end(&mut bytes)
        .await
        .map_err(super::store::storage)?;
    if bytes.len() > max {
        return Err(config_error("Docker output exceeded response limit"));
    }
    Ok(bytes)
}
type Exchange = (
    std::io::Result<()>,
    Vec<u8>,
    Vec<u8>,
    std::process::ExitStatus,
);
/// Sends code on stdin and collects stdout/stderr/exit status. A stdin write
/// failure is returned as data rather than failing the join: when Docker exits
/// before reading stdin, its exit status and stderr explain the failure, not
/// the resulting broken pipe.
async fn exchange(
    child: &mut tokio::process::Child,
    code: &[u8],
    max_stdout: usize,
) -> Result<Exchange, RuntimeError> {
    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    tokio::try_join!(
        async {
            let written = stdin.write_all(code).await;
            drop(stdin);
            Ok::<_, RuntimeError>(written)
        },
        read_limited(stdout, max_stdout),
        read_limited(stderr, 8192),
        async { child.wait().await.map_err(super::store::storage) }
    )
}
/// Prefers the process's own failure over a stdin write error.
fn check_exchange(
    written: &std::io::Result<()>,
    stderr: &[u8],
    status: std::process::ExitStatus,
) -> Result<(), RuntimeError> {
    if !status.success() {
        return Err(config_error(&format!(
            "Docker execution failed (no host fallback): {}",
            String::from_utf8_lossy(stderr)
                .chars()
                .take(1024)
                .collect::<String>()
        )));
    }
    if written.is_err() {
        return Err(config_error(
            "could not send code to the Docker supervisor (no host fallback)",
        ));
    }
    Ok(())
}
fn config_error(detail: &str) -> RuntimeError {
    RuntimeError::Configuration(format!("sandbox: {detail}"))
}
fn validate_engine_info(info: &Value) -> Result<(), RuntimeError> {
    if info["OSType"] != "linux"
        || ["MemoryLimit", "SwapLimit", "PidsLimit"]
            .iter()
            .any(|key| info[*key] != true)
        || info.get("CPUCfsQuota").or_else(|| info.get("CpuCfsQuota")) != Some(&Value::Bool(true))
        || !info["SecurityOptions"].as_array().is_some_and(|options| {
            options.iter().any(|v| v == "name=seccomp,profile=builtin")
                && options.iter().any(|v| v == "name=cgroupns")
        })
    {
        return Err(config_error("engine must support Linux memory/swap/PID/CPU limits, private cgroups and built-in seccomp"));
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn missing_engine_limits_and_unconfined_seccomp_fail_before_dispatch() {
        let supported = json!({"OSType":"linux","MemoryLimit":true,"SwapLimit":true,"PidsLimit":true,"CPUCfsQuota":true,"SecurityOptions":["name=seccomp,profile=builtin","name=cgroupns"]});
        validate_engine_info(&supported).unwrap();
        let mut newer = supported.clone();
        newer.as_object_mut().unwrap().remove("CPUCfsQuota");
        newer["CpuCfsQuota"] = json!(true);
        validate_engine_info(&newer).unwrap();
        for key in ["MemoryLimit", "SwapLimit", "PidsLimit", "CPUCfsQuota"] {
            let mut info = supported.clone();
            info[key] = json!(false);
            assert!(validate_engine_info(&info).is_err());
        }
        let mut info = supported.clone();
        info["SecurityOptions"] = json!(["name=seccomp,profile=unconfined", "name=cgroupns"]);
        assert!(validate_engine_info(&info).is_err());
        let mut info = supported;
        info["OSType"] = json!("windows");
        assert!(validate_engine_info(&info).is_err());
    }
    #[tokio::test]
    async fn process_failure_before_reading_stdin_wins_over_broken_pipe() {
        // Stands in for `docker run` failing before it reads any code.
        #[cfg(windows)]
        let mut command = {
            let mut command = tokio::process::Command::new("cmd");
            command.args(["/C", "echo docker: engine unavailable 1>&2 & exit /b 3"]);
            command
        };
        #[cfg(not(windows))]
        let mut command = {
            let mut command = tokio::process::Command::new("sh");
            command.args(["-c", "echo 'docker: engine unavailable' >&2; exit 3"]);
            command
        };
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        // Far larger than a pipe buffer, so the write cannot complete.
        let code = vec![b'#'; 4 * 1024 * 1024];
        let (written, _, stderr, status) =
            tokio::time::timeout(Duration::from_secs(30), exchange(&mut child, &code, 4096))
                .await
                .unwrap()
                .unwrap();
        assert!(written.is_err());
        let error = check_exchange(&written, &stderr, status)
            .unwrap_err()
            .to_string();
        assert!(error.contains("engine unavailable"), "{error}");
    }
    #[test]
    fn command_carries_code_size_and_digest_but_not_code() {
        let config = DockerSandboxConfig {
            executable: std::env::current_exe().unwrap(),
            endpoint: "unix:///var/run/docker.sock".into(),
            image: format!("sha256:{}", "a".repeat(64)),
            platform: "linux/amd64".into(),
            memory_mib: 128,
            cpu_millis: 500,
            timeout_ms: 1000,
            max_output_bytes: 4096,
        };
        let sandbox = DockerSandbox::new(config).unwrap();
        let context = ToolContext {
            run_id: "run".into(),
            call_id: "call".into(),
        };
        let code = "print('integrity')";
        let spec = sandbox.command_spec(code, &context).unwrap();
        let value = |flag: &str| {
            let at = spec.arguments.iter().position(|v| v == flag).unwrap();
            spec.arguments[at + 1].clone()
        };
        assert_eq!(value("--code-bytes"), code.len().to_string());
        assert_eq!(
            value("--code-sha256"),
            hex::encode(Sha256::digest(code.as_bytes()))
        );
        assert!(!spec.arguments.iter().any(|v| v.contains(code)));
    }
}
