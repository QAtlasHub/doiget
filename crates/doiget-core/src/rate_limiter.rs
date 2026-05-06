//! Process-wide rate limiter for HTTP fetches across all `Source` impls.
//!
//! See `docs/SECURITY.md` (per-session fetch flood mitigation) and
//! `docs/SOURCES.md` §6 (Politeness defaults). The constants enforced here
//! are the load-bearing safeguards from `docs/LEGAL.md` §6 safeguard 8.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore};
use tokio::time::{sleep_until, Instant};

use crate::RateLimits;

/// Process-wide async rate limiter.
///
/// Enforces three invariants on every [`acquire`](RateLimiter::acquire):
///   1. **Global concurrency** — at most
///      [`RateLimits::max_concurrent_fetches`](crate::RateLimits::max_concurrent_fetches)
///      in flight at once.
///   2. **Global rate** — at most
///      [`RateLimits::max_fetches_per_second`](crate::RateLimits::max_fetches_per_second)
///      starts in any rolling one-second window.
///   3. **Per-source backoff** — at least
///      [`RateLimits::per_source_backoff_ms`](crate::RateLimits::per_source_backoff_ms)
///      between consecutive starts to the same source name.
///
/// The returned [`Permit`] holds the concurrency slot for the lifetime of the
/// value; drop it when the fetch is done.
///
/// 429 / `Retry-After` handling is split: the limiter only exposes the admin
/// hook [`sleep_for`](RateLimiter::sleep_for); the actual `Retry-After`
/// header parse and call lives at the `Source::fetch` call site, per
/// `docs/SOURCES.md` §6.
#[derive(Debug)]
pub struct RateLimiter {
    limits: RateLimits,
    sem: Arc<Semaphore>,
    // Global rolling-second window: timestamps of starts within the last second.
    global_starts: Arc<Mutex<Vec<Instant>>>,
    // Earliest-allowed start time per source name.
    per_source_next: Arc<Mutex<HashMap<String, Instant>>>,
}

/// Held while a fetch is in flight; releases the concurrency slot on drop.
#[derive(Debug)]
pub struct Permit {
    _slot: OwnedSemaphorePermit,
}

impl RateLimiter {
    /// Construct from the hard-coded [`RateLimits`] (the only public path).
    pub fn new(limits: RateLimits) -> Self {
        let max = limits.max_concurrent_fetches() as usize;
        Self {
            limits,
            sem: Arc::new(Semaphore::new(max)),
            global_starts: Arc::new(Mutex::new(Vec::new())),
            per_source_next: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Block until a slot is available, then return a [`Permit`].
    ///
    /// Order of waits, in this exact sequence:
    ///   1. global concurrency (semaphore acquire),
    ///   2. global rate cap (sleep if the rolling-second window is full),
    ///   3. per-source backoff (sleep until the source's `next` time).
    ///
    /// Lock-acquisition order is always `global_starts` first, THEN
    /// `per_source_next`. Any future call site that needs both locks MUST
    /// follow the same order to keep the system deadlock-free.
    pub async fn acquire(&self, source: &str) -> Permit {
        // Step 1: global concurrency — bounded by Semaphore::new(max).
        // `acquire_owned` only errors when the semaphore is closed; this
        // type never closes it (no `close()` call exists), so the Err arm
        // is structurally unreachable. The local `allow` is the documented
        // exception to the workspace `expect_used` lint.
        #[allow(clippy::expect_used)]
        let slot = self
            .sem
            .clone()
            .acquire_owned()
            .await
            .expect("rate-limiter semaphore is never closed");

        // Step 2: global rate cap. Loop until the rolling-second window has
        // room, sleeping until the oldest entry ages out if it does not.
        let max_per_sec = self.limits.max_fetches_per_second() as usize;
        let one_sec = Duration::from_secs(1);
        loop {
            let mut starts = self.global_starts.lock().await;
            let now = Instant::now();
            // Prune entries older than 1 s. starts is FIFO so we can drop
            // a contiguous prefix.
            let cutoff = now.checked_sub(one_sec).unwrap_or(now);
            let drop_count = starts.iter().take_while(|t| **t <= cutoff).count();
            if drop_count > 0 {
                starts.drain(..drop_count);
            }
            if starts.len() < max_per_sec {
                break;
            }
            // Window is full — wake at the moment the oldest entry ages out.
            // starts.len() >= max_per_sec >= 1 here, so [0] is safe.
            let wake = starts[0] + one_sec;
            drop(starts);
            sleep_until(wake).await;
        }

        // Step 3: per-source backoff. Acquire `per_source_next` strictly
        // after dropping `global_starts` above (lock order documented).
        let backoff = Duration::from_millis(self.limits.per_source_backoff_ms());
        let mut next_map = self.per_source_next.lock().await;
        let now = Instant::now();
        if let Some(&next) = next_map.get(source) {
            if now < next {
                drop(next_map);
                sleep_until(next).await;
                next_map = self.per_source_next.lock().await;
            }
        }
        // Record this start in both ledgers. We re-read `Instant::now()`
        // because we may have slept in step 2 or step 3.
        let start = Instant::now();
        next_map.insert(source.to_string(), start + backoff);
        drop(next_map);

        // Push the start timestamp into the global window. Done AFTER
        // releasing per_source_next to keep the documented lock order
        // (global → per-source) on every code path.
        let mut starts = self.global_starts.lock().await;
        starts.push(start);
        drop(starts);

        Permit { _slot: slot }
    }

    /// Tell the limiter to delay further starts to `source` by at least
    /// `dur`. Used when the source returns 429 with `Retry-After`.
    pub async fn sleep_for(&self, source: &str, dur: Duration) {
        let mut next_map = self.per_source_next.lock().await;
        let target = Instant::now() + dur;
        let entry = next_map.entry(source.to_string()).or_insert(target);
        if *entry < target {
            *entry = target;
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::{RateLimits, MAX_CONCURRENT_FETCHES, MAX_FETCHES_PER_SECOND};

    /// Convenience: shared `Arc<RateLimiter>` initialized from
    /// `RateLimits::HARD_CODED`.
    fn limiter() -> Arc<RateLimiter> {
        Arc::new(RateLimiter::new(RateLimits::HARD_CODED))
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn concurrent_acquires_respect_max_concurrency() {
        // Spawn 10 tasks racing to acquire; assert the live count never
        // exceeds MAX_CONCURRENT_FETCHES.
        let rl = limiter();
        let live = Arc::new(AtomicUsize::new(0));
        let max_seen = Arc::new(AtomicUsize::new(0));
        let mut handles = Vec::new();
        for i in 0..10u32 {
            let rl = rl.clone();
            let live = live.clone();
            let max_seen = max_seen.clone();
            let src = format!("src-{}", i);
            handles.push(tokio::spawn(async move {
                let permit = rl.acquire(&src).await;
                let now = live.fetch_add(1, Ordering::SeqCst) + 1;
                max_seen.fetch_max(now, Ordering::SeqCst);
                // Hold the permit briefly so peers contend.
                tokio::time::sleep(Duration::from_millis(50)).await;
                live.fetch_sub(1, Ordering::SeqCst);
                drop(permit);
            }));
        }
        for h in handles {
            h.await.expect("task ok");
        }
        let max = max_seen.load(Ordering::SeqCst);
        assert!(
            max <= MAX_CONCURRENT_FETCHES as usize,
            "max concurrent live = {}, expected <= {}",
            max,
            MAX_CONCURRENT_FETCHES
        );
        assert!(max > 0, "at least one acquire should succeed");
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn same_source_starts_separated_by_backoff() {
        // Two acquires for the same source must be at least
        // per_source_backoff_ms apart.
        let rl = limiter();
        let backoff_ms = RateLimits::HARD_CODED.per_source_backoff_ms();

        let t0 = Instant::now();
        let p0 = rl.acquire("crossref").await;
        drop(p0);
        let _p1 = rl.acquire("crossref").await;
        let elapsed = Instant::now().duration_since(t0);

        assert!(
            elapsed >= Duration::from_millis(backoff_ms),
            "elapsed {:?} < backoff {} ms",
            elapsed,
            backoff_ms
        );
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn different_sources_no_per_source_wait() {
        // Acquire source A, then source B back-to-back: per-source backoff
        // must not apply between distinct sources. (Global rate still
        // applies; with only two starts it does not bind.)
        let rl = limiter();
        let backoff = Duration::from_millis(RateLimits::HARD_CODED.per_source_backoff_ms());

        let t0 = Instant::now();
        let _p_a = rl.acquire("source-a").await;
        let _p_b = rl.acquire("source-b").await;
        let elapsed = Instant::now().duration_since(t0);

        assert!(
            elapsed < backoff,
            "elapsed {:?} should be well under per-source backoff {:?}",
            elapsed,
            backoff
        );
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn global_rate_caps_starts_per_second() {
        // Acquire 10 distinct sources back-to-back, dropping each permit
        // immediately so the concurrency cap (5) does not collide with the
        // rate cap we're trying to observe. Only MAX_FETCHES_PER_SECOND
        // starts may complete in the first second; the remainder must wait
        // for the rolling-second window to free.
        let rl = limiter();
        let max_per_sec = MAX_FETCHES_PER_SECOND as usize;

        let t0 = Instant::now();
        let mut completion_offsets: Vec<Duration> = Vec::with_capacity(10);
        for i in 0..10u32 {
            let src = format!("src-{}", i);
            let p = rl.acquire(&src).await;
            completion_offsets.push(Instant::now().duration_since(t0));
            drop(p); // release immediately — we are testing rate, not concurrency.
        }

        // Within the first second from t0, at most max_per_sec acquires
        // should have completed.
        let in_first_sec = completion_offsets
            .iter()
            .filter(|d| **d < Duration::from_secs(1))
            .count();
        assert!(
            in_first_sec <= max_per_sec,
            "{} starts completed in first second, expected <= {}",
            in_first_sec,
            max_per_sec
        );
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn sleep_for_delays_target_source() {
        // sleep_for("X", 500ms) then acquire("X") must take at least 500
        // ms; acquire("Y") in the same window must NOT be delayed by it.
        let rl = limiter();
        let delay = Duration::from_millis(500);
        rl.sleep_for("X", delay).await;

        // Y is unaffected.
        let t_y = Instant::now();
        let _p_y = rl.acquire("Y").await;
        let elapsed_y = Instant::now().duration_since(t_y);
        assert!(
            elapsed_y < delay,
            "Y elapsed {:?} should be far less than {:?}",
            elapsed_y,
            delay
        );

        // X is delayed by at least `delay`.
        let t_x = Instant::now();
        let _p_x = rl.acquire("X").await;
        let elapsed_x = Instant::now().duration_since(t_x);
        assert!(
            elapsed_x >= delay,
            "X elapsed {:?} < requested delay {:?}",
            elapsed_x,
            delay
        );
    }
}
