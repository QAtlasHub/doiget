//! Citation keys from a template (#610).
//!
//! `cite` and `bib` key every entry by its safekey (`doi_10.1007_BF01340294`)
//! unless told otherwise, and that stays the default: it is stable and
//! collision-free by construction. A project that keeps human keys
//! (`fock1930naherungsmethode`) asks for a template instead:
//!
//! | placeholder | value |
//! |---|---|
//! | `{author}` | first author's family name, lower-cased and ASCII-folded |
//! | `{year}` | the year, or nothing when unknown |
//! | `{title_word}` | first significant title word, lower-cased and ASCII-folded |
//! | `{safekey}` | the safekey itself |
//!
//! Folding is Unicode NFD with the combining marks dropped (`Näherung` ->
//! `naherung`), plus the letters NFD does not decompose (`ß` -> `ss`,
//! `ø` -> `o`, `æ` -> `ae`, ...). "Significant" skips articles and
//! prepositions in English, German and French. Any character a BibTeX key
//! cannot carry is dropped. A template that renders empty -- no author, no
//! year, no title -- falls back to the safekey rather than emitting `@article{,`.

use unicode_normalization::UnicodeNormalization;

use super::Metadata;

/// Why a key template was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum KeyTemplateError {
    /// A `{name}` that is not one of the placeholders.
    #[error(
        "unknown placeholder {{{0}}} in key template; use {{author}}, {{year}}, {{title_word}} or {{safekey}}"
    )]
    UnknownPlaceholder(String),
    /// A `{` without its `}`.
    #[error("unclosed '{{' in key template {0:?}")]
    Unclosed(String),
}

/// Check `template` without rendering it, so a bad `--key-template` or
/// `[cite] key_template` fails before any network work.
///
/// # Errors
///
/// As [`render_key`].
pub fn validate_template(template: &str) -> Result<(), KeyTemplateError> {
    render_key(template, &Metadata::default(), "x").map(|_| ())
}

/// The key `template` gives `m`, whose safekey is `safekey`.
///
/// # Errors
///
/// [`KeyTemplateError`] for an unknown placeholder or an unclosed `{`.
pub fn render_key(template: &str, m: &Metadata, safekey: &str) -> Result<String, KeyTemplateError> {
    let mut out = String::new();
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let close = after
            .find('}')
            .ok_or_else(|| KeyTemplateError::Unclosed(template.to_string()))?;
        let value = match &after[..close] {
            "author" => m
                .authors
                .first()
                .map(|a| fold(family_name(a)))
                .unwrap_or_default(),
            "year" => m.year.map(|y| y.to_string()).unwrap_or_default(),
            "title_word" => first_significant_word(&m.title)
                .map(fold)
                .unwrap_or_default(),
            "safekey" => safekey.to_string(),
            other => return Err(KeyTemplateError::UnknownPlaceholder(other.to_string())),
        };
        out.push_str(&value);
        rest = &after[close + 1..];
    }
    out.push_str(rest);
    let key = sanitize(&out);
    Ok(if key.is_empty() {
        safekey.to_string()
    } else {
        key
    })
}

/// `family` from `Family, Given`, or the last word of `Given Family`.
fn family_name(author: &str) -> &str {
    match author.split_once(',') {
        Some((family, _)) => family.trim(),
        None => author.split_whitespace().last().unwrap_or(""),
    }
}

/// Words a key should not be built from: articles, and the prepositions and
/// conjunctions that open titles, in the three languages of most of the
/// historical physics literature.
const STOP_WORDS: &[&str] = &[
    "a", "an", "the", "of", "on", "in", "for", "to", "and", "with", "from", "at", "by", "via",
    "der", "die", "das", "des", "dem", "den", "ein", "eine", "einer", "eines", "zur", "zum", "und",
    "über", "uber", "von", "vom", "mit", "le", "la", "les", "un", "une", "du", "de", "sur", "et",
];

fn first_significant_word(title: &str) -> Option<&str> {
    // U+FFFD stays inside a word: Crossref's damaged `N\u{FFFD}herungsmethode`
    // (#608) should key as `nherungsmethode`, not `n`.
    title
        .split(|c: char| !c.is_alphanumeric() && c != '\u{FFFD}')
        .filter(|w| !w.is_empty())
        .find(|w| !STOP_WORDS.contains(&w.to_lowercase().as_str()))
}

/// Lower-case ASCII: NFD with combining marks dropped, plus the letters NFD
/// leaves whole.
fn fold(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.nfd() {
        match c {
            'ß' => out.push_str("ss"),
            'æ' | 'Æ' => out.push_str("ae"),
            'œ' | 'Œ' => out.push_str("oe"),
            'ø' | 'Ø' => out.push('o'),
            'ł' | 'Ł' => out.push('l'),
            'đ' | 'Đ' | 'ð' | 'Ð' => out.push('d'),
            'þ' | 'Þ' => out.push_str("th"),
            'ı' => out.push('i'),
            c if c.is_ascii_alphanumeric() => out.push(c.to_ascii_lowercase()),
            _ => {}
        }
    }
    out
}

/// Whether `key` is usable as-is as a BibTeX / biblatex citation key: non-empty
/// and made only of letters, digits and `-_:.+/`. An explicit `--key` is
/// checked with this and refused otherwise, rather than silently rewritten:
/// `smith, 2020` would otherwise end the key at the comma and shift every
/// field after it (review of #622).
#[must_use]
pub fn is_valid_key(key: &str) -> bool {
    !key.is_empty() && sanitize(key) == key
}

/// Drop what a BibTeX / biblatex key cannot carry. Letters, digits and
/// `-_:.+/` survive; whitespace, braces, commas, quotes and `%` do not.
fn sanitize(key: &str) -> String {
    key.chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ':' | '.' | '+' | '/'))
        .collect()
}

/// Suffix a key that already appears in `used` with `a`, `b`, ... (the
/// biblatex convention for the same author and year), and record it.
pub fn disambiguate(key: String, used: &mut std::collections::HashSet<String>) -> String {
    if used.insert(key.clone()) {
        return key;
    }
    let mut n = 0usize;
    loop {
        let candidate = format!("{key}{}", suffix(n));
        if used.insert(candidate.clone()) {
            return candidate;
        }
        n += 1;
    }
}

/// `a`..`z`, then `aa`, `ab`, ...
fn suffix(mut n: usize) -> String {
    let mut s = Vec::new();
    loop {
        // `n % 26` < 26, so the cast cannot truncate.
        #[allow(clippy::cast_possible_truncation)]
        s.push(b'a' + (n % 26) as u8);
        if n < 26 {
            break;
        }
        n = n / 26 - 1;
    }
    s.reverse();
    String::from_utf8(s).unwrap_or_default()
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    fn meta(title: &str, author: &str, year: Option<i32>) -> Metadata {
        Metadata {
            title: title.into(),
            authors: if author.is_empty() {
                Vec::new()
            } else {
                vec![author.into()]
            },
            year,
            ..Metadata::default()
        }
    }

    const T: &str = "{author}{year}{title_word}";

    /// The keys from the issue's own project.
    #[test]
    fn the_issue_examples_render_as_written_by_hand() {
        let fock = meta(
            "Näherungsmethode zur Lösung des quantenmechanischen Mehrkörperproblems",
            "Fock, V.",
            Some(1930),
        );
        assert_eq!(
            render_key(T, &fock, "sk").unwrap(),
            "fock1930naherungsmethode"
        );
        let slater = meta("The Theory of Complex Spectra", "Slater, J. C.", Some(1929));
        assert_eq!(render_key(T, &slater, "sk").unwrap(), "slater1929theory");
    }

    #[test]
    fn names_fold_to_ascii_and_given_family_order_is_understood() {
        let m = meta("Über die Quantenmechanik", "Erwin Schrödinger", Some(1926));
        assert_eq!(
            render_key(T, &m, "sk").unwrap(),
            "schrodinger1926quantenmechanik"
        );
        let m = meta("Ørsted's legacy", "Łukasiewicz, J.", None);
        assert_eq!(render_key(T, &m, "sk").unwrap(), "lukasiewiczorsted");
        let m = meta("Straße", "Weiß, A.", Some(2001));
        assert_eq!(
            render_key("{author}-{title_word}", &m, "sk").unwrap(),
            "weiss-strasse"
        );
    }

    #[test]
    fn a_replacement_character_does_not_cut_the_title_word_short() {
        let m = meta(
            "N\u{FFFD}herungsmethode zur L\u{FFFD}sung",
            "Fock, V.",
            Some(1930),
        );
        assert_eq!(render_key(T, &m, "sk").unwrap(), "fock1930nherungsmethode");
    }

    #[test]
    fn an_empty_render_falls_back_to_the_safekey() {
        let m = meta("", "", None);
        assert_eq!(render_key(T, &m, "doi_10.1_x").unwrap(), "doi_10.1_x");
        assert_eq!(
            render_key("{safekey}-v2", &m, "doi_10.1_x").unwrap(),
            "doi_10.1_x-v2"
        );
    }

    #[test]
    fn characters_a_key_cannot_carry_are_dropped() {
        let m = meta("x", "O'Brien, P.", Some(2000));
        assert_eq!(
            render_key("{author} {year},%", &m, "sk").unwrap(),
            "obrien2000"
        );
    }

    #[test]
    fn a_bad_template_is_refused_up_front() {
        assert_eq!(
            validate_template("{author}{yr}"),
            Err(KeyTemplateError::UnknownPlaceholder("yr".into()))
        );
        assert!(matches!(
            validate_template("{author"),
            Err(KeyTemplateError::Unclosed(_))
        ));
        assert!(validate_template(T).is_ok());
    }

    #[test]
    fn an_explicit_key_is_valid_only_if_bibtex_can_carry_it() {
        assert!(is_valid_key("fock1930"));
        assert!(is_valid_key("Fock:1930-a"));
        assert!(!is_valid_key("smith, 2020"));
        assert!(!is_valid_key("a}b"));
        assert!(!is_valid_key(""));
    }

    #[test]
    fn colliding_keys_get_biblatex_suffixes() {
        let mut used = std::collections::HashSet::new();
        assert_eq!(disambiguate("hartree1928".into(), &mut used), "hartree1928");
        assert_eq!(
            disambiguate("hartree1928".into(), &mut used),
            "hartree1928a"
        );
        assert_eq!(
            disambiguate("hartree1928".into(), &mut used),
            "hartree1928b"
        );
        assert_eq!(suffix(25), "z");
        assert_eq!(suffix(26), "aa");
    }
}
