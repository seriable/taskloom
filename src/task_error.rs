use crate::gateway::ModelError;

/// Why an answer was not usable: the task's own decoding or validation
/// refused it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rejection(pub String);

impl Rejection {
    pub fn new(reason: impl Into<String>) -> Self {
        Self(reason.into())
    }
}

impl std::fmt::Display for Rejection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl From<String> for Rejection {
    fn from(reason: String) -> Self {
        Self(reason)
    }
}

impl From<&str> for Rejection {
    fn from(reason: &str) -> Self {
        Self(reason.to_string())
    }
}

impl From<serde_json::Error> for Rejection {
    fn from(error: serde_json::Error) -> Self {
        Self(error.to_string())
    }
}

/// How a task failed. Callers branch on the kind, never on message text.
#[derive(Debug)]
/// New kinds may be added in minor releases; match with a wildcard arm.
#[non_exhaustive]
pub enum TaskError {
    /// Settings missing or unusable (an empty prompt, a refused input).
    /// Retrying does not help.
    Misconfigured(String),
    /// The provider gave up: retries and the fallback chain are spent, or
    /// the request is refused (context overflow, auth, content, no model
    /// configured). Keeps the gateway's classification.
    Provider(ModelError),
    /// Every model answered, but no answer decoded and validated.
    Rejected(Rejection),
    /// The task's own deadline passed.
    Deadline(std::time::Duration),
    /// Replay mode and no recorded answer for this request.
    NotRecorded(String),
    /// The run was cancelled ([`crate::CancelToken`]).
    Cancelled,
    /// A recording could not be persisted. The runner stops without
    /// sending another model request; step-cache writes remain best effort.
    Storage {
        path: std::path::PathBuf,
        source: std::io::Error,
    },
    /// A transport failure of the caller's own error type.
    Other(Box<dyn std::error::Error + Send + Sync>),
}

impl TaskError {
    /// Whether the gateway refused the request as too large for the model.
    pub fn is_context_overflow(&self) -> bool {
        matches!(self, Self::Provider(ModelError::ContextOverflow { .. }))
    }

    /// The provider error this failure stands for, for callers that branch
    /// on provider errors: a rejection is a schema mismatch, a misconfigured
    /// task a misconfiguration, anything else transient.
    pub fn into_model_error(self) -> ModelError {
        match self {
            Self::Provider(error) => error,
            Self::Rejected(rejection) => ModelError::schema_mismatch(rejection.0),
            Self::Misconfigured(detail) => ModelError::misconfigured(detail),
            Self::Deadline(limit) => ModelError::Deadline {
                limit,
                detail: format!("task exceeded its deadline of {limit:?}"),
            },
            Self::Storage { path, source } => ModelError::misconfigured(format!(
                "cannot persist recording {}: {source}",
                path.display()
            )),
            other => ModelError::transient(None, other.to_string()),
        }
    }
}

impl std::fmt::Display for TaskError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Misconfigured(detail) => write!(formatter, "{detail}"),
            Self::Provider(error) => write!(formatter, "{error}"),
            Self::Rejected(rejection) => write!(formatter, "{rejection}"),
            Self::Deadline(limit) => write!(formatter, "no answer within {}s", limit.as_secs()),
            Self::NotRecorded(key) => write!(formatter, "no recorded answer for {key}"),
            Self::Cancelled => write!(formatter, "the task was cancelled"),
            Self::Storage { path, source } => write!(
                formatter,
                "cannot persist recording {}: {source}",
                path.display()
            ),
            Self::Other(error) => write!(formatter, "{error}"),
        }
    }
}

impl std::error::Error for TaskError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Storage { source, .. } => Some(source),
            Self::Other(source) => Some(source.as_ref()),
            _ => None,
        }
    }
}

impl From<ModelError> for TaskError {
    fn from(error: ModelError) -> Self {
        match error {
            ModelError::Deadline { limit, .. } => Self::Deadline(limit),
            other => Self::Provider(other),
        }
    }
}
