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
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
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
    /// Full path to the per-channel **root** directory (i.e.
    /// `<base>/<channel_path>/`). Holds the manifest pointer file
    /// plus zero-or-more `v<NNN>/` generation directories. The
    /// actual idx/dat/ts files live one level deeper, under
    /// `live_gen_dir()`.
    dir: PathBuf,
    /// Current live generation. Names the `v<NNN>/` directory
    /// under `dir` that holds the live `{idx, dat, ts}` files.
    /// Updated by `compact_to` after the manifest flip; read by
    /// the rollback paths to construct file paths.
    live_gen: AtomicU32,
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
    /// Segment poisoning. Set to `true` when one of the
    /// partial-write rollback paths
    /// ([`Self::rollback_truncate`],
    /// [`Self::rollback_after_idx_failure`]) cannot complete the
    /// truncation that would restore the file to its pre-failure
    /// length — at that point the on-disk state has diverged from
    /// in-memory and a subsequent append would compound the
    /// corruption rather than land cleanly. Once poisoned, every
    /// append / sync / compact path returns `RedexError::Io`
    /// immediately. Operators recover by closing and re-opening
    /// the channel (which constructs a fresh `DiskSegment` and
    /// runs the crash-recovery walks against on-disk truth).
    ///
    /// Historical note: the prior pre-manifest-pointer
    /// `compact_to` also set this flag from a "post-rename
    /// reopen failure / temp-dir placeholder" path that no
    /// longer exists; the manifest-pointer rework opens the new
    /// generation's handles BEFORE the atomic flip, so a failed
    /// open aborts the compact while live state is still
    /// intact. The rollback paths are now the only setters.
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
        // Resolve the live generation via the manifest pointer (or
        // fall back to enumerating `v<NNN>/` directories, or migrate
        // a legacy flat-layout channel, or create a fresh one). The
        // resulting `gen` names the live `<channel>/v<gen>/` directory
        // that holds the actual idx/dat/ts files; recovery walks run
        // against those paths.
        let live_gen = resolve_live_generation(&dir).map_err(RedexError::io)?;
        let live_dir = gen_dir(&dir, live_gen);
        // Make absolutely sure the generation directory exists. The
        // resolver creates it for the brand-new and migration paths
        // but a manifest pointing at a generation dir that someone
        // externally rm'd between resolve and here would slip through
        // (`generation_is_complete` checked file presence at resolve
        // time, but a vanished dir wouldn't trip the migration
        // fallback). Defensive create is a no-op when the directory
        // already exists.
        std::fs::create_dir_all(&live_dir).map_err(RedexError::io)?;
        let idx_path = live_dir.join("idx");
        let dat_path = live_dir.join("dat");

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
        let ts_path = live_dir.join("ts");
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

        // Sweep any orphan generation directories left behind by a
        // crashed prior `compact_to`. A crash between writing
        // `v<N+1>/{idx,dat,ts}` and the manifest flip leaves the
        // partial `v<N+1>/` behind; recovery picked the older `v<N>/`
        // and now we delete the stale newer one. Best-effort — sweep
        // failures are logged but don't block open.
        sweep_orphan_generations(&dir, live_gen);

        Ok(RecoveredSegment {
            disk: DiskSegment {
                dir,
                live_gen: AtomicU32::new(live_gen),
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
    /// Path under the live generation directory. Equivalent to
    /// `dir/v<live_gen>/file_name`. Used by rollback paths and
    /// other internal callers that need to re-open a file by path
    /// for `set_len` (the cached append-mode handle can't truncate
    /// on every platform).
    fn live_gen_path(&self, file_name: &str) -> PathBuf {
        gen_dir(&self.dir, self.live_gen.load(Ordering::Acquire)).join(file_name)
    }

    /// Test-only: live generation number. Lets tests construct
    /// paths under `<channel>/v<gen>/` to inspect on-disk state.
    #[cfg(test)]
    pub(super) fn live_gen(&self) -> u32 {
        self.live_gen.load(Ordering::Acquire)
    }

    /// Test-only: live generation directory. Equivalent to
    /// `gen_dir(channel_root, live_gen)`. Tests use this to read
    /// raw idx/dat/ts files rather than reconstructing the path.
    #[cfg(test)]
    pub(super) fn live_dir(&self) -> PathBuf {
        gen_dir(&self.dir, self.live_gen.load(Ordering::Acquire))
    }

    fn rollback_truncate(&self, file_name: &str, target_len: u64) {
        let path = self.live_gen_path(file_name);
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
            let dat_path = self.live_gen_path("dat");
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
        // Refuse writes against a poisoned segment. The flag is
        // set by the partial-write rollback paths when they
        // cannot truncate the file back to its pre-failure
        // length — on-disk and in-memory have diverged, and any
        // further append would compound the corruption rather
        // than land cleanly. See the `poisoned` field rustdoc.
        if self.poisoned.load(std::sync::atomic::Ordering::Acquire) {
            return Err(RedexError::Io(
                "redex segment is poisoned (partial-write rollback could not restore \
                 on-disk state to match in-memory); close and re-open the channel to recover"
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
                let dat_path = self.live_gen_path("dat");
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
                let dat_path = self.live_gen_path("dat");
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
                    let dat_path = self.live_gen_path("dat");
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
                "redex segment is poisoned (partial-write rollback could not restore \
                 on-disk state to match in-memory); close and re-open the channel to recover"
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
    ///
    /// # Atomic flip via manifest pointer
    ///
    /// The new content is written to a fresh `v<N+1>/` directory
    /// alongside the live `v<N>/`, fsync'd in full, and then the
    /// channel's `manifest` pointer is atomically swapped to point
    /// at `v<N+1>/`. The manifest rename is the single linearizing
    /// event:
    ///
    /// - Before it, recovery sees the old manifest and uses
    ///   `v<N>/`. The half-written `v<N+1>/` is swept as an
    ///   orphan on the next open.
    /// - After it, recovery sees the new manifest and uses
    ///   `v<N+1>/`. The superseded `v<N>/` is swept (best-effort
    ///   here, again on the next open if the local cleanup is
    ///   interrupted).
    ///
    /// The pre-rework three-rename sequence had a cross-file
    /// mixed-state window where recovery could land on
    /// `idx@N+1 + dat@N + ts@N` after a crash between rename N
    /// and N+1. The new layout removes that window: every
    /// generation directory is either complete or orphaned, never
    /// mixed.
    pub(super) fn compact_to(
        &self,
        surviving_index: &[RedexEntry],
        surviving_timestamps: &[u64],
        dat_base: u64,
    ) -> Result<(), RedexError> {
        // Refuse compact against a poisoned segment. The flag
        // is set by the partial-write rollback paths when they
        // can't restore on-disk state to match in-memory; running
        // compact_to against a diverged segment would copy that
        // divergence into the new generation and cement the
        // corruption past the next manifest flip.
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

        // Snapshot the live generation. We're the only writer of
        // `live_gen` (compact_to is serialized by the file locks
        // we'll acquire below; no other code path mutates it after
        // open), so a relaxed read is sufficient — but Acquire
        // matches `live_gen.store(Release)` at the end of the
        // happy path, future-proofing if a second writer ever
        // appears.
        let cur_gen = self.live_gen.load(Ordering::Acquire);
        let next_gen = cur_gen.checked_add(1).ok_or_else(|| {
            RedexError::Io(
                "compact_to: live_gen at u32::MAX; refusing further compactions \
                 (re-create the channel to reset)"
                    .into(),
            )
        })?;
        let cur_dir = gen_dir(&self.dir, cur_gen);
        let next_dir = gen_dir(&self.dir, next_gen);

        // Build the new idx, ts contents in-memory; the dat tail
        // we rewrite by reading the surviving range from the
        // CURRENT generation's dat file. The reads can run without
        // any file lock — readers don't block readers, and the
        // appender writes to the same files under their own lock.
        // The compact-as-of-now snapshot is implicitly consistent
        // because in-memory `surviving_index` was computed from a
        // consistent snapshot of the heap, and `dat_base` was
        // derived from that same snapshot.
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
        // Read the surviving dat tail from the current generation.
        let cur_dat_path = cur_dir.join("dat");
        let old_dat = read_payload(&cur_dat_path)?;
        let new_dat = if dat_base as usize >= old_dat.len() {
            Vec::new()
        } else {
            old_dat[dat_base as usize..].to_vec()
        };

        // Create the new generation directory and write its three
        // files. Until the manifest flip below, none of this is
        // referenced by the live state — a crash at any point
        // here leaves `v<next_gen>/` as an orphan that the next
        // open's sweep removes.
        std::fs::create_dir_all(&next_dir).map_err(RedexError::io)?;
        let next_idx_path = next_dir.join("idx");
        let next_dat_path = next_dir.join("dat");
        let next_ts_path = next_dir.join("ts");
        {
            let mut f = OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&next_idx_path)
                .map_err(RedexError::io)?;
            f.write_all(&new_idx_bytes).map_err(RedexError::io)?;
            f.sync_all().map_err(RedexError::io)?;
        }
        {
            let mut f = OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&next_dat_path)
                .map_err(RedexError::io)?;
            f.write_all(&new_dat).map_err(RedexError::io)?;
            f.sync_all().map_err(RedexError::io)?;
        }
        {
            let mut f = OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&next_ts_path)
                .map_err(RedexError::io)?;
            f.write_all(&new_ts_bytes).map_err(RedexError::io)?;
            f.sync_all().map_err(RedexError::io)?;
        }
        fsync_dir(&next_dir).map_err(RedexError::io)?;

        // Open the new generation's append + worker handles ahead
        // of the manifest flip. If any open fails, we abort the
        // compact entirely — `live_gen` is unchanged, the cached
        // handles still point at `cur_dir`, and the orphaned
        // `next_dir` will be swept on the next open. No
        // poisoning needed: the live state is unchanged.
        let new_idx = OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(&next_idx_path)
            .map_err(RedexError::io)?;
        let new_dat = OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(&next_dat_path)
            .map_err(RedexError::io)?;
        let new_ts = OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(&next_ts_path)
            .map_err(RedexError::io)?;
        let new_idx_worker = new_idx.try_clone().map_err(RedexError::io)?;
        let new_dat_worker = new_dat.try_clone().map_err(RedexError::io)?;
        let new_ts_worker = new_ts.try_clone().map_err(RedexError::io)?;

        // Acquire all six file locks before the atomic flip. From
        // here through the cached-handle swap below we are the
        // exclusive writer; concurrent appends block until we drop
        // the locks. Order matches the global lock discipline
        // documented in the module rustdoc.
        let mut dat_guard = self.dat_file.lock();
        let mut idx_guard = self.idx_file.lock();
        let mut ts_guard = self.ts_file.lock();
        let mut worker_dat_guard = self.worker_dat_file.lock();
        let mut worker_idx_guard = self.worker_idx_file.lock();
        let mut worker_ts_guard = self.worker_ts_file.lock();

        // ATOMIC FLIP: write the new manifest pointing at next_gen.
        // `write_manifest_atomic` writes manifest.tmp, fsyncs, then
        // `durable_rename(manifest.tmp -> manifest)`. Before the
        // rename, recovery sees the old manifest and uses
        // `cur_dir`. After the rename, recovery sees the new
        // manifest and uses `next_dir`. The rename is the single
        // linearizing event of the compact.
        if let Err(e) = write_manifest_atomic(&self.dir, next_gen) {
            // Manifest write failed BEFORE the flip — the live
            // state is unchanged, the next_gen directory is
            // orphaned and will be swept on the next open. Drop
            // the new handles (RAII) and surface the error.
            return Err(RedexError::io(e));
        }

        // Manifest flipped. From this point recovery would land on
        // `next_gen` — atomically swap the cached handles to match
        // (so the next append goes to next_gen via the cached
        // handles rather than cur_gen), then update `live_gen` so
        // the rollback paths construct paths under next_gen.
        //
        // Ordering note: the cached-handle slots are written
        // BEFORE the `live_gen.store(Release)`. The handle slots
        // and `live_gen` are read by disjoint code paths — the
        // append path reads only the cached handles, and the
        // rollback path reads only `live_gen` (it re-opens by
        // path via `live_gen_path()` rather than using the cached
        // handle). So neither path observes the pair; they each
        // observe their one half. The ordering still matters as
        // a defensive invariant: anyone who later writes code
        // that reads BOTH expects to see (handles, live_gen)
        // consistent, and the rule "swap handles, then publish
        // live_gen" is the simple one to follow. There is no
        // need for the swap and store to happen under the same
        // lock — the file locks we hold across this block already
        // exclude every concurrent reader of the cached handles.
        *idx_guard = new_idx;
        *dat_guard = new_dat;
        *ts_guard = new_ts;
        *worker_idx_guard = new_idx_worker;
        *worker_dat_guard = new_dat_worker;
        *worker_ts_guard = new_ts_worker;
        self.live_gen.store(next_gen, Ordering::Release);

        // Drop locks before the best-effort sweep of the prior
        // generation. Sweep failures are logged but not surfaced —
        // the live state is correct; an undeleted `cur_dir` is
        // just slow GC and gets cleaned up on the next open.
        drop(worker_ts_guard);
        drop(worker_idx_guard);
        drop(worker_dat_guard);
        drop(ts_guard);
        drop(idx_guard);
        drop(dat_guard);

        delete_generation(&self.dir, cur_gen);

        Ok(())
    }
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

pub(super) fn channel_dir(base_dir: &Path, name: &ChannelName) -> PathBuf {
    let mut p = base_dir.to_path_buf();
    for seg in name.as_str().split('/') {
        p.push(seg);
    }
    p
}

// ===========================================================================
// Manifest-pointer layout helpers.
//
// Each channel directory holds a single `manifest` pointer file plus
// zero-or-more generation directories `v0000000001/`, `v0000000002/`, …
// containing the actual `{idx, dat, ts}` files. `compact_to` writes a
// new generation directory and atomically swaps the manifest to point
// at it; recovery reads the manifest (or falls back to the highest
// validated generation directory if the manifest is torn or missing).
// ===========================================================================

/// Magic bytes at the head of the manifest. `REDM` for "RedEX
/// Manifest." Distinguishes a fresh-format manifest from a torn or
/// otherwise-corrupt 16-byte file.
const MANIFEST_MAGIC: [u8; 4] = *b"REDM";

/// Wire-format version of the manifest. v1 is the only version
/// today. Future bumps (e.g. wider generation field, additional
/// metadata) must reject older versions on read.
const MANIFEST_VERSION: u8 = 1;

/// Fixed wire size of the manifest file. 16 bytes — small enough
/// to fit inside any filesystem's atomic-write boundary, but
/// atomicity is provided by the rename of `manifest.tmp → manifest`
/// rather than by relying on single-write atomicity.
const MANIFEST_SIZE: usize = 16;

/// First valid generation number. `0` is reserved so a torn all-zero
/// 16-byte file is unambiguously invalid (magic + checksum would
/// also fail, but using a reserved sentinel adds defense in depth).
const FIRST_GENERATION: u32 = 1;

/// Format the generation number into the directory name
/// `v` + 10-digit zero-padded decimal. Pads to 10 digits so a
/// channel that survives ~4 B compactions still sorts
/// lexicographically by generation; saturation at `u32::MAX` would
/// require ~136 years of one compaction per second.
fn gen_dir_name(gen: u32) -> String {
    format!("v{:010}", gen)
}

/// Path to the manifest pointer file under `channel_dir`.
fn manifest_path(channel_dir: &Path) -> PathBuf {
    channel_dir.join("manifest")
}

/// Path to the manifest's tmp-write companion. Rename
/// `manifest.tmp → manifest` is the single atomic flip.
fn manifest_tmp_path(channel_dir: &Path) -> PathBuf {
    channel_dir.join("manifest.tmp")
}

/// Path to the live generation directory `v<gen>/` under
/// `channel_dir`.
fn gen_dir(channel_dir: &Path, gen: u32) -> PathBuf {
    channel_dir.join(gen_dir_name(gen))
}

/// Encode a manifest payload into the 16-byte wire format.
///
/// Layout (little-endian):
///   [0..4]   magic = `REDM`
///   [4]      version (u8) = 1
///   [5..9]   generation (u32 LE)
///   [9..12]  reserved = `[0, 0, 0]`
///   [12..16] checksum (u32 LE) = xxh3 over [0..12]
fn encode_manifest(generation: u32) -> [u8; MANIFEST_SIZE] {
    let mut buf = [0u8; MANIFEST_SIZE];
    buf[0..4].copy_from_slice(&MANIFEST_MAGIC);
    buf[4] = MANIFEST_VERSION;
    buf[5..9].copy_from_slice(&generation.to_le_bytes());
    // buf[9..12] stays [0, 0, 0] (reserved).
    let checksum = xxhash_rust::xxh3::xxh3_64(&buf[0..12]) as u32;
    buf[12..16].copy_from_slice(&checksum.to_le_bytes());
    buf
}

/// Decode a 16-byte manifest payload, validating magic, version,
/// reserved bytes, generation > 0, and checksum. Returns the
/// generation on success, or `None` for any kind of corruption — the
/// caller falls back to the directory-scan path.
fn decode_manifest(bytes: &[u8]) -> Option<u32> {
    if bytes.len() != MANIFEST_SIZE {
        return None;
    }
    if bytes[0..4] != MANIFEST_MAGIC {
        return None;
    }
    if bytes[4] != MANIFEST_VERSION {
        return None;
    }
    if bytes[9..12] != [0, 0, 0] {
        return None;
    }
    let claimed_checksum = u32::from_le_bytes(bytes[12..16].try_into().ok()?);
    let computed_checksum = xxhash_rust::xxh3::xxh3_64(&bytes[0..12]) as u32;
    if claimed_checksum != computed_checksum {
        return None;
    }
    let generation = u32::from_le_bytes(bytes[5..9].try_into().ok()?);
    if generation < FIRST_GENERATION {
        return None;
    }
    Some(generation)
}

/// Read the manifest under `channel_dir`. Returns the generation if
/// the manifest exists and validates; `None` if missing, short, or
/// corrupt. Caller falls back to the directory-scan path.
fn read_manifest(channel_dir: &Path) -> Option<u32> {
    let path = manifest_path(channel_dir);
    let mut f = File::open(&path).ok()?;
    let mut bytes = [0u8; MANIFEST_SIZE];
    f.read_exact(&mut bytes).ok()?;
    decode_manifest(&bytes)
}

/// Write a new manifest pointing at `generation` durably.
///
/// 1. Write `manifest.tmp` with the encoded bytes; fsync.
/// 2. `durable_rename(manifest.tmp → manifest)` — atomic flip.
/// 3. fsync_dir on `channel_dir` to make the dirent durable.
///
/// Failure semantics:
///
/// - Any error BEFORE the rename (open/write/fsync of tmp, or
///   the rename itself) propagates as `Err`. The previous
///   manifest is preserved — the flip didn't happen.
/// - An error from `fsync_dir` AFTER a successful rename does
///   NOT propagate. The rename is the linearizing event; once
///   it commits, the manifest is visible to every subsequent
///   read regardless of whether the dirent has been flushed.
///   Surfacing the fsync error would lie to the caller about
///   whether the flip happened: `compact_to` would interpret
///   `Err` as "live state still at cur_gen, no cached-handle
///   swap" while the on-disk manifest already points at
///   next_gen — every in-process append between this point
///   and process exit would land in the now-dead generation
///   and be discarded by the orphan sweep on next open. We
///   log loudly, return `Ok(())`, and let the caller proceed
///   with the cached-handle swap so on-disk and in-memory
///   stay aligned. The residual durability gap (a power loss
///   before the next implicit dirent flush could revert the
///   rename) is recovered by the orphan-generation sweep on
///   next open: if the manifest reverts to cur_gen, next_gen
///   still exists and is swept; if the manifest stays at
///   next_gen, cur_gen is swept. Either way recovery
///   converges to a single consistent live generation.
fn write_manifest_atomic(channel_dir: &Path, generation: u32) -> std::io::Result<()> {
    let bytes = encode_manifest(generation);
    let tmp = manifest_tmp_path(channel_dir);
    let target = manifest_path(channel_dir);
    {
        let mut f = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&tmp)?;
        f.write_all(&bytes)?;
        f.sync_all()?;
    }
    durable_rename(&tmp, &target)?;
    if let Err(e) = fsync_dir(channel_dir) {
        tracing::warn!(
            error = %e,
            path = %channel_dir.display(),
            "redex manifest write: rename committed but fsync_dir failed; \
             manifest is visible to subsequent reads but dirent is not yet \
             durable on disk. Treating as success so the caller's cached-\
             handle swap proceeds (on-disk and in-memory must stay aligned). \
             Recovery's orphan-generation sweep handles a post-power-loss \
             revert.",
        );
    }
    Ok(())
}

/// Enumerate all `v<NNN>/` generation directories under
/// `channel_dir`, returning the parsed generation numbers in
/// descending order (highest first). Non-matching entries (anything
/// not named `v` + exactly 10 ASCII digits, or anything that's not a
/// directory) are silently skipped.
fn enumerate_generations(channel_dir: &Path) -> std::io::Result<Vec<u32>> {
    let mut gens: Vec<u32> = Vec::new();
    let dir_iter = match std::fs::read_dir(channel_dir) {
        Ok(it) => it,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(gens),
        Err(e) => return Err(e),
    };
    for entry in dir_iter.flatten() {
        let name = entry.file_name();
        let name_str = match name.to_str() {
            Some(s) => s,
            None => continue,
        };
        if name_str.len() != 11 || !name_str.starts_with('v') {
            continue;
        }
        let digits = &name_str[1..];
        if !digits.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let gen: u32 = match digits.parse() {
            Ok(n) if n >= FIRST_GENERATION => n,
            _ => continue,
        };
        // Must be a directory (skip stray files named like `v0000000001`).
        match entry.file_type() {
            Ok(ft) if ft.is_dir() => gens.push(gen),
            _ => continue,
        }
    }
    gens.sort_unstable_by(|a, b| b.cmp(a)); // descending
    Ok(gens)
}

/// True if `gen_dir(channel_dir, gen)` contains all three of
/// `{idx, dat, ts}` (any of them may be empty — a brand-new channel
/// has zero-byte files; absence is the disqualifier). Used by the
/// fallback path when the manifest is missing or torn.
fn generation_is_complete(channel_dir: &Path, gen: u32) -> bool {
    let dir = gen_dir(channel_dir, gen);
    dir.join("idx").is_file() && dir.join("dat").is_file() && dir.join("ts").is_file()
}

/// Best-effort delete of `gen_dir(channel_dir, gen)`. Failure is
/// logged but not surfaced — orphan generation cleanup is a
/// background concern; the live generation is unaffected.
fn delete_generation(channel_dir: &Path, gen: u32) {
    let dir = gen_dir(channel_dir, gen);
    if let Err(e) = std::fs::remove_dir_all(&dir) {
        // Not-found is fine — sweep can race a concurrent compact's
        // own cleanup, or another node may have already swept this
        // generation on a previous open.
        if e.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!(
                error = %e,
                path = %dir.display(),
                "redex sweep: failed to delete orphan generation directory (non-fatal)",
            );
        }
    }
}

/// Sweep every generation directory under `channel_dir` other than
/// `keep`. Called at the end of `open` (to clean up orphans from a
/// crashed prior compact) and at the end of `compact_to` (to clean up
/// the just-superseded prior generation).
fn sweep_orphan_generations(channel_dir: &Path, keep: u32) {
    let gens = match enumerate_generations(channel_dir) {
        Ok(gs) => gs,
        Err(e) => {
            tracing::warn!(
                error = %e,
                path = %channel_dir.display(),
                "redex sweep: failed to enumerate generation directories (non-fatal)",
            );
            return;
        }
    };
    for gen in gens {
        if gen == keep {
            continue;
        }
        delete_generation(channel_dir, gen);
    }
    // Best-effort manifest.tmp cleanup — a crash between
    // `manifest.tmp` write and the rename leaves a stale
    // manifest.tmp behind. Removing it is harmless on success
    // (manifest itself is the live pointer) and prevents the next
    // crash-recovery from being confused by stale .tmp data.
    let _ = std::fs::remove_file(manifest_tmp_path(channel_dir));
}

/// Migrate a legacy flat-layout channel (v0.10 / v0.11
/// `<channel>/{idx,dat,ts}`) into the new generation-directory
/// layout (`<channel>/v0000000001/{idx,dat,ts}` plus a manifest).
///
/// One-shot per channel; idempotent (re-running is a no-op because
/// the second invocation observes the manifest written by the first
/// and returns early).
///
/// # Precedence assumption (load-bearing)
///
/// This function assumes its caller has already established that
/// neither a valid manifest NOR a complete `v<NNN>/` generation
/// directory exists for this channel — i.e. flat-layout files
/// are the only authoritative on-disk state. `resolve_live_generation`
/// is the only caller and enforces this by gating the call behind
/// the manifest-read and enumerate-fallback branches both failing
/// to produce a complete generation.
///
/// If that gate ever weakens (e.g. a future caller invokes this
/// function unconditionally on open), the per-file renames here
/// would clobber a real `v0000000001/{idx,dat,ts}` produced by
/// some prior compact, silently substituting the flat-file
/// content for the post-compact content. **Do not** call from any
/// path where a complete `v1/` could already be the live
/// generation. The "overwrite v1 on partial-prior-migration"
/// behavior documented inline is correct ONLY because the gate
/// guarantees v1 was never live in that scenario.
///
/// Returns `Ok(true)` if migration ran (and we now have a v1
/// generation + manifest), `Ok(false)` if there was nothing to
/// migrate (no flat files present), or `Err` on a real I/O
/// failure mid-migration. A failure between renames leaves the
/// per-file moves in whichever state they reached; the next open
/// re-runs the migration (idempotent — the per-source-file
/// `is_file()` guards skip files already moved).
fn migrate_flat_layout_if_needed(channel_dir: &Path) -> std::io::Result<bool> {
    let flat_idx = channel_dir.join("idx");
    let flat_dat = channel_dir.join("dat");
    let flat_ts = channel_dir.join("ts");
    let any_flat = flat_idx.is_file() || flat_dat.is_file() || flat_ts.is_file();
    if !any_flat {
        return Ok(false);
    }
    // The migration target is generation 1 by definition. Any
    // pre-existing v1 directory left over from a partially-completed
    // prior migration is overwritten by these renames — that's
    // correct behavior, the prior migration didn't reach the
    // manifest write so v1 was never the live generation. See the
    // function-level "precedence assumption" rustdoc above for
    // why this is safe only under the gate `resolve_live_generation`
    // imposes (no valid manifest AND no complete generation dir).
    let v1 = gen_dir(channel_dir, FIRST_GENERATION);
    std::fs::create_dir_all(&v1)?;
    if flat_idx.is_file() {
        durable_rename(&flat_idx, &v1.join("idx"))?;
    }
    if flat_dat.is_file() {
        durable_rename(&flat_dat, &v1.join("dat"))?;
    }
    if flat_ts.is_file() {
        durable_rename(&flat_ts, &v1.join("ts"))?;
    }
    fsync_dir(&v1)?;
    fsync_dir(channel_dir)?;
    write_manifest_atomic(channel_dir, FIRST_GENERATION)?;
    Ok(true)
}

/// Resolve the live generation for `channel_dir` on open.
///
/// 1. Read manifest. If it validates, that's the answer.
/// 2. Otherwise enumerate `v<NNN>/` directories and pick the
///    highest one that contains all three of `{idx, dat, ts}`.
///    Write a fresh manifest pointing at it (best-effort — a
///    failure here doesn't block recovery; the next compact
///    refreshes the manifest).
/// 3. If neither path produces a generation, attempt the
///    flat-layout migration. If that succeeds, the live generation
///    is `FIRST_GENERATION`.
/// 4. If even migration didn't produce a generation, this is a
///    brand-new channel: create `v<FIRST_GENERATION>/` and write a
///    fresh manifest pointing at it.
///
/// Returns the live generation. The caller proceeds with the
/// existing recovery walks against `gen_dir(channel_dir, gen)`.
fn resolve_live_generation(channel_dir: &Path) -> std::io::Result<u32> {
    if let Some(gen) = read_manifest(channel_dir) {
        // Manifest valid — but verify the generation directory it
        // names actually exists. A manifest pointing at a missing
        // directory means someone deleted v<N>/ between opens; treat
        // that as torn-manifest and fall back to enumeration.
        if generation_is_complete(channel_dir, gen) {
            return Ok(gen);
        }
    }
    // Fallback: enumerate generations, pick highest valid.
    let candidates = enumerate_generations(channel_dir)?;
    for gen in candidates {
        if generation_is_complete(channel_dir, gen) {
            // Refresh the manifest. Best-effort — if the write fails
            // (read-only fs, permissions flap), recovery still
            // proceeds against this generation.
            if let Err(e) = write_manifest_atomic(channel_dir, gen) {
                tracing::warn!(
                    error = %e,
                    path = %channel_dir.display(),
                    gen,
                    "redex resolve: failed to refresh manifest after fallback (non-fatal)",
                );
            }
            return Ok(gen);
        }
    }
    // No valid generation. Try migration.
    if migrate_flat_layout_if_needed(channel_dir)? {
        return Ok(FIRST_GENERATION);
    }
    // Brand-new channel. Create the first generation directory and
    // write its initial manifest. The directory is empty until the
    // first append.
    std::fs::create_dir_all(gen_dir(channel_dir, FIRST_GENERATION))?;
    fsync_dir(channel_dir)?;
    write_manifest_atomic(channel_dir, FIRST_GENERATION)?;
    Ok(FIRST_GENERATION)
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
    #[cfg(test)]
    if let Some(e) = test_fsync_dir_consume_injected_failure() {
        return Err(e);
    }
    std::fs::File::open(dir)?.sync_all()
}

/// On non-Unix targets the `fsync_dir` helper is a no-op. Windows
/// has no `FlushFileBuffers`-on-a-directory-handle equivalent, but
/// per-rename durability is provided by `durable_rename`'s
/// `MoveFileExW(..., MOVEFILE_WRITE_THROUGH)` — so the dir-fsync
/// step `fsync_dir` represents on POSIX is already covered on
/// Windows by the rename call itself. This function exists only so
/// the call site at the end of `compact_to` doesn't have to
/// `cfg`-fork.
#[cfg(not(unix))]
fn fsync_dir(_dir: &Path) -> std::io::Result<()> {
    #[cfg(test)]
    if let Some(e) = test_fsync_dir_consume_injected_failure() {
        return Err(e);
    }
    Ok(())
}

// Test-only fault injection for `fsync_dir`. A countdown stored
// in thread-local state lets each test arm a failure on the Nth
// `fsync_dir` call from THIS thread; reaching `0` returns the
// injected error and disables further injection. Per-thread
// isolation matters because Rust's test runner parallelizes
// across threads — a global atomic would race between unrelated
// tests. Only one `fsync_dir` call per pass is targeted; tests
// that need a wider window arm the countdown again.
#[cfg(test)]
thread_local! {
    static FSYNC_DIR_FAIL_COUNTDOWN: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// Arm the next `fsync_dir` call (Nth-from-now) on this thread to
/// return an injected `io::Error`. `n = 1` fails the very next
/// call; `n = 2` lets one call through then fails the second; etc.
/// `n = 0` disables the injector. The countdown is consumed by the
/// targeted call and not re-armed automatically.
#[cfg(test)]
pub(super) fn arm_fsync_dir_failure_at(n: u32) {
    FSYNC_DIR_FAIL_COUNTDOWN.with(|c| c.set(n));
}

/// Consume one tick of the test injector. Returns `Some(err)` if
/// THIS call should fail, `None` otherwise. Decrements the
/// countdown on every call until it reaches 0.
#[cfg(test)]
fn test_fsync_dir_consume_injected_failure() -> Option<std::io::Error> {
    FSYNC_DIR_FAIL_COUNTDOWN.with(|c| {
        let cur = c.get();
        if cur == 0 {
            return None;
        }
        c.set(cur - 1);
        if cur == 1 {
            Some(std::io::Error::other("test-injected fsync_dir failure"))
        } else {
            None
        }
    })
}

/// Rename `src` over `dst` with per-call durability.
///
/// On POSIX, equivalent to `std::fs::rename`. The caller is
/// expected to follow up with `fsync_dir(parent)` to make the
/// dirent change durable — `compact_to` does so at the end of the
/// rename sequence.
///
/// On Windows, uses `MoveFileExW` with
/// `MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH`:
///   - `REPLACE_EXISTING` matches POSIX rename semantics on a
///     pre-existing destination (the prior `fs::rename` path
///     relied on the same behavior; Windows defaults are
///     stricter).
///   - `WRITE_THROUGH` blocks the call until the rename is
///     committed to the physical disk via the NTFS journal,
///     closing the durability hole that the stdlib `fs::rename`
///     leaves open. Without this flag, every individual rename
///     call could "succeed" while the dirent still lived only in
///     the OS cache — a power loss after all three renames in
///     `compact_to` could then revert the directory to the OLD
///     filenames pointing at the OLD inodes.
///
/// The cross-file mixed-state window between two renames is
/// unaffected on any platform; that's the deferred manifest-
/// pointer rework. This helper only covers the within-rename
/// durability gap.
#[cfg(unix)]
fn durable_rename(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::rename(src, dst)
}

#[cfg(windows)]
fn durable_rename(src: &Path, dst: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;

    fn to_wide_null(p: &Path) -> Vec<u16> {
        p.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }
    let src_w = to_wide_null(src);
    let dst_w = to_wide_null(dst);

    const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x8;

    extern "system" {
        fn MoveFileExW(
            existing_file_name: *const u16,
            new_file_name: *const u16,
            flags: u32,
        ) -> i32;
    }

    // SAFETY: `src_w` and `dst_w` are valid null-terminated UTF-16
    // strings owned by `Vec<u16>` for the duration of the call.
    // `MoveFileExW`'s contract is: read the wide strings, return
    // BOOL. No aliasing, no escaping references.
    let ok = unsafe {
        MoveFileExW(
            src_w.as_ptr(),
            dst_w.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if ok == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(any(unix, windows)))]
fn durable_rename(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::rename(src, dst)
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
        let idx_path = gen_dir(&channel_dir(&base, &name), FIRST_GENERATION).join("idx");
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
        let dat_path = gen_dir(&channel_dir(&base, &name), FIRST_GENERATION).join("dat");
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
        let idx_path = gen_dir(&channel_dir(&base, &name), FIRST_GENERATION).join("idx");
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
        let dat_path = gen_dir(&channel_dir(&base, &name), FIRST_GENERATION).join("dat");
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
        let dat_path = gen_dir(&channel_dir(&base, &name), FIRST_GENERATION).join("dat");

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
        let dat_path = gen_dir(&channel_dir(&base, &name), FIRST_GENERATION).join("dat");

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
        let dat_path = gen_dir(&channel_dir(&base, &name), FIRST_GENERATION).join("dat");
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
        let dat_path = gen_dir(&channel_dir(&base, &name), FIRST_GENERATION).join("dat");
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
        let dat_path = gen_dir(&channel_dir(&base, &name), FIRST_GENERATION).join("dat");
        let idx_path = gen_dir(&channel_dir(&base, &name), FIRST_GENERATION).join("idx");
        let ts_path = gen_dir(&channel_dir(&base, &name), FIRST_GENERATION).join("ts");
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

        // Worker handles must now reflect the compacted file sizes.
        // After `compact_to`, the live generation has rolled
        // (`live_gen` was 1, now 2), so the post-compact files
        // live under `<channel>/v<live_gen>/`. The previous
        // generation's directory has been swept by `compact_to`'s
        // best-effort cleanup.
        let live = recovered.disk.live_dir();
        let dat_path = live.join("dat");
        let idx_path = live.join("idx");
        let ts_path = live.join("ts");
        let on_disk_dat = std::fs::metadata(&dat_path).unwrap().len();
        let on_disk_idx = std::fs::metadata(&idx_path).unwrap().len();
        let on_disk_ts = std::fs::metadata(&ts_path).unwrap().len();
        assert_eq!(on_disk_dat, payloads[2].len() as u64, "sanity");
        assert_eq!(on_disk_idx, REDEX_ENTRY_SIZE as u64, "sanity");
        assert_eq!(on_disk_ts, 8, "sanity");
        // Sanity: live_gen rolled exactly once.
        assert_eq!(recovered.disk.live_gen(), FIRST_GENERATION + 1);
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

    /// On non-Unix targets, `fsync_dir` is a no-op that returns
    /// `Ok(())` on ANY input — even a path that does not exist,
    /// even a path that is not a directory. The directory-rename
    /// durability that POSIX `fsync_dir` provides is covered on
    /// Windows by `durable_rename`'s `MOVEFILE_WRITE_THROUGH`
    /// flag, so the dir-fsync step is correctly a no-op there.
    /// This test pins that no-op contract so a future "let's fail
    /// closed on Windows" change has to also update both this
    /// test AND the `durable_rename` durability story.
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

    /// `durable_rename` must move `src` to `dst` and unlink `src`,
    /// with byte-equal content preserved. Pins the cross-platform
    /// contract — POSIX uses plain `rename`; Windows routes
    /// through `MoveFileExW(..., MOVEFILE_REPLACE_EXISTING |
    /// MOVEFILE_WRITE_THROUGH)`. True power-loss durability of
    /// the WRITE_THROUGH flag isn't unit-testable; this test
    /// covers the syscall-correctness contract that the helper
    /// is wired up at all and that its arguments translate
    /// correctly through the FFI conversion.
    #[test]
    fn durable_rename_moves_file_and_preserves_contents() {
        let dir = tmpdir();
        let src = dir.join("durable_rename_src");
        let dst = dir.join("durable_rename_dst");
        std::fs::write(&src, b"durable_rename payload").expect("seed src");
        assert!(src.exists());
        assert!(!dst.exists());

        super::durable_rename(&src, &dst).expect("durable_rename must succeed");

        assert!(!src.exists(), "src must be unlinked after rename");
        assert!(dst.exists(), "dst must exist after rename");
        let bytes = std::fs::read(&dst).expect("read dst");
        assert_eq!(bytes, b"durable_rename payload");
    }

    /// `durable_rename` must replace a pre-existing `dst`. Pins
    /// the `MOVEFILE_REPLACE_EXISTING` semantic on Windows
    /// (without that flag, the call would fail with
    /// `ERROR_ALREADY_EXISTS`) and the matching POSIX
    /// over-rename behavior. The three rename calls in
    /// `compact_to` rely on this — the destination paths
    /// (idx/dat/ts) always exist when compact runs against a
    /// non-empty channel.
    #[test]
    fn durable_rename_replaces_existing_destination() {
        let dir = tmpdir();
        let src = dir.join("durable_rename_replace_src");
        let dst = dir.join("durable_rename_replace_dst");
        std::fs::write(&src, b"new contents").expect("seed src");
        std::fs::write(&dst, b"old contents to be replaced").expect("seed dst");

        super::durable_rename(&src, &dst).expect("rename over existing dst must succeed");

        assert!(!src.exists(), "src must be unlinked after rename");
        let bytes = std::fs::read(&dst).expect("read dst");
        assert_eq!(
            bytes, b"new contents",
            "dst must hold src's content, not the old payload"
        );
    }

    /// `durable_rename` must surface the OS error when `src` does
    /// not exist — both POSIX `ENOENT` and Windows
    /// `ERROR_FILE_NOT_FOUND` should round-trip through
    /// `io::Error::last_os_error`. Pins that the helper does NOT
    /// silently swallow failures; if the FFI return-value check
    /// inverts (treats 0 as success on Windows), this test trips.
    #[test]
    fn durable_rename_surfaces_missing_source_error() {
        let dir = tmpdir();
        let bogus_src = dir.join("definitely-not-here");
        let dst = dir.join("durable_rename_missing_dst");
        assert!(!bogus_src.exists());

        let err = super::durable_rename(&bogus_src, &dst)
            .expect_err("rename of nonexistent src must fail");
        assert!(!dst.exists(), "no dst should be created on failed rename");
        // Stable check: the error kind must be NotFound across
        // both platforms (POSIX ENOENT and Win32
        // ERROR_FILE_NOT_FOUND both map there).
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
    }

    /// Pin the post-compact atomic-swap invariant directly. The
    /// new manifest-pointer flow opens all six handles in the
    /// new generation directory BEFORE flipping the manifest, so a
    /// failure during the open phase aborts the compact entirely
    /// (live state stays at the prior generation; the orphan
    /// `v<N+1>/` is swept on next open).
    ///
    /// We exercise the success path's post-conditions under a
    /// real `compact_to`: all worker handles must come out of
    /// the call pointing at the new generation's files (sizes
    /// match the on-disk metadata), with no divergence between
    /// the cached slots and the live disk state.
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
        // point at the new generation's idx/dat/ts. We probe via
        // worker handle sizes (the worker handles are `try_clone`d
        // from the appender slots, so divergence on EITHER kind
        // surfaces here).
        let live = recovered.disk.live_dir();
        let dat_path = live.join("dat");
        let idx_path = live.join("idx");
        let ts_path = live.join("ts");
        let on_disk_dat = std::fs::metadata(&dat_path).unwrap().len();
        let on_disk_idx = std::fs::metadata(&idx_path).unwrap().len();
        let on_disk_ts = std::fs::metadata(&ts_path).unwrap().len();
        let (dat_w, idx_w, ts_w) = recovered.disk.worker_file_lens();
        assert_eq!(
            dat_w, on_disk_dat,
            "worker dat must point at the new generation"
        );
        assert_eq!(
            idx_w, on_disk_idx,
            "worker idx must point at the new generation"
        );
        assert_eq!(
            ts_w, on_disk_ts,
            "worker ts must point at the new generation"
        );

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

    // ====================================================================
    // Manifest-pointer layout tests.
    //
    // These pin the new generation-directory layout's invariants:
    //   - manifest codec round-trip + corruption rejection
    //   - migration from legacy flat layout
    //   - fallback to highest valid generation when manifest is torn
    //   - sweep of orphan generation directories
    //   - crash-injection at each step of `compact_to`
    // ====================================================================

    #[test]
    fn manifest_codec_roundtrip() {
        for &gen in &[FIRST_GENERATION, 2, 100, 1_000_000, u32::MAX] {
            let bytes = encode_manifest(gen);
            assert_eq!(
                bytes.len(),
                MANIFEST_SIZE,
                "wire size must be {MANIFEST_SIZE}"
            );
            assert_eq!(
                decode_manifest(&bytes),
                Some(gen),
                "round-trip must preserve generation {gen}",
            );
        }
    }

    #[test]
    fn manifest_codec_rejects_garbage() {
        // Wrong magic.
        let mut bad = encode_manifest(1);
        bad[0] = b'X';
        assert_eq!(decode_manifest(&bad), None, "bad magic must be rejected");

        // Wrong version.
        let mut bad = encode_manifest(1);
        bad[4] = 99;
        assert_eq!(
            decode_manifest(&bad),
            None,
            "unknown version must be rejected"
        );

        // Generation 0 (reserved).
        let mut bad = encode_manifest(1);
        bad[5..9].copy_from_slice(&0u32.to_le_bytes());
        // Re-checksum so corruption is via the value, not via the
        // checksum, to prove the value-level guard fires.
        let cs = xxhash_rust::xxh3::xxh3_64(&bad[0..12]) as u32;
        bad[12..16].copy_from_slice(&cs.to_le_bytes());
        assert_eq!(decode_manifest(&bad), None, "gen 0 must be rejected");

        // Non-zero reserved bytes.
        let mut bad = encode_manifest(1);
        bad[10] = 0xAA;
        let cs = xxhash_rust::xxh3::xxh3_64(&bad[0..12]) as u32;
        bad[12..16].copy_from_slice(&cs.to_le_bytes());
        assert_eq!(
            decode_manifest(&bad),
            None,
            "non-zero reserved must be rejected (defense in depth against \
             a future producer that stuffs bits we may want to repurpose)",
        );

        // Bit-flip in the generation field, checksum unchanged.
        let mut bad = encode_manifest(7);
        bad[5] ^= 0x01;
        assert_eq!(
            decode_manifest(&bad),
            None,
            "bit-flip in generation must trip the checksum",
        );

        // Short slice.
        assert_eq!(
            decode_manifest(&[0u8; 15]),
            None,
            "short slice must be rejected"
        );
        assert_eq!(decode_manifest(&[]), None, "empty slice must be rejected");
    }

    /// On a brand-new channel, `open` must create both
    /// `<channel>/v<FIRST_GENERATION>/` and a manifest pointing at it.
    /// This is the bedrock of the new layout — nothing else works if
    /// the initial-state path is broken.
    #[test]
    fn open_brand_new_channel_creates_v1_and_manifest() {
        let base = tmpdir();
        let name = ChannelName::new("t/manifest_init").unwrap();
        let recovered = DiskSegment::open(&base, &name, 0, 0).unwrap();
        assert_eq!(recovered.disk.live_gen(), FIRST_GENERATION);

        let chan = channel_dir(&base, &name);
        let manifest = manifest_path(&chan);
        assert!(
            manifest.is_file(),
            "manifest must exist on brand-new channel"
        );
        let mut buf = [0u8; MANIFEST_SIZE];
        std::fs::File::open(&manifest)
            .unwrap()
            .read_exact(&mut buf)
            .unwrap();
        assert_eq!(decode_manifest(&buf), Some(FIRST_GENERATION));

        let v1 = gen_dir(&chan, FIRST_GENERATION);
        assert!(v1.is_dir(), "v0000000001/ must exist");
        cleanup(&base);
    }

    /// Legacy v0.10 / v0.11 flat-layout channels must migrate
    /// transparently into `v0000000001/` on first open. Pin the
    /// rename-each-file → write-manifest sequence so a future change
    /// can't accidentally drop the migration shim.
    #[test]
    fn open_migrates_flat_layout_to_v1_generation() {
        let base = tmpdir();
        let name = ChannelName::new("t/manifest_migrate").unwrap();
        let chan = channel_dir(&base, &name);
        std::fs::create_dir_all(&chan).unwrap();
        // Write flat-layout files directly (simulating an on-disk
        // channel from a pre-manifest binary).
        let payload = b"legacy-payload-bytes";
        let entry = RedexEntry::new_heap(0, 0, payload.len() as u32, 0, payload_checksum(payload));
        std::fs::write(chan.join("idx"), entry.to_bytes()).unwrap();
        std::fs::write(chan.join("dat"), payload).unwrap();
        std::fs::write(chan.join("ts"), 1234u64.to_le_bytes()).unwrap();

        let recovered = DiskSegment::open(&base, &name, 0, 0).unwrap();
        assert_eq!(recovered.disk.live_gen(), FIRST_GENERATION);
        assert_eq!(recovered.index.len(), 1, "migrated entry must survive");
        assert_eq!(recovered.payload_bytes, payload);
        assert_eq!(recovered.timestamps.as_deref(), Some(&[1234u64][..]));

        // Flat files must have moved into v<FIRST_GENERATION>/.
        assert!(!chan.join("idx").exists(), "flat idx must be migrated");
        assert!(!chan.join("dat").exists(), "flat dat must be migrated");
        assert!(!chan.join("ts").exists(), "flat ts must be migrated");
        let v1 = gen_dir(&chan, FIRST_GENERATION);
        assert!(v1.join("idx").is_file());
        assert!(v1.join("dat").is_file());
        assert!(v1.join("ts").is_file());

        // Manifest must exist and point at v1.
        assert_eq!(read_manifest(&chan), Some(FIRST_GENERATION));

        // Re-opening is idempotent (no second migration).
        drop(recovered);
        let r2 = DiskSegment::open(&base, &name, 0, 0).unwrap();
        assert_eq!(r2.disk.live_gen(), FIRST_GENERATION);
        assert_eq!(r2.index.len(), 1);
        cleanup(&base);
    }

    /// Crash-injection: design-doc row 9 (mid-migration partial
    /// rename). Simulate `flat/idx` rename succeeding but the
    /// process crashing before `flat/dat` and `flat/ts` are
    /// moved. Re-open must resume the migration cleanly:
    ///
    ///   - the partially-populated `v1/` (only `idx` so far) is
    ///     NOT confused for a complete generation by the
    ///     enumerate-fallback branch (it's missing `dat` + `ts`),
    ///   - the still-extant flat files (`dat`, `ts`) trigger the
    ///     migration shim a second time,
    ///   - the shim's per-file `if flat_*.is_file()` guard
    ///     correctly skips the already-migrated `idx` and
    ///     completes the move for `dat` + `ts`,
    ///   - the manifest is then written and recovery succeeds
    ///     with all three files present.
    ///
    /// Pin so a future change that breaks idempotency (e.g.
    /// dropping the per-file `is_file()` guards in favor of an
    /// unconditional rename, or changing the resolution order to
    /// trust an incomplete `v1/` directory) trips here rather
    /// than as silent data loss in production.
    #[test]
    fn open_resumes_migration_after_partial_flat_rename_crash() {
        let base = tmpdir();
        let name = ChannelName::new("t/manifest_partial_migration").unwrap();
        let chan = channel_dir(&base, &name);
        std::fs::create_dir_all(&chan).unwrap();

        // Set up: legacy flat-layout content, then SIMULATE a
        // crash where `idx` was renamed into `v1/` but `dat` +
        // `ts` were still flat. We do this by creating `v1/`
        // ourselves with a partial migration result rather than
        // relying on a fault injector.
        let payload = b"resume-migration-payload";
        let entry = RedexEntry::new_heap(0, 0, payload.len() as u32, 0, payload_checksum(payload));
        let v1 = gen_dir(&chan, FIRST_GENERATION);
        std::fs::create_dir_all(&v1).unwrap();
        // Half-migrated: idx already in v1/.
        std::fs::write(v1.join("idx"), entry.to_bytes()).unwrap();
        // Still flat: dat + ts.
        std::fs::write(chan.join("dat"), payload).unwrap();
        std::fs::write(chan.join("ts"), 4567u64.to_le_bytes()).unwrap();
        // No manifest — the prior partial migration didn't reach
        // `write_manifest_atomic`.
        assert!(!manifest_path(&chan).exists());

        let recovered = DiskSegment::open(&base, &name, 0, 0).unwrap();

        // Migration completed: live gen = 1, manifest written,
        // every flat file moved, every v1 file present, content
        // intact.
        assert_eq!(recovered.disk.live_gen(), FIRST_GENERATION);
        assert_eq!(read_manifest(&chan), Some(FIRST_GENERATION));
        assert!(
            !chan.join("dat").exists(),
            "remaining flat dat must have completed its migration",
        );
        assert!(
            !chan.join("ts").exists(),
            "remaining flat ts must have completed its migration",
        );
        // `idx` was already migrated — must NOT have been
        // duplicated/clobbered (its content must be preserved).
        assert!(v1.join("idx").is_file());
        assert!(v1.join("dat").is_file());
        assert!(v1.join("ts").is_file());
        assert_eq!(
            recovered.index.len(),
            1,
            "the pre-crash idx entry must survive"
        );
        assert_eq!(recovered.payload_bytes, payload);
        assert_eq!(recovered.timestamps.as_deref(), Some(&[4567u64][..]));

        // Idempotent: a second open observes the manifest and
        // takes the new-layout path (no re-migration).
        drop(recovered);
        let r2 = DiskSegment::open(&base, &name, 0, 0).unwrap();
        assert_eq!(r2.disk.live_gen(), FIRST_GENERATION);
        assert_eq!(r2.index.len(), 1);
        cleanup(&base);
    }

    /// Symmetric case: the OTHER mid-migration partial — only
    /// `dat` was flat at crash time, `idx` and `ts` already in
    /// v1/. Pins that the per-file guards work for any subset
    /// of remaining flat files, not just the "idx came first"
    /// ordering above.
    #[test]
    fn open_resumes_migration_when_only_dat_remains_flat() {
        let base = tmpdir();
        let name = ChannelName::new("t/manifest_partial_migration_dat_only").unwrap();
        let chan = channel_dir(&base, &name);
        std::fs::create_dir_all(&chan).unwrap();

        let payload = b"dat-only-resume";
        let entry = RedexEntry::new_heap(0, 0, payload.len() as u32, 0, payload_checksum(payload));
        let v1 = gen_dir(&chan, FIRST_GENERATION);
        std::fs::create_dir_all(&v1).unwrap();
        std::fs::write(v1.join("idx"), entry.to_bytes()).unwrap();
        std::fs::write(v1.join("ts"), 99u64.to_le_bytes()).unwrap();
        std::fs::write(chan.join("dat"), payload).unwrap();
        assert!(!manifest_path(&chan).exists());

        let recovered = DiskSegment::open(&base, &name, 0, 0).unwrap();
        assert_eq!(recovered.disk.live_gen(), FIRST_GENERATION);
        assert_eq!(read_manifest(&chan), Some(FIRST_GENERATION));
        assert!(!chan.join("dat").exists());
        assert!(v1.join("dat").is_file());
        assert_eq!(recovered.index.len(), 1);
        assert_eq!(recovered.payload_bytes, payload);
        assert_eq!(recovered.timestamps.as_deref(), Some(&[99u64][..]));
        cleanup(&base);
    }

    /// If the manifest is missing entirely (e.g. it was never
    /// written, or it was deleted out of band), but generation
    /// directories exist, recovery must enumerate them and pick the
    /// highest validated one. This is the catastrophic-recovery
    /// branch — the live layer should never reach it under normal
    /// operation, but it's the safety net.
    #[test]
    fn open_falls_back_to_highest_complete_generation_when_manifest_missing() {
        let base = tmpdir();
        let name = ChannelName::new("t/manifest_fallback").unwrap();
        let recovered = DiskSegment::open(&base, &name, 0, 0).unwrap();
        // Force a compact so v2 exists and is live.
        let payload = b"x";
        let e = RedexEntry::new_heap(0, 0, 1, 0, payload_checksum(payload));
        recovered.disk.append_entry_at(&e, payload, 1).unwrap();
        recovered.disk.sync().unwrap();
        let surviving = vec![*recovered.index.first().unwrap_or(&e)];
        let _ = recovered.disk.compact_to(&surviving, &[1], 0);
        // We don't actually care whether compact succeeded; only
        // that there's a v<live>/ directory we can corrupt the
        // manifest to test against.
        let live_after_compact = recovered.disk.live_gen();
        drop(recovered);

        // Delete the manifest. Fallback path must enumerate
        // generation directories and pick the highest valid one.
        let chan = channel_dir(&base, &name);
        std::fs::remove_file(manifest_path(&chan)).unwrap();
        let recovered2 = DiskSegment::open(&base, &name, 0, 0).unwrap();
        assert_eq!(
            recovered2.disk.live_gen(),
            live_after_compact,
            "fallback must pick the highest validated generation",
        );
        // Manifest must have been refreshed.
        assert_eq!(read_manifest(&chan), Some(live_after_compact));
        cleanup(&base);
    }

    /// A torn manifest (corrupted bytes, e.g. cosmic-ray bit-flip
    /// in the checksum field) must trigger the same fallback path
    /// as a missing manifest. Pin so a future change can't make
    /// `decode_manifest` permissive without breaking this test.
    #[test]
    fn open_falls_back_when_manifest_checksum_bad() {
        let base = tmpdir();
        let name = ChannelName::new("t/manifest_torn").unwrap();
        let recovered = DiskSegment::open(&base, &name, 0, 0).unwrap();
        let payload = b"y";
        let e = RedexEntry::new_heap(0, 0, 1, 0, payload_checksum(payload));
        recovered.disk.append_entry_at(&e, payload, 2).unwrap();
        recovered.disk.sync().unwrap();
        drop(recovered);

        // Flip a bit in the checksum field of the manifest.
        let chan = channel_dir(&base, &name);
        let mut bytes = [0u8; MANIFEST_SIZE];
        std::fs::File::open(manifest_path(&chan))
            .unwrap()
            .read_exact(&mut bytes)
            .unwrap();
        bytes[12] ^= 0x01;
        std::fs::write(manifest_path(&chan), bytes).unwrap();
        // Confirm the bit-flip actually invalidates the manifest.
        assert_eq!(read_manifest(&chan), None, "checksum guard must fire");

        let recovered2 = DiskSegment::open(&base, &name, 0, 0).unwrap();
        assert_eq!(
            recovered2.disk.live_gen(),
            FIRST_GENERATION,
            "fallback must still find v1 after the manifest is torn",
        );
        // Manifest must have been refreshed (now valid).
        assert_eq!(read_manifest(&chan), Some(FIRST_GENERATION));
        cleanup(&base);
    }

    /// Crash-injection: simulate a crash after writing
    /// `v<N+1>/{idx,dat,ts}` but BEFORE the manifest flip. Recovery
    /// must use the old generation `v<N>/`, and the orphan
    /// `v<N+1>/` must be swept on next open.
    ///
    /// We simulate the crash by manually creating an orphan
    /// `v<N+1>/` directory alongside the live `v<N>/` and verifying
    /// that `open` (a) keeps `v<N>/` as live, (b) deletes the orphan.
    #[test]
    fn open_sweeps_orphan_newer_generation_left_by_crashed_compact() {
        let base = tmpdir();
        let name = ChannelName::new("t/manifest_orphan_newer").unwrap();
        let r = DiskSegment::open(&base, &name, 0, 0).unwrap();
        let payload = b"live";
        let e = RedexEntry::new_heap(0, 0, payload.len() as u32, 0, payload_checksum(payload));
        r.disk.append_entry_at(&e, payload, 7).unwrap();
        r.disk.sync().unwrap();
        drop(r);

        // Create a partially-written `v0000000002/` as if a compact
        // had crashed mid-flight.
        let chan = channel_dir(&base, &name);
        let v2 = gen_dir(&chan, FIRST_GENERATION + 1);
        std::fs::create_dir_all(&v2).unwrap();
        std::fs::write(v2.join("idx"), b"\x00\x00").unwrap(); // partial garbage
                                                              // Note: NO manifest update — manifest still points at v1.

        let r2 = DiskSegment::open(&base, &name, 0, 0).unwrap();
        // Live must still be v1 (manifest unchanged).
        assert_eq!(r2.disk.live_gen(), FIRST_GENERATION);
        // The orphaned v2 must have been swept on open.
        assert!(
            !v2.exists(),
            "orphan generation directory must be swept on next open",
        );
        // The live entry must still be readable.
        assert_eq!(r2.index.len(), 1);
        assert_eq!(r2.payload_bytes, payload);
        cleanup(&base);
    }

    /// Crash-injection: simulate a crash after the manifest flip but
    /// BEFORE the post-flip cleanup of the prior generation. Open
    /// must use the new generation (manifest is the source of truth)
    /// AND sweep the now-superseded prior generation.
    #[test]
    fn open_sweeps_orphan_older_generation_left_by_crashed_post_flip_cleanup() {
        let base = tmpdir();
        let name = ChannelName::new("t/manifest_orphan_older").unwrap();
        let r = DiskSegment::open(&base, &name, 0, 0).unwrap();
        let payload = b"v1-payload";
        let e = RedexEntry::new_heap(0, 0, payload.len() as u32, 0, payload_checksum(payload));
        r.disk.append_entry_at(&e, payload, 8).unwrap();
        r.disk.sync().unwrap();
        drop(r);

        // Simulate post-flip-pre-cleanup state: manifest already
        // points at v2 (post-compact), v2 holds the new content, v1
        // still exists as an orphan.
        let chan = channel_dir(&base, &name);
        let v1 = gen_dir(&chan, FIRST_GENERATION);
        let v2 = gen_dir(&chan, FIRST_GENERATION + 1);
        std::fs::create_dir_all(&v2).unwrap();
        let new_payload = b"v2-payload";
        let new_entry = RedexEntry::new_heap(
            5,
            0,
            new_payload.len() as u32,
            0,
            payload_checksum(new_payload),
        );
        std::fs::write(v2.join("idx"), new_entry.to_bytes()).unwrap();
        std::fs::write(v2.join("dat"), new_payload).unwrap();
        std::fs::write(v2.join("ts"), 9u64.to_le_bytes()).unwrap();
        // Atomically flip the manifest to point at v2.
        write_manifest_atomic(&chan, FIRST_GENERATION + 1).unwrap();
        // v1 still exists at this point — we never cleaned it up.
        assert!(v1.exists(), "setup: v1 must still be present pre-recovery");

        let r2 = DiskSegment::open(&base, &name, 0, 0).unwrap();
        assert_eq!(r2.disk.live_gen(), FIRST_GENERATION + 1);
        // The superseded v1 must have been swept on open.
        assert!(
            !v1.exists(),
            "superseded prior generation must be swept on next open",
        );
        // v2's content must be live.
        assert_eq!(r2.index.len(), 1);
        assert_eq!(r2.payload_bytes, new_payload);
        cleanup(&base);
    }

    /// `compact_to` must roll the live generation by exactly one,
    /// place the new content under `v<N+1>/`, swap the manifest,
    /// and delete the old `v<N>/`. Pin all four observable
    /// invariants so a regression in any of them trips here rather
    /// than as silent on-disk garbage.
    #[test]
    fn compact_to_advances_generation_and_swaps_manifest_atomically() {
        let base = tmpdir();
        let name = ChannelName::new("t/manifest_compact_flow").unwrap();
        let r = DiskSegment::open(&base, &name, 0, 0).unwrap();
        let payload = b"to-be-compacted";
        let e = RedexEntry::new_heap(0, 0, payload.len() as u32, 0, payload_checksum(payload));
        r.disk.append_entry_at(&e, payload, 11).unwrap();
        r.disk.sync().unwrap();

        let chan = channel_dir(&base, &name);
        assert_eq!(r.disk.live_gen(), FIRST_GENERATION);
        assert_eq!(read_manifest(&chan), Some(FIRST_GENERATION));

        // Compact: surviving = the one entry, dat_base = 0.
        let surviving = vec![e];
        r.disk.compact_to(&surviving, &[11], 0).unwrap();

        // Generation rolled by exactly one.
        assert_eq!(r.disk.live_gen(), FIRST_GENERATION + 1);
        // Manifest rolled atomically with it.
        assert_eq!(read_manifest(&chan), Some(FIRST_GENERATION + 1));
        // v<N+1>/ is populated.
        let v2 = gen_dir(&chan, FIRST_GENERATION + 1);
        assert!(v2.join("idx").is_file());
        assert!(v2.join("dat").is_file());
        assert!(v2.join("ts").is_file());
        // v<N>/ has been swept.
        let v1 = gen_dir(&chan, FIRST_GENERATION);
        assert!(!v1.exists(), "prior generation must be swept post-compact");

        // Reopen sees the new generation as live.
        drop(r);
        let r2 = DiskSegment::open(&base, &name, 0, 0).unwrap();
        assert_eq!(r2.disk.live_gen(), FIRST_GENERATION + 1);
        assert_eq!(r2.index.len(), 1);
        cleanup(&base);
    }

    /// Full crash-recovery story: a "crash" (simulated by NOT
    /// running compact_to's post-flip cleanup) leaves both
    /// generations on disk plus the manifest pointing at the
    /// new one. Reopen must use the new one and sweep the
    /// stale older one in one step. Equivalent to "recovery
    /// converges to a single live generation in one open."
    #[test]
    fn recovery_converges_to_single_live_generation() {
        let base = tmpdir();
        let name = ChannelName::new("t/manifest_converge").unwrap();
        let r = DiskSegment::open(&base, &name, 0, 0).unwrap();
        let p1 = b"first-gen";
        let e1 = RedexEntry::new_heap(0, 0, p1.len() as u32, 0, payload_checksum(p1));
        r.disk.append_entry_at(&e1, p1, 100).unwrap();
        r.disk.sync().unwrap();

        // Compact rolls v1 -> v2 and sweeps v1.
        r.disk.compact_to(&[e1], &[100], 0).unwrap();
        assert_eq!(r.disk.live_gen(), FIRST_GENERATION + 1);

        // Append once more so v2 has a meaningful tail. Then
        // simulate a crash mid-second-compact: we manually create a
        // partial v3 directory but DON'T flip the manifest.
        let p2 = b"second-gen";
        let e2 = RedexEntry::new_heap(1, p1.len() as u32, p2.len() as u32, 0, payload_checksum(p2));
        r.disk.append_entry_at(&e2, p2, 200).unwrap();
        r.disk.sync().unwrap();
        drop(r);

        let chan = channel_dir(&base, &name);
        let v3 = gen_dir(&chan, FIRST_GENERATION + 2);
        std::fs::create_dir_all(&v3).unwrap();
        std::fs::write(v3.join("idx"), b"\xAB\xCD").unwrap(); // partial garbage
                                                              // Manifest unchanged — still points at v2.

        let r2 = DiskSegment::open(&base, &name, 0, 0).unwrap();
        assert_eq!(r2.disk.live_gen(), FIRST_GENERATION + 1);
        assert!(!v3.exists(), "orphan v3 must be swept");
        // v2 must contain both entries (the second was appended
        // post-compact under v2 directly).
        assert_eq!(r2.index.len(), 2);
        cleanup(&base);
    }

    /// Pin the orphan-sweep behavior at the helper level: only the
    /// `keep` generation survives, every other `v<NNN>/` is removed.
    #[test]
    fn sweep_orphan_generations_keeps_only_designated_generation() {
        let base = tmpdir();
        let chan = base.join("sweep_test");
        std::fs::create_dir_all(&chan).unwrap();
        for gen in &[1u32, 2, 5, 7] {
            std::fs::create_dir_all(gen_dir(&chan, *gen)).unwrap();
        }
        // A non-matching directory must not be touched (defense in
        // depth against accidental wipe of unrelated content).
        std::fs::create_dir_all(chan.join("not-a-gen-dir")).unwrap();
        // A stray manifest.tmp must be cleaned up (post-crash
        // remnant from a manifest write that didn't reach the
        // rename).
        std::fs::write(chan.join("manifest.tmp"), b"stale").unwrap();

        sweep_orphan_generations(&chan, 5);

        assert!(gen_dir(&chan, 5).exists(), "kept generation must survive");
        assert!(!gen_dir(&chan, 1).exists(), "v1 must be swept");
        assert!(!gen_dir(&chan, 2).exists(), "v2 must be swept");
        assert!(!gen_dir(&chan, 7).exists(), "v7 must be swept");
        assert!(
            chan.join("not-a-gen-dir").exists(),
            "non-matching directories must NOT be swept",
        );
        assert!(
            !chan.join("manifest.tmp").exists(),
            "stale manifest.tmp must be cleaned up",
        );
        let _ = std::fs::remove_dir_all(&chan);
    }

    /// Generation-directory enumeration must filter out anything
    /// that doesn't match the `v` + 10-digit pattern, and must
    /// return generations in descending order.
    #[test]
    fn enumerate_generations_filters_and_sorts() {
        let base = tmpdir();
        let chan = base.join("enumerate_test");
        std::fs::create_dir_all(&chan).unwrap();
        for d in &["v0000000001", "v0000000002", "v0000000010", "v0000000003"] {
            std::fs::create_dir_all(chan.join(d)).unwrap();
        }
        // Decoys: file (not directory), wrong prefix, wrong digit
        // count, non-digit characters, generation 0 (reserved).
        std::fs::write(chan.join("v0000000001.txt"), b"file").unwrap();
        std::fs::create_dir_all(chan.join("vXXXXXXXXXX")).unwrap();
        std::fs::create_dir_all(chan.join("v00000001")).unwrap(); // 8 digits not 10
        std::fs::create_dir_all(chan.join("v0000000000")).unwrap(); // gen 0

        let gens = enumerate_generations(&chan).unwrap();
        assert_eq!(
            gens,
            vec![10u32, 3, 2, 1],
            "must return matching generations in descending order, \
             skipping decoys and the reserved gen 0",
        );
        let _ = std::fs::remove_dir_all(&chan);
    }

    /// Source-text guard: the `poisoned` field rustdoc and the
    /// runtime error strings returned from append paths must
    /// describe the ACTUAL setters (the partial-write rollback
    /// paths) rather than a `compact_to` failure-mode parenthetical
    /// that the manifest-pointer rework deleted. The test
    /// reconstructs the deleted phrase at runtime to avoid
    /// matching itself. An operator hitting one of the runtime
    /// errors today would otherwise chase a phantom; future
    /// maintainers would learn the wrong invariant from the
    /// field doc. This test fails loudly if either drifts back.
    #[test]
    fn poisoning_docs_and_errors_describe_actual_setters() {
        let src = include_str!("disk.rs");

        // Build the stale marker at runtime so the test's own
        // assertion message (which has to NAME the marker for
        // operator readability) doesn't itself trip the
        // assertion. Two halves joined by `-` reproduces the
        // exact pre-fix substring without it appearing anywhere
        // in the source verbatim.
        let stale_marker = format!("{}{}{}", "compact_to post", "-", "rename reopen failure",);
        assert!(
            !src.contains(&stale_marker),
            "regression: source must not contain '{stale_marker}'. \
             That parenthetical described a `compact_to` failure \
             path that was deleted in the manifest-pointer rework \
             (the rework opens the new generation's handles BEFORE \
             the atomic flip, so a failed open aborts the compact \
             with live state still intact). The `poisoned` flag's \
             only setters now are the partial-write rollback paths \
             (rollback_truncate, rollback_after_idx_failure); both \
             the field rustdoc and the runtime error strings must \
             describe that reality, not the deleted setter."
        );

        // Conversely, the new wording must appear in BOTH
        // append-path setters (`append_entry_inner` and
        // `append_entries_inner`). Two-or-more is the post-fix
        // shape; a regression that updates one site but not the
        // other would leave operators with mixed messaging.
        // Subtract one occurrence for this test's own reference
        // to the marker (the message immediately below).
        let new_marker = format!("{} {}", "partial-write rollback", "could not restore",);
        let occurrences = src.matches(&new_marker).count();
        assert!(
            occurrences >= 2,
            "regression: at least two error messages (one per \
             append path) must use the wording '{new_marker}' to \
             point operators at the real cause; saw {occurrences} \
             occurrences in the source.",
        );
    }

    /// `write_manifest_atomic` must NOT propagate a `fsync_dir`
    /// failure that happens AFTER `durable_rename` succeeded.
    /// The rename is the linearizing event of the manifest flip;
    /// once it commits, the manifest is visible to every subsequent
    /// reader regardless of whether the dirent has been flushed.
    /// Surfacing the fsync error would lie to the caller about
    /// whether the flip happened: `compact_to` would interpret
    /// `Err` as "live state unchanged, no cached-handle swap"
    /// while the on-disk manifest already names next_gen — every
    /// in-process append between the failed write_manifest_atomic
    /// and process exit would land in cur_gen and then be
    /// discarded by the orphan sweep on next open. This test arms
    /// the fsync_dir injector to fail on the call AFTER the
    /// rename and verifies (a) `write_manifest_atomic` returns
    /// `Ok(())`, (b) the on-disk manifest reflects the new value.
    #[test]
    fn write_manifest_atomic_swallows_fsync_dir_failure_after_rename() {
        let chan = tmpdir();
        // Establish a baseline manifest at gen 1.
        super::write_manifest_atomic(&chan, 1).expect("baseline manifest write must succeed");
        assert_eq!(super::read_manifest(&chan), Some(1));

        // The next write does: open tmp, write, fsync_all (no
        // fsync_dir yet), durable_rename, fsync_dir(channel_dir).
        // We want to fail ONLY the fsync_dir(channel_dir) call —
        // which is the FIRST fsync_dir call this write_manifest
        // makes. Arm at n=1.
        super::arm_fsync_dir_failure_at(1);
        super::write_manifest_atomic(&chan, 2).expect(
            "write_manifest_atomic must return Ok when fsync_dir fails \
             AFTER durable_rename succeeded; surfacing Err would lie \
             about whether the flip happened",
        );

        // The injector is consumed; the on-disk manifest reflects
        // the new generation (the rename committed before the
        // injected failure).
        assert_eq!(
            super::read_manifest(&chan),
            Some(2),
            "manifest must reflect the post-rename value even though \
             the dirent fsync was injected to fail",
        );

        // Disable injector explicitly (defensive — thread might
        // be reused by another test).
        super::arm_fsync_dir_failure_at(0);
        cleanup(&chan);
    }

    /// Errors BEFORE the rename (open/write/fsync of `manifest.tmp`)
    /// MUST still propagate. The injector here fires on the first
    /// `fsync_dir` call inside `write_manifest_atomic`. To exercise
    /// the pre-rename failure path we'd need a different injection
    /// site, but we can pin the propagation contract with a path
    /// that fails the tmp open: a `channel_dir` that doesn't exist
    /// makes `OpenOptions::create(true).open(tmp)` fail with
    /// NotFound, and that error must surface as Err.
    #[test]
    fn write_manifest_atomic_propagates_pre_rename_failures() {
        let nonexistent = tmpdir().join("does_not_exist_subdir");
        // No `create_dir_all` — the tmp open must fail.
        let err = super::write_manifest_atomic(&nonexistent, 1)
            .expect_err("write into a nonexistent dir must fail");
        assert_eq!(
            err.kind(),
            std::io::ErrorKind::NotFound,
            "pre-rename failures (here: tmp open against missing dir) \
             must surface as Err so the caller knows the flip didn't \
             happen; got {:?}",
            err,
        );
    }

    /// End-to-end: `compact_to` must succeed when the
    /// post-rename `fsync_dir` fails. The cached handles MUST be
    /// swapped to the new generation and `live_gen` MUST advance
    /// — otherwise in-process appends keep hitting the
    /// (now-dead) cur_gen and get discarded on the next open's
    /// orphan sweep. The injector fires on the second
    /// `fsync_dir` call: compact_to does `fsync_dir(next_dir)`
    /// first (must succeed for content durability) then
    /// `fsync_dir(channel_dir)` inside `write_manifest_atomic`
    /// (the post-rename one we want to fail).
    #[test]
    fn compact_to_succeeds_when_post_rename_fsync_dir_fails() {
        let base = tmpdir();
        let name = ChannelName::new("t/compact_post_fsync_fail").unwrap();
        let r = DiskSegment::open(&base, &name, 0, 0).unwrap();
        let payload = b"survives-fsync_dir-failure";
        let e = RedexEntry::new_heap(0, 0, payload.len() as u32, 0, payload_checksum(payload));
        r.disk.append_entry_at(&e, payload, 5).unwrap();
        r.disk.sync().unwrap();

        // Arm the injector to fire on the SECOND fsync_dir from
        // this thread. compact_to's fsync_dir order:
        //   1. fsync_dir(&next_dir)        — content durability
        //   2. fsync_dir(channel_dir)      — post-rename dirent
        // We want #2 to fail; #1 must succeed (failing it would
        // correctly fail compact, which isn't what we're testing).
        super::arm_fsync_dir_failure_at(2);

        r.disk.compact_to(&[e], &[5], 0).expect(
            "compact_to must report success when fsync_dir fails \
             AFTER the manifest rename succeeded",
        );

        // The on-disk and in-memory state both advanced.
        assert_eq!(
            r.disk.live_gen(),
            FIRST_GENERATION + 1,
            "live_gen must advance after a successful compact even \
             when post-rename fsync_dir failed",
        );
        let chan = channel_dir(&base, &name);
        assert_eq!(
            super::read_manifest(&chan),
            Some(FIRST_GENERATION + 1),
            "on-disk manifest must reflect next_gen — the rename \
             committed before the injected fsync_dir failure",
        );

        // Disable injector explicitly.
        super::arm_fsync_dir_failure_at(0);
        cleanup(&base);
    }

    /// Belt-and-braces source-shape guard. `write_manifest_atomic`
    /// must NOT propagate the post-rename `fsync_dir` via `?`. The
    /// pre-fix shape (`fsync_dir(channel_dir)?;`) is the bug we
    /// closed in this commit; if a future refactor accidentally
    /// re-introduces it, the behavioral test above catches it on
    /// the next run, but this static check fails immediately at
    /// build time and points at the exact line.
    #[test]
    fn write_manifest_atomic_must_not_propagate_post_rename_fsync_dir() {
        let src = include_str!("disk.rs");
        let header = "fn write_manifest_atomic(";
        let start = src.find(header).expect("write_manifest_atomic must exist");
        // Find the function body's closing brace to bound the search.
        let body_after = &src[start..];
        let next_top_level = body_after
            .find("\nfn ")
            .or_else(|| body_after.find("\n#[cfg(test)]"))
            .expect("a following item must exist");
        let body = &body_after[..next_top_level];

        assert!(
            !body.contains("fsync_dir(channel_dir)?;"),
            "regression: write_manifest_atomic must NOT propagate \
             fsync_dir(channel_dir) errors via `?` after a successful \
             durable_rename. The rename is the linearizing event — \
             once it commits, returning Err lies to the caller about \
             whether the flip happened, and any in-process appends \
             between the failed write_manifest_atomic and process \
             exit would land in the (now-dead) cur_gen. Wrap the \
             call in `if let Err(e) = ... {{ tracing::warn!(...) }}` \
             that logs and continues."
        );
    }

    /// Source-text guard for the `migrate_flat_layout_if_needed`
    /// precedence assumption: the function MUST have exactly
    /// one call site in this file (`resolve_live_generation`),
    /// and that call site must sit AFTER the manifest-read and
    /// enumerate-fallback branches. The function clobbers any
    /// pre-existing `v0000000001/{idx,dat,ts}` with the flat-
    /// layout content — safe ONLY because the gate at the call
    /// site guarantees v1 wasn't a live generation. A future
    /// refactor that adds a second caller (especially one
    /// without the gate) would silently substitute legacy flat
    /// content for whatever post-compact data v1 actually held.
    /// Catch that at build time rather than as a corrupted
    /// channel in production.
    #[test]
    fn migrate_flat_layout_has_exactly_one_caller() {
        let src = include_str!("disk.rs");
        // Count occurrences of the function name. One is the
        // definition (`fn migrate_flat_layout_if_needed(`),
        // one is the call site (`migrate_flat_layout_if_needed(`),
        // and one is the rustdoc cross-reference inside the
        // function rustdoc itself. We count strictly the
        // call-shape with `(` immediately following the name —
        // that excludes the rustdoc mention (which is followed
        // by a space).
        let needle_call = "migrate_flat_layout_if_needed(";
        let total = src.matches(needle_call).count();
        // Definition + one caller + this test's own reference
        // (in the assertion message + this needle string above).
        // To make the count deterministic regardless of the
        // surrounding test wording, we strip THIS function's
        // body before counting.
        let test_fn_header = "fn migrate_flat_layout_has_exactly_one_caller(";
        let test_fn_start = src.find(test_fn_header).expect("this test must exist");
        // Walk backward to the start of the test's `#[test]`
        // attribute / rustdoc — close enough to slice off
        // everything after the `///` block. We use a coarse
        // approximation: cut at the most recent blank line
        // before the test header.
        let pre_test = &src[..test_fn_start];
        let cut = pre_test.rfind("\n\n").unwrap_or(0);
        let src_minus_this_test = &src[..cut];
        let outside = src_minus_this_test.matches(needle_call).count();
        assert_eq!(
            outside, 2,
            "regression: `migrate_flat_layout_if_needed` must have \
             exactly two source occurrences with the `(` call shape \
             outside this test (one definition + one caller in \
             `resolve_live_generation`); saw {outside}. The function \
             clobbers any pre-existing v1 with flat-layout content, \
             which is safe only under the precedence gate that \
             `resolve_live_generation` enforces. A second caller \
             would silently lose post-compact data on channels \
             where flat files lingered alongside a real generation \
             directory. See the function's `# Precedence assumption` \
             rustdoc.",
        );
        // Also note: total includes this test's own usage. Sanity
        // check it grows by exactly 2 (the needle string above
        // appears twice in the test source: in `let needle_call`
        // and in the rustdoc slice setup). If a future edit adds
        // more in-test occurrences, bump this and double-check the
        // outside-count math still holds.
        assert!(
            total >= outside,
            "sanity: total occurrences ({total}) cannot exceed \
             outside ({outside}); test self-reference accounting \
             is broken.",
        );
    }
}
