// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! The background service: install/uninstall `sessiometer run` as a per-user
//! launchd **LaunchAgent** (issue #166).
//!
//! A LaunchAgent (not a system-wide LaunchDaemon) because the swap loop needs the
//! user's **login keychain**, which only exists inside the per-user GUI session.
//! The agent is `RunAtLoad` + `KeepAlive`, so it starts at login and stays up
//! across the whole session — the poll loop runs continuously and does not
//! idle-exit. That is what an always-present UI attaches to, and why swaps keep
//! happening with no terminal open.
//!
//! **The single-owner guard is upheld here by construction, not re-implemented.**
//! The plist's `ProgramArguments` is exactly `[<binary>, "run"]`, and `run`
//! acquires the single-instance [`InstanceLock`](crate::daemon::InstanceLock) on
//! `daemon.lock` FIRST — before the roster load, the socket bind, or any swap. So
//! whatever launchd invokes is the lock-guarded run-loop (never a path that enters
//! the loop without the lock), and a foreground `run` and the background agent can
//! never both drive the swap loop: whichever starts second gets
//! [`Error::AlreadyRunning`] (process exit
//! `3`) and performs no swap. This is a **safety** guard — there is deliberately
//! no `--force`-style bypass on `run`.
//!
//! Following the CONTRIBUTING "transport rule", the launchd control plane is driven
//! through the system CLI at an absolute path (`/bin/launchctl`) rather than a
//! client crate, so no new dependency enters the graph. The launchctl arguments
//! (a label, a plist path, a domain target) carry no secret, so they ride argv
//! normally — the "secrets on stdin" half of the rule does not apply here.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use tokio::process::Command;

use crate::error::{Error, Result};
use crate::paths;

/// The canonical launchd label — the ratified `org.sessiometer` bundle namespace
/// (grounded by the owned `sessiometer.org`; issue #329), which the macOS app and this
/// embedded daemon share for a coherent `org.sessiometer.*` identity. It is the exact
/// label the app's `SMAppService` registration (#170) will register. Doubles as the
/// plist filename stem (`<label>.plist`) and the trailing component of the `bootout`
/// service target.
const AGENT_LABEL: &str = "org.sessiometer.agent";

/// The label this daemon shipped under before the `org.sessiometer` rename (issue #329).
/// It is booted out and its plist deleted on every `install`/`uninstall`, so upgrading
/// past the rename never orphans a stale `io.github.alexey-pelykh.sessiometer.plist` or
/// a crash-looping agent (the old `RunAtLoad` + `KeepAlive` agent would otherwise keep
/// losing the single-instance lock to the new one and be restarted forever). Pre-0.1.0 /
/// unreleased ⇒ in practice this matches nothing; the cleanup is a defensive, idempotent
/// no-op.
const LEGACY_AGENT_LABEL: &str = "io.github.alexey-pelykh.sessiometer";

/// `/bin/launchctl`, absolute (the transport rule): a hijacked `$PATH` cannot
/// substitute a different binary for the service-control call.
const LAUNCHCTL: &str = "/bin/launchctl";

/// Install and start the LaunchAgent: render the plist for THIS binary, write it to
/// `~/Library/LaunchAgents`, and (re)load it into the per-user launchd domain.
///
/// Idempotent: a stale copy is booted out first (best-effort, tolerating "not
/// loaded"), so re-running `service install` refreshes the plist — e.g. after the
/// binary moved — and reloads cleanly.
pub(crate) async fn install() -> Result<()> {
    let program = current_binary()?;
    let logs = paths::logs_dir()?;
    let stdout_log = logs.join("daemon.out.log");
    let stderr_log = logs.join("daemon.err.log");
    // launchd creates the redirect files but not their parent, so ensure the log
    // dir exists (our own private dir → 0700).
    paths::ensure_private_dir(&logs)?;

    let environment = passthrough_environment();
    let contents = render_plist(
        AGENT_LABEL,
        &program,
        &["run"],
        &stdout_log,
        &stderr_log,
        &environment,
    );

    // `~/Library/LaunchAgents` is a shared, system-defined location — create it if
    // absent WITHOUT narrowing its permissions (unlike our private state dirs).
    let agents_dir = paths::launch_agents_dir()?;
    std::fs::create_dir_all(&agents_dir)?;
    let plist = agents_dir.join(format!("{AGENT_LABEL}.plist"));
    write_plist(&plist, &contents)?;

    // Best-effort pre-clean so a re-install is idempotent (tolerate "not loaded"),
    // then bootstrap the freshly-written plist. `bootstrap`/`bootout` are the modern
    // replacements for the deprecated `load -w`/`unload -w`.
    let _ = launchctl(&["bootout", &service_target(AGENT_LABEL)]).await;
    // Migration (#329): also retire any agent left over from the pre-rename label, so an
    // upgrade past the rename doesn't leave the old one crash-looping beside this one.
    // Best-effort (like the bootouts above): installing THIS agent must not be blocked by
    // trouble removing a stale OLD file — `uninstall` propagates the cleanup as the backstop.
    let _ = remove_legacy_agent().await;
    launchctl(&["bootstrap", &domain_target(), &plist.to_string_lossy()]).await?;

    // Operational status → stderr, matching the daemon's own `sessiometer: daemon
    // started …` line; stdout is reserved for command data (status/list/export).
    eprintln!(
        "sessiometer: background service installed and started ({AGENT_LABEL}).\n\
         It runs `sessiometer run` at login and stays up across the session.\n\
         plist: {}\n\
         logs:  {}",
        plist.display(),
        logs.display(),
    );
    Ok(())
}

/// Stop and uninstall the LaunchAgent: unload it from the per-user launchd domain
/// and delete its plist. Idempotent — an already-unloaded agent or an absent plist
/// is success, so `service uninstall` is safe to run twice.
pub(crate) async fn uninstall() -> Result<()> {
    // Tolerate "not loaded" so the unload half is idempotent.
    let _ = launchctl(&["bootout", &service_target(AGENT_LABEL)]).await;

    let plist = paths::launch_agents_dir()?.join(format!("{AGENT_LABEL}.plist"));
    remove_plist_file(&plist)?;

    // Migration (#329): also retire any leftover pre-rename agent + plist, so a user who
    // upgrades past the rename and then uninstalls isn't left with a stale
    // `io.github.alexey-pelykh.sessiometer.plist` / running agent behind.
    remove_legacy_agent().await?;

    eprintln!("sessiometer: background service uninstalled ({AGENT_LABEL}).");
    Ok(())
}

/// The launchd domain target for a per-user LaunchAgent: `gui/<uid>`.
fn domain_target() -> String {
    format!("gui/{}", paths::current_uid())
}

/// The launchd **service** target: `gui/<uid>/<label>` — the domain target plus an
/// agent's label, the form `bootout` takes to unload one named service. Parameterized by
/// label so the same builder targets both the current agent and the [`LEGACY_AGENT_LABEL`]
/// the #329 migration cleans up.
fn service_target(label: &str) -> String {
    format!("gui/{}/{label}", paths::current_uid())
}

/// Remove a LaunchAgent plist by path, tolerating absence (`NotFound` = success). The
/// file half of an uninstall / migration cleanup — factored out so it is exercised
/// directly in tests against a temp dir (the `launchctl bootout` half needs a live GUI
/// login domain and cannot run hermetically).
fn remove_plist_file(plist: &Path) -> Result<()> {
    match std::fs::remove_file(plist) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(Error::Io(e)),
    }
    Ok(())
}

/// Retire any LaunchAgent left over from the pre-rename [`LEGACY_AGENT_LABEL`] (issue #329
/// migration): boot it out of the per-user domain (best-effort, tolerating "not loaded")
/// and delete its plist (tolerating absence). Idempotent, and a no-op in the common
/// unreleased case where the old label was never installed. Called from both `install`
/// and `uninstall` so neither path can orphan the old identity.
async fn remove_legacy_agent() -> Result<()> {
    let _ = launchctl(&["bootout", &service_target(LEGACY_AGENT_LABEL)]).await;
    let plist = paths::launch_agents_dir()?.join(format!("{LEGACY_AGENT_LABEL}.plist"));
    remove_plist_file(&plist)
}

/// The absolute path to THIS running `sessiometer` binary — what the plist must
/// invoke. Canonicalized so a relative or symlinked launch still yields the stable
/// absolute path launchd needs to `exec`; falls back to the raw `current_exe` if
/// the canonical form is unavailable (should not happen for a live executable).
fn current_binary() -> Result<PathBuf> {
    let exe = std::env::current_exe()?;
    Ok(std::fs::canonicalize(&exe).unwrap_or(exe))
}

/// The environment to bake into the plist so the agent reads the SAME state the
/// installing shell does. launchd does not inherit the shell environment, so an
/// operator who set `XDG_CONFIG_HOME` (which redirects the config dir) would
/// otherwise get an agent reading a DIFFERENT config than their shell. Captured at
/// install time; empty in the common case (the native `~/Library/Application
/// Support` config, where the runtime lock/socket live regardless of XDG).
fn passthrough_environment() -> Vec<(String, String)> {
    let mut env = Vec::new();
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        if !xdg.is_empty() {
            env.push((
                "XDG_CONFIG_HOME".to_owned(),
                xdg.to_string_lossy().into_owned(),
            ));
        }
    }
    env
}

/// Write the plist `0644` (world-readable is conventional for a LaunchAgent plist —
/// it holds no secret), overwriting any prior copy so a re-install refreshes it.
/// `set_permissions` after the write pins the mode even on the overwrite path,
/// where `OpenOptions::mode` (create-only) would not.
fn write_plist(path: &Path, contents: &str) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, contents)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644))?;
    Ok(())
}

/// Run `launchctl <args…>`, mapping a non-zero exit to [`Error::LaunchctlFailed`]
/// with the subcommand, exit code, and stderr. launchctl's output is non-secret
/// (labels, paths, domain targets), so surfacing it verbatim points the operator at
/// the actionable failure rather than leaking anything.
async fn launchctl(args: &[&str]) -> Result<()> {
    let output = Command::new(LAUNCHCTL)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .await?;
    if output.status.success() {
        return Ok(());
    }
    let subcommand = args.first().copied().unwrap_or("");
    let code = output.status.code().unwrap_or(-1);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stderr = stderr.trim();
    let detail = if stderr.is_empty() {
        format!("`launchctl {subcommand}` exited {code}")
    } else {
        format!("`launchctl {subcommand}` exited {code}: {stderr}")
    };
    Err(Error::LaunchctlFailed(detail))
}

/// XML-escape a string for a plist `<string>` value. Only the five predefined XML
/// entities matter (paths are UTF-8), and escaping keeps a home dir containing `&`
/// (or `<`, `>`, quotes) from producing a malformed plist launchd would refuse.
fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

/// Render the LaunchAgent plist — the module's pure, fully-tested core.
///
/// `program` is the absolute `sessiometer` binary path; `args` follow it in
/// `ProgramArguments` (always `["run"]` — the lock-guarded verb, issue #166's
/// "whatever launchd invokes is the lock-guarded run-loop"). `RunAtLoad` +
/// `KeepAlive` are both `true` so the agent starts at login and is kept up across
/// the session. `environment` is baked into an `EnvironmentVariables` dict (omitted
/// entirely when empty).
fn render_plist(
    label: &str,
    program: &Path,
    args: &[&str],
    stdout_log: &Path,
    stderr_log: &Path,
    environment: &[(String, String)],
) -> String {
    // The binary first, then each argument — each its own <string>. This is argv,
    // not a shell line, so there is no quoting or word-splitting to get wrong.
    let mut program_args = format!(
        "    <string>{}</string>\n",
        xml_escape(&program.to_string_lossy())
    );
    for arg in args {
        program_args.push_str(&format!("    <string>{}</string>\n", xml_escape(arg)));
    }

    let environment_block = if environment.is_empty() {
        String::new()
    } else {
        let mut block = String::from("  <key>EnvironmentVariables</key>\n  <dict>\n");
        for (key, value) in environment {
            block.push_str(&format!(
                "    <key>{}</key>\n    <string>{}</string>\n",
                xml_escape(key),
                xml_escape(value),
            ));
        }
        block.push_str("  </dict>\n");
        block
    };

    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{label}</string>
  <key>ProgramArguments</key>
  <array>
{program_args}  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
  <key>StandardOutPath</key>
  <string>{stdout}</string>
  <key>StandardErrorPath</key>
  <string>{stderr}</string>
{environment_block}</dict>
</plist>
"#,
        label = xml_escape(label),
        stdout = xml_escape(&stdout_log.to_string_lossy()),
        stderr = xml_escape(&stderr_log.to_string_lossy()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::InstanceLock;

    /// Pull the `ProgramArguments` `<string>` values out of a rendered plist.
    fn program_arguments(plist: &str) -> Vec<String> {
        let key = plist
            .find("<key>ProgramArguments</key>")
            .expect("ProgramArguments key present");
        let array_start = plist[key..].find("<array>").expect("array open") + key;
        let array_end = plist[array_start..].find("</array>").expect("array close") + array_start;
        plist[array_start..array_end]
            .lines()
            .filter_map(|line| {
                let line = line.trim();
                line.strip_prefix("<string>")
                    .and_then(|rest| rest.strip_suffix("</string>"))
                    .map(str::to_owned)
            })
            .collect()
    }

    #[test]
    fn the_agent_label_is_the_ratified_org_sessiometer_namespace() {
        // Issue #329 AC: the daemon label is exactly `org.sessiometer.agent`. It is the
        // plist filename stem, the trailing component of the launchctl service target,
        // and the exact label #170 (SMAppService) will register — so the app and this
        // embedded daemon share one `org.sessiometer.*` identity.
        assert_eq!(AGENT_LABEL, "org.sessiometer.agent");
        assert_eq!(
            service_target(AGENT_LABEL),
            format!("gui/{}/org.sessiometer.agent", paths::current_uid()),
            "the service target's trailing component is the new label",
        );
    }

    #[test]
    fn the_pre_rename_agent_plist_is_cleaned_up_and_the_cleanup_is_idempotent() {
        // Issue #329 migration + AC: an install → uninstall cycle must leave no
        // `io.github.alexey-pelykh.sessiometer.plist` behind. The migration still knows
        // the OLD stem so it can target it, and — because the project is unreleased, so
        // nothing was installed under it in practice — the cleanup must be a tolerated
        // no-op when the legacy plist is absent. This exercises the file half against a
        // temp `LaunchAgents` dir; the `launchctl bootout` half needs a live GUI login
        // domain and is not hermetic.
        assert_eq!(
            LEGACY_AGENT_LABEL, "io.github.alexey-pelykh.sessiometer",
            "the migration targets the exact pre-rename label",
        );

        let dir = tempfile::tempdir().unwrap();
        let legacy = dir.path().join(format!("{LEGACY_AGENT_LABEL}.plist"));

        // A leftover legacy plist is removed …
        std::fs::write(&legacy, "stale").unwrap();
        remove_plist_file(&legacy).expect("removing an existing legacy plist succeeds");
        assert!(
            !legacy.exists(),
            "no `io.github.alexey-pelykh.sessiometer.plist` remains after cleanup",
        );

        // … and cleaning up again with nothing there is a no-op, not an error — the
        // unreleased common case, and what keeps install/uninstall idempotent.
        remove_plist_file(&legacy).expect("removing an absent legacy plist is a tolerated no-op");
    }

    #[test]
    fn the_launch_agent_invokes_the_lock_guarded_run_verb() {
        // AC4: "whatever launchd invokes is the lock-guarded run-loop, not a path
        // that enters the loop without the lock." The plist must exec the binary
        // with exactly `run` — the verb that acquires the single-instance lock FIRST
        // (cli::run) — and NOTHING that could bypass it (no `--force`, no other verb).
        let plist = render_plist(
            AGENT_LABEL,
            Path::new("/opt/sessiometer/bin/sessiometer"),
            &["run"],
            Path::new("/logs/out"),
            Path::new("/logs/err"),
            &[],
        );
        assert_eq!(
            program_arguments(&plist),
            vec![
                "/opt/sessiometer/bin/sessiometer".to_owned(),
                "run".to_owned(),
            ],
            "launchd invokes the binary with the lock-guarded `run` verb and nothing else",
        );
    }

    #[test]
    fn coexistence_is_rejected_because_the_agent_and_a_foreground_run_share_one_lock() {
        // AC2/AC3: with one instance holding the daemon lock, a second `run` refuses
        // and performs no swap. The LaunchAgent invokes `run` (asserted above), and
        // `run` takes this same single-instance lock before any swap — so the agent
        // and a foreground `run` can never both drive the loop. This asserts the
        // rejection AND the exit-`3` signal a supervisor / the shell observes.
        let dir = tempfile::tempdir().unwrap();
        let lock_path = dir.path().join("daemon.lock");

        // The background LaunchAgent's `run` owns the lock.
        let _agent = InstanceLock::acquire(&lock_path).expect("the agent acquires the lock");

        // A foreground `sessiometer run` started while the agent owns it is refused,
        // before it can reach a swap. (`InstanceLock` is not `Debug`, so match rather
        // than `unwrap_err` — the same shape the daemon-side lock test uses.)
        let err = match InstanceLock::acquire(&lock_path) {
            Ok(_) => panic!("a second run coexisting with the agent must be refused, not granted"),
            Err(err) => err,
        };
        assert!(
            matches!(err, Error::AlreadyRunning),
            "the refusal is AlreadyRunning: {err}",
        );
        assert_eq!(
            err.exit_code(),
            3,
            "the refusal exits 3 so a supervisor can tell it from a generic failure",
        );
    }

    #[test]
    fn keep_alive_and_run_at_load_persist_the_agent_across_the_session() {
        // AC1: installed as a LaunchAgent that persists across the session — it starts
        // at login (RunAtLoad) and is kept up (KeepAlive, the continuously-polling loop).
        let plist = render_plist(
            AGENT_LABEL,
            Path::new("/bin/sessiometer"),
            &["run"],
            Path::new("/logs/out"),
            Path::new("/logs/err"),
            &[],
        );
        assert!(
            plist.contains("<key>RunAtLoad</key>\n  <true/>"),
            "RunAtLoad is true so the agent starts at login",
        );
        assert!(
            plist.contains("<key>KeepAlive</key>\n  <true/>"),
            "KeepAlive is true so the poll loop is kept up across the session",
        );
        assert!(plist.contains(&format!("<string>{AGENT_LABEL}</string>")));
    }

    #[test]
    fn the_plist_wires_stdout_and_stderr_to_the_log_paths() {
        let plist = render_plist(
            AGENT_LABEL,
            Path::new("/bin/sessiometer"),
            &["run"],
            Path::new("/home/u/Library/Logs/sessiometer/daemon.out.log"),
            Path::new("/home/u/Library/Logs/sessiometer/daemon.err.log"),
            &[],
        );
        assert!(plist.contains(
            "<key>StandardOutPath</key>\n  <string>/home/u/Library/Logs/sessiometer/daemon.out.log</string>"
        ));
        assert!(plist.contains(
            "<key>StandardErrorPath</key>\n  <string>/home/u/Library/Logs/sessiometer/daemon.err.log</string>"
        ));
    }

    #[test]
    fn an_empty_environment_omits_the_environment_variables_block() {
        let plist = render_plist(
            AGENT_LABEL,
            Path::new("/bin/sessiometer"),
            &["run"],
            Path::new("/logs/out"),
            Path::new("/logs/err"),
            &[],
        );
        assert!(
            !plist.contains("EnvironmentVariables"),
            "no environment dict when there is nothing to pass through",
        );
    }

    #[test]
    fn a_passthrough_environment_is_baked_into_the_plist() {
        // The XDG_CONFIG_HOME passthrough: launchd does not inherit the shell env, so
        // an operator who redirected the config dir needs it captured into the plist.
        let plist = render_plist(
            AGENT_LABEL,
            Path::new("/bin/sessiometer"),
            &["run"],
            Path::new("/logs/out"),
            Path::new("/logs/err"),
            &[("XDG_CONFIG_HOME".to_owned(), "/home/u/.config".to_owned())],
        );
        assert!(plist.contains("<key>EnvironmentVariables</key>"));
        assert!(plist.contains("<key>XDG_CONFIG_HOME</key>\n    <string>/home/u/.config</string>"));
    }

    #[test]
    fn string_values_are_xml_escaped() {
        // A home dir (hence a binary or log path) can contain `&`; unescaped it would
        // produce a plist launchd rejects as malformed.
        let plist = render_plist(
            AGENT_LABEL,
            Path::new("/Users/a&b/sessiometer"),
            &["run"],
            Path::new("/Users/a&b/out"),
            Path::new("/Users/a&b/err"),
            &[],
        );
        assert!(
            plist.contains("<string>/Users/a&amp;b/sessiometer</string>"),
            "the `&` in the path is escaped to `&amp;`",
        );
        assert!(
            !plist.contains("a&b"),
            "no raw `&` survives into the rendered plist",
        );
    }

    #[test]
    fn xml_escape_covers_the_five_predefined_entities() {
        assert_eq!(
            xml_escape("a&b<c>d\"e'f"),
            "a&amp;b&lt;c&gt;d&quot;e&apos;f"
        );
        assert_eq!(xml_escape("plain/path"), "plain/path");
    }

    #[test]
    fn the_rendered_plist_is_well_formed_at_the_envelope() {
        let plist = render_plist(
            AGENT_LABEL,
            Path::new("/bin/sessiometer"),
            &["run"],
            Path::new("/logs/out"),
            Path::new("/logs/err"),
            &[],
        );
        assert!(plist.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n"));
        assert!(plist.trim_end().ends_with("</plist>"));
        assert!(plist.contains("<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\""));
    }

    #[test]
    fn the_rendered_plist_passes_macos_plutil_lint() {
        // The strongest correctness check for the artifact this module exists to
        // produce: macOS's OWN property-list parser accepts what we render — both the
        // plain form and the one carrying an `EnvironmentVariables` dict. `/usr/bin/
        // plutil` is always present on macOS, like the `security` the keychain tests
        // shell out to (the suite is already macOS-bound).
        for environment in [
            Vec::new(),
            vec![("XDG_CONFIG_HOME".to_owned(), "/home/u/.config".to_owned())],
        ] {
            let plist = render_plist(
                AGENT_LABEL,
                Path::new("/opt/sessiometer/bin/sessiometer"),
                &["run"],
                Path::new("/home/u/Library/Logs/sessiometer/daemon.out.log"),
                Path::new("/home/u/Library/Logs/sessiometer/daemon.err.log"),
                &environment,
            );
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("agent.plist");
            std::fs::write(&path, &plist).unwrap();

            let output = std::process::Command::new("/usr/bin/plutil")
                .arg("-lint")
                .arg(&path)
                .output()
                .expect("plutil is available on macOS");
            assert!(
                output.status.success(),
                "plutil rejected the rendered plist:\n{plist}\nstdout: {}\nstderr: {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr),
            );
        }
    }
}
