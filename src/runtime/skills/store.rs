use super::*;
use crate::runtime::store::{absolute_root, atomic_write, blocking, read_json, storage};
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillCatalogEntry {
    pub reference: SkillRef,
    pub retired: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillCatalogEvent {
    pub revision: u64,
    pub action: String,
    pub reference: SkillRef,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillCatalog {
    pub format: u32,
    pub revision: u64,
    pub entries: BTreeMap<String, SkillCatalogEntry>,
    pub events: Vec<SkillCatalogEvent>,
}
impl Default for SkillCatalog {
    fn default() -> Self {
        Self {
            format: 1,
            revision: 0,
            entries: BTreeMap::new(),
            events: Vec::new(),
        }
    }
}
impl SkillCatalog {
    pub(crate) fn validate(&self) -> Result<(), RuntimeError> {
        if self.format != 1
            || self.revision != self.events.len() as u64
            || self.events.len() > 20_000
            || self.entries.len() > 256
            || self
                .events
                .len()
                .saturating_add(self.entries.values().filter(|e| !e.retired).count())
                > 20_000
        {
            return Err(storage("invalid or oversized skill catalog"));
        }
        for (key, entry) in &self.entries {
            entry.reference.validate()?;
            if key != &entry.reference.catalog_key() {
                return Err(storage("skill catalog identity mismatch"));
            }
        }
        for (i, event) in self.events.iter().enumerate() {
            event.reference.validate()?;
            if event.revision != i as u64 + 1
                || !matches!(event.action.as_str(), "install" | "retire")
            {
                return Err(storage("invalid skill catalog audit"));
            }
        }
        Ok(())
    }
}
#[derive(Debug, Clone)]
pub struct FileSkillStore {
    root: PathBuf,
    root_error: Option<String>,
}
impl FileSkillStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let (root, root_error) = absolute_root(root.into());
        Self { root, root_error }
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    fn checked_root(&self) -> Result<PathBuf, RuntimeError> {
        if let Some(e) = &self.root_error {
            return Err(storage(e));
        }
        Ok(self.root.clone())
    }
    pub async fn catalog(&self) -> Result<SkillCatalog, RuntimeError> {
        let root = self.checked_root()?;
        blocking(move || catalog(&root)).await
    }
    /// Parsing and hashing precede any mutation. Installed objects are inert
    /// JSON blobs addressed only by their digest; archive paths never escape.
    pub async fn install(
        &self,
        package: SkillPackage,
        expected: u64,
    ) -> Result<SkillCatalog, RuntimeError> {
        let reference = package.reference()?;
        let root = self.checked_root()?;
        blocking(move || {
            let _lock = lock(&root)?;
            let mut current = catalog(&root)?;
            if current.revision != expected {
                return Err(conflict("skill catalog revision changed"));
            }
            if let Some(previous) = current.entries.get(&reference.catalog_key()) {
                if previous.reference != reference {
                    return Err(conflict(
                        "a package version is immutable; choose a new version",
                    ));
                }
                if !previous.retired {
                    object(&root, &reference)?;
                    return Ok(current);
                }
            }
            for dependency in &package.manifest.dependencies {
                resolve(&root, &current, dependency, false)?;
            }
            if !current.entries.contains_key(&reference.catalog_key())
                && current.entries.len() >= 256
            {
                return Err(RuntimeError::StorageLimit(
                    "skill catalog version limit reached".into(),
                ));
            }
            if current
                .events
                .len()
                .saturating_add(current.entries.values().filter(|e| !e.retired).count())
                .saturating_add(2)
                > 20_000
            {
                return Err(RuntimeError::StorageLimit(
                    "skill catalog must retain retirement audit capacity".into(),
                ));
            }
            let path = root
                .join("objects")
                .join(format!("{}.skill.json", reference.digest));
            if path.exists() {
                if object(&root, &reference)? != package {
                    return Err(storage("skill object collision"));
                }
            } else {
                std::fs::create_dir_all(root.join("objects")).map_err(storage)?;
                atomic_write(&path, &package, 16 * 1024 * 1024)?;
            }
            current.entries.insert(
                reference.catalog_key(),
                SkillCatalogEntry {
                    reference: reference.clone(),
                    retired: false,
                },
            );
            current.revision += 1;
            current.events.push(SkillCatalogEvent {
                revision: current.revision,
                action: "install".into(),
                reference,
            });
            current.validate()?;
            atomic_write(&root.join("catalog.json"), &current, 4 * 1024 * 1024)?;
            Ok(current)
        })
        .await
    }
    /// Retire from new activation/selection. Content-addressed objects remain
    /// for explicit RecoveryOnly pins, audit and existing durable contracts.
    pub async fn retire(
        &self,
        reference: &SkillRef,
        expected: u64,
    ) -> Result<SkillCatalog, RuntimeError> {
        reference.validate()?;
        let reference = reference.clone();
        let root = self.checked_root()?;
        blocking(move || {
            let _lock = lock(&root)?;
            let mut current = catalog(&root)?;
            if current.revision != expected {
                return Err(conflict("skill catalog revision changed"));
            }
            let entry = current
                .entries
                .get(&reference.catalog_key())
                .ok_or_else(|| conflict("package is not installed"))?;
            if entry.reference != reference {
                return Err(conflict("package digest differs"));
            }
            if entry.retired {
                return Ok(current);
            }
            for other in current
                .entries
                .values()
                .filter(|e| !e.retired && e.reference != reference)
            {
                if object(&root, &other.reference)?
                    .manifest
                    .dependencies
                    .contains(&reference)
                {
                    return Err(conflict(
                        "retire installed dependents first; no implicit cascade",
                    ));
                }
            }
            current
                .entries
                .get_mut(&reference.catalog_key())
                .unwrap()
                .retired = true;
            current.revision += 1;
            current.events.push(SkillCatalogEvent {
                revision: current.revision,
                action: "retire".into(),
                reference,
            });
            current.validate()?;
            atomic_write(&root.join("catalog.json"), &current, 4 * 1024 * 1024)?;
            Ok(current)
        })
        .await
    }
    pub async fn inspect(&self, reference: &SkillRef) -> Result<SkillPackage, RuntimeError> {
        reference.validate()?;
        let reference = reference.clone();
        let root = self.checked_root()?;
        blocking(move || object(&root, &reference)).await
    }
    pub async fn activate(
        &self,
        reference: &SkillRef,
        grant: SkillGrant,
        host: SkillHost,
        mode: SkillActivationMode,
    ) -> Result<ActivatedSkill, RuntimeError> {
        reference.validate()?;
        let reference = reference.clone();
        let root = self.checked_root()?;
        blocking(move || {
            // Take the registry lock during resolution to avoid racing a
            // dependency retirement against one activation's availability.
            let _lock = lock(&root)?;
            let current = catalog(&root)?;
            let packages = resolve(
                &root,
                &current,
                &reference,
                mode == SkillActivationMode::RecoveryOnly,
            )?;
            ActivatedSkill::new(reference, packages, grant, host, mode)
        })
        .await
    }
}
fn catalog(root: &Path) -> Result<SkillCatalog, RuntimeError> {
    let current =
        read_json::<SkillCatalog>(&root.join("catalog.json"), 4 * 1024 * 1024)?.unwrap_or_default();
    current.validate()?;
    Ok(current)
}
fn object(root: &Path, reference: &SkillRef) -> Result<SkillPackage, RuntimeError> {
    reference.validate()?;
    let package = read_json::<SkillPackage>(
        &root
            .join("objects")
            .join(format!("{}.skill.json", reference.digest)),
        16 * 1024 * 1024,
    )?
    .ok_or_else(|| conflict("pinned skill object is unavailable"))?;
    if package.reference()? != *reference {
        return Err(storage("skill object digest/identity mismatch"));
    }
    Ok(package)
}
fn resolve(
    root: &Path,
    catalog: &SkillCatalog,
    reference: &SkillRef,
    retired: bool,
) -> Result<BTreeMap<SkillRef, SkillPackage>, RuntimeError> {
    let mut packages = BTreeMap::new();
    let mut pending = vec![(reference.clone(), 0)];
    let mut total = 0usize;
    while let Some((reference, depth)) = pending.pop() {
        if packages.contains_key(&reference) {
            continue;
        }
        if depth > 8 || packages.len() >= 32 {
            return Err(invalid("dependency closure exceeds limits"));
        }
        let entry = catalog
            .entries
            .get(&reference.catalog_key())
            .ok_or_else(|| conflict("install exact dependencies before this package"))?;
        if entry.reference != reference || (!retired && entry.retired) {
            return Err(conflict(
                "pinned dependency/version is unavailable for new activation",
            ));
        }
        let package = object(root, &reference)?;
        total = total.saturating_add(package.files.values().map(String::len).sum::<usize>());
        if total > 16 * 1024 * 1024 {
            return Err(invalid("dependency content exceeds 16 MiB"));
        }
        pending.extend(
            package
                .manifest
                .dependencies
                .iter()
                .cloned()
                .map(|d| (d, depth + 1)),
        );
        packages.insert(reference, package);
    }
    Ok(packages)
}
fn lock(root: &Path) -> Result<SkillLock, RuntimeError> {
    std::fs::create_dir_all(root).map_err(storage)?;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(root.join("catalog.lock"))
        .map_err(storage)?;
    fs2::FileExt::try_lock_exclusive(&file).map_err(|e| {
        if e.kind() == std::io::ErrorKind::WouldBlock || matches!(e.raw_os_error(), Some(11 | 33)) {
            conflict("skill catalog is owned by another writer")
        } else {
            storage(e)
        }
    })?;
    Ok(SkillLock(file))
}
struct SkillLock(File);
impl Drop for SkillLock {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.0);
    }
}
fn conflict(detail: &str) -> RuntimeError {
    RuntimeError::Conflict(detail.into())
}
