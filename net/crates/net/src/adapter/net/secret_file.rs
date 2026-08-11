//! Fail-closed opener for files that hold secrets.
//!
//! SEC-05 / LINUX-02. Several files in this tree are secret-bearing —
//! the CLI profile and the aggregator config hold `psk_hex`, the
//! mesh-membership root secret; identity, organization and subnet key
//! files hold ed25519 seeds; device-enrollment state holds a private
//! device seed. Reading any of them from a location another local user
//! can read hands that user mesh membership or a signing identity.
//!
//! The checks that existed were inconsistent and mostly advisory: the
//! aggregator warned and continued, the CLI profile loader had no check
//! at all, and the ones that did check looked at the mode only.
//!
//! # What this validates
//!
//! Three things, on the **opened object** rather than the path:
//!
//! 1. it is a regular file — not a FIFO that blocks forever, not a
//!    device, not a directory;
//! 2. it is owned by the calling user — mode `0o600` proves nothing
//!    about *whose* `0o600` it is, and a privileged process reading a
//!    path under a directory someone else controls can be handed a
//!    perfectly-permissioned file full of an attacker's key material;
//! 3. `mode & 0o077 == 0` — no group or other access.
//!
//! # Why the opened object
//!
//! Checking `metadata(path)` and then reading `path` is two lookups of
//! a name that something else may control. Between them the name can
//! be replaced. Opening once and calling `File::metadata` — an `fstat`
//! on the descriptor — validates the exact object that will be read.
//! The one remaining window is between `open` and the check, which is
//! why the type check comes first: an already-open FIFO is refused
//! rather than read.
//!
//! # Windows
//!
//! `std::fs` exposes no usable NTFS ACL view, so the gate degrades to
//! a warning. That is a real gap, not a platform where the problem
//! does not exist: a config under a permissive inherited DACL is just
//! as readable. A `GetFileSecurityW` check would close it and needs
//! the `windows` crate.

use std::fs::File;
use std::path::{Path, PathBuf};

/// Why a secret-bearing file was refused.
#[derive(Debug)]
pub enum SecretFileError {
    /// The file could not be opened or inspected.
    Io {
        /// The path that failed.
        path: PathBuf,
        /// The underlying error.
        source: std::io::Error,
    },
    /// The path resolved to something that is not a regular file.
    NotRegularFile {
        /// The path that failed.
        path: PathBuf,
    },
    /// The file is owned by a different user.
    ForeignOwner {
        /// The path that failed.
        path: PathBuf,
        /// The uid that owns the file.
        owner_uid: u32,
        /// The uid this process is running as.
        expected_uid: u32,
    },
    /// The file is readable or writable by group or other.
    PermissiveMode {
        /// The path that failed.
        path: PathBuf,
        /// The offending mode, masked to the permission bits.
        mode: u32,
    },
}

impl std::fmt::Display for SecretFileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io { path, source } => {
                write!(f, "cannot read {}: {source}", path.display())
            }
            Self::NotRegularFile { path } => write!(
                f,
                "{} is not a regular file; refusing to read secret material from it \
                 (kind: not_regular_file)",
                path.display()
            ),
            Self::ForeignOwner {
                path,
                owner_uid,
                expected_uid,
            } => write!(
                f,
                "{} is owned by uid {owner_uid}, not by this process (uid \
                 {expected_uid}); refusing to trust secret material supplied by \
                 another user (kind: foreign_owner)",
                path.display()
            ),
            Self::PermissiveMode { path, mode } => write!(
                f,
                "{} has mode {mode:#o}; it holds secret material and must not be \
                 group- or world-accessible. Run `chmod 600 {}` (kind: \
                 permissive_mode)",
                path.display(),
                path.display()
            ),
        }
    }
}

impl std::error::Error for SecretFileError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

impl SecretFileError {
    /// A short, stable category for logs and typed error mapping.
    /// Never includes file contents.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Io { .. } => "io",
            Self::NotRegularFile { .. } => "not_regular_file",
            Self::ForeignOwner { .. } => "foreign_owner",
            Self::PermissiveMode { .. } => "permissive_mode",
        }
    }
}

/// Open `path` for reading, refusing it unless it is a regular file
/// owned by the calling user with no group/other access.
///
/// `allow_insecure` skips the ownership and mode checks — the escape
/// hatch for deployments that genuinely cannot satisfy them. It does
/// **not** skip the regular-file check, which is about not hanging or
/// reading a device rather than about permissions. Callers should
/// surface it as an explicitly-named flag (`--insecure-permissions`),
/// never as a default.
pub fn open_secret_file(path: &Path, allow_insecure: bool) -> Result<File, SecretFileError> {
    let file = File::open(path).map_err(|e| SecretFileError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    // fstat the descriptor, not the name: the name may not still refer
    // to this object.
    let meta = file.metadata().map_err(|e| SecretFileError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    if !meta.is_file() {
        return Err(SecretFileError::NotRegularFile {
            path: path.to_path_buf(),
        });
    }
    if allow_insecure {
        return Ok(file);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        // SAFETY: `geteuid` is always-succeeds, takes no arguments and
        // touches no memory. It is unsafe only because it is `extern`.
        let euid = unsafe { libc::geteuid() };
        if meta.uid() != euid {
            return Err(SecretFileError::ForeignOwner {
                path: path.to_path_buf(),
                owner_uid: meta.uid(),
                expected_uid: euid,
            });
        }
        let mode = meta.mode() & 0o777;
        if mode & 0o077 != 0 {
            return Err(SecretFileError::PermissiveMode {
                path: path.to_path_buf(),
                mode,
            });
        }
    }
    #[cfg(not(unix))]
    {
        tracing::warn!(
            path = %path.display(),
            "secret-file permission gate is a no-op on this platform: NTFS ACLs \
             are not validated. This file holds secret material — restrict its \
             ACL out-of-band, or it may be readable by other local users.",
        );
    }
    Ok(file)
}

/// [`open_secret_file`] plus a read to `String`.
///
/// The returned buffer holds secret material; scrub it before it
/// drops (`zeroize`) rather than relying on the allocator.
pub fn read_secret_file_to_string(
    path: &Path,
    allow_insecure: bool,
) -> Result<String, SecretFileError> {
    use std::io::Read as _;
    let mut file = open_secret_file(path, allow_insecure)?;
    let mut buf = String::new();
    file.read_to_string(&mut buf)
        .map_err(|e| SecretFileError::Io {
            path: path.to_path_buf(),
            source: e,
        })?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("net-sec05-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    /// A file this process just created is accepted on every platform:
    /// the gate must not break the ordinary case.
    #[test]
    fn an_owner_only_file_is_accepted() {
        let dir = scratch("ok");
        let path = dir.join("secret.toml");
        std::fs::write(&path, "psk_hex = \"aa\"\n").expect("write");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
                .expect("chmod");
        }
        let text = read_secret_file_to_string(&path, false).expect("owner-only file is readable");
        assert!(text.contains("psk_hex"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A directory is refused before anything tries to read it.
    /// Platform-independent: this is a type check, not a mode check.
    #[test]
    fn a_directory_is_not_a_secret_file() {
        let dir = scratch("dir");
        match open_secret_file(&dir, false) {
            Err(e) => assert!(
                matches!(e.kind(), "not_regular_file" | "io"),
                "expected a type refusal, got {}: {e}",
                e.kind()
            ),
            Ok(_) => panic!("a directory was accepted as a secret file"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Even `allow_insecure` must not accept a non-regular file: that
    /// flag is about permissions, and reading a FIFO can block the
    /// process forever.
    #[test]
    fn allow_insecure_still_refuses_a_non_regular_file() {
        let dir = scratch("insecure-dir");
        assert!(
            open_secret_file(&dir, true).is_err(),
            "the permission escape hatch also skipped the type check"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The headline case: mode 0644 on a file holding a PSK.
    #[cfg(unix)]
    #[test]
    fn a_group_readable_file_is_refused_and_the_override_admits_it() {
        use std::os::unix::fs::PermissionsExt;

        let dir = scratch("mode");
        let path = dir.join("config.toml");
        std::fs::write(&path, "psk_hex = \"aa\"\n").expect("write");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("chmod");

        match open_secret_file(&path, false) {
            Err(e) => assert_eq!(e.kind(), "permissive_mode", "got {e}"),
            Ok(_) => panic!("a 0644 secret file was accepted"),
        }
        // And the named override lets a deployment that cannot comply
        // proceed — the point is that it has to be asked for.
        assert!(open_secret_file(&path, true).is_ok());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The error must name the file and the category, and must not be
    /// able to carry contents — it never reads them.
    #[test]
    fn refusals_name_the_path_and_the_category() {
        let dir = scratch("msg");
        let missing = dir.join("nope.toml");
        let err = open_secret_file(&missing, false).expect_err("missing file");
        let rendered = format!("{err}");
        assert!(rendered.contains("nope.toml"), "got: {rendered}");
        assert_eq!(err.kind(), "io");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
