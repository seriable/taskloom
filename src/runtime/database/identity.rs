//! Host-managed SQL identities. SDK administration is trusted; untrusted HTTP
//! callers must never receive this store as an unrestricted configuration API.
use super::*;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectRole {
    Reader,
    Runner,
    Approver,
    Operator,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectPermission {
    Read,
    Submit,
    Approve,
    Control,
    ManageKnowledge,
    ExportTelemetry,
}
impl ProjectRole {
    fn permits(self, permission: ProjectPermission) -> bool {
        self == Self::Operator
            || permission == ProjectPermission::Read
            || self == Self::Runner && permission == ProjectPermission::Submit
            || self == Self::Approver && permission == ProjectPermission::Approve
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectMembership {
    pub tenant: String,
    pub project: String,
    pub role: ProjectRole,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceUser {
    pub id: String,
    pub enabled: bool,
    pub memberships: Vec<ProjectMembership>,
}
impl ServiceUser {
    fn validate(&self) -> Result<(), RuntimeError> {
        identity(&self.id)?;
        if self.memberships.len() > 64 {
            return Err(config("at most 64 project memberships per user"));
        }
        let mut seen = BTreeSet::new();
        for member in &self.memberships {
            DatabaseNamespace::new(&member.tenant, &member.project)?;
            if !seen.insert((&member.tenant, &member.project)) {
                return Err(config("duplicate project membership"));
            }
        }
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceTokenRecord {
    pub id: String,
    pub user: String,
    pub digest: String,
    pub expires_ms: i64,
    pub revoked: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IdentityEvent {
    pub revision: u64,
    pub actor: String,
    pub action: String,
    pub subject: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IdentitySnapshot {
    pub version: u32,
    pub revision: u64,
    pub users: BTreeMap<String, ServiceUser>,
    pub tokens: BTreeMap<String, ServiceTokenRecord>,
    pub events: Vec<IdentityEvent>,
}
impl Default for IdentitySnapshot {
    fn default() -> Self {
        Self {
            version: 1,
            revision: 0,
            users: BTreeMap::new(),
            tokens: BTreeMap::new(),
            events: Vec::new(),
        }
    }
}
impl IdentitySnapshot {
    fn validate(&self) -> Result<(), RuntimeError> {
        if self.version != 1
            || self.users.len() > 1024
            || self.tokens.len() > 4096
            || self.events.len() > 20_000
            || self.revision != self.events.len() as u64
        {
            return Err(storage("invalid or oversized identity catalog"));
        }
        for (id, user) in &self.users {
            user.validate()?;
            if id != &user.id {
                return Err(storage("identity user key differs"));
            }
        }
        let mut digests = BTreeSet::new();
        for (id, token) in &self.tokens {
            identity(id)?;
            if id != &token.id
                || !self.users.contains_key(&token.user)
                || token.expires_ms <= 0
                || token.digest.len() != 64
                || !token.digest.bytes().all(|b| b.is_ascii_hexdigit())
                || !digests.insert(&token.digest)
            {
                return Err(storage("invalid identity token binding"));
            }
        }
        for (index, event) in self.events.iter().enumerate() {
            identity(&event.actor)?;
            identity(&event.subject)?;
            if event.revision != (index as u64) + 1
                || !matches!(
                    event.action.as_str(),
                    "put_user" | "issue_token" | "revoke_token"
                )
            {
                return Err(storage("invalid identity audit event"));
            }
        }
        Ok(())
    }
}
/// Intentionally neither Debug nor Serialize. Read only at explicit issuance
/// to pass to the intended user through the host's chosen secret channel.
pub struct ApiToken(String);
impl ApiToken {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
pub struct IssuedToken {
    pub id: String,
    pub bearer: ApiToken,
    pub expires_ms: i64,
}
/// No Deserialize or public constructor: the host obtains this by checking an
/// actual bearer. Every authorization checks SQL revocation/membership again.
#[derive(Clone)]
pub struct AuthenticatedActor {
    user: String,
    token_id: String,
}
impl AuthenticatedActor {
    pub fn user_id(&self) -> &str {
        &self.user
    }
}

#[derive(Clone)]
pub struct SqlIdentityStore {
    db: SqlDatabase,
}
impl SqlIdentityStore {
    pub(super) fn new(db: SqlDatabase) -> Self {
        Self { db }
    }
    fn key(&self) -> Result<RecordKey, RuntimeError> {
        RecordKey::new(
            DatabaseNamespace::new("_taskloom_control", "identities")?,
            "identity",
            "catalog",
        )
    }
    pub async fn snapshot(&self) -> Result<IdentitySnapshot, RuntimeError> {
        self.db
            .read(&self.key()?)
            .await?
            .map(parse)
            .transpose()
            .map(Option::unwrap_or_default)
    }
    async fn update<T>(
        &self,
        actor: &str,
        action: &str,
        subject: &str,
        operation: impl FnOnce(&mut IdentitySnapshot, i64) -> Result<T, RuntimeError> + Send,
    ) -> Result<T, RuntimeError> {
        identity(actor)?;
        identity(subject)?;
        self.db.schema().await?;
        let key = self.key()?;
        let mut tx = self.db.write_transaction().await?;
        let mut snapshot = parse(
            self.db
                .lock_record(&mut tx, &key, &IdentitySnapshot::default())
                .await?,
        )?;
        let now: i64 = sqlx::query_scalar(&format!("SELECT {}", self.db.0.backend.now()))
            .fetch_one(&mut *tx)
            .await
            .map_err(database_error)?;
        let result = operation(&mut snapshot, now)?;
        snapshot.revision = snapshot
            .revision
            .checked_add(1)
            .ok_or_else(|| storage("identity revision overflow"))?;
        snapshot.events.push(IdentityEvent {
            revision: snapshot.revision,
            actor: actor.into(),
            action: action.into(),
            subject: subject.into(),
        });
        snapshot.validate()?;
        let body = self.db.serialize(&key, &snapshot)?;
        self.db
            .replace(&mut tx, &key, snapshot.revision, &body)
            .await?;
        tx.commit().await.map_err(database_error)?;
        Ok(result)
    }
    /// Trusted host administration only. CAS includes the full catalog revision.
    pub async fn put_user(
        &self,
        user: ServiceUser,
        expected: u64,
        operator: &str,
    ) -> Result<(), RuntimeError> {
        user.validate()?;
        let subject = user.id.clone();
        self.update(operator, "put_user", &subject, move |catalog, _| {
            if catalog.revision != expected {
                return Err(conflict("identity catalog revision changed"));
            }
            if !user.enabled {
                for token in catalog
                    .tokens
                    .values_mut()
                    .filter(|token| token.user == user.id)
                {
                    token.revoked = true;
                }
            }
            catalog.users.insert(user.id.clone(), user);
            Ok(())
        })
        .await
    }
    pub async fn issue_token(
        &self,
        user: &str,
        valid_for: Duration,
        operator: &str,
    ) -> Result<IssuedToken, RuntimeError> {
        identity(user)?;
        if valid_for < Duration::from_secs(1) || valid_for > Duration::from_secs(366 * 86400) {
            return Err(config("token lifetime must be 1 second..366 days"));
        }
        let id = uuid::Uuid::new_v4().to_string();
        let token = ApiToken(format!(
            "tlm_{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        ));
        let digest = hex::encode(Sha256::digest(token.as_str().as_bytes()));
        let expires_ms = self
            .update(operator, "issue_token", &id, |catalog, now| {
                if !catalog.users.get(user).is_some_and(|u| u.enabled) {
                    return Err(denied());
                }
                let expires = now
                    .checked_add(valid_for.as_millis() as i64)
                    .ok_or_else(|| config("token lifetime overflow"))?;
                catalog.tokens.insert(
                    id.clone(),
                    ServiceTokenRecord {
                        id: id.clone(),
                        user: user.into(),
                        digest,
                        expires_ms: expires,
                        revoked: false,
                    },
                );
                Ok(expires)
            })
            .await?;
        Ok(IssuedToken {
            id,
            bearer: token,
            expires_ms,
        })
    }
    pub async fn revoke_token(&self, id: &str, operator: &str) -> Result<(), RuntimeError> {
        self.update(operator, "revoke_token", id, |catalog, _| {
            catalog.tokens.get_mut(id).ok_or_else(denied)?.revoked = true;
            Ok(())
        })
        .await
    }
    async fn checked_snapshot(&self) -> Result<(IdentitySnapshot, i64), RuntimeError> {
        // Read catalog first, then database time. Expiry cannot be extended by
        // a slow read or a worker's incorrect local clock.
        let snapshot = self.snapshot().await?;
        let now: i64 = sqlx::query_scalar(&format!("SELECT {}", self.db.0.backend.now()))
            .fetch_one(&self.db.0.pool)
            .await
            .map_err(database_error)?;
        Ok((snapshot, now))
    }
    pub async fn authenticate(&self, bearer: &str) -> Result<AuthenticatedActor, RuntimeError> {
        if bearer.len() != 68
            || !bearer.starts_with("tlm_")
            || !bearer.as_bytes()[4..].iter().all(u8::is_ascii_hexdigit)
        {
            return Err(denied());
        }
        let digest = hex::encode(Sha256::digest(bearer.as_bytes()));
        let (snapshot, now) = self.checked_snapshot().await?;
        let mut matched = None;
        // Fixed-length comparison; do not expose stored digest or bearer in errors.
        for token in snapshot.tokens.values() {
            let difference = token
                .digest
                .bytes()
                .zip(digest.bytes())
                .fold(0u8, |n, (a, b)| n | (a ^ b));
            if difference == 0 {
                matched = Some(token);
            }
        }
        let token = matched
            .filter(|t| !t.revoked && t.expires_ms > now)
            .ok_or_else(denied)?;
        let user = snapshot
            .users
            .get(&token.user)
            .filter(|u| u.enabled)
            .ok_or_else(denied)?;
        Ok(AuthenticatedActor {
            user: user.id.clone(),
            token_id: token.id.clone(),
        })
    }
    pub async fn authorize(
        &self,
        actor: &AuthenticatedActor,
        namespace: &DatabaseNamespace,
        permission: ProjectPermission,
    ) -> Result<ProjectRole, RuntimeError> {
        let (snapshot, now) = self.checked_snapshot().await?;
        snapshot
            .tokens
            .get(&actor.token_id)
            .filter(|t| t.user == actor.user && !t.revoked && t.expires_ms > now)
            .ok_or_else(denied)?;
        let user = snapshot
            .users
            .get(&actor.user)
            .filter(|u| u.enabled)
            .ok_or_else(denied)?;
        user.memberships
            .iter()
            .find(|m| {
                DatabaseNamespace::new(&m.tenant, &m.project).as_ref().ok() == Some(namespace)
            })
            .filter(|m| m.role.permits(permission))
            .map(|m| m.role)
            .ok_or_else(denied)
    }
}
fn parse(row: Record) -> Result<IdentitySnapshot, RuntimeError> {
    let snapshot: IdentitySnapshot = row.parse()?;
    snapshot.validate()?;
    if snapshot.revision != row.revision {
        return Err(storage("identity catalog revision differs"));
    }
    Ok(snapshot)
}
fn identity(id: &str) -> Result<(), RuntimeError> {
    if id.is_empty()
        || id.len() > 128
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.@-".contains(&b))
    {
        return Err(config(
            "service identity must be 1..128 ASCII identity characters",
        ));
    }
    Ok(())
}
fn denied() -> RuntimeError {
    RuntimeError::Permission("identity authentication or project permission refused".into())
}
