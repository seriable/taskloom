//! Bounded, read-only OOXML snapshots. No formulas, macros, external fetch or rendering.
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Cursor, Read};
use std::path::Path;
use std::sync::Arc;

use futures_util::future::BoxFuture;
use quick_xml::{events::Event, Reader};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zip::{CompressionMethod, ZipArchive};

use super::{invalid, Document, DocumentLoader, MAX_DOCUMENT_BYTES};
use crate::runtime::RuntimeError;

const S: &str = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
const P: &str = "http://schemas.openxmlformats.org/presentationml/2006/main";
const A: &str = "http://schemas.openxmlformats.org/drawingml/2006/main";
const R: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const PR: &str = "http://schemas.openxmlformats.org/package/2006/relationships";
const CT: &str = "http://schemas.openxmlformats.org/package/2006/content-types";
const XML: &str = "http://www.w3.org/XML/1998/namespace";
const XLSX: &str = "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml";
const PPTX: &str =
    "application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml";
const SHEET: &str = "application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml";
const STRINGS: &str =
    "application/vnd.openxmlformats-officedocument.spreadsheetml.sharedStrings+xml";
const SLIDE: &str = "application/vnd.openxmlformats-officedocument.presentationml.slide+xml";

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OfficeLimits {
    pub max_archive_bytes: usize,
    pub max_entries: usize,
    pub max_entry_bytes: usize,
    pub max_uncompressed_bytes: usize,
    pub max_xml_depth: usize,
    /// Aggregate element count across every XML/relationship part.
    pub max_xml_nodes: usize,
    pub max_units: usize,
    /// Aggregate extracted cells/paragraphs/shared strings, not a dense grid.
    pub max_items: usize,
}
impl Default for OfficeLimits {
    fn default() -> Self {
        Self {
            max_archive_bytes: 8 * 1024 * 1024,
            max_entries: 256,
            max_entry_bytes: 8 * 1024 * 1024,
            max_uncompressed_bytes: 32 * 1024 * 1024,
            max_xml_depth: 128,
            max_xml_nodes: 100_000,
            max_units: 128,
            max_items: 50_000,
        }
    }
}
impl OfficeLimits {
    fn validate(self) -> Result<Self, RuntimeError> {
        if !(1..=8 * 1024 * 1024).contains(&self.max_archive_bytes)
            || !(1..=1024).contains(&self.max_entries)
            || !(1..=8 * 1024 * 1024).contains(&self.max_entry_bytes)
            || !(1..=64 * 1024 * 1024).contains(&self.max_uncompressed_bytes)
            || !(1..=128).contains(&self.max_xml_depth)
            || !(1..=200_000).contains(&self.max_xml_nodes)
            || !(1..=256).contains(&self.max_units)
            || !(1..=100_000).contains(&self.max_items)
        {
            return Err(invalid("invalid Office archive/XML/extraction limits"));
        }
        Ok(self)
    }
}
#[derive(Debug, Clone, Copy, Default)]
pub struct XlsxLoader {
    pub limits: OfficeLimits,
}
#[derive(Debug, Clone, Copy, Default)]
pub struct PptxLoader {
    pub limits: OfficeLimits,
}
macro_rules! loader {
    ($loader:ident, $format:literal, $extract:ident) => {
        impl DocumentLoader for $loader {
            fn load<'a>(&'a self, path: &'a Path, id: &'a str, title: &'a str)
                -> BoxFuture<'a, Result<Document, RuntimeError>> {
                Box::pin(async move {
                    let limits = self.limits.validate()?;
                    let path = path.to_path_buf();
                    let id = id.to_owned();
                    let title = title.to_owned();
                    crate::runtime::store::blocking(move || {
                        let path = std::fs::canonicalize(path).map_err(|_| invalid("Office path unavailable"))?;
                        let file = std::fs::File::open(&path).map_err(|_| invalid("Office source unavailable"))?;
                        if !file.metadata().map_err(|_| invalid("Office metadata unavailable"))?.is_file() {
                            return Err(invalid("Office source is not a regular file"));
                        }
                        let mut bytes = Vec::new();
                        file.take((limits.max_archive_bytes + 1) as u64).read_to_end(&mut bytes)
                            .map_err(|_| invalid("Office read failed"))?;
                        if bytes.len() > limits.max_archive_bytes {
                            return Err(invalid("Office compressed archive exceeds limit"));
                        }
                        let archive_sha = hex::encode(Sha256::digest(&bytes));
                        let package = Package::read(&bytes, limits)?;
                        let text = $extract(&package, limits)?;
                        let text_sha = hex::encode(Sha256::digest(text.as_bytes()));
                        let policy = serde_json::to_string(&limits).map_err(|_| invalid("Office policy encoding failed"))?;
                        let document = Document {
                            id, title,
                            source: format!("{}#taskloom-{}-extracted-v1;archive-sha256={};text-sha256={}",
                                path.to_string_lossy(), $format, archive_sha, text_sha),
                            text,
                            metadata: BTreeMap::from([
                                ("format".into(), concat!($format, "-ooxml-v1").into()),
                                ("source_archive_sha256".into(), archive_sha),
                                ("extracted_text_sha256".into(), text_sha),
                                ("extraction_policy_sha256".into(), hex::encode(Sha256::digest(policy.as_bytes()))),
                                ("extraction_policy".into(), policy),
                                ("citation_offsets".into(), "UTF-8 bytes in extracted text snapshot, not ZIP/XML file offsets".into()),
                                ("value_encoding".into(), "JSON strings; structural labels are generated locators, not source prose".into()),
                            ]),
                        };
                        document.validate()?;
                        Ok(document)
                    }).await
                })
            }
        }
    };
}
loader!(XlsxLoader, "xlsx", xlsx);
loader!(PptxLoader, "pptx", pptx);

type Name = (Arc<str>, String);
type Namespaces = BTreeMap<String, Arc<str>>;
#[derive(Default)]
struct Node {
    name: Name,
    attrs: BTreeMap<Name, String>,
    text: String,
    children: Vec<Node>,
}
impl Node {
    fn is(&self, ns: &str, local: &str) -> bool {
        self.name.0.as_ref() == ns && self.name.1 == local
    }
    fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .get(&(Arc::from(""), name.to_owned()))
            .map(String::as_str)
    }
    fn rid(&self) -> Result<&str, RuntimeError> {
        self.attrs
            .get(&(R.into(), "id".into()))
            .map(String::as_str)
            .ok_or_else(|| invalid("Office structural relationship ID missing"))
    }
    fn one(&self, ns: &str, name: &str) -> Result<&Node, RuntimeError> {
        let mut nodes = self.children.iter().filter(|n| n.is(ns, name));
        let node = nodes
            .next()
            .ok_or_else(|| invalid("Office required XML child missing"))?;
        if nodes.next().is_some() {
            return Err(invalid("Office structural XML child repeated"));
        }
        Ok(node)
    }
}
fn chars(value: &str) -> Result<(), RuntimeError> {
    if value.chars().any(|c| !matches!(c, '\t' | '\n' | '\r' | '\u{20}'..='\u{d7ff}' | '\u{e000}'..='\u{fffd}' | '\u{10000}'..='\u{10ffff}')) {
        return Err(invalid("invalid Office XML character"));
    }
    Ok(())
}
fn qname(raw: &[u8]) -> Result<(&str, &str), RuntimeError> {
    let raw = std::str::from_utf8(raw).map_err(|_| invalid("Office XML name is not UTF-8"))?;
    if raw.len() > 256 {
        return Err(invalid("Office XML name exceeds limit"));
    }
    let mut parts = raw.split(':');
    let first = parts.next().unwrap_or("");
    let second = parts.next();
    let valid = |s: &str| {
        !s.is_empty()
            && s.bytes().enumerate().all(|(i, b)| {
                b.is_ascii_alphabetic()
                    || b == b'_'
                    || (i > 0 && (b.is_ascii_digit() || b".-".contains(&b)))
            })
    };
    if parts.next().is_some() || !valid(first) || second.is_some_and(|s| !valid(s)) {
        return Err(invalid("unsupported Office XML name"));
    }
    Ok(second.map_or(("", first), |s| (first, s)))
}
fn expand(
    raw: &[u8],
    namespaces: &Namespaces,
    ancestors: &[(Node, Namespaces)],
    attribute: bool,
) -> Result<Name, RuntimeError> {
    let (prefix, local) = qname(raw)?;
    let ns = if prefix.is_empty() && attribute {
        Arc::from("")
    } else {
        namespaces
            .get(prefix)
            .or_else(|| ancestors.iter().rev().find_map(|(_, ns)| ns.get(prefix)))
            .cloned()
            .unwrap_or_else(|| Arc::from(if prefix == "xml" { XML } else { "" }))
    };
    if !prefix.is_empty() && ns.is_empty() {
        return Err(invalid("unbound Office XML prefix"));
    }
    Ok((ns, local.into()))
}
fn xml(bytes: &[u8], limits: OfficeLimits, nodes: &mut usize) -> Result<Node, RuntimeError> {
    let text = std::str::from_utf8(bytes).map_err(|_| invalid("Office XML must be UTF-8"))?;
    chars(text)?;
    let mut reader = Reader::from_str(text);
    reader.config_mut().check_end_names = true;
    let mut stack: Vec<(Node, Namespaces)> = Vec::new();
    let mut root = None;
    let mut declaration = false;
    loop {
        let event = reader
            .read_event()
            .map_err(|_| invalid("malformed Office XML"))?;
        match event {
            Event::Start(ref e) | Event::Empty(ref e) => {
                *nodes += 1;
                if *nodes > limits.max_xml_nodes || stack.len() >= limits.max_xml_depth {
                    return Err(invalid("Office XML node/depth allowance exceeded"));
                }
                if stack.is_empty() && root.is_some() {
                    return Err(invalid("multiple Office XML roots"));
                }
                // Keep declarations local and intern expanded namespace URIs.
                // Cloning inherited maps/long URIs for each element amplifies input.
                let mut ns = Namespaces::new();
                let mut attrs = Vec::new();
                for (i, a) in e.attributes().with_checks(true).enumerate() {
                    if i >= 256 {
                        return Err(invalid("too many Office XML attributes"));
                    }
                    let a = a.map_err(|_| invalid("malformed Office XML attribute"))?;
                    if a.value.len() > 65536 {
                        return Err(invalid("Office XML attribute too large"));
                    }
                    let key = a.key.as_ref().to_vec();
                    let (prefix, local) = qname(&key)?;
                    let value = a
                        .unescape_value()
                        .map_err(|_| invalid("unknown Office XML entity"))?
                        .into_owned();
                    chars(&value)?;
                    if key == b"xmlns" || prefix == "xmlns" {
                        let prefix = if key == b"xmlns" { "" } else { local };
                        if prefix == "xmlns"
                            || value == "http://www.w3.org/2000/xmlns/"
                            || (prefix == "xml") != (value == XML)
                            || (!prefix.is_empty() && value.is_empty())
                            || value.len() > 1024
                        {
                            return Err(invalid("invalid Office XML namespace binding"));
                        }
                        ns.insert(prefix.into(), Arc::from(value));
                    } else {
                        attrs.push((key, value));
                    }
                }
                let mut node = Node {
                    name: expand(e.name().as_ref(), &ns, &stack, false)?,
                    ..Default::default()
                };
                for (key, value) in attrs {
                    if node
                        .attrs
                        .insert(expand(&key, &ns, &stack, true)?, value)
                        .is_some()
                    {
                        return Err(invalid("duplicate expanded Office XML attribute"));
                    }
                }
                if matches!(event, Event::Empty(_)) {
                    if let Some(parent) = stack.last_mut() {
                        parent.0.children.push(node);
                    } else {
                        root = Some(node);
                    }
                } else {
                    stack.push((node, ns));
                }
            }
            Event::End(_) => {
                let (node, _) = stack
                    .pop()
                    .ok_or_else(|| invalid("unmatched Office XML end"))?;
                if let Some(parent) = stack.last_mut() {
                    parent.0.children.push(node);
                } else {
                    root = Some(node);
                }
            }
            Event::Text(t) => {
                let t = t
                    .unescape()
                    .map_err(|_| invalid("unknown Office XML entity"))?;
                chars(&t)?;
                if let Some(parent) = stack.last_mut() {
                    parent.0.text.push_str(&t);
                } else if !t.trim().is_empty() {
                    return Err(invalid("text outside Office XML root"));
                }
            }
            Event::CData(t) => {
                let t = t.decode().map_err(|_| invalid("invalid Office CDATA"))?;
                chars(&t)?;
                stack
                    .last_mut()
                    .ok_or_else(|| invalid("CDATA outside Office XML root"))?
                    .0
                    .text
                    .push_str(&t);
            }
            Event::Decl(d) => {
                if declaration
                    || root.is_some()
                    || !stack.is_empty()
                    || d.version()
                        .map_err(|_| invalid("invalid Office XML declaration"))?
                        .as_ref()
                        != b"1.0"
                    || d.encoding()
                        .transpose()
                        .map_err(|_| invalid("invalid Office XML encoding"))?
                        .is_some_and(|s| {
                            !s.eq_ignore_ascii_case(b"utf-8")
                                && !s.eq_ignore_ascii_case(b"us-ascii")
                        })
                {
                    return Err(invalid("unsupported Office XML declaration"));
                }
                declaration = true;
            }
            Event::DocType(_) | Event::PI(_) => {
                return Err(invalid(
                    "Office DTD/entities/processing instructions refused",
                ))
            }
            Event::Comment(_) => {}
            Event::Eof => break,
        }
    }
    if !stack.is_empty() {
        return Err(invalid("incomplete Office XML"));
    }
    root.ok_or_else(|| invalid("Office XML root missing"))
}

fn safe_path(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 1024
        && !name.starts_with('/')
        && !name.contains(['\\', ':', '%', '?', '#'])
        && !name.chars().any(char::is_control)
        && name
            .split('/')
            .all(|p| !p.is_empty() && p != "." && p != "..")
}
struct Package {
    parts: BTreeMap<String, Node>,
    paths: BTreeSet<String>,
    types: BTreeMap<String, String>,
}
impl Package {
    fn read(bytes: &[u8], limits: OfficeLimits) -> Result<Self, RuntimeError> {
        let mut zip =
            ZipArchive::new(Cursor::new(bytes)).map_err(|_| invalid("malformed Office ZIP"))?;
        if zip.len() > limits.max_entries {
            return Err(invalid("Office ZIP entry count exceeded"));
        }
        let mut result = Self {
            parts: BTreeMap::new(),
            paths: BTreeSet::new(),
            types: BTreeMap::new(),
        };
        let mut folded = BTreeSet::new();
        let mut total = 0usize;
        let mut nodes = 0usize;
        for i in 0..zip.len() {
            let mut entry = zip
                .by_index(i)
                .map_err(|_| invalid("Office ZIP entry unavailable"))?;
            let raw = std::str::from_utf8(entry.name_raw())
                .map_err(|_| invalid("Office ZIP path must be UTF-8"))?
                .to_owned();
            let name = raw.strip_suffix('/').unwrap_or(&raw);
            let lower = name.to_lowercase();
            if !safe_path(name) || !folded.insert(lower.clone()) {
                return Err(invalid("unsafe/duplicate Office ZIP path"));
            }
            if entry.encrypted()
                || !matches!(
                    entry.compression(),
                    CompressionMethod::Stored | CompressionMethod::Deflated
                )
                || entry.unix_mode().is_some_and(|m| {
                    matches!(
                        m & 0o170000,
                        0o120000 | 0o020000 | 0o060000 | 0o010000 | 0o140000
                    )
                })
            {
                return Err(invalid(
                    "encrypted/unsupported/link/device Office ZIP entry",
                ));
            }
            if lower.contains("vba")
                || lower.contains("activex")
                || lower.contains("embeddings/")
                || lower.contains("externallinks/")
                || lower.ends_with(".bin")
                || lower.contains("macrosheet")
            {
                return Err(invalid(
                    "Office macros/active embeddings/external workbook links refused",
                ));
            }
            let size =
                usize::try_from(entry.size()).map_err(|_| invalid("Office ZIP size overflow"))?;
            total = total
                .checked_add(size)
                .ok_or_else(|| invalid("Office ZIP total overflow"))?;
            if size > limits.max_entry_bytes
                || total > limits.max_uncompressed_bytes
                || (entry.is_dir() && size != 0)
            {
                return Err(invalid(
                    "Office uncompressed entry/total allowance exceeded",
                ));
            }
            let mut body = Vec::new();
            entry
                .by_ref()
                .take((limits.max_entry_bytes + 1) as u64)
                .read_to_end(&mut body)
                .map_err(|_| invalid("Office ZIP decode/CRC failed"))?;
            if body.len() != size {
                return Err(invalid("Office ZIP declared/actual size mismatch"));
            }
            if !entry.is_dir() {
                result.paths.insert(name.into());
                if lower.ends_with(".xml") || lower.ends_with(".rels") {
                    result
                        .parts
                        .insert(name.into(), xml(&body, limits, &mut nodes)?);
                }
            }
        }
        let types = result.part("[Content_Types].xml")?;
        if !types.is(CT, "Types") {
            return Err(invalid("Office content types namespace/root invalid"));
        }
        let mut overrides = BTreeMap::new();
        let mut defaults = BTreeSet::new();
        for n in &types.children {
            let content = n
                .attr("ContentType")
                .ok_or_else(|| invalid("Office content type missing"))?;
            let lower = content.to_lowercase();
            if lower.contains("macro")
                || lower.contains("vba")
                || lower.contains("activex")
                || lower.contains("oleobject")
            {
                return Err(invalid("Office active/macro content type refused"));
            }
            if n.is(CT, "Override") {
                let name = n
                    .attr("PartName")
                    .and_then(|s| s.strip_prefix('/'))
                    .ok_or_else(|| invalid("Office part name invalid"))?;
                if !safe_path(name)
                    || !result.paths.contains(name)
                    || overrides.insert(name.into(), content.into()).is_some()
                {
                    return Err(invalid(
                        "Office part type repeats or refers to missing/unsafe path",
                    ));
                }
            } else if n.is(CT, "Default") {
                if !defaults.insert(
                    n.attr("Extension")
                        .ok_or_else(|| invalid("Office extension missing"))?
                        .to_lowercase(),
                ) {
                    return Err(invalid("duplicate Office default content type"));
                }
            } else {
                return Err(invalid("unsupported Office content type element"));
            }
        }
        result.types = overrides;
        // Every relationship part is validated, including unused relationships.
        for path in result.parts.keys().filter(|p| p.ends_with(".rels")) {
            let source = relationship_source(path)?;
            if !source.is_empty() && !result.paths.contains(&source) {
                return Err(invalid("Office relationship source part missing"));
            }
            result.relationships(&source)?;
        }
        Ok(result)
    }
    fn part(&self, path: &str) -> Result<&Node, RuntimeError> {
        self.parts
            .get(path)
            .ok_or_else(|| invalid("required Office XML part missing"))
    }
    fn typed(&self, path: &str, kind: &str) -> Result<&Node, RuntimeError> {
        if self.types.get(path).map(String::as_str) != Some(kind) {
            return Err(invalid("Office structural part type mismatch"));
        }
        self.part(path)
    }
    fn main(&self, path: &str, kind: &str) -> Result<&Node, RuntimeError> {
        let rels = self.relationships("")?;
        let main = rels
            .values()
            .filter(|r| r.kind == format!("{R}/officeDocument"))
            .collect::<Vec<_>>();
        if main.len() != 1 || main[0].target.as_deref() != Some(path) {
            return Err(invalid("Office root document relationship invalid"));
        }
        self.typed(path, kind)
    }
    fn relationships(&self, source: &str) -> Result<BTreeMap<String, Relationship>, RuntimeError> {
        let path = if source.is_empty() {
            "_rels/.rels".into()
        } else {
            let (dir, file) = source.rsplit_once('/').unwrap_or(("", source));
            format!(
                "{}{}/{}.rels",
                if dir.is_empty() {
                    String::new()
                } else {
                    format!("{dir}/")
                },
                "_rels",
                file
            )
        };
        let root = self.part(&path)?;
        if !root.is(PR, "Relationships") {
            return Err(invalid("Office relationships root/namespace invalid"));
        }
        let mut result = BTreeMap::new();
        for rel in &root.children {
            if !rel.is(PR, "Relationship") {
                return Err(invalid("unsupported Office relationship element"));
            }
            let id = rel
                .attr("Id")
                .ok_or_else(|| invalid("Office relationship ID missing"))?;
            let kind = rel
                .attr("Type")
                .ok_or_else(|| invalid("Office relationship type missing"))?;
            let target = rel
                .attr("Target")
                .ok_or_else(|| invalid("Office relationship target missing"))?;
            if id.is_empty()
                || id.len() > 256
                || target.is_empty()
                || target.len() > 2048
                || kind.len() > 1024
            {
                return Err(invalid("Office relationship fields exceed limits"));
            }
            let target = match rel.attr("TargetMode") {
                Some("External") => None, // inert: never parsed as a file/URL or fetched
                None | Some("Internal") => {
                    let target = resolve(source, target)?;
                    if !self.paths.contains(&target) {
                        return Err(invalid("Office internal relationship target missing"));
                    }
                    Some(target)
                }
                _ => return Err(invalid("unknown Office relationship target mode")),
            };
            if result
                .insert(
                    id.into(),
                    Relationship {
                        kind: kind.into(),
                        target,
                    },
                )
                .is_some()
            {
                return Err(invalid("duplicate Office relationship ID"));
            }
        }
        Ok(result)
    }
}
fn relationship_source(path: &str) -> Result<String, RuntimeError> {
    if path == "_rels/.rels" {
        return Ok(String::new());
    }
    let (dir, file) = path
        .rsplit_once("/_rels/")
        .ok_or_else(|| invalid("unsupported relationship part placement"))?;
    let file = file
        .strip_suffix(".rels")
        .ok_or_else(|| invalid("invalid relationship path"))?;
    let source = format!("{dir}/{file}");
    if !safe_path(&source) {
        return Err(invalid("unsafe relationship source"));
    }
    Ok(source)
}
struct Relationship {
    kind: String,
    target: Option<String>,
}
fn resolve(source: &str, target: &str) -> Result<String, RuntimeError> {
    if target.starts_with('/')
        || target.contains(['\\', ':', '%', '?', '#'])
        || target.chars().any(char::is_control)
    {
        return Err(invalid("unsupported/unsafe internal Office relationship"));
    }
    let mut parts = source
        .rsplit_once('/')
        .map(|(d, _)| d.split('/').collect::<Vec<_>>())
        .unwrap_or_default();
    for part in target.split('/') {
        match part {
            ".." => {
                parts
                    .pop()
                    .ok_or_else(|| invalid("Office relationship escapes archive"))?;
            }
            "" | "." => return Err(invalid("ambiguous Office relationship path")),
            _ => parts.push(part),
        }
    }
    let result = parts.join("/");
    if !safe_path(&result) {
        return Err(invalid("unsafe Office relationship target"));
    }
    Ok(result)
}
fn related<'a>(
    rels: &'a BTreeMap<String, Relationship>,
    id: &str,
    kind: &str,
) -> Result<&'a str, RuntimeError> {
    let rel = rels
        .get(id)
        .ok_or_else(|| invalid("Office relationship ID not found"))?;
    if rel.kind != format!("{R}/{kind}") {
        return Err(invalid("Office structural relationship type differs"));
    }
    rel.target
        .as_deref()
        .ok_or_else(|| invalid("external Office structural relationship refused"))
}
fn leaf_text(node: &Node) -> Result<&str, RuntimeError> {
    if !node.children.is_empty() {
        return Err(invalid("nested markup inside Office text value"));
    }
    Ok(&node.text)
}
fn append(output: &mut String, line: &str) -> Result<(), RuntimeError> {
    if output.len().saturating_add(line.len()).saturating_add(1) > MAX_DOCUMENT_BYTES {
        return Err(invalid("Office extracted snapshot exceeds byte allowance"));
    }
    output.push_str(line);
    output.push('\n');
    Ok(())
}
fn quoted(value: &str) -> Result<String, RuntimeError> {
    serde_json::to_string(value).map_err(|_| invalid("Office value encoding failed"))
}
fn count(items: &mut usize, limits: OfficeLimits) -> Result<(), RuntimeError> {
    *items += 1;
    if *items > limits.max_items {
        return Err(invalid("Office extracted item allowance exceeded"));
    }
    Ok(())
}
fn rich_string(node: &Node) -> Result<String, RuntimeError> {
    let mut result = String::new();
    for child in &node.children {
        if child.is(S, "t") {
            result.push_str(leaf_text(child)?);
        } else if child.is(S, "r") {
            result.push_str(leaf_text(child.one(S, "t")?)?);
        } else if !child.is(S, "rPh") && !child.is(S, "phoneticPr") {
            return Err(invalid("unsupported Office shared/inline string child"));
        }
    }
    Ok(result)
}
fn positive(raw: Option<&str>, max: usize) -> Result<usize, RuntimeError> {
    let raw = raw.ok_or_else(|| invalid("Office structural index missing"))?;
    let n = raw
        .parse::<usize>()
        .map_err(|_| invalid("invalid Office structural index"))?;
    if n == 0 || n > max || n.to_string() != raw {
        return Err(invalid("Office structural index out of range"));
    }
    Ok(n)
}
fn address(raw: &str, row: usize) -> Result<usize, RuntimeError> {
    let at = raw
        .find(|c: char| !c.is_ascii_uppercase())
        .ok_or_else(|| invalid("invalid XLSX cell address"))?;
    let (letters, digits) = raw.split_at(at);
    if letters.is_empty() || letters.len() > 3 || positive(Some(digits), 1_048_576)? != row {
        return Err(invalid("XLSX cell address/row differs"));
    }
    let col = letters
        .bytes()
        .fold(0usize, |n, b| n * 26 + usize::from(b - b'A' + 1));
    if col > 16_384 {
        return Err(invalid("XLSX column exceeds XFD"));
    }
    Ok(col)
}
fn xlsx(pkg: &Package, limits: OfficeLimits) -> Result<String, RuntimeError> {
    let workbook = pkg.main("xl/workbook.xml", XLSX)?;
    if !workbook.is(S, "workbook") {
        return Err(invalid("unsupported XLSX workbook namespace/root"));
    }
    let rels = pkg.relationships("xl/workbook.xml")?;
    let mut shared = Vec::new();
    let mut items = 0;
    let shared_rels = rels
        .values()
        .filter(|r| r.kind == format!("{R}/sharedStrings"))
        .collect::<Vec<_>>();
    if shared_rels.len() > 1 {
        return Err(invalid("repeated XLSX shared strings relationship"));
    }
    if let Some(rel) = shared_rels.first() {
        let root = pkg.typed(
            rel.target
                .as_deref()
                .ok_or_else(|| invalid("external shared strings refused"))?,
            STRINGS,
        )?;
        if !root.is(S, "sst") {
            return Err(invalid("invalid XLSX shared strings root"));
        }
        for si in &root.children {
            if !si.is(S, "si") {
                return Err(invalid("invalid XLSX shared string item"));
            }
            count(&mut items, limits)?;
            shared.push(rich_string(si)?);
        }
    }
    let sheets = workbook.one(S, "sheets")?;
    if sheets.children.is_empty() || sheets.children.len() > limits.max_units {
        return Err(invalid("XLSX sheet allowance exceeded/empty"));
    }
    let mut output = String::new();
    let mut ids = BTreeSet::new();
    let mut names = BTreeSet::new();
    let mut targets = BTreeSet::new();
    for (i, sheet) in sheets.children.iter().enumerate() {
        if !sheet.is(S, "sheet") {
            return Err(invalid("unsupported XLSX sheet entry"));
        }
        let name = sheet
            .attr("name")
            .ok_or_else(|| invalid("XLSX sheet name missing"))?;
        if name.is_empty()
            || name.len() > 256
            || !names.insert(name.to_lowercase())
            || !ids.insert(positive(sheet.attr("sheetId"), u32::MAX as usize)?)
        {
            return Err(invalid("invalid/duplicate XLSX sheet name/ID"));
        }
        let target = related(&rels, sheet.rid()?, "worksheet")?;
        if !targets.insert(target) {
            return Err(invalid("duplicate XLSX worksheet target"));
        }
        let root = pkg.typed(target, SHEET)?;
        if !root.is(S, "worksheet") {
            return Err(invalid("unsupported XLSX worksheet root"));
        }
        append(
            &mut output,
            &format!(
                "[sheet {} name={} state={} part={}]",
                i + 1,
                quoted(name)?,
                quoted(sheet.attr("state").unwrap_or("visible"))?,
                quoted(target)?
            ),
        )?;
        let data = root.one(S, "sheetData")?;
        let mut previous_row = 0;
        for row in &data.children {
            if !row.is(S, "row") {
                return Err(invalid("unsupported XLSX sheetData child"));
            }
            let r = positive(row.attr("r"), 1_048_576)?;
            if r <= previous_row {
                return Err(invalid("XLSX rows not strictly increasing"));
            }
            previous_row = r;
            let mut previous_col = 0;
            for cell in &row.children {
                if !cell.is(S, "c") {
                    return Err(invalid("unsupported XLSX row child"));
                }
                count(&mut items, limits)?;
                let addr = cell
                    .attr("r")
                    .ok_or_else(|| invalid("XLSX cell address missing"))?;
                let col = address(addr, r)?;
                if col <= previous_col {
                    return Err(invalid("XLSX cells not strictly increasing"));
                }
                previous_col = col;
                if cell.children.iter().any(|n| n.is(S, "f")) {
                    return Err(invalid(
                        "XLSX formulas refused; cached values are not evaluated facts",
                    ));
                }
                let kind = cell.attr("t").unwrap_or("n");
                let value = if kind == "inlineStr" {
                    if cell.children.len() != 1 {
                        return Err(invalid("ambiguous XLSX inline cell"));
                    }
                    rich_string(cell.one(S, "is")?)?
                } else {
                    if cell.children.len() > 1 || cell.children.iter().any(|n| !n.is(S, "v")) {
                        return Err(invalid("unsupported XLSX cell child"));
                    }
                    let value = cell
                        .children
                        .first()
                        .map(leaf_text)
                        .transpose()?
                        .unwrap_or("");
                    match kind {
                        "s" => {
                            let index = value
                                .parse::<usize>()
                                .map_err(|_| invalid("invalid XLSX shared string index"))?;
                            if index.to_string() != value {
                                return Err(invalid("noncanonical XLSX shared string index"));
                            }
                            shared
                                .get(index)
                                .ok_or_else(|| invalid("XLSX shared string index missing"))?
                                .clone()
                        }
                        "n" => {
                            if !value.is_empty() && !value.parse::<f64>().is_ok_and(f64::is_finite)
                            {
                                return Err(invalid("invalid XLSX raw numeric value"));
                            }
                            value.into()
                        }
                        "b" => {
                            if !matches!(value, "0" | "1") {
                                return Err(invalid("invalid XLSX boolean value"));
                            }
                            value.into()
                        }
                        "str" | "d" | "e" => value.into(),
                        _ => return Err(invalid("unsupported XLSX cell type")),
                    }
                };
                append(
                    &mut output,
                    &format!("row={r} cell={addr} type={kind} value={}", quoted(&value)?),
                )?;
            }
        }
    }
    Ok(output)
}

fn pptx(pkg: &Package, limits: OfficeLimits) -> Result<String, RuntimeError> {
    let presentation = pkg.main("ppt/presentation.xml", PPTX)?;
    if !presentation.is(P, "presentation") {
        return Err(invalid("unsupported PPTX presentation root/namespace"));
    }
    let slides = presentation.one(P, "sldIdLst")?;
    if slides.children.is_empty() || slides.children.len() > limits.max_units {
        return Err(invalid("PPTX slide allowance exceeded/empty"));
    }
    let rels = pkg.relationships("ppt/presentation.xml")?;
    let mut output = String::new();
    let mut items = 0;
    let mut ids = BTreeSet::new();
    let mut targets = BTreeSet::new();
    for (i, slide) in slides.children.iter().enumerate() {
        if !slide.is(P, "sldId") || !ids.insert(positive(slide.attr("id"), u32::MAX as usize)?) {
            return Err(invalid("invalid/duplicate PPTX slide ID"));
        }
        let target = related(&rels, slide.rid()?, "slide")?;
        if !targets.insert(target) {
            return Err(invalid("duplicate PPTX slide target"));
        }
        let root = pkg.typed(target, SLIDE)?;
        if !root.is(P, "sld") {
            return Err(invalid("unsupported PPTX slide root/namespace"));
        }
        append(
            &mut output,
            &format!(
                "[slide {} id={} part={} show={}]",
                i + 1,
                slide.attr("id").unwrap(),
                quoted(target)?,
                quoted(root.attr("show").unwrap_or("1"))?
            ),
        )?;
        let tree = root.one(P, "cSld")?.one(P, "spTree")?;
        let mut shape = 0;
        slide_tree(tree, &mut output, &mut items, &mut shape, limits)?;
    }
    Ok(output)
}
fn slide_tree(
    node: &Node,
    out: &mut String,
    items: &mut usize,
    shape: &mut usize,
    limits: OfficeLimits,
) -> Result<(), RuntimeError> {
    for child in &node.children {
        if child.is(P, "sp") || child.is(P, "graphicFrame") {
            *shape += 1;
            let index = *shape;
            if child.is(P, "sp") {
                let bodies = child
                    .children
                    .iter()
                    .filter(|n| n.is(P, "txBody"))
                    .collect::<Vec<_>>();
                if bodies.len() > 1 {
                    return Err(invalid("duplicate PPTX shape text body"));
                }
                if let Some(body) = bodies.first() {
                    for (i, p) in body.children.iter().filter(|n| n.is(A, "p")).enumerate() {
                        count(items, limits)?;
                        append(
                            out,
                            &format!(
                                "shape={index} paragraph={} text={}",
                                i + 1,
                                quoted(&paragraph(p)?)?
                            ),
                        )?;
                    }
                }
            } else {
                let graphic = child.one(A, "graphic")?;
                let data = graphic.one(A, "graphicData")?;
                let tables = data
                    .children
                    .iter()
                    .filter(|n| n.is(A, "tbl"))
                    .collect::<Vec<_>>();
                if tables.len() > 1 {
                    return Err(invalid("duplicate PPTX graphic table"));
                }
                if let Some(table) = tables.first() {
                    for (r, row) in table.children.iter().filter(|n| n.is(A, "tr")).enumerate() {
                        for (c, cell) in row.children.iter().filter(|n| n.is(A, "tc")).enumerate() {
                            count(items, limits)?;
                            let body = cell.one(A, "txBody")?;
                            let texts = body
                                .children
                                .iter()
                                .filter(|n| n.is(A, "p"))
                                .map(paragraph)
                                .collect::<Result<Vec<_>, _>>()?;
                            append(
                                out,
                                &format!(
                                    "shape={index} table_row={} table_cell={} text={}",
                                    r + 1,
                                    c + 1,
                                    quoted(&texts.join("\n"))?
                                ),
                            )?;
                        }
                    }
                }
            }
        } else if child.is(P, "grpSp") {
            slide_tree(child, out, items, shape, limits)?;
        }
    }
    Ok(())
}
fn paragraph(node: &Node) -> Result<String, RuntimeError> {
    let mut text = String::new();
    for child in &node.children {
        if child.is(A, "r") {
            text.push_str(leaf_text(child.one(A, "t")?)?);
        } else if child.is(A, "br") {
            text.push('\n');
        } else if child.is(A, "fld") {
            return Err(invalid("PPTX dynamic fields refused"));
        } else if !child.is(A, "pPr") && !child.is(A, "endParaRPr") {
            return Err(invalid("unsupported PPTX paragraph content"));
        }
    }
    Ok(text)
}
