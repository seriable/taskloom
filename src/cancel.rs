//! Cancelling running tasks.
//!
//! A token reaches a run in one of two ways: given to the agent
//! ([`crate::Agent::with_cancel`]), or *ambient* — every run inside
//! [`scope`] uses the scope's token, so a job can be made cancellable
//! without passing a token through every call that ends in a model request.
//! The ambient token follows tasks spawned through
//! [`crate::ledger::propagate`] / [`crate::ledger::spawn_scoped`].

use std::future::Future;
use std::sync::Arc;

use tokio::sync::watch;

/// Cancels the task runs it is given to ([`crate::Agent::with_cancel`]).
/// Clones share one signal. A cancelled run drops its in-flight requests
/// (hedged copies included) and returns [`crate::TaskError::Cancelled`];
/// dropping a request closes its connection, so a cancelled request is not
/// left running.
#[derive(Debug, Clone)]
pub struct CancelToken {
    sender: Arc<watch::Sender<bool>>,
}

impl Default for CancelToken {
    fn default() -> Self {
        Self::new()
    }
}

impl CancelToken {
    pub fn new() -> Self {
        Self {
            sender: Arc::new(watch::channel(false).0),
        }
    }

    /// Cancels every run using this token, now and later.
    pub fn cancel(&self) {
        self.sender.send_replace(true);
    }

    pub fn is_cancelled(&self) -> bool {
        *self.sender.borrow()
    }

    /// Resolves once the token is cancelled.
    pub async fn cancelled(&self) {
        let mut receiver = self.sender.subscribe();
        // The sender lives in `self`, so the channel cannot close here.
        let _ = receiver.wait_for(|cancelled| *cancelled).await;
    }
}

tokio::task_local! {
    static AMBIENT: CancelToken;
}

/// Runs `future` with `token` as the ambient cancel token: every task run
/// underneath without a token of its own is stopped by it.
pub async fn scope<F: Future>(token: CancelToken, future: F) -> F::Output {
    AMBIENT.scope(token, future).await
}

/// The ambient token of the [`scope`] this code runs under, if any.
pub fn current() -> Option<CancelToken> {
    AMBIENT.try_with(Clone::clone).ok()
}

/// `future` carrying the current ambient token (if any), for running on
/// another task.
pub fn propagate<F: Future>(future: F) -> impl Future<Output = F::Output> {
    let token = current();
    async move {
        match token {
            Some(token) => AMBIENT.scope(token, future).await,
            None => future.await,
        }
    }
}

/// Runs `work` stopped by `explicit`, else by the ambient token.
pub(crate) async fn run_under<T, F>(
    explicit: Option<&CancelToken>,
    work: F,
) -> Result<T, crate::TaskError>
where
    F: Future<Output = Result<T, crate::TaskError>>,
{
    // A task's state machine is large (model chain, protocol turns, hedge,
    // recording); kept on the heap so an unoptimized build does not overflow
    // a small stack (1 MiB on a Windows main thread).
    let work = Box::pin(work);
    let Some(token) = explicit.cloned().or_else(current) else {
        return work.await;
    };
    if token.is_cancelled() {
        return Err(crate::TaskError::Cancelled);
    }
    tokio::select! {
        result = work => result,
        () = token.cancelled() => Err(crate::TaskError::Cancelled),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_ambient_token_follows_scopes_and_propagated_tasks() {
        assert!(current().is_none());
        let token = CancelToken::new();
        scope(token.clone(), async {
            assert!(current().is_some());
            let spawned = tokio::spawn(propagate(async { current().is_some() }));
            assert!(spawned.await.unwrap());
            token.cancel();
            let result: Result<(), _> = run_under(None, std::future::pending()).await;
            assert!(matches!(result, Err(crate::TaskError::Cancelled)));
        })
        .await;
    }
}
