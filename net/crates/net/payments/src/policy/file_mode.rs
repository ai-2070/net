//! Owner-only permissions for the payment stores, on every platform.
//!
//! The engine store holds base64 of preserved x402 payloads — signed
//! EIP-3009 authorizations, which are bearer instruments — and the
//! billing log holds the signed usage record. Both are created 0600 on
//! unix. Windows has no mode bits, so for a long time the same files were
//! created with whatever the parent directory's ACL happened to grant,
//! and the store's own doc comment claimed "owner-only (0600) from
//! creation" without qualification.
//!
//! In practice `%LOCALAPPDATA%` is already user-scoped, so the *default*
//! path was not exposed. That is a property of where the file happens to
//! live, though, not of how it was created — an operator who supplies a
//! custom `state_path` under a shared directory got no protection and no
//! warning. This module closes that by setting an explicit owner-only
//! DACL, so the guarantee comes from the file rather than from its
//! neighbourhood.
//!
//! Failure is loud. A store whose permissions could not be restricted is
//! not one to keep writing bearer material into, so the caller surfaces
//! the error rather than continuing.

use std::path::Path;

/// Restrict `path` to its owner.
///
/// Unix does this at creation time through `OpenOptions::mode(0o600)`, so
/// there this is a no-op that exists to keep the call sites uniform.
#[cfg(not(windows))]
pub(crate) fn restrict_to_owner(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

/// Windows: replace the file's DACL with a protected, owner-only one.
///
/// "Protected" (`P` in SDDL, `PROTECTED_DACL_SECURITY_INFORMATION` in the
/// call) is the load-bearing part — without it the ACE below is *added
/// to* whatever the parent directory inherits, which is the permissive
/// state being fixed. With it, inheritance is severed and the explicit
/// ACE is the whole ACL.
///
/// The ACE names the current process's user SID rather than a well-known
/// alias: `OW` (Owner Rights) applies only where an owner SID is already
/// set as expected, and `CU` is not valid in every SDDL context. Reading
/// the token is unambiguous.
#[cfg(windows)]
pub(crate) fn restrict_to_owner(path: &Path) -> std::io::Result<()> {
    let sid = current_user_sid_string()?;
    // D: DACL, P: protected (no inheritance), one ACE granting file-all
    // (FA) to the current user.
    let sddl = format!("D:P(A;;FA;;;{sid})");
    let descriptor = security_descriptor_from_sddl(&sddl)?;
    // `descriptor` stays alive across the call: `dacl()` borrows into it.
    set_dacl(path, descriptor.dacl())
}

#[cfg(windows)]
mod sys {
    pub(super) use windows_sys::Win32::Foundation::{LocalFree, ERROR_SUCCESS, HANDLE, HLOCAL};
    pub(super) use windows_sys::Win32::Security::Authorization::{
        ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
        SetNamedSecurityInfoW, SDDL_REVISION_1, SE_FILE_OBJECT,
    };
    pub(super) use windows_sys::Win32::Security::{
        GetSecurityDescriptorDacl, TokenUser, ACL, DACL_SECURITY_INFORMATION,
        PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, TOKEN_QUERY, TOKEN_USER,
    };
    pub(super) use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
}

/// An owning handle to a `LocalAlloc`'d security descriptor.
#[cfg(windows)]
struct LocalSecurityDescriptor(sys::PSECURITY_DESCRIPTOR);

#[cfg(windows)]
impl LocalSecurityDescriptor {
    /// The descriptor's DACL pointer. Borrowed from `self`, so it must
    /// not outlive it — which is why `restrict_to_owner` uses it in a
    /// single expression while the descriptor is still alive.
    fn dacl(&self) -> *const sys::ACL {
        let mut dacl: *mut sys::ACL = std::ptr::null_mut();
        let mut present = 0i32;
        let mut defaulted = 0i32;
        // SAFETY: `self.0` is a valid descriptor returned by
        // `ConvertStringSecurityDescriptorToSecurityDescriptorW` and still
        // owned by `self`; the three out-params are live locals.
        unsafe {
            sys::GetSecurityDescriptorDacl(self.0, &mut present, &mut dacl, &mut defaulted);
        }
        dacl as *const sys::ACL
    }
}

#[cfg(windows)]
impl Drop for LocalSecurityDescriptor {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: `self.0` was allocated by the conversion call with
            // `LocalAlloc`, is non-null here, and is dropped exactly once.
            unsafe {
                sys::LocalFree(self.0 as sys::HLOCAL);
            }
        }
    }
}

#[cfg(windows)]
fn security_descriptor_from_sddl(sddl: &str) -> std::io::Result<LocalSecurityDescriptor> {
    let wide: Vec<u16> = sddl.encode_utf16().chain(std::iter::once(0)).collect();
    let mut descriptor: sys::PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    // SAFETY: `wide` is a NUL-terminated UTF-16 buffer that outlives the
    // call; `descriptor` is a live local the callee fills in.
    let ok = unsafe {
        sys::ConvertStringSecurityDescriptorToSecurityDescriptorW(
            wide.as_ptr(),
            sys::SDDL_REVISION_1 as u32,
            &mut descriptor,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(LocalSecurityDescriptor(descriptor))
}

#[cfg(windows)]
fn set_dacl(path: &Path, dacl: *const sys::ACL) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt as _;
    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: `wide` is a NUL-terminated path buffer that outlives the
    // call; `dacl` points into a descriptor still owned by the caller;
    // the owner/group/sacl arguments are explicitly null because
    // `DACL_SECURITY_INFORMATION` is the only bit requested.
    let status = unsafe {
        sys::SetNamedSecurityInfoW(
            wide.as_ptr(),
            sys::SE_FILE_OBJECT,
            sys::DACL_SECURITY_INFORMATION | sys::PROTECTED_DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            dacl as *mut sys::ACL,
            std::ptr::null_mut(),
        )
    };
    if status != sys::ERROR_SUCCESS {
        return Err(std::io::Error::from_raw_os_error(status as i32));
    }
    Ok(())
}

/// The current process user's SID, as an SDDL string.
#[cfg(windows)]
fn current_user_sid_string() -> std::io::Result<String> {
    let mut token: sys::HANDLE = std::ptr::null_mut();
    // SAFETY: `GetCurrentProcess` returns a pseudo-handle needing no
    // close; `token` is a live local the callee fills in.
    let ok =
        unsafe { sys::OpenProcessToken(sys::GetCurrentProcess(), sys::TOKEN_QUERY, &mut token) };
    if ok == 0 {
        return Err(std::io::Error::last_os_error());
    }
    let _guard = TokenHandle(token);

    // Size probe, then the real read.
    let mut needed = 0u32;
    // SAFETY: a null buffer with zero length is the documented way to ask
    // for the required size; failure with ERROR_INSUFFICIENT_BUFFER is
    // expected and handled by checking `needed` rather than the return.
    unsafe {
        windows_sys::Win32::Security::GetTokenInformation(
            token,
            sys::TokenUser,
            std::ptr::null_mut(),
            0,
            &mut needed,
        );
    }
    if needed == 0 {
        return Err(std::io::Error::last_os_error());
    }
    let mut buffer = vec![0u8; needed as usize];
    // SAFETY: `buffer` is `needed` bytes, which is the size the probe
    // asked for; `needed` is re-read as an out-param.
    let ok = unsafe {
        windows_sys::Win32::Security::GetTokenInformation(
            token,
            sys::TokenUser,
            buffer.as_mut_ptr() as *mut std::ffi::c_void,
            needed,
            &mut needed,
        )
    };
    if ok == 0 {
        return Err(std::io::Error::last_os_error());
    }

    // SAFETY: on success the buffer holds a `TOKEN_USER` whose `User.Sid`
    // points inside the same allocation, which is still alive here.
    let sid = unsafe { (*(buffer.as_ptr() as *const sys::TOKEN_USER)).User.Sid };
    let mut raw: windows_sys::core::PWSTR = std::ptr::null_mut();
    // SAFETY: `sid` is a valid SID borrowed from `buffer`; `raw` is a
    // live local the callee fills with a `LocalAlloc`'d string.
    let ok = unsafe { sys::ConvertSidToStringSidW(sid, &mut raw) };
    if ok == 0 {
        return Err(std::io::Error::last_os_error());
    }
    let _sid_guard = LocalString(raw);
    // SAFETY: `raw` is a NUL-terminated UTF-16 string from the call above.
    let len = unsafe {
        let mut n = 0usize;
        while *raw.add(n) != 0 {
            n += 1;
        }
        n
    };
    // SAFETY: `raw` is valid for `len` u16s, established immediately above.
    let slice = unsafe { std::slice::from_raw_parts(raw, len) };
    String::from_utf16(slice)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))
}

#[cfg(windows)]
struct TokenHandle(sys::HANDLE);

#[cfg(windows)]
impl Drop for TokenHandle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: `self.0` is a token handle from `OpenProcessToken`,
            // non-null here, closed exactly once.
            unsafe {
                windows_sys::Win32::Foundation::CloseHandle(self.0);
            }
        }
    }
}

#[cfg(windows)]
struct LocalString(windows_sys::core::PWSTR);

#[cfg(windows)]
impl Drop for LocalString {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: allocated by `ConvertSidToStringSidW` with
            // `LocalAlloc`, non-null here, freed exactly once.
            unsafe {
                sys::LocalFree(self.0 as sys::HLOCAL);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The call succeeds on a real file, on every platform — a no-op on
    /// unix (where the mode is set at creation) and a real DACL write on
    /// Windows.
    ///
    /// This asserts the call path works, not that another user is
    /// actually excluded: verifying that needs a second account, which a
    /// unit test does not have. What it does catch is the failure that
    /// would otherwise ship silently — an FFI mistake making every store
    /// write fail, or the SDDL being rejected.
    #[test]
    fn restricting_a_real_file_succeeds() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("store.json");
        std::fs::write(&path, b"{}").expect("write");
        restrict_to_owner(&path).expect("restricting an owned file must succeed");
        // Still readable by us afterwards — an owner-only ACL must not
        // lock out the owner.
        assert_eq!(std::fs::read(&path).expect("read back"), b"{}");
    }

    /// A path that does not exist is an error rather than a silent pass:
    /// the caller is about to write bearer material there.
    #[cfg(windows)]
    #[test]
    fn restricting_a_missing_file_is_an_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(restrict_to_owner(&dir.path().join("nope.json")).is_err());
    }
}
