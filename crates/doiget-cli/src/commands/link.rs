//! `doiget link <doi>` — resolve a DOI to its arXiv preprint and identity
//! cluster (#281 item 5: arXiv ↔ published-DOI linking & dedup).
//!
//! Given a published **DOI**, this reports whether the same work has a free
//! **arXiv preprint** (plus the OpenAlex id and title), so an agent can read
//! the free full text (`doiget text arxiv:<id>`) or dedup a preprint against
//! its journal version without fetching both.
//!
//! Backed by [`doiget_core::discovery::resolve_links_for_doi`] over OpenAlex
//! (`/works?filter=doi:`): Tier-1 OA metadata, always-on; never fetches a
//! PDF. arXiv → DOI (the reverse direction) is a planned follow-up; a
//! non-DOI ref is rejected.

use std::io::Write;

use anyhow::{Context, Result};

use doiget_core::discovery::{resolve_links_for_doi, PaperLinks};
use doiget_core::{ErrorCode, Ref};

use super::fetch::{build_resolve_context, cli_exit_code, render_fetch_error, CliExit};
use super::output::OutputMode;

/// Production OpenAlex API base. Overridable via `DOIGET_OPENALEX_BASE`
/// (test wiremock origin), mirroring the `search` subcommand.
const OPENALEX_DEFAULT_BASE: &str = "https://api.openalex.org";

/// Run the `link` subcommand.
///
/// # Errors
///
/// A non-DOI ref is a usage error; OpenAlex failures surface a typed
/// [`ErrorCode`] as a process exit code via [`CliExit`] (e.g. a DOI with no
/// OpenAlex work → `NOT_FOUND`).
pub async fn run(ref_: String, mode: OutputMode, quiet_was_explicit: bool) -> Result<()> {
    let parsed = Ref::parse(&ref_).with_context(|| format!("invalid ref {ref_:?}"))?;
    let doi = match parsed {
        Ref::Doi(d) => d,
        Ref::Arxiv(_) => {
            anyhow::bail!(
                "`doiget link` resolves a DOI to its arXiv preprint; \
                 arXiv → DOI linking is a follow-up (#281). Pass a DOI."
            );
        }
    };

    let base = resolve_openalex_base()?;
    // Omit `mailto` when no contact email is configured (never a
    // placeholder); the empty string is skipped downstream.
    let contact_email = std::env::var("DOIGET_CONTACT_EMAIL").unwrap_or_default();
    let ctx = build_resolve_context().context("building fetch context")?;

    let links = match resolve_links_for_doi(&base, &contact_email, doi.as_str(), &ctx).await {
        Ok(l) => l,
        Err(e) => {
            // Route through the shared renderer so a denial-class failure
            // (e.g. an off-allowlist OpenAlex redirect) carries its ADR-0023
            // `= note:` line, single-sourced with the other commands (#287).
            render_fetch_error(&e);
            return Err(anyhow::Error::new(CliExit(cli_exit_code(ErrorCode::from(
                &e,
            )))));
        }
    };

    // Artifact-class (ADR-0017 Amendment 2 / #301): suppress only on
    // explicit Quiet; the non-TTY implicit fallback still emits.
    if mode == OutputMode::Quiet && quiet_was_explicit {
        return Ok(());
    }

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    if mode == OutputMode::Json {
        let s = serde_json::to_string_pretty(&links).context("serializing link JSON")?;
        writeln!(out, "{s}").context("writing link JSON to stdout")?;
        return Ok(());
    }

    render_human(&mut out, &links)?;
    Ok(())
}

/// Resolve the OpenAlex base URL: `DOIGET_OPENALEX_BASE` override (tests) or
/// the production default.
fn resolve_openalex_base() -> Result<url::Url> {
    let raw =
        std::env::var("DOIGET_OPENALEX_BASE").unwrap_or_else(|_| OPENALEX_DEFAULT_BASE.to_string());
    url::Url::parse(&raw).with_context(|| format!("DOIGET_OPENALEX_BASE is not a URL: {raw}"))
}

/// Render the identity cluster in human mode. The arXiv line is the
/// load-bearing signal (present preprint → readable / dedup target).
fn render_human(out: &mut impl Write, links: &PaperLinks) -> Result<()> {
    let arxiv = match &links.arxiv {
        Some(a) => a.as_str(),
        None => "-  (no arXiv preprint found)",
    };
    writeln!(out, "doi:      {}", links.doi.as_deref().unwrap_or("-"))
        .context("writing doi line")?;
    writeln!(out, "arxiv:    {arxiv}").context("writing arxiv line")?;
    writeln!(out, "openalex: {}", links.openalex_id).context("writing openalex line")?;
    writeln!(out, "title:    {}", links.title).context("writing title line")?;
    Ok(())
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    fn sample() -> PaperLinks {
        PaperLinks {
            doi: Some("10.1103/physrevb.1".into()),
            arxiv: Some("2101.54321v2".into()),
            openalex_id: "W55".into(),
            title: "Published Version".into(),
        }
    }

    #[test]
    fn json_output_is_the_paper_links_shape() {
        let v = serde_json::to_value(sample()).expect("serialize");
        assert_eq!(v["doi"], "10.1103/physrevb.1");
        assert_eq!(v["arxiv"], "2101.54321v2");
        assert_eq!(v["openalex_id"], "W55");
        assert_eq!(v["title"], "Published Version");
    }

    #[test]
    fn human_render_shows_arxiv_and_placeholder() {
        let mut buf: Vec<u8> = Vec::new();
        render_human(&mut buf, &sample()).expect("render");
        let s = String::from_utf8(buf).expect("utf8");
        assert!(s.contains("arxiv:    2101.54321v2"), "got: {s}");

        let mut none = sample();
        none.arxiv = None;
        let mut buf2: Vec<u8> = Vec::new();
        render_human(&mut buf2, &none).expect("render");
        let s2 = String::from_utf8(buf2).expect("utf8");
        assert!(s2.contains("no arXiv preprint found"), "got: {s2}");
    }

    #[tokio::test]
    async fn link_rejects_arxiv_input() {
        let err = run("arxiv:2401.12345".to_string(), OutputMode::Quiet, true)
            .await
            .expect_err("arXiv input must be a usage error");
        assert!(err.to_string().contains("Pass a DOI"), "got: {err}");
    }
}
