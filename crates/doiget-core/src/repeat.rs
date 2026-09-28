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
    /// Refs whose last request was answered by a replay. That request's own
    /// bookend is not a new answer -- a `Wait` replays as `RATE_LIMITED`,
    /// which would otherwise restart the clock it is enforcing.
    replayed: Mutex<std::collections::HashSet<String>>,
}

impl RepeatIndex {
    /// Record what a call told its caller about `ref_input`: an error code,
    /// or `None` for a clean success (which clears the ref).
    ///
    /// A repeat of the same code under the same configuration keeps the
    /// FIRST time: a caller looping on a refusal must not slide the window
    /// forward with every replay.
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
        if self
            .replayed
            .lock()
            .map(|mut r| r.remove(&key))
            .unwrap_or(false)
        {
            return;
        }
        let Ok(mut map) = self.entries.lock() else {
            return;
        };
        match code {
            None => {
                map.remove(&key);
            }
            Some(code) => {
                let keep = map
                    .get(&key)
                    .is_some_and(|e| e.code == code && e.fingerprint == fingerprint);
                if !keep {
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

    /// Note that the request for `ref_input` was answered by a replay, so
    /// its bookend does not count as a new answer.
    pub fn note_replayed(&self, ref_input: &str) {
        if let Ok(mut r) = self.replayed.lock() {
            r.insert(key(ref_input));
        }
    }

    /// What a request for `ref_input` should do now.
    #[must_use]
    pub fn check(&self, ref_input: &str) -> Verdict {
        self.check_at(ref_input, Instant::now(), config_fingerprint())
    }

    /// [`RepeatIndex::check`] with an explicit clock and fingerprint.
    #[must_use]
    pub fn check_at(&self, ref_input: &str, now: Instant, fingerprint: u64) -> Verdict {
        let Ok(map) = self.entries.lock() else {
            return Verdict::Proceed;
        };
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
/// it is the one input the HTTP client re-reads within a session.
#[must_use]
pub fn config_fingerprint() -> u64 {
    use std::hash::{Hash, Hasher};
    let bytes = crate::user_extension::config_path()
        .ok()
        .and_then(|p| std::fs::read(p).ok())
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
        idx.note_replayed(R);
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
}
