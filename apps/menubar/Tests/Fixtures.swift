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
    {"type":"snapshot","schema_version":{"major":1,"minor":8},"generated_at":42,"accounts":[{"label":"work","active":true,"enabled":true,"quarantined":false,"recovering":false,"session_pct":60,"weekly_pct":10,"session_resets_at":null,"weekly_resets_at":null,"weekly_exhausted":false,"access_expires_at":null,"refresh_health":null,"auth":"healthy"}],"next_swap":null,"refresh_enabled":false,"systemic_refresh_failure":null}
    """#

    /// The basic frame but with `next_swap` = target carrying the #393 structured reason
    /// (`soonest_reset` + its `resets_at` epoch) — byte-identical to the Rust
    /// `wire-snapshot-next-swap.json` golden (`WireGoldenTests`), so the reason field is under the
    /// cross-language byte-drift guard (#340). The basic golden's `next_swap` is null, so without
    /// this the `NextSwap.target` `reason` would have NO byte coverage.
    static let snapshotNextSwap = #"""
    {"type":"snapshot","schema_version":{"major":1,"minor":8},"generated_at":42,"accounts":[{"label":"work","active":true,"enabled":true,"quarantined":false,"recovering":false,"session_pct":60,"weekly_pct":10,"session_resets_at":null,"weekly_resets_at":null,"weekly_exhausted":false,"access_expires_at":null,"refresh_health":null,"auth":"healthy"}],"next_swap":{"state":"target","to":"spare","reason":{"kind":"soonest_reset","resets_at":1893800000}},"refresh_enabled":false,"systemic_refresh_failure":null}
    """#

    /// `next_swap` = target with the #393 `roster_order` reason: ≥2 accounts qualified but none
    /// reported a weekly reset, so no soonest-reset tiebreak existed and the earliest roster index
    /// won. Hand-built to the contract (the byte-pinned golden above carries `soonest_reset`) — it
    /// pins that the client decodes each KNOWN tag the daemon emits to its own case, which one golden
    /// cannot cover. (An UNKNOWN `kind`, by contrast, degrades to `reason: nil` — issue #412,
    /// `snapshotUnknownReasonKind` — rather than failing the frame.)
    static let snapshotRosterOrderTarget = #"""
    {"type":"snapshot","schema_version":{"major":1,"minor":8},"generated_at":42,"accounts":[{"label":"work","active":true,"enabled":true,"quarantined":false,"recovering":false,"session_pct":60,"weekly_pct":10,"session_resets_at":null,"weekly_resets_at":null,"weekly_exhausted":false,"access_expires_at":null,"refresh_health":null,"auth":"healthy"}],"next_swap":{"state":"target","to":"spare","reason":{"kind":"roster_order"}},"refresh_enabled":false,"systemic_refresh_failure":null}
    """#

    /// `next_swap` = target with the #393 `only_candidate` reason (personal is the lone viable
    /// spare — work is active), two accounts. Exercises `auth` at_risk + unknown, `refresh_health`
    /// present + null, `session_pct`/resets/expires present + null, `refresh_enabled` true.
    static let snapshotRichTarget = #"""
    {"type":"snapshot","schema_version":{"major":1,"minor":8},"generated_at":1893456000,"accounts":[{"label":"work","active":true,"enabled":true,"quarantined":false,"recovering":false,"session_pct":30,"weekly_pct":20,"session_resets_at":1893460000,"weekly_resets_at":1893800000,"weekly_exhausted":false,"access_expires_at":1893470000,"refresh_health":{"last_ok":true,"rotated":true,"consecutive_failures":0},"auth":"at_risk"},{"label":"personal","active":false,"enabled":true,"quarantined":false,"recovering":false,"session_pct":null,"weekly_pct":null,"session_resets_at":null,"weekly_resets_at":null,"weekly_exhausted":false,"access_expires_at":null,"refresh_health":null,"auth":"unknown"}],"next_swap":{"state":"target","to":"personal","reason":{"kind":"only_candidate"}},"refresh_enabled":true,"systemic_refresh_failure":null}
    """#

    /// `next_swap` = no_viable_target carrying the #405 fleet-capacity RELIEF: a weekly-exhausted,
    /// stale account with a failure streak → `cause` = `weekly`, `resets_at` = the soonest weekly
    /// reset (this lone account's `weekly_resets_at`). The renderer then reads "Out of capacity …
    /// resets in ⟨dur⟩ · add an account" rather than a content-free "no viable target".
    static let snapshotNoViable = #"""
    {"type":"snapshot","schema_version":{"major":1,"minor":8},"generated_at":1893456100,"accounts":[{"label":"work","active":true,"enabled":true,"quarantined":false,"recovering":false,"session_pct":95,"weekly_pct":100,"session_resets_at":1893460500,"weekly_resets_at":1893800500,"weekly_exhausted":true,"access_expires_at":1893470500,"refresh_health":{"last_ok":false,"rotated":false,"consecutive_failures":2},"auth":"stale"}],"next_swap":{"state":"no_viable_target","cause":"weekly","resets_at":1893800500},"refresh_enabled":true,"systemic_refresh_failure":null}
    """#

    /// A pre-#405 daemon (minor 2): `next_swap` = no_viable_target WITHOUT the `cause`/`resets_at`
    /// relief keys. Both were additive in 1.3, so an older daemon emits a bare no-viable-target — it
    /// must decode to `cause: nil, resetsAt: nil` (the `decodeIfPresent` forward-compat path), NOT a
    /// decode error. Freezes the additive contract that makes the #405 relief render-safe against an
    /// older daemon (mirrors `snapshotTargetNoReason` for #393).
    static let snapshotNoViableNoRelief = #"""
    {"type":"snapshot","schema_version":{"major":1,"minor":2},"generated_at":1893456100,"accounts":[{"label":"work","active":true,"enabled":true,"quarantined":false,"recovering":false,"session_pct":95,"weekly_pct":100,"session_resets_at":1893460500,"weekly_resets_at":1893800500,"weekly_exhausted":true,"access_expires_at":1893470500,"refresh_health":{"last_ok":false,"rotated":false,"consecutive_failures":2},"auth":"stale"}],"next_swap":{"state":"no_viable_target"},"refresh_enabled":true,"systemic_refresh_failure":null}
    """#

    /// `next_swap` = awaiting_data; a quarantined dead account with no usage.
    static let snapshotAwaitingDead = #"""
    {"type":"snapshot","schema_version":{"major":1,"minor":8},"generated_at":1893456200,"accounts":[{"label":"work","active":false,"enabled":true,"quarantined":true,"recovering":false,"session_pct":null,"weekly_pct":null,"session_resets_at":null,"weekly_resets_at":null,"weekly_exhausted":false,"access_expires_at":null,"refresh_health":null,"auth":"dead"}],"next_swap":{"state":"awaiting_data"},"refresh_enabled":false,"systemic_refresh_failure":null}
    """#

    /// A quarantined-but-refreshable account carrying the NON-TERMINAL `"auth":"degraded"` verdict
    /// (issue #427): the wire's new rollup token the daemon emits for a bare access-token 401-streak.
    /// The client MUST decode it (a value it cannot read is a hard decode error — a menubar that
    /// rejected `degraded` would blank on exactly the account this fix exists to render honestly).
    static let snapshotDegraded = #"""
    {"type":"snapshot","schema_version":{"major":1,"minor":8},"generated_at":1893456300,"accounts":[{"label":"work","active":false,"enabled":true,"quarantined":true,"recovering":false,"session_pct":null,"weekly_pct":null,"session_resets_at":null,"weekly_resets_at":null,"weekly_exhausted":false,"access_expires_at":null,"refresh_health":null,"auth":"degraded"}],"next_swap":{"state":"awaiting_data"},"refresh_enabled":false,"systemic_refresh_failure":null}
    """#

    /// A schema-supported snapshot with ZERO accounts — the real first-run / empty-roster frame the
    /// daemon emits before any account is captured (B-014). Supported major, so it is a DISTINCT
    /// "connected but empty" state, NOT the pre-freeze / unsupported empty snapshots below.
    static let snapshotEmptyRoster = #"""
    {"type":"snapshot","schema_version":{"major":1,"minor":8},"generated_at":100,"accounts":[],"next_swap":null,"refresh_enabled":false,"systemic_refresh_failure":null}
    """#

    /// The daemon-level `canonical_scrub` = `exhausted` rollup (issue #516): the shared canonical is
    /// scrubbed AND recovery-exhausted — the un-recoverable residual #469 renders with the
    /// `claude /login` remedy. Byte-identical to the Rust `wire-snapshot-canonical-scrub.json` golden
    /// (`WireGoldenTests`), so the `{"state":"exhausted"}` discriminant is under the cross-language
    /// byte-drift guard (#340) — the basic golden omits `canonical_scrub` (healthy), so without this
    /// the rollup would have NO byte coverage. Built as the basic frame with the scrub field set.
    static let snapshotCanonicalScrubExhausted = #"""
    {"type":"snapshot","schema_version":{"major":1,"minor":8},"generated_at":42,"accounts":[{"label":"work","active":true,"enabled":true,"quarantined":false,"recovering":false,"session_pct":60,"weekly_pct":10,"session_resets_at":null,"weekly_resets_at":null,"weekly_exhausted":false,"access_expires_at":null,"refresh_health":null,"auth":"healthy"}],"next_swap":null,"refresh_enabled":false,"systemic_refresh_failure":null,"canonical_scrub":{"state":"exhausted"}}
    """#

    /// The `canonical_scrub` = `recovering` rollup (issue #516): the canonical is scrubbed but the
    /// daemon's autonomous adopt-recovery is still in progress (the self-may-heal state). Hand-built to
    /// the current contract — the byte-pinned golden above carries `exhausted`, so this pins that the
    /// client decodes the OTHER known `state` the daemon emits to its own case, which one golden cannot
    /// cover (mirrors `snapshotRosterOrderTarget` for the #393 reason variants).
    static let snapshotCanonicalScrubRecovering = #"""
    {"type":"snapshot","schema_version":{"major":1,"minor":8},"generated_at":42,"accounts":[{"label":"work","active":true,"enabled":true,"quarantined":false,"recovering":false,"session_pct":60,"weekly_pct":10,"session_resets_at":null,"weekly_resets_at":null,"weekly_exhausted":false,"access_expires_at":null,"refresh_health":null,"auth":"healthy"}],"next_swap":null,"refresh_enabled":false,"systemic_refresh_failure":null,"canonical_scrub":{"state":"recovering"}}
    """#

    /// The daemon-level `keychain_locked` = `true` flag (issue #498): the macOS login keychain is
    /// LOCKED, so the daemon cannot READ the shared credential item at all (access denied) — the
    /// fleet-wide unreadable-credential lockout no per-account `auth` reflects, DISTINCT from
    /// `canonical_scrub` (a readable-but-scrubbed item; here the item is simply unreadable, so the
    /// operator remedy is "unlock the keychain", not `claude /login`). NOT byte-pinned to a Rust
    /// golden — the goldens cover the healthy UNLOCKED frame, which omits the flag
    /// (`skip_serializing_if`), so a non-locked frame is byte-unchanged; this hand-built frame gives
    /// the `"keychain_locked":true` discriminant decode coverage (mirrors
    /// `snapshotCanonicalScrubRecovering`). Built as the basic frame with the flag set.
    static let snapshotKeychainLocked = #"""
    {"type":"snapshot","schema_version":{"major":1,"minor":8},"generated_at":42,"accounts":[{"label":"work","active":true,"enabled":true,"quarantined":false,"recovering":false,"session_pct":60,"weekly_pct":10,"session_resets_at":null,"weekly_resets_at":null,"weekly_exhausted":false,"access_expires_at":null,"refresh_health":null,"auth":"healthy"}],"next_swap":null,"refresh_enabled":false,"systemic_refresh_failure":null,"keychain_locked":true}
    """#

    /// The daemon-level `systemic_refresh_failure` count (issue #378): the refresh MECHANISM is down — 3
    /// consecutive sweeps in which EVERY eligible account's cycle failed `outcome=error` (a stale pinned
    /// `claude` path #375, a wedged spawn), not one account's credentials. The THIRD daemon-level payload
    /// fault (#520 glyph-half / #523 panel-half), and the only one visible BEFORE any account dies — note
    /// the roster here reads perfectly HEALTHY (`auth":"healthy"`, a live `session_pct`), which is exactly
    /// the false-healthy state the banner + `!` glance exist to contradict. NOT byte-pinned to a Rust golden
    /// — the goldens cover the healthy frame, which carries `"systemic_refresh_failure":null`, so this
    /// hand-built frame gives the non-null COUNT decode coverage (mirrors `snapshotKeychainLocked` /
    /// `snapshotCanonicalScrubRecovering`). Carries only the count, never a token or path (issue #15).
    static let snapshotSystemicRefreshFailure = #"""
    {"type":"snapshot","schema_version":{"major":1,"minor":8},"generated_at":42,"accounts":[{"label":"work","active":true,"enabled":true,"quarantined":false,"recovering":false,"session_pct":60,"weekly_pct":10,"session_resets_at":null,"weekly_resets_at":null,"weekly_exhausted":false,"access_expires_at":null,"refresh_health":null,"auth":"healthy"}],"next_swap":null,"refresh_enabled":true,"systemic_refresh_failure":3}
    """#

    /// The active account's bounded-blindness projection (issues #479/#485) — auto-protection OK. The
    /// active account's usage poll is blind (429 / ADR-0017): `session_pct` / `weekly_pct` are null (the
    /// daemon's `usage: None`), and `blind_active` carries the SEMANTIC line — blind duration, the retained
    /// last-known session %, and `auto_protection_degraded: false`. A viable `spare` exists (non-cornered).
    /// NOT byte-pinned — the goldens cover non-blind frames that omit the key (`skip_serializing_if`), so a
    /// non-blind frame stays byte-unchanged; this hand-built frame gives `blind_active` decode coverage.
    static let snapshotBlindActiveOK = #"""
    {"type":"snapshot","schema_version":{"major":1,"minor":8},"generated_at":1893456000,"accounts":[{"label":"work","active":true,"enabled":true,"quarantined":false,"recovering":false,"session_pct":null,"weekly_pct":null,"session_resets_at":null,"weekly_resets_at":null,"weekly_exhausted":false,"access_expires_at":null,"refresh_health":null,"auth":"healthy","blind_active":{"blind_secs":240,"last_known_session_pct":64,"auto_protection_degraded":false}},{"label":"spare","active":false,"enabled":true,"quarantined":false,"recovering":false,"session_pct":20,"weekly_pct":10,"session_resets_at":null,"weekly_resets_at":null,"weekly_exhausted":false,"access_expires_at":null,"refresh_health":null,"auth":"healthy"}],"next_swap":{"state":"target","to":"spare","reason":{"kind":"soonest_reset","resets_at":1893800000}},"refresh_enabled":true,"systemic_refresh_failure":null}
    """#

    /// The blind projection with ADR-0017 auto-protection DEGRADED (`auto_protection_degraded: true`) — the
    /// gate is armed but acting on a STALE anchor (blind past the threshold, last-known sat in the risk
    /// band). Non-cornered (a viable `spare` remains), so the glance escalates to `.attention`, NOT
    /// `.noRunway`. 23m blind, last-known 87%.
    static let snapshotBlindActiveDegraded = #"""
    {"type":"snapshot","schema_version":{"major":1,"minor":8},"generated_at":1893456000,"accounts":[{"label":"work","active":true,"enabled":true,"quarantined":false,"recovering":false,"session_pct":null,"weekly_pct":null,"session_resets_at":null,"weekly_resets_at":null,"weekly_exhausted":false,"access_expires_at":null,"refresh_health":null,"auth":"healthy","blind_active":{"blind_secs":1380,"last_known_session_pct":87,"auto_protection_degraded":true}},{"label":"spare","active":false,"enabled":true,"quarantined":false,"recovering":false,"session_pct":20,"weekly_pct":10,"session_resets_at":null,"weekly_resets_at":null,"weekly_exhausted":false,"access_expires_at":null,"refresh_health":null,"auth":"healthy"}],"next_swap":{"state":"target","to":"spare","reason":{"kind":"soonest_reset","resets_at":1893800000}},"refresh_enabled":true,"systemic_refresh_failure":null}
    """#

    /// `encode_heartbeat_frame(42)` — the canonical beat the Rust test decodes.
    static let heartbeatBasic = #"""
    {"type":"heartbeat","generated_at":42,"schema_version":{"major":1,"minor":8}}
    """#

    /// The daemon `stats` socket reply (issue #356) — the bounded per-account daily series the panel
    /// Stats tab reads over the control socket. Byte-exact daemon output: `serde_json::to_string` of
    /// the SAME `StatsWire` the CLI `stats --json` emits, serialized COMPACT (the socket frame), from
    /// the CLI byte-stability golden report (`src/stats.rs` `wire_golden_report`). Byte-identical to
    /// the Rust-emitted `wire-stats-basic.json` golden (`WireGoldenTests`), putting `StatsWire` under
    /// the cross-language byte-drift guard (#340) for the first time. The stats DECODER lands with the
    /// Stats tab (#446); this fixture freezes the shape now so the decoder cannot drift from it.
    static let statsBasic = #"""
    {"schema":1,"window":{"start":1782864000,"end":1782907200,"label":"last 24h (Jul 1–Jul 1)","period":"day"},"accounts":[],"series":[{"start":0,"end":21600,"roster":{"swap_count":1,"swaps":{"session":1,"weekly":0,"manual":0,"forced":0,"emergency":0},"all_high_episodes":0,"all_high_secs":0},"accounts":{"work":{"seen":3,"coverage":1.0,"coverage_class":"complete","session":{"mean":0.5,"peak":0.9,"p95":0.85},"weekly":{"mean":0.3,"peak":0.4,"p95":0.38},"cap_hits":1,"time_at_cap_secs":300,"contribution_share":1.0,"band":"high"}}}],"summary":{"roster":{"swap_count":1,"swaps":{"session":1,"weekly":0,"manual":0,"forced":0,"emergency":0},"all_high_episodes":0,"all_high_secs":0},"accounts":{"work":{"seen":3,"coverage":1.0,"coverage_class":"complete","session":{"mean":0.5,"peak":0.9,"p95":0.85},"weekly":{"mean":0.3,"peak":0.4,"p95":0.38},"cap_hits":1,"time_at_cap_secs":300,"contribution_share":1.0,"band":"high"}}}}
    """#

    /// A `stats` reply from a daemon whose `config.toml` EXISTS but failed to parse (issue #642):
    /// `statsBasic` plus the additive `config_unreadable` key.
    ///
    /// The reason is the REAL string the daemon emits for a parse failure, copied verbatim from
    /// `wire_config_reason` (`src/stats.rs`) — one of a small, fixed set of STATIC strings. The daemon
    /// deliberately does NOT put the parser's own message here: that message re-prints the operator's
    /// config (the TOML span echo quotes the offending line; serde quotes bad values; validation errors
    /// interpolate them bare), and `label` / `account_uuid` are where an e-mail address lives. So the
    /// wire names the failure CLASS and the command that prints the detail, and the full detail stays
    /// in the daemon log. A static reason is also why the panel can render this unclipped.
    ///
    /// Hand-built (not byte-emitted) BY DESIGN: the Rust golden is emitted from a HEALTHY report, where
    /// `skip_serializing_if` omits the key entirely — which is exactly the property `statsBasic` above
    /// still pins. This fixture covers the other branch, so the decoder is tested on both.
    static let statsConfigUnreadable = #"""
    {"schema":1,"window":{"start":1782864000,"end":1782907200,"label":"last 24h (Jul 1–Jul 1)","period":"day"},"accounts":[],"series":[{"start":0,"end":21600,"roster":{"swap_count":1,"swaps":{"session":1,"weekly":0,"manual":0,"forced":0,"emergency":0},"all_high_episodes":0,"all_high_secs":0},"accounts":{"work":{"seen":3,"coverage":1.0,"coverage_class":"complete","session":{"mean":0.5,"peak":0.9,"p95":0.85},"weekly":{"mean":0.3,"peak":0.4,"p95":0.38},"cap_hits":1,"time_at_cap_secs":300,"contribution_share":1.0,"band":"high"}}}],"summary":{"roster":{"swap_count":1,"swaps":{"session":1,"weekly":0,"manual":0,"forced":0,"emergency":0},"all_high_episodes":0,"all_high_secs":0},"accounts":{"work":{"seen":3,"coverage":1.0,"coverage_class":"complete","session":{"mean":0.5,"peak":0.9,"p95":0.85},"weekly":{"mean":0.3,"peak":0.4,"p95":0.38},"cap_hits":1,"time_at_cap_secs":300,"contribution_share":1.0,"band":"high"}}},"config_unreadable":"config.toml is not valid TOML — run `sessiometer config validate` for the detail"}
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

    /// A pre-#393 daemon (minor 1): `next_swap` = target WITHOUT the `reason` key. The reason field
    /// was additive in 1.2, so an older daemon emits a bare target — it must decode to
    /// `reason: nil` (the `decodeIfPresent` forward-compat path), NOT a decode error. This freezes
    /// the additive contract that makes `NextSwap.target` render-safe against an older daemon.
    static let snapshotTargetNoReason = #"""
    {"type":"snapshot","schema_version":{"major":1,"minor":1},"generated_at":42,"accounts":[{"label":"work","active":true,"enabled":true,"quarantined":false,"recovering":false,"session_pct":60,"weekly_pct":10,"session_resets_at":null,"weekly_resets_at":null,"weekly_exhausted":false,"access_expires_at":null,"refresh_health":null,"auth":"healthy"}],"next_swap":{"state":"target","to":"spare"},"refresh_enabled":false,"systemic_refresh_failure":null}
    """#

    /// A NEWER daemon (minor 5) whose `next_swap.target` carries a `reason.kind` this panel does not
    /// recognise — a FUTURE #393 variant (here a hypothetical `headroom`, with its own additive
    /// payload). The unrecognised DECORATION must degrade to `reason: nil` (the bare target label —
    /// exactly the pre-#393 `snapshotTargetNoReason` path) and the frame must STILL decode, never a
    /// lost snapshot (issue #412). The forward-compat INVERSE of `snapshotUnknownNextSwap`: an unknown
    /// `reason.kind` is TOLERATED (a decoration), an unknown `state` is REJECTED (state). Hand-built to
    /// a state the current daemon never emits, so — like its siblings above — it has no Rust golden.
    static let snapshotUnknownReasonKind = #"""
    {"type":"snapshot","schema_version":{"major":1,"minor":5},"generated_at":42,"accounts":[{"label":"work","active":true,"enabled":true,"quarantined":false,"recovering":false,"session_pct":60,"weekly_pct":10,"session_resets_at":null,"weekly_resets_at":null,"weekly_exhausted":false,"access_expires_at":null,"refresh_health":null,"auth":"healthy"}],"next_swap":{"state":"target","to":"spare","reason":{"kind":"headroom","headroom_pct":42}},"refresh_enabled":false,"systemic_refresh_failure":null}
    """#

    // ---- Malformed / rejected bodies --------------------------------------------------------

    /// An unknown `next_swap` state — the daemon's internally-tagged enum rejects it, so the
    /// client must too (a hard decode error, not a tolerated unknown).
    static let snapshotUnknownNextSwap = #"""
    {"type":"snapshot","schema_version":{"major":1,"minor":0},"generated_at":1,"accounts":[],"next_swap":{"state":"future_state"},"refresh_enabled":false}
    """#

    /// An unknown `canonical_scrub` state (issue #516) — like `next_swap.state`, the daemon's
    /// internally-tagged enum rejects a variant it does not know, so the client must too (a hard decode
    /// error, NOT a tolerated unknown — a mis-rendered fleet state is dangerous). Minor 9 marks a NEWER
    /// daemon than this build (minor 8 is real as of issue #613) that added a `canonical_scrub` variant this
    /// client cannot read.
    static let snapshotUnknownCanonicalScrub = #"""
    {"type":"snapshot","schema_version":{"major":1,"minor":9},"generated_at":1,"accounts":[],"next_swap":null,"refresh_enabled":false,"canonical_scrub":{"state":"future_state"}}
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

    /// `next_swap` = target with a MALFORMED known `reason.kind`: `soonest_reset` WITHOUT its required
    /// `resets_at` epoch. This is CORRUPTION, not forward-compat — a KNOWN variant with a missing
    /// required field — so it must stay a hard decode error. The `UnknownKind` tolerance (issue #412)
    /// is for UNRECOGNISED kinds only, never a malformed known one; contrast `snapshotUnknownReasonKind`.
    static let snapshotTargetMalformedReason = #"""
    {"type":"snapshot","schema_version":{"major":1,"minor":0},"generated_at":1,"accounts":[],"next_swap":{"state":"target","to":"spare","reason":{"kind":"soonest_reset"}},"refresh_enabled":false}
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

    // ---- config-get / config-set wire (issue #268) ------------------------------------------
    // The `config-get` reply is a BARE `ConfigView` (or an `{"error":…}` envelope); the `config-set`
    // reply is a `ConfigSetAck`. Hand-built to the `src/config.rs` / `src/daemon/socket.rs` contract:
    // the `ConfigView` field order matches the Rust struct declaration, and every tunable carries a
    // DISTINCT value so a swapped `CodingKey` is caught. Semantic decoders assert values order-
    // independently, so these need not be byte-frozen (no golden guard, unlike the `watch` frames).

    /// A `config-get` reply: the bare `ConfigView` — the 14 effective tunables (each a distinct value)
    /// plus a two-account roster (one enabled, one parked). Mirrors `Config::view` (`src/config.rs`).
    static let configViewBasic = #"""
    {"tunables":{"poll_secs":300,"exhausted_poll_secs":3600,"near_limit_poll_secs":120,"cooldown_secs":45,"target_max_session_usage":85,"session_ceiling":90,"weekly_ceiling":95,"session_blind_swap_secs":900,"session_blind_risk_band":80,"session_velocity_horizon_secs":150,"session_velocity_min_project_above":88,"session_velocity_ema_alpha_pct":40,"monitor_401_n":3,"monitor_recovery_m":2},"accounts":[{"account_uuid":"11111111-1111-1111-1111-111111111111","label":"work","enabled":true},{"account_uuid":"22222222-2222-2222-2222-222222222222","label":"personal","enabled":false}]}
    """#

    /// A drifted-daemon `config-get`: a well-formed `ConfigView` whose roster carries a DUPLICATE
    /// `account_uuid`. It decodes cleanly (the wire types cannot forbid it), so the model must reject it at
    /// adoption — degrade loudly as `.undecodable`, NEVER trap building the uuid→label map.
    static let configViewDuplicateAccount = #"""
    {"tunables":{"poll_secs":300,"exhausted_poll_secs":3600,"near_limit_poll_secs":120,"cooldown_secs":45,"target_max_session_usage":85,"session_ceiling":90,"weekly_ceiling":95,"session_blind_swap_secs":900,"session_blind_risk_band":80,"session_velocity_horizon_secs":150,"session_velocity_min_project_above":88,"session_velocity_ema_alpha_pct":40,"monitor_401_n":3,"monitor_recovery_m":2},"accounts":[{"account_uuid":"11111111-1111-1111-1111-111111111111","label":"work","enabled":true},{"account_uuid":"11111111-1111-1111-1111-111111111111","label":"duplicate","enabled":false}]}
    """#

    /// `config-get` when `config.toml` does not exist — the redacted `{"error":"no config"}` envelope.
    static let configGetNoConfig = #"{"error":"no config"}"#

    /// `config-get` when `config.toml` exists but does not parse — `{"error":"config unreadable"}`.
    static let configGetUnreadable = #"{"error":"config unreadable"}"#

    /// `config-set` applied, a tunable changed → effective on restart.
    static let configSetAppliedRestart = #"{"result":"applied","effect":"restart_required"}"#

    /// `config-set` applied, only a label changed → adopted live.
    static let configSetAppliedLive = #"{"result":"applied","effect":"live"}"#

    /// `config-set` applied, submitted values equalled current → no-op.
    static let configSetAppliedUnchanged = #"{"result":"applied","effect":"unchanged"}"#

    /// `config-set` rejected: a tunable out of range / cross-field violation; `detail` names the field.
    static let configSetRejectedInvalid = #"""
    {"result":"rejected","reason":"invalid","detail":"exhausted_poll_secs (3600) must be >= poll_secs (7200)"}
    """#

    /// `config-set` rejected: a label edit named an `account_uuid` no longer in the roster (stale client);
    /// no `detail` (absent for every reason but `invalid`).
    static let configSetRejectedUnknownAccount = #"{"result":"rejected","reason":"unknown-account"}"#
}
