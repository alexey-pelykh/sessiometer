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
/// An UNKNOWN `kind` throws `UnknownKind`, which the CARRYING `NextSwap.target` decoder TOLERATES by
/// degrading `reason` to `nil` (the bare target label) rather than losing the whole frame (issue
/// #412). `reason` is a DECORATION on an already-understood `target` state, so a future variant an
/// older panel does not recognise must not brick the snapshot — unlike an unknown `next_swap.state`,
/// which stays a hard error (a mis-rendered STATE is dangerous; a missing rationale is not). A
/// MALFORMED known `kind` (e.g. `soonest_reset` without `resets_at`) is corruption, NOT forward-compat,
/// so it stays a hard `DecodingError`. Mirrors the daemon's variant set; render each medium's own way,
/// never a pre-formatted string (state-parity).
enum NextSwapReason: Equatable {
    case soonestReset(resetsAt: Int64)
    case onlyCandidate
    case rosterOrder

    /// Thrown by the decoder when `kind` is a value this client does not recognise — a forward-compat
    /// DECORATION the carrying `NextSwap.target` decoder catches and degrades to `reason == nil` (issue
    /// #412). Deliberately a DISTINCT error type, NOT a `DecodingError`, so that decoder can tell
    /// "unknown decoration → tolerate" apart from "malformed known `kind` → propagate as a hard error"
    /// by the error's TYPE alone.
    struct UnknownKind: Error, Equatable {
        /// The unrecognised `kind` string — carried for diagnostics, never rendered.
        let kind: String
    }
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
            // Forward-compat (issue #412): an unrecognised `kind` is a decoration a NEWER daemon
            // added that this panel does not know. Throw a DISTINCT `UnknownKind` (not a
            // `DecodingError`) so `NextSwap.target`'s decoder degrades it to `reason == nil` instead
            // of losing the frame — while a MALFORMED known `kind` (the `decode` calls above, e.g. a
            // `soonest_reset` missing `resets_at`) still throws a `DecodingError` that propagates as
            // the hard error corruption deserves.
            throw UnknownKind(kind: kind)
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
    /// The SESSION window gates the soonest-returning spare — relief arrives at that account's session
    /// reset. On a MIXED fleet this names the WINNING spare's gating dimension, NOT a fleet-wide
    /// property (issue #665); the renderer keys the "add an account" nudge off the actual WAIT, not
    /// this label (issue #666).
    case session
    /// The WEEKLY window gates the soonest-returning spare (issue #37) — relief arrives at that
    /// account's weekly reset. Likewise names the WINNING spare's dimension, not a fleet property
    /// (issue #665); the render nudges on wait length, not this label (issue #666).
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
/// (the same additive-minor forward-compat the whole contract rests on); an UNRECOGNISED
/// `reason.kind` from a NEWER daemon likewise degrades to `nil` here rather than losing the frame
/// (issue #412 — `reason` is a decoration, not state). `no_viable_target`'s
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
            let to = try container.decode(String.self, forKey: .to)
            // `reason` (issue #393) is an ADDITIVE, optional DECORATION, and two forward-compat
            // degradations collapse to the SAME `nil` (the bare target label): an OMITTED reason (a
            // pre-#393 daemon — `decodeIfPresent` → nil) and an UNRECOGNISED `reason.kind` (a newer
            // daemon's future variant — `NextSwapReason.UnknownKind` caught here → nil, issue #412).
            // A MALFORMED KNOWN kind throws a `DecodingError` instead, which is NOT caught and
            // propagates as the hard error corruption deserves. Tolerating an unknown kind here keeps
            // ONE unrenderable rationale from silently killing every row, meter and frame
            // (`WatchStatusStore` drops an undecodable line, so the whole panel would freeze).
            let reason: NextSwapReason?
            do {
                reason = try container.decodeIfPresent(NextSwapReason.self, forKey: .reason)
            } catch is NextSwapReason.UnknownKind {
                reason = nil
            }
            self = .target(to: to, reason: reason)
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

/// The daemon-level CANONICAL-SCRUB rollup (`src/daemon/snapshot.rs` `CanonicalScrub`, issue #516):
/// present only while the shared `Claude Code-credentials` canonical item is SCRUBBED — the
/// fleet-wide lockout NO per-account `auth` rollup reflects (the shared item is emptied while account
/// rows can read perfectly healthy). Distinguishes the daemon still autonomously RECOVERING (adopt in
/// progress) from RECOVERY-EXHAUSTED (the un-recoverable residual that needs a `claude /login`, which
/// #469 renders with that remedy). Internally tagged on `state` (`snake_case`), so a value is one of
/// two shapes:
///   * `{"state":"recovering"}`
///   * `{"state":"exhausted"}`
///
/// An UNKNOWN `state` is a HARD decode error — faithfully mirroring the daemon's internally-tagged
/// enum, which rejects a variant it does not know. A mis-rendered fleet STATE is dangerous, so this
/// takes the same reject posture as an unknown `next_swap.state` (NOT the tolerated-decoration posture
/// of an unknown `reason.kind`). The whole `canonical_scrub` key is optional (ABSENT when healthy),
/// handled at `VersionedStatus` via `decodeIfPresent` — the additive-minor forward-compat the #164
/// contract rests on. Non-secret — a bare state discriminant, never a token or email (issue #15).
enum CanonicalScrub: Equatable {
    /// Scrubbed, but the daemon's autonomous adopt-recovery is still in progress — the fleet may
    /// self-heal with no operator action. The lower-severity state.
    case recovering
    /// Scrubbed AND recovery exhausted — the daemon backed off, so the canonical stays empty until a
    /// `claude /login`. The residual un-recoverable state #469 renders with that remedy.
    case exhausted
}

extension CanonicalScrub: Decodable {
    private enum CodingKeys: String, CodingKey {
        case state
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        let state = try container.decode(String.self, forKey: .state)
        switch state {
        case "recovering":
            self = .recovering
        case "exhausted":
            self = .exhausted
        default:
            throw DecodingError.dataCorruptedError(
                forKey: .state,
                in: container,
                debugDescription:
                    "unknown canonical_scrub state '\(state)' — an incompatible wire contract"
            )
        }
    }
}

/// WHICH of a systemic-refresh episode's two opening brackets opened it (`src/daemon/snapshot.rs`
/// `SystemicRefreshSource`, issue #813) — the wire's half of a distinction the daemon's event log has
/// drawn since issue #787 but the snapshot could not.
///
/// THE CANONICAL HOME for the three-way rendering contract below: the two render sites
/// (`StatusPanelFormat.systemicRefreshFailureBanner`, `PresentationState.make`) carry their own
/// strings and point here for the WHY.
///
/// An episode opens EITHER on the #378 sweep crossing or on the #787 startup preflight failing to
/// resolve the `claude` binary; it closes one way, on the first working sweep. The
/// `systemicRefreshFailure` COUNT cannot carry that distinction: the preflight path seeds the count at
/// one so a pre-#813 client stays grammatical, which is byte-identical to a genuine one-sweep crossing
/// under `systemic_failure_n = 1`. Without this discriminant both the panel banner and the menu-bar
/// a11y label asserted "1 consecutive sweep failed" for an episode in which ZERO sweeps had run.
///
/// A bare STRING (`"sweep"` / `"preflight"`), not internally tagged like `CanonicalScrub`: the two
/// variants are fieldless classifications with no per-variant payload to grow into.
///
/// An UNRECOGNISED token does NOT fail the frame — the TOLERATED-DECORATION posture of an unknown
/// `next_swap.reason.kind`, deliberately NOT the hard-reject posture of an unknown `canonical_scrub`
/// state or `next_swap.state`. The split is about WHERE the alarm lives, not about house style: for
/// those two the unknown value IS the fault, so a frame that cannot be read faithfully must be refused
/// rather than mis-rendered. Here the fault rides in a SEPARATE field (`systemic_refresh_failure`)
/// that decodes fine, and this one only picks which evidence clause the banner cites. Rejecting would
/// throw away the whole frame — roster, vault pair, canary alarm and the mechanism-down count itself —
/// over a phrasing selector. It would also be unrecoverable rather than gateable: `WireContract`
/// `isSupported` keys on the MAJOR only, so a future 1.12 daemon adding a third bracket is a version
/// this client reports as SUPPORTED while blanking every frame it sends.
///
/// It degrades to [`unrecognized`](Self/unrecognized) rather than to `nil`, because those two states
/// are NOT the same and must not render the same. `nil` means "no provenance on this wire" — a
/// pre-#813 daemon — where the historical sweep phrasing is the best available reading and is what
/// that daemon's own client always showed. An unrecognised token means "this episode was opened by a
/// bracket this build has never heard of", and a third bracket is far likelier to be another
/// NON-sweep opener than a sweep one — so reusing the sweep phrasing there would assert a sweep that
/// never ran, re-creating the exact defect #813 exists to remove, in a build too old to know better.
/// The renderers therefore give it a provenance-NEUTRAL clause: the DOWN verdict and the remedy, with
/// no evidence claimed.
///
/// Deliberately NOT `String`-raw-valued: [`unrecognized`](Self/unrecognized) is a CLIENT-side decode
/// outcome, never a wire token, and a raw-value mapping would let some future daemon token collide
/// with it. `init(wireToken:)` is the only way in, which also keeps the mapping total.
///
/// The whole key is optional (ABSENT when healthy, and absent from every pre-#813 daemon), handled at
/// `VersionedStatus` via `decodeIfPresent`. Non-secret — a FIXED TOKEN from a closed set, never a path,
/// binary location, token, or email (issue #15).
enum SystemicRefreshSource: Equatable {
    /// The #378 SWEEP crossing: `systemic_failure_n` consecutive sweeps in a row failed with
    /// `outcome=error` across every eligible account. The count IS a sweep count here, so a renderer
    /// may cite it as one.
    case sweep
    /// The #787 startup PREFLIGHT: the `claude` binary could not be resolved, so every eligible
    /// account's refresh is guaranteed to fail before a single cycle runs. No sweep produced this
    /// verdict, so a renderer must NOT cite the count as a sweep count (issue #813).
    case preflight
    /// A bracket this build does not know — a NEWER daemon (the wire's forward direction). Never sent
    /// by any daemon; produced only here, by `init(wireToken:)`. The episode is real and the DOWN
    /// verdict stands, but this build cannot say what opened it, so renderers must claim no evidence
    /// at all rather than guess `sweep` and risk asserting a sweep that never ran.
    case unrecognized

    /// Map a wire token to a case, TOTALLY — an unrecognised token becomes
    /// [`unrecognized`](Self/unrecognized) rather than a decode failure. The only constructor from the
    /// wire, so the tolerate-don't-reject posture cannot be bypassed at a call site.
    init(wireToken: String) {
        switch wireToken {
        case "sweep": self = .sweep
        case "preflight": self = .preflight
        default: self = .unrecognized
        }
    }
}

// MARK: - The behavioral-canary verdict (issue #714 — the keychain-derivation identity check)

/// The behavioral canary's LAST verdict (`src/daemon/snapshot.rs` `CanaryStatus`, issue #714): did the
/// reverse-engineered #100 keychain derivation still point at the credential Claude Code is actually
/// using, the last time the canary ran? Internally tagged on `verdict` (`snake_case`, mirroring
/// `CanonicalScrub`'s `state`), so a value is one of five shapes:
///   * `{"verdict":"ok"}` — positive identity pass (quiet).
///   * `{"verdict":"inconclusive"}` — no positive evidence either way; fails open to Layer-1 (quiet).
///   * `{"verdict":"not_found"}` — zero items under the derived service (quiet here — already voiced by
///     the `canonical_scrub` / `keychain_locked` machinery, so a canary banner would double-report).
///   * `{"verdict":"ambiguous","count":N}` — MORE THAN ONE matching keychain item: no unique write
///     target, so credential writes are REFUSED (ALARM).
///   * `{"verdict":"drift","displayed":"..","matched":"..","overridden":bool}` — the resolved canonical
///     byte-matches a DIFFERENT account's stash than the one named active; writes REFUSED pre-mutation
///     unless `overridden` (ALARM).
///
/// An UNKNOWN `verdict` is a HARD decode error — faithfully mirroring the daemon's internally-tagged enum,
/// which rejects a variant it does not know. This is a STATE that drives an alarm banner, so it takes the
/// same reject posture as an unknown `canonical_scrub` / `next_swap.state` (a mis-rendered — or
/// under-rendered — alarm state is dangerous), NOT the tolerated-decoration posture of an unknown
/// `reason.kind`: dropping the frame degrades to the last-known render, strictly safer than silently
/// decoding a newer daemon's alarm to `nil` = a false "all clear". The whole `canary` key is optional
/// (ABSENT on a pre-#714 daemon, and until the first canary run concludes), handled at `VersionedStatus`
/// via `decodeIfPresent` — the additive-minor forward-compat the #164 contract rests on. Non-secret:
/// operator LABELS and a COUNT only, never a token, email, or account-uuid (issue #15).
enum CanaryStatus: Equatable {
    case ok
    case inconclusive
    case notFound
    case ambiguous(count: Int)
    case drift(displayed: String, matched: String, overridden: Bool)
    /// Layer 2 REFUSE (issues #730/#738, wire since schema 1.10): the resolved canonical matches no
    /// account's stash AND parses as no Claude Code credential — overwhelmingly an UNRELATED secret,
    /// which the daemon refuses to clobber, so credential writes (swaps AND auto-protection) are
    /// blocked. Carries no payload: the item's bytes are somebody else's secret, so there is nothing
    /// #15-safe to name. A pre-#738 daemon never sends it; a daemon whose `canary_nostashmatch_override`
    /// is set sends `inconclusive` instead, because with the override nothing is being refused.
    case refusedUnparseableCanonical
}

extension CanaryStatus: Decodable {
    private enum CodingKeys: String, CodingKey {
        case verdict
        case count
        case displayed
        case matched
        case overridden
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        let verdict = try container.decode(String.self, forKey: .verdict)
        switch verdict {
        case "ok":
            self = .ok
        case "inconclusive":
            self = .inconclusive
        case "not_found":
            self = .notFound
        case "ambiguous":
            self = .ambiguous(count: try container.decode(Int.self, forKey: .count))
        case "drift":
            self = .drift(
                displayed: try container.decode(String.self, forKey: .displayed),
                matched: try container.decode(String.self, forKey: .matched),
                overridden: try container.decode(Bool.self, forKey: .overridden)
            )
        case "refused_unparseable_canonical":
            self = .refusedUnparseableCanonical
        default:
            throw DecodingError.dataCorruptedError(
                forKey: .verdict,
                in: container,
                debugDescription:
                    "unknown canary verdict '\(verdict)' — an incompatible wire contract"
            )
        }
    }
}

// MARK: - The active account's bounded-blindness projection

/// The active account's bounded-blindness projection (issues #479/#485) — mirrors the daemon's
/// `BlindActive` (`src/daemon/snapshot.rs`): how long the active account has been blind, the retained
/// last-known session %, and whether ADR-0017 preemptive auto-protection is DEGRADED (armed, but acting
/// on a STALE anchor). Wire key `blind_active`, present ONLY on a blind active account — the daemon OMITS
/// the key otherwise (`skip_serializing_if`), so a non-blind line's bytes are unchanged (additive minor
/// 1.3 → 1.4). All three inner fields are required WHEN the object is present (the Rust fields carry no
/// default), so a malformed `blind_active` missing one throws rather than mis-reads. Non-secret — a
/// duration and two small numbers, never a token or email (issue #15).
struct BlindActive: Decodable, Equatable, Sendable {
    /// Seconds the active account has been blind (`blind_secs`) — a DURATION the client renders verbatim
    /// against its own clock-free `humanizeUntil`, never an absolute instant.
    let blindSecs: UInt64
    /// The retained pre-blind SESSION-window usage percent (`0…100`, `last_known_session_pct`) — the
    /// last-known reading before the account went blind. Why the row shows a HELD value, not "no data".
    let lastKnownSessionPct: UInt8
    /// Whether ADR-0017 auto-protection is DEGRADED — the gate is armed but acting on a STALE anchor.
    /// `false` = OK (blind, but not yet past the gate threshold, or the anchor sat below the risk band).
    let autoProtectionDegraded: Bool

    private enum CodingKeys: String, CodingKey {
        case blindSecs = "blind_secs"
        case lastKnownSessionPct = "last_known_session_pct"
        case autoProtectionDegraded = "auto_protection_degraded"
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
    /// The active account's bounded-blindness projection (issues #479/#485), wire key `blind_active`;
    /// present ONLY on a blind active account (the daemon omits the key otherwise). The panel renders it
    /// as a SEMANTIC per-row state — blind duration, last-known session %, and whether ADR-0017
    /// auto-protection is OK or DEGRADED — in place of a false-healthy row (#137). Reflect-only: the
    /// surface never self-polls or self-swaps off it (#169).
    let blindActive: BlindActive?

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
        case blindActive = "blind_active"
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
        blindActive = try c.decodeIfPresent(BlindActive.self, forKey: .blindActive)
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
    /// WHICH opening bracket opened the active systemic-refresh episode (`src/daemon/snapshot.rs`
    /// `StatusResponse.systemic_refresh_source`, issue #813): `.sweep` / `.preflight` alongside a non-nil
    /// `systemicRefreshFailure`, else `nil` (ABSENT) when the mechanism is healthy. Lets the panel banner
    /// and the menu-bar a11y label phrase a preflight-opened episode without asserting a sweep that never
    /// ran — the count alone cannot, because the preflight path seeds it at one for pre-#813 grammar.
    ///
    /// `nil` for a pre-#813 daemon, for a healthy one (`skip_serializing_if` omits it there, so a healthy
    /// frame is byte-unchanged), AND for a future token this build does not know. Added by the MINOR
    /// `1.10 → 1.11` bump — an older client tolerates it by ignoring, and THIS client tolerates both its
    /// absence and its un-decodability: a `nil` source beside a non-nil count keeps the historical sweep
    /// phrasing, which is the best a frame carrying no readable provenance supports. A fixed-token
    /// classification, never a path or secret (issue #15).
    let systemicRefreshSource: SystemicRefreshSource?
    /// The daemon-level CANONICAL-SCRUB rollup (`src/daemon/snapshot.rs` `StatusResponse.canonical_scrub`,
    /// issue #516): `.recovering` / `.exhausted` while the shared canonical item is scrubbed, else `nil`
    /// (ABSENT) when healthy — the fleet-wide scrubbed / un-recoverable lockout no per-account `auth`
    /// rollup reflects. `nil` for a pre-#516 daemon AND for a healthy one (`skip_serializing_if` omits it
    /// there, so a non-scrub frame is byte-unchanged). Added by the MINOR `1.4 → 1.5` bump — an older
    /// client tolerates it by ignoring; a bare state discriminant, never a token or email (issue #15).
    let canonicalScrub: CanonicalScrub?
    /// The daemon-level KEYCHAIN-LOCKED flag (`src/daemon/snapshot.rs` `StatusResponse.keychain_locked`,
    /// issue #498): `true` while the macOS login keychain is LOCKED, so the daemon cannot READ the shared
    /// credential item at all (access denied) — the daemon-LEVEL sibling of `canonicalScrub`, but for an
    /// UNREADABLE item rather than a readable-but-scrubbed one (so the operator remedy differs: unlock the
    /// keychain, not `claude /login`). `false` for a pre-#498 daemon AND for an unlocked one
    /// (`skip_serializing_if` omits it there, so a non-locked frame is byte-unchanged). Added by the MINOR
    /// `1.5 → 1.6` bump — an older client tolerates it by ignoring; a bare binary state discriminant,
    /// never a token or email (issue #15).
    let keychainLocked: Bool
    /// The behavioral-canary verdict (`src/daemon/snapshot.rs` `StatusResponse.canary`, `CanaryStatus`,
    /// issue #714): the keychain-derivation identity check's LAST result, or `nil` when there is no verdict
    /// — a pre-#714 daemon (which omits the key), OR the canary has not concluded a run yet (`skip_serializing_if`
    /// omits it until the first run concludes, and on a canary that could not run at all — e.g. a boot under a
    /// locked keychain: no evidence is not a verdict). Added by the MINOR `1.8 → 1.9` bump — an older client
    /// tolerates it by ignoring. The ALARM verdicts (`drift`, `ambiguous`) surface through
    /// `StatusPanelFormat.daemonFaultBanner` at the cross-surface ranks the CLI pins (`src/cli.rs`
    /// `DaemonPayloadFault`); the quiet verdicts (`ok` / `inconclusive` / `not_found`) render nothing.
    /// Operator LABELS and a COUNT only, never a token or email (issue #15).
    let canary: CanaryStatus?

    private enum CodingKeys: String, CodingKey {
        case schemaVersion = "schema_version"
        case generatedAt = "generated_at"
        case accounts
        case nextSwap = "next_swap"
        case refreshEnabled = "refresh_enabled"
        case systemicRefreshFailure = "systemic_refresh_failure"
        case systemicRefreshSource = "systemic_refresh_source"
        case canonicalScrub = "canonical_scrub"
        case keychainLocked = "keychain_locked"
        case canary
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
        // Decoded as a raw STRING then mapped TOTALLY, so an unrecognised token becomes `.unrecognized`
        // rather than failing the whole frame (the `reason.kind` tolerated-decoration posture). Note the
        // two nil-ish outcomes stay DISTINCT: an absent key is `nil` (a pre-#813 daemon, which renders
        // the historical sweep phrasing), an unreadable token is `.unrecognized` (a newer daemon, which
        // renders no evidence at all) — see the type's doc.
        systemicRefreshSource = try c.decodeIfPresent(String.self, forKey: .systemicRefreshSource)
            .map(SystemicRefreshSource.init(wireToken:))
        canonicalScrub = try c.decodeIfPresent(CanonicalScrub.self, forKey: .canonicalScrub)
        keychainLocked = try c.decodeIfPresent(Bool.self, forKey: .keychainLocked) ?? false
        canary = try c.decodeIfPresent(CanaryStatus.self, forKey: .canary)
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

// MARK: - Stats wire (issue #356 socket verb / #446 Stats-tab decoder)

// The daemon `stats` socket reply (`{"cmd":"stats","period":…}` → one `StatsWire` line, issue #356) —
// the bounded per-account daily usage series the panel Stats tab reads (#446). Hand-mirrors the Rust
// `StatsWire` serializer (`src/stats.rs` `stats_wire` / `StatsWire` … `SwapsWire` / `Band` / `CoverageClass`),
// which is the SAME document `sessiometer stats --json` emits — R-2 parity is STRUCTURAL (one Rust builder,
// `stats_wire`), not re-derived here. Decoded field-by-field like the `watch` mirror above; byte-frozen by
// `Fixtures.statsBasic` + the cross-language golden guard (`WireGoldenTests`, #340).
//
// Unlike the `watch` frames this carries NO `type` tag — it is a request→response body decoded directly
// (an `{"error":…}` envelope on an invalid period is detected first, see `decodeStatsReply`). `#159`/`#160`
// extend the contract ADDITIVELY without bumping `schema`, so absent keys are tolerated: `orphans`
// (`skip_serializing_if` when empty), `period` / `since` (`Option::is_none`), and `config_unreadable`
// (#642, `Option::is_none`) each decode to a default. NOTE this wire is versioned by its OWN `schema`
// (`src/stats.rs` `JSON_SCHEMA_VERSION`), NOT by `STATUS_SCHEMA_VERSION` — that governs the separate
// `status` / `watch` `VersionedStatus` payload mirrored above, which carries a `schema_version` object
// this document does not have.
//
// Source of truth (mirror, do not re-derive): `src/stats.rs` — `StatsWire`, `WindowWire`, `BucketWire`,
// `PeriodWire`, `AccountWire`, `DimWire`, `RosterWire`, `SwapsWire`, `Band`, `CoverageClass`.

/// The top-level `stats` reply document (`src/stats.rs` `StatsWire`).
struct StatsWire: Decodable, Equatable {
    let schema: UInt32
    let window: StatsWindow
    /// The applied account filter (redacted handles); empty means "all" — the socket verb never filters.
    let accounts: [String]
    /// The per-bucket series — the Stats-tab sparkline source (one `session.peak` per bucket, #446).
    let series: [StatsBucket]
    /// The whole-window aggregate — the Stats row's numeric body + the aggregate callout.
    let summary: StatsPeriodBody
    /// Non-roster ("orphan") handles (issue #314), keyed like `summary.accounts` but OMITTED when none
    /// (Rust `skip_serializing_if`), so an absent key decodes to empty — never plotted (summary-window only).
    let orphans: [String: StatsAccountStats]
    /// The daemon's honesty signal (issue #642): the secret-free reason `config.toml` could not be read,
    /// or `nil` when the config is fine (the key is then ABSENT, not null — Rust `skip_serializing_if`).
    ///
    /// Non-`nil` means every ceiling-dependent figure in this document — `capHits`, `timeAtCapSecs`, the
    /// `band`, the sparkline scale — was computed against the daemon's DEFAULT tunables, not the
    /// operator's (between `session_ceiling` 95 and 50 the same store yields cap-hits 112 vs 356). The
    /// panel MUST annotate the readout rather than render it as fact: a surface must not read more
    /// confident than reality (the #479 / #582 / #632 honesty family). Additive — a pre-#642 daemon
    /// simply omits the key and this decodes to `nil`, so no `schema` bump is involved.
    let configUnreadable: String?

    private enum CodingKeys: String, CodingKey {
        case schema, window, accounts, series, summary, orphans
        case configUnreadable = "config_unreadable"
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        schema = try c.decode(UInt32.self, forKey: .schema)
        window = try c.decode(StatsWindow.self, forKey: .window)
        accounts = try c.decodeIfPresent([String].self, forKey: .accounts) ?? []
        series = try c.decode([StatsBucket].self, forKey: .series)
        summary = try c.decode(StatsPeriodBody.self, forKey: .summary)
        orphans = try c.decodeIfPresent([String: StatsAccountStats].self, forKey: .orphans) ?? [:]
        configUnreadable = try c.decodeIfPresent(String.self, forKey: .configUnreadable)
    }
}

/// The resolved reporting window (`src/stats.rs` `WindowWire`): `[start, end)` epoch seconds, the human
/// echo, and how it was selected — a preset `period` (the socket path always sets this) OR a raw `since`.
/// Both selectors are optional (Rust `skip_serializing_if = Option::is_none`); the synthesized `Decodable`
/// decodes each with `decodeIfPresent`, so an absent OR null key is tolerated.
struct StatsWindow: Decodable, Equatable {
    let start: Int64
    let end: Int64
    let label: String
    let period: String?
    let since: String?
}

/// One series bucket (`src/stats.rs` `BucketWire`): its `[start, end)` plus the same per-account + roster
/// body as the summary. The `session.peak` of each account across the buckets IS the sparkline series.
struct StatsBucket: Decodable, Equatable {
    let start: Int64
    let end: Int64
    let roster: StatsRoster
    let accounts: [String: StatsAccountStats]
}

/// The per-account + roster body shared by the summary and each series bucket (`src/stats.rs` `PeriodWire`).
struct StatsPeriodBody: Decodable, Equatable {
    let roster: StatsRoster
    let accounts: [String: StatsAccountStats]
}

/// One account's window aggregate (`src/stats.rs` `AccountWire`). `session` / `weekly` are FRACTIONS
/// (0…1, over the quota cap) — the panel renders them as percents; `band` is the neutral session-peak
/// descriptor the Stats-tab signal pill collapses (`StatusPanelFormat.statsSignal`).
struct StatsAccountStats: Decodable, Equatable {
    let seen: UInt32
    let coverage: Double
    let coverageClass: StatsCoverageClass
    let session: StatsDim
    let weekly: StatsDim
    let capHits: UInt32
    let timeAtCapSecs: Int64
    let contributionShare: Double
    let band: StatsBand

    private enum CodingKeys: String, CodingKey {
        case seen, coverage
        case coverageClass = "coverage_class"
        case session, weekly
        case capHits = "cap_hits"
        case timeAtCapSecs = "time_at_cap_secs"
        case contributionShare = "contribution_share"
        case band
    }
}

/// One quota dimension's mean / peak / p95 (`src/stats.rs` `DimWire`) — each a fraction (0…1) of the cap.
struct StatsDim: Decodable, Equatable {
    let mean: Double
    let peak: Double
    let p95: Double
}

/// Roster-wide statistics for a window (`src/stats.rs` `RosterWire`): swap frequency and the
/// all-accounts-high water — the source of the Stats tab's aggregate callout.
struct StatsRoster: Decodable, Equatable {
    let swapCount: UInt32
    let swaps: StatsSwaps
    let allHighEpisodes: UInt32
    let allHighSecs: Int64
    /// The session-utilisation water each account had to be at/above for the census above, as a
    /// FRACTION (`session_ceiling` / 100 — 0.95 at defaults, but OPERATOR-TUNABLE). Issue #804
    /// carries it precisely so this surface states the water it actually measured instead of
    /// hardcoding a literal; issue #805 is the panel consuming it.
    ///
    /// OPTIONAL only for the pre-#804 daemon that never sent the key. The CURRENT daemon always
    /// sends it (`RosterWire` deliberately does NOT `skip_serializing_if` it), so `nil` here means
    /// "an older daemon, which never told us its water" — NOT "no water was used". The renderer
    /// must therefore DROP the qualifier rather than substitute a number
    /// (`StatusPanelFormat.statsAllHighLabel`): a fabricated threshold is the very defect #805
    /// exists to end. Absent-key → `nil` rather than a decode error is the same `decodeIfPresent`
    /// forward-compat path the snapshot mirror uses for its additive fields.
    let allHighThreshold: Double?

    private enum CodingKeys: String, CodingKey {
        case swapCount = "swap_count"
        case swaps
        case allHighEpisodes = "all_high_episodes"
        case allHighSecs = "all_high_secs"
        case allHighThreshold = "all_high_threshold"
    }
}

/// The swap-count breakdown by trigger (`src/stats.rs` `SwapsWire`).
struct StatsSwaps: Decodable, Equatable {
    let session: UInt32
    let weekly: UInt32
    let manual: UInt32
    let forced: UInt32
    let emergency: UInt32
}

/// A neutral utilisation band from the session peak (`src/stats.rs` `Band`, snake_case wire). A DESCRIPTOR,
/// not a signal — it classifies the level, never recommends. An UNKNOWN value is a hard decode error,
/// mirroring serde's rejection of an unknown unit-enum variant (a drifted daemon degrades loudly).
enum StatsBand: String, Decodable, Equatable {
    case idle
    case low
    case moderate
    case high
    case atCap = "at_cap"
}

/// A neutral data-completeness descriptor (`src/stats.rs` `CoverageClass`, snake_case wire). UNKNOWN →
/// decode error, exactly like `StatsBand`.
enum StatsCoverageClass: String, Decodable, Equatable {
    case complete
    case partial
    case absent
}

/// The two shapes a `stats` reply can take: the full `StatsWire` document, or a redacted `{"error":…}`
/// envelope (an invalid `--period` — never on the panel's always-`week` path, but surfaced honestly rather
/// than mis-decoded). `Equatable` so the model's phase can compare.
enum StatsReply: Equatable {
    case ok(StatsWire)
    case error(String)
}

/// The `error`-key probe: the daemon writes ONLY `{"error":…}` on the stats error path, so an object
/// carrying that key is the error envelope; any other object is the full document. `error` is optional, so
/// a valid `StatsWire` (no top-level `error` key) probes to `nil` and falls through to the full decode.
private struct StatsErrorProbe: Decodable {
    let error: String?
}

/// Decode one `stats` reply line into `.ok(StatsWire)` or `.error(reason)` — the probe-then-decode shape
/// `parseWatchFrame` uses. Probes the `error` key FIRST (the sole key on the daemon's error path), then
/// decodes the full document. THROWS on a non-JSON line or a well-formed-but-off-contract document (a
/// missing required field, an unknown `band` / `coverage_class`) — a drifted daemon degrades loudly, exactly
/// like the `watch` decoder. Pure: no I/O, no clock.
func decodeStatsReply(_ line: String) throws -> StatsReply {
    let data = Data(line.utf8)
    let decoder = JSONDecoder()
    // A valid error envelope carries a STRING `error`; anything else (a full document, or an object with a
    // non-string/absent `error`) falls through to the full decode.
    if let reason = (try? decoder.decode(StatsErrorProbe.self, from: data))?.error {
        return .error(reason)
    }
    return .ok(try decoder.decode(StatsWire.self, from: data))
}
