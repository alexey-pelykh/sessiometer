// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! The FRAMING guard's shared vocabulary (issues #160, #542, #918, #1123) — one banned list, one
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
//! # One list, four audiences
//!
//! [`BANNED_TOKENS`] stays whole and stats keeps scanning against all of it — nothing was
//! subtracted from what `stats.rs` saw before. Every other audience scans a DERIVED subset:
//! [`BANNED_TOKENS`] minus that audience's own exemption set, computed by
//! [`banned_tokens_except`]. Deriving the subsets rather than hand-copying them is the point: a
//! second hand-maintained list would drift from the first, which is the failure mode this module
//! was created to end — and that argument does not weaken as the number of audiences grows, it
//! strengthens.
//!
//! | Audience | Subset | Exemptions | Earned by |
//! |---|---|---|---|
//! | stats bands (#160, #542) | [`BANNED_TOKENS`] whole | none | — |
//! | `--help` (#918) | [`help_banned_tokens`] | [`HELP_EXEMPT_TOKENS`] | the shipped help |
//! | operator advisories (#1123) | [`advisory_banned_tokens`] | [`ADVISORY_EXEMPT_TOKENS`] | the shipped advisories |
//! | `Error::CliUsage` prose (#1123) | [`usage_banned_tokens`] | [`USAGE_EXEMPT_TOKENS`] | the shipped usage hints |
//!
//! That fourth row names ONE variant, and the narrowness is deliberate rather than shorthand.
//! `Error` has many other operator-facing variants and several carry central vocabulary today —
//! `ConfigTargetMaxSessionAboveTrigger` and `SharedCredentialMutated` spend `must`,
//! `ActiveAccountUnresolved` spends `add` and the value judgement `healthy`. None is in scope
//! here: issue #1123 scoped `Error::CliUsage`, whose prose is a usage HINT of the same register
//! as the help text issue #918 scanned. Issue #1139 carries the rest. Calling this row "error
//! prose" would advertise a reach it does not have, which is the exact defect issue #918 was
//! opened about.
//!
//! All four share ONE tokenizer ([`scan_with`]), so no guard can silently disagree with another
//! about what counts as a word.
//!
//! # Why each audience gets its OWN exemption set, and not the widest one
//!
//! As it happens the three EXEMPTION sets nest — `{enable}` ⊂ `{disable, enable, remove}` ⊂
//! `{add, disable, enable, remove}`, so the derived SUBSETS nest the other way round and help is
//! scanned against the fewest tokens — and it would be less code to point all three at
//! [`HELP_EXEMPT_TOKENS`]. That is precisely the move issue #1123 was opened to refuse. An
//! exemption is a hole in the guard, and a hole is only defensible where the shipped prose
//! measurably needs it: handing the advisories `add`, `disable` and `remove` — three tokens no
//! advisory spends — would widen the guard until it stopped biting, one convenience at a time. So
//! each set is MEASURED against the surface it excuses, and each has a test asserting it is still
//! earned there.
//!
//! That nesting is an OBSERVATION about today's three measurements, not a rule they owe: an
//! advisory that came to spend a verb help does not would break the chain and be perfectly
//! correct. `the_module_docs_nesting_claim_still_describes_the_measured_sets` guards this
//! paragraph against going stale, and says so — it is not a licence to widen a set.
//!
//! # The imperative question (issue #1123 AC-1)
//!
//! Issue #918 scoped this vocabulary against help text, which NAMES operations
//! (`disable <account>   Park an account`). The advisories DIRECT them (`run 'sessiometer poke'`),
//! and the open question was whether an imperative needs a vocabulary of its own before a scan is
//! pointed at it.
//!
//! Measured, it does not — and the measurement is half the answer. `run 'sessiometer poke'` costs
//! ZERO central tokens, because [`BANNED_TOKENS`] never banned the imperative MOOD. It bans four
//! things, and each is about what the sentence asks the operator to BUY, BELIEVE or FEEL: an
//! acquisitive call, a value judgement, a recommendation, an alarmist projection. Grammatical mood
//! is not the discriminator; the OBJECT of the imperative is. An imperative whose object is a
//! free, local, mechanical operation on the tool's own state is a REMEDY, while an imperative
//! whose object is acquisition (`top up`, `upgrade your plan`) is a purchase call and stays banned
//! by the list as it already stands.
//!
//! The other half is that this is a NEW boundary, and it is worth being exact about whose. It is
//! **not** an application of the head-room permit issue #542 and ADR-0020 settled: that permit is
//! explicitly for a fact stated "as an observation, not advice", and a remedy directive IS advice,
//! so the analogy would prove the opposite of what it is reached for. ADR-0020 also stated its
//! Context as "no imperative" — broader than its own Decision, which bans the *acquisitive*
//! purchase prompt. Issue #1123 narrowed that sentence and extended the boundary to a surface
//! class ADR-0020 never had (a `stats` band has no remedy to direct), and ADR-0020 § Status →
//! Amended 2026-08-10 records it.
//!
//! What the extension actually rests on is older and independently tested: this tool's operator
//! guidance is REQUIRED to be clear and FOLLOWABLE (issues #376 / #397) — see `crate::error`'s
//! `NoManagedService` and `UnmanagedDaemonNoRestart` and the
//! `unmanaged_daemon_no_restart_guides_the_operator_with_a_followable_action` test, `src/cli.rs`'s
//! "name the followable stop first", and `crate::log`'s "the refusal must name the followable
//! alternatives". A tool obliged to name a followable action cannot also be forbidden from naming
//! one; the two requirements are orthogonal, and reading the #160 firewall as a ban on directives
//! would put them in contradiction. That is the sense in which the cue not only *does* pass but
//! *should*.
//!
//! Structurally it is issue #918's semantic boundary — mechanical operation vs editorial framing —
//! carried one step further, from naming an operation to directing one, and it needed no new
//! vocabulary. The only central token these surfaces spend at all is `enable`, in the advisory's
//! "or enable [refresh] to maintain them": a config operation, the same mechanical class, which is
//! why the exemption sets below are drawn from the SAME four verbs #918 measured rather than from
//! a new list invented for imperatives.
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

/// The tokens the operator ADVISORIES are excused from (issue #1123): `enable` alone, spent by
/// `REFRESH_DISABLED_ADVISORY`'s "or enable [refresh] to maintain them". Naming a config section
/// the operator can switch on is a MECHANICAL OPERATION on the tool's own state — the same
/// semantic class [`HELP_EXEMPT_TOKENS`] documents, reached from the other side: help NAMES the
/// operation, an advisory DIRECTS it.
///
/// One token, not the four help gets, and that is the whole point of a separate constant. The
/// advisories do not spend `add`, `disable` or `remove`, so excusing them here would be a hole
/// nothing asked for — and issue #1123 exists because "point the existing guard at them" is the
/// tempting move that quietly widens a guard until it stops biting.
///
/// `DEGRADED_CUE` needs nothing from this set: it is clean against the WHOLE central vocabulary,
/// imperative and all, which is the empirical answer to issue #1123's imperative question rather
/// than an argument for one. `src/cli.rs`'s
/// `the_advisory_exemption_is_earned_by_the_advisory_alone_not_by_the_cue` asserts it, beside the
/// constant it is about.
pub const ADVISORY_EXEMPT_TOKENS: &[&str] = &["enable"];

/// The tokens [`Error::CliUsage`](crate::error::Error::CliUsage)'s authored prose is excused from
/// (issue #1123): the three CLI COMMAND NAMES that appear in a `usage_hint` — `sessiometer
/// disable --help`, `… enable --help`, `… remove --help`. An error whose whole job is to name the
/// command to run cannot avoid spelling that command, which is the narrowest possible reason for
/// an exemption to exist.
///
/// Scoped to that ONE variant, not to `Error` at large — see the module doc's audience table for
/// the other variants that carry central vocabulary today and the issue that covers them.
///
/// `add` is absent, and its absence is measured, not stylistic: `add` is not a verb in this CLI
/// (it is help-only, spent by `STATUS_USAGE`'s `-v` line), so no usage hint can earn it. That
/// asymmetry with [`HELP_EXEMPT_TOKENS`] is exactly why this set is its own constant.
pub const USAGE_EXEMPT_TOKENS: &[&str] = &["disable", "enable", "remove"];

/// [`BANNED_TOKENS`] minus `exempt` — the one derivation rule every audience's subset goes
/// through. DERIVED on every call rather than stored, so a token added centrally is covered on
/// every surface without a second edit, and an exemption cannot silently outlive the token it
/// excused.
///
/// Shared rather than copied per audience for the reason the module doc gives: a hand-maintained
/// second list drifts from the first, and a hand-maintained fourth one drifts faster.
fn banned_tokens_except(exempt: &[&str]) -> Vec<&'static str> {
    BANNED_TOKENS
        .iter()
        .copied()
        .filter(|t| !exempt.contains(t))
        .collect()
}

/// [`BANNED_TOKENS`] minus [`HELP_EXEMPT_TOKENS`] — the subset `--help` is scanned against.
pub fn help_banned_tokens() -> Vec<&'static str> {
    banned_tokens_except(HELP_EXEMPT_TOKENS)
}

/// [`BANNED_TOKENS`] minus [`ADVISORY_EXEMPT_TOKENS`] — the subset the operator advisories are
/// scanned against (issue #1123). The STRICTEST of the three derived subsets.
pub fn advisory_banned_tokens() -> Vec<&'static str> {
    banned_tokens_except(ADVISORY_EXEMPT_TOKENS)
}

/// [`BANNED_TOKENS`] minus [`USAGE_EXEMPT_TOKENS`] — the subset
/// [`Error::CliUsage`](crate::error::Error::CliUsage)'s authored prose is scanned against
/// (issue #1123).
pub fn usage_banned_tokens() -> Vec<&'static str> {
    banned_tokens_except(USAGE_EXEMPT_TOKENS)
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

/// Scan `text` against the advisory subset — every banned phrase, and every banned token except
/// [`ADVISORY_EXEMPT_TOKENS`] (issue #1123). For the static operator advisories `src/cli.rs`
/// renders into `status`.
pub fn scan_advisory_banned(text: &str) -> Option<&'static str> {
    scan_with(text, &advisory_banned_tokens(), BANNED_PHRASES)
}

/// Scan `text` against the usage subset — every banned phrase, and every banned token except the
/// command names [`USAGE_EXEMPT_TOKENS`] carries (issue #1123). For the AUTHORED half of
/// `Error::CliUsage`: its message templates driven with neutral argv, plus every `usage_hint`.
/// Interpolated operator argv is deliberately not this guard's subject — see `Error::CliUsage`'s
/// own doc comment, which records that split and why.
pub fn scan_usage_banned(text: &str) -> Option<&'static str> {
    scan_with(text, &usage_banned_tokens(), BANNED_PHRASES)
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

    // --- the advisory and usage subsets (issue #1123) --------------------------------

    /// The three derived audiences, each paired with its exemption set and its scanner, so the
    /// assertions below cover all of them rather than whichever one a future edit remembered.
    #[allow(clippy::type_complexity)]
    const DERIVED_AUDIENCES: &[(&str, &[&str], fn(&str) -> Option<&'static str>)] = &[
        ("help", HELP_EXEMPT_TOKENS, scan_help_banned),
        ("advisory", ADVISORY_EXEMPT_TOKENS, scan_advisory_banned),
        ("usage", USAGE_EXEMPT_TOKENS, scan_usage_banned),
    ];

    /// Both new exemption sets are PINNED to exactly the tokens issue #1123 measured on the
    /// surfaces they excuse, for the same reason issue #918 pinned the help set: widening one is
    /// a design decision that must redden a test, never something that accretes a token at a time
    /// to silence an inconvenient line.
    ///
    /// The two sets differ, and the difference is the evidence. `add` is in neither — it is
    /// help-only vocabulary — and `disable`/`remove` are in the usage set but not the advisory
    /// one, because a `usage_hint` names those commands and no advisory does.
    #[test]
    fn the_new_exemption_sets_are_exactly_the_measured_tokens() {
        assert_eq!(
            ADVISORY_EXEMPT_TOKENS,
            &["enable"],
            "the advisory exemption set moved — see issue #1123: `enable` is the ONE central \
             token the shipped advisories spend, and widening it needs the same measurement"
        );
        assert_eq!(
            USAGE_EXEMPT_TOKENS,
            &["disable", "enable", "remove"],
            "the usage exemption set moved — see issue #1123: these are the three CLI command \
             names a `usage_hint` must spell, and nothing else earns a hole in this guard"
        );
    }

    /// No dead exemptions and no invented ones, across every derived audience: excusing a token
    /// that was never banned would read as a real carve-out while carving out nothing. The
    /// help-side half of this is issue #918's `every_help_exemption_names_a_real_central_token`;
    /// this is the same discipline applied to all three sets at once, so a fourth audience cannot
    /// be added without inheriting it.
    #[test]
    fn every_derived_exemption_names_a_real_central_token() {
        for (audience, exempt, _) in DERIVED_AUDIENCES {
            for token in *exempt {
                assert!(
                    BANNED_TOKENS.contains(token),
                    "{audience}: {token:?} is exempted but is not in BANNED_TOKENS — an \
                     exemption for a token nobody bans is noise"
                );
            }
            let mut sorted = exempt.to_vec();
            sorted.sort_unstable();
            sorted.dedup();
            assert_eq!(
                sorted.len(),
                exempt.len(),
                "{audience}: the exemption set carries a duplicate"
            );
        }
    }

    /// Every derived subset is the central list minus exactly its own exemptions — neither a
    /// hand-copy that can drift nor a filter that quietly drops more than it advertises. All
    /// three go through [`banned_tokens_except`], so this also pins that the shared derivation
    /// did not change what `--help` sees when the advisory and usage subsets were added to it
    /// (issue #1123 AC-4).
    #[test]
    fn every_derived_subset_is_the_central_list_minus_exactly_its_exemptions() {
        for (audience, exempt, _) in DERIVED_AUDIENCES {
            let subset = banned_tokens_except(exempt);
            assert_eq!(
                subset.len(),
                BANNED_TOKENS.len() - exempt.len(),
                "{audience}: the derived subset is not the central list minus its exemptions"
            );
            for token in BANNED_TOKENS {
                assert_eq!(
                    subset.contains(token),
                    !exempt.contains(token),
                    "{audience}: {token:?} is on the wrong side of the subset"
                );
            }
        }
        // The named wrappers agree with the shared rule — a wrapper that filtered by some OTHER
        // set would satisfy every assertion above while scanning the wrong list.
        assert_eq!(
            help_banned_tokens(),
            banned_tokens_except(HELP_EXEMPT_TOKENS)
        );
        assert_eq!(
            advisory_banned_tokens(),
            banned_tokens_except(ADVISORY_EXEMPT_TOKENS)
        );
        assert_eq!(
            usage_banned_tokens(),
            banned_tokens_except(USAGE_EXEMPT_TOKENS)
        );
    }

    /// Issue #1123 AC-3, the exemption-swallow proof: an exemption set wide enough to swallow a
    /// whole editorial group would leave its guard green over the prose it was built to catch.
    /// Each of the four groups [`BANNED_TOKENS`] documents, plus the two-word acquisitive call,
    /// must still be represented in EVERY derived subset and must still bite there.
    ///
    /// Deliberately run per-audience over real-shaped prose rather than as a set-difference
    /// assertion: a subset can contain a token and still fail to catch it if the scanner and the
    /// list ever disagree, and that is the failure this whole module exists to make impossible.
    #[test]
    fn every_derived_subset_keeps_every_editorial_group_armed() {
        for (audience, exempt, scan) in DERIVED_AUDIENCES {
            let subset = banned_tokens_except(exempt);
            for (group, token) in [
                ("acquisitive imperative", "upgrade"),
                ("value judgement", "critical"),
                ("recommendation framing", "should"),
                ("alarmist projection", "imminent"),
            ] {
                assert!(
                    subset.contains(&token),
                    "{audience}: the {group} group lost its representative {token:?} to the \
                     exemption set"
                );
                assert_eq!(
                    scan(&format!("advisory: [refresh] is off — {token} something")),
                    Some(token),
                    "{audience}: the {group} group must still bite"
                );
            }
            for phrase in BANNED_PHRASES {
                assert_eq!(
                    scan(&format!("running out — {phrase} first")),
                    Some(*phrase),
                    "{audience}: the acquisitive phrase {phrase:?} must still bite"
                );
            }
        }
    }

    /// Excusing a mechanical verb does not excuse the sentence around it, on the new audiences
    /// just as on help: a genuine call to action is still caught, on a group no exemption set
    /// touches. This is what makes the carve-outs above a carve-out rather than a hole.
    #[test]
    fn an_excused_verb_inside_a_recommendation_is_still_caught_on_every_audience() {
        assert_eq!(
            scan_advisory_banned("advisory: you should enable [refresh]"),
            Some("should")
        );
        assert_eq!(
            scan_advisory_banned("advisory: consider enabling [refresh] while there is time"),
            Some("consider")
        );
        assert_eq!(
            scan_usage_banned("unknown flag — you should remove that account"),
            Some("should")
        );
        assert_eq!(
            scan_usage_banned("run `sessiometer disable --help` — or upgrade your plan"),
            Some("upgrade")
        );
    }

    /// Issue #1123 AC-4, from the side the new subsets could have broken it: the two new
    /// exemptions are scoped to their own audiences and reach neither the central guard nor the
    /// help one. Every token excused anywhere is still caught by `scan_banned`, and the help
    /// subset still excuses exactly what issue #918 measured — no more, because the advisory and
    /// usage sets were added beside it rather than merged into it.
    ///
    /// The central list's own no-regression assertion is `every_central_token_and_phrase_bites`
    /// above, which walks [`BANNED_TOKENS`] token by token and pins its size; this is the
    /// complementary half, about the exemptions rather than the list.
    #[test]
    fn the_new_exemptions_reach_neither_the_central_guard_nor_the_help_one() {
        for (audience, exempt, _) in DERIVED_AUDIENCES {
            for token in *exempt {
                let injected = format!("period mean 42% {token} rest of the line");
                assert_eq!(
                    scan_banned(&injected),
                    Some(*token),
                    "{audience}: {token:?} is excused on its own surface only — the stats scan \
                     must still catch it"
                );
            }
        }
        // The help subset is untouched by issue #1123: a token excused for the advisories or the
        // usage prose is still scanned on help unless issue #918 measured it there too. Nothing
        // in the new sets is outside #918's four, so the assertion is that help's exemptions are
        // still exactly those four — stated here as a REGRESSION check on the #1123 change, not
        // as a restatement of the #918 pin it deliberately duplicates.
        assert_eq!(
            HELP_EXEMPT_TOKENS,
            &["add", "disable", "enable", "remove"],
            "issue #1123 must not have widened or narrowed the help exemption set"
        );
        assert_eq!(
            help_banned_tokens().len(),
            BANNED_TOKENS.len() - 4,
            "the help subset changed size — its coverage is issue #1123 AC-4's subject"
        );
    }

    /// The module doc above states the three exemption sets happen to NEST — `{enable}` ⊂
    /// `{disable, enable, remove}` ⊂ `{add, disable, enable, remove}`. This keeps that sentence
    /// honest, and it is a DOC check, which is the whole of its authority.
    ///
    /// The distinction matters because the obvious stronger reading is wrong twice over. Nesting
    /// is a contingent OUTCOME of three independent measurements, not a property they owe: if an
    /// advisory legitimately came to spend `fix`, the advisory set would earn a token help does
    /// not, the chain would break, and NOTHING would be wrong — the sets would be exactly as
    /// tight as their surfaces measured. And containment does not imply tightness in any case;
    /// what actually holds each set to what its surface earns is its own
    /// `…_is_still_earned_by_…` test, one per audience, measured against the shipped prose.
    ///
    /// So the remedy when this reddens is to UPDATE THE MODULE DOC to describe the sets as they
    /// now are. Widening an exemption set to restore the chain would be the precise move issue
    /// #1123 exists to refuse — a hole opened in a guard to satisfy a comment.
    #[test]
    fn the_module_docs_nesting_claim_still_describes_the_measured_sets() {
        const REMEDY: &str = "the module doc's nesting sentence is now stale — describe the sets \
                              as measured; do NOT widen an exemption set to restore the chain";
        for token in ADVISORY_EXEMPT_TOKENS {
            assert!(
                USAGE_EXEMPT_TOKENS.contains(token),
                "{token:?} is excused for advisories but not for usage: {REMEDY}"
            );
        }
        for token in USAGE_EXEMPT_TOKENS {
            assert!(
                HELP_EXEMPT_TOKENS.contains(token),
                "{token:?} is excused for usage but not for help: {REMEDY}"
            );
        }
        // Strictly nested, not merely nested: each audience really is tighter than the next, so
        // none of the three constants is today a redundant alias of another.
        assert!(
            ADVISORY_EXEMPT_TOKENS.len() < USAGE_EXEMPT_TOKENS.len()
                && USAGE_EXEMPT_TOKENS.len() < HELP_EXEMPT_TOKENS.len(),
            "the three exemption sets are no longer strictly ordered by size: {REMEDY}"
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
