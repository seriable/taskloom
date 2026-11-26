//! Many small requests of one step, run with bounded fan-out.
//!
//! A request that carries a whole episode is the one an AI gateway drops
//! first (minutes of wait, then a 502 or a timeout), while the same content
//! sent as a handful of small requests goes through. A caller splits its
//! input into chunks, runs one request per chunk here and composes the
//! answers itself; a chunk that fails is only a missing part.

use std::future::Future;
use std::time::{Duration, Instant};

use futures_util::StreamExt;

/// How long each chunk request may take and how many run at once.
#[derive(Debug, Clone, Copy)]
pub struct ChunkBudget {
    /// Cap of one chunk request, retries included.
    pub per_call: Duration,
    /// No chunk request runs past this instant: a chunk started late gets
    /// only the time left, and one that would start after it is skipped.
    pub deadline: Instant,
    /// Chunk requests in flight at once.
    pub fanout: usize,
}

impl ChunkBudget {
    /// The wait one chunk request starting now is given, or `None` when the
    /// deadline has passed.
    fn wait_from_now(&self) -> Option<Duration> {
        let left = self.deadline.checked_duration_since(Instant::now())?;
        let wait = left.min(self.per_call);
        (!wait.is_zero()).then_some(wait)
    }
}

/// Runs `request(index)` for every chunk index in `0..count`, at most
/// `budget.fanout` at a time, each bounded by its own wait. The answers come
/// back in chunk order; a chunk that failed, timed out or never started is
/// `None` and is logged under `label`, and it never cancels the other chunks.
pub async fn run_chunks<T, E, F, Fut>(
    label: &str,
    count: usize,
    budget: ChunkBudget,
    request: F,
) -> Vec<Option<T>>
where
    F: Fn(usize) -> Fut,
    Fut: Future<Output = Result<T, E>>,
    E: std::fmt::Display,
{
    let request = &request;
    futures_util::stream::iter(0..count)
        .map(|index| async move {
            let Some(wait) = budget.wait_from_now() else {
                tracing::warn!(
                    label,
                    chunk = index + 1,
                    count,
                    "chunk request skipped: the step's deadline has passed"
                );
                return None;
            };
            match tokio::time::timeout(wait, request(index)).await {
                Ok(Ok(answer)) => Some(answer),
                Ok(Err(error)) => {
                    tracing::warn!(label, chunk = index + 1, count, error = %error, "chunk request failed; only its part degrades");
                    None
                }
                Err(_) => {
                    tracing::warn!(
                        label,
                        chunk = index + 1,
                        count,
                        wait_seconds = wait.as_secs(),
                        "chunk request timed out; only its part degrades"
                    );
                    None
                }
            }
        })
        .buffered(budget.fanout.max(1))
        .collect()
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn budget(per_call_ms: u64) -> ChunkBudget {
        ChunkBudget {
            per_call: Duration::from_millis(per_call_ms),
            deadline: Instant::now() + Duration::from_secs(5),
            fanout: 2,
        }
    }

    #[tokio::test]
    async fn a_failed_or_slow_chunk_is_only_a_missing_part() {
        let answers = run_chunks("test", 4, budget(200), |index| async move {
            match index {
                1 => Err("502".to_string()),
                2 => {
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    Ok(index)
                }
                _ => Ok(index * 10),
            }
        })
        .await;
        assert_eq!(answers, vec![Some(0), None, None, Some(30)]);
    }

    #[tokio::test]
    async fn no_chunk_starts_after_the_deadline() {
        let answers = run_chunks(
            "test",
            2,
            ChunkBudget {
                per_call: Duration::from_secs(1),
                deadline: Instant::now(),
                fanout: 1,
            },
            |index| async move { Ok::<_, String>(index) },
        )
        .await;
        assert_eq!(answers, vec![None, None]);
    }
}
