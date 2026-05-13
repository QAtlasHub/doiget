+++
title = "Safekey algorithm"
description = "Map any DOI or arXiv id to a deterministic, cross-platform, bit-identical filesystem-safe"
weight = 180
+++

# safekey algorithm

> **Status: NORMATIVE (shared spec).** Binding for both doiget and BiblioFetch.jl.
> Any change requires a coordinated ADR and an update to the reference test vectors at
> `tests/fixtures/safekey/vectors.json`.

## 1. Goal

Map any DOI or arXiv id to a deterministic, cross-platform, bit-identical filesystem-safe
key. The same input must produce the same `safekey` output on Linux, macOS, and Windows,
and across both Rust and Julia implementations.

## 2. Constraints

- Path-safe: no `/`, `\`, `..`, `:`, control chars, or other characters problematic on any
  major filesystem.
- Bounded length: ≤ 200 characters (filesystem limits).
- Collision-resistant: two distinct refs must produce two distinct safekeys with high
  probability (cryptographic, not just statistical).
- Visually traceable: a human reading a safekey should be able to recognize the original
  ref class (DOI vs arXiv) and significant prefix.
- Deterministic: no clock, no random, no host-dependent state.

## 3. Algorithm (NORMATIVE)

```rust
pub fn safekey(ref_: &Ref) -> Safekey {
    // Step 0: normalize. `Doi::as_str()` and `ArxivId::as_str()` return the
    // identifier WITHOUT a `doi:` / `arxiv:` URI scheme prefix — `Ref::parse`
    // is responsible for stripping those at construction time. The vectors
    // in §5 ("doi:..." / "arxiv:..." in the input column) document the
    // user-facing input form; by the time we reach `safekey`, the scheme
    // has already been removed. The single `doi_` / `arxiv_` prefix is
    // added here, exactly once, in step 0.
    let raw = match ref_ {
        Ref::Doi(d)   => format!("doi_{}",   d.as_str()),
        Ref::Arxiv(a) => format!("arxiv_{}", a.as_str()),
    };

    // Step 1: replace unsafe chars with '_'
    let escaped: String = raw.chars().map(|c| match c {
        'A'..='Z' | 'a'..='z' | '0'..='9' | '.' | '-' | '_' => c,
        _ => '_',
    }).collect();

    // Step 2: collapse consecutive '_' runs to a single '_'
    let collapsed = collapse_underscores(&escaped);

    // Step 3: trim leading/trailing '_'
    let trimmed = collapsed.trim_matches('_');

    // Step 4: length-bound. If > 192 chars, take prefix(192) + '_' + 8-hex SHA256
    if trimmed.len() > 192 {
        let hash = hex::encode(&sha2::Sha256::digest(raw.as_bytes())[..4]);
        format!("{}_{}", &trimmed[..192], hash)
    } else {
        trimmed.to_string()
    }
}

fn collapse_underscores(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_was_underscore = false;
    for c in s.chars() {
        if c == '_' {
            if !last_was_underscore { out.push('_'); }
            last_was_underscore = true;
        } else {
            out.push(c);
            last_was_underscore = false;
        }
    }
    out
}
```

## 4. Equivalent Julia reference

```julia
const SAFE_CHAR_RE = r"[A-Za-z0-9._\-]"

function safekey(ref::AbstractString)::String
    raw = startswith(ref, "10.") ? "doi_$ref" : "arxiv_$ref"
    escaped = replace(raw, r"[^A-Za-z0-9._\-]" => "_")
    collapsed = replace(escaped, r"_+" => "_")
    trimmed = strip(collapsed, '_')
    if length(trimmed) > 192
        h = bytes2hex(SHA.sha256(codeunits(raw))[1:4])
        return string(trimmed[1:192], "_", h)
    end
    return String(trimmed)
end
```

The two implementations MUST produce bit-identical output for every entry in the
reference vector set.

### 3.1 Filename derivation inputs (NORMATIVE)

The safekey is derived **solely from the canonical `Ref` string** (a validated
DOI or arXiv id, both already path-traversal-checked at parse time). doiget
MUST NOT use any of the following as filename input:

- HTTP `Content-Disposition` header (`filename=...` or `filename*=...`).
- Redirect URL path or any URL component returned by the network.
- Server-suggested filename (any header carrying a string the publisher
  proposes as a download name).
- Any byte stream from the network.

This guarantees that an attacker controlling a redirect target or a response
header cannot influence the on-disk path. The derivation is a pure function
of `Ref`, computed before the first network byte is sent.

This rule is the spec-side counterpart to the audit-trail
`canonical_digest` introduced by [ADR-0021](DECISIONS/0021-canonical-tuple-identity.md):
the on-disk identity stays keyed on `Ref` (so BiblioFetch.jl round-trip
keeps working — see §7), while the *audit* identity gains the resolver
profile.

## 5. Reference test vectors (sample)

Full set: `tests/fixtures/safekey/vectors.json` (100 entries, NORMATIVE).

Selected:

| Input ref | safekey output |
|---|---|
| `doi:10.1234/example` | `doi_10.1234_example` |
| `doi:10.1103/PhysRevLett.130.200601` | `doi_10.1103_PhysRevLett.130.200601` |
| `doi:10.1016/S0370-1573(98)00122-3` | `doi_10.1016_S0370-1573_98_00122-3` |
| `doi:10.1234/foo bar` | `doi_10.1234_foo_bar` |
| `doi:10.1234/foo  bar` | `doi_10.1234_foo_bar` (consecutive `_` collapsed) |
| `doi:10.1234/_leading` | `doi_10.1234_leading` (trim leading `_`) |
| `arxiv:2401.12345` | `arxiv_2401.12345` |
| `arxiv:2401.12345v2` | `arxiv_2401.12345v2` |
| `arxiv:cond-mat/9501001` | `arxiv_cond-mat_9501001` |

For very long refs (e.g. 250-char DOI suffix from a malformed source), the safekey is
truncated at 192 chars and an 8-hex SHA-256 prefix of the original is appended:

```
doi:10.1234/aaaaa...aaaaa(220 chars)  →  doi_10.1234_aaaaa...aaaaa(192 chars)_DEADBEEF
```

## 6. Property tests

```rust
proptest! {
    #[test]
    fn safekey_is_path_safe(ref_ in arb_ref()) {
        let key = safekey(&ref_);
        prop_assert!(!key.contains(".."));
        prop_assert!(!key.contains('/'));
        prop_assert!(!key.contains('\\'));
        prop_assert!(key.chars().all(|c|
            c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_'
        ));
        prop_assert!(!key.starts_with('_'));
        prop_assert!(!key.ends_with('_'));
        prop_assert!(key.len() <= 201);    // 192 + '_' + 8
    }

    #[test]
    fn safekey_is_deterministic(ref_ in arb_ref()) {
        prop_assert_eq!(safekey(&ref_), safekey(&ref_));
    }

    #[test]
    fn safekey_distinct_refs_distinct_keys(r1 in arb_ref(), r2 in arb_ref()) {
        prop_assume!(r1.canonical() != r2.canonical());
        prop_assert_ne!(safekey(&r1), safekey(&r2));
    }
}
```

## 7. Backwards compatibility note

Older BiblioFetch.jl versions used a slightly different algorithm (the H2 issue). When
this NORMATIVE spec is adopted, BiblioFetch.jl will publish a migration tool that
re-keys existing entries to the new spec. doiget will only ever read or write entries
produced by the spec defined in this document.
