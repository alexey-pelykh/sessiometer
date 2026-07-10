// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// The client-side decoder for the daemon's redacted `swap` control-command ack (issue #358) — the
// Swift mirror of `SwapAck` / `SwapRejection` in `src/daemon/socket.rs` (issue #167). The short-lived
// `ControlCommandClient` (#358) hands back a raw redacted ack LINE; this turns a `swap` verb's line
// into a typed, non-secret verdict for the swap-on-click call site (#169) to route on.
//
// REDACTED by construction (issue #15): a swap ack carries only a machine `result` tag plus, for a
// committed swap, the two non-secret roster LABELS (`from` / `to`) — NEVER a token, NEVER an email. So
// this decoder models NO credential field. It ALSO recognises the shared redacted `{"error":…}` ack
// (e.g. `{"error":"unauthorized"}`) — the same-user local peer is authorised by construction and
// should never actually see it, but modelling it means a stray error line decodes to a case instead of
// throwing, and the caller can surface it honestly.
//
// Per ADR-0010 the app links no Rust and shares no build graph — the AF_UNIX socket is the ENTIRE
// boundary — so this is a HAND-MAINTAINED mirror of the Rust wire types, not an FFI binding. Keep the
// `result` tag set and the `reason` code set in lockstep with `src/daemon/socket.rs`.

import Foundation

/// The daemon's redacted acknowledgement for a `swap` control command (`src/daemon/socket.rs`
/// `SwapAck`, issue #167), internally tagged on `result`. Non-secret by construction (issue #15).
enum SwapAck: Equatable {
    /// The swap COMMITTED: the active credential was rerouted OFF `from` ONTO `to` — both non-secret
    /// roster labels.
    case accepted(from: String, to: String)
    /// A no-op success: `to` was ALREADY the active account, so nothing was written. Label only.
    case alreadyActive(to: String)
    /// The daemon REFUSED with a redacted machine reason — ZERO writes happened.
    case rejected(SwapRejection)
    /// The shared redacted error ack (`{"error":"<reason>"}`): `unauthorized` / `malformed request` /
    /// `unknown command` / etc. Not a `swap`-specific `result`, but modelled so a stray error line
    /// decodes to a case rather than throwing. Carries the raw redacted reason string.
    case error(String)
}

/// Why the daemon refused a `swap` (`src/daemon/socket.rs` `SwapRejection`, issue #167) — a redacted,
/// stable machine code, serialized kebab-case. Keep in lockstep with the Rust enum.
enum SwapRejection: String, Equatable {
    /// The target handle matched no roster account.
    case unknownTarget = "unknown-target"
    /// The target handle matched more than one account (a duplicated label) — the daemon never guesses.
    case ambiguousTarget = "ambiguous-target"
    /// The target is quarantined (a dead credential) — refused WITHOUT `force`.
    case quarantined
    /// The target's weekly window is exhausted — refused WITHOUT `force`.
    case weeklyExhausted = "weekly-exhausted"
    /// A post-swap cooldown is active — refused WITHOUT `force`.
    case cooldown
    /// No active account to swap AWAY from (or its canonical credential is gone).
    case noActiveAccount = "no-active-account"
    /// The keychain is LOCKED — a SAFETY abort the engine makes even under `force`. ZERO writes.
    case keychainLocked = "keychain-locked"
    /// The single-writer swap lock stayed held the whole bounded wait — fail-closed. ZERO writes.
    case swapLockBusy = "swap-lock-busy"
    /// The swap engine aborted for another reason (a wrong-identity guard, an I/O error). ZERO writes.
    case failed
}

extension SwapAck {
    /// A decode failure for a redacted ack line. Non-secret: the line carries no credential, so echoing
    /// a short reason is safe.
    enum DecodeError: Error, Equatable {
        /// The line was not valid JSON.
        case notJSON
        /// The line was valid JSON but matched no known ack shape (neither `result` nor `error`), or
        /// carried an unknown `result` / `reason` value this client does not model — a hard error, so a
        /// caller degrades loudly rather than mis-reading an incompatible wire contract (mirroring
        /// serde's unknown-variant rejection).
        case unrecognized(String)
    }

    /// Decode one redacted `swap` ack line into a typed verdict. Probes for the `result` tag FIRST (the
    /// same probe-then-decode shape `parseWatchFrame` uses), then the shared `error` ack; anything else
    /// is a hard `unrecognized` error. Pure: no I/O, no clock. Models NO credential field (redaction,
    /// issue #15).
    static func decode(_ line: String) throws -> SwapAck {
        let data = Data(line.utf8)
        let decoder = JSONDecoder()

        let probe: AckProbe
        do {
            probe = try decoder.decode(AckProbe.self, from: data)
        } catch {
            throw DecodeError.notJSON
        }

        if let result = probe.result {
            switch result {
            case "accepted":
                // A recognized-but-incomplete body (a buggy daemon / wire drift) maps to the decoder's
                // OWN `unrecognized` taxonomy, never a raw `DecodingError` — the caller catches one
                // error type. `already_active` / `rejected` below do the same.
                guard let body = try? decoder.decode(AcceptedBody.self, from: data) else {
                    throw DecodeError.unrecognized("malformed 'accepted' ack (missing from/to)")
                }
                return .accepted(from: body.from, to: body.to)
            case "already_active":
                guard let body = try? decoder.decode(AlreadyActiveBody.self, from: data) else {
                    throw DecodeError.unrecognized("malformed 'already_active' ack (missing to)")
                }
                return .alreadyActive(to: body.to)
            case "rejected":
                guard let body = try? decoder.decode(RejectedBody.self, from: data) else {
                    throw DecodeError.unrecognized("malformed 'rejected' ack (missing reason)")
                }
                guard let rejection = SwapRejection(rawValue: body.reason) else {
                    throw DecodeError.unrecognized("unknown swap rejection '\(body.reason)'")
                }
                return .rejected(rejection)
            default:
                throw DecodeError.unrecognized("unknown swap result '\(result)'")
            }
        }
        if let error = probe.error {
            return .error(error)
        }
        throw DecodeError.unrecognized("ack line has neither a 'result' nor an 'error'")
    }
}

// MARK: - Wire decode shapes (private mirrors of the daemon reply objects)

/// The `result` / `error` tag probe: both optional, so a line with neither classifies as `unrecognized`
/// rather than failing the probe decode. Unknown sibling keys (a future minor field) are ignored.
private struct AckProbe: Decodable {
    let result: String?
    let error: String?
}

/// `{"result":"accepted","from":"<label>","to":"<label>"}` — labels only, no credential.
private struct AcceptedBody: Decodable {
    let from: String
    let to: String
}

/// `{"result":"already_active","to":"<label>"}` — label only.
private struct AlreadyActiveBody: Decodable {
    let to: String
}

/// `{"result":"rejected","reason":"<kebab-case>"}` — the redacted machine reason, mapped to
/// `SwapRejection`.
private struct RejectedBody: Decodable {
    let reason: String
}
