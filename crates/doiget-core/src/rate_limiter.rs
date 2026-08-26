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
    // #493: per-source concurrency, for sources whose terms cap it below
    // the global semaphore. Created lazily so a source with no override
    // costs nothing.
    per_source_sem: Arc<Mutex<HashMap<String, Arc<Semaphore>>>>,
}

/// Held while a fetch is in flight; releases the concurrency slot on drop.
#[derive(Debug)]
pub struct Permit {
    _slot: OwnedSemaphorePermit,
    // #493: held for the same lifetime as the global slot when the source
    // has a stricter concurrency cap.
    _source_slot: Option<OwnedSemaphorePermit>,
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
            per_source_sem: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Wait out the per-source interval for one ADDITIONAL request inside a
    /// fetch that already holds a [`Permit`].
    ///
    /// #493. arXiv's terms cap *requests*, and one arXiv attempt issues two
    /// of them -- the Atom feed, then the PDF -- under a single `acquire`.
    /// The second was previously unpaced, so even a perfectly serialised
    /// caller sent two requests back to back.
    ///
    /// Does not touch the concurrency slots: the caller already holds them,
    /// and taking them again would deadlock at `max_concurrent_for == 1`,
    /// which is exactly the arXiv case.
    pub async fn pace(&self, source: &str) {
        // The global cap admits this request too. Review of #493 caught the
        // first draft pushing a start into `global_starts` WITHOUT waiting
        // on the window -- which inflates the window for every other source
        // while never being bounded by it. It cannot bite at arXiv's 3 s
        // interval, and "it cannot bite in practice" is the reasoning that
        // produced #493 in the first place.
        self.await_global_rate_window().await;

        let backoff = Duration::from_millis(self.limits.backoff_ms_for(source));
        let mut next_map = self.per_source_next.lock().await;
        if let Some(&next) = next_map.get(source) {
            if Instant::now() < next {
                drop(next_map);
                sleep_until(next).await;
                next_map = self.per_source_next.lock().await;
            }
        }
        let start = Instant::now();
        next_map.insert(source.to_string(), start + backoff);
        drop(next_map);

        let mut starts = self.global_starts.lock().await;
        starts.push(start);
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

        // Step 2: global rate cap.
        self.await_global_rate_window().await;

        // Step 2b: per-source concurrency (#493). After the global
        // semaphore, so the global cap is still the ceiling, and before the
        // interval wait, so a source capped at one connection serialises
        // rather than piling up sleepers.
        let source_slot = {
            let cap = self.limits.max_concurrent_for(source) as usize;
            if cap >= self.limits.max_concurrent_fetches() as usize {
                None
            } else {
                let sem = {
                    let mut map = self.per_source_sem.lock().await;
                    Arc::clone(
                        map.entry(source.to_string())
                            .or_insert_with(|| Arc::new(Semaphore::new(cap))),
                    )
                };
                #[allow(clippy::expect_used)]
                Some(
                    sem.acquire_owned()
                        .await
                        .expect("per-source semaphore is never closed"),
                )
            }
        };

        // Step 3: per-source backoff. Acquire `per_source_next` strictly
        // after dropping `global_starts` above (lock order documented).
        //
        // #493: `backoff_ms_for`, not `per_source_backoff_ms` -- the
        // vendor's published guideline when it is stricter than the global
        // 200 ms floor.
        let backoff = Duration::from_millis(self.limits.backoff_ms_for(source));
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

        Permit {
            _slot: slot,
            _source_slot: source_slot,
        }
    }

    /// Block until the global rolling-second window has room.
    ///
    /// Loops because another task may take the slot between the wake and
    /// the re-check. Holds only `global_starts`, and never across a sleep,
    /// so it composes with the documented lock order.
    async fn await_global_rate_window(&self) {
        let max_per_sec = self.limits.max_fetches_per_second() as usize;
        let one_sec = Duration::from_secs(1);
        loop {
            let mut starts = self.global_starts.lock().await;
            let now = Instant::now();
            // Prune entries older than 1 s. `starts` is FIFO, so this is a
            // contiguous prefix.
            let cutoff = now.checked_sub(one_sec).unwrap_or(now);
            let drop_count = starts.iter().take_while(|t| **t <= cutoff).count();
            if drop_count > 0 {
                starts.drain(..drop_count);
            }
            if starts.len() < max_per_sec {
                return;
            }
            // Window is full -- wake when the oldest entry ages out.
            // `starts.len() >= max_per_sec >= 1` here, so `[0]` is safe.
            let wake = starts[0] + one_sec;
            drop(starts);
            sleep_until(wake).await;
        }
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

    // ---- #493: a vendor guideline stricter than the global cap ---------

    /// arXiv publishes one request every three seconds. The global cap is
    /// 5/s, so before #493 two consecutive arXiv requests were 200 ms
    /// apart -- 15x the permitted rate -- while three places in the tree
    /// asserted the global cap "comfortably respects" the guideline.
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn arxiv_requests_are_three_seconds_apart() {
        let rl = RateLimiter::new(RateLimits::HARD_CODED);
        let t0 = Instant::now();
        drop(rl.acquire("arxiv").await);
        drop(rl.acquire("arxiv").await);
        let elapsed = Instant::now() - t0;
        assert!(
            elapsed >= Duration::from_millis(3_000),
            "arXiv requests must be >= 3 s apart; got {elapsed:?}"
        );
    }

    /// The table only ever tightens. A source with no entry keeps the
    /// global 200 ms floor, so the fix cannot have slowed everything else
    /// down by accident.
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn a_source_without_an_override_keeps_the_global_backoff() {
        let rl = RateLimiter::new(RateLimits::HARD_CODED);
        let t0 = Instant::now();
        drop(rl.acquire("crossref").await);
        drop(rl.acquire("crossref").await);
        let elapsed = Instant::now() - t0;
        assert!(
            elapsed >= Duration::from_millis(200),
            "the global floor still applies; got {elapsed:?}"
        );
        assert!(
            elapsed < Duration::from_millis(3_000),
            "crossref has no override and must not inherit arXiv's; got {elapsed:?}"
        );
    }

    /// The second request of ONE arXiv attempt is paced too. arXiv caps
    /// requests, not attempts, and an attempt issues two -- the Atom feed
    /// and the PDF -- under a single permit (#493).
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn pace_spaces_a_second_request_inside_one_attempt() {
        let rl = RateLimiter::new(RateLimits::HARD_CODED);
        let permit = rl.acquire("arxiv").await;
        let t0 = Instant::now();
        // Deliberately while the permit is still held: `pace` must not
        // touch the concurrency slots, or arXiv's cap of one connection
        // would deadlock against itself.
        rl.pace("arxiv").await;
        let elapsed = Instant::now() - t0;
        drop(permit);
        assert!(
            elapsed >= Duration::from_millis(3_000),
            "the second leg must wait out the interval; got {elapsed:?}"
        );
    }

    /// `pace` is admitted by the global window, not merely recorded in it.
    ///
    /// Found by reading the diff, not by a failing test -- the first draft
    /// pushed a start into `global_starts` without ever waiting on the
    /// window, so an extra request inflated the cap for every other source
    /// while being bounded by none of it. Unreachable at arXiv's 3 s
    /// interval, which is precisely the argument that let #493 ship.
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn pace_is_admitted_by_the_global_window_too() {
        let rl = RateLimiter::new(RateLimits::HARD_CODED);
        // Fill the rolling second to the global maximum, on distinct
        // sources so no per-source interval is in play.
        for s in ["a", "b", "c", "d", "e"] {
            drop(rl.acquire(s).await);
        }
        let t0 = Instant::now();
        rl.pace("f").await;
        let elapsed = Instant::now() - t0;
        assert!(
            elapsed >= Duration::from_millis(900),
            "pace must wait for the global window exactly as acquire does; got {elapsed:?}"
        );
    }

    /// The table is a ceiling-tightener, not a general knob: an entry that
    /// tried to be *looser* than the global settings must not take effect.
    #[test]
    fn an_override_can_only_tighten() {
        let l = RateLimits::HARD_CODED;
        assert_eq!(l.backoff_ms_for("arxiv"), 3_000);
        assert_eq!(l.backoff_ms_for("crossref"), l.per_source_backoff_ms());
        assert_eq!(l.max_concurrent_for("arxiv"), 1);
        assert_eq!(l.max_concurrent_for("crossref"), l.max_concurrent_fetches());
        // Every entry is at least as strict as the global cap on both axes.
        // `backoff_ms_for` / `max_concurrent_for` enforce it at call time;
        // this pins the TABLE, so a future entry cannot be added in the
        // belief that it relaxes something and then silently do nothing.
        for (name, r) in crate::SOURCE_RATE_OVERRIDES {
            assert!(
                r.min_interval_ms >= l.per_source_backoff_ms(),
                "{name}: an override looser than the global floor is silently ignored"
            );
            assert!(
                r.max_concurrent <= l.max_concurrent_fetches(),
                "{name}: an override cannot raise the global concurrency cap"
            );
        }
    }
}
