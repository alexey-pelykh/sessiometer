// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT
//! Dummy credentials, a loopback Anthropic mock, and one persistent Claude process.
//! This measures credential selection, NOT whether Anthropic accepts a setup token.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{json, Value};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::process::{Child, Command};
use tokio::task::{JoinHandle, JoinSet};
use zeroize::Zeroizing;

use crate::file_policy;

#[derive(Debug, thiserror::Error)]
enum Failure {
    #[error("{0}")]
    Message(&'static str),
    #[error("probe I/O failed: {0}")]
    Io(#[from] std::io::Error),
}
type Result<T> = std::result::Result<T, Failure>;
impl From<crate::error::Error> for Failure {
    fn from(_: crate::error::Error) -> Self {
        Self::Message("Production storage/swap operation failed; credential data withheld.")
    }
}

/// Only dummy records; the real production adapters and swap lock perform every write.
mod profile {
    use super::*;
    use crate::file_storage::{FileAccountStash, FileCredentialStore, NativeFileAuthority};
    use crate::keychain::{Credential, CredentialStore};
    use crate::stash::{AccountStash, StashedAccount};

    pub(super) fn fingerprint(token: &str) -> String {
        crate::sha256::sha256_hex(token.as_bytes())[..16].to_owned()
    }

    fn store(path: &Path) -> Result<FileCredentialStore> {
        #[cfg(target_os = "macos")]
        let namespace = Some(crate::keychain::service_for_selection(
            None,
            Some(path.as_os_str()),
        )?);
        #[cfg(not(target_os = "macos"))]
        let namespace: Option<String> = None;
        Ok(FileCredentialStore::new(
            path.to_path_buf(),
            NativeFileAuthority::new(namespace.as_deref())?,
        ))
    }

    fn account(id: &str, token: &str) -> Result<StashedAccount> {
        Ok(StashedAccount {
            credential: Credential::new(
                serde_json::to_vec(&json!({"claudeAiOauth": {
                    "accessToken": token, "refreshToken": null, "expiresAt": null,
                    "scopes": ["user:inference"], "subscriptionType": null, "rateLimitTier": null
                }}))
                .map_err(|_| Failure::Message("Cannot encode dummy credential."))?,
            ),
            oauth_account: Some(crate::claude_state::OauthAccount::from_object_bytes(
                format!("{{\"accountUuid\":\"{id}\",\"emailAddress\":\"probe@example.invalid\"}}")
                    .as_bytes(),
            )?),
        })
    }

    pub(super) async fn initialize(path: &Path, token: &str) -> Result<Value> {
        let root = path
            .parent()
            .ok_or(Failure::Message("Missing probe root."))?;
        let stash = FileAccountStash::new(root.join("accounts"));
        let lock = root.join("swap.lock");
        let _guard = crate::swap::SwapLock::acquire(&lock, Duration::from_secs(1)).await?;
        let a = account("probe-A", token)?;
        stash.write("Sessiometer/probe-A", &a).await?;
        stash
            .write("Sessiometer/probe-B", &account("probe-B", TOKEN_B)?)
            .await?;
        store(path)?.write(&a.credential).await?;
        crate::paths::write_private_file(&path.join(".claude.json"), b"{}")?;
        crate::claude_state::write_oauth_account(
            &path.join(".claude.json"),
            a.require_identity()?,
        )?;
        Ok(json!({"selected": "A", "writer": "production-file-adapter"}))
    }

    pub(super) async fn rotate(path: &Path, token: &str) -> Result<Value> {
        let root = path
            .parent()
            .ok_or(Failure::Message("Missing probe root."))?;
        let (outgoing, incoming) = if token == TOKEN_B {
            ("Sessiometer/probe-A", "Sessiometer/probe-B")
        } else {
            ("Sessiometer/probe-B", "Sessiometer/probe-A")
        };
        let report = crate::swap::swap_locked(
            Some((&root.join("swap.lock"), Duration::from_secs(1))),
            &store(path)?,
            &FileAccountStash::new(root.join("accounts")),
            outgoing,
            incoming,
            &path.join(".claude.json"),
        )
        .await?;
        if !report.canonical_confirmed {
            return Err(Failure::Message("Canonical swap not confirmed."));
        }
        Ok(json!({"fingerprint": fingerprint(token), "writer": "production-swap-engine"}))
    }
}

// Deliberately invalid tokens. Never ask for or discover a user's real credentials.
const TOKEN_A: &str = "sessiometer-probe-not-a-real-oauth-token-A";
const TOKEN_B: &str = "sessiometer-probe-not-a-real-oauth-token-B";
const PROMPTS: [&str; 3] = [
    "Sessiometer file-auth probe turn 1. Reply OK.",
    "Sessiometer file-auth probe turn 2. Reply OK.",
    "Sessiometer file-auth probe turn 3. Reply OK.",
];
const MAX_BODY: usize = 2 * 1024 * 1024;

#[derive(Clone, Serialize)]
struct Observation {
    turn: Option<usize>,
    selected: &'static str,
    fingerprint: Option<String>,
}

#[derive(Default, Serialize)]
struct Evidence {
    observations: Vec<Observation>,
    protocol_errors: usize,
}

type SharedEvidence = Arc<Mutex<Evidence>>;

/// A completed result alone is insufficient: the mock must have seen the request.
fn verify_turn(evidence: &Evidence, turn: usize, expected: &str) -> Result<()> {
    let matching: Vec<_> = evidence
        .observations
        .iter()
        .filter(|o| o.turn == Some(turn))
        .collect();
    if matching.is_empty() {
        return Err(Failure::Message(
            "Claude completed without a matching mock API request.",
        ));
    }
    if matching.iter().any(|o| o.selected != expected) {
        return Err(Failure::Message(
            "Claude did not send the newly selected credential.",
        ));
    }
    if evidence.protocol_errors != 0
        || evidence
            .observations
            .iter()
            .any(|o| o.selected == "unexpected")
    {
        return Err(Failure::Message(
            "The mock observed an unsupported request or credential.",
        ));
    }
    Ok(())
}

struct AbortTask(JoinHandle<()>);

impl Drop for AbortTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

struct ChildGuard {
    child: Child,
    pid: u32,
}

impl ChildGuard {
    fn spawn(command: &mut Command) -> Result<Self> {
        // The process group makes timeout/error cleanup cover descendants too.
        command.as_std_mut().process_group(0);
        let child = command.kill_on_drop(true).spawn()?;
        let pid = child
            .id()
            .ok_or(Failure::Message("Missing Claude process identifier."))?;
        Ok(Self { child, pid })
    }

    async fn stop(&mut self) {
        // SAFETY: negative PID addresses only the process group created above.
        unsafe {
            libc::kill(-(self.pid as i32), libc::SIGKILL);
        }
        let _ = self.child.wait().await;
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        // Do not signal a reaped child's possibly reused identifier.
        if self.child.id().is_some() {
            // SAFETY: see spawn/stop; no pointers are passed to kill.
            unsafe {
                libc::kill(-(self.pid as i32), libc::SIGKILL);
            }
            let _ = self.child.start_kill();
        }
    }
}

// Needed for process_group on the std Command underneath tokio's wrapper.
use std::os::unix::process::CommandExt;

fn resolve_claude(explicit: Option<PathBuf>) -> Result<PathBuf> {
    let found = explicit
        .or_else(|| {
            std::env::var_os("PATH").and_then(|path| {
                std::env::split_paths(&path)
                    .map(|p| p.join("claude"))
                    .find(|p| p.is_file())
            })
        })
        .ok_or(Failure::Message(
            "Claude was not found; pass --claude with its executable path.",
        ))?;
    Ok(std::fs::canonicalize(found)?)
}

/// Build from an allowlist rather than trying to enumerate every auth override.
fn isolated_environment(root: &Path, port: u16) -> Vec<(OsString, OsString)> {
    let mut env = Vec::new();
    for key in ["PATH", "USER", "LOGNAME"] {
        if let Some(value) = std::env::var_os(key) {
            env.push((key.into(), value));
        }
    }
    for (key, relative) in [
        ("HOME", "home"),
        ("TMPDIR", "tmp"),
        ("PWD", "cwd"),
        ("XDG_CONFIG_HOME", "config"),
        ("XDG_CACHE_HOME", "cache"),
        ("XDG_DATA_HOME", "data"),
        ("CLAUDE_CONFIG_DIR", "profile"),
    ] {
        env.push((key.into(), root.join(relative).into_os_string()));
    }
    for (key, value) in [
        ("LANG", "en_US.UTF-8"),
        ("LC_ALL", "en_US.UTF-8"),
        ("DISABLE_AUTOUPDATER", "1"),
        ("DISABLE_TELEMETRY", "1"),
        ("DISABLE_ERROR_REPORTING", "1"),
        ("DISABLE_BUG_COMMAND", "1"),
        ("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1"),
        ("NO_PROXY", "127.0.0.1,localhost"),
        ("no_proxy", "127.0.0.1,localhost"),
    ] {
        env.push((key.into(), value.into()));
    }
    let endpoint = format!("http://127.0.0.1:{port}");
    for key in [
        "ANTHROPIC_BASE_URL",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
    ] {
        env.push((key.into(), endpoint.clone().into()));
    }
    env
}

fn sandbox_policy(root: &Path, port: u16) -> Result<String> {
    let root = root
        .to_str()
        .ok_or(Failure::Message("Sandbox paths must be UTF-8."))?;
    let quoted =
        serde_json::to_string(root).map_err(|_| Failure::Message("Cannot encode sandbox path."))?;
    Ok(format!(
        "(version 1)\n(allow default)\n(deny network*)\n\
        (allow network-outbound (remote ip \"localhost:{port}\"))\n\
        (deny file-write*)\n\
        (allow file-write* (subpath {quoted}) (literal \"/dev/null\"))\n"
    ))
}

fn command(binary: &Path, root: &Path, port: u16, confined: bool) -> Result<Command> {
    let mut command = if confined {
        let mut wrapped = Command::new("/usr/bin/sandbox-exec");
        wrapped
            .arg("-p")
            .arg(sandbox_policy(root, port)?)
            .arg(binary);
        wrapped
    } else {
        Command::new(binary)
    };
    command
        .env_clear()
        .envs(isolated_environment(root, port))
        .current_dir(root.join("cwd"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    Ok(command)
}

async fn version(
    binary: &Path,
    root: &Path,
    port: u16,
    confined: bool,
    limit: Duration,
) -> Result<String> {
    let mut cmd = command(binary, root, port, confined)?;
    let mut process = ChildGuard::spawn(cmd.arg("--version"))?;
    drop(process.child.stdin.take());
    let mut stdout = process
        .child
        .stdout
        .take()
        .ok_or(Failure::Message("Missing version output pipe."))?;
    let mut output = Vec::new();
    let outcome = tokio::time::timeout(limit.min(Duration::from_secs(10)), async {
        (&mut stdout).take(4097).read_to_end(&mut output).await?;
        if output.len() > 4096 { return Err(Failure::Message("Unexpected Claude version output.")); }
        if !process.child.wait().await?.success() {
            return Err(Failure::Message("Claude --version failed inside the probe environment; no unconfined retry was made."));
        }
        let text = std::str::from_utf8(&output).map_err(|_| Failure::Message("Unexpected Claude version output."))?.trim();
        // Do not forward arbitrary executable output, even from --version.
        let number = text.strip_suffix(" (Claude Code)").ok_or(Failure::Message("Unexpected Claude version format."))?;
        if number.split('.').count() != 3 || number.split('.').any(|p| p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit())) {
            return Err(Failure::Message("Unexpected Claude version format."));
        }
        Ok(text.to_owned())
    }).await;
    if process.child.id().is_some() {
        process.stop().await;
    }
    outcome.map_err(|_| Failure::Message("Claude version check timed out."))?
}

/// Capped line reader: read_line/read_until alone permit unbounded child output.
async fn read_line(reader: &mut (impl AsyncBufRead + Unpin), output: &mut Vec<u8>) -> Result<bool> {
    output.clear();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return Ok(!output.is_empty());
        }
        let count = available
            .iter()
            .position(|b| *b == b'\n')
            .map_or(available.len(), |i| i + 1);
        if output.len() + count > MAX_BODY {
            return Err(Failure::Message(
                "Claude output exceeded the per-line limit.",
            ));
        }
        let complete = available[count - 1] == b'\n';
        output.extend_from_slice(&available[..count]);
        reader.consume(count);
        if complete {
            return Ok(true);
        }
    }
}

async fn wait_result(reader: &mut (impl AsyncBufRead + Unpin)) -> Result<()> {
    let mut line = Zeroizing::new(Vec::new());
    while read_line(reader, &mut line).await? {
        let event: Value = serde_json::from_slice(&line)
            .map_err(|_| Failure::Message("Claude stdout was not stream-json."))?;
        if event["type"] == "result" {
            return if event["is_error"] == false && event["subtype"] == "success" {
                Ok(())
            } else {
                Err(Failure::Message(
                    "Claude returned an unsuccessful result; raw output is withheld.",
                ))
            };
        }
    }
    Err(Failure::Message(
        "Claude exited before a completed result; raw output is withheld.",
    ))
}

async fn conversation(
    process: &mut ChildGuard,
    root: &Path,
    evidence: &SharedEvidence,
    rotations: &mut Vec<Value>,
) -> Result<()> {
    let mut input = process
        .child
        .stdin
        .take()
        .ok_or(Failure::Message("Missing input pipe."))?;
    let output = process
        .child
        .stdout
        .take()
        .ok_or(Failure::Message("Missing output pipe."))?;
    let mut reader = BufReader::new(output);
    for (index, prompt) in PROMPTS.iter().enumerate() {
        if index > 0 {
            rotations.push(
                profile::rotate(
                    &root.join("profile"),
                    if index == 1 { TOKEN_B } else { TOKEN_A },
                )
                .await?,
            );
        }
        if process.child.try_wait()?.is_some() {
            return Err(Failure::Message("Claude exited between turns."));
        }
        let message = json!({"type": "user", "message": {"role": "user", "content": prompt}});
        let mut encoded = serde_json::to_vec(&message)
            .map_err(|_| Failure::Message("Cannot encode probe prompt."))?;
        encoded.push(b'\n');
        input.write_all(&encoded).await?;
        input.flush().await?;
        wait_result(&mut reader).await?;
        verify_turn(
            &*evidence
                .lock()
                .map_err(|_| Failure::Message("Probe evidence lock failed."))?,
            index + 1,
            if index == 1 { "B" } else { "A" },
        )?;
    }
    Ok(())
}

async fn run(explicit: Option<PathBuf>, limit: Duration, allow_unconfined: bool) -> Result<Value> {
    let started = Instant::now();
    if !cfg!(target_os = "macos") && !allow_unconfined {
        return Err(Failure::Message("OS confinement is implemented only on macOS; use --allow-unconfined explicitly on other Unix platforms."));
    }
    // These source-level CCR fallback paths ignore HOME. Check existence only.
    for path in [
        "/home/claude/.claude/remote/.oauth_token",
        "/home/claude/.claude/remote/.api_key",
    ] {
        if std::fs::symlink_metadata(path).is_ok() {
            return Err(Failure::Message(
                "Refusing a host with a fixed-path CCR credential file.",
            ));
        }
    }
    let binary = resolve_claude(explicit)?;
    let confined = cfg!(target_os = "macos") && !allow_unconfined;
    let temp = tempfile::Builder::new()
        .prefix("sessiometer-file-auth-")
        .tempdir()?;
    let root = std::fs::canonicalize(temp.path())?;
    file_policy::owner_only_dir(&root)?;
    for name in ["home", "tmp", "cwd", "config", "cache", "data"] {
        std::fs::create_dir(root.join(name))?;
        file_policy::owner_only_dir(&root.join(name))?;
    }
    let mut rotations = vec![profile::initialize(&root.join("profile"), TOKEN_A).await?];
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    let evidence = Arc::new(Mutex::new(Evidence::default()));
    let _server = AbortTask(tokio::spawn(serve(listener, Arc::clone(&evidence))));
    let version = version(
        &binary,
        &root,
        port,
        confined,
        limit.saturating_sub(started.elapsed()),
    )
    .await?;
    let mut cmd = command(&binary, &root, port, confined)?;
    cmd.args([
        "-p",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
        "--setting-sources",
        "",
        "--tools",
        "",
        "--strict-mcp-config",
        "--mcp-config",
        "{\"mcpServers\":{}}",
        "--no-session-persistence",
    ]);
    let mut process = ChildGuard::spawn(&mut cmd)?;
    let stderr = process
        .child
        .stderr
        .take()
        .ok_or(Failure::Message("Missing stderr pipe."))?;
    // Drain, but never persist or display diagnostics that might contain credentials.
    let _stderr = AbortTask(tokio::spawn(async move {
        let _ = tokio::io::copy(&mut BufReader::new(stderr), &mut tokio::io::sink()).await;
    }));
    let remaining = limit.saturating_sub(started.elapsed());
    let outcome = tokio::time::timeout(
        remaining,
        conversation(&mut process, &root, &evidence, &mut rotations),
    )
    .await
    .unwrap_or(Err(Failure::Message(
        "Probe timed out; its Claude process group was terminated.",
    )));
    process.stop().await;
    temp.close()?;
    let evidence = evidence
        .lock()
        .map_err(|_| Failure::Message("Probe evidence lock failed."))?;
    Ok(json!({
        "schema": 1, "passed": outcome.is_ok(), "proof": "local-mock-only",
        "real_authentication_tested": false, "claude_version": version,
        "claude_executable": binary, "claude_pid": process.pid,
        "os_confined": confined, "expected_sequence": ["A", "B", "A"],
        "evidence": &*evidence, "rotations": rotations,
        "duration_ms": started.elapsed().as_millis(),
        "error": outcome.err().map(|e| e.to_string()),
        "temporary_profile_removed": true,
    }))
}

struct Request {
    path: String,
    authorization: Zeroizing<String>,
    has_api_key: bool,
    body: Value,
}

async fn request(stream: &mut TcpStream) -> Result<Request> {
    let mut data = Zeroizing::new(Vec::new());
    let end = loop {
        if let Some(index) = data.windows(4).position(|s| s == b"\r\n\r\n") {
            break index + 4;
        }
        if data.len() > 65536 {
            return Err(Failure::Message("Mock request headers too large."));
        }
        let mut block = [0; 4096];
        let length = stream.read(&mut block).await?;
        if length == 0 {
            return Err(Failure::Message("Incomplete mock request."));
        }
        data.extend_from_slice(&block[..length]);
    };
    if end > 65536 {
        return Err(Failure::Message("Mock request headers too large."));
    }
    let header = std::str::from_utf8(&data[..end])
        .map_err(|_| Failure::Message("Invalid mock request headers."))?;
    let mut lines = header.split("\r\n");
    let mut first = lines.next().unwrap_or_default().split_whitespace();
    if first.next() != Some("POST") {
        return Err(Failure::Message(
            "Mock only accepts POST requests; it is not a forwarding proxy.",
        ));
    }
    let path = first
        .next()
        .unwrap_or_default()
        .split('?')
        .next()
        .unwrap_or_default()
        .to_owned();
    let mut content_length = None;
    let mut authorization = Zeroizing::new(String::new());
    let mut has_api_key = false;
    for line in lines.filter(|line| !line.is_empty()) {
        let (key, value) = line
            .split_once(':')
            .ok_or(Failure::Message("Invalid mock header."))?;
        let value = value.trim();
        if key.eq_ignore_ascii_case("content-length") {
            if content_length.is_some() {
                return Err(Failure::Message("Duplicate content length."));
            }
            content_length = Some(
                value
                    .parse::<usize>()
                    .map_err(|_| Failure::Message("Invalid content length."))?,
            );
        } else if key.eq_ignore_ascii_case("transfer-encoding") {
            return Err(Failure::Message(
                "Chunked requests are not supported by this small mock.",
            ));
        } else if key.eq_ignore_ascii_case("authorization") {
            if !authorization.is_empty() {
                return Err(Failure::Message("Duplicate authorization header."));
            }
            authorization.push_str(value);
        } else if key.eq_ignore_ascii_case("x-api-key") {
            has_api_key = true;
        }
    }
    let length = content_length.ok_or(Failure::Message("Missing content length."))?;
    if length > MAX_BODY {
        return Err(Failure::Message("Mock body too large."));
    }
    while data.len() < end + length {
        let mut block = [0; 8192];
        let size = (end + length - data.len()).min(block.len());
        let count = stream.read(&mut block[..size]).await?;
        if count == 0 {
            return Err(Failure::Message("Incomplete mock body."));
        }
        data.extend_from_slice(&block[..count]);
    }
    let body = serde_json::from_slice(&data[end..end + length])
        .map_err(|_| Failure::Message("Invalid mock JSON body."))?;
    Ok(Request {
        path,
        authorization,
        has_api_key,
        body,
    })
}

fn observe(request: &Request) -> Observation {
    let token = request
        .authorization
        .strip_prefix("Bearer ")
        .unwrap_or_default();
    let (selected, fingerprint) = match token {
        TOKEN_A if !request.has_api_key => ("A", Some(profile::fingerprint(TOKEN_A))),
        TOKEN_B if !request.has_api_key => ("B", Some(profile::fingerprint(TOKEN_B))),
        _ => ("unexpected", None),
    };
    let content = request.body["messages"]
        .as_array()
        .and_then(|messages| messages.iter().rev().find(|m| m["role"] == "user"))
        .map(|m| &m["content"]);
    let turn = PROMPTS
        .iter()
        .position(|prompt| {
            content.is_some_and(|c| {
                c.as_str() == Some(*prompt)
                    || c.as_array().is_some_and(|blocks| {
                        blocks
                            .iter()
                            .any(|b| b["type"] == "text" && b["text"].as_str() == Some(*prompt))
                    })
            })
        })
        .map(|index| index + 1);
    Observation {
        turn,
        selected,
        fingerprint,
    }
}

fn response(body: &Value) -> (String, String) {
    let message = json!({"id": "msg_file_auth_probe", "type": "message", "role": "assistant",
        "model": body["model"].as_str().unwrap_or("claude-sonnet-4-6"),
        "content": [{"type": "text", "text": "OK"}], "stop_reason": "end_turn", "stop_sequence": null,
        "usage": {"input_tokens": 10, "output_tokens": 1, "cache_creation_input_tokens": 0, "cache_read_input_tokens": 0}});
    if body["stream"] != true {
        return ("application/json".into(), message.to_string());
    }
    let mut start = message;
    start["content"] = json!([]);
    start["stop_reason"] = Value::Null;
    start["usage"]["output_tokens"] = json!(0);
    let events = [
        json!({"type": "message_start", "message": start}),
        json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "OK"}}),
        json!({"type": "content_block_stop", "index": 0}),
        json!({"type": "message_delta", "delta": {"stop_reason": "end_turn", "stop_sequence": null}, "usage": {"output_tokens": 1}}),
        json!({"type": "message_stop"}),
    ];
    let stream = events
        .iter()
        .map(|event| {
            format!(
                "event: {}\ndata: {event}\n\n",
                event["type"].as_str().unwrap_or_default()
            )
        })
        .collect::<String>();
    ("text/event-stream".into(), stream)
}

async fn reply(stream: &mut TcpStream, status: &str, mime: &str, body: &str) -> Result<()> {
    let header = format!("HTTP/1.1 {status}\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nConnection: close\r\nrequest-id: req_file_auth_probe\r\n\r\n", body.len());
    stream.write_all(header.as_bytes()).await?;
    stream.write_all(body.as_bytes()).await?;
    stream.shutdown().await?;
    Ok(())
}

async fn connection(mut stream: TcpStream, evidence: SharedEvidence) -> Result<()> {
    let request = request(&mut stream).await?;
    if request.path == "/v1/messages/count_tokens" {
        return reply(
            &mut stream,
            "200 OK",
            "application/json",
            "{\"input_tokens\":10}",
        )
        .await;
    }
    if request.path != "/v1/messages" {
        return reply(&mut stream, "404 Not Found", "application/json", "{}").await;
    }
    let observed = observe(&request);
    let valid = observed.selected != "unexpected";
    evidence
        .lock()
        .map_err(|_| Failure::Message("Probe evidence lock failed."))?
        .observations
        .push(observed);
    if !valid {
        return reply(&mut stream, "401 Unauthorized", "application/json",
            "{\"type\":\"error\",\"error\":{\"type\":\"authentication_error\",\"message\":\"Only dummy probe credentials are accepted\"}}").await;
    }
    // BOTH A and B are accepted at every phase: no 401 forces a cache refresh.
    let (mime, body) = response(&request.body);
    reply(&mut stream, "200 OK", &mime, &body).await
}

async fn serve(listener: TcpListener, evidence: SharedEvidence) {
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            incoming = listener.accept() => {
                let Ok((stream, _)) = incoming else { break; };
                let evidence = Arc::clone(&evidence);
                connections.spawn(async move {
                    let outcome = tokio::time::timeout(Duration::from_secs(5), connection(stream, Arc::clone(&evidence))).await;
                    if !matches!(outcome, Ok(Ok(()))) {
                        if let Ok(mut evidence) = evidence.lock() { evidence.protocol_errors += 1; }
                    }
                });
            }
            _ = connections.join_next(), if !connections.is_empty() => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(token: &str, turn: usize) -> Request {
        Request {
            path: "/v1/messages".into(),
            authorization: Zeroizing::new(format!("Bearer {token}")),
            has_api_key: false,
            body: json!({"messages": [{"role": "user", "content": [{"type": "text", "text": PROMPTS[turn - 1]}]}]}),
        }
    }

    #[test]
    fn rejects_stale_tokens_and_missing_requests_even_if_cli_claims_success() {
        let mut evidence = Evidence::default();
        assert!(verify_turn(&evidence, 1, "A").is_err());
        evidence.observations.push(observe(&message(TOKEN_A, 1)));
        verify_turn(&evidence, 1, "A").unwrap();
        evidence.observations.push(observe(&message(TOKEN_A, 2)));
        assert!(verify_turn(&evidence, 2, "B").is_err());
    }

    #[test]
    fn evidence_never_echoes_unexpected_credentials() {
        let observed = observe(&message("SECRET_CANARY", 1));
        assert_eq!(observed.selected, "unexpected");
        assert!(observed.fingerprint.is_none());
        assert!(!serde_json::to_string(&observed)
            .unwrap()
            .contains("SECRET_CANARY"));
        let mut dual = message(TOKEN_A, 1);
        dual.has_api_key = true;
        assert_eq!(observe(&dual).selected, "unexpected");
    }

    #[test]
    fn child_environment_contains_no_auth_overrides() {
        let env = isolated_environment(Path::new("/isolated"), 12345);
        for key in [
            "CLAUDE_CODE_OAUTH_TOKEN",
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_AUTH_TOKEN",
            "CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR",
            "CLAUDE_CODE_API_KEY_FILE_DESCRIPTOR",
            "CLAUDE_SECURESTORAGE_CONFIG_DIR",
            "ANTHROPIC_CUSTOM_HEADERS",
            "NODE_OPTIONS",
        ] {
            assert!(!env.iter().any(|(k, _)| k == key));
        }
        assert!(env.contains(&("HOME".into(), "/isolated/home".into())));
        assert!(env.contains(&("CLAUDE_CONFIG_DIR".into(), "/isolated/profile".into())));
    }

    #[tokio::test]
    async fn loopback_mock_accepts_a_b_a_without_auth_errors() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let evidence = Arc::new(Mutex::new(Evidence::default()));
        let _server = AbortTask(tokio::spawn(serve(listener, Arc::clone(&evidence))));
        for (index, token) in [TOKEN_A, TOKEN_B, TOKEN_A].into_iter().enumerate() {
            let mut client = TcpStream::connect(address).await.unwrap();
            let mut body = message(token, index + 1).body;
            body["stream"] = json!(true);
            let body = body.to_string();
            let http = format!("POST /v1/messages?beta=true HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {token}\r\nContent-Length: {}\r\n\r\n{body}", body.len());
            client.write_all(http.as_bytes()).await.unwrap();
            let mut response = String::new();
            tokio::time::timeout(Duration::from_secs(2), client.read_to_string(&mut response))
                .await
                .unwrap()
                .unwrap();
            assert!(response.starts_with("HTTP/1.1 200 OK"));
            assert!(response.contains("event: message_stop\n"));
            verify_turn(
                &evidence.lock().unwrap(),
                index + 1,
                if index == 1 { "B" } else { "A" },
            )
            .unwrap();
        }
    }

    #[tokio::test]
    async fn cleanup_reaps_the_child_and_removes_its_process_group() {
        let mut command = Command::new("sleep");
        command.arg("30");
        let mut child = ChildGuard::spawn(&mut command).unwrap();
        tokio::time::timeout(Duration::from_secs(2), child.stop())
            .await
            .unwrap();
        assert!(child.child.id().is_none());
        // SAFETY: signal 0 only checks the process group; it sends no signal.
        assert_eq!(unsafe { libc::kill(-(child.pid as i32), 0) }, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
    }

    #[tokio::test]
    async fn result_parser_requires_a_successful_complete_line() {
        assert!(wait_result(&mut BufReader::new(
            &b"{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false}\n"[..]
        ))
        .await
        .is_ok());
        assert!(wait_result(&mut BufReader::new(
            &b"{\"type\":\"result\",\"is_error\":true,\"result\":\"SECRET_CANARY\"}\n"[..]
        ))
        .await
        .unwrap_err()
        .to_string()
        .contains("withheld"));
        assert!(wait_result(&mut BufReader::new(&b""[..])).await.is_err());
        let large = vec![b'x'; MAX_BODY + 1];
        assert!(
            read_line(&mut BufReader::new(large.as_slice()), &mut Vec::new())
                .await
                .is_err()
        );
    }
}

/// Explicit opt-in: no provider access, no real credentials, never an unconfined fallback.
#[cfg(target_os = "macos")]
#[tokio::test]
#[ignore = "requires an installed Claude binary; dummy-only sandboxed local mock"]
async fn real_claude_observes_production_swaps_a_b_a() {
    let report = run(
        std::env::var_os("SESSIOMETER_TEST_CLAUDE_BIN").map(PathBuf::from),
        Duration::from_secs(60),
        false,
    )
    .await
    .unwrap();
    assert_eq!(report["passed"], true, "{report}");
    assert_eq!(report["os_confined"], true);
    eprintln!("{report}");
}
