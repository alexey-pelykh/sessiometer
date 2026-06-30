// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! The output-redaction METER (issue #15): the gate that proves no token, no
//! credential blob, and no raw email ever reaches an operator-facing daemon
//! output channel.
//!
//! Two guarantees, enforced at two different times:
//!
//! ## 1. Compile time — secret-carrying types are not printable
//!
//! [`Credential`], [`OauthAccount`] and [`StashedAccount`] each carry secret
//! material (a bearer token, an account email, or both). None may derive or
//! implement [`Debug`], so a stray `{:?}` can never format one onto a channel.
//! The guard just below fails the build the instant any of them gains a `Debug`
//! impl — a mechanical backstop for the hand-maintained "no `Debug`" notes on
//! each type. The bearer secret is additionally zeroized on drop: [`Credential`]
//! wraps `Zeroizing<Vec<u8>>` ([`crate::keychain`]), and every transient token
//! the poller derives is a `Zeroizing<String>` ([`crate::usage`]).
//!
//! ## 2. Test time (CI) — emitted output carries no secret
//!
//! The `meter` engine scans a corpus of ALL emitted output for four classes of
//! leak:
//!   - an OAuth token prefix (`sk-ant-…`), or a known token verbatim;
//!   - a fingerprint of a known injected credential blob — its leading bytes (a
//!     raw-blob leak) and its sha256 prefix (a "redacted to a hash" leak);
//!   - an `@`-bearing, email-shaped token (account identity is a stable, non-PII
//!     handle — never the email);
//!   - as a backstop for any secret that matches none of those exact patterns, a
//!     long, high-entropy run.
//!
//! The corpus is driven by the daemon's full poll→decide→swap loop under fault
//! injection. That driver lives with the loop it exercises — see
//! [`crate::daemon`]'s `redaction_meter_*` test — and feeds every channel (the
//! event log via [`crate::observability::Event::to_log_line`], the `status` text
//! and `list` view via [`crate::cli`], error `Display`, and the UDS control
//! replies) through `meter::scan`. This module owns the engine and proves it is
//! non-vacuous: its own tests plant each leak class and assert the scan catches
//! it.
//!
//! Out of scope here: the argv `-w` exposure of the keychain CLI, a distinct
//! surface tracked and minimized separately (issue #39). The METER covers OUTPUT
//! channels, not this process's argv.

use crate::claude_state::OauthAccount;
use crate::keychain::Credential;
use crate::stash::StashedAccount;

// Compile-time guard (issue #15): the secret-carrying types must NOT implement
// `Debug`.
//
// The trick is coherence-based. `AmbiguousIfDebug<A>` is implemented for every
// type at `A = ()`, and additionally — only for `Debug` types — at `A = u8`.
// Resolving `<T as AmbiguousIfDebug<_>>::check` with an inferred `A` is therefore
// unambiguous exactly when `T: !Debug`, and ambiguous (a compile error) the moment
// `T` gains a `Debug` impl. So a `#[derive(Debug)]` slipped onto any listed type
// fails the build. The closure is never called — its body is type-checked at
// definition, which is where the ambiguity (if any) is raised.
const _: fn() = || {
    trait AmbiguousIfDebug<A> {
        fn check() {}
    }
    impl<T: ?Sized> AmbiguousIfDebug<()> for T {}
    impl<T: ?Sized + core::fmt::Debug> AmbiguousIfDebug<u8> for T {}

    // Each line compiles only while the named type does NOT implement `Debug`.
    let _ = <Credential as AmbiguousIfDebug<_>>::check;
    let _ = <OauthAccount as AmbiguousIfDebug<_>>::check;
    let _ = <StashedAccount as AmbiguousIfDebug<_>>::check;
};

/// The redaction scan engine and the known-secret fixture it scans against.
///
/// `pub(crate)` so the daemon-side full-loop driver (and these tests) can feed a
/// harvested output corpus through [`scan`] / [`assert_clean`]. Test-only: the
/// engine is a CI gate, never runtime behavior.
#[cfg(test)]
pub(crate) mod meter {
    use crate::sha256::sha256_hex;

    /// The secrets injected into the full-loop corpus, plus the patterns derived
    /// from them that must never appear in emitted output. The full-loop test
    /// seeds the daemon's inputs (canonical store, stashes, `~/.claude.json`) from
    /// the SAME values, so a leak on any output channel would surface them here.
    pub(crate) struct Secrets {
        /// The canonical `Claude Code-credentials` blob (bearer-token JSON).
        blob: Vec<u8>,
        /// The account email that identifies the secrets' owner.
        email: String,
        /// The bearer-token strings embedded in `blob`, scanned for verbatim.
        tokens: Vec<String>,
    }

    impl Secrets {
        /// The fixture the full-loop METER injects: a realistic credential blob
        /// carrying two `sk-ant-…` tokens, plus a distinctive account email. The
        /// values are deliberately recognizable so a leak is unambiguous, and
        /// high-entropy so the entropy backstop would also catch the tokens.
        pub(crate) fn meter_fixture() -> Self {
            let access = "sk-ant-oat-METER0SECRET0ACCESS0bC9dE2fG7hJ4kL6mN8";
            let refresh = "sk-ant-ort-METER0SECRET0REFRESH0pQ3rS5tU7vW9xY1zA2";
            let email = "victim@meter-redaction.example";
            let blob = format!(
                r#"{{"claudeAiOauth":{{"accessToken":"{access}","refreshToken":"{refresh}","expiresAt":1782777600}}}}"#
            )
            .into_bytes();
            Self {
                blob,
                email: email.to_owned(),
                tokens: vec![access.to_owned(), refresh.to_owned()],
            }
        }

        /// The canonical credential blob, for seeding the daemon's store/stashes.
        pub(crate) fn blob(&self) -> &[u8] {
            &self.blob
        }

        /// The account email, for seeding the daemon's `oauthAccount` inputs.
        pub(crate) fn email(&self) -> &str {
            &self.email
        }
    }

    /// One detected leak — which class fired and a short locating snippet.
    #[derive(Debug, PartialEq)]
    pub(crate) enum Finding {
        /// An OAuth token prefix (e.g. `sk-ant-…`) appears in the output.
        TokenPrefix { prefix: String, at: String },
        /// A known bearer-token string appears verbatim.
        KnownToken { at: String },
        /// The leading bytes of the known blob appear (a raw-blob leak).
        BlobLeadingBytes,
        /// The known blob's sha256 prefix appears (a "redacted to a hash" leak).
        BlobSha256 { hex_prefix: String },
        /// An `@`-bearing, email-shaped token appears.
        EmailShape { matched: String },
        /// The known account email appears verbatim.
        KnownEmail,
        /// A long, high-entropy run — the backstop for unrecognized secret formats.
        HighEntropyRun { run: String, entropy: f64 },
    }

    /// OAuth token prefixes that must never surface. `sk-ant-` covers the
    /// Anthropic token family (`sk-ant-oat-`, `sk-ant-ort-`, `sk-ant-api…`).
    const TOKEN_PREFIXES: &[&str] = &["sk-ant-"];
    /// Bytes of the blob's leading prefix used as its raw-leak fingerprint.
    const BLOB_LEADING_BYTES: usize = 24;
    /// Hex chars of the blob's sha256 used as its hashed-leak fingerprint.
    const SHA256_HEX_PREFIX: usize = 16;
    /// Entropy backstop: a run of at least this many `[A-Za-z0-9]` chars …
    const ENTROPY_MIN_RUN: usize = 20;
    /// … with at least this Shannon entropy (bits/char) is flagged a regression.
    const ENTROPY_MIN_BITS: f64 = 3.5;

    /// Scan `corpus` for every class of secret leak, returning all findings
    /// (empty ⇒ clean). Pure, so the meter's own tests can plant a leak and
    /// assert it is caught.
    pub(crate) fn scan(corpus: &str, secrets: &Secrets) -> Vec<Finding> {
        let mut findings = Vec::new();

        // 1. OAuth token prefixes, then the known tokens verbatim.
        for prefix in TOKEN_PREFIXES {
            if let Some(idx) = corpus.find(prefix) {
                findings.push(Finding::TokenPrefix {
                    prefix: (*prefix).to_owned(),
                    at: snippet(corpus, idx),
                });
            }
        }
        for token in &secrets.tokens {
            if let Some(idx) = corpus.find(token.as_str()) {
                findings.push(Finding::KnownToken {
                    at: snippet(corpus, idx),
                });
            }
        }

        // 2. Blob fingerprint — leading bytes (raw leak) + sha256 prefix (hashed).
        if let Ok(blob_str) = std::str::from_utf8(secrets.blob()) {
            let lead_end = blob_str
                .char_indices()
                .nth(BLOB_LEADING_BYTES)
                .map_or(blob_str.len(), |(i, _)| i);
            let lead = &blob_str[..lead_end];
            if !lead.is_empty() && corpus.contains(lead) {
                findings.push(Finding::BlobLeadingBytes);
            }
        }
        let sha = sha256_hex(secrets.blob());
        let sha_prefix = &sha[..SHA256_HEX_PREFIX.min(sha.len())];
        if corpus.contains(sha_prefix) {
            findings.push(Finding::BlobSha256 {
                hex_prefix: sha_prefix.to_owned(),
            });
        }

        // 3. Email — any `@`-bearing email-shaped token, then the known email.
        if let Some(matched) = first_email_shape(corpus) {
            findings.push(Finding::EmailShape { matched });
        }
        if corpus.contains(secrets.email()) {
            findings.push(Finding::KnownEmail);
        }

        // 4. Entropy backstop — the longest high-entropy alnum run, if any.
        if let Some((run, entropy)) = highest_entropy_run(corpus) {
            findings.push(Finding::HighEntropyRun { run, entropy });
        }

        findings
    }

    /// Assert `corpus` carries no secret leak (the METER gate); panics with the
    /// findings on any leak.
    pub(crate) fn assert_clean(corpus: &str, secrets: &Secrets) {
        let findings = scan(corpus, secrets);
        assert!(
            findings.is_empty(),
            "redaction METER (#15): emitted output leaked a secret: {findings:#?}"
        );
    }

    /// A short, char-boundary-safe window of `corpus` starting at byte `idx`, for
    /// a finding's locating context.
    fn snippet(corpus: &str, idx: usize) -> String {
        let end = corpus[idx..]
            .char_indices()
            .nth(40)
            .map_or(corpus.len(), |(i, _)| idx + i);
        corpus[idx..end].to_owned()
    }

    /// The first `@`-bearing, email-shaped token in `corpus` (`local@domain.tld`),
    /// or `None`. Stricter than a bare `@` search so an operator label that merely
    /// contains an `@` is not flagged — only an actual email shape is.
    fn first_email_shape(corpus: &str) -> Option<String> {
        for (at, _) in corpus.match_indices('@') {
            // Local part: the maximal run of email-local chars ending at `@`.
            let local_start = corpus[..at]
                .char_indices()
                .rev()
                .take_while(|&(_, c)| is_email_local(c))
                .last()
                .map(|(i, _)| i);
            let Some(local_start) = local_start else {
                continue; // nothing email-shaped immediately before the `@`
            };
            // Domain: the maximal run of domain chars beginning after `@`.
            let after = &corpus[at + 1..];
            let domain_end = after
                .char_indices()
                .take_while(|&(_, c)| is_domain_char(c))
                .last()
                .map_or(0, |(i, c)| i + c.len_utf8());
            let domain = &after[..domain_end];
            if domain_has_tld(domain) {
                return Some(format!("{}@{}", &corpus[local_start..at], domain));
            }
        }
        None
    }

    /// A character valid in an email local-part (the conservative common subset).
    fn is_email_local(c: char) -> bool {
        c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '%' | '+' | '-')
    }

    /// A character valid in a DNS hostname label or its dots.
    fn is_domain_char(c: char) -> bool {
        c.is_ascii_alphanumeric() || matches!(c, '.' | '-')
    }

    /// Whether `domain` looks like `host.tld` with a ≥2-letter alphabetic TLD —
    /// the shape that distinguishes an email from a bare `user@host`.
    fn domain_has_tld(domain: &str) -> bool {
        match domain.rsplit_once('.') {
            Some((host, tld)) => {
                !host.is_empty() && tld.len() >= 2 && tld.chars().all(|c| c.is_ascii_alphabetic())
            }
            None => false,
        }
    }

    /// The highest-entropy `[A-Za-z0-9]` run in `corpus` that clears BOTH the
    /// length and entropy thresholds, or `None`. The length gate alone clears
    /// ordinary identifiers/words/UUID fragments; the entropy gate then ensures a
    /// long-but-structured run (a repeated-character pad) is not mistaken for a
    /// secret. A genuine token (a long, dense base64/hex run) clears both.
    fn highest_entropy_run(corpus: &str) -> Option<(String, f64)> {
        let mut best: Option<(String, f64)> = None;
        for run in corpus.split(|c: char| !c.is_ascii_alphanumeric()) {
            if run.len() < ENTROPY_MIN_RUN {
                continue;
            }
            let entropy = shannon_entropy(run);
            if entropy >= ENTROPY_MIN_BITS && best.as_ref().is_none_or(|(_, b)| entropy > *b) {
                best = Some((run.to_owned(), entropy));
            }
        }
        best
    }

    /// Shannon entropy (bits/char) of `s` over its byte-value frequencies. `s` is
    /// a non-empty ASCII-alnum run from [`highest_entropy_run`], so there is no
    /// division by zero.
    fn shannon_entropy(s: &str) -> f64 {
        let mut freq = [0u32; 256];
        for &b in s.as_bytes() {
            freq[b as usize] += 1;
        }
        let n = s.len() as f64;
        let mut entropy = 0.0;
        for &count in &freq {
            if count > 0 {
                let p = f64::from(count) / n;
                entropy -= p * p.log2();
            }
        }
        entropy
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        // --- Shannon entropy ---------------------------------------------------

        #[test]
        fn entropy_is_zero_for_a_single_repeated_symbol_and_maximal_for_distinct() {
            assert!(shannon_entropy("aaaaaaaaaa").abs() < 1e-9);
            // 16 distinct bytes → log2(16) = 4 bits/char.
            assert!((shannon_entropy("0123456789abcdef") - 4.0).abs() < 1e-9);
        }

        // --- email-shape detection --------------------------------------------

        #[test]
        fn email_shape_matches_a_real_address_and_ignores_non_emails() {
            assert_eq!(
                first_email_shape("hold=work victim@meter-redaction.example end"),
                Some("victim@meter-redaction.example".to_owned())
            );
            // A bare `@`, an `@` with no dotted TLD, and a label-like `@` are not
            // email-shaped — the precision that lets an operator handle carry an
            // `@` without a false positive.
            assert_eq!(first_email_shape("a @ b"), None);
            assert_eq!(first_email_shape("user@localhost"), None);
            assert_eq!(first_email_shape("@leading"), None);
        }

        // --- the meter, end to end (a clean corpus vs each planted leak) --------

        /// A representative CLEAN corpus: handles, percentages, an event line, a
        /// full UUID, a timestamp — every shape the real channels emit, none of
        /// them secret.
        const CLEAN: &str = "ts=2026-06-30T00:00:00Z event=swap from=work to=spare \
             reason=session session_pct=97\n\
             {\"accounts\":[{\"label\":\"work\",\"active\":true,\"session_pct\":97,\"weekly_pct\":40}],\
             \"next_swap\":{\"state\":\"target\",\"to\":\"spare\"}}\n\
             * work · session 97% · weekly 40%\n  spare · session 10% · weekly 20%\n\
             work  11111111-1111-1111-1111-111111111111\n\
             no Claude Code credential found in the keychain (capture an account first)\n";

        #[test]
        fn the_meter_passes_a_clean_corpus() {
            assert_eq!(scan(CLEAN, &Secrets::meter_fixture()), Vec::new());
        }

        #[test]
        fn the_meter_catches_a_leaked_token_prefix() {
            let secrets = Secrets::meter_fixture();
            let leaked = format!("{CLEAN}authorization: Bearer sk-ant-oat-LEAKED\n");
            let findings = scan(&leaked, &secrets);
            assert!(
                findings
                    .iter()
                    .any(|f| matches!(f, Finding::TokenPrefix { .. })),
                "a leaked sk-ant- prefix must be caught: {findings:#?}"
            );
        }

        #[test]
        fn the_meter_catches_a_leaked_known_token() {
            let secrets = Secrets::meter_fixture();
            // The verbatim access token from the fixture, embedded in output.
            let token = "sk-ant-oat-METER0SECRET0ACCESS0bC9dE2fG7hJ4kL6mN8";
            let leaked = format!("{CLEAN}debug: token={token}\n");
            let findings = scan(&leaked, &secrets);
            assert!(
                findings
                    .iter()
                    .any(|f| matches!(f, Finding::KnownToken { .. })),
                "a leaked known token must be caught: {findings:#?}"
            );
        }

        #[test]
        fn the_meter_catches_a_raw_blob_leak_by_its_leading_bytes() {
            let secrets = Secrets::meter_fixture();
            let blob = String::from_utf8(secrets.blob().to_vec()).unwrap();
            let leaked = format!("{CLEAN}dumped credential: {blob}\n");
            let findings = scan(&leaked, &secrets);
            assert!(
                findings.contains(&Finding::BlobLeadingBytes),
                "a raw blob dump must be caught by its leading bytes: {findings:#?}"
            );
        }

        #[test]
        fn the_meter_catches_a_blob_fingerprint_hashed_to_sha256() {
            let secrets = Secrets::meter_fixture();
            // Code that "redacts" a secret to sha256(secret) still leaks a stable,
            // correlatable fingerprint — the meter must catch the hash too.
            let sha = sha256_hex(secrets.blob());
            let leaked = format!("{CLEAN}credential fingerprint: {sha}\n");
            let findings = scan(&leaked, &secrets);
            assert!(
                findings
                    .iter()
                    .any(|f| matches!(f, Finding::BlobSha256 { .. })),
                "a sha256 fingerprint of the blob must be caught: {findings:#?}"
            );
        }

        #[test]
        fn the_meter_catches_a_leaked_email() {
            let secrets = Secrets::meter_fixture();
            let leaked = format!("{CLEAN}account: victim@meter-redaction.example\n");
            let findings = scan(&leaked, &secrets);
            assert!(
                findings
                    .iter()
                    .any(|f| matches!(f, Finding::EmailShape { .. })),
                "a leaked email must be caught by its shape: {findings:#?}"
            );
            assert!(
                findings.contains(&Finding::KnownEmail),
                "the known email must also be caught verbatim: {findings:#?}"
            );
        }

        #[test]
        fn the_meter_catches_an_unknown_secret_by_its_entropy() {
            let secrets = Secrets::meter_fixture();
            // A secret in NO recognized format (not sk-ant-, not the known blob,
            // not an email) — a long, dense base64-ish run. The entropy backstop is
            // the only thing that can catch it.
            let unknown = "Zm9vYmFyYmF6cXV4d29tYmF0MT234567890AbCdEfGh";
            let leaked = format!("{CLEAN}opaque={unknown}\n");
            let findings = scan(&leaked, &secrets);
            assert!(
                findings.iter().any(
                    |f| matches!(f, Finding::HighEntropyRun { entropy, .. } if *entropy >= 3.5)
                ),
                "an unrecognized high-entropy secret must be caught: {findings:#?}"
            );
        }

        #[test]
        fn the_entropy_backstop_does_not_flag_ordinary_low_entropy_runs() {
            // A long repeated-character run clears the LENGTH gate but not the
            // ENTROPY gate — so a structured pad is not mistaken for a secret.
            assert_eq!(highest_entropy_run("aaaaaaaaaaaaaaaaaaaaaaaaaaaa"), None);
            // A long UUID-ish digit run (after `-` splitting upstream) is likewise
            // low-entropy and unflagged.
            assert_eq!(highest_entropy_run("11111111111111111111111111"), None);
        }
    }
}
