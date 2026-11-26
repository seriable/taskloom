//! The retry number a request runs under.
//!
//! A request asked again because its answer was unusable should not be
//! sampled the same way: transports that derive a sampling seed from the
//! request mix [`current_retry_salt`] into it, and answer caches key by it.

use std::future::Future;

tokio::task_local! {
    /// How many times the request made underneath was already asked and
    /// answered unusably (0 on the first asking).
    static RETRY_SALT: u32;
}

/// Runs `future` as retry number `salt` of its request. Nested retries
/// combine (an inner transport retry of an outer re-reading).
pub async fn with_retry_salt<F: Future>(salt: u32, future: F) -> F::Output {
    let combined = current_retry_salt().wrapping_mul(31).wrapping_add(salt);
    RETRY_SALT.scope(combined, future).await
}

/// The retry salt the current task runs under (0 outside any retry).
pub fn current_retry_salt() -> u32 {
    RETRY_SALT.try_with(|salt| *salt).unwrap_or(0)
}
