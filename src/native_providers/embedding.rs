//! One-request Gemini/Vertex embedding protocols. No generation/tool fallback.
use super::{NativeProtocol, NativeProvider};
use crate::{
    gateway::{ModelError, TokenUsage},
    EmbeddingRequest, EmbeddingResponse,
};
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION};
use serde_json::{json, Value};
use std::time::Duration;

pub(super) async fn embed(
    provider: &NativeProvider,
    request: &EmbeddingRequest,
) -> Result<EmbeddingResponse, ModelError> {
    if !matches!(
        provider.protocol,
        NativeProtocol::Gemini | NativeProtocol::Vertex
    ) {
        return Err(ModelError::misconfigured("this native protocol has no generic embedding adapter; choose an explicit supported embedding protocol"));
    }
    if request.model.trim().is_empty()
        || request.model.len() > 512
        || request.model.contains('/')
        || request.model.chars().any(char::is_control)
        || request.inputs.is_empty()
        || request.inputs.len() > 32
        || request.inputs.iter().any(|input| input.trim().is_empty())
    {
        return Err(ModelError::misconfigured(
            "native embeddings require an explicit model identifier and1..32 nonempty inputs",
        ));
    }
    let mut url = reqwest::Url::parse(&provider.endpoint)
        .map_err(|_| ModelError::misconfigured("invalid embedding endpoint"))?;
    let action = if provider.protocol == NativeProtocol::Gemini {
        "batchEmbedContents"
    } else {
        "predict"
    };
    url.path_segments_mut()
        .map_err(|_| ModelError::misconfigured("invalid native embedding path"))?
        .pop_if_empty()
        .push("models")
        .push(&format!("{}:{action}", request.model));
    let body = if provider.protocol == NativeProtocol::Gemini {
        json!({"requests":request.inputs.iter().map(|input|json!({"model":format!("models/{}",request.model),"content":{"parts":[{"text":input}]}})).collect::<Vec<_>>()})
    } else {
        json!({"instances":request.inputs.iter().map(|input|json!({"content":input})).collect::<Vec<_>>(),"parameters":{"autoTruncate":false}})
    };
    let encoded = serde_json::to_vec(&body)
        .map_err(|_| ModelError::misconfigured("cannot serialize native embedding request"))?;
    if encoded.len() > 2 * 1024 * 1024 {
        return Err(ModelError::misconfigured(
            "complete serialized native embedding request exceeds2MiB",
        ));
    }
    if encoded
        .windows(provider.credential.len())
        .any(|window| window == provider.credential.as_bytes())
    {
        return Err(ModelError::misconfigured(
            "native embedding credential appeared in input body; dispatch refused",
        ));
    }
    let header = if provider.protocol == NativeProtocol::Gemini {
        "x-goog-api-key"
    } else {
        "authorization"
    };
    let mut auth = HeaderValue::from_str(&if provider.protocol == NativeProtocol::Gemini {
        provider.credential.clone()
    } else {
        format!("Bearer {}", provider.credential)
    })
    .map_err(|_| ModelError::misconfigured("invalid native embedding credential header"))?;
    auth.set_sensitive(true);
    let mut headers = HeaderMap::new();
    if header == "authorization" {
        headers.insert(AUTHORIZATION, auth);
    } else {
        headers.insert("x-goog-api-key", auth);
    }
    let client = reqwest::Client::builder()
        .retry(reqwest::retry::never())
        .default_headers(headers)
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .timeout(Duration::from_secs(120))
        .build()
        .map_err(|_| ModelError::misconfigured("cannot initialize native embedding client"))?;
    let mut response = client
        .post(url)
        .header("Content-Type", "application/json")
        .body(encoded)
        .send()
        .await
        .map_err(|error| {
            ModelError::from_transport_error(&error, "native embedding transport failed")
        })?;
    let status = response.status();
    let mut bytes = Vec::new();
    if response
        .content_length()
        .is_some_and(|length| length > 4 * 1024 * 1024)
    {
        return Err(ModelError::schema_mismatch(
            "native embedding response exceeds4MiB; usage unknown",
        ));
    }
    while let Some(part) = response.chunk().await.map_err(|error| {
        ModelError::from_transport_error(&error, "native embedding response interrupted")
    })? {
        if bytes.len().saturating_add(part.len()) > 4 * 1024 * 1024 {
            return Err(ModelError::schema_mismatch(
                "native embedding response exceeds4MiB; usage unknown",
            ));
        }
        bytes.extend_from_slice(&part);
    }
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| {
        ModelError::schema_mismatch("native embedding response is not JSON; usage unknown")
    })?;
    let values = if provider.protocol == NativeProtocol::Gemini {
        value.get("embeddings").and_then(Value::as_array)
    } else {
        value.get("predictions").and_then(Value::as_array)
    };
    let usage =
        if provider.protocol == NativeProtocol::Gemini || value.get("usageMetadata").is_some() {
            input_usage(&value)
        } else {
            (|| {
                let values = values?;
                if values.is_empty() {
                    return None;
                }
                let tokens = values.iter().try_fold(0u64, |sum, item| {
                    sum.checked_add(
                        item.pointer("/embeddings/statistics/token_count")?
                            .as_u64()?,
                    )
                })?;
                Some(TokenUsage {
                    prompt_tokens: Some(tokens),
                    completion_tokens: None,
                    total_tokens: Some(tokens),
                })
            })()
        };
    // EmbeddingResponse has no protocol-error-with-usage field. Return an
    // invalid vector envelope and known usage; GatewayTransport accounts usage
    // before rejecting cardinality/shape. No malformed vectors reach its cache.
    if !status.is_success() {
        return Ok(EmbeddingResponse {
            vectors: Vec::new(),
            usage,
        });
    }
    if value.get("model").is_some_and(|model| {
        model.as_str() != Some(request.model.as_str())
            && model.as_str() != Some(format!("models/{}", request.model).as_str())
    }) {
        return Ok(EmbeddingResponse {
            vectors: Vec::new(),
            usage: None,
        });
    }
    let Some(values) = values else {
        return Ok(EmbeddingResponse {
            vectors: Vec::new(),
            usage,
        });
    };
    let mut vectors = Vec::with_capacity(values.len());
    for (index, item) in values.iter().enumerate() {
        if item
            .get("index")
            .is_some_and(|v| v.as_u64() != Some(index as u64))
            || (provider.protocol == NativeProtocol::Vertex
                && item
                    .pointer("/embeddings/statistics/truncated")
                    .is_some_and(|value| value.as_bool() != Some(false)))
        {
            return Ok(EmbeddingResponse {
                vectors: Vec::new(),
                usage,
            });
        }
        let vector = if provider.protocol == NativeProtocol::Gemini {
            item.get("values")
        } else {
            item.pointer("/embeddings/values")
        };
        let Some(vector) = vector.and_then(Value::as_array).and_then(|values| {
            values
                .iter()
                .map(|value| value.as_f64().map(|value| value as f32))
                .collect::<Option<Vec<_>>>()
        }) else {
            return Ok(EmbeddingResponse {
                vectors: Vec::new(),
                usage,
            });
        };
        vectors.push(vector);
    }
    Ok(EmbeddingResponse { vectors, usage })
}

fn input_usage(value: &Value) -> Option<TokenUsage> {
    let usage = value.get("usageMetadata")?.as_object()?;
    let prompt = usage.get("promptTokenCount").and_then(Value::as_u64);
    let total = usage.get("totalTokenCount").and_then(Value::as_u64);
    if (prompt.is_none() && total.is_none())
        || usage
            .get("promptTokenCount")
            .is_some_and(|v| v.as_u64().is_none())
        || usage
            .get("totalTokenCount")
            .is_some_and(|v| v.as_u64().is_none())
        || matches!((prompt,total),(Some(a),Some(b)) if a!=b)
        || ["candidatesTokenCount", "thoughtsTokenCount"]
            .iter()
            .any(|field| usage.get(*field).is_some_and(|v| v.as_u64() != Some(0)))
    {
        return None;
    }
    Some(TokenUsage {
        prompt_tokens: prompt,
        completion_tokens: None,
        total_tokens: total,
    })
}
