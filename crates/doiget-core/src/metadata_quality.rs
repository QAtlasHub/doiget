//! U+FFFD in resolver metadata: detect it, and repair it only from a source
//! the user enabled (#608).
//!
//! Some Crossref records carry U+FFFD REPLACEMENT CHARACTER where the
//! publisher's deposit lost its umlauts (`Zeitschrift f\u{FFFD}r Physik`,
//! `N\u{FFFD}herungsmethode`). The entry still compiles, so the damage shows up
//! only in the rendered bibliography. OpenAlex ingests Crossref and carries
//! the same loss; Semantic Scholar, measured 2026-09-29 on the two DOIs in
//! the issue, does not.
//!
//! **Repair is guarded.** A candidate replaces a damaged value only when it
//! matches it with each U+FFFD standing for one or two characters and every
//! other character equal. So `Zeitschrift f\u{FFFD}r Physik` accepts
//! `Zeitschrift für Physik` and refuses OpenAlex's `The European Physical
//! Journal A` (the journal's later name), which leaves the field flagged
//! rather than swapped for a different fact.
//!
//! **No new network by default.** Only sources already enabled for this run
//! (`DOIGET_ENABLE_S2`, `DOIGET_ENABLE_OPENALEX`) are asked, through the
//! same rate limiter, allowlists and provenance log as any other call.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::source::{FetchContext, Source};
use crate::store::Metadata;
use crate::{CapabilityProfile, Ref};

/// U+FFFD REPLACEMENT CHARACTER.
pub const REPLACEMENT_CHAR: char = '\u{FFFD}';

/// Metadata fields a U+FFFD is looked for in, by their `docs/STORE.md` name.
const CHECKED_FIELDS: &[&str] = &["title", "authors", "venue", "publisher", "abstract"];

/// The fields of `m` that carry a U+FFFD: title, authors, venue,
/// publisher, abstract, in that order.
#[must_use]
pub fn replacement_char_fields(m: &Metadata) -> Vec<&'static str> {
    CHECKED_FIELDS
        .iter()
        .copied()
        .filter(|f| match *f {
            "title" => has_replacement_char(&m.title),
            "authors" => m.authors.iter().any(|a| has_replacement_char(a)),
            "venue" => m.venue.as_deref().is_some_and(has_replacement_char),
            "publisher" => m.publisher.as_deref().is_some_and(has_replacement_char),
            "abstract" => m.abstract_.as_deref().is_some_and(has_replacement_char),
            _ => false,
        })
        .collect()
}

/// Whether `s` carries a U+FFFD.
#[must_use]
pub fn has_replacement_char(s: &str) -> bool {
    s.contains(REPLACEMENT_CHAR)
}

/// Whether `candidate` is `damaged` with its U+FFFD characters restored:
/// each U+FFFD in `damaged` stands for one or two characters of `candidate`
/// (a lost UTF-8 sequence may have been one or two code points), and every
/// other character is equal. A candidate that itself carries a U+FFFD never
/// matches.
///
/// Both strings come from remote answers, so the match is bounded: longer
/// than [`RESTORE_MAX_CHARS`] never matches, and a candidate whose length
/// the U+FFFDs cannot account for is refused before any alignment.
#[must_use]
pub fn restores(damaged: &str, candidate: &str) -> bool {
    if has_replacement_char(candidate) || !has_replacement_char(damaged) {
        return false;
    }
    let c: Vec<char> = candidate.chars().collect();
    let d_len = damaged.chars().count();
    let lost = damaged.chars().filter(|&ch| ch == REPLACEMENT_CHAR).count();
    if d_len > RESTORE_MAX_CHARS || c.len() > RESTORE_MAX_CHARS {
        return false;
    }
    // Each U+FFFD stands for one or two characters.
    if c.len() < d_len || c.len() > d_len + lost {
        return false;
    }
    // reach[j]: candidate[..j] is matched by the prefix of `damaged` read so far.
    let mut reach = vec![false; c.len() + 1];
    let mut next = vec![false; c.len() + 1];
    reach[0] = true;
    for dc in damaged.chars() {
        next.fill(false);
        for j in (0..=c.len()).filter(|&j| reach[j]) {
            if dc == REPLACEMENT_CHAR {
                for step in 1..=2 {
                    if j + step <= c.len() {
                        next[j + step] = true;
                    }
                }
            } else if c.get(j) == Some(&dc) {
                next[j + 1] = true;
            }
        }
        if !next.contains(&true) {
            return false;
        }
        std::mem::swap(&mut reach, &mut next);
    }
    reach[c.len()]
}

/// The longest title or venue [`restores`] will align (#649 review): far
/// past any real one, short enough that a runaway answer costs nothing.
pub const RESTORE_MAX_CHARS: usize = 2_000;

/// What [`repair_with`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QualityReport {
    /// Field → key of the source whose value replaced the damaged one.
    pub repaired: BTreeMap<String, String>,
    /// Fields still carrying a U+FFFD after repair.
    pub remaining: Vec<&'static str>,
}

impl QualityReport {
    /// The machine-readable flags for [`QualityReport::remaining`], e.g.
    /// `replacement_char:venue`: the `metadata_quality` list of the fetch
    /// envelopes.
    #[must_use]
    pub fn flags(&self) -> Vec<String> {
        self.remaining
            .iter()
            .map(|f| format!("replacement_char:{f}"))
            .collect()
    }
}

/// Repair the U+FFFD fields of `m` that a source can supply (`title` and
/// `venue`), asking each of `sources` that can serve the DOI at most once,
/// and record each repair in `[doiget].repaired_fields`.
///
/// Authors, publisher and abstract are detected but never repaired: no
/// enabled source returns them in a shape that can be compared character by
/// character with Crossref's (`Family, Given`). A source that fails is
/// skipped with a warning; repair is best-effort and never fails the call.
pub async fn repair_with(
    m: &mut Metadata,
    sources: &[&dyn Source],
    profile: &CapabilityProfile,
    ctx: &FetchContext,
) -> QualityReport {
    let mut report = QualityReport::default();
    let repairable: Vec<&'static str> = replacement_char_fields(m)
        .into_iter()
        .filter(|f| matches!(*f, "title" | "venue"))
        .collect();
    if let (false, Some(doi)) = (repairable.is_empty(), m.doi.clone()) {
        let ref_ = Ref::Doi(doi);
        for src in sources {
            if repairable.iter().all(|f| report.repaired.contains_key(*f)) {
                break;
            }
            if !src.can_serve(profile, &ref_) {
                continue;
            }
            let work = match src.fetch(&ref_, profile, ctx).await {
                Ok(r) => r.metadata_json,
                Err(e) => {
                    tracing::warn!(
                        source = src.name(),
                        error = %e,
                        "could not ask this source to repair a U+FFFD in the metadata"
                    );
                    continue;
                }
            };
            let Some(work) = work else { continue };
            for field in &repairable {
                if report.repaired.contains_key(*field) {
                    continue;
                }
                let Some(found) = candidate(src.name(), field, &work) else {
                    continue;
                };
                let slot = match *field {
                    "title" => &mut m.title,
                    _ => match m.venue.as_mut() {
                        Some(v) => v,
                        None => continue,
                    },
                };
                if restores(slot, &found) {
                    *slot = found;
                    report
                        .repaired
                        .insert((*field).to_string(), src.name().to_string());
                }
            }
        }
    }
    report.remaining = replacement_char_fields(m);
    if let Some(d) = m.doiget.as_mut() {
        d.repaired_fields.extend(report.repaired.clone());
    }
    report
}

/// Repair `m` from the production Semantic Scholar and OpenAlex sources,
/// each asked only if enabled in `profile`. See [`repair_with`].
///
/// Both sources are compiled in only with the `metadata` feature; without
/// it this detects and reports, and repairs nothing.
pub async fn repair(
    m: &mut Metadata,
    profile: &CapabilityProfile,
    ctx: &FetchContext,
) -> QualityReport {
    if replacement_char_fields(m).is_empty() {
        return QualityReport::default();
    }
    #[cfg(not(feature = "metadata"))]
    {
        let _ = (profile, ctx);
        QualityReport {
            remaining: replacement_char_fields(m),
            ..QualityReport::default()
        }
    }
    #[cfg(feature = "metadata")]
    {
        let s2 = crate::sources::s2::S2Source::new(
            std::env::var("DOIGET_S2_API_KEY")
                .ok()
                .filter(|k| !k.is_empty()),
        );
        let openalex = crate::sources::openalex::OpenalexSource::new(
            crate::orchestrator::resolve_contact_email(),
        );
        repair_with(m, &[&s2, &openalex], profile, ctx).await
    }
}

/// The value `source` reports for `field` in its work record.
fn candidate(source: &str, field: &str, work: &Value) -> Option<String> {
    let s = match (source, field) {
        ("semantic_scholar", "title") => work.get("title")?.as_str()?,
        ("semantic_scholar", "venue") => work.get("venue")?.as_str()?,
        ("openalex", "title") => work
            .get("title")
            .or_else(|| work.get("display_name"))?
            .as_str()?,
        ("openalex", "venue") => work
            .get("primary_location")?
            .get("source")?
            .get("display_name")?
            .as_str()?,
        _ => return None,
    };
    let s = s.trim();
    (!s.is_empty()).then(|| s.to_string())
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    const F: char = REPLACEMENT_CHAR;

    #[test]
    fn a_candidate_restores_only_the_characters_that_were_lost() {
        let z = format!("Zeitschrift f{F}r Physik");
        assert!(restores(&z, "Zeitschrift für Physik"));
        // The journal's later name is a different fact, not a repair.
        assert!(!restores(&z, "The European Physical Journal A"));
        // Other characters must be equal, not merely similar.
        assert!(!restores(&z, "Zeitschrift für physik"));
        let n = format!("N{F}herungsmethode zur L{F}sung");
        assert!(restores(&n, "Näherungsmethode zur Lösung"));
        // One U+FFFD may stand for two code points (a decomposed umlaut).
        assert!(restores(&n, "Na\u{308}herungsmethode zur Lösung"));
        // But never for three, nor for none.
        assert!(!restores(&n, "Nxyzherungsmethode zur Lösung"));
        assert!(!restores(&n, "Nherungsmethode zur Lösung"));
    }

    #[test]
    fn an_oversized_answer_is_never_aligned() {
        let long = format!("{}{F}", "a".repeat(RESTORE_MAX_CHARS));
        assert!(!restores(
            &long,
            &format!("{}b", "a".repeat(RESTORE_MAX_CHARS))
        ));
        let short = format!("x{F}y");
        assert!(restores(&short, "xüy"));
        assert!(!restores(&short, &"x".repeat(50_000)));
    }

    #[test]
    fn a_candidate_carrying_the_same_loss_is_not_a_repair() {
        let z = format!("Zeitschrift f{F}r Physik");
        assert!(!restores(&z, &z));
        assert!(!restores(
            "Zeitschrift für Physik",
            "Zeitschrift für Physik"
        ));
    }

    #[test]
    fn every_checked_field_is_detected() {
        let m = Metadata {
            title: format!("Wechselwirkung neutraler Atome und {F} Bindung"),
            authors: vec!["London, F.".into(), format!("M{F}ller, A.")],
            venue: Some(format!("Zeitschrift f{F}r Physik")),
            publisher: Some("Springer".into()),
            ..Metadata::default()
        };
        assert_eq!(
            replacement_char_fields(&m),
            vec!["title", "authors", "venue"]
        );
        let report = QualityReport {
            remaining: vec!["venue"],
            ..QualityReport::default()
        };
        assert_eq!(report.flags(), vec!["replacement_char:venue".to_string()]);
    }

    /// End to end through a real `Source`: the damaged title is replaced by
    /// S2's matching one and recorded; the venue S2 does not carry stays
    /// flagged; a disabled source is never asked.
    #[cfg(feature = "metadata")]
    #[tokio::test]
    async fn repair_takes_a_matching_title_records_it_and_leaves_the_rest_flagged() {
        use std::sync::Arc;

        use camino::Utf8PathBuf;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        use crate::http::HttpClient;
        use crate::provenance::ProvenanceLog;
        use crate::rate_limiter::RateLimiter;
        use crate::sources::s2::S2Source;
        use crate::store::DoigetExtension;
        use crate::{Doi, RateLimits};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/graph/v1/paper/DOI:10.1007/BF01340294"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{"paperId": "bf0e", "title": "Näherungsmethode zur Lösung", "venue": ""}"#,
            ))
            .expect(1)
            .mount(&server)
            .await;
        let td = tempfile::TempDir::new().expect("tempdir");
        let log_path = Utf8PathBuf::try_from(td.path().join("t.jsonl")).expect("utf-8");
        let session_id = "01J0000000000000000000TEST".to_string();
        let ctx = FetchContext {
            http: Arc::new(HttpClient::new_for_tests_allow_http(
                "semantic_scholar",
                &server.uri(),
            )),
            rate_limiter: Arc::new(RateLimiter::new(RateLimits::HARD_CODED)),
            log: Arc::new(ProvenanceLog::open(log_path, session_id.clone()).expect("log")),
            session_id,
            cache_root: None,
        };
        let s2 = S2Source::with_base(url::Url::parse(&server.uri()).expect("uri"), None);

        let mut m = Metadata {
            title: format!("N{F}herungsmethode zur L{F}sung"),
            venue: Some(format!("Zeitschrift f{F}r Physik")),
            doi: Some(Doi::parse("10.1007/BF01340294").expect("doi")),
            doiget: Some(DoigetExtension {
                fetched_at: chrono::Utc::now(),
                source: "crossref".into(),
                license: "unknown".into(),
                oa_status: None,
                size_bytes: 0,
                mcp_call_id: None,
                tags: Vec::new(),
                collections: Vec::new(),
                annotation: None,
                repaired_fields: BTreeMap::new(),
                short_venue: None,
                origin: None,
            }),
            ..Metadata::default()
        };

        // S2 disabled: nothing is asked (`expect(1)` below counts the call).
        let off = CapabilityProfile::for_tests();
        let report = repair_with(&mut m.clone(), &[&s2], &off, &ctx).await;
        assert!(report.repaired.is_empty());
        assert_eq!(report.remaining, vec!["title", "venue"]);

        let mut on = CapabilityProfile::for_tests();
        on.metadata.semantic_scholar = true;
        let report = repair_with(&mut m, &[&s2], &on, &ctx).await;
        assert_eq!(m.title, "Näherungsmethode zur Lösung");
        assert_eq!(
            m.venue.as_deref(),
            Some(format!("Zeitschrift f{F}r Physik").as_str())
        );
        assert_eq!(
            report.repaired.get("title").map(String::as_str),
            Some("semantic_scholar")
        );
        assert_eq!(report.remaining, vec!["venue"]);
        assert_eq!(
            m.doiget.as_ref().map(|d| d.repaired_fields.clone()),
            Some(report.repaired.clone())
        );
    }

    #[test]
    fn candidates_are_read_from_each_source_shape() {
        let s2 = serde_json::json!({"paperId": "x", "title": " Näherungsmethode ", "venue": ""});
        assert_eq!(
            candidate("semantic_scholar", "title", &s2).as_deref(),
            Some("Näherungsmethode")
        );
        assert_eq!(candidate("semantic_scholar", "venue", &s2), None);
        let oa = serde_json::json!({
            "display_name": "Näherungsmethode",
            "primary_location": {"source": {"display_name": "Zeitschrift für Physik"}}
        });
        assert_eq!(
            candidate("openalex", "title", &oa).as_deref(),
            Some("Näherungsmethode")
        );
        assert_eq!(
            candidate("openalex", "venue", &oa).as_deref(),
            Some("Zeitschrift für Physik")
        );
    }
}
