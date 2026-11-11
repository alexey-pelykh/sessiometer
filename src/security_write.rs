// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! The size limit of a `security -i` keychain write, and the argv fallback for a write
//! past it — shared by [`crate::keychain`] and [`crate::stash`], the two modules that
//! write secrets through `/usr/bin/security`.
//!
//! `security -i` reads each command with one `fgets` into a 4096-byte buffer, so a line
//! longer than [`INTERACTIVE_LINE_MAX`] bytes (its trailing newline included) is split:
//! the first 4095 bytes run as a command of their own and the remainder as a second,
//! bogus one. The split is **not** a clean failure. When it lands inside the quoted
//! secret, the tokenizer accepts the unterminated quote, so the first fragment stores a
//! **truncated secret** (in the default keychain — the pinned keychain path, which comes
//! after the secret, has been cut off) and only the leftover fragment exits non-zero.
//! Measured against `/usr/bin/security` on macOS 26 (Darwin 25.6): a 4095-byte line
//! round-trips, a longer one truncates.
//!
//! A real Claude Code credential crosses that limit: once it carries `mcpOAuth` tokens
//! beside `claudeAiOauth` it is ~4 KB of JSON before the `\"` escaping. Nothing in
//! `security` takes a secret that size off argv — `-w` with no value prompts through
//! `getpass`, which keeps 128 bytes and silently drops the rest — so an over-limit write
//! goes on argv as `-X <hex>`. That is the fallback Claude Code itself takes for the
//! same item ("Keychain payload … exceeds security -i stdin limit; using argv"), and it
//! runs the same binary, so ADR-0002's `apple-tool:` ACL ride is unchanged. The cost is
//! issue #39's exposure, for that write only: the hex-encoded secret is visible in the
//! short-lived `security` process's argv, and the copies [`Command`] makes of it cannot
//! be zeroized. A line that fits still goes through `security -i`, off argv.

use std::ffi::OsStr;
use std::path::Path;
use std::process::{Output, Stdio};

use tokio::process::Command;
use zeroize::Zeroizing;

use crate::error::Result;

/// Absolute path to the system `security` tool (see [`crate::keychain`] for why it is
/// pinned rather than resolved through `$PATH`).
const SECURITY: &str = "/usr/bin/security";

/// The longest `security -i` command line, trailing newline included, that one read
/// takes whole: its `fgets` buffer is 4096 bytes, one of them the terminating NUL.
pub(crate) const INTERACTIVE_LINE_MAX: usize = 4095;

/// Whether `line` (trailing newline included) reaches `security -i` as one command. A
/// line that does not must be written through [`run_argv_hex_write`] instead — feeding
/// it to `security -i` stores a truncated secret (see the module docs).
pub(crate) fn fits_interactive_read(line: &[u8]) -> bool {
    line.len() <= INTERACTIVE_LINE_MAX
}

/// argv (after the program name) for the over-limit write: an in-place (`-U`)
/// `add-generic-password` of `(service, acct)` with the secret as `-X` hex, pinning
/// `keychain` as the trailing positional — the same item the `-i` line addresses.
fn hex_write_args<'a>(
    service: &'a OsStr,
    acct: &'a OsStr,
    hex: &'a str,
    keychain: &'a Path,
) -> [&'a OsStr; 9] {
    [
        OsStr::new("add-generic-password"),
        OsStr::new("-U"),
        OsStr::new("-s"),
        service,
        OsStr::new("-a"),
        acct,
        OsStr::new("-X"),
        OsStr::new(hex),
        keychain.as_os_str(),
    ]
}

/// Write `payload` as the secret of `(service, acct)` in `keychain` through argv
/// `-X <hex>` — the path for a payload whose `security -i` line would not fit one read
/// ([`fits_interactive_read`]). `-X` stores the decoded bytes, so the item holds
/// exactly what the `-w` form would have stored, and a `find-generic-password -w` reads
/// it back unchanged. The exit status is the caller's to map, as for the `-i` path.
pub(crate) async fn run_argv_hex_write(
    service: &OsStr,
    acct: &OsStr,
    keychain: &Path,
    payload: &[u8],
) -> Result<Output> {
    let hex = Zeroizing::new(crate::hex::encode(payload));
    Ok(Command::new(SECURITY)
        .args(hex_write_args(service, acct, hex.as_str(), keychain))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_fits_one_interactive_read_up_to_4095_bytes_newline_included() {
        assert!(fits_interactive_read(b""));
        assert!(fits_interactive_read(&[b'a'; INTERACTIVE_LINE_MAX]));
        assert!(!fits_interactive_read(&[b'a'; INTERACTIVE_LINE_MAX + 1]));
        assert!(!fits_interactive_read(&[b'a'; 3 * INTERACTIVE_LINE_MAX]));
    }

    #[test]
    fn hex_write_args_update_in_place_and_pin_the_keychain_last() {
        let kc = Path::new("/tmp/login.keychain-db");
        let hex = crate::hex::encode(br#"{"t":"a \"b\" \\c"}"#);
        let args = hex_write_args(
            OsStr::new("Sessiometer/11111111-1111-1111-1111-111111111111"),
            OsStr::new("credential"),
            &hex,
            kc,
        );
        assert_eq!(
            args,
            [
                "add-generic-password",
                "-U",
                "-s",
                "Sessiometer/11111111-1111-1111-1111-111111111111",
                "-a",
                "credential",
                "-X",
                "7b2274223a2261205c22625c22205c5c63227d",
                "/tmp/login.keychain-db",
            ]
            .map(OsStr::new)
        );
        // No `-w`: the secret is carried only by `-X`, as lowercase hex.
        assert!(!args.contains(&OsStr::new("-w")));
    }

    /// Drives the real `security` CLI against a throwaway keychain (created, used and
    /// deleted here) — never the login keychain. macOS-only: `/usr/bin/security` is the
    /// system under test.
    #[cfg(target_os = "macos")]
    mod real_cli {
        use super::*;
        use std::path::PathBuf;
        use std::process::Command as StdCommand;

        fn fresh_keychain() -> (tempfile::TempDir, PathBuf) {
            let dir = tempfile::tempdir().unwrap();
            let kc = dir.path().join("test.keychain-db");
            for args in [["create-keychain", "-p", ""], ["unlock-keychain", "-p", ""]] {
                assert!(StdCommand::new(SECURITY)
                    .args(args)
                    .arg(&kc)
                    .status()
                    .expect("spawn security")
                    .success());
            }
            (dir, kc)
        }

        fn read_back(kc: &Path, service: &str, acct: &str) -> Vec<u8> {
            let out = StdCommand::new(SECURITY)
                .args(["find-generic-password", "-w", "-s", service, "-a", acct])
                .arg(kc)
                .output()
                .expect("spawn find-generic-password");
            assert!(out.status.success(), "item should be readable");
            let mut bytes = out.stdout;
            assert_eq!(bytes.pop(), Some(b'\n'), "-w appends one newline");
            bytes
        }

        fn delete(kc: &Path) {
            let _ = StdCommand::new(SECURITY)
                .arg("delete-keychain")
                .arg(kc)
                .status();
        }

        #[tokio::test]
        async fn an_over_limit_payload_round_trips_byte_exact_and_updates_in_place() {
            let (_dir, kc) = fresh_keychain();
            let (service, acct) = ("sessiometer-hex-test", "acct");
            let first = format!(r#"{{"t":"{}","q":"a \"b\" \\c $x `y`"}}"#, "x".repeat(6000));
            let second = format!(r#"{{"t":"{}"}}"#, "y".repeat(9000));

            for payload in [&first, &second] {
                let out = run_argv_hex_write(
                    OsStr::new(service),
                    OsStr::new(acct),
                    &kc,
                    payload.as_bytes(),
                )
                .await
                .expect("spawn security");
                assert!(out.status.success(), "write failed: {out:?}");
                // Read back the whole payload, not a 4095-byte prefix of it; the second
                // write proves `-U` replaced the first item instead of failing on it.
                assert_eq!(read_back(&kc, service, acct), payload.as_bytes());
            }
            delete(&kc);
        }
    }
}
