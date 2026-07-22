// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! Fixtures shared by the `config` test modules (issue #653).
//!
//! Issue #638 split the production half of `config` into per-concern children; issue #653
//! then split the test block along the SAME seams. These three fixtures are the part that
//! could not follow a single seam — `VALID` is consumed by all four children AND the
//! parent — so they live here, in one shared parent-owned module, rather than being
//! duplicated where they would silently drift apart. A fixture with a single consumer
//! stays with it instead (`labels`, used only by `settings`).
//!
//! Every `config` test module reaches these with `use crate::config::test_support::*;`.
//!
//! This DIVERGES from the sibling `daemon` decomposition (issue #203), which shares its fakes
//! from inside its own `mod tests` (`use crate::daemon::tests::*;`). A dedicated module is
//! preferred here so a consumer globs the fixtures WITHOUT also globbing the parent's
//! `#[test]` fns — deliberate, not an oversight.

pub(super) const VALID: &str = r#"
[tunables]
poll_secs = 30
cooldown_secs = 45
target_max_session_usage = 70
session_ceiling = 90
weekly_ceiling = 97
monitor_401_n = 5
monitor_recovery_m = 4

[[account]]
account_uuid = "11111111-1111-1111-1111-111111111111"
label = "work"

[[account]]
account_uuid = "22222222-2222-2222-2222-222222222222"
label = "personal"
"#;

/// A minimal valid roster body with one account and the given `[tunables]`
/// fragment spliced in.
pub(super) fn with_tunables(fragment: &str) -> String {
    format!(
        "[tunables]\n{fragment}\n\
         [[account]]\n\
         account_uuid = \"u\"\n\
         label = \"l\"\n"
    )
}

/// A minimal valid roster body with one account and the given `[jitter]`
/// fragment spliced in (the `[tunables]` table is absent → its defaults).
pub(super) fn with_jitter(fragment: &str) -> String {
    format!(
        "[jitter]\n{fragment}\n\
         [[account]]\n\
         account_uuid = \"u\"\n\
         label = \"l\"\n"
    )
}
