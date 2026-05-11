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

/// Default cap on concurrent FS adapter spawn_blocking tasks. A
/// burst of stores against the default tokio blocking pool (512
/// threads) would otherwise starve other blocking work in the
/// process (RedEX writes, replication, etc.).
pub const DEFAULT_FS_ADAPTER_CONCURRENCY: usize = 64;

/// Filesystem-backed blob adapter. Content-addressed by BLAKE3 hash
/// under a caller-supplied root directory.
#[derive(Debug, Clone)]
pub struct FileSystemAdapter {
    id: String,
    root: PathBuf,
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
            concurrency: Arc::new(Semaphore::new(DEFAULT_FS_ADAPTER_CONCURRENCY)),
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
    let trimmed = if uri.len() > MAX_LEN {
        &uri[..MAX_LEN]
    } else {
        uri
    };
    let mut out = String::with_capacity(trimmed.len());
    for c in trimmed.chars() {
        if c.is_control() {
            out.push_str(&format!("\\x{:02X}", c as u32));
        } else {
            out.push(c);
        }
    }
    if uri.len() > MAX_LEN {
        out.push_str("…");
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
        // Verify the bytes hash to the BlobRef's hash BEFORE writing.
        // Without this an adapter author (or compromised binding) can
        // pre-seed an arbitrary hash slot with attacker content; a
        // later honest BlobRef would then resolve to that content
        // because the on-disk path is keyed only on the hash. Hash
        // here and reject mismatches at the trust boundary.
        let computed: [u8; 32] = blake3::hash(bytes).into();
        if computed != blob_ref.hash {
            return Err(BlobError::HashMismatch {
                expected: blob_ref.hash,
                actual: computed,
            });
        }
        let path = self.path_for(&blob_ref.hash);
        let bytes = bytes.to_vec();
        let _permit = self.concurrency.clone().acquire_owned().await
            .map_err(|_| backend("adapter concurrency semaphore closed"))?;
        tokio::task::spawn_blocking(move || -> Result<(), BlobError> {
            let _permit = _permit; // hold across the blocking work
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(backend)?;
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
            // fails; treat that as a benign "another writer beat us"
            // and clean up the temp (the canonical path already
            // holds the same content thanks to the hash check).
            match std::fs::rename(&tmp, &path) {
                Ok(()) => {}
                Err(_) if path.is_file() => {
                    let _ = std::fs::remove_file(&tmp);
                }
                Err(e) => return Err(backend(e)),
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

    async fn fetch(&self, blob_ref: &BlobRef) -> Result<Vec<u8>, BlobError> {
        let path = self.path_for(&blob_ref.hash);
        let uri = sanitize_uri_for_error(&blob_ref.uri);
        let _permit = self.concurrency.clone().acquire_owned().await
            .map_err(|_| backend("adapter concurrency semaphore closed"))?;
        tokio::task::spawn_blocking(move || -> Result<Vec<u8>, BlobError> {
            let _permit = _permit;
            match std::fs::read(&path) {
                Ok(bytes) => Ok(bytes),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(BlobError::NotFound(uri)),
                Err(e) => Err(backend(e)),
            }
        })
        .await
        .map_err(|e| backend(format!("join error: {}", e)))?
    }

    async fn fetch_range(
        &self,
        blob_ref: &BlobRef,
        range: Range<u64>,
    ) -> Result<Vec<u8>, BlobError> {
        if range.start > range.end {
            return Err(backend(format!(
                "range.start ({}) > range.end ({})",
                range.start, range.end
            )));
        }
        let len = range.end.saturating_sub(range.start);
        if len == 0 {
            return Ok(Vec::new());
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
        let path = self.path_for(&blob_ref.hash);
        let uri = sanitize_uri_for_error(&blob_ref.uri);
        let start = range.start;
        let _permit = self.concurrency.clone().acquire_owned().await
            .map_err(|_| backend("adapter concurrency semaphore closed"))?;
        tokio::task::spawn_blocking(move || -> Result<Vec<u8>, BlobError> {
            let _permit = _permit;
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
            Ok(buf)
        })
        .await
        .map_err(|e| backend(format!("join error: {}", e)))?
    }

    async fn exists(&self, blob_ref: &BlobRef) -> Result<bool, BlobError> {
        let path = self.path_for(&blob_ref.hash);
        let permit = self.concurrency.clone().acquire_owned().await
            .map_err(|_| backend("adapter concurrency semaphore closed"))?;
        let res = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            Path::new(&path).is_file()
        })
            .await
            .map_err(|e| backend(format!("join error: {}", e)))?;
        Ok(res)
    }

    async fn fetch_stream(&self, blob_ref: &BlobRef) -> Result<BlobByteStream, BlobError> {
        // Stream the file in fixed-size chunks via an mpsc channel.
        // A dedicated `spawn_blocking` task reads each chunk and
        // forwards through the channel; the returned stream wraps
        // the receiver. Bounded channel keeps the read-ahead window
        // tight so a slow consumer doesn't pile up unbounded memory
        // on the producer side.
        let path = self.path_for(&blob_ref.hash);
        let uri = sanitize_uri_for_error(&blob_ref.uri);
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
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn unique_root() -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("net-blob-fs-test-{}-{}", std::process::id(), n))
    }

    fn make_ref(payload: &[u8], uri: &str) -> BlobRef {
        let hash: [u8; 32] = blake3::hash(payload).into();
        BlobRef::new(uri, hash, payload.len() as u64)
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
        assert_eq!(fetched, payload);
        blob.verify(&fetched).unwrap();
        cleanup(&root);
    }

    #[tokio::test]
    async fn fetch_missing_returns_not_found() {
        let root = unique_root();
        let adapter = FileSystemAdapter::new("fs-test", &root);
        let blob = BlobRef::new("file:///ghost", [0xFF; 32], 0);
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
        assert_eq!(mid, b"defg");
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
        let blob = BlobRef::new("file:///r", [0x00; 32], 10);
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
        let blob = BlobRef::new("file:///impostor", real_hash, fake.len() as u64);
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
        let blob = BlobRef::new("file:///ghost", [0xFF; 32], 0);
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
        assert_eq!(adapter.fetch(&blob_a).await.unwrap(), p1);
        assert_eq!(adapter.fetch(&blob_b).await.unwrap(), p2);
        cleanup(&root);
    }
}
