// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! Relative-duration (`<non-negative int><unit>`) parsing — the single copy of the `--since`
//! span grammar `reliability` (issue #494) and `log` (issue #773) share.
//!
//! Hand-rolled on purpose (the minimal-dependency line, `CONTRIBUTING.md`): a `<int><unit>`
//! span is not worth a date crate in a credential-adjacent supply chain. Lifted here from
//! `reliability`, unchanged in behaviour, so the second offline reader reuses the grammar
//! rather than growing a copy that could drift.
//!
//! The parser returns [`Option`], not a `Result`, deliberately: the *grammar* is shared but the
//! *diagnosis* is per-verb. Each caller maps `None` onto its own error variant
//! ([`crate::error::Error::ReliabilitySinceInvalid`], [`crate::error::Error::LogSinceInvalid`]),
//! so an operator is told which flag they mistyped without this module knowing any verb.
//!
//! `stats --since` deliberately keeps its own parser and is NOT folded in here: its grammar is a
//! superset (a relative offset **or** an absolute `YYYY-MM-DD` / RFC 3339 instant) resolved by
//! *fall-through* — an input this module rejects is one `stats` must still try to read as a date.
//! Sharing the strict half would not remove that fall-through, so the copy it would save is not
//! the copy that matters.

/// Parse a relative-duration `<non-negative int><unit>` into whole seconds; `None` when the
/// input is not that grammar.
///
/// Units are `s`/`m`/`h`/`d`/`w` (seconds/minutes/hours/days/weeks). Surrounding whitespace is
/// trimmed. Rejected as `None`: an empty string, a missing or unknown unit (`7`, `7x`), and a
/// non-integer, negative, or empty count (`-1d`, `d`, `1.5h`) — `parse::<u64>` inherently rejects
/// a NEGATIVE sign, an empty string, and any non-digit, so no separate guard is needed. A leading
/// `+` is the one sign it does accept, so `+7d` resolves as `7d`; that is inherited from the
/// `reliability` parser this was lifted from, and harmless — it denotes the same span. Matching is
/// case-sensitive: `7D` is not `7d`.
///
/// Saturating multiply, so an absurd count yields [`u64::MAX`] — which callers clamp into a
/// cutoff — rather than overflowing.
pub(crate) fn parse_duration_secs(raw: &str) -> Option<u64> {
    let s = raw.trim();
    let unit = s.chars().last()?;
    let per_unit: u64 = match unit {
        's' => 1,
        'm' => 60,
        'h' => 3_600,
        'd' => 86_400,
        'w' => 7 * 86_400,
        _ => return None,
    };
    // The count is everything before the unit char; `len_utf8` keeps the slice on a char
    // boundary even when the trailing char is multi-byte (a rejected unit, but not a panic).
    let digits = &s[..s.len() - unit.len_utf8()];
    let n: u64 = digits.parse().ok()?;
    Some(n.saturating_mul(per_unit))
}

#[cfg(test)]
mod tests {
    use super::parse_duration_secs;

    #[test]
    fn every_unit_scales_as_documented() {
        assert_eq!(parse_duration_secs("30s"), Some(30));
        assert_eq!(parse_duration_secs("30m"), Some(1_800));
        assert_eq!(parse_duration_secs("24h"), Some(86_400));
        assert_eq!(parse_duration_secs("7d"), Some(604_800));
        assert_eq!(parse_duration_secs("2w"), Some(1_209_600));
    }

    #[test]
    fn zero_is_a_valid_span() {
        // `0` is a non-negative integer, so it parses — it simply selects "now onward".
        assert_eq!(parse_duration_secs("0d"), Some(0));
    }

    #[test]
    fn surrounding_whitespace_is_trimmed() {
        assert_eq!(parse_duration_secs("  7d  "), Some(604_800));
    }

    #[test]
    fn malformed_input_is_rejected() {
        // The three shapes issue #773's acceptance names explicitly, plus their neighbours.
        assert_eq!(parse_duration_secs("7x"), None, "unknown unit");
        assert_eq!(parse_duration_secs("-1d"), None, "negative count");
        assert_eq!(parse_duration_secs(""), None, "empty input");
        assert_eq!(parse_duration_secs("   "), None, "whitespace-only input");
        assert_eq!(parse_duration_secs("7"), None, "missing unit");
        assert_eq!(parse_duration_secs("d"), None, "missing count");
        assert_eq!(parse_duration_secs("1.5h"), None, "non-integer count");
        assert_eq!(parse_duration_secs("7 d"), None, "internal whitespace");
    }

    #[test]
    fn a_leading_plus_is_accepted_as_the_same_span() {
        // `u64::from_str` rejects `-` but ACCEPTS `+`, so `+7d` resolves as `7d`. Inherited from
        // the `reliability` parser this was lifted from and left unchanged — the lift is a move,
        // not a redesign — and harmless, since it denotes the same span. Pinned so the quirk is a
        // documented decision rather than an undiscovered one.
        assert_eq!(parse_duration_secs("+7d"), parse_duration_secs("7d"));
        assert_eq!(parse_duration_secs("+7d"), Some(604_800));
        // The negative sign is still rejected — that is the half that matters.
        assert_eq!(parse_duration_secs("-7d"), None);
    }

    #[test]
    fn unit_matching_is_case_sensitive() {
        assert_eq!(parse_duration_secs("7D"), None);
        assert_eq!(parse_duration_secs("24H"), None);
    }

    #[test]
    fn an_absurd_count_saturates_instead_of_overflowing() {
        // The documented saturation: a count that would overflow `u64` seconds yields MAX,
        // which every caller clamps into a cutoff. Must not panic in a release-mode overflow
        // check, and must not wrap into a small (future-dated) span.
        assert_eq!(
            parse_duration_secs("99999999999999999999999w"),
            None,
            "count exceeds u64"
        );
        assert_eq!(parse_duration_secs("18446744073709551615w"), Some(u64::MAX));
    }

    #[test]
    fn a_multibyte_trailing_char_is_rejected_without_panicking() {
        // `len_utf8` (not a bare `-1`) keeps the count slice on a char boundary, so a
        // multi-byte trailing char is a clean rejection rather than a slice panic.
        assert_eq!(parse_duration_secs("7é"), None);
        assert_eq!(parse_duration_secs("7日"), None);
    }
}
