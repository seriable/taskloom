use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;

use super::{invalid, Document, DocumentLoader, MAX_DOCUMENT_BYTES};
use crate::runtime::{
    store::{blocking, storage},
    RuntimeError,
};

/// An explicitly installed trusted Poppler pdftotext executable. The parser
/// runs as the host user; this is text extraction, not hostile-code isolation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PdfConfig {
    pub executable: PathBuf,
    pub extractor_id: String,
    pub max_input_bytes: usize,
    pub max_text_bytes: usize,
    pub max_pages: usize,
    pub timeout_ms: u64,
}
impl PdfConfig {
    pub fn validate(&self) -> Result<(), RuntimeError> {
        if !self.executable.is_absolute()
            || self.extractor_id.trim().is_empty()
            || self.extractor_id.len() > 1024
            || !(1..=MAX_DOCUMENT_BYTES).contains(&self.max_input_bytes)
            || !(1..=MAX_DOCUMENT_BYTES).contains(&self.max_text_bytes)
            || !(1..=2000).contains(&self.max_pages)
            || !(1..=60_000).contains(&self.timeout_ms)
        {
            return Err(invalid("invalid PDF extractor configuration"));
        }
        Ok(())
    }
}
#[derive(Debug, Clone)]
pub struct PdfLoader {
    config: PdfConfig,
}
impl PdfLoader {
    pub fn new(mut config: PdfConfig) -> Result<Self, RuntimeError> {
        config.validate()?;
        config.executable = std::fs::canonicalize(&config.executable).map_err(storage)?;
        if !config.executable.is_file() {
            return Err(invalid("PDF extractor must be a regular executable file"));
        }
        Ok(Self { config })
    }
    /// Uses a private, immutable input copy. The model/user cannot supply an
    /// executable or subprocess options. No shell, network fetch or OCR fallback.
    pub async fn load_bytes(
        &self,
        bytes: Vec<u8>,
        id: &str,
        title: &str,
        source: &str,
    ) -> Result<Document, RuntimeError> {
        if bytes.len() > self.config.max_input_bytes || !bytes.starts_with(b"%PDF-") {
            return Err(invalid(
                "PDF source exceeds byte limit or has no PDF header",
            ));
        }
        let hash = hex::encode(Sha256::digest(&bytes));
        let temp = blocking(move || {
            let temp = tempfile::tempdir().map_err(storage)?;
            let input = temp.path().join("source.pdf");
            std::fs::write(&input, bytes).map_err(storage)?;
            Ok(temp)
        })
        .await?;
        let mut command = tokio::process::Command::new(&self.config.executable);
        command
            .args(["-enc", "UTF-8", "-layout", "-f", "1", "-l"])
            .arg((self.config.max_pages + 1).to_string())
            .arg(temp.path().join("source.pdf"))
            .arg("-")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(windows)]
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW
        let mut child = command
            .spawn()
            .map_err(|_| invalid("cannot start configured PDF extractor"))?;
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        let output = tokio::time::timeout(Duration::from_millis(self.config.timeout_ms), async {
            tokio::try_join!(
                read_limited(stdout, self.config.max_text_bytes),
                read_limited(stderr, 16 * 1024),
                async { child.wait().await.map_err(storage) }
            )
        })
        .await;
        let (text, diagnostic, status) = match output {
            Ok(Ok(output)) => output,
            error => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                return Err(match error {
                    Ok(Err(error)) => error,
                    _ => invalid("PDF extraction exceeded deadline"),
                });
            }
        };
        if !status.success() {
            return Err(invalid(&format!(
                "PDF extraction failed: {}",
                String::from_utf8_lossy(&diagnostic)
                    .chars()
                    .take(1024)
                    .collect::<String>()
            )));
        }
        let text =
            String::from_utf8(text).map_err(|_| invalid("PDF extractor returned invalid UTF-8"))?;
        let pages = text.matches('\u{c}').count();
        if pages > self.config.max_pages {
            return Err(invalid("PDF exceeds page limit"));
        }
        if text.trim().is_empty() {
            return Err(invalid(
                "PDF has no extractable text; scanned PDFs require explicit OCR",
            ));
        }
        let document = Document {
            id: id.into(),
            title: title.into(),
            source: source.into(),
            text,
            metadata: BTreeMap::from([
                ("format".into(), "pdf".into()),
                ("source_sha256".into(), hash),
                ("extractor".into(), self.config.extractor_id.clone()),
                ("pages".into(), pages.to_string()),
                (
                    "offsets".into(),
                    "UTF-8 bytes in extracted text; form-feed separates PDF pages".into(),
                ),
            ]),
        };
        document.validate()?;
        Ok(document)
    }
}
impl DocumentLoader for PdfLoader {
    fn load<'a>(
        &'a self,
        path: &'a Path,
        id: &'a str,
        title: &'a str,
    ) -> BoxFuture<'a, Result<Document, RuntimeError>> {
        Box::pin(async move {
            let path = path.to_path_buf();
            let max = self.config.max_input_bytes;
            let (bytes, source) = blocking(move || {
                use std::io::Read;
                let path = std::fs::canonicalize(path).map_err(storage)?;
                let file = std::fs::File::open(&path).map_err(storage)?;
                if !file.metadata().map_err(storage)?.is_file() {
                    return Err(invalid("PDF source must be a regular file"));
                }
                let mut bytes = Vec::new();
                file.take((max + 1) as u64)
                    .read_to_end(&mut bytes)
                    .map_err(storage)?;
                if bytes.len() > max {
                    return Err(invalid("PDF source exceeds byte limit"));
                }
                Ok((bytes, path.to_string_lossy().into_owned()))
            })
            .await?;
            self.load_bytes(bytes, id, title, &source).await
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
        .map_err(storage)?;
    if bytes.len() > max {
        return Err(invalid("PDF extractor output exceeds byte limit"));
    }
    Ok(bytes)
}
