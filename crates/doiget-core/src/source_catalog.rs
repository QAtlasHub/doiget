//! Every source doiget knows, whether or not this binary was built with it,
//! and whether it can serve a given ref right now (#605).
//!
//! `fetch --dry-run` lists `candidate_hosts`, and its own help says that is
//! the static allowlist, "not a prediction". Before deciding whether to wait
//! for doiget or download a paper by hand, the question is different: can
//! **any** configured source deliver this DOI, and if not, why not -- not
//! built, not enabled, no credentials, or the publisher is not one it covers.
//! [`CATALOG`] is the list that answers it, and it exists in every build:
//! a default binary has no Tier-3 code (ADR-0002), but it can still say that
//! `tdm-aps` covers `10.1103` and needs `--features tdm-aps`.
//!
//! This is a statement about **reach**, never about outcome: a source that is
//! `Ready` for a DOI may still find nothing. The coverage report says so.

use crate::{CapabilityProfile, Ref};

/// What a source can contribute to a fetch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Bibliographic metadata only; never a PDF.
    Metadata,
    /// Where an OA copy lives -- a URL the `oa-publisher` leg then fetches.
    OaLocation,
    /// The PDF itself.
    Content,
}

impl Role {
    /// Wire token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Metadata => "metadata",
            Self::OaLocation => "oa_location",
            Self::Content => "content",
        }
    }
}

/// Which refs a source is able to answer for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Covers {
    /// Any DOI.
    AnyDoi,
    /// arXiv ids (and DOIs through the preprint fallback).
    Arxiv,
    /// DOIs registered with DataCite (Zenodo, figshare, Dryad, OSF, ...).
    DataCiteDois,
    /// DOIs under these registrant prefixes only (ADR-0041).
    Prefixes(&'static [&'static str]),
}

/// One catalog row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceInfo {
    /// Source key, as in the attempt trace.
    pub name: &'static str,
    /// Tier 1 (always on), 2 (opt-in, no key), 3 (TDM agreement + key).
    pub tier: u8,
    /// What it contributes.
    pub role: Role,
    /// Which refs it answers for.
    pub covers: Covers,
    /// The publisher a prefix-scoped source belongs to.
    pub publisher: Option<&'static str>,
    /// Cargo feature the source is compiled under, if any.
    pub feature: Option<&'static str>,
    /// Environment the source needs at run time (enable flag, key, agreement).
    pub enable: &'static [&'static str],
    /// Whether this binary contains the source.
    pub compiled: bool,
}

const fn t1(name: &'static str, role: Role, covers: Covers) -> SourceInfo {
    SourceInfo {
        name,
        tier: 1,
        role,
        covers,
        publisher: None,
        feature: None,
        enable: &[],
        compiled: true,
    }
}

const fn t2(
    name: &'static str,
    role: Role,
    covers: Covers,
    enable: &'static [&'static str],
) -> SourceInfo {
    SourceInfo {
        name,
        tier: 2,
        role,
        covers,
        publisher: None,
        feature: Some("metadata"),
        enable,
        compiled: cfg!(feature = "metadata"),
    }
}

/// Every source, in the order a fetch consults them.
pub const CATALOG: &[SourceInfo] = &[
    t1("crossref", Role::Metadata, Covers::AnyDoi),
    t1("unpaywall", Role::OaLocation, Covers::AnyDoi),
    t1("oa-publisher", Role::Content, Covers::AnyDoi),
    t1("arxiv", Role::Content, Covers::Arxiv),
    t2(
        "datacite",
        Role::Metadata,
        Covers::DataCiteDois,
        &["DOIGET_ENABLE_DATACITE"],
    ),
    t2(
        "europe-pmc",
        Role::OaLocation,
        Covers::AnyDoi,
        &["DOIGET_ENABLE_EUROPE_PMC"],
    ),
    t2(
        "openaire",
        Role::OaLocation,
        Covers::AnyDoi,
        &["DOIGET_ENABLE_OPENAIRE"],
    ),
    t2(
        "hal",
        Role::OaLocation,
        Covers::AnyDoi,
        &["DOIGET_ENABLE_HAL"],
    ),
    t2(
        "core",
        Role::OaLocation,
        Covers::AnyDoi,
        &["DOIGET_ENABLE_CORE", "DOIGET_CORE_API_KEY"],
    ),
    t2(
        "openalex",
        Role::OaLocation,
        Covers::AnyDoi,
        &["DOIGET_ENABLE_OPENALEX"],
    ),
    t2(
        "semantic_scholar",
        Role::Metadata,
        Covers::AnyDoi,
        &["DOIGET_ENABLE_S2"],
    ),
    t2(
        "inspire",
        Role::OaLocation,
        Covers::AnyDoi,
        &["DOIGET_ENABLE_INSPIRE"],
    ),
    t2(
        "biorxiv",
        Role::OaLocation,
        Covers::AnyDoi,
        &["DOIGET_ENABLE_BIORXIV"],
    ),
    t2(
        "doaj",
        Role::Metadata,
        Covers::AnyDoi,
        &["DOIGET_ENABLE_DOAJ"],
    ),
    SourceInfo {
        name: "tdm-aps",
        tier: 3,
        role: Role::Content,
        covers: Covers::Prefixes(&["10.1103"]),
        publisher: Some("American Physical Society (APS)"),
        feature: Some("tdm-aps"),
        enable: &["DOIGET_KEY_APS", "DOIGET_AGREE_TDM_APS"],
        compiled: cfg!(feature = "tdm-aps"),
    },
    SourceInfo {
        name: "tdm-elsevier",
        tier: 3,
        role: Role::Content,
        covers: Covers::Prefixes(&["10.1016", "10.1006", "10.1053"]),
        publisher: Some("Elsevier BV"),
        feature: Some("tdm-elsevier"),
        enable: &["DOIGET_KEY_ELSEVIER", "DOIGET_AGREE_TDM_ELSEVIER"],
        compiled: cfg!(feature = "tdm-elsevier"),
    },
    SourceInfo {
        name: "tdm-springer",
        tier: 3,
        role: Role::Content,
        covers: Covers::Prefixes(&["10.1007", "10.1038", "10.1057", "10.1140"]),
        publisher: Some("Springer Nature"),
        feature: Some("tdm-springer"),
        enable: &["DOIGET_KEY_SPRINGER", "DOIGET_AGREE_TDM_SPRINGER"],
        compiled: cfg!(feature = "tdm-springer"),
    },
    SourceInfo {
        name: "tdm-ieee",
        tier: 3,
        role: Role::Content,
        covers: Covers::Prefixes(&["10.1109", "10.23919"]),
        publisher: Some("IEEE"),
        feature: Some("tdm-ieee"),
        enable: &["DOIGET_KEY_IEEE", "DOIGET_AGREE_TDM_IEEE"],
        compiled: cfg!(feature = "tdm-ieee"),
    },
];

/// Whether a source can be asked about a ref right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Availability {
    /// Compiled, enabled, and covers the ref. Says nothing about whether it
    /// will find anything.
    Ready,
    /// This binary was built without it.
    NotBuilt {
        /// The Cargo feature to build with.
        feature: &'static str,
    },
    /// Built, but its environment is not set.
    NotEnabled {
        /// What to set.
        enable: &'static [&'static str],
    },
    /// Built and enabled, but the ref is outside what it covers.
    NotCovered,
}

impl Availability {
    /// Wire token.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::NotBuilt { .. } => "not_built",
            Self::NotEnabled { .. } => "not_enabled",
            Self::NotCovered => "not_covered",
        }
    }

    /// One line a person can act on.
    #[must_use]
    pub fn remedy(&self) -> Option<String> {
        match self {
            Self::Ready | Self::NotCovered => None,
            Self::NotBuilt { feature } => Some(format!("build with --features {feature}")),
            Self::NotEnabled { enable } => Some(format!("set {}", enable.join(" and "))),
        }
    }
}

/// Whether `info` covers `ref_` by its own scope, ignoring build and config.
/// A DataCite-only source is reported as covering every DOI: which agency
/// registered a DOI is only known by asking.
#[must_use]
pub fn covers(info: &SourceInfo, ref_: &Ref) -> bool {
    match (info.covers, ref_) {
        // A DOI reaches arXiv only through the preprint fallback (#325), and
        // only when Unpaywall named the arXiv copy -- which `coverage`
        // already reports as the open copy. On its own it does not cover one.
        (Covers::Arxiv, Ref::Arxiv(_)) => true,
        (Covers::Arxiv, Ref::Doi(_)) | (_, Ref::Arxiv(_)) => false,
        (Covers::AnyDoi | Covers::DataCiteDois, Ref::Doi(_)) => true,
        (Covers::Prefixes(p), Ref::Doi(d)) => {
            let prefix = d.as_str().split('/').next().unwrap_or("");
            p.contains(&prefix)
        }
    }
}

/// Build and configuration state of `info`, with no ref to be in scope for:
/// [`Availability::NotBuilt`], [`Availability::NotEnabled`] or
/// [`Availability::Ready`] -- never `NotCovered`.
#[must_use]
pub fn configured(info: &SourceInfo, profile: &CapabilityProfile) -> Availability {
    if !info.compiled {
        Availability::NotBuilt {
            feature: info.feature.unwrap_or("?"),
        }
    } else if !enabled(info.name, profile) {
        Availability::NotEnabled {
            enable: info.enable,
        }
    } else {
        Availability::Ready
    }
}

/// [`Availability`] of `info` for `ref_` under `profile`.
#[must_use]
pub fn availability(info: &SourceInfo, profile: &CapabilityProfile, ref_: &Ref) -> Availability {
    match configured(info, profile) {
        Availability::Ready if !covers(info, ref_) => Availability::NotCovered,
        other => other,
    }
}

fn enabled(name: &str, p: &CapabilityProfile) -> bool {
    let m = &p.metadata;
    match name {
        "datacite" => m.datacite,
        "europe-pmc" => m.europe_pmc,
        "openaire" => m.openaire,
        "hal" => m.hal,
        "core" => m.core,
        "openalex" => m.openalex,
        "semantic_scholar" => m.semantic_scholar,
        "doaj" => m.doaj,
        "biorxiv" => m.biorxiv,
        "inspire" => m.inspire,
        "tdm-aps" => p.tdm_aps.is_some(),
        "tdm-elsevier" => p.tdm_elsevier.is_some(),
        "tdm-springer" => p.tdm_springer.is_some(),
        "tdm-ieee" => p.tdm_ieee.is_some(),
        _ => true,
    }
}

/// Catalog rows relevant to `publisher`: a registrant prefix (`10.1103`) or a
/// case-insensitive fragment of a publisher's name (`aps`, `springer`).
/// Sources that cover any DOI are always relevant.
#[must_use]
pub fn for_publisher(publisher: &str) -> Vec<&'static SourceInfo> {
    let q = publisher.trim().to_lowercase();
    CATALOG
        .iter()
        .filter(|s| match s.covers {
            Covers::Prefixes(p) => {
                p.iter().any(|x| q == *x || q.starts_with(&format!("{x}/")))
                    || s.publisher.is_some_and(|n| n.to_lowercase().contains(&q))
            }
            Covers::Arxiv => q == "arxiv" || q == "10.48550",
            _ => true,
        })
        .collect()
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    fn doi(s: &str) -> Ref {
        Ref::parse(s).expect("ref")
    }

    #[test]
    fn a_default_profile_names_the_switch_for_each_source_it_will_not_ask() {
        let p = CapabilityProfile::for_tests();
        let r = doi("10.1103/PhysRevB.48.10345");
        let by = |n: &str| CATALOG.iter().find(|s| s.name == n).expect("row");
        assert_eq!(availability(by("crossref"), &p, &r), Availability::Ready);
        let aps = availability(by("tdm-aps"), &p, &r);
        if cfg!(feature = "tdm-aps") {
            assert!(matches!(aps, Availability::NotEnabled { .. }));
        } else {
            assert_eq!(
                aps.remedy().as_deref(),
                Some("build with --features tdm-aps")
            );
        }
        if cfg!(feature = "metadata") {
            assert_eq!(
                availability(by("hal"), &p, &r).remedy().as_deref(),
                Some("set DOIGET_ENABLE_HAL")
            );
        }
    }

    #[test]
    fn a_publisher_scoped_source_covers_only_its_prefixes() {
        let aps = CATALOG.iter().find(|s| s.name == "tdm-aps").unwrap();
        assert!(covers(aps, &doi("10.1103/PhysRevB.48.10345")));
        assert!(!covers(aps, &doi("10.1007/BF01340294")));
        assert!(!covers(aps, &doi("arxiv:2401.00001")));
    }

    #[test]
    fn a_publisher_query_matches_prefix_or_name_and_keeps_general_sources() {
        let names = |q: &str| -> Vec<&str> { for_publisher(q).iter().map(|s| s.name).collect() };
        assert!(names("10.1103").contains(&"tdm-aps"));
        assert!(!names("10.1103").contains(&"tdm-springer"));
        assert!(names("Springer").contains(&"tdm-springer"));
        assert!(names("springer").contains(&"crossref"));
        assert!(!names("springer").contains(&"arxiv"));
    }

    /// The catalog repeats the TDM prefixes so a build without the source
    /// can still name them; it must never disagree with the source itself.
    #[test]
    fn the_catalog_prefixes_are_the_sources_own() {
        #[allow(unused_mut)]
        let mut pairs: Vec<(&str, &[&str])> = Vec::new();
        #[cfg(feature = "tdm-aps")]
        pairs.push(("tdm-aps", crate::sources::tdm_aps::PUBLISHER_PREFIXES));
        #[cfg(feature = "tdm-elsevier")]
        pairs.push((
            "tdm-elsevier",
            crate::sources::tdm_elsevier::PUBLISHER_PREFIXES,
        ));
        #[cfg(feature = "tdm-springer")]
        pairs.push((
            "tdm-springer",
            crate::sources::tdm_springer::PUBLISHER_PREFIXES,
        ));
        #[cfg(feature = "tdm-ieee")]
        pairs.push(("tdm-ieee", crate::sources::tdm_ieee::PUBLISHER_PREFIXES));
        for (name, own) in pairs {
            let row = CATALOG.iter().find(|s| s.name == name).expect("row");
            assert_eq!(row.covers, Covers::Prefixes(own), "{name}");
        }
    }

    #[test]
    fn arxiv_covers_arxiv_ids_and_not_dois() {
        let arxiv = CATALOG.iter().find(|s| s.name == "arxiv").unwrap();
        assert!(covers(arxiv, &doi("arXiv:cond-mat/0409292")));
        assert!(!covers(arxiv, &doi("10.1038/nphys1170")));
    }

    /// Every module under `src/sources/` has a catalog row, so a new source
    /// cannot ship invisible to `doiget sources` (DOAJ once did).
    #[test]
    fn every_source_module_has_a_catalog_row() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/src/sources");
        for entry in std::fs::read_dir(dir).expect("sources dir") {
            let file = entry.expect("entry").file_name();
            let stem = file.to_str().unwrap().trim_end_matches(".rs");
            let name = match stem {
                "mod" => continue,
                "core_oa" => "core",
                "europepmc" => "europe-pmc",
                "s2" => "semantic_scholar",
                other => &other.replace('_', "-"),
            };
            assert!(
                CATALOG.iter().any(|s| s.name == name),
                "src/sources/{stem}.rs has no CATALOG row named {name:?}"
            );
        }
    }
}
