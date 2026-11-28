#![cfg(feature = "openai")]
//! Actual loopback native protocol controls, not learned model/cloud acceptance.
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use taskloom::{
    gateway::{ModelPrice, PriceTable},
    native_providers::{NativeProtocol, NativeProvider},
    *,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
#[derive(Debug)]
struct Observed {
    path: String,
    headers: BTreeMap<String, String>,
    body: Value,
}
async fn server(
    body: Value,
    status: u16,
) -> (
    String,
    Arc<Mutex<Vec<Observed>>>,
    tokio::task::JoinHandle<()>,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let observed = Arc::new(Mutex::new(Vec::new()));
    let saved = observed.clone();
    let worker = tokio::spawn(async move {
        let (mut socket, _) = tokio::time::timeout(Duration::from_secs(5), listener.accept())
            .await
            .unwrap()
            .unwrap();
        let mut bytes = Vec::new();
        let mut buffer = [0u8; 4096];
        let end = loop {
            let n = socket.read(&mut buffer).await.unwrap();
            assert!(n > 0);
            bytes.extend_from_slice(&buffer[..n]);
            if let Some(at) = bytes.windows(4).position(|v| v == b"\r\n\r\n") {
                break at + 4;
            }
            assert!(bytes.len() < 65536);
        };
        let header = std::str::from_utf8(&bytes[..end]).unwrap();
        let path = header
            .lines()
            .next()
            .unwrap()
            .split_whitespace()
            .nth(1)
            .unwrap()
            .to_owned();
        let headers = header
            .lines()
            .skip(1)
            .filter_map(|line| line.split_once(':'))
            .map(|(k, v)| (k.to_ascii_lowercase(), v.trim().to_owned()))
            .collect::<BTreeMap<_, _>>();
        let length = headers["content-length"].parse::<usize>().unwrap();
        while bytes.len() < end + length {
            let n = socket.read(&mut buffer).await.unwrap();
            assert!(n > 0);
            bytes.extend_from_slice(&buffer[..n]);
        }
        let body_input = serde_json::from_slice(&bytes[end..end + length]).unwrap();
        saved.lock().unwrap().push(Observed {
            path,
            headers,
            body: body_input,
        });
        let bytes = serde_json::to_vec(&body).unwrap();
        let redirect = if status == 302 {
            "Location: http://127.0.0.1:9/no-follow\r\n"
        } else {
            ""
        };
        let head=format!("HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n{redirect}\r\n",bytes.len());
        socket.write_all(head.as_bytes()).await.unwrap();
        let _ = socket.write_all(&bytes).await;
        assert!(
            tokio::time::timeout(Duration::from_millis(50), listener.accept())
                .await
                .is_err()
        );
    });
    (base, observed, worker)
}
fn request() -> EmbeddingRequest {
    EmbeddingRequest {
        model: "embedding-fixture".into(),
        inputs: vec!["first 中文 source".into(), "second source".into()],
    }
}
fn gemini_usage() -> Value {
    json!({"promptTokenCount":13,"totalTokenCount":13})
}
#[tokio::test]
async fn gemini_batch_native_path_header_input_order_and_optional_usage_are_exact() {
    let (base,seen,worker)=server(json!({"embeddings":[{"values":[0.1,0.2]},{"values":[0.3,0.4]}],"usageMetadata":gemini_usage()}),200).await;
    let provider = NativeProvider::new(
        NativeProtocol::Gemini,
        format!("{base}/v1beta"),
        "native-embedding-fixture-key",
    )
    .unwrap();
    let result = provider.embed(&request()).await.unwrap();
    assert_eq!(result.vectors, vec![vec![0.1, 0.2], vec![0.3, 0.4]]);
    assert_eq!(result.usage.unwrap().prompt_tokens, Some(13));
    worker.await.unwrap();
    {
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(
            seen[0].path,
            "/v1beta/models/embedding-fixture:batchEmbedContents"
        );
        assert_eq!(
            seen[0].headers["x-goog-api-key"],
            "native-embedding-fixture-key"
        );
        assert!(!seen[0].headers.contains_key("authorization"));
        assert_eq!(
            seen[0].body["requests"][0]["model"],
            "models/embedding-fixture"
        );
        assert_eq!(
            seen[0].body["requests"][0]["content"]["parts"][0]["text"],
            "first 中文 source"
        );
        assert_eq!(
            seen[0].body["requests"][1]["content"]["parts"][0]["text"],
            "second source"
        );
    }
    let (base, _, worker) = server(
        json!({"embeddings":[{"values":[1.0]},{"values":[2.0]}]}),
        200,
    )
    .await;
    let provider = NativeProvider::new(
        NativeProtocol::Gemini,
        format!("{base}/v1beta"),
        "native-embedding-fixture-key",
    )
    .unwrap();
    assert!(provider.embed(&request()).await.unwrap().usage.is_none());
    worker.await.unwrap();
}
#[tokio::test]
async fn vertex_predict_is_one_native_batch_with_no_truncation_and_complete_token_statistics() {
    let response = json!({"predictions":[{"embeddings":{"values":[1.0,2.0],"statistics":{"token_count":3,"truncated":false}}},{"embeddings":{"values":[3.0,4.0],"statistics":{"token_count":5,"truncated":false}}}]});
    let (base, seen, worker) = server(response, 200).await;
    let provider = NativeProvider::new(
        NativeProtocol::Vertex,
        format!("{base}/v1/projects/host-project/locations/host-region/publishers/google"),
        "native-embedding-fixture-bearer",
    )
    .unwrap();
    let result = provider.embed(&request()).await.unwrap();
    assert_eq!(result.vectors, vec![vec![1.0, 2.0], vec![3.0, 4.0]]);
    assert_eq!(result.usage.unwrap().prompt_tokens, Some(8));
    worker.await.unwrap();
    {
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].path,"/v1/projects/host-project/locations/host-region/publishers/google/models/embedding-fixture:predict");
        assert_eq!(
            seen[0].headers["authorization"],
            "Bearer native-embedding-fixture-bearer"
        );
        assert_eq!(seen[0].body["parameters"]["autoTruncate"], false);
        assert_eq!(seen[0].body["instances"][0]["content"], "first 中文 source");
    }
    let (base,_,worker)=server(json!({"predictions":[{"embeddings":{"values":[1.0],"statistics":{"token_count":3}}},{"embeddings":{"values":[2.0]}}]}),200).await;
    let provider = NativeProvider::new(
        NativeProtocol::Vertex,
        format!("{base}/v1/projects/host-project/locations/host-region/publishers/google"),
        "native-embedding-fixture-bearer",
    )
    .unwrap();
    assert!(provider.embed(&request()).await.unwrap().usage.is_none());
    worker.await.unwrap();
}
fn gateway() -> Arc<Gateway> {
    Arc::new(Gateway::new(GatewayConfig {
        prices: PriceTable::new([ModelPrice::new("embedding-fixture", 1.0, 0.0)]),
        ..Default::default()
    }))
}
async fn via_gateway(
    provider: NativeProvider,
    gateway: Arc<Gateway>,
) -> Result<Embedded, TaskError> {
    let transport = GatewayTransport::new(gateway, provider).with_transient_attempts(1);
    let request = request();
    transport
        .embed(transport::EmbedRequest {
            label: "native-embedding-fixture",
            model: &request.model,
            inputs: &request.inputs,
            options: &TransportOptions::default(),
        })
        .await
}
#[tokio::test]
async fn malformed_numeric_order_cardinality_and_dimensions_are_rejected_after_usage_accounting() {
    for values in [
        json!([{"values":[1.0]},{"values":[2.0,3.0]}]),
        json!([{"values":[]},{"values":[]}]),
        json!([{"values":[1e100]},{"values":[2.0]}]),
        json!([{"index":1,"values":[1.0]},{"index":0,"values":[2.0]}]),
        json!([{"values":[1.0]},{"values":["not-numeric"]}]),
        json!([{"values":[1.0]}]),
    ] {
        let (base, seen, worker) = server(
            json!({"embeddings":values,"usageMetadata":gemini_usage()}),
            200,
        )
        .await;
        let provider = NativeProvider::new(
            NativeProtocol::Gemini,
            format!("{base}/v1beta"),
            "native-embedding-fixture-key",
        )
        .unwrap();
        let gateway = gateway();
        assert!(via_gateway(provider, gateway.clone()).await.is_err());
        worker.await.unwrap();
        assert_eq!(seen.lock().unwrap().len(), 1);
        let stats = gateway.stats();
        assert_eq!(stats.sent_attempts, 1);
        assert_eq!(stats.usage.input_tokens, 13);
        assert_eq!(stats.usage.cost_micros, 13);
        assert_eq!(stats.outcomes.get("schema_mismatch"), Some(&1));
    }
}
#[tokio::test]
async fn vertex_truncation_or_wrong_explicit_order_preserves_all_reported_usage_but_never_caches_vectors(
) {
    for flag in [true, false] {
        let mut response = json!({"predictions":[{"embeddings":{"values":[1.0],"statistics":{"token_count":3,"truncated":false}}},{"embeddings":{"values":[2.0],"statistics":{"token_count":5,"truncated":false}}}]});
        if flag {
            response["predictions"][1]["embeddings"]["statistics"]["truncated"] = json!(true);
        } else {
            response["predictions"][1]["index"] = json!(0);
        }
        let (base, _, worker) = server(response, 200).await;
        let provider = NativeProvider::new(
            NativeProtocol::Vertex,
            format!("{base}/v1/projects/host-project/locations/host-region/publishers/google"),
            "native-embedding-fixture-bearer",
        )
        .unwrap();
        let gateway = gateway();
        assert!(via_gateway(provider, gateway.clone()).await.is_err());
        worker.await.unwrap();
        assert_eq!(gateway.stats().usage.input_tokens, 8);
        assert_eq!(gateway.stats().outcomes.get("schema_mismatch"), Some(&1));
    }
}
#[tokio::test]
async fn unsupported_protocols_and_oversized_or_invalid_inputs_fail_before_dispatch() {
    for protocol in [NativeProtocol::Responses, NativeProtocol::Bedrock] {
        let provider = NativeProvider::new(
            protocol,
            "http://127.0.0.1:9/v1",
            "native-embedding-fixture-key",
        )
        .unwrap();
        assert!(matches!(
            provider.embed(&request()).await,
            Err(ModelError::Misconfigured { .. })
        ));
    }
    let provider = NativeProvider::new(
        NativeProtocol::Gemini,
        "http://127.0.0.1:9/v1beta",
        "native-embedding-fixture-key",
    )
    .unwrap();
    for request in [
        EmbeddingRequest {
            model: "embedding-fixture".into(),
            inputs: vec![],
        },
        EmbeddingRequest {
            model: "embedding-fixture".into(),
            inputs: vec!["text".into(); 33],
        },
        EmbeddingRequest {
            model: "model/path".into(),
            inputs: vec!["text".into()],
        },
        EmbeddingRequest {
            model: "embedding-fixture".into(),
            inputs: vec!["x".repeat(2 * 1024 * 1024)],
        },
    ] {
        assert!(matches!(
            provider.embed(&request).await,
            Err(ModelError::Misconfigured { .. })
        ));
    }
}
#[tokio::test]
async fn redirect_or_http_error_with_reported_usage_is_not_replayed_and_does_not_leak_body() {
    for status in [302, 500] {
        let (base,seen,worker)=server(json!({"error":{"message":"do-not-echo-sensitive-native-body"},"usageMetadata":gemini_usage()}),status).await;
        let provider = NativeProvider::new(
            NativeProtocol::Gemini,
            format!("{base}/v1beta"),
            "native-embedding-fixture-key",
        )
        .unwrap();
        let gateway = gateway();
        let error = via_gateway(provider, gateway.clone()).await.unwrap_err();
        assert!(!error.to_string().contains("do-not-echo"));
        worker.await.unwrap();
        assert_eq!(seen.lock().unwrap().len(), 1);
        assert_eq!(gateway.stats().usage.input_tokens, 13);
        assert_eq!(gateway.stats().sent_attempts, 1);
    }
}

#[tokio::test]
async fn missing_partial_or_inconsistent_native_usage_remains_unknown_without_synthetic_zero() {
    for usage in [
        json!({"promptTokenCount":13,"totalTokenCount":12}),
        json!({"promptTokenCount":"bad","totalTokenCount":13}),
        json!({"promptTokenCount":13,"totalTokenCount":13,"candidatesTokenCount":2}),
    ] {
        let (base, _, worker) = server(
            json!({"embeddings":[{"values":[1.0]},{"values":[2.0]}],"usageMetadata":usage}),
            200,
        )
        .await;
        let provider = NativeProvider::new(
            NativeProtocol::Gemini,
            format!("{base}/v1beta"),
            "native-embedding-fixture-key",
        )
        .unwrap();
        assert!(provider.embed(&request()).await.unwrap().usage.is_none());
        worker.await.unwrap();
    }
}

#[tokio::test]
async fn native_response_bound_model_mismatch_and_partial_counters_remain_explicitly_unknown() {
    let mut large = json!({"embeddings":[{"values":[1.0]},{"values":[2.0]}]});
    large["padding"] = json!("x".repeat(4 * 1024 * 1024));
    let (base, _, worker) = server(large, 200).await;
    let provider = NativeProvider::new(
        NativeProtocol::Gemini,
        format!("{base}/v1beta"),
        "native-embedding-fixture-key",
    )
    .unwrap();
    assert!(matches!(
        provider.embed(&request()).await,
        Err(ModelError::SchemaMismatch { .. })
    ));
    worker.await.unwrap();
    let (base,_,worker)=server(json!({"model":"other-model","embeddings":[{"values":[1.0]},{"values":[2.0]}],"usageMetadata":gemini_usage()}),200).await;
    let provider = NativeProvider::new(
        NativeProtocol::Gemini,
        format!("{base}/v1beta"),
        "native-embedding-fixture-key",
    )
    .unwrap();
    let response = provider.embed(&request()).await.unwrap();
    assert!(response.vectors.is_empty());
    assert!(response.usage.is_none());
    worker.await.unwrap();
    let (base,_,worker)=server(json!({"embeddings":[{"values":[1.0]},{"values":[2.0]}],"usageMetadata":{"totalTokenCount":13}}),200).await;
    let provider = NativeProvider::new(
        NativeProtocol::Gemini,
        format!("{base}/v1beta"),
        "native-embedding-fixture-key",
    )
    .unwrap();
    let usage = provider.embed(&request()).await.unwrap().usage.unwrap();
    assert_eq!(usage.prompt_tokens, None);
    assert_eq!(usage.total_tokens, Some(13));
    worker.await.unwrap();
}
