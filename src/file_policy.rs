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
//!   [`open_owner_only`] therefore creates and then tightens. What covers the gap between the two
//!   is the PARENT: every such file is created inside a directory this same module has already
//!   made explicitly owner-only, so the transient inherits from a DACL we control rather than
//!   from the profile's.
//! - **Unix applies a creation mode only when it creates; Windows applies the DACL either way.**
//!   `open(2)` ignores `mode` for an existing file, and [`open_owner_only`] keeps that. On Windows
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
//! — plus the "preserve what is already there" case ([`copy_policy`]) that the swap engine needs
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
        if !(trustee == "SY" || trustee == "BA" || owners.contains(&trustee)) {
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
