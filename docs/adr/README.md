# Architecture Decision Records

This directory holds **Architecture Decision Records (ADRs)** — short,
immutable, point-in-time notes that capture a load-bearing technical decision
together with the context that made it, the alternatives weighed, and the
consequences accepted.

An ADR exists so a contributor does not have to reverse-engineer *why* the code
is the way it is. Rationale that today lives only in module doc-comments and
issue threads is consolidated here in a stable, discoverable place.

## Index

| ADR | Title | Status |
|-----|-------|--------|
| [0001](0001-current-thread-tokio-runtime.md) | Single-threaded (`current_thread`) Tokio runtime | Accepted |
| [0002](0002-keychain-via-security-cli-zero-ffi.md) | Keychain access via `/usr/bin/security` CLI (zero FFI) | Accepted |
| [0003](0003-no-torn-swap-invariant.md) | No-torn-swap invariant | Accepted |
| [0004](0004-incidental-libc-ffi-kept-raw.md) | Incidental `libc` FFI kept raw (no wrapper crate) | Accepted |
| [0005](0005-config-parsed-by-crate-emitted-by-hand.md) | Config parsed with the `toml` crate, emitted by hand | Accepted |
| [0006](0006-migration-schema-evolution-policy.md) | Migration-artifact schema-evolution policy; v1 format frozen | Accepted |
| [0007](0007-decided-against-credential-recovery-options.md) | Decided-against credential-recovery options for dead accounts | Accepted |
| [0008](0008-login-decouples-unquarantine-from-activation.md) | `login` decouples un-quarantine from activation | Accepted |
| [0009](0009-rate-limit-backoff-per-account.md) | Rate-limit back-off is scoped per-account, not endpoint-global | Accepted |
| [0010](0010-macos-app-repo-topology.md) | macOS app repo topology — monorepo, first-party daemon, Rust crate at root | Accepted |
| [0011](0011-menubar-transport-raw-posix-af-unix.md) | menubar↔daemon transport — raw POSIX AF_UNIX from Swift (not Network.framework) | Accepted |
| [0012](0012-active-reobservation-via-schedule-interleave.md) | Active-account re-observation via schedule interleave, not a lower `poll_secs` | Accepted |
| [0013](0013-session-floor-default-on-reserve-emergency-exempt.md) | `session_floor` (renamed `target_max_usage` #415, then `target_max_session_usage` #443) is a default-on swap-target reserve, exempt on the emergency path | Accepted |
| [0014](0014-refresh-error-backoff-is-tick-owned.md) | Refresh error back-off is tick-owned, not on `AccountHealth` | Accepted |
| [0015](0015-reactive-refresh-unconditional-proactive-gated.md) | Reactive on-401 refresh is unconditional; `[refresh].enabled` gates only proactive maintenance | Accepted |
| [0016](0016-dead-active-no-target-surfaced-not-relaxed.md) | A dead active with no viable target is a surfaced capacity signal, not a swap-eligibility bug | Accepted |
| [0017](0017-bounded-blindness-preemptive-swap-not-header-observation.md) | Bounded-blindness preemptive swap-away, not header-based active-observation | Accepted |
| [0018](0018-shared-credential-scrub-multi-writer-lockout.md) | Shared-credential scrub on the first `invalid_grant`: the multi-writer "Not logged in" lockout | Accepted |
| [0019](0019-slow-poll-out-of-rotation-peers.md) | Out-of-rotation (exhausted) peers are slow-polled on a widened, reset-aware cadence | Accepted |
| [0020](0020-stats-framing-guard-permits-neutral-runway.md) | The `stats` framing guard permits a neutral runway, bans the acquisitive call | Accepted |
| [0021](0021-homebrew-tap-topology.md) | Homebrew tap topology — an org-owned distribution repo, not a second product repo | Accepted |
| [0022](0022-session-trigger-one-predicate-two-estimators.md) | `session_trigger` is one predicate on two estimators of the same quantity, not two knobs | Superseded by [0023](0023-session-trigger-ceiling-semantics.md) |
| [0023](0023-session-trigger-ceiling-semantics.md) | `session_trigger` is a settled ceiling both swap arms derive their fire point backward from | Accepted |

## Conventions

- **Filename**: `NNNN-kebab-case-title.md`, `NNNN` zero-padded and sequential;
  numbers are never reused, even after supersession.
- **Sections**: every ADR carries **Context**, **Decision**, **Alternatives
  considered**, and **Consequences** (positive and negative/trade-offs).
- **Status vocabulary**: `Accepted` (in force) · `Superseded` (replaced by a
  later ADR — link both ways) · `Deprecated` (no longer applies, not replaced).
- **Immutability**: ADRs are historical artifacts. Do not rewrite an accepted
  ADR to match newer thinking — write a new ADR that supersedes it, and mark the
  old one `Superseded by ADR-NNNN`.
- **Provenance**: cite the code (`file` / symbol) and the originating issue
  numbers, matching this repo's house style.
