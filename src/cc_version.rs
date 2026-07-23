// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! Provenance of the Claude Code range `sessiometer`'s reverse-engineered internals were
//! verified against, surfaced as a neutral line in `sessiometer --version` (issue #716).
//!
//! `sessiometer` depends on reverse-engineered Claude Code internals — the keychain-service
//! derivation (#100) and the credential-refresh lifecycle (#101) — verified only within the
//! range recorded in `build/version-compat.md`. This module bakes that range in and reports
//! it: a RECORD of what was verified, not a check on what is installed.
//!
//! **Provenance, not a runtime version gate.** There is no `claude --version` probe and no
//! operator advisory here. The installed version was never a control — a matching version
//! number does not prove the reverse-engineered internals are unchanged, nor a mismatched one
//! prove they changed — so the earlier startup/`status` advisory (issue #715) was removed in
//! #716. The drift that actually matters is caught at runtime by the #714 behavioral canary,
//! which re-verifies the keychain-service derivation and refuses the credential write when it
//! drifts — on the user's machine, where the risk lands. The range survives only as pull
//! provenance: this `--version` line, the ledger, and ADR #717.
//!
//! **No external reads.** This module reads nothing outside the process — not `claude`, not
//! the keychain, not `~/.claude.json`. It formats two baked constants, so it cannot block,
//! delay, or fail any operation, and it has no secret to leak (issue #15).
//!
//! # Why the range is a pair of constants
//!
//! `build/version-compat.md` is the authoritative source of truth, but a SHIPPED binary
//! cannot read it: the ledger is a repository document, not an installed asset. The range
//! must therefore be baked in at compile time. It is baked as two [`Version`] constants
//! rather than by `include_str!`-ing and parsing the ledger at runtime, because that would
//! embed ~14 KB of prose in the binary. Drift is instead caught in CI: the
//! `the_baked_range_matches_the_ledger` test makes the ledger a compile-time input of the
//! TEST build and asserts the constants still match it, so widening the range without updating
//! this file is a red test rather than silently stale provenance. This mirrors the crate's
//! existing committed-fixture idiom (`stats.rs`, `daemon/snapshot_build.rs`).

use std::fmt;

/// The lowest Claude Code the reverse-engineered assumptions were verified on.
///
/// Baked from `build/version-compat.md` § Supported Claude Code range
/// (`- CC_SUPPORTED_MIN:`); kept honest by the `the_baked_range_matches_the_ledger` test.
const CC_SUPPORTED_MIN: Version = Version::new(2, 1, 181);

/// The highest Claude Code the reverse-engineered assumptions were verified on.
///
/// Baked from `build/version-compat.md` § Supported Claude Code range
/// (`- CC_SUPPORTED_MAX:`); kept honest by the `the_baked_range_matches_the_ledger` test.
const CC_SUPPORTED_MAX: Version = Version::new(2, 1, 217);

/// A three-part `major.minor.patch` Claude Code version.
///
/// Ordered NUMERICALLY by field, in declaration order — the derived [`Ord`] compares
/// `major`, then `minor`, then `patch`. That is the whole reason this is a struct rather
/// than the raw strings the ledger holds: lexicographically `"2.1.99" > "2.1.181"`, which
/// would place a genuinely-too-old CC above the range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Version {
    major: u32,
    minor: u32,
    patch: u32,
}

impl Version {
    /// A version from its three parts. `const` so the range bounds above are compile-time
    /// values with no parse step — a malformed constant is a compile error, not a runtime one.
    const fn new(major: u32, minor: u32, patch: u32) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// The provenance line for `sessiometer --version`: the Claude Code range the
/// reverse-engineered internals were verified against.
///
/// A neutral RECORD, printed UNCONDITIONALLY — it names the two baked constants and never
/// probes `claude`, so it cannot vary by machine, alarm, or fail. Surfacing the range in
/// `--version` (alongside the ledger and ADR #717) is what keeps `CC_SUPPORTED_MIN` /
/// `CC_SUPPORTED_MAX` provenance a user can pull, now that nothing pushes an advisory at them.
pub(crate) fn supported_range_provenance() -> String {
    format!("verified against Claude Code {CC_SUPPORTED_MIN}-{CC_SUPPORTED_MAX}")
}

/// Parse a leading `major.minor.patch` from one line, tolerating leading whitespace, an
/// optional `v` prefix, and any trailing remainder.
///
/// Test-only helper: `the_baked_range_matches_the_ledger` reads the ledger's
/// `- CC_SUPPORTED_{MIN,MAX}: x.y.z` values through this. Mirrors the anchored
/// `^[[:space:]]*v?[0-9]+\.[0-9]+\.[0-9]+` version regex in `scripts/check-cc-version.sh`, so
/// this and that script agree on what counts as a version. A component ≥ 2³² overflows this
/// `u32` parse to `None`.
#[cfg(test)]
fn leading_version(line: &str) -> Option<Version> {
    fn take_number(s: &str) -> Option<(u32, &str)> {
        let end = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
        // A `u32` overflow (an absurd component) parses to `None` and degrades to silence,
        // exactly like any other unparseable output.
        Some((s[..end].parse().ok()?, &s[end..]))
    }

    let s = line.trim_start();
    let s = s.strip_prefix('v').unwrap_or(s);
    let (major, s) = take_number(s)?;
    let (minor, s) = take_number(s.strip_prefix('.')?)?;
    let (patch, _rest) = take_number(s.strip_prefix('.')?)?;
    Some(Version::new(major, minor, patch))
}

#[cfg(test)]
mod tests {
    use super::{
        leading_version, supported_range_provenance, Version, CC_SUPPORTED_MAX, CC_SUPPORTED_MIN,
    };

    /// The authoritative range ledger, made a compile-time input of the TEST build so the
    /// baked constants cannot drift from it unnoticed. Same idiom as the committed wire
    /// goldens (`stats.rs`, `daemon/snapshot_build.rs`).
    const LEDGER: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/build/version-compat.md"
    ));

    /// The `- CC_SUPPORTED_{MIN,MAX}: x.y.z` value from the ledger, parsed with the same
    /// `leading_version` used to read a version anywhere else. Skips the HTML comment that
    /// documents the format by requiring the `-` list-item prefix — exactly the discriminator
    /// `scripts/check-cc-version.sh` anchors its `grep` on.
    fn ledger_bound(which: &str) -> Version {
        let needle = format!("CC_SUPPORTED_{which}:");
        let raw = LEDGER
            .lines()
            .filter(|line| line.trim_start().starts_with('-'))
            .find_map(|line| line.split_once(&needle))
            .map(|(_, rest)| rest.trim())
            .unwrap_or_else(|| panic!("no `- {needle}` line in build/version-compat.md"));
        let parsed = leading_version(raw)
            .unwrap_or_else(|| panic!("unparseable {needle} value in the ledger: {raw:?}"));
        // Round-trip equality is the strictness check: the ledger value must be EXACTLY a
        // three-part version, with no trailing remainder `leading_version` would tolerate.
        assert_eq!(
            parsed.to_string(),
            raw,
            "the ledger's {needle} value is not a bare x.y.z version"
        );
        parsed
    }

    #[test]
    fn the_baked_range_matches_the_ledger() {
        // build/version-compat.md is the authoritative source of truth (its own words); these
        // constants are a compile-time COPY of it. Widening the range there without updating
        // `cc_version.rs` must fail here rather than ship provenance that quietly still states
        // the old range.
        assert_eq!(
            CC_SUPPORTED_MIN,
            ledger_bound("MIN"),
            "CC_SUPPORTED_MIN has drifted from build/version-compat.md"
        );
        assert_eq!(
            CC_SUPPORTED_MAX,
            ledger_bound("MAX"),
            "CC_SUPPORTED_MAX has drifted from build/version-compat.md"
        );
    }

    #[test]
    fn the_range_is_well_formed() {
        assert!(
            CC_SUPPORTED_MIN <= CC_SUPPORTED_MAX,
            "the supported range is inverted"
        );
    }

    #[test]
    fn the_provenance_line_names_the_verified_range() {
        // The `--version` provenance line records both baked bounds, unconditionally.
        let line = supported_range_provenance();
        assert!(
            line.starts_with("verified against Claude Code "),
            "unexpected provenance prefix: {line}"
        );
        assert!(
            line.contains(&CC_SUPPORTED_MIN.to_string())
                && line.contains(&CC_SUPPORTED_MAX.to_string()),
            "the provenance line must name both bounds: {line}"
        );
    }

    #[test]
    fn tolerated_shapes_parse() {
        assert_eq!(leading_version("2.1.181"), Some(Version::new(2, 1, 181)));
        assert_eq!(leading_version("v2.1.181"), Some(Version::new(2, 1, 181)));
        assert_eq!(leading_version("  2.1.181 "), Some(Version::new(2, 1, 181)));
        // A trailing fourth component is truncated to the three-part prefix, matching the
        // shell regex's prefix match rather than rejecting the line.
        assert_eq!(leading_version("2.1.181.4"), Some(Version::new(2, 1, 181)));
    }
}
