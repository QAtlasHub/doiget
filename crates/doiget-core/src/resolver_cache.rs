//! Resolver response cache (docs/CACHE.md §1–3).
//!
//! Caches the [`MetadataOnlyOutcome`] of a resolve at
//! `<cache_root>/resolver/<safekey>.toml` so a repeat resolve of the same
//! ref within the TTL ([`crate::RESOLVER_CACHE_TTL_DAYS`], 7 days) is
//! served from disk instead of hitting Crossref / arXiv. This is the
//! mechanism by which `doiget verify` avoids upstream rate limits: in CI
//! the directory is persisted across runs (e.g. `actions/cache`), so an
//! unchanged bibliography resolves with zero network calls.
//!
//! The on-disk entry follows CACHE.md §2: a TOML file with
//! `schema_version` / `fetched_at` / `ttl_seconds` / `source`, plus the
//! resolver outcome stored as a JSON string under `response` (the
//! `MetadataOnlyOutcome.metadata` field is arbitrary JSON that does not
//! round-trip cleanly through TOML, so it is kept as a JSON blob).
//!
//! All operations are best-effort: a read miss, a stale entry, a parse
//! error, or a write failure degrade to "no cache" rather than failing
//! the resolve. The cache is a latency/politeness optimisation, never a
//! correctness dependency.

use camino::{Utf8Path, Utf8PathBuf};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::orchestrator::{MetadataOnlyOptions, MetadataOnlyOutcome};
use crate::{Ref, RESOLVER_CACHE_TTL_DAYS};

/// Current cache-entry schema version (CACHE.md §2).
const CACHE_SCHEMA_VERSION: &str = "1.0";

/// On-disk cache entry (CACHE.md §2). `response` holds the
/// `MetadataOnlyOutcome` serialized as a JSON string.
#[derive(Debug, Serialize, Deserialize)]
struct CacheEntry {
    schema_version: String,
    /// RFC 3339 UTC timestamp of the resolve that produced this entry.
    fetched_at: String,
    ttl_seconds: i64,
    source: String,
    /// `serde_json::to_string(&MetadataOnlyOutcome)`.
    response: String,
}

/// The on-disk path for a ref's cache entry:
/// `<cache_root>/resolver/<safekey>.toml`.
#[must_use]
pub fn cache_file(cache_root: &Utf8Path, ref_: &Ref) -> Utf8PathBuf {
    cache_file_with_options(cache_root, ref_, MetadataOnlyOptions::default())
}

/// [`cache_file`], keyed by the options as well as the ref.
///
/// A default resolve and an `include_oa_location` resolve ask different
/// questions of the network and get different answers, so they cannot share
/// an entry. Serving a default entry to an opt-in caller would answer with
/// `oa_url: None` -- indistinguishable from "Unpaywall was asked and this
/// work has no OA location", which is the one thing the caller paid a
/// request to find out.
///
/// The other direction is just as wrong: reading the opt-in entry and
/// re-fetching whenever `oa_url` is `None` would re-fetch forever for
/// exactly the closed-access works one asks about repeatedly.
///
/// So: two entries, separated by a SUBDIRECTORY rather than a filename
/// suffix. A `<safekey>.oa.toml` suffix would collide -- [`Ref::safekey`]
/// keeps `.` (it is in the allowed set), so the DOI `10.1234/foo.oa`
/// resolved by default and the DOI `10.1234/foo` resolved with the flag
/// would both want `doi_10.1234_foo.oa.toml`. A safekey can never contain a
/// path separator (`/` is replaced with `_`), so a subdirectory cannot.
#[must_use]
pub fn cache_file_with_options(
    cache_root: &Utf8Path,
    ref_: &Ref,
    opts: MetadataOnlyOptions,
) -> Utf8PathBuf {
    let dir = cache_root.join("resolver");
    let dir = if opts.include_oa_location {
        dir.join("oa")
    } else {
        dir
    };
    dir.join(format!("{}.toml", ref_.safekey().as_str()))
}

/// Read a cached outcome for `ref_` if present and still within its TTL.
///
/// Returns `None` on any miss condition: file absent, unparsable,
/// expired, or a `response` blob that no longer deserializes. `now` is
/// injected so tests can pin expiry without touching the clock.
#[must_use]
pub fn read_at(
    cache_root: &Utf8Path,
    ref_: &Ref,
    now: DateTime<Utc>,
) -> Option<MetadataOnlyOutcome> {
    read_at_with_options(cache_root, ref_, now, MetadataOnlyOptions::default())
}

/// [`read_at`], reading the entry keyed by `opts`. See
/// [`cache_file_with_options`] for why the options are part of the key.
#[must_use]
pub fn read_at_with_options(
    cache_root: &Utf8Path,
    ref_: &Ref,
    now: DateTime<Utc>,
    opts: MetadataOnlyOptions,
) -> Option<MetadataOnlyOutcome> {
    let path = cache_file_with_options(cache_root, ref_, opts);
    let text = std::fs::read_to_string(&path).ok()?;
    let entry: CacheEntry = toml::from_str(&text).ok()?;
    let fetched: DateTime<Utc> = DateTime::parse_from_rfc3339(&entry.fetched_at)
        .ok()?
        .with_timezone(&Utc);
    if now > fetched + Duration::seconds(entry.ttl_seconds) {
        // Stale: treat as a miss; the caller will re-fetch and overwrite.
        return None;
    }
    serde_json::from_str(&entry.response).ok()
}

/// Read using the current wall clock. See [`read_at`].
#[must_use]
pub fn read(cache_root: &Utf8Path, ref_: &Ref) -> Option<MetadataOnlyOutcome> {
    read_at(cache_root, ref_, Utc::now())
}

/// [`read`], reading the entry keyed by `opts`.
#[must_use]
pub fn read_with_options(
    cache_root: &Utf8Path,
    ref_: &Ref,
    opts: MetadataOnlyOptions,
) -> Option<MetadataOnlyOutcome> {
    read_at_with_options(cache_root, ref_, Utc::now(), opts)
}

/// Write `outcome` to the cache for `ref_`. Best-effort: returns `false`
/// (after a `tracing::debug!`) on any I/O or serialization failure rather
/// than propagating, since a cache write must never fail a resolve.
pub fn write_at(
    cache_root: &Utf8Path,
    ref_: &Ref,
    outcome: &MetadataOnlyOutcome,
    now: DateTime<Utc>,
) -> bool {
    write_at_with_options(
        cache_root,
        ref_,
        outcome,
        now,
        MetadataOnlyOptions::default(),
    )
}

/// [`write_at`], writing the entry keyed by `opts`.
pub fn write_at_with_options(
    cache_root: &Utf8Path,
    ref_: &Ref,
    outcome: &MetadataOnlyOutcome,
    now: DateTime<Utc>,
    opts: MetadataOnlyOptions,
) -> bool {
    let response = match serde_json::to_string(outcome) {
        Ok(s) => s,
        Err(e) => {
            tracing::debug!(error = %e, "resolver cache: serialize failed; skipping write");
            return false;
        }
    };
    let entry = CacheEntry {
        schema_version: CACHE_SCHEMA_VERSION.to_string(),
        fetched_at: now.to_rfc3339(),
        ttl_seconds: i64::from(RESOLVER_CACHE_TTL_DAYS) * 86_400,
        source: outcome.source.clone(),
        response,
    };
    let toml_text = match toml::to_string(&entry) {
        Ok(t) => t,
        Err(e) => {
            tracing::debug!(error = %e, "resolver cache: toml encode failed; skipping write");
            return false;
        }
    };
    let path = cache_file_with_options(cache_root, ref_, opts);
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            tracing::debug!(error = %e, dir = %parent, "resolver cache: mkdir failed; skipping write");
            return false;
        }
    }
    if let Err(e) = std::fs::write(&path, toml_text) {
        tracing::debug!(error = %e, path = %path, "resolver cache: write failed");
        return false;
    }
    true
}

/// Write using the current wall clock. See [`write_at`].
pub fn write(cache_root: &Utf8Path, ref_: &Ref, outcome: &MetadataOnlyOutcome) -> bool {
    write_at(cache_root, ref_, outcome, Utc::now())
}

/// [`write()`], writing the entry keyed by `opts`.
pub fn write_with_options(
    cache_root: &Utf8Path,
    ref_: &Ref,
    outcome: &MetadataOnlyOutcome,
    opts: MetadataOnlyOptions,
) -> bool {
    write_at_with_options(cache_root, ref_, outcome, Utc::now(), opts)
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use serde_json::json;

    fn outcome() -> MetadataOnlyOutcome {
        MetadataOnlyOutcome {
            source: "crossref".to_string(),
            resolver_profile: "crossref".to_string(),
            license: Some("cc-by".to_string()),
            oa_url: None,
            oa_status: Some("gold".to_string()),
            metadata: json!({"title": ["Example"], "DOI": "10.1234/x"}),
        }
    }

    #[test]
    fn write_then_read_round_trips() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = Utf8Path::from_path(dir.path()).unwrap();
        let r = Ref::parse("10.1234/x").unwrap();
        let now = Utc::now();
        assert!(write_at(root, &r, &outcome(), now));
        let got = read_at(root, &r, now).expect("cache hit");
        assert_eq!(got.source, "crossref");
        assert_eq!(got.metadata["DOI"], "10.1234/x");
    }

    /// #539: the options are part of the key, not just the request.
    ///
    /// A default resolve caches `oa_url: None` because it never asked. Serving
    /// that entry to a caller who DID ask would answer the one question it
    /// paid a round-trip for, with a value that means something else.
    #[test]
    fn an_opt_in_read_does_not_hit_the_default_entry() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = Utf8Path::from_path(dir.path()).unwrap();
        let r = Ref::parse("10.1234/x").unwrap();
        let now = Utc::now();
        let with_oa = MetadataOnlyOptions::default().with_oa_location(true);

        assert!(write_at(root, &r, &outcome(), now));
        assert!(
            read_at_with_options(root, &r, now, with_oa).is_none(),
            "the default entry must not satisfy an opt-in read"
        );
        // ... and the converse, so a warm opt-in cache does not start
        // answering default calls with a field they did not ask for.
        let dir2 = tempfile::TempDir::new().unwrap();
        let root2 = Utf8Path::from_path(dir2.path()).unwrap();
        assert!(write_at_with_options(root2, &r, &outcome(), now, with_oa));
        assert!(read_at(root2, &r, now).is_none());
        assert!(read_at_with_options(root2, &r, now, with_oa).is_some());
    }

    /// The first version of this used a `<safekey>.oa.toml` SUFFIX, which
    /// collides: [`Ref::safekey`] keeps `.` (it is in the allowed character
    /// set), so the DOI `10.1234/foo.oa` resolved by default and the DOI
    /// `10.1234/foo` resolved with the flag both wanted
    /// `doi_10.1234_foo.oa.toml` -- one silently serving the other's answer.
    /// A subdirectory cannot collide, because a safekey can never contain a
    /// path separator.
    #[test]
    fn a_dot_oa_doi_cannot_collide_with_an_opt_in_entry() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = Utf8Path::from_path(dir.path()).unwrap();
        let plain = Ref::parse("10.1234/foo").unwrap();
        let dotted = Ref::parse("10.1234/foo.oa").unwrap();

        // Guard the premise: if safekey ever starts escaping `.`, this test
        // is no longer testing what it says it is.
        assert!(
            dotted.safekey().as_str().ends_with(".oa"),
            "premise: safekey keeps '.', so a '.oa' suffix is reachable"
        );

        assert_ne!(
            cache_file_with_options(root, &dotted, MetadataOnlyOptions::default()),
            cache_file_with_options(
                root,
                &plain,
                MetadataOnlyOptions::default().with_oa_location(true)
            ),
        );
    }

    #[test]
    fn miss_when_absent() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = Utf8Path::from_path(dir.path()).unwrap();
        let r = Ref::parse("10.1234/absent").unwrap();
        assert!(read_at(root, &r, Utc::now()).is_none());
    }

    #[test]
    fn miss_when_expired() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = Utf8Path::from_path(dir.path()).unwrap();
        let r = Ref::parse("10.1234/x").unwrap();
        let written = Utc::now();
        assert!(write_at(root, &r, &outcome(), written));
        // 8 days later — past the 7-day TTL.
        let later = written + Duration::days(8);
        assert!(read_at(root, &r, later).is_none());
    }

    #[test]
    fn fresh_within_ttl() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = Utf8Path::from_path(dir.path()).unwrap();
        let r = Ref::parse("10.1234/x").unwrap();
        let written = Utc::now();
        assert!(write_at(root, &r, &outcome(), written));
        // 6 days later — still within the 7-day TTL.
        let later = written + Duration::days(6);
        assert!(read_at(root, &r, later).is_some());
    }

    #[test]
    fn cache_file_path_uses_safekey() {
        let root = Utf8Path::new("/tmp/cache");
        let r = Ref::parse("10.1234/x").unwrap();
        let p = cache_file(root, &r);
        // Use components, not a substring, so the assertion is independent
        // of the platform path separator (`/` vs `\`).
        assert!(p.components().any(|c| c.as_str() == "resolver"));
        assert!(p.as_str().ends_with(".toml"));
    }
}
