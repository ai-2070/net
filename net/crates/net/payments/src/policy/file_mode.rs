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

/// Tighten an **existing** file to owner-only.
///
/// Weaker than [`create_owner_only`] by construction — see the module
/// note: it cannot revoke access already granted to an open handle. Use
/// it only to repair a file that predates this code, never as the
/// primary guard for one being created now.
#[cfg(not(windows))]
pub(crate) fn restrict_existing(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(windows)]
pub(crate) fn restrict_existing(path: &Path) -> std::io::Result<()> {
    windows_impl::set_dacl_by_path(path)
}

#[cfg(windows)]
mod windows_impl {
    use std::os::windows::ffi::OsStrExt as _;
    use std::os::windows::io::FromRawHandle as _;
    use std::path::Path;

    use windows_sys::Win32::Foundation::{
        LocalFree, ERROR_SUCCESS, GENERIC_WRITE, HANDLE, HLOCAL, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Security::Authorization::{
        ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
        SetNamedSecurityInfoW, SDDL_REVISION_1, SE_FILE_OBJECT,
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

    /// Replace an existing file's DACL by pathname.
    pub(super) fn set_dacl_by_path(path: &Path) -> std::io::Result<()> {
        let descriptor = owner_only_descriptor()?;
        let wide = wide(path);
        // SAFETY: `wide` is NUL-terminated and alive across the call;
        // `dacl` borrows from `descriptor`, also alive; owner/group/sacl
        // are null because only `DACL_SECURITY_INFORMATION` is requested.
        let status = unsafe {
            SetNamedSecurityInfoW(
                wide.as_ptr(),
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
        Ok(())
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

    /// The repair path works on a file that already exists.
    #[test]
    fn restricting_an_existing_file_succeeds() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("legacy.json");
        std::fs::write(&path, b"{}").expect("write");
        restrict_existing(&path).expect("restrict");
        assert_eq!(std::fs::read(&path).expect("read back"), b"{}");
    }

    /// A missing file is an error rather than a silent pass: the caller
    /// is about to write bearer material.
    #[cfg(windows)]
    #[test]
    fn restricting_a_missing_file_is_an_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(restrict_existing(&dir.path().join("nope.json")).is_err());
    }
}
