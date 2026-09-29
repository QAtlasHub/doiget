//! Repeat suppression: a session that already asked about a ref, and was
//! told something a retry cannot change yet, is not sent back to the network
//! for the same answer (#507, ADR-0057).
//!
//! The rate cap bounds how fast doiget asks. It did not bound how often it
//! asks the same thing: an agent retrying 500 refused DOIs ten times each is
//! 5,000 requests without ever exceeding 5/s. The provenance log already
//! records, per call, what the caller was told (`session_end` rows carry
//! `ref` and `error_code`, #507 step 1), so the data to answer "has this
//! session been told this already?" exists before the network is touched.
//! [`RepeatIndex`] is that answer, fed from the log as it is written.
//!
//! Scope, and why each bound is what it is:
//!
//! - **One session**: a `doiget serve` process, or one CLI run (`batch`). The
//!   capability profile is fixed for its lifetime, so "the configuration has
//!   not changed" holds by construction -- except for `config.toml`, which
//!   the HTTP client reads per call. Each entry therefore carries a
//!   fingerprint of that file, and a changed file lifts the replay.
//! - **By disposition** (ADR-0055): `terminal` and `needs_config` answers are
//!   replayed for [`REPLAY_WINDOW`]; a `retry_after` answer is let through
//!   once [`RETRY_AFTER_GAP`] has passed and refused with the true remaining
//!   time before that. Backoff keeps working; hammering does not.
//! - **Never silent**: a replay is an error the caller can see is a replay,
//!   with the time of the original answer.
//! - **Always overridable by the caller, never by configuration**: `force`
//!   on the request asks anyway, and the log records that it did. There is no
//!   setting that turns suppression off, which is what LEGAL.md 6a's
//!   "cannot be overridden by configuration" requires of a safeguard.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};

use crate::{Disposition, ErrorCode, Ref};

/// How long a `terminal` or `needs_config` answer is replayed.
pub const REPLAY_WINDOW: Duration = Duration::from_secs(10 * 60);

/// The minimum gap before a `retry_after` answer is asked again.
pub const RETRY_AFTER_GAP: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
struct Entry {
    code: ErrorCode,
    at: Instant,
    at_utc: DateTime<Utc>,
    fingerprint: u64,
}

/// What a new request for a ref should do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Ask the network.
    Proceed,
    /// Report the earlier answer again without asking.
    Replay {
        /// The code the caller was given.
        code: ErrorCode,
        /// When, RFC 3339.
        at: String,
    },
    /// The earlier answer said to back off, and not long enough ago.
    Wait {
        /// The earlier code.
        code: ErrorCode,
        /// When it was given, RFC 3339.
        at: String,
        /// Seconds until a retry is let through.
        remaining_secs: u64,
    },
}

/// Recent answers, per ref, for one session.
#[derive(Debug, Default)]
pub struct RepeatIndex {
    entries: Mutex<HashMap<String, Entry>>,
}

impl RepeatIndex {
    /// Record what a call told its caller about `ref_input`: an error code,
    /// or `None` for a clean success (which clears the ref).
    ///
    /// An answer with the same disposition under the same configuration
    /// keeps the FIRST time: a caller looping on a refusal must not slide
    /// the window forward. That rule is also what makes a replay's own
    /// bookend harmless -- it repeats the disposition it replayed (a `Wait`
    /// reports `RATE_LIMITED`, which is `retry_after` like what it waits on)
    /// -- without marking requests, so a real answer arriving concurrently
    /// is never mistaken for one. A different disposition, or a success,
    /// replaces the entry.
    pub fn observe(&self, ref_input: &str, code: Option<ErrorCode>) {
        self.observe_at(ref_input, code, Instant::now(), config_fingerprint());
    }

    /// [`RepeatIndex::observe`] with an explicit clock and fingerprint.
    pub fn observe_at(
        &self,
        ref_input: &str,
        code: Option<ErrorCode>,
        now: Instant,
        fingerprint: u64,
    ) {
        let key = key(ref_input);
        let mut map = self.lock();
        match code {
            None => {
                map.remove(&key);
            }
            Some(code) => {
                let same = |e: &Entry| {
                    e.fingerprint == fingerprint && e.code.disposition() == code.disposition()
                };
                if let Some(e) = map.get_mut(&key).filter(|e| same(e)) {
                    // Keep the time. A terminal answer takes the newer code
                    // (what the caller was last told); a wait keeps the code
                    // it is waiting on, since its replay reports RATE_LIMITED.
                    if code.disposition() != Disposition::RetryAfter {
                        e.code = code;
                    }
                } else {
                    // A new entry sweeps the ones no rule can act on any
                    // more (#649 review): nothing replays or waits past
                    // REPLAY_WINDOW, and a `doiget serve` session otherwise
                    // kept one entry per refused ref for its whole life.
                    map.retain(|_, e| now.saturating_duration_since(e.at) < REPLAY_WINDOW);
                    map.insert(
                        key,
                        Entry {
                            code,
                            at: now,
                            at_utc: Utc::now(),
                            fingerprint,
                        },
                    );
                }
            }
        }
    }

    /// The map, recovered from a poisoned lock: a panic elsewhere must not
    /// silently switch suppression off for the rest of the session, which
    /// would be the configuration-free off switch ADR-0057 rules out. Every
    /// write leaves the map consistent, so the data is still sound.
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Entry>> {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// What a request for `ref_input` should do now.
    #[must_use]
    pub fn check(&self, ref_input: &str) -> Verdict {
        self.check_at(ref_input, Instant::now(), config_fingerprint())
    }

    /// [`RepeatIndex::check`] with an explicit clock and fingerprint.
    #[must_use]
    pub fn check_at(&self, ref_input: &str, now: Instant, fingerprint: u64) -> Verdict {
        let map = self.lock();
        let Some(e) = map.get(&key(ref_input)) else {
            return Verdict::Proceed;
        };
        if e.fingerprint != fingerprint {
            return Verdict::Proceed;
        }
        let elapsed = now.saturating_duration_since(e.at);
        let at = e.at_utc.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        match e.code.disposition() {
            Disposition::Terminal | Disposition::NeedsConfig if elapsed < REPLAY_WINDOW => {
                Verdict::Replay { code: e.code, at }
            }
            Disposition::RetryAfter if elapsed < RETRY_AFTER_GAP => Verdict::Wait {
                code: e.code,
                at,
                remaining_secs: (RETRY_AFTER_GAP - elapsed).as_secs().max(1),
            },
            _ => Verdict::Proceed,
        }
    }
}

/// One key per work, however it was written (`doi:10.1/x`, `10.1/X`).
fn key(ref_input: &str) -> String {
    Ref::parse(ref_input).map_or_else(
        |_| ref_input.trim().to_string(),
        |r| r.safekey().as_str().to_string(),
    )
}

/// A hash of `config.toml`'s bytes, or 0 when there is none. Read per call:
/// it is the one input the HTTP client re-reads within a session. The read
/// is a [`crate::store::blocking_section`], as store reads are, since it
/// runs inside every fetch (#649 review).
#[must_use]
pub fn config_fingerprint() -> u64 {
    use std::hash::{Hash, Hasher};
    let bytes = crate::user_extension::config_path()
        .ok()
        .and_then(|p| crate::store::blocking_section(|| std::fs::read(p)).ok())
        .unwrap_or_default();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut h);
    h.finish()
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    const R: &str = "10.1137/0117004";

    #[test]
    fn entries_past_every_window_are_swept_when_a_new_one_lands() {
        let idx = RepeatIndex::default();
        let t0 = Instant::now();
        idx.observe_at(R, Some(ErrorCode::NotFound), t0, 7);
        idx.observe_at("10.1/b", Some(ErrorCode::RateLimited), t0, 7);
        let later = t0 + REPLAY_WINDOW + Duration::from_secs(1);
        idx.observe_at("10.1/c", Some(ErrorCode::NotFound), later, 7);
        assert_eq!(idx.lock().len(), 1, "only the new entry is left");
        assert!(matches!(
            idx.check_at("10.1/c", later, 7),
            Verdict::Replay { .. }
        ));
        assert_eq!(idx.check_at(R, later, 7), Verdict::Proceed);
    }

    #[test]
    fn a_terminal_answer_is_replayed_within_the_window_and_not_after() {
        let idx = RepeatIndex::default();
        let t0 = Instant::now();
        idx.observe_at(R, Some(ErrorCode::NotFound), t0, 7);
        assert!(matches!(
            idx.check_at(R, t0 + Duration::from_secs(5), 7),
            Verdict::Replay {
                code: ErrorCode::NotFound,
                ..
            }
        ));
        assert_eq!(idx.check_at(R, t0 + REPLAY_WINDOW, 7), Verdict::Proceed);
    }

    #[test]
    fn the_same_work_written_differently_is_one_key() {
        let idx = RepeatIndex::default();
        let t0 = Instant::now();
        idx.observe_at(
            "doi:10.1137/0117004",
            Some(ErrorCode::CapabilityDenied),
            t0,
            7,
        );
        assert!(matches!(idx.check_at(R, t0, 7), Verdict::Replay { .. }));
    }

    #[test]
    fn a_retry_after_answer_is_let_through_after_the_gap_and_timed_before_it() {
        let idx = RepeatIndex::default();
        let t0 = Instant::now();
        idx.observe_at(R, Some(ErrorCode::RateLimited), t0, 7);
        match idx.check_at(R, t0 + Duration::from_secs(10), 7) {
            Verdict::Wait { remaining_secs, .. } => assert_eq!(remaining_secs, 20),
            v => panic!("expected Wait, got {v:?}"),
        }
        assert_eq!(idx.check_at(R, t0 + RETRY_AFTER_GAP, 7), Verdict::Proceed);
    }

    #[test]
    fn a_changed_config_or_a_success_lifts_the_replay() {
        let idx = RepeatIndex::default();
        let t0 = Instant::now();
        idx.observe_at(R, Some(ErrorCode::CapabilityDenied), t0, 7);
        assert_eq!(
            idx.check_at(R, t0, 8),
            Verdict::Proceed,
            "config.toml changed"
        );
        idx.observe_at(R, None, t0, 7);
        assert_eq!(
            idx.check_at(R, t0, 7),
            Verdict::Proceed,
            "a later success clears it"
        );
    }

    #[test]
    fn a_replayed_wait_does_not_restart_the_clock_it_enforces() {
        let idx = RepeatIndex::default();
        let t0 = Instant::now();
        idx.observe_at(R, Some(ErrorCode::NetworkError), t0, 7);
        assert!(matches!(
            idx.check_at(R, t0 + Duration::from_secs(10), 7),
            Verdict::Wait { .. }
        ));
        // The replay reports RATE_LIMITED; its bookend must not reset `at`.
        idx.observe_at(
            R,
            Some(ErrorCode::RateLimited),
            t0 + Duration::from_secs(10),
            7,
        );
        assert_eq!(idx.check_at(R, t0 + RETRY_AFTER_GAP, 7), Verdict::Proceed);
    }

    #[test]
    fn a_looping_caller_does_not_slide_the_window() {
        let idx = RepeatIndex::default();
        let t0 = Instant::now();
        idx.observe_at(R, Some(ErrorCode::NotFound), t0, 7);
        // The replayed call's own bookend records the same code again.
        idx.observe_at(R, Some(ErrorCode::NotFound), t0 + REPLAY_WINDOW / 2, 7);
        assert_eq!(idx.check_at(R, t0 + REPLAY_WINDOW, 7), Verdict::Proceed);
    }

    #[test]
    fn a_different_answer_replaces_the_entry_and_a_terminal_code_is_updated() {
        let idx = RepeatIndex::default();
        let t0 = Instant::now();
        idx.observe_at(R, Some(ErrorCode::RateLimited), t0, 7);
        // A concurrent real request comes back NOT_FOUND: a new answer.
        idx.observe_at(R, Some(ErrorCode::NotFound), t0 + Duration::from_secs(1), 7);
        assert!(matches!(
            idx.check_at(R, t0 + Duration::from_secs(40), 7),
            Verdict::Replay {
                code: ErrorCode::NotFound,
                ..
            }
        ));
        // Another code of the same disposition keeps the clock but reports
        // the newer code.
        let other = ErrorCode::ALL
            .iter()
            .copied()
            .find(|c| {
                *c != ErrorCode::NotFound && c.disposition() == ErrorCode::NotFound.disposition()
            })
            .expect("a second terminal code");
        idx.observe_at(R, Some(other), t0 + Duration::from_secs(50), 7);
        match idx.check_at(R, t0 + Duration::from_secs(60), 7) {
            Verdict::Replay { code, .. } => assert_eq!(code, other),
            v => panic!("expected Replay, got {v:?}"),
        }
        assert_eq!(
            idx.check_at(R, t0 + Duration::from_secs(1) + REPLAY_WINDOW, 7),
            Verdict::Proceed,
            "the window runs from the first terminal answer"
        );
    }

    #[test]
    fn a_poisoned_lock_does_not_switch_suppression_off() {
        let idx = std::sync::Arc::new(RepeatIndex::default());
        let t0 = Instant::now();
        idx.observe_at(R, Some(ErrorCode::NotFound), t0, 7);
        let poisoner = std::sync::Arc::clone(&idx);
        let _ = std::thread::spawn(move || {
            let _guard = poisoner.entries.lock().unwrap();
            panic!("poison the lock");
        })
        .join();
        assert!(idx.entries.is_poisoned());
        assert!(matches!(idx.check_at(R, t0, 7), Verdict::Replay { .. }));
    }
}
