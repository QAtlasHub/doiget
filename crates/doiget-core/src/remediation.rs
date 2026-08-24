//! Machine-readable remediation hints for a denial (#459).
//!
//! [`DenialContext`] (ADR-0023) says what was refused. It does not say what
//! to do about it, and until now the only place that did was CLI text —
//! the `= help:` block from #443. An agent driving doiget over MCP, or a
//! CI job reading `batch --json`, got the refusal and nothing else.
//!
//! That gap has a concrete cost. A hybrid-OA paper whose only free copy
//! sits in a university repository is refused with
//! `redirect_not_in_allowlist`, and is fetchable after **one line** of
//! config — either the host, or the `trust_academic_repos` flag that
//! already knows about `*.ac.uk`. An agent that cannot find that line
//! reports "this paper is not available", which is false.
//!
//! So the hints live here, in core, computed once and rendered by every
//! surface. The CLI's `= help:` block, the MCP envelope and the
//! `batch --json` record all read the same [`Remediation`] list; #454 is
//! the recent lesson about what happens when two surfaces each keep their
//! own copy of a rule.

use serde::Serialize;

use crate::user_extension::{academic_repo_hosts, oa_registry_hosts};
use crate::{DenialContext, DenialReason};

/// What kind of change would lift this denial.
///
/// Deliberately a closed set, and deliberately **not** collapsed into one
/// "here is a string to paste": the two kinds do different things to the
/// trusted surface, and a caller has to be able to tell them apart. Adding
/// a host trusts one publisher. Setting a trust flag trusts a curated
/// class of hosts (ADR-0028). An agent choosing between them is making a
/// policy decision on the user's behalf and should be able to see that.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum RemediationKind {
    /// Add a host pattern under `[[network.additional_hosts]]`.
    AdditionalHost,
    /// Set a `[network]` boolean that trusts a curated host class.
    TrustFlag,
}

/// One suggested change, with the reason it is being suggested.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct Remediation {
    /// Which kind of change this is.
    pub kind: RemediationKind,
    /// The host pattern to add, or the flag name to set.
    pub value: String,
    /// Why this entry is offered, phrased for a human reading a log.
    pub note: String,
}

/// Suggestions that would lift `denial`, most specific first.
///
/// Empty for reasons with no configuration channel — a size cap or a
/// capability gate is not fixed by editing the allowlist, and offering a
/// host to add there would be actively misleading.
#[must_use]
pub fn for_denial(denial: &DenialContext) -> Vec<Remediation> {
    let mut out = Vec::new();
    // Only the host-allowlist reasons have a config channel.
    // `InsecureScheme` is deliberately excluded: the fix for an `http://`
    // redirect is not to trust the host, and ADR-0027 offers no opt-out.
    if !matches!(
        denial.reason,
        DenialReason::RedirectNotInAllowlist | DenialReason::HostInBlockList
    ) {
        return out;
    }
    let Some(host) = denial.attempted.as_deref() else {
        return out;
    };

    for (pattern, why) in widening_suggestions(host) {
        out.push(Remediation {
            kind: RemediationKind::AdditionalHost,
            value: pattern,
            note: why.to_string(),
        });
    }

    // The flag comes last but is usually the better answer when it
    // applies: it is one line, it is curated, and it covers the next
    // repository as well as this one.
    if let Some((flag, pattern, why)) = trust_flag_for_host(host) {
        out.push(Remediation {
            kind: RemediationKind::TrustFlag,
            value: flag.to_string(),
            note: format!("{host} matches {pattern} — {why}"),
        });
    }
    out
}

/// The `[network]` trust flag that already covers `host`, if any.
///
/// Returns the flag name, the curated pattern that matched, and that
/// pattern's own note, so the caller can say *why* the flag applies rather
/// than asserting that it does.
#[must_use]
pub fn trust_flag_for_host(host: &str) -> Option<(&'static str, String, String)> {
    let host_lc = host.to_ascii_lowercase();
    for (flag, hosts) in [
        ("trust_academic_repos", academic_repo_hosts()),
        ("trust_oa_registries", oa_registry_hosts()),
    ] {
        for h in hosts {
            if pattern_matches(&host_lc, h.host.as_str()) {
                return Some((
                    flag,
                    h.host.as_str().to_string(),
                    h.note.unwrap_or_else(|| "a curated host class".to_string()),
                ));
            }
        }
    }
    None
}

/// `docs/REDIRECT_ALLOWLIST.md` §2.2 matching, against an already-lowercased
/// host.
///
/// The same rule `http::SourceAllowlist::matches` applies, kept local
/// rather than building a throwaway `SourceAllowlist` per call. Both are
/// three lines and neither is likely to change — but if §2.2 ever does,
/// this must change with it.
fn pattern_matches(host_lc: &str, pattern: &str) -> bool {
    let pat_lc = pattern.to_ascii_lowercase();
    match pat_lc.strip_prefix("*.") {
        Some(suffix) => host_lc == suffix || host_lc.ends_with(&format!(".{suffix}")),
        None => host_lc == pat_lc,
    }
}

/// Widening suggestions for a refused host, most specific first (#443).
///
/// The `= help:` block used to name only the hop that was just refused, so
/// a publisher whose PDF sits behind `www.x.org -> pubs.x.org` cost the
/// user one edit-run cycle per hop. Naming the registrable domain too ends
/// it in one.
///
/// It is also the policy-consistent suggestion. The built-in allowlist is
/// written almost entirely as registrable-domain wildcards
/// (`*.springer.com`, `*.wiley.com`, `*.aps.org`), and ADR-0027's stated
/// mitigation for widening the trusted surface is exactly that they are
/// "bounded registrable-domain wildcards". Suggesting a bare FQDN was both
/// more work for the user and narrower than the convention the project
/// applies to itself. The apex is offered alongside the wildcard because a
/// single-suffix wildcard does not match it — the reason the built-in list
/// already carries both forms for `doaj.org`, `arxiv.org` and friends.
///
/// Conservative by construction: a suggestion is emitted only when the
/// derived parent is clearly registrable. Getting this exactly right needs
/// the public suffix list, and a wrong guess here is not cosmetic — it
/// would invite the user to trust `*.co.uk`.
///
/// Moved here from `doiget-cli` in #459 so the MCP and `batch --json`
/// surfaces get the same suggestions as the CLI rather than a second
/// implementation of them.
#[must_use]
pub fn widening_suggestions(host: &str) -> Vec<(String, &'static str)> {
    let mut out = vec![(host.to_string(), "this hop only")];
    let labels: Vec<&str> = host.split('.').filter(|l| !l.is_empty()).collect();
    if labels.len() < 2 || looks_like_public_suffix(&labels) {
        return out;
    }
    if labels.len() == 2 {
        // Already the apex: the useful widening is its subdomains.
        out.push((format!("*.{host}"), "and its subdomains"));
        return out;
    }
    let parent_labels = &labels[1..];
    if looks_like_public_suffix(parent_labels) {
        return out;
    }
    let parent = parent_labels.join(".");
    out.push((format!("*.{parent}"), "the whole publisher"));
    out.push((parent, "apex too (a wildcard does not match it)"));
    out
}

/// Whether `labels` looks like a public suffix rather than something a
/// single organisation registered.
///
/// Deliberately crude and deliberately over-cautious: the cost of a false
/// positive is one missing suggestion, and the cost of a false negative is
/// telling a user to trust every domain under `co.uk`.
fn looks_like_public_suffix(labels: &[&str]) -> bool {
    match labels {
        // A bare TLD.
        [_] => true,
        // `co.uk`, `ac.jp`, `com.au`, … — a known second level under a
        // two-letter ccTLD. `example.co.uk` has three labels and is NOT
        // caught here, which is correct.
        [sld, tld] => {
            tld.len() == 2
                && matches!(
                    *sld,
                    "co" | "com"
                        | "ne"
                        | "net"
                        | "or"
                        | "org"
                        | "ac"
                        | "edu"
                        | "gov"
                        | "go"
                        | "gr"
                        | "lg"
                        | "mil"
                        | "id"
                        | "in"
                )
        }
        _ => false,
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    fn denial(host: &str) -> DenialContext {
        DenialContext {
            reason: DenialReason::RedirectNotInAllowlist,
            source: Some("oa-publisher".to_string()),
            attempted: Some(host.to_string()),
            expected: Some(vec!["*.arxiv.org".to_string()]),
            hop_index: None,
            cap: None,
            actual: None,
        }
    }

    /// The exact case that motivated #459: a real fetch, refused, that
    /// `trust_academic_repos` fixes in one line. The flag has to be in the
    /// output or an agent cannot find it.
    #[test]
    fn a_university_repository_offers_the_trust_flag_as_well_as_the_host() {
        let r = for_denial(&denial("strathprints.strath.ac.uk"));
        let hosts: Vec<&str> = r
            .iter()
            .filter(|x| x.kind == RemediationKind::AdditionalHost)
            .map(|x| x.value.as_str())
            .collect();
        assert_eq!(
            hosts,
            vec![
                "strathprints.strath.ac.uk",
                "*.strath.ac.uk",
                "strath.ac.uk"
            ],
            "the #443 widening set, unchanged by the move"
        );

        let flag = r
            .iter()
            .find(|x| x.kind == RemediationKind::TrustFlag)
            .expect("an *.ac.uk host must surface trust_academic_repos");
        assert_eq!(flag.value, "trust_academic_repos");
        assert!(
            flag.note.contains("*.ac.uk"),
            "say WHICH curated pattern matched, or the flag looks like a guess: {}",
            flag.note
        );
    }

    /// A publisher host has no curated class, so only the host route is
    /// offered. Suggesting a trust flag that would not have helped is the
    /// same failure as #442's "go find an API key" — it sends the user
    /// after the wrong fix.
    #[test]
    fn a_publisher_host_offers_no_trust_flag() {
        let r = for_denial(&denial("pubs.ams.org"));
        assert!(
            r.iter().all(|x| x.kind == RemediationKind::AdditionalHost),
            "no curated class covers ams.org: {r:?}"
        );
        assert_eq!(
            r.iter().map(|x| x.value.as_str()).collect::<Vec<_>>(),
            vec!["pubs.ams.org", "*.ams.org", "ams.org"]
        );
    }

    /// Reasons with no configuration channel must offer nothing. An
    /// oversized body is not fixed by trusting the host, and saying so
    /// would be worse than silence.
    #[test]
    fn a_reason_with_no_config_channel_suggests_nothing() {
        for reason in [
            DenialReason::SizeCapExceeded,
            DenialReason::InsecureScheme,
            DenialReason::CapabilityNotGranted,
        ] {
            let mut d = denial("example.org");
            d.reason = reason;
            assert!(
                for_denial(&d).is_empty(),
                "{reason:?} has no allowlist channel, so it must suggest nothing"
            );
        }
    }

    /// Guarding the one mistake that is not cosmetic.
    #[test]
    fn a_public_suffix_is_never_offered_for_trust() {
        for host in ["example.co.uk", "example.ac.jp", "foo.com", "localhost"] {
            for (pattern, _) in widening_suggestions(host) {
                assert!(
                    !matches!(pattern.as_str(), "*.co.uk" | "co.uk" | "*.ac.jp" | "ac.jp"),
                    "{host} must never suggest trusting a public suffix, got {pattern}"
                );
            }
        }
    }
}
