//! Tier-1 source implementations (Phase 1+).
//!
//! Each source is a concrete `Source` trait impl. Per `docs/SOURCES.md` §1, Tier 1
//! is the always-on Open Access tier (Crossref / Unpaywall / arXiv). Tier 2/3
//! sources land in Phase 4/5.

pub mod crossref;
