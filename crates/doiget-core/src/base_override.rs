//! The `DOIGET_*_BASE` test / proxy overrides, as one table (#587).
//!
//! Every fetch source honours a `DOIGET_<SOURCE>_BASE` that points it at
//! another origin: a wiremock server in tests, an institutional proxy in the
//! field. The HTTP client has to agree with the sources about that, and it
//! used to do so through two hand-maintained copies (the CLI's
//! `build_http_client` and the MCP server's `build_http_client_for_fetch`),
//! each with a selection condition and a registration list. The lists knew
//! six to eleven of the fifteen overridable keys, so setting any one base put
//! the process in test mode and silently dropped the rest from the client --
//! `datacite`, `hal`, `openaire`, `core` and `europe-pmc` could not be mocked
//! at all.
//!
//! [`BASE_OVERRIDES`] is now the only list, and [`test_client_from_env`] the
//! only builder that reads it.
//!
//! ## Which variables switch to the test client
//!
//! Only the rows marked [`BaseOverride::selects_test_client`] -- the same
//! seven the condition always tested. The others are honoured *within* test
//! mode but never cause it, and that asymmetry is deliberate: the production
//! client is `https_only` and applies its allowlists to redirects, so
//! `DOIGET_APS_BASE=https://proxy.example.edu` alone works today on the
//! production client, with every other source intact. Making it select the
//! test client would rebuild the process from the overrides alone and drop
//! Crossref, which is the regression the issue's option 2 would have
//! shipped.
//!
//! Test mode stays **exclusive**: a source with no override is absent from
//! the test client, so a test that forgot to mock one fails immediately and
//! offline with `UnknownSource` instead of reaching the real API. That
//! isolation is the property the issue's first-proposed additive fix would
//! have lost.

use crate::http::HttpClient;

/// One overridable source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BaseOverride {
    /// The HTTP client's source key, i.e. the key the source passes to
    /// `HttpClient::fetch_*`.
    pub source: &'static str,
    /// The environment variable naming the replacement base URL.
    pub env: &'static str,
    /// Whether setting this variable switches the whole process to the
    /// allow-http test client. See the module docs for why only some do.
    pub selects_test_client: bool,
}

const fn row(source: &'static str, env: &'static str, selects: bool) -> BaseOverride {
    BaseOverride {
        source,
        env,
        selects_test_client: selects,
    }
}

/// Every `DOIGET_*_BASE` a fetch source reads. Rows sharing a `source` are
/// alternatives, the first one set winning: `DOIGET_ARXIV_SRC_BASE` is the
/// source-bundle endpoint's override and stands in for `DOIGET_ARXIV_BASE`
/// only when that is unset.
pub const BASE_OVERRIDES: &[BaseOverride] = &[
    // Tier 1 and the always-on metadata / full-text keys.
    row("arxiv", "DOIGET_ARXIV_BASE", true),
    row("arxiv", "DOIGET_ARXIV_SRC_BASE", true),
    row("crossref", "DOIGET_CROSSREF_BASE", true),
    row("unpaywall", "DOIGET_UNPAYWALL_BASE", true),
    row("oa-publisher", "DOIGET_OA_PUBLISHER_BASE", true),
    row("openalex", "DOIGET_OPENALEX_BASE", true),
    row("ar5iv", "DOIGET_AR5IV_BASE", true),
    // A published DOI's bioRxiv / medRxiv preprint (#640).
    row("biorxiv", "DOIGET_BIORXIV_BASE", false),
    row("inspire", "DOIGET_INSPIRE_BASE", false),
    row("ads", "DOIGET_ADS_BASE", false),
    // PubMed id -> DOI (#500).
    row("ncbi", "DOIGET_NCBI_BASE", true),
    // `cite <github URL>` (#614).
    row("github", "DOIGET_GITHUB_API_BASE", true),
    row("github-raw", "DOIGET_GITHUB_RAW_BASE", true),
    // Tier 2 optional chain (`metadata`).
    row("datacite", "DOIGET_DATACITE_BASE", false),
    row("europe-pmc", "DOIGET_EUROPE_PMC_BASE", false),
    row("openaire", "DOIGET_OPENAIRE_BASE", false),
    row("hal", "DOIGET_HAL_BASE", false),
    row("core", "DOIGET_CORE_BASE", false),
    // Tier 3 (`tdm-*`). Registering a key whose source is not compiled in
    // is harmless: nothing asks for it.
    row("tdm-aps", "DOIGET_APS_BASE", false),
    row("tdm-elsevier", "DOIGET_ELSEVIER_BASE", false),
    row("tdm-springer", "DOIGET_SPRINGER_BASE", false),
    row("tdm-ieee", "DOIGET_IEEE_BASE", false),
];

/// Why the override environment could not become a client.
#[derive(Debug, thiserror::Error)]
pub enum BaseOverrideError {
    /// A `DOIGET_*_BASE` is set but is not an absolute URL with a host.
    #[error("{env} is not a URL with a host: {value:?}")]
    NotAUrl {
        /// The variable.
        env: &'static str,
        /// What it was set to.
        value: String,
    },
}

/// The allow-http test client for the overrides set in the environment, or
/// `None` when no [`BaseOverride::selects_test_client`] variable is set and
/// the caller should build its production client.
///
/// In test mode every row that is set is registered -- selecting or not --
/// and nothing else is: see the module docs on exclusivity.
///
/// # Errors
///
/// [`BaseOverrideError::NotAUrl`] for a set variable that does not parse, in
/// test mode. A malformed non-selecting variable outside test mode is the
/// source's to report, as before.
pub fn test_client_from_env() -> Result<Option<HttpClient>, BaseOverrideError> {
    test_client_from(|env| std::env::var(env).ok())
}

/// [`test_client_from_env`] over an arbitrary variable lookup, for tests.
///
/// # Errors
///
/// As [`test_client_from_env`].
pub fn test_client_from(
    lookup: impl Fn(&str) -> Option<String>,
) -> Result<Option<HttpClient>, BaseOverrideError> {
    match test_entries(lookup)? {
        Some(entries) => {
            let borrowed: Vec<(&str, &str)> =
                entries.iter().map(|(s, h)| (*s, h.as_str())).collect();
            Ok(Some(HttpClient::new_for_tests_allow_http_multi(&borrowed)))
        }
        None => Ok(None),
    }
}

/// The `(source key, host)` pairs the test client registers, or `None` for
/// production. Split out so the selection logic is testable without
/// building a client.
fn test_entries(
    lookup: impl Fn(&str) -> Option<String>,
) -> Result<Option<Vec<(&'static str, String)>>, BaseOverrideError> {
    let set: Vec<(BaseOverride, String)> = BASE_OVERRIDES
        .iter()
        .filter_map(|o| lookup(o.env).map(|v| (*o, v)))
        .collect();
    if !set.iter().any(|(o, _)| o.selects_test_client) {
        return Ok(None);
    }
    let mut entries: Vec<(&'static str, String)> = Vec::new();
    for (o, value) in set {
        if entries.iter().any(|(s, _)| *s == o.source) {
            continue;
        }
        let host = url::Url::parse(&value)
            .ok()
            .and_then(|u| u.host_str().map(str::to_string))
            .ok_or_else(|| BaseOverrideError::NotAUrl {
                env: o.env,
                value: value.clone(),
            })?;
        entries.push((o.source, host));
    }
    Ok(Some(entries))
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |k| {
            pairs
                .iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| (*v).to_string())
        }
    }

    #[test]
    fn nothing_set_is_production() {
        assert!(test_entries(env(&[])).unwrap().is_none());
    }

    /// The institutional-proxy case: a TDM or Tier-2 base alone keeps the
    /// production client, so every other source keeps its allowlist.
    #[test]
    fn a_non_selecting_base_alone_stays_on_the_production_client() {
        for o in BASE_OVERRIDES.iter().filter(|o| !o.selects_test_client) {
            let pairs = [(o.env, "https://proxy.example.edu")];
            assert!(test_entries(env(&pairs)).unwrap().is_none(), "{}", o.env);
        }
    }

    /// #587: in test mode, every overridden key is registered -- the Tier-2
    /// and Tier-3 keys used to be dropped.
    #[test]
    fn test_mode_registers_every_overridden_source_and_nothing_else() {
        let pairs = [
            ("DOIGET_CROSSREF_BASE", "http://127.0.0.1:9001"),
            ("DOIGET_DATACITE_BASE", "http://127.0.0.1:9002"),
            ("DOIGET_EUROPE_PMC_BASE", "http://127.0.0.1:9003"),
            ("DOIGET_APS_BASE", "http://127.0.0.1:9004"),
        ];
        let entries = test_entries(env(&pairs)).unwrap().expect("test mode");
        let keys: Vec<&str> = entries.iter().map(|(s, _)| *s).collect();
        assert_eq!(keys, vec!["crossref", "datacite", "europe-pmc", "tdm-aps"]);
        assert!(entries.iter().all(|(_, h)| h == "127.0.0.1"));
    }

    #[test]
    fn the_source_bundle_base_stands_in_for_arxiv_only_when_it_is_unset() {
        let src_only = [("DOIGET_ARXIV_SRC_BASE", "http://src.test:1")];
        let entries = test_entries(env(&src_only)).unwrap().expect("test mode");
        assert_eq!(entries, vec![("arxiv", "src.test".to_string())]);
        let both = [
            ("DOIGET_ARXIV_BASE", "http://api.test:1"),
            ("DOIGET_ARXIV_SRC_BASE", "http://src.test:1"),
        ];
        let entries = test_entries(env(&both)).unwrap().expect("test mode");
        assert_eq!(entries, vec![("arxiv", "api.test".to_string())]);
    }

    #[test]
    fn a_malformed_base_in_test_mode_names_its_variable() {
        let pairs = [
            ("DOIGET_CROSSREF_BASE", "http://127.0.0.1:1"),
            ("DOIGET_HAL_BASE", "not a url"),
        ];
        let err = test_entries(env(&pairs)).unwrap_err();
        assert!(
            err.to_string().starts_with("DOIGET_HAL_BASE is not a URL"),
            "{err}"
        );
    }

    /// The table is only worth having if it is complete: every
    /// `DOIGET_*_BASE` a fetch source reads must have a row, or setting it
    /// in test mode drops its key again. `DOIGET_GITHUB_BASE` is
    /// `doiget version`'s own client, not the fetch client, and lives in
    /// doiget-cli.
    #[test]
    fn every_base_override_read_by_a_source_has_a_row() {
        let sources = [
            include_str!("orchestrator.rs"),
            include_str!("paper_tex_source.rs"),
            include_str!("paper_text.rs"),
            include_str!("discovery.rs"),
            include_str!("citation_graph.rs"),
        ]
        .concat();
        let mut read: Vec<&str> = sources
            .match_indices("\"DOIGET_")
            .filter_map(|(i, _)| {
                let rest = &sources[i + 1..];
                let name = &rest[..rest.find('"')?];
                name.ends_with("_BASE").then_some(name)
            })
            .collect();
        read.sort_unstable();
        read.dedup();
        assert!(read.len() >= 10, "scan found too few variables: {read:?}");
        let missing: Vec<&str> = read
            .into_iter()
            .filter(|v| !BASE_OVERRIDES.iter().any(|o| o.env == *v))
            .collect();
        assert!(
            missing.is_empty(),
            "DOIGET_*_BASE without a BASE_OVERRIDES row: {missing:?}"
        );
    }
}
