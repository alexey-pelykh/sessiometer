// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! The active Claude Code credential in the macOS login keychain.
//!
//! Reads and rewrites the generic-password item whose service is
//! `Claude Code-credentials` by driving the `/usr/bin/security` CLI — never the
//! Security.framework SDK. Writing the item as our own code identity through the
//! SDK would re-stamp its ACL partition list to our team id and evict the
//! `apple-tool:` entry Claude Code's silent read relies on; the CLI write rides
//! `apple-tool:` and preserves it. A CI guard
//! (`scripts/check-no-security-framework.sh`) keeps the SDK out of the
//! dependency graph.
//!
//! **Service name.** The item's service is `Claude Code-credentials` for the
//! default config dir, but Claude Code suffixes it with
//! `-<sha256(CLAUDE_CONFIG_DIR)[..8]>` under a non-default `CLAUDE_CONFIG_DIR`
//! (replicated byte-for-byte by [`canonical_service_from`], issue #100). Every site
//! that names the canonical item — read/poll, swap-write, resolve — addresses the
//! *resolved* name, so a CC instance run under an isolated config dir is managed,
//! not invisible.
//!
//! **Item `acct`.** The two item paths reach the `acct` attribute from opposite
//! ends, and only one of them has to derive it. The **canonical** item is read back
//! *as stored* (see resolve, below), so it is immune to however Claude Code computed
//! the name. The **isolated** item (issue #102 refresh / issue #132 capture) has
//! nothing to read back — it pins `acct` up front, *before* the `claude` it seeds
//! for has written anything — so it must replicate CC's own derivation exactly
//! ([`claude_code_acct_from`], issue #711: `$USER` first, the passwd login name only
//! as its fallback). Deriving it as the bare login name instead would mis-target the
//! item whenever `$USER` diverges from passwd, and the only symptom would be a
//! misleading `Not logged in`.
//!
//! A third path reaches the `acct` from neither end: the startup orphan **reap**
//! (issues #103 / #133) sweeps what a `SIGKILL`ed process left behind, so it runs in a
//! DIFFERENT process from the one that seeded — the only scenario it exists for. It
//! therefore cannot re-derive the name: `$USER` may have changed at the same uid since
//! #711 made the derivation `$USER`-first, while the path-hash-derived service has
//! not, so a delete under a re-derived `acct` returns `errSecItemNotFound` and is
//! swallowed as idempotent success while the credential stays. That path enumerates
//! the SERVICE and names no `acct` at all ([`IsolatedServiceReaper`], issue #769).
//!
//! The mechanism and the facts this module depends on were verified empirically
//! before implementation — see `build/version-compat.md` (the issue #16 ledger):
//! the store is the legacy file-based `login.keychain-db`, every call pins that
//! path explicitly (keeps the item on the classic-ACL path), and `add-generic-password -U`
//! is an atomic in-place update (no rename window a concurrent reader could see
//! a missing item through).
//!
//! Three operations:
//!   - **resolve** — read back the item's `acct` attribute *as stored* (never
//!     assume it equals `$USER`) and enforce uniqueness: zero matches →
//!     [`Error::CredentialNotFound`], more than one → [`Error::CredentialAmbiguous`],
//!     exactly one → that `acct`, pinned for later calls. Driven off
//!     `security dump-keychain` (metadata only — no `-d`, so no secret data and
//!     no prompt), handling both quoted-string and `0x`-hex attribute rendering.
//!   - **read** — `find-generic-password -w -s <service> -a <resolved-acct> <keychain>`;
//!     `-w` prints the secret with a single trailing newline, which [`finish_read`]
//!     strips so a read→write round-trip is byte-exact.
//!   - **write** — `add-generic-password -U -s <service> -a <resolved-acct> -w <blob> <keychain>`,
//!     fed to `security -i` on **stdin** (not argv) so the blob is never visible in
//!     this process's command line (issue #39; `build/version-compat.md`).

use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::OnceLock;

#[cfg(test)]
use std::cell::{Cell, RefCell};

use tokio::process::Command;
use zeroize::{Zeroize, Zeroizing};

use crate::error::{Error, Result};
use crate::paths;
use crate::sha256::sha256_hex;

/// Absolute path to the system `security` tool. Absolute (not bare `security`
/// resolved through `$PATH`) so a hijacked `PATH` cannot substitute a different
/// binary for this security-sensitive call.
const SECURITY: &str = "/usr/bin/security";

/// The base generic-password service name Claude Code stores its credential
/// under, for the **default** config dir (`~/.claude` — `CLAUDE_CONFIG_DIR` unset
/// or empty). Under a non-default config dir CC appends a hash suffix; the full
/// name is resolved by [`canonical_service_from`].
const SERVICE_BASE: &str = "Claude Code-credentials";

/// Derive the keychain service suffix for a non-default config-dir `value`,
/// replicating Claude Code 2.1.181's `-${sha256(value)[..8]}` (its `n1()`).
///
/// CC hashes the value **NFC-normalized**. For an ASCII value NFC is the identity,
/// so the raw bytes hash byte-identically and no Unicode-normalizer dependency is
/// pulled in (the crate hand-rolls its primitives to keep the dependency graph
/// minimal — see [`crate::sha256`]). A non-ASCII value could differ between its NFC
/// form and its raw bytes, so it is **refused** ([`Error::NonAsciiConfigDir`]) rather
/// than risk computing a suffix that silently addresses the wrong keychain item. The
/// value is read as bytes (`OsStrExt`), never `to_string_lossy` — a lossy decode
/// would hash different bytes than CC sees.
fn service_suffix(value: &OsStr) -> Result<String> {
    let bytes = value.as_bytes();
    if !bytes.is_ascii() {
        return Err(Error::NonAsciiConfigDir);
    }
    // CC: `createHash("sha256").update(value).digest("hex").substring(0,8)`.
    Ok(format!("-{}", &sha256_hex(bytes)[..8]))
}

/// Resolve the canonical keychain service name from the two config-dir env values,
/// replicating Claude Code 2.1.181's `n1("-credentials")` exactly so sessiometer
/// addresses the **same** item a live CC instance does:
///
/// - `CLAUDE_SECURESTORAGE_CONFIG_DIR` (`securestorage`) takes precedence when
///   **defined**: a defined-empty value forces the bare base name, a non-empty value
///   is the hashed value — and `CLAUDE_CONFIG_DIR` is then NOT consulted (CC's
///   `n = !t`, `r = t`).
/// - otherwise `CLAUDE_CONFIG_DIR` (`config_dir`): unset OR empty → bare base name;
///   non-empty → hashed.
///
/// Both unset (the default config dir) → bare `Claude Code-credentials`, unchanged
/// from the prior behaviour (no regression for current usage). Pure — the env read
/// lives in [`canonical_service`] — so every arm is unit-testable without mutating
/// process-global env (mirrors `paths::config_dir_from`). (issue #100)
fn canonical_service_from(
    securestorage: Option<&OsStr>,
    config_dir: Option<&OsStr>,
) -> Result<String> {
    let suffix = match securestorage {
        // SECURESTORAGE defined wins outright: defined-empty → bare; non-empty →
        // hashed. CONFIG_DIR is never consulted once it is defined.
        Some(s) if s.is_empty() => String::new(),
        Some(s) => service_suffix(s)?,
        // SECURESTORAGE unset → fall through to CONFIG_DIR: unset/empty → bare.
        None => match config_dir {
            Some(c) if !c.is_empty() => service_suffix(c)?,
            _ => String::new(),
        },
    };
    Ok(format!("{SERVICE_BASE}{suffix}"))
}

/// The canonical service for **this process's** environment — the thin env wrapper
/// over [`canonical_service_from`] (the env read is kept out of the pure helper so
/// the helper stays unit-testable without touching process-global env).
fn canonical_service() -> Result<String> {
    canonical_service_from(
        std::env::var_os("CLAUDE_SECURESTORAGE_CONFIG_DIR").as_deref(),
        std::env::var_os("CLAUDE_CONFIG_DIR").as_deref(),
    )
}

/// The canonical keychain service name a Claude Code instance run under
/// `config_dir` as its `CLAUDE_CONFIG_DIR` reads/writes —
/// `Claude Code-credentials-<sha256(NFC(config_dir))[..8]>` (the bare base for an
/// empty value). The `config_dir`-as-argument form of [`canonical_service`], built
/// by the SAME #100 derivation ([`canonical_service_from`]) — never re-derived — so
/// the isolated-refresh engine (issue #102) addresses exactly the item a `claude` it
/// spawns under that isolated config dir will read and refresh. (`securestorage` is
/// modelled as unset: the engine never sets `CLAUDE_SECURESTORAGE_CONFIG_DIR` for the
/// spawn, and unsets any inherited one, so only `CLAUDE_CONFIG_DIR` governs.)
pub(crate) fn service_for_config_dir(config_dir: &OsStr) -> Result<String> {
    canonical_service_from(None, Some(config_dir))
}

/// The literal Claude Code substitutes for a login name it will not use. Both of
/// `uq()`'s reject arms — the `catch` (lookup failed) and the `!xGh.test(e)`
/// (charset failed) — converge on this same string. (issue #711)
const ACCT_FALLBACK: &str = "claude-code-user";

/// Does `name` satisfy Claude Code's `xGh` = `/^[a-zA-Z0-9._-]+$/` — one or more
/// ASCII letters, ASCII digits, `.`, `_` or `-`, anchored at both ends?
///
/// Hand-rolled over the raw bytes rather than pulled from a regex crate: the crate
/// hand-rolls its primitives to keep the dependency graph minimal (see
/// [`crate::sha256`]), and byte-wise is exactly equivalent here. Every byte the class
/// admits is ASCII, so any non-ASCII byte — every byte of a multi-byte UTF-8
/// sequence, and every byte of a name that is not UTF-8 at all — is rejected, which
/// is what CC's test does to the same names once decoded. The `+` makes the empty
/// name a non-match, and the anchors mean an embedded newline or space is rejected
/// too (JS `$` without the `m` flag matches only the true end of input).
///
/// NOT to be unified with `config::account_uuid_violation`, the crate's other hand-rolled
/// ASCII-class check over a keychain-addressing identifier. This one admits `.` because
/// CC's `xGh` does and this must match CC byte for byte; that one excludes `.` precisely
/// because `.` is what makes the `../x` shape in issue #1052 expressible. Same technique,
/// different authorities — the divergence is deliberate.
fn is_well_formed_acct(name: &OsStr) -> bool {
    let bytes = name.as_bytes();
    !bytes.is_empty()
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// Derive the `acct` attribute Claude Code addresses its credential item under,
/// replicating CC 2.1.217's `uq()`:
///
/// ```js
/// function uq(){
///   let e;
///   try{ e=process.env.USER||FWn.userInfo().username }catch{ e="claude-code-user" }
///   if(!xGh.test(e)) return "claude-code-user";            // xGh = /^[a-zA-Z0-9._-]+$/
///   return e
/// }
/// ```
///
/// Three load-bearing constituents, each pinned by a test vector below:
///
/// - **precedence** — `$USER` wins; the passwd login name is consulted ONLY when
///   `$USER` is unset or empty (JS `||` tests falsiness, so a defined-but-empty
///   `USER=` falls through). `login_name` is a thunk, not a value, so a usable
///   `$USER` costs no passwd lookup — the short-circuit is part of the semantics.
/// - **sanitize** — the RESOLVED name, from whichever source, must pass
///   [`is_well_formed_acct`]. Note the ordering subtlety: a non-empty `$USER` that
///   fails the charset does **not** fall back to the passwd name, it becomes
///   [`ACCT_FALLBACK`] — the `||` has already committed to it by the time `xGh` runs.
/// - **fallback** — the literal [`ACCT_FALLBACK`], for both a failed lookup
///   (`login_name` yields `None`, CC's `catch`) and a failed charset test.
///
/// Infallible by construction, as `uq()` is: there is no input for which CC declines
/// to name an item, so neither may we. A `Result` here would let the isolated path
/// abort exactly where a live CC would go on reading `claude-code-user`.
///
/// Pure — the env read and the passwd lookup both live in
/// [`IsolatedKeychainItem::new`] — so every arm is unit-testable without mutating
/// process-global env, which `cargo test`'s parallelism would make racy (mirrors
/// [`canonical_service_from`]). (issue #711)
fn claude_code_acct_from<F>(user_env: Option<&OsStr>, login_name: F) -> CcAcct
where
    F: FnOnce() -> Option<OsString>,
{
    let resolved = match user_env {
        // CC's `||`: a non-empty `$USER` short-circuits — passwd is never consulted.
        Some(user) if !user.is_empty() => Some(user.to_owned()),
        // Unset or defined-empty `$USER` is falsy → fall through to the login name.
        _ => login_name(),
    };
    CcAcct::from_derivation(match resolved {
        Some(name) if is_well_formed_acct(&name) => name,
        _ => OsString::from(ACCT_FALLBACK),
    })
}

/// Privacy boundary for [`CcAcct`]. The type is a newtype over `OsString`, and the
/// point of it is that the wrapped value cannot be supplied from just anywhere — but
/// a tuple struct's `CcAcct(..)` constructor and its `.0` field are visible
/// everywhere in the *defining* module, which is exactly where both seeding sites
/// live. Nesting it one module down makes the field and the tuple constructor
/// genuinely unreachable from `keychain`, leaving the named constructors below as the
/// only way in. (issue #711)
mod cc_acct {
    use std::ffi::{OsStr, OsString};

    /// An `acct` that came from Claude Code's own derivation.
    ///
    /// A newtype rather than a bare `OsString` because the *derivation* being right
    /// is not the same guarantee as the *call site* using it, and only the latter is
    /// what issue #711 actually fixes. Outside this module the only ways to build one
    /// are [`from_derivation`](CcAcct::from_derivation) — whose sole caller is
    /// `claude_code_acct_from` — and a `#[cfg(test)]` escape hatch, so re-inlining
    /// `acct: paths::username()?` at a seeding site is a COMPILE error rather than a
    /// silent regression.
    ///
    /// Tests pin the seeding sites that exist today; only the type covers one added
    /// later. What remains possible is passing a non-derived name to a function named
    /// `from_derivation` — self-evidently wrong at the call site, rather than a
    /// plausible-looking refactor.
    pub(super) struct CcAcct(OsString);

    /// Render the wrapped name in test failures. Not available in production builds:
    /// an `acct` is not secret, but nothing outside tests has a reason to print one.
    #[cfg(test)]
    impl std::fmt::Debug for CcAcct {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "CcAcct({:?})", self.0)
        }
    }

    /// Compare a derived `acct` against an expected name, so the pinned vectors keep
    /// reading as the derivation's semantics rather than as newtype plumbing.
    #[cfg(test)]
    impl PartialEq<OsString> for CcAcct {
        fn eq(&self, other: &OsString) -> bool {
            self.0 == *other
        }
    }

    impl CcAcct {
        /// Wrap the output of Claude Code's derivation. Called only by
        /// `claude_code_acct_from`, which is the derivation.
        pub(super) fn from_derivation(name: OsString) -> Self {
            Self(name)
        }

        /// Borrow the derived name for the `security` argument builders.
        pub(super) fn as_os_str(&self) -> &OsStr {
            &self.0
        }

        /// Wrap an explicit name, bypassing the derivation — tests only, so the
        /// real-CLI round-trip can pin a throwaway `acct` without weakening the
        /// production guarantee above.
        #[cfg(test)]
        pub(super) fn for_test(name: OsString) -> Self {
            Self(name)
        }
    }
}

use cc_acct::CcAcct;

/// An opaque credential blob (the active account's OAuth tokens).
///
/// The inner buffer is zeroized when the last owner is dropped, and the type
/// deliberately does **not** derive `Debug`: no secret-bearing value may be
/// printable. `PartialEq` is gated to tests — comparing secrets in production
/// would invite a non-constant-time equality check.
#[derive(Clone)]
#[cfg_attr(test, derive(PartialEq))]
pub(crate) struct Credential(Zeroizing<Vec<u8>>);

impl Credential {
    /// Wrap a raw credential blob.
    pub(crate) fn new(blob: Vec<u8>) -> Self {
        Self(Zeroizing::new(blob))
    }

    /// Borrow the raw blob bytes. Named to flag that the borrow exposes secret
    /// material: keep its lifetime as short as possible and never log it.
    /// Crate-visible because the per-account stash (issue #4) and the swap engine
    /// (#6) must move the blob between the canonical item and the stash items.
    pub(crate) fn expose(&self) -> &[u8] {
        self.0.as_slice()
    }

    /// Whether two credential blobs are byte-identical.
    ///
    /// Named to flag that it reads both secrets. The swap engine (#6) uses it for
    /// the post-swap re-read — comparing the re-read canonical item against the
    /// token it just wrote, to detect whether a third writer (a concurrent
    /// `/login` or a token refresh) changed it in between. Both operands are
    /// already held in this process, so a non-constant-time comparison leaks
    /// nothing a holder of both does not already have — unlike a
    /// secret-vs-attacker-guess check, where constant time matters (the reason a
    /// production [`Credential`] deliberately has no `PartialEq`).
    ///
    /// Also the comparison behind [`CanonicalWatch`] — the production caller that
    /// retired this method's former `dead_code` allowance.
    pub(crate) fn matches(&self, other: &Credential) -> bool {
        self.0.as_slice() == other.0.as_slice()
    }
}

/// Watches the canonical credential for **out-of-band** changes — the reusable
/// detection primitive behind re-auth re-stash (issue #13) and the
/// dead-credential recovery path (#42, which consumes this seam unchanged).
///
/// It holds the last *committed* canonical blob and answers "did the canonical
/// change since I last looked?" in two steps, deliberately separated so a handler
/// can fail and have the change re-fire next cycle:
///   - [`classify`](CanonicalWatch::classify) compares a freshly-read blob
///     against the baseline **without** advancing it (idempotent), and
///   - [`commit`](CanonicalWatch::commit) advances the baseline — called once the
///     change has been *handled* (the re-stash succeeded), or to prime against the
///     daemon's OWN write (a swap), so that write is not re-detected as external.
///
/// A `Changed` verdict means the canonical was rewritten by something other than
/// the last thing we committed: a `claude /login` re-auth (a fresh token matching
/// no stash) or a silent in-place token refresh — both warrant re-stashing the
/// affected account with the fresh token. The daemon owns the *instance* (it is
/// poll-loop state); the *type* lives here, next to [`Credential`], so #42 reuses
/// it without reaching into the daemon module.
#[derive(Default)]
pub(crate) struct CanonicalWatch {
    /// The last committed canonical blob, or `None` before the first commit.
    last: Option<Credential>,
}

/// How a freshly-read canonical compares to a [`CanonicalWatch`]'s last committed
/// observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CanonicalChange {
    /// No prior observation — the baseline is unset. The caller
    /// [`commit`](CanonicalWatch::commit)s to prime it; never treated as a change
    /// (nothing to compare against).
    Primed,
    /// Byte-identical to the last committed observation — no out-of-band write.
    Unchanged,
    /// Differs from the last committed observation — an out-of-band rewrite (a
    /// `claude /login` re-auth, or a silent in-place token refresh).
    Changed,
}

impl CanonicalWatch {
    /// A watch with no baseline yet (the first [`classify`](Self::classify)
    /// returns [`CanonicalChange::Primed`]). Production constructs the watch via
    /// `Default` (inside `DecisionState`); this named constructor is the readable
    /// form the unit tests use, hence the test-only `dead_code` allowance.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn new() -> Self {
        Self { last: None }
    }

    /// Classify `current` against the last committed blob **without** advancing
    /// the baseline. Idempotent: repeated calls return the same verdict until a
    /// [`commit`](Self::commit) moves the baseline, so a handler that fails (e.g.
    /// a locked keychain mid-re-stash) leaves the change to re-fire next cycle.
    pub(crate) fn classify(&self, current: &Credential) -> CanonicalChange {
        match &self.last {
            None => CanonicalChange::Primed,
            Some(prev) if prev.matches(current) => CanonicalChange::Unchanged,
            Some(_) => CanonicalChange::Changed,
        }
    }

    /// Advance the baseline to `current`. Call after a change is handled, after
    /// priming (the [`CanonicalChange::Primed`] arm), or right after the daemon's
    /// OWN canonical write (a swap) so that write is not re-detected as an
    /// external change on the next [`classify`](Self::classify).
    pub(crate) fn commit(&mut self, current: &Credential) {
        self.last = Some(current.clone());
    }

    /// The last committed canonical blob (a read-only clone), or `None` before the first
    /// commit. The external-login watch (issue #140) snapshots this before the idle to tell an
    /// out-of-band write it reads during the idle from the daemon's own last-committed
    /// canonical — WITHOUT advancing the baseline (that stays [`commit`](Self::commit)'s job).
    pub(crate) fn baseline(&self) -> Option<Credential> {
        self.last.clone()
    }
}

/// Seam: reads/writes the active credential. The real impl drives the macOS
/// `security` CLI; the test impl is an in-memory cell.
///
/// The daemon holds this seam but does not yet call it; the out-of-band swap
/// engine (#6/#7) reads and rewrites the credential through it.
#[allow(dead_code)]
pub(crate) trait CredentialStore {
    async fn read(&self) -> Result<Credential>;
    async fn write(&self, credential: &Credential) -> Result<()>;
    /// FRESH canonical-service resolution probe (issue #714, canary Layer 1):
    /// re-run the #100 enumeration + uniqueness rule NOW — `Ok(())` when exactly
    /// one item sits under the derived service, [`Error::CredentialNotFound`] on
    /// zero (a service-name derivation change, or a scrubbed keychain),
    /// [`Error::CredentialAmbiguous`] on more than one.
    ///
    /// DELIBERATELY bypasses the resolve-once caches ([`RealCredentialStore`]'s
    /// `acct`/`service` `OnceLock`s): those pin the boot-time resolution, so an
    /// item that APPEARS after boot (Claude Code re-keying its storage into a
    /// second item under the same service) would never re-trip the uniqueness
    /// rule through the cached [`read`](Self::read) path — the pre-swap canary
    /// must re-enumerate to see it. The probe never reads secret data
    /// (`dump-keychain` is metadata-only, prompt-free, and works on a locked
    /// keychain), and it does NOT update the pinned caches — a probe/read
    /// divergence (fresh-unique item under a NEW `acct` while the pinned one is
    /// gone) surfaces as [`Error::CredentialNotFound`] from the very next cached
    /// `read`, which is the loud Layer-1 abort the canary wants.
    ///
    /// Defaulted to `Ok(())` (a clean probe): the canary is the only caller, and
    /// only the daemon's REAL canonical store (plus the scripted test fake) has an
    /// enumeration to re-run — the narrow test doubles and the stash-backed
    /// adapter ([`crate::daemon`]'s seams) have no service namespace to probe.
    async fn probe_resolution(&self) -> Result<()> {
        Ok(())
    }
}

/// Real keychain-backed store, driving `/usr/bin/security`.
pub(crate) struct RealCredentialStore {
    /// Keychain to operate on. `None` is production (the login keychain via
    /// [`paths::login_keychain`]); `Some` pins a specific keychain file — used by
    /// the round-trip test to drive the real CLI against a throwaway keychain
    /// without touching the login keychain.
    keychain: Option<PathBuf>,
    /// The resolved `acct`, read back from the item once and pinned for all
    /// later calls (issue #2 "resolve once at start").
    acct: OnceLock<OsString>,
    /// The resolved canonical service name, computed once from the environment
    /// (issue #100) and pinned for every read/poll, swap-write, and resolve. Lazy +
    /// cached like [`acct`](Self::acct), but resolution is a pure synchronous env
    /// read (no keychain round-trip): its only failure is a non-ASCII config dir
    /// ([`Error::NonAsciiConfigDir`]), so it surfaces at first keychain use rather
    /// than forcing a fallible `new()` on every construction site.
    service: OnceLock<String>,
}

impl RealCredentialStore {
    /// Production store, operating on the login keychain.
    pub(crate) fn new() -> Self {
        Self {
            keychain: None,
            acct: OnceLock::new(),
            service: OnceLock::new(),
        }
    }

    /// Store pinned to a specific keychain file. The service is pinned to the bare
    /// [`SERVICE_BASE`] so the round-trip tests (which seed the item under that name)
    /// are hermetic regardless of the ambient `CLAUDE_CONFIG_DIR`.
    #[cfg(all(test, target_os = "macos"))]
    pub(crate) fn for_keychain(path: PathBuf) -> Self {
        let service = OnceLock::new();
        let _ = service.set(SERVICE_BASE.to_owned());
        Self {
            keychain: Some(path),
            acct: OnceLock::new(),
            service,
        }
    }

    /// The keychain path to pin on every call.
    fn keychain_path(&self) -> Result<PathBuf> {
        match &self.keychain {
            Some(kc) => Ok(kc.clone()),
            None => paths::login_keychain(),
        }
    }

    /// The resolved `acct`, computed once and cached.
    async fn acct(&self) -> Result<OsString> {
        if let Some(acct) = self.acct.get() {
            return Ok(acct.clone());
        }
        let resolved = self.resolve().await?;
        // A concurrent caller may have set it first; the value is identical, so
        // ignore the `Err` and read the stored one back.
        let _ = self.acct.set(resolved);
        Ok(self.acct.get().expect("just set").clone())
    }

    /// The resolved canonical service name, computed once from the environment and
    /// cached (issue #100). Mirrors [`acct`](Self::acct) (lazy + cached) but the
    /// computation is a pure synchronous env read, so this stays sync and its only
    /// error is a non-ASCII config dir. Returns a borrow — the resolved name is short
    /// and consumed within each `security`-arg builder.
    fn service(&self) -> Result<&str> {
        if let Some(service) = self.service.get() {
            return Ok(service);
        }
        let resolved = canonical_service()?;
        // A concurrent caller may have set it first; the value is identical, so
        // ignore the `Err` and read the stored one back.
        let _ = self.service.set(resolved);
        Ok(self.service.get().expect("just set"))
    }

    /// Read back the item's `acct` attribute as stored, enforcing uniqueness.
    /// Uses `dump-keychain` (metadata only — no `-d`, so it works even on a
    /// locked keychain and never decrypts secret data) rather than the issue's
    /// literal `find-generic-password -s`: the latter returns only the first
    /// match, so it cannot detect the >1 (ambiguous) case the uniqueness rule
    /// requires.
    async fn resolve(&self) -> Result<OsString> {
        let keychain = self.keychain_path()?;
        let output = Command::new(SECURITY)
            .arg("dump-keychain")
            .arg(&keychain)
            .stdin(Stdio::null())
            .output()
            .await?;
        if !output.status.success() {
            return Err(keychain_error(
                "resolve",
                output.status.code().unwrap_or(-1),
            ));
        }
        // The dump is metadata text (attribute names + quoted/hex values), not
        // secret data; lossy decode is safe and never touches a token.
        parse_resolve(self.service()?, &String::from_utf8_lossy(&output.stdout))
    }
}

/// `find-generic-password` arguments (after the program name): read the secret
/// of the resolved item, pinning `-s <service>`, `-a <acct>` and the keychain path.
fn read_args(service: &str, acct: &OsStr, keychain: &Path) -> Vec<OsString> {
    vec![
        "find-generic-password".into(),
        "-w".into(),
        "-s".into(),
        service.into(),
        "-a".into(),
        acct.to_owned(),
        keychain.as_os_str().to_owned(),
    ]
}

/// Append `token` to `out` double-quoted and backslash-escaped for the
/// `security -i` interactive tokenizer: escape `\` → `\\` and `"` → `\"`, then
/// wrap in `"…"`. The tokenizer is **not** a shell — `$`, backticks, `;`, `|`
/// and whitespace are all literal inside the quotes — so this suffices to carry
/// an arbitrary single-line byte string as exactly one argument. Validated
/// byte-exact across adversarial payloads (issue #39; `build/version-compat.md`).
fn push_quoted(out: &mut Vec<u8>, token: &[u8]) {
    out.push(b'"');
    for &b in token {
        if b == b'\\' || b == b'"' {
            out.push(b'\\');
        }
        out.push(b);
    }
    out.push(b'"');
}

/// The `security -i` stdin line for the canonical write: an atomic in-place
/// (`-U`) `add-generic-password` of the resolved item, every field double-quoted
/// (incl. the blob). Feeding this on stdin keeps the blob off this process's argv
/// — the spawned `security` carries only `-i` — closing the #2 residual risk
/// (issue #39). The returned buffer holds the secret, so it is `Zeroizing`.
fn write_command_line(
    service: &str,
    acct: &OsStr,
    keychain: &Path,
    blob: &[u8],
) -> Zeroizing<Vec<u8>> {
    // The interactive reader is line-based: an embedded newline would truncate
    // the command. Real payloads (single-line OAuth JSON) never contain one — and
    // if one ever did, `security` exits non-zero and `finish_write` reports the
    // failure rather than writing a truncated secret (never a silent partial).
    debug_assert!(
        !blob.contains(&b'\n'),
        "interactive command line is newline-delimited"
    );
    let mut line = Vec::new();
    line.extend_from_slice(b"add-generic-password -U -s ");
    push_quoted(&mut line, service.as_bytes());
    line.extend_from_slice(b" -a ");
    push_quoted(&mut line, acct.as_bytes());
    line.extend_from_slice(b" -w ");
    push_quoted(&mut line, blob);
    line.push(b' ');
    push_quoted(&mut line, keychain.as_os_str().as_bytes());
    line.push(b'\n');
    Zeroizing::new(line)
}

/// Run one off-argv write: spawn `security -i` (argv is only `-i` — the blob
/// rides stdin, never the process command line, issue #39), feed `line`, then
/// close stdin so the CLI hits EOF and exits, and collect the result. `line`
/// holds the secret and stays owned (and `Zeroizing`) at the call site.
async fn run_interactive_write(line: &[u8]) -> Result<std::process::Output> {
    use tokio::io::AsyncWriteExt;
    let mut child = Command::new(SECURITY)
        .arg("-i")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()?;
    // The whole write lands before `wait_with_output` drains stderr, so it rests on
    // `line` fitting the pipe buffer — and nothing bounds `line`: neither the
    // credential blob (uncapped all along) nor, since issue #711, the `$USER`-derived
    // `acct`. Unreachable in practice (a deadlock needs a ~64 KiB `line` AND
    // `security` filling its own stderr buffer in the same call), and capping the
    // `acct` here would re-introduce the very divergence #711 closed — so the
    // concurrent-drain hardening that removes the precondition for both is tracked as
    // issue #747. Dropping the handle at the end of the statement closes the pipe → EOF.
    child
        .stdin
        .take()
        .expect("stdin was piped")
        .write_all(line)
        .await?;
    Ok(child.wait_with_output().await?)
}

/// Map a non-zero `security` exit `code` to a typed error. `36` is
/// `errSecInteractionNotAllowed` (locked keychain); `44` is item-not-found.
fn keychain_error(op: &'static str, code: i32) -> Error {
    match code {
        36 => Error::KeychainLocked { op },
        44 => Error::CredentialNotFound,
        _ => Error::Keychain { op, code },
    }
}

/// Turn a `find-generic-password -w` result into a [`Credential`], stripping the
/// single trailing newline `-w` appends so a read→write round-trip is byte-exact.
/// On failure the buffer is wiped and a typed error returned (never the output,
/// which could hold partial secret bytes).
fn finish_read(mut stdout: Vec<u8>, success: bool, code: i32) -> Result<Credential> {
    if !success {
        stdout.zeroize();
        return Err(keychain_error("read", code));
    }
    if stdout.last() == Some(&b'\n') {
        stdout.pop();
    }
    Ok(Credential::new(stdout))
}

/// Turn an `add-generic-password` result into `Ok(())` or a typed keychain error.
fn finish_write(success: bool, code: i32) -> Result<()> {
    if success {
        Ok(())
    } else {
        Err(keychain_error("write", code))
    }
}

/// Decode a dumped attribute value (the text after `<blob>=`): a quoted string,
/// a `0x`-hex blob, or `<NULL>`. Returns the raw bytes.
fn decode_attr_value(rest: &str) -> Option<Vec<u8>> {
    let rest = rest.trim();
    if let Some(after) = rest.strip_prefix('"') {
        // Quoted: bytes up to the final quote on the line.
        after.rfind('"').map(|end| after.as_bytes()[..end].to_vec())
    } else if let Some(hex) = rest.strip_prefix("0x") {
        // Take the hex run (the dumped line may append trailing content); an empty
        // run is not a valid `0x` value. Pair-decoding — including the odd-length
        // rejection — is the shared codec's job ([`crate::hex`]).
        let digits: Vec<u8> = hex.bytes().take_while(|b| b.is_ascii_hexdigit()).collect();
        if digits.is_empty() {
            return None;
        }
        crate::hex::decode(&digits)
    } else if rest == "<NULL>" {
        Some(Vec::new())
    } else {
        None
    }
}

/// Find attribute `name` (e.g. `acct`, `svce`) within one dumped item block and
/// decode its value.
fn block_attr(block: &str, name: &str) -> Option<Vec<u8>> {
    let needle = format!("\"{name}\"<blob>=");
    block
        .lines()
        .find_map(|line| line.trim_start().strip_prefix(needle.as_str()))
        .and_then(decode_attr_value)
}

/// Every generic-password item in a `security dump-keychain` whose service is
/// `service`, as that item's `acct` — `None` for a service-match carrying no `acct`
/// attribute at all.
///
/// The one enumeration shared by both service-scoped passes: [`parse_resolve`], which
/// enforces uniqueness over it for the CANONICAL item, and [`parse_reap_targets`],
/// which reaps ALL of it for an ISOLATED one (issue #769). Keeping a single parser
/// means the resolve and the reap can never disagree about what "an item under this
/// service" is — a divergence would let the reap skip something the resolve counts.
///
/// `service` is the config-dir-resolved name (issue #100), so under a non-default
/// `CLAUDE_CONFIG_DIR` this matches the **suffixed** item, not the bare base.
fn service_matches(service: &str, dump: &str) -> Vec<Option<Vec<u8>>> {
    let mut matches: Vec<Option<Vec<u8>>> = Vec::new();
    // Each item block begins with a `keychain: "<path>"` header line.
    for block in dump.split("\nkeychain: ") {
        if !block.contains("class: \"genp\"") {
            continue;
        }
        if block_attr(block, "svce").as_deref() == Some(service.as_bytes()) {
            matches.push(block_attr(block, "acct"));
        }
    }
    matches
}

/// Parse `security dump-keychain` output: find every generic-password item whose
/// service is the resolved `service`, then enforce uniqueness — 0 → not found,
/// >1 → ambiguous, exactly 1 → that item's `acct`.
fn parse_resolve(service: &str, dump: &str) -> Result<OsString> {
    // One entry per service-matching item (its `acct`, if present). ALL matches are
    // counted — including any with an absent `acct` — so a malformed item can never
    // mask an ambiguity by going uncounted.
    let mut matches = service_matches(service, dump);
    match matches.len() {
        0 => Err(Error::CredentialNotFound),
        // Exactly one item, but a usable `acct` is required to address it; a
        // service-match with no `acct` is unusable (treated as not found).
        1 => matches
            .pop()
            .unwrap()
            .map(OsString::from_vec)
            .ok_or(Error::CredentialNotFound),
        n => Err(Error::CredentialAmbiguous { count: n }),
    }
}

/// Everything a service-scoped reap must delete under ONE isolated service
/// (issue #769), split by how each item has to be addressed.
///
/// Deliberately NOT a `Vec<Option<OsString>>`: the two arms are deleted by different
/// `security` invocations and in a mandatory order (see [`ReapTargets::plan`]), so
/// keeping them apart in the type stops a future edit from flattening the distinction
/// the order rests on.
#[cfg_attr(test, derive(Debug, PartialEq, Eq))]
struct ReapTargets {
    /// The `acct` of every service-matching item that carries one. Each is deleted by
    /// its own `(service, acct)` pair, WHATEVER name it holds — that is the whole
    /// point of issue #769: the reaping process never re-derives a name and never
    /// compares one, so a `$USER` that changed since the item was seeded is
    /// immaterial.
    accts: Vec<OsString>,
    /// How many service-matching items carry NO `acct`. Such an item cannot be named
    /// by `-a`, so it is deleted by service alone — but it is COUNTED here rather
    /// than dropped, because it is an item under our own ephemeral service and may
    /// hold a live credential, which is exactly what the reap exists to remove.
    /// [`parse_resolve`] above already refuses to let such an item go uncounted; this
    /// refuses to let one go unreaped.
    ///
    /// Measured against `/usr/bin/security` on macOS 26.5.2 / 25F84 before relying on
    /// either half: `delete-generic-password -s <service> <keychain>` with no `-a` exits 0
    /// having removed one matching item and 44 once none is left, and
    /// `add-generic-password` rejects a call without `-a` outright — so nothing this
    /// crate or Claude Code writes through the CLI can land here, and the arm is
    /// reachable only via a Security-framework writer. That is also why its
    /// composition is unit-tested rather than exercised end-to-end: the arguments
    /// ([`IsolatedService::delete_args`]) and the ordering ([`ReapTargets::plan`])
    /// each have a test, while a real-CLI test cannot create the item it would need.
    ///
    /// The same contract is recorded in `build/version-compat.md` (`# Issue #769`),
    /// which is the copy re-walked on a macOS major bump — this comment is not, which
    /// is why both exist. Keep the two in step.
    acctless: usize,
}

/// Parse `security dump-keychain` output into the [`ReapTargets`] under `service` —
/// the `$USER`-independent half of the isolated orphan reap (issue #769).
///
/// The service is the only identity used. Under an isolated config dir that service
/// is path-hash-derived from an ephemeral dir this process owns
/// ([`service_for_config_dir`]), so every item it matches is ours by construction and
/// a sibling `CLAUDE_CONFIG_DIR` profile — which hashes to a DIFFERENT suffix — can
/// never appear in the result (the #133 safety AC).
fn parse_reap_targets(service: &str, dump: &str) -> ReapTargets {
    let (accts, acctless) = service_matches(service, dump).into_iter().fold(
        (Vec::new(), 0),
        |(mut accts, acctless), acct| match acct {
            Some(bytes) => {
                accts.push(OsString::from_vec(bytes));
                (accts, acctless)
            }
            None => (accts, acctless + 1),
        },
    );
    ReapTargets { accts, acctless }
}

impl ReapTargets {
    /// The deletes this sweep must issue, IN ORDER: every acct-addressed one first
    /// (`Some(acct)`), then exactly one service-only delete (`None`) per acct-less
    /// match.
    ///
    /// The order is load-bearing, which is why it is a pure function with its own test
    /// rather than a loop shape inside [`reap`](IsolatedServiceReaper::reap). A
    /// service-only delete removes an ARBITRARY match, so issuing one while
    /// acct-bearing items are still present could
    /// consume one of THOSE instead of the acct-less item it is for — leaving the
    /// acct-less item behind and no one the wiser. Draining the addressed items first
    /// leaves only acct-less items under the service, so each service-only delete can
    /// reach nothing else.
    fn plan(&self) -> Vec<Option<&OsStr>> {
        self.accts
            .iter()
            .map(|acct| Some(acct.as_os_str()))
            .chain(std::iter::repeat_n(None, self.acctless))
            .collect()
    }
}

impl CredentialStore for RealCredentialStore {
    async fn read(&self) -> Result<Credential> {
        let acct = self.acct().await?;
        let keychain = self.keychain_path()?;
        let output = Command::new(SECURITY)
            .args(read_args(self.service()?, &acct, &keychain))
            // Non-interactive: a child read can never block on our stdin. (The
            // daemon-context no-prompt / exit-36-on-lock guarantee is #13's
            // scope — `security` may still raise a GUI dialog in a UI session.)
            .stdin(Stdio::null())
            .output()
            .await?;
        finish_read(
            output.stdout,
            output.status.success(),
            output.status.code().unwrap_or(-1),
        )
    }

    async fn write(&self, credential: &Credential) -> Result<()> {
        let acct = self.acct().await?;
        let keychain = self.keychain_path()?;
        // Build the command (blob included) and feed it to `security -i` on
        // stdin, so the blob never lands on this process's argv (issue #39).
        // `line` is the only heap copy of the escaped secret and is `Zeroizing`.
        let line = write_command_line(self.service()?, &acct, &keychain, credential.expose());
        let output = run_interactive_write(&line).await?;
        finish_write(output.status.success(), output.status.code().unwrap_or(-1))
    }

    async fn probe_resolution(&self) -> Result<()> {
        // A fresh enumeration pass, discarding the resolved `acct` on purpose: the
        // probe asserts the uniqueness rule still holds NOW; addressing stays with
        // the pinned caches (see the trait docs for why a divergence is safe).
        self.resolve().await.map(drop)
    }
}

#[cfg(test)]
pub(crate) struct FakeCredentialStore {
    slot: RefCell<Option<Credential>>,
    /// When set, [`read`](CredentialStore::read) returns [`Error::KeychainLocked`]
    /// — the in-memory analog of a locked login keychain (exit 36), so the daemon's
    /// locked-path backoff (#13) is testable without a real keychain.
    locked: Cell<bool>,
    /// When set, [`read`](CredentialStore::read) returns [`Error::CredentialNotFound`]
    /// — the in-memory analog of an ABSENT canonical item (exit 44, an empty keychain),
    /// so the adopt-target recovery (#212) is testable against a scrubbed canonical. A
    /// [`write`](CredentialStore::write) CLEARS it (an `add-generic-password -U` creates
    /// the item), so a post-adopt re-read confirms. Distinct from the empty-`slot`
    /// [`Error::Unimplemented`] (which `login.rs` relies on): this is the FAITHFUL
    /// gone-canonical signal (`errSecItemNotFound`), matching the real store.
    not_found: Cell<bool>,
    /// When set, [`read`](CredentialStore::read) returns [`Error::Keychain`] — the
    /// in-memory analog of a PRESENT-but-unreadable canonical (a non-lock, non-not-found
    /// `security` exit, e.g. an ACL / auth-deny on the secret read in a UI session). This
    /// is a "could not read" that is NOT "gone": the item may still be present, so the
    /// adopt-target recovery (#212) must ABORT with ZERO writes rather than clobber it —
    /// the same discipline as the engine `swap`'s step-1 read (`?` on any error). Takes
    /// precedence over the `slot` value (the item is present but its secret is
    /// unreadable), NOT cleared by a write.
    unreadable: Cell<bool>,
    /// When `Some(n)`, [`probe_resolution`](CredentialStore::probe_resolution) returns
    /// [`Error::CredentialAmbiguous`] with that count — the in-memory analog of a SECOND
    /// item appearing under the derived service AFTER boot (issue #714 canary Layer 1).
    /// Deliberately affects ONLY the fresh probe, never [`read`](CredentialStore::read):
    /// the real store's cached `acct` keeps addressing the boot-time item, so its reads
    /// keep succeeding while the enumeration has gone ambiguous — exactly the
    /// masked-by-the-cache state the pre-swap probe exists to catch.
    ambiguous: Cell<Option<usize>>,
}

#[cfg(test)]
impl FakeCredentialStore {
    pub(crate) fn empty() -> Self {
        Self {
            slot: RefCell::new(None),
            locked: Cell::new(false),
            not_found: Cell::new(false),
            unreadable: Cell::new(false),
            ambiguous: Cell::new(None),
        }
    }

    /// Simulate the login keychain locking (`true`) or unlocking (`false`): while
    /// locked, `read` returns [`Error::KeychainLocked`] (issue #13).
    pub(crate) fn set_locked(&self, locked: bool) {
        self.locked.set(locked);
    }

    /// Simulate the canonical item being ABSENT (`true`) — `read` returns
    /// [`Error::CredentialNotFound`], the scrubbed / gone canonical the adopt-target
    /// recovery (#212) faces. A subsequent `write` clears it (the item is created).
    pub(crate) fn set_not_found(&self, not_found: bool) {
        self.not_found.set(not_found);
    }

    /// Simulate the canonical item being PRESENT but its secret unreadable (`true`) — a
    /// non-lock, non-not-found `security` failure (an ACL / auth-deny on the read in a
    /// UI session): `read` returns [`Error::Keychain`]. This is a "could not read" that
    /// is NOT "gone", so the adopt-target recovery (#212) must abort here with ZERO
    /// writes rather than clobber a canonical it could not read.
    pub(crate) fn set_unreadable(&self, unreadable: bool) {
        self.unreadable.set(unreadable);
    }

    /// Simulate a SECOND item appearing under the derived service (issue #714 canary
    /// Layer 1): while `Some(n)`, a fresh [`probe_resolution`](CredentialStore::probe_resolution)
    /// reports [`Error::CredentialAmbiguous`] with that count, while `read` keeps
    /// answering from the pinned boot-time item — the exact cache-masked ambiguity
    /// the pre-swap canary re-enumerates to catch. `None` restores a clean probe.
    pub(crate) fn set_ambiguous(&self, count: Option<usize>) {
        self.ambiguous.set(count);
    }
}

#[cfg(test)]
impl CredentialStore for FakeCredentialStore {
    async fn read(&self) -> Result<Credential> {
        // Locked takes precedence: a locked keychain cannot be read to tell whether
        // the item is present, so it wins over the absent signal ("locked ≠ gone").
        if self.locked.get() {
            return Err(Error::KeychainLocked { op: "read" });
        }
        // Present-but-unreadable wins over the `slot` value (the item is there, but its
        // secret cannot be read) and is DISTINCT from absent below: "could not read" is
        // not "gone", so adopt-target must abort here (issue #212).
        if self.unreadable.get() {
            return Err(Error::Keychain {
                op: "read",
                code: 1,
            });
        }
        if self.not_found.get() {
            return Err(Error::CredentialNotFound);
        }
        self.slot
            .borrow()
            .clone()
            .ok_or(Error::Unimplemented("no credential stashed in the fake"))
    }

    async fn write(&self, credential: &Credential) -> Result<()> {
        *self.slot.borrow_mut() = Some(credential.clone());
        // An `add-generic-password -U` creates the item if it was absent, so a write
        // clears the gone-canonical signal — a post-write re-read now finds it.
        self.not_found.set(false);
        Ok(())
    }

    async fn probe_resolution(&self) -> Result<()> {
        // Mirrors the real probe's semantics, NOT `read`'s: `dump-keychain` is
        // metadata-only, so a LOCKED keychain and an unreadable SECRET both still
        // enumerate — only the item being gone (zero) or duplicated (ambiguous)
        // fails the fresh uniqueness rule (issue #714 canary Layer 1).
        if let Some(count) = self.ambiguous.get() {
            return Err(Error::CredentialAmbiguous { count });
        }
        if self.not_found.get() {
            return Err(Error::CredentialNotFound);
        }
        Ok(())
    }
}

/// Seam: the isolated keychain item a spawned `claude` refreshes for the
/// isolated-refresh engine (issue #102) — the generic-password item at the
/// config-dir-suffixed service ([`service_for_config_dir`]), keyed by the `acct`
/// Claude Code itself derives ([`claude_code_acct_from`], issue #711). Usually that
/// IS the login name, but characterizing it as the login name is what made the
/// divergence invisible.
///
/// Distinct from [`CredentialStore`] (the single CANONICAL active item, whose `acct`
/// is resolved by uniqueness): the isolated item's `acct` is KNOWN up front, and it is
/// seeded, read back, and deleted within one short-lived cycle, never resolved. Both
/// writes ride the `apple-tool:` identity (`/usr/bin/security`), like the canonical
/// item, so a spawned CC's own `apple-tool:` save leaves the partition list intact and
/// sessiometer's read-back stays silent (no heal-write — `build/version-compat.md`
/// issue #101 AC-2).
#[allow(dead_code)]
pub(crate) trait IsolatedKeychain {
    /// Seed the isolated item with `blob` — `add-generic-password -U` fed to
    /// `security -i` on stdin, so the blob never lands on this process's argv (#39),
    /// under the `apple-tool:` identity (#101 AC-2).
    async fn seed(&self, blob: &[u8]) -> Result<()>;
    /// Read the (CC-refreshed) blob back — `find-generic-password -w`, silent under
    /// the preserved `apple-tool:` partition (#101 AC-2).
    async fn read_back(&self) -> Result<Credential>;
    /// Delete the isolated item — `delete-generic-password`; an already-absent item is
    /// success (teardown is idempotent). The async happy-path teardown.
    async fn delete(&self) -> Result<()>;
    /// Best-effort SYNCHRONOUS delete for an RAII teardown on the drop / panic /
    /// timer-kill path, where `await` is unavailable. Errors are swallowed — `Drop`
    /// cannot surface them; the async [`delete`](Self::delete) is the primary path.
    fn delete_blocking(&self);
}

/// Real isolated keychain item, driving `/usr/bin/security` against the login
/// keychain (issue #102). Reuses the canonical item's off-argv `security -i` write
/// and `-w` read primitives, addressing the config-dir-suffixed service under the
/// `acct` CC itself would derive.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) struct IsolatedKeychainItem {
    /// The config-dir-suffixed service ([`service_for_config_dir`]) of the spawned
    /// `claude`'s isolated `CLAUDE_CONFIG_DIR`.
    service: String,
    /// The `acct` CC reads/writes the item under, derived by CC's own rule
    /// ([`claude_code_acct_from`], issue #711). Typed [`CcAcct`] so it can only ever
    /// hold a name that came from that derivation.
    acct: CcAcct,
    /// Keychain to operate on. `None` is production (the login keychain); `Some` pins
    /// a throwaway keychain for the real-CLI round-trip test.
    keychain: Option<PathBuf>,
}

#[cfg_attr(not(test), allow(dead_code))]
impl IsolatedKeychainItem {
    /// Production isolated store for a `claude` spawned under `config_dir`: the
    /// service is derived from `config_dir` by #100, the `acct` by CC's own `uq()`
    /// rule ([`claude_code_acct_from`], issue #711 — NOT the bare login name), and
    /// operations target the login keychain.
    ///
    /// A pure delegation to [`new_from`](Self::new_from), which holds all of the
    /// wiring: this shim exists only to read the two environment inputs, exactly as
    /// [`canonical_service`] shims [`canonical_service_from`]. Keep it that way —
    /// logic added here would sit outside the seam the tests drive.
    pub(crate) fn new(config_dir: &OsStr) -> Result<Self> {
        Self::new_from(config_dir, std::env::var_os("USER").as_deref(), || {
            paths::username().ok()
        })
    }

    /// [`new`](Self::new) with its two environment inputs threaded in — the `$USER`
    /// value, and a thunk for the passwd login name (deferred so a usable `$USER`
    /// short-circuits it, as CC's `||` does).
    ///
    /// This is where the isolated path commits to an `acct`, and it takes those
    /// inputs as arguments so a test can drive the one environment that can tell a
    /// correct wiring from a wrong one: a DIVERGENT one, where `$USER` and the passwd
    /// name differ. That is the whole case issue #711 is about, and it is exactly the
    /// case the ambient environment cannot exercise — on any machine where the two
    /// agree (this one, and every standard CI runner) an assertion made against
    /// ambient env passes whichever name the constructor picked, so it would guard
    /// nothing. Threading beats mutating `$USER` in-process: the suite runs parallel.
    fn new_from<F>(config_dir: &OsStr, user_env: Option<&OsStr>, login_name: F) -> Result<Self>
    where
        F: FnOnce() -> Option<OsString>,
    {
        Ok(Self {
            service: service_for_config_dir(config_dir)?,
            acct: claude_code_acct_from(user_env, login_name),
            keychain: None,
        })
    }

    /// Isolated store pinned to a specific keychain file with an explicit `acct`
    /// (real-CLI round-trip test only — drives the real `security` against a
    /// throwaway keychain, never the login keychain).
    #[cfg(all(test, target_os = "macos"))]
    pub(crate) fn for_keychain(
        config_dir: &OsStr,
        acct: OsString,
        keychain: PathBuf,
    ) -> Result<Self> {
        Ok(Self {
            service: service_for_config_dir(config_dir)?,
            acct: CcAcct::for_test(acct),
            keychain: Some(keychain),
        })
    }

    /// The keychain path to pin on every call.
    fn keychain_path(&self) -> Result<PathBuf> {
        match &self.keychain {
            Some(kc) => Ok(kc.clone()),
            None => paths::login_keychain(),
        }
    }

    /// `delete-generic-password` arguments: the isolated item by `(service, acct)`,
    /// pinning the keychain. No `-w` / payload — delete needs only the non-secret
    /// identifiers, so (unlike seed) there is nothing to keep off argv.
    fn delete_args(&self, keychain: &Path) -> Vec<OsString> {
        vec![
            "delete-generic-password".into(),
            "-s".into(),
            self.service.as_str().into(),
            "-a".into(),
            self.acct.as_os_str().to_owned(),
            keychain.as_os_str().to_owned(),
        ]
    }
}

impl IsolatedKeychain for IsolatedKeychainItem {
    async fn seed(&self, blob: &[u8]) -> Result<()> {
        let keychain = self.keychain_path()?;
        let line = write_command_line(&self.service, self.acct.as_os_str(), &keychain, blob);
        let output = run_interactive_write(&line).await?;
        finish_write(output.status.success(), output.status.code().unwrap_or(-1))
    }

    async fn read_back(&self) -> Result<Credential> {
        let keychain = self.keychain_path()?;
        let output = Command::new(SECURITY)
            .args(read_args(&self.service, self.acct.as_os_str(), &keychain))
            .stdin(Stdio::null())
            .output()
            .await?;
        finish_read(
            output.stdout,
            output.status.success(),
            output.status.code().unwrap_or(-1),
        )
    }

    async fn delete(&self) -> Result<()> {
        let keychain = self.keychain_path()?;
        let output = Command::new(SECURITY)
            .args(self.delete_args(&keychain))
            .stdin(Stdio::null())
            .output()
            .await?;
        if output.status.success() {
            return Ok(());
        }
        let code = output.status.code().unwrap_or(-1);
        // 44 == errSecItemNotFound: the item is already gone → idempotent success.
        if code == 44 {
            Ok(())
        } else {
            Err(keychain_error("isolated delete", code))
        }
    }

    fn delete_blocking(&self) {
        let Ok(keychain) = self.keychain_path() else {
            return;
        };
        // Best-effort, synchronous (Drop cannot await): every outcome — deleted,
        // already-absent, or a transient failure — is swallowed, since the async
        // `delete` is the primary path and Drop has no channel to surface an error.
        let _ = std::process::Command::new(SECURITY)
            .args(self.delete_args(&keychain))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

/// The seam the startup orphan reapers drive (issues #103 / #133): delete EVERY
/// isolated keychain item under ONE service, whatever `acct` each carries.
///
/// A separate trait from [`IsolatedKeychain`], not another method on it, because the
/// two address items from opposite ends and only one of them may name an `acct`. A
/// live [`IsolatedKeychain`] session seeds, reads back and tears down WITHIN one
/// process, so the `acct` it derives ([`claude_code_acct_from`]) is the same one it
/// seeded under by construction. A reap runs in a DIFFERENT process from the one that
/// seeded — that is the entire scenario it exists for, a `SIGKILL`ed cycle swept at
/// the next daemon start — so it has no such guarantee, and since issue #711 made the
/// derivation `$USER`-first rather than uid-derived, a `$USER` that changed between
/// the two processes yields a different name at the same uid. The reaper therefore
/// carries NO `acct` at all (see [`IsolatedService`]): there is no name for it to get
/// wrong. (issue #769)
pub(crate) trait IsolatedServiceReaper {
    /// Delete every item under this reaper's service, returning how many were
    /// actually deleted — `0` is the ordinary clean-start case (nothing was
    /// stranded), and the count is what lets a caller's operator-facing line claim a
    /// reap only when one provably happened.
    async fn reap(&self) -> Result<usize>;
}

/// An isolated keychain SERVICE addressed with no `acct` — the production
/// [`IsolatedServiceReaper`], driving `/usr/bin/security` against the login keychain.
///
/// Deliberately holds `service` and nothing else identifying: the field an
/// [`IsolatedKeychainItem`] would also carry, the derived `acct`, is ABSENT rather
/// than merely unused, so no future edit can re-introduce the cross-process
/// derivation issue #769 removed. Constructed from the same `config_dir` the engine
/// spawns under, through the same #100 derivation, so it names exactly the service
/// that engine's items live under and no other.
pub(crate) struct IsolatedService {
    /// The config-dir-suffixed service ([`service_for_config_dir`]) whose items are
    /// swept. The ONLY identity the reap ever names.
    service: String,
    /// Keychain to operate on. `None` is production (the login keychain); `Some` pins
    /// a throwaway keychain for the real-CLI reap test.
    keychain: Option<PathBuf>,
}

impl IsolatedService {
    /// The reaper for the isolated artifacts a `claude` spawned under `config_dir`
    /// leaves behind, operating on the login keychain. The service is derived by #100
    /// — the SAME derivation [`IsolatedKeychainItem::new`] seeds under, never a
    /// re-normalization — so the reap addresses precisely the engine's own items.
    pub(crate) fn new(config_dir: &OsStr) -> Result<Self> {
        Ok(Self {
            service: service_for_config_dir(config_dir)?,
            keychain: None,
        })
    }

    /// Reaper pinned to a specific keychain file (real-CLI test only — drives the
    /// real `security` against a throwaway keychain, never the login keychain).
    #[cfg(all(test, target_os = "macos"))]
    pub(crate) fn for_keychain(config_dir: &OsStr, keychain: PathBuf) -> Result<Self> {
        Ok(Self {
            service: service_for_config_dir(config_dir)?,
            keychain: Some(keychain),
        })
    }

    /// The keychain path to pin on every call.
    fn keychain_path(&self) -> Result<PathBuf> {
        match &self.keychain {
            Some(kc) => Ok(kc.clone()),
            None => paths::login_keychain(),
        }
    }

    /// Enumerate what sits under this service right now.
    ///
    /// `dump-keychain` (no `-d`) like the canonical [`RealCredentialStore::resolve`]:
    /// metadata only, so it raises no prompt, decrypts no secret, and works on a
    /// locked keychain — which matters here, since a reap runs at daemon start where
    /// the keychain may well still be locked. The dump covers the whole keychain but
    /// never leaves this function: [`parse_reap_targets`] keeps only this service's
    /// items, and nothing renders the text.
    async fn enumerate(&self, keychain: &Path) -> Result<ReapTargets> {
        let output = Command::new(SECURITY)
            .arg("dump-keychain")
            .arg(keychain)
            .stdin(Stdio::null())
            .output()
            .await?;
        if !output.status.success() {
            return Err(keychain_error(
                "isolated reap enumerate",
                output.status.code().unwrap_or(-1),
            ));
        }
        // Metadata text (attribute names + quoted/hex values), not secret data; a
        // lossy decode is safe and never touches a token.
        Ok(parse_reap_targets(
            &self.service,
            &String::from_utf8_lossy(&output.stdout),
        ))
    }

    /// `delete-generic-password` arguments: `-a <acct>` pins one enumerated item;
    /// `None` names the service alone, which deletes an arbitrary ONE of its
    /// remaining items (the only way to reach an item that carries no `acct`).
    fn delete_args(&self, keychain: &Path, acct: Option<&OsStr>) -> Vec<OsString> {
        let mut args: Vec<OsString> = vec![
            "delete-generic-password".into(),
            "-s".into(),
            self.service.as_str().into(),
        ];
        if let Some(acct) = acct {
            args.push("-a".into());
            args.push(acct.to_owned());
        }
        args.push(keychain.as_os_str().to_owned());
        args
    }

    /// Delete one item under this service. `Ok(true)` deleted it, `Ok(false)` found
    /// it already absent (exit 44 — it vanished between the enumeration and this
    /// call), `Err` anything else.
    ///
    /// The false arm is why the reap can report a count at all. An
    /// [`IsolatedKeychainItem::delete`](IsolatedKeychain::delete) folds exit 44 into
    /// `Ok(())` because its whole contract is idempotent teardown, which leaves its
    /// caller unable to tell a delete from a miss; here the enumeration has already
    /// established that the item existed, so a 44 is a distinguishable outcome and is
    /// reported as one rather than counted as a reap.
    async fn delete_one(&self, keychain: &Path, acct: Option<&OsStr>) -> Result<bool> {
        let output = Command::new(SECURITY)
            .args(self.delete_args(keychain, acct))
            .stdin(Stdio::null())
            .output()
            .await?;
        if output.status.success() {
            return Ok(true);
        }
        match output.status.code().unwrap_or(-1) {
            // 44 == errSecItemNotFound.
            44 => Ok(false),
            code => Err(keychain_error("isolated reap delete", code)),
        }
    }
}

impl IsolatedServiceReaper for IsolatedService {
    async fn reap(&self) -> Result<usize> {
        let keychain = self.keychain_path()?;
        let targets = self.enumerate(&keychain).await?;
        let mut deleted = 0;
        let mut first_err = None;
        // Every delete is ATTEMPTED even after one fails — the items are independent
        // orphans, so a single locked/erroring one must not strand the rest — and the
        // FIRST error is surfaced for the caller's log and retry. The delete ORDER
        // belongs to [`ReapTargets::plan`], which owns why it matters.
        for acct in targets.plan() {
            match self.delete_one(&keychain, acct).await {
                Ok(true) => deleted += 1,
                Ok(false) => {}
                Err(err) => first_err = first_err.or(Some(err)),
            }
        }
        match first_err {
            // The count is DROPPED on the error path, which is the safe direction:
            // its only consumer states a reap happened, and under-reporting makes
            // that claim weaker, never false.
            Some(err) => Err(err),
            None => Ok(deleted),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_args_pin_service_acct_and_keychain() {
        let kc = Path::new("/tmp/login.keychain-db");
        assert_eq!(
            read_args(SERVICE_BASE, OsStr::new("alice"), kc),
            vec![
                OsString::from("find-generic-password"),
                OsString::from("-w"),
                OsString::from("-s"),
                OsString::from(SERVICE_BASE),
                OsString::from("-a"),
                OsString::from("alice"),
                kc.as_os_str().to_owned(),
            ]
        );
    }

    // --- canonical service-name resolution (issue #100) --------------------
    //
    // Replicates Claude Code 2.1.181's `n1("-credentials")`. The suffixes are
    // ground truth, generated from CC's exact expression
    // `sha256(value.normalize("NFC")).digest("hex").slice(0,8)` — NFC is the
    // identity for these ASCII paths — so the assertions prove byte-for-byte
    // fidelity to a live CC instance, not just self-consistency.

    #[test]
    fn canonical_service_is_the_bare_base_for_the_default_config_dir() {
        // Both env values unset → no suffix → the unchanged legacy name (no
        // regression for current default-config-dir usage).
        assert_eq!(
            canonical_service_from(None, None).unwrap(),
            "Claude Code-credentials"
        );
    }

    #[test]
    fn an_empty_config_dir_is_treated_as_unset() {
        // `CLAUDE_CONFIG_DIR=` (empty) is falsy in CC's
        // `!process.env.CLAUDE_CONFIG_DIR` gate → bare base name.
        assert_eq!(
            canonical_service_from(None, Some(OsStr::new(""))).unwrap(),
            "Claude Code-credentials"
        );
    }

    #[test]
    fn a_non_default_config_dir_appends_the_sha256_suffix() {
        // The issue's own AC example: sha256("/abs/path")[..8] = 6d80187b.
        assert_eq!(
            canonical_service_from(None, Some(OsStr::new("/abs/path"))).unwrap(),
            "Claude Code-credentials-6d80187b"
        );
        // A second pinned path, same provenance.
        assert_eq!(
            canonical_service_from(None, Some(OsStr::new("/opt/cc"))).unwrap(),
            "Claude Code-credentials-34fd9c6e"
        );
    }

    #[test]
    fn securestorage_config_dir_takes_precedence_over_config_dir() {
        // When CLAUDE_SECURESTORAGE_CONFIG_DIR is defined and non-empty it is the
        // hashed value and CLAUDE_CONFIG_DIR is NOT consulted — so the result equals
        // hashing the securestorage value alone, and differs from the CONFIG_DIR one.
        let with_both =
            canonical_service_from(Some(OsStr::new("/opt/cc")), Some(OsStr::new("/abs/path")))
                .unwrap();
        assert_eq!(
            with_both,
            canonical_service_from(None, Some(OsStr::new("/opt/cc"))).unwrap()
        );
        assert_ne!(
            with_both,
            canonical_service_from(None, Some(OsStr::new("/abs/path"))).unwrap()
        );
    }

    #[test]
    fn a_defined_empty_securestorage_config_dir_forces_the_bare_name() {
        // CC's `n = !t`: a DEFINED-but-empty CLAUDE_SECURESTORAGE_CONFIG_DIR forces
        // the bare name and never falls through to CLAUDE_CONFIG_DIR, even when the
        // latter is set and non-empty. The subtle precedence arm.
        assert_eq!(
            canonical_service_from(Some(OsStr::new("")), Some(OsStr::new("/abs/path"))).unwrap(),
            "Claude Code-credentials"
        );
    }

    #[test]
    fn a_non_ascii_config_dir_is_refused_rather_than_mis_hashed() {
        // We hash raw bytes (no Unicode-normalizer dependency); CC hashes the NFC
        // form. For a non-ASCII value the two could differ, so we refuse rather than
        // silently address the wrong keychain item.
        assert!(matches!(
            canonical_service_from(None, Some(OsStr::new("/Users/café/.claude"))),
            Err(Error::NonAsciiConfigDir)
        ));
        // The precedence path refuses on a non-ASCII securestorage value too.
        assert!(matches!(
            canonical_service_from(Some(OsStr::new("/naïve")), None),
            Err(Error::NonAsciiConfigDir)
        ));
    }

    #[test]
    fn service_for_config_dir_reuses_the_100_derivation_for_the_isolated_engine() {
        // The isolated-refresh engine (#102) addresses a `claude`'s isolated config
        // dir by the SAME #100 suffix — never re-derived. It is exactly
        // `canonical_service_from(None, Some(dir))` (securestorage modelled as unset),
        // matching the spike's pinned vectors (`build/version-compat.md`).
        assert_eq!(
            service_for_config_dir(OsStr::new("/abs/path")).unwrap(),
            "Claude Code-credentials-6d80187b"
        );
        assert_eq!(
            service_for_config_dir(OsStr::new("/opt/cc")).unwrap(),
            canonical_service_from(None, Some(OsStr::new("/opt/cc"))).unwrap()
        );
        // A non-ASCII isolated dir is refused, not mis-hashed — same guard as #100.
        assert!(matches!(
            service_for_config_dir(OsStr::new("/Users/café/refresh")),
            Err(Error::NonAsciiConfigDir)
        ));
    }

    // --- item `acct` derivation (issue #711) --------------------------------
    //
    // Replicates Claude Code 2.1.217's `uq()`, decoded verbatim from the stock binary
    // (the same shape is present in 2.1.209, 2.1.215, 2.1.216, 2.1.217 —
    // `build/version-compat.md`); the decode itself is quoted on
    // `claude_code_acct_from`. These vectors are the tripwire: they are written
    // against CC's derivation, not against ours, so a future CC change trips a test
    // rather than silently mis-targeting the isolated item (whose only symptom would
    // be a misleading `Not logged in`).

    /// A passwd thunk yielding `name`, for the arms where the login name is reached.
    fn passwd(name: &str) -> impl FnOnce() -> Option<OsString> + use<'_> {
        move || Some(OsString::from(name))
    }

    #[test]
    fn a_valid_user_env_wins_over_the_passwd_login_name() {
        // CC's `process.env.USER || userInfo().username`: `$USER` is consulted FIRST
        // and a usable value wins outright, even when passwd says something else.
        // This is the whole divergence #711 exists to close — deriving the bare login
        // name here would seed the isolated item under `loginname` while the `claude`
        // we spawned for it reads `envuser`.
        assert_eq!(
            claude_code_acct_from(Some(OsStr::new("envuser")), passwd("loginname")),
            OsString::from("envuser")
        );
    }

    #[test]
    fn a_valid_user_env_short_circuits_the_passwd_lookup_entirely() {
        // The `||` short-circuits: with a usable `$USER`, `userInfo()` is never
        // called. Pinned because it is load-bearing beyond cost — CC cannot fail on a
        // broken passwd entry when `$USER` is set, so neither may we.
        let consulted = Cell::new(false);
        let acct = claude_code_acct_from(Some(OsStr::new("envuser")), || {
            consulted.set(true);
            Some(OsString::from("loginname"))
        });
        assert_eq!(acct, OsString::from("envuser"));
        assert!(
            !consulted.get(),
            "passwd was consulted despite a usable $USER"
        );
    }

    #[test]
    fn an_unset_user_env_falls_back_to_the_passwd_login_name() {
        // `process.env.USER` is `undefined` → falsy → `userInfo().username`. This is
        // the ordinary case on every machine where the two agree, and it is why this
        // change is a no-op for current usage rather than a behaviour break.
        assert_eq!(
            claude_code_acct_from(None, passwd("loginname")),
            OsString::from("loginname")
        );
    }

    #[test]
    fn a_defined_but_empty_user_env_falls_back_to_the_passwd_login_name() {
        // `USER=` (defined, empty) is falsy in JS exactly as `undefined` is, so the
        // `||` falls through. The subtle arm: "defined" is not "usable".
        assert_eq!(
            claude_code_acct_from(Some(OsStr::new("")), passwd("loginname")),
            OsString::from("loginname")
        );
    }

    #[test]
    fn a_user_env_outside_the_charset_becomes_the_literal_fallback() {
        // `!xGh.test(e)` → `"claude-code-user"`. Note the ordering subtlety: the `||`
        // has ALREADY committed to `$USER` by the time the charset test runs, so a
        // non-empty-but-invalid `$USER` does NOT fall back to the passwd name — it
        // becomes the literal. Asserting `!= "loginname"` is the point of the vector.
        for bad in [
            "has space",
            "naïve",
            "back\\slash",
            "semi;colon",
            "new\nline",
            // TRAILING newline specifically: JS `$` without the `m` flag matches only
            // the true end of input (unlike Python/PCRE, where `$` also matches before
            // a final newline), so CC rejects this too. Pinned separately from the
            // embedded case because the byte scan makes them one code path here, while
            // a future port to a regex crate could silently diverge on exactly this.
            "trailing\n",
        ] {
            let acct = claude_code_acct_from(Some(OsStr::new(bad)), passwd("loginname"));
            assert_eq!(
                acct,
                OsString::from("claude-code-user"),
                "expected the literal fallback for {bad:?}"
            );
            assert_ne!(
                acct,
                OsString::from("loginname"),
                "an invalid $USER must NOT fall back to the passwd name ({bad:?})"
            );
        }
    }

    #[test]
    fn a_name_within_the_charset_is_accepted_verbatim() {
        // A name inside `[a-zA-Z0-9._-]+` is returned as-is — CC addresses the item
        // under it verbatim, so we must too rather than "sanitize" it away. Covers
        // each punctuation character alone (the edge the class still admits) and the
        // realistic mixed forms.
        for ok in [
            "_",
            ".",
            "-",
            "._-",
            "first.last",
            "svc_acct",
            "build-bot",
            "u2",
        ] {
            assert_eq!(
                claude_code_acct_from(Some(OsStr::new(ok)), passwd("loginname")),
                OsString::from(ok),
                "expected {ok:?} to be accepted verbatim"
            );
        }
    }

    #[test]
    fn a_failed_passwd_lookup_becomes_the_literal_fallback() {
        // CC's `catch` arm: `userInfo()` throws → `e = "claude-code-user"`, which then
        // passes `xGh` and is returned. Our thunk yields `None` for the same case
        // (`paths::username()` `Err`), and the derivation stays infallible — CC never
        // declines to name an item, so neither may we.
        assert_eq!(
            claude_code_acct_from(None, || None),
            OsString::from("claude-code-user")
        );
    }

    #[test]
    fn a_passwd_login_name_outside_the_charset_becomes_the_literal_fallback() {
        // `xGh.test(e)` runs on the RESOLVED name whatever its source, so a weird
        // passwd entry is sanitized identically to a weird `$USER` — the regex sits
        // after the `||`, not inside either branch of it.
        assert_eq!(
            claude_code_acct_from(None, passwd("weird name")),
            OsString::from("claude-code-user")
        );
        // Including the empty passwd name: `+` requires at least one character.
        assert_eq!(
            claude_code_acct_from(None, passwd("")),
            OsString::from("claude-code-user")
        );
    }

    #[test]
    fn a_non_utf8_name_is_rejected_rather_than_addressed_raw() {
        // Env values and passwd entries are bytes on Unix; CC sees them decoded, where
        // an invalid sequence surfaces as U+FFFD and fails `xGh`. Our byte-wise test
        // rejects the same input (no byte of a non-ASCII sequence is in the class), so
        // both land on the literal — we never address an item under raw garbage.
        let raw = OsString::from_vec(vec![b'a', 0xFF, b'b']);
        assert_eq!(
            claude_code_acct_from(Some(&raw), passwd("loginname")),
            OsString::from("claude-code-user")
        );
    }

    #[test]
    fn the_isolated_item_commits_to_the_derived_acct_not_the_bare_login_name() {
        // The seam this issue is about: the constructor must commit to the DERIVED
        // `acct` (issue #711), never to `paths::username()`. Driven through a
        // DIVERGENT environment ($USER != passwd name) — the only one that
        // discriminates a correct wiring from a wrong one, and the reason `new_from`
        // takes these as arguments at all (see its doc). Revert the `acct:` line
        // there to `paths::username()?` and this test fails; that is the point of it.
        let item = IsolatedKeychainItem::new_from(
            OsStr::new("/abs/path"),
            Some(OsStr::new("envuser")),
            || Some(OsString::from("loginname")),
        )
        .unwrap();
        assert_eq!(item.acct, OsString::from("envuser"));
        assert_ne!(
            item.acct,
            OsString::from("loginname"),
            "the isolated item committed to the passwd name — the #711 divergence is back"
        );
        // …and the #100 service derivation is unchanged alongside it.
        assert_eq!(item.service, "Claude Code-credentials-6d80187b");
    }

    #[test]
    fn the_production_constructor_commits_to_a_well_formed_acct() {
        // `new` is a pure delegation to `new_from` (which the test above drives), so
        // what is left to check is the shim itself: against the real ambient
        // environment it must still yield a name that can actually be pinned as an
        // `acct`. Reads env, never mutates it.
        let item = IsolatedKeychainItem::new(OsStr::new("/abs/path")).unwrap();
        assert!(is_well_formed_acct(item.acct.as_os_str()));
        assert_eq!(item.service, "Claude Code-credentials-6d80187b");
    }

    #[test]
    fn the_derived_acct_is_always_well_formed_whatever_the_environment() {
        // The totality property that lets this derivation drop the `Result` the bare
        // `paths::username()` carried: for EVERY environment — no `$USER`, a hostile
        // one, a broken passwd entry — the result is a name that can actually be
        // pinned as an `acct`, because the fallback is itself charset-clean.
        for user in [None, Some(""), Some("has space"), Some("ok.name")] {
            for login in [None, Some(""), Some("weird name"), Some("loginname")] {
                let acct =
                    claude_code_acct_from(user.map(OsStr::new), || login.map(OsString::from));
                assert!(
                    is_well_formed_acct(acct.as_os_str()),
                    "malformed acct {acct:?} from $USER={user:?} passwd={login:?}"
                );
            }
        }
    }

    #[test]
    fn push_quoted_wraps_and_escapes_only_backslash_and_quote() {
        let mut out = Vec::new();
        push_quoted(&mut out, b"plain");
        assert_eq!(out, b"\"plain\"");

        // `"` → `\"` and `\` → `\\`, wrapped in quotes.
        let mut out = Vec::new();
        push_quoted(&mut out, br#"a"b\c"#);
        assert_eq!(out, br#""a\"b\\c""#);

        // Whitespace and shell metacharacters are literal — the interactive
        // tokenizer is not a shell, so nothing else needs escaping.
        let mut out = Vec::new();
        push_quoted(&mut out, b"a b$c`d;e|f&g");
        assert_eq!(out, b"\"a b$c`d;e|f&g\"");
    }

    #[test]
    fn write_command_line_quotes_every_field_and_keeps_the_blob_off_argv() {
        let kc = Path::new("/tmp/login.keychain-db");
        let line = write_command_line(SERVICE_BASE, OsStr::new("alice"), kc, br#"tok "x" \y"#);
        // Exactly the `-w` command, every field double-quoted, the blob's `"` and
        // `\` escaped, one trailing newline. The blob lives only inside this
        // stdin line — the spawned process's argv is the constant `-i`.
        let expected = format!(
            "add-generic-password -U -s \"{SERVICE_BASE}\" -a \"alice\" -w \"tok \\\"x\\\" \\\\y\" \"{}\"\n",
            kc.display()
        );
        assert_eq!(&line[..], expected.as_bytes());
    }

    #[test]
    fn finish_read_strips_one_trailing_newline() {
        let cred = finish_read(b"a-token\n".to_vec(), true, 0).unwrap();
        assert_eq!(cred.expose(), b"a-token");
    }

    #[test]
    fn finish_read_keeps_bytes_without_a_trailing_newline() {
        let cred = finish_read(b"a-token".to_vec(), true, 0).unwrap();
        assert_eq!(cred.expose(), b"a-token");
    }

    #[test]
    fn finish_read_strips_only_one_of_several_trailing_newlines() {
        // `-w` appends exactly one newline; an embedded trailing newline in the
        // stored secret must be preserved.
        let cred = finish_read(b"a\n\n".to_vec(), true, 0).unwrap();
        assert_eq!(cred.expose(), b"a\n");
    }

    #[test]
    fn finish_read_classifies_failure_codes() {
        // Matched on the `Result` directly: `Credential` has no `Debug`, so
        // `.unwrap_err()` would not compile — the no-secret-is-printable
        // invariant doing its job.
        assert!(matches!(
            finish_read(Vec::new(), false, 44),
            Err(Error::CredentialNotFound)
        ));
        assert!(matches!(
            finish_read(Vec::new(), false, 36),
            Err(Error::KeychainLocked { op: "read" })
        ));
        assert!(matches!(
            finish_read(Vec::new(), false, 1),
            Err(Error::Keychain {
                op: "read",
                code: 1
            })
        ));
    }

    #[test]
    fn finish_write_maps_success_and_failure() {
        assert!(finish_write(true, 0).is_ok());
        assert!(matches!(
            finish_write(false, 1),
            Err(Error::Keychain {
                op: "write",
                code: 1
            })
        ));
        assert!(matches!(
            finish_write(false, 36),
            Err(Error::KeychainLocked { op: "write" })
        ));
    }

    #[test]
    fn decode_attr_value_handles_quoted_hex_and_null() {
        assert_eq!(
            decode_attr_value("\"alexey-pelykh\"").unwrap(),
            b"alexey-pelykh"
        );
        // 0x616c696365 == "alice"
        assert_eq!(decode_attr_value("0x616C696365").unwrap(), b"alice");
        assert_eq!(decode_attr_value("<NULL>").unwrap(), b"");
        assert!(decode_attr_value("0xZZ").is_none());
    }

    const ONE_MATCH: &str = r#"keychain: "/tmp/x.keychain-db"
version: 512
class: "genp"
attributes:
    0x00000007 <blob>="Claude Code-credentials"
    "acct"<blob>="alexey-pelykh"
    "svce"<blob>="Claude Code-credentials"
keychain: "/tmp/x.keychain-db"
version: 512
class: "genp"
attributes:
    "acct"<blob>="someone"
    "svce"<blob>="Some Other Service"
"#;

    #[test]
    fn parse_resolve_returns_the_unique_acct() {
        assert_eq!(
            parse_resolve(SERVICE_BASE, ONE_MATCH).unwrap(),
            OsString::from("alexey-pelykh")
        );
    }

    #[test]
    fn parse_resolve_decodes_a_hex_acct() {
        let dump = r#"keychain: "/tmp/x.keychain-db"
class: "genp"
    "acct"<blob>=0x616C696365
    "svce"<blob>="Claude Code-credentials"
"#;
        assert_eq!(
            parse_resolve(SERVICE_BASE, dump).unwrap(),
            OsString::from("alice")
        );
    }

    #[test]
    fn parse_resolve_reports_not_found_when_absent() {
        let dump = r#"keychain: "/tmp/x.keychain-db"
class: "genp"
    "acct"<blob>="someone"
    "svce"<blob>="Some Other Service"
"#;
        assert!(matches!(
            parse_resolve(SERVICE_BASE, dump),
            Err(Error::CredentialNotFound)
        ));
    }

    #[test]
    fn parse_resolve_reports_ambiguous_on_duplicates() {
        let dump = r#"keychain: "/tmp/x.keychain-db"
class: "genp"
    "acct"<blob>="acct-one"
    "svce"<blob>="Claude Code-credentials"
keychain: "/tmp/x.keychain-db"
class: "genp"
    "acct"<blob>="acct-two"
    "svce"<blob>="Claude Code-credentials"
"#;
        assert!(matches!(
            parse_resolve(SERVICE_BASE, dump),
            Err(Error::CredentialAmbiguous { count: 2 })
        ));
    }

    #[test]
    fn parse_resolve_counts_an_acctless_match_so_it_cannot_mask_ambiguity() {
        // One service match has no `acct`; it must still be counted, so the pair
        // is reported ambiguous rather than the acct-bearing one winning.
        let dump = r#"keychain: "/tmp/x.keychain-db"
class: "genp"
    "svce"<blob>="Claude Code-credentials"
keychain: "/tmp/x.keychain-db"
class: "genp"
    "acct"<blob>="acct-two"
    "svce"<blob>="Claude Code-credentials"
"#;
        assert!(matches!(
            parse_resolve(SERVICE_BASE, dump),
            Err(Error::CredentialAmbiguous { count: 2 })
        ));
    }

    // --- isolated service reap (#769): address the service, never a re-derived acct ---
    //
    // A dump holding SEVERAL items under one service — the state repeated crashes under
    // differing `$USER` values leave behind, and the state `parse_resolve` above rejects
    // as ambiguous for the canonical item but the isolated reap must sweep whole.
    const MANY_UNDER_ONE_SERVICE: &str = r#"keychain: "/tmp/x.keychain-db"
class: "genp"
    "acct"<blob>="user-a"
    "svce"<blob>="Claude Code-credentials"
keychain: "/tmp/x.keychain-db"
class: "genp"
    "acct"<blob>="user-b"
    "svce"<blob>="Claude Code-credentials"
keychain: "/tmp/x.keychain-db"
class: "genp"
    "acct"<blob>="user-a"
    "svce"<blob>="Claude Code-credentials-deadbeef"
"#;

    #[test]
    fn parse_reap_targets_returns_every_acct_under_the_service() {
        // All of them, in dump order, and nothing from the sibling service — the parse-level
        // half of the #133 safety AC.
        assert_eq!(
            parse_reap_targets(SERVICE_BASE, MANY_UNDER_ONE_SERVICE),
            ReapTargets {
                accts: vec![OsString::from("user-a"), OsString::from("user-b")],
                acctless: 0,
            }
        );
    }

    #[test]
    fn parse_reap_targets_is_empty_for_a_service_with_no_items() {
        // The ordinary clean-start case: nothing was stranded, so the sweep has nothing
        // to delete and its caller stays silent.
        assert_eq!(
            parse_reap_targets("Claude Code-credentials-nosuch", MANY_UNDER_ONE_SERVICE),
            ReapTargets {
                accts: Vec::new(),
                acctless: 0,
            }
        );
    }

    #[test]
    fn parse_reap_targets_decodes_a_hex_acct() {
        // Hex-rendered attributes are the same codec `parse_resolve` uses; an `acct` the
        // dump chose to hex-render is still addressable and must not be skipped.
        let dump = r#"keychain: "/tmp/x.keychain-db"
class: "genp"
    "acct"<blob>=0x616C696365
    "svce"<blob>="Claude Code-credentials"
"#;
        assert_eq!(
            parse_reap_targets(SERVICE_BASE, dump).accts,
            vec![OsString::from("alice")]
        );
    }

    #[test]
    fn parse_reap_targets_counts_an_acctless_match_rather_than_dropping_it() {
        // A service-match carrying no `acct` cannot be named by `-a`, but it is an item
        // under our own ephemeral service and may hold a live credential — so it is
        // counted for a service-only delete, not silently left behind.
        let dump = r#"keychain: "/tmp/x.keychain-db"
class: "genp"
    "svce"<blob>="Claude Code-credentials"
keychain: "/tmp/x.keychain-db"
class: "genp"
    "acct"<blob>="user-a"
    "svce"<blob>="Claude Code-credentials"
"#;
        assert_eq!(
            parse_reap_targets(SERVICE_BASE, dump),
            ReapTargets {
                accts: vec![OsString::from("user-a")],
                acctless: 1,
            }
        );
    }

    #[test]
    fn the_reap_and_the_resolve_enumerate_the_same_items() {
        // Both passes run off `service_matches`, so what the canonical resolve treats as
        // "the item under this service" is exactly what the reap deletes. Pinned because a
        // second, drifting enumeration is how the reap would come to skip something.
        let resolved = parse_resolve(SERVICE_BASE, ONE_MATCH).unwrap();
        assert_eq!(
            parse_reap_targets(SERVICE_BASE, ONE_MATCH).accts,
            [resolved]
        );
    }

    #[test]
    fn a_reap_plan_drains_addressed_items_before_any_service_only_delete() {
        // The ordering invariant: a service-only delete removes an ARBITRARY match, so it
        // may only run once no acct-bearing item is left for it to consume.
        let targets = ReapTargets {
            accts: vec![OsString::from("user-a"), OsString::from("user-b")],
            acctless: 2,
        };
        assert_eq!(
            targets.plan(),
            vec![
                Some(OsStr::new("user-a")),
                Some(OsStr::new("user-b")),
                None,
                None,
            ]
        );
    }

    #[test]
    fn isolated_service_delete_args_name_the_service_and_only_an_enumerated_acct() {
        let kc = Path::new("/tmp/login.keychain-db");
        let svc = IsolatedService {
            service: "Claude Code-credentials-deadbeef".to_owned(),
            keychain: None,
        };
        // An enumerated `acct` is pinned as read from the dump — never compared against,
        // or replaced by, one this process would derive (issue #769).
        assert_eq!(
            svc.delete_args(kc, Some(OsStr::new("user-a"))),
            vec![
                OsString::from("delete-generic-password"),
                OsString::from("-s"),
                OsString::from("Claude Code-credentials-deadbeef"),
                OsString::from("-a"),
                OsString::from("user-a"),
                OsString::from(kc),
            ]
        );
        // With no `acct` there is no `-a` at all — the service alone, which is the only
        // way to reach an item that carries none.
        assert_eq!(
            svc.delete_args(kc, None),
            vec![
                OsString::from("delete-generic-password"),
                OsString::from("-s"),
                OsString::from("Claude Code-credentials-deadbeef"),
                OsString::from(kc),
            ]
        );
    }

    #[tokio::test]
    async fn fake_store_round_trips() {
        let store = FakeCredentialStore::empty();
        let cred = Credential::new(b"oauth-blob".to_vec());
        store.write(&cred).await.unwrap();
        // `Credential` has no `Debug`, so compare with `==` rather than `assert_eq!`.
        assert!(store.read().await.unwrap() == cred);
    }

    #[test]
    fn credential_matches_compares_blob_bytes() {
        let a = Credential::new(b"same-token".to_vec());
        let same = Credential::new(b"same-token".to_vec());
        let different = Credential::new(b"other-token".to_vec());
        assert!(a.matches(&same));
        assert!(!a.matches(&different));
    }

    // --- CanonicalWatch (the re-auth / dead-credential detection primitive, #13/#42) ---

    fn cred(blob: &[u8]) -> Credential {
        Credential::new(blob.to_vec())
    }

    #[test]
    fn canonical_watch_primes_on_the_first_observation() {
        // No baseline yet → Primed (never a Changed on the very first look), so a
        // daemon that has just started never spuriously re-stashes.
        let watch = CanonicalWatch::new();
        assert_eq!(watch.classify(&cred(b"A-token")), CanonicalChange::Primed);
    }

    #[test]
    fn canonical_watch_reports_unchanged_after_committing_the_same_blob() {
        let mut watch = CanonicalWatch::new();
        watch.commit(&cred(b"A-token"));
        assert_eq!(
            watch.classify(&cred(b"A-token")),
            CanonicalChange::Unchanged
        );
    }

    #[test]
    fn canonical_watch_reports_changed_for_a_different_blob() {
        // A fresh `/login` token (matching no prior commit) is a Changed.
        let mut watch = CanonicalWatch::new();
        watch.commit(&cred(b"A-token"));
        assert_eq!(
            watch.classify(&cred(b"A-relogin-token")),
            CanonicalChange::Changed
        );
    }

    #[test]
    fn canonical_watch_classify_is_idempotent_until_commit() {
        // classify does NOT advance the baseline: an unhandled change keeps
        // reporting Changed until commit moves the baseline (so a failed re-stash
        // re-fires next cycle). After commit, the same blob is Unchanged.
        let mut watch = CanonicalWatch::new();
        watch.commit(&cred(b"A-token"));
        let fresh = cred(b"A-relogin-token");
        assert_eq!(watch.classify(&fresh), CanonicalChange::Changed);
        assert_eq!(watch.classify(&fresh), CanonicalChange::Changed);
        watch.commit(&fresh);
        assert_eq!(watch.classify(&fresh), CanonicalChange::Unchanged);
    }

    #[test]
    fn canonical_watch_commit_excludes_the_daemons_own_write() {
        // The Q3 invariant: priming (commit) to the token we just WROTE means our
        // own swap is not re-detected as an external change…
        let mut watch = CanonicalWatch::new();
        watch.commit(&cred(b"A-token"));
        watch.commit(&cred(b"B-token")); // we wrote B (a swap)
        assert_eq!(
            watch.classify(&cred(b"B-token")),
            CanonicalChange::Unchanged
        );
        // …while an external write landing AFTER our commit is still caught.
        assert_eq!(
            watch.classify(&cred(b"C-from-a-concurrent-login")),
            CanonicalChange::Changed
        );
    }

    #[test]
    fn canonical_watch_baseline_exposes_the_last_committed_blob() {
        // Issue #140: the external-login watch snapshots the last-committed baseline (read-only)
        // to compare an idle-time canonical read against. `None` before the first commit; the
        // committed blob after; a later commit advances it — and reading it never advances it.
        let mut watch = CanonicalWatch::new();
        assert!(
            watch.baseline().is_none(),
            "no baseline before the first commit"
        );
        watch.commit(&cred(b"A-token"));
        assert_eq!(watch.baseline().unwrap().expose(), b"A-token");
        watch.commit(&cred(b"B-token"));
        assert_eq!(watch.baseline().unwrap().expose(), b"B-token");
        // Read-only: taking the baseline does not move it, so the next classify is unaffected.
        let _ = watch.baseline();
        assert_eq!(
            watch.classify(&cred(b"B-token")),
            CanonicalChange::Unchanged
        );
    }

    /// Drives the real `security` CLI end-to-end against a throwaway keychain
    /// (created, used, and deleted here) — never the login keychain. macOS-only:
    /// `/usr/bin/security` is the system under test.
    #[cfg(target_os = "macos")]
    mod real_cli {
        use super::*;
        use std::process::Command as StdCommand;

        /// Make + unlock a throwaway keychain; return its path (kept alive by the
        /// returned tempdir guard).
        fn fresh_keychain() -> (tempfile::TempDir, PathBuf) {
            let dir = tempfile::tempdir().unwrap();
            let kc = dir.path().join("test.keychain-db");
            assert!(StdCommand::new(SECURITY)
                .args(["create-keychain", "-p", ""])
                .arg(&kc)
                .status()
                .expect("spawn create-keychain")
                .success());
            assert!(StdCommand::new(SECURITY)
                .args(["unlock-keychain", "-p", ""])
                .arg(&kc)
                .status()
                .expect("spawn unlock-keychain")
                .success());
            (dir, kc)
        }

        /// Seed a `Claude Code-credentials` item with a chosen `acct`/secret,
        /// simulating Claude Code's `/login` (or #4 capture).
        fn seed(kc: &Path, acct: &str, secret: &str) {
            assert!(StdCommand::new(SECURITY)
                .args([
                    "add-generic-password",
                    "-U",
                    "-s",
                    SERVICE_BASE,
                    "-a",
                    acct,
                    "-w",
                    secret
                ])
                .arg(kc)
                .status()
                .expect("spawn add-generic-password")
                .success());
        }

        fn delete(kc: &Path) {
            let _ = StdCommand::new(SECURITY)
                .arg("delete-keychain")
                .arg(kc)
                .status();
        }

        #[tokio::test]
        async fn resolves_stored_acct_then_round_trips_in_place() {
            let (_dir, kc) = fresh_keychain();
            // Deliberately NOT the macOS username, to prove resolve reads the
            // STORED acct rather than guessing `$USER`/`getpwuid`.
            seed(&kc, "sessiometer-roundtrip-acct", "initial-token");

            let store = RealCredentialStore::for_keychain(kc.clone());

            // Read resolves the stored acct and returns the seeded secret.
            let got = store.read().await.expect("read seeded credential");
            assert_eq!(got.expose(), b"initial-token");

            // In-place update via `-U`.
            let updated = Credential::new(b"updated-token-value".to_vec());
            store
                .write(&updated)
                .await
                .expect("write updated credential");

            // Re-reading succeeds AND returns the new value. A successful read
            // here also proves the write was in place: resolve enforces
            // uniqueness, so if `-U` had created a second item (the bug a
            // `getpwuid` guess would cause, since the seeded acct differs), this
            // read would fail `CredentialAmbiguous`.
            let reread = store.read().await.expect("re-read updated credential");
            assert_eq!(reread.expose(), b"updated-token-value");

            delete(&kc);
        }

        #[tokio::test]
        async fn read_reports_not_found_on_empty_keychain() {
            let (_dir, kc) = fresh_keychain();
            let store = RealCredentialStore::for_keychain(kc.clone());
            assert!(matches!(store.read().await, Err(Error::CredentialNotFound)));
            delete(&kc);
        }

        #[tokio::test]
        async fn read_reports_ambiguous_with_two_items() {
            let (_dir, kc) = fresh_keychain();
            seed(&kc, "acct-one", "token-one");
            seed(&kc, "acct-two", "token-two");
            let store = RealCredentialStore::for_keychain(kc.clone());
            assert!(matches!(
                store.read().await,
                Err(Error::CredentialAmbiguous { count: 2 })
            ));
            delete(&kc);
        }

        #[tokio::test]
        async fn write_round_trips_a_blob_with_shell_metacharacters() {
            // The off-argv `security -i` path must carry an arbitrary single-line
            // blob byte-exact — including every character that would matter to a
            // shell or a naive tokenizer: spaces, double quotes, backslashes, and
            // `$`/backticks/`;`/`|`/`&`. (The canonical blob is opaque to us.)
            let (_dir, kc) = fresh_keychain();
            seed(&kc, "sessiometer-meta-acct", "seed-token");
            let store = RealCredentialStore::for_keychain(kc.clone());
            let nasty = br#"{"t":"a b \" c \\ d $x `y` ;z |w &q"}"#;
            store
                .write(&Credential::new(nasty.to_vec()))
                .await
                .expect("write a blob with metacharacters");
            let got = store.read().await.expect("read it back");
            assert_eq!(got.expose(), nasty);
            delete(&kc);
        }

        /// Issue #39 acceptance, verified directly: the blob does not appear in
        /// the process command line during a write. Hold the `security -i` child's
        /// stdin open after feeding it the command — the CLI runs the line but
        /// stays alive reading stdin — then snapshot its argv via `ps`. The
        /// sentinel blob must be absent; argv is only `-i`.
        #[test]
        fn the_blob_never_appears_in_the_process_argv() {
            use std::io::Write as _;
            use std::thread::sleep;
            use std::time::Duration;

            let (_dir, kc) = fresh_keychain();
            const SENTINEL: &str = "SENTINEL-oauth-blob-must-never-reach-argv-39";
            let line = write_command_line(
                SERVICE_BASE,
                OsStr::new("ps-acct"),
                &kc,
                SENTINEL.as_bytes(),
            );

            let mut child = StdCommand::new(SECURITY)
                .arg("-i")
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .expect("spawn security -i");
            let mut stdin = child.stdin.take().expect("piped stdin");
            stdin.write_all(&line).expect("feed the write command");
            stdin.flush().expect("flush stdin");
            // Keep `stdin` open → `security -i` runs the line but stays alive, so
            // `ps` can observe a live process whose argv is fixed at spawn.
            sleep(Duration::from_millis(200));

            let pid = child.id().to_string();
            let ps = StdCommand::new("/bin/ps")
                .args(["-o", "command=", "-p", pid.as_str()])
                .output()
                .expect("spawn ps");
            let argv = String::from_utf8_lossy(&ps.stdout);

            // Close stdin → EOF → the CLI exits; reap it.
            drop(stdin);
            let _ = child.wait();

            assert!(
                argv.contains("security") && argv.contains("-i"),
                "sanity: ps should show the live `security -i` (got {argv:?})"
            );
            assert!(
                !argv.contains(SENTINEL),
                "the blob leaked into the process argv: {argv:?}"
            );

            // The off-argv write is functional, not inert: the item landed.
            let check = StdCommand::new(SECURITY)
                .args([
                    "find-generic-password",
                    "-w",
                    "-s",
                    SERVICE_BASE,
                    "-a",
                    "ps-acct",
                ])
                .arg(&kc)
                .output()
                .expect("spawn find-generic-password");
            assert!(check.status.success(), "the item should have been written");
            let mut stored = check.stdout;
            if stored.last() == Some(&b'\n') {
                stored.pop();
            }
            assert_eq!(stored, SENTINEL.as_bytes());
            delete(&kc);
        }

        /// The isolated-refresh engine's keychain primitive (issue #102), end-to-end
        /// against the real `security` CLI on a throwaway keychain: seed → read-back
        /// (silent, byte-exact, off-argv) → delete (idempotent). Targets the
        /// config-dir-suffixed service, not the bare canonical name.
        #[tokio::test]
        async fn isolated_item_seeds_reads_back_and_deletes_idempotently() {
            let (_dir, kc) = fresh_keychain();
            // An arbitrary isolated config dir → its #100-suffixed service; an
            // explicit acct keeps the test hermetic (no dependence on the live login
            // name). The service must be the SUFFIXED name, never the bare base.
            let config_dir = OsStr::new("/tmp/sessiometer-iso-roundtrip");
            let item = IsolatedKeychainItem::for_keychain(
                config_dir,
                OsString::from("iso-acct"),
                kc.clone(),
            )
            .unwrap();
            assert_ne!(
                service_for_config_dir(config_dir).unwrap(),
                SERVICE_BASE,
                "the isolated service must be suffixed, never the bare canonical"
            );

            // A blob carrying shell metacharacters, to prove the off-argv `security -i`
            // seed path is byte-exact for the isolated item too.
            let blob = br#"{"claudeAiOauth":{"accessToken":"sk-ant-oat-X $y `z` ;|&","refreshToken":"sk-ant-ort-X","expiresAt":9999999999999}}"#;
            item.seed(blob).await.expect("seed isolated item");

            let got = item.read_back().await.expect("read the isolated item back");
            assert_eq!(got.expose(), blob, "isolated read-back must be byte-exact");

            // Delete removes it; a second delete is an idempotent success (44 → Ok).
            item.delete().await.expect("delete isolated item");
            assert!(matches!(
                item.read_back().await,
                Err(Error::CredentialNotFound)
            ));
            item.delete().await.expect("re-delete is idempotent");

            // The synchronous Drop-path delete is also tolerant of an absent item.
            item.delete_blocking();

            delete(&kc);
        }
    }
}
