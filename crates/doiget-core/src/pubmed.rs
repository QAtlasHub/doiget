//! PubMed identifiers (#500, ADR-0061): a PMID or PMCID is resolved to the
//! DOI PubMed lists for it, and the paper is then fetched, cited and stored
//! under that DOI like any other.
//!
//! A PubMed-identified entry used to be reported and left: "identified only
//! by PMID, which doiget cannot resolve yet". It is now an alias. The store
//! identity stays the DOI, so no safekey class, no `Ref` variant and no
//! store layout change: the id is looked up once, through NCBI E-utilities
//! (`esummary.fcgi`, `db=pubmed` for a PMID, `db=pmc` for a PMCID), and the
//! `articleids[]` entry with `idtype == "doi"` is the answer. A record with
//! no DOI there is reported as such, never guessed from.
//!
//! NCBI's usage guidance (NBK25497): at most 3 requests a second without an
//! API key, which is tighter than doiget's global cap and therefore a
//! [`crate::SOURCE_RATE_OVERRIDES`] entry; `tool` and `email` identify the
//! caller and are sent (the email only when one is configured). doiget sends
//! no API key, so the 10-a-second keyed rate is never assumed.

use serde_json::Value;
use url::Url;

use crate::provenance::{Capability, LogEvent, LogResult, RowInput};
use crate::refs::{ParseError, ParsedEntry};
use crate::source::{FetchContext, FetchError};
use crate::{Doi, Ref};

/// HTTP and rate-limiter source key for NCBI E-utilities.
pub const NCBI: &str = "ncbi";
/// Base-URL override for [`NCBI`].
pub const NCBI_BASE_ENV: &str = "DOIGET_NCBI_BASE";
const NCBI_DEFAULT: &str = "https://eutils.ncbi.nlm.nih.gov/entrez/eutils/";

/// A PubMed identifier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PubmedId {
    /// A PubMed id, digits only.
    Pmid(String),
    /// A PubMed Central id, digits only (the `PMC` prefix removed).
    Pmcid(String),
}

impl PubmedId {
    /// Parse `pmid:9659853`, `PMID: 9659853`, `pmcid:PMC3531190`,
    /// `PMC3531190`, or a `pubmed.ncbi.nlm.nih.gov/<pmid>` /
    /// `…/pmc/articles/PMC<id>` URL. Bare digits are not a PMID: they are too
    /// easily something else.
    #[must_use]
    pub fn parse(input: &str) -> Option<Self> {
        let s = input.trim();
        let lower = s.to_ascii_lowercase();
        if let Some(rest) = lower.strip_prefix("pmid:") {
            return digits(rest.trim()).map(Self::Pmid);
        }
        if let Some(rest) = lower.strip_prefix("pmcid:") {
            let rest = rest.trim();
            return digits(rest.strip_prefix("pmc").unwrap_or(rest)).map(Self::Pmcid);
        }
        if let Some(rest) = lower.strip_prefix("pmc") {
            return digits(rest).map(Self::Pmcid);
        }
        let url = Url::parse(s).ok()?;
        let segs: Vec<&str> = url.path_segments()?.filter(|p| !p.is_empty()).collect();
        match (url.host_str()?, segs.as_slice()) {
            ("pubmed.ncbi.nlm.nih.gov", [id]) => digits(id).map(Self::Pmid),
            ("www.ncbi.nlm.nih.gov" | "ncbi.nlm.nih.gov", ["pmc", "articles", id]) => {
                let id = id.to_ascii_lowercase();
                digits(id.strip_prefix("pmc")?).map(Self::Pmcid)
            }
            ("pmc.ncbi.nlm.nih.gov", ["articles", id]) => {
                let id = id.to_ascii_lowercase();
                digits(id.strip_prefix("pmc")?).map(Self::Pmcid)
            }
            _ => None,
        }
    }

    /// From a bibliography's `UnsupportedIdentifier { kind, value }`.
    #[must_use]
    pub fn from_kind(kind: &str, value: &str) -> Option<Self> {
        match kind {
            "PMID" => digits(value.trim()).map(Self::Pmid),
            "PMCID" => {
                let v = value.trim().to_ascii_lowercase();
                digits(v.strip_prefix("pmc").unwrap_or(&v)).map(Self::Pmcid)
            }
            _ => None,
        }
    }

    /// `PMID 9659853` / `PMCID PMC3531190`.
    #[must_use]
    pub fn display(&self) -> String {
        match self {
            Self::Pmid(id) => format!("PMID {id}"),
            Self::Pmcid(id) => format!("PMCID PMC{id}"),
        }
    }

    fn db_and_id(&self) -> (&'static str, &str) {
        match self {
            Self::Pmid(id) => ("pubmed", id),
            Self::Pmcid(id) => ("pmc", id),
        }
    }
}

fn digits(s: &str) -> Option<String> {
    (!s.is_empty() && s.len() <= 12 && s.chars().all(|c| c.is_ascii_digit())).then(|| s.to_string())
}

/// What PubMed says about an id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lookup {
    /// The record lists this DOI.
    Doi(Doi),
    /// The record exists and lists no DOI.
    NoDoi,
    /// PubMed has no record under this id.
    NoRecord,
}

impl Lookup {
    /// A sentence for the user when there is no DOI to go on.
    #[must_use]
    pub fn reason(&self, id: &PubmedId) -> Option<String> {
        match self {
            Self::Doi(_) => None,
            Self::NoDoi => Some(format!(
                "PubMed's record for {} lists no DOI, and doiget reaches PubMed records through their DOI",
                id.display()
            )),
            Self::NoRecord => Some(format!("PubMed has no record for {}", id.display())),
        }
    }
}

/// Ask NCBI E-utilities for the DOI of `id`.
///
/// # Errors
///
/// [`FetchError`] for a transport failure or an answer that is not
/// E-utilities JSON; a record without a DOI, or no record, is a [`Lookup`].
pub async fn lookup(id: &PubmedId, ctx: &FetchContext) -> Result<Lookup, FetchError> {
    let (db, uid) = id.db_and_id();
    let raw_base = std::env::var(NCBI_BASE_ENV).unwrap_or_else(|_| NCBI_DEFAULT.to_string());
    let mut base = Url::parse(&raw_base).map_err(|e| FetchError::SourceSchema {
        hint: format!("{NCBI_BASE_ENV}={raw_base:?} is not a URL: {e}"),
    })?;
    if !base.path().ends_with('/') {
        base.set_path(&format!("{}/", base.path()));
    }
    let mut url = base
        .join("esummary.fcgi")
        .map_err(|e| FetchError::SourceSchema {
            hint: format!("building the E-utilities URL: {e}"),
        })?;
    {
        let mut q = url.query_pairs_mut();
        q.append_pair("db", db)
            .append_pair("id", uid)
            .append_pair("retmode", "json")
            .append_pair("tool", "doiget");
        if let Some(email) = crate::orchestrator::configured_contact_email() {
            q.append_pair("email", &email);
        }
    }

    let _permit = ctx.rate_limiter.acquire(NCBI).await;
    let shown = id.display();
    let row = |result, size, error_code| RowInput {
        event: LogEvent::Resolve,
        result,
        capability: Capability::Metadata,
        ref_: Some(shown.as_str()),
        source: Some(NCBI),
        error_code,
        size_bytes: size,
        license: None,
        store_path: None,
        canonical_digest: None,
    };
    let body = match ctx.http.fetch_bytes(NCBI, url).await {
        Ok((body, _)) => body,
        Err(e) => {
            let e = FetchError::Http(e);
            let code = crate::ErrorCode::from(&e);
            ctx.log
                .append(row(LogResult::Err, None, Some(code.as_wire())))?;
            return Err(e);
        }
    };
    let v: Value = serde_json::from_slice(&body).map_err(|e| FetchError::SourceSchema {
        hint: format!("E-utilities returned non-JSON for {shown}: {e}"),
    })?;
    let found = classify(&v, uid);
    ctx.log.append(row(
        LogResult::Ok,
        Some(body.len() as u64),
        matches!(found, Lookup::NoRecord).then_some("NOT_FOUND"),
    ))?;
    Ok(found)
}

/// Read an ESummary JSON answer for `uid`.
fn classify(v: &Value, uid: &str) -> Lookup {
    let Some(rec) = v.pointer("/result").and_then(|r| r.get(uid)) else {
        return Lookup::NoRecord;
    };
    if rec.get("error").is_some() {
        return Lookup::NoRecord;
    }
    rec.get("articleids")
        .and_then(Value::as_array)
        .and_then(|ids| {
            ids.iter()
                .find(|i| i.get("idtype").and_then(Value::as_str) == Some("doi"))
        })
        .and_then(|i| i.get("value").and_then(Value::as_str))
        .and_then(|d| Doi::parse(d).ok())
        .map_or(Lookup::NoDoi, Lookup::Doi)
}

/// A bibliography entry PubMed could not turn into a DOI.
#[derive(Debug, Clone)]
pub struct Unresolved {
    /// The id as the entry named it.
    pub id: PubmedId,
    /// The citation key.
    pub entry_key: Option<String>,
    /// Why, for the user.
    pub reason: String,
    /// The code a surface reports it under: `NOT_FOUND` when PubMed has no
    /// record, `NOT_IMPLEMENTED` when the record has no DOI, or the
    /// transport error's.
    pub code: crate::ErrorCode,
}

/// One bibliography entry after PubMed resolution.
#[derive(Debug)]
pub enum Resolved {
    /// As parsed, or a PubMed id turned into its DOI.
    Entry(Result<ParsedEntry, ParseError>),
    /// A PubMed id with no DOI to go on.
    Unresolved(Unresolved),
}

/// Resolve every PMID / PMCID entry of a parsed bibliography to its DOI,
/// leaving every other entry as it was. One request per PubMed id, at
/// NCBI's rate.
///
/// # Errors
///
/// A provenance-log failure (fail-closed); every other failure is an
/// [`Unresolved`] entry.
pub async fn resolve_entries(
    entries: Vec<Result<ParsedEntry, ParseError>>,
    ctx: &FetchContext,
) -> Result<Vec<Resolved>, FetchError> {
    let mut out = Vec::with_capacity(entries.len());
    for entry in entries {
        let (id, entry_key) = match &entry {
            Err(ParseError::UnsupportedIdentifier {
                kind,
                value,
                entry_key,
            }) => match PubmedId::from_kind(kind, value) {
                Some(id) => (id, entry_key.clone()),
                None => {
                    out.push(Resolved::Entry(entry));
                    continue;
                }
            },
            _ => {
                out.push(Resolved::Entry(entry));
                continue;
            }
        };
        out.push(match lookup(&id, ctx).await {
            Ok(Lookup::Doi(doi)) => Resolved::Entry(Ok(ParsedEntry {
                ref_: Ref::Doi(doi),
                entry_key,
            })),
            Ok(found) => Resolved::Unresolved(Unresolved {
                reason: found.reason(&id).unwrap_or_default(),
                code: if found == Lookup::NoRecord {
                    crate::ErrorCode::NotFound
                } else {
                    crate::ErrorCode::NotImplemented
                },
                id,
                entry_key,
            }),
            Err(FetchError::Log(e)) => return Err(FetchError::Log(e)),
            Err(e) => Resolved::Unresolved(Unresolved {
                reason: format!("looking up {} at NCBI failed: {e}", id.display()),
                code: crate::ErrorCode::from(&e),
                id,
                entry_key,
            }),
        });
    }
    Ok(out)
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn pubmed_ids_parse_in_their_written_forms_and_bare_digits_do_not() {
        for (input, want) in [
            ("pmid:9659853", PubmedId::Pmid("9659853".into())),
            ("PMID: 9659853", PubmedId::Pmid("9659853".into())),
            ("pmcid:PMC3531190", PubmedId::Pmcid("3531190".into())),
            ("PMC3531190", PubmedId::Pmcid("3531190".into())),
            (
                "https://pubmed.ncbi.nlm.nih.gov/9659853/",
                PubmedId::Pmid("9659853".into()),
            ),
            (
                "https://www.ncbi.nlm.nih.gov/pmc/articles/PMC3531190/",
                PubmedId::Pmcid("3531190".into()),
            ),
            (
                "https://pmc.ncbi.nlm.nih.gov/articles/PMC3531190/",
                PubmedId::Pmcid("3531190".into()),
            ),
        ] {
            assert_eq!(PubmedId::parse(input), Some(want), "{input}");
        }
        for no in [
            "9659853",
            "pmid:",
            "pmid:12a",
            "10.1176/ajp.155.7.895",
            "https://example.org/9659853",
        ] {
            assert_eq!(PubmedId::parse(no), None, "{no}");
        }
        assert_eq!(
            PubmedId::from_kind("PMCID", "PMC3531190"),
            Some(PubmedId::Pmcid("3531190".into()))
        );
        assert_eq!(PubmedId::from_kind("ISBN", "x"), None);
    }

    /// Shapes measured against the live API on 2026-09-29.
    #[test]
    fn an_esummary_answer_gives_the_doi_no_doi_or_no_record() {
        let with_doi = serde_json::json!({"result": {"uids": ["9659853"], "9659853": {
        "articleids": [
            {"idtype": "pubmed", "value": "9659853"},
            {"idtype": "doi", "value": "10.1176/ajp.155.7.895"}
        ]}}});
        assert_eq!(
            classify(&with_doi, "9659853"),
            Lookup::Doi(Doi::parse("10.1176/ajp.155.7.895").unwrap())
        );
        let no_doi = serde_json::json!({"result": {"uids": ["1"], "1": {
            "articleids": [{"idtype": "pubmed", "value": "1"}]}}});
        assert_eq!(classify(&no_doi, "1"), Lookup::NoDoi);
        let missing = serde_json::json!({"result": {"uids": ["99"], "99": {
            "uid": "99", "error": "cannot get document summary"}}});
        assert_eq!(classify(&missing, "99"), Lookup::NoRecord);
    }
}
