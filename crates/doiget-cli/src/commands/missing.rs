//! `doiget missing <bibliography>` -- which cited works still have no local
//! PDF, and where to get each one (#607).
//!
//! `list-recent --missing-pdf` answers that for the **store**. While writing,
//! the question is about a **bibliography file**: of the 71 works a talk
//! cites, which 24 have no PDF yet, and what should each download be saved
//! as. Every entry gets one status:
//!
//! | status | meaning | counts as missing |
//! |---|---|---|
//! | `in_store` | the store holds its PDF | no |
//! | `local_file` | `--path-pattern` names a file that exists | no |
//! | `oa_available` | an OA copy is known; `doiget fetch <ref>` gets it | yes |
//! | `no_oa` | resolved, and no OA copy is known; `landing_url` is the publisher's page | yes |
//! | `oa_unknown` | resolved, but the OA lookup did not complete | yes |
//! | `not_in_store` | `--offline`: no PDF in the store, not looked up | yes |
//! | `unresolved` | the id did not resolve (absent, or unreachable) | yes |
//! | `unsupported` | no DOI / arXiv id, or one doiget cannot resolve yet | yes |
//!
//! Human mode prints a table on stdout and a summary on stderr; `--mode json`
//! prints one JSON object per entry (like `verify`). The exit code is the
//! number of entries still missing, capped at 255 (the `batch` convention).
//!
//! Read-only: it never fetches a PDF or writes the store. The online path
//! costs a metadata lookup plus one Unpaywall request per entry not already
//! satisfied, under the usual rate limiter.

use std::io::Write;

use anyhow::Result;
use camino::{Utf8Path, Utf8PathBuf};
use serde_json::{json, Value};

use doiget_core::orchestrator::{cite_metadata, resolve_only_with_options, MetadataOnlyOptions};
use doiget_core::refs::{parse_input, ParseError};
use doiget_core::store::{FsStore, Metadata, Store};
use doiget_core::{CapabilityProfile, ErrorCode, Ref};

use super::fetch::CliExit;
use super::output::{print_err, OutputMode};

/// One entry's answer.
#[derive(Debug, Default)]
struct Row {
    entry_key: Option<String>,
    ref_: Option<String>,
    title: Option<String>,
    year: Option<i32>,
    status: &'static str,
    oa_url: Option<String>,
    landing_url: Option<String>,
    expected_path: Option<String>,
    detail: Option<String>,
}

impl Row {
    fn is_missing(&self) -> bool {
        !matches!(self.status, "in_store" | "local_file")
    }

    fn to_json(&self) -> Value {
        let mut v = json!({
            "entry_key": self.entry_key,
            "ref": self.ref_,
            "status": self.status,
            "missing": self.is_missing(),
        });
        for (k, val) in [
            ("title", self.title.clone().map(Value::from)),
            ("year", self.year.map(Value::from)),
            ("oa_url", self.oa_url.clone().map(Value::from)),
            ("landing_url", self.landing_url.clone().map(Value::from)),
            ("expected_path", self.expected_path.clone().map(Value::from)),
            ("detail", self.detail.clone().map(Value::from)),
        ] {
            if let Some(val) = val {
                v[k] = val;
            }
        }
        if self.status == "oa_available" {
            if let Some(r) = &self.ref_ {
                v["fetch_command"] = Value::from(format!("doiget fetch {r}"));
            }
        }
        v
    }
}

/// Entry point for `doiget missing <path> [--format] [--path-pattern] [--offline]`.
///
/// # Errors
///
/// An unreadable file or unknown `--format` is misuse (exit 2); otherwise
/// the number of missing entries is the exit code.
pub async fn run(
    path: String,
    format: String,
    path_pattern: Option<String>,
    offline: bool,
    mode: OutputMode,
    quiet_was_explicit: bool,
) -> Result<()> {
    // Artifact-class (ADR-0017 Amendment 1): the table IS the requested
    // output, so only a Quiet the user asked for suppresses it -- not the
    // implicit Quiet of a non-TTY stdout.
    let mode = if mode == OutputMode::Quiet && !quiet_was_explicit {
        OutputMode::Human
    } else {
        mode
    };
    let fmt = super::verify::parse_format(&format)?;
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) => {
            print_err(format_args!(
                "error: failed to read reference file {path}: {e}"
            ));
            return Err(anyhow::Error::new(CliExit(2)));
        }
    };
    let entries = parse_input(&text, fmt, Some(Utf8Path::new(&path)));
    let store_root = super::resolve_store_root()?;
    let store = FsStore::new(store_root.clone())?;
    let online = if offline {
        None
    } else {
        Some((
            super::fetch::build_resolve_context()?,
            CapabilityProfile::from_env()?,
        ))
    };

    let mut rows = Vec::new();
    for entry in entries {
        let parsed = match entry {
            Ok(p) => p,
            Err(e) => {
                rows.push(unparsed_row(e));
                continue;
            }
        };
        let ref_ = parsed.ref_;
        let safekey = ref_.safekey();
        let mut row = Row {
            entry_key: parsed.entry_key.clone(),
            ref_: Some(ref_.as_input_str().to_string()),
            ..Row::default()
        };
        if let Some(m) = doiget_core::store::blocking_section(|| store.read(&safekey))
            .ok()
            .flatten()
        {
            fill_identity(&mut row, &m);
        }
        let key = parsed.entry_key.as_deref().unwrap_or(safekey.as_str());
        row.expected_path = path_pattern.as_ref().map(|p| {
            p.replace("{key}", key)
                .replace("{safekey}", safekey.as_str())
        });

        if store_root
            .join(format!("{}.pdf", safekey.as_str()))
            .exists()
        {
            row.status = "in_store";
        } else if row
            .expected_path
            .as_deref()
            .is_some_and(|p| Utf8PathBuf::from(p).exists())
        {
            row.status = "local_file";
        } else if let Some((ctx, profile)) = &online {
            look_up(&mut row, &ref_, profile, ctx).await?;
        } else {
            row.status = "not_in_store";
        }
        rows.push(row);
    }

    emit(&rows, mode)?;
    let missing = rows.iter().filter(|r| r.is_missing()).count();
    if mode != OutputMode::Quiet {
        let count = |s: &str| rows.iter().filter(|r| r.status == s).count();
        print_err(format_args!(
            "missing: {missing} of {} entries have no local PDF (in store: {}, local file: {}, \
             OA available: {}, no OA copy: {})",
            rows.len(),
            count("in_store"),
            count("local_file"),
            count("oa_available"),
            count("no_oa"),
        ));
    }
    if missing > 0 {
        let code = i32::try_from(missing.min(255)).unwrap_or(255);
        return Err(anyhow::Error::new(CliExit(code)));
    }
    Ok(())
}

/// Metadata lookup with the OA location, for an entry with no local PDF.
async fn look_up(
    row: &mut Row,
    ref_: &Ref,
    profile: &CapabilityProfile,
    ctx: &doiget_core::source::FetchContext,
) -> Result<()> {
    let opts = MetadataOnlyOptions::default().with_oa_location(true);
    match resolve_only_with_options(ref_, profile, ctx, opts).await {
        Ok(outcome) => {
            let m = cite_metadata(ref_, &outcome);
            if row.title.is_none() {
                fill_identity(row, &m);
            }
            row.landing_url = landing_url(ref_, &outcome.metadata);
            match (&outcome.oa_url, outcome.oa_status.as_deref(), ref_) {
                (Some(url), _, _) => {
                    row.status = "oa_available";
                    row.oa_url = Some(url.clone());
                }
                // arXiv is open by construction; the id is the location.
                (None, _, Ref::Arxiv(id)) => {
                    row.status = "oa_available";
                    row.oa_url = Some(format!("https://arxiv.org/pdf/{}", id.as_str()));
                }
                (None, Some(_), _) => row.status = "no_oa",
                (None, None, _) => {
                    row.status = "oa_unknown";
                    row.detail = Some("the Unpaywall lookup did not complete".into());
                }
            }
        }
        Err(e) => {
            let code: ErrorCode = (&e).into();
            // Same fail-closed rule as `verify`: a provenance-log failure is
            // the operator's fault, not a property of the reference.
            if code == ErrorCode::LogError {
                anyhow::bail!("provenance log error (aborting): {e}");
            }
            row.status = "unresolved";
            row.detail = Some(format!("{}: {e}", code.as_wire()));
        }
    }
    Ok(())
}

fn fill_identity(row: &mut Row, m: &Metadata) {
    if !m.title.is_empty() {
        row.title = Some(m.title.clone());
    }
    row.year = m.year;
}

/// The publisher's page for a work: Crossref's `resource.primary.URL`, else
/// the DOI resolver (ADR-0053: a DOI link is an address, not a fetch).
fn landing_url(ref_: &Ref, crossref: &Value) -> Option<String> {
    match ref_ {
        Ref::Doi(doi) => Some(
            crossref
                .pointer("/resource/primary/URL")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| format!("https://doi.org/{}", doi.as_str())),
        ),
        Ref::Arxiv(id) => Some(format!("https://arxiv.org/abs/{}", id.as_str())),
    }
}

fn unparsed_row(e: ParseError) -> Row {
    let (entry_key, detail) = match e {
        ParseError::NoIdentifier { entry_key } => {
            (entry_key, "entry has no DOI / arXiv id".to_string())
        }
        ParseError::UnsupportedIdentifier {
            kind,
            value,
            entry_key,
        } => (
            entry_key,
            doiget_core::refs::unsupported_identifier_claim(kind, &value),
        ),
        ParseError::InvalidRef {
            raw,
            entry_key,
            source,
        } => (
            entry_key,
            format!("{raw:?} is not a DOI / arXiv id: {source}"),
        ),
        other => (None, other.to_string()),
    };
    Row {
        entry_key,
        status: "unsupported",
        detail: Some(detail),
        ..Row::default()
    }
}

fn emit(rows: &[Row], mode: OutputMode) -> Result<()> {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    if mode == OutputMode::Json {
        for r in rows {
            writeln!(out, "{}", r.to_json())?;
        }
        return Ok(());
    }
    if mode == OutputMode::Quiet {
        return Ok(());
    }
    for r in rows.iter().filter(|r| r.is_missing()) {
        let what = r.ref_.as_deref().or(r.entry_key.as_deref()).unwrap_or("?");
        let key = r.entry_key.as_deref().unwrap_or("-");
        let title = r.title.as_deref().map(truncate).unwrap_or_default();
        writeln!(out, "{:<13} {key:<24} {what}  {title}", r.status)?;
        if let Some(u) = &r.oa_url {
            writeln!(out, "{:<13}   fetch: doiget fetch {what}   ({u})", "")?;
        }
        if let Some(u) = r.landing_url.as_ref().filter(|_| r.oa_url.is_none()) {
            writeln!(out, "{:<13}   landing: {u}", "")?;
        }
        if let Some(p) = &r.expected_path {
            writeln!(out, "{:<13}   save as: {p}", "")?;
        }
        if let Some(d) = &r.detail {
            writeln!(out, "{:<13}   {d}", "")?;
        }
    }
    Ok(())
}

fn truncate(s: &str) -> String {
    let t: String = s.chars().take(60).collect();
    if t.len() < s.len() {
        format!("{t}...")
    } else {
        t
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn only_a_local_pdf_counts_as_not_missing() {
        for (status, missing) in [
            ("in_store", false),
            ("local_file", false),
            ("oa_available", true),
            ("no_oa", true),
            ("oa_unknown", true),
            ("not_in_store", true),
            ("unresolved", true),
            ("unsupported", true),
        ] {
            let r = Row {
                status,
                ..Row::default()
            };
            assert_eq!(r.is_missing(), missing, "{status}");
        }
    }

    #[test]
    fn the_landing_url_prefers_crossrefs_resource_and_falls_back_to_the_resolver() {
        let doi = Ref::parse("10.1007/BF01340294").expect("doi");
        let cr = json!({"resource": {"primary": {"URL": "https://link.springer.com/10.1007/BF01340294"}}});
        assert_eq!(
            landing_url(&doi, &cr).as_deref(),
            Some("https://link.springer.com/10.1007/BF01340294")
        );
        assert_eq!(
            landing_url(&doi, &json!({})).as_deref(),
            Some("https://doi.org/10.1007/BF01340294")
        );
    }

    #[test]
    fn a_row_names_the_fetch_command_only_when_an_oa_copy_is_known() {
        let r = Row {
            ref_: Some("10.1/x".into()),
            status: "oa_available",
            oa_url: Some("https://example.org/x.pdf".into()),
            ..Row::default()
        };
        assert_eq!(r.to_json()["fetch_command"], "doiget fetch 10.1/x");
        let r = Row {
            ref_: Some("10.1/x".into()),
            status: "no_oa",
            ..Row::default()
        };
        assert!(r.to_json().get("fetch_command").is_none());
    }
}
