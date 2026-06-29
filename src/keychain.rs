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

/// Absolute path to the system `security` tool. Absolute (not bare `security`
/// resolved through `$PATH`) so a hijacked `PATH` cannot substitute a different
/// binary for this security-sensitive call.
const SECURITY: &str = "/usr/bin/security";

/// The generic-password service name Claude Code stores its credential under.
const SERVICE: &str = "Claude Code-credentials";

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
}

impl RealCredentialStore {
    /// Production store, operating on the login keychain.
    pub(crate) fn new() -> Self {
        Self {
            keychain: None,
            acct: OnceLock::new(),
        }
    }

    /// Store pinned to a specific keychain file.
    #[cfg(all(test, target_os = "macos"))]
    pub(crate) fn for_keychain(path: PathBuf) -> Self {
        Self {
            keychain: Some(path),
            acct: OnceLock::new(),
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
        parse_resolve(&String::from_utf8_lossy(&output.stdout))
    }
}

/// `find-generic-password` arguments (after the program name): read the secret
/// of the resolved item, pinning both `-a <acct>` and the keychain path.
fn read_args(acct: &OsStr, keychain: &Path) -> Vec<OsString> {
    vec![
        "find-generic-password".into(),
        "-w".into(),
        "-s".into(),
        SERVICE.into(),
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
fn write_command_line(acct: &OsStr, keychain: &Path, blob: &[u8]) -> Zeroizing<Vec<u8>> {
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
    push_quoted(&mut line, SERVICE.as_bytes());
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
    // One small (< pipe-buffer) write, so there is no stdin/stderr deadlock risk;
    // dropping the handle at the end of the statement closes the pipe → EOF.
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
        let digits: Vec<u8> = hex.bytes().take_while(|b| b.is_ascii_hexdigit()).collect();
        if digits.is_empty() || !digits.len().is_multiple_of(2) {
            return None;
        }
        let mut bytes = Vec::with_capacity(digits.len() / 2);
        for pair in digits.chunks_exact(2) {
            let hi = (pair[0] as char).to_digit(16)?;
            let lo = (pair[1] as char).to_digit(16)?;
            bytes.push((hi * 16 + lo) as u8);
        }
        Some(bytes)
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

/// Parse `security dump-keychain` output: find every generic-password item whose
/// service is `Claude Code-credentials`, then enforce uniqueness — 0 → not found,
/// >1 → ambiguous, exactly 1 → that item's `acct`.
fn parse_resolve(dump: &str) -> Result<OsString> {
    // One entry per service-matching item (its `acct`, if present). Count ALL
    // matches — including any with an absent `acct` — so a malformed item can
    // never mask an ambiguity by going uncounted.
    let mut matches: Vec<Option<Vec<u8>>> = Vec::new();
    // Each item block begins with a `keychain: "<path>"` header line.
    for block in dump.split("\nkeychain: ") {
        if !block.contains("class: \"genp\"") {
            continue;
        }
        if block_attr(block, "svce").as_deref() == Some(SERVICE.as_bytes()) {
            matches.push(block_attr(block, "acct"));
        }
    }
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

impl CredentialStore for RealCredentialStore {
    async fn read(&self) -> Result<Credential> {
        let acct = self.acct().await?;
        let keychain = self.keychain_path()?;
        let output = Command::new(SECURITY)
            .args(read_args(&acct, &keychain))
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
        let line = write_command_line(&acct, &keychain, credential.expose());
        let output = run_interactive_write(&line).await?;
        finish_write(output.status.success(), output.status.code().unwrap_or(-1))
    }
}

#[cfg(test)]
pub(crate) struct FakeCredentialStore {
    slot: RefCell<Option<Credential>>,
    /// When set, [`read`](CredentialStore::read) returns [`Error::KeychainLocked`]
    /// — the in-memory analog of a locked login keychain (exit 36), so the daemon's
    /// locked-path backoff (#13) is testable without a real keychain.
    locked: Cell<bool>,
}

#[cfg(test)]
impl FakeCredentialStore {
    pub(crate) fn empty() -> Self {
        Self {
            slot: RefCell::new(None),
            locked: Cell::new(false),
        }
    }

    /// Simulate the login keychain locking (`true`) or unlocking (`false`): while
    /// locked, `read` returns [`Error::KeychainLocked`] (issue #13).
    pub(crate) fn set_locked(&self, locked: bool) {
        self.locked.set(locked);
    }
}

#[cfg(test)]
impl CredentialStore for FakeCredentialStore {
    async fn read(&self) -> Result<Credential> {
        if self.locked.get() {
            return Err(Error::KeychainLocked { op: "read" });
        }
        self.slot
            .borrow()
            .clone()
            .ok_or(Error::Unimplemented("no credential stashed in the fake"))
    }

    async fn write(&self, credential: &Credential) -> Result<()> {
        *self.slot.borrow_mut() = Some(credential.clone());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_args_pin_service_acct_and_keychain() {
        let kc = Path::new("/tmp/login.keychain-db");
        assert_eq!(
            read_args(OsStr::new("alice"), kc),
            vec![
                OsString::from("find-generic-password"),
                OsString::from("-w"),
                OsString::from("-s"),
                OsString::from(SERVICE),
                OsString::from("-a"),
                OsString::from("alice"),
                kc.as_os_str().to_owned(),
            ]
        );
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
        let line = write_command_line(OsStr::new("alice"), kc, br#"tok "x" \y"#);
        // Exactly the `-w` command, every field double-quoted, the blob's `"` and
        // `\` escaped, one trailing newline. The blob lives only inside this
        // stdin line — the spawned process's argv is the constant `-i`.
        let expected = format!(
            "add-generic-password -U -s \"{SERVICE}\" -a \"alice\" -w \"tok \\\"x\\\" \\\\y\" \"{}\"\n",
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
            parse_resolve(ONE_MATCH).unwrap(),
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
        assert_eq!(parse_resolve(dump).unwrap(), OsString::from("alice"));
    }

    #[test]
    fn parse_resolve_reports_not_found_when_absent() {
        let dump = r#"keychain: "/tmp/x.keychain-db"
class: "genp"
    "acct"<blob>="someone"
    "svce"<blob>="Some Other Service"
"#;
        assert!(matches!(
            parse_resolve(dump),
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
            parse_resolve(dump),
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
            parse_resolve(dump),
            Err(Error::CredentialAmbiguous { count: 2 })
        ));
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
                    SERVICE,
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
            let line = write_command_line(OsStr::new("ps-acct"), &kc, SENTINEL.as_bytes());

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
                    SERVICE,
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
    }
}
