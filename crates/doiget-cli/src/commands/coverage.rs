//! `doiget coverage <doi>` and `doiget sources` (#605): which source, if any,
//! can deliver a DOI's PDF -- asked before fetching, not learned from a
//! failure.
//!
//! `coverage` makes the two read-only metadata calls a fetch starts with
//! (Crossref, then Unpaywall for the OA location), never touches a publisher
//! and never writes the store. Per source it reports whether this doiget can
//! ask it at all -- `ready`, `not_built`, `not_enabled`, `not_covered` --
//! with the switch that changes it, and ends with a verdict. Everything is a
//! statement about **reach**: a `ready` optional source may still find
//! nothing, and the report says so rather than predicting.
//!
//! `sources [--publisher NAME|PREFIX]` is the same catalog without a DOI:
//! "is this journal covered?" answered from the prefix scopes (ADR-0041).

use std::io::Write;

use anyhow::Result;
use serde_json::{json, Value};

use doiget_core::orchestrator::{resolve_only_with_options, MetadataOnlyOptions};
use doiget_core::source_catalog::{
    availability, configured, for_publisher, Availability, Covers, Role, SourceInfo, CATALOG,
};
use doiget_core::{CapabilityProfile, ErrorCode, Ref};

use super::output::OutputMode;

/// Entry point for `doiget coverage <ref>`.
///
/// # Errors
///
/// An invalid ref is misuse; a resolve failure is reported with its code.
pub async fn run_coverage(input: String, mode: OutputMode, quiet_was_explicit: bool) -> Result<()> {
    let ref_ = super::parse_ref_or_exit(&input)?;
    let ctx = super::fetch::build_resolve_context()?;
    let profile = CapabilityProfile::from_env()?;
    let opts = MetadataOnlyOptions::default().with_oa_location(true);
    let outcome = match resolve_only_with_options(&ref_, &profile, &ctx, opts).await {
        Ok(o) => o,
        Err(e) => {
            // The shared renderer, so `coverage` honours the error[CODE]
            // contract every ref-taking command does.
            super::fetch::render_fetch_error(&e);
            let code: ErrorCode = (&e).into();
            return Err(anyhow::Error::new(super::fetch::CliExit(
                super::fetch::cli_exit_code(code),
            )));
        }
    };
    let meta = &outcome.metadata;
    let text = |ptr: &str| {
        meta.pointer(ptr)
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    let rows: Vec<(&SourceInfo, Availability)> = CATALOG
        .iter()
        .map(|s| (s, availability(s, &profile, &ref_)))
        .collect();
    let report = json!({
        "ref": ref_.as_input_str(),
        "publisher": text("/publisher"),
        "journal": text("/container-title/0").map(|v| doiget_core::markup::plain_title(&v)),
        "type": text("/type"),
        "oa_status": outcome.oa_status,
        "oa_url": outcome.oa_url,
        "landing_url": landing_url(&ref_, meta),
        "sources": rows.iter().map(|(s, a)| row_json(s, a)).collect::<Vec<_>>(),
        "verdict": verdict(&ref_, outcome.oa_url.as_deref(), &rows),
    });
    emit(&report, mode, quiet_was_explicit, render_coverage)
}

/// Entry point for `doiget sources [--publisher]`.
///
/// # Errors
///
/// Only a capability-profile error.
pub fn run_sources(
    publisher: Option<String>,
    mode: OutputMode,
    quiet_was_explicit: bool,
) -> Result<()> {
    let profile = CapabilityProfile::from_env()?;
    let rows: Vec<&SourceInfo> = match &publisher {
        Some(p) => for_publisher(p),
        None => CATALOG.iter().collect(),
    };
    let list: Vec<Value> = rows
        .iter()
        .map(|s| {
            // No DOI, so nothing to be out of scope for: build and
            // configuration state only.
            let a = configured(s, &profile);
            let mut v = row_json(s, &a);
            v["covers"] = covers_json(s);
            if let Some(p) = s.publisher {
                v["publisher"] = Value::from(p);
            }
            v
        })
        .collect();
    let report = json!({ "publisher_query": publisher, "sources": list });
    emit(&report, mode, quiet_was_explicit, render_sources)
}

fn row_json(s: &SourceInfo, a: &Availability) -> Value {
    let mut v = json!({
        "source": s.name,
        "tier": s.tier,
        "role": s.role.as_str(),
        "status": a.as_str(),
    });
    if let Some(r) = a.remedy() {
        v["enable"] = Value::from(r);
    }
    v
}

fn covers_json(s: &SourceInfo) -> Value {
    match s.covers {
        Covers::AnyDoi => json!("any DOI"),
        Covers::Arxiv => json!("arXiv ids (and DOIs with an arXiv preprint)"),
        Covers::DataCiteDois => json!("DataCite DOIs (Zenodo, figshare, Dryad, OSF, ...)"),
        Covers::Prefixes(p) => json!(p),
    }
}

/// What the report concludes, in one sentence plus the facts behind it.
fn verdict(ref_: &Ref, oa_url: Option<&str>, rows: &[(&SourceInfo, Availability)]) -> Value {
    if let Some(url) = oa_url {
        return json!({
            "summary": "an open copy is known; `doiget fetch` will try it",
            "oa_url": url,
            "note": "if its host is not on the allowlist, fetch reports CAPABILITY_DENIED naming the setting that allows it",
        });
    }
    if let Ref::Arxiv(_) = ref_ {
        return json!({ "summary": "arXiv serves every id it has; `doiget fetch` gets it" });
    }
    let tdm_ready: Vec<&str> = rows
        .iter()
        .filter(|(s, a)| s.tier == 3 && *a == Availability::Ready)
        .map(|(s, _)| s.name)
        .collect();
    if !tdm_ready.is_empty() {
        return json!({
            "summary": "no open copy is known, but a publisher source you are entitled to covers this DOI",
            "sources": tdm_ready,
        });
    }
    // Sources that could look but were not asked, because of this build or
    // this configuration. Listed, never ranked: whether one of them holds a
    // copy is exactly what is not known (#505).
    let could_look: Vec<Value> = rows
        .iter()
        .filter(|(s, a)| {
            matches!(s.role, Role::OaLocation | Role::Content)
                && matches!(
                    a,
                    Availability::NotEnabled { .. } | Availability::NotBuilt { .. }
                )
                && doiget_core::source_catalog::covers(s, ref_)
        })
        .map(|(s, a)| json!({ "source": s.name, "enable": a.remedy() }))
        .collect();
    json!({
        "summary": "nothing this doiget asked has an open copy on record; download it from the publisher's page if you have access",
        "not_asked": could_look,
    })
}

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

/// Artifact-class output (ADR-0017 Amendment 1): JSON in `--mode json`, the
/// rendered report otherwise, suppressed only by a Quiet the user asked for.
fn emit(
    report: &Value,
    mode: OutputMode,
    quiet_was_explicit: bool,
    render: fn(&Value) -> String,
) -> Result<()> {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    match mode {
        OutputMode::Json => writeln!(out, "{}", serde_json::to_string_pretty(report)?)?,
        OutputMode::Quiet if quiet_was_explicit => {}
        _ => write!(out, "{}", render(report))?,
    }
    Ok(())
}

fn render_coverage(r: &Value) -> String {
    let s = |k: &str| r[k].as_str().unwrap_or("-").to_string();
    let mut out = format!(
        "{}\n  publisher: {}\n  journal:   {}\n  OA status: {}   OA copy: {}\n\n",
        s("ref"),
        s("publisher"),
        s("journal"),
        s("oa_status"),
        s("oa_url"),
    );
    out.push_str(&render_rows(&r["sources"]));
    let v = &r["verdict"];
    out.push_str(&format!("\n{}\n", v["summary"].as_str().unwrap_or("")));
    if let Some(list) = v["not_asked"].as_array().filter(|l| !l.is_empty()) {
        out.push_str("  not asked (listed, not ranked -- whether one holds a copy is unknown):\n");
        for x in list {
            out.push_str(&format!(
                "    {:<14} {}\n",
                x["source"].as_str().unwrap_or(""),
                x["enable"].as_str().unwrap_or("")
            ));
        }
    }
    if v.get("oa_url").is_none() {
        out.push_str(&format!("  publisher's page: {}\n", s("landing_url")));
    }
    out
}

fn render_sources(r: &Value) -> String {
    let mut out = String::new();
    for x in r["sources"].as_array().into_iter().flatten() {
        let covers = match &x["covers"] {
            Value::Array(a) => a
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(" "),
            other => other.as_str().unwrap_or("").to_string(),
        };
        let publisher = x["publisher"]
            .as_str()
            .map(|p| format!(" ({p})"))
            .unwrap_or_default();
        out.push_str(&format!(
            "{:<17} tier {}  {:<12} {:<12} {covers}{publisher}\n",
            x["source"].as_str().unwrap_or(""),
            x["tier"],
            x["role"].as_str().unwrap_or(""),
            x["status"].as_str().unwrap_or(""),
        ));
        if let Some(e) = x["enable"].as_str() {
            out.push_str(&format!("{:<17}   -> {e}\n", ""));
        }
    }
    out
}

fn render_rows(rows: &Value) -> String {
    let mut out = String::new();
    for x in rows.as_array().into_iter().flatten() {
        out.push_str(&format!(
            "  {:<17} tier {}  {:<12} {:<12} {}\n",
            x["source"].as_str().unwrap_or(""),
            x["tier"],
            x["role"].as_str().unwrap_or(""),
            x["status"].as_str().unwrap_or(""),
            x["enable"].as_str().unwrap_or(""),
        ));
    }
    out
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    fn rows(p: &CapabilityProfile, r: &Ref) -> Vec<(&'static SourceInfo, Availability)> {
        CATALOG.iter().map(|s| (s, availability(s, p, r))).collect()
    }

    #[test]
    fn a_known_open_copy_is_the_verdict() {
        let r = Ref::parse("10.1234/x").unwrap();
        let p = CapabilityProfile::from_env().expect("profile");
        let v = verdict(&r, Some("https://example.org/x.pdf"), &rows(&p, &r));
        assert_eq!(v["oa_url"], "https://example.org/x.pdf");
    }

    #[test]
    fn with_nothing_open_the_verdict_lists_what_was_not_asked_without_ranking() {
        let r = Ref::parse("10.1103/PhysRevB.48.10345").unwrap();
        let p = CapabilityProfile::from_env().expect("profile");
        let v = verdict(&r, None, &rows(&p, &r));
        let names: Vec<&str> = v["not_asked"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x["source"].as_str().unwrap())
            .collect();
        // APS's own source covers 10.1103; Springer's does not.
        assert!(names.contains(&"tdm-aps"), "{names:?}");
        assert!(!names.contains(&"tdm-springer"), "{names:?}");
        // Metadata-only sources cannot deliver a PDF, so they are not listed.
        assert!(!names.contains(&"semantic_scholar"), "{names:?}");
    }
}
