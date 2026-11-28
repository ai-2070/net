//! The locked payment state store — the `sdk/src/pins.rs` regime, reused
//! for every piece of shared payment state (the engine's replay index and
//! quote records now; the spend-policy counters in Workstream 3).
//!
//! The rules, copied verbatim from the pin store because each one closes
//! a real failure mode:
//!
//! - **State is machine-shared**; every consumer resolves the same file
//!   through one path resolver, or "approved anywhere is approved
//!   everywhere" breaks.
//! - **Every read-modify-write runs under a cross-process advisory lock**
//!   on a sidecar `.lock` file — the lock is *not* on the store file
//!   because the atomic-rename save replaces the store inode, which would
//!   silently drop a lock held on it.
//! - **Lock acquisition is a poll loop with async exponential backoff**
//!   (1ms doubling, capped at 50ms), never a blocking acquire: a blocking
//!   acquire parks a `spawn_blocking` thread for the whole wait, and
//!   enough contending mutators starve the pool the holder's own tokio
//!   I/O needs — deadlock. Async sleep parks no thread.
//! - **Saves are atomic**: per-pid temp file, owner-only from creation
//!   (`0600` on unix, an explicit owner-only DACL on Windows — see the
//!   crate-private `policy::file_mode`), `fsync` before the rename, temp
//!   removed on any failure. Readers see the whole old file or the whole
//!   new file, never a tear.
//! - **Missing file = empty state** (the first-run case); a
//!   present-but-unparseable file is [`StoreError::Corrupt`], never a
//!   silent reset.

use std::path::{Path, PathBuf};

use fs2::FileExt as _;
use serde::de::DeserializeOwned;
use serde::Serialize;

/// Errors from the locked store.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StoreError {
    #[error("payment store I/O error at {path}: {reason}")]
    Io { path: String, reason: String },
    #[error("payment store at {path} is corrupt: {reason}")]
    Corrupt { path: String, reason: String },
}

impl StoreError {
    fn io(path: &Path, e: impl std::fmt::Display) -> Self {
        Self::Io {
            path: path.display().to_string(),
            reason: e.to_string(),
        }
    }
}

/// The per-user default payment policy path:
/// `<local data>/net-mesh/payment-policy.json` (Workstream 3's spend
/// policy + counters).
pub fn default_payment_policy_path() -> Option<PathBuf> {
    default_store_file("payment-policy.json")
}

/// The per-user default payment engine state path:
/// `<local data>/net-mesh/payment-engine.json` (provider-side quote
/// records, replay index, verification chains).
pub fn default_payment_engine_path() -> Option<PathBuf> {
    default_store_file("payment-engine.json")
}

/// The per-user default A2A purchase-attempt path:
/// `<local data>/net-mesh/a2a-purchases.json` — the caller-side durable
/// record of one authoritative purchase attempt per intent key
/// (`flow::a2a`). Beside `payment-policy.json` on purpose: the attempt
/// and the spend reservation it holds are read together during recovery.
pub fn default_a2a_purchase_path() -> Option<PathBuf> {
    default_store_file("a2a-purchases.json")
}

fn default_store_file(name: &str) -> Option<PathBuf> {
    dirs::data_local_dir()
        .or_else(dirs::home_dir)
        .map(|d| d.join("net-mesh").join(name))
}

/// Cross-process advisory lock on `<store>.lock`. Dropping the guard
/// closes the fd and releases the OS lock.
pub struct LockGuard {
    _file: std::fs::File,
}

impl LockGuard {
    /// Acquire the sidecar lock with async exponential backoff.
    pub async fn acquire(store_path: &Path) -> Result<Self, StoreError> {
        const MAX_BACKOFF: std::time::Duration = std::time::Duration::from_millis(50);

        let lock_path = lock_path_for(store_path);

        if let Some(parent) = lock_path.parent() {
            if !parent.as_os_str().is_empty() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .map_err(|e| StoreError::io(&lock_path, e))?;
            }
        }

        // Open/create on the blocking pool. `truncate(false)`: the lock
        // file's content is never written, only its lock matters — and a
        // sibling's lock file must not be clobbered.
        let path_for_open = lock_path.clone();
        let file = tokio::task::spawn_blocking(move || {
            std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(false)
                .open(&path_for_open)
        })
        .await
        .map_err(|e| StoreError::io(&lock_path, e))?
        .map_err(|e| StoreError::io(&lock_path, e))?;

        let mut backoff = std::time::Duration::from_millis(1);
        loop {
            match file.try_lock_exclusive() {
                Ok(()) => return Ok(Self { _file: file }),
                Err(e) if e.kind() == fs2::lock_contended_error().kind() => {
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(MAX_BACKOFF);
                }
                Err(e) => return Err(StoreError::io(&lock_path, e)),
            }
        }
    }
}

/// Lock-free read: missing file = `T::default()`; the atomic rename
/// guarantees no torn read.
pub async fn load_json<T>(path: &Path) -> Result<T, StoreError>
where
    T: DeserializeOwned + Default,
{
    let raw = match tokio::fs::read(path).await {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(T::default()),
        Err(e) => return Err(StoreError::io(path, e)),
    };
    serde_json::from_slice(&raw).map_err(|e| StoreError::Corrupt {
        path: path.display().to_string(),
        reason: e.to_string(),
    })
}

/// Serialize a store for saving.
fn encode_store<T: Serialize>(path: &Path, value: &T) -> Result<Vec<u8>, StoreError> {
    // Compact, not pretty: the store is machine-shared state, never
    // hand-authored config, and pretty-printing costs ~11% more bytes to
    // serialize, write, and fsync on *every* dirty mutation — a tax paid
    // on a path whose cost is already linear in file size. Both formats
    // parse identically, so this is forward- and backward-compatible with
    // stores written by an older build. (The canonical envelope encoding
    // pinned by the cross-language golden vectors is
    // `core::canonical` — a different encoder entirely, untouched here.)
    serde_json::to_vec(value).map_err(|e| StoreError::io(path, e))
}

/// Publish `bytes` as the store at `path` — atomically and durably, on the
/// blocking pool, while the transaction's lock guard is held — then release
/// the lock.
///
/// The guard MOVES into the blocking task (BUG_AUDIT_2026_11_14 PAY-04).
/// Tokio cannot abort a blocking task that has started, so when the save
/// was a series of `spawn_blocking` steps awaited by the async caller, a
/// cancelled caller dropped its lock while a step (the final rename, say)
/// kept running. A second transaction could then take the lock and commit,
/// and the first one's delayed rename landed over it, publishing older
/// state. Now the lock is released only when the whole filesystem
/// sequence has finished, whether or not anyone is still awaiting it.
async fn publish_locked(guard: LockGuard, path: &Path, bytes: Vec<u8>) -> Result<(), StoreError> {
    let store = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let _guard = guard;
        publish_blocking(&store, &bytes)
    })
    .await
    .map_err(|e| StoreError::io(path, e))?
}

/// The synchronous save: a uniquely named owner-only staging file, written
/// and fsynced, renamed over the store, then the directory fsynced.
fn publish_blocking(path: &Path, bytes: &[u8]) -> Result<(), StoreError> {
    use std::io::Write as _;
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| StoreError::io(path, e))?;
        }
    }

    // Created with its permissions already in place — owner-only from the
    // instant the name exists. This is why it is a create rather than an
    // open-then-restrict: on Windows, access is evaluated when a handle is
    // opened, so a reader that got in before a later DACL change keeps what
    // it was granted for the life of that handle. There must be no window
    // at all, not merely a short one.
    //
    // `create_new` also means a leftover temp from a crashed same-pid
    // process cannot be silently reused with its old permissions. A taken
    // name is skipped for the next candidate, never removed: an existing
    // file there is not established to be ours.
    const ATTEMPTS: usize = 64;
    let mut tmp = staging_path(path);
    let mut attempt = 1;
    let mut file = loop {
        match super::file_mode::create_owner_only(&tmp) {
            Ok(file) => break file,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && attempt < ATTEMPTS => {
                attempt += 1;
                tmp = staging_path(path);
            }
            Err(e) => return Err(StoreError::io(&tmp, e)),
        }
    };

    let staged: Result<(), StoreError> = (|| {
        file.write_all(bytes).map_err(|e| StoreError::io(&tmp, e))?;
        // fsync before the rename so a crash right after the rename can
        // never surface a truncated store.
        file.sync_all().map_err(|e| StoreError::io(&tmp, e))?;
        drop(file);
        #[cfg(test)]
        test_hooks::pause_before_rename(path);
        rename_durably(&tmp, path).map_err(|e| StoreError::io(path, e))
    })();
    if staged.is_err() {
        // Only a temp this save created is removed: a name it could not
        // create belongs to someone else.
        let _ = std::fs::remove_file(&tmp);
        return staged;
    }

    // BUG_AUDIT_2026_11_14 PAY-05: the rename is atomic but, on POSIX, not
    // durable until the containing directory is synced. A power loss after
    // an acknowledged reservation or redemption could otherwise restore the
    // predecessor store, losing the acknowledged state and reopening budget
    // or execution admission. A transaction is reported committed only
    // once the directory entry is durable. If this sync fails, the new
    // store is already visible but not known to be durable, so the
    // transaction reports the failure rather than success.
    sync_parent_dir(path).map_err(|e| StoreError::Io {
        path: path.display().to_string(),
        reason: format!("store renamed into place but its directory could not be synced: {e}"),
    })
}

/// Fsync the directory holding `path`, so a rename into it survives power
/// loss.
#[cfg(unix)]
fn sync_parent_dir(path: &Path) -> std::io::Result<()> {
    #[cfg(test)]
    test_hooks::count_dir_sync();
    let parent = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    std::fs::File::open(parent)?.sync_all()
}

/// Windows has no directory fsync (`FlushFileBuffers` on a directory
/// handle is `ERROR_ACCESS_DENIED`). There the rename itself is made
/// durable instead, by [`rename_durably`]'s `MOVEFILE_WRITE_THROUGH`, so
/// nothing is left to do here. Same policy as the org revocation store's
/// `fsync_parent_dir`.
#[cfg(not(unix))]
fn sync_parent_dir(_path: &Path) -> std::io::Result<()> {
    #[cfg(test)]
    test_hooks::count_dir_sync();
    Ok(())
}

/// Atomically replace `dest` with `src`. On POSIX a plain rename;
/// [`sync_parent_dir`] makes it durable afterwards.
#[cfg(not(windows))]
fn rename_durably(src: &Path, dest: &Path) -> std::io::Result<()> {
    std::fs::rename(src, dest)
}

/// Atomically and DURABLY replace `dest` with `src` on Windows (PAY-05).
///
/// `std::fs::rename` is `MoveFileExW(MOVEFILE_REPLACE_EXISTING)` without
/// write-through: the new directory entry can sit in the volume's metadata
/// cache, and a power loss shortly after an acknowledged commit could bring
/// back the predecessor store. NTFS journaling keeps metadata CONSISTENT
/// after a crash; it does not promise a completed rename was flushed.
/// `MOVEFILE_WRITE_THROUGH` is documented to flush the move before
/// returning.
#[cfg(windows)]
fn rename_durably(src: &Path, dest: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };
    // Verbatim (`\\?\`) form, as `std::fs` builds internally: a raw
    // `MoveFileExW` is otherwise limited to MAX_PATH, and a valid store
    // name near the 255-byte component limit already exceeds it.
    // `std::path::absolute` resolves `.`/`..` and `/` first, which a
    // verbatim path no longer would.
    let wide = |p: &Path| -> std::io::Result<Vec<u16>> {
        let abs = std::path::absolute(p)?;
        let raw: Vec<u16> = abs.as_os_str().encode_wide().collect();
        let bs = u16::from(b'\\');
        let q = u16::from(b'?');
        let mut out: Vec<u16> = Vec::with_capacity(raw.len() + 9);
        if raw.starts_with(&[bs, bs, q, bs]) {
            // Already verbatim.
        } else if raw.starts_with(&[bs, bs]) {
            // UNC `\\server\share\…` → `\\?\UNC\server\share\…`.
            out.extend(r"\\?\UNC".encode_utf16());
            out.extend_from_slice(&raw[1..]);
            out.push(0);
            return Ok(out);
        } else {
            out.extend(r"\\?\".encode_utf16());
        }
        out.extend_from_slice(&raw);
        out.push(0);
        Ok(out)
    };
    let (from, to) = (wide(src)?, wide(dest)?);
    // SAFETY: both buffers are NUL-terminated UTF-16 and outlive the call;
    // no pointer escapes it, and the result is checked below.
    let ok = unsafe {
        MoveFileExW(
            from.as_ptr(),
            to.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if ok == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// Test-only seams into the blocking publish (PAY-04 / PAY-05 witnesses).
#[cfg(test)]
mod test_hooks {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::mpsc::{Receiver, Sender};

    type Pause = (std::path::PathBuf, Sender<()>, Receiver<()>);

    /// `(store, entered, resume)`: the next publish OF `store` signals
    /// `entered` just before its rename and waits for `resume`. One-shot,
    /// and scoped to one store path, so tests sharing a process (plain
    /// `cargo test`) cannot catch each other's saves.
    static PAUSE: parking_lot::Mutex<Option<Pause>> = parking_lot::Mutex::new(None);
    static DIR_SYNCS: AtomicU64 = AtomicU64::new(0);

    pub(super) fn arm_pause_before_rename(
        store: &std::path::Path,
        entered: Sender<()>,
        resume: Receiver<()>,
    ) {
        *PAUSE.lock() = Some((store.to_path_buf(), entered, resume));
    }

    pub(super) fn pause_before_rename(store: &std::path::Path) {
        let armed = {
            let mut slot = PAUSE.lock();
            match slot.as_ref() {
                Some((armed_for, _, _)) if armed_for == store => slot.take(),
                _ => None,
            }
        };
        if let Some((_, entered, resume)) = armed {
            let _ = entered.send(());
            let _ = resume.recv();
        }
    }

    pub(super) fn count_dir_sync() {
        DIR_SYNCS.fetch_add(1, Ordering::SeqCst);
    }

    pub(super) fn dir_syncs() -> u64 {
        DIR_SYNCS.load(Ordering::SeqCst)
    }
}

/// The locked read-modify-write transaction. The load happens *inside*
/// the lock — never a stale snapshot — which is exactly what makes a
/// spend counter or replay-index check-and-claim safe across processes.
/// The closure's verdict crosses back out as `R`.
pub async fn mutate_json<T, R, F>(path: &Path, f: F) -> Result<R, StoreError>
where
    T: DeserializeOwned + Serialize + Default,
    F: FnOnce(&mut T) -> R,
{
    let guard = LockGuard::acquire(path).await?;
    let mut state: T = load_json(path).await?;
    let result = f(&mut state);
    let bytes = encode_store(path, &state)?;
    publish_locked(guard, path, bytes).await?;
    Ok(result)
}

/// Like [`mutate_json`], but the closure returns `(R, dirty)`: the store is
/// serialized + fsync'd + renamed **only when `dirty` is true**. A read-only
/// outcome — e.g. a payment-redemption *denial*, which inspects a record but
/// changes nothing — pays the lock + load, not a whole-file durable write.
///
/// This closes a write-amplification / denial-of-service surface: with plain
/// `mutate_json`, an outcome that mutates nothing still re-serializes and
/// `fsync`s the entire store, so a caller spraying (say) unknown quote ids
/// could force one global-lock + fsync per attempt. See
/// `docs/internal/performance/payments-redeem-write-amplification.md`.
///
/// **At-most-once is unchanged.** The load, decision, and conditional save
/// all run under the *same* advisory lock, so a check-and-set
/// (settled-and-not-yet-redeemed → mark redeemed → `dirty = true`) is still
/// atomic across processes; we only skip the write when the closure reports
/// there was nothing to persist. A closure that mutates state but returns
/// `dirty = false` would silently drop that mutation — so the dirty flag
/// must be derived from the same branch that did the mutating.
pub async fn mutate_json_if_changed<T, R, F>(path: &Path, f: F) -> Result<R, StoreError>
where
    T: DeserializeOwned + Serialize + Default,
    F: FnOnce(&mut T) -> (R, bool),
{
    let guard = LockGuard::acquire(path).await?;
    let mut state: T = load_json(path).await?;
    let (result, dirty) = f(&mut state);
    if dirty {
        let bytes = encode_store(path, &state)?;
        publish_locked(guard, path, bytes).await?;
    }
    Ok(result)
}

/// The lock sidecar for `store`: `<store>.lock`, exactly as every earlier
/// version names it, so processes of different versions still exclude each
/// other. Only a filename too long to take the `.lock` suffix within the
/// 255-byte component limit — one no earlier version could lock at all —
/// uses the bounded per-store name from [`sibling_path`] instead.
fn lock_path_for(store: &Path) -> PathBuf {
    // From the raw OS path bytes — not the lossy `display()` — so non-UTF-8
    // paths still lock the right file.
    let mut lock_os = store.as_os_str().to_os_string();
    lock_os.push(".lock");
    let legacy = PathBuf::from(lock_os);
    if legacy.file_name().is_some_and(|n| n.len() <= 255) {
        legacy
    } else {
        sibling_path(store, ".lock")
    }
}

/// A unique staging name beside `path` that keeps its FULL filename:
/// `<filename>.tmp.<pid>.<seq>`. `with_extension("tmp.<pid>")` replaced the
/// extension instead, so two stores sharing a stem in one directory (`x.json`
/// and `x.pins`) staged through ONE name while their locks never serialized
/// them (PSTORE-03). The sequence is process-wide, so two saves in one
/// process never share a name either; create it with `create_new`.
///
/// A store whose filename is already near the 255-byte component limit has
/// no room for the suffix, and the save would fail with ENAMETOOLONG; it then
/// stages under a short name beside it (see [`sibling_path`]), still unique
/// by pid and sequence.
fn staging_path(path: &Path) -> PathBuf {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    sibling_path(path, &format!(".tmp.{}.{seq}", std::process::id()))
}

/// `<filename><suffix>` beside `path` — or, when that would exceed the
/// 255-byte file-name component limit (ext4/XFS/APFS bytes, NTFS UTF-16
/// units), the short `.net-mesh-store.<id><suffix>`, where `<id>` is 16 hex
/// digits of a hash of the store's filename. The short form is therefore
/// specific to its store, never a name another store in the directory (or a
/// file the operator chose) shares by construction.
fn sibling_path(path: &Path, suffix: &str) -> PathBuf {
    const MAX_NAME_COMPONENT: usize = 255;
    let file_name = path.file_name().unwrap_or_default();
    let mut name = file_name.to_os_string();
    name.push(suffix);
    if name.len() <= MAX_NAME_COMPONENT {
        return path.with_file_name(name);
    }
    let id: String = blake3::hash(file_name.as_encoded_bytes()).to_hex()[..16].to_string();
    path.with_file_name(format!(".net-mesh-store.{id}{suffix}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// PSTORE-03: stores sharing a stem never share a staging name, and two
    /// saves of one store never do either.
    #[test]
    fn staging_names_keep_the_full_filename_and_are_unique() {
        let dir = std::path::Path::new("dir");
        let a = staging_path(&dir.join("profile.json"));
        let b = staging_path(&dir.join("profile.pins"));
        let name = |p: &std::path::Path| p.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name(&a).starts_with("profile.json.tmp."), "{}", name(&a));
        assert!(name(&b).starts_with("profile.pins.tmp."), "{}", name(&b));
        assert_ne!(
            staging_path(&dir.join("x.json")),
            staging_path(&dir.join("x.json"))
        );
    }

    /// A store filename near the 255-byte limit has no room for the staging
    /// suffix; the stage falls back to a short name in the same directory
    /// instead of failing the save with ENAMETOOLONG.
    #[test]
    fn a_near_limit_store_name_stages_within_the_name_limit() {
        let dir = std::path::Path::new("some").join("dir");
        let long = staging_path(&dir.join("s".repeat(250)));
        let name = long.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.len() <= 255, "{}", name.len());
        assert!(name.starts_with(".net-mesh-store."), "{name}");
        assert!(name.contains(".tmp."), "{name}");
        assert_eq!(long.parent(), Some(dir.as_path()));
    }

    /// A file already at the staging name is not this save's: it is skipped
    /// for the next candidate and left untouched, never removed as stale.
    /// (The next candidate is predicted from the process-wide sequence;
    /// nextest runs each test in its own process, so nothing else advances
    /// it in between.)
    #[tokio::test]
    async fn a_taken_staging_name_is_skipped_not_removed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let probe = staging_path(&path).to_string_lossy().into_owned();
        let (head, seq) = probe.rsplit_once('.').unwrap();
        let next = PathBuf::from(format!("{head}.{}", seq.parse::<u64>().unwrap() + 1));
        std::fs::write(&next, b"not ours").unwrap();
        mutate_json::<Counters, _, _>(&path, |s| {
            *s.counts.entry("a".into()).or_default() += 1;
        })
        .await
        .expect("the save stages past the taken name");
        assert_eq!(std::fs::read(&next).unwrap(), b"not ours");
        let loaded: Counters = load_json(&path).await.unwrap();
        assert_eq!(loaded.counts["a"], 1);
    }

    /// The short fallback is specific to its store: two near-limit stores in
    /// one directory never share a fallback prefix, and one store always
    /// gets the same one.
    #[test]
    fn the_short_fallback_is_specific_to_its_store() {
        let dir = std::path::Path::new("some").join("dir");
        let prefix = |p: &std::path::Path| {
            let n = p.file_name().unwrap().to_string_lossy().into_owned();
            n[..n.find(".tmp.").unwrap()].to_string()
        };
        let a = dir.join("a".repeat(250));
        let b = dir.join("b".repeat(250));
        assert_ne!(prefix(&staging_path(&a)), prefix(&staging_path(&b)));
        assert_eq!(prefix(&staging_path(&a)), prefix(&staging_path(&a)));
    }

    /// Red-witness: with the suffix appended to a near-limit store name the
    /// staging file (and, above 250 bytes, the `.lock` sidecar) could not be
    /// created, so a valid custom store path could never be saved. Up to the
    /// 255-byte limit itself.
    #[tokio::test]
    async fn a_near_limit_store_path_saves() {
        for len in [250, 251, 255] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("s".repeat(len));
            mutate_json::<Counters, _, _>(&path, |s| {
                *s.counts.entry("a".into()).or_default() += 1;
            })
            .await
            .unwrap_or_else(|e| panic!("a {len}-byte store name saves: {e}"));
            let loaded: Counters = load_json(&path).await.unwrap();
            assert_eq!(loaded.counts["a"], 1, "{len}");
        }
    }

    /// The lock sidecar keeps the name every earlier version uses whenever it
    /// fits, so processes of different versions still exclude each other.
    #[test]
    fn the_lock_sidecar_keeps_its_name_when_it_fits() {
        let dir = std::path::Path::new("some").join("dir");
        assert_eq!(
            lock_path_for(&dir.join("state.json")),
            dir.join("state.json.lock")
        );
        assert_eq!(
            lock_path_for(&dir.join("s".repeat(250))),
            dir.join(format!("{}.lock", "s".repeat(250)))
        );
        let long = lock_path_for(&dir.join("s".repeat(251)));
        let name = long.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.len() <= 255 && name.ends_with(".lock"), "{name}");
        assert_eq!(long.parent(), Some(dir.as_path()));
    }

    #[derive(Default, serde::Serialize, serde::Deserialize)]
    struct Counters {
        #[serde(default)]
        counts: BTreeMap<String, u64>,
    }

    #[test]
    fn default_paths_land_in_the_net_mesh_dir() {
        let p = default_payment_policy_path().expect("test hosts have a data dir");
        assert!(p.ends_with(PathBuf::from("net-mesh").join("payment-policy.json")));
        let p = default_payment_engine_path().expect("test hosts have a data dir");
        assert!(p.ends_with(PathBuf::from("net-mesh").join("payment-engine.json")));
    }

    #[tokio::test]
    async fn missing_file_is_empty_corrupt_file_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let loaded: Counters = load_json(&path).await.unwrap();
        assert!(loaded.counts.is_empty());

        tokio::fs::write(&path, b"{not json").await.unwrap();
        assert!(matches!(
            load_json::<Counters>(&path).await,
            Err(StoreError::Corrupt { .. })
        ));
    }

    #[tokio::test]
    async fn mutate_round_trips_and_leaves_no_temp() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let verdict = mutate_json::<Counters, _, _>(&path, |s| {
            *s.counts.entry("a".into()).or_default() += 1;
            s.counts["a"]
        })
        .await
        .unwrap();
        assert_eq!(verdict, 1);

        let loaded: Counters = load_json(&path).await.unwrap();
        assert_eq!(loaded.counts["a"], 1);

        let mut entries = std::fs::read_dir(dir.path()).unwrap();
        assert!(entries.all(|e| {
            let name = e.unwrap().file_name();
            let name = name.to_string_lossy();
            name == "state.json" || name == "state.json.lock"
        }));
    }

    /// Saves are compact, and a store written by an older (pretty-printing)
    /// build still loads — the format change is a pure byte reduction, not a
    /// schema change, so a rolling upgrade never sees a corrupt store.
    #[tokio::test]
    async fn saves_are_compact_and_pretty_stores_still_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");

        // A store left behind by an older build: pretty-printed.
        let mut legacy = Counters::default();
        legacy.counts.insert("a".into(), 1);
        tokio::fs::write(&path, serde_json::to_vec_pretty(&legacy).unwrap())
            .await
            .unwrap();
        let loaded: Counters = load_json(&path).await.unwrap();
        assert_eq!(loaded.counts["a"], 1, "a pretty store must still parse");

        // Rewriting it emits compact bytes. The assertion is the strongest
        // one available: compact output contains no raw newline ANYWHERE,
        // not merely no newline-plus-indent run. That holds even for
        // newline-bearing data, because serde escapes a newline inside a
        // string as the two bytes `\n` — which is why the second key here
        // carries one.
        mutate_json::<Counters, _, _>(&path, |s| {
            s.counts.insert("b\nwith a newline in the key".into(), 2);
        })
        .await
        .unwrap();
        let raw = tokio::fs::read(&path).await.unwrap();
        assert!(
            !raw.contains(&b'\n'),
            "saves must be compact, not pretty-printed"
        );
        let loaded: Counters = load_json(&path).await.unwrap();
        assert_eq!(loaded.counts.len(), 2, "the mutation still round-trips");
        assert_eq!(
            loaded.counts["b\nwith a newline in the key"], 2,
            "escaping is what makes the no-raw-newline assertion safe"
        );
    }

    /// `mutate_json_if_changed` writes on a dirty pass and skips the write
    /// on a clean one. "Skips the write" is checked by the store inode: a
    /// save renames a fresh temp over the file (new inode), so a clean pass
    /// that leaves the inode is proof no serialize/fsync/rename happened.
    #[cfg(unix)]
    #[tokio::test]
    async fn mutate_if_changed_writes_only_when_dirty() {
        use std::os::unix::fs::MetadataExt as _;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");

        // A dirty pass creates the file.
        mutate_json_if_changed::<Counters, _, _>(&path, |s| {
            s.counts.insert("a".into(), 1);
            ((), true)
        })
        .await
        .unwrap();
        let ino0 = std::fs::metadata(&path).unwrap().ino();

        // A clean pass reads and returns dirty=false → no rewrite.
        let seen = mutate_json_if_changed::<Counters, _, _>(&path, |s| {
            (s.counts.get("a").copied(), false)
        })
        .await
        .unwrap();
        assert_eq!(seen, Some(1));
        assert_eq!(
            std::fs::metadata(&path).unwrap().ino(),
            ino0,
            "a clean (dirty=false) pass must not rewrite the store"
        );

        // A dirty pass persists (rename → new inode) and the mutation lands.
        mutate_json_if_changed::<Counters, _, _>(&path, |s| {
            s.counts.insert("b".into(), 2);
            ((), true)
        })
        .await
        .unwrap();
        assert_ne!(
            std::fs::metadata(&path).unwrap().ino(),
            ino0,
            "a dirty pass must persist the store"
        );
        let loaded: Counters = load_json(&path).await.unwrap();
        assert_eq!(loaded.counts.len(), 2);
    }

    /// The pin store's lost-update regression, on this machinery: two
    /// concurrent mutators against one file must both land.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_mutations_do_not_lose_updates() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let a = mutate_json::<Counters, _, _>(&path, |s| {
            s.counts.insert("a".into(), 1);
        });
        let b = mutate_json::<Counters, _, _>(&path, |s| {
            s.counts.insert("b".into(), 1);
        });
        let (ra, rb) = tokio::join!(a, b);
        ra.unwrap();
        rb.unwrap();
        let loaded: Counters = load_json(&path).await.unwrap();
        assert_eq!(loaded.counts.len(), 2, "one mutation was lost");
    }

    /// BUG_AUDIT_2026_11_14 PAY-04: cancelling a transaction while its
    /// blocking save is in flight must not release the store's lock before
    /// that save finishes. Before the fix the lock guard lived in the async
    /// caller, so a cancelled caller released the lock while its rename was
    /// still pending on the blocking pool. A second transaction then
    /// committed, and the first one's delayed rename published older state
    /// over it, losing the second update.
    ///
    /// The first save is paused (by a test hook) just before its rename;
    /// its caller is then aborted.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_cancelled_transaction_keeps_the_lock_until_its_save_lands() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (resume_tx, resume_rx) = std::sync::mpsc::channel();
        test_hooks::arm_pause_before_rename(&path, entered_tx, resume_rx);

        let first = tokio::spawn({
            let path = path.clone();
            async move {
                mutate_json::<Counters, _, _>(&path, |s| {
                    s.counts.insert("first".into(), 1);
                })
                .await
            }
        });
        tokio::task::spawn_blocking(move || entered_rx.recv().unwrap())
            .await
            .unwrap();
        first.abort();
        let _ = first.await;

        let second = tokio::spawn({
            let path = path.clone();
            async move {
                mutate_json::<Counters, _, _>(&path, |s| {
                    s.counts.insert("second".into(), 1);
                })
                .await
            }
        });
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert!(
            !second.is_finished(),
            "the second transaction must wait for the cancelled one's save"
        );

        resume_tx.send(()).unwrap();
        second.await.unwrap().unwrap();
        let loaded: Counters = load_json(&path).await.unwrap();
        assert_eq!(
            loaded.counts.get("second"),
            Some(&1),
            "the second, acknowledged update must not be overwritten"
        );
        assert_eq!(
            loaded.counts.get("first"),
            Some(&1),
            "the first save ran to completion before the second loaded"
        );
    }

    /// BUG_AUDIT_2026_11_14 PAY-05: a committed transaction syncs the
    /// store's directory after the rename (the sync is a no-op on Windows,
    /// but it is reached on every platform).
    #[tokio::test]
    async fn a_committed_transaction_syncs_its_directory() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let before = test_hooks::dir_syncs();
        mutate_json::<Counters, _, _>(&path, |s| {
            s.counts.insert("a".into(), 1);
        })
        .await
        .unwrap();
        assert!(test_hooks::dir_syncs() > before);
    }

    /// The pool-exhaustion regression: many contenders on a tiny runtime
    /// must drain (async backoff parks no blocking thread).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn contended_mutations_make_progress_under_a_tiny_pool() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let tasks: Vec<_> = (0..24)
            .map(|i| {
                let path = path.clone();
                tokio::spawn(async move {
                    mutate_json::<Counters, _, _>(&path, move |s| {
                        s.counts.insert(format!("k{i}"), 1);
                    })
                    .await
                })
            })
            .collect();
        let all = futures_join_all(tasks);
        tokio::time::timeout(std::time::Duration::from_secs(20), all)
            .await
            .expect("contended mutators deadlocked");
        let loaded: Counters = load_json(&path).await.unwrap();
        assert_eq!(loaded.counts.len(), 24);
    }

    async fn futures_join_all(tasks: Vec<tokio::task::JoinHandle<Result<(), StoreError>>>) {
        for t in tasks {
            t.await.expect("join").expect("mutate");
        }
    }
}
