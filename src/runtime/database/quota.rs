//! Transactional admission shared by every SQL run writer in a project.
use super::*;
use std::collections::BTreeMap;

const MAX_IMPORT_ROWS: usize = 100_000;
const MAX_POLICY_EVENTS: usize = 1024;

/// Host prices and model adapters must use this declared currency unit. This
/// ledger does not perform currency conversion or include embedding/tool bills.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GenerationQuota {
    pub currency: String,
    pub max_cost_micros: u64,
    /// In-flight plus uncertain attempts, including zero-priced attempts.
    pub max_outstanding_attempts: u64,
}
impl GenerationQuota {
    fn validate(&self) -> Result<(), RuntimeError> {
        if self.currency.len() != 3 || !self.currency.bytes().all(|b| b.is_ascii_uppercase()) {
            return Err(config(
                "generation quota currency requires three uppercase ASCII letters",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GenerationQuotaUsage {
    pub runs: u64,
    pub committed_micros: u64,
    pub in_flight_micros: u64,
    pub uncertain_micros: u64,
    pub in_flight_attempts: u64,
    pub uncertain_attempts: u64,
}
impl GenerationQuotaUsage {
    pub fn held_micros(&self) -> Result<u64, RuntimeError> {
        add(self.committed_micros, self.in_flight_micros)
    }
    pub fn outstanding_attempts(&self) -> Result<u64, RuntimeError> {
        add(self.in_flight_attempts, self.uncertain_attempts)
    }
    fn exceeds(&self, policy: &GenerationQuota) -> Result<bool, RuntimeError> {
        Ok(self.held_micros()? > policy.max_cost_micros
            || self.outstanding_attempts()? > policy.max_outstanding_attempts)
    }
    fn replace(&mut self, old: &BudgetSnapshot, new: &BudgetSnapshot) -> Result<(), RuntimeError> {
        fn delta(total: &mut u64, old: u64, new: u64) -> Result<(), RuntimeError> {
            *total = add(
                total
                    .checked_sub(old)
                    .ok_or_else(|| storage("quota aggregate underflow"))?,
                new,
            )?;
            Ok(())
        }
        delta(
            &mut self.committed_micros,
            old.committed_micros,
            new.committed_micros,
        )?;
        delta(
            &mut self.in_flight_micros,
            old.in_flight_micros,
            new.in_flight_micros,
        )?;
        delta(
            &mut self.uncertain_micros,
            old.uncertain_micros,
            new.uncertain_micros,
        )?;
        delta(
            &mut self.in_flight_attempts,
            old.in_flight_attempts,
            new.in_flight_attempts,
        )?;
        delta(
            &mut self.uncertain_attempts,
            old.uncertain_attempts,
            new.uncertain_attempts,
        )?;
        self.held_micros()?;
        self.outstanding_attempts()?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GenerationQuotaEvent {
    pub policy_revision: u64,
    pub operator: String,
    pub note: String,
    pub unix_ms: i64,
    pub policy: Option<GenerationQuota>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GenerationQuotaSnapshot {
    /// Changes with accounting as well as administration.
    pub revision: u64,
    /// Expected revision for explicit policy CAS; usage updates do not race it.
    pub policy_revision: u64,
    /// Frozen on first configuration, including while enforcement is disabled.
    pub currency: Option<String>,
    pub policy: Option<GenerationQuota>,
    /// None means history has never been imported, not known zero spending.
    pub usage: Option<GenerationQuotaUsage>,
    /// Settlement/import exceeded policy. Only an explicit policy review can
    /// clear this; a settlement is always recorded rather than rolled back.
    pub overrun: bool,
    pub events: Vec<GenerationQuotaEvent>,
}
impl GenerationQuotaSnapshot {
    fn validate(&self) -> Result<(), RuntimeError> {
        if self.events.len() > MAX_POLICY_EVENTS
            || self.policy_revision != self.events.len() as u64
            || self.revision < self.policy_revision
            || self.usage.is_some() != self.currency.is_some()
            || self.usage.is_none()
                && (self.policy.is_some() || self.overrun || !self.events.is_empty())
        {
            return Err(storage("invalid generation quota control record"));
        }
        if let Some(currency) = &self.currency {
            GenerationQuota {
                currency: currency.clone(),
                max_cost_micros: 0,
                max_outstanding_attempts: 0,
            }
            .validate()?;
        }
        if let Some(policy) = &self.policy {
            policy.validate()?;
            if Some(&policy.currency) != self.currency.as_ref() {
                return Err(storage("generation quota currency changed"));
            }
        }
        for (index, event) in self.events.iter().enumerate() {
            audit_text(&event.operator, 128)?;
            audit_text(&event.note, 2048)?;
            if event.policy_revision != index as u64 + 1 || event.unix_ms <= 0 {
                return Err(storage("invalid generation quota audit revision/time"));
            }
            if let Some(policy) = &event.policy {
                policy.validate()?;
                if Some(&policy.currency) != self.currency.as_ref() {
                    return Err(storage("generation quota audit currency differs"));
                }
            }
        }
        if self
            .events
            .last()
            .is_some_and(|event| event.policy != self.policy)
        {
            return Err(storage("generation quota policy differs from audit"));
        }
        if let Some(usage) = &self.usage {
            usage.held_micros()?;
            usage.outstanding_attempts()?;
            if usage.uncertain_micros > usage.committed_micros {
                return Err(storage("invalid quota uncertainty aggregate"));
            }
        }
        Ok(())
    }
}

/// Trusted host administration, never a model tool or unauthenticated API.
#[derive(Clone)]
pub struct SqlGenerationQuotaStore {
    db: SqlDatabase,
    namespace: DatabaseNamespace,
}
impl SqlGenerationQuotaStore {
    pub(super) fn new(db: SqlDatabase, namespace: DatabaseNamespace) -> Self {
        Self { db, namespace }
    }
    fn key(&self) -> Result<RecordKey, RuntimeError> {
        RecordKey::new(self.namespace.clone(), "quota", "generation")
    }
    pub async fn snapshot(&self) -> Result<GenerationQuotaSnapshot, RuntimeError> {
        self.db
            .read(&self.key()?)
            .await?
            .map(parse)
            .transpose()
            .map(Option::unwrap_or_default)
    }
    async fn lock(
        &self,
        tx: &mut Transaction<'_, Any>,
    ) -> Result<GenerationQuotaSnapshot, RuntimeError> {
        parse(
            self.db
                .lock_record(tx, &self.key()?, &GenerationQuotaSnapshot::default())
                .await?,
        )
    }
    async fn write(
        &self,
        tx: &mut Transaction<'_, Any>,
        value: &mut GenerationQuotaSnapshot,
    ) -> Result<(), RuntimeError> {
        value.revision = add(value.revision, 1)?;
        value.validate()?;
        let key = self.key()?;
        self.db
            .replace(tx, &key, value.revision, &self.db.serialize(&key, value)?)
            .await
    }
    /// First configuration imports all existing run/budget generations in the
    /// same transaction. Disable explicitly with None; accounting never resets.
    /// Re-enabling/reviewing can clear overrun only when retained usage fits.
    pub async fn configure(
        &self,
        policy: Option<GenerationQuota>,
        expected_policy_revision: u64,
        operator: &str,
        note: &str,
    ) -> Result<GenerationQuotaSnapshot, RuntimeError> {
        audit_text(operator, 128)?;
        audit_text(note, 2048)?;
        if let Some(policy) = &policy {
            policy.validate()?;
        }
        self.db.schema().await?;
        let mut tx = self.db.write_transaction().await?;
        let mut control = self.lock(&mut tx).await?;
        if control.policy_revision != expected_policy_revision {
            return Err(conflict("generation quota policy revision changed"));
        }
        if control.events.len() == MAX_POLICY_EVENTS {
            return Err(RuntimeError::StorageLimit("generation quota audit limit reached; archive through an explicit future migration".into()));
        }
        if control.usage.is_none() {
            let policy = policy.as_ref().ok_or_else(|| {
                config("first quota configuration requires a currency and explicit limits")
            })?;
            control.currency = Some(policy.currency.clone());
            control.usage = Some(self.bootstrap(&mut tx).await?);
        }
        if let Some(policy) = &policy {
            if control.currency.as_ref() != Some(&policy.currency) {
                return Err(config(
                    "cannot change the currency of retained generation spending",
                ));
            }
            control.overrun = control.usage.as_ref().unwrap().exceeds(policy)?;
        }
        control.policy = policy.clone();
        control.policy_revision = add(control.policy_revision, 1)?;
        let now: i64 = sqlx::query_scalar(&format!("SELECT {}", self.db.0.backend.now()))
            .fetch_one(&mut *tx)
            .await
            .map_err(database_error)?;
        control.events.push(GenerationQuotaEvent {
            policy_revision: control.policy_revision,
            operator: operator.into(),
            note: note.into(),
            unix_ms: now,
            policy,
        });
        self.write(&mut tx, &mut control).await?;
        tx.commit().await.map_err(database_error)?;
        Ok(control)
    }
    async fn bootstrap(
        &self,
        tx: &mut Transaction<'_, Any>,
    ) -> Result<GenerationQuotaUsage, RuntimeError> {
        let retained_private: Option<i64> = sqlx::query_scalar("SELECT 1 FROM taskloom_records WHERE namespace=$1 AND family='quota_private_run' LIMIT 1")
            .bind(&self.namespace.0).fetch_optional(&mut **tx).await.map_err(database_error)?;
        if retained_private.is_some() {
            return Err(storage("uninitialized project quota contains retained private accounting; refusing incomplete history import"));
        }
        let query = format!("SELECT object_id,family,revision,{} AS payload FROM taskloom_records WHERE namespace=$1 AND family IN ($2,'budget') ORDER BY object_id,family LIMIT $3", self.db.0.backend.body());
        // Bounded stream; never use the UI's truncated 1000-record listing.
        let mut rows = sqlx::query(&query)
            .bind(&self.namespace.0)
            .bind("run")
            .bind((MAX_IMPORT_ROWS + 1) as i64)
            .bind((self.db.0.options.max_record_bytes + 1) as i32)
            .fetch(&mut **tx);
        let mut latest: BTreeMap<String, BudgetSnapshot> = BTreeMap::new();
        let mut count = 0;
        let mut bytes = 0_u64;
        while let Some(row) = rows.try_next().await.map_err(database_error)? {
            count += 1;
            if count > MAX_IMPORT_ROWS {
                return Err(RuntimeError::StorageLimit(
                    "generation quota import exceeds 100000 journal rows".into(),
                ));
            }
            let id: String = row.try_get("object_id").map_err(database_error)?;
            let family: String = row.try_get("family").map_err(database_error)?;
            let family = if family == "run" { "run" } else { "budget" };
            let record = decode_record(
                row,
                RecordKey::new(self.namespace.clone(), family, &id)?,
                self.db.0.options.max_record_bytes,
            )?;
            bytes = add(bytes, record.body.len() as u64)?;
            if bytes > MAX_BYTES {
                return Err(RuntimeError::StorageLimit(
                    "generation quota import exceeds 64MiB".into(),
                ));
            }
            let value = if family == "run" {
                parse_run(record, &id)?.report.budget
            } else {
                parse_budget(record)?
            };
            validate_budget(&value)?;
            if let Some(old) = latest.get(&id) {
                let newest = newest(old, &value)?;
                latest.insert(id, newest.clone());
            } else {
                latest.insert(id, value);
            }
        }
        drop(rows);
        let mut usage = GenerationQuotaUsage::default();
        for (id, budget) in latest {
            let key = RecordKey::new(self.namespace.clone(), "quota_run", &id)?;
            if self.db.read_tx(tx, &key).await?.is_some() {
                return Err(storage(
                    "uninitialized quota contains orphan accounting rows",
                ));
            }
            self.db
                .replace(
                    tx,
                    &key,
                    budget.generation,
                    &self.db.serialize(&key, &budget)?,
                )
                .await?;
            usage.runs = add(usage.runs, 1)?;
            usage.replace(&BudgetSnapshot::default(), &budget)?;
        }
        Ok(usage)
    }
    /// Called under the runtime ownership lock, before updating its journal.
    /// A Budget error here always rolls back the entire admission transaction.
    pub(super) async fn reconcile(
        &self,
        tx: &mut Transaction<'_, Any>,
        run_id: &str,
        value: &BudgetSnapshot,
        admission: bool,
    ) -> Result<(), RuntimeError> {
        self.reconcile_account(tx, "quota_run", run_id, value, admission)
            .await
    }
    pub(super) async fn private_scope_admission(
        &self,
        tx: &mut Transaction<'_, Any>,
        create: bool,
    ) -> Result<(), RuntimeError> {
        let control = self.lock(tx).await?;
        if control.usage.is_none() || control.currency.is_none() {
            return Err(config(
                "private stores require explicitly initialized project generation quota",
            ));
        }
        if create && control.policy.is_none() {
            return Err(RuntimeError::Budget(
                "new private stores require enabled project generation quota".into(),
            ));
        }
        Ok(())
    }
    pub(super) async fn reconcile_private(
        &self,
        tx: &mut Transaction<'_, Any>,
        owner: &DatabaseNamespace,
        run_id: &str,
        value: &BudgetSnapshot,
        admission: bool,
    ) -> Result<(), RuntimeError> {
        let id =
            crate::runtime::state::fingerprint(&("private-project-account-v1", &owner.0, run_id))?;
        self.reconcile_account(tx, "quota_private_run", &id, value, admission)
            .await
    }
    async fn reconcile_account(
        &self,
        tx: &mut Transaction<'_, Any>,
        family: &'static str,
        account_id: &str,
        value: &BudgetSnapshot,
        admission: bool,
    ) -> Result<(), RuntimeError> {
        let private = family == "quota_private_run";
        validate_budget(value)?;
        let mut control = self.lock(tx).await?;
        if private && control.usage.is_none() {
            return Err(storage(
                "private accounting cannot bypass uninitialized project quota",
            ));
        }
        if private && admission && value.in_flight_attempts > 0 && control.policy.is_none() {
            return Err(RuntimeError::Budget("private generation refused while project quota is disabled; existing uncertainty remains retained".into()));
        }
        let Some(mut usage) = control.usage.clone() else {
            return Ok(());
        };
        let key = RecordKey::new(self.namespace.clone(), family, account_id)?;
        let old = self
            .db
            .read_tx(tx, &key)
            .await?
            .map(parse_budget)
            .transpose()?;
        if let Some(old) = &old {
            validate_budget(old)?;
            let latest = newest(old, value)?;
            if latest == old {
                return Ok(());
            }
        }
        let empty = BudgetSnapshot::default();
        usage.replace(old.as_ref().unwrap_or(&empty), value)?;
        if old.is_none() {
            usage.runs = add(usage.runs, 1)?;
        }
        if let Some(policy) = &control.policy {
            let exceeds = usage.exceeds(policy)?;
            if admission && value.in_flight_attempts > 0 && (control.overrun || exceeds) {
                return Err(RuntimeError::Budget("project generation quota refused admission; inspect retained spending/outstanding attempts and request explicit host review".into()));
            }
            control.overrun |= exceeds;
        }
        // A known cost above its admitted ceilings is retained even if there
        // is still room under the project cap; require explicit host review.
        if let Some(old) = &old {
            let prior_held = add(old.committed_micros, old.in_flight_micros)?;
            let now_held = add(value.committed_micros, value.in_flight_micros)?;
            control.overrun |=
                value.committed_micros > prior_held || !admission && now_held > prior_held;
        }
        self.db
            .replace(tx, &key, value.generation, &self.db.serialize(&key, value)?)
            .await?;
        control.usage = Some(usage);
        self.write(tx, &mut control).await
    }
}

fn parse(row: Record) -> Result<GenerationQuotaSnapshot, RuntimeError> {
    let value: GenerationQuotaSnapshot = row.parse()?;
    if value.revision != row.revision {
        return Err(storage("generation quota revision differs from record"));
    }
    value.validate()?;
    Ok(value)
}
fn validate_budget(value: &BudgetSnapshot) -> Result<(), RuntimeError> {
    sequence(value.generation)?;
    if value.generation >= u64::MAX - 1024
        || value.uncertain_micros > value.committed_micros
        || value.in_flight_micros > 0 && value.in_flight_attempts == 0
    {
        return Err(storage("invalid generation quota budget snapshot"));
    }
    add(value.committed_micros, value.in_flight_micros)?;
    add(value.in_flight_attempts, value.uncertain_attempts)?;
    Ok(())
}
fn newest<'a>(
    a: &'a BudgetSnapshot,
    b: &'a BudgetSnapshot,
) -> Result<&'a BudgetSnapshot, RuntimeError> {
    let (old, new) = if a.generation <= b.generation {
        (a, b)
    } else {
        (b, a)
    };
    if old.generation == new.generation && old != new
        || old.limit_micros != new.limit_micros
        || old.committed_micros > new.committed_micros
        || old.uncertain_micros > new.uncertain_micros
        || old.uncertain_attempts > new.uncertain_attempts
    {
        return Err(storage(
            "generation quota conflicts with retained budget history",
        ));
    }
    Ok(new)
}
fn add(a: u64, b: u64) -> Result<u64, RuntimeError> {
    a.checked_add(b)
        .ok_or_else(|| storage("generation quota accounting overflow"))
}
fn audit_text(value: &str, max: usize) -> Result<(), RuntimeError> {
    if value.trim().is_empty() || value.len() > max || value.chars().any(char::is_control) {
        return Err(config(
            "quota operator/note must be bounded nonempty text without control characters",
        ));
    }
    Ok(())
}
