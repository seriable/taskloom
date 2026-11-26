//! Recorded and reused model answers.
//!
//! - **Recording** ([`Recording::Record`]): every raw answer is written under
//!   a directory.
//! - **Replay** ([`Recording::Replay`]): answers come only from that
//!   directory, nothing is sent, and a request without a recording fails
//!   ([`crate::TaskError::NotRecorded`]). Re-running deterministic code over
//!   the same model answers costs nothing.
//! - **Step cache** ([`Environment::set_step_cache_dir`],
//!   [`crate::TaskSpec::step_cache`]): a validated answer of a task that
//!   opted in is served again for the same request within the cache's
//!   maximum age, so a job resumed after a crash does not pay for the answers
//!   it already had.
//!
//! A request is identified by its label, model, protocol, attachment,
//! transport options, temperature, output limit, retry salt ([`crate::retry`]),
//! prompts, and the path, size and modification time of every image or file it sends: a re-asking
//! for another sample (another salt) or a file rewritten in place is another
//! request.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use sha2::{Digest, Sha256};

use crate::locale::Locale;
use crate::task::{Answer, Protocol};
use crate::transport::{Request, Response};

/// How long a step-cache entry is served by default.
pub const DEFAULT_STEP_CACHE_MAX_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// Whether answers are recorded or replayed.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Recording {
    #[default]
    Off,
    Record(PathBuf),
    Replay(PathBuf),
}

impl Recording {
    /// The mode named by `{prefix}_RECORDING` (`record` or `replay`) in the
    /// directory `{prefix}_RECORDING_DIR`; off when either is missing.
    pub fn from_env(prefix: &str) -> Self {
        let mode_var = format!("{prefix}_RECORDING");
        let dir_var = format!("{prefix}_RECORDING_DIR");
        let dir = std::env::var(&dir_var)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .map(PathBuf::from);
        let requested = std::env::var(&mode_var)
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        match (requested.as_str(), dir) {
            ("record", Some(dir)) => Self::Record(dir),
            ("replay", Some(dir)) => Self::Replay(dir),
            ("record" | "replay", None) => {
                tracing::warn!("{mode_var} is set without {dir_var}; recording is off");
                Self::Off
            }
            _ => Self::Off,
        }
    }
}

/// What every task run shares: the recording mode, the step cache and the
/// message language.
#[derive(Debug)]
pub struct Environment {
    recording: Recording,
    step_cache_dir: OnceLock<PathBuf>,
    step_cache_max_age: Duration,
    locale: Locale,
}

impl Default for Environment {
    fn default() -> Self {
        Self {
            recording: Recording::Off,
            step_cache_dir: OnceLock::new(),
            step_cache_max_age: DEFAULT_STEP_CACHE_MAX_AGE,
            locale: Locale::default(),
        }
    }
}

impl Environment {
    /// No recording, no step cache, English messages.
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_recording(mut self, recording: Recording) -> Self {
        self.recording = recording;
        self
    }

    pub fn with_locale(mut self, locale: Locale) -> Self {
        self.locale = locale;
        self
    }

    /// Where opted-in tasks keep reusable answers.
    pub fn with_step_cache_dir(self, dir: PathBuf) -> Self {
        self.set_step_cache_dir(dir);
        self
    }

    pub fn with_step_cache_max_age(mut self, age: Duration) -> Self {
        self.step_cache_max_age = age;
        self
    }

    /// Sets the step-cache directory once (it is often known only after
    /// start-up); returns whether this call set it.
    pub fn set_step_cache_dir(&self, dir: PathBuf) -> bool {
        self.step_cache_dir.set(dir).is_ok()
    }

    pub fn recording(&self) -> &Recording {
        &self.recording
    }

    pub fn locale(&self) -> Locale {
        self.locale
    }

    pub fn step_cache_dir(&self) -> Option<&Path> {
        self.step_cache_dir.get().map(PathBuf::as_path)
    }

    pub(crate) async fn lookup(&self, key: &RequestKey, step_cache: bool) -> Lookup {
        if let Recording::Replay(dir) = &self.recording {
            return match read(&key.file_in(dir), &key.model).await {
                Some(answer) => Lookup::Found(answer),
                None => Lookup::MissingInReplay,
            };
        }
        if step_cache {
            if let Some(dir) = self.step_cache_dir.get() {
                let file = key.file_in(dir);
                let max_age = self.step_cache_max_age;
                let fresh = tokio::fs::metadata(&file)
                    .await
                    .ok()
                    .and_then(|metadata| metadata.modified().ok())
                    .and_then(|modified| modified.elapsed().ok())
                    .is_some_and(|age| age < max_age);
                if fresh {
                    if let Some(answer) = read(&file, &key.model).await {
                        return Lookup::Found(answer);
                    }
                }
            }
        }
        Lookup::Ask
    }

    /// Writes the raw answer, with the request it answers, in recording
    /// mode (evaluations re-run recorded requests: [`crate::eval`]).
    pub(crate) async fn record(
        &self,
        key: &RequestKey,
        response: &Response,
        request: &Request<'_>,
    ) -> Result<(), crate::TaskError> {
        if let Recording::Record(dir) = &self.recording {
            let recorded = RecordedRequest::of(request);
            let file = key.file_in(dir);
            write_record(
                &file,
                &response.answer,
                Some(&recorded),
                Some(&response.model),
            )
            .await
            .map_err(|source| crate::TaskError::Storage { path: file, source })?;
        }
        Ok(())
    }

    /// Writes a raw answer that has no chat request (embeddings).
    pub(crate) async fn record_payload(
        &self,
        key: &RequestKey,
        answer: &Answer,
    ) -> Result<(), crate::TaskError> {
        if let Recording::Record(dir) = &self.recording {
            let file = key.file_in(dir);
            write_record(&file, answer, None, None)
                .await
                .map_err(|source| crate::TaskError::Storage { path: file, source })?;
        }
        Ok(())
    }

    /// Keeps a validated answer for the step cache.
    pub(crate) async fn cache(&self, key: &RequestKey, answer: &Answer) {
        if let Some(dir) = self.step_cache_dir.get() {
            write(&key.file_in(dir), answer).await;
        }
    }

    pub(crate) async fn cache_response(&self, key: &RequestKey, response: &Response) {
        if let Some(dir) = self.step_cache_dir.get() {
            let file = key.file_in(dir);
            let result = write_record(&file, &response.answer, None, Some(&response.model)).await;
            warn_on_store_error(&file, result);
        }
    }

    /// Writes `answer` as the recording (or cache entry) of a request under
    /// `dir` — for tests that prepare a replay directory or a warm cache.
    pub async fn write_answer(dir: &Path, key: &RequestKey, answer: &Answer) {
        write(&key.file_in(dir), answer).await;
    }

    /// Removes step-cache entries older than the maximum age.
    pub fn prune_step_cache(&self) {
        let Some(dir) = self.step_cache_dir.get() else {
            return;
        };
        let Ok(folders) = std::fs::read_dir(dir) else {
            return;
        };
        for folder in folders.flatten() {
            let Ok(files) = std::fs::read_dir(folder.path()) else {
                continue;
            };
            for file in files.flatten() {
                // The persistent directory lock must not be unlinked while
                // another process can still hold it.
                if file
                    .path()
                    .extension()
                    .is_none_or(|extension| extension != "json")
                {
                    continue;
                }
                let stale = file
                    .metadata()
                    .ok()
                    .and_then(|metadata| metadata.modified().ok())
                    .and_then(|modified| modified.elapsed().ok())
                    .is_some_and(|age| age >= self.step_cache_max_age);
                if stale {
                    let _ = std::fs::remove_file(file.path());
                }
            }
        }
    }
}

/// The identity of one request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestKey {
    label: String,
    model: String,
    hash: String,
}

impl RequestKey {
    /// Isolates an endpoint or transport configuration in a shared store.
    /// The namespace length is encoded before hashing.
    pub fn in_namespace(mut self, namespace: &str) -> Self {
        let mut hasher = Sha256::new();
        hasher.update((namespace.len() as u64).to_le_bytes());
        hasher.update(namespace.as_bytes());
        hasher.update(self.hash.as_bytes());
        self.hash = hex::encode(hasher.finalize());
        self
    }

    /// The key of `request` sent under `protocol` (and the current retry
    /// salt). The phase is not part of it: a repair request differs by its
    /// prompt.
    pub fn for_request(request: &Request<'_>, protocol: Protocol) -> Self {
        let mut hasher = Sha256::new();
        let schema = request.schema.map(|schema| schema.to_string());
        for part in [
            request.label,
            request.model,
            &format!(
                "{protocol:?}|{:?}|{:?}",
                request.attachment, request.options
            ),
            &schema.unwrap_or_default(),
            &format!(
                "{:?}|{}|{}",
                request.format,
                request.temperature.to_bits(),
                request.max_output_tokens
            ),
            &crate::retry::current_retry_salt().to_string(),
            request.system_prompt,
            request.user_prompt,
        ] {
            hasher.update(part.as_bytes());
            hasher.update([0]);
        }
        // Files by path plus size and modification time: a frame or clip
        // rewritten in place under the same name is another request.
        let file = request.attachment.map(|attachment| &attachment.path);
        for path in request.images.iter().chain(file) {
            hasher.update(path.to_string_lossy().as_bytes());
            if let Ok(metadata) = std::fs::metadata(path) {
                hasher.update(metadata.len().to_le_bytes());
                if let Ok(modified) = metadata.modified().map(|time| {
                    time.duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                }) {
                    hasher.update(modified.as_nanos().to_le_bytes());
                }
            }
            hasher.update([0]);
        }
        Self {
            label: request.label.to_string(),
            model: request.model.to_string(),
            hash: hex::encode(hasher.finalize()),
        }
    }

    /// The key of an arbitrary payload under `label` (embeddings).
    pub fn for_payload(label: &str, model: &str, kind: &str, payload: &str) -> Self {
        let mut hasher = Sha256::new();
        for part in [label, model, kind, payload] {
            hasher.update(part.as_bytes());
            hasher.update([0]);
        }
        Self {
            label: label.to_string(),
            model: model.to_string(),
            hash: hex::encode(hasher.finalize()),
        }
    }

    pub fn describe(&self) -> String {
        format!("{}#{}", self.label, &self.hash[..16])
    }

    fn file_in(&self, dir: &Path) -> PathBuf {
        let folder = self
            .label
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
                    character
                } else {
                    '_'
                }
            })
            .collect::<String>();
        dir.join(
            if folder.is_empty() || matches!(folder.as_str(), "." | "..") {
                "_".to_string()
            } else {
                folder
            },
        )
        .join(format!("{}.json", self.hash))
    }
}

pub(crate) enum Lookup {
    Found(Response),
    MissingInReplay,
    Ask,
}

async fn read(file: &Path, requested_model: &str) -> Option<Response> {
    let bytes = tokio::fs::read(file).await.ok()?;
    let record = serde_json::from_slice::<serde_json::Value>(&bytes).ok()?;
    Some(Response {
        answer: Answer::from_record(&record)?,
        model: record
            .get("served_model")
            .and_then(serde_json::Value::as_str)
            .unwrap_or(requested_model)
            .to_string(),
    })
}

/// The request a recording answers.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RecordedRequest {
    pub label: String,
    pub model: String,
    pub system_prompt: String,
    pub user_prompt: String,
    #[serde(default)]
    pub images: Vec<PathBuf>,
    /// `json` or `text`.
    pub format: String,
    #[serde(default)]
    pub temperature: f32,
    #[serde(default)]
    pub max_output_tokens: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<serde_json::Value>,
}

impl RecordedRequest {
    fn of(request: &Request<'_>) -> Self {
        Self {
            label: request.label.to_string(),
            model: request.model.to_string(),
            system_prompt: request.system_prompt.to_string(),
            user_prompt: request.user_prompt.to_string(),
            images: request.images.to_vec(),
            format: match request.format {
                crate::transport::Format::Json => "json",
                crate::transport::Format::Text => "text",
            }
            .to_string(),
            temperature: request.temperature,
            max_output_tokens: request.max_output_tokens,
            schema: request.schema.cloned(),
        }
    }
}

async fn write(file: &Path, answer: &Answer) {
    warn_on_store_error(file, write_record(file, answer, None, None).await);
}

fn warn_on_store_error(file: &Path, result: std::io::Result<()>) {
    if let Err(error) = result {
        tracing::warn!(path = %file.display(), %error, "agent answer store write failed");
    }
}

/// Writes through a temporary file, so a reader never sees half an answer.
async fn write_record(
    file: &Path,
    answer: &Answer,
    request: Option<&RecordedRequest>,
    served_model: Option<&str>,
) -> std::io::Result<()> {
    let parent = file.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "recording has no parent directory",
        )
    })?;
    tokio::fs::create_dir_all(parent).await?;
    let mut record = answer.to_record();
    if let Some(model) = served_model {
        record["served_model"] = model.into();
    }
    if let Some(request) = request {
        record["request"] = serde_json::to_value(request).unwrap_or_default();
    }
    let file = file.to_path_buf();
    let bytes = record.to_string().into_bytes();
    // Each writer owns a unique file. `persist` atomically replaces the
    // destination on Unix and Windows; readers see an old or a complete new
    // entry. Blocking disk flush/replacement does not block Tokio workers.
    tokio::task::spawn_blocking(move || {
        // Keep the lock file in place: deleting it would allow another
        // process to lock a different inode for the same directory. Writers
        // are serialized per task label, avoiding Windows replacement races.
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(file.parent().unwrap().join(".taskloom-store.lock"))?;
        fs2::FileExt::lock_exclusive(&lock)?;
        let mut temporary = tempfile::NamedTempFile::new_in(file.parent().unwrap())?;
        temporary.write_all(&bytes)?;
        temporary.as_file().sync_all()?;
        for retry in 0..10 {
            match temporary.persist(&file) {
                Ok(_) => return Ok(()),
                Err(error) => {
                    // Windows can temporarily deny replacement while another
                    // writer or a scanner holds the destination. Keep our
                    // complete unique file and retry; never delete the target.
                    let retryable =
                        cfg!(windows) && matches!(error.error.raw_os_error(), Some(5 | 32 | 33));
                    if !retryable || retry == 9 {
                        return Err(error.error);
                    }
                    temporary = error.file;
                    std::thread::sleep(Duration::from_millis(1 << retry));
                }
            }
        }
        unreachable!()
    })
    .await
    .map_err(std::io::Error::other)?
}
