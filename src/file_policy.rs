// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! The **owner-only** file-permission policy: stated once, implemented per target (issue #974).
//!
//! Every private file this crate writes is `0600` and every private directory `0700` — a
//! discipline `src/paths.rs` has documented since the first commit and expressed as a raw mode
//! integer at roughly forty call sites. That spelling has no Windows analogue at all: `mode`,
//! `from_mode` and `uid` live on `std::os::unix`, so each of those sites was a compile error on
//! that target and, worse, forty independent chances to get a security property wrong.
//!
//! This module is the alternative that #974 § The design question this item owns records as
//! strongly preferred: the INTENT ("this file belongs to its owner and to nobody else") is named
//! once, and the platform mechanism sits behind it. On Unix the mechanism is the mode bits, byte
//! for byte what it always was. On Windows it is an **explicit, protected DACL**.
//!
//! # What Windows actually needed, which is not what the project record first assumed
//!
//! The earlier expectation was that Windows "needs an explicit DACL to reach a `0600` equivalent",
//! implying the files were wide open there. Measurement on a live host (the #40 recon, quoted in
//! #974 and #28) says otherwise. A Claude Code credential file under the user profile carries
//! exactly three ACEs — `NT AUTHORITY\SYSTEM`, `BUILTIN\Administrators`, and the user — with no
//! `Users`, no `Everyone` and no `Authenticated Users`. SYSTEM and Administrators are the
//! unavoidable Windows floor: an administrator reads anything through ownership or
//! `SeBackupPrivilege`, so naming them is not additional exposure. In effect that already IS the
//! `0600` equivalent.
//!
//! The real finding is narrower. **Every one of those ACEs is INHERITED**, so the file carries no
//! DACL of its own: it is protected only by whatever the profile directory happens to grant, and
//! follows that directory silently if it ever changes. Fine today; not a guarantee. So what this
//! module writes on Windows is the same effective access, made EXPLICIT and PROTECTED — the `P`
//! flag in the SDDL below, which stops the parent's ACEs from flowing in at all.
//!
//! # The two targets are not identical, and the differences are recorded rather than hidden
//!
//! - **Creation is atomic on Unix and is not on Windows.** `OpenOptionsExt::mode` hands the mode
//!   to `open(2)`, so the file is never visible at a wider one. Windows has no equivalent hook on
//!   `std::fs::OpenOptions` — `std::os::windows::fs::OpenOptionsExt` exposes `access_mode`,
//!   `share_mode`, `custom_flags`, `attributes` and `security_qos_flags`, and no
//!   `security_attributes`, so a `SECURITY_ATTRIBUTES` cannot reach `CreateFileW` through it.
//!   [`open_owner_only`](crate::file_policy::open_owner_only) therefore creates and then tightens. What covers the gap for MOST callers
//!   is the PARENT: a file created inside a directory this same module has already made
//!   explicitly owner-only has its transient inherit from a DACL we control rather than from the
//!   profile's. **That argument is bounded, and the bound is where the callers put their staging
//!   file.** `paths::write_private_file` stages `<path>.tmp` in `path`'s OWN directory and neither
//!   creates nor tightens it, so where `path` comes from outside this crate's tree the transient
//!   inherits whatever that directory grants: `cli::write_export` takes the directory from the
//!   operator (`sessiometer export <PATH> --plaintext` writes decrypted secrets wherever they
//!   point it), and `paths::write_preserving_mode` stages `~/.claude.json.tmp` in the profile
//!   root. Windows evaluates a DACL at OPEN time and stamps the granted mask into the handle, so
//!   a later `SetNamedSecurityInfoW` does not revoke access a live handle already has, and
//!   `<path>.tmp` is predictable from the operator's own argument. Closing it needs an atomic
//!   owner-only create, which `src/control_transport.rs`'s `SECURITY_ATTRIBUTES` construction for
//!   `CreateNamedPipeW` already has the parts for — **#1528** carries it.
//! - **A filesystem that cannot hold the policy DEGRADES on Unix and fails CLOSED on Windows.**
//!   `set_permissions` against a mount that ignores mode bits succeeds and the file simply lands
//!   wider, so only `src/roster_backup.rs`'s read-back refuses. `SetNamedSecurityInfoW` against
//!   FAT/exFAT — a USB volume, some sync-provider shims — returns an error instead, so
//!   [`owner_only_file`](crate::file_policy::owner_only_file) fails and EVERY private write under
//!   such a directory fails with it. Failing closed is the right direction for a security policy;
//!   it is recorded because it is a different operator experience, not because it is wrong.
//! - **Unix applies a creation mode only when it creates; Windows applies the DACL either way.**
//!   `open(2)` ignores `mode` for an existing file, and [`open_owner_only`](crate::file_policy::open_owner_only) keeps that. On Windows
//!   there is no way to ask "only if you created it", so an existing file is tightened too. The
//!   divergence narrows access rather than widening it, and every caller in this crate wants
//!   owner-only whichever way it got there.
//! - **`owner_is_current_user_nofollow` is `lstat`-exact on Unix and is not on Windows.**
//!   `GetNamedSecurityInfoW` resolves reparse points and there is no named no-follow form; the
//!   handle-based one would need `CreateFileW(FILE_FLAG_OPEN_REPARSE_POINT)`, which is not ported
//!   here. Its one caller refuses a symlink at the same path before it asks, so the difference is
//!   reachable only through a race, and it is stated rather than papered over.
//!
//! # What this module is NOT
//!
//! It is not a general permission library. It expresses the one policy this crate has — owner-only
//! — plus the "preserve what is already there" case ([`copy_policy`](crate::file_policy::copy_policy)) that the swap engine needs
//! for a file it co-writes but does not own. The LaunchAgent plist's deliberately world-readable
//! `0644` is not here: launchd is a macOS concept, so `src/service.rs` keeps that mode inline
//! under its own `#[cfg(unix)]`, where a reader can see that the policy and the platform are one
//! decision rather than two.
//!
//! # Nothing here has ever RUN on Windows
//!
//! The same disclosure `src/paths.rs` and `src/control_transport.rs` carry: no CI job compiles this
//! crate for Windows (**#978** is the job that will), so every Windows arm below is a type-checked
//! hypothesis reasoned from the documented API contract, not an observation. The committed
//! `#[cfg(windows)]` tests at the foot of this file are what run the moment that job exists.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;

/// `0700` — owner `rwx`, nothing for group or other.
#[cfg(unix)]
pub(crate) const DIR_MODE: u32 = 0o700;
/// `0600` — owner `rw`, nothing for group or other.
#[cfg(unix)]
pub(crate) const FILE_MODE: u32 = 0o600;

/// Open `path` through `options`, leaving it readable and writable by its owner alone.
///
/// The caller supplies every other flag; this adds the policy and nothing else, so the three
/// lock-file openers and the two writers in `src/paths.rs` keep their own creation semantics.
///
/// On Unix the mode reaches `open(2)`, so a file this call CREATES is never visible at a wider
/// one and a file that already existed keeps whatever it had. On Windows the file is opened first
/// and its DACL written immediately after — see the module docs for both halves of that
/// difference.
pub(crate) fn open_owner_only(options: &mut OpenOptions, path: &Path) -> io::Result<File> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(FILE_MODE).open(path)
    }
    #[cfg(windows)]
    {
        let file = options.open(path)?;
        owner_only_file(path)?;
        Ok(file)
    }
}

/// Restrict the EXISTING file at `path` to its owner.
///
/// Unix: `0600`. Windows: an explicit, protected DACL naming the owner plus the unavoidable
/// SYSTEM/Administrators floor, and nothing else.
pub(crate) fn owner_only_file(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(FILE_MODE))
    }
    #[cfg(windows)]
    {
        windows_impl::write_owner_only_dacl(path, Inheritance::No)
    }
}

/// Restrict the EXISTING directory at `path` to its owner.
///
/// Unix: `0700`. Windows: the same explicit, protected DACL as [`owner_only_file`], with its ACEs
/// marked inheritable so anything created inside starts owner-only rather than starting at the
/// profile's defaults and being narrowed afterwards. That is belt-and-braces, not the guarantee:
/// every file this crate creates in such a directory is given its OWN explicit DACL, which
/// replaces the inherited ACEs outright.
pub(crate) fn owner_only_dir(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(DIR_MODE))
    }
    #[cfg(windows)]
    {
        windows_impl::write_owner_only_dacl(path, Inheritance::Yes)
    }
}

/// Copy `from`'s permission policy onto `to` — for a file whose policy is **not ours to set**.
///
/// The swap engine co-writes into `~/.claude.json`, a file owned by Claude Code, through a staging
/// file it renames over the original. Rename does not preserve the DESTINATION's permissions on
/// either target — Unix carries the staging file's mode, and Windows `MoveFileEx` carries its
/// security descriptor — so the policy has to be copied onto the staging file before the rename or
/// the co-write silently re-permissions the user's own file.
///
/// Unix copies the mode bits including any setuid/setgid/sticky. Windows copies the DACL, and
/// copies its PROTECTED-ness with it: a destination whose ACEs were inherited gets an inheriting
/// staging file, one that carried an explicit DACL gets an explicit one. Copying a DACL without
/// its protection flag would turn an explicit policy into a floor the parent could widen, which is
/// the exact fragility this module exists to remove.
pub(crate) fn copy_policy(from: &Path, to: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(from)?.permissions().mode() & 0o7777;
        std::fs::set_permissions(to, std::fs::Permissions::from_mode(mode))
    }
    #[cfg(windows)]
    {
        windows_impl::copy_dacl(from, to)
    }
}

/// Whether `path`'s owner is the current user, FOLLOWING a final symlink (`stat`).
pub(crate) fn owner_is_current_user(path: &Path) -> io::Result<bool> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(std::fs::metadata(path)?.uid() == crate::paths::current_uid())
    }
    #[cfg(windows)]
    {
        windows_impl::owner_is_us(path)
    }
}

/// Whether `path`'s owner is the current user, NEVER following a final symlink (`lstat`).
///
/// Windows cannot honour the no-follow half — see the module docs. The one caller
/// (`paths::create_isolated_dir`) refuses a symlink at this path immediately before asking, so
/// the two targets differ only under a race.
pub(crate) fn owner_is_current_user_nofollow(path: &Path) -> io::Result<bool> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(std::fs::symlink_metadata(path)?.uid() == crate::paths::current_uid())
    }
    #[cfg(windows)]
    {
        windows_impl::owner_is_us(path)
    }
}

/// How `path` DEVIATES from the owner-only policy, or `None` if it does not.
///
/// A read-BACK, for a caller that will not take "the writer was asked for owner-only" as evidence
/// that the filesystem delivered it: `src/roster_backup.rs` refuses to replace the live roster if
/// the backup it just wrote landed wider, because a config directory that is a symlink onto exFAT
/// or a sync-provider shim is an operator affordance nothing here forbids.
///
/// On Unix the deviation is the mode it landed at. On Windows it is the first defect in the
/// rendered DACL — unprotected, an inherited ACE, or an ACE naming somebody who is neither the
/// owner nor the SYSTEM/Administrators floor. Note what the Windows arm deliberately does NOT
/// check: the exact rights mask. `ConvertSecurityDescriptorToStringSecurityDescriptorW` may render
/// one as an alias or as a hex literal, and a production refusal path that turns on that rendering
/// would refuse real roster writes over a formatting difference. The trustee set and the
/// protection flag are what the policy is about, and they are what is checked.
pub(crate) fn owner_only_deviation(path: &Path) -> io::Result<Option<String>> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(path)?.permissions().mode() & 0o777;
        Ok((mode != FILE_MODE).then(|| format!("mode {mode:o}, not {FILE_MODE:o}")))
    }
    #[cfg(windows)]
    {
        windows_impl::dacl_deviation(path)
    }
}

/// Whether a directory's ACEs are marked inheritable — the one axis on which the file and
/// directory DACLs differ.
///
/// Target-neutral so that [`owner_only_sddl`] is, and therefore unit-testable on every target this
/// crate builds for rather than only on the one that can run it.
#[cfg(any(windows, test))]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Inheritance {
    /// A file: the ACEs apply to this object and nothing else.
    No,
    /// A directory: `OICI` — object-inherit and container-inherit, so children start here.
    Yes,
}

/// The SDDL for this crate's owner-only DACL, granting `sid`, LocalSystem and the local
/// Administrators group full access, and nobody else anything (issue #974 AC2, #28's Windows half).
///
/// Three parts carry the whole guarantee.
///
/// `D:` opens the DACL. **`P` makes it PROTECTED**, which is the entire point of the item: without
/// it the ACEs below would be a floor the parent directory could widen, and the file would be back
/// to depending on the profile's own permissions — the fragility the #40 recon actually found.
/// `FA` is `FILE_ALL_ACCESS`, the file object's own full-access mask, rather than the `GA`
/// (`GENERIC_ALL`) that `src/control_transport.rs` writes for a pipe: generic rights are mapped to
/// specific ones per object type, and naming the specific mask means the DACL reads back as it was
/// written instead of as whatever the mapping produced.
///
/// The trustees are `SY` (LocalSystem) and `BA` (the local Administrators group) — the Windows
/// floor the module docs justify — plus `sid`, which is THIS PROCESS's own token user and never a
/// literal: a SID from anywhere else would be a DACL naming somebody the daemon merely believes it
/// is. `SY` and `BA` are the two-letter SDDL aliases for well-known accounts, which is how SDDL
/// itself spells them.
///
/// TARGET-NEUTRAL on purpose, exactly as `src/control_transport.rs`'s sibling is and for the same
/// reason: the grammar is pure text while everything around it is a syscall that exists on one
/// target, so lifting it out means the DACL's SHAPE is asserted by an ordinary unit test on macOS
/// and Linux rather than resting on a Windows job that does not exist yet.
#[cfg(any(windows, test))]
fn owner_only_sddl(sid: &str, inheritance: Inheritance) -> String {
    let flags = match inheritance {
        Inheritance::No => "",
        Inheritance::Yes => "OICI",
    };
    format!("D:P(A;{flags};FA;;;SY)(A;{flags};FA;;;BA)(A;{flags};FA;;;{sid})")
}

/// The two-letter SDDL alias `ConvertSecurityDescriptorToStringSecurityDescriptorW` may render
/// `sid` as, or `None` if it renders no alias for it.
///
/// The read-back path compares two renderings produced by DIFFERENT calls.
/// `ConvertSecurityDescriptorToStringSecurityDescriptorW` substitutes a two-letter alias for a
/// well-known SID; `ConvertSidToStringSidW` — the call behind [`owner_only_deviation`]'s notion of
/// "us" — is documented always to produce the `S-1-…` form and never an alias. So a DACL this
/// module wrote correctly reads back with an ALIASED trustee where the account is a well-known
/// one, matches neither our `S-1-…` string nor the hard-coded floor, and
/// `src/roster_backup.rs` refuses a roster write over a formatting difference. That is the same
/// hazard [`owner_only_deviation`] already records for the rights MASK, one field over: it stopped
/// at the mask, and the trustee is rendered by the same call.
///
/// The fix is to accept the alias only for a SID that IS the aliased account, which is what this
/// maps. It can never widen the policy: an alias is admitted only when our own SID string is the
/// one it stands for, so a wrong entry here fails closed exactly as today.
///
/// **Deliberately narrow, and the residue is named rather than papered over.** The two mapped are
/// the domain-relative user RIDs — `LA` (the built-in Administrator, RID 500) and `LG` (Guest,
/// RID 501) — the only aliases a process token's USER SID can carry that the floor does not
/// already hard-code (`SY` is `S-1-5-18`, `BA` is `S-1-5-32-544`, and the elevated-owner case
/// resolves to that group). An alias outside this set would still read back as a stranger; nothing
/// here has run on Windows, so the rendering is REASONED from the documented API contract and
/// has been observed nowhere: **#1530** is the item that observes it and settles the set, and a
/// green `#[cfg(windows)]` run under an ordinary account will not settle it (an ordinary
/// account's SID carries no alias, so such a run passes either way).
///
/// Target-neutral, like the SDDL builder and the predicate it feeds: it is a suffix match on a
/// string, so it is graded by ordinary unit tests on macOS and Linux today.
#[cfg(any(windows, test))]
fn sddl_alias_for(sid: &str) -> Option<&'static str> {
    // A domain- or machine-relative account SID: `S-1-5-21-<48 bits of authority>-<RID>`. Matching
    // the prefix as well as the RID keeps `S-1-5-32-500`-shaped strings — a different authority —
    // out of it.
    if !sid.starts_with("S-1-5-21-") {
        return None;
    }
    match sid.rsplit_once('-') {
        Some((_, "500")) => Some("LA"),
        Some((_, "501")) => Some("LG"),
        _ => None,
    }
}

/// The first way `dacl` — a DACL rendered in SDDL — falls short of the owner-only policy, or
/// `None` if it does not.
///
/// `owners` is every SID string that counts as "us" for this check. It is a SET rather than one
/// value because ADR-0037 § Consequences records the case: a process holding a full Administrators
/// token, on a machine whose "default owner for objects created by members of the Administrators
/// group" policy names the group rather than the creator, creates objects owned by
/// `BUILTIN\Administrators` while its token user is still the account. `src/control_transport.rs`
/// accepts either for the same reason.
///
/// Each entry is matched BOTH as itself and as the two-letter alias the renderer may substitute
/// for it ([`sddl_alias_for`]) — the two sides of this comparison come from different calls with
/// different alias policies, and that helper's docs carry the mechanism and what it does not
/// cover. Callers pass the `S-1-…` form and nothing else.
///
/// Deliberately NOT a check of the rights mask — see [`owner_only_deviation`] for why a production
/// refusal path must not turn on how a mask was rendered.
///
/// Target-neutral, so the predicate AC3 is about is exercised on every target while the read that
/// feeds it runs only on Windows.
#[cfg(any(windows, test))]
fn owner_only_dacl_defect(dacl: &str, owners: &[&str]) -> Option<String> {
    let Some(rest) = dacl.strip_prefix("D:") else {
        return Some(format!("no DACL was rendered at all: {dacl:?}"));
    };
    let split = rest.find('(').unwrap_or(rest.len());
    let (flags, body) = rest.split_at(split);
    if !flags.contains('P') {
        return Some(format!(
            "the DACL is not PROTECTED, so it still inherits from its parent: {dacl:?}"
        ));
    }
    let mut seen = 0usize;
    for ace in body.split('(').skip(1) {
        let Some(ace) = ace.strip_suffix(')') else {
            return Some(format!("an ACE is not closed: {dacl:?}"));
        };
        seen += 1;
        let fields: Vec<&str> = ace.split(';').collect();
        // `(type;flags;rights;object_guid;inherit_object_guid;trustee)` — six fields, and this
        // reads the second and the last. A shorter split means a rendering this predicate cannot
        // reason about, which is a defect rather than a pass.
        if fields.len() < 6 {
            return Some(format!(
                "an ACE has {} fields, not 6: {dacl:?}",
                fields.len()
            ));
        }
        if fields[1].contains("ID") {
            return Some(format!(
                "ACE {seen} is INHERITED, so this DACL is the parent's rather than its own: \
                 {dacl:?}"
            ));
        }
        let trustee = fields[5];
        // Matched as written AND as the alias the renderer may have substituted — see
        // [`sddl_alias_for`]. `SY` / `BA` are the floor's own aliases, which is how the renderer
        // spells the two SIDs this module writes there.
        let is_ours =
            owners.contains(&trustee) || owners.iter().any(|o| sddl_alias_for(o) == Some(trustee));
        if !(trustee == "SY" || trustee == "BA" || is_ours) {
            return Some(format!(
                "ACE {seen} names {trustee}, who is neither the owner nor the \
                 SYSTEM/Administrators floor: {dacl:?}"
            ));
        }
    }
    if seen == 0 {
        // An empty DACL denies everyone, including us. It is not a widening, but it is not the
        // policy either, and reporting it beats a caller puzzling over an access denial later.
        return Some(format!("the DACL has no ACEs at all: {dacl:?}"));
    }
    None
}

#[cfg(windows)]
mod windows_impl {
    //! The Win32 half: read and write a file object's DACL and owner.
    //!
    //! Every call is `advapi32` through `windows-sys`, which is ALREADY a dependency of this crate
    //! on this target (`src/control_transport.rs` has used its security surface since #1513), so
    //! #974's "do not add a dependency to get this" holds with nothing added. The two SID-string
    //! shims at the foot are deliberate near-duplicates of `control_transport`'s: ten lines of FFI
    //! marshalling, and hoisting them into a shared module would put a generic Win32 helper inside
    //! a transport or invent a third module to hold two functions.

    use std::ffi::c_void;
    use std::io;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    use windows_sys::Win32::Foundation::{GetLastError, LocalFree, ERROR_SUCCESS};
    use windows_sys::Win32::Security::Authorization::{
        ConvertSecurityDescriptorToStringSecurityDescriptorW, ConvertSidToStringSidW,
        ConvertStringSecurityDescriptorToSecurityDescriptorW, GetNamedSecurityInfoW,
        SetNamedSecurityInfoW, SDDL_REVISION_1, SE_FILE_OBJECT,
    };
    use windows_sys::Win32::Security::{
        GetSecurityDescriptorControl, GetSecurityDescriptorDacl, ACL, DACL_SECURITY_INFORMATION,
        OWNER_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
        SE_DACL_PROTECTED, UNPROTECTED_DACL_SECURITY_INFORMATION,
    };

    use super::{owner_only_dacl_defect, owner_only_sddl, Inheritance};

    /// Write this crate's explicit, protected owner-only DACL onto `path`.
    ///
    /// FAIL CLOSED at every stage: a failure to read the token, to parse the SDDL or to apply the
    /// descriptor returns `Err`, and the caller aborts whatever it was doing rather than leaving a
    /// file behind at the parent's permissions.
    pub(super) fn write_owner_only_dacl(path: &Path, inheritance: Inheritance) -> io::Result<()> {
        let sddl = owner_only_sddl(&crate::control_transport::our_user_sid()?, inheritance);
        let descriptor = security_descriptor_from_sddl(&sddl)?;
        let result = apply_dacl_from(descriptor, path, PROTECTED_DACL_SECURITY_INFORMATION);
        // SAFETY: `descriptor` is exactly the `LocalAlloc`-ed pointer the converter returned, and
        // `SetNamedSecurityInfoW` copies what it is given, so it is dead by here. Freed once.
        unsafe { LocalFree(descriptor.cast::<c_void>()) };
        result
    }

    /// Copy `from`'s DACL onto `to`, protection flag included ([`super::copy_policy`]).
    pub(super) fn copy_dacl(from: &Path, to: &Path) -> io::Result<()> {
        let (descriptor, _) = named_security_info(from, DACL_SECURITY_INFORMATION)?;
        let protection = if is_protected(descriptor) {
            PROTECTED_DACL_SECURITY_INFORMATION
        } else {
            UNPROTECTED_DACL_SECURITY_INFORMATION
        };
        let result = apply_dacl_from(descriptor, to, protection);
        // SAFETY: the descriptor `GetNamedSecurityInfoW` allocated, freed once, after the copy the
        // setter made of the ACL inside it.
        unsafe { LocalFree(descriptor.cast::<c_void>()) };
        result
    }

    /// Whether `path`'s owner SID is one this process may call its own.
    pub(super) fn owner_is_us(path: &Path) -> io::Result<bool> {
        let (descriptor, owner) = named_security_info(path, OWNER_SECURITY_INFORMATION)?;
        let rendered = (!owner.is_null())
            .then(|| sid_to_string(owner))
            .transpose()?;
        // SAFETY: the descriptor the call allocated; `owner` pointed INTO it and has been rendered
        // to an owned `String` above, so nothing borrows it past here. Freed once.
        unsafe { LocalFree(descriptor.cast::<c_void>()) };
        let Some(rendered) = rendered else {
            return Err(io::Error::other(format!(
                "GetNamedSecurityInfoW({}) reported no owner SID",
                path.display()
            )));
        };
        Ok(rendered == crate::control_transport::our_user_sid()?
            || rendered == crate::control_transport::our_owner_sid()?)
    }

    /// How `path`'s DACL deviates from the owner-only policy ([`super::owner_only_deviation`]).
    pub(super) fn dacl_deviation(path: &Path) -> io::Result<Option<String>> {
        let rendered = rendered_dacl(path)?;
        let user = crate::control_transport::our_user_sid()?;
        let owner = crate::control_transport::our_owner_sid()?;
        Ok(owner_only_dacl_defect(
            &rendered,
            &[user.as_str(), owner.as_str()],
        ))
    }

    /// `path`'s DACL, rendered back into SDDL — the read the predicate above grades, and what the
    /// `#[cfg(windows)]` tests assert against.
    pub(super) fn rendered_dacl(path: &Path) -> io::Result<String> {
        let (descriptor, _) = named_security_info(path, DACL_SECURITY_INFORMATION)?;
        let rendered = descriptor_to_sddl(descriptor, DACL_SECURITY_INFORMATION);
        // SAFETY: the descriptor the call allocated, rendered to an owned `String` above. Freed
        // once.
        unsafe { LocalFree(descriptor.cast::<c_void>()) };
        rendered
    }

    /// Pull the DACL out of `descriptor` and write it onto `path` with `protection`.
    ///
    /// `protection` is `PROTECTED_DACL_SECURITY_INFORMATION` or its unprotected twin, OR-ed
    /// alongside `DACL_SECURITY_INFORMATION`; passing the protection bit is what makes the write a
    /// CEILING rather than a floor, so it is a parameter rather than a constant only because
    /// [`copy_dacl`] must be able to reproduce an unprotected source faithfully.
    fn apply_dacl_from(
        descriptor: PSECURITY_DESCRIPTOR,
        path: &Path,
        protection: u32,
    ) -> io::Result<()> {
        let mut present = 0;
        let mut acl: *mut ACL = std::ptr::null_mut();
        let mut defaulted = 0;
        // SAFETY: `descriptor` is a valid self-relative security descriptor; the three
        // out-parameters are live locals.
        let ok = unsafe {
            GetSecurityDescriptorDacl(descriptor, &mut present, &mut acl, &mut defaulted)
        };
        if ok == 0 {
            // SAFETY: reads the last-error slot set by the call above.
            let code = unsafe { GetLastError() };
            return Err(io::Error::other(format!(
                "GetSecurityDescriptorDacl failed: GetLastError={code}"
            )));
        }
        // A descriptor with no DACL grants EVERYONE full access — the one outcome that would
        // silently undo this whole item while every call above returned success. Refused rather
        // than reasoned about, exactly as `control_transport`'s null-descriptor check is.
        if present == 0 || acl.is_null() {
            return Err(io::Error::other(
                "the security descriptor carries no DACL; applying it would grant everyone full \
                 access",
            ));
        }
        let mut wide = wide(path);
        // SAFETY: `wide` is a live NUL-terminated UTF-16 path; `acl` points into the live
        // `descriptor`; the SID and SACL parameters are null, which is documented as "leave that
        // information alone".
        let status = unsafe {
            SetNamedSecurityInfoW(
                wide.as_mut_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | protection,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                acl,
                std::ptr::null(),
            )
        };
        if status != ERROR_SUCCESS {
            return Err(io::Error::other(format!(
                "SetNamedSecurityInfoW({}) failed: WIN32_ERROR={status}",
                path.display()
            )));
        }
        Ok(())
    }

    /// `GetNamedSecurityInfoW` for one information class, returning the allocated descriptor and
    /// whichever pointer INTO it the class asked for.
    ///
    /// The CALLER owns the descriptor and must `LocalFree` it; the second pointer is borrowed from
    /// inside it and is only valid until then.
    fn named_security_info(
        path: &Path,
        info: u32,
    ) -> io::Result<(PSECURITY_DESCRIPTOR, *mut c_void)> {
        let wide = wide(path);
        let mut owner: *mut c_void = std::ptr::null_mut();
        let mut dacl: *mut ACL = std::ptr::null_mut();
        let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        // SAFETY: `wide` is a live NUL-terminated UTF-16 path; every out-parameter is a live
        // local; the group and SACL out-parameters are null, which the API documents as "do not
        // report that".
        let status = unsafe {
            GetNamedSecurityInfoW(
                wide.as_ptr(),
                SE_FILE_OBJECT,
                info,
                &mut owner,
                std::ptr::null_mut(),
                &mut dacl,
                std::ptr::null_mut(),
                &mut descriptor,
            )
        };
        if status != ERROR_SUCCESS {
            return Err(io::Error::other(format!(
                "GetNamedSecurityInfoW({}) failed: WIN32_ERROR={status}",
                path.display()
            )));
        }
        if descriptor.is_null() {
            // SUCCESS with a null descriptor is a contract violation, and reading on from it would
            // be a null deref inside `advapi32` rather than an error this crate could report.
            return Err(io::Error::other(format!(
                "GetNamedSecurityInfoW({}) returned success with a null descriptor",
                path.display()
            )));
        }
        Ok((descriptor, owner))
    }

    /// Whether `descriptor`'s DACL carries `SE_DACL_PROTECTED`.
    ///
    /// A read that cannot fail usefully: a descriptor this module obtained from
    /// `GetNamedSecurityInfoW` is valid by construction, and the only honest answer if the control
    /// read fails anyway is "not protected", which is the conservative half — [`copy_dacl`] then
    /// reproduces the source as unprotected, never as a protection this crate invented.
    fn is_protected(descriptor: PSECURITY_DESCRIPTOR) -> bool {
        let mut control: u16 = 0;
        let mut revision: u32 = 0;
        // SAFETY: `descriptor` is a valid security descriptor; both out-parameters are live
        // locals.
        let ok = unsafe { GetSecurityDescriptorControl(descriptor, &mut control, &mut revision) };
        ok != 0 && control & SE_DACL_PROTECTED != 0
    }

    /// `descriptor`'s `info` classes rendered as an SDDL string.
    fn descriptor_to_sddl(descriptor: PSECURITY_DESCRIPTOR, info: u32) -> io::Result<String> {
        let mut wide: *mut u16 = std::ptr::null_mut();
        // SAFETY: `descriptor` is a valid security descriptor; `wide` is a live local the API
        // writes only on success; a null length out-parameter is documented as "do not report the
        // length".
        let ok = unsafe {
            ConvertSecurityDescriptorToStringSecurityDescriptorW(
                descriptor,
                SDDL_REVISION_1,
                info,
                &mut wide,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            // SAFETY: reads the last-error slot set by the call above.
            let code = unsafe { GetLastError() };
            return Err(io::Error::other(format!(
                "ConvertSecurityDescriptorToStringSecurityDescriptorW failed: GetLastError={code}"
            )));
        }
        if wide.is_null() {
            return Err(io::Error::other(
                "ConvertSecurityDescriptorToStringSecurityDescriptorW returned success with a \
                 null string",
            ));
        }
        // SAFETY: on success `wide` is a valid NUL-terminated UTF-16 string allocated with
        // `LocalAlloc`.
        let rendered = unsafe { wide_to_string(wide) };
        // SAFETY: exactly the `LocalAlloc`-ed pointer the call returned, freed once.
        unsafe { LocalFree(wide.cast::<c_void>()) };
        Ok(rendered)
    }

    /// A self-relative security descriptor built from an SDDL string; the CALLER owns the
    /// `LocalAlloc`-ed result.
    ///
    /// The null check after a TRUE return is the one place this path could fail OPEN, and it is
    /// checked rather than reasoned about — the same refusal `src/control_transport.rs` makes at
    /// its own converter, and for the same reason: a null descriptor MEANS "default security" to
    /// everything downstream.
    fn security_descriptor_from_sddl(sddl: &str) -> io::Result<PSECURITY_DESCRIPTOR> {
        let wide: Vec<u16> = sddl.encode_utf16().chain(std::iter::once(0)).collect();
        let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        // SAFETY: `wide` is a live NUL-terminated UTF-16 buffer that outlives the call;
        // `descriptor` is a live local the API writes only on success; a null size out-parameter
        // is documented as "do not report the size".
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                wide.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            // SAFETY: reads the last-error slot set by the call above.
            let code = unsafe { GetLastError() };
            return Err(io::Error::other(format!(
                "ConvertStringSecurityDescriptorToSecurityDescriptorW({sddl}) failed: \
                 GetLastError={code}"
            )));
        }
        if descriptor.is_null() {
            return Err(io::Error::other(format!(
                "ConvertStringSecurityDescriptorToSecurityDescriptorW({sddl}) returned success \
                 with a null descriptor"
            )));
        }
        Ok(descriptor)
    }

    /// `path` as a NUL-terminated UTF-16 buffer, which is what every named Win32 call here wants.
    fn wide(path: &Path) -> Vec<u16> {
        path.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    /// `ConvertSidToStringSidW`, with the `LocalFree` the API requires of its caller.
    fn sid_to_string(sid: *mut c_void) -> io::Result<String> {
        let mut wide: *mut u16 = std::ptr::null_mut();
        // SAFETY: `sid` is a non-null pointer to a valid SID inside a live descriptor (checked by
        // the caller); `wide` is a live local the API writes only on success.
        if unsafe { ConvertSidToStringSidW(sid, &mut wide) } == 0 {
            // SAFETY: reads the last-error slot set by the call above.
            let code = unsafe { GetLastError() };
            return Err(io::Error::other(format!(
                "ConvertSidToStringSidW failed: GetLastError={code}"
            )));
        }
        // SAFETY: on success `wide` is a valid NUL-terminated UTF-16 string allocated with
        // `LocalAlloc`.
        let rendered = unsafe { wide_to_string(wide) };
        // SAFETY: exactly the `LocalAlloc`-ed pointer the call returned, freed once.
        unsafe { LocalFree(wide.cast::<c_void>()) };
        Ok(rendered)
    }

    /// A NUL-terminated UTF-16 Win32 string as a Rust `String`.
    ///
    /// # Safety
    ///
    /// `ptr` must be non-null and point at a NUL-terminated UTF-16 sequence that stays valid for
    /// the duration of the call.
    unsafe fn wide_to_string(ptr: *const u16) -> String {
        let mut len = 0usize;
        // SAFETY: the caller guarantees a NUL terminator, so this walk stops inside the
        // allocation.
        while unsafe { *ptr.add(len) } != 0 {
            len += 1;
        }
        // SAFETY: `ptr[..len]` is exactly the sequence walked above, all within the caller's
        // allocation.
        String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(ptr, len) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A user SID of the shape `ConvertSidToStringSidW` renders. Not a real account — it is a
    /// fixture for the pure string machinery, which never resolves it.
    const SID: &str = "S-1-5-21-1004336348-1177238915-682003330-1001";

    /// Every SID string this process may call its own, for the predicate's `owners` argument.
    fn ours() -> [&'static str; 1] {
        [SID]
    }

    // --- The SDDL this module writes (issue #974 AC1, AC2) -------------------
    //
    // Target-neutral: the grammar is pure text, so the DACL's SHAPE is graded on macOS and Linux
    // rather than waiting on a Windows job that does not exist yet. `src/control_transport.rs`
    // makes the same split for the pipe's descriptor and records why.

    #[test]
    fn the_file_dacl_is_protected_and_names_exactly_the_floor_plus_the_owner() {
        let sddl = owner_only_sddl(SID, Inheritance::No);
        assert_eq!(
            sddl,
            format!("D:P(A;;FA;;;SY)(A;;FA;;;BA)(A;;FA;;;{SID})"),
            "the file DACL is three allow ACEs and the P flag; anything else is a different policy"
        );
        // `P` is the whole item. Without it these ACEs are a FLOOR the parent directory can
        // widen, which is precisely the inherited-only state the #40 recon found.
        assert!(
            sddl.starts_with("D:P"),
            "an unprotected DACL still inherits from its parent: {sddl}"
        );
        // No inheritance flags on a FILE: an ACE that propagates from a leaf object is
        // meaningless, and writing one would make the file and directory forms indistinguishable.
        assert!(
            !sddl.contains("OICI"),
            "a file's ACEs must not be inheritable: {sddl}"
        );
    }

    #[test]
    fn the_directory_dacl_differs_from_the_file_one_only_in_inheritance() {
        let file = owner_only_sddl(SID, Inheritance::No);
        let dir = owner_only_sddl(SID, Inheritance::Yes);
        assert_ne!(file, dir, "the two forms must not collapse into one");
        assert_eq!(
            dir.replace("OICI", ""),
            file,
            "inheritance is the ONLY axis on which the directory form may differ — same P flag, \
             same three trustees, same rights"
        );
        assert_eq!(
            dir.matches("OICI").count(),
            3,
            "every ACE inherits, or a child gets a partial policy: {dir}"
        );
    }

    #[test]
    fn the_trustee_is_the_sid_it_is_given_rather_than_a_constant() {
        // The provenance half of AC2, asserted behaviourally: a builder that ignored its argument
        // and baked a SID would return the same string for two different callers.
        let other = "S-1-5-21-9999999999-9999999999-9999999999-1002";
        assert_ne!(
            owner_only_sddl(SID, Inheritance::No),
            owner_only_sddl(other, Inheritance::No)
        );
        assert!(owner_only_sddl(other, Inheritance::No).contains(other));
    }

    #[test]
    fn the_floor_is_exactly_localsystem_and_the_administrators_group() {
        let sddl = owner_only_sddl(SID, Inheritance::No);
        // The two the #40 recon measured as unavoidable, in the two-letter aliases SDDL itself
        // uses. Their ABSENCE would be a policy this crate has no business inventing; anything
        // ELSE present would be the exposure AC2 forbids.
        assert!(sddl.contains("(A;;FA;;;SY)"), "LocalSystem: {sddl}");
        assert!(sddl.contains("(A;;FA;;;BA)"), "Administrators: {sddl}");
        for stranger in [";WD)", ";AU)", ";BU)", ";WD;", ";AU;", ";BU;"] {
            assert!(
                !sddl.contains(stranger),
                "`{stranger}` names Everyone, Authenticated Users or Users — the accounts the \
                 recon found ABSENT and which this policy must not add: {sddl}"
            );
        }
    }

    // --- The read-back predicate (issue #974 AC3's core) ---------------------

    #[test]
    fn what_this_module_writes_passes_its_own_read_back() {
        // The writer and the reader are two independent statements of one policy, and nothing
        // else in this file makes them agree. Without this, a DACL could be written correctly and
        // graded as a deviation on every read — a production refusal path that refuses everything.
        for inheritance in [Inheritance::No, Inheritance::Yes] {
            let sddl = owner_only_sddl(SID, inheritance);
            assert_eq!(
                owner_only_dacl_defect(&sddl, &ours()),
                None,
                "{sddl} is what this module writes and must not read back as a deviation"
            );
        }
    }

    #[test]
    fn an_inherited_ace_is_rejected_however_correct_its_trustees_are() {
        // AC3 in one assertion. This DACL grants EXACTLY the accounts the policy allows and
        // nobody else — an "the owner can read it, and only the owner" check passes it — and it
        // is still the wrong answer, because the ACEs are the PARENT's. That distinction is the
        // whole of #974's Windows half.
        let inherited = format!("D:PAI(A;ID;FA;;;SY)(A;ID;FA;;;BA)(A;ID;FA;;;{SID})");
        let defect = owner_only_dacl_defect(&inherited, &ours())
            .expect("an inherited DACL is not this policy, however permissive it looks");
        assert!(
            defect.contains("INHERITED"),
            "the report must say WHICH property failed: {defect}"
        );
    }

    #[test]
    fn an_unprotected_dacl_is_rejected() {
        // No `P`: the three ACEs below are a floor the parent can widen at any time, which is
        // exactly the state the item exists to leave behind.
        let unprotected = format!("D:(A;;FA;;;SY)(A;;FA;;;BA)(A;;FA;;;{SID})");
        let defect =
            owner_only_dacl_defect(&unprotected, &ours()).expect("an unprotected DACL is a floor");
        assert!(defect.contains("PROTECTED"), "{defect}");
    }

    #[test]
    fn a_stranger_in_the_dacl_is_rejected() {
        for stranger in ["WD", "AU", "BU", "S-1-5-21-1-2-3-1002"] {
            let widened = format!("D:P(A;;FA;;;SY)(A;;FA;;;BA)(A;;FA;;;{SID})(A;;FA;;;{stranger})");
            let defect = owner_only_dacl_defect(&widened, &ours())
                .unwrap_or_else(|| panic!("{stranger} must not be admitted: {widened}"));
            assert!(defect.contains(stranger), "{defect}");
        }
    }

    #[test]
    fn the_elevated_administrators_owner_case_is_admitted() {
        // ADR-0037 § Consequences: a process holding a full Administrators token, on a machine
        // whose default-owner policy names the group, creates objects owned by
        // `BUILTIN\Administrators` while its token user is still the account. Both SIDs are read
        // off THIS process's own token, so admitting either never widens to somebody else.
        let owner_sid = "S-1-5-32-544";
        let sddl = owner_only_sddl(owner_sid, Inheritance::No);
        assert_eq!(owner_only_dacl_defect(&sddl, &[SID, owner_sid]), None);
        // …and only because it was offered: the same DACL against the user SID alone is a
        // stranger, so the widening is the caller's to grant rather than the predicate's to
        // assume.
        assert!(owner_only_dacl_defect(&sddl, &ours()).is_some());
    }

    #[test]
    fn an_aliased_owner_ace_is_admitted_only_for_the_account_it_aliases() {
        // The read-back's two sides come from different calls:
        // `ConvertSecurityDescriptorToStringSecurityDescriptorW` substitutes a two-letter alias
        // for a well-known SID, while `ConvertSidToStringSidW` — how this crate learns its own
        // SIDs — never does. Under the built-in Administrator account, a DACL this module wrote
        // correctly therefore reads back with `LA` in the owner ACE, and before this was handled
        // `src/roster_backup.rs` refused a real roster write over the rendering.
        let admin = "S-1-5-21-1004336348-1177238915-682003330-500";
        let guest = "S-1-5-21-1004336348-1177238915-682003330-501";
        for (sid, alias) in [(admin, "LA"), (guest, "LG")] {
            let aliased = format!("D:P(A;;FA;;;SY)(A;;FA;;;BA)(A;;FA;;;{alias})");
            assert_eq!(
                owner_only_dacl_defect(&aliased, &[sid]),
                None,
                "{alias} is how the renderer spells {sid}"
            );
            // The unaliased rendering of the same account keeps working — the acceptance is
            // additive, not a substitution.
            let plain = owner_only_sddl(sid, Inheritance::No);
            assert_eq!(owner_only_dacl_defect(&plain, &[sid]), None);
        }
        // And it never widens: an alias is admitted only when OUR OWN SID is the one it stands
        // for. Offered any other SID, `LA` is the stranger it would have been all along.
        let aliased = "D:P(A;;FA;;;SY)(A;;FA;;;BA)(A;;FA;;;LA)";
        assert!(
            owner_only_dacl_defect(aliased, &ours()).is_some(),
            "LA must stay a stranger to an account it does not alias"
        );
        assert!(owner_only_dacl_defect(aliased, &[guest]).is_some());
    }

    #[test]
    fn only_the_two_domain_relative_user_rids_carry_an_alias() {
        // Narrow by design, and the boundary is what the test pins: a wrong entry here could
        // never widen the policy (the alias is matched against our own SID), but a MISSING one is
        // the false-refusal above, so the mapped set is stated rather than assumed.
        assert_eq!(
            sddl_alias_for("S-1-5-21-1004336348-1177238915-682003330-500"),
            Some("LA")
        );
        assert_eq!(
            sddl_alias_for("S-1-5-21-1004336348-1177238915-682003330-501"),
            Some("LG")
        );
        for other in [
            SID,
            // The floor's own SIDs: already hard-coded in the predicate, never routed here.
            "S-1-5-18",
            "S-1-5-32-544",
            // A different authority that merely ENDS in an aliased RID.
            "S-1-5-32-500",
            "S-1-5-21-1-2-3-1002",
            // Degenerate shapes must not panic or match.
            "",
            "S-1-5-21-",
            "LA",
        ] {
            assert_eq!(sddl_alias_for(other), None, "{other:?}");
        }
    }

    #[test]
    fn a_dacl_that_is_missing_absent_or_unparseable_is_a_defect_not_a_pass() {
        // Three failure-open shapes. `NO_ACCESS_CONTROL` and an absent DACL both mean "everyone,
        // full control" to Windows; an ACE this predicate cannot parse is a rendering it has no
        // business grading as clean.
        for hopeless in [
            "",
            "O:BAG:BA",
            "D:NO_ACCESS_CONTROL",
            "D:P(A;;FA;;;SY",
            "D:P(A;;FA)",
        ] {
            assert!(
                owner_only_dacl_defect(hopeless, &ours()).is_some(),
                "{hopeless:?} must not read back as the owner-only policy"
            );
        }
        // An empty PROTECTED DACL denies everyone, us included. Not a widening, but not the
        // policy either, and a caller deserves to hear it here rather than at an access denial.
        assert!(owner_only_dacl_defect("D:P", &ours()).is_some());
    }

    // --- The Unix mechanism (issue #974 AC4) ---------------------------------

    #[cfg(unix)]
    mod unix {
        use super::*;

        use std::os::unix::fs::PermissionsExt;

        fn mode_of(path: &Path) -> u32 {
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777
        }

        #[test]
        fn open_owner_only_creates_at_0600() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("f");
            let _file =
                open_owner_only(OpenOptions::new().create(true).write(true), &path).unwrap();
            assert_eq!(mode_of(&path), FILE_MODE);
        }

        #[test]
        fn open_owner_only_leaves_an_existing_file_alone() {
            // `open(2)` applies a creation mode only when it creates. Pinned because the Windows
            // arm deliberately CANNOT keep that promise — see the module docs — and a reader
            // comparing the two needs the Unix half to be a stated property rather than a habit.
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("f");
            std::fs::write(&path, b"x").unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
            let _file = open_owner_only(OpenOptions::new().write(true), &path).unwrap();
            assert_eq!(mode_of(&path), 0o644);
        }

        #[test]
        fn owner_only_file_and_dir_are_0600_and_0700() {
            let dir = tempfile::tempdir().unwrap();
            let file = dir.path().join("f");
            std::fs::write(&file, b"x").unwrap();
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o666)).unwrap();
            owner_only_file(&file).unwrap();
            assert_eq!(mode_of(&file), FILE_MODE);

            let nested = dir.path().join("d");
            std::fs::create_dir(&nested).unwrap();
            std::fs::set_permissions(&nested, std::fs::Permissions::from_mode(0o777)).unwrap();
            owner_only_dir(&nested).unwrap();
            assert_eq!(mode_of(&nested), DIR_MODE);
        }

        #[test]
        fn copy_policy_carries_the_source_mode_including_the_high_bits() {
            let dir = tempfile::tempdir().unwrap();
            let from = dir.path().join("from");
            let to = dir.path().join("to");
            std::fs::write(&from, b"a").unwrap();
            std::fs::write(&to, b"b").unwrap();
            // `1644` — the sticky bit alongside an ordinary mode, so a `& 0o777` mask anywhere in
            // the copy path shows up as a lost bit rather than passing unnoticed.
            std::fs::set_permissions(&from, std::fs::Permissions::from_mode(0o1644)).unwrap();
            copy_policy(&from, &to).unwrap();
            assert_eq!(
                std::fs::metadata(&to).unwrap().permissions().mode() & 0o7777,
                0o1644
            );
        }

        #[test]
        fn owner_only_deviation_reports_the_mode_it_found() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("f");
            std::fs::write(&path, b"x").unwrap();

            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
            let deviation = owner_only_deviation(&path).unwrap().expect("0644 is wider");
            assert!(deviation.contains("644"), "{deviation}");

            owner_only_file(&path).unwrap();
            assert_eq!(owner_only_deviation(&path).unwrap(), None);
        }

        #[test]
        fn ownership_is_read_through_the_link_and_around_it() {
            let dir = tempfile::tempdir().unwrap();
            let target = dir.path().join("target");
            std::fs::write(&target, b"x").unwrap();
            let link = dir.path().join("link");
            std::os::unix::fs::symlink(&target, &link).unwrap();

            // Both answer "yes" here — this test host owns both the link and its target — so what
            // it pins is that the two functions ASK about different objects, which is what makes
            // the no-follow one worth having at all.
            assert!(owner_is_current_user(&link).unwrap());
            assert!(owner_is_current_user_nofollow(&link).unwrap());
            let broken = dir.path().join("broken");
            std::os::unix::fs::symlink(dir.path().join("gone"), &broken).unwrap();
            assert!(
                owner_is_current_user(&broken).is_err(),
                "the following form stats the TARGET, which is absent"
            );
            assert!(
                owner_is_current_user_nofollow(&broken).unwrap(),
                "the no-follow form lstats the LINK, which exists and is ours"
            );
        }
    }

    // --- The Windows mechanism (issue #974 AC3) ------------------------------
    //
    // COMMITTED but never RUN: no CI job compiles this crate for Windows, so these are what
    // execute the moment **#978** lands, and AC3's execution half is deferred to it. They are
    // written against the documented API contract, not against an observation.

    #[cfg(windows)]
    mod windows {
        use super::*;

        use crate::control_transport::our_user_sid;

        #[test]
        fn owner_only_file_writes_an_explicit_dacl_rather_than_an_inherited_one() {
            let dir = tempfile::tempdir().expect("tempdir");
            let path = dir.path().join("private");
            std::fs::write(&path, b"secret").expect("write");

            // Recorded, never asserted on: what a fresh file in the runner's temp inherits is the
            // environment's business, and a test that pinned it would be measuring the runner
            // rather than this module. It rides in the failure message below because it is the
            // first thing a reader of a red run wants.
            let before = windows_impl::rendered_dacl(&path).expect("read the DACL before");

            owner_only_file(&path).expect("write the owner-only DACL");
            let after = windows_impl::rendered_dacl(&path).expect("read the DACL back");

            // AC3, and the reason it says "asserting only that the owner can read it does not
            // satisfy this": every assertion here is about the DACL's OWN shape.
            assert!(
                after.starts_with("D:P"),
                "the DACL must be PROTECTED, or it is a floor the profile directory can widen \
                 (before={before:?}, after={after:?})"
            );
            for ace in after.split('(').skip(1) {
                let flags = ace.split(';').nth(1).unwrap_or("");
                assert!(
                    !flags.contains("ID"),
                    "an INHERITED ACE survived: this DACL is still the parent's \
                     (before={before:?}, after={after:?})"
                );
            }
            // The read really came off this file, rather than from anything canned: our own token
            // user is in it.
            let sid = our_user_sid().expect("our token user");
            assert!(
                after.contains(&sid),
                "the trustee must be this process's own token user (after={after:?})"
            );
            // …and the whole predicate agrees, which is what `roster_backup` will act on.
            assert_eq!(
                owner_only_deviation(&path).expect("grade it"),
                None,
                "before={before:?}, after={after:?}"
            );
        }

        #[test]
        fn owner_only_dir_writes_an_inheritable_explicit_dacl() {
            let dir = tempfile::tempdir().expect("tempdir");
            let path = dir.path().join("private");
            std::fs::create_dir(&path).expect("create_dir");

            owner_only_dir(&path).expect("write the owner-only DACL");
            let rendered = windows_impl::rendered_dacl(&path).expect("read the DACL back");

            assert!(rendered.starts_with("D:P"), "{rendered}");
            let aces: Vec<&str> = rendered.split('(').skip(1).collect();
            assert!(!aces.is_empty(), "{rendered}");
            for ace in aces {
                let flags = ace.split(';').nth(1).unwrap_or("");
                assert!(
                    !flags.contains("ID"),
                    "an INHERITED ACE survived: {rendered}"
                );
                assert!(
                    flags.contains("OI") && flags.contains("CI"),
                    "a directory's ACEs must reach its children, or a file created inside starts \
                     at the profile's defaults: {rendered}"
                );
            }
        }

        #[test]
        fn a_file_created_inside_an_owner_only_dir_still_gets_its_own_explicit_dacl() {
            // The layering the module docs claim: the directory covers the window between
            // `CreateFileW` and the DACL write, and the file's OWN explicit DACL is what replaces
            // the inherited ACEs afterwards. Both halves, in the order production runs them.
            let dir = tempfile::tempdir().expect("tempdir");
            let nested = dir.path().join("private");
            std::fs::create_dir(&nested).expect("create_dir");
            owner_only_dir(&nested).expect("tighten the directory first");

            let path = nested.join("state");
            let _file = open_owner_only(OpenOptions::new().create(true).write(true), &path)
                .expect("open owner-only");

            let rendered = windows_impl::rendered_dacl(&path).expect("read the DACL back");
            assert!(
                rendered.starts_with("D:P"),
                "inheriting the right ACEs is not the same as carrying them: {rendered}"
            );
            for ace in rendered.split('(').skip(1) {
                let flags = ace.split(';').nth(1).unwrap_or("");
                assert!(
                    !flags.contains("ID"),
                    "an INHERITED ACE survived: {rendered}"
                );
            }
        }

        #[test]
        fn copy_policy_reproduces_an_unprotected_source_as_unprotected() {
            // `write_preserving_mode` co-writes a file owned by Claude Code. Turning ITS
            // inherited DACL into a protected one would freeze the user's file against a profile
            // change they may want — a narrowing, but still not the policy they had.
            let dir = tempfile::tempdir().expect("tempdir");
            let from = dir.path().join("theirs");
            let to = dir.path().join("staging");
            std::fs::write(&from, b"a").expect("write");
            std::fs::write(&to, b"b").expect("write");
            // The staging file starts PROTECTED, so a `copy_policy` that silently left the
            // protection flag alone would pass without this.
            owner_only_file(&to).expect("protect the staging file first");

            let source = windows_impl::rendered_dacl(&from).expect("read the source DACL");
            copy_policy(&from, &to).expect("copy the policy");
            let copied = windows_impl::rendered_dacl(&to).expect("read the copy back");

            assert_eq!(
                copied.starts_with("D:P"),
                source.starts_with("D:P"),
                "protection must be copied, not invented or dropped (source={source:?}, \
                 copied={copied:?})"
            );
        }

        #[test]
        fn a_file_we_just_created_is_owned_by_us() {
            let dir = tempfile::tempdir().expect("tempdir");
            let path = dir.path().join("ours");
            std::fs::write(&path, b"x").expect("write");
            assert!(owner_is_current_user(&path).expect("read the owner"));
            assert!(owner_is_current_user_nofollow(&path).expect("read the owner"));
        }
    }
}
