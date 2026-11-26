//! Typed model-gateway failures.
//!
//! Every provider failure is classified exactly once, where the HTTP status
//! and response body are still available. Callers decide on the typed kind
//! (retry, compress, repair, rewrite) instead of matching error strings.

use std::fmt;

/// Stable marker carried by every `ContextOverflow` message, including the
/// legacy string conversion, so callers that have not yet switched to the
/// typed API can still recognise an over-budget request.
pub const CONTEXT_OVERFLOW_MARKER: &str = "context_overflow";

/// Stable marker for a provider whose circuit breaker is open. The message
/// also keeps the historical "gateway failed" wording that legacy transport
/// fallbacks recognise.
pub const CIRCUIT_OPEN_MARKER: &str = "ai_gateway_circuit_open";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelError {
    /// The complete logical call exhausted its time limit. Not retried.
    Deadline {
        limit: std::time::Duration,
        detail: String,
    },
    /// Connection failure, timeout, 408/429 or 5xx. Safe to retry unchanged.
    Transient { status: Option<u16>, detail: String },
    /// The request cannot fit the model window or the task input budget.
    /// Retrying the same request is pointless; the context must be reduced.
    ContextOverflow {
        required_tokens: Option<usize>,
        limit_tokens: Option<usize>,
        detail: String,
    },
    /// The provider answered, but the output is not usable JSON / was
    /// truncated / does not decode. Repair belongs to the generation layer.
    SchemaMismatch { detail: String },
    /// The provider refused the content (safety / content filter).
    ContentRejected { detail: String },
    /// Credentials were rejected (401/403).
    Auth { detail: String },
    /// Local configuration or request shape is invalid (missing model, 404,
    /// unsupported parameters, too many images, ...).
    Misconfigured { detail: String },
}

impl ModelError {
    pub fn transient(status: Option<u16>, detail: impl Into<String>) -> Self {
        Self::Transient {
            status,
            detail: detail.into(),
        }
    }

    pub fn schema_mismatch(detail: impl Into<String>) -> Self {
        Self::SchemaMismatch {
            detail: detail.into(),
        }
    }

    pub fn misconfigured(detail: impl Into<String>) -> Self {
        Self::Misconfigured {
            detail: detail.into(),
        }
    }

    /// Short machine-readable kind used by the call log and statistics.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Deadline { .. } => "deadline",
            Self::Transient { .. } => "transient",
            Self::ContextOverflow { .. } => CONTEXT_OVERFLOW_MARKER,
            Self::SchemaMismatch { .. } => "schema_mismatch",
            Self::ContentRejected { .. } => "content_rejected",
            Self::Auth { .. } => "auth",
            Self::Misconfigured { .. } => "misconfigured",
        }
    }

    pub fn is_transient(&self) -> bool {
        matches!(self, Self::Transient { .. })
    }

    pub fn is_context_overflow(&self) -> bool {
        matches!(self, Self::ContextOverflow { .. })
    }

    pub fn detail(&self) -> &str {
        match self {
            Self::Deadline { detail, .. }
            | Self::Transient { detail, .. }
            | Self::ContextOverflow { detail, .. }
            | Self::SchemaMismatch { detail }
            | Self::ContentRejected { detail }
            | Self::Auth { detail }
            | Self::Misconfigured { detail } => detail,
        }
    }

    /// Replace the human-readable detail while keeping the typed kind.
    pub fn with_detail(self, detail: impl Into<String>) -> Self {
        let detail = detail.into();
        match self {
            Self::Deadline { limit, .. } => Self::Deadline { limit, detail },
            Self::Transient { status, .. } => Self::Transient { status, detail },
            Self::ContextOverflow {
                required_tokens,
                limit_tokens,
                ..
            } => Self::ContextOverflow {
                required_tokens,
                limit_tokens,
                detail,
            },
            Self::SchemaMismatch { .. } => Self::SchemaMismatch { detail },
            Self::ContentRejected { .. } => Self::ContentRejected { detail },
            Self::Auth { .. } => Self::Auth { detail },
            Self::Misconfigured { .. } => Self::Misconfigured { detail },
        }
    }

    /// True when the provider said it cannot honour the requested structured
    /// output mode (or truncated it), so a compatible JSON fallback request is
    /// worth one more attempt.
    pub fn rejects_structured_output(&self) -> bool {
        !self.is_transient()
            && !self.is_context_overflow()
            && structured_output_rejection_text(&self.detail().to_ascii_lowercase())
    }

    /// Classify a non-success HTTP response from the provider.
    ///
    /// `detail` is the full human-readable message (kept verbatim so logs
    /// and legacy messages stay unchanged); `body` is the raw response body
    /// that is inspected for provider error codes.
    pub fn from_http_status(status: u16, body: &str, detail: impl Into<String>) -> Self {
        let detail = detail.into();
        let lower = body.to_ascii_lowercase();
        if status == 413 || context_overflow_text(&lower) {
            let (required_tokens, limit_tokens) = parse_context_token_counts(&lower);
            return Self::ContextOverflow {
                required_tokens,
                limit_tokens,
                detail,
            };
        }
        if matches!(status, 401 | 403) {
            return Self::Auth { detail };
        }
        if matches!(status, 408 | 425 | 429) || (500..=599).contains(&status) {
            return Self::Transient {
                status: Some(status),
                detail,
            };
        }
        // Some gateways wrap their own backend outage in a 400.
        if gateway_backend_outage_text(&lower) {
            return Self::Transient {
                status: Some(status),
                detail,
            };
        }
        if content_rejection_text(&lower) {
            return Self::ContentRejected { detail };
        }
        Self::Misconfigured { detail }
    }

    /// Classify an asynchronous provider task that reported `failed`. There
    /// is usually no HTTP status, only the provider's failure message (and
    /// sometimes an HTTP-like `code`). A request the provider will never
    /// accept stops immediately; an unrecognised failure stays `Transient`
    /// so a passing backend fault is still retried.
    pub fn from_async_task_failure(
        message: &str,
        status_code: Option<u16>,
        detail: impl Into<String>,
    ) -> Self {
        let detail = detail.into();
        if let Some(status) = status_code.filter(|status| (400..=599).contains(status)) {
            return Self::from_http_status(status, message, detail);
        }
        let lower = message.to_ascii_lowercase();
        if context_overflow_text(&lower) {
            let (required_tokens, limit_tokens) = parse_context_token_counts(&lower);
            return Self::ContextOverflow {
                required_tokens,
                limit_tokens,
                detail,
            };
        }
        if auth_failure_text(&lower) {
            return Self::Auth { detail };
        }
        if content_rejection_text(&lower) {
            return Self::ContentRejected { detail };
        }
        if permanent_request_failure_text(&lower) {
            return Self::Misconfigured { detail };
        }
        Self::Transient {
            status: None,
            detail,
        }
    }

    /// Classify a `reqwest` transport error (no HTTP status was received, or
    /// the body could not be read).
    #[cfg(any(feature = "openai", feature = "anthropic"))]
    pub fn from_transport_error(error: &reqwest::Error, detail: impl Into<String>) -> Self {
        let detail = detail.into();
        if error.is_builder() {
            return Self::Misconfigured { detail };
        }
        Self::Transient {
            status: error.status().map(|status| status.as_u16()),
            detail,
        }
    }

    /// Best-effort classifier for an error that is only known by its message
    /// (an error type of the caller's own, a legacy path). `invalid_request`
    /// says the caller already knows the request itself was refused as
    /// invalid (bad configuration, not found): such an error is `Auth` on a
    /// 401/403 and `Misconfigured` otherwise. New code should produce the
    /// typed error directly.
    pub fn classify_message(detail: impl Into<String>, invalid_request: bool) -> Self {
        let detail = detail.into();
        let lower = detail.to_ascii_lowercase();
        if invalid_request {
            if lower.contains("status 401") || lower.contains("status 403") {
                return Self::Auth { detail };
            }
            return Self::Misconfigured { detail };
        }
        if lower.contains("[404102]") || detail.contains("资源不存在") {
            return Self::Misconfigured { detail };
        }
        if lower.contains(CONTEXT_OVERFLOW_MARKER)
            || lower.contains("status 413")
            || context_overflow_text(&lower)
        {
            let (required_tokens, limit_tokens) = parse_context_token_counts(&lower);
            return Self::ContextOverflow {
                required_tokens,
                limit_tokens,
                detail,
            };
        }
        let status = parse_status_code(&lower);
        if matches!(status, Some(401 | 403)) {
            return Self::Auth { detail };
        }
        let transient = match status {
            Some(code) => {
                matches!(code, 408 | 425 | 429)
                    || (500..=599).contains(&code)
                    || gateway_backend_outage_text(&lower)
            }
            None => legacy_transient_text(&lower),
        };
        if transient {
            return Self::Transient { status, detail };
        }
        if content_rejection_text(&lower) {
            return Self::ContentRejected { detail };
        }
        Self::SchemaMismatch { detail }
    }
}

impl fmt::Display for ModelError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ContextOverflow {
                required_tokens,
                limit_tokens,
                detail,
            } => {
                write!(formatter, "{CONTEXT_OVERFLOW_MARKER}")?;
                if let (Some(required), Some(limit)) = (required_tokens, limit_tokens) {
                    write!(formatter, " (~{required} tokens, limit {limit} tokens)")?;
                }
                write!(formatter, ": {detail}")
            }
            other => formatter.write_str(other.detail()),
        }
    }
}

impl std::error::Error for ModelError {}

fn context_overflow_text(lower: &str) -> bool {
    [
        "context_too_large",
        "context_length_exceeded",
        "maximum context length",
        "context window",
        "request_too_large",
        "prompt is too long",
        "input is too long",
        "too many tokens",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
}

fn gateway_backend_outage_text(lower: &str) -> bool {
    lower.contains("[500002]")
        || lower.contains("数据库错误")
        || lower.contains("database error")
        || lower.contains("context deadline exceeded")
        || lower.contains("i/o timeout")
        || lower.contains("upstream timed out")
        || lower.contains("server overloaded")
        || lower.contains("overloaded_error")
}

fn content_rejection_text(lower: &str) -> bool {
    [
        "content_filter",
        "content_policy",
        "content policy",
        "data_inspection_failed",
        "inappropriate content",
        "safety system",
        "responsible ai",
        "sensitive_words",
        "内容安全",
        "敏感内容",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
}

fn auth_failure_text(lower: &str) -> bool {
    [
        "unauthorized",
        "invalid api key",
        "invalid_api_key",
        "incorrect api key",
        "authentication",
        "forbidden",
        "permission denied",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
}

/// Failure messages of a request the provider will never accept as sent:
/// the model or endpoint does not exist or may not be called this way.
fn permanent_request_failure_text(lower: &str) -> bool {
    [
        "model not found",
        "model_not_found",
        "unknown model",
        "invalid model",
        "unsupported model",
        "does not exist",
        "not supported",
        "[404102]",
        "资源不存在",
        "不可在该接口调用",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
}

fn structured_output_rejection_text(lower: &str) -> bool {
    lower.contains("response_format")
        || lower.contains("json_object")
        || lower.contains("json_schema")
        || lower.contains("json mode")
        || lower.contains("truncated")
        || lower.contains("max_tokens")
        || lower.contains("max_output_tokens")
        || lower.contains("unsupported")
        // Some compatible gateways wrap unsupported strict schemas in their
        // generic multimodal/provider error instead of naming response_format.
        || lower.contains("[404102]")
        || lower.contains("资源不存在")
}

fn legacy_transient_text(lower: &str) -> bool {
    gateway_backend_outage_text(lower)
        || [
            CIRCUIT_OPEN_MARKER,
            "transport timeout",
            "timeout",
            "timed out",
            "error sending request",
            "request failed",
            "connection reset",
            "connection refused",
            "connection closed",
            "broken pipe",
            "dns error",
            "429 too many requests",
            "too many requests",
        ]
        .iter()
        .any(|marker| lower.contains(marker))
}

/// Extract `status NNN` (or `http NNN`) from a legacy error message.
fn parse_status_code(lower: &str) -> Option<u16> {
    ["status ", "http "].iter().find_map(|marker| {
        lower.match_indices(marker).find_map(|(index, _)| {
            let digits = lower[index + marker.len()..]
                .chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>();
            (digits.len() == 3)
                .then(|| digits.parse::<u16>().ok())
                .flatten()
        })
    })
}

/// Parse the required and limit token counts from provider overflow bodies,
/// e.g. `context_too_large: input requires about 276743 tokens plus 5000
/// reserved output tokens; model limit is 258400 effective tokens` or the
/// OpenAI form `maximum context length is 128000 tokens. However, your
/// messages resulted in 130000 tokens`.
pub(crate) fn parse_context_token_counts(lower: &str) -> (Option<usize>, Option<usize>) {
    let required = number_after(lower, "requires about ")
        .or_else(|| number_after(lower, "resulted in "))
        .or_else(|| number_after(lower, "requires "))
        .or_else(|| number_after(lower, "requested "));
    let limit = number_after(lower, "limit is ")
        .or_else(|| number_after(lower, "maximum context length is "))
        .or_else(|| number_after(lower, "limit of "));
    (required, limit)
}

fn number_after(text: &str, marker: &str) -> Option<usize> {
    text.match_indices(marker).find_map(|(index, _)| {
        let digits = text[index + marker.len()..]
            .chars()
            .take_while(|character| character.is_ascii_digit() || *character == ',')
            .filter(char::is_ascii_digit)
            .collect::<String>();
        digits.parse::<usize>().ok()
    })
}
