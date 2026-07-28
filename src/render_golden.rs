//! Shared machinery for the CLI's FULL-OUTPUT render goldens (issue #767).
//!
//! # Why goldens at all
//!
//! Every pre-#767 render assertion in this crate is a SUBSTRING check
//! (`assert!(out.contains(…))`). That shape verifies a fragment and is structurally blind to
//! everything around it: a change that misaligns a column, duplicates a block, drops a whole
//! section, or reorders two lines leaves the asserted substring intact and passes green. A
//! committed full-output golden, compared WHOLE, is the only assertion whose subject is the
//! entire render — so the corruptions a substring check cannot see become the diff.
//!
//! # The one predicate
//!
//! [`matches`] is THE comparison. The real per-case assertions ([`assert_matches_goldens`])
//! and the CONSTRAINT-A canary ([`assert_canary`]) both route through it, so the canary
//! demonstrates that the gate WHICH ACTUALLY RUNS can fail — not a look-alike written beside
//! it. A gate proven only by inspection is not evidence; these mutations prove it by
//! MUTATION, which is why [`LAYOUT_MUTATIONS`] corrupts real rendered bytes rather than
//! asserting on a hand-written "bad" string.
//!
//! # Re-baselining is deliberate, never a side effect
//!
//! A golden IS the gate's assertion content: changing one changes what the gate asserts,
//! exactly as changing a threshold would. So blessing a render takes two acts — an explicit
//! command ([`emit`], reachable only through the `#[ignore]`d `emit_cli_render_goldens_*`
//! tests) and a recorded reason (a `CLI-Goldens-Rebaselined:` commit trailer, enforced in CI
//! by `scripts/check-cli-golden-rebaseline.sh`). The discipline mirrors the panel goldens'
//! (issue #754); the trailer is deliberately DISTINCT from both siblings so all three audit
//! trails stay separately greppable — `Gate-Change-Acknowledged:` answers "is this weakening of
//! the merge gate safe?", `Panel-Goldens-Rebaselined:` "what changed in the panel's
//! appearance?", and this one "what changed in the CLI's rendered text?".
//!
//! Compiled only under `cfg(test)`: this is test machinery, and nothing in the shipping binary
//! reads a golden.

use std::path::PathBuf;

/// One rendered case: the stable name its committed golden is filed under, and the bytes the
/// renderer produced for it in THIS run.
pub(crate) struct Case {
    /// Golden file stem, e.g. `status-wide-plain`. Also the `.txt` file name under
    /// [`goldens_dir`].
    pub(crate) name: &'static str,
    /// The freshly-rendered output being pinned.
    pub(crate) rendered: String,
}

impl Case {
    /// A case from its name and rendered text.
    pub(crate) fn new(name: &'static str, rendered: String) -> Self {
        Self { name, rendered }
    }
}

/// The committed goldens' directory: `build/fixtures/cli-renders/`, resolved from
/// `CARGO_MANIFEST_DIR` so it is the same path whatever the test's working directory.
pub(crate) fn goldens_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("build/fixtures/cli-renders")
}

/// A surface's committed goldens, keyed by case name — the name IS the filename, so a table
/// entry cannot pair a case with someone else's bytes.
///
/// Worth a macro (the crate's only one, and `#[cfg(test)]`-scoped) because the hand-paired form
/// carried the name and the path as two independent strings. A mispairing is mostly
/// self-correcting — the bytes differ, the test fails — but not always: `status-piped` and
/// `status-wide-plain` are byte-identical BY DESIGN (a non-TTY `status` must not shed columns,
/// which `each_width_case_exercises_the_degradation_it_claims` asserts as a contract), so
/// swapping exactly those two entries would be invisible to the entire suite. Deriving the path
/// from the name removes the hole rather than documenting it.
///
/// `include_str!` keeps every golden a COMPILE-TIME input, so a missing file is a build error
/// rather than a test that quietly skips.
macro_rules! cli_render_goldens {
    ($($name:literal),+ $(,)?) => {
        &[$((
            $name,
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"), "/build/fixtures/cli-renders/", $name, ".txt"
            )),
        )),+]
    };
}
pub(crate) use cli_render_goldens;

/// **THE** golden predicate — a whole-output byte comparison.
///
/// Deliberately trivial, and deliberately the ONLY comparison in this module: both the real
/// assertions and the canary call it, so "the gate can fail" is a statement about the gate
/// that actually guards the renders.
pub(crate) fn matches(rendered: &str, golden: &str) -> bool {
    rendered == golden
}

/// Look a case's committed golden up by name.
fn golden_for<'a>(goldens: &'a [(&'a str, &'a str)], name: &str) -> Option<&'a str> {
    goldens
        .iter()
        .find(|(golden_name, _)| *golden_name == name)
        .map(|(_, text)| *text)
}

/// One case's rendered text, by name — the lookup every per-surface property test needs ("the
/// coloured case", "the narrow case"). The twin of [`golden_for`] on the case side, hosted here
/// so the three `mod goldens` do not each re-write it under a different local name.
pub(crate) fn rendered<'a>(cases: &'a [Case], name: &str) -> &'a str {
    cases
        .iter()
        .find(|case| case.name == name)
        .unwrap_or_else(|| panic!("`{name}` is not in the case matrix"))
        .rendered
        .as_str()
}

/// The re-baseline instructions, appended to every drift failure so the operator never has to
/// go hunting for the command or the trailer.
fn rebaseline_hint(surface: &str) -> String {
    format!(
        "\n\nIf this change to the `{surface}` render is INTENTIONAL, re-baseline deliberately:\n\
         \x20   cargo test -- --ignored emit_cli_render_goldens\n\
         then LOOK at the regenerated files (a reference you have not looked at is not a\n\
         reference) and record why they changed:\n\
         \x20   git commit --trailer 'CLI-Goldens-Rebaselined: <what changed in the render and why>'\n\
         `scripts/check-cli-golden-rebaseline.sh` requires that trailer in CI whenever a PR\n\
         touches build/fixtures/cli-renders/."
    )
}

/// Assert every case matches its committed golden, WHOLE.
///
/// Also runs the degenerate-subject guard both directions: a pass over a partial case set is
/// not evidence, and a golden with no case behind it is a stale file nothing asserts. Returns
/// nothing — it panics on the first drift, like any assertion.
pub(crate) fn assert_matches_goldens(surface: &str, cases: &[Case], goldens: &[(&str, &str)]) {
    // Cardinality-zero is an automatic FAIL, never a pass — checked FIRST, so a surface whose
    // case list emptied out cannot reach the loops below and "pass" over nothing.
    assert!(
        !cases.is_empty(),
        "{surface}: zero cases — cardinality-zero is an automatic FAIL, never a pass"
    );
    // …and the two sets are the same SIZE. Redundant with the two name checks below only while
    // both name sets are duplicate-free; this catches the case they cannot — a duplicated name
    // on one side, which lets every name resolve while one file goes unasserted.
    assert_eq!(
        cases.len(),
        goldens.len(),
        "{surface}: {} cases vs {} committed goldens — the two lists have drifted apart",
        cases.len(),
        goldens.len()
    );

    // Degenerate-subject guard (direction 1): every committed golden is claimed by a case.
    // Without this a deleted case leaves its golden behind, still committed, asserting nothing.
    for (name, _) in goldens {
        assert!(
            cases.iter().any(|case| case.name == *name),
            "{surface}: committed golden `{name}.txt` has no case rendering it — either the \
             case was deleted (remove the golden and its include_str! entry) or it was renamed \
             (the golden is stale)"
        );
    }

    for case in cases {
        // Direction 2: every case has a committed golden. `include_str!` already makes a
        // MISSING FILE a compile error, so this catches the other half — a case whose name was
        // never added to the goldens table, which would otherwise be silently unasserted.
        let golden = golden_for(goldens, case.name).unwrap_or_else(|| {
            panic!(
                "{surface}: case `{}` has no entry in the goldens table — add an include_str! \
                 for build/fixtures/cli-renders/{}.txt, then emit it{}",
                case.name,
                case.name,
                rebaseline_hint(surface)
            )
        });
        assert!(
            matches(&case.rendered, golden),
            "{surface}: `{}` drifted from its committed golden.\n{}\n\
             --- committed golden ---\n{golden}\n\
             --- fresh render ---\n{}\n--- end ---{}",
            case.name,
            first_divergence(&case.rendered, golden),
            case.rendered,
            rebaseline_hint(surface)
        );
    }
}

/// Where two renders first diverge, in the terms a reader needs: a 1-based line number and the
/// two lines, `{:?}`-formatted so a lost padding space or a stray escape is VISIBLE rather than
/// swallowed by the terminal. Prepended to the full dump above — `reliability-full` is 47 lines,
/// and printing two of them side by side leaves the maintainer to eyeball 94 column-aligned
/// lines for what is usually one missing space. That is not a diagnosis.
fn first_divergence(rendered: &str, golden: &str) -> String {
    let mut golden_lines = golden.lines();
    let mut rendered_lines = rendered.lines();
    for at in 1.. {
        match (golden_lines.next(), rendered_lines.next()) {
            (Some(g), Some(r)) if g == r => continue,
            (Some(g), Some(r)) => {
                return format!(
                    "first divergence at line {at}:\n  golden:   {g:?}\n  rendered: {r:?}"
                )
            }
            (Some(g), None) => {
                return format!("the render STOPS at line {at}; the golden has {g:?}")
            }
            (None, Some(r)) => return format!("the render has an EXTRA line {at}: {r:?}"),
            // Byte-exactness means a lost trailing newline is real drift — and without this arm
            // the dump prints two renders that look identical.
            (None, None) => {
                return "every line matches — the difference is the TRAILING NEWLINE".to_owned()
            }
        }
    }
    unreachable!("the loop returns on the first divergence or on the shared end")
}

/// Write every case's rendered text to [`goldens_dir`] — the body of the `#[ignore]`d
/// `emit_cli_render_goldens_*` emitters, and the ONLY way a golden is ever written. There is
/// deliberately no auto-bless-on-missing anywhere in this module.
pub(crate) fn emit(cases: &[Case]) {
    let dir = goldens_dir();
    std::fs::create_dir_all(&dir).expect("create build/fixtures/cli-renders");
    for case in cases {
        std::fs::write(dir.join(format!("{}.txt", case.name)), &case.rendered)
            .unwrap_or_else(|err| panic!("write golden {}.txt: {err}", case.name));
    }
}

/// Remove the line at `index`, keeping the text's trailing-newline shape.
fn without_line(text: &str, index: usize) -> String {
    let mut lines: Vec<&str> = text.lines().collect();
    lines.remove(index);
    rejoin(&lines, text)
}

/// Rejoin lines, restoring the original's trailing newline if it had one.
fn rejoin(lines: &[&str], original: &str) -> String {
    let mut out = lines.join("\n");
    if original.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// One named corruption of a rendered output — the vocabulary [`assert_canary`] proves the
/// golden gate against.
pub(crate) struct Mutation {
    /// Stable identifier, used in failure messages and in a surface's inapplicability
    /// declaration (see [`assert_canary`]).
    pub(crate) name: &'static str,
    /// Corrupt the render, or return `None` when it has no such shape to corrupt — a one-line
    /// output has no two lines to swap, an uncoloured one has no escape to strip.
    pub(crate) apply: fn(&str) -> Option<String>,
}

/// The corruption classes a `contains()` assertion is structurally blind to (issue #767's
/// stated failure modes: column misalignment, a duplicated block, a dropped section, wrong
/// ordering) plus two byte-level ones (a lost colour overlay, a truncated stream).
///
/// [`assert_canary`] requires every mutation to apply somewhere across a surface's case set
/// unless that surface explicitly declares it inapplicable — so a mutation that quietly
/// stopped applying anywhere (and therefore stopped proving anything) fails rather than passes.
pub(crate) const LAYOUT_MUTATIONS: &[Mutation] = &[
    // A dropped section: the render loses a line entirely.
    Mutation {
        name: "drop-a-line",
        apply: |text| {
            let lines: Vec<&str> = text.lines().collect();
            (lines.len() >= 2).then(|| without_line(text, lines.len() / 2))
        },
    },
    // A duplicated block: a line renders twice.
    Mutation {
        name: "duplicate-a-line",
        apply: |text| {
            let mut lines: Vec<&str> = text.lines().collect();
            (!lines.is_empty()).then(|| {
                let at = lines.len() / 2;
                lines.insert(at, lines[at]);
                rejoin(&lines, text)
            })
        },
    },
    // Wrong ordering: two adjacent, DISTINCT lines trade places. Distinct-only, because
    // swapping two identical lines is a no-op the predicate would rightly accept.
    Mutation {
        name: "swap-adjacent-lines",
        apply: |text| {
            let mut lines: Vec<&str> = text.lines().collect();
            let at = (0..lines.len().saturating_sub(1)).find(|&i| lines[i] != lines[i + 1])?;
            lines.swap(at, at + 1);
            Some(rejoin(&lines, text))
        },
    },
    // Column misalignment: ONE space vanishes from a padding run. This is the canonical
    // substring-invisible defect — every `contains()` in the suite still passes, and the
    // table is crooked.
    Mutation {
        name: "collapse-one-pad-space",
        apply: |text| {
            let at = text.find("  ")?;
            let mut out = String::with_capacity(text.len() - 1);
            out.push_str(&text[..at]);
            out.push_str(&text[at + 1..]);
            Some(out)
        },
    },
    // A lost colour overlay: every SGR escape is stripped. Applies only to coloured cases,
    // and is what pins that colour AUGMENTS rather than replaces (the padded text underneath
    // must be identical either way).
    Mutation {
        name: "strip-ansi",
        apply: strip_ansi,
    },
    // A truncated stream: the terminating newline is lost.
    Mutation {
        name: "trim-trailing-newline",
        apply: |text| text.strip_suffix('\n').map(str::to_owned),
    },
];

/// Strip every SGR escape, or decline (`None`) when there is none to strip.
///
/// Named rather than inlined into [`LAYOUT_MUTATIONS`] because the per-surface tests reuse it to
/// assert that colour AUGMENTS the render rather than re-laying it out. The table points at THIS
/// function, so "the canary's stripper and the property test's stripper are the same operation"
/// is a compile-time fact rather than a name lookup that a rename could quietly stale.
pub(crate) fn strip_ansi(text: &str) -> Option<String> {
    if !text.contains('\x1b') {
        return None;
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('\x1b') {
        out.push_str(&rest[..at]);
        match rest[at..].find('m') {
            Some(end) => rest = &rest[at + end + 1..],
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    Some(out)
}

/// The CONSTRAINT-A canary: prove — by MUTATION, through the SAME [`matches`] predicate the
/// real assertions use — that this gate can FAIL.
///
/// A golden suite that cannot fail is not evidence, and the failure mode is not hypothetical:
/// issue #437's three render bugs were read five times as "the DESIGN fails distinctness"; a
/// golden authored in that window would have frozen the bugs and then DEFENDED them. So this
/// asserts three things:
///
/// 1. a byte-identical copy is ACCEPTED (the predicate is not a rubber "always false");
/// 2. every corruption in [`LAYOUT_MUTATIONS`] is REJECTED wherever it applies;
/// 3. every mutation applies to at least one case, so none has quietly become inert.
///
/// `inapplicable` names the mutations this surface legitimately cannot exercise — e.g.
/// `reliability` renders no colour, so `strip-ansi` has nothing to strip. The declaration is
/// checked in BOTH directions: a name that is not a real mutation fails, and so does one that
/// turns out to APPLY after all. So it can never be used to quietly silence a mutation that
/// stopped working — if `reliability` ever grew a colour gate, this list would go red and
/// force the coverage decision rather than hiding it.
pub(crate) fn assert_canary(surface: &str, cases: &[Case], inapplicable: &[&str]) {
    assert!(
        !cases.is_empty(),
        "{surface}: the canary needs cases — a canary over an empty set proves nothing"
    );
    for declared in inapplicable {
        assert!(
            LAYOUT_MUTATIONS
                .iter()
                .any(|mutation| mutation.name == *declared),
            "{surface}: `{declared}` is declared inapplicable but is not a mutation — a typo \
             here would silently exempt nothing while looking like it exempted something"
        );
    }

    // Positive control, once per case: the predicate ACCEPTS an identical render. Without this,
    // a `matches` that always returned `false` would pass every rejection assertion below and
    // the canary would certify a gate that rejects everything. It is a property of the CASE, not
    // of any one mutation, so it belongs out here rather than inside the mutation loop.
    for case in cases {
        assert!(
            matches(&case.rendered, &case.rendered),
            "{surface}: `{}` does not match itself — the predicate rejects everything, so its \
             rejections below prove nothing",
            case.name
        );
    }

    let mut applied = 0_usize;
    for Mutation { name, apply } in LAYOUT_MUTATIONS {
        let exempt = inapplicable.contains(name);
        let mut applied_here = 0_usize;
        for case in cases {
            let Some(corrupted) = apply(&case.rendered) else {
                continue;
            };
            assert_ne!(
                corrupted, case.rendered,
                "{surface}: mutation `{name}` returned the input unchanged for `{}` — an \
                 inert mutation cannot demonstrate the gate fails",
                case.name
            );
            assert!(
                !matches(&corrupted, &case.rendered),
                "{surface}: mutation `{name}` on `{}` was ACCEPTED by the golden predicate — \
                 the gate cannot catch this corruption class, so it is not evidence against it",
                case.name
            );
            applied_here += 1;
        }
        if exempt {
            // The declaration is a CLAIM about this surface, so it is checked: a mutation
            // declared inapplicable that in fact applies means the surface changed shape
            // (colour was added, say) and its coverage decision needs re-taking.
            assert_eq!(
                applied_here, 0,
                "{surface}: mutation `{name}` is declared inapplicable, but it applied to \
                 {applied_here} case(s) — the surface grew the shape it targets, so remove the \
                 exemption and let the mutation guard it"
            );
            continue;
        }
        assert!(
            applied_here > 0,
            "{surface}: mutation `{name}` applied to NONE of the {} cases — it proves nothing \
             about this surface; either the case set lost the shape it targets (add a case), \
             the mutation is dead (remove it), or this surface genuinely cannot exercise it \
             (declare it inapplicable, with a reason)",
            cases.len()
        );
        applied += 1;
    }

    assert_eq!(
        applied + inapplicable.len(),
        LAYOUT_MUTATIONS.len(),
        "{surface}: {applied} mutations exercised + {} declared inapplicable != {} in the table \
         — with every name already checked against the table, the way to reach here is a \
         DUPLICATE entry in `inapplicable`",
        inapplicable.len(),
        LAYOUT_MUTATIONS.len()
    );
}

/// The INPUT-side half of the canary: a render produced from a deliberately perturbed FIXTURE
/// must not match the unperturbed golden.
///
/// [`assert_canary`] corrupts rendered bytes, which proves the comparison is byte-exact.
/// This proves the other, more important half — that the gate is sensitive to a real change in
/// the data flowing through the renderer, which is the shape an actual regression takes.
pub(crate) fn assert_perturbed_input_is_rejected(
    surface: &str,
    case_name: &str,
    baseline: &str,
    perturbed: &str,
) {
    assert_ne!(
        perturbed, baseline,
        "{surface}: the perturbed fixture rendered IDENTICALLY to the baseline for \
         `{case_name}` — the perturbation does not reach this render, so it cannot show the \
         gate is sensitive to input change"
    );
    assert!(
        !matches(perturbed, baseline),
        "{surface}: a render from a PERTURBED fixture still matched the `{case_name}` golden \
         — the gate is blind to input change"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every mutation must be able to corrupt a realistic multi-line, padded, coloured render.
    /// This is the canary's own canary: it pins that the corruption helpers work on the SHAPE
    /// the CLI actually produces, independent of any one surface's case list.
    #[test]
    fn every_layout_mutation_corrupts_a_representative_render() {
        let sample =
            "ACCOUNT  SESSION% RESET\n* work   \x1b[33m97%\x1b[0m      12m\n  spare  10%      2h\n";
        for Mutation { name, apply } in LAYOUT_MUTATIONS {
            let corrupted = apply(sample)
                .unwrap_or_else(|| panic!("mutation `{name}` does not apply to the sample"));
            assert!(
                !matches(&corrupted, sample),
                "mutation `{name}` produced something the predicate still accepts"
            );
        }
    }

    /// The predicate is byte-exact: whitespace-only and escape-only differences are drift, not
    /// noise to be normalised away. Pinning this here stops a later "helpful" trim/normalise
    /// from silently blinding every golden in the crate.
    #[test]
    fn the_predicate_is_byte_exact() {
        assert!(matches("a b\n", "a b\n"));
        assert!(!matches("a b\n", "a  b\n"), "padding width is significant");
        assert!(
            !matches("a b\n", "a b"),
            "the trailing newline is significant"
        );
        assert!(
            !matches("\x1b[33ma\x1b[0m\n", "a\n"),
            "the colour overlay is significant"
        );
    }

    /// `strip-ansi` declines an uncoloured render rather than returning it unchanged — the
    /// `None` contract [`assert_canary`]'s applicability accounting depends on. Also pins that
    /// stripping removes ONLY the escapes: a stripper that also ate a padding space would make
    /// the per-surface "colour augments" assertions pass over a render it had itself corrupted.
    #[test]
    fn a_mutation_declines_when_it_cannot_apply() {
        assert!(strip_ansi("plain text\n").is_none());
        assert_eq!(
            strip_ansi("a  \x1b[33mb\x1b[0m  c\n").as_deref(),
            Some("a  b  c\n"),
            "stripping SGR escapes must leave every other byte, padding included, untouched"
        );
    }
}
