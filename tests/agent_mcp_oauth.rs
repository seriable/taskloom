#![cfg(feature = "mcp")]
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use std::time::Duration;
use taskloom::runtime::{mcp::*, *};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn read(socket: &mut tokio::net::TcpStream) -> (String, BTreeMap<String, String>, Vec<u8>) {
    let mut bytes = Vec::new();
    let mut buffer = [0; 4096];
    let end = loop {
        let count = socket.read(&mut buffer).await.unwrap();
        assert!(count > 0);
        bytes.extend_from_slice(&buffer[..count]);
        if let Some(at) = bytes.windows(4).position(|v| v == b"\r\n\r\n") {
            break at + 4;
        }
    };
    let head = std::str::from_utf8(&bytes[..end]).unwrap();
    let first = head.lines().next().unwrap().to_owned();
    let headers = head
        .lines()
        .skip(1)
        .filter_map(|line| line.split_once(':'))
        .map(|(k, v)| (k.to_ascii_lowercase(), v.trim().to_owned()))
        .collect::<BTreeMap<_, _>>();
    let length = headers
        .get("content-length")
        .map_or(0, |v| v.parse::<usize>().unwrap());
    while bytes.len() < end + length {
        let count = socket.read(&mut buffer).await.unwrap();
        assert!(count > 0);
        bytes.extend_from_slice(&buffer[..count]);
    }
    (first, headers, bytes[end..end + length].to_vec())
}
async fn reply(socket: &mut tokio::net::TcpStream, status: &str, value: Value) {
    let body = serde_json::to_vec(&value).unwrap();
    socket.write_all(format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",body.len()).as_bytes()).await.unwrap();
    socket.write_all(&body).await.unwrap();
}
async fn fixture(
    mode: &'static str,
) -> (
    McpOAuthConfig,
    Arc<AtomicUsize>,
    Arc<AtomicUsize>,
    tokio::task::JoinHandle<()>,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let config = McpOAuthConfig {
        resource: format!("{base}/mcp"),
        resource_metadata_url: format!("{base}/resource-metadata"),
        issuer: format!("{base}/issuer"),
        authorization_metadata_url: format!("{base}/authorization-metadata"),
        authorization_endpoint: format!("{base}/authorize"),
        token_endpoint: format!("{base}/token"),
        client_id: "explicit-public-mcp-client".into(),
        redirect_uri: format!("{base}/callback"),
        scopes: BTreeSet::from(["mcp:read".into()]),
        timeout: Duration::from_secs(2),
        authorization_ttl: Duration::from_secs(60),
        max_response_bytes: 16384,
        allow_http_loopback_for_tests: true,
    };
    let token_calls = Arc::new(AtomicUsize::new(0));
    let tool_calls = Arc::new(AtomicUsize::new(0));
    let tokens = token_calls.clone();
    let tools = tool_calls.clone();
    let cfg = config.clone();
    let worker = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let (first, headers, body) = read(&mut socket).await;
            assert!(!headers.contains_key("authorization") || first.starts_with("POST /mcp "));
            if first.starts_with("GET /resource-metadata ") {
                reply(&mut socket,"200 OK",json!({"resource":cfg.resource,"authorization_servers":[cfg.issuer],"scopes_supported":["mcp:read"]})).await;
            } else if first.starts_with("GET /authorization-metadata ") {
                if mode == "redirect" {
                    socket.write_all(b"HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:1/do-not-follow\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap();
                    continue;
                }
                reply(&mut socket,"200 OK",json!({
                    "issuer":if mode=="issuer" {"https://other.invalid"}else{&cfg.issuer},
                    "authorization_endpoint":cfg.authorization_endpoint,
                    "token_endpoint":if mode=="endpoint" {"https://do-not-send.invalid/token"}else{&cfg.token_endpoint},
                    "response_types_supported":["code"],"grant_types_supported":["authorization_code"],
                    "code_challenge_methods_supported":if mode=="pkce"{vec!["plain"]}else{vec!["S256"]},
                    "token_endpoint_auth_methods_supported":["none"],
                    "authorization_response_iss_parameter_supported":true,"scopes_supported":["mcp:read"]
                })).await;
            } else if first.starts_with("POST /token ") {
                tokens.fetch_add(1, Ordering::SeqCst);
                assert_eq!(headers["content-type"], "application/x-www-form-urlencoded");
                let form = reqwest::Url::parse(&format!(
                    "http://fixture/?{}",
                    std::str::from_utf8(&body).unwrap()
                ))
                .unwrap()
                .query_pairs()
                .map(|(k, v)| (k.into_owned(), v.into_owned()))
                .collect::<BTreeMap<_, _>>();
                assert_eq!(form.len(), 6);
                assert_eq!(form["grant_type"], "authorization_code");
                assert_eq!(form["client_id"], cfg.client_id);
                assert_eq!(form["redirect_uri"], cfg.redirect_uri);
                assert_eq!(form["resource"], cfg.resource);
                let parts = form["code"].split_once('.').unwrap();
                assert_eq!(parts.0, "fixture-code");
                assert_eq!(
                    URL_SAFE_NO_PAD.encode(Sha256::digest(form["code_verifier"].as_bytes())),
                    parts.1
                );
                let token = json!({"access_token":"opaque-fixture-token-never-public","token_type":if mode=="type"{"MAC"}else{"Bearer"},"scope":if mode=="scope"{"mcp:admin"}else{"mcp:read"},"expires_in":if mode=="expiry"{0}else if mode=="slow-token"{1}else{60}});
                if mode == "slow-token" {
                    let body = serde_json::to_vec(&token).unwrap();
                    socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:x}\r\n",body.len()).as_bytes()).await.unwrap();
                    socket.write_all(&body[..1]).await.unwrap();
                    tokio::time::sleep(Duration::from_millis(1100)).await;
                    socket.write_all(&body[1..]).await.unwrap();
                    socket.write_all(b"\r\n0\r\n\r\n").await.unwrap();
                    continue;
                }
                reply(
                    &mut socket,
                    if mode == "failed-exchange" {
                        "500 Internal Server Error"
                    } else {
                        "200 OK"
                    },
                    token,
                )
                .await;
            } else if first.starts_with("POST /mcp ") {
                assert_eq!(
                    headers["authorization"],
                    "Bearer opaque-fixture-token-never-public"
                );
                let request: Value = serde_json::from_slice(&body).unwrap();
                let result = match request["method"].as_str().unwrap() {
                    "initialize" => {
                        json!({"protocolVersion":"2025-06-18","capabilities":{"tools":{}}})
                    }
                    "notifications/initialized" => {
                        socket.write_all(b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap();
                        continue;
                    }
                    "tools/list" => {
                        json!({"tools":[{"name":"echo","inputSchema":{"type":"object"}}]})
                    }
                    "tools/call" => {
                        tools.fetch_add(1, Ordering::SeqCst);
                        json!({"content":[{"type":"text","text":"authenticated fixture result"}]})
                    }
                    _ => panic!("unexpected method"),
                };
                reply(
                    &mut socket,
                    "200 OK",
                    json!({"jsonrpc":"2.0","id":request["id"],"result":result}),
                )
                .await;
            } else {
                panic!("untrusted discovery endpoint followed: {first}");
            }
        }
    });
    (config, token_calls, tool_calls, worker)
}
fn callback(flow: &McpOAuthAuthorization, config: &McpOAuthConfig) -> String {
    let authorize = reqwest::Url::parse(flow.authorization_url()).unwrap();
    let query = authorize
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(query["resource"], config.resource);
    assert_eq!(query["client_id"], config.client_id);
    assert_eq!(query["redirect_uri"], config.redirect_uri);
    assert_eq!(query["code_challenge_method"], "S256");
    assert!(!query.contains_key("code_verifier"));
    let mut url = reqwest::Url::parse(&config.redirect_uri).unwrap();
    url.query_pairs_mut()
        .append_pair("code", &format!("fixture-code.{}", query["code_challenge"]))
        .append_pair("state", &query["state"])
        .append_pair("iss", &config.issuer);
    url.into()
}
#[tokio::test]
async fn pinned_oauth_pkce_code_exchange_and_resource_only_credential_preserve_explicit_tool_grants(
) {
    let (config, tokens, tools, worker) = fixture("normal").await;
    let client = McpOAuthClient::discover(config.clone()).await.unwrap();
    let flow = client.begin().unwrap();
    let second = client.begin().unwrap();
    assert_ne!(flow.authorization_url(), second.authorization_url());
    let url = callback(&flow, &config);
    let credential = flow.exchange(&url).await.unwrap();
    let mcp = McpClient::connect_with_oauth(McpHttpConfig::new(&config.resource), credential)
        .await
        .unwrap();
    let tool = mcp
        .bind(McpToolBinding {
            name: "read".into(),
            remote_name: "echo".into(),
            effect: ToolEffect::ReadOnly,
            permissions: BTreeSet::from(["explicit-host-read".into()]),
            description: Some("host declaration".into()),
        })
        .unwrap();
    assert_eq!(
        tool.definition().permissions,
        BTreeSet::from(["explicit-host-read".into()])
    );
    let result = tool
        .execute(
            json!({}),
            ToolContext {
                run_id: "oauth-fixture".into(),
                call_id: "read".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(result["content"][0]["text"], "authenticated fixture result");
    assert_eq!(tokens.load(Ordering::SeqCst), 1);
    assert_eq!(tools.load(Ordering::SeqCst), 1);
    worker.abort();
}
#[tokio::test]
async fn discovery_never_accepts_changed_issuer_token_endpoint_weak_pkce_or_redirect() {
    for mode in ["issuer", "endpoint", "pkce", "redirect"] {
        let (config, tokens, _, worker) = fixture(mode).await;
        assert!(McpOAuthClient::discover(config).await.is_err(), "{mode}");
        assert_eq!(tokens.load(Ordering::SeqCst), 0);
        worker.abort();
    }
}
#[tokio::test]
async fn callback_state_issuer_duplicate_query_and_redirect_mismatch_are_pre_exchange() {
    let (config, tokens, _, worker) = fixture("normal").await;
    let client = McpOAuthClient::discover(config.clone()).await.unwrap();
    for mode in ["state", "issuer", "duplicate", "redirect"] {
        let flow = client.begin().unwrap();
        let original = callback(&flow, &config);
        let mut url = reqwest::Url::parse(&original).unwrap();
        match mode {
            "state" => {
                let query = url
                    .query_pairs()
                    .map(|(key, value)| {
                        let replacement = if key == "state" {
                            "wrong".into()
                        } else {
                            value.into_owned()
                        };
                        (key.into_owned(), replacement)
                    })
                    .collect::<Vec<_>>();
                url.set_query(None);
                url.query_pairs_mut().extend_pairs(query);
            }
            "issuer" => {
                let query = url
                    .query_pairs()
                    .map(|(key, value)| {
                        let replacement = if key == "iss" {
                            "https://other.invalid".into()
                        } else {
                            value.into_owned()
                        };
                        (key.into_owned(), replacement)
                    })
                    .collect::<Vec<_>>();
                url.set_query(None);
                url.query_pairs_mut().extend_pairs(query);
            }
            "duplicate" => {
                url.query_pairs_mut().append_pair("code", "second-code");
            }
            "redirect" => url.set_path("/different-callback"),
            _ => unreachable!(),
        }
        assert!(flow.exchange(url.as_str()).await.is_err());
    }
    assert_eq!(tokens.load(Ordering::SeqCst), 0);
    worker.abort();
}
#[tokio::test]
async fn unsuccessful_or_scope_type_expiry_invalid_token_never_becomes_credential_and_exchange_not_retried(
) {
    for mode in ["scope", "type", "expiry", "failed-exchange"] {
        let (config, tokens, _, worker) = fixture(mode).await;
        let client = McpOAuthClient::discover(config.clone()).await.unwrap();
        let flow = client.begin().unwrap();
        let error = match flow
            .exchange(&callback(&client.begin().unwrap(), &config))
            .await
        {
            Err(error) => error,
            Ok(_) => panic!("different flow's state accepted"),
        };
        assert!(!error.to_string().contains("opaque-fixture"));
        assert_eq!(tokens.load(Ordering::SeqCst), 0);
        let flow = client.begin().unwrap();
        let url = callback(&flow, &config);
        assert!(flow.exchange(&url).await.is_err());
        assert_eq!(tokens.load(Ordering::SeqCst), 1);
        worker.abort();
    }
}
#[tokio::test]
async fn oauth_token_cannot_target_other_resource_or_override_another_bearer() {
    let (config, tokens, _, worker) = fixture("normal").await;
    let client = McpOAuthClient::discover(config.clone()).await.unwrap();
    for other in [false, true] {
        let flow = client.begin().unwrap();
        let url = callback(&flow, &config);
        let credential = flow.exchange(&url).await.unwrap();
        let mut http = McpHttpConfig::new(if other {
            format!("{}/different", config.resource)
        } else {
            config.resource.clone()
        });
        if !other {
            http.bearer_token = Some("host-existing-token".into());
        }
        assert!(McpClient::connect_with_oauth(http, credential)
            .await
            .is_err());
    }
    assert_eq!(tokens.load(Ordering::SeqCst), 2);
    worker.abort();
}
#[tokio::test]
async fn delayed_chunked_token_cannot_extend_expiry_from_response_completion() {
    let (config, tokens, _, worker) = fixture("slow-token").await;
    let client = McpOAuthClient::discover(config.clone()).await.unwrap();
    let flow = client.begin().unwrap();
    let url = callback(&flow, &config);
    assert!(flow.exchange(&url).await.is_err());
    assert_eq!(tokens.load(Ordering::SeqCst), 1);
    worker.abort();
}
