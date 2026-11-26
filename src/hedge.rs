//! Hedged requests: the same request sent again while the first is still
//! silent, the first answer kept.
//!
//! The model gateway sends nothing until an answer exists and is cut off when
//! that takes 30 s, while the same request answers in a few seconds one time
//! and runs past 30 s the next. Waiting out a slow call and then retrying it
//! spends the whole limit before the second try starts; a copy started after
//! `after` seconds races the slow one instead, and only slow calls cost the
//! extra request.

use std::future::Future;
use std::time::Duration;

use futures_util::stream::{FuturesUnordered, StreamExt};

/// Runs `request` and, while no copy has answered, starts another copy every
/// `after` until `copies` are in flight in total. The first `Ok` wins and the
/// other copies are dropped. A copy that fails while others are still running
/// only leaves the race to them; one that fails when none is running starts
/// the next copy at once. With every copy spent, the last error is returned.
/// `after` of zero or `copies` of at most one sends the request once.
pub async fn hedged<T, E, F, Fut>(after: Duration, copies: usize, request: F) -> Result<T, E>
where
    F: Fn() -> Fut,
    Fut: Future<Output = Result<T, E>>,
{
    if after.is_zero() || copies <= 1 {
        return request().await;
    }
    let mut in_flight = FuturesUnordered::new();
    in_flight.push(request());
    let mut started = 1;
    let mut next_copy_at = tokio::time::Instant::now() + after;
    loop {
        let can_start = started < copies;
        tokio::select! {
            Some(result) = in_flight.next() => match result {
                Ok(answer) => return Ok(answer),
                Err(error) => {
                    if !in_flight.is_empty() {
                        continue;
                    }
                    if !can_start {
                        return Err(error);
                    }
                    tracing::info!(copy = started + 1, "hedged request failed; starting the next copy now");
                    in_flight.push(request());
                    started += 1;
                    next_copy_at = tokio::time::Instant::now() + after;
                }
            },
            _ = tokio::time::sleep_until(next_copy_at), if can_start => {
                tracing::info!(copy = started + 1, after_seconds = after.as_secs(), "request still silent; racing a hedged copy");
                in_flight.push(request());
                started += 1;
                next_copy_at = tokio::time::Instant::now() + after;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::pin::Pin;
    use std::sync::atomic::{AtomicUsize, Ordering};

    type Answer = Result<u32, &'static str>;

    /// Copy `n` (0-based) answers `results[n]` after `delays[n]` time units of
    /// 20 ms.
    fn scripted<'a>(
        delays: &'static [u64],
        results: &'static [Answer],
        calls: &'a AtomicUsize,
    ) -> impl Fn() -> Pin<Box<dyn Future<Output = Answer> + 'a>> + 'a {
        move || {
            let index = calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move {
                tokio::time::sleep(unit(delays[index])).await;
                results[index]
            })
        }
    }

    fn unit(count: u64) -> Duration {
        Duration::from_millis(20 * count)
    }

    #[tokio::test]
    async fn a_fast_answer_sends_no_copy() {
        let calls = AtomicUsize::new(0);
        let answer = hedged(unit(10), 2, scripted(&[3], &[Ok(1)], &calls)).await;
        assert_eq!(answer, Ok(1));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_slow_call_is_raced_and_the_first_answer_wins() {
        let calls = AtomicUsize::new(0);
        let started = std::time::Instant::now();
        // The first copy would take 30 units; the copy started at 10 answers at 15.
        let answer = hedged(unit(10), 2, scripted(&[30, 5], &[Ok(1), Ok(2)], &calls)).await;
        assert_eq!(answer, Ok(2));
        assert!(started.elapsed() < unit(25), "{:?}", started.elapsed());
    }

    #[tokio::test]
    async fn a_failed_copy_leaves_the_race_to_the_others_and_the_last_error_returns() {
        let calls = AtomicUsize::new(0);
        let answer = hedged(
            unit(10),
            2,
            scripted(&[20, 2], &[Ok(1), Err("cut")], &calls),
        )
        .await;
        assert_eq!(answer, Ok(1), "the first copy still answers");
        let calls = AtomicUsize::new(0);
        let answer = hedged(
            unit(10),
            2,
            scripted(&[20, 20], &[Err("first"), Err("second")], &calls),
        )
        .await;
        assert_eq!(answer, Err("second"));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn an_early_failure_starts_the_next_copy_at_once() {
        let calls = AtomicUsize::new(0);
        let started = std::time::Instant::now();
        let answer = hedged(
            unit(50),
            2,
            scripted(&[1, 4], &[Err("busy"), Ok(2)], &calls),
        )
        .await;
        assert_eq!(answer, Ok(2));
        assert!(started.elapsed() < unit(20), "{:?}", started.elapsed());
    }

    #[tokio::test]
    async fn hedging_off_sends_once() {
        let calls = AtomicUsize::new(0);
        let answer = hedged(Duration::ZERO, 3, scripted(&[2], &[Err("cut")], &calls)).await;
        assert_eq!(answer, Err("cut"));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}
