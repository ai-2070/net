//! Directory transfer over router streams (FairScheduler transport plan, T-5).
//!
//! A thin wrapper over the per-chunk [`blob transfer`] primitive that
//! moves a whole directory tree from one peer to another. The sender
//! [`store_dir`]s a tree — every file becomes one or more
//! content-addressed blobs, and a single **directory manifest** blob
//! records the tree shape (relative paths, modes, symlinks, and each
//! file's [`BlobRef`]). The receiver [`fetch_dir`]s from a **known
//! source**: it pulls the manifest, then every leaf, over the reliable
//! scheduled stream transport — and reconstructs the tree on disk.
//!
//! [`blob transfer`]: crate::adapter::net::dataforts::blob::transfer
//!
//! # Why a known source (no per-chunk discovery)
//!
//! The receiver already knows which peer it is pulling the directory
//! from, and that peer holds the whole tree, so every chunk is fetched
//! with [`crate::adapter::net::MeshNode::transfer_fetch_chunk`] against
//! that one `source`.
//! There is deliberately **no** `causal:<hash>` per-chunk advertisement
//! in this path: capability announcements are a single datagram and the
//! per-chunk tag caps at ~15-20 chunks/node, so advertisement-driven
//! discovery never scaled to a directory anyway. Discovery (finding a
//! holder for a blob whose source you *don't* know) is a separate,
//! lower-priority concern for ad-hoc single-blob fetch.
//!
//! # Wire shape
//!
//! [`DirManifest`] is postcard-encoded and itself stored as a blob (so
//! a large tree's manifest chunks like any other blob). [`fetch_dir`]
//! receives the manifest's [`BlobRef`] out-of-band (the caller knows
//! what it asked for) and bootstraps from there.

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use bytes::{BufMut, BytesMut};
use serde::{Deserialize, Serialize};

use super::blob::{
    chunk_payload, BlobAdapter, BlobError, BlobRef, ChunkRef, Encoding, MeshBlobAdapter,
    BLOB_CHUNK_SIZE_BYTES,
};
use crate::adapter::net::MeshNode;

/// Manifest schema version. Bumps independently of the blob-ref wire
/// version so the directory layout can evolve without re-cutting the
/// blob format.
pub const DIR_MANIFEST_VERSION: u8 = 1;

/// Default fan-out for concurrent leaf fetches in [`fetch_dir`]. Each
/// permit is one file's chunk-pull chain in flight; the stream
/// transport + FairScheduler handle byte-level fairness underneath, so
/// this only bounds how many files race for the window at once.
pub const DEFAULT_FETCH_CONCURRENCY: usize = 16;

/// File size above which a single read/write is offloaded to the
/// blocking pool (T-3). Below it the I/O is sub-millisecond, so doing it
/// inline avoids `spawn_blocking` dispatch overhead — which at
/// node_modules scale (tens of thousands of small files) otherwise
/// dominates and tanks throughput. Large files (where the blocking I/O
/// could actually stall an executor thread) are offloaded. The recursive
/// directory walk and the mkdir / symlink passes are always offloaded
/// (one `spawn_blocking` each — they're tight syscall loops regardless
/// of file size).
const BLOCKING_FS_THRESHOLD: u64 = 256 * 1024;

/// Aggregate in-flight byte budget across concurrent leaf fetches in
/// [`fetch_dir`]. The per-stream tx window is large (≈ a whole chunk),
/// so the file-count cap alone does NOT bound how many bytes are in
/// flight at once: N concurrent large files put ≈ N × chunk bytes on the
/// wire, and once that exceeds what the receiver's single recv loop can
/// drain, the kernel recv buffer overflows, packets drop, and (today)
/// the transfer can't recover. This budget caps aggregate in-flight: a
/// file reserves ≈ its current chunk's worth, so many tiny files run
/// wide while large files run only a couple at a time. 8 MiB matches the
/// concurrency that transfers cleanly in the diagnostic sweep against an
/// 8-16 MiB socket recv buffer; deployments that size their recv buffer
/// higher can raise this constant for more large-file parallelism.
pub const DEFAULT_INFLIGHT_BUDGET_BYTES: usize = 8 * 1024 * 1024;

/// Byte-semaphore permits a file reserves while in flight: ≈ its
/// current chunk's worth (chunks move sequentially), clamped to
/// `[1, budget]` so a file larger than the whole budget can still
/// run (alone). Shared by `store_dir` and `fetch_dir` so both
/// sides reserve identically.
///
/// The min-chain runs in the u64 domain BEFORE the `u32` cast —
/// `store_dir` previously cast `meta.len() as u32` first, which
/// wraps for files > 4 GiB (4 GiB + 1 B → 1 permit) and
/// under-reserves the budget while the entire file is buffered in
/// memory. The result fits `u32` by construction (≤ `budget`).
fn in_flight_byte_permits(len: u64, budget: u32) -> u32 {
    len.min(BLOB_CHUNK_SIZE_BYTES).min(budget as u64).max(1) as u32
}

// ── Errors ──────────────────────────────────────────────────────────

/// Failure surface for directory store / fetch.
#[derive(Debug)]
pub enum DirError {
    /// Filesystem I/O failed (walk, read, mkdir, write, symlink).
    Io(std::io::Error),
    /// A blob store / fetch / decode failed.
    Blob(BlobError),
    /// A manifest entry's path escaped the destination root (absolute,
    /// `..`, or a drive/root prefix). Never reconstructed — a malicious
    /// or buggy sender must not write outside `dest`.
    UnsafePath(String),
    /// The manifest blob failed to decode.
    Manifest(String),
}

impl std::fmt::Display for DirError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "dir transfer io: {e}"),
            Self::Blob(e) => write!(f, "dir transfer blob: {e}"),
            Self::UnsafePath(p) => write!(f, "dir transfer: unsafe manifest path {p:?}"),
            Self::Manifest(m) => write!(f, "dir transfer: bad manifest: {m}"),
        }
    }
}

impl std::error::Error for DirError {}

impl From<std::io::Error> for DirError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<BlobError> for DirError {
    fn from(e: BlobError) -> Self {
        Self::Blob(e)
    }
}

// ── Manifest ────────────────────────────────────────────────────────

/// One entry in a [`DirManifest`]. Paths are relative to the tree root
/// and always use `/` separators (canonicalised on store, re-split on
/// fetch) so a tree stored on one OS reconstructs on another.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirEntry {
    /// Relative path from the tree root, `/`-separated.
    pub path: String,
    /// What lives at `path`.
    pub kind: EntryKind,
}

/// The three node kinds a directory tree carries. Devices, sockets,
/// FIFOs, etc. are skipped on store (not represented).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EntryKind {
    /// A regular file. `mode` is the Unix permission bits (0 on
    /// non-Unix stores); `blob` is the encoded [`BlobRef`] for the
    /// file's content.
    File {
        /// Unix permission bits, or 0 when stored on a non-Unix host.
        mode: u32,
        /// Encoded [`BlobRef`] (`BlobRef::encode`) for the content.
        blob: Vec<u8>,
    },
    /// A directory. Recorded explicitly so empty directories survive
    /// the round trip (file parents are created implicitly too).
    Dir {
        /// Unix permission bits, or 0 when stored on a non-Unix host.
        mode: u32,
    },
    /// A symbolic link. `target` is stored verbatim (may be relative or
    /// absolute); reconstructed as a symlink, never followed on store.
    Symlink {
        /// Link target, exactly as read from the source tree.
        target: String,
    },
}

/// The directory manifest — the single structure that ties a tree's
/// blobs together. Postcard-encoded, then stored as a blob; its
/// [`BlobRef`] is what [`fetch_dir`] bootstraps from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirManifest {
    /// Schema version ([`DIR_MANIFEST_VERSION`]).
    pub version: u8,
    /// Entries in deterministic (sorted-by-path) order, so two stores
    /// of the same tree produce the same manifest bytes.
    pub entries: Vec<DirEntry>,
}

impl DirManifest {
    /// Count of file entries.
    pub fn file_count(&self) -> usize {
        self.entries
            .iter()
            .filter(|e| matches!(e.kind, EntryKind::File { .. }))
            .count()
    }
}

/// Outcome of a [`fetch_dir`] — what was reconstructed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DirStats {
    /// Files written.
    pub files: usize,
    /// Directories created (including implicit parents).
    pub dirs: usize,
    /// Symlinks created (0 on platforms where symlink creation failed
    /// and was tolerated — see [`fetch_dir`]).
    pub symlinks: usize,
    /// Total file bytes written.
    pub bytes: u64,
}

// ── Store ───────────────────────────────────────────────────────────

/// Walk `root` and store every file as content-addressed blob(s) in
/// `adapter`, returning the [`BlobRef`] of the directory manifest. The
/// manifest itself is stored as a blob, so the returned `BlobRef` is
/// all a receiver needs (plus the source node id) to pull the tree.
///
/// Symlinks are recorded by target (not followed). Empty directories
/// are recorded. Entries are sorted by path for a deterministic
/// manifest. Non-regular, non-dir, non-symlink nodes are skipped.
pub async fn store_dir(adapter: &MeshBlobAdapter, root: &Path) -> Result<BlobRef, DirError> {
    // Directory traversal (`read_dir` + `symlink_metadata`, recursive) is
    // blocking FS — run it on the blocking pool so it doesn't stall an
    // async executor thread at node_modules scale (T-3). Deterministic
    // order: collect, then sort by relative path.
    let root_buf = root.to_path_buf();
    let mut raw = tokio::task::spawn_blocking(
        move || -> Result<Vec<(String, std::fs::Metadata, PathBuf)>, DirError> {
            let mut raw = Vec::new();
            walk(&root_buf, &root_buf, &mut raw)?;
            Ok(raw)
        },
    )
    .await
    .map_err(|e| DirError::Io(std::io::Error::other(e)))??;
    raw.sort_by(|a, b| a.0.cmp(&b.0));

    // PERF_AUDIT §6.9 — parallelize file processing with the same
    // shape `fetch_dir` uses (bounded `Semaphore` over file count +
    // a `byte_sem` budget). Pre-fix the loop fully serialized:
    // one file at a time read → hash → chunk → store. With
    // `buffer_unordered`-style spawned work, disk reads and
    // adapter stores overlap and the manifest finalizes when the
    // last file lands, not after the sum of per-file latencies.
    //
    // Dirs and symlinks are still inline — they're sub-µs syscalls
    // and not worth a tokio spawn dispatch.
    use futures::stream::StreamExt;
    let store_concurrency = DEFAULT_FETCH_CONCURRENCY;
    let store_budget = u32::try_from(DEFAULT_INFLIGHT_BUDGET_BYTES).unwrap_or(u32::MAX);
    let sem = std::sync::Arc::new(tokio::sync::Semaphore::new(store_concurrency));
    let byte_sem = std::sync::Arc::new(tokio::sync::Semaphore::new(store_budget as usize));

    // Build a per-entry task stream. We preserve manifest order
    // by emitting entries in the same `raw` order: dirs/symlinks
    // resolve to `Ready` futures so they slot into the stream
    // alongside file futures and don't need a separate pass.
    // First-error abort flag: once any entry fails, later entries
    // short-circuit before doing I/O. The drain loop below still
    // awaits every STARTED future (see drain comment), but this
    // flag keeps a failure in a 100k-file tree from paying the
    // read + hash + store cost for every remaining entry.
    let abort = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let file_futures = raw.into_iter().map(|(rel, meta, abs)| {
        let sem = sem.clone();
        let byte_sem = byte_sem.clone();
        let abort = abort.clone();
        let mode = mode_of(&meta);
        async move {
            if abort.load(std::sync::atomic::Ordering::Relaxed) {
                // A sibling entry already failed; the overall
                // store_dir returns its error, so this entry's
                // output is discarded either way.
                return Ok::<Option<DirEntry>, DirError>(None);
            }
            let file_type = meta.file_type();
            if file_type.is_symlink() {
                let target = std::fs::read_link(&abs)?;
                return Ok::<Option<DirEntry>, DirError>(Some(DirEntry {
                    path: rel,
                    kind: EntryKind::Symlink {
                        target: target.to_string_lossy().into_owned(),
                    },
                }));
            }
            if file_type.is_dir() {
                return Ok(Some(DirEntry {
                    path: rel,
                    kind: EntryKind::Dir { mode },
                }));
            }
            if file_type.is_file() {
                // Bound concurrency before the disk read so a
                // wide directory doesn't trigger N parallel
                // `spawn_blocking`s saturating the blocking pool.
                let _permit = sem.acquire().await.map_err(|_| {
                    DirError::Blob(BlobError::Backend("dir store: semaphore closed".into()))
                })?;
                // Per-file in-flight byte reservation — same
                // helper as fetch_dir so both sides reserve
                // identically: big files self-limit while small
                // files stay bounded only by the count cap.
                let in_flight = in_flight_byte_permits(meta.len(), store_budget);
                let _bytes_permit = byte_sem.acquire_many(in_flight).await.map_err(|_| {
                    DirError::Blob(BlobError::Backend(
                        "dir store: byte semaphore closed".into(),
                    ))
                })?;
                let bytes = if meta.len() > BLOCKING_FS_THRESHOLD {
                    tokio::task::spawn_blocking(move || std::fs::read(&abs))
                        .await
                        .map_err(|e| DirError::Io(std::io::Error::other(e)))??
                } else {
                    std::fs::read(&abs)?
                };
                let chunked = chunk_payload(&bytes)?;
                let hash: [u8; 32] = blake3::hash(&bytes).into();
                let uri = format!("mesh://{}", hex(&hash));
                let blob_ref = chunked.into_blob_ref(uri, Encoding::Replicated)?;
                adapter.store(&blob_ref, &bytes).await?;
                return Ok(Some(DirEntry {
                    path: rel,
                    kind: EntryKind::File {
                        mode,
                        blob: blob_ref.encode(),
                    },
                }));
            }
            // Device / socket / fifo — skip.
            Ok(None)
        }
    });

    // `buffer_unordered` lets futures resolve as soon as they're
    // ready (vs `buffered` which waits for in-order completion);
    // we re-sort at the end to keep the manifest deterministic.
    //
    // Drain the stream fully — don't short-circuit on the first
    // error. Same rationale as the manifest store loop in
    // mesh.rs: `store_chunk` registers a per-hash entry in
    // `in_flight_stores` on entry and removes it after the store
    // completes (success or error); dropping a buffered future
    // mid-flight skips that cleanup and leaks the entry. Awaiting
    // every started future lets in-flight stores run their own
    // cleanup paths; the `abort` flag set on first error keeps
    // not-yet-started entries from doing any new work. First
    // error wins so the caller observes the same failure shape
    // as the pre-§6.9 sequential loop.
    let mut results: Vec<DirEntry> = Vec::new();
    let mut first_err: Option<DirError> = None;
    let mut stream = futures::stream::iter(file_futures).buffer_unordered(store_concurrency);
    while let Some(res) = stream.next().await {
        match res {
            Ok(Some(entry)) => results.push(entry),
            Ok(None) => {}
            Err(e) => {
                abort.store(true, std::sync::atomic::Ordering::Relaxed);
                if first_err.is_none() {
                    first_err = Some(e);
                }
            }
        }
    }
    if let Some(e) = first_err {
        return Err(e);
    }
    // Restore the deterministic-by-path order that the pre-fix
    // sequential loop produced (raw was already sorted before
    // dispatch; `buffer_unordered` returns in completion order).
    results.sort_by(|a, b| a.path.cmp(&b.path));
    let entries = results;

    let manifest = DirManifest {
        version: DIR_MANIFEST_VERSION,
        entries,
    };
    let manifest_bytes =
        postcard::to_allocvec(&manifest).map_err(|e| DirError::Manifest(format!("encode: {e}")))?;
    let chunked = chunk_payload(&manifest_bytes)?;
    let mhash: [u8; 32] = blake3::hash(&manifest_bytes).into();
    let manifest_ref =
        chunked.into_blob_ref(format!("mesh://{}", hex(&mhash)), Encoding::Replicated)?;
    adapter.store(&manifest_ref, &manifest_bytes).await?;
    Ok(manifest_ref)
}

/// Recursively collect `(relative-path, metadata, absolute-path)` for
/// every entry under `dir`. Uses `symlink_metadata` so symlinks are
/// reported as links (not followed).
fn walk(
    root: &Path,
    dir: &Path,
    out: &mut Vec<(String, std::fs::Metadata, PathBuf)>,
) -> Result<(), DirError> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let abs = entry.path();
        let meta = std::fs::symlink_metadata(&abs)?;
        let rel = rel_path(root, &abs);
        let is_dir_descend = meta.file_type().is_dir() && !meta.file_type().is_symlink();
        out.push((rel, meta, abs.clone()));
        if is_dir_descend {
            walk(root, &abs, out)?;
        }
    }
    Ok(())
}

/// Render an absolute path relative to `root` as a `/`-separated
/// string. Falls back to the raw lossy rendering if `strip_prefix`
/// fails (shouldn't happen for a path produced by walking `root`).
fn rel_path(root: &Path, abs: &Path) -> String {
    let rel = abs.strip_prefix(root).unwrap_or(abs);
    let mut parts: Vec<String> = Vec::new();
    for comp in rel.components() {
        if let Component::Normal(c) = comp {
            parts.push(c.to_string_lossy().into_owned());
        }
    }
    parts.join("/")
}

#[cfg(unix)]
fn mode_of(meta: &std::fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode()
}

#[cfg(not(unix))]
fn mode_of(_meta: &std::fs::Metadata) -> u32 {
    0
}

// ── Fetch ───────────────────────────────────────────────────────────

/// Pull the directory whose manifest is `manifest_ref` from `source`
/// and reconstruct it under `dest`, **atomically**. Every blob (the
/// manifest, then each file) is fetched over the reliable scheduled
/// stream transport via [`MeshNode::transfer_fetch_chunk`] against the
/// single known `source`. File fetches run with bounded concurrency
/// (`concurrency`, or [`DEFAULT_FETCH_CONCURRENCY`] when 0).
///
/// # Atomicity
///
/// The tree is reconstructed in a sibling temp directory
/// (`<parent>/.<basename>.fetch_<rand>` — same filesystem as `dest`, so
/// the final rename is atomic), then swapped into place. `dest` therefore
/// ends up as the **complete** new tree, or is left **exactly as it was**:
///
/// - On any failure mid-fetch (a chunk fails, the peer drops, a manifest
///   path is unsafe), the temp tree is removed and `dest` is untouched.
/// - On success, the temp tree is renamed onto `dest`. If `dest` already
///   existed, its old contents are moved aside and removed after the new
///   tree is in place — so a replace also drops files from a previous
///   version that aren't in the new manifest (no stale-file accumulation).
///
/// This is *replacement*-atomicity, **not** *observer*-atomicity: a
/// process reading files inside `dest` during the swap may see the old
/// tree one moment and a missing path the next — the rename invalidates
/// open handles, and the two-rename replace has a brief window where
/// `dest` is absent. Callers needing observer-atomicity coordinate at a
/// higher layer.
///
/// **Platform note:** atomicity relies on POSIX `rename` semantics. On
/// Windows `rename` differs around an existing destination; the swap
/// moves the old tree aside first so it works there too, but the
/// substrate is POSIX-first and Windows support is best-effort.
///
/// Directories are created before files (no mkdir races), then files are
/// fetched concurrently, then symlinks last. Manifest paths are validated
/// to stay within the destination.
pub async fn fetch_dir(
    node: &Arc<MeshNode>,
    source: u64,
    manifest_ref: &BlobRef,
    dest: &Path,
    concurrency: usize,
) -> Result<DirStats, DirError> {
    let manifest_bytes = transfer_fetch_blob(node, source, manifest_ref).await?;
    let manifest: DirManifest = postcard::from_bytes(&manifest_bytes)
        .map_err(|e| DirError::Manifest(format!("decode: {e}")))?;
    if manifest.version != DIR_MANIFEST_VERSION {
        return Err(DirError::Manifest(format!(
            "unsupported manifest version {}",
            manifest.version
        )));
    }

    let dest = dest.to_path_buf();
    // Reconstruct into a sibling temp dir, then install it atomically.
    let work = alloc_temp_dir(&dest).await?;
    let stats = match reconstruct_tree(node, source, &manifest, &work, concurrency).await {
        Ok(stats) => stats,
        Err(e) => {
            // Best-effort cleanup so no `.<base>.fetch_*` orphan lingers;
            // the unique, `.`-prefixed name keeps an orphan (if removal
            // also fails) from colliding with a future run and visible to
            // operators. `dest` was never touched.
            let work = work.clone();
            let _ = tokio::task::spawn_blocking(move || std::fs::remove_dir_all(&work)).await;
            return Err(e);
        }
    };
    install_tree(work, dest).await?;
    Ok(stats)
}

/// Reconstruct the manifest's tree under `root` (a temp dir). Pass 1
/// creates directories, pass 2 fetches + writes files concurrently, pass
/// 3 creates symlinks. The concurrency / byte-budget / blocking-pool
/// logic is unchanged from the in-place version — only the write root
/// differs; the caller renames `root` onto the user's `dest` on success.
async fn reconstruct_tree(
    node: &Arc<MeshNode>,
    source: u64,
    manifest: &DirManifest,
    root: &Path,
    concurrency: usize,
) -> Result<DirStats, DirError> {
    let root = root.to_path_buf();
    let mut stats = DirStats::default();

    // Pass 1: create every directory (explicit Dir entries + each
    // file/symlink parent), sequentially, so concurrent file writes
    // never race on mkdir.
    let mut want_dirs: BTreeSet<PathBuf> = BTreeSet::new();
    for entry in &manifest.entries {
        let safe = safe_join(&root, &entry.path)?;
        match &entry.kind {
            EntryKind::Dir { .. } => {
                want_dirs.insert(safe);
            }
            EntryKind::File { .. } | EntryKind::Symlink { .. } => {
                if let Some(parent) = safe.parent() {
                    want_dirs.insert(parent.to_path_buf());
                }
            }
        }
    }
    // Create dirs (incl. the temp root) on the blocking pool (T-3).
    let root_for_dirs = root.clone();
    stats.dirs = tokio::task::spawn_blocking(move || -> Result<usize, DirError> {
        std::fs::create_dir_all(&root_for_dirs)?;
        let mut n = 0;
        for dir in &want_dirs {
            if !dir.exists() {
                std::fs::create_dir_all(dir)?;
                n += 1;
            }
        }
        Ok(n)
    })
    .await
    .map_err(|e| DirError::Io(std::io::Error::other(e)))??;

    // Pass 2: fetch + write files with bounded concurrency.
    let concurrency = if concurrency == 0 {
        DEFAULT_FETCH_CONCURRENCY
    } else {
        concurrency
    };
    let sem = Arc::new(tokio::sync::Semaphore::new(concurrency));
    // Aggregate in-flight byte budget (permits = bytes). A file reserves
    // ≈ its current chunk's worth, so large files self-limit to a couple
    // concurrent while small files stay bounded only by the count cap.
    // Capped at u32 (the budget fits) and clamped to [1, budget] so even
    // a file larger than the budget can run (alone).
    let budget = u32::try_from(DEFAULT_INFLIGHT_BUDGET_BYTES).unwrap_or(u32::MAX);
    let byte_sem = Arc::new(tokio::sync::Semaphore::new(budget as usize));
    let mut tasks = Vec::new();
    for entry in &manifest.entries {
        let EntryKind::File { mode, blob } = &entry.kind else {
            continue;
        };
        let safe = safe_join(&root, &entry.path)?;
        let blob_ref = BlobRef::decode(blob)
            .map_err(DirError::Blob)?
            .ok_or_else(|| DirError::Manifest(format!("entry {} has no blob ref", entry.path)))?;
        // Bytes this file can have in flight at once ≈ its current chunk
        // (chunks pull sequentially), bounded by the budget.
        let in_flight = in_flight_byte_permits(blob_ref.size(), budget);
        let node = node.clone();
        let sem = sem.clone();
        let byte_sem = byte_sem.clone();
        let mode = *mode;
        tasks.push(tokio::spawn(async move {
            // The semaphores live for the whole reconstruction and are
            // never closed, so `acquire` can't actually fail here — map
            // the impossible error to a typed failure rather than panic.
            let _permit = sem.acquire().await.map_err(|_| {
                DirError::Blob(BlobError::Backend("dir fetch: semaphore closed".into()))
            })?;
            let _bytes_permit = byte_sem.acquire_many(in_flight).await.map_err(|_| {
                DirError::Blob(BlobError::Backend(
                    "dir fetch: byte semaphore closed".into(),
                ))
            })?;
            // A multi-chunk (Manifest) leaf streams straight to disk one
            // chunk at a time, so a large single file never spikes memory to
            // its full size — peak is ~one chunk. A single-chunk (Small)
            // leaf is ≤ one chunk anyway, so it takes the buffered
            // inline/offloaded write fast path.
            match &blob_ref {
                BlobRef::Manifest { chunks, .. } => {
                    fetch_blob_to_file(&node, source, chunks, &safe, mode).await
                }
                _ => {
                    let bytes = transfer_fetch_blob(&node, source, &blob_ref).await?;
                    let len = bytes.len() as u64;
                    // Offload only large writes (T-3); small files write inline.
                    if len > BLOCKING_FS_THRESHOLD {
                        tokio::task::spawn_blocking(move || write_file(&safe, &bytes, mode))
                            .await
                            .map_err(|e| DirError::Io(std::io::Error::other(e)))??;
                    } else {
                        write_file(&safe, &bytes, mode)?;
                    }
                    Ok::<u64, DirError>(len)
                }
            }
        }));
    }
    for task in tasks {
        match task.await {
            Ok(Ok(n)) => {
                stats.files += 1;
                stats.bytes += n;
            }
            Ok(Err(e)) => return Err(e),
            Err(join) => {
                return Err(DirError::Blob(BlobError::Backend(format!(
                    "dir fetch task panicked: {join}"
                ))))
            }
        }
    }

    // Pass 3: symlinks last (targets may be files just written).
    // Resolve safe paths (CPU) here, then create the links on the
    // blocking pool (T-3). A platform that can't create a symlink (e.g.
    // Windows without privilege) is tolerated — the files still landed.
    // Collect the normalized paths of every declared symlink first. The
    // tree is reconstructed into a private, initially-empty temp dir, so
    // these are the only symlinks that can ever exist in it — which is
    // what lets the lexical `check_link_target` below reason about
    // symlink *composition* (one link resolving through another).
    let mut symlink_paths: BTreeSet<Vec<String>> = BTreeSet::new();
    for entry in &manifest.entries {
        if let EntryKind::Symlink { .. } = &entry.kind {
            if let Some(c) = normal_components(&entry.path) {
                symlink_paths.insert(c);
            }
        }
    }
    let mut links: Vec<(String, PathBuf)> = Vec::new();
    for entry in &manifest.entries {
        if let EntryKind::Symlink { target } = &entry.kind {
            let safe = safe_join(&root, &entry.path)?;
            // The link path is confined by `safe_join`, but the target is
            // attacker-controlled too — a hostile sender could plant
            // `link -> /etc/passwd`, `link -> ../../../../etc`, or a *pair*
            // of links that compose to escape (`a -> .`, `b -> a/../..`).
            // Reject any link the OS could resolve outside the tree.
            check_link_target(&entry.path, target, &symlink_paths)?;
            links.push((target.clone(), safe));
        }
    }
    stats.symlinks = tokio::task::spawn_blocking(move || {
        links
            .into_iter()
            .filter(|(target, safe)| make_symlink(target, safe).is_ok())
            .count()
    })
    .await
    .map_err(|e| DirError::Io(std::io::Error::other(e)))?;

    Ok(stats)
}

/// A process-unique `u64` for temp / backup path suffixes. The monotonic
/// counter guarantees two concurrent allocations in this process never
/// collide; the time + pid mix guards against cross-process / cross-run
/// reuse of the same parent directory. The caller's create-with-
/// `AlreadyExists`-retry is the final backstop. Dependency-free on purpose
/// — `rand` is only a dev-dependency here.
fn unique_suffix() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    seq ^ nanos.rotate_left(17) ^ (std::process::id() as u64).rotate_left(43)
}

/// Allocate a fresh sibling temp directory for `dest`
/// (`<parent>/.<basename>.fetch_<suffix>`), creating `dest`'s parent first
/// so the eventual rename has a target. Sibling placement keeps the temp
/// on the same filesystem as `dest` — a cross-filesystem temp would make
/// `rename` silently fall back to copy-and-delete, breaking atomicity.
/// Retries on the (astronomically unlikely) random-suffix collision.
async fn alloc_temp_dir(dest: &Path) -> Result<PathBuf, DirError> {
    let parent = match dest.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        // Bare relative name ("foo") or no parent → use the cwd.
        _ => PathBuf::from("."),
    };
    let base = dest
        .file_name()
        .ok_or_else(|| DirError::UnsafePath(dest.to_string_lossy().into_owned()))?
        .to_string_lossy()
        .into_owned();
    tokio::task::spawn_blocking(move || -> Result<PathBuf, DirError> {
        std::fs::create_dir_all(&parent)?;
        for _ in 0..8 {
            let work = parent.join(format!(".{base}.fetch_{:016x}", unique_suffix()));
            match std::fs::create_dir(&work) {
                Ok(()) => return Ok(work),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(DirError::Io(e)),
            }
        }
        Err(DirError::Io(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "fetch_dir: could not allocate a unique temp directory",
        )))
    })
    .await
    .map_err(|e| DirError::Io(std::io::Error::other(e)))?
}

/// Atomically install the completed temp tree `work` at `dest`.
///
/// If `dest` is absent, a single `rename` moves the tree in. If `dest`
/// exists, a two-rename swap (move the old tree to a `.replaced_<rand>`
/// sibling, move the new tree in, then drop the old) replaces it: a crash
/// between renames leaves either the old or the new tree at `dest`, never
/// neither. The swap window where `dest` is briefly absent is the
/// documented limit on observer-atomicity (see [`fetch_dir`]). On the
/// rare failure of the second rename the old tree is restored. All FS ops
/// run on the blocking pool (T-3).
async fn install_tree(work: PathBuf, dest: PathBuf) -> Result<(), DirError> {
    tokio::task::spawn_blocking(move || -> Result<(), DirError> {
        if !dest.exists() {
            return std::fs::rename(&work, &dest).map_err(|e| {
                let _ = std::fs::remove_dir_all(&work);
                DirError::Io(e)
            });
        }
        let parent = match dest.parent() {
            Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
            _ => PathBuf::from("."),
        };
        let base = dest
            .file_name()
            .ok_or_else(|| DirError::UnsafePath(dest.to_string_lossy().into_owned()))?
            .to_string_lossy()
            .into_owned();
        // Pick an unused backup path for the old tree.
        let mut backup = None;
        for _ in 0..8 {
            let cand = parent.join(format!(".{base}.replaced_{:016x}", unique_suffix()));
            if !cand.exists() {
                backup = Some(cand);
                break;
            }
        }
        let backup = backup.ok_or_else(|| {
            DirError::Io(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "fetch_dir: could not allocate a backup path",
            ))
        })?;
        // Move the old tree aside, then the new tree in. If the second
        // rename fails, restore the old tree and surface the error.
        std::fs::rename(&dest, &backup).map_err(DirError::Io)?;
        if let Err(e) = std::fs::rename(&work, &dest) {
            let _ = std::fs::rename(&backup, &dest);
            let _ = std::fs::remove_dir_all(&work);
            return Err(DirError::Io(e));
        }
        // New tree is in place; drop the old one (best effort).
        let _ = std::fs::remove_dir_all(&backup);
        Ok(())
    })
    .await
    .map_err(|e| DirError::Io(std::io::Error::other(e)))?
}

/// Fetch a whole blob (all of its chunks) from `source` over the
/// transfer transport and return the reassembled bytes. A `Small` blob
/// is one chunk (its content hash); a `Manifest` is its ordered chunk
/// list. Each chunk is BLAKE3-verified by `transfer_fetch_chunk`; the
/// concatenation order is the manifest order.
async fn transfer_fetch_blob(
    node: &Arc<MeshNode>,
    source: u64,
    blob_ref: &BlobRef,
) -> Result<bytes::Bytes, DirError> {
    match blob_ref {
        BlobRef::Small { hash, .. } => Ok(node.transfer_fetch_chunk(source, *hash).await?),
        BlobRef::Manifest { chunks, .. } => {
            let mut buf = BytesMut::with_capacity(blob_ref.size() as usize);
            for chunk in chunks {
                let bytes = node.transfer_fetch_chunk(source, chunk.hash).await?;
                buf.put_slice(&bytes);
            }
            Ok(buf.freeze())
        }
        BlobRef::Tree { .. } => Err(DirError::Blob(BlobError::Backend(
            "dir transfer: BlobRef::Tree not supported by the directory wrapper".into(),
        ))),
    }
}

/// Stream a multi-chunk leaf straight to `path`, fetching and writing one
/// chunk at a time so a large leaf is never buffered whole (peak ~one
/// chunk). Returns the bytes written. Mirrors [`write_file`]'s mode
/// application.
///
/// The substrate's tokio build has no `fs` feature (file I/O is sync
/// `std::fs` offloaded to the blocking pool — see [`write_file`]), so each
/// chunk write runs on `spawn_blocking` with the open handle threaded
/// through, keeping the async worker free during the actual write.
async fn fetch_blob_to_file(
    node: &Arc<MeshNode>,
    source: u64,
    chunks: &[ChunkRef],
    path: &Path,
    mode: u32,
) -> Result<u64, DirError> {
    let create_path = path.to_path_buf();
    let mut file = tokio::task::spawn_blocking(move || std::fs::File::create(&create_path))
        .await
        .map_err(|e| DirError::Io(std::io::Error::other(e)))??;
    let mut written: u64 = 0;
    for chunk in chunks {
        let bytes = node.transfer_fetch_chunk(source, chunk.hash).await?;
        written += bytes.len() as u64;
        // Offload the blocking write, moving the handle in and back out so
        // the next chunk writes to the same file.
        file = tokio::task::spawn_blocking(move || -> std::io::Result<std::fs::File> {
            use std::io::Write as _;
            let mut f = file;
            f.write_all(&bytes)?;
            Ok(f)
        })
        .await
        .map_err(|e| DirError::Io(std::io::Error::other(e)))??;
    }
    // Flush + close on the blocking pool, then apply the mode.
    tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        use std::io::Write as _;
        file.flush()
    })
    .await
    .map_err(|e| DirError::Io(std::io::Error::other(e)))??;
    apply_mode(path, mode)?;
    Ok(written)
}

// ── Path safety + FS apply ──────────────────────────────────────────

/// Join a manifest-supplied relative path onto `dest`, rejecting any
/// path that would escape `dest` (absolute, drive-prefixed, or
/// containing a `..` / root component). The manifest is attacker-
/// controlled across a transfer, so this is the security boundary that
/// keeps a hostile sender from writing outside the destination.
fn safe_join(dest: &Path, rel: &str) -> Result<PathBuf, DirError> {
    if rel.is_empty() {
        return Err(DirError::UnsafePath(rel.to_owned()));
    }
    let mut out = dest.to_path_buf();
    for comp in Path::new(rel).components() {
        match comp {
            Component::Normal(c) => out.push(c),
            // Anything that isn't a plain name — `..`, `/`, `C:\`,
            // `\\?\`, a bare `.` is harmless but we reject the lot for
            // a tight, auditable rule.
            _ => return Err(DirError::UnsafePath(rel.to_owned())),
        }
    }
    Ok(out)
}

/// Fold one path component to a canonical key for declared-symlink
/// matching, tracking how the destination filesystem compares names.
///
/// Default macOS (APFS/HFS+) and Windows volumes compare filenames
/// **case-insensitively**, and APFS/HFS+ additionally compare
/// **normalization-insensitively** — the precomposed `é` (U+00E9) and
/// the decomposed `e`+◌́ (U+0065 U+0301) name the same entry. The
/// composition check matches a target's components against the declared
/// symlink set; if that match used raw bytes, a hostile manifest could
/// declare symlink `é` (one form) and route a target through the other
/// form (or a case variant) to slip past the check while the OS still
/// resolves them as the same link.
///
/// Fold to a single key — lowercase, then NFC-normalize — so equivalent
/// names collapse together. This over-rejects names that are genuinely
/// distinct on a case-/normalization-sensitive FS (Linux ext4), an
/// acceptable safety bias for an attacker-controlled manifest. It's a
/// close approximation of the OS folding (true caseless matching also
/// folds e.g. `ß`→`ss`), erring toward rejection.
fn fold_component(c: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    c.to_lowercase().nfc().collect()
}

/// Normalize a manifest relative path to its sequence of plain-name
/// components (case-folded via [`fold_component`]), dropping `.`
/// segments and rejecting anything else (`..`, root, drive prefix).
/// Returns `None` for a path that isn't a pure in-tree descent — used
/// both to key the declared-symlink set and to walk a link's own
/// location.
fn normal_components(rel: &str) -> Option<Vec<String>> {
    let mut out = Vec::new();
    for comp in Path::new(rel).components() {
        match comp {
            Component::Normal(c) => out.push(fold_component(&c.to_string_lossy())),
            Component::CurDir => {}
            _ => return None,
        }
    }
    Some(out)
}

/// Reject a symlink the OS could resolve outside the reconstruction
/// root, *accounting for other symlinks in the same manifest*.
///
/// The link itself lives at `root/link_rel` (already confined by
/// [`safe_join`]); its target is resolved by the OS relative to the
/// link's parent directory. Since the manifest is attacker-controlled,
/// a target like `/etc/passwd` or `../../../../etc` must not yield a
/// link pointing outside the fetched tree.
///
/// This is a purely lexical check — no filesystem access — so it is
/// immune to TOCTOU and copes with dangling targets. It is *complete*
/// against symlink composition because the tree is reconstructed into a
/// private, initially-empty temp dir: `symlinks` holds every path that
/// can be a symlink in it, so the check can refuse any link the OS would
/// resolve *through* another symlink (where the simple depth model no
/// longer holds). Concretely it rejects:
///
///  1. absolute / drive-prefixed targets;
///  2. targets whose `..` chain pops above root;
///  3. links whose own parent path, or whose target's intermediate
///     components, traverse a declared symlink — e.g. `a -> .` plus
///     `b -> a/sub/../../etc`, where following `a` (→ root) then `..`
///     escapes even though every component looks in-tree lexically.
///
/// A symlink as the link's *final* target component is fine (the OS
/// lands on it but doesn't traverse it); that destination is validated
/// in its own right when its entry is processed.
///
/// Matching against `symlinks` is case- and normalization-insensitive
/// (see [`fold_component`]) so a casing or Unicode-normalization variant
/// can't bypass the check on a case-/normalization-insensitive
/// filesystem (notably default APFS).
fn check_link_target(
    link_rel: &str,
    target: &str,
    symlinks: &BTreeSet<Vec<String>>,
) -> Result<(), DirError> {
    let t = Path::new(target);
    if t.is_absolute() {
        return Err(DirError::UnsafePath(target.to_owned()));
    }
    // Resolution starts in the link's parent directory. Build it as a
    // root-relative component stack we can push/pop as `..`/names apply.
    let mut stack =
        normal_components(link_rel).ok_or_else(|| DirError::UnsafePath(link_rel.to_owned()))?;
    // Drop the link's own filename → its parent directory.
    stack.pop();
    // Every ancestor of the link must be a real directory, not a
    // symlink, or the link isn't actually created where its path says.
    for i in 1..=stack.len() {
        if symlinks.contains(&stack[..i].to_vec()) {
            return Err(DirError::UnsafePath(link_rel.to_owned()));
        }
    }

    let comps: Vec<Component> = t.components().collect();
    for (idx, comp) in comps.iter().enumerate() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                // `..` pops one level; underflow means the resolved path
                // escaped above root.
                if stack.pop().is_none() {
                    return Err(DirError::UnsafePath(target.to_owned()));
                }
            }
            Component::Normal(c) => {
                // Case-folded to match `symlinks` (built via
                // `normal_components`), so a casing variant can't dodge
                // the traversal check on a case-insensitive FS.
                stack.push(fold_component(&c.to_string_lossy()));
                // A symlink in any *non-final* position is followed by the
                // OS to an unknown location, so the lexical depth model no
                // longer holds — reject (can't prove confinement).
                let is_final = idx + 1 == comps.len();
                if !is_final && symlinks.contains(&stack) {
                    return Err(DirError::UnsafePath(target.to_owned()));
                }
            }
            // RootDir / Prefix (Windows `C:\`, `\\?\`) — absolute-ish;
            // `is_absolute` catches most, this is the defensive backstop.
            _ => return Err(DirError::UnsafePath(target.to_owned())),
        }
    }
    Ok(())
}

fn write_file(path: &Path, bytes: &[u8], mode: u32) -> Result<(), DirError> {
    std::fs::write(path, bytes)?;
    apply_mode(path, mode)?;
    Ok(())
}

#[cfg(unix)]
fn apply_mode(path: &Path, mode: u32) -> Result<(), DirError> {
    if mode != 0 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn apply_mode(_path: &Path, _mode: u32) -> Result<(), DirError> {
    Ok(())
}

#[cfg(unix)]
fn make_symlink(target: &str, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(windows)]
fn make_symlink(target: &str, link: &Path) -> std::io::Result<()> {
    // Best-effort: assume a file target. Directory symlinks need a
    // different call; the tolerated-failure path in `fetch_dir`
    // absorbs the mismatch.
    std::os::windows::fs::symlink_file(target, link)
}

#[cfg(not(any(unix, windows)))]
fn make_symlink(_target: &str, _link: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "symlinks unsupported on this platform",
    ))
}

/// Lowercase-hex render of a 32-byte hash for cosmetic `mesh://` URIs.
fn hex(hash: &[u8; 32]) -> String {
    let mut s = String::with_capacity(64);
    for b in hash {
        use std::fmt::Write as _;
        let _ = write!(s, "{b:02x}");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Review-bot P2 regression — the byte-permit clamp must run
    /// in the u64 domain BEFORE the u32 cast. `store_dir`
    /// previously did `(len as u32).min(...)`, so a > 4 GiB file
    /// wrapped (4 GiB + 1 B → 1 permit) and held the whole file in
    /// memory against a 1-byte budget reservation.
    #[test]
    fn in_flight_byte_permits_clamps_before_casting() {
        let budget = u32::try_from(DEFAULT_INFLIGHT_BUDGET_BYTES).unwrap();
        let chunk = u32::try_from(BLOB_CHUNK_SIZE_BYTES).unwrap();

        // Empty file still needs one permit to make progress.
        assert_eq!(in_flight_byte_permits(0, budget), 1);
        // Small file reserves exactly its size.
        assert_eq!(in_flight_byte_permits(1024, budget), 1024);
        // At and past the chunk size: capped to one chunk's worth.
        assert_eq!(in_flight_byte_permits(BLOB_CHUNK_SIZE_BYTES, budget), chunk);
        assert_eq!(
            in_flight_byte_permits(BLOB_CHUNK_SIZE_BYTES + 1, budget),
            chunk
        );
        // The wrap case: 4 GiB + 1 byte. `as u32` first would give
        // 1; the u64-domain clamp gives the chunk cap.
        assert_eq!(
            in_flight_byte_permits(u64::from(u32::MAX) + 2, budget),
            chunk
        );
        assert_eq!(in_flight_byte_permits(u64::MAX, budget), chunk);
        // A budget smaller than the chunk size wins the clamp, so
        // a giant file can still run alone.
        assert_eq!(in_flight_byte_permits(u64::MAX, 64), 64);
    }

    #[test]
    fn safe_join_accepts_plain_relative_paths() {
        let dest = Path::new("/tmp/dest");
        let p = safe_join(dest, "a/b/c.txt").unwrap();
        assert!(p.ends_with("a/b/c.txt") || p.ends_with("a\\b\\c.txt"));
    }

    #[test]
    fn safe_join_rejects_escapes() {
        let dest = Path::new("/tmp/dest");
        assert!(safe_join(dest, "../escape").is_err());
        assert!(safe_join(dest, "a/../../escape").is_err());
        assert!(safe_join(dest, "/abs/path").is_err());
        assert!(safe_join(dest, "").is_err());
    }

    fn no_symlinks() -> BTreeSet<Vec<String>> {
        BTreeSet::new()
    }

    #[test]
    fn check_link_target_accepts_in_tree_targets() {
        let s = no_symlinks();
        // Relative target staying inside the tree, from a nested link.
        assert!(check_link_target("sub/link", "file.txt", &s).is_ok());
        assert!(check_link_target("sub/link", "../other/file.txt", &s).is_ok());
        // `..` that pops back to root but no further is fine.
        assert!(check_link_target("a/b/link", "../../c", &s).is_ok());
        // Self-referential `.` components are harmless.
        assert!(check_link_target("link", "./peer", &s).is_ok());
    }

    #[test]
    fn check_link_target_rejects_escapes() {
        let s = no_symlinks();
        // Absolute target — the classic `link -> /etc/passwd`.
        assert!(check_link_target("link", "/etc/passwd", &s).is_err());
        // Relative target escaping above root from a top-level link.
        assert!(check_link_target("link", "../etc", &s).is_err());
        // Deep `..` chain that escapes even from a nested link.
        assert!(check_link_target("a/b/link", "../../../../etc", &s).is_err());
        // Escapes midway then comes back — still rejected (the
        // intermediate step left the tree).
        assert!(check_link_target("link", "../../a/b", &s).is_err());
    }

    #[test]
    fn check_link_target_rejects_composed_symlink_escape() {
        // The composition attack the lexical-only check missed: `a -> .`
        // is in-tree (resolves to root), but a *second* link that
        // traverses `a` and then walks `..` escapes, because following
        // `a` lands at root yet the lexical model counted `a` as a
        // descent. With `a` in the declared-symlink set, the second link
        // is rejected.
        let mut s = BTreeSet::new();
        s.insert(vec!["a".to_string()]);

        // `a -> .` itself is fine — it stays at root.
        assert!(check_link_target("a", ".", &s).is_ok());
        // `b -> a/sub/../../etc` is lexically depth-neutral but resolves
        // through `a` (a symlink) → reject.
        assert!(check_link_target("b", "a/sub/../../etc", &s).is_err());
        // Pointing *at* the symlink as the final component is allowed
        // (the OS lands on `a`, doesn't traverse it; `a` is validated
        // separately).
        assert!(check_link_target("b", "a", &s).is_ok());
    }

    #[test]
    fn check_link_target_rejects_composed_escape_with_different_case() {
        // On a case-insensitive FS (Windows, default macOS) `A` and `a`
        // are the same entry, so a casing variant must not dodge the
        // traversal check. The set is keyed via `normal_components`,
        // which case-folds, and the target walk folds too.
        let mut s = BTreeSet::new();
        s.insert(normal_components("a").unwrap()); // declared symlink `a`

        // Target routes through `A` (== `a`) then escapes → reject.
        assert!(check_link_target("b", "A/sub/../../etc", &s).is_err());
        // A symlink declared in upper case is matched by a lower-case
        // traversal too (folding is symmetric).
        let mut s2 = BTreeSet::new();
        s2.insert(normal_components("Dir").unwrap());
        assert!(check_link_target("b", "dir/sub/../../../etc", &s2).is_err());
        // A link sitting under a case-variant of a symlinked parent is
        // likewise rejected.
        assert!(check_link_target("A/inner", "x", &s).is_err());
    }

    #[test]
    fn check_link_target_rejects_composed_escape_with_different_unicode_form() {
        // On a normalization-insensitive FS (default APFS) the
        // precomposed and decomposed forms of an accented name are the
        // same entry, so a normalization variant must not dodge the
        // traversal check. Declare the symlink in NFC form, route the
        // target through the NFD form.
        const NFC: &str = "caf\u{00E9}"; // "café" precomposed
        const NFD: &str = "cafe\u{0301}"; // "café" decomposed
                                          // Sanity: the two forms are byte-distinct (raw-bytes matching
                                          // would miss the bypass) but fold to the same key.
        assert_ne!(NFC, NFD);
        assert_eq!(fold_component(NFC), fold_component(NFD));

        let mut s = BTreeSet::new();
        s.insert(normal_components(NFC).unwrap()); // symlink declared NFC

        // Target routes through the NFD form (== NFC on APFS) → reject.
        assert!(check_link_target("b", &format!("{NFD}/sub/../../etc"), &s).is_err());
        // Mixed case + normalization variant also folds and is rejected.
        let upper_nfd = format!("CAFE{}", "\u{0301}");
        assert!(check_link_target("b", &format!("{upper_nfd}/x/../../../etc"), &s).is_err());
    }

    #[test]
    fn check_link_target_rejects_link_under_a_symlinked_parent() {
        // Write-side composition: if `d` is a symlink, a link declared at
        // `d/inner` would be created *through* `d` (an unknown location).
        // Reject it regardless of its target.
        let mut s = BTreeSet::new();
        s.insert(vec!["d".to_string()]);
        assert!(check_link_target("d/inner", "file.txt", &s).is_err());
    }

    #[test]
    fn manifest_round_trips_through_postcard() {
        let manifest = DirManifest {
            version: DIR_MANIFEST_VERSION,
            entries: vec![
                DirEntry {
                    path: "dir".into(),
                    kind: EntryKind::Dir { mode: 0o755 },
                },
                DirEntry {
                    path: "dir/file.txt".into(),
                    kind: EntryKind::File {
                        mode: 0o644,
                        blob: BlobRef::small("mesh://x", [7u8; 32], 3).encode(),
                    },
                },
                DirEntry {
                    path: "link".into(),
                    kind: EntryKind::Symlink {
                        target: "dir/file.txt".into(),
                    },
                },
            ],
        };
        let bytes = postcard::to_allocvec(&manifest).unwrap();
        let decoded: DirManifest = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(decoded, manifest);
        assert_eq!(decoded.file_count(), 1);
    }
}
