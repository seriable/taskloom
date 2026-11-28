//! Actual SQL/HTTP sources; no cloud model or hosted S3 account is used.
#![cfg(any(feature = "database", feature = "research"))]

#[cfg(feature = "database")]
mod sql {
    use sqlx::Row;
    use taskloom::runtime::{database::SqlDatabase, rag::*};
    fn config(table: &str) -> SqlSourceConfig {
        SqlSourceConfig {
            source_namespace: "reviewed-policies".into(),
            source_version: "v1".into(),
            table: table.into(),
            namespace_column: "scope".into(),
            namespace_value: "Alice".into(),
            id_column: "id".into(),
            title_column: "title".into(),
            text_column: "body".into(),
            limits: SqlSourceLimits::default(),
        }
    }
    async fn open(url: &str, sqlite: bool, cfg: SqlSourceConfig) -> SqlDocumentSource {
        if sqlite {
            let options = url.parse::<sqlx::sqlite::SqliteConnectOptions>().unwrap();
            SqlDocumentSource::sqlite(options.get_filename(), cfg)
                .await
                .unwrap()
        } else {
            SqlDocumentSource::postgres(url, cfg).await.unwrap()
        }
    }
    async fn exercise(url: &str, sqlite: bool) {
        sqlx::any::install_default_drivers();
        let pool = sqlx::AnyPool::connect(url).await.unwrap();
        if sqlite {
            sqlx::query("PRAGMA journal_mode=WAL")
                .execute(&pool)
                .await
                .unwrap();
        }
        let table = format!("source_{}", uuid::Uuid::new_v4().simple());
        let create = format!(
            "CREATE TABLE {table} (scope TEXT {},id TEXT,title TEXT,body TEXT)",
            if sqlite { "COLLATE NOCASE" } else { "" }
        );
        sqlx::query(&create).execute(&pool).await.unwrap();
        let insert = format!("INSERT INTO {table} (scope,id,title,body) VALUES($1,$2,$3,$4)");
        for (scope, id, title, body) in [
            (
                "Alice",
                "b",
                "B policy",
                "审批与审计：操作必须显式授权，保留原始预算与证据。",
            ),
            (
                "Alice",
                "a",
                "A policy",
                "审批与审计：操作必须显式授权，保留原始预算与证据。",
            ),
            (
                "alice",
                "other",
                "Other owner",
                "never mix case-insensitive namespace",
            ),
        ] {
            sqlx::query(&insert)
                .bind(scope)
                .bind(id)
                .bind(title)
                .bind(body)
                .execute(&pool)
                .await
                .unwrap();
        }
        let source = open(url, sqlite, config(&table)).await;
        let first = source.snapshot().await.unwrap();
        assert_eq!(first.documents.len(), 2);
        assert_eq!(source.snapshot().await.unwrap(), first);
        assert!(first.documents.iter().all(|d| d
            .source
            .starts_with("taskloom-sql://reviewed-policies/v1/")
            && !d.text.contains("never mix")));
        let serialized = serde_json::to_string(&first).unwrap();
        assert!(!serialized.contains(url));
        assert_eq!(first.policy_sha256.len(), 64);
        // The returned snapshot does not import or mutate a corpus.
        let dir = tempfile::tempdir().unwrap();
        let store = FileCorpusStore::new(dir.path().join("corpus"));
        assert_eq!(store.snapshot().await.unwrap().revision, 0);
        ingest(
            &store,
            first.documents[0].clone(),
            &WindowChunker::default(),
            Some(0),
        )
        .await
        .unwrap();
        sqlx::query(&format!(
            "UPDATE {table} SET body=$1 WHERE scope=$2 AND id=$3"
        ))
        .bind("explicitly updated source content")
        .bind("Alice")
        .bind("a")
        .execute(&pool)
        .await
        .unwrap();
        let changed = source.snapshot().await.unwrap();
        assert_eq!(changed.documents[0].id, first.documents[0].id);
        assert_ne!(changed.snapshot_sha256, first.snapshot_sha256);
        assert_ne!(changed.documents[1].source, first.documents[1].source);
        assert_eq!(store.snapshot().await.unwrap().revision, 1);
        // No row truncation: max_rows+1 refuses the whole snapshot.
        let mut cfg = config(&table);
        cfg.limits.max_rows = 1;
        let bounded = open(url, sqlite, cfg).await;
        assert!(bounded.snapshot().await.is_err());
        bounded.close().await;
        let mut cfg = config(&table);
        cfg.limits.max_document_bytes = 4;
        let bounded = open(url, sqlite, cfg).await;
        assert!(bounded.snapshot().await.is_err());
        bounded.close().await;
        let mut cfg = config(&table);
        cfg.limits.max_document_bytes = 512;
        cfg.limits.max_total_bytes = 1024;
        let bounded = open(url, sqlite, cfg).await;
        assert!(bounded.snapshot().await.is_err());
        bounded.close().await;
        // Duplicate keys, null values, overlong identities cannot escape limits.
        for (id, title, body) in [
            ("a".to_string(), Some("duplicate"), "duplicate"),
            ("null".to_string(), None, "not null body"),
            ("x".repeat(513), Some("overlong"), "bounded body"),
        ] {
            sqlx::query(&insert)
                .bind("Alice")
                .bind(&id)
                .bind(title)
                .bind(body)
                .execute(&pool)
                .await
                .unwrap();
            let error = source.snapshot().await.unwrap_err().to_string();
            assert!(!error.contains(url));
            assert!(!error.contains(&"x".repeat(100)));
            if id == "a" {
                sqlx::query(&format!("DELETE FROM {table} WHERE title=$1"))
                    .bind("duplicate")
                    .execute(&pool)
                    .await
                    .unwrap();
            } else {
                sqlx::query(&format!("DELETE FROM {table} WHERE id=$1"))
                    .bind(&id)
                    .execute(&pool)
                    .await
                    .unwrap();
            }
        }
        let mut cfg = config(&table);
        cfg.table = "source; DROP TABLE source".into();
        assert!(cfg.validate().is_err());
        let mut cfg = config(&table);
        cfg.namespace_column = "scope OR 1=1".into();
        assert!(cfg.validate().is_err());
        let view = format!("view_{}", uuid::Uuid::new_v4().simple());
        sqlx::query(&format!("CREATE VIEW {view} AS SELECT * FROM {table}"))
            .execute(&pool)
            .await
            .unwrap();
        let view_source = open(url, sqlite, config(&view)).await;
        assert!(view_source.snapshot().await.is_err());
        view_source.close().await;
        sqlx::query(&format!("DROP VIEW {view}"))
            .execute(&pool)
            .await
            .unwrap();
        let generated = format!("generated_{}", uuid::Uuid::new_v4().simple());
        sqlx::query(&format!("CREATE TABLE {generated} (scope TEXT,id TEXT,title TEXT,body TEXT GENERATED ALWAYS AS (title) STORED)"))
            .execute(&pool).await.unwrap();
        let generated_source = open(url, sqlite, config(&generated)).await;
        assert!(generated_source.snapshot().await.is_err());
        generated_source.close().await;
        sqlx::query(&format!("DROP TABLE {generated}"))
            .execute(&pool)
            .await
            .unwrap();
        if sqlite {
            sqlx::query(&format!(
                "UPDATE {table} SET body=CAST('binary-value' AS BLOB) WHERE id='a'"
            ))
            .execute(&pool)
            .await
            .unwrap();
            assert!(source.snapshot().await.is_err());
        }
        // Concurrent atomic two-row edits must yield one consistent generation.
        let update = format!("UPDATE {table} SET body=$1 WHERE scope=$2");
        sqlx::query(&update)
            .bind("generation zero")
            .bind("Alice")
            .execute(&pool)
            .await
            .unwrap();
        let writer_pool = pool.clone();
        let writer_query = update.clone();
        let writer = tokio::spawn(async move {
            for i in 1..=30 {
                let mut tx = writer_pool.begin().await.unwrap();
                sqlx::query(&writer_query)
                    .bind(format!("generation {i}"))
                    .bind("Alice")
                    .execute(&mut *tx)
                    .await
                    .unwrap();
                tx.commit().await.unwrap();
                tokio::task::yield_now().await;
            }
        });
        for _ in 0..15 {
            let snapshot = source.snapshot().await.unwrap();
            assert_eq!(snapshot.documents.len(), 2);
            assert_eq!(snapshot.documents[0].text, snapshot.documents[1].text);
        }
        writer.await.unwrap();
        let remaining = sqlx::query(&format!("SELECT COUNT(*) AS count FROM {table}"))
            .fetch_one(&pool)
            .await
            .unwrap()
            .get::<i64, _>("count");
        assert_eq!(remaining, 3);
        source.close().await;
        sqlx::query(&format!("DROP TABLE {table}"))
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;
    }
    #[tokio::test]
    async fn sqlite_readonly_scoped_snapshot_limits_provenance_and_concurrent_consistency() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("source 空间.sqlite");
        let url = SqlDatabase::sqlite_url(&path).unwrap();
        exercise(&url, true).await;
        assert!(
            SqlDocumentSource::sqlite(dir.path().join("absent.sqlite"), config("source"))
                .await
                .is_err()
        );
        assert!(!dir.path().join("absent.sqlite").exists());
    }
    #[tokio::test]
    #[ignore = "requires explicitly owned TASKLOOM_TEST_POSTGRES_URL; creates/drops only unique source test table/view"]
    async fn postgres_readonly_repeatable_scoped_snapshot_limits_and_provenance() {
        let url = std::env::var("TASKLOOM_TEST_POSTGRES_URL").expect("owned PostgreSQL required");
        exercise(&url, false).await;
    }
}

#[cfg(feature = "research")]
mod object {
    use sha2::{Digest, Sha256};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    use taskloom::runtime::rag::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    const BODY: &[u8] = "固定对象快照：审批、预算和引用必须保留原始来源。".as_bytes();
    fn config(body: &[u8]) -> PresignedObjectConfig {
        PresignedObjectConfig {
            source_namespace: "reviewed-objects".into(),
            source_version: "v1".into(),
            object_id: "policy-object".into(),
            expected_sha256: hex::encode(Sha256::digest(body)),
            expected_bytes: body.len(),
            max_bytes: 8 * 1024 * 1024,
            timeout_ms: 1000,
            allow_http_loopback_for_tests: true,
        }
    }
    async fn fixture(
        mode: &'static str,
        body: Vec<u8>,
    ) -> (String, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url=format!("http://{}/bucket/key?X-Amz-Credential=fixture-credential&X-Amz-Signature=fixture-signature",listener.local_addr().unwrap());
        let count = Arc::new(AtomicUsize::new(0));
        let calls = count.clone();
        let worker = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut header = Vec::new();
                while !header.ends_with(b"\r\n\r\n") {
                    if header.len() > 32768 {
                        return;
                    }
                    let Ok(byte) = socket.read_u8().await else {
                        return;
                    };
                    header.push(byte);
                }
                assert!(header.starts_with(b"GET /bucket/key?"));
                calls.fetch_add(1, Ordering::SeqCst);
                match mode {
                    "redirect" => {
                        let _=socket.write_all(b"HTTP/1.1 302 Found\r\nLocation: /secret-leak\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
                    }
                    "denied" => {
                        let _=socket.write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 16\r\nConnection: close\r\n\r\nsecret-errorbody").await;
                    }
                    "chunked" | "slow" | "oversize" => {
                        let _=socket.write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n").await;
                        if mode == "slow" {
                            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                        }
                        let mut bytes = body.clone();
                        if mode == "oversize" {
                            bytes.push(b'x');
                        }
                        let mut frame = format!("{:x}\r\n", bytes.len()).into_bytes();
                        frame.extend(&bytes);
                        frame.extend(b"\r\n0\r\n\r\n");
                        let _ = socket.write_all(&frame).await;
                    }
                    _ => {
                        let extra = if mode == "encoding" {
                            "Content-Encoding: gzip\r\n"
                        } else if mode == "range" {
                            "Content-Range: bytes 0-4/10\r\n"
                        } else {
                            ""
                        };
                        let header=format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n{extra}Connection: close\r\n\r\n",body.len());
                        let _ = socket.write_all(header.as_bytes()).await;
                        let bytes = if mode == "truncated" {
                            &body[..body.len() - 1]
                        } else {
                            &body
                        };
                        let _ = socket.write_all(bytes).await;
                    }
                }
            }
        });
        (url, count, worker)
    }
    #[tokio::test]
    async fn object_exact_bounded_get_is_inert_secret_url_free_and_citation_compatible() {
        for mode in ["normal", "chunked"] {
            let (url, count, worker) = fixture(mode, BODY.to_vec()).await;
            let source = PresignedObjectSource::new(config(BODY), url.clone()).unwrap();
            let snapshot = source.load().await.unwrap();
            assert_eq!(snapshot.bytes(), BODY);
            assert_eq!(snapshot.source_namespace(), "reviewed-objects");
            assert_eq!(snapshot.source_version(), "v1");
            assert_eq!(snapshot.sha256(), hex::encode(Sha256::digest(BODY)));
            let document = snapshot.document("object-policy", "Policy").unwrap();
            let encoded = serde_json::to_string(&document).unwrap();
            assert!(!encoded.contains(&url));
            assert!(!encoded.contains("fixture-credential"));
            assert!(!encoded.contains("fixture-signature"));
            let dir = tempfile::tempdir().unwrap();
            let store = FileCorpusStore::new(dir.path());
            assert_eq!(store.snapshot().await.unwrap().revision, 0);
            ingest(&store, document, &WindowChunker::default(), Some(0))
                .await
                .unwrap();
            let found = search(&store, &LexicalRetriever, &RetrievalQuery::new("审批"))
                .await
                .unwrap();
            let chunk = &found.hits[0].chunk;
            found
                .verify(CitedReport {
                    text: format!("[[{}]]", chunk.id),
                    citations: vec![Citation {
                        chunk_id: chunk.id.clone(),
                        quote: std::str::from_utf8(BODY).unwrap().into(),
                    }],
                })
                .unwrap();
            assert_eq!(count.load(Ordering::SeqCst), 1);
            worker.abort();
        }
    }
    #[tokio::test]
    async fn object_redirect_errors_partial_encoding_size_hash_and_timeout_never_retry_or_leak_secret_url(
    ) {
        for mode in [
            "redirect",
            "denied",
            "truncated",
            "encoding",
            "range",
            "oversize",
            "slow",
            "hash",
            "size",
        ] {
            let (url, count, worker) = fixture(mode, BODY.to_vec()).await;
            let mut cfg = config(BODY);
            if mode == "hash" {
                cfg.expected_sha256 = "0".repeat(64);
            }
            if mode == "size" {
                cfg.expected_bytes -= 1;
            }
            if mode == "slow" {
                cfg.timeout_ms = 50;
            }
            let source = PresignedObjectSource::new(cfg, url.clone()).unwrap();
            let error = match source.load().await {
                Ok(_) => panic!("unexpected accepted {mode}"),
                Err(e) => e.to_string(),
            };
            assert!(!error.contains(&url));
            assert!(!error.contains("fixture-credential"));
            assert!(!error.contains("signature"));
            assert!(!error.contains("secret-errorbody"));
            assert_eq!(count.load(Ordering::SeqCst), 1, "{mode}");
            worker.abort();
        }
    }
    #[tokio::test]
    async fn object_url_policy_and_binary_text_conversion_are_explicit() {
        for url in [
            "http://remote.invalid/key?secret=yes",
            "http://localhost/key?secret=yes",
            "https://user:password@host/key",
            "https://host/key#fragment",
            "file:///not-read",
        ] {
            let error = match PresignedObjectSource::new(config(BODY), url.into()) {
                Ok(_) => panic!("unexpected accepted URL"),
                Err(e) => e.to_string(),
            };
            assert!(!error.contains("password"));
            assert!(!error.contains("secret=yes"));
        }
        let (url, count, worker) = fixture("normal", vec![0xff, 0xfe, 0xfd]).await;
        let mut no_fixture = config(&[0xff, 0xfe, 0xfd]);
        no_fixture.allow_http_loopback_for_tests = false;
        assert!(PresignedObjectSource::new(no_fixture, url.clone()).is_err());
        assert_eq!(count.load(Ordering::SeqCst), 0);
        let source = PresignedObjectSource::new(config(&[0xff, 0xfe, 0xfd]), url).unwrap();
        let snapshot = source.load().await.unwrap();
        assert!(snapshot.document("binary", "Binary").is_err());
        worker.abort();
    }
}
