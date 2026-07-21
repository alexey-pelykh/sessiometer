// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// The daemon `config-get` / `config-set` wire model + codecs for the settings UI (issue #268) — the
// config-editing sibling of the `watch` / `stats` decoders in `WireModel.swift`. Per ADR-0010 the app
// links NO Rust and shares NO build graph with the crate; the AF_UNIX socket carrying these lines is the
// ENTIRE boundary, so this file is a hand-maintained MIRROR of the Rust source of truth, NOT an FFI binding.
//
// Source of truth (mirror these — do not re-derive):
//   * `src/config.rs` — `ConfigView`, `TunablesView`, `AccountView` (the `config-get` read projection,
//     `Config::view`); `SetTunables` (the config-set tunable allow-list, mirrored 1:1 as the ENCODE side).
//   * `src/daemon/socket.rs` — `config_get_reply` (the bare-`ConfigView`-or-`{"error":…}` framing),
//     `ConfigSetRequest` (the strict `deny_unknown_fields` request schema), `ConfigSetAck` /
//     `ConfigSetEffect` / `ConfigSetRejection` (the redacted ack).
//
// SAFETY BOUNDARY (issue #268, load-bearing): the settable surface IS these types. `SetTunables` is a
// 14-scalar allow-list and `labels` is a `[uuid: String]` map — a credential, an `[[account]]` add/remove,
// or any roster STRUCTURE change is UNREPRESENTABLE here, mirroring the daemon's `deny_unknown_fields`
// backstop. This client moves only non-secret tunables + labels; it performs NO credential handling
// (issue #15), exactly like every other `ControlCommandClient` verb.

import Foundation

// MARK: - config-get reply (the read projection)

/// The scalar tunables in a `config-get` `ConfigView` (`src/config.rs` `TunablesView`, issue #268) — the
/// effective values the settings form displays and edits. LITERAL snake_case wire keys (the Rust struct
/// carries no `rename_all`; the field names ARE the wire keys). Every field is required (a concrete scalar,
/// no default), so a missing key is a hard decode error — a drifted daemon degrades loudly.
struct TunablesView: Decodable, Equatable {
    let pollSecs: UInt64
    let exhaustedPollSecs: UInt64
    let nearLimitPollSecs: UInt64
    let cooldownSecs: UInt64
    let targetMaxSessionUsage: UInt8
    let sessionCeiling: UInt8
    let weeklyCeiling: UInt8
    let sessionBlindSwapSecs: UInt64
    let sessionBlindRiskBand: UInt8
    let sessionVelocityHorizonSecs: UInt64
    let sessionVelocityMinProjectAbove: UInt8
    let sessionVelocityEmaAlphaPct: UInt8
    let monitor401N: UInt8
    let monitorRecoveryM: UInt8

    private enum CodingKeys: String, CodingKey {
        case pollSecs = "poll_secs"
        case exhaustedPollSecs = "exhausted_poll_secs"
        case nearLimitPollSecs = "near_limit_poll_secs"
        case cooldownSecs = "cooldown_secs"
        case targetMaxSessionUsage = "target_max_session_usage"
        case sessionCeiling = "session_ceiling"
        case weeklyCeiling = "weekly_ceiling"
        case sessionBlindSwapSecs = "session_blind_swap_secs"
        case sessionBlindRiskBand = "session_blind_risk_band"
        case sessionVelocityHorizonSecs = "session_velocity_horizon_secs"
        case sessionVelocityMinProjectAbove = "session_velocity_min_project_above"
        case sessionVelocityEmaAlphaPct = "session_velocity_ema_alpha_pct"
        case monitor401N = "monitor_401_n"
        case monitorRecoveryM = "monitor_recovery_m"
    }
}

/// One roster account in a `config-get` `ConfigView` (`src/config.rs` `AccountView`, issue #268): its
/// non-secret `account_uuid` (the STABLE label-edit key — a label edit is keyed by uuid, never by the
/// mutable label), its current `label`, and its `enabled` flag. NO credential — the roster holds none
/// (issue #15). All three fields required.
struct AccountView: Decodable, Equatable {
    let accountUuid: String
    let label: String
    let enabled: Bool

    private enum CodingKeys: String, CodingKey {
        case accountUuid = "account_uuid"
        case label
        case enabled
    }
}

/// The `config-get` reply document (`src/config.rs` `ConfigView`, issue #268): the effective scalar
/// tunables the settings UI edits + the roster's non-secret per-account fields. A BARE object on the wire
/// (no `type` tag, no `result` tag) — `config_get_reply` writes `serde_json::to_string(&view)` directly.
struct ConfigView: Decodable, Equatable {
    let tunables: TunablesView
    let accounts: [AccountView]
}

/// The two shapes a `config-get` reply can take (`src/daemon/socket.rs` `config_get_reply`): the full
/// `ConfigView` document, or a redacted `{"error":…}` envelope — `"no config"` (capture the first account
/// via the CLI first), `"config unreadable"` (the file exists but does not parse), or `"encode failed"`.
/// The READ-side sibling of `StatsReply`. `Equatable` so the model's phase can compare.
enum ConfigGetReply: Equatable {
    case ok(ConfigView)
    case error(String)
}

/// The literal `error` reasons `config_get_reply` emits (`src/daemon/socket.rs`), named so the view can match
/// each ACTIONABLE case without an inline string literal — giving the READ side the same single source of
/// truth the config-SET side already has in `ConfigSetRejection`. Both are matched by `SettingsView` for
/// tailored, operator-actionable copy (issue #573). A reason with no remedy to offer (`encode failed`) is
/// deliberately unnamed, and a reworded / new daemon string simply falls through to the view's generic
/// "Configuration unavailable" copy (graceful degradation, never a crash).
enum ConfigGetErrorReason {
    /// No `config.toml` yet — capture the first account with the CLI.
    static let noConfig = "no config"
    /// `config.toml` exists but does not parse — the daemon left it untouched.
    static let unreadable = "config unreadable"
}

/// The `error`-key probe for BOTH config replies: `config_get_reply` and the `config-set` serve path each
/// write ONLY `{"error":…}` (config-set optionally `+ "detail"`, issue #628) on their redacted failure
/// paths, so an object carrying `error` is the error envelope; any other object is the full `ConfigView` /
/// `ConfigSetAck`. `error` is optional, so a valid document (no top-level `error` key) probes to `nil` and
/// falls through to the full decode. `detail` is the additive #628 message (the config-set path's stale-key
/// naming — issue #645); it is absent on the `config-get` envelopes, so `decodeConfigGetReply` ignores it.
private struct ConfigErrorProbe: Decodable {
    let error: String?
    let detail: String?
}

/// Decode one `config-get` reply line into `.ok(ConfigView)` or `.error(reason)` — the probe-then-decode
/// shape `decodeStatsReply` / `parseWatchFrame` use. Probes the `error` key FIRST (the sole key on the
/// daemon's error path), then decodes the full document. THROWS on a non-JSON line or a well-formed-but-off-
/// contract document (a missing required field) — a drifted daemon degrades loudly. Pure: no I/O, no clock.
func decodeConfigGetReply(_ line: String) throws -> ConfigGetReply {
    let data = Data(line.utf8)
    let decoder = JSONDecoder()
    if let reason = (try? decoder.decode(ConfigErrorProbe.self, from: data))?.error {
        return .error(reason)
    }
    return .ok(try decoder.decode(ConfigView.self, from: data))
}

// MARK: - config-get request

/// The `config-get` control-command request (issue #268 wire): `{"cmd":"config-get"}`. A non-secret READ,
/// so — like `stats` — it is un-auth-gated on the daemon; the reply is the `ConfigGetReply` above.
struct ConfigGetCommand: Encodable, Sendable {
    let cmd = "config-get"
}

// MARK: - config-set request (the write allow-list — the ENCODE side)

/// The scalar `[tunables]` edits a `config-set` may carry (`src/config.rs` `SetTunables`, issue #268),
/// mirrored 1:1 as the ENCODE side. Every field is `Optional` — an omitted key is an UNEDITED key, and
/// Swift's synthesized `Encodable` emits `encodeIfPresent`, so a `nil` field is OMITTED from the wire
/// object (exactly what the daemon's `#[serde(default)]` per-field allow-list expects). This type IS the
/// settable allow-list: the roster structure and every credential are unrepresentable (issue #268 safety
/// boundary, by construction not convention). Snake_case wire keys mirror `SetTunables`.
struct SetTunables: Encodable, Equatable {
    var pollSecs: Int64?
    var exhaustedPollSecs: Int64?
    var nearLimitPollSecs: Int64?
    var cooldownSecs: Int64?
    var targetMaxSessionUsage: Int64?
    var sessionCeiling: Int64?
    var weeklyCeiling: Int64?
    var sessionBlindSwapSecs: Int64?
    var sessionBlindRiskBand: Int64?
    var sessionVelocityHorizonSecs: Int64?
    var sessionVelocityMinProjectAbove: Int64?
    var sessionVelocityEmaAlphaPct: Int64?
    var monitor401N: Int64?
    var monitorRecoveryM: Int64?

    private enum CodingKeys: String, CodingKey {
        case pollSecs = "poll_secs"
        case exhaustedPollSecs = "exhausted_poll_secs"
        case nearLimitPollSecs = "near_limit_poll_secs"
        case cooldownSecs = "cooldown_secs"
        case targetMaxSessionUsage = "target_max_session_usage"
        case sessionCeiling = "session_ceiling"
        case weeklyCeiling = "weekly_ceiling"
        case sessionBlindSwapSecs = "session_blind_swap_secs"
        case sessionBlindRiskBand = "session_blind_risk_band"
        case sessionVelocityHorizonSecs = "session_velocity_horizon_secs"
        case sessionVelocityMinProjectAbove = "session_velocity_min_project_above"
        case sessionVelocityEmaAlphaPct = "session_velocity_ema_alpha_pct"
        case monitor401N = "monitor_401_n"
        case monitorRecoveryM = "monitor_recovery_m"
    }

    /// Whether NO tunable is edited (every field `nil`) — the "labels-only or no-op" fast path the model
    /// uses to decide the expected effect and to keep an all-empty submit honest.
    var isEmpty: Bool {
        pollSecs == nil && exhaustedPollSecs == nil && nearLimitPollSecs == nil && cooldownSecs == nil
            && targetMaxSessionUsage == nil && sessionCeiling == nil && weeklyCeiling == nil
            && sessionBlindSwapSecs == nil && sessionBlindRiskBand == nil
            && sessionVelocityHorizonSecs == nil && sessionVelocityMinProjectAbove == nil
            && sessionVelocityEmaAlphaPct == nil && monitor401N == nil && monitorRecoveryM == nil
    }
}

/// The `config-set` control-command request (`src/daemon/socket.rs` `ConfigSetRequest`, issue #268):
/// `{"cmd":"config-set","tunables":{…edited…},"labels":{"<account_uuid>":"<label>"}}`. Both sub-objects
/// are always present (the daemon defaults them, and an empty one is a valid no-op); `tunables` carries
/// only edited keys (nils omitted, above), `labels` maps each edited account's uuid → its new label.
struct ConfigSetCommand: Encodable {
    let cmd = "config-set"
    let tunables: SetTunables
    let labels: [String: String]
}

// MARK: - config-set reply (ConfigSetAck)

/// What a successful `config-set` requires for its edits to take effect (`src/daemon/socket.rs`
/// `ConfigSetEffect`, issue #268) — the `effect` the UI renders. snake_case wire; an UNKNOWN value is a
/// hard decode error (a drifted daemon degrades loudly, mirroring serde's unknown-variant rejection).
enum ConfigSetEffect: String, Decodable, Equatable {
    /// Only label(s) changed — adopted LIVE (the daemon reconciled its roster in-process); no restart.
    case live
    /// A tunable changed — persisted, effective on the NEXT daemon start (no hot-reload). Restart hint.
    case restartRequired = "restart_required"
    /// The submitted values equalled the current ones — nothing was written.
    case unchanged
}

/// Why the daemon refused a `config-set` (`src/daemon/socket.rs` `ConfigSetRejection`, issue #268) — a
/// redacted, stable machine code. kebab-case wire; an UNKNOWN value is a hard decode error.
enum ConfigSetRejection: String, Decodable, Equatable {
    /// A tunable was out of range, or a cross-field rule failed (e.g. `exhausted_poll_secs < poll_secs`).
    /// The ack's `detail` names the offending field.
    case invalid
    /// A label edit named an `account_uuid` matching no roster account (a stale client).
    case unknownAccount = "unknown-account"
    /// `config.toml` does not exist — capture the first account via the CLI first.
    case noConfig = "no-config"
    /// `config.toml` exists but could not be read / parsed — refused rather than overwrite it.
    case configUnreadable = "config-unreadable"
    /// The validated config could not be persisted (an atomic-write failure) — the OLD file is intact.
    case saveFailed = "save-failed"
    /// The daemon has no wired config path — config-set is unavailable.
    case unavailable
}

/// The redacted `config-set` acknowledgement (`src/daemon/socket.rs` `ConfigSetAck`, issue #268),
/// internally tagged on `result` (mirroring `SwapAck`): `.applied(effect)` (validated + persisted, or a
/// no-op) or `.rejected(reason, detail)` (ZERO writes; `detail` carries the non-secret validation message
/// for the `invalid` reason, absent otherwise). Non-secret by construction (issue #15).
enum ConfigSetAck: Equatable {
    case applied(effect: ConfigSetEffect)
    case rejected(reason: ConfigSetRejection, detail: String?)
}

extension ConfigSetAck: Decodable {
    private enum CodingKeys: String, CodingKey {
        case result
        case effect
        case reason
        case detail
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        let result = try c.decode(String.self, forKey: .result)
        switch result {
        case "applied":
            self = .applied(effect: try c.decode(ConfigSetEffect.self, forKey: .effect))
        case "rejected":
            self = .rejected(
                reason: try c.decode(ConfigSetRejection.self, forKey: .reason),
                detail: try c.decodeIfPresent(String.self, forKey: .detail)
            )
        default:
            throw DecodingError.dataCorruptedError(
                forKey: .result,
                in: c,
                debugDescription: "unknown config-set result '\(result)' — an incompatible wire contract"
            )
        }
    }
}

/// Decode one `config-set` ack line into a `ConfigSetAck`. THROWS on a non-JSON line or a well-formed-but-
/// off-contract ack (an unknown `result` / `effect` / `reason`, a missing required field) — a drifted
/// daemon degrades loudly, exactly like the `watch` / `stats` decoders. Pure: no I/O, no clock.
func decodeConfigSetAck(_ line: String) throws -> ConfigSetAck {
    try JSONDecoder().decode(ConfigSetAck.self, from: Data(line.utf8))
}

// MARK: - config-set reply (ConfigSetAck OR a redacted error envelope)

/// The two shapes a `config-set` reply can take (`src/daemon/socket.rs`) — the write-side sibling of
/// `ConfigGetReply`: the internally-`result`-tagged `ConfigSetAck` (the run loop's redacted outcome), or a
/// redacted `{"error":…,"detail":…}` envelope the `serve_control` path writes BEFORE the run loop reaches
/// `&mut Daemon` — an `unauthorized` peer, or a `malformed request` whose strict `deny_unknown_fields`
/// re-parse rejected a renamed/stale tunable from a version-skewed client (issue #628 threads serde's
/// key-naming message into `detail`; e.g. a pre-#606 menubar sending `session_trigger`). `Equatable` so the
/// model + tests can compare.
enum ConfigSetReply: Equatable {
    case ack(ConfigSetAck)
    case error(reason: String, detail: String?)
}

/// Decode one `config-set` reply line into `.ack(ConfigSetAck)` or `.error(reason, detail)` — the
/// probe-then-decode shape `decodeConfigGetReply` uses. Probes the `error` key FIRST (the daemon's redacted
/// envelope carries it; a `ConfigSetAck` is tagged on `result` and never carries `error`, so the `rejected`
/// ack's OWN `detail` key can't false-positive), so a version-skew rejection surfaces the offending key
/// (issue #645) instead of collapsing to `.undecodable` the way the missing-`result` decode used to. THROWS
/// on a non-JSON line or an off-contract ack (an unknown `result` / `effect` / `reason`) — a drifted daemon
/// still degrades loudly, exactly like `decodeConfigSetAck`. Pure: no I/O, no clock.
func decodeConfigSetReply(_ line: String) throws -> ConfigSetReply {
    let data = Data(line.utf8)
    if let probe = try? JSONDecoder().decode(ConfigErrorProbe.self, from: data), let reason = probe.error {
        return .error(reason: reason, detail: probe.detail)
    }
    return .ack(try decodeConfigSetAck(line))
}
