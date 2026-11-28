//! Opaque authenticated-owner storage namespaces with project-wide accounting.
use super::*;

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct PrivateBinding {
    version: u32,
    project_namespace: String,
    owner_namespace: String,
}

#[derive(Clone)]
pub(super) struct PrivateRunScope {
    pub(super) project: DatabaseNamespace,
    owner: DatabaseNamespace,
}
impl PrivateRunScope {
    fn binding(&self) -> PrivateBinding {
        PrivateBinding {
            version: 1,
            project_namespace: self.project.0.clone(),
            owner_namespace: self.owner.0.clone(),
        }
    }
    fn key(&self) -> Result<RecordKey, RuntimeError> {
        RecordKey::new(self.owner.clone(), "private_binding", "project")
    }
    pub(super) async fn verify(
        &self,
        database: &SqlDatabase,
        tx: Option<&mut Transaction<'_, Any>>,
    ) -> Result<(), RuntimeError> {
        let key = self.key()?;
        let record = match tx {
            Some(tx) => database.read_tx(tx, &key).await?,
            None => database.read(&key).await?,
        }
        .ok_or_else(|| storage("private storage project binding is absent"))?;
        let binding: PrivateBinding = record.parse()?;
        if record.revision != 1 || binding != self.binding() {
            return Err(storage("immutable private storage project binding differs"));
        }
        Ok(())
    }
}

/// Trusted host-selected private stores. Authenticate and authorize an actor
/// before constructing these: this SDK primitive is not an HTTP grant.
/// Its canonical three-component namespace cannot be made with the legacy
/// two-component `DatabaseNamespace::new`. No namespace setter is exposed.
#[derive(Clone)]
pub struct SqlPrivateStores {
    database: SqlDatabase,
    scope: PrivateRunScope,
}
impl SqlPrivateStores {
    pub fn runs(&self) -> SqlRunStore {
        SqlRunStore {
            db: self.database.clone(),
            namespace: self.scope.owner.clone(),
            queue: None,
            private: Some(self.scope.clone()),
        }
    }
    pub fn sessions(&self) -> SqlSessionStore {
        self.database.sessions(self.scope.owner.clone())
    }
    pub fn workflows(&self) -> SqlWorkflowStore {
        self.database.workflows(self.scope.owner.clone())
    }
}

impl SqlDatabase {
    /// Non-creating lookup for an owner's first visit. Absence is returned only
    /// when both binding and private history are absent; corruption, unavailable
    /// SQL and uninitialized quota for existing histories remain errors.
    pub async fn private_stores_if_exists(
        &self,
        tenant: &str,
        project: &str,
        owner: &str,
    ) -> Result<Option<SqlPrivateStores>, RuntimeError> {
        DatabaseNamespace::new(tenant, project)?;
        DatabaseNamespace::new(tenant, owner)?;
        let namespace =
            DatabaseNamespace(serde_json::to_string(&(tenant, project, owner)).map_err(storage)?);
        let key = RecordKey::new(namespace.clone(), "private_binding", "project")?;
        if self.read(&key).await?.is_none() {
            let history: Option<i64> =
                sqlx::query_scalar("SELECT 1 FROM taskloom_records WHERE namespace=$1 LIMIT 1")
                    .bind(&namespace.0)
                    .fetch_optional(&self.0.pool)
                    .await
                    .map_err(database_error)?;
            if history.is_some() {
                // A concurrent first creation may commit between these two
                // individually atomic reads. Recheck before declaring damage.
                if self.read(&key).await?.is_some() {
                    return self
                        .open_private_stores(tenant, project, owner)
                        .await
                        .map(Some);
                }
                return Err(storage(
                    "private history exists without its frozen project binding",
                ));
            }
            return Ok(None);
        }
        self.open_private_stores(tenant, project, owner)
            .await
            .map(Some)
    }
    /// Create/open an owner scope only under an initialized, enabled project
    /// generation quota. The binding and quota check share one transaction.
    pub async fn private_stores(
        &self,
        tenant: &str,
        project: &str,
        owner: &str,
    ) -> Result<SqlPrivateStores, RuntimeError> {
        self.private_stores_inner(tenant, project, owner, true)
            .await
    }
    /// Open an existing binding for inspection/settlement even while quota
    /// enforcement is disabled. Never creates a namespace or admits generation.
    /// Every future private reservation rechecks the enabled project quota.
    pub async fn open_private_stores(
        &self,
        tenant: &str,
        project: &str,
        owner: &str,
    ) -> Result<SqlPrivateStores, RuntimeError> {
        self.private_stores_inner(tenant, project, owner, false)
            .await
    }
    async fn private_stores_inner(
        &self,
        tenant: &str,
        project: &str,
        owner: &str,
        create: bool,
    ) -> Result<SqlPrivateStores, RuntimeError> {
        let project_namespace = DatabaseNamespace::new(tenant, project)?;
        // Reuse the ordinary bounded identifier validation, but never expose
        // the resulting private triple as a mutable/general namespace handle.
        DatabaseNamespace::new(tenant, owner)?;
        let owner_namespace =
            DatabaseNamespace(serde_json::to_string(&(tenant, project, owner)).map_err(storage)?);
        let scope = PrivateRunScope {
            project: project_namespace,
            owner: owner_namespace,
        };
        self.schema().await?;
        let mut tx = self.write_transaction().await?;
        self.generation_quota(scope.project.clone())
            .private_scope_admission(&mut tx, create)
            .await?;
        let key = scope.key()?;
        if self.read_tx(&mut tx, &key).await?.is_some() {
            scope.verify(self, Some(&mut tx)).await?;
        } else if create {
            let history: Option<i64> =
                sqlx::query_scalar("SELECT 1 FROM taskloom_records WHERE namespace=$1 LIMIT 1")
                    .bind(&scope.owner.0)
                    .fetch_optional(&mut *tx)
                    .await
                    .map_err(database_error)?;
            if history.is_some() {
                return Err(storage("private history exists without its frozen project binding; refusing implicit rebind"));
            }
            self.replace(&mut tx, &key, 1, &self.serialize(&key, &scope.binding())?)
                .await?;
        } else {
            return Err(storage("private storage binding does not exist; explicit initialized quota required for creation"));
        }
        tx.commit().await.map_err(database_error)?;
        Ok(SqlPrivateStores {
            database: self.clone(),
            scope,
        })
    }
}
