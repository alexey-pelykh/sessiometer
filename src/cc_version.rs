// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! The supported-Claude-Code-range advisory (issue #715): tell the OPERATOR, on their own
//! machine, when the installed `claude` sits outside the range sessiometer's
//! reverse-engineered CC internals were verified against.
//!
//! `sessiometer` depends on reverse-engineered Claude Code internals — the keychain-service
//! derivation (#100) and the credential-refresh lifecycle (#101) — verified only within the
//! range recorded in `build/version-compat.md`. A CC release outside that range may have
//! silently changed them, and sessiometer would then target the wrong keychain item with no
//! other signal. Until #715 that check existed ONLY in the maintainer's pre-release process
//! (`scripts/check-cc-version.sh`, then run as a release gate by `build/release-checklist.md`;
//! demoted to an advisory provenance check in #716 once the #714 runtime canary landed), so a
//! user who upgraded `claude` AFTER installing a release got no signal at all. This module
//! moves that informed consent onto the user's machine, where the risk actually lands.
//!
//! **Advisory, never a gate.** Every failure mode — no `claude`, an unparseable
//! `--version`, a spawn error, a wedged binary — degrades to SILENCE. Nothing here can
//! block, delay, or fail an operation; the range is a caveat on behavior, not a
//! precondition for it.
//!
//! **No credential access.** The only external read is `claude --version`, whose stdout is
//! a version string. The keychain, `~/.claude.json`, and the account stash are never
//! touched, so this surface cannot leak a secret (issue #15) — it has none to leak.
//!
//! # Why the range is a pair of constants
//!
//! `build/version-compat.md` is the authoritative source of truth, but a SHIPPED binary
//! cannot read it: the ledger is a repository document, not an installed asset. The range
//! must therefore be baked in at compile time. It is baked as two [`Version`] constants
//! rather than by `include_str!`-ing and parsing the ledger at runtime, because that would
//! embed ~14 KB of prose in the binary and give a never-fails surface a runtime parse
//! failure path. Drift is instead caught in CI: the `the_baked_range_matches_the_ledger`
//! test makes the ledger a compile-time input of the TEST build and asserts the constants
//! still match it, so widening the range without updating this file is a red test rather
//! than a silently stale advisory. This mirrors the crate's existing committed-fixture
//! idiom (`stats.rs`, `daemon/snapshot_build.rs`).

use std::fmt;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::AsyncReadExt as _;
use tokio::process::Command;

use crate::paths;

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

/// How long the `claude --version` probe may take before it is abandoned.
///
/// AC-3 ("never blocks operation") is not satisfied by merely ignoring the RESULT — a
/// wedged `claude` would still hang `sessiometer status` indefinitely while the advisory
/// waited on it. The probe is measured at ~0.05 s, so 2 s is ~40× headroom: generous enough
/// that a merely slow machine still gets its advisory, short enough that a hung binary
/// costs a noticeable-but-bounded pause instead of the whole command. On expiry the child
/// is killed (`kill_on_drop`) and the advisory goes silent.
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// The most stdout the `claude --version` probe reads before giving up on finding a version.
///
/// `claude --version` prints ~20 bytes (`x.y.z (Claude Code)\n`), so 64 KiB is orders of
/// magnitude above any realistic multi-line banner yet hard-bounds what a pathological or
/// hostile `claude` / `$CLAUDE_BIN` / `[refresh].claude_bin` can make this advisory allocate.
/// It is the memory counterpart of [`PROBE_TIMEOUT`]'s time bound: without it,
/// `Command::output()` accumulates the child's ENTIRE stdout, so a binary streaming
/// `/dev/zero` grows the heap by gigabytes-per-second until the timeout — which on a
/// memory-constrained host could OOM the very operation this advisory promised never to block.
/// Reading a capped prefix instead makes a flooding child fill the OS pipe buffer and block on
/// write, not our heap. Paired with a nulled stderr in [`installed_version`], so neither of the
/// child's streams is an unbounded sink.
const READ_CAP: u64 = 64 * 1024;

/// A three-part `major.minor.patch` Claude Code version.
///
/// Ordered NUMERICALLY by field, in declaration order — the derived [`Ord`] compares
/// `major`, then `minor`, then `patch`. That is the whole reason this is a struct rather
/// than the raw strings the ledger holds: lexicographically `"2.1.99" > "2.1.181"`, which
/// would place a genuinely-too-old CC inside the range.
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

/// Whether `installed` sits within the verified range, inclusive of both bounds.
///
/// The bounds are themselves verified points (`build/version-compat.md` records the walks at
/// `2.1.181` and `2.1.217`), so both ends are IN range.
fn in_supported_range(installed: Version) -> bool {
    installed >= CC_SUPPORTED_MIN && installed <= CC_SUPPORTED_MAX
}

/// The advisory line for `installed`, or `None` when it is in range.
///
/// One line, non-alarming, and deliberately UNDIRECTIONAL — below-range and above-range
/// produce the same text. The operator-facing point is the caveat ("behavior is
/// best-effort"), not the remediation: widening the range means re-walking the
/// reverse-engineered findings, which is a MAINTAINER action, and
/// `scripts/check-cc-version.sh` already carries that directional guidance for the person
/// who can act on it. Prefixed `advisory:` to match the crate's other operator advisories.
fn advisory_for(installed: Version) -> Option<String> {
    if in_supported_range(installed) {
        return None;
    }
    Some(format!(
        "advisory: Claude Code {installed} is outside the verified range \
         {CC_SUPPORTED_MIN}-{CC_SUPPORTED_MAX} — behavior is best-effort"
    ))
}

/// Parse a leading `major.minor.patch` from one line, tolerating leading whitespace, an
/// optional `v` prefix, and any trailing remainder.
///
/// The trailing remainder is the point: `claude --version` prints `2.1.218 (Claude Code)`.
/// Mirrors the anchored `^[[:space:]]*v?[0-9]+\.[0-9]+\.[0-9]+` regex in
/// `scripts/check-cc-version.sh`, so the two consumers of the same command agree on what
/// counts as a version for every version `claude` can actually print — including that a
/// version-like number appearing LATER in the line cannot hijack the parse. They diverge
/// only past the edge of reachability: a component ≥ 2³² overflows this `u32` parse to `None`
/// (silence) where the shell's string `grep` would still match — unreachable via a real
/// `claude` (~20-byte output), and toward the safe direction (an unparseable version says
/// nothing rather than guessing).
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

/// The first line of `raw` that STARTS with a version, or `None`.
///
/// Scans line-by-line rather than searching anywhere in the blob so that a banner, a
/// deprecation notice, or an update nag printed above the version cannot supply a false
/// match — the same "anchor to line start, first match wins" discipline
/// `scripts/check-cc-version.sh` uses.
fn parse_version_output(raw: &str) -> Option<Version> {
    raw.lines().find_map(leading_version)
}

/// Run `<bin> --version` and parse the version it prints; `None` on any failure.
///
/// Reads a [`READ_CAP`]-bounded prefix of STDOUT and ignores the exit status — matching
/// `scripts/check-cc-version.sh`, which sends stderr to `/dev/null` and parses whatever
/// stdout carried regardless of status. Both of the child's streams are bounded sinks, the
/// same discipline the crate's credential-bearing spawn uses (`isolated_spawn` nulls both):
/// stderr is `Stdio::null()` (so a stderr flood is discarded by the kernel, never buffered —
/// the `2>/dev/null` the shell script relies on), and stdout is read through
/// `AsyncReadExt::take` so a stdout flood fills the OS pipe buffer and blocks the child on
/// write rather than growing this process's heap. stdin is nulled so a `claude` that reads
/// stdin gets EOF instead of inheriting the terminal. `kill_on_drop` pairs with the
/// [`PROBE_TIMEOUT`] in [`range_advisory`] AND with the capped read: dropping `child` on
/// either the timeout or a short read kills and reaps it rather than leaving it behind.
async fn installed_version(bin: &Path) -> Option<Version> {
    let mut child = Command::new(bin)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .ok()?;
    let stdout = child.stdout.take()?;
    let mut buf = Vec::new();
    // A capped read: only the first line's leading `x.y.z` matters, so reading past 64 KiB
    // could not change the verdict and would only expose the unbounded-allocation path.
    stdout.take(READ_CAP).read_to_end(&mut buf).await.ok()?;
    parse_version_output(&String::from_utf8_lossy(&buf))
}

/// The operator-facing supported-range advisory, or `None` when there is nothing to say.
///
/// `None` — stay silent — covers BOTH "the installed CC is in range" and "the installed CC
/// could not be determined" (no `claude` on `$PATH`, a configured binary that no longer
/// exists, an unparseable `--version`, a spawn failure, a probe that outran
/// [`PROBE_TIMEOUT`]). Collapsing those into one silent arm is deliberate: an advisory that
/// itself started reporting failures would become the noise it exists to avoid, and it has
/// no standing to complain about a `claude` the operator may not even use.
///
/// `config_bin` threads the `[refresh].claude_bin` override through
/// [`paths::claude_binary_with_override`], so the daemon — which has a loaded `Config` —
/// advises about the very binary its refresh engine spawns. The thin `status` client passes
/// `None` (it deliberately loads no config, and gaining a config-parse failure mode for an
/// advisory would be a bad trade), which resolves `$CLAUDE_BIN` → `$PATH` — the same
/// resolution `scripts/check-cc-version.sh` documents for itself.
pub(crate) async fn range_advisory(config_bin: Option<&Path>) -> Option<String> {
    let bin = paths::claude_binary_with_override(config_bin).ok()?;
    let installed = tokio::time::timeout(PROBE_TIMEOUT, installed_version(&bin))
        .await
        .ok()??;
    advisory_for(installed)
}

#[cfg(test)]
mod tests {
    use super::{
        advisory_for, in_supported_range, installed_version, leading_version, parse_version_output,
        Version, CC_SUPPORTED_MAX, CC_SUPPORTED_MIN, READ_CAP,
    };
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    /// The authoritative range ledger, made a compile-time input of the TEST build so the
    /// baked constants cannot drift from it unnoticed. Same idiom as the committed wire
    /// goldens (`stats.rs`, `daemon/snapshot_build.rs`).
    const LEDGER: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/build/version-compat.md"
    ));

    /// The `- CC_SUPPORTED_{MIN,MAX}: x.y.z` value from the ledger, parsed with the same
    /// [`leading_version`] the runtime uses. Skips the HTML comment that documents the
    /// format by requiring the `-` list-item prefix — exactly the discriminator
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
        // `cc_version.rs` must fail here rather than ship an advisory that quietly still
        // advises against the old range.
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
    fn an_in_range_version_is_silent() {
        // AC-3: silent when in range. Both BOUNDS are verified points, so both are in range.
        assert_eq!(advisory_for(CC_SUPPORTED_MIN), None);
        assert_eq!(advisory_for(CC_SUPPORTED_MAX), None);
        assert_eq!(advisory_for(Version::new(2, 1, 200)), None);
    }

    #[test]
    fn an_out_of_range_version_advises() {
        // AC-1: one non-alarming line naming the installed version and the verified range.
        for installed in [
            Version::new(2, 1, 180), // one patch below MIN
            Version::new(2, 1, 218), // one patch above MAX
            Version::new(2, 0, 999), // a lower MINOR, however high its patch
            Version::new(3, 0, 0),   // a higher MAJOR
            Version::new(1, 9, 999), // a lower MAJOR
        ] {
            let advisory = advisory_for(installed)
                .unwrap_or_else(|| panic!("{installed} is out of range but stayed silent"));
            assert!(
                advisory.contains(&installed.to_string()),
                "the advisory must name the installed version: {advisory}"
            );
            assert!(
                advisory.contains(&format!("{CC_SUPPORTED_MIN}-{CC_SUPPORTED_MAX}")),
                "the advisory must name the verified range: {advisory}"
            );
            assert!(
                advisory.starts_with("advisory: "),
                "the advisory must carry the crate's advisory prefix: {advisory}"
            );
            assert_eq!(
                advisory.lines().count(),
                1,
                "the advisory must be a SINGLE line: {advisory}"
            );
        }
    }

    #[test]
    fn range_membership_is_numeric_not_lexicographic() {
        // The regression this struct exists to prevent: as STRINGS "2.1.99" > "2.1.181", which
        // would place a genuinely-too-old CC inside the range and silence the advisory.
        assert!(Version::new(2, 1, 99) < CC_SUPPORTED_MIN);
        assert!(!in_supported_range(Version::new(2, 1, 99)));
        assert!(advisory_for(Version::new(2, 1, 99)).is_some());
        // The mirror at the top: "2.1.9" as a string sorts above "2.1.217".
        assert!(Version::new(2, 1, 9) < CC_SUPPORTED_MAX);
    }

    #[test]
    fn the_real_claude_version_line_parses() {
        // The exact shape `claude --version` prints.
        assert_eq!(
            parse_version_output("2.1.218 (Claude Code)\n"),
            Some(Version::new(2, 1, 218))
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

    #[test]
    fn unparseable_output_is_silent() {
        // Every one of these degrades to `None` — the advisory never speaks on a guess.
        for raw in [
            "",
            "\n\n",
            "claude: command not found",
            "Claude Code 2.1.218", // version present but NOT at line start
            "2.1",                 // too few components
            "2.1.x",
            "v.1.2.3",
            "99999999999999999999.1.1", // a component that overflows u32
        ] {
            assert_eq!(
                parse_version_output(raw),
                None,
                "expected no version from {raw:?}"
            );
        }
    }

    #[test]
    fn only_a_line_initial_version_is_taken() {
        // A banner above the version must not be mistaken for it, and the FIRST anchored
        // line wins over any later one.
        assert_eq!(
            parse_version_output("Update available: 9.9.9\n2.1.218 (Claude Code)\n"),
            Some(Version::new(2, 1, 218))
        );
        assert_eq!(
            parse_version_output("2.1.218 (Claude Code)\n3.0.0 (other)\n"),
            Some(Version::new(2, 1, 218))
        );
    }

    /// Write `body` as a mode-0755 script named `claude` in a fresh tempdir. The returned
    /// guard removes the dir on drop, so it must outlive the [`installed_version`] call.
    /// Mirrors the executable-fixture idiom in `swap.rs` tests (`PermissionsExt` + `from_mode`).
    fn fake_claude(body: &str) -> (tempfile::TempDir, PathBuf) {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("claude");
        std::fs::write(&path, body).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        (dir, path)
    }

    #[tokio::test]
    async fn installed_version_reads_a_normal_version_line() {
        // The happy path of the real spawn structure (piped stdout, nulled stderr, capped read)
        // — the first automated coverage of the async I/O layer the pure tests above can't reach.
        let (_dir, path) = fake_claude("#!/bin/sh\necho '2.1.200 (Claude Code)'\n");
        assert_eq!(
            installed_version(&path).await,
            Some(Version::new(2, 1, 200))
        );
    }

    #[tokio::test]
    async fn installed_version_is_silent_for_a_missing_binary() {
        // A path that does not resolve → spawn fails → `None`, never a panic (AC-3).
        assert_eq!(
            installed_version(Path::new("/nonexistent/sessiometer/claude")).await,
            None
        );
    }

    #[tokio::test]
    async fn installed_version_ignores_stderr_only_output() {
        // stderr is `Stdio::null()`, so a version arriving ONLY on stderr is discarded by the
        // kernel and never parsed — the property that makes a stderr flood a non-event.
        let (_dir, path) = fake_claude("#!/bin/sh\necho '2.1.200 (Claude Code)' >&2\n");
        assert_eq!(installed_version(&path).await, None);
    }

    #[tokio::test]
    async fn the_read_cap_bounds_an_infinite_stdout_flood() {
        // The version arrives first, then an UNBOUNDED NUL flood (`cat /dev/zero` never exits).
        // The capped read must parse the leading version and return PROMPTLY. This is the
        // regression lock for the finding: pre-fix `.output()` waited for the never-exiting
        // child, so the whole-output timeout (not a hang) is the failure signal — and the
        // `exec` means the direct child IS `cat`, so `kill_on_drop` reaps it cleanly.
        let (_dir, path) =
            fake_claude("#!/bin/sh\nprintf '2.1.200 (Claude Code)\\n'\nexec cat /dev/zero\n");
        let got = tokio::time::timeout(Duration::from_secs(5), installed_version(&path)).await;
        assert!(
            got.is_ok(),
            "installed_version did not honor READ_CAP ({READ_CAP} B) — it hung on an unbounded stdout flood"
        );
        assert_eq!(got.unwrap(), Some(Version::new(2, 1, 200)));
    }
}
