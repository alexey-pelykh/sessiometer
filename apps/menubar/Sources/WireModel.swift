// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// The daemon `watch` wire model + frame decoder for the menu-bar app (issue #322).
//
// Hand-written Swift `Codable` mirrors of the daemon's frozen, versioned JSON status
// snapshot and its `watch`-stream frames — pure value types plus one decode function,
// no I/O. Per ADR-0010 the app links NO Rust and shares NO build graph with the crate;
// the AF_UNIX socket carrying these frames is the ENTIRE boundary, so this file is a
// hand-maintained mirror of the Rust source of truth, NOT an FFI binding.
//
// Source of truth (mirror these — do not re-derive):
//   * `src/daemon/snapshot.rs` — `VersionedStatus`, `StatusResponse`, `AccountStatusLine`,
//     `RefreshHealth`, `SchemaVersion`, `NextSwap`, `STATUS_SCHEMA_VERSION` (the #164
//     frozen contract).
//   * `src/daemon/socket.rs` — `SnapshotFrame` / `HeartbeatFrame` encoders and
//     `parse_watch_frame` (the reference decoder this file mirrors), issue #165.
//   * `src/observability.rs` — `CredentialHealth` (the 5+1 state rollup).
//
// The wire is FLAT: the daemon `#[serde(flatten)]`s the payload into the envelope, so a
// snapshot line is `{"type":"snapshot","schema_version":…,"generated_at":…,"accounts":…,
// "next_swap":…,"refresh_enabled":…}`. Decode strategy mirrors serde field-by-field:
// required fields use `decode`; `Option<T>` fields (and `#[serde(default)]` scalars)
// use `decodeIfPresent` so an absent OR null key is tolerated — the additive-minor
// forward-compatibility the #164 contract is built on.

import Foundation

// MARK: - Contract version + major gate

/// The status-snapshot wire contract version (`src/daemon/snapshot.rs` `SchemaVersion`):
/// a `major.minor` the daemon stamps on every reply. A MAJOR bump is a BREAKING change an
/// older client MUST refuse to render rather than mis-read; a MINOR bump is additive and an
/// older client tolerates by ignoring what it does not know.
///
/// Both fields are required WHEN the object is present (the Rust fields carry no
/// `#[serde(default)]`); an ABSENT `schema_version` key defaults the whole object to
/// `{0, 0}` at the envelope level (see `VersionedStatus`), the "unknown, pre-freeze"
/// version the major gate treats as unsupported.
struct SchemaVersion: Decodable, Equatable {
    let major: UInt32
    let minor: UInt32

    /// The sentinel an ABSENT `schema_version` decodes to (major `0`) — a pre-#164, pre-freeze
    /// daemon. Always unsupported by the major gate (fail-safe), so it is degraded on rather
    /// than mis-read as a compatible contract.
    static let preFreeze = SchemaVersion(major: 0, minor: 0)
}

/// The frozen-contract compatibility gate (`src/daemon/snapshot.rs`
/// `STATUS_SCHEMA_VERSION`, `parse_watch_frame`'s "the client gates on `major`
/// before rendering"). Decoding NEVER fails on a major mismatch — the frame still decodes so
/// the client can read `generated_at` and degrade gracefully; the gate is a SEPARATE
/// render-time check. The MINOR is deliberately not restated here — it moves on every additive
/// bump, and `supportedSchemaMajor` below is the only version this gate reads.
enum WireContract {
    /// The status-snapshot contract MAJOR this client speaks. Mirrors
    /// `STATUS_SCHEMA_VERSION.major` in `src/daemon/snapshot.rs`; bump in lockstep when the
    /// client is updated to a new breaking contract.
    static let supportedSchemaMajor: UInt32 = 1

    /// Whether `version`'s major is one this client can safely render. A pre-freeze / absent
    /// version decodes to major `0`, which is NOT `supportedSchemaMajor` → unsupported
    /// (fail-safe): an unknown contract is degraded on, never mis-rendered as compatible.
    static func isSupported(_ version: SchemaVersion) -> Bool {
        version.major == supportedSchemaMajor
    }
}

// MARK: - Leaf value types

/// The daemon-computed credential-auth rollup (`src/observability.rs` `CredentialHealth`),
/// carried on the wire under the key **`auth`** (the Rust field is named `health`; issue #143
/// renamed the wire key). Serialized `snake_case`; a value the client does not recognise is a
/// decode error (mirrors serde's unknown-variant rejection), but `auth` is optional so an
/// absent / null key is tolerated (`None`).
///
/// `degraded` (issue #427) is the NON-TERMINAL split of the old `dead` catch-all: a bare
/// quarantine (an access-token 401-streak) needs a REFRESH, not a re-login, so it renders 🟠
/// with a needs-refresh cue — the terminal 🔴 `dead` is reserved for a PROVEN refresh-token
/// death. The status client and menubar must AGREE on this (single source of truth, #169).
enum CredentialHealth: String, Decodable, Equatable {
    case healthy
    case unknown
    case stale
    case atRisk = "at_risk"
    case degraded
    case dead
}

/// The non-secret refresh-health inputs (`src/daemon/snapshot.rs` `RefreshHealth`): whether
/// the last refresh kept the credential alive, whether the refresh token VALUE rotated, and
/// the consecutive-failure streak. All three fields are required when the object is present
/// (the Rust struct carries no per-field default); the CARRYING field on the account is
/// optional, so a whole absent / null `refresh_health` is tolerated.
struct RefreshHealth: Decodable, Equatable {
    let lastOk: Bool
    let rotated: Bool
    let consecutiveFailures: UInt32

    private enum CodingKeys: String, CodingKey {
        case lastOk = "last_ok"
        case rotated
        case consecutiveFailures = "consecutive_failures"
    }
}

/// WHY the daemon chose the `target` it did (`src/daemon/snapshot.rs` `NextSwapReason`, issue
/// #393) — its OWN selection rationale, carried so the client renders the reason the daemon
/// actually used rather than re-deriving a (superseded, wrong) one. Internally tagged on `kind`
/// (`snake_case`), so a value is one of three shapes:
///   * `{"kind":"soonest_reset","resets_at":<epoch>}` — ≥2 accounts qualified and this one's
///     weekly window resets soonest (the live #37 axis); `resetsAt` is that epoch.
///   * `{"kind":"only_candidate"}` — exactly one account qualified; nothing discriminated it.
///   * `{"kind":"roster_order"}` — ≥2 qualified but none reported a weekly reset, so no tiebreak
///     existed and the earliest roster index won. NEVER render this as "only viable target":
///     other targets were viable, and that false claim is the bug #393 removes.
///
/// An UNKNOWN `kind` is a decode error, mirroring serde's internally-tagged enum. Mirrors the
/// daemon's variant set; render each medium's own way, never a pre-formatted string (state-parity).
enum NextSwapReason: Equatable {
    case soonestReset(resetsAt: Int64)
    case onlyCandidate
    case rosterOrder
}

extension NextSwapReason: Decodable {
    private enum CodingKeys: String, CodingKey {
        case kind
        case resetsAt = "resets_at"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        let kind = try container.decode(String.self, forKey: .kind)
        switch kind {
        case "soonest_reset":
            self = .soonestReset(resetsAt: try container.decode(Int64.self, forKey: .resetsAt))
        case "only_candidate":
            self = .onlyCandidate
        case "roster_order":
            self = .rosterOrder
        default:
            throw DecodingError.dataCorruptedError(
                forKey: .kind,
                in: container,
                debugDescription:
                    "unknown next_swap reason '\(kind)' — an incompatible wire contract"
            )
        }
    }
}

/// WHY the daemon has no viable swap target (`src/daemon/snapshot.rs` `NoTargetCause`), carried on
/// `NextSwap.noViableTarget` (issue #405) so a renderer can name the fleet-capacity RELIEF instead
/// of a content-free "no viable target". A PLAIN string on the wire (`"session"` / `"weekly"`, NOT
/// internally tagged — it is the daemon's `all_exhausted_relief` classification, not one of serde's
/// tagged enums), so a `String`-raw enum decodes it; an UNKNOWN value is a hard decode error,
/// mirroring serde's rejection of an unknown unit-enum variant. Non-secret — a bare discriminant
/// (issue #15). Distinct from `NextSwapReason` (why a particular TARGET won): this says why NONE did.
enum NoTargetCause: String, Decodable, Equatable {
    /// Every other account is over its session limit — a transient block; the session windows reset
    /// soon, so the remedy is to WAIT (the CLI names the reset and does not nudge "add an account").
    case session
    /// Every other account is weekly-exhausted (issue #37) — a week-long block; capacity returns only
    /// at the soonest weekly reset, so ADDING an account is the real remedy (the render nudges it).
    case weekly
}

/// The next swap candidate (`src/daemon/snapshot.rs` `NextSwap`): who the daemon would rotate
/// the active session to, or why there is no candidate. Internally tagged on `state`
/// (`snake_case`), so a value is one of three shapes:
///   * `{"state":"target","to":"<label>","reason":<NextSwapReason>}`
///   * `{"state":"no_viable_target","cause":<NoTargetCause>,"resets_at":<epoch>}`
///   * `{"state":"awaiting_data"}`
///
/// An UNKNOWN `state` is a decode error — faithfully mirroring serde's internally-tagged enum,
/// which rejects a variant it does not know (verified against the daemon: `unknown variant …`).
/// The whole `next_swap` key is optional (`null` when there is no active anchor), handled at
/// `VersionedStatus`. The target's `reason` (issue #393) is ADDITIVE and optional — a current
/// daemon always sends it, but a pre-#393 daemon omits it → `nil`, tolerated via `decodeIfPresent`
/// (the same additive-minor forward-compat the whole contract rests on). `no_viable_target`'s
/// `cause` + `resets_at` (issue #405) are ADDITIVE the same way — a current daemon carries the
/// fleet-capacity relief, a pre-#405 daemon omits both → `nil`, tolerated identically.
enum NextSwap: Equatable {
    case target(to: String, reason: NextSwapReason?)
    case noViableTarget(cause: NoTargetCause?, resetsAt: Int64?)
    case awaitingData
}

extension NextSwap: Decodable {
    private enum CodingKeys: String, CodingKey {
        case state
        case to
        case reason
        case cause
        case resetsAt = "resets_at"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        let state = try container.decode(String.self, forKey: .state)
        switch state {
        case "target":
            self = .target(
                to: try container.decode(String.self, forKey: .to),
                reason: try container.decodeIfPresent(NextSwapReason.self, forKey: .reason)
            )
        case "no_viable_target":
            self = .noViableTarget(
                cause: try container.decodeIfPresent(NoTargetCause.self, forKey: .cause),
                resetsAt: try container.decodeIfPresent(Int64.self, forKey: .resetsAt)
            )
        case "awaiting_data":
            self = .awaitingData
        default:
            throw DecodingError.dataCorruptedError(
                forKey: .state,
                in: container,
                debugDescription:
                    "unknown next_swap state '\(state)' — an incompatible wire contract"
            )
        }
    }
}

// MARK: - The redacted per-account payload line

/// One account's redacted status line (`src/daemon/snapshot.rs` `AccountStatusLine`) — a
/// handle plus flags, percentages, and clocks; NEVER a token or email (issue #15).
///
/// `label` / `active` / `enabled` / `quarantined` are required (the Rust fields carry no
/// default). Every other field is additive: `recovering` / `weeklyExhausted` default `false`
/// and the rest default to `nil` when the key is absent OR null — so a pre-#109 / pre-#119
/// daemon that omits them still decodes. The `auth` property is the wire key; the Rust field
/// it mirrors is named `health`.
struct AccountStatusLine: Decodable, Equatable {
    /// The operator-chosen handle — never the email (issue #15).
    let label: String
    let active: Bool
    /// Whether the account is in the rotation (issue #36); `false` for a parked account.
    let enabled: Bool
    /// Whether the credential is dead and needs a re-login (issue #42).
    let quarantined: Bool
    /// Whether a quarantined account is mid-recovery (issue #109). Defaults `false`.
    let recovering: Bool
    /// Last-polled session-window usage percent (`0…100`); `nil` if the last poll failed.
    let sessionPct: UInt8?
    /// Last-polled weekly-window usage percent (`0…100`).
    let weeklyPct: UInt8?
    /// Epoch seconds at which the rolling 5-hour session window resets; `nil` when unknown.
    let sessionResetsAt: Int64?
    /// Epoch seconds at which the weekly window resets; `nil` when unknown.
    let weeklyResetsAt: Int64?
    /// Whether the weekly window is exhausted (issue #11/#37). Defaults `false`.
    let weeklyExhausted: Bool
    /// The stored access-token `expiresAt` as epoch seconds (issue #119); `nil` until polled.
    let accessExpiresAt: Int64?
    /// The non-secret refresh-health inputs (issue #119); `nil` until a refresh is observed.
    let refreshHealth: RefreshHealth?
    /// The 5+1-state credential-auth rollup (issue #119), wire key `auth`; `nil` for a pre-#119
    /// daemon (the client then falls back to the quarantine flag rather than a defaulted value).
    let auth: CredentialHealth?

    private enum CodingKeys: String, CodingKey {
        case label
        case active
        case enabled
        case quarantined
        case recovering
        case sessionPct = "session_pct"
        case weeklyPct = "weekly_pct"
        case sessionResetsAt = "session_resets_at"
        case weeklyResetsAt = "weekly_resets_at"
        case weeklyExhausted = "weekly_exhausted"
        case accessExpiresAt = "access_expires_at"
        case refreshHealth = "refresh_health"
        case auth
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        label = try c.decode(String.self, forKey: .label)
        active = try c.decode(Bool.self, forKey: .active)
        enabled = try c.decode(Bool.self, forKey: .enabled)
        quarantined = try c.decode(Bool.self, forKey: .quarantined)
        recovering = try c.decodeIfPresent(Bool.self, forKey: .recovering) ?? false
        sessionPct = try c.decodeIfPresent(UInt8.self, forKey: .sessionPct)
        weeklyPct = try c.decodeIfPresent(UInt8.self, forKey: .weeklyPct)
        sessionResetsAt = try c.decodeIfPresent(Int64.self, forKey: .sessionResetsAt)
        weeklyResetsAt = try c.decodeIfPresent(Int64.self, forKey: .weeklyResetsAt)
        weeklyExhausted = try c.decodeIfPresent(Bool.self, forKey: .weeklyExhausted) ?? false
        accessExpiresAt = try c.decodeIfPresent(Int64.self, forKey: .accessExpiresAt)
        refreshHealth = try c.decodeIfPresent(RefreshHealth.self, forKey: .refreshHealth)
        auth = try c.decodeIfPresent(CredentialHealth.self, forKey: .auth)
    }
}

// MARK: - The frozen versioned envelope

/// The FROZEN status-snapshot wire contract (`src/daemon/snapshot.rs` `VersionedStatus`,
/// issue #164): the redacted `StatusResponse` payload plus the two envelope fields
/// (`schema_version`, `generated_at`), FLATTENED into one object. Modeled flat here (matching
/// the wire) rather than as a nested payload, and the client version-gates on
/// `schemaVersion` before rendering (`isSchemaSupported`).
///
/// `schemaVersion` defaults to `{0, 0}` and `generatedAt` to `0` when their keys are absent
/// (a pre-#164 daemon) — the exact serde `#[serde(default)]` behaviour. `accounts` is required
/// (a bare `Vec` with no default); `nextSwap` / `refreshEnabled` are optional. An extra
/// top-level key (e.g. the frame's own `type`, or a future minor field) is ignored.
struct VersionedStatus: Decodable, Equatable {
    /// The contract version the payload conforms to; the client gates on `major` before render.
    let schemaVersion: SchemaVersion
    /// Wall-clock epoch seconds at which the daemon assembled this snapshot — the live-vs-stale
    /// signal a client compares against its own clock.
    let generatedAt: Int64
    /// The redacted per-account payload (issue #15).
    let accounts: [AccountStatusLine]
    /// The next swap candidate, or `nil` when there is no active anchor to swap from.
    let nextSwap: NextSwap?
    /// Whether the daemon's periodic isolated-refresh tick is enabled (issue #105/#138); `nil`
    /// for a pre-#138 daemon (client treats unknown as "suppress the advisory").
    let refreshEnabled: Bool?
    /// The daemon-level SYSTEMIC refresh-failure indicator (`src/daemon/snapshot.rs`
    /// `StatusResponse.systemic_refresh_failure`, issue #378): a COUNT (never a token — issue #15)
    /// of consecutive all-eligible-account `outcome=error` sweeps while the refresh MECHANISM is
    /// down, or `nil`/absent when healthy. A signal distinct from the per-account `auth` rollup,
    /// visible without waiting for an account to die; `nil` for a pre-#378 daemon (rendered as
    /// healthy). Added by the MINOR `1.0 → 1.1` bump — an older client tolerates it by ignoring.
    let systemicRefreshFailure: UInt32?

    private enum CodingKeys: String, CodingKey {
        case schemaVersion = "schema_version"
        case generatedAt = "generated_at"
        case accounts
        case nextSwap = "next_swap"
        case refreshEnabled = "refresh_enabled"
        case systemicRefreshFailure = "systemic_refresh_failure"
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        schemaVersion =
            try c.decodeIfPresent(SchemaVersion.self, forKey: .schemaVersion)
            ?? .preFreeze
        generatedAt = try c.decodeIfPresent(Int64.self, forKey: .generatedAt) ?? 0
        accounts = try c.decode([AccountStatusLine].self, forKey: .accounts)
        nextSwap = try c.decodeIfPresent(NextSwap.self, forKey: .nextSwap)
        refreshEnabled = try c.decodeIfPresent(Bool.self, forKey: .refreshEnabled)
        systemicRefreshFailure = try c.decodeIfPresent(UInt32.self, forKey: .systemicRefreshFailure)
    }

    /// Whether this snapshot's contract major is one the client can render (`WireContract`).
    /// A major mismatch (including the pre-freeze `0`) is flagged, never mis-rendered.
    var isSchemaSupported: Bool { WireContract.isSupported(schemaVersion) }
}

// MARK: - Watch-stream frames + decoder

/// A decoded `watch` stream frame (`src/daemon/socket.rs` `WatchFrame` / `parse_watch_frame`,
/// issue #165) — the client-side counterpart of the daemon's `type`-tagged frame encoders.
enum WatchFrame: Equatable {
    /// A full status snapshot (the frozen #164 envelope).
    case snapshot(VersionedStatus)
    /// A liveness beat carrying the last-known freshness stamp and contract version.
    case heartbeat(generatedAt: Int64, schemaVersion: SchemaVersion)
    /// A frame whose `type` this client does not understand, or a line with no `type` tag —
    /// IGNORED by a forward-compatible client (the #164 additive ethos), never a hard error.
    case unknown
}

extension WatchFrame {
    /// The contract version this frame carries (`snapshot` and `heartbeat` both stamp one), or
    /// `nil` for `.unknown`. A client version-gates on `schemaVersion?.major` before rendering
    /// a snapshot or trusting a heartbeat's freshness.
    var schemaVersion: SchemaVersion? {
        switch self {
        case .snapshot(let versioned):
            return versioned.schemaVersion
        case .heartbeat(_, let version):
            return version
        case .unknown:
            return nil
        }
    }
}

/// Classify + decode one `watch` stream line (`src/daemon/socket.rs` `parse_watch_frame`,
/// issue #165). Probes the `type` tag FIRST — the same probe-then-decode shape the daemon's
/// reference decoder uses — then decodes the matching frame:
///   * `"snapshot"` → the frozen #164 envelope (the extra `type` key is ignored by the payload
///     decode).
///   * `"heartbeat"` → its freshness envelope (`generated_at` + `schema_version`).
///   * any other `type`, or a MISSING `type` → `.unknown` (ignored, not an error).
///
/// A line that is not valid JSON — or a well-tagged line whose body does not match the contract
/// (a missing required field, an unknown `next_swap` state, a bad `auth` value) — THROWS,
/// mirroring the daemon: "a malformed line is a hard error." Pure: no I/O, no clock.
func parseWatchFrame(_ line: String) throws -> WatchFrame {
    let data = Data(line.utf8)
    let decoder = JSONDecoder()
    // Probe the discriminator. A non-JSON line fails here (→ throw); a valid line whose `type`
    // is absent decodes `nil` (→ `.unknown`), never an error.
    let probe = try decoder.decode(FrameProbe.self, from: data)
    switch probe.type {
    case "snapshot":
        return .snapshot(try decoder.decode(VersionedStatus.self, from: data))
    case "heartbeat":
        let frame = try decoder.decode(HeartbeatFrame.self, from: data)
        return .heartbeat(generatedAt: frame.generatedAt, schemaVersion: frame.schemaVersion)
    default:
        return .unknown
    }
}

/// The `type`-tag probe (`parse_watch_frame`'s `Probe`): `type` is optional, so a line with no
/// tag classifies as `.unknown` rather than failing. Unknown sibling keys are ignored.
private struct FrameProbe: Decodable {
    let type: String?
}

/// A `watch` heartbeat frame (`src/daemon/socket.rs` `HeartbeatFrame`): the `type` tag plus the
/// freshness envelope and no payload. `generated_at` is required (no default); `schema_version`
/// defaults to `{0, 0}` when absent, so an older daemon's beat still decodes and gates.
private struct HeartbeatFrame: Decodable {
    let generatedAt: Int64
    let schemaVersion: SchemaVersion

    private enum CodingKeys: String, CodingKey {
        case generatedAt = "generated_at"
        case schemaVersion = "schema_version"
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        generatedAt = try c.decode(Int64.self, forKey: .generatedAt)
        schemaVersion =
            try c.decodeIfPresent(SchemaVersion.self, forKey: .schemaVersion)
            ?? .preFreeze
    }
}
