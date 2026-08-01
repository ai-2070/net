//! Owner-only payment files, on every platform.
//!
//! The engine store holds base64 of preserved x402 payloads — signed
//! EIP-3009 authorizations, which are bearer instruments — and the
//! billing log holds the signed usage record. On unix both are created
//! `0600` through `OpenOptions::mode`. Windows has no mode bits, so the
//! same files were created with whatever the parent directory's ACL
//! happened to grant.
//!
//! In practice `%LOCALAPPDATA%` is already user-scoped, so the *default*
//! path was not exposed. That is a property of where the file happens to
//! live, though, not of how it was created — an operator who supplies a
//! custom `state_path` under a shared directory got no protection and no
//! warning.
//!
//! ## Why creation, not `SetNamedSecurityInfoW`
//!
//! An earlier version of this module created the file and then replaced
//! its DACL. That is not sufficient, and the reason is the part worth
//! remembering: **Windows evaluates access when a handle is opened, not
//! when it is used.** A reader that opened the file during the window
//! before the DACL landed keeps the access it was granted, for the life
//! of its handle — tightening the ACL afterwards does not revoke it. So
//! a shared-directory observer could hold a read handle from the empty
//! file and read every authorization written into it later.
//!
//! The descriptor therefore has to be present at `CreateFileW` time,
//! which is what [`create_owner_only`] does. There is no window to race.
//!
//! ## Why the caller gets a handle, never a repaired pathname
//!
//! Everything here returns an open [`std::fs::File`], and the caller
//! writes through *that*. Securing a name and then reopening it is not
//! the same operation: between the two calls the name can be replaced —
//! with a permissive file, or with a symlink pointing somewhere else
//! entirely — and the write lands outside the guarantee that was just
//! established. A handle refers to the object, not to the path, so
//! nothing can be substituted underneath it.
//!
//! The same reasoning is why the repair path
//! ([`open_append_owner_only`]) refuses a symlink outright and applies
//! its permission change to the handle rather than to the pathname.
//!
//! Failure is loud. A file whose permissions could not be established is
//! not one to write bearer material into, so the caller surfaces the
//! error rather than continuing.

use std::path::Path;

/// Create `path` for writing, readable and writable only by its owner.
///
/// Fails if the file already exists — callers decide what a pre-existing
/// file means (the store retries once after removing a stale temp; the
/// billing log treats it as "already created, already restricted").
///
/// On unix this is `OpenOptions::new().write(true).create_new(true).mode(0o600)`.
/// On Windows it is `CreateFileW` with a security descriptor carrying a
/// protected owner-only DACL, so the permissions exist from the first
/// instant the name does.
pub(crate) fn create_owner_only(path: &Path) -> std::io::Result<std::fs::File> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
    }
    #[cfg(windows)]
    {
        windows_impl::create_owner_only(path)
    }
    #[cfg(not(any(unix, windows)))]
    {
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
    }
}

/// Open `path` for **appending**, owner-only, creating it if absent —
/// and hand back the handle to append through.
///
/// The handle is the point. An append log is reopened on every write, so
/// this is the one path where "secure the name, then open the name" is
/// most obviously wrong: a writer to a shared parent directory only has
/// to win the gap once to have every later record appended to a file it
/// controls.
///
/// Two cases, and the second is strictly weaker:
///
/// - **Absent** — [`create_owner_only`] creates it with the permissions
///   already attached, and that handle is returned. No window exists.
/// - **Present** — the ordinary case after the first append, and also
///   the case where an older build or an operator left a file with
///   whatever permissions it happened to get. A symlink here is refused
///   rather than followed: a link planted at a shared `state_path` would
///   otherwise have this chmod, and then append signed usage records to,
///   a file the caller never named. The surviving file is opened, the
///   restriction applied **to the handle**, and the same handle
///   returned.
///
/// Repair cannot revoke access already granted to a handle someone else
/// holds (see the module note), which is why it is confined to the
/// pre-existing case and never used for creation.
pub(crate) fn open_append_owner_only(path: &Path) -> std::io::Result<std::fs::File> {
    match create_append_owner_only(path) {
        Ok(file) => return Ok(file),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e),
    }

    // `symlink_metadata` does not follow the link, so this sees the link
    // itself rather than its target. A reparse point on Windows reports
    // as a symlink here too.
    if std::fs::symlink_metadata(path)?.file_type().is_symlink() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "{} is a symbolic link; refusing to append bearer-adjacent records through it",
                path.display()
            ),
        ));
    }
    open_and_restrict_handle(path)
}

/// [`create_owner_only`], but the handle is positioned for appending.
fn create_append_owner_only(path: &Path) -> std::io::Result<std::fs::File> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        std::fs::OpenOptions::new()
            .append(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
    }
    #[cfg(windows)]
    {
        // No append flag needed: `CREATE_NEW` guarantees the file did not
        // exist, so the handle starts at offset zero of an empty file and
        // the first write *is* the append.
        windows_impl::create_owner_only(path)
    }
    #[cfg(not(any(unix, windows)))]
    {
        std::fs::OpenOptions::new()
            .append(true)
            .create_new(true)
            .open(path)
    }
}

/// Open an existing file for appending and apply the owner-only
/// restriction to the resulting handle.
#[cfg(unix)]
fn open_and_restrict_handle(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::PermissionsExt as _;
    let file = std::fs::OpenOptions::new().append(true).open(path)?;
    // `File::set_permissions` is `fchmod` — it names the open file, not
    // the path, so a rename or replacement racing this call cannot
    // redirect it.
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    Ok(file)
}

#[cfg(not(any(unix, windows)))]
fn open_and_restrict_handle(path: &Path) -> std::io::Result<std::fs::File> {
    // No permission model to apply, but the open still has to succeed —
    // the contract is "checked" rather than silently skipped.
    std::fs::OpenOptions::new().append(true).open(path)
}

#[cfg(windows)]
fn open_and_restrict_handle(path: &Path) -> std::io::Result<std::fs::File> {
    windows_impl::open_append_and_set_dacl(path)
}

#[cfg(windows)]
mod windows_impl {
    use std::os::windows::ffi::OsStrExt as _;
    use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _};
    use std::path::Path;

    use windows_sys::Win32::Foundation::{
        LocalFree, ERROR_SUCCESS, GENERIC_WRITE, HANDLE, HLOCAL, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Security::Authorization::{
        ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
        SetSecurityInfo, SDDL_REVISION_1, SE_FILE_OBJECT,
    };
    use windows_sys::Win32::Security::{
        GetSecurityDescriptorDacl, GetTokenInformation, TokenUser, ACL, DACL_SECURITY_INFORMATION,
        PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES,
        TOKEN_QUERY, TOKEN_USER,
    };
    use windows_sys::Win32::Storage::FileSystem::{CreateFileW, CREATE_NEW, FILE_ATTRIBUTE_NORMAL};
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    /// `CreateFileW` with an owner-only descriptor attached, so the
    /// permissions are in place before the name exists.
    pub(super) fn create_owner_only(path: &Path) -> std::io::Result<std::fs::File> {
        let descriptor = owner_only_descriptor()?;
        let wide = wide(path);
        let mut attrs = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor.0,
            bInheritHandle: 0,
        };
        // SAFETY: `wide` is a NUL-terminated path buffer and `attrs`
        // points at a descriptor still owned by `descriptor`, both alive
        // across the call. `CREATE_NEW` fails rather than truncating if
        // the name exists, which is the contract callers rely on.
        let handle = unsafe {
            CreateFileW(
                wide.as_ptr(),
                GENERIC_WRITE,
                0, // no sharing: nobody opens it alongside us either
                &mut attrs,
                CREATE_NEW,
                FILE_ATTRIBUTE_NORMAL,
                std::ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: `handle` is a valid, exclusively-owned file handle that
        // was just created and is not closed anywhere else; `File` takes
        // ownership of it.
        Ok(unsafe { std::fs::File::from_raw_handle(handle as _) })
    }

    /// `FILE_APPEND_DATA | READ_CONTROL | WRITE_DAC | SYNCHRONIZE`.
    ///
    /// Spelled out rather than imported: they live in different
    /// `windows-sys` modules, so naming them here keeps the crate's
    /// feature list from growing for four integers.
    ///
    /// `WRITE_DAC` is what lets [`SetSecurityInfo`] modify the handle's
    /// DACL, and `READ_CONTROL` is what lets it read the one already
    /// there — replacing a DACL with a protected one needs both, and
    /// omitting `READ_CONTROL` fails with `ERROR_ACCESS_DENIED` rather
    /// than with anything that names the missing right. `SYNCHRONIZE`
    /// makes the handle usable as an ordinary blocking `File`.
    const APPEND_AND_WRITE_DAC: u32 = 0x0004 | 0x0002_0000 | 0x0004_0000 | 0x0010_0000;

    /// Open an existing file for appending and replace the DACL **on the
    /// returned handle**.
    ///
    /// [`SetSecurityInfo`] takes a handle where `SetNamedSecurityInfoW`
    /// takes a pathname. That difference is the whole point: the
    /// pathname form re-resolves the name, so it can be pointed at a
    /// different object than the one about to be written to.
    pub(super) fn open_append_and_set_dacl(path: &Path) -> std::io::Result<std::fs::File> {
        use std::os::windows::fs::OpenOptionsExt as _;

        let file = std::fs::OpenOptions::new()
            .append(true)
            .access_mode(APPEND_AND_WRITE_DAC)
            .open(path)?;

        let descriptor = owner_only_descriptor()?;
        // SAFETY: `file` owns a live handle for the duration of the call;
        // `dacl` borrows from `descriptor`, also alive; owner/group/sacl
        // are null because only `DACL_SECURITY_INFORMATION` is requested.
        let status = unsafe {
            SetSecurityInfo(
                file.as_raw_handle() as HANDLE,
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                descriptor.dacl() as *mut ACL,
                std::ptr::null_mut(),
            )
        };
        if status != ERROR_SUCCESS {
            return Err(std::io::Error::from_raw_os_error(status as i32));
        }
        Ok(file)
    }

    fn wide(path: &Path) -> Vec<u16> {
        path.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    /// A protected, owner-only security descriptor for the current user.
    ///
    /// "Protected" (`P`) severs inheritance — without it the ACE is
    /// merely *added to* whatever the parent grants, which is the
    /// permissive state being fixed. The ACE names the process token's
    /// user SID rather than a well-known alias: `OW` (Owner Rights)
    /// applies only where the owner SID is already what you expect.
    fn owner_only_descriptor() -> std::io::Result<LocalSecurityDescriptor> {
        let sid = current_user_sid_string()?;
        from_sddl(&format!("D:P(A;;FA;;;{sid})"))
    }

    /// An owning handle to a `LocalAlloc`'d security descriptor.
    pub(super) struct LocalSecurityDescriptor(pub(super) PSECURITY_DESCRIPTOR);

    impl LocalSecurityDescriptor {
        /// The descriptor's DACL. Borrowed from `self`, so `self` must
        /// outlive the use.
        fn dacl(&self) -> *const ACL {
            let mut dacl: *mut ACL = std::ptr::null_mut();
            let mut present = 0i32;
            let mut defaulted = 0i32;
            // SAFETY: `self.0` is a valid descriptor still owned by
            // `self`; the three out-params are live locals.
            unsafe {
                GetSecurityDescriptorDacl(self.0, &mut present, &mut dacl, &mut defaulted);
            }
            dacl as *const ACL
        }
    }

    impl Drop for LocalSecurityDescriptor {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: allocated by the conversion below with
                // `LocalAlloc`, non-null here, freed exactly once.
                unsafe {
                    LocalFree(self.0 as HLOCAL);
                }
            }
        }
    }

    fn from_sddl(sddl: &str) -> std::io::Result<LocalSecurityDescriptor> {
        let wide: Vec<u16> = sddl.encode_utf16().chain(std::iter::once(0)).collect();
        let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        // SAFETY: `wide` is a NUL-terminated UTF-16 buffer alive across
        // the call; `descriptor` is a live local the callee fills in.
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                wide.as_ptr(),
                SDDL_REVISION_1 as u32,
                &mut descriptor,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(LocalSecurityDescriptor(descriptor))
    }

    struct TokenHandle(HANDLE);

    impl Drop for TokenHandle {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: a token handle from `OpenProcessToken`,
                // non-null here, closed exactly once.
                unsafe {
                    windows_sys::Win32::Foundation::CloseHandle(self.0);
                }
            }
        }
    }

    struct LocalString(windows_sys::core::PWSTR);

    impl Drop for LocalString {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: allocated by `ConvertSidToStringSidW` with
                // `LocalAlloc`, non-null here, freed exactly once.
                unsafe {
                    LocalFree(self.0 as HLOCAL);
                }
            }
        }
    }

    /// The current process user's SID, as an SDDL string.
    fn current_user_sid_string() -> std::io::Result<String> {
        let mut token: HANDLE = std::ptr::null_mut();
        // SAFETY: `GetCurrentProcess` returns a pseudo-handle needing no
        // close; `token` is a live local the callee fills in.
        let ok = unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) };
        if ok == 0 {
            return Err(std::io::Error::last_os_error());
        }
        let _guard = TokenHandle(token);

        let mut needed = 0u32;
        // SAFETY: a null buffer with zero length is the documented way to
        // ask for the required size; the expected failure is handled by
        // checking `needed` rather than the return value.
        unsafe {
            GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut needed);
        }
        if needed == 0 {
            return Err(std::io::Error::last_os_error());
        }
        let mut buffer = vec![0u8; needed as usize];
        // SAFETY: `buffer` is exactly the size the probe asked for.
        let ok = unsafe {
            GetTokenInformation(
                token,
                TokenUser,
                buffer.as_mut_ptr() as *mut std::ffi::c_void,
                needed,
                &mut needed,
            )
        };
        if ok == 0 {
            return Err(std::io::Error::last_os_error());
        }

        // SAFETY: on success the buffer holds a `TOKEN_USER` whose
        // `User.Sid` points inside the same allocation, still alive here.
        let sid = unsafe { (*(buffer.as_ptr() as *const TOKEN_USER)).User.Sid };
        let mut raw: windows_sys::core::PWSTR = std::ptr::null_mut();
        // SAFETY: `sid` is a valid SID borrowed from `buffer`; `raw` is a
        // live local the callee fills with a `LocalAlloc`'d string.
        let ok = unsafe { ConvertSidToStringSidW(sid, &mut raw) };
        if ok == 0 {
            return Err(std::io::Error::last_os_error());
        }
        let _sid_guard = LocalString(raw);
        // SAFETY: `raw` is a NUL-terminated UTF-16 string from above.
        let len = unsafe {
            let mut n = 0usize;
            while *raw.add(n) != 0 {
                n += 1;
            }
            n
        };
        // SAFETY: `raw` is valid for `len` u16s, established just above.
        let slice = unsafe { std::slice::from_raw_parts(raw, len) };
        String::from_utf16(slice)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    /// A file created this way is writable by us and refuses to clobber
    /// an existing name.
    ///
    /// This asserts the call path works, not that another user is
    /// excluded — verifying that needs a second account, which a unit
    /// test does not have. What it does catch is the failure that would
    /// otherwise ship silently: an FFI mistake or rejected SDDL making
    /// every store write fail.
    #[test]
    fn creates_a_writable_file_and_refuses_to_clobber() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("store.json");

        let mut file = create_owner_only(&path).expect("create");
        file.write_all(b"{}").expect("write");
        drop(file);
        assert_eq!(std::fs::read(&path).expect("read back"), b"{}");

        // `create_new` semantics: a second create fails rather than
        // truncating, which is what lets the store detect a stale temp.
        let err = create_owner_only(&path).expect_err("must not clobber");
        assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
    }

    /// The append path creates a missing file and appends to an existing
    /// one through the handle it returns — never truncating.
    #[test]
    fn appending_creates_then_extends_through_the_handle() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("billing.jsonl");

        let mut first = open_append_owner_only(&path).expect("create");
        first.write_all(b"one\n").expect("write");
        drop(first);

        let mut second = open_append_owner_only(&path).expect("reopen");
        second.write_all(b"two\n").expect("append");
        drop(second);

        assert_eq!(
            std::fs::read(&path).expect("read back"),
            b"one\ntwo\n",
            "the second open must append, not truncate"
        );
    }

    /// The repair path tightens a file that already exists — including
    /// one an older build left world-readable — and does it on the
    /// handle it hands back.
    #[test]
    fn appending_to_a_permissive_file_tightens_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("legacy.jsonl");
        std::fs::write(&path, b"{}\n").expect("write");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            // Start deliberately permissive, as a legacy log might be.
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
                .expect("loosen");
        }

        let file = open_append_owner_only(&path).expect("open existing");
        drop(file);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&path).expect("stat").permissions().mode();
            assert_eq!(
                mode & 0o777,
                0o600,
                "repair must actually change the mode, not just return Ok"
            );
        }

        assert_eq!(std::fs::read(&path).expect("read back"), b"{}\n");
    }

    /// A symlink at the log path is refused rather than followed. A
    /// writer to a shared directory could otherwise redirect signed
    /// usage records — and this function's own chmod — onto a file the
    /// caller never named.
    #[cfg(unix)]
    #[test]
    fn a_symlinked_path_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("elsewhere");
        std::fs::write(&target, b"not ours\n").expect("write target");
        let link = dir.path().join("billing.jsonl");
        std::os::unix::fs::symlink(&target, &link).expect("symlink");

        let err = open_append_owner_only(&link).expect_err("must refuse a link");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
        assert_eq!(
            std::fs::read(&target).expect("read target"),
            b"not ours\n",
            "the target must be untouched"
        );
    }
}
