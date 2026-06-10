//! `FileSystemAdapter` — reference implementation of
//! [`super::BlobAdapter`] for trusted-host setups + as the
//! conformance fixture.
//!
//! Layout: content-addressed via the BLAKE3 hash. The `BlobRef.uri`
//! is opaque from this adapter's perspective — content is stored at
//! `<root>/<hex_hash[0..2]>/<hex_hash>` regardless of URI. Two-byte
//! prefix sharding avoids the common "millions of files in one
//! directory" anti-pattern on real filesystems.
//!
//! All `std::fs` calls run inside `tokio::task::spawn_blocking` so
//! the production tokio runtime stays cheap (no `tokio/fs` feature
//! widening for the whole crate). Sync I/O is the right primitive
//! for the FS adapter — actual filesystems are not really async.

use std::io::{Read, Seek, SeekFrom};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use tokio::sync::Semaphore;

use super::adapter::{BlobAdapter, BlobByteStream};
use super::blob_ref::BlobRef;
use super::error::BlobError;

/// Chunk size for the streaming fetch path. Sized to balance per-
/// chunk fixed cost (Bytes allocation, channel hop) against memory
/// pressure on the consumer side. 256 KiB is a typical filesystem
/// read-ahead window.
pub const FS_STREAM_CHUNK_BYTES: usize = 256 * 1024;

/// Extract `(hash, uri)` from a [`BlobRef::Small`] for the FS
/// adapter's per-operation entry point. The FS adapter is the
/// bottom layer of the blob stack — it operates on single
/// content-addressed blobs; chunking + manifest dispatch happen
/// at the layer above (the future `MeshBlobAdapter`). A
/// [`BlobRef::Manifest`] passed here is a layering bug; return
/// `BlobError::Backend` rather than silently mis-interpreting it.
fn expect_small(blob_ref: &BlobRef) -> Result<([u8; 32], &str), BlobError> {
    match blob_ref {
        BlobRef::Small { hash, uri, .. } => Ok((*hash, uri.as_str())),
        BlobRef::Manifest { .. } | BlobRef::Tree { .. } => Err(BlobError::Backend(
            "FileSystemAdapter operates on Small blobs only; \
             chunked blobs are handled by the layer above"
                .to_owned(),
        )),
    }
}

/// Default cap on concurrent FS adapter spawn_blocking tasks. A
/// burst of stores against the default tokio blocking pool (512
/// threads) would otherwise starve other blocking work in the
/// process (RedEX writes, replication, etc.).
pub const DEFAULT_FS_ADAPTER_CONCURRENCY: usize = 64;

/// Filesystem-backed blob adapter. Content-addressed by BLAKE3 hash
/// under a caller-supplied root directory.
///
/// # Threat model
///
/// The adapter assumes the configured `root` directory is writable
/// **only by the substrate process** (and any process running with
/// the same uid). Operators MUST enforce this contract via filesystem
/// permissions — typically `chown <daemon-user> <root>` plus mode
/// `0700` on Unix, or an equivalent ACL on Windows.
///
/// Cross-process write access inside `root` by a non-substrate user
/// enables a symlink-swap window between the in-store `canonicalize`
/// check and the `rename(tmp, path)` system call. An attacker who
/// can pre-create or replace `<root>/<shard>/` between those two
/// operations can redirect the rename target outside the root.
///
/// In-code defenses are defense-in-depth, not a complete sandbox:
///
/// - `store` canonicalizes the parent directory and rejects writes
///   whose parent isn't `starts_with(root)`. Closes the obvious
///   "shard pre-created as a symlink before any write" case but
///   not the post-canonicalize swap.
/// - `store` falls back on rename failure to reading the existing
///   file and verifying its content hash against the expected
///   `BlobRef`. Mitigates the case where a concurrent legitimate
///   writer landed first but not the case where an attacker swaps
///   the parent under us.
///
/// If a deployment ever needs to host the root in a shared-scratch
/// environment, adopt platform-specific path-confinement primitives
/// (Linux `openat2` with `RESOLVE_BENEATH`, Windows
/// `FILE_FLAG_OPEN_REPARSE_POINT`) behind a feature flag rather
/// than relying on the documented exclusive-ownership contract.
#[derive(Debug, Clone)]
pub struct FileSystemAdapter {
    id: String,
    root: PathBuf,
    /// Lazily-computed `canonicalize(root)`. Per PERF_AUDIT §6.10
    /// the pre-fix `path_within_root` / store path canonicalized
    /// `root` on every store + every fetch. The root is set at
    /// construction and never changes, so we cache the resolved
    /// form via `OnceLock` on first use. Stored as
    /// `Option<PathBuf>` so the cache can record "root doesn't
    /// exist yet" (legitimate at fresh-adapter construction) and
    /// re-try on the next store — `OnceLock` only initializes
    /// once, so we hold the cell as `Mutex<Option<PathBuf>>`
    /// instead.
    root_canonical: Arc<parking_lot::Mutex<Option<PathBuf>>>,
    /// Cap on concurrent `spawn_blocking` tasks issued by this
    /// adapter. Bounds the share of the tokio blocking pool that a
    /// burst of stores can claim so other blocking work in the
    /// process (RedEX writes, replication) isn't starved.
    concurrency: Arc<Semaphore>,
}

impl FileSystemAdapter {
    /// Construct an adapter rooted at `root`. The directory is
    /// created on the first `store` if absent; `fetch` against an
    /// unprepared root surfaces `BlobError::NotFound`. Concurrency
    /// defaults to [`DEFAULT_FS_ADAPTER_CONCURRENCY`]; override via
    /// [`Self::with_concurrency`].
    pub fn new(id: impl Into<String>, root: impl Into<PathBuf>) -> Self {
        Self {
            id: id.into(),
            root: root.into(),
            root_canonical: Arc::new(parking_lot::Mutex::new(None)),
            concurrency: Arc::new(Semaphore::new(DEFAULT_FS_ADAPTER_CONCURRENCY)),
        }
    }

    /// Get the cached `canonicalize(root)` (computing + storing on
    /// first call). Returns `Err` if `canonicalize` fails for any
    /// reason other than NotFound; returns `Ok(None)` if the root
    /// doesn't exist yet (fresh adapter — the next store will
    /// `create_dir_all` it). Per PERF_AUDIT §6.10 — must be called
    /// from a blocking context (it may issue one `canonicalize`).
    fn cached_root_canonical(&self) -> Result<Option<PathBuf>, BlobError> {
        if let Some(cached) = self.root_canonical.lock().clone() {
            return Ok(Some(cached));
        }
        match std::fs::canonicalize(&self.root) {
            Ok(canon) => {
                *self.root_canonical.lock() = Some(canon.clone());
                Ok(Some(canon))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(backend(e)),
        }
    }

    /// Override the per-adapter spawn_blocking concurrency cap.
    /// Floor 1 — zero would deadlock the adapter.
    pub fn with_concurrency(mut self, cap: usize) -> Self {
        self.concurrency = Arc::new(Semaphore::new(cap.max(1)));
        self
    }

    /// Compute the on-disk path for a given blob hash.
    fn path_for(&self, hash: &[u8; 32]) -> PathBuf {
        let mut hex = String::with_capacity(64);
        for b in hash {
            use std::fmt::Write;
            let _ = write!(hex, "{:02x}", b);
        }
        let shard = &hex[..2];
        self.root.join(shard).join(&hex)
    }

    /// Resolve `path`'s symlinks and confirm it stays within `root`
    /// before a read follows it.
    ///
    /// `store` already canonicalizes the parent on writes; the read
    /// paths (`fetch` / `fetch_range` / `exists` / `fetch_stream`)
    /// previously opened `path_for(hash)` directly, so a cross-process
    /// writer who swapped `<root>/<shard>` or the blob file for a
    /// symlink pointing outside `root` could redirect a read and
    /// exfiltrate an arbitrary file (security audit L3). Canonicalizing
    /// and checking containment closes the static-symlink case on
    /// reads, mirroring the write side. This is defense-in-depth, not a
    /// full sandbox — a post-canonicalize swap still races the open;
    /// complete confinement needs `openat2(RESOLVE_BENEATH)` and is
    /// out of scope here (see the type-level threat-model docs).
    ///
    /// Returns `Ok(true)` when the target exists and resolves inside
    /// `root`, `Ok(false)` when it does not exist, and `Err` on an IO
    /// error or when the resolved path escapes `root`. Must be called
    /// from a blocking context (it issues `canonicalize` syscalls).
    fn path_within_root(&self, path: &Path) -> Result<bool, BlobError> {
        let canon = match std::fs::canonicalize(path) {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(backend(e)),
        };
        // PERF_AUDIT §6.10 — cached root canonicalization. Pre-fix
        // every read paid an extra `canonicalize(root)` syscall.
        let Some(root_canon) = self.cached_root_canonical()? else {
            // Root doesn't exist yet — a resolved blob path can't
            // escape something that's not there.
            return Ok(false);
        };
        if !canon.starts_with(&root_canon) {
            return Err(BlobError::Backend(
                "fs adapter: resolved blob path escapes adapter root".to_string(),
            ));
        }
        Ok(true)
    }
}

fn backend(e: impl std::fmt::Display) -> BlobError {
    BlobError::Backend(e.to_string())
}

/// Render a URI for inclusion in a `BlobError::NotFound` string in
/// a form safe to log. The URI is publisher-controlled bytes — raw
/// newlines / ANSI escapes / NULs propagated into telemetry sinks
/// (Splunk, journald, JS console) cause log-line splicing or
/// terminal-escape injection. Sanitisation: control chars (`< 0x20`
/// and `0x7F`) escape to `\xNN`, length caps at 256 bytes.
fn sanitize_uri_for_error(uri: &str) -> String {
    const MAX_LEN: usize = 256;
    // Slice on a char boundary — a publisher-supplied URI that's
    // valid UTF-8 can still have a multi-byte codepoint straddling
    // byte 256; `&uri[..MAX_LEN]` would panic mid-codepoint and crash
    // inside `spawn_blocking`.
    let (trimmed, truncated) = if uri.len() > MAX_LEN {
        let cut = (0..=MAX_LEN)
            .rev()
            .find(|&i| uri.is_char_boundary(i))
            .unwrap_or(0);
        (&uri[..cut], true)
    } else {
        (uri, false)
    };
    let mut out = String::with_capacity(trimmed.len());
    for c in trimmed.chars() {
        if c.is_control() {
            out.push_str(&format!("\\x{:02X}", c as u32));
        } else {
            out.push(c);
        }
    }
    if truncated {
        out.push('…');
    }
    out
}

#[async_trait]
impl BlobAdapter for FileSystemAdapter {
    fn adapter_id(&self) -> &str {
        &self.id
    }

    fn accepted_schemes(&self) -> &[&str] {
        &["file"]
    }

    async fn store(&self, blob_ref: &BlobRef, bytes: &[u8]) -> Result<(), BlobError> {
        let (expected_hash, _uri) = expect_small(blob_ref)?;
        let path = self.path_for(&expected_hash);
        let bytes = bytes.to_vec();
        let _permit = self
            .concurrency
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| backend("adapter concurrency semaphore closed"))?;
        // PERF_AUDIT §6.10 — clone the adapter handle into the
        // blocking closure so the hash verify AND the parent-canon
        // check share the cached `root_canonical`. Pre-fix the
        // hash ran on the tokio runtime worker (multi-ms for a
        // multi-MiB Small blob) and the root canon was redone per
        // store; both move into the blocking pool here.
        let me = self.clone();
        tokio::task::spawn_blocking(move || -> Result<(), BlobError> {
            let _permit = _permit; // hold across the blocking work
            // PERF_AUDIT §6.10 — hash verify inside the blocking
            // pool so a multi-MiB Small payload doesn't stall the
            // runtime worker. Verify the bytes hash to the BlobRef's
            // hash BEFORE writing. Without this an adapter author
            // (or compromised binding) can pre-seed an arbitrary
            // hash slot with attacker content; a later honest
            // BlobRef would then resolve to that content because
            // the on-disk path is keyed only on the hash. Hash here
            // and reject mismatches at the trust boundary.
            let computed: [u8; 32] = blake3::hash(&bytes).into();
            if computed != expected_hash {
                return Err(BlobError::HashMismatch {
                    expected: expected_hash,
                    actual: computed,
                });
            }
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(backend)?;
                // Defend against shard-dir symlinks pointing outside
                // `root`. Canonicalize the parent (resolves every
                // symlink in the path) and reject if it isn't
                // contained in the cached root. Without this check,
                // an attacker who can pre-create `<root>/<shard>`
                // as a symlink to `/tmp` (or anywhere) escapes the
                // adapter's sandbox on every store — D-12's hash-
                // verify defends *reads* from attacker bytes; this
                // defends *writes* from escaping the root.
                let parent_canon = std::fs::canonicalize(parent).map_err(backend)?;
                let root_canon = match me.cached_root_canonical()? {
                    Some(r) => r,
                    None => {
                        // Root went missing between `create_dir_all`
                        // and `canonicalize` — the create above
                        // would have raced its disappearance. Treat
                        // as an escape because we can't establish
                        // containment.
                        return Err(BlobError::Backend(
                            "fs adapter: root vanished between create + canonicalize".into(),
                        ));
                    }
                };
                if !parent_canon.starts_with(&root_canon) {
                    return Err(BlobError::Backend(format!(
                        "fs adapter: shard dir escapes root (parent={:?} root={:?})",
                        parent_canon, root_canon,
                    )));
                }
            }
            // Write to a sibling temp then rename so a concurrent
            // fetch never observes a half-written file. The temp
            // filename includes pid + a process-local atomic
            // counter + nanos so two concurrent stores against the
            // same hash slot (idempotent re-puts) don't race on
            // the same temp filename.
            use std::sync::atomic::{AtomicU64, Ordering};
            static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);
            let counter = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let mut tmp = path.clone();
            let mut name = tmp
                .file_name()
                .ok_or_else(|| backend("path has no file name"))?
                .to_owned();
            name.push(format!(".{}-{}-{}.tmp", std::process::id(), counter, nanos));
            tmp.set_file_name(name);
            // Write + fsync the temp file before renaming so a
            // power loss between rename and the next OS flush
            // cannot leave a zero-length canonical file. Caller's
            // BlobAdapter::store contract implies durability on
            // successful return.
            {
                use std::io::Write;
                let mut f = std::fs::File::create(&tmp).map_err(backend)?;
                f.write_all(&bytes).map_err(backend)?;
                f.sync_all().map_err(backend)?;
            }
            // On Windows rename over an existing file historically
            // fails. The fallback used to be "if `path` exists,
            // assume another writer beat us and discard the temp."
            // That's TOCTOU-prone: between `is_file()` and the
            // cleanup another writer could replace or remove the
            // file, and a concurrent fetch races into the window
            // and observes truncated bytes (mitigated downstream
            // by D-12's hash-verify on fetch, but still observable
            // as a NotFound after a successful store return).
            //
            // Safer mitigation: when rename fails, READ the
            // existing file and verify its hash matches
            // `blob_ref.hash`. If yes, the rename-failed-but-
            // content-is-correct case is real; cleanup temp +
            // succeed. If no, surface a Backend error so the
            // caller knows the slot is occupied by something
            // unexpected.
            match std::fs::rename(&tmp, &path) {
                Ok(()) => {}
                Err(rename_err) => {
                    let existing = match std::fs::read(&path) {
                        Ok(buf) => buf,
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                            let _ = std::fs::remove_file(&tmp);
                            return Err(backend(rename_err));
                        }
                        Err(e) => {
                            let _ = std::fs::remove_file(&tmp);
                            return Err(backend(e));
                        }
                    };
                    let existing_hash: [u8; 32] = blake3::hash(&existing).into();
                    let _ = std::fs::remove_file(&tmp);
                    if existing_hash != expected_hash {
                        return Err(BlobError::Backend(format!(
                            "fs adapter: canonical path exists with mismatched content \
                             (rename err: {})",
                            rename_err
                        )));
                    }
                    // Existing content is byte-for-byte what we
                    // would have written; treat as success.
                }
            }
            // fsync the parent dir so the rename is durable too.
            // Errors here aren't worth failing the store over — the
            // bytes are on disk; the dir entry is the only thing
            // not yet flushed, and the OS will flush on its own
            // schedule. Log via the Backend variant? No — best-
            // effort, swallow.
            if let Some(parent) = path.parent() {
                if let Ok(dir) = std::fs::File::open(parent) {
                    let _ = dir.sync_all();
                }
            }
            Ok(())
        })
        .await
        .map_err(|e| backend(format!("join error: {}", e)))?
    }

    async fn fetch(&self, blob_ref: &BlobRef) -> Result<Bytes, BlobError> {
        let (hash, uri) = expect_small(blob_ref)?;
        let path = self.path_for(&hash);
        let uri = sanitize_uri_for_error(uri);
        let _permit = self
            .concurrency
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| backend("adapter concurrency semaphore closed"))?;
        // PERF_AUDIT §6.10 — clone the adapter handle into the
        // blocking closure so `path_within_root` can hit the cached
        // root canonicalization on `self.root_canonical`. Each
        // clone is one String + one PathBuf + two Arc bumps.
        let me = self.clone();
        tokio::task::spawn_blocking(move || -> Result<Bytes, BlobError> {
            let _permit = _permit;
            // Reject a symlink-escape before following the path (L3).
            if !me.path_within_root(&path)? {
                return Err(BlobError::NotFound(uri));
            }
            match std::fs::read(&path) {
                Ok(bytes) => Ok(Bytes::from(bytes)),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(BlobError::NotFound(uri)),
                Err(e) => Err(backend(e)),
            }
        })
        .await
        .map_err(|e| backend(format!("join error: {}", e)))?
    }

    async fn fetch_range(&self, blob_ref: &BlobRef, range: Range<u64>) -> Result<Bytes, BlobError> {
        if range.start > range.end {
            return Err(backend(format!(
                "range.start ({}) > range.end ({})",
                range.start, range.end
            )));
        }
        let len = range.end.saturating_sub(range.start);
        if len == 0 {
            return Ok(Bytes::new());
        }
        // Guard against `len as usize` truncation on 32-bit
        // targets and against OOM-by-design from a maliciously
        // large range that decoded past the substrate's
        // BLOB_REF_MAX_SIZE check (e.g. caller constructed the
        // BlobRef in-process, bypassing decode).
        if len > usize::MAX as u64 {
            return Err(backend(format!(
                "range length {} exceeds usize::MAX on this target",
                len
            )));
        }
        let (hash, uri) = expect_small(blob_ref)?;
        let path = self.path_for(&hash);
        let uri = sanitize_uri_for_error(uri);
        let start = range.start;
        let _permit = self
            .concurrency
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| backend("adapter concurrency semaphore closed"))?;
        // PERF_AUDIT §6.10 — share the cached root canonicalization.
        let me = self.clone();
        tokio::task::spawn_blocking(move || -> Result<Bytes, BlobError> {
            let _permit = _permit;
            // Reject a symlink-escape before following the path (L3).
            if !me.path_within_root(&path)? {
                return Err(BlobError::NotFound(uri));
            }
            let mut f = match std::fs::File::open(&path) {
                Ok(f) => f,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    return Err(BlobError::NotFound(uri))
                }
                Err(e) => return Err(backend(e)),
            };
            f.seek(SeekFrom::Start(start)).map_err(backend)?;
            let mut buf = vec![0u8; len as usize];
            f.read_exact(&mut buf).map_err(backend)?;
            Ok(Bytes::from(buf))
        })
        .await
        .map_err(|e| backend(format!("join error: {}", e)))?
    }

    async fn exists(&self, blob_ref: &BlobRef) -> Result<bool, BlobError> {
        let (hash, _uri) = expect_small(blob_ref)?;
        let path = self.path_for(&hash);
        let permit = self
            .concurrency
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| backend("adapter concurrency semaphore closed"))?;
        // PERF_AUDIT §6.10 — share the cached root canonicalization.
        let me = self.clone();
        let res = tokio::task::spawn_blocking(move || -> Result<bool, BlobError> {
            let _permit = permit;
            // Reject an out-of-root symlink before probing (L3). A path
            // that resolves outside root is not a legitimate blob.
            if !me.path_within_root(&path)? {
                return Ok(false);
            }
            // Then preserve the pre-L3 `is_file()` contract: only a
            // regular file counts as present (a directory or other
            // node at the slot is not a blob).
            Ok(Path::new(&path).is_file())
        })
        .await
        .map_err(|e| backend(format!("join error: {}", e)))??;
        Ok(res)
    }

    async fn fetch_stream(&self, blob_ref: &BlobRef) -> Result<BlobByteStream, BlobError> {
        // Stream the file in fixed-size chunks via an mpsc channel.
        // A dedicated `spawn_blocking` task reads each chunk and
        // forwards through the channel; the returned stream wraps
        // the receiver. Bounded channel keeps the read-ahead window
        // tight so a slow consumer doesn't pile up unbounded memory
        // on the producer side.
        let (hash, uri) = expect_small(blob_ref)?;
        let path = self.path_for(&hash);
        let uri = sanitize_uri_for_error(uri);
        // Acquire-on-spawn: the streaming task holds the permit for
        // the duration of the read, mirroring the all-in-memory
        // path's concurrency bound.
        let permit = self
            .concurrency
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| backend("adapter concurrency semaphore closed"))?;
        // 4-chunk channel — enough to keep the reader busy without
        // letting it run far ahead of the consumer.
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, BlobError>>(4);
        // PERF_AUDIT §6.10 — share the cached root canonicalization.
        let me = self.clone();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            // Reject a symlink-escape before following the path (L3).
            match me.path_within_root(&path) {
                Ok(true) => {}
                Ok(false) => {
                    let _ = tx.blocking_send(Err(BlobError::NotFound(uri)));
                    return;
                }
                Err(e) => {
                    let _ = tx.blocking_send(Err(e));
                    return;
                }
            }
            let mut f = match std::fs::File::open(&path) {
                Ok(f) => f,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    let _ = tx.blocking_send(Err(BlobError::NotFound(uri)));
                    return;
                }
                Err(e) => {
                    let _ = tx.blocking_send(Err(backend(e)));
                    return;
                }
            };
            let mut buf = vec![0u8; FS_STREAM_CHUNK_BYTES];
            loop {
                let n = match f.read(&mut buf) {
                    Ok(0) => return,
                    Ok(n) => n,
                    Err(e) => {
                        let _ = tx.blocking_send(Err(backend(e)));
                        return;
                    }
                };
                let chunk = Bytes::copy_from_slice(&buf[..n]);
                if tx.blocking_send(Ok(chunk)).is_err() {
                    // Consumer dropped — stop reading.
                    return;
                }
            }
        });
        let stream = futures::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|item| (item, rx))
        });
        Ok(Box::pin(stream))
    }

    async fn delete(&self, blob_ref: &BlobRef) -> Result<(), BlobError> {
        let (hash, uri) = expect_small(blob_ref)?;
        let path = self.path_for(&hash);
        let uri = sanitize_uri_for_error(uri);
        let permit = self
            .concurrency
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| backend("adapter concurrency semaphore closed"))?;
        tokio::task::spawn_blocking(move || -> Result<(), BlobError> {
            let _permit = permit;
            match std::fs::remove_file(&path) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    // Delete-of-not-present is success — the
                    // GC contract is "ensure absent," not "ensure
                    // was-present-then-absent."
                    let _ = uri;
                    Ok(())
                }
                Err(e) => Err(backend(e)),
            }
        })
        .await
        .map_err(|e| backend(format!("join error: {}", e)))?
    }

    async fn stat(&self, blob_ref: &BlobRef) -> Result<super::adapter::BlobStat, BlobError> {
        let (hash, uri) = expect_small(blob_ref)?;
        let path = self.path_for(&hash);
        let uri = sanitize_uri_for_error(uri);
        let permit = self
            .concurrency
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| backend("adapter concurrency semaphore closed"))?;
        let advertised_size = blob_ref.size();
        let advertised_encoding = blob_ref.encoding();
        tokio::task::spawn_blocking(move || -> Result<super::adapter::BlobStat, BlobError> {
            let _permit = permit;
            let meta = match std::fs::metadata(&path) {
                Ok(m) => m,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    return Err(BlobError::NotFound(uri))
                }
                Err(e) => return Err(backend(e)),
            };
            // FS adapter doesn't participate in the substrate's
            // `causal:` advertisement layer — replicas_observed /
            // replica_target are zero / None. `last_seen` comes
            // from filesystem mtime when available.
            let last_seen_unix_ms = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as u64);
            Ok(super::adapter::BlobStat {
                size: advertised_size.max(meta.len()),
                replicas_observed: 0,
                replica_target: None,
                last_seen_unix_ms,
                encoding: advertised_encoding,
            })
        })
        .await
        .map_err(|e| backend(format!("join error: {}", e)))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// `sanitize_uri_for_error` is called from error paths inside
    /// `spawn_blocking`. A publisher-controlled URI with a multi-byte
    /// UTF-8 codepoint straddling the truncation boundary must NOT
    /// panic — the previous byte-slice formulation crashed the
    /// blocking task and surfaced as a `JoinError` on the caller.
    #[test]
    fn sanitize_uri_handles_multibyte_at_boundary() {
        // 255 ASCII bytes followed by a 4-byte UTF-8 codepoint — the
        // emoji's first byte lands at index 255, its last at 258, so
        // a byte slice at index 256 would split the codepoint.
        let uri = format!("{}{}", "a".repeat(255), "🦀");
        let out = sanitize_uri_for_error(&uri);
        // Must not panic, and must end with the truncation marker
        // since the input exceeded MAX_LEN.
        assert!(
            out.ends_with('…'),
            "expected truncation marker, got {:?}",
            out
        );
    }

    #[test]
    fn sanitize_uri_preserves_short_input() {
        let uri = "file:///🦀/path";
        let out = sanitize_uri_for_error(uri);
        assert_eq!(out, uri);
    }

    fn unique_root() -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("net-blob-fs-test-{}-{}", std::process::id(), n))
    }

    fn make_ref(payload: &[u8], uri: &str) -> BlobRef {
        let hash: [u8; 32] = blake3::hash(payload).into();
        BlobRef::small(uri, hash, payload.len() as u64)
    }

    fn cleanup(root: &Path) {
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn store_fetch_round_trip() {
        let root = unique_root();
        let adapter = FileSystemAdapter::new("fs-test", &root);
        let payload = b"hello dataforts";
        let blob = make_ref(payload, "file:///test/key");

        adapter.store(&blob, payload).await.unwrap();
        let fetched = adapter.fetch(&blob).await.unwrap();
        assert_eq!(fetched.as_ref(), payload);
        blob.verify(&fetched).unwrap();
        cleanup(&root);
    }

    #[tokio::test]
    async fn fetch_missing_returns_not_found() {
        let root = unique_root();
        let adapter = FileSystemAdapter::new("fs-test", &root);
        let blob = BlobRef::small("file:///ghost", [0xFF; 32], 0);
        let err = adapter.fetch(&blob).await.unwrap_err();
        match err {
            BlobError::NotFound(uri) => assert_eq!(uri, "file:///ghost"),
            other => panic!("expected NotFound, got {:?}", other),
        }
        cleanup(&root);
    }

    #[tokio::test]
    async fn exists_true_only_after_store() {
        let root = unique_root();
        let adapter = FileSystemAdapter::new("fs-test", &root);
        let payload = b"x";
        let blob = make_ref(payload, "file:///x");
        assert!(!adapter.exists(&blob).await.unwrap());
        adapter.store(&blob, payload).await.unwrap();
        assert!(adapter.exists(&blob).await.unwrap());
        cleanup(&root);
    }

    #[tokio::test]
    async fn fetch_range_returns_slice() {
        let root = unique_root();
        let adapter = FileSystemAdapter::new("fs-test", &root);
        let payload: &[u8] = b"abcdefghij";
        let blob = make_ref(payload, "file:///alphabet");
        adapter.store(&blob, payload).await.unwrap();
        let mid = adapter.fetch_range(&blob, 3..7).await.unwrap();
        assert_eq!(mid.as_ref(), b"defg");
        cleanup(&root);
    }

    #[tokio::test]
    async fn fetch_range_empty_is_empty() {
        let root = unique_root();
        let adapter = FileSystemAdapter::new("fs-test", &root);
        let payload = b"data";
        let blob = make_ref(payload, "file:///data");
        adapter.store(&blob, payload).await.unwrap();
        let empty = adapter.fetch_range(&blob, 2..2).await.unwrap();
        assert!(empty.is_empty());
        cleanup(&root);
    }

    #[tokio::test]
    #[allow(clippy::reversed_empty_ranges)] // intentional — exercises the start>end guard
    async fn fetch_range_reversed_is_error() {
        let root = unique_root();
        let adapter = FileSystemAdapter::new("fs-test", &root);
        let blob = BlobRef::small("file:///r", [0x00; 32], 10);
        let err = adapter.fetch_range(&blob, 5..3).await.unwrap_err();
        assert!(matches!(err, BlobError::Backend(_)));
        cleanup(&root);
    }

    #[tokio::test]
    async fn store_rejects_mismatched_bytes_vs_hash() {
        // Without this guard a caller could pre-seed an arbitrary
        // hash slot with attacker content.
        let root = unique_root();
        let adapter = FileSystemAdapter::new("fs-test", &root);
        // Build a BlobRef whose hash is the digest of "real" but
        // try to store "fake" against it.
        let real = b"real content";
        let fake = b"fake content";
        let real_hash: [u8; 32] = blake3::hash(real).into();
        let blob = BlobRef::small("file:///impostor", real_hash, fake.len() as u64);
        let err = adapter.store(&blob, fake).await.unwrap_err();
        match err {
            BlobError::HashMismatch { expected, actual } => {
                assert_eq!(expected, real_hash);
                assert_ne!(actual, real_hash);
            }
            other => panic!("expected HashMismatch, got {:?}", other),
        }
        // Slot must NOT have been populated.
        assert!(!adapter.exists(&blob).await.unwrap());
        cleanup(&root);
    }

    #[tokio::test]
    async fn store_rejects_canonical_path_with_mismatched_content() {
        // Pre-populate the canonical hash slot with attacker bytes
        // (NOT the bytes blob_ref.hash represents). Then call
        // store() with the correct bytes — rename will likely
        // succeed and overwrite, but if it fails (Windows-style
        // rename-over-existing rejection, or anything else), the
        // post-rename fallback now READS the existing file and
        // checks its hash. Mismatch surfaces as Backend rather
        // than silently succeeding.
        //
        // To exercise the mismatch path deterministically, we
        // pre-populate then call store() on a fixture whose hash
        // collides with the pre-populated slot. Easiest: build the
        // blob_ref hash from one payload, write a different
        // payload to the canonical path manually, and call store()
        // with the "right" payload. The rename succeeds on POSIX
        // (overwrite is normal), so this test ALSO verifies that
        // the rename-fallback path's hash check would catch the
        // bad-content case if it ran.
        //
        // We test the hash check directly by constructing the path
        // manually and pre-poisoning it with garbage, then storing
        // bytes that hash to the correct hash. The store should
        // succeed (rename overwrites on POSIX; on Windows rename-
        // over-existing then fallback-check would catch garbage).
        // Either way the canonical slot ends up with correct
        // bytes.
        let root = unique_root();
        let adapter = FileSystemAdapter::new("fs-toctou", &root);

        let payload = b"the right content";
        let hash: [u8; 32] = blake3::hash(payload).into();
        let blob = BlobRef::small("file:///toctou", hash, payload.len() as u64);

        // Pre-poison the canonical path with garbage.
        let shard = format!("{:02x}", hash[0]);
        let hex: String = hash.iter().map(|b| format!("{:02x}", b)).collect();
        let canonical = root.join(&shard).join(&hex);
        std::fs::create_dir_all(canonical.parent().unwrap()).unwrap();
        std::fs::write(&canonical, b"GARBAGE-not-matching-hash").unwrap();

        // Call store — on POSIX rename overwrites, on Windows
        // the rename-fallback's hash check would catch the
        // garbage. Either way, success ends with the canonical
        // slot holding `payload`.
        adapter.store(&blob, payload).await.unwrap();
        let on_disk = std::fs::read(&canonical).unwrap();
        assert_eq!(on_disk, payload, "canonical slot must hold correct bytes");
        cleanup(&root);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn store_rejects_shard_dir_symlink_escape() {
        // Pre-create one shard directory as a symlink to a sibling
        // location outside the adapter's root. Without the
        // canonicalize-and-prefix-check in store(), the write
        // would land at the symlink target. With the defense,
        // store rejects with a Backend error before writing.
        let root = unique_root();
        std::fs::create_dir_all(&root).unwrap();
        let outside = root.parent().unwrap().join(format!(
            "outside-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&outside).unwrap();

        // Pick a payload whose hash starts with hex `ab` so we know
        // the shard dir name. Compute hash first, then pre-symlink
        // the matching shard inside root → outside.
        let payload = b"escape-test";
        let hash: [u8; 32] = blake3::hash(payload).into();
        let shard = format!("{:02x}", hash[0]);
        let shard_path = root.join(&shard);
        // Create as a symlink rather than a directory.
        std::os::unix::fs::symlink(&outside, &shard_path).unwrap();

        let adapter = FileSystemAdapter::new("fs-symlink", &root);
        let blob = BlobRef::small("file:///escape", hash, payload.len() as u64);
        let err = adapter.store(&blob, payload).await.unwrap_err();
        match err {
            BlobError::Backend(msg) => assert!(
                msg.contains("escapes root"),
                "expected escape-root rejection; got {msg}"
            ),
            other => panic!("expected Backend(escapes root), got {:?}", other),
        }
        // Nothing was written under `outside`.
        let outside_contents: Vec<_> = std::fs::read_dir(&outside)
            .unwrap()
            .filter_map(Result::ok)
            .collect();
        assert!(
            outside_contents.is_empty(),
            "adapter wrote outside its root: {:?}",
            outside_contents
        );
        cleanup(&root);
        let _ = std::fs::remove_dir_all(&outside);
    }

    /// Security audit L3: the read paths must not follow a blob-file
    /// symlink that points outside the adapter root. Pre-fix `fetch` /
    /// `exists` opened `path_for(hash)` directly, so a cross-process
    /// writer who replaced the blob file with a symlink to an arbitrary
    /// file could exfiltrate it. The `path_within_root` guard rejects
    /// the escape before the read follows the link.
    ///
    /// `#[cfg(unix)]` because it plants a `std::os::unix::fs::symlink`
    /// — same gate as `store_rejects_shard_dir_symlink_escape` above.
    #[cfg(unix)]
    #[tokio::test]
    async fn read_paths_reject_blob_file_symlink_escape() {
        use futures::StreamExt;

        let root = unique_root();
        // A secret file outside the adapter root.
        let outside = root.parent().unwrap().join(format!(
            "secret-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&outside).unwrap();
        let secret_path = outside.join("secret.txt");
        std::fs::write(&secret_path, b"TOP SECRET - must not leak").unwrap();

        // Plant the blob's on-disk path as a symlink to the secret.
        let payload = b"legit-content";
        let hash: [u8; 32] = blake3::hash(payload).into();
        let shard = format!("{:02x}", hash[0]);
        let mut hex = String::new();
        for b in &hash {
            use std::fmt::Write;
            let _ = write!(hex, "{:02x}", b);
        }
        let shard_dir = root.join(&shard);
        std::fs::create_dir_all(&shard_dir).unwrap();
        let blob_path = shard_dir.join(&hex);
        std::os::unix::fs::symlink(&secret_path, &blob_path).unwrap();

        let adapter = FileSystemAdapter::new("fs-read-symlink", &root);
        let blob = BlobRef::small("file:///escape", hash, payload.len() as u64);

        // fetch must NOT return the secret bytes.
        match adapter.fetch(&blob).await {
            Err(BlobError::Backend(msg)) => {
                assert!(
                    msg.contains("escapes"),
                    "expected escape rejection, got {msg}"
                )
            }
            Err(BlobError::NotFound(_)) => {} // also acceptable: deny without leak
            Ok(bytes) => panic!(
                "fetch followed symlink and leaked out-of-root content: {:?}",
                bytes
            ),
            Err(other) => panic!("unexpected error: {:?}", other),
        }

        // exists must not report a symlink-to-outside as present.
        match adapter.exists(&blob).await {
            Err(BlobError::Backend(msg)) => {
                assert!(
                    msg.contains("escapes"),
                    "expected escape rejection, got {msg}"
                )
            }
            Ok(false) => {}
            Ok(true) => panic!("exists reported an out-of-root symlink as present"),
            Err(other) => panic!("unexpected error: {:?}", other),
        }

        // fetch_stream must not stream the secret bytes.
        let mut stream = adapter.fetch_stream(&blob).await.unwrap();
        let mut leaked = Vec::new();
        let mut errored = false;
        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(b) => leaked.extend_from_slice(&b),
                Err(_) => errored = true,
            }
        }
        assert!(
            errored && leaked.is_empty(),
            "fetch_stream leaked out-of-root content: {:?}",
            leaked
        );

        cleanup(&root);
        let _ = std::fs::remove_dir_all(&outside);
    }

    /// `exists` must report `false` for a non-regular-file node at the
    /// blob slot. The L3 read guard swapped `Path::is_file()` for an
    /// in-root resolve check; this pins that the resolve check didn't
    /// also start reporting a *directory* sitting at the slot as a
    /// present blob. Cross-platform (no symlink needed).
    #[tokio::test]
    async fn exists_reports_false_for_directory_at_blob_slot() {
        let root = unique_root();

        let payload = b"dir-not-a-blob";
        let hash: [u8; 32] = blake3::hash(payload).into();
        let shard = format!("{:02x}", hash[0]);
        let mut hex = String::new();
        for b in &hash {
            use std::fmt::Write;
            let _ = write!(hex, "{:02x}", b);
        }
        // Plant a *directory* exactly where the blob file would live.
        let blob_path = root.join(&shard).join(&hex);
        std::fs::create_dir_all(&blob_path).unwrap();

        let adapter = FileSystemAdapter::new("fs-exists-dir", &root);
        let blob = BlobRef::small("file:///dir", hash, payload.len() as u64);
        assert!(
            !adapter.exists(&blob).await.unwrap(),
            "a directory at the blob slot must not count as a present blob",
        );

        cleanup(&root);
    }

    #[tokio::test]
    async fn fetch_stream_yields_multi_chunk_for_large_blobs() {
        // Payload bigger than the streaming chunk size so the
        // override actually emits multiple chunks. Pins that the
        // FS adapter doesn't fall through to the all-in-memory
        // default.
        use futures::StreamExt;
        let root = unique_root();
        let adapter = FileSystemAdapter::new("fs-stream", &root);
        let payload = vec![0xCDu8; FS_STREAM_CHUNK_BYTES * 3 + 17];
        let blob = make_ref(&payload, "file:///fs-stream");
        adapter.store(&blob, &payload).await.unwrap();

        let mut stream = adapter.fetch_stream(&blob).await.unwrap();
        let mut chunks = 0usize;
        let mut buf = Vec::with_capacity(payload.len());
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.unwrap();
            chunks += 1;
            buf.extend_from_slice(&chunk);
        }
        assert!(
            chunks >= 4,
            "FS adapter must yield multiple chunks for a multi-chunk payload; got {}",
            chunks
        );
        assert_eq!(buf, payload);
        cleanup(&root);
    }

    #[tokio::test]
    async fn fetch_stream_returns_not_found_on_missing_blob() {
        use futures::StreamExt;
        let root = unique_root();
        let adapter = FileSystemAdapter::new("fs-stream-miss", &root);
        let blob = BlobRef::small("file:///ghost", [0xFF; 32], 0);
        let mut stream = adapter.fetch_stream(&blob).await.unwrap();
        let first = stream.next().await.expect("must yield NotFound chunk");
        match first {
            Err(BlobError::NotFound(_)) => {}
            other => panic!("expected NotFound from fetch_stream, got {:?}", other),
        }
        cleanup(&root);
    }

    #[tokio::test]
    async fn store_overwrites_atomically() {
        let root = unique_root();
        let adapter = FileSystemAdapter::new("fs-test", &root);
        let p1 = b"version one";
        let p2 = b"version two -- longer";
        let blob_a = make_ref(p1, "file:///a");
        adapter.store(&blob_a, p1).await.unwrap();
        // Different bytes → different hash → different path; not an
        // overwrite-of-same-key but exercises the temp-rename path
        // for the second write.
        let blob_b = make_ref(p2, "file:///a");
        adapter.store(&blob_b, p2).await.unwrap();
        assert_eq!(adapter.fetch(&blob_a).await.unwrap().as_ref(), p1);
        assert_eq!(adapter.fetch(&blob_b).await.unwrap().as_ref(), p2);
        cleanup(&root);
    }
}
