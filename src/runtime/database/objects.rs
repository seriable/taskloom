use super::*;

/// Namespace-bound host persistence, subject to the same bounded envelope and
/// transaction rules as runtime journals. Values never grant host authority.
#[derive(Clone)]
pub struct SqlObjectStore {
    db: SqlDatabase,
    namespace: DatabaseNamespace,
}
impl SqlObjectStore {
    pub(super) fn new(db: SqlDatabase, namespace: DatabaseNamespace) -> Self {
        Self { db, namespace }
    }
    fn key(&self, id: &str) -> Result<RecordKey, RuntimeError> {
        RecordKey::new(self.namespace.clone(), "app_object", id)
    }
    pub async fn load<T: DeserializeOwned>(&self, id: &str) -> Result<Option<T>, RuntimeError> {
        self.db
            .read(&self.key(id)?)
            .await?
            .map(|row| row.parse())
            .transpose()
    }
    /// Mutate a short aggregate atomically. The callback must validate the
    /// previous value and cannot perform network/service work inside the txn.
    pub async fn modify<T: Serialize + DeserializeOwned, R>(
        &self,
        id: &str,
        initial: T,
        operation: impl FnOnce(&mut T) -> Result<R, RuntimeError> + Send,
    ) -> Result<R, RuntimeError> {
        self.db.schema().await?;
        let key = self.key(id)?;
        let mut tx = self.db.write_transaction().await?;
        let row = self.db.lock_record(&mut tx, &key, &initial).await?;
        let revision = row
            .revision
            .checked_add(1)
            .ok_or_else(|| storage("SQL object revision overflow"))?;
        let mut value: T = row.parse()?;
        let result = operation(&mut value)?;
        let body = self.db.serialize(&key, &value)?;
        self.db.replace(&mut tx, &key, revision, &body).await?;
        tx.commit().await.map_err(database_error)?;
        Ok(result)
    }
    pub async fn acquire(&self, id: &str) -> Result<SqlObjectLease, RuntimeError> {
        Ok(SqlObjectLease(self.db.acquire(self.key(id)?).await?))
    }
}
/// A renewable/fenced owner for an explicit potentially long build operation.
#[derive(Clone)]
pub struct SqlObjectLease(Arc<Lease>);
impl SqlObjectLease {
    pub async fn verify_ownership(&self) -> Result<(), RuntimeError> {
        self.0.check().await
    }
    pub async fn save<T: Serialize>(&self, value: &T) -> Result<(), RuntimeError> {
        let body = self.0.db.serialize(&self.0.key, value)?;
        let mut tx = self.0.db.write_transaction().await?;
        self.0.lock(&mut tx).await?;
        let revision = self
            .0
            .db
            .read_tx(&mut tx, &self.0.key)
            .await?
            .map_or(Ok(0), |row| {
                row.revision
                    .checked_add(1)
                    .ok_or_else(|| storage("SQL object revision overflow"))
            })?;
        self.0
            .db
            .replace(&mut tx, &self.0.key, revision, &body)
            .await?;
        self.0.commit(tx).await
    }
}
