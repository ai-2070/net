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
//! The same reasoning is why [`open_append_owner_only`] refuses a
//! symlink — and anything else that is not a regular file — atomically
//! with the open, and applies its permission change to the handle rather
//! than to the pathname.
//!
//! ## Why a permissive predecessor is replaced, not repaired
//!
//! It follows from the same fact. A file another user could already open
//! may already *be* open by them, and no amount of tightening takes that
//! handle away. Chmod'ing it to `0600` and then appending signed usage
//! records would be writing into a file with a reader attached, while
//! reporting the file as owner-only.
//!
//! So on unix, a pre-existing log whose mode grants anyone but its owner
//! is copied into a fresh owner-only file which is renamed over the name.
//! The old inode keeps its readers and stops receiving anything; the name
//! now refers to a file that never existed unprotected. Windows has no
//! equally cheap way to ask "is this already owner-only?", so it
//! re-asserts the descriptor on the handle instead — see `is_owner_only`
//! for what that leaves on the table.
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
        // Exclusive: the store's temp is written once and renamed away, so
        // nothing else has any business holding it open.
        windows_impl::create_owner_only(path, windows_impl::SHARE_NONE)
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
/// The handle is the point. Securing a name and then opening the name are
/// two operations, and a writer to a shared parent directory only has to
/// win the gap once to have records appended to a file it controls. Every
/// check below is therefore made against the **open handle**, never
/// against the path a second time, and callers are expected to keep the
/// handle rather than reopen.
///
/// Three cases:
///
/// - **Absent** — [`create_owner_only`] creates it with the permissions
///   already attached, and that handle is returned. No window exists.
/// - **Present and already owner-only** — the ordinary case after the
///   first append. Opened without following links, checked to be a
///   regular file, restriction re-asserted on the handle.
/// - **Present and permissive** — an older build, or an operator, left a
///   file others can read. This is **migrated, not repaired**: tightening
///   the permissions cannot revoke access already granted to a handle
///   someone else holds, so records would keep flowing to a reader that
///   got in first. A fresh owner-only file is created, the existing
///   contents copied into it, and it is renamed over the name — so the
///   old inode, and every handle onto it, stops receiving anything.
///
/// A symlink is refused rather than followed, atomically with the open
/// (`O_NOFOLLOW`, `FILE_FLAG_OPEN_REPARSE_POINT`) so no separate check
/// can be raced. Anything that is not a regular file is refused too: the
/// symlink test alone still admits a FIFO, and a FIFO planted at a shared
/// `state_path` turns every appended record into a message delivered to
/// whoever holds the read end.
pub(crate) fn open_append_owner_only(path: &Path) -> std::io::Result<std::fs::File> {
    match create_append_owner_only(path) {
        Ok(file) => return Ok(file),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e),
    }

    let file = open_existing_no_follow(path)?;
    let metadata = file.metadata()?;
    if !metadata.file_type().is_file() {
        return Err(not_a_regular_file(path));
    }
    if is_owner_only(&metadata) {
        restrict_handle(&file)?;
        return Ok(file);
    }
    migrate_to_a_fresh_owner_only_file(path, file)
}

fn not_a_regular_file(path: &Path) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        format!(
            "{} is not a regular file; refusing to append bearer-adjacent records through it",
            path.display()
        ),
    )
}

/// Copy an existing permissive log into a fresh owner-only file and put
/// that file at `path`, returning the handle to append through.
///
/// The rename is what makes this a migration rather than a repair: a
/// reader holding a handle on the old inode keeps that handle, and the old
/// inode stops being what `path` names — so it receives nothing further.
/// Tightening permissions in place could not achieve that, because access
/// is granted at open time on every platform this runs on.
///
/// The handle returned is the one created here, already renamed into
/// place, so the rename cannot be raced either.
fn migrate_to_a_fresh_owner_only_file(
    path: &Path,
    mut existing: std::fs::File,
) -> std::io::Result<std::fs::File> {
    use std::io::{Read as _, Seek as _, Write as _};

    // Per-pid temp beside the log, so two processes migrating at once do
    // not clobber each other and the rename stays same-filesystem.
    let temp = path.with_extension(format!("owner-only-migrate.{}", std::process::id()));
    let _ = std::fs::remove_file(&temp);
    let mut fresh = create_append_owner_only(&temp)?;

    let mut carried = Vec::new();
    existing.rewind()?;
    existing.read_to_end(&mut carried)?;
    drop(existing);
    fresh.write_all(&carried)?;
    fresh.sync_all()?;

    // Rename over the name. The handle keeps referring to the object it
    // was opened on, which is now the object `path` names.
    match std::fs::rename(&temp, path) {
        Ok(()) => Ok(fresh),
        Err(e) => {
            let _ = std::fs::remove_file(&temp);
            Err(e)
        }
    }
}

/// [`create_owner_only`], but the handle is positioned for appending.
fn create_append_owner_only(path: &Path) -> std::io::Result<std::fs::File> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        std::fs::OpenOptions::new()
            .append(true)
            .read(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
    }
    #[cfg(windows)]
    {
        // No append flag needed: `CREATE_NEW` guarantees the file did not
        // exist, so the handle starts at offset zero of an empty file and
        // the first write *is* the append.
        //
        // Shared, unlike the store's temp: the log is read back by
        // `read_all` while the appending handle is held for the life of
        // the process, and an exclusive handle would make the log
        // unreadable to its own owner.
        windows_impl::create_owner_only(path, windows_impl::SHARE_ALL)
    }
    #[cfg(not(any(unix, windows)))]
    {
        std::fs::OpenOptions::new()
            .append(true)
            .read(true)
            .create_new(true)
            .open(path)
    }
}

/// Open an existing file for appending **without following a link**, and
/// without blocking if it turns out to be a pipe.
#[cfg(unix)]
fn open_existing_no_follow(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt as _;
    std::fs::OpenOptions::new()
        .append(true)
        .read(true)
        // `O_NOFOLLOW` makes the refusal atomic with the open, so a link
        // cannot be swapped in after a separate `symlink_metadata` check.
        // `O_NONBLOCK` is for the FIFO case: opening a pipe for writing
        // otherwise blocks until a reader appears, turning a planted FIFO
        // into a hang rather than an error. It has no effect on a regular
        // file, which is the only thing that gets past the check above.
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
}

#[cfg(windows)]
fn open_existing_no_follow(path: &Path) -> std::io::Result<std::fs::File> {
    windows_impl::open_append_no_follow(path)
}

#[cfg(not(any(unix, windows)))]
fn open_existing_no_follow(path: &Path) -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .append(true)
        .read(true)
        .open(path)
}

/// Is this file already readable and writable by its owner alone?
#[cfg(unix)]
fn is_owner_only(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    metadata.permissions().mode() & 0o077 == 0
}

/// Windows has no mode bits, and reading a DACL back to compare it
/// against the one this module writes is a great deal of FFI for a
/// question with a cheaper answer: re-assert the descriptor on the
/// handle, every time.
///
/// So the migration path is not taken there. The residual risk is the one
/// the module note describes — a handle opened before the descriptor
/// landed keeps its access — bounded by the fact that the default state
/// path lives under `%LOCALAPPDATA%`, which is already user-scoped. An
/// operator who puts the log in a shared directory and lets another user
/// create it first is outside what this can repair.
#[cfg(not(unix))]
fn is_owner_only(_metadata: &std::fs::Metadata) -> bool {
    true
}

/// Apply the owner-only restriction to an already-open handle.
#[cfg(unix)]
fn restrict_handle(file: &std::fs::File) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    // `File::set_permissions` is `fchmod` — it names the open file, not
    // the path, so a rename or replacement racing this call cannot
    // redirect it.
    file.set_permissions(std::fs::Permissions::from_mode(0o600))
}

#[cfg(windows)]
fn restrict_handle(file: &std::fs::File) -> std::io::Result<()> {
    windows_impl::set_dacl_on_handle(file)
}

#[cfg(not(any(unix, windows)))]
fn restrict_handle(_file: &std::fs::File) -> std::io::Result<()> {
    // No permission model to apply. The open already succeeded, so the
    // contract is "checked" rather than silently skipped.
    Ok(())
}

#[cfg(windows)]
mod windows_impl {
    use std::os::windows::ffi::OsStrExt as _;
    use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _};
    use std::path::Path;

    use windows_sys::Win32::Foundation::{
        LocalFree, ERROR_SUCCESS, GENERIC_READ, GENERIC_WRITE, HANDLE, HLOCAL, INVALID_HANDLE_VALUE,
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

    /// No sharing: nobody opens the file alongside us. Right for the
    /// store's temp, which is written once and renamed away.
    pub(super) const SHARE_NONE: u32 = 0;
    /// `FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE`. Right for
    /// the billing log, whose appending handle is held for the life of the
    /// process while `read_all` reads the same file back — an exclusive
    /// handle would make the log unreadable to its own owner.
    pub(super) const SHARE_ALL: u32 = 0x0000_0001 | 0x0000_0002 | 0x0000_0004;

    /// `CreateFileW` with an owner-only descriptor attached, so the
    /// permissions are in place before the name exists.
    pub(super) fn create_owner_only(path: &Path, share: u32) -> std::io::Result<std::fs::File> {
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
                GENERIC_WRITE | GENERIC_READ,
                share,
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

    /// `FILE_FLAG_OPEN_REPARSE_POINT` — open the reparse point itself
    /// rather than what it points at, so a symlink or junction planted at
    /// the log name is refused by the regular-file check that follows
    /// instead of silently redirecting the append. Atomic with the open,
    /// unlike a separate `symlink_metadata` call.
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;

    /// Open an existing file for appending, without following a link.
    ///
    /// The caller checks the returned handle is a regular file and then
    /// restricts it; nothing here re-resolves the pathname.
    pub(super) fn open_append_no_follow(path: &Path) -> std::io::Result<std::fs::File> {
        use std::os::windows::fs::OpenOptionsExt as _;
        std::fs::OpenOptions::new()
            .append(true)
            .read(true)
            .access_mode(APPEND_AND_WRITE_DAC)
            .share_mode(SHARE_ALL)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
    }

    /// Replace the DACL **on an open handle**.
    ///
    /// [`SetSecurityInfo`] takes a handle where `SetNamedSecurityInfoW`
    /// takes a pathname. That difference is the whole point: the pathname
    /// form re-resolves the name, so it can be pointed at a different
    /// object than the one about to be written to.
    pub(super) fn set_dacl_on_handle(file: &std::fs::File) -> std::io::Result<()> {
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
    #[cfg(unix)]
    use std::io::Read as _;
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

        // The refusal comes from `O_NOFOLLOW` inside the open itself, so
        // the error is the platform's (ELOOP and friends) rather than one
        // this module builds. What matters is that it failed and the
        // target was not touched — atomicity is the point, not the kind.
        open_append_owner_only(&link).expect_err("must refuse a link");
        assert_eq!(
            std::fs::read(&target).expect("read target"),
            b"not ours\n",
            "the target must be untouched"
        );
    }

    /// A FIFO at the log path is refused.
    ///
    /// The symlink check alone does not cover this: a named pipe is not a
    /// link, so it passes, and every appended record then becomes a
    /// message delivered to whoever holds the read end. The check is on
    /// the *handle's* type, which is why it catches this at all.
    #[cfg(unix)]
    #[test]
    fn a_fifo_at_the_log_path_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("billing.jsonl");
        let c_path = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).expect("cstring");
        // SAFETY: a NUL-terminated path in a directory this test owns.
        let made = unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) };
        assert_eq!(made, 0, "mkfifo: {}", std::io::Error::last_os_error());

        // Must not hang, either — the open is `O_NONBLOCK`, so a pipe
        // with no reader returns rather than parking forever. Whether the
        // refusal comes from the open (some platforms reject a FIFO
        // outright) or from the regular-file check afterwards is a
        // platform detail; that it refuses is not.
        open_append_owner_only(&path).expect_err("must refuse a pipe");
    }

    /// A pre-existing world-readable log is **migrated**, not chmod'ed.
    ///
    /// Tightening it in place would leave any handle another user already
    /// holds intact — access is granted at open time — so the records
    /// would keep flowing to a reader that got in first. Migration puts a
    /// fresh file at the name, and the old inode stops being what the
    /// name refers to.
    #[cfg(unix)]
    #[test]
    fn a_permissive_predecessor_is_migrated_to_a_fresh_file() {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("legacy.jsonl");
        std::fs::write(&path, b"carried\n").expect("write");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("loosen");
        let old_inode = std::fs::metadata(&path).expect("stat").ino();

        // Stand in for the reader that got in before the repair.
        let mut squatter = std::fs::File::open(&path).expect("pre-existing handle");

        let mut file = open_append_owner_only(&path).expect("migrate");
        file.write_all(b"appended\n").expect("append");
        drop(file);

        let migrated = std::fs::metadata(&path).expect("stat");
        assert_ne!(
            migrated.ino(),
            old_inode,
            "the name must refer to a new file, or the old handle still sees the appends"
        );
        assert_eq!(migrated.permissions().mode() & 0o777, 0o600);
        assert_eq!(
            std::fs::read(&path).expect("read back"),
            b"carried\nappended\n",
            "existing records are carried across, not dropped"
        );

        // The squatter's handle is intact but stale: it sees what was
        // there when it opened, and nothing since.
        let mut seen = Vec::new();
        squatter.read_to_end(&mut seen).expect("read old inode");
        assert_eq!(
            seen, b"carried\n",
            "the pre-existing handle must not receive anything appended after the migration"
        );
    }

    /// An already-owner-only log is opened in place rather than migrated
    /// — the ordinary case, and it must not rewrite the file on every
    /// process start.
    #[cfg(unix)]
    #[test]
    fn an_already_restricted_log_is_not_migrated() {
        use std::os::unix::fs::MetadataExt as _;

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("billing.jsonl");
        let mut first = open_append_owner_only(&path).expect("create");
        first.write_all(b"one\n").expect("write");
        drop(first);
        let inode = std::fs::metadata(&path).expect("stat").ino();

        let second = open_append_owner_only(&path).expect("reopen");
        drop(second);
        assert_eq!(
            std::fs::metadata(&path).expect("stat").ino(),
            inode,
            "a log that is already owner-only is opened, not copied"
        );
    }
}
