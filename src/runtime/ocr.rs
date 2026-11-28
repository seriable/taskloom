//! Explicit PNG bytes -> isolated Tesseract -> extracted UTF-8 snapshot.
//! No model-facing tool, server path input or host native OCR fallback.
use super::{
    rag::Document,
    sandbox::{DockerSandboxConfig, SandboxCommand},
    state::fingerprint,
    RuntimeError, ToolContext,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::Path,
    process::Stdio,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OcrLanguage {
    English,
    SimplifiedChinese,
    SimplifiedChineseEnglish,
}
impl OcrLanguage {
    fn argument(self) -> &'static str {
        match self {
            Self::English => "eng",
            Self::SimplifiedChinese => "chi_sim",
            Self::SimplifiedChineseEnglish => "chi_sim+eng",
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DockerOcrConfig {
    pub execution: DockerSandboxConfig,
    pub language: OcrLanguage,
    pub max_image_bytes: usize,
    pub max_pixels: u64,
    pub max_text_bytes: usize,
}
impl DockerOcrConfig {
    pub fn validate(&self) -> Result<(), RuntimeError> {
        self.execution.validate()?;
        if !(1..=4 * 1024 * 1024).contains(&self.max_image_bytes)
            || !(1..=16 * 1024 * 1024).contains(&self.max_pixels)
            || self.max_text_bytes == 0
            || self.max_text_bytes > self.execution.max_output_bytes
        {
            return Err(error("invalid OCR image/pixel/text limits"));
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OcrProvenance {
    pub version: u32,
    pub source_image_sha256: String,
    pub source_image_bytes: usize,
    pub width: u32,
    pub height: u32,
    pub worker_image: String,
    pub platform: String,
    pub language: OcrLanguage,
    pub manifest_sha256: String,
    pub tesseract_package: String,
    pub traineddata_sha256: BTreeMap<String, String>,
    pub text_sha256: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OcrOutcome {
    pub container_name: String,
    pub worker_binding: String,
    pub worker_image: String,
    pub exit_code: i32,
    pub text: Option<String>,
    pub stderr: String,
    pub timed_out: bool,
    pub output_limited: bool,
    pub cleanup_confirmed: bool,
    pub elapsed_ms: u64,
    pub provenance: Option<OcrProvenance>,
}
impl OcrOutcome {
    /// Offsets/citations describe this extracted UTF-8 snapshot, never image
    /// pixels or verified OCR truth. Failure/partial/unknown cleanup is refused.
    pub fn into_document(
        self,
        id: impl Into<String>,
        title: impl Into<String>,
    ) -> Result<Document, RuntimeError> {
        if self.exit_code != 0 || self.timed_out || self.output_limited || !self.cleanup_confirmed {
            return Err(error(
                "OCR was incomplete or cleanup unconfirmed; no document",
            ));
        }
        let text = self.text.ok_or_else(|| error("OCR text unavailable"))?;
        let provenance = self
            .provenance
            .ok_or_else(|| error("OCR provenance unavailable"))?;
        if text.trim().is_empty() || hash(text.as_bytes()) != provenance.text_sha256 {
            return Err(error("OCR text empty or provenance changed"));
        }
        let source = format!(
            "ocr:taskloom-png-v1:{}:{}",
            provenance.source_image_sha256,
            fingerprint(&provenance)?
        );
        let document = Document {
            id: id.into(),
            title: title.into(),
            source,
            text,
            metadata: BTreeMap::from([
                ("format".into(), "ocr-png-text-snapshot-v1".into()),
                (
                    "ocr_provenance".into(),
                    serde_json::to_string(&provenance).map_err(super::store::storage)?,
                ),
            ]),
        };
        document.validate()?;
        Ok(document)
    }
}
#[derive(Clone)]
pub struct DockerOcr {
    config: DockerOcrConfig,
    binding: String,
}
impl DockerOcr {
    pub fn new(mut config: DockerOcrConfig) -> Result<Self, RuntimeError> {
        config.validate()?;
        config.execution.executable =
            std::fs::canonicalize(&config.execution.executable).map_err(super::store::storage)?;
        if !config.execution.executable.is_file() {
            return Err(error("Docker executable must be regular"));
        }
        let binding = format!("docker-ocr-png-supervisor-v1:{}", fingerprint(&config)?);
        Ok(Self { config, binding })
    }
    pub fn identity(&self) -> &str {
        &self.binding
    }
    /// Pure input/argv review. Source bytes never appear in shell or argv.
    pub fn command_spec(
        &self,
        image: &[u8],
        context: &ToolContext,
    ) -> Result<SandboxCommand, RuntimeError> {
        png(image, self.config.max_image_bytes, self.config.max_pixels)?;
        if context.run_id.is_empty()
            || context.run_id.len() > 1024
            || context.call_id.is_empty()
            || context.call_id.len() > 1024
        {
            return Err(error("invalid OCR context"));
        }
        let name = format!(
            "taskloom-ocr-{}",
            fingerprint(&(
                &self.binding,
                &context.run_id,
                &context.call_id,
                hash(image),
                uuid::Uuid::new_v4().simple().to_string()
            ))?
        );
        let cfg = &self.config.execution;
        let memory = format!("{}m", cfg.memory_mib);
        let cpu = format!("{}.{:03}", cfg.cpu_millis / 1000, cfg.cpu_millis % 1000);
        let arguments = vec![
            "--host".into(),
            cfg.endpoint.clone(),
            "run".into(),
            "--rm".into(),
            "--pull=never".into(),
            "--init=false".into(),
            "--name".into(),
            name.clone(),
            "--platform".into(),
            cfg.platform.clone(),
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
            "--ulimit=fsize=4194304:4194304".into(),
            "--ulimit=core=0:0".into(),
            "--tmpfs=/tmp:rw,nosuid,nodev,noexec,size=16m,uid=0,gid=0,mode=0711".into(),
            "--workdir=/tmp".into(),
            "--ipc=none".into(),
            "--cgroupns=private".into(),
            "--log-driver=none".into(),
            "--label=org.taskloom.sandbox=ocr-png-supervisor-v1".into(),
            format!(
                "--label=org.taskloom.ocr.request={}",
                fingerprint(&(
                    &self.binding,
                    &context.run_id,
                    &context.call_id,
                    hash(image)
                ))?
            ),
            "--interactive".into(),
            "--entrypoint=/usr/bin/python3".into(),
            cfg.image.clone(),
            "-I".into(),
            "-B".into(),
            "/opt/taskloom/ocr-supervisor.py".into(),
            "--timeout-ms".into(),
            cfg.timeout_ms.to_string(),
            "--max-output-bytes".into(),
            cfg.max_output_bytes.to_string(),
            "--max-text-bytes".into(),
            self.config.max_text_bytes.to_string(),
            "--png-bytes".into(),
            image.len().to_string(),
            "--png-sha256".into(),
            hash(image),
            "--max-pixels".into(),
            self.config.max_pixels.to_string(),
            "--language".into(),
            self.config.language.argument().into(),
        ];
        Ok(SandboxCommand {
            program: cfg.executable.clone(),
            arguments,
            container_name: name,
        })
    }
    fn command(&self, dir: &Path) -> tokio::process::Command {
        let mut command = tokio::process::Command::new(&self.config.execution.executable);
        command
            .env_clear()
            .env("DOCKER_CONFIG", dir)
            .env("HOME", dir)
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
    async fn control(
        &self,
        dir: &Path,
        args: &[&str],
    ) -> Result<(std::process::ExitStatus, Vec<u8>, Vec<u8>), RuntimeError> {
        let mut child = self
            .command(dir)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|_| error("cannot start Docker control command; no host fallback"))?;
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        match tokio::time::timeout(Duration::from_secs(5), async {
            tokio::try_join!(child.wait(), read(stdout, 65536), read(stderr, 8192))
        })
        .await
        {
            Ok(Ok((status, out, err))) => Ok((status, out, err)),
            _ => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                Err(error(
                    "Docker control unavailable; cleanup/capabilities unknown",
                ))
            }
        }
    }
    async fn cleanup(&self, name: &str, dir: &Path) -> bool {
        let endpoint = &self.config.execution.endpoint;
        let Ok((status, _, stderr)) = self
            .control(dir, &["--host", endpoint, "rm", "--force", name])
            .await
        else {
            return false;
        };
        if !status.success() && !String::from_utf8_lossy(&stderr).contains("No such container") {
            return false;
        }
        let Ok((status, _, stderr)) = self
            .control(
                dir,
                &["--host", endpoint, "inspect", "--type=container", name],
            )
            .await
        else {
            return false;
        };
        !status.success()
            && (String::from_utf8_lossy(&stderr).contains("No such object")
                || String::from_utf8_lossy(&stderr).contains("No such container"))
    }
    async fn engine(&self, dir: &Path) -> Result<(), RuntimeError> {
        let (status, out, _) = self
            .control(
                dir,
                &[
                    "--host",
                    &self.config.execution.endpoint,
                    "info",
                    "--format={{json .}}",
                ],
            )
            .await?;
        let info: serde_json::Value = serde_json::from_slice(&out)
            .map_err(|_| error("invalid Docker capability response"))?;
        if !status.success()
            || info["OSType"] != "linux"
            || ["MemoryLimit", "SwapLimit", "PidsLimit"]
                .iter()
                .any(|key| info[*key] != true)
            || info.get("CPUCfsQuota").or_else(|| info.get("CpuCfsQuota"))
                != Some(&serde_json::Value::Bool(true))
            || !info["SecurityOptions"].as_array().is_some_and(|v| {
                v.iter().any(|s| s == "name=seccomp,profile=builtin")
                    && v.iter().any(|s| s == "name=cgroupns")
            })
        {
            return Err(error("verified Linux memory/swap/PID/CPU/private cgroup/seccomp required; no host fallback"));
        }
        Ok(())
    }
    pub async fn extract(
        &self,
        image: &[u8],
        context: &ToolContext,
    ) -> Result<OcrOutcome, RuntimeError> {
        let spec = self.command_spec(image, context)?;
        let dimensions = png(image, self.config.max_image_bytes, self.config.max_pixels)?;
        let started = Instant::now();
        let dir = Arc::new(tempfile::tempdir().map_err(super::store::storage)?);
        self.engine(dir.path()).await?;
        let mut cleanup = Cleanup {
            worker: self.clone(),
            name: spec.container_name.clone(),
            dir: dir.clone(),
            armed: true,
        };
        let mut child = self
            .command(dir.path())
            .args(&spec.arguments)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|_| error("cannot start pinned OCR image; no host fallback"))?;
        let mut stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        let exchange = tokio::time::timeout(
            Duration::from_millis(self.config.execution.timeout_ms + 10000),
            async {
                tokio::try_join!(
                    async {
                        let result = stdin.write_all(image).await;
                        drop(stdin);
                        Ok::<_, std::io::Error>(result)
                    },
                    read(stdout, self.config.execution.max_output_bytes * 2 + 32768),
                    read(stderr, 8192),
                    child.wait()
                )
            },
        )
        .await;
        let result = match exchange {
            Ok(Ok(value)) => Ok(value),
            _ => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                Err(error(&format!(
                    "OCR exchange failed/deadline; named container {}; outcome unknown",
                    spec.container_name
                )))
            }
        };
        let removed = self.cleanup(&spec.container_name, dir.path()).await;
        cleanup.armed = !removed;
        let (written, raw, stderr, status) = result?;
        if !status.success() || written.is_err() {
            return Err(error(&format!("OCR Docker failed; container {}; cleanup_confirmed={removed}; no host fallback: {}",spec.container_name,String::from_utf8_lossy(&stderr).chars().take(1024).collect::<String>())));
        }
        let audit_name = spec.container_name.clone();
        let outcome = (|| -> Result<OcrOutcome, RuntimeError> {
            let wire: Wire = serde_json::from_slice(&raw).map_err(|_| {
                error(&format!(
                    "invalid OCR supervisor contract; container {}; cleanup_confirmed={removed}",
                    spec.container_name
                ))
            })?;
            if wire.version != 1 {
                return Err(error("unsupported OCR supervisor contract"));
            }
            let text = STANDARD
                .decode(wire.text_base64)
                .map_err(|_| error("invalid OCR text encoding"))?;
            let stderr = STANDARD
                .decode(wire.stderr_base64)
                .map_err(|_| error("invalid OCR error encoding"))?;
            if text.len() > self.config.max_text_bytes
                || stderr.len() > 4096
                || text.len().saturating_add(stderr.len()) > self.config.execution.max_output_bytes
            {
                return Err(error("OCR output ceiling violated"));
            }
            let success = wire.exit_code == 0 && !wire.timed_out && !wire.output_limited;
            let provenance = if success {
                let p = wire
                    .provenance
                    .ok_or_else(|| error("successful OCR has no provenance"))?;
                if p.image_sha256 != hash(image)
                    || p.image_bytes != image.len()
                    || (p.width, p.height) != dimensions
                    || p.language != self.config.language.argument()
                    || p.manifest.version != 1
                    || p.manifest.tesseract_package.is_empty()
                    || p.manifest.tesseract_package.len() > 128
                    || p.manifest
                        .traineddata_sha256
                        .keys()
                        .map(String::as_str)
                        .collect::<Vec<_>>()
                        != vec!["chi_sim", "eng"]
                    || p.manifest
                        .traineddata_sha256
                        .values()
                        .any(|v| !valid_hash(v))
                    || !valid_hash(&p.manifest_sha256)
                    || p.text_sha256.as_deref() != Some(hash(&text).as_str())
                {
                    return Err(error("OCR provenance/source/model contract mismatch"));
                }
                Some(OcrProvenance {
                    version: 1,
                    source_image_sha256: p.image_sha256,
                    source_image_bytes: p.image_bytes,
                    width: p.width,
                    height: p.height,
                    worker_image: self.config.execution.image.clone(),
                    platform: self.config.execution.platform.clone(),
                    language: self.config.language,
                    manifest_sha256: p.manifest_sha256,
                    tesseract_package: p.manifest.tesseract_package,
                    traineddata_sha256: p.manifest.traineddata_sha256,
                    text_sha256: hash(&text),
                })
            } else {
                None
            };
            Ok(OcrOutcome {
                container_name: spec.container_name,
                worker_binding: self.binding.clone(),
                worker_image: self.config.execution.image.clone(),
                exit_code: wire.exit_code,
                text: if success {
                    Some(String::from_utf8(text).map_err(|_| error("OCR text not UTF-8"))?)
                } else {
                    None
                },
                stderr: String::from_utf8_lossy(&stderr).into_owned(),
                timed_out: wire.timed_out,
                output_limited: wire.output_limited,
                cleanup_confirmed: removed,
                elapsed_ms: started.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
                provenance,
            })
        })();
        outcome.map_err(|failure| {
            error(&format!(
                "{failure}; container {audit_name}; cleanup_confirmed={removed}"
            ))
        })
    }
}
struct Cleanup {
    worker: DockerOcr,
    name: String,
    dir: Arc<tempfile::TempDir>,
    armed: bool,
}
impl Drop for Cleanup {
    fn drop(&mut self) {
        if self.armed {
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                let worker = self.worker.clone();
                let name = self.name.clone();
                let dir = self.dir.clone();
                runtime.spawn(async move {
                    let _ = worker.cleanup(&name, dir.path()).await;
                });
            }
        }
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Wire {
    version: u32,
    exit_code: i32,
    text_base64: String,
    stderr_base64: String,
    timed_out: bool,
    output_limited: bool,
    provenance: Option<WireProvenance>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireProvenance {
    image_sha256: String,
    image_bytes: usize,
    width: u32,
    height: u32,
    manifest_sha256: String,
    manifest: Manifest,
    language: String,
    text_sha256: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    version: u32,
    tesseract_package: String,
    traineddata_sha256: BTreeMap<String, String>,
}
async fn read(reader: impl tokio::io::AsyncRead + Unpin, limit: usize) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .await?;
    if bytes.len() > limit {
        return Err(std::io::Error::other("OCR response exceeded bound"));
    }
    Ok(bytes)
}
fn hash(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
fn valid_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn error(message: &str) -> RuntimeError {
    RuntimeError::Configuration(format!("OCR: {message}"))
}
fn crc(bytes: &[u8]) -> u32 {
    let mut value = !0u32;
    for byte in bytes {
        value ^= u32::from(*byte);
        for _ in 0..8 {
            value = (value >> 1) ^ (0xedb88320u32.wrapping_mul(value & 1));
        }
    }
    !value
}
fn png(bytes: &[u8], max_bytes: usize, max_pixels: u64) -> Result<(u32, u32), RuntimeError> {
    if bytes.len() > max_bytes || !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Err(error("bounded PNG required; no dispatch"));
    }
    let mut at = 8usize;
    let mut dimensions = None;
    let mut data = false;
    let mut ended = false;
    while at < bytes.len() {
        let header = bytes
            .get(at..at + 8)
            .ok_or_else(|| error("truncated PNG header; no dispatch"))?;
        let length = u32::from_be_bytes(header[..4].try_into().unwrap()) as usize;
        let end = at
            .checked_add(length)
            .and_then(|v| v.checked_add(12))
            .filter(|end| *end <= bytes.len())
            .ok_or_else(|| error("truncated PNG chunk; no dispatch"))?;
        let kind = &header[4..8];
        let content = &bytes[at + 8..end - 4];
        if !kind.iter().all(u8::is_ascii_alphabetic)
            || !kind[2].is_ascii_uppercase()
            || crc(&bytes[at + 4..end - 4])
                != u32::from_be_bytes(bytes[end - 4..end].try_into().unwrap())
        {
            return Err(error("PNG type/CRC invalid; no dispatch"));
        }
        if dimensions.is_none() {
            if kind != b"IHDR" || length != 13 {
                return Err(error("PNG IHDR required; no dispatch"));
            }
            let width = u32::from_be_bytes(content[..4].try_into().unwrap());
            let height = u32::from_be_bytes(content[4..8].try_into().unwrap());
            let depth = content[8];
            let color = content[9];
            let valid = match color {
                0 => matches!(depth, 1 | 2 | 4 | 8 | 16),
                2 | 4 | 6 => matches!(depth, 8 | 16),
                3 => matches!(depth, 1 | 2 | 4 | 8),
                _ => false,
            };
            if width == 0
                || height == 0
                || width > 16384
                || height > 16384
                || u64::from(width) * u64::from(height) > max_pixels
                || !valid
                || content[10] != 0
                || content[11] != 0
                || content[12] > 1
            {
                return Err(error("PNG pixel/format limits exceeded; no dispatch"));
            }
            dimensions = Some((width, height));
        } else if kind == b"IHDR" || matches!(kind, b"acTL" | b"fcTL" | b"fdAT") {
            return Err(error("multiple/animated PNG refused; no dispatch"));
        } else if kind == b"IDAT" {
            if ended {
                return Err(error("nonconsecutive PNG data; no dispatch"));
            }
            data = true;
        } else if kind == b"IEND" {
            if length != 0 || end != bytes.len() || !data {
                return Err(error("invalid PNG end; no dispatch"));
            }
            return dimensions.ok_or_else(|| error("PNG dimensions missing"));
        } else {
            if data {
                ended = true;
            }
            if kind[0].is_ascii_uppercase() && kind != b"PLTE" {
                return Err(error("unknown critical PNG chunk; no dispatch"));
            }
        }
        at = end;
    }
    Err(error("PNG end missing; no dispatch"))
}
