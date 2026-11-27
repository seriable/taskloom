use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::Semaphore;

use super::{invalid, OtlpTracePayload, TraceSnapshot};
use crate::runtime::RuntimeError;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OtlpHttpConfig {
    /// Host-owned base URL, without /v1/traces or /v1/metrics. No query,
    /// credentials or fragment. Cleartext HTTP is restricted to loopback.
    pub endpoint: String,
    pub service_name: String,
    /// Environment NAME only. Resolved once when the host constructs an
    /// exporter; no standard OTEL_* environment is discovered implicitly.
    pub bearer_env: Option<String>,
    pub timeout_ms: u64,
    pub max_in_flight: usize,
    pub max_payload_bytes: usize,
}
impl Default for OtlpHttpConfig {
    fn default() -> Self {
        Self {
            endpoint: "http://127.0.0.1:4318".into(),
            service_name: "taskloom".into(),
            bearer_env: None,
            timeout_ms: 10000,
            max_in_flight: 2,
            max_payload_bytes: 2 * 1024 * 1024,
        }
    }
}
impl OtlpHttpConfig {
    pub fn validate(&self) -> Result<(), RuntimeError> {
        if self.endpoint.len() > 8192 {
            return Err(invalid("OTLP endpoint exceeds 8192 bytes"));
        }
        let url =
            reqwest::Url::parse(&self.endpoint).map_err(|_| invalid("invalid OTLP base URL"))?;
        let host = url.host_str().unwrap_or("");
        let loopback = host == "localhost"
            || host
                .trim_matches(['[', ']'])
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback());
        if !matches!(url.scheme(), "http" | "https")
            || (url.scheme() == "http" && !loopback)
            || host.is_empty()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || url.path().ends_with("/v1/traces")
            || url.path().ends_with("/v1/metrics")
        {
            return Err(invalid("OTLP requires a host-owned HTTP(S) base URL without credentials/query/fragment; HTTP must be loopback"));
        }
        if !(100..=60000).contains(&self.timeout_ms)
            || !(1..=16).contains(&self.max_in_flight)
            || !(1024..=8 * 1024 * 1024).contains(&self.max_payload_bytes)
        {
            return Err(invalid("invalid OTLP timeout/concurrency/payload limits"));
        }
        if self.bearer_env.as_ref().is_some_and(|s| {
            s.is_empty()
                || s.len() > 128
                || !s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        }) {
            return Err(invalid("invalid OTLP bearer environment name"));
        }
        // Reuse the wire resource validation without reading credentials.
        if self.service_name.trim().is_empty()
            || self.service_name.len() > 128
            || self.service_name.chars().any(char::is_control)
        {
            return Err(invalid("invalid OTLP service name"));
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ExportAttempt {
    Accepted,
    Partial {
        rejected_items: u64,
    },
    HttpError {
        code: u16,
    },
    /// The destination may have accepted data. Never silently retry it.
    Unknown {
        reason: String,
    },
    NotSent {
        reason: String,
    },
}
#[derive(Debug, Clone, Serialize)]
pub struct ExportReceipt {
    pub source_revision_key: String,
    pub trace_id: String,
    pub traces: ExportAttempt,
    pub metrics: ExportAttempt,
    pub spans_sent: usize,
    pub omitted_open: usize,
    pub omitted_without_clock: usize,
    pub omitted_invalid_clock: usize,
    pub missing_children: usize,
}

/// Explicit request-scoped export: no background queue, automatic retry,
/// process-global subscriber, implicit proxy, redirect, or send-on-drop. A
/// bounded semaphore rejects excess work before allocating serialized payloads.
/// `shutdown` refuses new requests and waits a bounded time for admitted calls.
pub struct OtlpHttpExporter {
    config: OtlpHttpConfig,
    client: reqwest::Client,
    permits: Semaphore,
    closed: AtomicBool,
}
impl OtlpHttpExporter {
    pub fn new(config: OtlpHttpConfig) -> Result<Self, RuntimeError> {
        config.validate()?;
        let mut headers = reqwest::header::HeaderMap::new();
        if let Some(name) = &config.bearer_env {
            let token = std::env::var(name)
                .map_err(|_| invalid("configured OTLP credential environment is unavailable"))?;
            if token.is_empty()
                || token.len() > 4096
                || token.bytes().any(|b| b.is_ascii_whitespace())
            {
                return Err(invalid(
                    "configured OTLP credential has invalid size/whitespace",
                ));
            }
            let mut value = reqwest::header::HeaderValue::from_str(&format!("Bearer {token}"))
                .map_err(|_| invalid("invalid OTLP credential encoding"))?;
            value.set_sensitive(true);
            headers.insert(reqwest::header::AUTHORIZATION, value);
        }
        let client = reqwest::Client::builder()
            .default_headers(headers)
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_millis(config.timeout_ms))
            .build()
            .map_err(|_| invalid("OTLP HTTP client initialization failed"))?;
        Ok(Self {
            permits: Semaphore::new(config.max_in_flight),
            closed: AtomicBool::new(false),
            config,
            client,
        })
    }
    pub async fn export(&self, snapshot: &TraceSnapshot) -> Result<ExportReceipt, RuntimeError> {
        if self.closed.load(Ordering::Acquire) {
            return Err(invalid("exporter is shut down"));
        }
        let _permit = self.permits.try_acquire().map_err(|_| {
            RuntimeError::Conflict("OTLP exporter busy; no payload queued or sent".into())
        })?;
        if self.closed.load(Ordering::Acquire) {
            return Err(invalid("exporter is shut down"));
        }
        let OtlpTracePayload {
            body,
            exported_spans,
            omitted_open,
            omitted_without_clock,
            omitted_invalid_clock,
        } = snapshot.otlp_traces(&self.config.service_name)?;
        let traces = self.encode(&body)?;
        let metrics = self.encode(&snapshot.otlp_metrics(&self.config.service_name)?)?;
        let traces = if exported_spans == 0 {
            ExportAttempt::NotSent {
                reason: "no ended spans with valid recorded wall clocks".into(),
            }
        } else {
            self.send("traces", traces).await
        };
        let metrics = self.send("metrics", metrics).await;
        Ok(ExportReceipt {
            source_revision_key: snapshot.source_revision_key.clone(),
            trace_id: snapshot.trace_id.clone(),
            traces,
            metrics,
            spans_sent: exported_spans,
            omitted_open,
            omitted_without_clock,
            omitted_invalid_clock,
            missing_children: snapshot.missing_children,
        })
    }
    fn encode(&self, value: &Value) -> Result<Vec<u8>, RuntimeError> {
        let bytes = serde_json::to_vec(value).map_err(crate::runtime::store::storage)?;
        if bytes.len() > self.config.max_payload_bytes {
            return Err(invalid(
                "OTLP payload exceeds configured bound; nothing sent",
            ));
        }
        Ok(bytes)
    }
    async fn send(&self, signal: &str, bytes: Vec<u8>) -> ExportAttempt {
        let endpoint = format!("{}/v1/{signal}", self.config.endpoint.trim_end_matches('/'));
        let response = self
            .client
            .post(endpoint)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(bytes)
            .send()
            .await;
        let mut response = match response {
            Ok(r) => r,
            Err(_) => {
                return ExportAttempt::Unknown {
                    reason: "request failed or timed out; receiver acceptance is unknown".into(),
                }
            }
        };
        if !response.status().is_success() {
            return ExportAttempt::HttpError {
                code: response.status().as_u16(),
            };
        }
        let mut body = Vec::new();
        loop {
            match response.chunk().await {
                Ok(Some(chunk)) if body.len() + chunk.len() <= 16384 => {
                    body.extend_from_slice(&chunk)
                }
                Ok(Some(_)) => {
                    return ExportAttempt::Unknown {
                        reason: "response exceeded 16 KiB after dispatch".into(),
                    }
                }
                Err(_) => {
                    return ExportAttempt::Unknown {
                        reason: "response read failed after dispatch".into(),
                    }
                }
                Ok(None) => break,
            }
        }
        let value = if body.is_empty() {
            serde_json::json!({})
        } else {
            match serde_json::from_slice::<Value>(&body) {
                Ok(v) if v.is_object() => v,
                _ => {
                    return ExportAttempt::Unknown {
                        reason: "invalid OTLP JSON response after dispatch".into(),
                    }
                }
            }
        };
        if let Some(partial) = value.get("partialSuccess") {
            let field = if signal == "traces" {
                "rejectedSpans"
            } else {
                "rejectedDataPoints"
            };
            let count = partial.get(field).map(|v| {
                v.as_u64()
                    .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
            });
            match count {
                Some(Some(0)) | None
                    if partial
                        .get("errorMessage")
                        .is_none_or(|v| v.as_str() == Some("")) => {}
                Some(Some(rejected_items)) => return ExportAttempt::Partial { rejected_items },
                _ => {
                    return ExportAttempt::Unknown {
                        reason: "OTLP partial-success response needs operator inspection".into(),
                    }
                }
            }
        }
        ExportAttempt::Accepted
    }
    pub async fn shutdown(&self) -> Result<(), RuntimeError> {
        self.closed.store(true, Ordering::Release);
        if self.permits.is_closed() {
            return Ok(());
        }
        match tokio::time::timeout(Duration::from_millis(self.config.timeout_ms),self.permits.acquire_many(self.config.max_in_flight as u32)).await {
            Ok(Ok(permit))=>{self.permits.close();drop(permit);Ok(())},
            Ok(Err(_))=>Ok(()),
            Err(_)=>Err(invalid("exporter stopped accepting work; shutdown deadline reached with admitted requests still active")),
        }
    }
}
