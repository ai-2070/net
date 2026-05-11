//! `Redex` — manager owning the `ChannelName -> RedexFile` map.
//!
//! Holds an optional reference to an [`AuthGuard`](super::super::AuthGuard)
//! plus a local origin-hash. When auth is wired up, `open_file` rejects
//! opens unless `(origin, canonical channel name)` has been explicitly
//! authorized via [`AuthGuard::allow_channel`]. The 16-bit wire
//! `channel_hash` alone is not sufficient here — at mesh scale it
//! collides often enough to allow ACL bypass between unrelated names,
//! and even a 64-bit non-cryptographic hash would be crackable by
//! birthday search offline. Keying on the canonical name is the only
//! collision-free answer.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use dashmap::DashMap;

use super::super::channel::{AuthGuard, ChannelName};
use super::config::RedexFileConfig;
use super::error::RedexError;
use super::file::RedexFile;

#[cfg(feature = "redex-disk")]
use std::path::PathBuf;

/// Manager for a set of RedEX files bound to channel names.
pub struct Redex {
    files: DashMap<ChannelName, RedexFile>,
    auth: Option<Arc<AuthGuard>>,
    origin_hash: u64,
    #[cfg(feature = "redex-disk")]
    persistent_dir: Option<PathBuf>,
    /// Cumulative count of `build_file` invocations. Sits next to the
    /// `files` map purely so regression tests can assert that
    /// concurrent `open_file` calls for the same name don't both
    /// build — a previous version had two threads race past the
    /// `files.get()` precheck, both run `build_file`, and the loser
    /// of the subsequent `or_insert` was dropped without `close()`,
    /// leaking its `Interval` fsync task and dup file handles for
    /// the lifetime of the runtime.
    build_count: AtomicU64,
}

impl Redex {
    /// Create a manager without auth enforcement. Suitable for
    /// single-process tests and local workloads.
    pub fn new() -> Self {
        Self {
            files: DashMap::new(),
            auth: None,
            origin_hash: 0,
            #[cfg(feature = "redex-disk")]
            persistent_dir: None,
            build_count: AtomicU64::new(0),
        }
    }

    /// Create a manager that rejects `open_file` unless the
    /// `(origin_hash, channel)` pair has been authorized by `guard`
    /// via [`AuthGuard::allow_channel`]. Uses the exact 64-bit
    /// channel identity, not the 16-bit wire hash — see the module
    /// docs for rationale.
    pub fn with_auth(guard: Arc<AuthGuard>, origin_hash: u64) -> Self {
        Self {
            files: DashMap::new(),
            auth: Some(guard),
            origin_hash,
            #[cfg(feature = "redex-disk")]
            persistent_dir: None,
            build_count: AtomicU64::new(0),
        }
    }

    /// Set the base directory for disk-backed (`persistent: true`)
    /// files. All files opened with `persistent: true` use
    /// `<dir>/<channel_path>/{idx,dat}` for durability.
    #[cfg(feature = "redex-disk")]
    pub fn with_persistent_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.persistent_dir = Some(dir.into());
        self
    }

    /// Open (create if absent) a RedEX file bound to `name`.
    ///
    /// Re-opening an existing name returns the existing handle. The
    /// `config` argument is honored only on first open; subsequent
    /// opens ignore it and return the live file.
    ///
    /// With `persistent: true`, the manager must have been configured
    /// via `with_persistent_dir` (feature `redex-disk`) — otherwise
    /// `open_file` returns a [`RedexError::Channel`] that describes
    /// the missing base dir.
    pub fn open_file(
        &self,
        name: &ChannelName,
        config: RedexFileConfig,
    ) -> Result<RedexFile, RedexError> {
        if let Some(auth) = &self.auth {
            // Use the canonical-name ACL for the storage decision —
            // `is_authorized` (16-bit hash) is reserved for the
            // fast-path packet check where AEAD integrity backstops
            // any bloom-filter false positives. Storage access has
            // no such backstop, and even a 64-bit non-cryptographic
            // hash would be birthday-crackable offline, so the ACL
            // keys on the full canonical name.
            // Widen the 32-bit local origin_hash to match
            // `AuthGuard`'s 64-bit key. The guard keeps the local
            // entity and remote subscribers in disjoint key ranges
            // simply by the natural spread of node_ids — the local
            // entity lives in the lower 2^32 and remote subscribers'
            // full node_ids occupy the full range, so there is no
            // cross-contamination.
            if !auth.is_authorized_full(self.origin_hash, name) {
                return Err(RedexError::Unauthorized);
            }
        }

        // Lock-free fast path for the common re-open case: avoid taking
        // a shard write entry when the file is already present.
        if let Some(existing) = self.files.get(name) {
            return Ok(existing.clone());
        }

        // First-open path. Take the shard's write entry BEFORE running
        // `build_file`. Holding the entry vacant across the build is
        // what serializes concurrent first-openers for the same name:
        // the loser blocks on the shard write lock and observes the
        // winner's `Occupied` entry on retry. The previous code ran
        // `build_file` outside any lock and resolved with
        // `or_insert(file)`; under `persistent: true` +
        // `FsyncPolicy::Interval` both threads spawned an `Interval`
        // fsync task and held independent file handles, and the
        // loser of `or_insert` was dropped without `close()` — so
        // its Notify never fired and the leaked task plus dup
        // handles outlived the call.
        use dashmap::mapref::entry::Entry;
        match self.files.entry(name.clone()) {
            Entry::Occupied(e) => Ok(e.get().clone()),
            Entry::Vacant(e) => {
                let file = self.build_file(name, config)?;
                Ok(e.insert(file).clone())
            }
        }
    }

    fn build_file(
        &self,
        name: &ChannelName,
        config: RedexFileConfig,
    ) -> Result<RedexFile, RedexError> {
        self.build_count.fetch_add(1, Ordering::Relaxed);
        #[cfg(feature = "redex-disk")]
        if config.persistent {
            let dir = self.persistent_dir.as_ref().ok_or_else(|| {
                RedexError::Channel(
                    "config.persistent=true requires Redex::with_persistent_dir(...)".into(),
                )
            })?;
            return RedexFile::open_persistent(name.clone(), config, dir);
        }
        Ok(RedexFile::new(name.clone(), config))
    }

    /// Cumulative number of times `build_file` has run on this manager.
    /// Increments once per *first* open of a `ChannelName`; re-opens of
    /// an already-built file do not. Tests assert this against the
    /// number of distinct names opened to confirm concurrent
    /// `open_file` calls did not double-build.
    #[cfg(test)]
    pub(crate) fn build_count(&self) -> u64 {
        self.build_count.load(Ordering::Relaxed)
    }

    /// Look up an already-opened file.
    pub fn get_file(&self, name: &ChannelName) -> Option<RedexFile> {
        self.files.get(name).map(|r| r.clone())
    }

    /// Close and remove a file. Outstanding tail streams receive
    /// `RedexError::Closed`. No-op if no file is open under `name`.
    pub fn close_file(&self, name: &ChannelName) -> Result<(), RedexError> {
        if let Some((_, file)) = self.files.remove(name) {
            file.close()?;
        }
        Ok(())
    }

    /// Snapshot list of currently open files. Cheap clone.
    pub fn open_files(&self) -> Vec<RedexFile> {
        self.files.iter().map(|r| r.value().clone()).collect()
    }

    /// Run retention on every open file. Typically called on a
    /// heartbeat tick by the owning runtime.
    pub fn sweep_retention(&self) {
        for entry in self.files.iter() {
            entry.value().sweep_retention();
        }
    }
}

impl Default for Redex {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for Redex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut dbg = f.debug_struct("Redex");
        dbg.field("files", &self.files.len())
            .field("auth", &self.auth.is_some())
            .field("origin_hash", &self.origin_hash);
        #[cfg(feature = "redex-disk")]
        dbg.field("persistent_dir", &self.persistent_dir);
        dbg.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cn(s: &str) -> ChannelName {
        ChannelName::new(s).unwrap()
    }

    #[test]
    fn test_open_and_get() {
        let r = Redex::new();
        let name = cn("sensors/lidar");
        let f = r.open_file(&name, RedexFileConfig::default()).unwrap();
        f.append(b"x").unwrap();

        let g = r.get_file(&name).unwrap();
        assert_eq!(g.len(), 1);
    }

    #[test]
    fn test_reopen_returns_same_file() {
        let r = Redex::new();
        let name = cn("shared");
        let f = r.open_file(&name, RedexFileConfig::default()).unwrap();
        f.append(b"a").unwrap();
        let f2 = r.open_file(&name, RedexFileConfig::default()).unwrap();
        assert_eq!(f2.len(), 1); // sees existing append
        f2.append(b"b").unwrap();
        assert_eq!(f.len(), 2); // original handle also sees it
    }

    #[test]
    fn test_get_file_missing_returns_none() {
        let r = Redex::new();
        assert!(r.get_file(&cn("missing")).is_none());
    }

    #[test]
    fn test_auth_denies_unknown_origin() {
        let guard = Arc::new(AuthGuard::new());
        let r = Redex::with_auth(guard, 0xAAAA_BBBB);
        let name = cn("restricted");
        assert!(matches!(
            r.open_file(&name, RedexFileConfig::default()),
            Err(RedexError::Unauthorized)
        ));
    }

    #[test]
    fn test_auth_allows_authorized_origin() {
        let guard = Arc::new(AuthGuard::new());
        let name = cn("allowed");
        // `allow_channel` populates the exact (control-plane) ACL
        // used by `open_file`, plus the fast-path bloom so packet
        // checks on the same channel also pass.
        guard.allow_channel(0x1234_5678, &name);
        let r = Redex::with_auth(guard, 0x1234_5678);
        assert!(r.open_file(&name, RedexFileConfig::default()).is_ok());
    }

    #[test]
    fn test_auth_fast_path_alone_does_not_authorize_open_file() {
        // Regression: `open_file` used to accept any origin that
        // had the 16-bit `channel_hash` in its fast-path bloom. A
        // different channel name whose 16-bit hash collided with an
        // authorized one would then grant unauthorized storage
        // access. The fix requires the canonical channel name in
        // the exact ACL, so a fast-path-only authorization is
        // insufficient.
        let guard = Arc::new(AuthGuard::new());
        let name = cn("sensitive");
        // Authorize the fast path ONLY (no allow_channel).
        guard.authorize(0x1234_5678, name.hash());
        let r = Redex::with_auth(guard, 0x1234_5678);
        assert!(matches!(
            r.open_file(&name, RedexFileConfig::default()),
            Err(RedexError::Unauthorized)
        ));
    }

    #[test]
    fn test_close_file_rejects_append_on_existing_handle() {
        let r = Redex::new();
        let name = cn("closable");
        let f = r.open_file(&name, RedexFileConfig::default()).unwrap();
        f.append(b"x").unwrap();
        r.close_file(&name).unwrap();
        assert!(f.append(b"y").is_err());
    }

    #[test]
    fn test_sweep_retention_runs_on_all_open_files() {
        let r = Redex::new();
        let cfg = RedexFileConfig::default().with_retention_max_events(1);
        let f1 = r.open_file(&cn("f1"), cfg.clone()).unwrap();
        let f2 = r.open_file(&cn("f2"), cfg).unwrap();
        for i in 0..3 {
            f1.append(format!("{}", i).as_bytes()).unwrap();
            f2.append(format!("{}", i).as_bytes()).unwrap();
        }
        r.sweep_retention();
        assert_eq!(f1.len(), 1);
        assert_eq!(f2.len(), 1);
    }

    #[test]
    fn test_regression_concurrent_first_open_does_not_double_build() {
        // Regression: `open_file` ran `build_file` outside any lock and
        // resolved with `entry().or_insert(file)`. Two threads calling
        // `open_file(name, ...)` for the same brand-new name could both
        // pass the `files.get()` precheck and both run `build_file`.
        // Under `persistent: true` + `FsyncPolicy::Interval`, each
        // build spawned a tokio interval task and opened independent
        // idx/dat handles; the loser of the `or_insert` was dropped
        // without `close()`, so its `Notify` shutdown never fired and
        // the leaked task plus dup file handles outlived the call for
        // the lifetime of the runtime.
        //
        // The fix takes the shard write entry BEFORE running
        // `build_file` so the loser blocks on the shard lock and
        // observes an `Occupied` entry on retry. We don't need a tokio
        // runtime to exercise the race — `build_count` is incremented
        // unconditionally in `build_file`, so any code path that
        // triggers a double-build shows up here.
        let r = Arc::new(Redex::new());
        let name = cn("contended");

        // 32 threads × 1 trial each — release-mode Windows can resolve
        // a 32-way race in microseconds, plenty of opportunity for the
        // buggy path to run `build_file` more than once.
        let threads: Vec<_> = (0..32)
            .map(|_| {
                let r = Arc::clone(&r);
                let name = name.clone();
                std::thread::spawn(move || {
                    r.open_file(&name, RedexFileConfig::default()).unwrap();
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }

        assert_eq!(
            r.build_count(),
            1,
            "concurrent first-open of the same name double-built — \
             each extra build leaks a fsync interval task and a set \
             of file handles under FsyncPolicy::Interval + persistent"
        );
        // And the public surface still resolves to a single file.
        assert!(r.get_file(&name).is_some());
    }
}
