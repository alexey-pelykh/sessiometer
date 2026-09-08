//! Shared boundary contract between [`crate::daemon`] and [`crate::refresh_tick`].
//!
//! These types are the seam the two modules speak across: the time [`Clock`] and the
//! periodic-refresh [`RefreshTicker`] the daemon's run loop drives, plus the per-sweep data
//! ([`SweepOutcome`], [`RefreshObservation`], [`RefreshDelta`]) the ticker hands back. Housing
//! them here — rather than inside `daemon` — lets `refresh_tick` depend on the contract WITHOUT
//! depending on the whole daemon, untangling the `daemon ↔ refresh_tick` dependency cycle
//! (issue #202; the enabling step for the #195 per-concern decomposition). The module depends
//! only on [`crate::observability`] and `std` / `tokio`, never on `daemon` or `refresh_tick`, so
//! it is a leaf both build on. `daemon` re-exports these under `crate::daemon::*` for its own
//! callers, so relocating them is source-compatible for every existing consumer.

use std::time::{Duration, Instant};

use crate::observability::{Event, RefreshEventOutcome};

// [`slept_since_boot`] is the one per-target piece of this module, and neither arm's clock pair
// exists on the other's target. A third target has no arm at all, so say so here — naming the port
// that is missing — rather than leaving a bare "cannot find function `slept_since_boot`" at the
// single call site in [`RealClock::now`]. ADR-0029 declares macOS and Linux the supported set.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
compile_error!(
    "the sleep-inclusive clock needs a per-target accumulated-suspend read; only macOS \
     (`mach_continuous_time`) and Linux (`CLOCK_BOOTTIME`) are ported — see ADR-0029"
);

/// Time seam: the daemon reads "now" and sleeps until the next poll through
/// this, so a fake can drive time and make the loop run instantly in tests.
pub(crate) trait Clock {
    /// The current instant, INCLUDING any time the host spent asleep.
    ///
    /// # Suspend/resume contract (issues #615, #624)
    ///
    /// Callers that measure a RATE across two readings — chiefly the session-velocity interval in
    /// [`crate::daemon`], which divides a usage delta by
    /// `now.saturating_duration_since(prev_at)` — require this clock to keep counting while the
    /// system is ASLEEP (boottime / `mach_continuous_time` semantics). Session usage accrues in
    /// wall-clock time (the 5 h session window is a wall-clock window), so the honest rate across a
    /// laptop suspend is the delta over the full wall-clock gap.
    ///
    /// A clock that FREEZES during sleep (uptime / `mach_absolute_time` semantics) reports a resume
    /// interval far shorter than the span it covers, inflating the derived rate — and BOTH
    /// velocity-aware swap arms then fire early on a climb that never happened: the projection
    /// `observed + rate × horizon` reaches the effective ceiling sooner, and the reactive fire point
    /// `effective_ceiling - velocity × poll_gap`
    /// ([`crate::swap::reactive_session_threshold`]) is dragged down toward the observed reading.
    ///
    /// ## `std`'s `Instant` does NOT satisfy this on EITHER supported target
    ///
    /// On Linux `Instant::now()` reads `CLOCK_MONOTONIC`, which `man 2 clock_gettime` defines as
    /// NOT including "any time that the system is suspended" — that is `CLOCK_BOOTTIME`'s job. The
    /// macOS case below is the same defect under different names, and issue #624 settled it there
    /// first; [`slept_since_boot`] carries a per-target arm for each.
    ///
    /// ### The macOS half — settled by issue #624
    ///
    /// `Instant::now()` on Apple targets reads `CLOCK_UPTIME_RAW`, which `man 3 clock_gettime`
    /// defines as the clock that "does not increment while the system is asleep" and whose value
    /// "is identical to the result of `mach_absolute_time()`". That holds across the WHOLE supported
    /// toolchain range — the crate pins no `rust-toolchain.toml`, so the range is MSRV 1.87.0
    /// (`Cargo.toml` `rust-version`, CI `RUST_MSRV`) through the CI-pinned stable 1.96.0
    /// (`RUST_STABLE`), and the two ends select it in different files but identically:
    ///
    /// - 1.87.0 — `library/std/src/sys/pal/unix/time.rs`: `const clock_id = libc::CLOCK_UPTIME_RAW`
    /// - 1.96.0 — `library/std/src/sys/time/unix.rs`: `const CLOCK_ID = libc::CLOCK_UPTIME_RAW`
    ///
    /// both under `#[cfg(target_vendor = "apple")]`, each quoting that man-page text verbatim as its
    /// rationale. A probe on macOS 26.5 confirms the value domain empirically under both toolchains:
    /// `Instant`'s `Debug` timespec equals `clock_gettime(CLOCK_UPTIME_RAW)` to sub-microsecond, and
    /// that in turn equals `mach_absolute_time()` exactly. (Issue #615 could not settle this from a
    /// live probe because its host had not slept since boot, making `mach_continuous_time ==
    /// mach_absolute_time` — the comparison was degenerate. Reading which clock `std` selects, then
    /// applying Apple's documented semantics for THAT clock, needs no sleeping host.)
    ///
    /// [`RealClock`] therefore does NOT return a bare `Instant::now()` — it adds back the sleep that
    /// clock skipped, so this contract is GUARANTEED rather than merely documented. The daemon test
    /// `a_suspend_resume_gap_neither_spurious_swaps_nor_misses_one` pins the FOLD's half: driving a
    /// fake clock, it fixes the arithmetic — the rate is the usage delta over the FULL measured gap
    /// — so a regression that clamped or discarded a long interval fails there.
    fn now(&self) -> Instant;
    /// Sleep for `interval` — the (jittered) wait until the next poll, computed
    /// per cycle by the daemon (issue #38). The clock no longer owns the
    /// interval; it just sleeps the duration it is handed.
    async fn tick(&self, interval: Duration);
}

/// Real clock: a sleep-INCLUSIVE monotonic instant (see [`Clock::now`]) and a Tokio sleep of the
/// handed interval.
#[derive(Default)]
pub(crate) struct RealClock;

impl RealClock {
    pub(crate) fn new() -> Self {
        Self
    }
}

/// The mach timebase ratio: mach tick counts are in these units, NOT nanoseconds — 1/1 on x86_64,
/// 125/3 on Apple silicon — so the ratio must be read at runtime, never assumed.
#[cfg(target_os = "macos")]
#[repr(C)]
#[derive(Clone, Copy)]
struct MachTimebaseInfo {
    numer: u32,
    denom: u32,
}

// The three libSystem entry points the sleep-inclusive clock needs on macOS (issue #624).
// This block is why a Linux `cargo check` could not see the port that was owed (issue
// #963): an `extern` block resolves at LINK, so an un-gated one compiles cleanly on a
// target whose libc has none of these symbols and fails only when `build` or `test` links
// a binary. Linux takes the `clock_gettime` arm below instead.
//
// Hand-declared rather than taken from `libc`. Two independent reasons, either sufficient:
// `libc` 0.2.186 exposes only TWO of these three — `mach_absolute_time` and `mach_timebase_info`
// — and both are `#[deprecated(since = "0.2.55", note = "Use the `mach2` crate instead")]`, which
// CI's `-D warnings` rejects; and the one doing the actual work here, `mach_continuous_time`, the
// crate does not expose at all. Following the deprecation note to `mach2` would add a crate to a
// credential-adjacent supply chain for three argument-less counter reads — exactly the trade
// `CONTRIBUTING.md`'s minimal-dependency line rejects, and the issue's own steer (a small
// hand-rolled FFI shim over a time crate). This is the same call the raw `libc::flock` FFI makes
// (ADR-0004). All three ship in libSystem, which the crate already links; `mach_continuous_time`
// is macOS 10.12+.
#[cfg(target_os = "macos")]
extern "C" {
    /// Mach ticks since boot, EXCLUDING time spent asleep — what `Instant::now()` already reads.
    fn mach_absolute_time() -> u64;
    /// Mach ticks since boot, INCLUDING time spent asleep. Same epoch and timebase as
    /// `mach_absolute_time`, so the two differ by exactly the sleep.
    fn mach_continuous_time() -> u64;
    /// Writes the tick-to-nanosecond ratio. Returns `KERN_SUCCESS` (0) on success.
    fn mach_timebase_info(info: *mut MachTimebaseInfo) -> i32;
}

/// Convert a mach tick count to a [`Duration`] through the timebase ratio.
///
/// Split out from [`slept_since_boot`] because it is the part that can be wrong silently: an
/// inverted ratio still produces a plausible-looking `Duration`, just off by ~1700x on Apple
/// silicon. Pure and total, so the unit tests below pin it against both real-world timebases.
#[cfg(target_os = "macos")]
fn ticks_to_duration(ticks: u64, timebase: MachTimebaseInfo) -> Duration {
    if timebase.denom == 0 {
        // Unreachable via `mach_timebase_info`; guards the division rather than trusting that.
        return Duration::ZERO;
    }
    let nanos = u128::from(ticks) * u128::from(timebase.numer) / u128::from(timebase.denom);
    Duration::from_nanos(u64::try_from(nanos).unwrap_or(u64::MAX))
}

/// How long the host has been ASLEEP since boot, on macOS (issue #624).
///
/// `mach_continuous_time - mach_absolute_time`: both count mach ticks from the SAME boot epoch, the
/// former through sleep and the latter not, so the difference is exactly the accumulated sleep. It
/// is ZERO on a host that has never slept, which is what makes adding it back a strict no-op until
/// the first suspend.
///
/// Darwin's `CLOCK_MONOTONIC` is the documented sleep-inclusive `clock_gettime` id and would seem
/// the tidier source, but it is NOT epoch-aligned with `CLOCK_UPTIME_RAW` — measured on macOS 26.5,
/// the two sit a constant ~4.87 s apart with ZERO drift between samples. Differencing THAT pair
/// would fold a fixed constant into every reading; the mach pair is the one with a shared origin.
/// Linux's own pair does not have that defect — see the `linux` arm below.
#[cfg(target_os = "macos")]
fn slept_since_boot() -> Duration {
    // Read continuous FIRST: should the host suspend between these two reads, `absolute` lands after
    // the wake and the difference UNDER-counts the sleep rather than over-counting it. That is the
    // safe direction — an over-counted gap would dilute a rate toward a MISSED swap, whereas
    // under-counting merely leaves this reading as accurate as the un-adjusted clock was.
    // SAFETY: both are argument-less libSystem reads of a kernel counter, with no preconditions.
    let continuous = unsafe { mach_continuous_time() };
    // SAFETY: as above.
    let absolute = unsafe { mach_absolute_time() };

    let mut timebase = MachTimebaseInfo { numer: 0, denom: 0 };
    // SAFETY: `mach_timebase_info` writes the two `u32`s of the struct handed to it by pointer;
    // `timebase` is a live local of exactly that layout (`#[repr(C)]`).
    if unsafe { mach_timebase_info(&mut timebase) } != 0 {
        // Unreachable in practice; degrade to "no sleep to add back", i.e. the old behaviour.
        return Duration::ZERO;
    }
    ticks_to_duration(continuous.saturating_sub(absolute), timebase)
}

/// The accumulated-suspend duration implied by ONE paired reading of the two boot-epoch clocks
/// (issue #963).
///
/// Split out from [`slept_since_boot`] for exactly the reason [`ticks_to_duration`] is split out
/// on the mach arm: it is the part that can be wrong SILENTLY. Swapping the two operands still
/// yields a perfectly plausible `Duration::ZERO` on every host that has never suspended — which
/// is every CI runner — so only a pure test driving a synthetic suspended reading can tell a real
/// subtraction from an inverted one. Pure and total, so the unit tests below do exactly that.
///
/// `saturating_sub` rather than a panic or a wrap: [`slept_since_boot`] reads `boottime` FIRST, so
/// on a never-suspended host `monotonic` lands a few nanoseconds LATER and the difference is
/// negative by that much. Flooring it to zero under-counts by nanoseconds, which is the safe
/// direction (see [`slept_since_boot`]); wrapping would turn it into ~584 years.
#[cfg(target_os = "linux")]
fn sleep_between_boot_clocks(boottime: Duration, monotonic: Duration) -> Duration {
    boottime.saturating_sub(monotonic)
}

/// One `clock_gettime` reading as a [`Duration`] since the clock's epoch, or `None` when the
/// syscall fails. `None` is what makes [`slept_since_boot`] degrade to "no sleep to add back" —
/// the unadjusted clock — rather than fabricating an offset from a reading it never got.
#[cfg(target_os = "linux")]
fn read_clock(clock: libc::clockid_t) -> Option<Duration> {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `clock_gettime` writes the two fields of the `timespec` handed to it by pointer;
    // `ts` is a live local of exactly that type, and the kernel writes it ONLY on success
    // (return 0). A `clockid_t` it does not know returns `EINVAL` rather than UB.
    if unsafe { libc::clock_gettime(clock, &mut ts) } != 0 {
        return None;
    }
    // Both fields are non-negative for these two clocks (each counts UP from boot); a negative
    // value would be a kernel contract violation, and refusing it here is the same fail-soft
    // branch as the syscall error above rather than a panic in a long-lived daemon.
    Some(Duration::new(
        u64::try_from(ts.tv_sec).ok()?,
        u32::try_from(ts.tv_nsec).ok()?,
    ))
}

/// How long the host has been SUSPENDED since boot, on Linux (issue #963).
///
/// `CLOCK_BOOTTIME - CLOCK_MONOTONIC`, the direct analogue of the mach arm above: `man 2
/// clock_gettime` defines `CLOCK_BOOTTIME` as "identical to `CLOCK_MONOTONIC`, except that it also
/// includes any time that the system is suspended", so the two share a boot epoch and their
/// difference is exactly the accumulated suspend. It is ZERO on a host that has never suspended,
/// which is what makes adding it back a strict no-op until the first suspend.
///
/// This pair does NOT carry the constant epoch offset that rules Darwin's `CLOCK_MONOTONIC` /
/// `CLOCK_UPTIME_RAW` pair out on macOS — Linux's `CLOCK_MONOTONIC` IS the uptime clock the
/// correction is applied to. `std`'s `Instant::now()` reads `CLOCK_MONOTONIC` on Linux, so
/// [`RealClock::now`] adding this offset back yields precisely `CLOCK_BOOTTIME`, which is the
/// sleep-inclusive reading [`Clock::now`]'s contract requires.
#[cfg(target_os = "linux")]
fn slept_since_boot() -> Duration {
    // Read boottime FIRST, for the same reason the mach arm reads `continuous` first: should the
    // host suspend between these two reads, `monotonic` lands after the wake and the difference
    // UNDER-counts the suspend rather than over-counting it. Under-counting merely leaves this
    // reading as accurate as the un-adjusted clock was; over-counting would dilute a rate toward a
    // MISSED swap.
    let (Some(boottime), Some(monotonic)) = (
        read_clock(libc::CLOCK_BOOTTIME),
        read_clock(libc::CLOCK_MONOTONIC),
    ) else {
        // Unreachable in practice — both ids are unconditionally supported on every kernel this
        // crate builds for; degrade to "no sleep to add back", i.e. a bare `Instant::now()`.
        return Duration::ZERO;
    };
    sleep_between_boot_clocks(boottime, monotonic)
}

impl Clock for RealClock {
    fn now(&self) -> Instant {
        // `Instant::now()` is the host's sleep-EXCLUSIVE uptime clock on BOTH supported targets —
        // `CLOCK_UPTIME_RAW` on macOS, `CLOCK_MONOTONIC` on Linux (see [`Clock::now`]); adding back
        // the sleep it skipped yields the boottime reading this trait's contract requires.
        //
        // The offset is re-read EVERY call — it must be, to pick up sleep that happens during the
        // daemon's lifetime — and `slept_since_boot` samples its two counters a few nanoseconds
        // apart, so on a slept host the offset carries sub-microsecond jitter and two calls in the
        // same microsecond can come back out of order. Strict per-call monotonicity is therefore
        // deliberately NOT promised; forward progress across any real interval is. That is harmless
        // by construction: every consumer ages a reading with `saturating_duration_since` (a
        // backward step floors to zero, never negative) over readings taken a full poll apart
        // (seconds, via `.as_secs()`), so a sub-µs wobble cannot invert a measured interval.
        let uptime = Instant::now();
        // `checked_add` cannot realistically overflow — it would take centuries of accumulated
        // suspend — but saturating to the unadjusted instant keeps even that from panicking a
        // long-lived daemon.
        uptime.checked_add(slept_since_boot()).unwrap_or(uptime)
    }

    async fn tick(&self, interval: Duration) {
        tokio::time::sleep(interval).await;
    }
}

/// Periodic-refresh seam (issue #105): the run loop drives the in-daemon isolated-refresh
/// tick from its idle path, off the poll→usage→swap seam. The production impl
/// ([`crate::refresh_tick::RefreshTick`]) keeps PARKED accounts' stored tokens fresh through
/// the #102 engine — and is wholly inert when the feature is off: its `until_due` never
/// resolves, so a feature-off daemon (or a hermetic test wired with a no-op ticker) behaves
/// exactly as it did before #105.
///
/// Two methods so the run loop can serve the control socket WHILE waiting for the tick to
/// fall due, yet protect an in-flight sweep from being cancelled by a control read (only
/// shutdown interrupts a sweep): [`until_due`](RefreshTicker::until_due) is the wait;
/// [`sweep`](RefreshTicker::sweep) is the bounded work.
pub(crate) trait RefreshTicker {
    /// Whether the tick currently has #106 RESTORE work (issue #280): ≥1 account THIS sweep would
    /// actually refresh for the restore path — quarantined (in the daemon's `quarantined` set), NOT
    /// in `excluded` (the active account + imminent swap target), AND within the refresh allowlist.
    /// It is the EXACT per-account predicate [`sweep`](RefreshTicker::sweep) gates on, evaluated by
    /// the ticker (which owns the allowlist) and kept in one place so the two cannot drift — so a
    /// quarantined account the sweep would SKIP (an excluded active/target, or one outside a
    /// configured allowlist) never raises a prompt for a restore that would not happen. The run
    /// loop threads the result into [`until_due`](RefreshTicker::until_due). `false` when disabled.
    fn recovery_pending(&self, excluded: &[String], quarantined: &[String]) -> bool;
    /// Resolve when a refresh sweep is due (the ticker's own cadence/idle gating, on its own
    /// [`Clock`] seam). MUST never resolve when the feature is disabled, so it never wins the
    /// idle select and adds no clock activity. Re-armable: the run loop awaits it afresh each
    /// idle iteration, and a control read between waits simply restarts it.
    ///
    /// `has_recovery_work` is the ticker's own [`recovery_pending`](RefreshTicker::recovery_pending)
    /// verdict, gated by the run loop to fire at most once per idle period (issue #280). When set,
    /// the ticker becomes due within a short bounded interval (the idle floor) instead of deferring
    /// the restore up to a full refresh cadence after an unrelated recent sweep. The run loop
    /// passes it TRUE only until the current idle period's sweep has run, so the prompt fires at
    /// most once per idle period (poll cadence) — never the sub-poll retry storm ADR-0007 decided
    /// against.
    async fn until_due(&mut self, has_recovery_work: bool);
    /// Run ONE refresh sweep over the due parked accounts, EXCLUDING the `excluded` uuids
    /// (the active account + the imminent swap target the daemon supplies). `quarantined` is
    /// the daemon's currently-dead ("needs re-login") set: those accounts are refreshed even
    /// when not near expiry, and a successful one is reported for RESTORE (issue #106).
    /// Records the sweep for cadence gating. Per-account failures are non-fatal (the engine
    /// Caller contract). Returns the per-cycle [`SweepOutcome`] for the daemon to emit + apply.
    async fn sweep(&mut self, excluded: &[String], quarantined: &[String]) -> SweepOutcome;
}

/// What one [`RefreshTick::sweep`](RefreshTicker::sweep) produced (issue #106): the
/// per-cycle [`Event::Refresh`] log lines, plus the `account_uuid`s of QUARANTINED
/// accounts whose refresh succeeded and so should be RESTORED to eligible.
///
/// Both are handed back to the daemon (which owns the event log and the health machine)
/// rather than acted on here: the tick is a hermetic seam with no `EventLog` handle and
/// no view of the quarantine state. The daemon emits the events and applies the restores
/// ([`crate::daemon`]'s run loop) — keeping each `restored` flip paired with its
/// [`Event::CredentialRestored`] in the one place that owns the health machine.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct SweepOutcome {
    /// The sweep's events, in sweep order: one [`Event::Refresh`] per refreshed account, plus —
    /// since issue #786 — an [`Event::RefreshBinaryResolved`] immediately BEFORE the cycle whose
    /// `claude`-binary resolution was news (the first one, or a change). NOT homogeneous, then: a
    /// consumer must not assume every element is a `Refresh`. Neither current one has to — the
    /// daemon's run loop emits them variant-agnostically through [`Event::to_log_line`], and the
    /// #378 systemic classification keys off [`observations`](Self::observations), not this list.
    pub(crate) events: Vec<Event>,
    /// `account_uuid`s of quarantined accounts the cycle proved still refreshable.
    pub(crate) restored: Vec<String>,
    /// One [`RefreshObservation`] per account the sweep READ this cycle (issue #119) —
    /// the credential clocks the daemon folds into its per-account health state for the
    /// `status` rollup. Recorded for EVERY non-excluded, allowlisted account whose stash
    /// the sweep touched (so a healthy far-from-expiry account still surfaces its expiry
    /// clock), with the refresh-health delta present only on the ones actually refreshed.
    pub(crate) observations: Vec<RefreshObservation>,
}

/// One account's credential-clock observation from a sweep (issue #119): the stored
/// access-token expiry the sweep read, plus — only when the account was actually
/// refreshed this cycle — the refresh-health delta. The daemon folds these into its
/// per-account health state ([`crate::daemon`]) for the `status` 5-state rollup; every
/// field is non-secret (a timestamp, a classification, a boolean — never a token / email).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RefreshObservation {
    /// The account, keyed by `account_uuid` (the daemon resolves it to a roster slot) —
    /// the same resolution key `restored` uses; never the email or a token.
    pub(crate) account_uuid: String,
    /// The stored access-token `expiresAt` (epoch MS, CC's native unit) the sweep read
    /// this cycle, or `None` when the stash was unreadable. The daemon converts to epoch
    /// seconds at the fold boundary.
    pub(crate) expires_at_ms: Option<i64>,
    /// The refresh-health delta — `Some` ONLY when this cycle actually ran a refresh (a
    /// near-expiry or quarantined account); `None` when the sweep merely READ the
    /// account's expiry without refreshing it (a healthy, far-from-expiry account).
    pub(crate) refresh: Option<RefreshDelta>,
}

/// The non-secret refresh-health signal from one completed refresh cycle (issue #119):
/// the classification, which since issue #1004 carries whether the refresh token rotated as
/// a payload on its two refreshed variants. The expiry slide lives in
/// [`RefreshObservation::expires_at_ms`]; this is the "did it work / did the token value
/// change" half the rollup's at-risk / dead inputs key off.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct RefreshDelta {
    /// The cycle's non-secret classification (the same one the [`Event::Refresh`] carries),
    /// including its rotation payload where the outcome admits one — so this struct cannot
    /// pair a rotation with an outcome that never exchanged a token.
    pub(crate) outcome: RefreshEventOutcome,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two mach timebases that occur in the wild.
    #[cfg(target_os = "macos")]
    const APPLE_SILICON: MachTimebaseInfo = MachTimebaseInfo {
        numer: 125,
        denom: 3,
    };
    #[cfg(target_os = "macos")]
    const X86_64: MachTimebaseInfo = MachTimebaseInfo { numer: 1, denom: 1 };

    /// The tick-to-nanosecond conversion, pinned against both real timebases. An inverted ratio is
    /// the silent failure mode this exists to catch: it still yields a plausible `Duration`, just
    /// ~1736x wrong on Apple silicon.
    ///
    /// macOS-only, and it does NOT transfer: mach ticks and their timebase have no Linux
    /// counterpart at all. The Linux arm's own silent-failure guard is
    /// [`the_suspend_offset_is_the_boottime_excess_over_monotonic`] below.
    #[cfg(target_os = "macos")]
    #[test]
    fn ticks_convert_through_the_real_world_timebases() {
        // Apple silicon: a 24 MHz counter, so 24e6 ticks is exactly one second.
        assert_eq!(
            ticks_to_duration(24_000_000, APPLE_SILICON),
            Duration::from_secs(1)
        );
        // x86_64: mach ticks are already nanoseconds.
        assert_eq!(
            ticks_to_duration(1_000_000_000, X86_64),
            Duration::from_secs(1)
        );
        // Zero ticks is zero time on any timebase — the never-slept host's case.
        assert_eq!(ticks_to_duration(0, APPLE_SILICON), Duration::ZERO);
    }

    /// A zero denominator degrades to "no sleep to add back" instead of dividing by zero, and a tick
    /// count large enough to overflow the nanosecond `u64` saturates instead of wrapping to a small
    /// value — a wrap would silently reverse the correction's effect.
    #[cfg(target_os = "macos")]
    #[test]
    fn tick_conversion_is_total() {
        let degenerate = MachTimebaseInfo {
            numer: 125,
            denom: 0,
        };
        assert_eq!(ticks_to_duration(24_000_000, degenerate), Duration::ZERO);
        assert_eq!(
            ticks_to_duration(u64::MAX, APPLE_SILICON),
            Duration::from_nanos(u64::MAX),
        );
    }

    /// The Linux arm's silent-failure guard, and the counterpart to the mach arm's
    /// [`ticks_convert_through_the_real_world_timebases`] (issue #963): the suspend offset is
    /// `CLOCK_BOOTTIME`'s EXCESS over `CLOCK_MONOTONIC`, in that order.
    ///
    /// The middle case is the whole point. Every CI runner — and every developer machine that has
    /// not suspended — reads the two clocks equal, so a swapped subtraction, a hard-coded
    /// `Duration::ZERO`, or a `saturating_sub` pointing the wrong way are ALL indistinguishable
    /// from the real thing on a live host: each one returns zero and each one passes
    /// [`accumulated_sleep_does_not_drift_while_awake`] and
    /// [`real_clock_now_is_the_uptime_instant_plus_accumulated_sleep`] below. Only a synthetic
    /// suspended reading separates them, and only a PURE function can be handed one.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_suspend_offset_is_the_boottime_excess_over_monotonic() {
        // Never suspended: the two clocks agree, so the correction is a strict no-op.
        assert_eq!(
            sleep_between_boot_clocks(Duration::from_secs(100), Duration::from_secs(100)),
            Duration::ZERO,
        );
        // Suspended for 50 s: boottime kept counting, monotonic did not. An inverted subtraction
        // saturates to ZERO here and fails; the live-host cases above cannot tell the two apart.
        assert_eq!(
            sleep_between_boot_clocks(Duration::from_secs(150), Duration::from_secs(100)),
            Duration::from_secs(50),
        );
        // The ordering the kernel's contract forbids, which the nanoseconds between the two reads
        // can nonetheless produce on a never-suspended host: floor to zero rather than wrap to
        // ~584 years, which would swamp every rate the daemon derives.
        assert_eq!(
            sleep_between_boot_clocks(Duration::from_secs(100), Duration::from_secs(150)),
            Duration::ZERO,
        );
    }

    /// The two clock ids [`slept_since_boot`] differences are BOTH readable on this host, and the
    /// offset they yield is bounded by the host's own time since boot (issue #963).
    ///
    /// What this catches that the pure test above cannot: a wrong clock id. `CLOCK_REALTIME`
    /// differenced against `CLOCK_MONOTONIC` is just as stable while awake and just as
    /// self-consistent, so it passes every other test in this module — and reports the host as
    /// having been asleep since 1970. Accumulated suspend cannot exceed time since boot.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_suspend_offset_cannot_exceed_the_time_since_boot() {
        let boottime = read_clock(libc::CLOCK_BOOTTIME)
            .expect("CLOCK_BOOTTIME is readable on every kernel this crate builds for");
        assert!(
            read_clock(libc::CLOCK_MONOTONIC).is_some(),
            "CLOCK_MONOTONIC must be readable — it is what `Instant::now()` itself reads",
        );
        let offset = slept_since_boot();
        assert!(
            offset <= boottime,
            "the host cannot have been suspended for {offset:?} of its {boottime:?} since boot — \
             that is a differenced pair with no shared boot epoch",
        );
    }

    /// The accumulated-sleep offset is a property of the HOST, not of the moment it is read, so two
    /// reads taken while awake must agree. This is the guard against picking a source whose two
    /// halves drift apart while running — and the reason Darwin's `CLOCK_MONOTONIC` /
    /// `CLOCK_UPTIME_RAW` pair is not the source here (a constant, but nonzero, epoch offset).
    ///
    /// Target-neutral by construction: it calls [`slept_since_boot`], so it exercises whichever arm
    /// this target compiled. What it does NOT do is tell a real arm from a stub, since both hold
    /// steady while awake — that is the pure per-arm test's job.
    #[test]
    fn accumulated_sleep_does_not_drift_while_awake() {
        let first = slept_since_boot();
        std::thread::sleep(Duration::from_millis(50));
        let second = slept_since_boot();
        let drift = second.abs_diff(first);
        assert!(
            drift < Duration::from_millis(5),
            "the sleep offset must not advance while the host is awake: {first:?} -> {second:?}",
        );
    }

    /// `RealClock::now` is the uptime instant plus exactly that offset — the whole of issue #624's
    /// fix. On a host that has never slept the offset is zero and this reduces to the old
    /// `Instant::now()`, which is why the assertion is a bound rather than an equality: it must hold
    /// on a freshly-booted CI runner AND on a laptop that has suspended for days.
    #[test]
    fn real_clock_now_is_the_uptime_instant_plus_accumulated_sleep() {
        let offset = slept_since_boot();
        let uptime = Instant::now();
        let now = RealClock::new().now();
        // `now` is sampled after `uptime`, so the gap is the offset plus the (sub-ms) wall time
        // between the two reads — never less than the offset, never much more.
        let observed = now.saturating_duration_since(uptime);
        assert!(
            observed >= offset && observed - offset < Duration::from_millis(50),
            "expected the uptime instant offset by ~{offset:?}, observed {observed:?}",
        );
    }

    /// `RealClock::now` makes forward progress across a real interval — the property the velocity
    /// divisor actually needs. Deliberately NOT strict per-call monotonicity: the offset is re-read
    /// each call from two counters sampled a few ns apart, so successive readings can wobble sub-µs
    /// on a slept host (see [`RealClock::now`]); every consumer absorbs that via
    /// `saturating_duration_since` over readings a full poll apart. A tight-loop `next >= prev`
    /// assertion would encode an invariant `now` does not hold — and would pass only on never-slept
    /// hosts (every CI runner), where the offset is a hard zero, while failing on a laptop that has
    /// actually suspended.
    #[test]
    fn real_clock_now_advances_across_a_real_interval() {
        let clock = RealClock::new();
        let start = clock.now();
        std::thread::sleep(Duration::from_millis(20));
        let elapsed = clock.now().saturating_duration_since(start);
        assert!(
            elapsed >= Duration::from_millis(15),
            "expected ~20 ms of forward progress, saw {elapsed:?}",
        );
    }
}
