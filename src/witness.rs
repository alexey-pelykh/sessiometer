// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! The prior-configuration witness (issue #1440, design D-1) — how an append-only roster
//! write tells *"this machine was never configured"* from *"this machine's configuration
//! disappeared"*.
//!
//! On 2026-08-27 `login` found no `config.toml`, concluded first run, wrote a one-account
//! roster and notified a daemon still holding six. An absent config is **ambiguous**, and
//! every write path resolved it the same way, unconditionally.
//!
//! The two obvious guards — consult the control socket, or refuse on an absent config
//! alone — are each dominated, and `docs/adr/0036-prior-configuration-witness.md` is where
//! that argument lives. The refusal reuses the ratified shape of `ConfigSetRejection::NoConfig`
//! (returned by `perform_config_set` in `src/daemon/commands.rs`) rather than inventing a
//! parallel one.
//!
//! # The rule
//!
//! Consult durable local state that is independent of **both** the config file and the
//! socket, and that a machine only acquires by having been configured:
//!
//! | witness | why it counts |
//! |---|---|
//! | any `Sessiometer/…` keychain item | all six survived the incident; read metadata-only, so no prompt and no decryption ([`crate::keychain::any_stash_item_present`]) |
//! | a non-empty usage sample store | survived too — and polling requires rostered accounts, so a populated store implies a roster existed |
//!
//! **Witness present → refuse. Witness absent → allow.** A genuine first run has neither,
//! so it is never prompted, never asked a question, and lands a roster identical to what
//! the unguarded path produces; what it pays is the observation itself — two `stat` calls
//! and one `security dump-keychain`, all of them silent.
//!
//! Scope: this module decides the rule and observes the usage-store half; the keychain half
//! is [`crate::keychain::any_stash_item_present`], where the rest of this crate's `security`
//! handling lives. WHERE the rule is applied is the caller's — today [`crate::capture`]'s
//! `login` and `capture` verbs (issue #1440); the socket-borne `capture` entry point takes
//! the same rule under issue #1441.
//!
//! A reachable, populated daemon would corroborate a refusal but is never what establishes
//! one, which is what keeps the rule correct with the daemon down. Corroboration is not
//! implemented: nothing here reads the socket, by design.

use crate::error::{Error, Result};
use crate::paths;
use std::path::{Path, PathBuf};

/// What durable local state says about whether this machine has been configured before.
///
/// Two-valued on purpose. A third *"could not tell"* state would have to resolve to one of
/// these at the call site anyway, and putting that resolution behind a name invites each
/// caller to answer it differently; [`WitnessSources::observe`] resolves it once, where
/// the reasoning is written down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PriorConfiguration {
    /// Durable state survives that only a configured machine produces.
    Present,
    /// Nothing survives. Either a genuine first run, or a loss that also took every
    /// witness — the design's accepted false-negative, whose second line of defence is the
    /// roster backup ring (issue #1439), not this module.
    Absent,
}

/// Where the witness is read from — the injectable seam over the two durable sources.
///
/// Production wires the real machine ([`WitnessSources::real`]); a test pins a throwaway
/// keychain and a temp-dir usage store, so the rule is asserted against real `security`
/// behaviour without touching the operator's login keychain.
pub(crate) struct WitnessSources {
    /// The keychain file to enumerate for `Sessiometer/…` items.
    keychain: PathBuf,
    /// Files whose NON-EMPTINESS is the second witness. A list rather than two fields
    /// because the rule is "any of these", and a list cannot grow a third source without
    /// the loop already handling it.
    usage_store: Vec<PathBuf>,
}

impl WitnessSources {
    /// The real machine: the login keychain plus both usage-store files.
    pub(crate) fn real() -> Result<Self> {
        Ok(Self {
            keychain: paths::login_keychain()?,
            usage_store: vec![paths::usage_samples()?, paths::usage_rollup()?],
        })
    }

    /// Sources pinned for a test — never the login keychain.
    #[cfg(test)]
    pub(crate) fn pinned(keychain: PathBuf, usage_store: Vec<PathBuf>) -> Self {
        Self {
            keychain,
            usage_store,
        }
    }

    /// Observe the witness.
    ///
    /// The usage store is checked FIRST because it is a `stat` and the keychain probe is a
    /// subprocess: on a machine that has been configured the cheap source usually answers,
    /// and the spawn is skipped. Correctness does not depend on the order — either source
    /// alone is sufficient.
    ///
    /// A keychain probe that ERRORS resolves to `Present`, with a diagnostic on stderr.
    /// This is the one direction this module chooses, and it fails CLOSED: only `Absent`
    /// permits the write, so *"cannot tell"* must not be spelled `Absent`. Note that this
    /// arm is reached only AFTER the usage store has already answered negative, so there
    /// is no second witness left to catch what it lets through — an error here is the last
    /// word, not one opinion among two.
    ///
    /// The cost of choosing `Present` is bounded by what else a broken probe breaks. The
    /// error path is `security dump-keychain`, which is also how
    /// [`crate::keychain::RealCredentialStore::resolve`] reads the canonical item's `acct`
    /// — so on a machine where it fails, `store.write` fails too, and an activating verb
    /// aborts whichever way this resolves. What a false refusal costs there is a less
    /// accurate message on a machine that was already going to fail. The opposite
    /// direction is not symmetric: `login <other>` with `activate == false` skips
    /// `store.write` entirely, so on that path this gate is the ONLY thing standing
    /// between a broken probe and the incident — resolving to `Absent` reproduces it.
    ///
    /// A genuine first run does not meet this arm at all: on a fresh machine the probe
    /// SUCCEEDS and simply finds nothing. The diagnostic is what keeps a machine that
    /// *does* meet it from being told only that it was refused.
    pub(crate) async fn observe(&self) -> PriorConfiguration {
        if self.usage_store.iter().any(|path| is_non_empty(path)) {
            return PriorConfiguration::Present;
        }
        resolve_probe(crate::keychain::any_stash_item_present(&self.keychain).await)
    }
}

/// A keychain probe's answer as a witness value — the fail-closed resolution, split out so
/// the `Err` arm can be exercised directly.
///
/// It cannot be reached by handing `WitnessSources::pinned` a bad path (that constructor is
/// test-only): measured against `/usr/bin/security` on macOS 26.5.2 / 25F84,
/// `security dump-keychain` exits 0 with EMPTY output for a nonexistent path, a junk file,
/// an empty file and a directory alike, and does not fall back to the login keychain. So an unreadable keychain arrives as `Ok(false)` — indistinguishable
/// from an empty one — and what actually produces `Err` is a non-zero exit or a failure to
/// spawn `security` at all.
///
/// That measurement is worth keeping written down: it means the keychain half degrades
/// SILENTLY to "no witness", which is the false negative the usage-store half exists to
/// cover, and it is why a test asserting this arm must construct the `Err` rather than
/// arrange for one.
fn resolve_probe(probe: Result<bool>) -> PriorConfiguration {
    match probe {
        Ok(true) => PriorConfiguration::Present,
        Ok(false) => PriorConfiguration::Absent,
        Err(err) => {
            eprintln!(
                "sessiometer: could not read the keychain half of the prior-configuration witness: {err}"
            );
            PriorConfiguration::Present
        }
    }
}

/// Whether `path` names a regular file with a non-zero length.
///
/// Metadata only — the contents are never read, so an operator's usage history is not
/// opened to answer a question about its existence. A zero-length file is NOT a witness:
/// it proves a store was created, never that a roster existed.
///
/// Length is therefore a LOWER bound on "something was written", not a count of samples,
/// and the two files differ in how tight that bound is. `usage-samples.jsonl` is empty
/// until its first sample. `usage-rollup.json` serialises a struct, so its empty state is
/// still a non-zero-length JSON object — for that file, existence and non-emptiness
/// coincide in practice. Both are deliberately admitted: a rolled store means a daemon
/// polled, and polling requires a roster.
fn is_non_empty(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|meta| meta.is_file() && meta.len() > 0)
}

/// The D-1 rule, pure over its two facts — the ONLY place the refusal is decided.
///
/// `config_present` is whether `config.toml` EXISTS, never whether it holds accounts: a
/// present file with an empty roster is the tunables-only first-account case, which is
/// unchanged from today and is not this rule's business.
///
/// A malformed or unreadable file never reaches here — it is already a hard error at the
/// load, and stays one.
pub(crate) fn admit(config_present: bool, witness: PriorConfiguration) -> Result<()> {
    match (config_present, witness) {
        // No file, yet the machine carries state only a configured machine produces.
        // Treating this as a first run is the incident.
        (false, PriorConfiguration::Present) => Err(Error::PriorConfigurationWithoutConfig),
        // No file and nothing else survives: a genuine first run, allowed exactly as today.
        (false, PriorConfiguration::Absent) => Ok(()),
        // The file is there. Appending to it is the ordinary path (an existing roster
        // widens, a tunables-only file takes its first account) and D-1 has nothing to
        // decide either way — which is what lets the caller skip the probe entirely.
        (true, _) => Ok(()),
    }
}

/// [`admit`] against observed sources — the production entry point for an append-only verb.
///
/// The witness is observed ONLY when the config is absent. That is not an optimisation
/// bolted onto the rule: [`admit`]'s `(true, _)` arm returns `Ok` for BOTH witness values,
/// so with the file present no observation can change the outcome, and probing would spawn
/// `security` on every capture to reach a verdict already fixed.
///
/// `sources` is a parameter rather than a [`WitnessSources::real`] call inside this body,
/// and that is a testability requirement rather than a style preference. Wrapping the real
/// sources here would leave this function's own body unreachable from a test — a mutation
/// replacing it with `Ok(())` would keep the whole suite green, so the guard would have no
/// gate at all. Callers pass `&WitnessSources::real()?`; construction is pure path
/// resolution and spawns nothing, so the present-config path stays free.
pub(crate) async fn admit_append_only(
    sources: &WitnessSources,
    config_present: bool,
) -> Result<()> {
    if config_present {
        return Ok(());
    }
    admit(false, sources.observe().await)
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- the rule (issue #1440, design D-1) ------------------------------------------

    #[test]
    fn an_absent_config_with_a_surviving_witness_is_refused() {
        // The incident: `config.toml` gone, durable state still saying the machine was
        // configured. This is the whole point of D-1, and the state in which today's
        // unguarded path writes a one-account roster over a live six-account one.
        let err = admit(false, PriorConfiguration::Present).unwrap_err();
        assert!(matches!(err, Error::PriorConfigurationWithoutConfig));
    }

    #[test]
    fn an_absent_config_with_no_witness_is_a_genuine_first_run() {
        // PRD AC-4, and the case the absent-config-alone route breaks: a fresh machine has
        // no stash and no usage store, so the verb proceeds exactly as it does today.
        assert!(admit(false, PriorConfiguration::Absent).is_ok());
    }

    #[test]
    fn a_present_config_is_admitted_whatever_the_witness_says() {
        // Both arms, because this pair is what makes `admit_append_only`'s skip-the-probe
        // shortcut provably equivalent to observing and then admitting. If either of these
        // ever became a refusal, that shortcut would silently start lying.
        assert!(admit(true, PriorConfiguration::Present).is_ok());
        assert!(admit(true, PriorConfiguration::Absent).is_ok());
    }

    #[test]
    fn the_refusal_names_an_ambient_prior_configuration_and_nothing_that_indexes_it() {
        // Issue #1440 AC: no account label, no path, no count, no keychain item name. A
        // refusal goes to stderr, which has a wider audience than the `0600` file it is
        // about — and the roster indexes credentials, so what it names matters even though
        // `config.toml` itself carries no secret.
        let message = Error::PriorConfigurationWithoutConfig.to_string();

        // No path and no keychain item name: every stash service is `Sessiometer/<uuid>`
        // and every path has a separator, so a single `/` is what both would carry.
        assert!(
            !message.contains('/'),
            "the refusal carries a `/`, which is either a path or a stash service name: {message}"
        );
        assert!(!message.contains("Sessiometer"));
        // No count: how many accounts survived is exactly the fact an operator's shoulder-
        // surfer does not get from a refusal.
        assert!(
            !message.chars().any(|c| c.is_ascii_digit()),
            "the refusal carries a digit, which is a count: {message}"
        );
        // It still SAYS the thing — a refusal that named nothing at all would be unactionable.
        // Pinned whole rather than by keyword: this message is an acceptance-criterion
        // surface, so any edit to it should fail here and be re-read against the AC rather
        // than slip through a `contains` that a reworded refusal still satisfies.
        assert_eq!(
            message,
            "refusing to write the roster: this machine carries a prior configuration but \
             config.toml is absent — restore it before capturing an account"
        );
    }

    #[test]
    fn the_witness_cannot_consult_the_control_socket() {
        // The rejected route, ruled out structurally rather than by intention. The design
        // turns on the witness holding WITH THE DAEMON DOWN — that is the entire reason it
        // dominates socket-consulting, which degrades to permissive there and so fails
        // silently in the one state it was chosen for. A module that cannot reach the socket
        // cannot regress into asking it.
        //
        // The daemon is still permitted to CORROBORATE a refusal; that would be the caller's
        // doing, at a call site where the fallback is visible, never hidden inside the rule.
        let source = witness_source_above_the_tests();
        for forbidden in [
            "control_socket",
            "UnixStream",
            "notify_roster_reload",
            "crate::daemon",
        ] {
            assert!(
                !source.contains(forbidden),
                "the witness reaches for `{forbidden}`; the rule has to hold with the daemon down"
            );
        }
        // Canary the corpus: a truncated read would satisfy every assertion above by having
        // read nothing (the absence-dual of a degenerate gate).
        assert!(source.contains("any_stash_item_present"));
    }

    /// This module's own source, above its test block — the subject of the structural
    /// assertion above.
    ///
    /// Cut at a column-0 `#[cfg(test)]`, which for THIS file is the test module and nothing
    /// else: it carries no `#[cfg(test)]` import at column 0, and the `pinned` constructor's
    /// attribute is indented inside the `impl`. The canary above is what keeps that true —
    /// if a future edit moves the boundary up, the read loses its subject and says so.
    fn witness_source_above_the_tests() -> String {
        let text = std::fs::read_to_string("src/witness.rs").expect("cannot read src/witness.rs");
        text.split("\n#[cfg(test)]\nmod tests")
            .next()
            .expect("split always yields a first element")
            .to_owned()
    }

    #[tokio::test]
    async fn the_witness_sees_an_item_named_the_way_the_roster_actually_names_them() {
        // The probe asks about `Sessiometer/…` items; `Account::stash` is what names the
        // ones the roster writes. Asserting that the two share a constant would be a
        // tautology — both read `STASH_PREFIX`, so they cannot drift — so this seeds a real
        // keychain with an item named by `Account::stash()` ITSELF and asks the real probe
        // whether it sees it. What that catches is a change to how an account is named that
        // the prefix constant does not capture, which would leave the witness hunting a
        // family nothing creates: a probe that can only ever answer `Absent`.
        let account = crate::config::Account {
            account_uuid: "11111111-1111-1111-1111-111111111111".to_owned(),
            label: "any".to_owned(),
            enabled: true,
        };
        let dir = tempfile::tempdir().unwrap();
        let keychain = empty_keychain(dir.path()).await;
        assert_eq!(
            WitnessSources::pinned(keychain.clone(), Vec::new())
                .observe()
                .await,
            PriorConfiguration::Absent,
            "the fixture keychain is not empty, so this proves nothing"
        );

        add_item(&keychain, &account.stash()).await;
        assert_eq!(
            WitnessSources::pinned(keychain.clone(), Vec::new())
                .observe()
                .await,
            PriorConfiguration::Present
        );

        delete_keychain(&keychain).await;
    }

    #[test]
    fn the_production_sources_are_the_ones_the_rest_of_the_crate_writes_to() {
        // `WitnessSources::real` is the production wiring, and until this test existed
        // nothing checked it: an independent verify gate pointed it at a nonexistent
        // keychain with an empty usage list — which makes the guard answer `Absent` on
        // every real machine, i.e. reproduces the incident — and the whole suite stayed
        // green. That is the same one-call-short gap `admit_append_only`'s own doc comment
        // describes, so it gets the same treatment.
        //
        // Compared against the SAME resolvers the writers use, not against literals: this
        // has to fail when the witness and the writer drift apart, which a hard-coded path
        // could not detect.
        let sources = WitnessSources::real().expect("the real sources resolve on this machine");
        assert_eq!(
            sources.keychain,
            paths::login_keychain().unwrap(),
            "the witness reads a different keychain than `stash` writes to, so it cannot see a stash"
        );
        assert_eq!(
            sources.usage_store,
            vec![paths::usage_samples().unwrap(), paths::usage_rollup().unwrap()],
            "the witness watches different files than the usage store writes, so it cannot see a sample"
        );
    }

    // --- the usage-store half --------------------------------------------------------

    #[test]
    fn a_populated_usage_store_is_a_witness_and_an_empty_one_is_not() {
        let dir = tempfile::tempdir().unwrap();
        let samples = dir.path().join("usage-samples.jsonl");

        // Absent entirely — a fresh machine.
        assert!(!is_non_empty(&samples));

        // Created but empty: the daemon started and no sample has landed. That proves a
        // daemon ran, NOT that a roster existed, so it must not fire the guard.
        std::fs::write(&samples, b"").unwrap();
        assert!(!is_non_empty(&samples));

        // One sample: polling requires rostered accounts, so the roster existed.
        std::fs::write(&samples, b"{\"ts\":0}\n").unwrap();
        assert!(is_non_empty(&samples));

        // A DIRECTORY at the path is not a file and is not a witness — a `len()` test
        // alone would read a directory's size as a positive.
        let as_dir = dir.path().join("usage-rollup.json");
        std::fs::create_dir(&as_dir).unwrap();
        assert!(!is_non_empty(&as_dir));
    }

    #[tokio::test]
    async fn either_usage_store_file_alone_carries_the_witness() {
        // "Two independent witnesses, either sufficient" applies WITHIN the usage store
        // too: the rollup outlives the sample series (samples are pruned, the rollup is
        // not), so keying only on the sample file would lose the witness on exactly the
        // machine that has been configured longest.
        //
        // Both arms are exercised against a REAL, EMPTY keychain, so the keychain half
        // genuinely answers `Ok(false)` and the store is demonstrably what carried the
        // verdict. Pointing at a nonexistent keychain would not show that: the probe
        // errors, and an error is now `Present` on its own.
        let dir = tempfile::tempdir().unwrap();
        let keychain = empty_keychain(dir.path()).await;
        let samples = dir.path().join("usage-samples.jsonl");
        let rollup = dir.path().join("usage-rollup.json");
        let both = vec![samples.clone(), rollup.clone()];

        // Neither file: the empty keychain agrees, so this is a genuine first run.
        let sources = WitnessSources::pinned(keychain.clone(), both.clone());
        assert_eq!(sources.observe().await, PriorConfiguration::Absent);

        // The rollup alone. `{"daily":[]}` is what an EMPTY rollup serialises to — a
        // non-zero-length file — which is exactly the state this arm has to admit.
        std::fs::write(&rollup, b"{\"daily\":[]}").unwrap();
        let sources = WitnessSources::pinned(keychain.clone(), both.clone());
        assert_eq!(sources.observe().await, PriorConfiguration::Present);

        // The samples file alone.
        std::fs::remove_file(&rollup).unwrap();
        std::fs::write(&samples, b"{\"ts\":0}\n").unwrap();
        let sources = WitnessSources::pinned(keychain.clone(), both);
        assert_eq!(sources.observe().await, PriorConfiguration::Present);

        delete_keychain(&keychain).await;
    }

    #[test]
    fn a_keychain_probe_failure_is_a_witness_rather_than_a_clean_bill() {
        // The fail-CLOSED direction, asserted rather than left to the doc comment. Only
        // `Absent` permits the write, so "cannot tell" must not resolve there: this arm is
        // reached only after the usage store already answered negative, leaving no second
        // witness to catch what it would let through.
        //
        // The error is CONSTRUCTED, not arranged. An earlier version of this test pointed
        // at a nonexistent keychain and asserted the direction it wanted; that path exits 0
        // with empty output (see `resolve_probe`), so it was asserting over `Ok(false)` and
        // would have passed whichever way the `Err` arm resolved.
        let probe = Err(Error::Keychain {
            op: "prior-configuration witness",
            code: 1,
        });
        assert_eq!(resolve_probe(probe), PriorConfiguration::Present);
        // …and the rule then refuses, which is the property that actually matters.
        assert!(matches!(
            admit(false, PriorConfiguration::Present).unwrap_err(),
            Error::PriorConfigurationWithoutConfig
        ));
    }

    #[tokio::test]
    async fn an_unreadable_keychain_degrades_to_no_witness_and_the_store_is_the_backstop() {
        // The measured reality, pinned so nobody re-derives it wrongly: `dump-keychain` on
        // a path that is not a readable keychain exits 0 with empty output, so the probe
        // returns `Ok(false)` and the keychain half degrades SILENTLY to "no witness" —
        // no error to fail closed on.
        let dir = tempfile::tempdir().unwrap();
        let unreadable = dir.path().join("no-such.keychain-db");
        let bare = WitnessSources::pinned(unreadable.clone(), Vec::new());
        assert_eq!(bare.observe().await, PriorConfiguration::Absent);

        // Which is exactly what the second, independent witness is for.
        let samples = dir.path().join("usage-samples.jsonl");
        std::fs::write(&samples, b"{\"ts\":0}\n").unwrap();
        let backstopped = WitnessSources::pinned(unreadable, vec![samples]);
        assert_eq!(backstopped.observe().await, PriorConfiguration::Present);
    }

    // --- the production entry point --------------------------------------------------

    #[tokio::test]
    async fn the_gate_refuses_allows_and_skips_against_observed_sources() {
        // `admit_append_only` is what the verbs actually call, and until it took its
        // sources as a parameter nothing exercised its body: a mutation replacing it with
        // `Ok(())` left the whole suite green. All three of its outcomes are asserted here
        // THROUGH it, not through `admit`.
        let dir = tempfile::tempdir().unwrap();
        let keychain = empty_keychain(dir.path()).await;
        let samples = dir.path().join("usage-samples.jsonl");
        let store = vec![samples.clone()];

        // No config, no witness → a genuine first run proceeds.
        let fresh = WitnessSources::pinned(keychain.clone(), store.clone());
        assert!(admit_append_only(&fresh, false).await.is_ok());

        // No config, witness present → refused. This is the incident.
        std::fs::write(&samples, b"{\"ts\":0}\n").unwrap();
        let configured = WitnessSources::pinned(keychain.clone(), store.clone());
        assert!(matches!(
            admit_append_only(&configured, false).await.unwrap_err(),
            Error::PriorConfigurationWithoutConfig
        ));

        // Config present → admitted, and the same sources that just refused are now moot,
        // which is the skip-the-probe shortcut behaving as its doc claims.
        assert!(admit_append_only(&configured, true).await.is_ok());

        delete_keychain(&keychain).await;
    }

    #[tokio::test]
    async fn a_surviving_six_account_stash_set_is_refused() {
        // Issue #1440 AC-3, the incident's own shape: six accounts stashed and `config.toml`
        // gone. Read through real `security` against a real keychain, so what is exercised
        // is the probe, the prefix match and the rule together.
        //
        // SCOPE, stated because the AC asks for more than this test can honestly carry.
        // What is asserted here is the refusal, against six real stashed items read through
        // real `security`. The other half — "the daemon's roster is unchanged at N" — is
        // NOT asserted here and cannot be: `capture`/`login` resolve `paths::config_file`
        // and `paths::login_keychain` off the live machine, so driving them from a test
        // would read the operator's own keychain, and this module is structurally unable to
        // reach a socket in the first place.
        //
        // An earlier version of this test did claim that half, with a `UnixListener` on a
        // temp path and an assertion that it saw zero connections. That was theatre: the
        // path was never handed to production code, so the counter could not have been
        // non-zero whatever the code did — an independent verify gate demonstrated exactly
        // that by making the gate open three real connections while this test stayed green.
        // It is deleted rather than repaired, because a control that cannot fail is worse
        // than none: it reports assurance it never had.
        //
        // What does carry that half: `the_witness_cannot_consult_the_control_socket` (this
        // module cannot open a socket at all) and, in `src/capture.rs`,
        // `both_append_only_verbs_consult_the_witness_before_they_can_write` (the gate
        // precedes `notify_daemon_roster_reload()` in both verbs). Both are mutation-proven.
        // Closing the gap properly means making the verbs path-injectable, which is a
        // wider change than this fix owns.
        let dir = tempfile::tempdir().unwrap();
        let keychain = empty_keychain(dir.path()).await;
        let uuids = [
            "11111111-1111-1111-1111-111111111111",
            "22222222-2222-2222-2222-222222222222",
            "33333333-3333-3333-3333-333333333333",
            "44444444-4444-4444-4444-444444444444",
            "55555555-5555-5555-5555-555555555555",
            "66666666-6666-6666-6666-666666666666",
        ];
        for uuid in uuids {
            seed_stash(&keychain, uuid).await;
        }

        // No `config.toml`, six stashes surviving.
        let sources = WitnessSources::pinned(keychain.clone(), Vec::new());
        let refusal = admit_append_only(&sources, false).await.unwrap_err();
        assert!(matches!(refusal, Error::PriorConfigurationWithoutConfig));

        delete_keychain(&keychain).await;
    }

    // --- real-`security` helpers -----------------------------------------------------

    /// A throwaway keychain holding nothing — the keychain half's honest `Ok(false)`.
    ///
    /// Created with a password and never added to the search list, so it is invisible to
    /// everything but the explicit path handed to `dump-keychain`; the operator's login
    /// keychain is never touched.
    async fn empty_keychain(dir: &Path) -> PathBuf {
        let keychain = dir.join("witness-test.keychain-db");
        let status = tokio::process::Command::new("/usr/bin/security")
            .args(["create-keychain", "-p", "witness-test"])
            .arg(&keychain)
            .status()
            .await
            .expect("cannot run /usr/bin/security");
        assert!(status.success(), "create-keychain failed: {status}");
        keychain
    }

    /// Add one `Sessiometer/<uuid>` item, exactly as `Account::stash` names them.
    async fn seed_stash(keychain: &Path, uuid: &str) {
        add_item(keychain, &format!("{}{uuid}", crate::config::STASH_PREFIX)).await;
    }

    /// Add one generic-password item under `service`.
    async fn add_item(keychain: &Path, service: &str) {
        let status = tokio::process::Command::new("/usr/bin/security")
            .args(["add-generic-password", "-a", "sessiometer", "-s"])
            .arg(service)
            .args(["-w", "not-a-real-token"])
            .arg(keychain)
            .status()
            .await
            .expect("cannot run /usr/bin/security");
        assert!(status.success(), "add-generic-password failed: {status}");
    }

    async fn delete_keychain(keychain: &Path) {
        let _ = tokio::process::Command::new("/usr/bin/security")
            .arg("delete-keychain")
            .arg(keychain)
            .status()
            .await;
    }
}
