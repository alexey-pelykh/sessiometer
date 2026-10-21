---
type: architecture-decision-record
number: 38
title: "The owner-only file policy is one abstraction, not forty `cfg` gates; on Windows it is an explicit protected DACL"
date: 2026-09-10
status: accepted
decision_makers: [Oleksii PELYKH (maintainer)]
---

# ADR-0038: The owner-only file policy is one abstraction — on Windows, an explicit protected DACL

## Status

**Accepted** — 2026-09-10. Recorded on **#974**, under umbrella **#970**, and it settles the
Windows half of **#28** *Cross-platform credential at-rest hygiene* — which stays open, because
#28 also carries the Linux posture and the `ReplaceFileW`-versus-`MoveFileEx` sub-question this
record deliberately does not close.

Unlike **ADR-0037**, which fixed an approach before any code existed, this record is written
**alongside the code it governs**: `src/file_policy.rs` and its call sites land on the same PR.

**It is a decision in force, not a measured one.** No CI job compiles this crate for Windows —
**#978** is the enforcing job, exactly as **#964** is on the Linux side — so the Windows arm is
reasoned from the documented `advapi32` contract and has never run. #974's AC3 asks for a test
that asserts the ACL is explicit rather than inherited; that test is committed and
`#[cfg(windows)]`, and its EXECUTION is deferred to #978.

## Context

### What the discipline was, and what it cost

`src/paths.rs` has documented one rule since the first commit: directories `0700`, files `0600`,
and every directory we create is checked to be owned by the current user before use. The rule was
expressed as a **raw mode integer at roughly forty call sites** — `Permissions::from_mode`,
`OpenOptionsExt::mode`, `Permissions::mode`, `Metadata::uid` — spread across `src/paths.rs`,
`src/usage_store.rs`, `src/swap.rs`, `src/service.rs`, `src/observability.rs`, `src/canary.rs`,
`src/use_account.rs`, `src/claude_state.rs`, `src/config/render.rs`, `src/config/write_lock.rs`,
`src/roster_backup.rs`, `src/control_transport.rs` and `src/daemon/commands.rs`.

Every one of those symbols lives on `std::os::unix`. Measured at `49561e8` with `cargo check
--target x86_64-pc-windows-msvc --all-targets`, the crate had **156** error lines on that target,
of which **47** were this cluster — the largest single group in the Windows build tier.

### What Windows actually gives us — this revises the project record

The earlier framing was that Windows "needs an explicit DACL to reach a `0600` equivalent",
which implies the files are wide open there. **Measurement on a live host (recon #40) says
otherwise.** A Claude Code credential file under the user profile carries exactly:

```
Owner: <host>\<user>
NT AUTHORITY\SYSTEM     : FullControl (Allow, inherited=True)
BUILTIN\Administrators  : FullControl (Allow, inherited=True)
<host>\<user>           : FullControl (Allow, inherited=True)
```

**No `Users`, no `Everyone`, no `Authenticated Users`.** SYSTEM and Administrators are the
unavoidable Windows floor — an administrator reads anything through ownership or
`SeBackupPrivilege` — so naming them is not additional exposure. **In practice that already is
the `0600` equivalent**, and any framing of this work as "closing a hole" would be false.

The real finding is narrower and better specified: **every one of those ACEs is INHERITED, so
the file carries no DACL of its own.** It is protected only by whatever the user-profile
directory grants, and follows that directory silently if it ever changes. Fine today; not a
guarantee. That, and not "Windows is wide open", is what this record addresses.

## Decision

### 1. One abstraction, not forty `cfg` gates

`src/file_policy.rs` states the INTENT — *this belongs to its owner and to nobody else* — once,
and holds the per-target mechanism behind it. Its surface is small and named after what callers
mean rather than after what either platform does: `open_owner_only`, `owner_only_file`,
`owner_only_dir`, `copy_policy`, `owner_is_current_user`, `owner_is_current_user_nofollow`,
`owner_only_deviation`.

The alternative #974 offers is gating each site with `#[cfg(unix)]` / `#[cfg(windows)]`. It is
rejected: it makes a security property into forty independent chances to get one wrong, and it
would make #28's "write an explicit DACL" a forty-site sweep rather than one function.

### 2. On Windows the mechanism is an explicit, PROTECTED DACL

Written with `SetNamedSecurityInfoW(SE_FILE_OBJECT, DACL_SECURITY_INFORMATION |
PROTECTED_DACL_SECURITY_INFORMATION)`, from the SDDL:

```
files:       D:P(A;;FA;;;SY)(A;;FA;;;BA)(A;;FA;;;<sid>)
directories: D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FA;;;<sid>)
```

`P` is the whole item: without it these ACEs are a **floor the parent can widen**, which is the
inherited-only state the recon found. `FA` is `FILE_ALL_ACCESS`, the file object's own mask,
rather than the `GA` ADR-0037 § Decision 2 writes for a pipe — a generic right is mapped per
object type, and naming the specific mask means the DACL reads back as it was written. `SY` and
`BA` are the measured floor above. `<sid>` is **this process's own token user**, read through
`crate::control_transport::our_user_sid` so that the identity a file is granted to and the
identity the control channel authenticates cannot drift apart; it is never a literal and never
configurable.

A directory's ACEs are marked inheritable (`OICI`) so anything created inside starts owner-only.
That is defence in depth, not the guarantee: every file this crate creates in such a directory
is given its **own** explicit DACL, which replaces the inherited ACEs outright.

### 3. Three per-target differences are recorded, not hidden

- **Creation is atomic on Unix and is not on Windows.** `OpenOptionsExt::mode` hands the mode to
  `open(2)`. Windows has no equivalent hook on `std::fs::OpenOptions`:
  `std::os::windows::fs::OpenOptionsExt` exposes `access_mode`, `share_mode`, `custom_flags`,
  `attributes` and `security_qos_flags`, and no `security_attributes`, so a `SECURITY_ATTRIBUTES`
  cannot reach `CreateFileW` through it. `open_owner_only` therefore creates and then tightens,
  and what covers the window **for the callers that stage inside this crate's own tree** is the
  PARENT: such a file is created inside a directory this same module has already made explicitly
  owner-only. Two callers stage elsewhere and are outside that argument —
  `paths::write_private_file` puts `<path>.tmp` in `path`'s own directory, which
  `cli::write_export` takes from the operator, and `paths::write_preserving_mode` puts
  `~/.claude.json.tmp` in the profile root. **#1528** carries that work.
- **Unix applies a creation mode only when it creates; Windows applies the DACL either way.**
  There is no "only if you created it" on that path, so an existing file is tightened too. The
  divergence narrows access rather than widening it.
- **A filesystem that cannot hold the policy degrades on Unix and fails CLOSED on Windows.**
  `set_permissions` against a mount that ignores mode bits succeeds and the file lands wider, so
  only `roster_backup`'s read-back refuses. `SetNamedSecurityInfoW` against FAT/exFAT returns an
  error instead, so `owner_only_file` fails and every private write under such a directory fails
  with it. Fail-closed is the right direction for a security policy; it is recorded because it is
  a different operator experience, not because it is wrong.

### 4. `0644` is not this policy

`src/service.rs`'s LaunchAgent plist is deliberately world-readable. It stays inline under its
own `#[cfg(unix)]` rather than growing a second policy in `file_policy`: launchd is a macOS
concept with no Windows analogue, so a Windows arm would be a no-op wearing a policy's name.

### 5. No dependency is added

`windows-sys` has been a target-gated dependency of this crate since **#1513**, and every call
this needs is in features already enabled. Widening a feature list resolves no new package and
leaves `Cargo.lock` byte-identical, so `deny`'s advisory, source and licence gates see the graph
they saw before. #974's "do not add a dependency to get this" holds with nothing added.

## Alternatives considered

- **Gate each of the ~40 sites.** #974 offers it and records that it makes the security property
  forty independent chances to get one wrong. Rejected — see § Decision 1.
- **Write an UNPROTECTED explicit DACL.** Cheaper by one flag, and it would satisfy a naive
  "the owner can read it, and only the owner" check. It also leaves the profile directory able
  to widen the file at any time, which is exactly the fragility the recon found. Rejected; #974
  AC3 exists to make the distinction testable.
- **Omit SYSTEM and Administrators, granting the owner alone.** Strictly narrower on paper, and
  worthless in fact: an administrator reads anything through ownership or `SeBackupPrivilege`.
  It would break backup and endpoint tooling for a guarantee Windows does not offer. Rejected —
  and #974 AC2 names the floor as admissible for this reason.
- **Synthesize a Unix mode from the DACL** so that existing `mode() & 0o777` comparisons keep
  working unchanged. Rejected: it is a fiction that reads as fact at every future call site, and
  the questions this crate actually asks ("is it ours?", "is it still owner-only?") are answered
  directly on both targets by `owner_is_current_user` and `owner_only_deviation`.
- **Add a Win32 wrapper crate** to avoid hand-rolled `unsafe`. Rejected by the crate's
  minimal-dependency line, which #974 § Boundaries restates, and by ADR-0004's precedent for
  incidental FFI kept raw.

## Consequences

### Positive

- The `mode` / `from_mode` / `uid` cluster is at **zero** on the Windows target: 156 error lines
  at `49561e8` down to 89, with the remainder belonging to other tier items (`flock`, `termios`,
  `UnixListener`, `OsStrExt`, `localtime_r`).
- #28's Windows work item — "write our stashes with an explicit DACL rather than relying on
  inheritance" — is delivered as a property of the writer, so no future call site can opt out of
  it by spelling a mode.
- The DACL's SHAPE is asserted by ordinary unit tests on macOS and Linux, because the SDDL
  builder and the read-back predicate are target-neutral text. Only the syscalls are gated.
- Unix behaviour is the same modes at the same moments, and the existing Unix permission tests
  assert it with their bodies untouched. One mechanism did change beneath that: in
  `write_preserving_mode` the source's policy was applied with `file.set_permissions` — `fchmod`
  on the held fd — and `copy_policy(path, &tmp)` applies it BY PATH. The resulting mode and the
  moment are identical and the writer is same-user throughout, so no privilege boundary moves;
  it is recorded because "byte for byte" would otherwise cover a fd-exact operation becoming a
  path-resolved one.

### Negative / trade-offs

- **Nothing here has run.** Every Windows claim is a type-checked hypothesis until #978. The
  committed `#[cfg(windows)]` tests are what will grade it.
- **A create-then-tighten window exists on Windows** and does not on Unix (§ Decision 3). For the
  callers that stage inside this crate's own tree it is covered by the parent directory's own
  explicit DACL — a layered argument rather than an atomic one — and for `write_export` and
  `write_preserving_mode` it is not covered at all, since neither stages in a directory this
  module made. Closing it needs a `SECURITY_ATTRIBUTES` on `CreateFileW`, which **std** does not
  expose — but this crate is already past std on that API: `src/control_transport.rs` builds one
  from an SDDL string for `CreateNamedPipeW`, so the remaining work is a direct `CreateFileW` plus
  `File::from_raw_handle`, on ADR-0004's incidental-FFI precedent. **#1528** carries it, and the
  sentence is worded this way because a future reader would otherwise take "std does not expose
  it" as the reason not to try.
- **`owner_is_current_user_nofollow` is `lstat`-exact on Unix and is not on Windows.**
  `GetNamedSecurityInfoW` resolves reparse points and there is no named no-follow form; the
  handle-based one would need `CreateFileW(FILE_FLAG_OPEN_REPARSE_POINT)`, which is not ported.
  Its one caller refuses a symlink at the same path immediately before asking, so the difference
  is reachable only through a race.
- **#28's `ReplaceFileW` sub-question stays open, and is now load-bearing.** `MoveFileEx` — what
  `std::fs::rename` uses — carries the SOURCE's security descriptor, so the atomic writers stage
  a file with the policy they want and the rename delivers it. `write_preserving_mode` is the
  case that needs the opposite, and `copy_policy` handles it by copying the destination's DACL
  and its protection flag onto the staging file before the rename. The recon's own
  ACL-preservation probe was **degenerate** — both scratch files inherited the same ACL — so
  that behaviour is reasoned, not measured, and #28 records it as needing a deliberately
  divergent source ACL to test properly.
- **Two ten-line SID-string shims are duplicated** between `file_policy` and
  `control_transport`. Hoisting them would put a generic Win32 helper inside a transport, or
  invent a third module to hold two functions; the duplication is the cheaper of the three.
- Test-only permission manipulation — the `0500` directory freezes in the canary and drift
  fixtures — is `cfg`-gated rather than ported, per #974 § Boundaries. Those fixtures assert a
  POSIX DAC behaviour with no Windows analogue: a read-only directory there does not stop a file
  being created inside it. The Windows equivalents of those scenarios are unwritten; **#1529**
  owns writing them, or recording per fixture why no equivalent exists. #978 is scoped to RUNNING
  the tests that are already committed and does not cover these.
