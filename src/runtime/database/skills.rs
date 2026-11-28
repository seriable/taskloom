use super::*;
use crate::runtime::skills::*;
use std::collections::BTreeMap;

/// Inert content-addressed skills and transactional availability catalog.
#[derive(Clone)]
pub struct SqlSkillStore {
    db: SqlDatabase,
    namespace: DatabaseNamespace,
}
impl SqlSkillStore {
    pub(super) fn new(db: SqlDatabase, namespace: DatabaseNamespace) -> Self {
        Self { db, namespace }
    }
    fn catalog_key(&self) -> Result<RecordKey, RuntimeError> {
        RecordKey::new(self.namespace.clone(), "skill_catalog", "default")
    }
    fn object_key(&self, reference: &SkillRef) -> Result<RecordKey, RuntimeError> {
        reference.validate()?;
        RecordKey::new(self.namespace.clone(), "skill_object", &reference.digest)
    }
    pub async fn catalog(&self) -> Result<SkillCatalog, RuntimeError> {
        self.db
            .read(&self.catalog_key()?)
            .await?
            .map(parse_catalog)
            .transpose()
            .map(Option::unwrap_or_default)
    }
    async fn locked_catalog(
        &self,
        tx: &mut Transaction<'_, Any>,
    ) -> Result<SkillCatalog, RuntimeError> {
        parse_catalog(
            self.db
                .lock_record(tx, &self.catalog_key()?, &SkillCatalog::default())
                .await?,
        )
    }
    async fn save_catalog(
        &self,
        tx: &mut Transaction<'_, Any>,
        catalog: &SkillCatalog,
    ) -> Result<(), RuntimeError> {
        catalog.validate()?;
        let key = self.catalog_key()?;
        let body = self.db.serialize(&key, catalog)?;
        if body.len() > 4 * 1024 * 1024 {
            return Err(RuntimeError::StorageLimit(
                "SQL skill catalog exceeds 4MiB".into(),
            ));
        }
        self.db.replace(tx, &key, catalog.revision, &body).await
    }
    async fn object(
        &self,
        tx: &mut Transaction<'_, Any>,
        reference: &SkillRef,
    ) -> Result<SkillPackage, RuntimeError> {
        parse_object(
            self.db
                .read_tx(tx, &self.object_key(reference)?)
                .await?
                .ok_or_else(|| conflict("pinned SQL skill object unavailable"))?,
            reference,
        )
    }
    async fn resolve(
        &self,
        tx: &mut Transaction<'_, Any>,
        catalog: &SkillCatalog,
        reference: &SkillRef,
        retired: bool,
    ) -> Result<BTreeMap<SkillRef, SkillPackage>, RuntimeError> {
        let mut packages = BTreeMap::new();
        let mut pending = vec![(reference.clone(), 0)];
        let mut total = 0_usize;
        while let Some((reference, depth)) = pending.pop() {
            if packages.contains_key(&reference) {
                continue;
            }
            if depth > 8 || packages.len() >= 32 {
                return Err(config("skill dependency closure exceeds limits"));
            }
            let entry = catalog
                .entries
                .get(&reference.catalog_key())
                .ok_or_else(|| conflict("install exact skill dependencies first"))?;
            if entry.reference != reference || !retired && entry.retired {
                return Err(conflict("pinned skill unavailable for new activation"));
            }
            let package = self.object(tx, &reference).await?;
            total = total.saturating_add(package.files.values().map(String::len).sum::<usize>());
            if total > 16 * 1024 * 1024 {
                return Err(config("skill dependency content exceeds 16MiB"));
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
    pub async fn install(
        &self,
        package: SkillPackage,
        expected: u64,
    ) -> Result<SkillCatalog, RuntimeError> {
        let reference = package.reference()?;
        self.db.schema().await?;
        let mut tx = self.db.write_transaction().await?;
        let mut catalog = self.locked_catalog(&mut tx).await?;
        if catalog.revision != expected {
            return Err(conflict("skill catalog revision changed"));
        }
        if let Some(previous) = catalog.entries.get(&reference.catalog_key()) {
            if previous.reference != reference {
                return Err(conflict(
                    "a package version is immutable; choose a new version",
                ));
            }
            if !previous.retired {
                self.object(&mut tx, &reference).await?;
                tx.commit().await.map_err(database_error)?;
                return Ok(catalog);
            }
        }
        for dependency in &package.manifest.dependencies {
            self.resolve(&mut tx, &catalog, dependency, false).await?;
        }
        if !catalog.entries.contains_key(&reference.catalog_key()) && catalog.entries.len() >= 256 {
            return Err(RuntimeError::StorageLimit(
                "skill catalog version limit reached".into(),
            ));
        }
        if catalog
            .events
            .len()
            .saturating_add(catalog.entries.values().filter(|e| !e.retired).count())
            .saturating_add(2)
            > 20_000
        {
            return Err(RuntimeError::StorageLimit(
                "skill catalog must retain retirement capacity".into(),
            ));
        }
        let key = self.object_key(&reference)?;
        if let Some(row) = self.db.read_tx(&mut tx, &key).await? {
            if parse_object(row, &reference)? != package {
                return Err(storage("skill object collision"));
            }
        } else {
            let body = self.db.serialize(&key, &package)?;
            if body.len() > 16 * 1024 * 1024 {
                return Err(RuntimeError::StorageLimit(
                    "SQL skill object exceeds 16MiB".into(),
                ));
            }
            self.db.replace(&mut tx, &key, 0, &body).await?;
        }
        catalog.entries.insert(
            reference.catalog_key(),
            SkillCatalogEntry {
                reference: reference.clone(),
                retired: false,
            },
        );
        catalog.revision += 1;
        catalog.events.push(SkillCatalogEvent {
            revision: catalog.revision,
            action: "install".into(),
            reference,
        });
        self.save_catalog(&mut tx, &catalog).await?;
        tx.commit().await.map_err(database_error)?;
        Ok(catalog)
    }
    pub async fn retire(
        &self,
        reference: &SkillRef,
        expected: u64,
    ) -> Result<SkillCatalog, RuntimeError> {
        reference.validate()?;
        self.db.schema().await?;
        let mut tx = self.db.write_transaction().await?;
        let mut catalog = self.locked_catalog(&mut tx).await?;
        if catalog.revision != expected {
            return Err(conflict("skill catalog revision changed"));
        }
        let entry = catalog
            .entries
            .get(&reference.catalog_key())
            .ok_or_else(|| conflict("package is not installed"))?;
        if entry.reference != *reference {
            return Err(conflict("skill digest differs"));
        }
        if entry.retired {
            tx.commit().await.map_err(database_error)?;
            return Ok(catalog);
        }
        for other in catalog
            .entries
            .values()
            .filter(|e| !e.retired && e.reference != *reference)
        {
            if self
                .object(&mut tx, &other.reference)
                .await?
                .manifest
                .dependencies
                .contains(reference)
            {
                return Err(conflict(
                    "retire installed dependents first; no implicit cascade",
                ));
            }
        }
        catalog
            .entries
            .get_mut(&reference.catalog_key())
            .expect("validated entry")
            .retired = true;
        catalog.revision += 1;
        catalog.events.push(SkillCatalogEvent {
            revision: catalog.revision,
            action: "retire".into(),
            reference: reference.clone(),
        });
        self.save_catalog(&mut tx, &catalog).await?;
        tx.commit().await.map_err(database_error)?;
        Ok(catalog)
    }
    pub async fn inspect(&self, reference: &SkillRef) -> Result<SkillPackage, RuntimeError> {
        parse_object(
            self.db
                .read(&self.object_key(reference)?)
                .await?
                .ok_or_else(|| conflict("pinned SQL skill object unavailable"))?,
            reference,
        )
    }
    pub async fn activate(
        &self,
        reference: &SkillRef,
        grant: SkillGrant,
        host: SkillHost,
        mode: SkillActivationMode,
    ) -> Result<ActivatedSkill, RuntimeError> {
        reference.validate()?;
        self.db.schema().await?;
        let mut tx = self.db.write_transaction().await?;
        let catalog = self.locked_catalog(&mut tx).await?;
        let packages = self
            .resolve(
                &mut tx,
                &catalog,
                reference,
                mode == SkillActivationMode::RecoveryOnly,
            )
            .await?;
        let activated = ActivatedSkill::new(reference.clone(), packages, grant, host, mode)?;
        tx.commit().await.map_err(database_error)?;
        Ok(activated)
    }
}
fn parse_catalog(row: Record) -> Result<SkillCatalog, RuntimeError> {
    let catalog: SkillCatalog = row.parse()?;
    catalog.validate()?;
    if catalog.revision != row.revision {
        return Err(storage("SQL skill catalog revision differs"));
    }
    Ok(catalog)
}
fn parse_object(row: Record, reference: &SkillRef) -> Result<SkillPackage, RuntimeError> {
    let package: SkillPackage = row.parse()?;
    if row.revision != 0 || package.reference()? != *reference {
        return Err(storage("SQL skill object identity differs"));
    }
    Ok(package)
}
