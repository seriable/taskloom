#![cfg(feature = "research")]
use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use taskloom::runtime::rag::*;
use taskloom::runtime::{RuntimeError, Tool};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn server() -> (String, Arc<Mutex<Vec<String>>>, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let received = requests.clone();
    let address = base.clone();
    let handle = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let received = received.clone();
            let base = address.clone();
            tokio::spawn(async move {
                let mut request = Vec::new();
                let mut block = [0; 1024];
                while !request.windows(4).any(|v| v == b"\r\n\r\n") {
                    let n = socket.read(&mut block).await.unwrap();
                    if n == 0 {
                        return;
                    }
                    request.extend_from_slice(&block[..n]);
                    if request.len() > 16 * 1024 {
                        return;
                    }
                }
                let request = String::from_utf8_lossy(&request).into_owned();
                let route = request.split_whitespace().nth(1).unwrap_or("/").to_owned();
                received.lock().unwrap().push(request);
                let (status,headers,body)=match route.split('?').next().unwrap() {
                    "/page"=>("200 OK","Content-Type: text/html; charset=utf-8\r\n".to_string(),"<html><head><title>not body</title></head><body><h1>Approval research</h1><script>steal secrets</script><style>hidden style</style><template>execute malware</template><p>Tools require exact input approval &amp; recovery review.</p><iframe src='http://127.0.0.1/private'>never fetch frame</iframe></body></html>".to_string()),
                    "/redirect"=>("302 Found","Location: /page\r\n".into(),String::new()),
                    "/foreign"=>("302 Found","Location: http://127.0.0.1:1/private\r\n".into(),String::new()),
                    "/search-redirect"=>("302 Found","Location: /search\r\n".into(),String::new()),
                    "/stall"=>{tokio::time::sleep(Duration::from_millis(300)).await;("200 OK","Content-Type: text/plain\r\n".into(),"late".into())},
                    "/huge"=>{let _=socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n1000\r\n{}\r\n0\r\n\r\n","x".repeat(4096)).as_bytes()).await;return;},
                    "/charset"=>("200 OK","Content-Type: text/plain; charset=latin-1\r\n".into(),"no implicit recoding".into()),
                    "/search"=>("200 OK","Content-Type: application/json\r\n".into(),serde_json::json!({"web":{"results":[{"title":"Approval guide","url":format!("{base}/page"),"description":"Unverified lead"}]}}).to_string()),
                    _=>("503 Service Unavailable","Content-Type: text/plain\r\n".into(),"unavailable".into()),
                };
                let response=format!("HTTP/1.1 {status}\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len());
                let _ = socket.write_all(response.as_bytes()).await;
            });
        }
    });
    (base, requests, handle)
}
fn access(origin: &str) -> WebAccess {
    WebAccess {
        allowed_origins: BTreeSet::from([origin.into()]),
        allow_private: true,
        max_body_bytes: 32 * 1024,
        max_text_bytes: 16 * 1024,
        max_redirects: 2,
        timeout_ms: 1000,
    }
}

#[tokio::test]
async fn search_fetch_ingest_and_cite_preserves_exact_extracted_source_evidence() {
    let (base, requests, server) = server().await;
    let search = WebSearch::new(
        WebSearchConfig {
            endpoint: format!("{base}/search"),
            key_env: "FIXTURE_SEARCH_KEY".into(),
            namespace: "fixture-only".into(),
            access: access(&base),
        },
        "fixture-key-do-not-publish-as-real",
    )
    .unwrap();
    let hits = search.search("approval recovery", 2).await.unwrap();
    assert_eq!(hits.len(), 1);
    let page = WebClient::new(access(&base))
        .unwrap()
        .fetch(&hits[0].url, "source", "Approval guide")
        .await
        .unwrap();
    assert_eq!(
        page.text,
        "Approval research Tools require exact input approval & recovery review."
    );
    assert_eq!(page.source, format!("{base}/page"));
    assert_eq!(page.metadata["body_sha256"].len(), 64);
    assert!(
        !serde_json::to_string(&WebSearchTool(Arc::new(search)).definition())
            .unwrap()
            .contains("fixture-key-do-not-publish-as-real")
    );
    let dir = tempfile::tempdir().unwrap();
    let store = FileCorpusStore::new(dir.path());
    ingest(&store, page, &WindowChunker::default(), Some(0))
        .await
        .unwrap();
    let evidence = search_source(&store).await;
    store.remove("source", Some(1)).await.unwrap();
    let chunk = &evidence.hits[0].chunk;
    SearchResult::verify_all(
        std::slice::from_ref(&evidence),
        CitedReport {
            text: format!("Evidence [[{}]]", chunk.id),
            citations: vec![Citation {
                chunk_id: chunk.id.clone(),
                quote: "exact input approval".into(),
            }],
        },
    )
    .unwrap();
    let sent = requests.lock().unwrap();
    assert_eq!(sent.len(), 2);
    assert!(sent[0].contains("q=approval+recovery"));
    assert!(sent[0]
        .to_lowercase()
        .contains("x-subscription-token: fixture-key-do-not-publish-as-real"));
    assert!(!sent[1].to_lowercase().contains("x-subscription-token"));
    server.abort();
}
async fn search_source(store: &FileCorpusStore) -> SearchResult {
    search(store, &LexicalRetriever, &RetrievalQuery::new("approval"))
        .await
        .unwrap()
}

#[tokio::test]
async fn origins_and_private_addresses_are_denied_before_any_http_dispatch() {
    let (base, requests, server) = server().await;
    let mut policy = access(&base);
    policy.allow_private = false;
    let web = WebClient::new(policy).unwrap();
    assert!(matches!(
        web.fetch(&format!("{base}/page"), "a", "a").await,
        Err(RuntimeError::Permission(_))
    ));
    let web = WebClient::new(access("https://example.com")).unwrap();
    assert!(matches!(
        web.fetch(&format!("{base}/page"), "a", "a").await,
        Err(RuntimeError::Permission(_))
    ));
    assert!(requests.lock().unwrap().is_empty());
    server.abort();
}

#[tokio::test]
async fn redirects_are_rechecked_and_search_credentials_are_never_redirected() {
    let (base, requests, server) = server().await;
    let web = WebClient::new(access(&base)).unwrap();
    let page = web
        .fetch(&format!("{base}/redirect"), "a", "a")
        .await
        .unwrap();
    assert_eq!(page.source, format!("{base}/page"));
    assert!(matches!(
        web.fetch(&format!("{base}/foreign"), "a", "a").await,
        Err(RuntimeError::Permission(_))
    ));
    let search = WebSearch::new(
        WebSearchConfig {
            endpoint: format!("{base}/search-redirect"),
            key_env: "FIXTURE_SEARCH_KEY".into(),
            namespace: "fixture".into(),
            access: access(&base),
        },
        "private-fixture-header",
    )
    .unwrap();
    assert!(search.search("test", 1).await.is_err());
    assert_eq!(requests.lock().unwrap().len(), 4);
    server.abort();
}

#[tokio::test]
async fn chunked_size_deadline_status_and_charset_fail_without_retry_or_partial_document() {
    let (base, requests, server) = server().await;
    let mut limits = access(&base);
    limits.max_body_bytes = 512;
    limits.timeout_ms = 100;
    let web = WebClient::new(limits).unwrap();
    for route in ["huge", "stall", "unavailable", "charset"] {
        assert!(
            web.fetch(&format!("{base}/{route}"), "a", "a")
                .await
                .is_err(),
            "{route}"
        );
    }
    assert_eq!(requests.lock().unwrap().len(), 4);
    server.abort();
}

#[test]
fn web_grants_and_urls_cannot_expand_to_credentials_wildcards_or_other_schemes() {
    for origin in [
        "https://example.com/path",
        "https://user:pass@example.com",
        "file:///tmp",
        "https://example.com?key=value",
        "https://*.example.com",
    ] {
        assert!(WebClient::new(access(origin)).is_err(), "{origin}");
    }
}
