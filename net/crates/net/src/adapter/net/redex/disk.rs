//! Disk-backed durability segment for persistent `RedexFile`s.
//!
//! Feature-gated behind `redex-disk`. When `RedexFileConfig::persistent`
//! is set and the owning [`super::Redex`] manager was given a
//! persistent directory, appends are mirrored to three append-only
//! files per channel:
//!
//! - `<base>/<channel_path>/idx` — 20-byte [`RedexEntry`] records.
//! - `<base>/<channel_path>/dat` — payload bytes (offsets match the
//!   in-memory [`super::segment::HeapSegment`]).
//! - `<base>/<channel_path>/ts` — 8-byte little-endian unix-nanos
//!   timestamps, one per `idx` record. Restores age-based retention
//!   across restart.
//!
//! The heap segment remains authoritative during normal operation; the
//! disk files exist for crash recovery. On reopen the full `dat` file
//! is replayed into the heap, so retention is in-memory-only in v1
//! (the disk files grow unbounded; operators delete old files manually
//! between runs when that matters). v2 will reconcile this.
//!
//! # Durability policy
//!
//! Append-path fsync is governed by [`super::FsyncPolicy`], threaded
//! into the segment at open time as two thresholds:
//!
//! - `fsync_every_n` — notify after every Nth successful append
//!   (`EveryN(N)`).
//! - `fsync_max_bytes` — notify after `max_bytes` of accumulated
//!   writes across `dat` + `idx` + `ts`
//!   (`IntervalOrBytes { max_bytes, .. }`).
//!
//! Each threshold is independent and either may be zero (disabled).
//! Crossing a threshold fires `fsync_signal` (a `tokio::sync::Notify`);
//! the actual fsync runs on a background worker spawned by
//! [`super::RedexFile::open_persistent`], so the appender returns as
//! soon as the bytes land in the page cache. Multiple notifies that
//! arrive while the worker is mid-sync coalesce into a single
//! follow-up sync — `Notify`'s single-permit semantics are exactly
//! what the durability contract wants.
//!
//! `close()` and explicit [`super::RedexFile::sync()`] always run a
//! full synchronous fsync of all three files, regardless of policy —
//! they are the caller's explicit durability barriers.
//!
//! Each of the three files is held twice: once behind the appender's
//! `Mutex<File>` (locked for `write_all`) and once behind a parallel
//! worker `Mutex<File>` (locked only by [`DiskSegment::sync`] and
//! [`DiskSegment::compact_to`]). The two slots are cloned from the
//! same `OpenOptions::new().append(true)` handle via
//! [`std::fs::File::try_clone`], so they share the underlying OS
//! file — `sync_all` on either flushes the same pending writes — but
//! the worker's `sync_all` doesn't contend with the appender's
//! `write_all`. Without this split, a high-cadence policy
//! (`EveryN(1)`, byte-threshold-every-batch) would serialize every
//! appender behind the worker's millisecond-range fsync.
//!
//! # Invariants (do not regress)
//!
//! 1. **No seeks on the hot path.** `idx`, `dat`, `ts` open with
//!    `OpenOptions::new().append(true)`; `set_len` only fires on
//!    rollback (partial-write recovery) or on reopen (torn-tail
//!    truncation). Any new append-time path must preserve this.
//! 2. **Write order is `dat → idx → ts`.** The reopen-time recovery
//!    walk depends on `dat` being durable before `idx`, and `idx`
//!    before `ts`. Reordering is a silent corruption risk.
//! 3. **Lock acquisition order is appender-dat → appender-idx →
//!    appender-ts → worker-dat → worker-idx → worker-ts.** Appender,
//!    worker, and rollback paths only ever hold one file lock at a
//!    time, so the order is incidentally fine today; compaction is
//!    the one path that holds multiple simultaneously and must
//!    follow this order.
//! 4. **Fsync order inside [`DiskSegment::sync`] is `dat → idx → ts`.**
//!    A crash mid-sync can only leave `idx` shorter than `dat` (or
//!    `ts` shorter than `idx`), which the reopen-time truncation
//!    logic already handles. Reversing the order would let the index
//!    reference dat bytes that were never flushed.

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::atomic::AtomicBool;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;
use tokio::sync::Notify;

use super::super::channel::ChannelName;
use super::entry::{RedexEntry, REDEX_ENTRY_SIZE};
use super::error::RedexError;

/// Result of opening a persistent segment. Carries both file handles
/// and the index state recovered from disk.
pub(super) struct RecoveredSegment {
    pub disk: DiskSegment,
    pub index: Vec<RedexEntry>,
    pub payload_bytes: Vec<u8>,
    /// Per-entry timestamps (unix nanos), parallel to `index`.
    /// `None` if the sidecar `ts` file was missing or didn't match
    /// the index length — caller falls back to `now()` for those
    /// entries (with reduced fidelity for age-based retention
    /// across the gap).
    pub timestamps: Option<Vec<u64>>,
}

/// Disk-backed durability segment: append-only idx + dat files,
/// plus a parallel `ts` sidecar carrying per-entry timestamps.
///
/// The `ts` sidecar restores age-based retention across restart:
/// without it, recovered entries got `now()` as their timestamp,
/// so a 1-hour retention on a process restarted every 30 minutes
/// never evicted anything.
pub(super) struct DiskSegment {
    /// Full path to the per-channel directory. Used by the
    /// partial-write rollback paths (which open a fresh write
    /// handle for `set_len` because the cached append-mode
    /// handle can't truncate on every platform).
    dir: PathBuf,
    idx_file: Mutex<File>,
    dat_file: Mutex<File>,
    /// Per-entry timestamps (8 bytes each, little-endian unix
    /// nanos), parallel to `idx`. Same append cadence — every
    /// `append_entry` writes both, and rollback covers both
    /// together so no entry can be on disk without its timestamp.
    ts_file: Mutex<File>,
    /// Worker-only fsync handles. Cloned via [`File::try_clone`]
    /// from the appender handles above; both clones share the
    /// underlying OS file, so `sync_all` on the worker handle
    /// flushes the same pending writes the appender just made.
    /// The point is that the worker doesn't go through the
    /// appender's `Mutex<File>` — at high `EveryN` cadences the
    /// worker would otherwise hold the appender's lock for the
    /// duration of `sync_all` (millisecond-range on Windows
    /// NVMe), stalling every concurrent `write_all`. Only
    /// [`Self::sync`] and [`Self::compact_to`] touch these.
    worker_idx_file: Mutex<File>,
    worker_dat_file: Mutex<File>,
    worker_ts_file: Mutex<File>,
    /// Append-path fsync interval: after `fsync_every_n` successful
    /// appends, the segment notifies the background fsync worker.
    /// `0` disables append-count-based syncing.
    fsync_every_n: u64,
    /// Append-path byte threshold for `IntervalOrBytes`: after
    /// `fsync_max_bytes` bytes have accumulated since the last
    /// successful sync, the segment notifies the worker. `0`
    /// disables byte-based syncing.
    fsync_max_bytes: u64,
    /// Appends since the last append-driven sync (successful or not).
    /// Only meaningful when `fsync_every_n > 0`.
    appends_since_sync: AtomicU64,
    /// Bytes appended since the last successful sync. Bumped from
    /// the append paths, reset to 0 inside `sync()` after the
    /// fsyncs succeed. Only meaningful when `fsync_max_bytes > 0`.
    bytes_since_sync: AtomicU64,
    /// Wake-up signal for the background fsync worker. The
    /// appender notifies this when either threshold (appends or
    /// bytes) is crossed, then returns immediately — the fsync
    /// runs off the appender thread. Always allocated (cheap);
    /// only signaled when at least one threshold is configured
    /// AND a worker is listening (spawned by
    /// `RedexFile::open_persistent`).
    pub(super) fsync_signal: Arc<Notify>,
    /// Segment poisoning. Set to `true` when `compact_to`'s
    /// post-rename re-open phase fails after the renames committed
    /// AND the cached file handles are pointing at temp-dir
    /// placeholders rather than the channel files. Once poisoned,
    /// every append / sync / compact path returns
    /// `RedexError::Io` immediately — preventing acknowledged
    /// writes from landing in `/tmp` instead of the channel
    /// directory. Operators recover by closing and re-opening the
    /// channel (which constructs a fresh `DiskSegment` with valid
    /// handles).
    poisoned: std::sync::atomic::AtomicBool,
    /// Test-only injection: when set, the next `append_entry` /
    /// `append_entries` call returns `RedexError::Io` before touching
    /// either file. Exercises the caller's rollback paths without
    /// needing a real I/O failure (disk full, permission denied).
    #[cfg(test)]
    fail_next_append: AtomicBool,
    /// Test-only injection: when set, the next `append_entry` /
    /// `append_entries` writes the dat payload successfully but
    /// returns `Io` before writing the idx record. Exercises the
    /// dat-rollback path that closes the partial-write stranding
    /// hazard.
    #[cfg(test)]
    fail_after_dat_write: AtomicBool,
    /// Test-only injection: when set, the next `sync()` returns
    /// `RedexError::Io` before touching any file. Used to verify
    /// the EveryN background worker logs and continues rather than
    /// terminating on a sync error.
    #[cfg(test)]
    fail_next_sync: AtomicBool,
    /// Test-only injection: when set, the next `compact_to` returns
    /// `RedexError::Io` before touching any file. Used to exercise
    /// the `sweep_retention` rollback path — verifies
    /// that a failed disk compaction leaves in-memory state
    /// untouched so reopen replays a consistent picture.
    #[cfg(test)]
    fail_next_compact: AtomicBool,
    /// Test-only injection: when set, the next `idx.metadata()`
    /// call inside an append path returns `RedexError::Io`. Used
    /// to exercise the append rollback path — that an idx
    /// metadata failure after a successful dat write rolls the
    /// dat back rather than leaving orphan bytes on disk. The
    /// flag is consumed on the next idx-metadata call (success
    /// or failure clears it).
    #[cfg(test)]
    fail_next_idx_metadata: AtomicBool,
    /// Test-only injection: when set, the next `ts.metadata()`
    /// call inside an append path returns `RedexError::Io`. Used
    /// to exercise the append rollback path — that a ts metadata
    /// failure after successful dat + idx writes rolls both back.
    #[cfg(test)]
    fail_next_ts_metadata: AtomicBool,
    /// Test-only counter: cumulative successful `sync()` calls —
    /// close-time, append-driven (EveryN), or external
    /// (Interval / explicit). Lets policy tests assert the observed
    /// fsync cadence without racing real I/O.
    #[cfg(test)]
    sync_count: AtomicU64,
}

impl DiskSegment {
    /// Open (or create) the idx + dat files for `name` under `base_dir`
    /// and recover the index from disk.
    ///
    /// `fsync_every_n` and `fsync_max_bytes` are derived from
    /// [`super::FsyncPolicy`] at the `RedexFile` layer:
    ///
    /// - `EveryN(n)` → `(n.max(1), 0)`
    /// - `IntervalOrBytes { max_bytes, .. }` → `(0, max_bytes)`
    /// - `Never` / `Interval` → `(0, 0)`
    ///
    /// Each nonzero threshold enables one trigger of the background
    /// fsync worker; `0` disables that trigger.
    pub(super) fn open(
        base_dir: &Path,
        name: &ChannelName,
        fsync_every_n: u64,
        fsync_max_bytes: u64,
    ) -> Result<RecoveredSegment, RedexError> {
        let dir = channel_dir(base_dir, name);
        std::fs::create_dir_all(&dir).map_err(RedexError::io)?;
        let idx_path = dir.join("idx");
        let dat_path = dir.join("dat");

        // Recover existing index.
        let (mut index, idx_len_truncated) = read_index(&idx_path)?;
        let mut payload_bytes = read_payload(&dat_path)?;

        // Torn-idx tail: the last 20-byte write was partial (crash
        // mid-append). Truncate idx to a whole multiple of 20 bytes.
        // The `set_len` MUST be paired with `sync_all`: a crash
        // between truncation and the next durable write would otherwise
        // leave the torn tail on disk and we'd re-recover the same
        // bytes on the next open, indefinitely.
        if idx_len_truncated {
            let file = OpenOptions::new()
                .write(true)
                .open(&idx_path)
                .map_err(RedexError::io)?;
            file.set_len((index.len() * REDEX_ENTRY_SIZE) as u64)
                .map_err(RedexError::io)?;
            file.sync_all().map_err(RedexError::io)?;
        }

        // Torn-dat tail: our write ordering is dat-before-idx, so a
        // crash between the two writes leaves dat shorter than the
        // last idx entry thinks it should be. Separately, external
        // truncation (disk corruption, filesystem bug, admin action)
        // can shrink dat past ANY heap entry, not just the tail.
        //
        // Walk the index backward, skipping inline entries (their
        // payload rides inside the 20-byte idx record and doesn't
        // reference dat). Track the earliest heap entry whose
        // `(offset + len)` runs past the actual dat size — because
        // dat is append-only, heap offsets are monotonic, so if an
        // entry at position `i` is torn then every heap entry at
        // positions `>= i` is either torn or a later append that
        // never got its dat write. Drop everything from that point
        // onward.
        let dat_len = payload_bytes.len() as u64;
        let mut truncate_at: Option<usize> = None;
        for (i, e) in index.iter().enumerate().rev() {
            if e.is_inline() {
                // Inline entries are always valid regardless of dat
                // state. Keep walking back to check earlier heap
                // entries.
                continue;
            }
            let end = (e.payload_offset as u64).saturating_add(e.payload_len as u64);
            if end > dat_len {
                // Torn. Everything from here to the end of the index
                // must go. Record this position and keep walking —
                // an even earlier heap entry might also be torn
                // (external truncation scenarios).
                truncate_at = Some(i);
            } else {
                // First heap entry that fits. By dat's append-only
                // monotonicity, every earlier heap entry also fits.
                break;
            }
        }
        let idx_trimmed = truncate_at.is_some();
        if let Some(cut) = truncate_at {
            index.truncate(cut);
        }
        if idx_trimmed {
            let file = OpenOptions::new()
                .write(true)
                .open(&idx_path)
                .map_err(RedexError::io)?;
            file.set_len((index.len() * REDEX_ENTRY_SIZE) as u64)
                .map_err(RedexError::io)?;
            // sync_all so a crash before the next durable write
            // doesn't reincarnate the torn idx entries.
            file.sync_all().map_err(RedexError::io)?;
        }
        // Trim any trailing dat bytes that no idx entry references.
        // Finds the highest `(offset + len)` among retained heap
        // entries and truncates dat to that.
        let retained_dat_end = index
            .iter()
            .filter(|e| !e.is_inline())
            .map(|e| (e.payload_offset as u64).saturating_add(e.payload_len as u64))
            .max()
            .unwrap_or(0);
        if retained_dat_end < dat_len {
            let file = OpenOptions::new()
                .write(true)
                .open(&dat_path)
                .map_err(RedexError::io)?;
            file.set_len(retained_dat_end).map_err(RedexError::io)?;
            // sync_all so the dat-trim survives a crash that lands
            // before the next durable write — otherwise reopening
            // would observe the un-trimmed dat past `retained_dat_end`
            // and re-trigger the recovery walk.
            file.sync_all().map_err(RedexError::io)?;
            payload_bytes.truncate(retained_dat_end as usize);
        }

        // Read ts sidecar BEFORE the checksum filter so we have a
        // timestamp for every original index position. The checksum
        // filter below can drop entries from anywhere in the file
        // (mid-file bit-rot, not just the tail); pairing surviving
        // entries with the **first N** timestamps after the filter
        // would misalign every surviving entry that follows a dropped
        // one.
        let ts_path = dir.join("ts");
        let original_timestamps = read_timestamps(&ts_path, index.len())?;

        // Verify per-entry checksums during recovery. Without
        // this check, on-disk corruption (torn writes, bit-rot,
        // FS bug, or external tampering) is silently accepted
        // and becomes part of the recovered state. Drop entries
        // whose checksum doesn't match. Inline entries are
        // self-contained 8-byte payloads carried inside the
        // index record; we still verify them in case the index
        // file itself was corrupted.
        //
        // Use a manual loop (not `retain`) so we can track which
        // original indices survived. The survivor indices are then
        // used to pick matching timestamps from `original_timestamps`
        // — without this, dropping mid-file entries would shift the
        // ts↔index pairing.
        let mut survivors: Vec<usize> = Vec::with_capacity(index.len());
        for (i, e) in index.iter().enumerate() {
            let payload: &[u8] = if e.is_inline() {
                let Some(inline) = e.inline_payload() else {
                    continue;
                };
                let computed = super::entry::payload_checksum(&inline);
                if e.checksum() != computed {
                    continue;
                }
                survivors.push(i);
                continue;
            } else {
                let off = e.payload_offset as usize;
                let len = e.payload_len as usize;
                let end = off.saturating_add(len);
                if end > payload_bytes.len() {
                    // Should be impossible after the truncation
                    // pass above, but stay defensive.
                    continue;
                }
                &payload_bytes[off..end]
            };
            let computed = super::entry::payload_checksum(payload);
            if e.checksum() == computed {
                survivors.push(i);
            }
        }
        let bad_entries = index.len() - survivors.len();
        if bad_entries > 0 {
            // Compact `index` to surviving entries. The corresponding
            // ts compaction below picks `original_timestamps[i]` for
            // each `i in survivors`.
            let mut compacted = Vec::with_capacity(survivors.len());
            for &i in &survivors {
                compacted.push(index[i]);
            }
            index = compacted;
            tracing::error!(
                bad_entries,
                surviving = index.len(),
                "DiskSegment::open: dropped {} entries with bad checksums during recovery; \
                 on-disk dat may have torn writes or be corrupt",
                bad_entries
            );
        }

        // Compact timestamps to match the surviving index. If the
        // ts sidecar was missing/corrupt, propagate `None` so the
        // file.rs layer falls back to `now()`.
        let timestamps = original_timestamps.as_ref().map(|all_ts| {
            survivors
                .iter()
                .map(|&i| all_ts.get(i).copied().unwrap_or(0))
                .collect::<Vec<u64>>()
        });

        // If we dropped mid-file entries, the on-disk ts file no
        // longer matches the surviving index byte-for-byte. Rewrite
        // it with the compacted timestamps so the next reopen sees a
        // file of length `surviving * 8` whose i-th entry pairs with
        // `index[i]`. When no entries were dropped, just truncate
        // the trailing slack from the tail-truncation step.
        if let Some(ts) = timestamps.as_ref() {
            if bad_entries > 0 {
                if let Ok(mut file) = OpenOptions::new().write(true).truncate(true).open(&ts_path) {
                    use std::io::Write as _;
                    let mut buf = Vec::with_capacity(ts.len() * 8);
                    for &t in ts {
                        buf.extend_from_slice(&t.to_le_bytes());
                    }
                    let _ = file.write_all(&buf);
                }
            } else if !index.is_empty() {
                // No mid-file drops; just align length with idx.
                if let Ok(file) = OpenOptions::new().write(true).open(&ts_path) {
                    let _ = file.set_len((index.len() * 8) as u64);
                }
            }
        }

        let idx_file = OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(&idx_path)
            .map_err(RedexError::io)?;
        let dat_file = OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(&dat_path)
            .map_err(RedexError::io)?;

        let ts_file = OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(&ts_path)
            .map_err(RedexError::io)?;

        // Worker handles share the underlying OS file with the
        // appender handles via `try_clone`, but live behind their
        // own mutexes so the worker's `sync_all` never contends
        // with the appender's `write_all`.
        let worker_idx_file = idx_file.try_clone().map_err(RedexError::io)?;
        let worker_dat_file = dat_file.try_clone().map_err(RedexError::io)?;
        let worker_ts_file = ts_file.try_clone().map_err(RedexError::io)?;

        Ok(RecoveredSegment {
            disk: DiskSegment {
                dir,
                idx_file: Mutex::new(idx_file),
                dat_file: Mutex::new(dat_file),
                ts_file: Mutex::new(ts_file),
                worker_idx_file: Mutex::new(worker_idx_file),
                worker_dat_file: Mutex::new(worker_dat_file),
                worker_ts_file: Mutex::new(worker_ts_file),
                fsync_every_n,
                fsync_max_bytes,
                appends_since_sync: AtomicU64::new(0),
                bytes_since_sync: AtomicU64::new(0),
                fsync_signal: Arc::new(Notify::new()),
                poisoned: std::sync::atomic::AtomicBool::new(false),
                #[cfg(test)]
                fail_next_append: AtomicBool::new(false),
                #[cfg(test)]
                fail_after_dat_write: AtomicBool::new(false),
                #[cfg(test)]
                fail_next_sync: AtomicBool::new(false),
                #[cfg(test)]
                fail_next_compact: AtomicBool::new(false),
                #[cfg(test)]
                fail_next_idx_metadata: AtomicBool::new(false),
                #[cfg(test)]
                fail_next_ts_metadata: AtomicBool::new(false),
                #[cfg(test)]
                sync_count: AtomicU64::new(0),
            },
            index,
            payload_bytes,
            timestamps,
        })
    }

    /// Test-only: cumulative successful `sync()` count.
    #[cfg(test)]
    pub(super) fn sync_count(&self) -> u64 {
        self.sync_count.load(Ordering::Acquire)
    }

    /// Test-only: lengths reported by the three worker-side file
    /// handles. After a successful append, these must match the
    /// on-disk file lengths — that's the proof the worker handles
    /// share the same OS file as the appender handles
    /// (`try_clone` correctness) and, after `compact_to`, that the
    /// worker handles were re-cloned from the new appender handles
    /// rather than left pointing at the temp-dir placeholder.
    #[cfg(test)]
    pub(super) fn worker_file_lens(&self) -> (u64, u64, u64) {
        let dat = self
            .worker_dat_file
            .lock()
            .metadata()
            .map(|m| m.len())
            .unwrap_or(0);
        let idx = self
            .worker_idx_file
            .lock()
            .metadata()
            .map(|m| m.len())
            .unwrap_or(0);
        let ts = self
            .worker_ts_file
            .lock()
            .metadata()
            .map(|m| m.len())
            .unwrap_or(0);
        (dat, idx, ts)
    }

    /// Bump per-append and per-byte counters; if either crosses its
    /// configured threshold, notify the background fsync worker.
    /// The actual fsync runs off the appender thread; this returns
    /// immediately after the page-cache write so the appender's
    /// caller doesn't block on disk durability.
    ///
    /// Each counter resets independently when its threshold fires
    /// so the cadence stays periodic. Explicit `sync()` / `close()`
    /// are unaffected and continue to fsync synchronously,
    /// surfacing any error.
    fn maybe_sync_after_append(&self, applied: u64, bytes_written: u64) {
        let mut should_signal = false;
        if self.fsync_every_n > 0 && applied > 0 {
            let prev = self.appends_since_sync.fetch_add(applied, Ordering::AcqRel);
            let now = prev.saturating_add(applied);
            if now >= self.fsync_every_n {
                self.appends_since_sync.store(0, Ordering::Release);
                should_signal = true;
            }
        }
        if self.fsync_max_bytes > 0 && bytes_written > 0 {
            let prev = self
                .bytes_since_sync
                .fetch_add(bytes_written, Ordering::AcqRel);
            let now = prev.saturating_add(bytes_written);
            if now >= self.fsync_max_bytes {
                // Reset before notify so concurrent appenders don't
                // double-fire on already-counted bytes. The worker's
                // sync() also stores 0; the redundant store here
                // closes the appender-side race.
                self.bytes_since_sync.store(0, Ordering::Release);
                should_signal = true;
            }
        }
        if should_signal {
            // notify_one stores a permit if no worker is parked yet
            // — covers the open→first-poll window. If multiple
            // notifies arrive while the worker is mid-sync, they
            // coalesce into a single follow-up sync, which is the
            // intended semantics.
            self.fsync_signal.notify_one();
        }
    }

    /// Roll back one of the three append files to a recorded length.
    /// If the open or `set_len` fails, log AND poison the segment:
    /// the on-disk state is now inconsistent and any subsequent
    /// append would compound the divergence rather than narrow it.
    ///
    /// Pre-fix the rollback used `if let Ok(f) = ...` and silently
    /// continued on open failure, leaving the segment in a
    /// permanently-divergent state with no diagnostic.
    fn rollback_truncate(&self, file_name: &str, target_len: u64) {
        let path = self.dir.join(file_name);
        match OpenOptions::new().write(true).open(&path) {
            Ok(f) => {
                if let Err(e) = f.set_len(target_len) {
                    tracing::error!(
                        error = %e,
                        path = %path.display(),
                        "redex disk rollback: {file_name} set_len failed; poisoning segment",
                    );
                    self.poisoned.store(true, Ordering::Release);
                }
            }
            Err(e) => {
                tracing::error!(
                    error = %e,
                    path = %path.display(),
                    "redex disk rollback: {file_name} open failed; poisoning segment",
                );
                self.poisoned.store(true, Ordering::Release);
            }
        }
    }

    /// Roll back a partial single-entry append after the idx (or
    /// ts-metadata read) failed. The dat rollback uses
    /// (current_len - payload_len) as the target since the
    /// single-entry path doesn't snapshot the pre-write dat length.
    fn rollback_after_idx_failure(&self, pre_idx_len: u64, dat_rollback: Option<u64>) {
        self.rollback_truncate("idx", pre_idx_len);
        if let Some(payload_len) = dat_rollback {
            let dat_path = self.dir.join("dat");
            let dat_target = match OpenOptions::new().read(true).open(&dat_path) {
                Ok(f) => match f.metadata() {
                    Ok(m) => Some(m.len().saturating_sub(payload_len)),
                    Err(e) => {
                        tracing::error!(
                            error = %e,
                            path = %dat_path.display(),
                            "redex disk rollback: dat metadata failed; poisoning segment",
                        );
                        self.poisoned.store(true, Ordering::Release);
                        None
                    }
                },
                Err(e) => {
                    tracing::error!(
                        error = %e,
                        path = %dat_path.display(),
                        "redex disk rollback: dat open(read) failed; poisoning segment",
                    );
                    self.poisoned.store(true, Ordering::Release);
                    None
                }
            };
            if let Some(target) = dat_target {
                self.rollback_truncate("dat", target);
            }
        }
    }

    /// Roll back a partial single-entry append after the ts write
    /// failed: idx + ts + (optionally) dat all need to be trimmed
    /// back. Same poison-on-failure semantics as
    /// `rollback_after_idx_failure`.
    fn rollback_after_ts_failure(
        &self,
        pre_idx_len: u64,
        pre_ts_len: u64,
        dat_rollback: Option<u64>,
    ) {
        self.rollback_truncate("ts", pre_ts_len);
        self.rollback_after_idx_failure(pre_idx_len, dat_rollback);
    }

    /// Test-only: arm a one-shot failure on the next
    /// `append_entry` / `append_entries` call. Returns `Io` before
    /// touching either file. Used to exercise the caller's rollback
    /// paths without needing a real I/O failure.
    #[cfg(test)]
    pub(super) fn arm_next_append_failure(&self) {
        self.fail_next_append.store(true, Ordering::Release);
    }

    /// Append an entry with an explicit timestamp (unix nanos).
    /// Inline entries skip the dat write — their payload rides in
    /// the 20-byte idx record. The timestamp is persisted to the
    /// `ts` sidecar so age-based retention survives restart.
    ///
    /// Writes go through the OS page cache. After a successful
    /// append, [`Self::maybe_sync_after_append`] bumps the per-append
    /// and per-byte counters and signals the background fsync worker
    /// if either threshold (`fsync_every_n` / `fsync_max_bytes`) is
    /// crossed — the actual fsync runs off the appender thread, so
    /// this call returns as soon as the bytes land in the page cache.
    /// Explicit [`Self::sync`] and `close()` still fsync
    /// synchronously regardless of policy.
    #[allow(dead_code)]
    pub(super) fn append_entry_at(
        &self,
        entry: &RedexEntry,
        payload: &[u8],
        timestamp_ns: u64,
    ) -> Result<(), RedexError> {
        self.append_entry_inner(entry, payload, timestamp_ns)
    }

    #[allow(dead_code)]
    pub(super) fn append_entry(
        &self,
        entry: &RedexEntry,
        payload: &[u8],
    ) -> Result<(), RedexError> {
        // Default timestamp: now. Production callers should use
        // `append_entry_at` so the in-memory and on-disk
        // timestamps stay in sync; this overload exists for
        // legacy / test paths.
        self.append_entry_inner(entry, payload, now_ns_disk())
    }

    fn append_entry_inner(
        &self,
        entry: &RedexEntry,
        payload: &[u8],
        timestamp_ns: u64,
    ) -> Result<(), RedexError> {
        // Refuse writes against a poisoned segment. `compact_to`
        // sets this flag when its post-rename re-open phase fails
        // — the cached file handles are pointing at temp-dir
        // placeholders and any append would land in `/tmp`
        // instead of the channel directory.
        if self.poisoned.load(std::sync::atomic::Ordering::Acquire) {
            return Err(RedexError::Io(
                "redex segment is poisoned (compact_to post-rename reopen failure); \
                 close and re-open the channel to recover"
                    .into(),
            ));
        }
        #[cfg(test)]
        if self.fail_next_append.swap(false, Ordering::AcqRel) {
            return Err(RedexError::Io("test-injected append failure".into()));
        }
        // Partial-write rollback. If `write_all` fails halfway
        // through a multi-page payload, some bytes have already
        // committed to disk; without truncating back, the next
        // append lands AFTER those stranded bytes but its idx
        // record points at the pre-failure offset — every read
        // of that entry returns `partial_old ++ truncated_new`.
        // Recovery's tail-only inspection misses the gap. We
        // capture the pre-write length and `set_len` back on
        // error so the dat file always matches what the index
        // describes.
        if !entry.is_inline() {
            let mut dat = self.dat_file.lock();
            let pre_len = dat.metadata().map_err(RedexError::io)?.len();
            if let Err(e) = dat.write_all(payload) {
                // Best-effort rollback: truncate dat back to its
                // pre-write length so partial bytes don't strand
                // on disk. `.append(true)` open mode prevents
                // `set_len` on the same handle on some platforms,
                // so use a fresh write handle. If even that fails
                // (filesystem error), surface the original error.
                drop(dat);
                let dat_path = self.dir.join("dat");
                if let Ok(f) = OpenOptions::new().write(true).open(&dat_path) {
                    let _ = f.set_len(pre_len);
                }
                return Err(RedexError::io(e));
            }
        }
        // Test-only injection: dat write succeeded, now bail
        // before touching idx. Exercises the dat-rollback path.
        #[cfg(test)]
        if self.fail_after_dat_write.swap(false, Ordering::AcqRel) {
            if !entry.is_inline() {
                let dat_path = self.dir.join("dat");
                if let Ok(f) = OpenOptions::new().write(true).open(&dat_path) {
                    let cur = f.metadata().map(|m| m.len()).unwrap_or(0);
                    let _ = f.set_len(cur.saturating_sub(payload.len() as u64));
                }
            }
            return Err(RedexError::Io(
                "test-injected post-dat-pre-idx failure".into(),
            ));
        }
        let mut idx = self.idx_file.lock();
        // `idx.metadata()?` can fail (file handle invalidated
        // after compact_to placeholder swap, fs error, etc.) AFTER
        // the dat write at line 607 has already committed bytes
        // to disk. A `?` early-return would skip the rollback
        // block below, leaving orphan dat bytes on disk while the
        // caller is told the append failed. Wrap explicitly so
        // the dat rollback runs on this path too.
        #[cfg(test)]
        let idx_metadata = if self.fail_next_idx_metadata.swap(false, Ordering::AcqRel) {
            Err(std::io::Error::other("test-injected idx.metadata failure"))
        } else {
            idx.metadata()
        };
        #[cfg(not(test))]
        let idx_metadata = idx.metadata();
        let pre_idx_len = match idx_metadata {
            Ok(m) => m.len(),
            Err(e) => {
                drop(idx);
                if !entry.is_inline() {
                    let dat_path = self.dir.join("dat");
                    if let Ok(f) = OpenOptions::new().write(true).open(&dat_path) {
                        let cur = f.metadata().map(|m| m.len()).unwrap_or(0);
                        let _ = f.set_len(cur.saturating_sub(payload.len() as u64));
                    }
                }
                return Err(RedexError::io(e));
            }
        };
        if let Err(e) = idx.write_all(&entry.to_bytes()) {
            drop(idx);
            let dat_rollback = if entry.is_inline() {
                None
            } else {
                Some(payload.len() as u64)
            };
            self.rollback_after_idx_failure(pre_idx_len, dat_rollback);
            return Err(RedexError::io(e));
        }
        drop(idx);

        // Append the timestamp to the ts sidecar. If it fails we
        // roll back idx (and dat) so the on-disk index never
        // reaches an entry without a matching timestamp.
        let mut ts = self.ts_file.lock();
        // `ts.metadata()?` can fail AFTER both dat AND idx have
        // committed bytes to disk. A `?` early-return would skip
        // the rollback block below, leaving the on-disk idx with
        // a record whose ts entry never landed (so on reopen
        // `read_timestamps` returns None for the length mismatch
        // and every recovered entry gets `now()` as its
        // timestamp, breaking age-based retention). Wrap
        // explicitly so the idx + dat rollback runs on this path
        // too.
        #[cfg(test)]
        let ts_metadata = if self.fail_next_ts_metadata.swap(false, Ordering::AcqRel) {
            Err(std::io::Error::other("test-injected ts.metadata failure"))
        } else {
            ts.metadata()
        };
        #[cfg(not(test))]
        let ts_metadata = ts.metadata();
        let pre_ts_len = match ts_metadata {
            Ok(m) => m.len(),
            Err(e) => {
                drop(ts);
                let dat_rollback = if entry.is_inline() {
                    None
                } else {
                    Some(payload.len() as u64)
                };
                self.rollback_after_idx_failure(pre_idx_len, dat_rollback);
                return Err(RedexError::io(e));
            }
        };
        if let Err(e) = ts.write_all(&timestamp_ns.to_le_bytes()) {
            drop(ts);
            let dat_rollback = if entry.is_inline() {
                None
            } else {
                Some(payload.len() as u64)
            };
            self.rollback_after_ts_failure(pre_idx_len, pre_ts_len, dat_rollback);
            return Err(RedexError::io(e));
        }
        drop(ts);

        // Bytes written across all three files this call. ts is 8
        // bytes, idx is REDEX_ENTRY_SIZE, dat is payload.len() for
        // heap entries (inline entries skip dat).
        let dat_bytes = if entry.is_inline() {
            0
        } else {
            payload.len() as u64
        };
        let total_bytes = dat_bytes + REDEX_ENTRY_SIZE as u64 + 8;
        self.maybe_sync_after_append(1, total_bytes);
        Ok(())
    }

    /// Test-only: arm a one-shot post-dat / pre-idx failure on the
    /// next `append_entry` call. Used to exercise the dat-rollback
    /// path that closes the partial-write stranding hazard.
    #[cfg(test)]
    pub(super) fn arm_next_post_dat_failure(&self) {
        self.fail_after_dat_write.store(true, Ordering::Release);
    }

    /// Append several entries and their payloads atomically (per-file:
    /// each file's buffered write is contiguous). Inline entries only
    /// touch the idx file.
    ///
    /// Same fsync semantics as [`Self::append_entry`]: a batch of N
    /// counts as N applied appends against the `EveryN` cadence and
    /// `dat_buf.len() + idx_buf.len() + ts_buf.len()` bytes against
    /// the byte threshold. At most one notify fires per batch.
    #[allow(dead_code)]
    pub(super) fn append_entries(
        &self,
        entries_and_payloads: &[(RedexEntry, &[u8])],
    ) -> Result<(), RedexError> {
        // Default timestamp: now, applied to every entry. Production
        // callers should use `append_entries_at` so the in-memory
        // and on-disk timestamps stay in sync.
        let now = now_ns_disk();
        let timestamps: Vec<u64> = vec![now; entries_and_payloads.len()];
        self.append_entries_inner(entries_and_payloads, &timestamps)
    }

    /// Append a batch with explicit per-entry timestamps. The
    /// timestamps slice must have the same length as
    /// `entries_and_payloads`.
    #[allow(dead_code)]
    pub(super) fn append_entries_at(
        &self,
        entries_and_payloads: &[(RedexEntry, &[u8])],
        timestamps: &[u64],
    ) -> Result<(), RedexError> {
        if timestamps.len() != entries_and_payloads.len() {
            return Err(RedexError::Io(format!(
                "append_entries_at: timestamps len ({}) != entries len ({})",
                timestamps.len(),
                entries_and_payloads.len()
            )));
        }
        self.append_entries_inner(entries_and_payloads, timestamps)
    }

    fn append_entries_inner(
        &self,
        entries_and_payloads: &[(RedexEntry, &[u8])],
        timestamps: &[u64],
    ) -> Result<(), RedexError> {
        // Refuse writes against a poisoned segment. See
        // `append_entry_inner` for the full rationale.
        if self.poisoned.load(std::sync::atomic::Ordering::Acquire) {
            return Err(RedexError::Io(
                "redex segment is poisoned (compact_to post-rename reopen failure); \
                 close and re-open the channel to recover"
                    .into(),
            ));
        }
        #[cfg(test)]
        if self.fail_next_append.swap(false, Ordering::AcqRel) {
            return Err(RedexError::Io("test-injected append failure".into()));
        }

        // Build one contiguous buffer per file in a single pass, then
        // issue one `write_all` per file. A batch of N entries used to
        // emit 3·N syscalls (one per entry per file); now it emits at
        // most 3. Write order — dat → idx → ts — is preserved so
        // recovery's torn-tail logic still holds: a crash mid-batch
        // can only leave dat ahead of idx (or idx ahead of ts), never
        // the reverse.
        let total_dat: usize = entries_and_payloads
            .iter()
            .filter(|(e, _)| !e.is_inline())
            .map(|(_, p)| p.len())
            .sum();
        let mut dat_buf: Vec<u8> = Vec::with_capacity(total_dat);
        let mut idx_buf: Vec<u8> =
            Vec::with_capacity(entries_and_payloads.len() * REDEX_ENTRY_SIZE);
        let mut ts_buf: Vec<u8> = Vec::with_capacity(timestamps.len() * 8);
        for ((entry, payload), &t) in entries_and_payloads.iter().zip(timestamps) {
            if !entry.is_inline() {
                dat_buf.extend_from_slice(payload);
            }
            idx_buf.extend_from_slice(&entry.to_bytes());
            ts_buf.extend_from_slice(&t.to_le_bytes());
        }

        // Skip dat entirely when every entry is inline. Track
        // `dat_pre_len` only when we actually wrote, so the idx/ts
        // rollback paths know whether a dat truncation is needed.
        let dat_pre_len: Option<u64> = if !dat_buf.is_empty() {
            let mut dat = self.dat_file.lock();
            let pre_len = dat.metadata().map_err(RedexError::io)?.len();
            if let Err(e) = dat.write_all(&dat_buf) {
                drop(dat);
                self.rollback_truncate("dat", pre_len);
                return Err(RedexError::io(e));
            }
            drop(dat);
            Some(pre_len)
        } else {
            None
        };

        let mut idx = self.idx_file.lock();
        // Same hazard as `append_entry_inner` — `metadata()?` can
        // fail after the dat write at line 787 has committed
        // bytes. Wrap explicitly so the dat rollback runs.
        #[cfg(test)]
        let idx_metadata = if self.fail_next_idx_metadata.swap(false, Ordering::AcqRel) {
            Err(std::io::Error::other("test-injected idx.metadata failure"))
        } else {
            idx.metadata()
        };
        #[cfg(not(test))]
        let idx_metadata = idx.metadata();
        let idx_pre_len = match idx_metadata {
            Ok(m) => m.len(),
            Err(e) => {
                drop(idx);
                if let Some(pre_len) = dat_pre_len {
                    self.rollback_truncate("dat", pre_len);
                }
                return Err(RedexError::io(e));
            }
        };
        if let Err(e) = idx.write_all(&idx_buf) {
            drop(idx);
            self.rollback_truncate("idx", idx_pre_len);
            if let Some(pre_len) = dat_pre_len {
                self.rollback_truncate("dat", pre_len);
            }
            return Err(RedexError::io(e));
        }
        drop(idx);

        let mut ts = self.ts_file.lock();
        // `metadata()?` can fail after dat AND idx have both
        // committed bytes. Wrap explicitly so the full rollback
        // runs on this path too — without it the on-disk idx
        // would end up with records whose ts never landed,
        // breaking `read_timestamps` alignment and age-based
        // retention.
        #[cfg(test)]
        let ts_metadata = if self.fail_next_ts_metadata.swap(false, Ordering::AcqRel) {
            Err(std::io::Error::other("test-injected ts.metadata failure"))
        } else {
            ts.metadata()
        };
        #[cfg(not(test))]
        let ts_metadata = ts.metadata();
        let ts_pre_len = match ts_metadata {
            Ok(m) => m.len(),
            Err(e) => {
                drop(ts);
                self.rollback_truncate("idx", idx_pre_len);
                if let Some(pre_len) = dat_pre_len {
                    self.rollback_truncate("dat", pre_len);
                }
                return Err(RedexError::io(e));
            }
        };
        if let Err(e) = ts.write_all(&ts_buf) {
            drop(ts);
            self.rollback_truncate("ts", ts_pre_len);
            self.rollback_truncate("idx", idx_pre_len);
            if let Some(pre_len) = dat_pre_len {
                self.rollback_truncate("dat", pre_len);
            }
            return Err(RedexError::io(e));
        }
        drop(ts);

        let total_bytes = (dat_buf.len() + idx_buf.len() + ts_buf.len()) as u64;
        self.maybe_sync_after_append(entries_and_payloads.len() as u64, total_bytes);
        Ok(())
    }

    /// Flush all three files to durable storage. Order matters for
    /// crash consistency: the payload (`dat`) must be durable before
    /// the index entry (`idx`) that references it, and `idx` before
    /// `ts`. A crash between syncs in the wrong order could leave
    /// an index entry pointing at bytes that were never flushed —
    /// on recovery the index would reference torn payload data.
    /// With dat-first the worst case is an index that's one or more
    /// entries shorter than the dat, which the torn-tail truncation
    /// logic on reopen already handles correctly.
    ///
    /// Goes through the *worker* handles, not the appender handles.
    /// Both share the same OS file (via `try_clone` at open time),
    /// so `sync_all` here flushes the same pending writes the
    /// appender just made — but acquiring a lock the appender
    /// doesn't touch means the appender's `write_all` doesn't stall
    /// for the duration of the fsync. This is the fix that lets
    /// `EveryN(1)` keep up with the appender instead of serializing
    /// behind it.
    pub(super) fn sync(&self) -> Result<(), RedexError> {
        #[cfg(test)]
        if self.fail_next_sync.swap(false, Ordering::AcqRel) {
            return Err(RedexError::Io("test-injected sync failure".into()));
        }
        self.worker_dat_file
            .lock()
            .sync_all()
            .map_err(RedexError::io)?;
        self.worker_idx_file
            .lock()
            .sync_all()
            .map_err(RedexError::io)?;
        // The ts sidecar is fsynced last — same dat-before-idx
        // logic: a crash that loses the ts tail just means the
        // recovered timestamps for the trailing N entries fall
        // back to `now()`. Losing the idx tail without the dat
        // would be worse, so dat-first.
        self.worker_ts_file
            .lock()
            .sync_all()
            .map_err(RedexError::io)?;
        // All bytes accumulated up to this point are now durable.
        // Reset the byte counter so `IntervalOrBytes` measures from
        // here forward; the timer-driven sync path relies on this
        // (it doesn't cross a threshold itself, so without the
        // reset, byte-trigger would over-fire on subsequent appends).
        self.bytes_since_sync.store(0, Ordering::Release);
        #[cfg(test)]
        self.sync_count.fetch_add(1, Ordering::AcqRel);
        Ok(())
    }

    /// Test-only: arm a one-shot failure on the next `sync()` call.
    /// Used to verify the EveryN background worker logs and continues
    /// rather than terminating on a sync error. The flag is consumed
    /// on the next `sync()` (success or failure clears it).
    #[cfg(test)]
    pub(super) fn arm_next_sync_failure(&self) {
        self.fail_next_sync.store(true, Ordering::Release);
    }

    /// Test-only: arm a one-shot failure on the next `compact_to`
    /// call. Used to verify the `sweep_retention` rollback path
    /// — that a failed disk compaction leaves in-memory
    /// state untouched. The flag is consumed on the next
    /// `compact_to` (success or failure clears it).
    #[cfg(test)]
    pub(super) fn arm_next_compact_failure(&self) {
        self.fail_next_compact.store(true, Ordering::Release);
    }

    /// Test-only: arm a one-shot failure on the next idx
    /// `metadata()` call inside an append path. Used to exercise
    /// the append rollback path — verifies that a metadata error
    /// after a successful dat write rolls the dat back instead of
    /// leaving orphan bytes on disk.
    #[cfg(test)]
    pub(super) fn arm_next_idx_metadata_failure(&self) {
        self.fail_next_idx_metadata.store(true, Ordering::Release);
    }

    /// Test-only: arm a one-shot failure on the next ts
    /// `metadata()` call inside an append path. Used to exercise
    /// the append rollback path — verifies that a metadata error
    /// after successful dat + idx writes rolls both back.
    #[cfg(test)]
    pub(super) fn arm_next_ts_metadata_failure(&self) {
        self.fail_next_ts_metadata.store(true, Ordering::Release);
    }

    /// Compact the on-disk segment to the surviving in-memory
    /// state. Called by `sweep_retention` so age / size eviction
    /// reflects on disk too — without this, the idx + dat files
    /// grow unbounded across restarts and re-resurrect previously
    /// evicted entries on the next reopen.
    ///
    /// `surviving_index` is the post-sweep in-memory index;
    /// `surviving_timestamps` is its parallel ts vector;
    /// `dat_base` is the absolute offset of the first heap entry's
    /// payload in the *original* dat file (i.e., the byte-position
    /// at which the new dat must start).
    pub(super) fn compact_to(
        &self,
        surviving_index: &[RedexEntry],
        surviving_timestamps: &[u64],
        dat_base: u64,
    ) -> Result<(), RedexError> {
        // Refuse compact against a poisoned segment. A poisoned
        // segment's cached handles point at temp-dir placeholders;
        // running another compact would compound the
        // off-channel-directory hazard.
        if self.poisoned.load(std::sync::atomic::Ordering::Acquire) {
            return Err(RedexError::Io(
                "redex segment is poisoned; refusing compact_to".into(),
            ));
        }
        #[cfg(test)]
        if self.fail_next_compact.swap(false, Ordering::AcqRel) {
            return Err(RedexError::Io("test-injected compact_to failure".into()));
        }
        if surviving_index.len() != surviving_timestamps.len() {
            return Err(RedexError::Io(format!(
                "compact_to: index/timestamp length mismatch ({} vs {})",
                surviving_index.len(),
                surviving_timestamps.len()
            )));
        }

        // Atomic-rewrite pattern: write each file into `*.tmp`
        // alongside the original, fsync, then rename over. A
        // crash before the rename leaves the original intact;
        // after the rename, the new content is durable.
        let idx_path = self.dir.join("idx");
        let dat_path = self.dir.join("dat");
        let ts_path = self.dir.join("ts");
        let idx_tmp = self.dir.join("idx.tmp");
        let dat_tmp = self.dir.join("dat.tmp");
        let ts_tmp = self.dir.join("ts.tmp");

        // Drop the cached append handles before the rename so
        // the original files aren't held open. We'll re-open
        // them at the end of this method.
        //
        // Acquire in (appender) dat → idx → ts → (worker) dat → idx
        // → ts order to match the global lock discipline documented
        // in the module rustdoc. This is the only path that holds
        // multiple file locks simultaneously; the appender and the
        // worker each only hold one at a time, so the order is
        // incidentally safe today, but a future change that
        // overlaps locks on either path could deadlock if compaction
        // held them in any other order.
        //
        // Worker handles also point at the destination paths (they
        // were `try_clone`d at open time), so they pin the OS files
        // the same way the appender handles do — both must be
        // swapped to placeholders before the rename can succeed
        // on Windows.
        let mut dat_guard = self.dat_file.lock();
        let mut idx_guard = self.idx_file.lock();
        let mut ts_guard = self.ts_file.lock();
        let mut worker_dat_guard = self.worker_dat_file.lock();
        let mut worker_idx_guard = self.worker_idx_file.lock();
        let mut worker_ts_guard = self.worker_ts_file.lock();

        // Build the new idx, ts contents in-memory; the dat tail
        // we rewrite by reading the surviving range from the old
        // dat file.
        let mut new_idx_bytes = Vec::with_capacity(surviving_index.len() * REDEX_ENTRY_SIZE);
        for entry in surviving_index {
            // Rewrite the heap offsets so the surviving entries
            // index from 0 in the compacted dat.
            if entry.is_inline() {
                new_idx_bytes.extend_from_slice(&entry.to_bytes());
            } else {
                // Heap entry offsets are absolute in the original
                // dat. Rebase to the new dat by subtracting
                // `dat_base`. By invariant, every surviving heap
                // entry has `payload_offset >= dat_base` (dat_base
                // is derived from the first surviving heap entry's
                // offset), so a `< dat_base` value indicates the
                // caller violated the invariant — surface as an
                // error rather than silently writing 0 (which
                // would point the entry at unrelated bytes in the
                // new dat).
                let abs = entry.payload_offset as u64;
                let rebased = abs.checked_sub(dat_base).ok_or_else(|| {
                    RedexError::Encode(format!(
                        "compact_to: heap entry with payload_offset={} \
                         below dat_base={} would corrupt the new dat \
                         layout",
                        abs, dat_base
                    ))
                })?;
                let mut e = *entry;
                e.payload_offset = rebased as u32;
                new_idx_bytes.extend_from_slice(&e.to_bytes());
            }
        }
        let mut new_ts_bytes = Vec::with_capacity(surviving_timestamps.len() * 8);
        for &t in surviving_timestamps {
            new_ts_bytes.extend_from_slice(&t.to_le_bytes());
        }
        // Read the surviving dat tail.
        let old_dat = read_payload(&dat_path)?;
        let new_dat = if dat_base as usize >= old_dat.len() {
            Vec::new()
        } else {
            old_dat[dat_base as usize..].to_vec()
        };

        // Write tmp files.
        {
            let mut f = OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&idx_tmp)
                .map_err(RedexError::io)?;
            f.write_all(&new_idx_bytes).map_err(RedexError::io)?;
            f.sync_all().map_err(RedexError::io)?;
        }
        {
            let mut f = OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&dat_tmp)
                .map_err(RedexError::io)?;
            f.write_all(&new_dat).map_err(RedexError::io)?;
            f.sync_all().map_err(RedexError::io)?;
        }
        {
            let mut f = OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&ts_tmp)
                .map_err(RedexError::io)?;
            f.write_all(&new_ts_bytes).map_err(RedexError::io)?;
            f.sync_all().map_err(RedexError::io)?;
        }

        // Drop the old open append handles before rename. On
        // Windows, an open handle to the destination prevents
        // rename; POSIX is more permissive but the consistency is
        // valuable.
        //
        // We need each `File` slot to hold a valid `File` value
        // (it's not `Option<File>`), so swap in a throwaway file
        // outside the channel directory. Using `std::env::temp_dir`
        // keeps the channel dir clean — the OS reclaims the
        // placeholder if a crash mid-compaction prevents our
        // explicit `remove_file` below from running. A unique
        // suffix (process id + nanos) prevents two concurrent
        // compactions across different channels from colliding on
        // the same placeholder name.
        let placeholder_suffix = format!(
            "{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        let placeholder_idx =
            std::env::temp_dir().join(format!("redex-compact-idx-{}", placeholder_suffix));
        let placeholder_dat =
            std::env::temp_dir().join(format!("redex-compact-dat-{}", placeholder_suffix));
        let placeholder_ts =
            std::env::temp_dir().join(format!("redex-compact-ts-{}", placeholder_suffix));
        // RAII cleanup of the three placeholder files. Pre-fix the
        // happy-path removal at the bottom of `compact_to` only ran
        // when every post-rename reopen succeeded; any `?` early
        // return on `open_or_poison` / `clone_or_poison` left the
        // placeholders behind in `/tmp` forever, growing without
        // bound on every reopen-failure event.
        struct PlaceholderCleanup<'a> {
            paths: [&'a Path; 3],
        }
        impl Drop for PlaceholderCleanup<'_> {
            fn drop(&mut self) {
                for path in self.paths {
                    let _ = std::fs::remove_file(path);
                }
            }
        }
        let _placeholder_cleanup = PlaceholderCleanup {
            paths: [
                placeholder_idx.as_path(),
                placeholder_dat.as_path(),
                placeholder_ts.as_path(),
            ],
        };
        let null_idx = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&placeholder_idx)
            .map_err(RedexError::io)?;
        let null_dat = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&placeholder_dat)
            .map_err(RedexError::io)?;
        let null_ts = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&placeholder_ts)
            .map_err(RedexError::io)?;
        // Clone the placeholders for the worker slots — cheaper
        // than opening three more temp files, and works for our
        // purposes since the placeholder is just "any valid File
        // value to park in the slot until we re-open."
        let null_idx_worker = null_idx.try_clone().map_err(RedexError::io)?;
        let null_dat_worker = null_dat.try_clone().map_err(RedexError::io)?;
        let null_ts_worker = null_ts.try_clone().map_err(RedexError::io)?;
        *idx_guard = null_idx;
        *dat_guard = null_dat;
        *ts_guard = null_ts;
        *worker_idx_guard = null_idx_worker;
        *worker_dat_guard = null_dat_worker;
        *worker_ts_guard = null_ts_worker;

        // Atomic renames.
        //
        // This is a *three-rename* sequence rather than one
        // atomic flip — a crash between renames N and N+1 leaves
        // a mixed-version on-disk state that recovery cannot
        // distinguish from a clean half-finished compact. A full
        // fix is a manifest-pointer scheme (write versioned
        // filenames, atomically swap a single "manifest" pointer),
        // which is a format change deferred. The interim
        // mitigation is the parent-dir fsync below: on POSIX,
        // individual renames are not durable until the dirent is
        // fsynced, so without that fsync a power loss could leave
        // the directory pointing at the OLD inodes even after all
        // three rename calls returned successfully. The fsync
        // narrows but does not close the cross-file gap.
        std::fs::rename(&idx_tmp, &idx_path).map_err(RedexError::io)?;
        std::fs::rename(&dat_tmp, &dat_path).map_err(RedexError::io)?;
        std::fs::rename(&ts_tmp, &ts_path).map_err(RedexError::io)?;
        fsync_dir(&self.dir).map_err(RedexError::io)?;

        // Open all six new handles (3 base + 3 worker clones) into
        // local variables first, with bounded retry on transient
        // failures (ENFILE / EMFILE / antivirus interference /
        // brief permission flap). If ANY of them fails after
        // retries, set `poisoned = true` so the segment refuses
        // all further write paths until the channel is re-opened.
        //
        // A `?` early-return here would leave the cached guards
        // pointing at the temp-dir placeholders even though the
        // disk state is already post-compact (the renames have
        // committed) — any subsequent `append_entry_inner` call
        // would then write into `/tmp` rather than the channel
        // dir. The in-memory state preservation contract still
        // holds, but the next append would still hit the
        // placeholders, so the `poisoned` flag gates that: append
        // paths consult it and refuse to write rather than
        // dropping events into temp-dir placeholders. This trades
        // a noisy hard-error for silent write-to-`/tmp`.
        let open_or_poison = |path: &Path| -> Result<File, RedexError> {
            reopen_with_retries(path).map_err(|e| {
                self.poisoned
                    .store(true, std::sync::atomic::Ordering::Release);
                tracing::error!(
                    error = %e,
                    path = %path.display(),
                    "redex compact_to: post-rename reopen FAILED — segment \
                     poisoned to prevent writes from landing in temp-dir \
                     placeholders. Channel must be re-opened to recover."
                );
                RedexError::io(e)
            })
        };
        let new_idx = open_or_poison(&idx_path)?;
        let new_dat = open_or_poison(&dat_path)?;
        let new_ts = open_or_poison(&ts_path)?;

        // Per-file durability flush. On POSIX `fsync_dir`
        // above already covered the dir-level rename durability;
        // on Windows it's a no-op (stdlib doesn't expose the
        // dir-flush API). Calling `sync_all` on each renamed
        // file's freshly-opened handle here ensures FILE CONTENT
        // is durable on every target, even when dir-level
        // atomicity is best-effort. Best-effort on the sync_all
        // itself: a failure here does NOT roll back the rename
        // (already committed), so we log and continue rather than
        // surfacing a fail-the-compact error after the disk state
        // has flipped.
        if let Err(e) = new_idx.sync_all() {
            tracing::warn!(error = %e, "post-compact sync_all on idx failed (best-effort)");
        }
        if let Err(e) = new_dat.sync_all() {
            tracing::warn!(error = %e, "post-compact sync_all on dat failed (best-effort)");
        }
        if let Err(e) = new_ts.sync_all() {
            tracing::warn!(error = %e, "post-compact sync_all on ts failed (best-effort)");
        }

        // Clone failures also poison (same hazard — the cached
        // guard slots aren't yet swapped, but a `?` early-return
        // here would leave the slots at temp-dir placeholders).
        let clone_or_poison = |f: &File, kind: &str| -> Result<File, RedexError> {
            f.try_clone().map_err(|e| {
                self.poisoned
                    .store(true, std::sync::atomic::Ordering::Release);
                tracing::error!(
                    error = %e,
                    kind = kind,
                    "redex compact_to: post-rename try_clone FAILED — segment poisoned"
                );
                RedexError::io(e)
            })
        };
        let new_idx_worker = clone_or_poison(&new_idx, "idx")?;
        let new_dat_worker = clone_or_poison(&new_dat, "dat")?;
        let new_ts_worker = clone_or_poison(&new_ts, "ts")?;

        // All six handles open successfully — atomically slot them.
        *idx_guard = new_idx;
        *dat_guard = new_dat;
        *ts_guard = new_ts;
        *worker_idx_guard = new_idx_worker;
        *worker_dat_guard = new_dat_worker;
        *worker_ts_guard = new_ts_worker;

        // Placeholder cleanup is handled by `_placeholder_cleanup`'s
        // Drop above — runs whether we reach this success path OR
        // bail via an earlier `?` on a post-rename reopen failure.

        Ok(())
    }
}

/// Reopen a redex file with bounded retry on transient
/// failures (ENFILE, EMFILE, brief antivirus locking, sharing
/// violations). All redex files are opened with the same flag
/// shape — `create+read+append` — so this helper is the single
/// canonical re-open primitive for the post-compact path.
///
/// Retry schedule: 5 attempts at 0ms, 25ms, 50ms, 100ms, 200ms
/// (total <= 375ms). This window covers all the realistic
/// transient causes without blocking the sweep path for
/// noticeably long. If all five attempts fail, the most recent
/// error is surfaced — the caller (only `compact_to` today) wraps
/// it in `RedexError::io`.
fn reopen_with_retries(path: &Path) -> std::io::Result<File> {
    const ATTEMPTS: u32 = 5;
    let mut last_err: Option<std::io::Error> = None;
    for attempt in 0..ATTEMPTS {
        if attempt > 0 {
            // 25ms, 50ms, 100ms, 200ms.
            let delay = std::time::Duration::from_millis(25u64 << (attempt - 1));
            std::thread::sleep(delay);
        }
        match OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(path)
        {
            Ok(f) => return Ok(f),
            Err(e) => last_err = Some(e),
        }
    }
    Err(last_err.expect("at least one attempt must have run"))
}

#[inline]
#[allow(dead_code)]
fn now_ns_disk() -> u64 {
    // Fallback timestamp source for the legacy `append_entry` /
    // `append_entries` overloads (no caller in production after
    // the file.rs migration to `append_entry_at`); kept so test
    // code that constructs DiskSegments directly without going
    // through the file.rs layer still works.
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

fn channel_dir(base_dir: &Path, name: &ChannelName) -> PathBuf {
    let mut p = base_dir.to_path_buf();
    for seg in name.as_str().split('/') {
        p.push(seg);
    }
    p
}

/// Fsync a directory inode so any prior `rename()` calls into
/// it become durable.
///
/// On POSIX, `rename` only updates the dirent in memory — the
/// directory's on-disk metadata isn't flushed until the dir
/// itself is fsynced. Without this, a power-loss between a
/// successful `rename` syscall return and a subsequent fsync
/// can leave the directory pointing at the OLD inodes, making
/// the rename's apparent atomicity a lie. Closes part of BUG
/// #93 — the `compact_to` rename sequence now fsyncs the
/// containing directory after the renames complete.
///
/// On Windows, rename durability is governed by separate APIs
/// (`MoveFileEx` with `MOVEFILE_WRITE_THROUGH`) and the
/// stdlib's `std::fs::rename` does not expose them.
/// Opening a directory as a `File` and calling `sync_all` is
/// not a defined operation on Windows; we no-op here. Power-
/// loss durability of `compact_to` on Windows is therefore
/// best-effort under the current implementation.
#[cfg(unix)]
fn fsync_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::File::open(dir)?.sync_all()
}

/// On non-Unix targets the `fsync_dir` helper is a no-op. Stdlib
/// does not expose the Windows equivalent (`MoveFileExW` with
/// `MOVEFILE_WRITE_THROUGH`, or `FlushFileBuffers` on a directory
/// handle), so a power-loss between successful renames can leave
/// the directory pointing at the OLD inodes even after every
/// individual file has been flushed.
///
/// We log loudly ONCE per process so operators see the durability
/// gap rather than silently relying on an empty-Ok return that
/// looks indistinguishable from a successful POSIX fsync. The
/// `compact_to` caller still benefits from per-file `sync_all`
/// calls earlier in the sequence, so file CONTENT is durable —
/// only the directory-level atomicity of the three-rename
/// sequence is best-effort.
#[cfg(not(unix))]
fn fsync_dir(_dir: &Path) -> std::io::Result<()> {
    use std::sync::atomic::{AtomicBool, Ordering};
    static WARNED: AtomicBool = AtomicBool::new(false);
    if !WARNED.swap(true, Ordering::AcqRel) {
        tracing::warn!(
            os = std::env::consts::OS,
            "redex fsync_dir is a NO-OP on this platform — directory-level \
             rename atomicity is best-effort. Per-file content remains durable \
             via the explicit sync_all calls in compact_to, but the cross-file \
             rename sequence is not transactional."
        );
    }
    Ok(())
}

/// Read the full idx file into a `Vec<RedexEntry>`.
///
/// Returns `(entries, truncated)` where `truncated` is true if the
/// tail of the file was a partial record (torn write from a crash).
/// Callers should `set_len` the file to `entries.len() * 20` if so.
fn read_index(path: &Path) -> Result<(Vec<RedexEntry>, bool), RedexError> {
    if !path.exists() {
        return Ok((Vec::new(), false));
    }
    let mut f = File::open(path).map_err(RedexError::io)?;
    let mut bytes = Vec::new();
    f.read_to_end(&mut bytes).map_err(RedexError::io)?;
    let full_records = bytes.len() / REDEX_ENTRY_SIZE;
    let truncated = bytes.len() % REDEX_ENTRY_SIZE != 0;
    let mut entries = Vec::with_capacity(full_records);
    for i in 0..full_records {
        let start = i * REDEX_ENTRY_SIZE;
        let chunk: [u8; REDEX_ENTRY_SIZE] = bytes[start..start + REDEX_ENTRY_SIZE]
            .try_into()
            .expect("20-byte chunk");
        entries.push(RedexEntry::from_bytes(&chunk));
    }
    Ok((entries, truncated))
}

/// Read the ts sidecar (8 bytes per entry, little-endian unix
/// nanos). Returns `Some(timestamps)` only when the file exists
/// AND has exactly `expected_entries * 8` bytes; any mismatch
/// (missing file, partial last record, length disagreement with
/// the index) returns `None` so the caller can fall back to
/// `now()` and surface a warning.
fn read_timestamps(path: &Path, expected_entries: usize) -> Result<Option<Vec<u64>>, RedexError> {
    if !path.exists() {
        return Ok(None);
    }
    let mut f = File::open(path).map_err(RedexError::io)?;
    let mut bytes = Vec::new();
    f.read_to_end(&mut bytes).map_err(RedexError::io)?;
    if bytes.len() < expected_entries * 8 {
        // Index has more entries than ts has timestamps — the
        // sidecar is partial / lagged. Reject and fall back.
        return Ok(None);
    }
    // The ts file may have MORE entries than the index — e.g.
    // after a torn-tail truncation of idx during recovery. Take
    // only the first `expected_entries` timestamps.
    let mut out = Vec::with_capacity(expected_entries);
    for i in 0..expected_entries {
        let chunk: [u8; 8] = bytes[i * 8..i * 8 + 8].try_into().expect("8 bytes");
        out.push(u64::from_le_bytes(chunk));
    }
    Ok(Some(out))
}

/// Read the full dat file into a byte vector.
fn read_payload(path: &Path) -> Result<Vec<u8>, RedexError> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let mut f = File::open(path).map_err(RedexError::io)?;
    let mut bytes = Vec::new();
    f.read_to_end(&mut bytes).map_err(RedexError::io)?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::super::entry::payload_checksum;
    use super::*;

    fn tmpdir() -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "redex_disk_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn cleanup(p: &Path) {
        let _ = std::fs::remove_dir_all(p);
    }

    #[test]
    fn test_disk_append_and_recover() {
        let base = tmpdir();
        let name = ChannelName::new("t/disk1").unwrap();

        {
            let recovered = DiskSegment::open(&base, &name, 0, 0).unwrap();
            assert!(recovered.index.is_empty());
            assert!(recovered.payload_bytes.is_empty());

            // Simulate writing two heap entries.
            let p1 = b"alpha";
            let e1 = RedexEntry::new_heap(0, 0, p1.len() as u32, 0, payload_checksum(p1));
            recovered.disk.append_entry(&e1, p1).unwrap();

            let p2 = b"beta";
            let e2 = RedexEntry::new_heap(1, 5, p2.len() as u32, 0, payload_checksum(p2));
            recovered.disk.append_entry(&e2, p2).unwrap();

            recovered.disk.sync().unwrap();
        }

        // Reopen; recover both entries and their payload.
        let recovered = DiskSegment::open(&base, &name, 0, 0).unwrap();
        assert_eq!(recovered.index.len(), 2);
        assert_eq!(recovered.index[0].seq, 0);
        assert_eq!(recovered.index[1].seq, 1);
        assert_eq!(&recovered.payload_bytes[..5], b"alpha");
        assert_eq!(&recovered.payload_bytes[5..9], b"beta");

        cleanup(&base);
    }

    #[test]
    fn test_disk_inline_entries_skip_dat_file() {
        let base = tmpdir();
        let name = ChannelName::new("t/inline").unwrap();

        let recovered = DiskSegment::open(&base, &name, 0, 0).unwrap();
        let payload = *b"abcdefgh";
        let entry = RedexEntry::new_inline(0, &payload, payload_checksum(&payload));
        recovered.disk.append_entry(&entry, &payload).unwrap();
        recovered.disk.sync().unwrap();
        drop(recovered);

        let recovered = DiskSegment::open(&base, &name, 0, 0).unwrap();
        assert_eq!(recovered.index.len(), 1);
        assert!(recovered.index[0].is_inline());
        // Dat file should be empty — inline payload lives in idx.
        assert!(recovered.payload_bytes.is_empty());

        cleanup(&base);
    }

    #[test]
    fn test_torn_idx_tail_is_truncated_on_reopen() {
        let base = tmpdir();
        let name = ChannelName::new("t/torn").unwrap();

        // Write one good entry.
        {
            let recovered = DiskSegment::open(&base, &name, 0, 0).unwrap();
            let p = b"ok";
            let e = RedexEntry::new_heap(0, 0, p.len() as u32, 0, payload_checksum(p));
            recovered.disk.append_entry(&e, p).unwrap();
            recovered.disk.sync().unwrap();
        }

        // Manually append 7 garbage bytes to simulate a torn write.
        let idx_path = channel_dir(&base, &name).join("idx");
        let mut f = OpenOptions::new().append(true).open(&idx_path).unwrap();
        f.write_all(&[0xFF; 7]).unwrap();
        f.sync_all().unwrap();
        drop(f);

        // Reopen: partial tail must be truncated; one entry recovered.
        let recovered = DiskSegment::open(&base, &name, 0, 0).unwrap();
        assert_eq!(recovered.index.len(), 1);
        assert_eq!(recovered.index[0].seq, 0);

        // Verify the file was actually truncated back to 20 bytes.
        let after_len = std::fs::metadata(&idx_path).unwrap().len();
        assert_eq!(after_len, 20);

        cleanup(&base);
    }

    #[test]
    fn test_channel_dir_handles_nested_names() {
        let base = PathBuf::from("/tmp/base");
        let name = ChannelName::new("sensors/lidar/front").unwrap();
        let dir = channel_dir(&base, &name);
        assert_eq!(dir, PathBuf::from("/tmp/base/sensors/lidar/front"));
    }

    /// Externally truncating the dat file (admin action, FS bug,
    /// crash mid-rename) past a heap entry's tail must drop only
    /// the torn entries — every preceding heap entry AND every
    /// inline entry between them must survive. The recovery walk
    /// at lines 127-160 documents this scenario; this test pins
    /// the documented behavior so a refactor can't quietly trim
    /// too much (data loss) or too little (stale offset → garbage
    /// reads). Lay-out under test:
    ///
    ///     idx: [heap1, inline1, heap2, inline2, heap3]
    ///     dat: [<h1 bytes>, <h2 bytes>, <h3 bytes>]
    ///
    /// Truncate dat to keep h1 and h2 but kill h3. After reopen,
    /// the surviving index must be `[heap1, inline1, heap2,
    /// inline2]`.
    #[test]
    fn test_external_dat_truncation_drops_torn_heap_after_inlines() {
        let base = tmpdir();
        let name = ChannelName::new("t/external-trunc").unwrap();

        // Inline payloads must be exactly INLINE_PAYLOAD_SIZE bytes.
        let inline_a = *b"in_a____";
        let inline_b = *b"in_b____";
        let h1_payload = b"heap1";
        let h2_payload = b"heap2_longer";
        let h3_payload = b"heap3_data";

        let h1_off = 0u32;
        let h2_off = h1_off + h1_payload.len() as u32;
        let h3_off = h2_off + h2_payload.len() as u32;
        let dat_keep_len = (h2_off + h2_payload.len() as u32) as u64;

        // Phase 1 — write the layout.
        {
            let recovered = DiskSegment::open(&base, &name, 0, 0).unwrap();

            recovered
                .disk
                .append_entry(
                    &RedexEntry::new_heap(
                        0,
                        h1_off,
                        h1_payload.len() as u32,
                        0,
                        payload_checksum(h1_payload),
                    ),
                    h1_payload,
                )
                .unwrap();

            recovered
                .disk
                .append_entry(
                    &RedexEntry::new_inline(1, &inline_a, payload_checksum(&inline_a)),
                    &inline_a,
                )
                .unwrap();

            recovered
                .disk
                .append_entry(
                    &RedexEntry::new_heap(
                        2,
                        h2_off,
                        h2_payload.len() as u32,
                        0,
                        payload_checksum(h2_payload),
                    ),
                    h2_payload,
                )
                .unwrap();

            recovered
                .disk
                .append_entry(
                    &RedexEntry::new_inline(3, &inline_b, payload_checksum(&inline_b)),
                    &inline_b,
                )
                .unwrap();

            recovered
                .disk
                .append_entry(
                    &RedexEntry::new_heap(
                        4,
                        h3_off,
                        h3_payload.len() as u32,
                        0,
                        payload_checksum(h3_payload),
                    ),
                    h3_payload,
                )
                .unwrap();

            recovered.disk.sync().unwrap();
        }

        // Phase 2 — externally truncate dat to kill heap3 only.
        let dat_path = channel_dir(&base, &name).join("dat");
        OpenOptions::new()
            .write(true)
            .open(&dat_path)
            .unwrap()
            .set_len(dat_keep_len)
            .unwrap();

        // Phase 3 — reopen and assert.
        let recovered = DiskSegment::open(&base, &name, 0, 0).unwrap();
        let seqs: Vec<u64> = recovered.index.iter().map(|e| e.seq).collect();
        assert_eq!(
            seqs,
            vec![0, 1, 2, 3],
            "expected heap1,inline1,heap2,inline2 to survive (got seqs {:?})",
            seqs
        );

        // Both inlines must still report inline.
        assert!(!recovered.index[0].is_inline(), "seq 0 should be heap");
        assert!(recovered.index[1].is_inline(), "seq 1 should be inline");
        assert!(!recovered.index[2].is_inline(), "seq 2 should be heap");
        assert!(recovered.index[3].is_inline(), "seq 3 should be inline");

        // Dat must have been re-trimmed to exactly the surviving
        // heap entries' end. (Lines 161-177 do a final sweep that
        // truncates dat to the highest retained `(offset + len)`.)
        assert_eq!(
            recovered.payload_bytes.len() as u64,
            dat_keep_len,
            "dat should be exactly heap1+heap2 bytes after recovery"
        );
        assert_eq!(
            std::fs::metadata(&dat_path).unwrap().len(),
            dat_keep_len,
            "dat file size mismatch after recovery"
        );
        // Index must have been re-trimmed to drop heap3's record
        // (one 20-byte slot removed from the tail).
        let idx_path = channel_dir(&base, &name).join("idx");
        assert_eq!(
            std::fs::metadata(&idx_path).unwrap().len(),
            (4 * REDEX_ENTRY_SIZE) as u64,
            "idx file should have exactly 4 records after recovery"
        );

        cleanup(&base);
    }

    /// Truncating dat all the way back to BEFORE heap2 must drop
    /// heap2 and heap3, but keep heap1 and any inlines between
    /// them. Layout:
    ///
    ///     idx: [heap1, inline1, heap2, inline2, heap3]
    ///     dat truncated to: <h1 bytes>
    ///
    /// Surviving index: `[heap1, inline1]`. heap2's tail is
    /// torn → it and everything after it must be dropped. inline2
    /// sits *after* heap2 in the index, so even though it does
    /// not depend on dat, it is dropped by the backward-walk
    /// because heap2's tear marks position 2 as truncated and
    /// later positions are torn-or-later.
    #[test]
    fn test_external_dat_truncation_to_first_heap_drops_everything_after() {
        let base = tmpdir();
        let name = ChannelName::new("t/external-trunc-deep").unwrap();

        let inline_a = *b"in_a____";
        let inline_b = *b"in_b____";
        let h1_payload = b"heap1";
        let h2_payload = b"heap2";
        let h3_payload = b"heap3";

        {
            let recovered = DiskSegment::open(&base, &name, 0, 0).unwrap();

            recovered
                .disk
                .append_entry(
                    &RedexEntry::new_heap(0, 0, 5, 0, payload_checksum(h1_payload)),
                    h1_payload,
                )
                .unwrap();
            recovered
                .disk
                .append_entry(
                    &RedexEntry::new_inline(1, &inline_a, payload_checksum(&inline_a)),
                    &inline_a,
                )
                .unwrap();
            recovered
                .disk
                .append_entry(
                    &RedexEntry::new_heap(2, 5, 5, 0, payload_checksum(h2_payload)),
                    h2_payload,
                )
                .unwrap();
            recovered
                .disk
                .append_entry(
                    &RedexEntry::new_inline(3, &inline_b, payload_checksum(&inline_b)),
                    &inline_b,
                )
                .unwrap();
            recovered
                .disk
                .append_entry(
                    &RedexEntry::new_heap(4, 10, 5, 0, payload_checksum(h3_payload)),
                    h3_payload,
                )
                .unwrap();
            recovered.disk.sync().unwrap();
        }

        // Truncate dat to keep heap1 only.
        let dat_path = channel_dir(&base, &name).join("dat");
        OpenOptions::new()
            .write(true)
            .open(&dat_path)
            .unwrap()
            .set_len(5)
            .unwrap();

        let recovered = DiskSegment::open(&base, &name, 0, 0).unwrap();
        let seqs: Vec<u64> = recovered.index.iter().map(|e| e.seq).collect();
        assert_eq!(
            seqs,
            vec![0, 1],
            "deep dat truncation must keep only entries up to (but not past) the earliest torn heap (got seqs {:?})",
            seqs
        );
        assert!(!recovered.index[0].is_inline());
        assert!(recovered.index[1].is_inline());

        // Dat file should be re-trimmed to heap1 only.
        assert_eq!(
            std::fs::metadata(&dat_path).unwrap().len(),
            5,
            "dat should remain exactly heap1's bytes after recovery"
        );

        cleanup(&base);
    }

    /// Regression: a partial dat write used to leave stranded
    /// payload bytes on disk while the index never recorded the
    /// entry. The next successful append landed AFTER the
    /// stranded bytes, but its idx record pointed at the original
    /// pre-failure offset — every read of that entry returned
    /// `partial_old ++ truncated_new`. Recovery's tail-only
    /// inspection missed the gap.
    ///
    /// We simulate a mid-write failure by using the
    /// post-dat / pre-idx test injection: the dat bytes are
    /// written, then the call bails before idx. The rollback
    /// must truncate dat back to its pre-write length so a
    /// subsequent successful append doesn't strand the failed
    /// payload's bytes.
    #[test]
    fn append_failure_after_dat_write_rolls_back_dat() {
        let base = tmpdir();
        let name = ChannelName::new("t/rollback").unwrap();
        let dat_path = channel_dir(&base, &name).join("dat");

        let recovered = DiskSegment::open(&base, &name, 0, 0).unwrap();

        // Successful first append establishes a known starting
        // point.
        let p1 = b"good-payload-A";
        let e1 = RedexEntry::new_heap(0, 0, p1.len() as u32, 0, payload_checksum(p1));
        recovered.disk.append_entry(&e1, p1).unwrap();
        recovered.disk.sync().unwrap();
        let dat_len_after_a = std::fs::metadata(&dat_path).unwrap().len();
        assert_eq!(dat_len_after_a as usize, p1.len());

        // Arm the post-dat / pre-idx injection.
        recovered.disk.arm_next_post_dat_failure();

        // This call writes dat bytes, then bails. The rollback
        // must truncate dat back to `dat_len_after_a`.
        let p2 = b"would-strand-these-bytes";
        let e2 = RedexEntry::new_heap(1, p1.len() as u32, p2.len() as u32, 0, payload_checksum(p2));
        let result = recovered.disk.append_entry(&e2, p2);
        assert!(result.is_err(), "injected failure must surface as Err");

        // Crucial invariant: dat is back to pre-write length.
        // Without rollback, dat would now be `p1.len() +
        // p2.len()` and the next append would land past p2's
        // stranded bytes.
        let dat_len_after_failure = std::fs::metadata(&dat_path).unwrap().len();
        assert_eq!(
            dat_len_after_failure, dat_len_after_a,
            "dat must be rolled back to its pre-failure length; \
             stranded bytes here would corrupt later reads"
        );

        // Successful retry of the same logical append. The dat
        // should now contain p1 + p2 exactly, with no stranded
        // bytes from the failed attempt.
        recovered.disk.append_entry(&e2, p2).unwrap();
        recovered.disk.sync().unwrap();

        let final_dat_len = std::fs::metadata(&dat_path).unwrap().len();
        assert_eq!(
            final_dat_len as usize,
            p1.len() + p2.len(),
            "after successful retry, dat must contain exactly p1 + p2"
        );

        // And recovery sees the right entries.
        drop(recovered);
        let recovered = DiskSegment::open(&base, &name, 0, 0).unwrap();
        assert_eq!(recovered.index.len(), 2);
        assert_eq!(&recovered.payload_bytes[..p1.len()], p1);
        assert_eq!(&recovered.payload_bytes[p1.len()..p1.len() + p2.len()], p2);

        cleanup(&base);
    }

    /// Regression: when the checksum filter drops a *mid-file*
    /// entry, the surviving timestamps must be picked by original
    /// index position — not by sequential prefix. Previously we
    /// read `read_timestamps(.., index.len())` AFTER the filter,
    /// which returned `[ts0, ts1, …, ts_{N-bad-1}]` from the start
    /// of the ts file, misaligning every surviving entry that
    /// followed a dropped one.
    #[test]
    fn checksum_filter_preserves_ts_pairing_on_mid_file_drop() {
        let base = tmpdir();
        let name = ChannelName::new("t/ts_pair").unwrap();
        let dat_path = channel_dir(&base, &name).join("dat");

        // Append four entries with distinct, recognizable timestamps.
        // Use the explicit-timestamp variant so we can pin which
        // ts pairs with which idx record.
        let recovered = DiskSegment::open(&base, &name, 0, 0).unwrap();
        let payloads: [&[u8]; 4] = [b"AAAAAAAAA", b"BBBBBBBBB", b"CCCCCCCCC", b"DDDDDDDDD"];
        let timestamps: [u64; 4] = [1000, 2000, 3000, 4000];
        let mut offset = 0u32;
        for (i, p) in payloads.iter().enumerate() {
            let e = RedexEntry::new_heap(i as u64, offset, p.len() as u32, 0, payload_checksum(p));
            recovered
                .disk
                .append_entry_at(&e, p, timestamps[i])
                .unwrap();
            offset += p.len() as u32;
        }
        recovered.disk.sync().unwrap();
        drop(recovered);

        // Corrupt the SECOND payload's bytes on disk so its
        // checksum no longer verifies. We mutate `dat` in place;
        // the idx record still claims the original checksum.
        {
            let mut bytes = std::fs::read(&dat_path).unwrap();
            bytes[payloads[0].len()] ^= 0xFF; // flip a byte inside p2
            std::fs::write(&dat_path, &bytes).unwrap();
        }

        // Reopen — recovery's checksum filter must drop entry 1
        // (B) and pair the surviving entries with their original
        // timestamps (1000, 3000, 4000), NOT (1000, 2000, 3000).
        let recovered = DiskSegment::open(&base, &name, 0, 0).unwrap();
        assert_eq!(recovered.index.len(), 3, "one corrupt entry must drop");
        let surviving_seqs: Vec<u64> = recovered.index.iter().map(|e| e.seq).collect();
        assert_eq!(surviving_seqs, vec![0, 2, 3], "B should be dropped");

        let ts = recovered.timestamps.expect("ts sidecar present");
        assert_eq!(
            ts,
            vec![1000, 3000, 4000],
            "surviving timestamps must come from the original index \
             positions of the surviving entries; pre-fix this would \
             have been [1000, 2000, 3000]"
        );

        cleanup(&base);
    }

    /// Mixed heap + inline batch round-trip through the buffered
    /// `append_entries_at` path. The disk-layer batch API stitches
    /// `dat_buf` from heap payloads only while idx/ts cover every
    /// entry; this test pins three things at once:
    ///
    ///   1. inline payloads do NOT leak into dat,
    ///   2. heap-payload bytes land at the offsets their idx records
    ///      claim,
    ///   3. each per-entry timestamp pairs with the matching idx
    ///      record on reopen.
    ///
    /// A zip-mismatch or wrong-buffer regression would surface as
    /// either a checksum failure on reopen or a misaligned timestamp.
    #[test]
    fn test_disk_batch_mixed_heap_and_inline_roundtrip() {
        let base = tmpdir();
        let name = ChannelName::new("t/batch_mixed").unwrap();

        let h1 = b"heap-one";
        let inline_a = *b"inline_A"; // 8 bytes
        let h2 = b"heap-two-longer";
        let inline_b = *b"inline_B";

        let h1_off = 0u32;
        let h2_off = h1_off + h1.len() as u32;

        let entries = [
            (
                RedexEntry::new_heap(10, h1_off, h1.len() as u32, 0, payload_checksum(h1)),
                h1.as_slice(),
            ),
            (
                RedexEntry::new_inline(11, &inline_a, payload_checksum(&inline_a)),
                inline_a.as_slice(),
            ),
            (
                RedexEntry::new_heap(12, h2_off, h2.len() as u32, 0, payload_checksum(h2)),
                h2.as_slice(),
            ),
            (
                RedexEntry::new_inline(13, &inline_b, payload_checksum(&inline_b)),
                inline_b.as_slice(),
            ),
        ];
        let timestamps = [11_000u64, 22_000, 33_000, 44_000];

        {
            let recovered = DiskSegment::open(&base, &name, 0, 0).unwrap();
            recovered
                .disk
                .append_entries_at(&entries, &timestamps)
                .unwrap();
            recovered.disk.sync().unwrap();
        }

        // dat must contain exactly h1 || h2 — inline bytes must NOT
        // have been appended to the dat buffer during stitching.
        let dat_path = channel_dir(&base, &name).join("dat");
        let dat_bytes = std::fs::read(&dat_path).unwrap();
        assert_eq!(dat_bytes.len(), h1.len() + h2.len());
        assert_eq!(&dat_bytes[..h1.len()], h1);
        assert_eq!(&dat_bytes[h1.len()..], h2);

        // Reopen: all four entries survive with their correct seqs,
        // inline flags, and timestamps.
        let recovered = DiskSegment::open(&base, &name, 0, 0).unwrap();
        let seqs: Vec<u64> = recovered.index.iter().map(|e| e.seq).collect();
        assert_eq!(seqs, vec![10, 11, 12, 13]);
        assert!(!recovered.index[0].is_inline());
        assert!(recovered.index[1].is_inline());
        assert!(!recovered.index[2].is_inline());
        assert!(recovered.index[3].is_inline());
        let ts = recovered.timestamps.expect("ts sidecar present");
        assert_eq!(ts, vec![11_000, 22_000, 33_000, 44_000]);

        cleanup(&base);
    }

    /// All-inline batch must skip the dat lock entirely (the new
    /// `dat_pre_len: Option<u64>` branch). dat file stays at zero
    /// bytes; idx and ts both receive their batched writes.
    #[test]
    fn test_disk_batch_all_inline_skips_dat() {
        let base = tmpdir();
        let name = ChannelName::new("t/batch_all_inline").unwrap();

        let p1 = *b"alpha___";
        let p2 = *b"beta____";
        let p3 = *b"gamma___";
        let entries = [
            (
                RedexEntry::new_inline(0, &p1, payload_checksum(&p1)),
                p1.as_slice(),
            ),
            (
                RedexEntry::new_inline(1, &p2, payload_checksum(&p2)),
                p2.as_slice(),
            ),
            (
                RedexEntry::new_inline(2, &p3, payload_checksum(&p3)),
                p3.as_slice(),
            ),
        ];
        let timestamps = [100u64, 200, 300];

        {
            let recovered = DiskSegment::open(&base, &name, 0, 0).unwrap();
            recovered
                .disk
                .append_entries_at(&entries, &timestamps)
                .unwrap();
            recovered.disk.sync().unwrap();
        }

        // dat file was created by `open` but should still be empty —
        // no heap payloads means the dat write path was skipped.
        let dat_path = channel_dir(&base, &name).join("dat");
        assert_eq!(
            std::fs::metadata(&dat_path).unwrap().len(),
            0,
            "all-inline batch must not write to dat"
        );

        let recovered = DiskSegment::open(&base, &name, 0, 0).unwrap();
        assert_eq!(recovered.index.len(), 3);
        for e in &recovered.index {
            assert!(e.is_inline());
        }
        let ts = recovered.timestamps.expect("ts sidecar present");
        assert_eq!(ts, vec![100, 200, 300]);

        cleanup(&base);
    }

    /// Worker-side file handles are cloned from the appender's via
    /// `File::try_clone` so they share the same underlying OS file.
    /// This is the contract that lets the background worker call
    /// `sync_all` without holding the appender's mutex; if a
    /// regression accidentally opened a separate file, the worker
    /// would fsync the wrong descriptor and durability would
    /// silently break.
    ///
    /// The check: after appending through the appender path, the
    /// worker handles' `metadata().len()` must equal the on-disk
    /// file length. Different File instances of the same OS file
    /// see the same length; different files would not.
    #[test]
    fn test_worker_handles_share_os_file_with_appender() {
        let base = tmpdir();
        let name = ChannelName::new("t/worker_share").unwrap();
        let recovered = DiskSegment::open(&base, &name, 0, 0).unwrap();

        let p1 = b"alpha";
        let e1 = RedexEntry::new_heap(0, 0, p1.len() as u32, 0, payload_checksum(p1));
        recovered.disk.append_entry(&e1, p1).unwrap();
        let p2 = b"beta_payload";
        let e2 = RedexEntry::new_heap(1, p1.len() as u32, p2.len() as u32, 0, payload_checksum(p2));
        recovered.disk.append_entry(&e2, p2).unwrap();

        // Worker handles must observe the same file lengths the
        // appender just produced.
        let (dat_w, idx_w, ts_w) = recovered.disk.worker_file_lens();
        assert_eq!(
            dat_w,
            (p1.len() + p2.len()) as u64,
            "worker dat handle must reflect the appender's heap writes"
        );
        assert_eq!(idx_w, 2 * REDEX_ENTRY_SIZE as u64);
        assert_eq!(ts_w, 2 * 8);

        // Cross-check against the on-disk files. If the worker
        // handles pointed at separate files (regression scenario),
        // these would diverge.
        let dat_path = channel_dir(&base, &name).join("dat");
        let idx_path = channel_dir(&base, &name).join("idx");
        let ts_path = channel_dir(&base, &name).join("ts");
        assert_eq!(dat_w, std::fs::metadata(&dat_path).unwrap().len());
        assert_eq!(idx_w, std::fs::metadata(&idx_path).unwrap().len());
        assert_eq!(ts_w, std::fs::metadata(&ts_path).unwrap().len());

        // sync() goes through the worker handles. It must succeed
        // without locking against the appender (the test runs
        // single-threaded so we're really only confirming
        // operational correctness here, not the no-contention
        // property which lives in the bench).
        recovered.disk.sync().unwrap();

        cleanup(&base);
    }

    /// `compact_to` does a placeholder swap → atomic rename →
    /// re-open of the appender handles; the worker handles must be
    /// re-cloned from the new appender handles afterward, otherwise
    /// they're left pointing at the temp-dir placeholder file. A
    /// regression that forgot the re-clone would have `sync()`
    /// silently fsync the placeholder instead of the channel files
    /// — durable on close (the appender handles still work) but
    /// not on the policy-driven worker path.
    ///
    /// The check: after `compact_to`, the worker handles' lengths
    /// must equal the post-compaction file lengths. A subsequent
    /// append through the appender then `sync()` then reopen must
    /// round-trip both the surviving compacted entry and the new
    /// post-compaction append.
    #[test]
    fn test_compact_to_re_clones_worker_handles() {
        let base = tmpdir();
        let name = ChannelName::new("t/compact_workers").unwrap();
        let recovered = DiskSegment::open(&base, &name, 0, 0).unwrap();

        // Three heap appends with explicit timestamps so the
        // surviving entry's ts is predictable.
        let payloads: [&[u8]; 3] = [b"first_one", b"second_one", b"third_one_"];
        let mut offset = 0u32;
        for (i, p) in payloads.iter().enumerate() {
            let e = RedexEntry::new_heap(i as u64, offset, p.len() as u32, 0, payload_checksum(p));
            recovered
                .disk
                .append_entry_at(&e, p, 1_000 * (i as u64 + 1))
                .unwrap();
            offset += p.len() as u32;
        }
        recovered.disk.sync().unwrap();

        // Compact down to the third entry only. `compact_to`
        // rewrites its `payload_offset` to be relative to the new
        // dat file (so 0 in the compacted file).
        let third_dat_base = (payloads[0].len() + payloads[1].len()) as u64;
        let surviving = vec![RedexEntry::new_heap(
            2,
            third_dat_base as u32,
            payloads[2].len() as u32,
            0,
            payload_checksum(payloads[2]),
        )];
        let surviving_ts = vec![3_000u64];
        recovered
            .disk
            .compact_to(&surviving, &surviving_ts, third_dat_base)
            .unwrap();

        // Worker handles must now reflect the compacted file sizes,
        // not the placeholder (which had size 0). This is the
        // direct probe for the re-clone in `compact_to`.
        let dat_path = channel_dir(&base, &name).join("dat");
        let idx_path = channel_dir(&base, &name).join("idx");
        let ts_path = channel_dir(&base, &name).join("ts");
        let on_disk_dat = std::fs::metadata(&dat_path).unwrap().len();
        let on_disk_idx = std::fs::metadata(&idx_path).unwrap().len();
        let on_disk_ts = std::fs::metadata(&ts_path).unwrap().len();
        assert_eq!(on_disk_dat, payloads[2].len() as u64, "sanity");
        assert_eq!(on_disk_idx, REDEX_ENTRY_SIZE as u64, "sanity");
        assert_eq!(on_disk_ts, 8, "sanity");
        let (dat_w, idx_w, ts_w) = recovered.disk.worker_file_lens();
        assert_eq!(
            dat_w, on_disk_dat,
            "worker dat handle must point at the compacted dat file, \
             not the placeholder"
        );
        assert_eq!(
            idx_w, on_disk_idx,
            "worker idx handle must point at the compacted idx file"
        );
        assert_eq!(
            ts_w, on_disk_ts,
            "worker ts handle must point at the compacted ts file"
        );

        // Append a new entry through the appender; with the worker
        // handles re-cloned correctly, `sync()` flushes the right
        // file and the post-compact append is durable across reopen.
        let new_payload = b"after_compact";
        let new_entry = RedexEntry::new_heap(
            99,
            on_disk_dat as u32,
            new_payload.len() as u32,
            0,
            payload_checksum(new_payload),
        );
        recovered
            .disk
            .append_entry_at(&new_entry, new_payload, 4_000)
            .unwrap();
        recovered.disk.sync().unwrap();

        drop(recovered);
        let recovered2 = DiskSegment::open(&base, &name, 0, 0).unwrap();
        let seqs: Vec<u64> = recovered2.index.iter().map(|e| e.seq).collect();
        assert_eq!(
            seqs,
            vec![2, 99],
            "post-compact reopen must show the surviving entry and \
             the post-compaction append"
        );
        let ts = recovered2.timestamps.expect("ts sidecar present");
        assert_eq!(
            ts,
            vec![3_000, 4_000],
            "timestamps must pair with the right index records after \
             compaction + post-compact append"
        );

        cleanup(&base);
    }

    /// Regression for BUG_AUDIT_2026_04_30_CORE.md #93: the
    /// `fsync_dir` helper added to make `compact_to`'s rename
    /// sequence durable on POSIX must complete cleanly on a
    /// normal directory and on a directory with `O_RDONLY`
    /// access (the open mode `File::open` uses).
    ///
    /// On Windows the helper is a no-op and returns `Ok(())`
    /// regardless of input — see `fsync_dir_no_op_on_non_unix`
    /// for that branch's regression. This test pins POSIX-only
    /// success.
    #[test]
    fn fsync_dir_helper_succeeds_on_a_normal_directory() {
        let tmp = std::env::temp_dir().join(format!(
            "redex-fsync-dir-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&tmp).expect("create tempdir");

        super::fsync_dir(&tmp).expect("fsync_dir must succeed on a normal dir");

        // Cleanup. Best-effort; not load-bearing for the test.
        let _ = std::fs::remove_dir(&tmp);
    }

    /// CR-11: on non-Unix targets, `fsync_dir` is a no-op that
    /// returns `Ok(())` on ANY input — even a path that does not
    /// exist, even a path that is not a directory. This pins the
    /// no-op contract so a future "let's fail closed on Windows"
    /// change has to also update this test (and consider whether
    /// the change actually closes the durability hazard or just
    /// trades durability gap for crash-on-bad-path).
    ///
    /// The compact_to caller covers the per-file durability via
    /// the `sync_all` calls added in CR-11 — that's where the
    /// real durability work happens on Windows.
    #[cfg(not(unix))]
    #[test]
    fn fsync_dir_no_op_on_non_unix_returns_ok_even_for_nonexistent_paths() {
        // Path that demonstrably does not exist: no-op should
        // STILL return Ok. This is the documented contract.
        let bogus = std::path::PathBuf::from(format!(
            "{}/redex-no-such-dir-{}",
            std::env::temp_dir().display(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        assert!(!bogus.exists(), "test fixture: bogus path must not exist");
        super::fsync_dir(&bogus).expect("non-Unix fsync_dir must be a no-op Ok");
    }

    /// CR-4: `reopen_with_retries` must succeed on the first
    /// attempt for a normally-openable redex file path. This pins
    /// the happy path — if the helper accidentally always took a
    /// retry delay, the post-compact path would acquire 25ms of
    /// wall-clock latency per call.
    #[test]
    fn reopen_with_retries_succeeds_immediately_on_normal_path() {
        let dir = tmpdir();
        let path = dir.join("idx_existing");
        // Create the file with the same options redex uses so the
        // open flags match the production path.
        {
            let _ = OpenOptions::new()
                .create(true)
                .read(true)
                .append(true)
                .open(&path)
                .expect("seed file");
        }
        let start = std::time::Instant::now();
        let f = super::reopen_with_retries(&path).expect("reopen normal file");
        let elapsed = start.elapsed();
        assert!(
            elapsed < std::time::Duration::from_millis(20),
            "happy-path reopen must NOT incur a retry delay; took {:?}",
            elapsed
        );
        // Confirm the handle is usable (write-then-flush via the
        // append handle returned).
        drop(f);
        cleanup(&dir);
    }

    /// CR-4: `reopen_with_retries` must surface the LAST io::Error
    /// when all attempts fail. We fake a permanent failure with a
    /// path whose parent directory does not exist (so every open
    /// attempt returns NotFound). Returning a meaningful error
    /// from the final attempt is what lets the caller wrap it in
    /// `RedexError::io` with a real diagnosis.
    #[test]
    fn reopen_with_retries_returns_last_error_after_exhaustion() {
        let dir = tmpdir();
        // Path INSIDE a directory that doesn't exist — every
        // `OpenOptions::open` call will fail with NotFound. The
        // `create(true)` flag won't auto-create parent dirs, so
        // this is a permanent failure.
        let bogus = dir.join("does_not_exist_subdir").join("idx");
        let start = std::time::Instant::now();
        let err =
            super::reopen_with_retries(&bogus).expect_err("nonexistent parent dir must fail open");
        let elapsed = start.elapsed();
        // We attempt 5 times with 0+25+50+100+200 = 375ms of
        // total sleep across the retries. Allow generous slack
        // for slow CI but pin the order-of-magnitude.
        assert!(
            elapsed >= std::time::Duration::from_millis(300),
            "must have done full retry budget before giving up; took {:?}",
            elapsed
        );
        // The error MUST be a real io::Error (NotFound on most
        // platforms when the parent doesn't exist), not a
        // synthesized placeholder. This is what makes the
        // surfaced `RedexError::io(err)` debuggable.
        assert!(
            matches!(
                err.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
            ),
            "expected NotFound or PermissionDenied; got {:?}: {}",
            err.kind(),
            err
        );
        cleanup(&dir);
    }

    /// CR-4: pin the post-rename atomic-swap invariant directly.
    /// If `reopen_with_retries` returned `Err` for any of the six
    /// post-rename opens, the cached slots in `DiskSegment` MUST
    /// still hold the placeholder files (NOT mid-swap mixed
    /// state) — pre-CR-4 the `?` early-return would leave some
    /// slots updated and others not, depending on which call
    /// failed.
    ///
    /// The current production design opens all six handles into
    /// LOCAL VARIABLES first and only swaps after all six
    /// succeed. We don't have a clean fault-injection seam for
    /// the post-rename open phase (a real test would need an
    /// fs-level hook), but we CAN exercise the success path's
    /// post-conditions under a real `compact_to`: all worker
    /// handles must come out of the call pointing at the new
    /// channel files (sizes match the on-disk metadata), with
    /// zero placeholder contamination.
    #[test]
    fn compact_to_post_rename_swap_is_atomic_on_success_path() {
        let base = tmpdir();
        let name = ChannelName::new("t/cr4_atomic").unwrap();
        let recovered = DiskSegment::open(&base, &name, 0, 0).unwrap();

        // Three heap appends; compact down to the third.
        let payloads: [&[u8]; 3] = [b"alpha_aaaa", b"beta_bbbbb", b"gamma_cccc"];
        let mut offset = 0u32;
        for (i, p) in payloads.iter().enumerate() {
            let e = RedexEntry::new_heap(i as u64, offset, p.len() as u32, 0, payload_checksum(p));
            recovered
                .disk
                .append_entry_at(&e, p, 1_000 * (i as u64 + 1))
                .unwrap();
            offset += p.len() as u32;
        }
        recovered.disk.sync().unwrap();

        let third_dat_base = (payloads[0].len() + payloads[1].len()) as u64;
        let surviving = vec![RedexEntry::new_heap(
            2,
            third_dat_base as u32,
            payloads[2].len() as u32,
            0,
            payload_checksum(payloads[2]),
        )];
        let surviving_ts = vec![3_000u64];
        recovered
            .disk
            .compact_to(&surviving, &surviving_ts, third_dat_base)
            .expect("happy path compact must succeed");

        // After successful compact, every cached file slot must
        // point at the channel directory's idx/dat/ts — NOT at a
        // placeholder in the OS temp dir. We probe via worker
        // handle sizes (the worker handles are `try_clone`d from
        // the appender slots, so if EITHER kind ended up holding
        // a placeholder, the size would be 0 / wrong).
        let dat_path = channel_dir(&base, &name).join("dat");
        let idx_path = channel_dir(&base, &name).join("idx");
        let ts_path = channel_dir(&base, &name).join("ts");
        let on_disk_dat = std::fs::metadata(&dat_path).unwrap().len();
        let on_disk_idx = std::fs::metadata(&idx_path).unwrap().len();
        let on_disk_ts = std::fs::metadata(&ts_path).unwrap().len();
        let (dat_w, idx_w, ts_w) = recovered.disk.worker_file_lens();
        assert_eq!(dat_w, on_disk_dat, "worker dat must NOT hold a placeholder");
        assert_eq!(idx_w, on_disk_idx, "worker idx must NOT hold a placeholder");
        assert_eq!(ts_w, on_disk_ts, "worker ts must NOT hold a placeholder");

        // Also exercise that the appender slots are usable: a new
        // append must reach the on-disk dat file (placeholder dat
        // was 0 bytes — placeholder writes wouldn't survive
        // reopen).
        let new_payload = b"after_compact";
        let new_entry = RedexEntry::new_heap(
            99,
            on_disk_dat as u32,
            new_payload.len() as u32,
            0,
            payload_checksum(new_payload),
        );
        recovered
            .disk
            .append_entry_at(&new_entry, new_payload, 4_000)
            .unwrap();
        recovered.disk.sync().unwrap();
        drop(recovered);

        // Reopen — the post-compact append MUST be present.
        let recovered2 = DiskSegment::open(&base, &name, 0, 0).unwrap();
        let seqs: Vec<u64> = recovered2.index.iter().map(|e| e.seq).collect();
        assert_eq!(
            seqs,
            vec![2, 99],
            "post-compact append must be on the channel dat, not a tmp placeholder"
        );

        cleanup(&base);
    }

    /// Cubic P1: pin the segment-poisoning recovery contract.
    /// When `compact_to`'s post-rename re-open fails, the cached
    /// file handles point at temp-dir placeholders. Subsequent
    /// appends MUST refuse rather than silently writing into
    /// `/tmp`. We test the post-condition (poison flag → all
    /// write paths refuse) by manually flipping the flag,
    /// independent of the fs-fault-injection seam.
    #[test]
    fn cubic_p1_poisoned_segment_refuses_writes() {
        let base = tmpdir();
        let name = ChannelName::new("t/poison").unwrap();
        let recovered = DiskSegment::open(&base, &name, 0, 0).unwrap();

        // Sanity: a fresh segment accepts appends.
        let payload: &[u8] = b"first";
        let entry = RedexEntry::new_heap(0, 0, payload.len() as u32, 0, payload_checksum(payload));
        recovered
            .disk
            .append_entry_at(&entry, payload, 1_000)
            .expect("fresh segment must accept appends");

        // Poison the segment as if `compact_to`'s re-open phase
        // had failed.
        recovered
            .disk
            .poisoned
            .store(true, std::sync::atomic::Ordering::Release);

        // Every write path must refuse.
        let entry2 = RedexEntry::new_heap(1, 5, payload.len() as u32, 0, payload_checksum(payload));
        let err = recovered
            .disk
            .append_entry_at(&entry2, payload, 2_000)
            .expect_err("poisoned segment must refuse append_entry_at");
        let msg = format!("{}", err);
        assert!(
            msg.contains("poisoned"),
            "error message must reference poisoning; got: {msg}"
        );

        let err = recovered
            .disk
            .append_entries_at(&[(entry2, payload)], &[3_000])
            .expect_err("poisoned segment must refuse append_entries_at");
        assert!(
            format!("{}", err).contains("poisoned"),
            "append_entries_at error must reference poisoning"
        );

        let err = recovered
            .disk
            .compact_to(&[], &[], 0)
            .expect_err("poisoned segment must refuse compact_to");
        assert!(
            format!("{}", err).contains("poisoned"),
            "compact_to error must reference poisoning"
        );

        cleanup(&base);
    }

    /// Source pin: the idx/dat truncations in
    /// `pub(super) fn open` recovery MUST each be followed by
    /// `sync_all()` on the same handle. Pre-fix the recovery code
    /// did `set_len` without `sync_all`, so a crash between
    /// truncation and the next durable write could leave the
    /// torn tail on disk; on next reopen the dat-vs-idx
    /// invariants would still hold transiently, but a later
    /// append (which extends the file from the un-synced length
    /// the OS buffers held) could resurrect the torn region.
    ///
    /// The ts (timestamp) sidecar uses `set_len(index.len() * 8)`
    /// to align length with the (already-recovered) idx — a
    /// crash here is idempotent because reopen recomputes the
    /// same surviving index and applies the same alignment, so
    /// that one set_len is intentionally NOT in scope of this
    /// pin.
    ///
    /// We assert the two recovery-path truncations are present
    /// in the post-fix shape: `set_len((index.len() * REDEX_ENTRY_SIZE)`
    /// for idx and `set_len(retained_dat_end)` for dat. For each,
    /// the next non-blank, non-comment, non-`?` continuation
    /// line within a small window must contain `sync_all()`.
    #[test]
    fn recovery_idx_dat_truncations_must_be_paired_with_sync_all() {
        let src = include_str!("disk.rs");

        let header = "pub(super) fn open(";
        let start = src.find(header).expect("DiskSegment::open must exist");
        let body_start = start + header.len();
        let next_fn_offsets: Vec<usize> = ["\n    fn ", "\n    pub fn ", "\n    pub(super) fn "]
            .iter()
            .filter_map(|p| src[body_start..].find(p).map(|i| i + body_start))
            .collect();
        let next_fn = *next_fn_offsets
            .iter()
            .min()
            .expect("a following fn must exist after open()");
        let body = &src[start..next_fn];

        let lines: Vec<&str> = body
            .lines()
            .map(|l| match l.find("//") {
                Some(idx) => &l[..idx],
                None => l,
            })
            .collect();

        // The two recovery-path truncations the audit's #11 fix
        // added sync_all for. The ts-sidecar `set_len` is
        // intentionally excluded (idempotent across crashes).
        let in_scope_markers = [
            "set_len((index.len() * REDEX_ENTRY_SIZE)",
            "set_len(retained_dat_end)",
        ];

        let mut found_any = [false; 2];
        let window = 5;
        for (i, line) in lines.iter().enumerate() {
            for (mi, marker) in in_scope_markers.iter().enumerate() {
                if !line.contains(marker) {
                    continue;
                }
                found_any[mi] = true;

                let mut paired = false;
                for off in 1..=window {
                    if i + off >= lines.len() {
                        break;
                    }
                    if lines[i + off].contains("sync_all()") {
                        paired = true;
                        break;
                    }
                }
                assert!(
                    paired,
                    "regression: `{}` in DiskSegment::open recovery at \
                     (relative) line {} is not followed within {} lines \
                     by `sync_all()`. A crash between truncation and \
                     the next durable write reincarnates the torn tail.",
                    marker, i, window
                );
            }
        }

        for (mi, marker) in in_scope_markers.iter().enumerate() {
            assert!(
                found_any[mi],
                "expected to find `{}` in DiskSegment::open — the \
                 recovery walk's truncation step appears to have \
                 been removed or refactored. Audit the new shape \
                 to confirm the fsync pairing is preserved.",
                marker
            );
        }
    }

    /// Source pin: `rollback_truncate` must poison the segment
    /// on BOTH the open-failure and `set_len`-failure branches.
    /// Pre-fix the rollback used `if let Ok(f) = OpenOptions::...`
    /// and silently dropped the open error, leaving the segment
    /// in a permanently-divergent state with no diagnostic. The
    /// fixed shape uses `match` and stores `true` to
    /// `self.poisoned` in every error arm.
    #[test]
    fn rollback_truncate_must_poison_on_failure() {
        let src = include_str!("disk.rs");

        let header = "fn rollback_truncate(";
        let start = src.find(header).expect("rollback_truncate must exist");
        let body_start = start + header.len();
        let next_fn_offsets: Vec<usize> = ["\n    fn ", "\n    pub fn ", "\n    pub(super) fn "]
            .iter()
            .filter_map(|p| src[body_start..].find(p).map(|i| i + body_start))
            .collect();
        let next_fn = *next_fn_offsets
            .iter()
            .min()
            .expect("a following fn must exist after rollback_truncate");
        let body = &src[start..next_fn];

        // The `poisoned.store(true, Ordering::Release)` line
        // must appear at least twice in the body (once for the
        // set_len failure arm, once for the open failure arm).
        // Pre-fix the function silently swallowed the open error
        // — only one (or zero) `poisoned.store` calls were
        // present. Two-or-more is the post-fix shape.
        let poison_count = body.matches("poisoned.store(true").count();
        assert!(
            poison_count >= 2,
            "regression: rollback_truncate must call \
             `poisoned.store(true, ...)` in BOTH the open-failure \
             and set_len-failure arms (saw {} occurrences). \
             Pre-fix the open-failure arm silently dropped the \
             error and left the segment divergent.",
            poison_count
        );

        // The buggy pattern `if let Ok(f) = OpenOptions::` (which
        // discards the Err and proceeds without poisoning) must
        // not reappear inside this function.
        assert!(
            !body.contains("if let Ok(f) = OpenOptions::"),
            "regression: rollback_truncate must not use the \
             `if let Ok(f) = OpenOptions::...` shape — that \
             discards the open error, the exact pre-fix bug. \
             Use `match` and poison on the Err arm."
        );
    }
}
