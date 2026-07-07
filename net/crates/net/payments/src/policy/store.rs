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
//! - **Saves are atomic**: per-pid temp file, owner-only (0600) from
//!   creation, `fsync` before the rename, temp removed on any failure.
//!   Readers see the whole old file or the whole new file, never a tear.
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

        // Sidecar path from the raw OS path bytes — not the lossy
        // `display()` — so non-UTF-8 paths still lock the right file.
        let mut lock_os = store_path.as_os_str().to_os_string();
        lock_os.push(".lock");
        let lock_path = PathBuf::from(lock_os);

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

/// Atomic save: per-pid temp, 0600 from creation on unix, fsync, rename.
async fn save_json<T: Serialize>(path: &Path, value: &T) -> Result<(), StoreError> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| StoreError::io(path, e))?;
        }
    }
    let bytes = serde_json::to_vec_pretty(value).map_err(|e| StoreError::io(path, e))?;

    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    let result: Result<(), StoreError> = async {
        let mut opts = tokio::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            // `mode` is inherent on tokio's OpenOptions (no trait import).
            opts.mode(0o600);
        }
        let mut file = opts.open(&tmp).await.map_err(|e| StoreError::io(&tmp, e))?;
        use tokio::io::AsyncWriteExt as _;
        file.write_all(&bytes)
            .await
            .map_err(|e| StoreError::io(&tmp, e))?;
        file.flush().await.map_err(|e| StoreError::io(&tmp, e))?;
        // fsync before the rename so a crash right after the rename can
        // never surface a truncated store.
        file.sync_all().await.map_err(|e| StoreError::io(&tmp, e))?;
        drop(file);
        tokio::fs::rename(&tmp, path)
            .await
            .map_err(|e| StoreError::io(path, e))
    }
    .await;

    if result.is_err() {
        let _ = tokio::fs::remove_file(&tmp).await;
    }
    result
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
    let _guard = LockGuard::acquire(path).await?;
    let mut state: T = load_json(path).await?;
    let result = f(&mut state);
    save_json(path, &state).await?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

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
