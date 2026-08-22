//! Source implementations.
//!
//! Each source is a concrete `Source` trait impl. Per `docs/SOURCES.md` §1:
//!
//! - Tier 1 (Open Access, always compiled in): Crossref / Unpaywall / arXiv.
//! - Tier 2 (metadata enrichment, Phase 4, behind the `metadata` Cargo
//!   feature): OpenAlex / Semantic Scholar / DOAJ / DataCite.
//! - Tier 3 (TDM, Phase 5, behind per-publisher Cargo features):
//!   Springer Nature OA / APS Harvest / Elsevier ScienceDirect.

pub mod arxiv;
pub mod crossref;
pub mod unpaywall;

// ---------------------------------------------------------------------------
// Tier 2 (Phase 4) — compile-gated by the `metadata` Cargo feature.
// ---------------------------------------------------------------------------

#[cfg(feature = "metadata")]
pub mod openalex;

#[cfg(feature = "metadata")]
pub mod s2;

#[cfg(feature = "metadata")]
pub mod doaj;

/// DataCite — DOI **resolution** for the second registration agency
/// (Zenodo / figshare / Dryad / OSF). Not enrichment: without it those
/// DOIs report `NotFound`. Runtime-gated by `DOIGET_ENABLE_DATACITE`,
/// off by default (ADR-0040, #414).
#[cfg(feature = "metadata")]
pub mod datacite;

// ---------------------------------------------------------------------------
// Tier 3 (Phase 5) — compile-gated by per-publisher Cargo features.
// Default release binaries ship NONE of these; opt-in builds enable one
// or more via `--features tdm-<publisher>` (ADR-0002).
// ---------------------------------------------------------------------------

#[cfg(feature = "tdm-springer")]
pub mod tdm_springer;

#[cfg(feature = "tdm-aps")]
pub mod tdm_aps;

#[cfg(feature = "tdm-elsevier")]
pub mod tdm_elsevier;
