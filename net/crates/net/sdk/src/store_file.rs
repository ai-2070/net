//! Shared atomic-publication staging for the SDK's machine-shared JSON
//! stores (the pin store, the device registry, the revocation store).
//!
//! Each store publishes a whole snapshot by writing a sibling temp file and
//! renaming it over the target. The temp name used to be built with
//! `path.with_extension("tmp.<pid>")`, which *replaces* the extension rather
//! than extending the name: `profile.json` and `profile.pins` both staged
//! through `profile.tmp.<pid>`, while their `.lock` sidecars keep the full
//! filename and so never serialize the two stores against each other. One
//! store's save could then truncate the other's temp and rename the wrong
//! bytes over its own target (PSTORE-03).
//!
//! [`create_staging`] keeps the **full destination filename** and makes every
//! staging file unique per save — `<filename>.tmp.<pid>.<seq>`, created with
//! `create_new` — so a collision is an error to retry, never a silent
//! truncation of somebody else's temp. [`durability_barrier`] commits the
//! rename itself to stable storage once it has happened.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Process-wide, so two stores (or two saves of one store) in this process
/// never pick the same candidate; the pid separates processes.
static STAGING_SEQ: AtomicU64 = AtomicU64::new(0);

/// How many already-taken candidates to skip before giving up. A taken name
/// means a stale temp from a crashed earlier process that happened to share
/// this pid — rare, and never more than a handful.
const MAX_ATTEMPTS: usize = 64;

/// The staging-file name for one candidate: `<full filename>.tmp.<pid>.<seq>`
/// beside `path`, built from the raw OS path bytes so a non-UTF-8 path is
/// preserved.
fn staging_candidate(path: &Path, pid: u32, seq: u64) -> PathBuf {
    let mut os = path.as_os_str().to_os_string();
    os.push(format!(".tmp.{pid}.{seq}"));
    PathBuf::from(os)
}

/// Exclusively create a fresh staging file beside `path`, owner-only (0600)
/// from the start on Unix (the mode travels with the inode through the
/// rename; on Windows the per-user directory's inherited ACLs scope access).
///
/// Returns the staging path and the open, empty file. The caller writes,
/// fsyncs, and renames it over `path`, removing it on any failure.
pub(crate) fn create_staging(path: &Path) -> std::io::Result<(PathBuf, std::fs::File)> {
    let pid = std::process::id();
    let mut last = None;
    for _ in 0..MAX_ATTEMPTS {
        let seq = STAGING_SEQ.fetch_add(1, Ordering::Relaxed);
        let candidate = staging_candidate(path, pid, seq);
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            opts.mode(0o600);
        }
        match opts.open(&candidate) {
            Ok(file) => return Ok((candidate, file)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => last = Some(e),
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        match last {
            Some(e) => format!("no unused staging name beside the store: {e}"),
            None => "no unused staging name beside the store".to_string(),
        },
    ))
}

/// Commit a just-completed rename onto `path` to stable storage.
///
/// On POSIX `rename()` updates the directory entry only in memory; until the
/// parent directory is fsynced a crash can revert to the previous file. On
/// other platforms (no directory handles to sync) the renamed file itself is
/// reopened and flushed — the same barrier the A2A admission journal uses.
pub(crate) fn durability_barrier(path: &Path) -> std::io::Result<()> {
    #[cfg(test)]
    if FAIL_DURABILITY_BARRIER.with(std::cell::Cell::get) {
        return Err(std::io::Error::other("injected durability-barrier failure"));
    }
    #[cfg(unix)]
    {
        let parent = match path.parent() {
            Some(p) if !p.as_os_str().is_empty() => p,
            _ => Path::new("."),
        };
        std::fs::File::open(parent).and_then(|dir| dir.sync_all())
    }
    #[cfg(not(unix))]
    {
        std::fs::OpenOptions::new()
            .write(true)
            .open(path)
            .and_then(|f| f.sync_all())
    }
}

#[cfg(test)]
thread_local! {
    /// Fault injection: while set on this thread, [`durability_barrier`]
    /// fails as a full or failing disk would, AFTER the rename published.
    static FAIL_DURABILITY_BARRIER: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Make [`durability_barrier`] fail on this thread until the guard drops.
#[cfg(test)]
pub(crate) fn fail_durability_barrier_for_test() -> impl Drop {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            FAIL_DURABILITY_BARRIER.with(|f| f.set(false));
        }
    }
    FAIL_DURABILITY_BARRIER.with(|f| f.set(true));
    Reset
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distinct_filenames_sharing_a_stem_never_share_a_staging_name() {
        // `profile.json` and `profile.pins` differ only in extension. Their
        // staging files must keep that difference.
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("profile.json");
        let b = dir.path().join("profile.pins");
        let (ta, _fa) = create_staging(&a).unwrap();
        let (tb, _fb) = create_staging(&b).unwrap();
        assert_ne!(ta, tb);
        let name = |p: &Path| p.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name(&ta).starts_with("profile.json.tmp."), "{}", name(&ta));
        assert!(name(&tb).starts_with("profile.pins.tmp."), "{}", name(&tb));
    }

    #[test]
    fn two_saves_of_one_store_get_distinct_staging_files() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("pins.json");
        let (t1, _f1) = create_staging(&p).unwrap();
        let (t2, _f2) = create_staging(&p).unwrap();
        assert_ne!(t1, t2, "a second save must not reuse a live staging file");
    }

    #[test]
    fn a_taken_candidate_is_skipped_not_truncated() {
        // Pre-occupy the next candidate with foreign bytes: create_new must
        // step past it rather than truncate it.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("pins.json");
        let next = STAGING_SEQ.load(Ordering::Relaxed);
        let mut squatted = Vec::new();
        for seq in next..next + 4 {
            let c = staging_candidate(&p, std::process::id(), seq);
            std::fs::write(&c, b"foreign").unwrap();
            squatted.push(c);
        }
        let (t, _f) = create_staging(&p).unwrap();
        assert!(!squatted.contains(&t));
        for c in &squatted {
            assert_eq!(std::fs::read(c).unwrap(), b"foreign");
        }
    }
}
