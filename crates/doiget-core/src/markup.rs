//! Plain text from publisher metadata strings that carry inline markup.
//!
//! Crossref serves titles as the publisher deposited them, and several
//! publishers deposit JATS / MathML inline elements pretty-printed onto their
//! own lines (#609):
//!
//! ```text
//! "Recent developments in the P\n                    <scp>y</scp>\n                    SCF program package"
//! ```
//!
//! Stripping the tags is not enough. The newline-plus-indent runs around an
//! inline element were added by the pretty-printer, so they say nothing about
//! whether the author wrote a space there: `P<scp>y</scp>SCF` is one word,
//! `in <i>γ</i>-InSe` is two. [`plain_title`] decides each such run from its
//! neighbours; the rules are pinned against real deposits in the tests below.
//!
//! Whitespace that is not a pretty-printer run (no line break, or no tag
//! beside it) collapses to one space, and `$...$` is left alone: a
//! hand-written TeX title is content, which `doiget lint` already treats as
//! intentional.

/// `raw` with inline markup reduced to its text content, XML entities
/// decoded and whitespace collapsed. A string with no tag, entity or line
/// break is returned unchanged.
#[must_use]
pub fn plain_title(raw: &str) -> String {
    if !raw.contains(['<', '&', '\n', '\r']) {
        return raw.to_string();
    }
    let mut out = String::with_capacity(raw.len());
    // The previous non-blank text run, and what lies between it and the next.
    let mut prev: Option<(&str, usize)> = None;
    let mut gap = Gap::default();
    for chunk in split_on_tags(raw) {
        let (text, depth) = match chunk {
            Chunk::Tag => {
                gap.tag = true;
                continue;
            }
            Chunk::Text { text, depth } => (text, depth),
        };
        let body = text.trim();
        if body.is_empty() {
            gap.absorb(text);
            continue;
        }
        gap.absorb(&text[..text.len() - text.trim_start().len()]);
        if let Some((prev_body, prev_depth)) = prev {
            out.push_str(if gap.tag && gap.line_break {
                boundary(prev_body, prev_depth, body, depth)
            } else if gap.space {
                " "
            } else {
                ""
            });
        }
        out.push_str(&collapse(body));
        prev = Some((body, depth));
        gap = Gap::default();
        gap.absorb(&text[text.trim_end().len()..]);
    }
    decode_entities(&out)
}

/// Whether `s` carries a line break or an inline markup tag, i.e. whether
/// [`plain_title`] would rewrite more than entities. `doiget lint` uses this
/// to find entries pasted from an older `doiget cite` (#609).
#[must_use]
pub fn has_inline_markup(s: &str) -> bool {
    s.contains(['\n', '\r']) || split_on_tags(s).iter().any(|c| matches!(c, Chunk::Tag))
}

/// What separates two text runs.
#[derive(Default)]
struct Gap {
    tag: bool,
    space: bool,
    line_break: bool,
}

impl Gap {
    fn absorb(&mut self, ws: &str) {
        self.space |= !ws.is_empty();
        self.line_break |= ws.contains(['\n', '\r']);
    }
}

enum Chunk<'a> {
    Tag,
    /// Text between tags, and how many inline elements enclose it.
    Text {
        text: &'a str,
        depth: usize,
    },
}

/// Split into text runs and tags. A `<` that does not open a tag (`a < b`)
/// stays text.
fn split_on_tags(raw: &str) -> Vec<Chunk<'_>> {
    let mut chunks = Vec::new();
    let mut depth = 0usize;
    let mut start = 0;
    let mut i = 0;
    // `<` is ASCII, so every index this slices at is a char boundary.
    while let Some(off) = raw[i..].find('<') {
        let at = i + off;
        match tag_len(&raw[at..]) {
            Some(len) => {
                if start < at {
                    chunks.push(Chunk::Text {
                        text: &raw[start..at],
                        depth,
                    });
                }
                let tag = &raw[at..at + len];
                if tag.starts_with("</") {
                    depth = depth.saturating_sub(1);
                } else if !tag.ends_with("/>") {
                    depth += 1;
                }
                chunks.push(Chunk::Tag);
                i = at + len;
                start = i;
            }
            None => i = at + 1,
        }
    }
    if start < raw.len() {
        chunks.push(Chunk::Text {
            text: &raw[start..],
            depth,
        });
    }
    chunks
}

/// Inline elements publishers deposit in titles. An opening tag with one of
/// these names is markup even when its closing tag is elsewhere; any other
/// name counts only if a matching `</name` follows (see [`tag_len`]).
const INLINE_ELEMENTS: &[&str] = &[
    "b",
    "bold",
    "br",
    "em",
    "i",
    "inline-formula",
    "italic",
    "math",
    "monospace",
    "sc",
    "scp",
    "small",
    "span",
    "strong",
    "sub",
    "sup",
    "tex-math",
    "tt",
    "u",
    "underline",
];

/// Length of the tag at the start of `s`, if it is one.
///
/// A real grammar rather than "`<` up to the next `>`": the name is
/// `[A-Za-z][A-Za-z0-9:._-]*` and anything before `>` must be
/// `name="value"` attributes. The loose rule treated `T<Tc in samples with
/// applied field H>Hc2` as one tag and stored `THc2` (review of #618). An
/// opening tag must also be plausible as markup: a known inline element, a
/// namespaced one (`mml:mi`, `jats:italic`), or one whose `</name` follows --
/// so `x<y>z` stays text.
fn tag_len(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut i = 1; // past '<'
    let closing = bytes.get(i) == Some(&b'/');
    if closing {
        i += 1;
    }
    let name_start = i;
    if !bytes.get(i)?.is_ascii_alphabetic() {
        return None;
    }
    while bytes
        .get(i)
        .is_some_and(|b| b.is_ascii_alphanumeric() || matches!(b, b':' | b'.' | b'_' | b'-'))
    {
        i += 1;
    }
    let name = &s[name_start..i];
    let mut self_closing = false;
    loop {
        while bytes.get(i).is_some_and(u8::is_ascii_whitespace) {
            i += 1;
        }
        match bytes.get(i)? {
            b'>' => {
                i += 1;
                break;
            }
            b'/' if !closing && bytes.get(i + 1) == Some(&b'>') => {
                self_closing = true;
                i += 2;
                break;
            }
            b if !closing && (b.is_ascii_alphabetic() || *b == b'_') => {
                // attribute: name = "value" | 'value'
                while bytes.get(i).is_some_and(|b| {
                    b.is_ascii_alphanumeric() || matches!(b, b':' | b'.' | b'_' | b'-')
                }) {
                    i += 1;
                }
                while bytes.get(i).is_some_and(u8::is_ascii_whitespace) {
                    i += 1;
                }
                if bytes.get(i) != Some(&b'=') {
                    return None;
                }
                i += 1;
                while bytes.get(i).is_some_and(u8::is_ascii_whitespace) {
                    i += 1;
                }
                let quote = *bytes.get(i)?;
                if !matches!(quote, b'"' | b'\'') {
                    return None;
                }
                let close = s[i + 1..].find(quote as char)?;
                if s[i + 1..i + 1 + close].contains('<') {
                    return None;
                }
                i += close + 2;
            }
            _ => return None,
        }
    }
    let known = INLINE_ELEMENTS.contains(&name.to_ascii_lowercase().as_str()) || name.contains(':');
    let plausible = closing || self_closing || known || s[i..].contains(&format!("</{name}"));
    plausible.then_some(i)
}

fn collapse(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The separator for a pretty-printer run between `left` and `right` that
/// crosses a tag.
///
/// The side enclosed by more elements is the element's content. A word
/// inside it (`<i>Escherichia coli</i>`, `<b>31</b>`) is spaced from a
/// neighbouring word. A symbol (`<i>x</i>`, `<scp>y</scp>`, `<i>β</i>`) is
/// joined to its neighbour unless that neighbour is a lower-case word,
/// sentence punctuation before it, or a relational operator.
fn boundary(left: &str, left_depth: usize, right: &str, right_depth: usize) -> &'static str {
    if left_depth == right_depth {
        // `<i>L</i>\n<i>p</i>`: two elements and nothing outside to go by.
        return "";
    }
    let (inner, outer, outer_is_left) = if right_depth > left_depth {
        (right, left, true)
    } else {
        (left, right, false)
    };
    let edge = if outer_is_left {
        outer.chars().next_back()
    } else {
        outer.chars().next()
    };
    let Some(edge) = edge else { return "" };
    let is_word = inner.chars().filter(|c| c.is_alphanumeric()).count() >= 2;
    // `&` is the start of an escaped `&gt;` / `&lt;`, decoded after this.
    let operator = matches!(edge, '=' | '<' | '>' | '≤' | '≥' | '±' | '&');
    let sentence_punct = outer_is_left && matches!(edge, '.' | ',' | ';' | ':');
    let space = if is_word {
        edge.is_alphanumeric() || sentence_punct
    } else {
        lowercase_word_at_edge(outer, outer_is_left) || sentence_punct || operator
    };
    if space {
        " "
    } else {
        ""
    }
}

/// Whether the letters at the inner edge of `outer` form a lower-case word
/// of two or more letters (`in`, `noise`, the `grown` of `MOVPE-grown`).
fn lowercase_word_at_edge(outer: &str, at_end: bool) -> bool {
    let is_lower_word =
        |letters: &[char]| letters.len() >= 2 && letters.iter().all(|c| c.is_lowercase());
    if at_end {
        let letters: Vec<char> = outer
            .chars()
            .rev()
            .take_while(|c| c.is_alphabetic())
            .collect();
        is_lower_word(&letters)
    } else {
        let letters: Vec<char> = outer.chars().take_while(|c| c.is_alphabetic()).collect();
        is_lower_word(&letters)
    }
}

/// Decode the XML entities a deposit carries. Crossref sometimes escapes a
/// deposit's own entity a second time (`&amp;gt;` for `>`), so a string that
/// contained `&amp;` is decoded twice.
fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let once = decode_once(s);
    if s.contains("&amp;") && once.contains('&') {
        decode_once(&once)
    } else {
        once
    }
}

fn decode_once(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(pos) = rest.find('&') {
        out.push_str(&rest[..pos]);
        let tail = &rest[pos..];
        match entity(tail) {
            Some((c, len)) => {
                out.push(c);
                rest = &tail[len..];
            }
            None => {
                out.push('&');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// The character and byte length of the entity at the start of `s`.
fn entity(s: &str) -> Option<(char, usize)> {
    let end = s.find(';').filter(|&end| end <= 10)?;
    let name = &s[1..end];
    let c = match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        _ => {
            let code = match name.strip_prefix("#x").or_else(|| name.strip_prefix("#X")) {
                Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                None => name.strip_prefix('#')?.parse().ok()?,
            };
            char::from_u32(code)?
        }
    };
    Some((c, end + 1))
}

#[cfg(test)]
mod tests {
    use super::plain_title;

    /// Real Crossref deposits (AIP, prefix `10.1063`, sampled 2026-09-29),
    /// shortened, with the pretty-printer's indentation kept verbatim.
    #[test]
    fn pretty_printed_inline_markup_reads_as_the_title_the_author_wrote() {
        let ind = "\n                    ";
        let cases = [
            (
                format!("Recent developments in the P{ind}<scp>y</scp>{ind}SCF program package"),
                "Recent developments in the PySCF program package",
            ),
            (
                format!("control of [Pr1−{ind}<i>x</i>{ind}Ca{ind}<i>x</i>{ind}MnO3/SrTiO3]15 superlattices"),
                "control of [Pr1−xCaxMnO3/SrTiO3]15 superlattices",
            ),
            (
                format!("Thermal transport in{ind}<b>{ind}  <i>γ</i>{ind}</b>{ind}-InSe: Bulk single crystals"),
                "Thermal transport in γ-InSe: Bulk single crystals",
            ),
            (
                format!("Erratum: [Phys. Plasmas{ind}<b>31</b>{ind}, 042509 (2024)]"),
                "Erratum: [Phys. Plasmas 31, 042509 (2024)]",
            ),
            (
                format!("[Appl. Phys. Lett.{ind}<b>117</b>{ind}, 092101 (2020)]"),
                "[Appl. Phys. Lett. 117, 092101 (2020)]",
            ),
            (format!("1/{ind}<i>f</i>{ind}noise model"), "1/f noise model"),
            (
                format!("Approximate Symmetry in{ind}<i>Z’</i>{ind}&amp;gt; 1 Structures"),
                "Approximate Symmetry in Z’ > 1 Structures",
            ),
            (
                format!("coupling in 2{ind}<i>H</i>{ind}-VSe2 bilayer"),
                "coupling in 2H-VSe2 bilayer",
            ),
            (
                format!("their isomers{ind}<i>cis</i>{ind}- and{ind}<i>trans</i>{ind}-HNCHO"),
                "their isomers cis- and trans-HNCHO",
            ),
            (
                format!("insights into{ind}<i>Escherichia coli</i>{ind}O32:H37 contact"),
                "insights into Escherichia coli O32:H37 contact",
            ),
            (
                format!("non-commutative{ind}<i>L</i>{ind}<i>p</i>{ind}-spaces"),
                "non-commutative Lp-spaces",
            ),
            (
                format!("MOVPE-grown <b>{ind} <i>β</i>{ind}</b>-Ga2O3 films"),
                "MOVPE-grown β-Ga2O3 films",
            ),
            (
                format!("in Ce2(Cu1<b>−</b>{ind}<i>x</i>Ni<i>x</i>)2In"),
                "in Ce2(Cu1−xNix)2In",
            ),
            (
                format!("operators with <i>L</i>{ind}<i>p</i> potentials"),
                "operators with Lp potentials",
            ),
        ];
        for (raw, want) in cases {
            assert_eq!(plain_title(&raw), want, "raw: {raw:?}");
        }
    }

    /// Review of #618: a letter after `<` is an inequality in a physics
    /// title far more often than a tag, and the text up to some later `>`
    /// must never be taken for one.
    #[test]
    fn an_inequality_is_not_mistaken_for_a_tag() {
        for s in [
            "Resistivity anomaly for T<Tc in samples with applied field H>Hc2",
            "Comparing groups where n<N states and m>M bands coexist",
            "a<b and c>d",
            "the regime x<y>z",
            "for L<M, the <unclosed",
        ] {
            assert_eq!(plain_title(s), s);
            assert!(!super::has_inline_markup(s), "{s}");
        }
        // Real markup next to an inequality is still reduced.
        assert_eq!(
            plain_title("T<Tc for <i>x</i> > 0 and <span class=\"x\">y</span>"),
            "T<Tc for x > 0 and y"
        );
        assert_eq!(plain_title("a <jats:italic>b</jats:italic> c"), "a b c");
        assert_eq!(plain_title("an <unknown-el>x</unknown-el> y"), "an x y");
    }

    #[test]
    fn plain_strings_and_tex_titles_pass_through_unchanged() {
        for s in [
            "Density matrix formulation for quantum renormalization groups",
            "The $T\\bar{T}$ deformation",
            "  two  spaces  ",
        ] {
            assert_eq!(plain_title(s), s);
        }
        assert_eq!(plain_title("a < b and c > d"), "a < b and c > d");
    }

    #[test]
    fn ordinary_spacing_around_inline_elements_is_kept() {
        assert_eq!(
            plain_title("The <i>ab initio</i> method for H<sub>2</sub>O"),
            "The ab initio method for H2O"
        );
        assert_eq!(plain_title("Line one\n  line two"), "Line one line two");
    }

    #[test]
    fn entities_decode_and_a_bare_ampersand_survives() {
        assert_eq!(
            plain_title("Tom &amp; Jerry &#x3B2; &#946;"),
            "Tom & Jerry β β"
        );
        assert_eq!(plain_title("R&D &unknown; &"), "R&D &unknown; &");
    }

    #[test]
    fn markup_detection_ignores_bare_comparisons_and_tex() {
        use super::has_inline_markup;
        assert!(has_inline_markup(
            "the P\n    <scp>y</scp>\n    SCF package"
        ));
        assert!(has_inline_markup("Spin-<i>S</i> chains"));
        assert!(has_inline_markup("Line one\nline two"));
        assert!(!has_inline_markup("Regime a < b holds"));
        assert!(!has_inline_markup("The $T\\bar{T}$ deformation"));
    }

    #[test]
    fn mathml_reduces_to_its_text_content() {
        assert_eq!(
            plain_title(
                "Spin <mml:math><mml:msub><mml:mi>S</mml:mi><mml:mn>1</mml:mn></mml:msub></mml:math> chains"
            ),
            "Spin S1 chains"
        );
    }
}
