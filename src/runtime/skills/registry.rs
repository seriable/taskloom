//! Host-owned local publisher trust, signed releases and rollback watermarks.
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs::{File, OpenOptions};
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use ed25519_dalek::{Signature, VerifyingKey};
use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{
    hash, invalid, name, read_skill_archive, ActivatedSkill, FileSkillStore, SkillActivationMode,
    SkillCatalog, SkillGrant, SkillHost, SkillPackage, SkillRef,
};
use crate::runtime::{
    state::fingerprint,
    store::{atomic_write, blocking, storage},
    RuntimeError,
};
const STATE_MAX: u64 = 8 * 1024 * 1024;
const ARCHIVE_MAX: u64 = 8 * 1024 * 1024;

/// Trusted inert package/catalog backend. The registry's signed admission and
/// monotonic receipt journal remain separate from backend CAS/transactions.
pub trait RegistrySkillStore: Send + Sync {
    fn registry_catalog(&self) -> BoxFuture<'_, Result<SkillCatalog, RuntimeError>>;
    fn registry_install(
        &self,
        package: SkillPackage,
        expected: u64,
    ) -> BoxFuture<'_, Result<SkillCatalog, RuntimeError>>;
    fn registry_inspect<'a>(
        &'a self,
        reference: &'a SkillRef,
    ) -> BoxFuture<'a, Result<SkillPackage, RuntimeError>>;
    fn registry_activate<'a>(
        &'a self,
        reference: &'a SkillRef,
        grant: SkillGrant,
        host: SkillHost,
        mode: SkillActivationMode,
    ) -> BoxFuture<'a, Result<ActivatedSkill, RuntimeError>>;
}
impl RegistrySkillStore for FileSkillStore {
    fn registry_catalog(&self) -> BoxFuture<'_, Result<SkillCatalog, RuntimeError>> {
        Box::pin(self.catalog())
    }
    fn registry_install(
        &self,
        package: SkillPackage,
        expected: u64,
    ) -> BoxFuture<'_, Result<SkillCatalog, RuntimeError>> {
        Box::pin(self.install(package, expected))
    }
    fn registry_inspect<'a>(
        &'a self,
        reference: &'a SkillRef,
    ) -> BoxFuture<'a, Result<SkillPackage, RuntimeError>> {
        Box::pin(self.inspect(reference))
    }
    fn registry_activate<'a>(
        &'a self,
        reference: &'a SkillRef,
        grant: SkillGrant,
        host: SkillHost,
        mode: SkillActivationMode,
    ) -> BoxFuture<'a, Result<ActivatedSkill, RuntimeError>> {
        Box::pin(self.activate(reference, grant, host, mode))
    }
}
#[cfg(feature = "database")]
impl RegistrySkillStore for crate::runtime::database::SqlSkillStore {
    fn registry_catalog(&self) -> BoxFuture<'_, Result<SkillCatalog, RuntimeError>> {
        Box::pin(self.catalog())
    }
    fn registry_install(
        &self,
        package: SkillPackage,
        expected: u64,
    ) -> BoxFuture<'_, Result<SkillCatalog, RuntimeError>> {
        Box::pin(self.install(package, expected))
    }
    fn registry_inspect<'a>(
        &'a self,
        reference: &'a SkillRef,
    ) -> BoxFuture<'a, Result<SkillPackage, RuntimeError>> {
        Box::pin(self.inspect(reference))
    }
    fn registry_activate<'a>(
        &'a self,
        reference: &'a SkillRef,
        grant: SkillGrant,
        host: SkillHost,
        mode: SkillActivationMode,
    ) -> BoxFuture<'a, Result<ActivatedSkill, RuntimeError>> {
        Box::pin(self.activate(reference, grant, host, mode))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryPublisher {
    /// Public key IDs and lowercase hex Ed25519 public keys. Keys never grant tools.
    pub keys: BTreeMap<String, String>,
    pub revoked_keys: BTreeSet<String>,
    /// Exact package names this publisher may endorse, selected by the host.
    pub allowed_packages: BTreeSet<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillRegistryTrust {
    pub format: u32,
    pub revision: u64,
    pub publishers: BTreeMap<String, RegistryPublisher>,
    pub revoked_digests: BTreeSet<String>,
}
impl SkillRegistryTrust {
    pub fn validate(&self) -> Result<(), RuntimeError> {
        if self.format != 1
            || self.revision == 0
            || self.publishers.len() > 64
            || self.revoked_digests.len() > 4096
        {
            return Err(invalid("invalid registry trust version/limits"));
        }
        let mut owners = BTreeSet::new();
        for (publisher, policy) in &self.publishers {
            name(publisher)?;
            if policy.keys.len() > 64
                || policy.revoked_keys.len() > 64
                || policy.allowed_packages.len() > 256
            {
                return Err(invalid("oversized publisher trust policy"));
            }
            for (id, key) in &policy.keys {
                name(id)?;
                let bytes = decode::<32>(key)?;
                VerifyingKey::from_bytes(&bytes)
                    .map_err(|_| invalid("invalid Ed25519 publisher key"))?;
            }
            for id in &policy.revoked_keys {
                name(id)?;
            }
            for package in &policy.allowed_packages {
                name(package)?;
                if !owners.insert(package) {
                    return Err(invalid(
                        "package has multiple publisher owners in host trust",
                    ));
                }
            }
        }
        for digest in &self.revoked_digests {
            hash(digest)?;
        }
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillRegistryRelease {
    pub format: u32,
    pub publisher: String,
    pub key_id: String,
    pub trust_revision: u64,
    /// Monotonic per publisher/package, including across key rotation.
    pub sequence: u64,
    pub expires_unix: u64,
    pub package: SkillRef,
    pub archive_sha256: String,
    pub archive_bytes: u64,
}
impl SkillRegistryRelease {
    pub fn validate(&self) -> Result<(), RuntimeError> {
        name(&self.publisher)?;
        name(&self.key_id)?;
        self.package.validate()?;
        hash(&self.archive_sha256)?;
        if self.format != 1
            || self.trust_revision == 0
            || self.sequence == 0
            || self.expires_unix == 0
            || !(1024..=ARCHIVE_MAX).contains(&self.archive_bytes)
            || self.archive_bytes % 512 != 0
        {
            return Err(invalid("invalid signed release version/limits"));
        }
        Ok(())
    }
    /// Canonical ASCII JSON array; no object ordering, whitespace or Unicode
    /// normalization ambiguity. This is encoding, not a custom crypto primitive.
    pub fn signing_bytes(&self) -> Result<Vec<u8>, RuntimeError> {
        self.validate()?;
        serde_json::to_vec(&(
            "taskloom-skill-registry-v1",
            self.format,
            &self.publisher,
            &self.key_id,
            self.trust_revision,
            self.sequence,
            self.expires_unix,
            &self.package.name,
            &self.package.version,
            &self.package.digest,
            &self.archive_sha256,
            self.archive_bytes,
        ))
        .map_err(storage)
    }
    fn watermark_key(&self) -> String {
        format!("{}/{}", self.publisher, self.package.name)
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedSkillRelease {
    pub manifest: SkillRegistryRelease,
    pub signature: String,
}
impl SignedSkillRelease {
    /// Signature/host policy verification. New installation binds exact trust
    /// revision; activation may use a retained key under newer current trust.
    pub fn verify(
        &self,
        trust: &SkillRegistryTrust,
        now_unix: u64,
        new_install: bool,
    ) -> Result<(), RuntimeError> {
        trust.validate()?;
        self.manifest.validate()?;
        let m = &self.manifest;
        if now_unix >= m.expires_unix
            || (new_install && m.trust_revision != trust.revision)
            || m.trust_revision > trust.revision
            || trust.revoked_digests.contains(&m.package.digest)
        {
            return Err(invalid(
                "release expired, revoked or trust revision differs",
            ));
        }
        let publisher = trust
            .publishers
            .get(&m.publisher)
            .ok_or_else(|| invalid("publisher is not host trusted"))?;
        if !publisher.allowed_packages.contains(&m.package.name)
            || publisher.revoked_keys.contains(&m.key_id)
        {
            return Err(invalid("publisher package/key is not authorized"));
        }
        let encoded = publisher
            .keys
            .get(&m.key_id)
            .ok_or_else(|| invalid("publisher key is not trusted"))?;
        let key = VerifyingKey::from_bytes(&decode::<32>(encoded)?)
            .map_err(|_| invalid("invalid publisher key"))?;
        key.verify_strict(
            &m.signing_bytes()?,
            &Signature::from_bytes(&decode::<64>(&self.signature)?),
        )
        .map_err(|_| invalid("signed skill release verification failed"))
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryInstallReceipt {
    pub release: SignedSkillRelease,
    pub installed: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryWatermark {
    pub sequence: u64,
    pub package: SkillRef,
    pub release_fingerprint: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillRegistrySnapshot {
    pub format: u32,
    pub revision: u64,
    pub trust: Option<SkillRegistryTrust>,
    pub watermarks: BTreeMap<String, RegistryWatermark>,
    pub releases: BTreeMap<String, RegistryInstallReceipt>,
}
impl Default for SkillRegistrySnapshot {
    fn default() -> Self {
        Self {
            format: 1,
            revision: 0,
            trust: None,
            watermarks: BTreeMap::new(),
            releases: BTreeMap::new(),
        }
    }
}
impl SkillRegistrySnapshot {
    fn validate(&self) -> Result<(), RuntimeError> {
        if self.format != 1 || self.watermarks.len() > 1024 || self.releases.len() > 4096 {
            return Err(storage("invalid or oversized registry snapshot"));
        }
        if self.trust.is_none()
            && (self.revision != 0 || !self.watermarks.is_empty() || !self.releases.is_empty())
        {
            return Err(storage("registry state has no trust history"));
        }
        if let Some(trust) = &self.trust {
            trust.validate()?;
            if trust.revision > self.revision {
                return Err(storage("registry trust revision is inconsistent"));
            }
        }
        for (digest, receipt) in &self.releases {
            receipt.release.manifest.validate()?;
            decode::<64>(&receipt.release.signature)?;
            if digest != &receipt.release.manifest.package.digest {
                return Err(storage("registry release identity mismatch"));
            }
        }
        for (key, watermark) in &self.watermarks {
            watermark.package.validate()?;
            hash(&watermark.release_fingerprint)?;
            let receipt = self
                .releases
                .get(&watermark.package.digest)
                .ok_or_else(|| storage("registry watermark has no release"))?;
            if key != &receipt.release.manifest.watermark_key()
                || watermark.sequence != receipt.release.manifest.sequence
                || watermark.release_fingerprint != fingerprint(&receipt.release)?
            {
                return Err(storage("registry watermark differs from release"));
            }
        }
        Ok(())
    }
}
/// Host-supplied local files and CAS revisions; never a model-facing grant API.
pub struct RegistryInstallRequest {
    pub directory: PathBuf,
    pub metadata_file: String,
    pub archive_file: String,
    pub expected_registry_revision: u64,
    pub expected_catalog_revision: u64,
    pub now_unix: u64,
}
#[derive(Debug, Clone)]
pub struct FileSkillRegistry {
    root: PathBuf,
}
impl FileSkillRegistry {
    /// Absolute, protected host state directory. A registry state reset or
    /// restoration to an old backup also resets rollback memory; protect it.
    pub fn new(root: impl Into<PathBuf>) -> Result<Self, RuntimeError> {
        let root = root.into();
        if !root.is_absolute() {
            return Err(invalid("registry state root must be absolute"));
        }
        check_path(&root, true)?;
        Ok(Self { root })
    }
    pub async fn snapshot(&self) -> Result<SkillRegistrySnapshot, RuntimeError> {
        let root = self.root.clone();
        blocking(move || {
            let _lock = lock(&root)?;
            read_state(&root)
        })
        .await
    }
    /// Host administration only. Revision must advance exactly once, retaining
    /// publisher/package watermarks. Remove/revoke old keys explicitly on rotation.
    pub async fn set_trust(
        &self,
        trust: SkillRegistryTrust,
        expected: u64,
    ) -> Result<SkillRegistrySnapshot, RuntimeError> {
        trust.validate()?;
        let root = self.root.clone();
        blocking(move || {
            let _lock = lock(&root)?;
            let mut state = read_state(&root)?;
            if state.revision != expected {
                return Err(conflict("registry revision changed"));
            }
            let current = state.trust.as_ref().map_or(0, |t| t.revision);
            if trust.revision
                != current
                    .checked_add(1)
                    .ok_or_else(|| storage("trust revision exhausted"))?
            {
                return Err(conflict("trust revision must advance by exactly one"));
            }
            state.trust = Some(trust);
            persist(&root, &mut state)?;
            Ok(state)
        })
        .await
    }
    /// Verifies bounded local metadata/archive before any skill mutation. A
    /// durable release watermark precedes installation; interruption retains a
    /// pending receipt and permits only an explicit exact-release retry.
    pub async fn install_from_directory<S: RegistrySkillStore + ?Sized>(
        &self,
        store: &S,
        request: RegistryInstallRequest,
    ) -> Result<SkillRegistrySnapshot, RuntimeError> {
        let catalog = store.registry_catalog().await?;
        if catalog.revision != request.expected_catalog_revision {
            return Err(conflict(
                "skill catalog revision changed before registry verification",
            ));
        }
        let root = self.root.clone();
        let RegistryInstallRequest {
            directory,
            metadata_file,
            archive_file,
            expected_registry_revision,
            expected_catalog_revision,
            now_unix,
        } = request;
        let (guard, mut state, package, release) = blocking(move || {
            let guard = lock(&root)?;
            let mut state = read_state(&root)?;
            if state.revision != expected_registry_revision {
                return Err(conflict("registry revision changed"));
            }
            filename(&metadata_file)?;
            filename(&archive_file)?;
            if !directory.is_absolute() {
                return Err(invalid("registry source directory must be absolute"));
            }
            check_path(&directory, false)?;
            let release: SignedSkillRelease =
                serde_json::from_slice(&read_file(&directory.join(metadata_file), 128 * 1024)?)
                    .map_err(|_| invalid("invalid signed release JSON"))?;
            let trust = state
                .trust
                .as_ref()
                .ok_or_else(|| invalid("registry trust is not configured"))?;
            let exact_retry = state
                .releases
                .get(&release.manifest.package.digest)
                .is_some_and(|receipt| receipt.release == release);
            release.verify(trust, now_unix, !exact_retry)?;
            let archive = read_file(&directory.join(archive_file), ARCHIVE_MAX)?;
            if archive.len() as u64 != release.manifest.archive_bytes
                || hex::encode(Sha256::digest(&archive)) != release.manifest.archive_sha256
            {
                return Err(invalid(
                    "registry archive size/hash differs from signed release",
                ));
            }
            let package = read_skill_archive(&archive)?;
            if package.reference()? != release.manifest.package {
                return Err(invalid(
                    "archive package identity differs from signed release",
                ));
            }
            if catalog
                .entries
                .get(&release.manifest.package.catalog_key())
                .is_some_and(|entry| entry.reference != release.manifest.package)
            {
                return Err(conflict("installed package version is immutable"));
            }
            for previous in state
                .watermarks
                .values()
                .filter(|entry| entry.package.name == release.manifest.package.name)
            {
                if version_tuple(&release.manifest.package.version)?
                    < version_tuple(&previous.package.version)?
                {
                    return Err(conflict(
                        "registry package version rollback across publisher ownership refused",
                    ));
                }
            }
            let key = release.manifest.watermark_key();
            let signed_hash = fingerprint(&release)?;
            if let Some(previous) = state.watermarks.get(&key) {
                if release.manifest.sequence < previous.sequence
                    || version_tuple(&release.manifest.package.version)?
                        < version_tuple(&previous.package.version)?
                {
                    return Err(conflict("registry sequence/version rollback refused"));
                }
                if release.manifest.sequence == previous.sequence
                    && signed_hash != previous.release_fingerprint
                {
                    return Err(conflict("registry sequence is immutable"));
                }
            }
            if state
                .releases
                .get(&release.manifest.package.digest)
                .is_some_and(|old| old.release != release)
            {
                return Err(conflict("package digest endorsement is immutable"));
            }
            if !state.watermarks.contains_key(&key) && state.watermarks.len() >= 1024
                || !state
                    .releases
                    .contains_key(&release.manifest.package.digest)
                    && state.releases.len() >= 4096
            {
                return Err(invalid("registry release capacity exceeded"));
            }
            state.watermarks.insert(
                key,
                RegistryWatermark {
                    sequence: release.manifest.sequence,
                    package: release.manifest.package.clone(),
                    release_fingerprint: signed_hash,
                },
            );
            state
                .releases
                .entry(release.manifest.package.digest.clone())
                .or_insert_with(|| RegistryInstallReceipt {
                    release: release.clone(),
                    installed: false,
                });
            persist(&root, &mut state)?;
            Ok((guard, state, package, release))
        })
        .await?;
        // Holding the registry lock prevents concurrent trust changes/new admission.
        // The existing catalog parser/store retains independent CAS and host grants.
        store
            .registry_install(package, expected_catalog_revision)
            .await?;
        state
            .releases
            .get_mut(&release.manifest.package.digest)
            .expect("reserved release")
            .installed = true;
        persist(&self.root, &mut state)?;
        drop(guard);
        Ok(state)
    }
    /// Fresh trust checks precede new activation, including every dependency.
    /// RecoveryOnly preserves explicitly pinned historical objects without
    /// granting admission to new workflows; original host contracts still apply.
    pub async fn activate<S: RegistrySkillStore + ?Sized>(
        &self,
        store: &S,
        reference: &SkillRef,
        grant: SkillGrant,
        host: SkillHost,
        mode: SkillActivationMode,
        now_unix: u64,
    ) -> Result<ActivatedSkill, RuntimeError> {
        let root = self.root.clone();
        let (guard, state) = blocking(move || {
            let guard = lock(&root)?;
            Ok((guard, read_state(&root)?))
        })
        .await?;
        let mut pending = VecDeque::from([reference.clone()]);
        let mut seen = BTreeSet::new();
        while let Some(pin) = pending.pop_front() {
            if !seen.insert(pin.clone()) {
                continue;
            }
            if seen.len() > 32 {
                return Err(invalid("registry activation dependency limit exceeded"));
            }
            let receipt = state
                .releases
                .get(&pin.digest)
                .filter(|r| r.installed && r.release.manifest.package == pin)
                .ok_or_else(|| {
                    invalid("package has no completed verified registry installation")
                })?;
            if mode == SkillActivationMode::Active {
                receipt.release.verify(
                    state
                        .trust
                        .as_ref()
                        .ok_or_else(|| invalid("registry trust missing"))?,
                    now_unix,
                    false,
                )?;
                let latest = state
                    .watermarks
                    .get(&receipt.release.manifest.watermark_key())
                    .ok_or_else(|| invalid("registry watermark missing"))?;
                if latest.package != pin {
                    return Err(conflict(
                        "new activation of an older registry release is refused",
                    ));
                }
            }
            pending.extend(store.registry_inspect(&pin).await?.manifest.dependencies);
        }
        let active = store
            .registry_activate(reference, grant, host, mode)
            .await?;
        drop(guard);
        Ok(active)
    }
}
fn version_tuple(version: &str) -> Result<(u32, u32, u32), RuntimeError> {
    super::version(version)?;
    let parts = version
        .split('.')
        .map(|v| v.parse::<u32>().expect("validated version"))
        .collect::<Vec<_>>();
    Ok((parts[0], parts[1], parts[2]))
}
fn decode<const N: usize>(value: &str) -> Result<[u8; N], RuntimeError> {
    if value.len() != N * 2
        || !value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(invalid("invalid lowercase hex registry key/signature"));
    }
    let mut bytes = [0u8; N];
    hex::decode_to_slice(value, &mut bytes).map_err(|_| invalid("invalid registry hex"))?;
    Ok(bytes)
}
fn filename(value: &str) -> Result<(), RuntimeError> {
    if value.is_empty()
        || value.len() > 128
        || matches!(value, "." | "..")
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
    {
        Err(invalid(
            "registry filename must be one safe local component",
        ))
    } else {
        Ok(())
    }
}
fn check_path(path: &Path, allow_missing: bool) -> Result<(), RuntimeError> {
    if !path.is_absolute() {
        return Err(invalid("registry path must be absolute"));
    }
    let mut current = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir | Component::CurDir => {
                return Err(invalid("registry path traversal refused"))
            }
            Component::Prefix(_) | Component::RootDir => {
                current.push(component.as_os_str());
                continue;
            }
            Component::Normal(_) => current.push(component.as_os_str()),
        }
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || is_reparse(&metadata) {
                    return Err(invalid("registry symlink/reparse point refused"));
                }
            }
            Err(error) if allow_missing && error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(invalid("registry path unavailable")),
        }
    }
    Ok(())
}
#[cfg(windows)]
fn is_reparse(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    metadata.file_attributes() & 0x400 != 0
}
#[cfg(not(windows))]
fn is_reparse(_: &std::fs::Metadata) -> bool {
    false
}
fn read_file(path: &Path, max: u64) -> Result<Vec<u8>, RuntimeError> {
    check_path(path, false)?;
    let file = File::open(path).map_err(|_| invalid("registry file unavailable"))?;
    if !file.metadata().map_err(storage)?.is_file() {
        return Err(invalid("registry input must be a regular file"));
    }
    let mut data = Vec::new();
    file.take(max + 1).read_to_end(&mut data).map_err(storage)?;
    if data.len() as u64 > max {
        return Err(invalid("registry file exceeds limit"));
    }
    Ok(data)
}
fn read_state(root: &Path) -> Result<SkillRegistrySnapshot, RuntimeError> {
    let path = root.join("registry.json");
    let state = match std::fs::symlink_metadata(&path) {
        Ok(_) => serde_json::from_slice(&read_file(&path, STATE_MAX)?)
            .map_err(|_| storage("malformed registry snapshot"))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            SkillRegistrySnapshot::default()
        }
        Err(_) => return Err(storage("registry snapshot cannot be inspected")),
    };
    state.validate()?;
    Ok(state)
}
fn persist(root: &Path, state: &mut SkillRegistrySnapshot) -> Result<(), RuntimeError> {
    state.revision = state
        .revision
        .checked_add(1)
        .ok_or_else(|| storage("registry revision exhausted"))?;
    state.validate()?;
    check_path(&root.join("registry.json"), true)?;
    atomic_write(&root.join("registry.json"), state, STATE_MAX)
}
struct Guard(File);
impl Drop for Guard {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.0);
    }
}
fn lock(root: &Path) -> Result<Guard, RuntimeError> {
    check_path(root, true)?;
    std::fs::create_dir_all(root).map_err(storage)?;
    let path = root.join("registry.lock");
    check_path(&path, true)?;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
        .map_err(storage)?;
    fs2::FileExt::try_lock_exclusive(&file)
        .map_err(|_| conflict("registry is owned by another operation"))?;
    Ok(Guard(file))
}
fn conflict(detail: &str) -> RuntimeError {
    RuntimeError::Conflict(detail.into())
}
