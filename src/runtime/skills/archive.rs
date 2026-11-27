use super::*;
use std::collections::{BTreeMap, BTreeSet};

/// Read a deliberately small, uncompressed POSIX USTAR subset into memory.
/// No path is extracted to disk. Links/devices/PAX/GNU extensions and ambiguous
/// trailing content are rejected, rather than delegated to a shell extractor.
pub fn read_skill_archive(bytes: &[u8]) -> Result<SkillPackage, RuntimeError> {
    if bytes.len() > 8 * 1024 * 1024 || bytes.len() < 1024 || bytes.len() % 512 != 0 {
        return Err(invalid(
            "archive must be bounded, uncompressed USTAR blocks",
        ));
    }
    let mut offset = 0;
    let mut count = 0;
    let mut total = 0;
    let mut paths = BTreeSet::new();
    let mut files = BTreeMap::new();
    let mut directories = BTreeSet::new();
    let mut ended = false;
    while offset + 512 <= bytes.len() {
        let header = &bytes[offset..offset + 512];
        offset += 512;
        if header.iter().all(|b| *b == 0) {
            if offset + 512 > bytes.len() || bytes[offset..].iter().any(|b| *b != 0) {
                return Err(invalid("archive needs two end blocks and no trailing data"));
            }
            ended = true;
            break;
        }
        count += 1;
        if count > 256 {
            return Err(invalid("too many archive entries"));
        }
        if &header[257..263] != b"ustar\0"
            || &header[263..265] != b"00"
            || header[157..257].iter().any(|b| *b != 0)
        {
            return Err(invalid("unsupported archive format or link target"));
        }
        let checksum = octal(&header[148..156])?;
        let actual = header
            .iter()
            .enumerate()
            .map(|(i, b)| {
                if (148..156).contains(&i) {
                    32u64
                } else {
                    *b as u64
                }
            })
            .sum::<u64>();
        if checksum != actual {
            return Err(invalid("archive header checksum mismatch"));
        }
        let size = usize::try_from(octal(&header[124..136])?)
            .map_err(|_| invalid("archive size overflow"))?;
        let base = field(&header[..100])?;
        let prefix = field(&header[345..500])?;
        let path = if prefix.is_empty() {
            base
        } else {
            format!("{prefix}/{base}")
        };
        let directory = header[156] == b'5';
        if !directory && !matches!(header[156], 0 | b'0') {
            return Err(invalid(
                "archive links, devices and extension records are forbidden",
            ));
        }
        let path = if directory {
            path.strip_suffix('/').unwrap_or(&path).to_string()
        } else {
            path
        };
        asset_path(&path)?;
        if !paths.insert(path.to_ascii_lowercase()) {
            return Err(invalid("duplicate/case-colliding archive path"));
        }
        if size > MAX_FILE_BYTES || (directory && size != 0) {
            return Err(invalid("archive entry exceeds its byte allowance"));
        }
        let end = offset
            .checked_add(size)
            .ok_or_else(|| invalid("archive offset overflow"))?;
        let padded = offset
            .checked_add(size.div_ceil(512) * 512)
            .ok_or_else(|| invalid("archive offset overflow"))?;
        if padded > bytes.len() || bytes[end..padded].iter().any(|b| *b != 0) {
            return Err(invalid("truncated archive or nonzero entry padding"));
        }
        if directory {
            directories.insert(path);
        } else {
            total += size;
            if total > MAX_CONTENT_BYTES || files.len() > MAX_FILES {
                return Err(invalid("archive contents exceed limits"));
            }
            let content = std::str::from_utf8(&bytes[offset..end])
                .map_err(|_| invalid("skill assets must be UTF-8 text"))?
                .to_string();
            files.insert(path, content);
        }
        offset = padded;
    }
    if !ended {
        return Err(invalid("archive end marker missing"));
    }
    for path in files.keys() {
        if directories.iter().any(|d| d.eq_ignore_ascii_case(path)) {
            return Err(invalid("archive file/directory collision"));
        }
    }
    let manifest = files
        .remove("skill.json")
        .ok_or_else(|| invalid("root skill.json is required"))?;
    if manifest.len() > 128 * 1024 {
        return Err(invalid("manifest exceeds 128 KiB"));
    }
    let package = SkillPackage {
        manifest: serde_json::from_str(&manifest).map_err(json_error)?,
        files,
    };
    package.validate()?;
    Ok(package)
}
fn field(bytes: &[u8]) -> Result<String, RuntimeError> {
    let end = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
    if bytes[end..].iter().any(|b| *b != 0) {
        return Err(invalid("nonzero bytes after archive field terminator"));
    }
    std::str::from_utf8(&bytes[..end])
        .map(str::to_owned)
        .map_err(|_| invalid("invalid archive text field"))
}
fn octal(bytes: &[u8]) -> Result<u64, RuntimeError> {
    let value = std::str::from_utf8(bytes)
        .map_err(|_| invalid("non-octal archive integer"))?
        .trim_matches(|c| c == '\0' || c == ' ');
    if value.is_empty() {
        return Ok(0);
    }
    if !value.bytes().all(|b| (b'0'..=b'7').contains(&b)) {
        return Err(invalid("unsupported archive integer encoding"));
    }
    u64::from_str_radix(value, 8).map_err(|_| invalid("archive integer overflow"))
}
