// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// The client-side decoder for the daemon's redacted `capture` control-command ack (issues #358 / #360) —
// the Swift mirror of the `capture` reply the daemon returns (issue #359). The short-lived
// `ControlCommandClient` (#358) hands back a raw redacted ack LINE; this turns a `capture` verb's line
// into a typed, non-secret verdict for the in-app capture affordance (#360) to render. Its sibling is
// `SwapAck` (#167) — the two share the ONE control-command transport and the SAME probe-then-decode shape.
//
// CONTRACT PROVENANCE — read before editing. As of #360 the daemon `capture` control command (#359) is
// NOT yet wired on the socket: its branch carries the capture ENGINE (`src/capture.rs`) but does not yet
// extend `src/daemon/socket.rs`, so this ack's wire shape is DEFINED here — from the issue #359 spec,
// which directs it to "mirror the existing manual-swap command (#167) 1:1". It is therefore the 1:1
// analogue of `SwapAck`'s shape: a `result`-tagged success/refusal plus the shared `error` ack. Keep it
// in lockstep with `src/daemon/socket.rs` once #359 lands — a HAND-MAINTAINED mirror, not an FFI binding
// (ADR-0010: the app links no Rust; the AF_UNIX socket is the entire boundary).
//
// REDACTED by construction (issue #15): a capture ack carries only the operator LABEL the account was
// captured under (the daemon-derived UUID handle when the request omitted a label — never the email) plus
// a non-secret roster COUNT. NEVER a token, email, or oauth blob — this decoder models no credential field.

import Foundation

/// The daemon's redacted acknowledgement for a `capture` control command (issue #359), internally tagged
/// on `result` — the 1:1 mirror of `SwapAck`'s shape (#167). Non-secret by construction (issue #15).
enum CaptureAck: Equatable {
    /// The active account was CAPTURED into the roster under `label` — the daemon-derived UUID handle when
    /// the request omitted one, so the affordance echoes the label the daemon actually assigned. `accounts`
    /// is the post-capture roster count when the daemon includes it. An already-rostered active account is
    /// an idempotent refresh — still `.captured`, no duplicate row.
    case captured(label: String, accounts: Int?)
    /// The daemon REFUSED with a redacted machine reason — ZERO roster writes.
    case rejected(CaptureRejection)
    /// The shared redacted error ack (`{"error":"<reason>"}`): `unauthorized` / `malformed request` /
    /// `unknown command` / etc. Not a `capture`-specific `result`, but modelled so a stray error line
    /// decodes to a case rather than throwing. Carries the raw redacted reason string.
    case error(String)
}

/// Why the daemon refused a `capture` (issue #359 spec: "failure = a bare machine error tag") — a redacted,
/// stable machine code, serialized kebab-case. These are the four tags #359 enumerates; each already
/// exists in the sibling `SwapRejection`, so the daemon can share the reason vocabulary. Keep in lockstep
/// with the Rust enum once #359 lands.
enum CaptureRejection: String, Equatable {
    /// No active account to capture — nothing is logged into Claude Code. The operator runs `claude /login`
    /// first, then captures (the honest scope boundary: capture snapshots the ACTIVE account, it is not a
    /// picker).
    case noActiveAccount = "no-active-account"
    /// The keychain is LOCKED — a SAFETY abort. ZERO writes.
    case keychainLocked = "keychain-locked"
    /// The single-writer swap lock (#357) stayed held the whole bounded wait — fail-closed. ZERO writes.
    case swapLockBusy = "swap-lock-busy"
    /// The capture engine aborted for another reason (an I/O error, a read-back mismatch). ZERO writes.
    case failed
}

extension CaptureAck {
    /// A decode failure for a redacted ack line. Non-secret: the line carries no credential, so echoing a
    /// short reason is safe.
    enum DecodeError: Error, Equatable {
        /// The line was not valid JSON.
        case notJSON
        /// The line was valid JSON but matched no known ack shape (neither `result` nor `error`), or
        /// carried an unknown `result` / `reason` value this client does not model — a hard error, so a
        /// caller degrades loudly rather than mis-reading an incompatible wire contract.
        case unrecognized(String)
    }

    /// Decode one redacted `capture` ack line into a typed verdict. Probes for the `result` tag FIRST (the
    /// same probe-then-decode shape `SwapAck` / `parseWatchFrame` use), then the shared `error` ack;
    /// anything else is a hard `unrecognized` error. Pure: no I/O, no clock. Models NO credential field
    /// (redaction, issue #15).
    static func decode(_ line: String) throws -> CaptureAck {
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
            case "captured":
                // `label` is load-bearing (the affordance renders "Captured '<label>'"), so a `captured`
                // ack that omits it is malformed → the decoder's OWN `.unrecognized`, never a raw
                // `DecodingError`. `accounts` is an informational count the UI does not render, so it is
                // decoded when present and TOLERATED when absent (`Int?`) — a narrower exact-match burden
                // on the not-yet-built #359, and the label alone is what the panel needs.
                guard let body = try? decoder.decode(CapturedBody.self, from: data) else {
                    throw DecodeError.unrecognized("malformed 'captured' ack (missing label)")
                }
                return .captured(label: body.label, accounts: body.accounts)
            case "rejected":
                guard let body = try? decoder.decode(RejectedBody.self, from: data) else {
                    throw DecodeError.unrecognized("malformed 'rejected' ack (missing reason)")
                }
                guard let rejection = CaptureRejection(rawValue: body.reason) else {
                    throw DecodeError.unrecognized("unknown capture rejection '\(body.reason)'")
                }
                return .rejected(rejection)
            default:
                throw DecodeError.unrecognized("unknown capture result '\(result)'")
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

/// `{"result":"captured","label":"<label>","accounts":<n>}` — the label the account was captured under
/// (required) plus an optional post-capture roster count. Labels / counts only, no credential.
private struct CapturedBody: Decodable {
    let label: String
    let accounts: Int?
}

/// `{"result":"rejected","reason":"<kebab-case>"}` — the redacted machine reason, mapped to
/// `CaptureRejection`.
private struct RejectedBody: Decodable {
    let reason: String
}
