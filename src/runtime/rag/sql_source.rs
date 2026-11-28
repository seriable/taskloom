//! Explicit host-selected, read-only SQL source snapshots; no caller SQL or sync.
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::Duration;

use futures_util::TryStreamExt;
use serde::{Deserialize, Serialize};
use sqlx::{
    any::AnyConnectOptions, any::AnyPoolOptions, Any, AnyPool, ConnectOptions, Row, Transaction,
};

use super::{invalid, Document, MAX_DOCUMENT_BYTES};
use crate::runtime::{state::fingerprint, RuntimeError};

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SqlSourceLimits {
    pub max_rows: usize,
    pub max_document_bytes: usize,
    /// Complete serialized Documents, including provenance and identities.
    pub max_total_bytes: usize,
    pub timeout_ms: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore = "requires explicitly owned LATIN1 TASKLOOM_TEST_LATIN1_POSTGRES_URL; unique table only"]
    async fn postgres_latin1_metadata_counts_extracted_utf8_bytes_before_body_read() {
        let url = std::env::var("TASKLOOM_TEST_LATIN1_POSTGRES_URL")
            .expect("owned LATIN1 PostgreSQL required");
        sqlx::any::install_default_drivers();
        let pool = AnyPool::connect(&url).await.unwrap();
        let encoding: String = sqlx::query_scalar("SHOW server_encoding")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(encoding, "LATIN1");
        let table = format!("latin1_source_{}", uuid::Uuid::new_v4().simple());
        sqlx::query(&format!(
            "CREATE TABLE {table} (scope TEXT,id TEXT,title TEXT,body TEXT)"
        ))
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(&format!("INSERT INTO {table} VALUES($1,$2,$3,$4)"))
            .bind("authorized")
            .bind("é")
            .bind("é")
            .bind("é")
            .execute(&pool)
            .await
            .unwrap();
        let policy = SqlSourceConfig {
            source_namespace: "latin1-source".into(),
            source_version: "v1".into(),
            table: table.clone(),
            namespace_column: "scope".into(),
            namespace_value: "authorized".into(),
            id_column: "id".into(),
            title_column: "title".into(),
            text_column: "body".into(),
            limits: SqlSourceLimits {
                max_document_bytes: 2,
                ..Default::default()
            },
        };
        let source = SqlDocumentSource::postgres(&url, policy.clone())
            .await
            .unwrap();
        let mut tx = source
            .pool
            .begin_with("BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY")
            .await
            .unwrap();
        source.validate_table(&mut tx).await.unwrap();
        // Exercise the actual preflight query, before any body projection. Raw
        // database text is one LATIN1 byte; the extracted UTF-8 contract is two.
        let query = source.query(true);
        let metadata = sqlx::query(&query)
            .bind("authorized")
            .bind(2i64)
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        for field in ["id_bytes", "title_bytes", "text_bytes"] {
            assert_eq!(metadata.get::<i64, _>(field), 2);
        }
        let native_bytes: i64 = sqlx::query_scalar(&format!(
            "SELECT CAST(octet_length(body) AS BIGINT) FROM public.{table}"
        ))
        .fetch_one(&mut *tx)
        .await
        .unwrap();
        assert_eq!(native_bytes, 1);
        tx.commit().await.unwrap();
        let snapshot = source.snapshot().await.unwrap();
        assert_eq!(snapshot.documents.len(), 1);
        assert_eq!(snapshot.documents[0].text, "é");
        source.close().await;
        let bounded = SqlDocumentSource::postgres(
            &url,
            SqlSourceConfig {
                limits: SqlSourceLimits {
                    max_document_bytes: 1,
                    ..policy.limits
                },
                ..policy
            },
        )
        .await
        .unwrap();
        assert!(bounded.snapshot().await.is_err());
        bounded.close().await;
        sqlx::query(&format!("DROP TABLE {table}"))
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;
    }
}
impl Default for SqlSourceLimits {
    fn default() -> Self {
        Self {
            max_rows: 256,
            max_document_bytes: 1024 * 1024,
            max_total_bytes: 16 * 1024 * 1024,
            timeout_ms: 5000,
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SqlSourceConfig {
    /// Public logical identity; change it when changing source database/account.
    pub source_namespace: String,
    pub source_version: String,
    /// One base table, in SQLite main / PostgreSQL public. No schema/expression.
    pub table: String,
    pub namespace_column: String,
    pub namespace_value: String,
    pub id_column: String,
    pub title_column: String,
    pub text_column: String,
    pub limits: SqlSourceLimits,
}
impl SqlSourceConfig {
    pub fn validate(&self) -> Result<(), RuntimeError> {
        if !label(&self.source_namespace)
            || !label(&self.source_version)
            || self.namespace_value.is_empty()
            || self.namespace_value.len() > 512
            || self.namespace_value.chars().any(char::is_control)
            || !identifier(&self.table)
            || self.columns().iter().any(|s| !identifier(s))
            || self.columns().into_iter().collect::<BTreeSet<_>>().len() != 4
            || !(1..=4096).contains(&self.limits.max_rows)
            || !(1..=MAX_DOCUMENT_BYTES).contains(&self.limits.max_document_bytes)
            || !(1024..=64 * 1024 * 1024).contains(&self.limits.max_total_bytes)
            || self.limits.max_document_bytes > self.limits.max_total_bytes
            || !(1..=60_000).contains(&self.limits.timeout_ms)
        {
            return Err(invalid("invalid fixed SQL source policy/limits"));
        }
        Ok(())
    }
    fn columns(&self) -> [&str; 4] {
        [
            &self.namespace_column,
            &self.id_column,
            &self.title_column,
            &self.text_column,
        ]
    }
}
fn label(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
}
fn identifier(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 63
        && s.bytes()
            .enumerate()
            .all(|(i, b)| b.is_ascii_alphabetic() || b == b'_' || i > 0 && b.is_ascii_digit())
}
fn quoted(s: &str) -> String {
    format!("\"{s}\"")
} // only validated identifiers reach SQL formatting
fn rejected() -> RuntimeError {
    invalid("SQL source snapshot unavailable/invalid; no partial snapshot or automatic retry")
}
#[derive(Clone, Copy)]
enum Backend {
    Sqlite,
    Postgres,
}

/// Does not implement Debug/Serialize and never retains a connection URL.
pub struct SqlDocumentSource {
    pool: AnyPool,
    backend: Backend,
    config: SqlSourceConfig,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SqlDocumentSnapshot {
    pub version: u32,
    pub source_namespace: String,
    pub source_version: String,
    pub policy_sha256: String,
    pub snapshot_sha256: String,
    pub documents: Vec<Document>,
}
impl SqlDocumentSource {
    pub async fn sqlite(
        path: impl AsRef<Path>,
        config: SqlSourceConfig,
    ) -> Result<Self, RuntimeError> {
        config.validate()?;
        let path = std::fs::canonicalize(path).map_err(|_| rejected())?;
        if !path.metadata().map_err(|_| rejected())?.is_file() {
            return Err(rejected());
        }
        let url = crate::runtime::database::SqlDatabase::sqlite_url(&path)
            .map_err(|_| rejected())?
            .replace("mode=rwc", "mode=ro");
        Self::connect(&url, Backend::Sqlite, config).await
    }
    /// URL is a secret host argument, never a model/HTTP request parameter.
    pub async fn postgres(url: &str, config: SqlSourceConfig) -> Result<Self, RuntimeError> {
        config.validate()?;
        if !url.starts_with("postgres:") && !url.starts_with("postgresql:") {
            return Err(rejected());
        }
        Self::connect(url, Backend::Postgres, config).await
    }
    async fn connect(
        url: &str,
        backend: Backend,
        config: SqlSourceConfig,
    ) -> Result<Self, RuntimeError> {
        sqlx::any::install_default_drivers();
        let options = url
            .parse::<AnyConnectOptions>()
            .map_err(|_| rejected())?
            .disable_statement_logging();
        let statement_timeout = config.limits.timeout_ms;
        let pool = tokio::time::timeout(
            Duration::from_millis(config.limits.timeout_ms),
            AnyPoolOptions::new()
                .max_connections(2)
                .acquire_timeout(Duration::from_millis(config.limits.timeout_ms))
                .after_connect(move |conn, _| {
                    Box::pin(async move {
                        let statements = match backend {
                            Backend::Sqlite => ["PRAGMA query_only=ON", "PRAGMA busy_timeout=5000"],
                            Backend::Postgres => [
                                "SET default_transaction_read_only=on",
                                "SET search_path=pg_catalog",
                            ],
                        };
                        for sql in statements {
                            sqlx::query(sql).execute(&mut *conn).await?;
                        }
                        if matches!(backend, Backend::Postgres) {
                            sqlx::query(&format!("SET statement_timeout={statement_timeout}"))
                                .execute(&mut *conn)
                                .await?;
                            sqlx::query(&format!("SET lock_timeout={statement_timeout}"))
                                .execute(&mut *conn)
                                .await?;
                        }
                        Ok(())
                    })
                })
                .connect_with(options),
        )
        .await
        .map_err(|_| rejected())?
        .map_err(|_| rejected())?;
        Ok(Self {
            pool,
            backend,
            config,
        })
    }
    pub async fn close(&self) {
        self.pool.close().await;
    }
    pub async fn snapshot(&self) -> Result<SqlDocumentSnapshot, RuntimeError> {
        tokio::time::timeout(
            Duration::from_millis(self.config.limits.timeout_ms),
            self.snapshot_inner(),
        )
        .await
        .map_err(|_| rejected())?
    }
    async fn validate_table(&self, tx: &mut Transaction<'_, Any>) -> Result<(), RuntimeError> {
        match self.backend {
            Backend::Sqlite => {
                let definition: Option<String> = sqlx::query_scalar(
                    "SELECT type FROM pragma_table_list WHERE schema='main' AND name=$1",
                )
                .bind(&self.config.table)
                .fetch_optional(&mut **tx)
                .await
                .map_err(|_| rejected())?;
                let definition = definition.ok_or_else(rejected)?;
                if definition != "table" {
                    return Err(rejected());
                }
                let rows = sqlx::query(
                    "SELECT name,hidden FROM pragma_table_xinfo($1) WHERE name IN ($2,$3,$4,$5)",
                )
                .bind(&self.config.table)
                .bind(&self.config.namespace_column)
                .bind(&self.config.id_column)
                .bind(&self.config.title_column)
                .bind(&self.config.text_column)
                .fetch_all(&mut **tx)
                .await
                .map_err(|_| rejected())?;
                if rows.len() != 4
                    || rows
                        .iter()
                        .any(|r| r.try_get::<i64, _>("hidden").ok() != Some(0))
                {
                    return Err(rejected());
                }
            }
            Backend::Postgres => {
                let rows=sqlx::query("SELECT CAST(a.atttypid AS BIGINT) AS type_id,CAST(a.attgenerated AS TEXT) AS generated FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace JOIN pg_catalog.pg_attribute a ON a.attrelid=c.oid WHERE n.nspname='public' AND c.relkind='r' AND c.relname=$1 AND a.attnum>0 AND NOT a.attisdropped AND a.attname IN($2,$3,$4,$5)")
                    .bind(&self.config.table).bind(&self.config.namespace_column).bind(&self.config.id_column)
                    .bind(&self.config.title_column).bind(&self.config.text_column)
                    .fetch_all(&mut **tx).await.map_err(|_|rejected())?;
                if rows.len() != 4
                    || rows.iter().any(|r| {
                        !matches!(r.try_get::<i64, _>("type_id").ok(), Some(25 | 1043))
                            || r.try_get::<String, _>("generated").ok().as_deref() != Some("")
                    })
                {
                    return Err(rejected());
                }
            }
        }
        Ok(())
    }
    fn query(&self, metadata: bool) -> String {
        let cfg = &self.config;
        let id = quoted(&cfg.id_column);
        let title = quoted(&cfg.title_column);
        let text = quoted(&cfg.text_column);
        let (length,prefix,order,table)=match self.backend {
            Backend::Sqlite=>(format!("length(CAST({id} AS BLOB)) AS id_bytes,length(CAST({title} AS BLOB)) AS title_bytes,length(CAST({text} AS BLOB)) AS text_bytes,typeof({id}) AS id_type,typeof({title}) AS title_type,typeof({text}) AS text_type"),
                format!("substr(CAST({id} AS BLOB),1,513) AS object_id,substr(CAST({title} AS BLOB),1,4097) AS title,substr(CAST({text} AS BLOB),1,{}) AS body",cfg.limits.max_document_bytes+1),
                format!("CAST({id} AS BLOB)"),format!("main.{}",quoted(&cfg.table))),
            Backend::Postgres=>(format!("CAST(octet_length(convert_to({id},'UTF8')) AS BIGINT) AS id_bytes,CAST(octet_length(convert_to({title},'UTF8')) AS BIGINT) AS title_bytes,CAST(octet_length(convert_to({text},'UTF8')) AS BIGINT) AS text_bytes,'text' AS id_type,'text' AS title_type,'text' AS text_type"),
                format!("substring(convert_to({id},'UTF8') from 1 for 513) AS object_id,substring(convert_to({title},'UTF8') from 1 for 4097) AS title,substring(convert_to({text},'UTF8') from 1 for {}) AS body",cfg.limits.max_document_bytes+1),
                format!("{id} COLLATE \"C\""),format!("public.{}",quoted(&cfg.table))),
        };
        let namespace = quoted(&cfg.namespace_column);
        let predicate = match self.backend {
            Backend::Sqlite => format!("CAST({namespace} AS BLOB)=CAST($1 AS BLOB)"),
            Backend::Postgres => {
                format!("convert_to({namespace},'UTF8')=convert_to(CAST($1 AS TEXT),'UTF8')")
            }
        };
        format!(
            "SELECT {} FROM {table} WHERE {predicate} ORDER BY {order} LIMIT $2",
            if metadata { length } else { prefix }
        )
    }
    async fn snapshot_inner(&self) -> Result<SqlDocumentSnapshot, RuntimeError> {
        self.config.validate()?;
        let mut tx = self
            .pool
            .begin_with(match self.backend {
                Backend::Sqlite => "BEGIN DEFERRED",
                Backend::Postgres => "BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY",
            })
            .await
            .map_err(|_| rejected())?;
        self.validate_table(&mut tx).await?;
        let limit = (self.config.limits.max_rows + 1) as i64;
        let rows = sqlx::query(&self.query(true))
            .bind(&self.config.namespace_value)
            .bind(limit)
            .fetch_all(&mut *tx)
            .await
            .map_err(|_| rejected())?;
        if rows.len() > self.config.limits.max_rows {
            return Err(rejected());
        }
        let mut raw_bytes = 0usize;
        for row in &rows {
            for (name, kind, max) in [
                ("id_bytes", "id_type", 512),
                ("title_bytes", "title_type", 4096),
                (
                    "text_bytes",
                    "text_type",
                    self.config.limits.max_document_bytes,
                ),
            ] {
                let bytes = row.try_get::<i64, _>(name).map_err(|_| rejected())?;
                if bytes <= 0
                    || bytes as u64 > max as u64
                    || row.try_get::<String, _>(kind).map_err(|_| rejected())? != "text"
                {
                    return Err(rejected());
                }
                raw_bytes = raw_bytes.checked_add(bytes as usize).ok_or_else(rejected)?;
                if raw_bytes > self.config.limits.max_total_bytes {
                    return Err(rejected());
                }
            }
        }
        let query = self.query(false);
        let mut stream = sqlx::query(&query)
            .bind(&self.config.namespace_value)
            .bind(limit)
            .fetch(&mut *tx);
        let mut values = Vec::<(String, String, String)>::new();
        let mut ids = BTreeSet::new();
        while let Some(row) = stream.try_next().await.map_err(|_| rejected())? {
            let decode = |name| -> Result<String, RuntimeError> {
                String::from_utf8(row.try_get::<Vec<u8>, _>(name).map_err(|_| rejected())?)
                    .map_err(|_| rejected())
            };
            let id = decode("object_id")?;
            let title = decode("title")?;
            let text = decode("body")?;
            if id.trim().is_empty()
                || id.len() > 512
                || id.chars().any(char::is_control)
                || !ids.insert(id.clone())
                || title.trim().is_empty()
                || title.len() > 4096
                || text.trim().is_empty()
                || text.len() > self.config.limits.max_document_bytes
            {
                return Err(rejected());
            }
            values.push((id, title, text));
        }
        drop(stream);
        if values.len() != rows.len() {
            return Err(rejected());
        }
        let policy = fingerprint(&("sql-document-source-v1", &self.config))?;
        let snapshot = fingerprint(&("sql-document-snapshot-v1", &policy, &values))?;
        let mut documents = Vec::new();
        let mut total = 0usize;
        for (id, title, text) in values {
            let row_id = fingerprint(&("sql-source-row-v1", &policy, &id))?;
            let text_sha = fingerprint(&text)?;
            let document = Document {
                id: format!("sql-{row_id}"),
                title,
                text,
                source: format!(
                    "taskloom-sql://{}/{}/{}#snapshot-sha256={snapshot}",
                    self.config.source_namespace, self.config.source_version, row_id
                ),
                metadata: BTreeMap::from([
                    ("format".into(), "sql-document-snapshot-v1".into()),
                    (
                        "source_namespace".into(),
                        self.config.source_namespace.clone(),
                    ),
                    ("source_version".into(), self.config.source_version.clone()),
                    ("source_policy_sha256".into(), policy.clone()),
                    ("source_snapshot_sha256".into(), snapshot.clone()),
                    ("source_row_sha256".into(), row_id),
                    ("text_value_sha256".into(), text_sha),
                ]),
            };
            document.validate().map_err(|_| rejected())?;
            total = total
                .checked_add(serde_json::to_vec(&document).map_err(|_| rejected())?.len())
                .ok_or_else(rejected)?;
            if total > self.config.limits.max_total_bytes {
                return Err(rejected());
            }
            documents.push(document);
        }
        tx.commit().await.map_err(|_| rejected())?;
        Ok(SqlDocumentSnapshot {
            version: 1,
            source_namespace: self.config.source_namespace.clone(),
            source_version: self.config.source_version.clone(),
            policy_sha256: policy,
            snapshot_sha256: snapshot,
            documents,
        })
    }
}
