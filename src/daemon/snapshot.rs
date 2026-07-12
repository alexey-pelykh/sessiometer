// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! Status-snapshot assembly: the non-secret `status` snapshot + wire types and the pure
//! projections that build them (issue #203, the #195 per-concern decomposition).
//!
//! [`StatusSnapshot`] is the daemon's per-cycle reading set; [`status_response`] projects it
//! into the [`StatusResponse`] wire reply — handles + percentages + the `next_swap` candidate,
//! never a token or email (the #15 discipline). [`credential_health`] is the pure 5-state
//! rollup the display snapshot and the transition-event diff share. Each item is re-exported
//! under `crate::daemon::*`, so relocating them is source-compatible for every existing consumer
//! (cli / poke / use_account) and for the in-module test suite (`mod tests`' `use super::*`).

use serde::{Deserialize, Serialize};

use super::*;

/// The latest per-account reading the daemon exposes — over the control socket
/// and in the event log. Non-secret by construction: a handle (label), the active
/// flag, and percentages — never a token or email (issue #15).
#[derive(Debug, Clone, Default)]
pub(crate) struct StatusSnapshot {
    pub(crate) accounts: Vec<AccountReading>,
    /// The next swap candidate as of this cycle (issue #88): who [`pick_target`]
    /// would rotate the active session to, or why there is no candidate. Computed
    /// daemon-side ([`Daemon::next_swap`]); [`status_response`] copies it straight
    /// onto the wire. `None` only when there is no active anchor to swap from.
    pub(crate) next_swap: Option<NextSwap>,
    /// Whether the periodic isolated-refresh tick is enabled in config (`[refresh].enabled`,
    /// issue #105) — copied from [`Daemon::refresh_enabled`] at build. Carried to the wire so
    /// the thin `status` client can surface the issue-#138 advisory (with the tick OFF,
    /// non-active accounts get no maintenance). `false` by `Default` (an all-defaults snapshot
    /// reads as tick-off), matching the opt-in default.
    pub(crate) refresh_enabled: bool,
    /// Wall-clock epoch SECONDS at which the daemon assembled this snapshot (issue #164) — the
    /// freshness stamp the frozen wire contract carries so a read-only client (e.g. a menubar
    /// app) can tell a LIVE snapshot from a STALE one: a healthy daemon advances it every cycle,
    /// a wedged or dead one stops, so a client compares it against its own clock and greys out
    /// once the gap grows. Stamped in [`Daemon::snapshot`] from the same `now_secs` the #119
    /// health rollup reads, so ONE wall-clock read backs the whole cycle. Epoch seconds — the
    /// unit the rest of the wire already speaks (`access_expires_at`, `session_resets_at`).
    /// `0` by `Default` (an all-defaults snapshot has no generation instant).
    pub(crate) generated_at: i64,
    /// The daemon-level SYSTEMIC refresh-health indicator (issue #378): `Some(n)` while the
    /// refresh MECHANISM is down — `n` consecutive sweeps failed with `outcome=error` across every
    /// eligible account, past the configured threshold — else `None`. Copied from
    /// [`SystemicRefreshHealth::status`](crate::systemic_refresh::SystemicRefreshHealth::status) at
    /// build. Distinct from the per-account [`AccountReading::health`] `at_risk` rollup: it
    /// reflects the whole mechanism, visible without waiting for an account to die. `None` by
    /// `Default` (an all-defaults snapshot reads as healthy). A COUNT only — never a token,
    /// path, or email (the #15 discipline).
    pub(crate) systemic_refresh: Option<u32>,
    /// The daemon-level CANONICAL-SCRUB rollup (issue #516): `Some(Recovering | Exhausted)` while the
    /// shared canonical item is scrubbed, else `None` when healthy. Computed in [`Daemon::snapshot`]
    /// from the edge-latched scrub signals (`signaled_canonical_scrubbed` / `signaled_scrub_adopt_exhausted`);
    /// [`status_response`] copies it straight onto the wire. `None` by `Default` (an all-defaults
    /// snapshot reads as healthy). A STATE discriminant only — never a token or email (the #15 discipline).
    pub(crate) canonical_scrub: Option<CanonicalScrub>,
    /// The daemon-level KEYCHAIN-LOCKED rollup (issue #498): `true` while the macOS login keychain is
    /// LOCKED, so the daemon cannot READ the shared credential item at ALL (access denied) — distinct
    /// from `canonical_scrub`, where the item IS readable but its token was scrubbed/emptied (#469/#463).
    /// Computed in [`Daemon::snapshot`] from the edge-latched `signaled_keychain_locked` signal;
    /// [`status_response`] copies it straight onto the wire. `false` by `Default` (an all-defaults
    /// snapshot reads as unlocked/healthy). A bare BINARY state discriminant — never a token or email
    /// (the #15 discipline). The remedy the operator sees (unlock the keychain) is the surfacing
    /// consumer's concern (the menubar #498 card), NOT this wire increment's.
    pub(crate) keychain_locked: bool,
}

/// The non-secret refresh-health inputs `status` surfaces in `--json` (issue #119): the
/// daemon's reduced projection of the refresh observations its per-account health state
/// carries — whether the last refresh kept the credential alive, whether CC rotated the
/// refresh-token VALUE, and the consecutive-failure streak. `None` (the whole struct) until
/// the refresh engine has observed the account at least once (e.g. the `[refresh]` feature
/// is off, or the account has not yet been swept). Every field is a boolean / count — never
/// a token or expiry (the #15 discipline). Derives `Deserialize` so the `status` client can
/// read it back; `#[serde(default)]` on the carrying field handles a pre-#119 daemon.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RefreshHealth {
    /// Whether the LAST observed refresh kept the credential ALIVE (`refreshed` /
    /// `no_change`), as opposed to a `dead` (refresh token cleared) or `error` (cycle
    /// failed) outcome.
    pub(crate) last_ok: bool,
    /// Whether CC ROTATED the refresh-token value on the last refresh (the AC-3 durability
    /// signal) — the boolean only, never either token value. Named `rotated` (not
    /// `token_rotated`) so the `--json` field carries no `token` substring that a coarse
    /// #15 leak-proxy (`!contains("token")`) could false-positive on.
    pub(crate) rotated: bool,
    /// Consecutive refresh FAILURES (`dead` / `error` outcomes), reset to 0 by the next
    /// alive refresh — the rollup's at-risk input.
    pub(crate) consecutive_failures: u32,
}

/// The active account's BOUNDED-BLINDNESS state (issue #479, umbrella #363 Path B) — present only
/// when the active account has gone blind (its `/oauth/usage` poll is failing / backing off, so its
/// live reading is cleared) AND the daemon still holds a retained pre-blind anchor (`last_good`,
/// #450). Surfaced so `status` renders a SEMANTIC line — blind duration, last-known session %, and
/// whether ADR-0017 auto-protection is OK or DEGRADED — instead of the content-free `n/a … 🟡` a
/// bare failed-poll row shows. The surface only REFLECTS this daemon-pushed state; it never
/// self-polls or self-swaps (the #169 UI-never-acts invariant). Non-secret — a duration and two
/// small numbers, never a token or email (issue #15).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct BlindActive {
    /// Seconds the active account has been blind — `blind_elapsed`, measured from the retained
    /// pre-blind anchor's observation instant (`last_good.at`, #450) to snapshot assembly on the
    /// daemon's MONOTONIC clock (the SAME clock #452's gate and the swap cooldown use). A DURATION
    /// (not an absolute instant, which an [`std::time::Instant`] cannot cross the socket as), so the
    /// client renders it verbatim against nothing.
    pub(crate) blind_secs: u64,
    /// The retained pre-blind SESSION-window usage percent (`0..=100`) the anchor holds
    /// (`last_good.session`, #450) — the last-known reading before the account went blind. This is
    /// why the row stops reporting "no data": the daemon DID retain a reading.
    pub(crate) last_known_session_pct: u8,
    /// Whether ADR-0017 preemptive auto-protection is DEGRADED — the gate is armed but acting on a
    /// STALE anchor: `blind_secs > BLIND_GATE_SECS` AND the anchor sat at/over `BLIND_GATE_RISK_BAND`
    /// (the gate's first two ADR-0017 conditions, mirroring [`Daemon::note_blind_gate_eligibility`]
    /// exactly). `false` = OK: the account is blind, but not yet past the gate threshold, or the
    /// anchor sat below the risk band — auto-protection is nominally intact.
    pub(crate) auto_protection_degraded: bool,
}

/// The daemon-level CANONICAL-SCRUB rollup (issue #516, umbrella #463) — present only while the
/// shared `Claude Code-credentials` canonical item is SCRUBBED (its refresh token cleared): the
/// fleet-wide lockout NO per-account `credential_dead` fires for (the shared item is emptied while
/// account rows can still read perfectly healthy). Surfaced so `status` + the menubar (issue #469)
/// can render the scrubbed / un-recoverable state that no per-account `auth` rollup, and no #479
/// `blind_active`, reflects — a signal only the DAEMON holds (it lives in the durable event log,
/// #464/#467, never on the frozen wire until this field). Distinct from the ADR-0016
/// `ActiveDeadNoTarget` case (which IS wire-derivable from `next_swap` + a dead active row).
///
/// Internally tagged on `state` (mirroring [`NextSwap`]), so a future per-variant field — e.g. the
/// roster handle the `canonical_scrubbed` / `canonical_recovery_exhausted` events already carry — is
/// an ADDITIVE change rather than a breaking `string → object` reshape. A fleet-wide STATE
/// discriminant only: never per-account, never a token or email (issue #15).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "state")]
pub(crate) enum CanonicalScrub {
    /// The canonical is scrubbed, but the daemon's autonomous adopt-recovery is still in progress
    /// (issue #467): a viable spare's known-live token may yet be adopted into the emptied canonical,
    /// healing the fleet with no operator action. The lower-severity, self-may-heal state.
    Recovering,
    /// The canonical is scrubbed AND recovery is EXHAUSTED (issue #467): the bounded adopt churn hit
    /// its cap (or no viable adopt target exists), so the daemon has BACKED OFF and the canonical
    /// stays empty until a `claude /login` re-authenticates it. The residual UN-RECOVERABLE state
    /// #469 renders with that remedy. Ranks above [`Self::Recovering`] (most-severe wins).
    Exhausted,
}

/// One account's latest reading.
#[derive(Debug, Clone, Default)]
pub(crate) struct AccountReading {
    pub(crate) label: String,
    pub(crate) active: bool,
    /// Whether the account is in the rotation (issue #36) — surfaced so `status`
    /// can mark a parked account. A disabled account is shown but never swapped to.
    pub(crate) enabled: bool,
    /// Whether the account is QUARANTINED — its stored ACCESS token was rejected (a #42
    /// 401-streak), so it is out of rotation. NON-TERMINAL (issue #427): the remedy is a
    /// refresh (`poke`), not necessarily a re-login. Non-secret (a plain flag on the handle).
    pub(crate) quarantined: bool,
    /// Whether a quarantined account is mid-RECOVERY — its credential is currently
    /// answering again (`quarantined && recovery_successes > 0`), climbing toward the
    /// un-quarantine threshold on the spontaneous-revival path (issue #109). A refinement
    /// of `quarantined` (always implies it), surfaced so `status` can render `recovering`
    /// instead of the alarming `needs re-login` for a healing account. Derived from the
    /// health counter (where it lives); non-secret — a plain flag, no raw count exposed.
    pub(crate) recovering: bool,
    /// Whether the account's WEEKLY window is EXHAUSTED — `weekly >= weekly_trigger`
    /// (the base, un-jittered threshold; issue #11/#37), the daemon's own viability
    /// verdict. When true the account is blocked until its weekly reset, so `status`
    /// keys its "resets in" off the weekly reset rather than the sooner session
    /// reset (issue #72). Precomputed here (where the threshold lives) so the wire
    /// projection stays threshold-free; `false` when the last poll failed.
    pub(crate) weekly_exhausted: bool,
    pub(crate) usage: Option<Usage>,
    /// The stored access-token `expiresAt` as epoch SECONDS (issue #119), or `None` until
    /// the refresh engine has observed this account's stash. An absolute instant (not a
    /// relative duration, like `session_resets_at`) carried RAW on the wire, from which a
    /// consumer (`--json` | `jq`) can derive an "expires in" against its own clock; the lean
    /// text view projects only the rollup glyph, not a clock cell. Non-secret — a timestamp.
    pub(crate) access_expires_at: Option<i64>,
    /// The non-secret refresh-health inputs (issue #119), or `None` until a refresh has been
    /// observed. The rollup's at-risk / dead inputs plus the `--json` durability signal.
    pub(crate) refresh_health: Option<RefreshHealth>,
    /// The daemon-computed 5-state credential-health rollup (issue #119) — the verdict the
    /// thin `status` client projects to a glyph. Computed in [`Daemon::snapshot`] from this
    /// account's health state and the wall clock.
    pub(crate) health: CredentialHealth,
    /// The active account's bounded-blindness projection (issue #479), or `None` when this is not
    /// the active account, or the active account is not blind, or there is no retained anchor.
    /// Computed in [`Daemon::snapshot`] from the retained `last_good` anchor (#450) and the ADR-0017
    /// gate thresholds; copied straight to the wire ([`status_response`]).
    pub(crate) blind_active: Option<BlindActive>,
}

/// The status-snapshot wire contract's version (issue #164): a `major.minor` the daemon stamps
/// on every reply so an independently-released read-only client (a menubar app) can bind to it
/// safely. Semver-for-a-wire-struct: a MAJOR bump is a BREAKING change (a field removed /
/// renamed / re-typed / re-meant) an older client MUST refuse to render rather than mis-read; a
/// MINOR bump is ADDITIVE (a new optional field) an older client tolerates by ignoring what it
/// does not know. Non-secret — two integers.
///
/// Derives `Default` (`{0, 0}`) so a `#[serde(default)]` decode of a PRE-#164 daemon's reply
/// (which omits the field) yields major `0` — an "unknown, pre-freeze" version the client treats
/// as a mismatched major and DEGRADES on, rather than assuming compatibility (fail-safe).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub(crate) struct SchemaVersion {
    pub(crate) major: u32,
    pub(crate) minor: u32,
}

/// The status-snapshot contract version THIS build speaks (as the daemon) and understands (as
/// the reference `status` client) — issue #164. `1.0` is the FIRST frozen contract: the 0.1.0
/// status snapshot settled by #137–#143. Bump MAJOR on any breaking field change, MINOR on an
/// additive one (see [`SchemaVersion`]). `1.1` ADDED the daemon-level
/// [`StatusResponse::systemic_refresh_failure`] indicator (issue #378) — an optional field an
/// older client tolerates by ignoring. `1.2` ADDED the [`NextSwap::Target`] `reason`
/// ([`NextSwapReason`], issue #393) — the daemon's own selection rationale, likewise optional and
/// tolerated-by-ignoring. `1.3` ADDED the [`NextSwap::NoViableTarget`] `cause` + `resets_at`
/// fleet-capacity relief hint ([`NoTargetCause`], issue #405) — two more optional
/// tolerated-by-ignoring fields on a variant that was previously payload-free. `1.4` ADDED the
/// per-account [`AccountStatusLine::blind_active`] bounded-blindness projection ([`BlindActive`],
/// issue #479) — an optional field an older client tolerates by ignoring, and (via
/// `skip_serializing_if`) omitted entirely except on a blind active account, so a non-blind frame's
/// per-line bytes are unchanged. `1.5` ADDED the daemon-level
/// [`StatusResponse::canonical_scrub`] canonical-scrub rollup ([`CanonicalScrub`], issue #516) — a
/// fleet-wide scrubbed / recovery-exhausted signal, likewise optional and (via `skip_serializing_if`)
/// omitted entirely when healthy, so a non-scrub frame's bytes are unchanged. Like
/// `systemic_refresh_failure` it is daemon-level, but it takes `blind_active`'s `skip_serializing_if`
/// omit-when-healthy pattern rather than `systemic_refresh_failure`'s always-emitted `null`. `1.6`
/// ADDED the daemon-level [`StatusResponse::keychain_locked`] flag ([issue #498]) — a fleet-wide
/// "the login keychain is LOCKED so the shared credential is unreadable" signal, a bare `bool`
/// (via `skip_serializing_if`) omitted entirely when unlocked, so a non-locked frame's bytes are
/// unchanged. The daemon-level sibling of `canonical_scrub`, but for an UNREADABLE item rather than
/// a readable-but-scrubbed one; the wire prerequisite for the menubar #498 surface.
pub(crate) const STATUS_SCHEMA_VERSION: SchemaVersion = SchemaVersion { major: 1, minor: 6 };

/// The control socket's `status` reply PAYLOAD — handles + percentages + the forward-looking
/// `next_swap` candidate, and nothing else (issue #15: never a token or email).
/// Derives both `Serialize` (the daemon writes it) and `Deserialize` (the `status`
/// client reads it). This is the payload the frozen wire envelope ([`VersionedStatus`], issue
/// #164) carries; the durable, timestamped swap HISTORY remains the event-log view (#9), not
/// `status`.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct StatusResponse {
    pub(crate) accounts: Vec<AccountStatusLine>,
    /// The next swap candidate (issue #88), or `null` when there is no active anchor
    /// to swap from. `#[serde(default)]` per the added-field convention (cf.
    /// `session_resets_at`): a pre-#88 daemon that omits the field decodes to `None`.
    #[serde(default)]
    pub(crate) next_swap: Option<NextSwap>,
    /// Whether the daemon's periodic isolated-refresh tick is enabled (`[refresh].enabled`,
    /// issue #105). `Some(false)` is the ONLY value that arms the issue-#138 discoverability
    /// advisory (paired with ≥1 unhealthy/unverified non-active account); `Some(true)`
    /// suppresses it. `Option` + `#[serde(default)]` per the added-field convention (cf.
    /// `auth`): a pre-#138 daemon that omits the field decodes to `None`, which the client
    /// treats as "unknown → suppress" rather than mis-firing a stale advisory against an old
    /// daemon. Non-secret — a plain flag.
    #[serde(default)]
    pub(crate) refresh_enabled: Option<bool>,
    /// The daemon-level SYSTEMIC refresh-failure indicator (issue #378): `Some(n)` while the
    /// refresh MECHANISM is down (`n` consecutive all-eligible-account `outcome=error` sweeps past
    /// the configured threshold), else `None`/absent when healthy. Lets `sessiometer status` show
    /// the mechanism is down — a signal distinct from the per-account `auth` rollup, visible
    /// without waiting for an account to die. `Option` + `#[serde(default)]` per the added-field
    /// convention (this is the MINOR [`STATUS_SCHEMA_VERSION`] bump 1.0 → 1.1): a pre-#378 daemon
    /// omits the field → `None`, which the client renders as healthy. A COUNT only — never a token,
    /// path, or email (issue #15).
    #[serde(default)]
    pub(crate) systemic_refresh_failure: Option<u32>,
    /// The daemon-level CANONICAL-SCRUB rollup (issue #516): `Some(Recovering | Exhausted)` while the
    /// shared canonical item is scrubbed (recovering vs recovery-exhausted / un-recoverable), else
    /// absent when healthy. Lets `sessiometer status` + the menubar (#469) surface the fleet-wide
    /// scrubbed lockout that no per-account `auth` rollup reflects — the daemon-LEVEL sibling of the
    /// per-account `blind_active`. `Option` + `#[serde(default, skip_serializing_if = "Option::is_none")]`
    /// per the added-field convention (the MINOR [`STATUS_SCHEMA_VERSION`] bump 1.4 → 1.5, mirroring
    /// `blind_active`): a pre-#516 daemon omits the field → `None`, AND a HEALTHY snapshot omits it
    /// entirely, so a non-scrub frame's bytes are byte-for-byte unchanged (a pre-#516 client ignores
    /// the unknown key, the minor-bump tolerate-by-ignoring convention). A STATE discriminant only —
    /// never a token or email (issue #15).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) canonical_scrub: Option<CanonicalScrub>,
    /// The daemon-level KEYCHAIN-LOCKED flag (issue #498): `true` while the macOS login keychain is
    /// LOCKED, so the daemon cannot READ the shared credential item at ALL — the daemon-LEVEL sibling
    /// of `canonical_scrub`, but for an UNREADABLE item (access denied) rather than a readable-but-
    /// scrubbed one, so the operator remedy differs (unlock the keychain, not `claude /login`). Lets
    /// `sessiometer status` + the menubar (#498) surface a fleet-wide unreadable-credential lockout no
    /// per-account `auth` rollup reflects. A bare `bool` + `#[serde(default, skip_serializing_if =
    /// "std::ops::Not::not")]` per the added-field convention (the MINOR [`STATUS_SCHEMA_VERSION`] bump
    /// 1.5 → 1.6, taking `canonical_scrub`'s omit-when-healthy pattern): a pre-#498 daemon omits the
    /// field → `false`, AND an unlocked snapshot omits it entirely, so a non-locked frame's bytes are
    /// byte-for-byte unchanged (a pre-#498 client ignores the unknown key, the minor-bump
    /// tolerate-by-ignoring convention). A bare BINARY state discriminant — never a token or email
    /// (issue #15).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) keychain_locked: bool,
}

/// The FROZEN status-snapshot wire contract (issue #164): the [`StatusResponse`] payload plus the
/// two envelope fields that make it safe for an independently-released read-only client to bind
/// to — the contract [`SchemaVersion`] and the `generated_at` freshness stamp. This is the exact
/// struct the daemon serializes onto the control socket for a `status` request.
///
/// The payload is `#[serde(flatten)]`ed, so the wire JSON stays FLAT —
/// `{"schema_version":…,"generated_at":…,"accounts":…,"next_swap":…,"refresh_enabled":…}` — the
/// settled #137–#143 payload shape unchanged at top level, only PREFIXED with the two meta
/// fields. So existing internal readers that decode a bare [`StatusResponse`] (`poke`,
/// `use_account`) keep working: serde ignores the two extra top-level keys they do not name.
/// Non-secret by construction: the envelope adds a version object and a timestamp, and the
/// payload is the same redacted [`StatusResponse`] (issue #15).
///
/// `#[serde(default)]` on the two meta fields makes a PRE-#164 daemon's reply (which omits them)
/// decode to `SchemaVersion { major: 0, minor: 0 }` / `generated_at: 0` — a mismatched major the
/// client degrades on — rather than a decode error.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct VersionedStatus {
    /// The contract version the payload conforms to ([`STATUS_SCHEMA_VERSION`] from a current
    /// daemon). The reference client gates on `major` before rendering.
    #[serde(default)]
    pub(crate) schema_version: SchemaVersion,
    /// Wall-clock epoch SECONDS at which the daemon assembled this snapshot — the client's
    /// live-vs-stale signal. Copied from [`StatusSnapshot::generated_at`].
    #[serde(default)]
    pub(crate) generated_at: i64,
    /// The redacted per-account payload (issue #15), flattened so its fields sit at the top
    /// level of the wire JSON alongside the two envelope fields above.
    #[serde(flatten)]
    pub(crate) status: StatusResponse,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct AccountStatusLine {
    /// The operator-chosen handle (label) — never the email (issue #15).
    pub(crate) label: String,
    pub(crate) active: bool,
    /// Whether the account is in the rotation (issue #36); `false` for a parked
    /// account, which `status` marks. Non-secret — a plain flag.
    pub(crate) enabled: bool,
    /// Whether the account is QUARANTINED — its stored ACCESS token was rejected (a #42
    /// 401-streak), so it is out of rotation. NON-TERMINAL — the remedy is a refresh, not
    /// necessarily a re-login (issue #427); `false` for a healthy account. Non-secret — a
    /// plain flag.
    pub(crate) quarantined: bool,
    /// Whether a quarantined account is mid-RECOVERY — its credential is answering
    /// again and climbing toward un-quarantine (issue #109). Refines `quarantined`
    /// (true only when it is): lets `status` render `recovering` instead of the
    /// alarming `needs re-login` for a healing account, so an operator does not swap
    /// away from a recovering — and often healthier — account. Non-secret — a derived
    /// flag, no raw count. `#[serde(default)]` per the added-field convention (cf.
    /// `session_resets_at`): a pre-#109 daemon that omits it decodes to `false`.
    #[serde(default)]
    pub(crate) recovering: bool,
    /// Last-polled session-window usage percent (`0..=100`); `null` if the last
    /// poll for this account failed (never a fabricated `0`).
    pub(crate) session_pct: Option<u8>,
    /// Last-polled weekly-window usage percent (`0..=100`).
    pub(crate) weekly_pct: Option<u8>,
    /// Epoch seconds at which the rolling 5-hour SESSION window resets, or `null`
    /// when the last poll failed or the API supplied no parseable timestamp.
    /// Carried so the client can render a per-account "resets in" (issue #72); an
    /// absolute instant (not a relative duration), so the client computes the
    /// freshest delta against its own clock at print time. Non-secret — an integer.
    #[serde(default)]
    pub(crate) session_resets_at: Option<i64>,
    /// Epoch seconds at which the WEEKLY window resets (see `session_resets_at`).
    /// `null` when unknown. Non-secret — an integer.
    #[serde(default)]
    pub(crate) weekly_resets_at: Option<i64>,
    /// Whether the account's WEEKLY window is exhausted (`weekly >= weekly_trigger`),
    /// the daemon's own viability verdict (issue #11/#37). The client keys "resets
    /// in" off this: a weekly-exhausted account is blocked until the WEEKLY reset,
    /// otherwise the sooner SESSION reset governs (issue #72). Non-secret — a flag.
    #[serde(default)]
    pub(crate) weekly_exhausted: bool,
    /// The stored access-token `expiresAt` as epoch SECONDS (issue #119), or `null` until
    /// this account has been polled (issue #141) — sourced from the refresh sweep when
    /// `[refresh]` is on, otherwise from the poll path, so it is populated in the default
    /// config too. An absolute instant (not a relative duration, like `session_resets_at`)
    /// carried RAW for a consumer (`--json` | `jq`) to derive an "expires in" against its
    /// own clock; the lean text view projects only the rollup glyph, not a clock cell.
    /// Non-secret — a timestamp, never the token. `#[serde(default)]` per the added-field
    /// convention: a pre-#119 daemon that omits it decodes to `None`.
    #[serde(default)]
    pub(crate) access_expires_at: Option<i64>,
    /// The non-secret refresh-health inputs (issue #119) — last refresh ok? token rotated?
    /// consecutive failures — or `null` until a refresh has been observed (e.g. `[refresh]`
    /// off). The `--json` durability signal; also feeds the daemon's rollup. `#[serde(default)]`:
    /// a pre-#119 daemon omits it → `None`.
    #[serde(default)]
    pub(crate) refresh_health: Option<RefreshHealth>,
    /// The daemon-computed 5-state credential-auth rollup (issue #119): the verdict the
    /// thin read-only client projects to a glyph (🟢/🟡/🟠/🔴/⚪) under the `AUTH` column.
    /// Serialized on the `--json` wire as **`auth`** (issue #143 — the field reports the
    /// credential-AUTH standing, not a vague "health"; renamed while pre-release, no stable
    /// `--json` consumers yet); the Rust field keeps the name `health` to localize the
    /// rename to the wire key. `Option` for backward compatibility — `#[serde(default)]`
    /// makes a pre-#119 daemon (which omits the field) decode to `None`, and the client then
    /// FALLS BACK to the legacy quarantine-based text rather than mis-reading a defaulted
    /// `healthy` over a dead account.
    #[serde(default, rename = "auth")]
    pub(crate) health: Option<CredentialHealth>,
    /// The active account's bounded-blindness projection (issue #479, umbrella #363 Path B): blind
    /// duration + last-known session % + whether ADR-0017 auto-protection is DEGRADED — or absent
    /// when the active account is not blind (or this is not the active account). The client renders
    /// it as a SEMANTIC status line in place of the bare `n/a … 🟡` active row. `#[serde(default)]`
    /// decodes an omitting daemon to `None`; `skip_serializing_if` OMITS it whenever absent, so a
    /// non-blind account's per-line wire bytes are byte-for-byte unchanged — the additive MINOR
    /// `1.3 → 1.4` field appears ONLY on a blind active account (a pre-#479 client ignores the
    /// unknown key, the minor-bump tolerate-by-ignoring convention). Non-secret (issue #15).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) blind_active: Option<BlindActive>,
}

/// The next swap candidate shown by `status` (issue #88): who the daemon would
/// rotate the active session TO if a swap fired right now. DERIVED state —
/// recomputed each cycle from the latest readings — so, unlike the dropped in-process
/// `last_swap` (#8), it survives a daemon restart by construction and never reads
/// `none` merely because the process is young. Non-secret by construction: a roster
/// label or a bare reason, never a token or email (issue #15). One serializable type
/// for both [`StatusSnapshot`] (built each cycle) and [`StatusResponse`] (the wire),
/// mirroring the redaction posture of the now-removed `LastSwapLine`. Internally
/// tagged (`state`), so the three cases stay one self-describing field.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "state")]
pub(crate) enum NextSwap {
    /// A viable target exists — [`pick_target`]'s choice, by roster label, plus the daemon's own
    /// `reason` for choosing it ([`NextSwapReason`], issue #393). The reason is DAEMON-AUTHORITATIVE:
    /// a client cannot re-derive it (the session trigger / floor `pick_target` consumes are
    /// daemon-only, never on the wire), so it is carried here rather than guessed client-side. An
    /// ADDITIVE `Option` field (`#[serde(default)]`, the #164 minor-bump convention): a current
    /// daemon always sends `Some`, but a pre-#393 daemon omits it → `None`, which a renderer shows
    /// as a bare target label with no rationale (mirroring the `health` / `refresh_enabled`
    /// pre-freeze-compat posture).
    Target {
        to: String,
        #[serde(default)]
        reason: Option<NextSwapReason>,
    },
    /// No sound swap destination — [`pick_target`] picked nothing AND this is not the
    /// post-restart all-unpolled moment (`AwaitingData`). Reached when at least one
    /// *live* (enabled, non-quarantined) other account has already been polled and none
    /// qualifies (weekly-exhausted, or over the `target_max_session_usage` reserve) — even while other
    /// live accounts are still unpolled (the staggered-warm-up #80 mixed case) — or when
    /// there is no live other account at all (every other disabled #36 or quarantined #42,
    /// its reading masked away by `decision_readings`, or there is simply no other account).
    ///
    /// Carries the same fleet-capacity RELIEF hint the durable `all_exhausted` /
    /// `active_dead_no_target` events do (issue #405): `cause` names WHY the fleet is blocked
    /// ([`NoTargetCause`]) and `resets_at` WHEN capacity returns — the [`all_exhausted_relief`]
    /// classification, so BOTH the CLI footer and the menubar can tell the operator "out of
    /// capacity, resets in ⟨dur⟩ — add an account" instead of a content-free "no viable target".
    /// This surfaces the SAME hint whether the active is alive-and-over-trigger or DEAD-and-stranded
    /// (the dead active's 🔴 health shows separately on its own account row, so the composite —
    /// re-login the dead credential AND wait for / add capacity — emerges). Both fields are ADDITIVE
    /// `Option`s (`#[serde(default)]`, the #164 minor-bump convention): a current daemon always
    /// sends `Some(cause)` (relief always classifies a cause), and `resets_at` whenever the relevant
    /// window reported a parseable reset; a pre-#405 daemon omits both → `None`, which a renderer
    /// falls back to the bare "no viable target" on (the pre-freeze-compat posture `reason` on
    /// [`Self::Target`] and `health` share).
    NoViableTarget {
        #[serde(default)]
        cause: Option<NoTargetCause>,
        #[serde(default)]
        resets_at: Option<i64>,
    },
    /// No reading yet for any *live* (enabled, non-quarantined) other account — the
    /// post-restart moment, before the staggered poll loop (#80) has read the rotation.
    /// Kept distinct from `NoViableTarget` because it is exactly the moment an operator
    /// checks `status`; a quarantined account's masked-away reading does NOT count here
    /// (its data needs a re-login, not a poll).
    AwaitingData,
}

/// WHY [`pick_target`] chose the [`NextSwap::Target`] it did (issue #393) — the daemon's own
/// selection rationale, carried on the wire so BOTH the panel footer and `sessiometer status`
/// render the ONE reason the daemon actually used, each in its own idiom (R-2 STATE-parity: a
/// structured discriminant, not a pre-formatted string that would force identical wording on both
/// media). Distinct from [`crate::observability::SwapReason`], which records why a swap FIRED
/// (session / weekly / manual / forced); this records why a particular TARGET won selection.
///
/// The variants track [`pick_target`]'s ACTUAL axis (issue #37 — soonest weekly reset among
/// viable accounts), NOT the superseded "most headroom" rule the client used to assert. Internally
/// tagged on `kind` (NOT `reason`, which is the carrying field name on [`NextSwap::Target`], nor
/// `state`, [`NextSwap`]'s own tag), so the nested wire shape is unambiguous:
/// `{"state":"target","to":"…","reason":{"kind":"soonest_reset","resets_at":…}}`. Non-secret — a
/// discriminant plus one epoch timestamp, never a token or email (issue #15).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub(crate) enum NextSwapReason {
    /// Two or more accounts qualified and this one's WEEKLY window resets SOONEST — the live #37
    /// selection axis. `resets_at` is the winner's weekly-reset epoch (the `min_by_key` key
    /// `pick_target` sorts on, previously computed then discarded before serialization).
    SoonestReset { resets_at: i64 },
    /// Exactly ONE account qualified, so nothing discriminated the winner — it is the sole viable
    /// target. Carries no epoch: its weekly reset, known or not, decided nothing.
    OnlyCandidate,
    /// Two or more accounts qualified but NONE reported a weekly reset, so no soonest-reset
    /// tiebreak existed and selection fell to the earliest roster index (`min_by_key` keeps the
    /// first of equal keys). Deliberately DISTINCT from [`Self::OnlyCandidate`]: several targets
    /// were viable here, so a renderer must never claim this one was the only one — that would be
    /// the very false-rationale bug #393 exists to remove. Carries no epoch because none exists.
    RosterOrder,
}

/// WHY [`NextSwap::NoViableTarget`] has no target — the fleet-capacity RELIEF cause (issue #405),
/// the forward-looking sibling of the durable `all_exhausted` / `active_dead_no_target` events'
/// `cause`. Carried on the wire so BOTH the CLI footer and the menubar render the ONE cause the
/// daemon's [`all_exhausted_relief`] classification produced, each in its own idiom (R-2
/// STATE-parity: a structured discriminant, not a pre-formatted string).
///
/// Deliberately a WIRE-LOCAL enum distinct from [`crate::observability::SwapReason`] — exactly as
/// [`NextSwapReason`] is — for two reasons: `SwapReason` additionally carries `Manual` / `Forced`
/// (operator-swap reasons that cannot arise from a no-target verdict), and it is not `serde`, so
/// putting it on the wire would both widen the contract's value set nonsensically and couple the
/// diagnostic enum to the wire. This carries ONLY the two causes relief can report. `snake_case`,
/// so a value is `"session"` or `"weekly"`. Non-secret — a bare discriminant (issue #15).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum NoTargetCause {
    /// A weekly-VIABLE account is held out only by session (over the session ceiling
    /// `min(session_trigger, target_max_session_usage)`) — relief arrives at the sooner SESSION reset.
    Session,
    /// Every candidate is weekly-EXHAUSTED (`weekly >= weekly_trigger`) — relief arrives at the
    /// WEEKLY reset (the #11 default, and the ONLY cause reachable on the emergency/dead-active
    /// path, which bypasses the session gate entirely).
    Weekly,
}

/// Project a [`StatusSnapshot`] into the wire [`StatusResponse`]. Sourced solely
/// from non-secret fields, so it can never carry a token or email (issue #15).
pub(crate) fn status_response(snapshot: &StatusSnapshot) -> StatusResponse {
    StatusResponse {
        accounts: snapshot
            .accounts
            .iter()
            .map(|account| AccountStatusLine {
                label: account.label.clone(),
                active: account.active,
                enabled: account.enabled,
                quarantined: account.quarantined,
                recovering: account.recovering,
                session_pct: account.usage.map(|u| to_pct(u.session)),
                weekly_pct: account.usage.map(|u| to_pct(u.weekly)),
                session_resets_at: account.usage.and_then(|u| u.session_resets_at),
                weekly_resets_at: account.usage.and_then(|u| u.weekly_resets_at),
                weekly_exhausted: account.weekly_exhausted,
                // The credential clocks + the daemon-computed rollup (issue #119), already
                // resolved at snapshot build; `health` is wrapped `Some` since a current
                // daemon always sends a verdict (the `Option` is purely pre-#119 wire compat).
                access_expires_at: account.access_expires_at,
                refresh_health: account.refresh_health,
                health: Some(account.health),
                // The bounded-blindness projection (issue #479), already resolved daemon-side in
                // `Daemon::snapshot`; copied straight to the wire. `None` for every non-active or
                // non-blind account, so `skip_serializing_if` omits it there.
                blind_active: account.blind_active,
            })
            .collect(),
        // Already computed at snapshot build (issue #88); copy it to the wire.
        next_swap: snapshot.next_swap.clone(),
        // The config `[refresh].enabled` (#105) for the #138 advisory; wrapped `Some` since a
        // current daemon always knows it (the `Option` is purely pre-#138 wire compat, mirroring
        // `health`).
        refresh_enabled: Some(snapshot.refresh_enabled),
        // The daemon-level systemic refresh-failure indicator (issue #378), copied straight to the
        // wire: `Some(n)` while the mechanism is down, `None` when healthy.
        systemic_refresh_failure: snapshot.systemic_refresh,
        // The daemon-level canonical-scrub rollup (issue #516), copied straight to the wire:
        // `Some(Recovering | Exhausted)` while the shared canonical is scrubbed, `None` when healthy.
        canonical_scrub: snapshot.canonical_scrub,
        // The daemon-level keychain-locked flag (issue #498), copied straight to the wire: `true`
        // while the login keychain is locked (the shared credential is unreadable), `false` when
        // unlocked (and then omitted from the wire via `skip_serializing_if`).
        keychain_locked: snapshot.keychain_locked,
    }
}

/// Wrap a [`StatusSnapshot`] into the FROZEN wire envelope (issue #164): stamp the current
/// [`STATUS_SCHEMA_VERSION`] and copy the snapshot's `generated_at`, around the same
/// [`status_response`] payload projection. This is the single function the control socket
/// serializes for a `status` request, so EVERY reply carries the contract version + freshness
/// stamp. Non-secret for the same reason `status_response` is — the envelope adds only a version
/// object and a timestamp (issue #15).
pub(crate) fn versioned_status_response(snapshot: &StatusSnapshot) -> VersionedStatus {
    VersionedStatus {
        schema_version: STATUS_SCHEMA_VERSION,
        generated_at: snapshot.generated_at,
        status: status_response(snapshot),
    }
}

/// A usage fraction in `[0.0, 1.0]` as a rounded, clamped `0..=100` percent.
pub(crate) fn to_pct(fraction: f64) -> u8 {
    (fraction * 100.0).round().clamp(0.0, 100.0) as u8
}

/// The daemon-side credential-health rollup (issue #119, extended by #137) — a PURE function
/// of one account's health inputs, its fresh-reading liveness signal, and the wall clock, so
/// it is unit-tested directly and computed identically for the display snapshot and the
/// transition-event diff. The thin `status` client just projects the returned verdict to a
/// glyph.
///
/// A SEVERITY ladder (most-severe wins), matching the issue's 🟢→🟡→🟠→🔴 ordering, plus a
/// distinct ⚪ `Unknown` for the no-evidence case (#137):
/// - **Dead** — the last refresh outcome was `Dead`: a sweep-refresh actually rejected the
///   REFRESH token (the #261 / `CredentialUnrecoverable` cue). This is PROVEN death and the
///   ONLY 🔴 / `claude /login` case (issue #427). A DISPLAY rollup — it never flips the
///   quarantine machinery; surfacing a refresh-detected death is more honest than hiding it.
/// - **Degraded** — `quarantined` (the #42 access-token 401-streak verdict) but NOT proven
///   dead (issue #427). A usage-endpoint 401 rejects the ACCESS token and says nothing about
///   the REFRESH token (a resource server never sees it), so the account is out of rotation
///   right now yet `poke` / a restart revive it — it needs a REFRESH, not a re-login. 🟠
///   NON-TERMINAL; checked AFTER proven `Dead` so a quarantined account whose refresh has
///   ALSO returned `Dead` still reads the terminal 🔴.
/// - **AtRisk** — the refresh safety-net is failing (`consecutive_refresh_failures > 0`):
///   a streak of `Error` cycles means the mechanism that prevents staleness/death is
///   struggling, so the account trends toward dead even while its token may still work.
/// - **Stale** — the stored REFRESH-sourced access token has EXPIRED (`access_expires_at <=
///   now_secs`) but the refresh token is still valid (not dead, not failing): a transient
///   window the next refresh recovers. Keys off `access_expires_at` ONLY (never the
///   poll-sourced clock), so an idle account's naturally-lapsed stashed expiry never
///   false-🟠s (#141/#137).
/// - **Healthy** — a POSITIVE liveness signal exists: a fresh successful usage reading
///   (`has_fresh_reading`), OR refresh telemetry, OR a (future) refresh-sourced expiry.
/// - **Unknown** — none of the above AND no positive liveness signal (#137): a non-active
///   account never successfully polled, `[refresh]` off, no/unknown `access_expires_at`.
///   Absence of a NEGATIVE signal is not health; the daemon reports "unverified" rather than
///   a false 🟢 that would jump straight to 🔴 the moment the 401-streak quarantines it.
///
/// `has_fresh_reading` is this account's masked [`decision_readings`](Daemon::decision_readings)
/// entry being `Some` — a SUCCESSFUL poll against the live API (the strongest liveness proof),
/// `None` for a failed poll or an out-of-rotation account. Deliberately NOT `poll_expires_at`:
/// that clock is written on every poll ATTEMPT (even a 401 against a readable-but-revoked
/// stash), so it cannot distinguish alive from the exact lapsed-credential bug #137 fixes; it
/// stays the display clock only (`--json`, via [`Daemon::snapshot`]'s `.or()` fallback).
pub(crate) fn credential_health(
    quarantined: bool,
    last_refresh_outcome: Option<RefreshEventOutcome>,
    consecutive_refresh_failures: u32,
    access_expires_at: Option<i64>,
    has_fresh_reading: bool,
    now_secs: i64,
) -> CredentialHealth {
    if last_refresh_outcome == Some(RefreshEventOutcome::Dead) {
        // PROVEN death: a refresh actually rejected the REFRESH token (#261). The only 🔴
        // `claude /login` case — checked FIRST so it wins over a co-occurring quarantine.
        CredentialHealth::Dead
    } else if quarantined {
        // The ACCESS token 401-streaked into quarantine (#42) but the refresh token is
        // unproven — NON-TERMINAL (issue #427): needs a REFRESH (`poke` / restart), not a
        // re-login. Ranks above `AtRisk`: the account is out of rotation NOW, not merely
        // trending, so a quarantine wins even alongside a refresh-failure streak.
        CredentialHealth::Degraded
    } else if consecutive_refresh_failures > 0 {
        CredentialHealth::AtRisk
    } else if access_expires_at.is_some_and(|expires_at| expires_at <= now_secs) {
        CredentialHealth::Stale
    } else if has_fresh_reading || last_refresh_outcome.is_some() || access_expires_at.is_some() {
        CredentialHealth::Healthy
    } else {
        CredentialHealth::Unknown
    }
}

/// Reduce one account's stored refresh observations into the non-secret [`RefreshHealth`]
/// the wire surfaces (issue #119), or `None` when no refresh has been observed yet. `last_ok`
/// collapses the full outcome to alive-vs-not (`Refreshed` / `NoChange` ⇒ ok; `Dead` /
/// `Error` ⇒ not), the rollup's finer `Dead`-vs-`Error` distinction having already been
/// applied by [`credential_health`].
pub(crate) fn refresh_health_view(health: &AccountHealth) -> Option<RefreshHealth> {
    let outcome = health.last_refresh_outcome?;
    Some(RefreshHealth {
        last_ok: matches!(
            outcome,
            RefreshEventOutcome::Refreshed
                | RefreshEventOutcome::RefreshedNotReStashed
                | RefreshEventOutcome::NoChange
        ),
        rotated: health.refresh_token_rotated.unwrap_or(false),
        consecutive_failures: health.consecutive_refresh_failures,
    })
}
