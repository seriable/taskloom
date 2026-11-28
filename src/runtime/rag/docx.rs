//! Strict bounded DOCX WordprocessingML extraction; never an Office executable.
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Cursor, Read};
use std::path::Path;

use futures_util::future::BoxFuture;
use quick_xml::{events::Event, Reader};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zip::{CompressionMethod, ZipArchive};

use super::{invalid, Document, DocumentLoader, MAX_DOCUMENT_BYTES};
use crate::runtime::RuntimeError;
const WML: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const DOCX_MAIN: &str =
    "application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml";

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocxLimits {
    pub max_archive_bytes: usize,
    pub max_entries: usize,
    pub max_entry_bytes: usize,
    pub max_uncompressed_bytes: usize,
    pub max_xml_depth: usize,
}
impl Default for DocxLimits {
    fn default() -> Self {
        Self {
            max_archive_bytes: 8 * 1024 * 1024,
            max_entries: 256,
            max_entry_bytes: 8 * 1024 * 1024,
            max_uncompressed_bytes: 32 * 1024 * 1024,
            max_xml_depth: 128,
        }
    }
}
impl DocxLimits {
    fn validate(self) -> Result<Self, RuntimeError> {
        if !(1..=8 * 1024 * 1024).contains(&self.max_archive_bytes)
            || !(1..=1024).contains(&self.max_entries)
            || !(1..=8 * 1024 * 1024).contains(&self.max_entry_bytes)
            || !(1..=64 * 1024 * 1024).contains(&self.max_uncompressed_bytes)
            || !(1..=256).contains(&self.max_xml_depth)
        {
            Err(invalid("invalid DOCX archive/XML limits"))
        } else {
            Ok(self)
        }
    }
}
#[derive(Debug, Clone, Copy, Default)]
pub struct DocxLoader {
    pub limits: DocxLimits,
}
impl DocumentLoader for DocxLoader {
    fn load<'a>(
        &'a self,
        path: &'a Path,
        id: &'a str,
        title: &'a str,
    ) -> BoxFuture<'a, Result<Document, RuntimeError>> {
        Box::pin(async move {
            let limits = self.limits.validate()?;
            let path = path.to_path_buf();
            let id = id.to_owned();
            let title = title.to_owned();
            crate::runtime::store::blocking(move || {
                let path =
                    std::fs::canonicalize(path).map_err(|_| invalid("DOCX path unavailable"))?;
                let file =
                    std::fs::File::open(&path).map_err(|_| invalid("DOCX source unavailable"))?;
                if !file
                    .metadata()
                    .map_err(|_| invalid("DOCX metadata unavailable"))?
                    .is_file()
                {
                    return Err(invalid("DOCX source is not a regular file"));
                }
                let mut bytes = Vec::new();
                file.take((limits.max_archive_bytes + 1) as u64)
                    .read_to_end(&mut bytes)
                    .map_err(|_| invalid("DOCX read failed"))?;
                if bytes.len() > limits.max_archive_bytes {
                    return Err(invalid("DOCX compressed archive exceeds limit"));
                }
                let archive_sha = hex::encode(Sha256::digest(&bytes));
                let text = extract(&bytes, limits)?;
                let text_sha = hex::encode(Sha256::digest(text.as_bytes()));
                let source = format!(
                    "{}#taskloom-docx-extracted-v1;archive-sha256={};text-sha256={}",
                    path.to_string_lossy(),
                    archive_sha,
                    text_sha
                );
                let metadata = BTreeMap::from([
                    ("format".into(), "docx-wordprocessingml-v1".into()),
                    ("source_archive_sha256".into(), archive_sha),
                    (
                        "extracted_text_sha256".into(),
                        hex::encode(Sha256::digest(text.as_bytes())),
                    ),
                    (
                        "citation_offsets".into(),
                        "UTF-8 bytes in extracted text snapshot, not ZIP/XML file offsets".into(),
                    ),
                    (
                        "extraction_policy".into(),
                        serde_json::to_string(&limits)
                            .map_err(|_| invalid("cannot encode DOCX extraction policy"))?,
                    ),
                ]);
                let document = Document {
                    id,
                    title,
                    source,
                    text,
                    metadata,
                };
                document.validate()?;
                Ok(document)
            })
            .await
        })
    }
}
fn extract(bytes: &[u8], limits: DocxLimits) -> Result<String, RuntimeError> {
    let mut archive =
        ZipArchive::new(Cursor::new(bytes)).map_err(|_| invalid("malformed DOCX ZIP archive"))?;
    if archive.len() > limits.max_entries {
        return Err(invalid("DOCX entry count exceeds limit"));
    }
    let mut paths = BTreeSet::new();
    let mut total = 0usize;
    let mut main = None;
    let mut types = false;
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|_| invalid("cannot open DOCX ZIP entry"))?;
        let name = std::str::from_utf8(entry.name_raw())
            .map_err(|_| invalid("DOCX entry name is not UTF-8"))?
            .to_owned();
        let lower = name.to_ascii_lowercase();
        let plain = name.strip_suffix('/').unwrap_or(&name);
        if name.len() > 1024
            || plain.is_empty()
            || plain.starts_with('/')
            || plain.contains('\\')
            || plain.contains(':')
            || plain.chars().any(char::is_control)
            || plain
                .split('/')
                .any(|part| part.is_empty() || part == "." || part == "..")
            || !paths.insert(plain.to_lowercase())
        {
            return Err(invalid("unsafe or duplicate DOCX ZIP path"));
        }
        if entry.encrypted()
            || !matches!(
                entry.compression(),
                CompressionMethod::Stored | CompressionMethod::Deflated
            )
            || entry.unix_mode().is_some_and(|mode| {
                matches!(
                    mode & 0o170000,
                    0o120000 | 0o020000 | 0o060000 | 0o010000 | 0o140000
                )
            })
        {
            return Err(invalid(
                "DOCX encrypted/unsupported/link/device entry refused",
            ));
        }
        if lower.contains("vbaproject")
            || lower.contains("vba/")
            || lower.starts_with("word/activex/")
            || lower.starts_with("word/embeddings/")
        {
            return Err(invalid("DOCX macro/active embedded content refused"));
        }
        let size =
            usize::try_from(entry.size()).map_err(|_| invalid("DOCX entry size overflow"))?;
        total = total
            .checked_add(size)
            .ok_or_else(|| invalid("DOCX total size overflow"))?;
        if size > limits.max_entry_bytes
            || total > limits.max_uncompressed_bytes
            || (entry.is_dir() && size != 0)
        {
            return Err(invalid("DOCX uncompressed entry/total limit exceeded"));
        }
        let mut body = Vec::new();
        entry
            .by_ref()
            .take((limits.max_entry_bytes + 1) as u64)
            .read_to_end(&mut body)
            .map_err(|_| invalid("DOCX entry decode/CRC failed"))?;
        if body.len() != size {
            return Err(invalid("DOCX entry size differs from declared bytes"));
        }
        if lower.ends_with(".xml") || lower.ends_with(".rels") {
            let xml = std::str::from_utf8(&body).map_err(|_| invalid("DOCX XML must be UTF-8"))?;
            let parsed = parse_xml(
                xml,
                limits,
                name == "word/document.xml",
                name == "[Content_Types].xml",
            )?;
            if name == "word/document.xml" {
                main = Some(parsed.text);
            }
            if name == "[Content_Types].xml" {
                types = parsed.main_type;
            }
        }
    }
    if !types {
        return Err(invalid("DOCX main content type is missing or unsupported"));
    }
    let text = main.ok_or_else(|| invalid("DOCX word/document.xml missing"))?;
    if text.trim().is_empty() {
        return Err(invalid("DOCX extracted body is empty"));
    }
    Ok(text)
}
struct Parsed {
    text: String,
    main_type: bool,
}
fn parse_xml(
    xml: &str,
    limits: DocxLimits,
    word: bool,
    content_types: bool,
) -> Result<Parsed, RuntimeError> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().check_end_names = true;
    let mut stack = Vec::<Vec<u8>>::new();
    let mut roots = 0usize;
    let mut body_depth = None;
    let mut body_seen = false;
    let mut text_depth = None;
    let mut result = Parsed {
        text: String::new(),
        main_type: false,
    };
    loop {
        let event = reader
            .read_event()
            .map_err(|_| invalid("malformed DOCX XML"))?;
        match event {
            Event::Start(ref element) | Event::Empty(ref element) => {
                let empty = matches!(event, Event::Empty(_));
                let tag = element.name().as_ref().to_vec();
                if tag.len() > 256 {
                    return Err(invalid("DOCX XML element name exceeds limit"));
                }
                if stack.len() >= limits.max_xml_depth {
                    return Err(invalid("DOCX XML depth exceeds limit"));
                }
                if stack.is_empty() {
                    roots += 1;
                    if roots > 1 || (word && tag != b"w:document") {
                        return Err(invalid("DOCX XML root differs or is repeated"));
                    }
                }
                if text_depth.is_some() {
                    return Err(invalid("nested markup inside DOCX text element"));
                }
                let mut word_namespace = false;
                let mut part = None;
                let mut kind = None;
                for (attribute_index, attribute) in
                    element.attributes().with_checks(true).enumerate()
                {
                    if attribute_index >= 256 {
                        return Err(invalid("DOCX XML attribute count exceeds limit"));
                    }
                    let attribute = attribute.map_err(|_| invalid("invalid DOCX XML attribute"))?;
                    if attribute.key.as_ref().len() > 256 || attribute.value.len() > 65536 {
                        return Err(invalid("DOCX XML attribute exceeds limit"));
                    }
                    let value = attribute
                        .unescape_value()
                        .map_err(|_| invalid("unknown DOCX XML entity/attribute"))?;
                    if attribute.key.as_ref() == b"xmlns:w" {
                        if word && value != WML {
                            return Err(invalid("DOCX WordprocessingML namespace changed"));
                        }
                        word_namespace = value == WML;
                    }
                    if content_types {
                        if attribute.key.as_ref() == b"PartName" {
                            part = Some(value.to_string());
                        }
                        if attribute.key.as_ref() == b"ContentType" {
                            let lower = value.to_ascii_lowercase();
                            if lower.contains("macroenabled")
                                || lower.contains("vba")
                                || lower.contains("activex")
                            {
                                return Err(invalid("DOCX macro-enabled content type refused"));
                            }
                            kind = Some(value.to_string());
                        }
                    }
                }
                if word && stack.is_empty() && !word_namespace {
                    return Err(invalid(
                        "DOCX root lacks supported WordprocessingML namespace",
                    ));
                }
                if content_types
                    && tag == b"Override"
                    && part.as_deref() == Some("/word/document.xml")
                {
                    if result.main_type || kind.as_deref() != Some(DOCX_MAIN) {
                        return Err(invalid("DOCX main content type differs or repeats"));
                    }
                    result.main_type = true;
                }
                if word && tag == b"w:body" {
                    if body_seen || stack.len() != 1 {
                        return Err(invalid("DOCX body placement/repetition is invalid"));
                    }
                    body_seen = true;
                    if !empty {
                        body_depth = Some(stack.len() + 1);
                    }
                }
                if word && body_depth.is_some() {
                    if tag == b"w:t" && !empty {
                        text_depth = Some(stack.len() + 1);
                    }
                    if matches!(tag.as_slice(), b"w:tab") {
                        result.text.push('\t');
                    }
                    if matches!(tag.as_slice(), b"w:br" | b"w:cr") {
                        result.text.push('\n');
                    }
                }
                if !empty {
                    stack.push(tag);
                }
            }
            Event::End(element) => {
                let tag = element.name().as_ref().to_vec();
                if stack.pop().as_ref() != Some(&tag) {
                    return Err(invalid("DOCX XML closing tag mismatch"));
                }
                if word && body_depth.is_some() {
                    if tag == b"w:t" {
                        text_depth = None;
                    }
                    if tag == b"w:p" {
                        result.text.push('\n');
                    }
                    if tag == b"w:tc" {
                        while result.text.ends_with('\n') {
                            result.text.pop();
                        }
                        result.text.push('\t');
                    }
                    if tag == b"w:tr" {
                        while result.text.ends_with('\t') {
                            result.text.pop();
                        }
                        result.text.push('\n');
                    }
                    if tag == b"w:body" {
                        body_depth = None;
                    }
                }
            }
            Event::Text(value) => {
                let value = value
                    .unescape()
                    .map_err(|_| invalid("unknown DOCX XML entity"))?;
                if stack.is_empty() && !value.trim().is_empty() {
                    return Err(invalid("text outside DOCX XML root"));
                }
                if text_depth.is_some() {
                    result.text.push_str(&value);
                }
            }
            Event::CData(value) => {
                let value = value
                    .decode()
                    .map_err(|_| invalid("DOCX XML CDATA is not UTF-8"))?;
                if text_depth.is_some() {
                    result.text.push_str(&value);
                } else if stack.is_empty() {
                    return Err(invalid("CDATA outside DOCX XML root"));
                }
            }
            Event::DocType(_) => {
                return Err(invalid("DOCX XML DTD/entity declarations are forbidden"))
            }
            Event::Decl(declaration) => {
                if declaration
                    .version()
                    .map_err(|_| invalid("invalid DOCX XML declaration"))?
                    .as_ref()
                    != b"1.0"
                {
                    return Err(invalid("unsupported DOCX XML version"));
                }
                if declaration
                    .encoding()
                    .transpose()
                    .map_err(|_| invalid("invalid DOCX XML encoding"))?
                    .is_some_and(|v| {
                        !v.eq_ignore_ascii_case(b"utf-8") && !v.eq_ignore_ascii_case(b"us-ascii")
                    })
                {
                    return Err(invalid("unsupported DOCX XML encoding"));
                }
            }
            Event::Eof => break,
            Event::Comment(_) | Event::PI(_) => {}
        }
        if result.text.len() > MAX_DOCUMENT_BYTES {
            return Err(invalid("DOCX extracted text exceeds limit"));
        }
    }
    if roots != 1 || !stack.is_empty() || (word && !body_seen) {
        return Err(invalid("DOCX XML is incomplete or lacks body"));
    }
    while result.text.ends_with('\n') {
        result.text.pop();
    }
    Ok(result)
}
