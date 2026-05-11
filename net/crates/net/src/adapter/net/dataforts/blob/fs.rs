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

use async_trait::async_trait;

use super::adapter::BlobAdapter;
use super::blob_ref::BlobRef;
use super::error::BlobError;

/// Filesystem-backed blob adapter. Content-addressed by BLAKE3 hash
/// under a caller-supplied root directory.
#[derive(Debug, Clone)]
pub struct FileSystemAdapter {
    id: String,
    root: PathBuf,
}

impl FileSystemAdapter {
    /// Construct an adapter rooted at `root`. The directory is
    /// created on the first `store` if absent; `fetch` against an
    /// unprepared root surfaces `BlobError::NotFound`.
    pub fn new(id: impl Into<String>, root: impl Into<PathBuf>) -> Self {
        Self {
            id: id.into(),
            root: root.into(),
        }
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
        tokio::task::spawn_blocking(move || -> Result<(), BlobError> {
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
            std::fs::write(&tmp, &bytes).map_err(backend)?;
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
            Ok(())
        })
        .await
        .map_err(|e| backend(format!("join error: {}", e)))?
    }

    async fn fetch(&self, blob_ref: &BlobRef) -> Result<Vec<u8>, BlobError> {
        let path = self.path_for(&blob_ref.hash);
        let uri = blob_ref.uri.clone();
        tokio::task::spawn_blocking(move || -> Result<Vec<u8>, BlobError> {
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
        let uri = blob_ref.uri.clone();
        let start = range.start;
        tokio::task::spawn_blocking(move || -> Result<Vec<u8>, BlobError> {
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
        let res = tokio::task::spawn_blocking(move || Path::new(&path).is_file())
            .await
            .map_err(|e| backend(format!("join error: {}", e)))?;
        Ok(res)
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
