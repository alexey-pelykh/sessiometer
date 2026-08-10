// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! The FRAMING guard's shared vocabulary (issues #160, #542, #918) — one banned list, one
//! scanner, reachable from every operator-facing surface's tests.
//!
//! # Why this module exists at all
//!
//! The vocabulary and its scanner were born inside `src/stats.rs`'s `#[cfg(test)] mod tests`,
//! private to it, because the neutral summary band (issue #160) was the only surface being
//! guarded. That privacy quietly became a correctness problem. Issue #885's AC4 asserted "the CI
//! grep test covers help lines"; issue #918 measured the claim and found **no such coverage** —
//! the scanner could not be reached from `src/cli.rs` at all, so `--help` was an unchecked
//! surface that read as a checked one. A guard that cannot fire is worse than no guard.
//!
//! Hoisting the list here is the split issue #918 records: the vocabulary is not a stats
//! implementation detail, it is the FRAMING contract every operator-facing prose surface owes.
//!
//! # One list, two audiences
//!
//! [`BANNED_TOKENS`] stays whole and stats keeps scanning against all of it — nothing was
//! subtracted from what `stats.rs` saw before. `--help` scans a DERIVED subset,
//! [`help_banned_tokens`], which is [`BANNED_TOKENS`] minus [`HELP_EXEMPT_TOKENS`]. Deriving the
//! subset rather than hand-copying it is the point: a second hand-maintained list would drift
//! from the first, which is the failure mode this module was created to end.
//!
//! Both audiences share ONE tokenizer ([`scan_with`]), so the help guard cannot silently disagree
//! with the stats guard about what counts as a word.
//!
//! Compiled only under `cfg(test)` — this is the gates' vocabulary, and nothing in the shipping
//! binary reads it.

/// The editorialising vocabulary the neutral summary band (issue #160) — and every surface this
/// guard scans — must NEVER contain: a value judgement (`healthy`, `danger`), an acquisitive
/// imperative (`add`, `upgrade`, `buy`), a recommendation (`should`, `recommend`), or ALARMIST
/// projection FRAMING (`forecast`, `imminent`, `soon`). CENTRAL + explicit so the guard stays
/// maintainable: one list, one scanner, extended in a single place.
///
/// Boundary (issue #542, ADR-0020) — these ban the FRAMING, not the FACT. A neutrally framed
/// velocity + runway readout — a `%/min` rate, an approximate time-to-trigger or days-of-runway
/// phrased as an observation (`~4h to trigger`, `~3 days at current rate`) — is PERMITTED: it
/// uses none of this vocabulary. What stays banned is the acquisitive CALL (a purchase prompt)
/// and the alarmist projection words, never a head-room number. Neutral MAGNITUDE words the wire
/// legitimately uses (`idle`/`low`/`moderate`/`high`/`at_cap`) are likewise absent — they
/// describe, they do not editorialise.
///
/// The four groups below are load-bearing, not decorative. A test named for keeping every
/// editorial group armed asserts each one still has a member after [`HELP_EXEMPT_TOKENS`] is
/// subtracted, so an over-wide exemption cannot hollow the help guard out into a scan of nothing.
pub const BANNED_TOKENS: &[&str] = &[
    // Imperatives / recommended actions (issue #160: "add / buy / upgrade / cancel /
    // bypass / need more").
    "add",
    "buy",
    "upgrade",
    "cancel",
    "bypass",
    "need",
    "purchase",
    "remove",
    "disable",
    "enable",
    "fix",
    "avoid",
    "reduce",
    "increase",
    "throttle",
    "rotate",
    // Value judgements (caller: "healthy / at risk / warning / danger / good / bad").
    "healthy",
    "unhealthy",
    "risk",
    "risky",
    "warning",
    "warn",
    "danger",
    "dangerous",
    "good",
    "bad",
    "critical",
    "severe",
    "poor",
    "safe",
    "unsafe",
    "optimal",
    // Recommendation framing (caller: "you should").
    "should",
    "must",
    "ought",
    "recommend",
    "recommended",
    "recommendation",
    "suggest",
    "suggestion",
    "consider",
    "advise",
    "advice",
    // Alarmist / editorialising projection FRAMING. A neutral numeric runway is a
    // permitted FACT (issue #542, ADR-0020); these ban the ALARM ("forecast", "imminent",
    // "soon"), not the head-room number ("~4h to trigger").
    "forecast",
    "predict",
    "prediction",
    "projected",
    "projection",
    "anticipate",
    "imminent",
    "soon",
];

/// Acquisitive purchase-CALLS that span two adjacent words, so the single-token scan above
/// misses them (issue #542): the imperative-free `top up` / `get more` a purchase prompt
/// reaches for once `buy`/`add`/`upgrade` are gone. The discriminator the guard draws is the
/// CALL to acquire, never the head-room fact — `runs out in ~4h` is permitted, `runs out —
/// top up` is not. Kept SHORT and matched on WORD boundaries (adjacent tokens, not a raw
/// substring) so a neutral render never false-trips (`laptop update` is not `top up`).
///
/// Scanned in FULL by both audiences: an acquisitive purchase-call has no more business in
/// `--help` than in a stats band, so nothing here is exempted.
pub const BANNED_PHRASES: &[&str] = &["top up", "get more"];

/// The tokens `--help` alone is excused from, and the ONLY difference between the two scans
/// (issue #918). Every one is a MECHANICAL OPERATION verb — it names something the tool does to
/// its own state — and every one is measured, not guessed: these are exactly the members of
/// [`BANNED_TOKENS`] that appear in the shipped help today.
///
/// - `disable`, `enable`, `remove` — this CLI's own COMMAND NAMES. `ROOT_USAGE` lists all three
///   in its verb table and each owns a `<VERB>_USAGE` block that must spell its own name.
/// - `remove` again, and `add`, as ordinary mechanical verbs a usage line cannot avoid:
///   `SERVICE_USAGE` says "unload + remove that LaunchAgent", `STATUS_USAGE` says
///   "-v, --verbose  add each account's access-token expiry under the table".
///
/// That second bullet is why issue #918's option 2 — "one list plus an explicit COMMAND-NAME
/// exemption set" — was rejected on the evidence rather than on taste: `add` is not a command in
/// this CLI, and `remove` in `SERVICE_USAGE` is not being used as one, so a command-name
/// exemption set would not have sufficed. The real boundary is semantic (mechanical verb vs
/// editorial framing), not lexical.
///
/// Exempting these does NOT hand `--help` a licence to editorialise, and the tests say so rather
/// than trusting the argument: every exempt token is still caught on the stats side
/// (`help_exemption_does_not_weaken_the_central_guard`), and a genuine call to action in help
/// still trips a DIFFERENT group — "you should remove that account" is caught on `should` even
/// though `remove` is excused. What the exemption removes is the vocabulary a CLI must use to
/// NAME its own operations; the acquisitive call, the value judgement, the recommendation and
/// the alarmist projection all stay armed.
pub const HELP_EXEMPT_TOKENS: &[&str] = &["add", "disable", "enable", "remove"];

/// [`BANNED_TOKENS`] minus [`HELP_EXEMPT_TOKENS`] — the subset `--help` is scanned against.
/// DERIVED on every call rather than stored, so a token added centrally is covered on help too
/// without a second edit, and an exemption cannot silently outlive the token it excused.
pub fn help_banned_tokens() -> Vec<&'static str> {
    BANNED_TOKENS
        .iter()
        .copied()
        .filter(|t| !HELP_EXEMPT_TOKENS.contains(t))
        .collect()
}

/// The first banned token OR acquisitive phrase from the given lists appearing in `text`, or
/// `None` when it is clean. Strips ANSI SGR runs first (so a colour-wrapped word tokenises
/// intact), then matches whole lowercase WORDS on non-alphanumeric boundaries — so `at-risk`,
/// `At Risk`, and `risk!` all trip `risk`, while `saturated` or an account handle never
/// false-trips — and finally adjacent-word purchase-calls (`top up`), so a neutral head-room fact
/// passes while an acquisitive call does not (issue #542).
///
/// The single tokenizer both audiences share: [`scan_banned`] passes the whole list,
/// [`scan_help_banned`] passes the derived subset, and neither can drift from the other on what
/// counts as a word.
pub fn scan_with(
    text: &str,
    tokens: &[&'static str],
    phrases: &[&'static str],
) -> Option<&'static str> {
    let mut plain = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // Drop the SGR sequence up to and including its `m` terminator.
            for c2 in chars.by_ref() {
                if c2 == 'm' {
                    break;
                }
            }
        } else {
            plain.push(c);
        }
    }
    // Lowercase words in READING ORDER (a Vec, not a set) — the order lets the phrase scan
    // below match an adjacent-word purchase-call without a fragile substring test.
    let words: Vec<String> = plain
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_ascii_lowercase)
        .collect();
    // A single editorialising / acquisitive WORD (issue #160).
    if let Some(hit) = tokens
        .iter()
        .copied()
        .find(|b| words.iter().any(|w| w == b))
    {
        return Some(hit);
    }
    // A purchase-CALL spanning adjacent words (issue #542): `top up` / `get more`.
    phrases.iter().copied().find(|phrase| {
        let parts: Vec<&str> = phrase.split(' ').collect();
        words
            .windows(parts.len())
            .any(|win| win.iter().zip(&parts).all(|(w, p)| w.as_str() == *p))
    })
}

/// Scan `text` against the WHOLE central vocabulary — the stats-side guard (issues #160, #542),
/// unchanged in reach by issue #918's split.
pub fn scan_banned(text: &str) -> Option<&'static str> {
    scan_with(text, BANNED_TOKENS, BANNED_PHRASES)
}

/// Scan `text` against the help-side subset — every banned phrase, and every banned token except
/// the mechanical-operation verbs [`HELP_EXEMPT_TOKENS`] names (issue #918).
pub fn scan_help_banned(text: &str) -> Option<&'static str> {
    scan_with(text, &help_banned_tokens(), BANNED_PHRASES)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The central list is only evidence if EVERY member of it can fire. A token that matches
    /// nothing passes every "no false positives" test on every surface while proving nothing —
    /// the exact shape issue #918 was opened about, one token down instead of one surface down.
    ///
    /// This also discharges issue #918's no-regression obligation from the other side: the stats
    /// guard is asserted to still see all of [`BANNED_TOKENS`] and both [`BANNED_PHRASES`] after
    /// the split, token by token, rather than assumed to because the array was moved intact.
    ///
    /// The CENSUS below is not ceremony, and it is the first thing to understand about this test:
    /// the loops derive their subject FROM the lists, so deleting a token deletes its own check
    /// and every loop here still passes over the smaller list. Pinning the counts is what makes a
    /// SHRINKING vocabulary — the only change to it that is a coverage regression — visible.
    /// Growing the lists is expected and cheap: add the token, bump the number in the same commit.
    #[test]
    fn every_central_token_and_phrase_bites() {
        assert_eq!(
            (BANNED_TOKENS.len(), BANNED_PHRASES.len()),
            (51, 2),
            "the central vocabulary changed size — GROWING it is fine (bump this alongside), but \
             SHRINKING it drops coverage that every other assertion here would still pass over"
        );
        for token in BANNED_TOKENS {
            let injected = format!("period mean 42% {token} rest of the line");
            assert_eq!(
                scan_banned(&injected),
                Some(*token),
                "central token {token:?} must be caught by the stats-side scan"
            );
        }
        for phrase in BANNED_PHRASES {
            let injected = format!("runs out in ~4h — {phrase} before then");
            assert_eq!(
                scan_banned(&injected),
                Some(*phrase),
                "central phrase {phrase:?} must be caught by the stats-side scan"
            );
        }
    }

    /// No dead exemptions, and no exemption invented outside the central list: excusing a token
    /// that was never banned would read as a real carve-out while carving out nothing.
    #[test]
    fn every_help_exemption_names_a_real_central_token() {
        for exempt in HELP_EXEMPT_TOKENS {
            assert!(
                BANNED_TOKENS.contains(exempt),
                "{exempt:?} is exempted from the help scan but is not in BANNED_TOKENS — \
                 an exemption for a token nobody bans is noise"
            );
        }
        let mut sorted = HELP_EXEMPT_TOKENS.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            HELP_EXEMPT_TOKENS.len(),
            "the exemption set carries a duplicate"
        );
    }

    /// The exemption set is PINNED to exactly the measured four. Widening it is a design decision
    /// (issue #918 recorded one), so it must be a deliberate edit that reddens this test — never
    /// something that accretes a token at a time to silence an inconvenient help line.
    #[test]
    fn the_help_exemption_set_is_exactly_the_measured_mechanical_verbs() {
        assert_eq!(
            HELP_EXEMPT_TOKENS,
            &["add", "disable", "enable", "remove"],
            "the help exemption set moved — see issue #918: it is the mechanical-operation verbs \
             the shipped help measurably spends, and widening it needs the same argument"
        );
    }

    /// The exemption is scoped to `--help` and does not reach the central guard: every excused
    /// token is still caught on the stats side. This is the half that keeps the split honest —
    /// an exemption that leaked centrally would be a silent deletion from [`BANNED_TOKENS`].
    #[test]
    fn help_exemption_does_not_weaken_the_central_guard() {
        for exempt in HELP_EXEMPT_TOKENS {
            let injected = format!("period mean 42% {exempt} rest of the line");
            assert_eq!(
                scan_banned(&injected),
                Some(*exempt),
                "{exempt:?} is excused on help only — the stats scan must still catch it"
            );
            assert_eq!(
                scan_help_banned(&injected),
                None,
                "{exempt:?} must be excused by the help scan (that is what exempting means)"
            );
        }
    }

    /// The derived subset is the central list minus exactly the exemptions — neither a hand-copy
    /// that can drift nor a filter that quietly drops more than it advertises.
    #[test]
    fn the_help_subset_is_the_central_list_minus_exactly_the_exemptions() {
        let subset = help_banned_tokens();
        assert_eq!(
            subset.len(),
            BANNED_TOKENS.len() - HELP_EXEMPT_TOKENS.len(),
            "the derived help subset is not the central list minus the exemptions"
        );
        for token in BANNED_TOKENS {
            let excused = HELP_EXEMPT_TOKENS.contains(token);
            assert_eq!(
                subset.contains(token),
                !excused,
                "{token:?} is on the wrong side of the help subset"
            );
        }
    }

    /// An exemption set wide enough to swallow a whole editorial group would leave the help scan
    /// green over prose it was built to catch. Each of the four groups [`BANNED_TOKENS`]
    /// documents must still be represented in the derived subset AND still bite in help prose.
    #[test]
    fn help_subset_keeps_every_editorial_group_armed() {
        let subset = help_banned_tokens();
        // One representative per documented group, plus the two-word acquisitive call.
        for (group, token) in [
            ("acquisitive imperative", "upgrade"),
            ("value judgement", "critical"),
            ("recommendation framing", "should"),
            ("alarmist projection", "imminent"),
        ] {
            assert!(
                subset.contains(&token),
                "the {group} group lost its representative {token:?} to the help exemption"
            );
            assert_eq!(
                scan_help_banned(&format!("sessiometer status — {token} something")),
                Some(token),
                "the {group} group must still bite in help prose"
            );
        }
        for phrase in BANNED_PHRASES {
            assert_eq!(
                scan_help_banned(&format!("sessiometer status — {phrase} first")),
                Some(*phrase),
                "the acquisitive phrase {phrase:?} must still bite in help prose"
            );
        }
    }

    /// Excusing the mechanical verb does not excuse the sentence it sits in: a real call to
    /// action in help is still caught, on a group the exemption never touched.
    #[test]
    fn an_excused_verb_inside_a_recommendation_is_still_caught() {
        assert_eq!(
            scan_help_banned("you should remove that account"),
            Some("should")
        );
        assert_eq!(
            scan_help_banned("consider disabling the noisy account"),
            Some("consider")
        );
        assert_eq!(
            scan_help_banned("running out — top up before you add another"),
            Some("top up")
        );
    }

    /// Both scans share one tokenizer, so they agree about word boundaries, ANSI, and case. Only
    /// the LIST differs — assert that, so a future edit cannot fork the matching rules.
    #[test]
    fn both_scans_share_the_tokenizer_and_differ_only_by_list() {
        // Word boundaries: `bypasses` is not `bypass` on either side.
        assert_eq!(scan_banned("nothing bypasses it"), None);
        assert_eq!(scan_help_banned("nothing bypasses it"), None);
        // Case and punctuation trip both.
        assert_eq!(scan_banned("period — you SHOULD."), Some("should"));
        assert_eq!(scan_help_banned("period — you SHOULD."), Some("should"));
        // A colour-wrapped word tokenises intact on both.
        let coloured = "\x1b[31mcritical\x1b[0m";
        assert_eq!(scan_banned(coloured), Some("critical"));
        assert_eq!(scan_help_banned(coloured), Some("critical"));
        // …and the one documented divergence is the exemption, nothing else.
        assert_eq!(scan_banned("remove the account"), Some("remove"));
        assert_eq!(scan_help_banned("remove the account"), None);
    }
}
