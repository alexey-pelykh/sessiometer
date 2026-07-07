// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

// Golden `watch`-frame fixtures for the wire decoder tests (issue #322).
//
// The snapshot / heartbeat fixtures are BYTE-EXACT daemon encoder output, produced by
// serializing the daemon's own wire types through `encode_snapshot_frame` /
// `encode_heartbeat_frame` (`src/daemon/socket.rs`) with the pinned `serde_json`.
// `snapshotBasic` and `heartbeatBasic` reproduce the exact frames the Rust `parse_watch_frame`
// test decodes (`src/daemon.rs` `parse_watch_frame_classifies_each_frame_kind`:
// `watch_snapshot("work", 42, 0.60)` and `encode_heartbeat_frame(42)`); the other
// current-daemon fixtures are the same encoder's output, constructed to cover states the named
// test frames do not. The backward/forward-compat + rejected fixtures (auth-null, pre-freeze,
// unsupported-major, unknown-variant, missing-required-field) are hand-built to the same
// contract to exercise cases the CURRENT daemon never emits but the #164 additive contract
// requires a client to tolerate (or, for a breaking major / a malformed body, to flag / reject).
//
// NOTE: byte-fidelity of the `snapshotBasic` / `heartbeatBasic` current-daemon fixtures is a
// HAND-MAINTAINED mirror — ADR-0010 keeps Rust out of the Swift build, so a future daemon wire
// change could desync them while the semantic decoder tests (which assert values order-
// independently) stay green. That drift is now GUARDED (issue #340): the Rust crate emits a
// committed golden by serializing its own wire encoders (`build/fixtures/wire-*.json`, emitted +
// byte-equality-pinned in `src/daemon.rs`), and `WireGoldenTests.swift` asserts these two fixtures
// are byte-identical to it — so a daemon wire change not mirrored here fails CI (the `swift` job's
// path filter covers `build/fixtures/**`, so a golden regeneration re-runs the check). The
// backward/forward-compat + rejected fixtures below are hand-built to states the current daemon
// never emits, so they have no golden and are intentionally outside that guard.
//
// Kept in a dedicated file with no `XCTest` dependency so the fixtures are one source of truth
// shared by the XCTest suite (`WireDecoderTests.swift`, under `xcodebuild test`) and any plain
// verifier. They are inline constants, not bundle resources: the decoder is pure `JSONDecoder`,
// so there is no resource-bundling surface to differ between build systems.

enum Fixtures {
    // ---- Byte-exact daemon encoder output ---------------------------------------------------
    // `snapshotBasic` / `heartbeatBasic` reproduce the exact frames the Rust `parse_watch_frame`
    // test decodes; the rest are the same encoder's output, built for additional state coverage.

    /// `encode_snapshot_frame(&versioned_status_response(&watch_snapshot("work", 42, 0.60)))` —
    /// the canonical frame the Rust `parse_watch_frame` test decodes. One account, session 60,
    /// weekly 10, all-default flags, `auth` = the default `healthy`, `next_swap` null.
    static let snapshotBasic = #"""
    {"type":"snapshot","schema_version":{"major":1,"minor":0},"generated_at":42,"accounts":[{"label":"work","active":true,"enabled":true,"quarantined":false,"recovering":false,"session_pct":60,"weekly_pct":10,"session_resets_at":null,"weekly_resets_at":null,"weekly_exhausted":false,"access_expires_at":null,"refresh_health":null,"auth":"healthy"}],"next_swap":null,"refresh_enabled":false}
    """#

    /// `next_swap` = target, two accounts. Exercises `auth` at_risk + unknown, `refresh_health`
    /// present + null, `session_pct`/resets/expires present + null, `refresh_enabled` true.
    static let snapshotRichTarget = #"""
    {"type":"snapshot","schema_version":{"major":1,"minor":0},"generated_at":1893456000,"accounts":[{"label":"work","active":true,"enabled":true,"quarantined":false,"recovering":false,"session_pct":30,"weekly_pct":20,"session_resets_at":1893460000,"weekly_resets_at":1893800000,"weekly_exhausted":false,"access_expires_at":1893470000,"refresh_health":{"last_ok":true,"rotated":true,"consecutive_failures":0},"auth":"at_risk"},{"label":"personal","active":false,"enabled":true,"quarantined":false,"recovering":false,"session_pct":null,"weekly_pct":null,"session_resets_at":null,"weekly_resets_at":null,"weekly_exhausted":false,"access_expires_at":null,"refresh_health":null,"auth":"unknown"}],"next_swap":{"state":"target","to":"personal"},"refresh_enabled":true}
    """#

    /// `next_swap` = no_viable_target; a weekly-exhausted, stale account with a failure streak.
    static let snapshotNoViable = #"""
    {"type":"snapshot","schema_version":{"major":1,"minor":0},"generated_at":1893456100,"accounts":[{"label":"work","active":true,"enabled":true,"quarantined":false,"recovering":false,"session_pct":95,"weekly_pct":100,"session_resets_at":1893460500,"weekly_resets_at":1893800500,"weekly_exhausted":true,"access_expires_at":1893470500,"refresh_health":{"last_ok":false,"rotated":false,"consecutive_failures":2},"auth":"stale"}],"next_swap":{"state":"no_viable_target"},"refresh_enabled":true}
    """#

    /// `next_swap` = awaiting_data; a quarantined dead account with no usage.
    static let snapshotAwaitingDead = #"""
    {"type":"snapshot","schema_version":{"major":1,"minor":0},"generated_at":1893456200,"accounts":[{"label":"work","active":false,"enabled":true,"quarantined":true,"recovering":false,"session_pct":null,"weekly_pct":null,"session_resets_at":null,"weekly_resets_at":null,"weekly_exhausted":false,"access_expires_at":null,"refresh_health":null,"auth":"dead"}],"next_swap":{"state":"awaiting_data"},"refresh_enabled":false}
    """#

    /// A schema-supported snapshot with ZERO accounts — the real first-run / empty-roster frame the
    /// daemon emits before any account is captured (B-014). Supported major, so it is a DISTINCT
    /// "connected but empty" state, NOT the pre-freeze / unsupported empty snapshots below.
    static let snapshotEmptyRoster = #"""
    {"type":"snapshot","schema_version":{"major":1,"minor":0},"generated_at":100,"accounts":[],"next_swap":null,"refresh_enabled":false}
    """#

    /// `encode_heartbeat_frame(42)` — the canonical beat the Rust test decodes.
    static let heartbeatBasic = #"""
    {"type":"heartbeat","generated_at":42,"schema_version":{"major":1,"minor":0}}
    """#

    // ---- Backward/forward-compat frames (hand-built to the same contract) -------------------

    /// A pre-#119 daemon: `auth` present as null. The client must read it as "no verdict" (nil),
    /// not a defaulted health.
    static let snapshotAuthNull = #"""
    {"type":"snapshot","schema_version":{"major":1,"minor":0},"generated_at":42,"accounts":[{"label":"work","active":true,"enabled":true,"quarantined":false,"recovering":false,"session_pct":60,"weekly_pct":10,"session_resets_at":null,"weekly_resets_at":null,"weekly_exhausted":false,"access_expires_at":null,"refresh_health":null,"auth":null}],"next_swap":null,"refresh_enabled":false}
    """#

    /// A pre-#109/#119 daemon: an account carrying ONLY the required fields. Every additive field
    /// is absent (not null) — the strongest test of the additive-default decode path.
    static let snapshotLegacyMinimal = #"""
    {"type":"snapshot","schema_version":{"major":1,"minor":0},"generated_at":5,"accounts":[{"label":"work","active":true,"enabled":true,"quarantined":false}],"next_swap":null,"refresh_enabled":false}
    """#

    /// A future BREAKING contract (major 2). Decodes (so `generated_at` is still readable) but the
    /// major gate flags it unsupported — never mis-rendered as major-1 data.
    static let snapshotUnsupportedMajor = #"""
    {"type":"snapshot","schema_version":{"major":2,"minor":0},"generated_at":42,"accounts":[],"next_swap":null,"refresh_enabled":false}
    """#

    /// A pre-freeze (pre-#164) daemon: no `schema_version`, no `generated_at`. Defaults to major 0
    /// (→ unsupported) and `generated_at` 0.
    static let snapshotPreFreeze = #"""
    {"type":"snapshot","accounts":[],"next_swap":null,"refresh_enabled":false}
    """#

    /// A pre-freeze heartbeat: no `schema_version` → defaults `{0,0}` → unsupported.
    static let heartbeatPreFreeze = #"""
    {"type":"heartbeat","generated_at":7}
    """#

    /// A forward-compat MINOR frame: minor 5 plus unknown additive keys at both the envelope and
    /// account level. Unknown keys are ignored; major 1 stays supported.
    static let snapshotUnknownAdditiveFields = #"""
    {"type":"snapshot","schema_version":{"major":1,"minor":5},"generated_at":9,"future_top":123,"accounts":[{"label":"work","active":true,"enabled":true,"quarantined":false,"future_field":"x"}],"next_swap":null,"refresh_enabled":false}
    """#

    // ---- Malformed / rejected bodies --------------------------------------------------------

    /// An unknown `next_swap` state — the daemon's internally-tagged enum rejects it, so the
    /// client must too (a hard decode error, not a tolerated unknown).
    static let snapshotUnknownNextSwap = #"""
    {"type":"snapshot","schema_version":{"major":1,"minor":0},"generated_at":1,"accounts":[],"next_swap":{"state":"future_state"},"refresh_enabled":false}
    """#

    /// An unknown `auth` value — rejected (mirrors serde's unknown-variant error).
    static let snapshotUnknownAuth = #"""
    {"type":"snapshot","schema_version":{"major":1,"minor":0},"generated_at":1,"accounts":[{"label":"w","active":true,"enabled":true,"quarantined":false,"auth":"future_health"}],"next_swap":null,"refresh_enabled":false}
    """#

    /// A snapshot account missing the required `label` — a hard decode error.
    static let snapshotMissingLabel = #"""
    {"type":"snapshot","schema_version":{"major":1,"minor":0},"generated_at":1,"accounts":[{"active":true,"enabled":true,"quarantined":false}],"next_swap":null,"refresh_enabled":false}
    """#

    /// A snapshot missing the required `accounts` array (a bare `Vec`, no default) — an error.
    static let snapshotMissingAccounts = #"""
    {"type":"snapshot","schema_version":{"major":1,"minor":0},"generated_at":1,"next_swap":null,"refresh_enabled":false}
    """#

    /// `next_swap` = target but missing the required `to` label — an error.
    static let snapshotTargetMissingTo = #"""
    {"type":"snapshot","schema_version":{"major":1,"minor":0},"generated_at":1,"accounts":[],"next_swap":{"state":"target"},"refresh_enabled":false}
    """#

    /// A `schema_version` object present but missing `minor` (the inner fields carry no default,
    /// unlike the whole-object default) — an error, NOT a `{major,0}` coercion.
    static let snapshotSchemaMissingMinor = #"""
    {"type":"snapshot","schema_version":{"major":1},"generated_at":1,"accounts":[],"next_swap":null,"refresh_enabled":false}
    """#

    /// A heartbeat missing the required `generated_at` — an error.
    static let heartbeatMissingGeneratedAt = #"""
    {"type":"heartbeat","schema_version":{"major":1,"minor":0}}
    """#

    // ---- `type`-probe cases (literals from the Rust test) -----------------------------------

    /// A future frame KIND — ignored, not an error.
    static let unknownFutureType = #"{"type":"future","x":1}"#
    /// A line with no `type` tag — ignored, not an error.
    static let noTypeTag = #"{"nope":1}"#
    /// A line that is not JSON — a hard error.
    static let notJSON = "not json"
}
