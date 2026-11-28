//! Validated bounded UTF-8 structured source loaders that preserve raw quote bytes.
use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;

use futures_util::future::BoxFuture;
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::{invalid, Document, DocumentLoader, MAX_DOCUMENT_BYTES};
use crate::runtime::RuntimeError;

/// Host limits apply before parsing and before any corpus update. No loader
/// executes cells, mail content, URLs, attachments, formulas or source commands.
#[derive(Debug, Clone, Copy)]
pub struct StructuredTextLimits {
    pub max_bytes: usize,
    pub max_records: usize,
    pub max_fields: usize,
}
impl Default for StructuredTextLimits {
    fn default() -> Self {
        Self {
            max_bytes: MAX_DOCUMENT_BYTES,
            max_records: 100_000,
            max_fields: 1024,
        }
    }
}
impl StructuredTextLimits {
    fn validate(self) -> Result<Self, RuntimeError> {
        if !(1..=MAX_DOCUMENT_BYTES).contains(&self.max_bytes)
            || !(1..=100_000).contains(&self.max_records)
            || !(1..=4096).contains(&self.max_fields)
        {
            Err(invalid("invalid structured source limits"))
        } else {
            Ok(self)
        }
    }
}
#[derive(Debug, Clone, Copy, Default)]
pub struct CsvLoader {
    pub limits: StructuredTextLimits,
}
#[derive(Debug, Clone, Copy, Default)]
pub struct JsonlLoader {
    pub limits: StructuredTextLimits,
}
#[derive(Debug, Clone, Copy, Default)]
pub struct EmlLoader {
    pub limits: StructuredTextLimits,
}
#[derive(Clone, Copy)]
enum Format {
    Csv,
    Jsonl,
    Eml,
}
macro_rules! loader {
    ($loader:ident,$format:ident) => {
        impl DocumentLoader for $loader {
            fn load<'a>(
                &'a self,
                path: &'a Path,
                id: &'a str,
                title: &'a str,
            ) -> BoxFuture<'a, Result<Document, RuntimeError>> {
                Box::pin(async move {
                    let path = path.to_path_buf();
                    let id = id.to_owned();
                    let title = title.to_owned();
                    let limits = self.limits.validate()?;
                    crate::runtime::store::blocking(move || {
                        load(&path, id, title, limits, Format::$format)
                    })
                    .await
                })
            }
        }
    };
}
loader!(CsvLoader, Csv);
loader!(JsonlLoader, Jsonl);
loader!(EmlLoader, Eml);
fn load(
    path: &Path,
    id: String,
    title: String,
    limits: StructuredTextLimits,
    format: Format,
) -> Result<Document, RuntimeError> {
    let path =
        std::fs::canonicalize(path).map_err(|_| invalid("structured source path unavailable"))?;
    let file = std::fs::File::open(&path).map_err(|_| invalid("cannot open structured source"))?;
    if !file
        .metadata()
        .map_err(|_| invalid("cannot inspect structured source"))?
        .is_file()
    {
        return Err(invalid("structured source is not a regular file"));
    }
    let mut bytes = Vec::new();
    file.take((limits.max_bytes + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| invalid("cannot read structured source"))?;
    if bytes.len() > limits.max_bytes {
        return Err(invalid("structured source exceeds byte limit"));
    }
    let sha = hex::encode(Sha256::digest(&bytes));
    let text = String::from_utf8(bytes).map_err(|_| invalid("structured source is not UTF-8"))?;
    let mut metadata = BTreeMap::from([("source_sha256".into(), sha)]);
    match format {
        Format::Csv => {
            let (records, fields) = csv(&text, limits)?;
            metadata.insert("format".into(), "csv-utf8-v1".into());
            metadata.insert("records".into(), records.to_string());
            metadata.insert("fields".into(), fields.to_string());
        }
        Format::Jsonl => {
            let records = jsonl(&text, limits)?;
            metadata.insert("format".into(), "jsonl-objects-utf8-v1".into());
            metadata.insert("records".into(), records.to_string());
        }
        Format::Eml => {
            eml(&text, limits)?;
            metadata.insert("format".into(), "eml-plain-utf8-v1".into());
        }
    }
    let document = Document {
        id,
        title,
        source: path.to_string_lossy().into_owned(),
        text,
        metadata,
    };
    document.validate()?;
    Ok(document)
}
fn csv(text: &str, limits: StructuredTextLimits) -> Result<(usize, usize), RuntimeError> {
    if text.is_empty() || text.contains('\0') {
        return Err(invalid("empty or NUL CSV input"));
    }
    let bytes = text.as_bytes();
    let mut at = 0;
    let mut records = 0;
    let mut expected = None;
    while at < bytes.len() {
        let mut fields = 0;
        loop {
            fields += 1;
            if fields > limits.max_fields {
                return Err(invalid("CSV field count exceeds limit"));
            }
            if bytes.get(at) == Some(&b'"') {
                at += 1;
                let mut closed = false;
                while at < bytes.len() {
                    if bytes[at] == b'"' {
                        if bytes.get(at + 1) == Some(&b'"') {
                            at += 2;
                        } else {
                            at += 1;
                            closed = true;
                            break;
                        }
                    } else {
                        at += 1;
                    }
                }
                if !closed {
                    return Err(invalid("unterminated quoted CSV field"));
                }
                if bytes
                    .get(at)
                    .is_some_and(|b| !matches!(b, b',' | b'\r' | b'\n'))
                {
                    return Err(invalid("bytes after quoted CSV field"));
                }
            } else {
                while at < bytes.len() && !matches!(bytes[at], b',' | b'\r' | b'\n') {
                    if bytes[at] == b'"' {
                        return Err(invalid("quote inside unquoted CSV field"));
                    }
                    at += 1;
                }
            }
            match bytes.get(at) {
                Some(b',') => {
                    at += 1;
                    continue;
                }
                Some(b'\r') => {
                    if bytes.get(at + 1) != Some(&b'\n') {
                        return Err(invalid("CSV bare CR record terminator"));
                    }
                    at += 2;
                }
                Some(b'\n') => {
                    at += 1;
                }
                None => {}
                _ => return Err(invalid("invalid CSV field delimiter")),
            }
            break;
        }
        if expected.is_some_and(|n| n != fields) {
            return Err(invalid("CSV rows have inconsistent field counts"));
        }
        expected = Some(fields);
        records += 1;
        if records > limits.max_records {
            return Err(invalid("CSV record count exceeds limit"));
        }
    }
    Ok((records, expected.unwrap_or(0)))
}
fn jsonl(text: &str, limits: StructuredTextLimits) -> Result<usize, RuntimeError> {
    let mut records = 0;
    for line in text.lines() {
        if line.trim().is_empty() {
            return Err(invalid("JSONL contains an empty record"));
        }
        let value: Value =
            serde_json::from_str(line).map_err(|_| invalid("malformed JSONL object"))?;
        let object = value
            .as_object()
            .ok_or_else(|| invalid("JSONL record is not an object"))?;
        if object.len() > limits.max_fields {
            return Err(invalid("JSONL field count exceeds limit"));
        }
        records += 1;
        if records > limits.max_records {
            return Err(invalid("JSONL record count exceeds limit"));
        }
    }
    if records == 0 {
        return Err(invalid("JSONL has no records"));
    }
    Ok(records)
}
fn eml(text: &str, limits: StructuredTextLimits) -> Result<(), RuntimeError> {
    if text.contains('\0') {
        return Err(invalid("EML contains NUL"));
    }
    let (headers, body) = text
        .split_once("\r\n\r\n")
        .or_else(|| text.split_once("\n\n"))
        .ok_or_else(|| invalid("EML lacks header/body separator"))?;
    if body.trim().is_empty() || headers.len() > 65536 {
        return Err(invalid("EML body empty or header limit exceeded"));
    }
    let mut fields = BTreeMap::<String, String>::new();
    let mut current = None::<String>;
    for line in headers.lines() {
        if line.starts_with([' ', '\t']) {
            let key = current
                .as_ref()
                .ok_or_else(|| invalid("EML folded header has no field"))?;
            let value = fields.get_mut(key).expect("previous validated header");
            value.push(' ');
            value.push_str(line.trim());
            continue;
        }
        let (key, value) = line
            .split_once(':')
            .ok_or_else(|| invalid("malformed EML header"))?;
        if key.is_empty() || !key.bytes().all(|b| b.is_ascii_graphic() && b != b':') {
            return Err(invalid("invalid EML header name"));
        }
        let key = key.to_ascii_lowercase();
        if fields.insert(key.clone(), value.trim().into()).is_some() {
            return Err(invalid("duplicate EML header"));
        }
        current = Some(key);
        if fields.len() > limits.max_fields {
            return Err(invalid("EML field count exceeds limit"));
        }
    }
    if let Some(value) = fields.get("content-type") {
        let mut pieces = value.split(';');
        if !pieces
            .next()
            .unwrap_or("")
            .trim()
            .eq_ignore_ascii_case("text/plain")
        {
            return Err(invalid(
                "EML supports text/plain only; MIME decoding is not implicit",
            ));
        }
        for option in pieces {
            let (key, value) = option
                .trim()
                .split_once('=')
                .ok_or_else(|| invalid("invalid EML content-type parameter"))?;
            if !key.trim().eq_ignore_ascii_case("charset")
                || !matches!(
                    value.trim().trim_matches('"').to_ascii_lowercase().as_str(),
                    "utf-8" | "us-ascii"
                )
            {
                return Err(invalid("unsupported EML charset/content-type parameter"));
            }
        }
    }
    let declared_ascii = fields
        .get("content-type")
        .is_some_and(|v| v.to_ascii_lowercase().contains("us-ascii"))
        || fields
            .get("content-transfer-encoding")
            .is_some_and(|v| v.eq_ignore_ascii_case("7bit"));
    if declared_ascii && !body.is_ascii() {
        return Err(invalid(
            "EML non-ASCII bytes contradict declared ASCII/7bit content",
        ));
    }
    if fields
        .get("content-transfer-encoding")
        .is_some_and(|v| !matches!(v.to_ascii_lowercase().as_str(), "7bit" | "8bit"))
    {
        return Err(invalid(
            "encoded EML content requires an explicit MIME decoder",
        ));
    }
    Ok(())
}
