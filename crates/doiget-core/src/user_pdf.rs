//! Add a PDF the user downloaded themselves to the store (#606).
//!
//! For a paper with no OA copy, the user downloads it under their own access
//! and wants the store to know, so no other project hits the same wall. What
//! this may and may not check is fixed by ADR-0003: **the PDF is an opaque
//! blob**, so nothing here reads its text, its metadata streams or its page
//! structure. The checks are the ones a fetch already makes of bytes off the
//! wire, plus one about the file's *name*:
//!
//! - a regular file, not a symlink, starting with `%PDF-`, under
//!   [`crate::PDF_MAX_BYTES`];
//! - a file name that is itself a work id -- `BF01397394.pdf`,
//!   `PhysRev.34.1293.pdf` -- must be the id of the ref it is added under.
//!   A name like `1928-024 PCPS Hartree - The wave mechanics.pdf` makes no
//!   claim and is accepted. `force` overrides the mismatch.
//!
//! The entry is recorded as what it is: `[doiget] source = "user"`,
//! `license = "unknown"`, `origin = "user-supplied"` -- never as an open
//! copy, since `bib`, `paper_pdf_path` and an agent would otherwise read a
//! licensed download as free to use. The file is copied, not moved, and the
//! provenance row names the store path only: the original path, which
//! carries the user's home directory, is not logged.

use camino::{Utf8Path, Utf8PathBuf};
use chrono::Utc;

use crate::orchestrator::{cite_metadata, resolve_only, write_metadata_and_pdf};
use crate::source::{FetchContext, FetchError};
use crate::store::{DoigetExtension, Metadata, Store, ORIGIN_USER_SUPPLIED};
use crate::{CapabilityProfile, Ref};

/// What [`add_user_pdf`] stored.
#[derive(Debug, Clone)]
pub struct AddOutcome {
    /// The entry's safekey.
    pub safekey: String,
    /// Where the PDF now lives in the store.
    pub path: Utf8PathBuf,
    /// Its size.
    pub size_bytes: u64,
    /// The title it was recorded under, for the confirmation line.
    pub title: String,
    /// Whether a PDF already in the store was replaced (`force`).
    pub replaced: bool,
}

/// Why a file was not added.
#[derive(Debug, thiserror::Error)]
pub enum AddError {
    /// Not a regular file (missing, a directory, a device, ...).
    #[error("{0} is not a regular file")]
    NotAFile(Utf8PathBuf),
    /// A symlink: resolved elsewhere, it is not the file the user named.
    #[error("{0} is a symbolic link; pass the file it points to")]
    Symlink(Utf8PathBuf),
    /// Reading the file failed.
    #[error("reading {path}: {source}")]
    Io {
        /// The file.
        path: Utf8PathBuf,
        /// The error.
        #[source]
        source: std::io::Error,
    },
    /// The first bytes are not `%PDF-` -- the same check a fetch makes.
    #[error("{0} does not start with %PDF-, so it is not a PDF")]
    NotAPdf(Utf8PathBuf),
    /// Larger than a fetch would accept.
    #[error("{path} is {actual} bytes, over the {cap}-byte cap a fetched PDF is held to")]
    TooLarge {
        /// The file.
        path: Utf8PathBuf,
        /// Its size.
        actual: u64,
        /// [`crate::PDF_MAX_BYTES`].
        cap: u64,
    },
    /// The store already holds a PDF for this ref.
    #[error("the store already holds a PDF for {ref_} at {path}; pass --force to replace it")]
    AlreadyStored {
        /// The ref.
        ref_: String,
        /// The stored PDF.
        path: Utf8PathBuf,
    },
    /// The file name is another work's id.
    #[error(
        "the file name {stem:?} looks like the id of a different work than {ref_}; \
         check the download, or pass --force if the name is wrong and the file is right"
    )]
    NamesAnotherWork {
        /// The file name without `.pdf`.
        stem: String,
        /// The ref it was being added under.
        ref_: String,
    },
    /// The ref's metadata could not be resolved (and the store had none).
    #[error("resolving {ref_}: {source}")]
    Resolve {
        /// The ref.
        ref_: String,
        /// The resolver error.
        #[source]
        source: FetchError,
    },
    /// The store write failed.
    #[error("writing the store: {0}")]
    Store(#[source] FetchError),
}

/// The file's name without its `.pdf` extension (case-insensitive) and a
/// browser's duplicate suffix (`name (1).pdf`).
#[must_use]
pub fn file_stem(file: &Utf8Path) -> String {
    let name = file.file_name().unwrap_or("");
    let stem = name
        .len()
        .checked_sub(4)
        .filter(|&i| name.is_char_boundary(i) && name[i..].eq_ignore_ascii_case(".pdf"))
        .map_or(name, |i| &name[..i]);
    let stem = match stem.rfind(" (") {
        Some(i)
            if stem.ends_with(')')
                && stem[i + 2..stem.len() - 1]
                    .chars()
                    .all(|c| c.is_ascii_digit()) =>
        {
            &stem[..i]
        }
        _ => stem,
    };
    stem.trim().to_string()
}

/// Whether `stem` names `ref_`: the DOI suffix (`BF01340294` for
/// `10.1007/BF01340294`), the whole DOI with `/` as `_` or `-`, or the arXiv
/// id with or without a version. Case-insensitive.
#[must_use]
pub fn stem_names(stem: &str, ref_: &Ref) -> bool {
    let s = stem.to_lowercase();
    match ref_ {
        Ref::Doi(d) => {
            let doi = d.as_str().to_lowercase();
            let suffix = doi.split_once('/').map_or(doi.as_str(), |(_, x)| x);
            s == suffix || s == doi.replace('/', "_") || s == doi.replace('/', "-")
        }
        Ref::Arxiv(id) => {
            let id = id.as_str().to_lowercase().replace('/', "_");
            let unversioned = match s.rfind('v') {
                Some(i)
                    if i > 0
                        && i + 1 < s.len()
                        && s[i + 1..].chars().all(|c| c.is_ascii_digit()) =>
                {
                    &s[..i]
                }
                _ => s.as_str(),
            };
            unversioned == id
                || unversioned == format!("arxiv_{id}")
                || unversioned == format!("arxiv-{id}")
        }
    }
}

/// Whether `stem` reads as a work id rather than a description: six or more
/// characters, only letters, digits, `.`, `-` and `_`, and at least one
/// digit. `BF01397394`, `PhysRev.34.1293` and `rspa.1950.0036` are ids;
/// `1928-024 PCPS Hartree - The wave mechanics` is not.
#[must_use]
pub fn looks_like_an_id(stem: &str) -> bool {
    stem.len() >= 6
        && stem
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        && stem.chars().any(|c| c.is_ascii_digit())
}

/// Check `file` the way a fetch checks bytes off the wire, without reading
/// past the magic number (ADR-0003). Returns its size.
///
/// # Errors
///
/// [`AddError::NotAFile`], [`AddError::Symlink`], [`AddError::TooLarge`],
/// [`AddError::NotAPdf`] or [`AddError::Io`].
pub fn check_file(file: &Utf8Path) -> Result<u64, AddError> {
    let io = |source| AddError::Io {
        path: file.to_path_buf(),
        source,
    };
    let meta = std::fs::symlink_metadata(file).map_err(io)?;
    if meta.file_type().is_symlink() {
        return Err(AddError::Symlink(file.to_path_buf()));
    }
    if !meta.is_file() {
        return Err(AddError::NotAFile(file.to_path_buf()));
    }
    if meta.len() > crate::PDF_MAX_BYTES {
        return Err(AddError::TooLarge {
            path: file.to_path_buf(),
            actual: meta.len(),
            cap: crate::PDF_MAX_BYTES,
        });
    }
    let mut magic = [0u8; 5];
    let n = {
        use std::io::Read;
        let mut f = std::fs::File::open(file).map_err(io)?;
        f.read(&mut magic).map_err(io)?
    };
    if n < 5 || &magic != b"%PDF-" {
        return Err(AddError::NotAPdf(file.to_path_buf()));
    }
    Ok(meta.len())
}

/// Add `file` to the store as `ref_`'s PDF.
///
/// Metadata comes from the store when the ref is already there, else from
/// the same resolver `cite` uses. `force` replaces a stored PDF and
/// overrides a file name that names another work.
///
/// # Errors
///
/// Any [`AddError`]; nothing is written unless every check passed.
pub async fn add_user_pdf(
    ref_: &Ref,
    file: &Utf8Path,
    force: bool,
    profile: &CapabilityProfile,
    ctx: &FetchContext,
    store: &dyn Store,
    store_root: &Utf8Path,
) -> Result<AddOutcome, AddError> {
    let size = check_file(file)?;
    let stem = file_stem(file);
    if !force && looks_like_an_id(&stem) && !stem_names(&stem, ref_) {
        return Err(AddError::NamesAnotherWork {
            stem,
            ref_: ref_.as_input_str().to_string(),
        });
    }
    let safekey = ref_.safekey();
    let pdf_path = store_root.join(format!("{}.pdf", safekey.as_str()));
    let replaced = pdf_path.exists();
    if replaced && !force {
        return Err(AddError::AlreadyStored {
            ref_: ref_.as_input_str().to_string(),
            path: pdf_path,
        });
    }
    let stored = crate::store::blocking_section(|| store.read(&safekey))
        .ok()
        .flatten();
    let mut m: Metadata = match stored {
        Some(m) => m,
        None => {
            let outcome =
                resolve_only(ref_, profile, ctx)
                    .await
                    .map_err(|source| AddError::Resolve {
                        ref_: ref_.as_input_str().to_string(),
                        source,
                    })?;
            cite_metadata(ref_, &outcome)
        }
    };
    let prior = m.doiget.take();
    m.pdf_path = Some(format!("{}.pdf", safekey.as_str()));
    m.doiget = Some(DoigetExtension {
        fetched_at: Utc::now(),
        source: "user".to_string(),
        license: crate::store::metadata::LICENSE_UNDETERMINED.to_string(),
        oa_status: prior.as_ref().and_then(|d| d.oa_status.clone()),
        size_bytes: size,
        mcp_call_id: None,
        tags: prior.as_ref().map(|d| d.tags.clone()).unwrap_or_default(),
        collections: prior
            .as_ref()
            .map(|d| d.collections.clone())
            .unwrap_or_default(),
        annotation: prior.as_ref().and_then(|d| d.annotation.clone()),
        repaired_fields: prior
            .as_ref()
            .map(|d| d.repaired_fields.clone())
            .unwrap_or_default(),
        short_venue: prior.as_ref().and_then(|d| d.short_venue.clone()),
        origin: Some(ORIGIN_USER_SUPPLIED.to_string()),
    });
    crate::store::blocking_section(|| write_metadata_and_pdf(store, &safekey, &m, Some(file), ctx))
        .map_err(AddError::Store)?;
    Ok(AddOutcome {
        safekey: safekey.as_str().to_string(),
        path: pdf_path,
        size_bytes: size,
        title: m.title,
        replaced,
    })
}

/// The one ref in `candidates` whose id `stem` is, if exactly one (#606
/// `--from-dir`). Two candidates sharing a DOI suffix is ambiguous and
/// matches none.
#[must_use]
pub fn match_file<'a>(stem: &str, candidates: &'a [Ref]) -> Option<&'a Ref> {
    let mut hits = candidates.iter().filter(|r| stem_names(stem, r));
    let first = hits.next()?;
    hits.next().is_none().then_some(first)
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    fn r(s: &str) -> Ref {
        Ref::parse(s).expect("ref")
    }

    /// The download names from the issue.
    #[test]
    fn publisher_download_names_match_their_dois() {
        for (name, doi) in [
            ("BF01340294.pdf", "10.1007/BF01340294"),
            ("PhysRev.34.1293.pdf", "10.1103/PhysRev.34.1293"),
            ("RevModPhys.23.69.pdf", "10.1103/RevModPhys.23.69"),
            ("rspa.1950.0036.pdf", "10.1098/rspa.1950.0036"),
            ("BF01340294 (1).pdf", "10.1007/BF01340294"),
            ("10.1007_BF01340294.PDF", "10.1007/BF01340294"),
        ] {
            let stem = file_stem(Utf8Path::new(name));
            assert!(stem_names(&stem, &r(doi)), "{name} -> {stem}");
        }
        assert!(stem_names("2401.12345v2", &r("arxiv:2401.12345")));
        assert!(stem_names("cond-mat_0409292", &r("cond-mat/0409292")));
    }

    #[test]
    fn a_descriptive_name_makes_no_claim_and_an_id_name_does() {
        assert!(!looks_like_an_id(&file_stem(Utf8Path::new(
            "1928-024 PCPS Hartree - The wave mechanics of an atom.pdf"
        ))));
        assert!(looks_like_an_id("BF01397394"));
        assert!(looks_like_an_id("PhysRev.34.1293"));
        assert!(!looks_like_an_id("thesis"));
    }

    #[test]
    fn from_dir_matching_requires_exactly_one_candidate() {
        let c = vec![r("10.1007/BF01340294"), r("10.1103/PhysRev.34.1293")];
        assert_eq!(
            match_file("bf01340294", &c).map(Ref::as_input_str),
            Some("10.1007/BF01340294")
        );
        assert!(match_file("unrelated-2020", &c).is_none());
        let dup = vec![r("10.1007/X123456"), r("10.9999/X123456")];
        assert!(match_file("X123456", &dup).is_none());
    }

    #[test]
    fn the_file_checks_are_the_ones_a_fetch_makes() {
        let td = tempfile::TempDir::new().expect("tempdir");
        let dir = Utf8Path::from_path(td.path()).expect("utf-8");
        let pdf = dir.join("ok.pdf");
        std::fs::write(&pdf, b"%PDF-1.4\n...").expect("write");
        assert_eq!(check_file(&pdf).expect("ok"), 12);
        let html = dir.join("login.pdf");
        std::fs::write(&html, b"<!doctype html>").expect("write");
        assert!(matches!(check_file(&html), Err(AddError::NotAPdf(_))));
        assert!(matches!(check_file(dir), Err(AddError::NotAFile(_))));
        assert!(matches!(
            check_file(&dir.join("absent.pdf")),
            Err(AddError::Io { .. })
        ));
        #[cfg(unix)]
        {
            let link = dir.join("link.pdf");
            std::os::unix::fs::symlink(&pdf, &link).expect("symlink");
            assert!(matches!(check_file(&link), Err(AddError::Symlink(_))));
        }
    }
}
