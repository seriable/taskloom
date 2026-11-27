//! Recoverable diagnostics derived from validated journals, not model output.
//! No subscriber, uploader, environment discovery or background task is started
//! by constructing a snapshot. OTLP network export is separately opt-in.
#[cfg(feature = "telemetry")]
mod export;
mod projection;
mod wire;

#[cfg(feature = "telemetry")]
pub use export::{ExportAttempt, ExportReceipt, OtlpHttpConfig, OtlpHttpExporter};
pub use projection::*;
pub use wire::OtlpTracePayload;

pub(crate) fn unix_ms() -> Option<u64> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| d.as_millis().try_into().ok())
}
fn invalid(detail: &str) -> super::RuntimeError {
    super::RuntimeError::Configuration(format!("observability: {detail}"))
}
