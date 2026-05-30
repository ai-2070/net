//! Directory transfer over the blob layer (federation phase 2).
//!
//! The [`BlobAdapter`](super::blob::BlobAdapter) moves opaque bytes; a
//! directory has structure. This thin wrapper bridges the two:
//!
//! - [`store_dir`] walks a source tree, stores **each file as its own
//!   content-addressed blob** (one [`BlobRef`] per file — a per-file
//!   substrate operation, the architectural property the demo makes
//!   visible), records the tree shape (relative path, mode, symlink
//!   target) in a [`DirManifest`], stores the manifest itself as a
//!   blob, and returns one root [`BlobRef`] for the whole directory.
//! - [`fetch_dir`] fetches the manifest, then fetches each leaf blob —
//!   **each leaf is an independent fetch through the adapter**, which
//!   on a cross-peer adapter routes to whichever peer advertises the
//!   chunk (federation S-2) — and reconstructs the tree with correct
//!   paths, modes, and symlinks. Leaf fetches run with bounded-high
//!   concurrency.
//!
//! Content addressing makes dedup automatic: two files with identical
//! bytes resolve to the same blob and store/transfer once.
//!
//! # Safety
//!
//! Manifest paths can come from a peer, so [`fetch_dir`] validates
//! every entry path stays within the destination (no absolute paths,
//! no `..` traversal, no drive prefixes) before touching the
//! filesystem.
//!
//! # Portability
//!
//! Unix file modes and symlinks are preserved where the platform
//! supports them; on other platforms modes are recorded but not
//! reapplied and symlink creation is best-effort.

use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::blob::{chunk_payload, BlobAdapter, BlobError, BlobRef, Encoding};

/// URI stamped on directory-layer blobs. Opaque to the mesh adapter
/// (the content hash is the authoritative address); the scheme just
/// routes to the `mesh://` adapter.
const DIR_BLOB_URI: &str = "mesh://dir";

/// Default in-flight leaf-fetch concurrency for [`fetch_dir`]. High
/// enough to keep the UDP-multiplexed transport busy across many
/// per-file operations, bounded so a huge tree doesn't open unbounded
/// fetches at once. Tunable via [`fetch_dir_with_concurrency`].
pub const DEFAULT_DIR_FETCH_CONCURRENCY: usize = 64;

/// Typed failure surface for directory transfer.
#[derive(Debug)]
pub enum DirError {
    /// Local filesystem I/O error (walk, read, create, write).
    Io(String),
    /// Underlying blob store/fetch error.
    Blob(BlobError),
    /// Manifest encode/decode failure.
    Manifest(String),
    /// A manifest entry's path was unsafe (absolute / `..` / drive
    /// prefix) — rejected before any filesystem write.
    UnsafePath(String),
}

impl std::fmt::Display for DirError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(m) => write!(f, "dir io error: {m}"),
            Self::Blob(e) => write!(f, "dir blob error: {e}"),
            Self::Manifest(m) => write!(f, "dir manifest error: {m}"),
            Self::UnsafePath(p) => write!(f, "dir unsafe manifest path: {p}"),
        }
    }
}

impl std::error::Error for DirError {}

impl From<BlobError> for DirError {
    fn from(e: BlobError) -> Self {
        Self::Blob(e)
    }
}

fn io<E: std::fmt::Display>(e: E) -> DirError {
    DirError::Io(e.to_string())
}

/// One node of a transferred directory tree. Paths are relative to the
/// tree root and use `/` separators regardless of platform.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DirNode {
    /// A directory.
    Dir {
        /// Unix permission bits (0 on platforms that don't model them).
        mode: u32,
    },
    /// A regular file whose content is a content-addressed blob.
    File {
        /// Encoded [`BlobRef`] (`BlobRef::encode`) for the file content.
        blob: Vec<u8>,
        /// File byte length.
        size: u64,
        /// Unix permission bits (0 on platforms that don't model them).
        mode: u32,
    },
    /// A symbolic link.
    Symlink {
        /// Link target verbatim, as stored on the source.
        target: String,
    },
}

/// One manifest entry: a relative path and its node.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirEntry {
    /// Relative path from the tree root, `/`-separated.
    pub path: String,
    /// The node at that path.
    pub node: DirNode,
}

/// The serialized shape of a directory tree. Stored as a blob; its
/// [`BlobRef`] is the root handle [`store_dir`] returns.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirManifest {
    /// Entries sorted by path (so directories sort before their
    /// children — parents are created first on reconstruction).
    pub entries: Vec<DirEntry>,
}

/// Store `bytes` as a content-addressed blob and return its
/// [`BlobRef`]. Picks `Small` (≤ one chunk) or `Manifest` (chunked)
/// automatically via [`chunk_payload`].
async fn store_bytes(adapter: &dyn BlobAdapter, bytes: &[u8]) -> Result<BlobRef, DirError> {
    let blob_ref = chunk_payload(bytes)?.into_blob_ref(DIR_BLOB_URI, Encoding::Replicated)?;
    adapter.store(&blob_ref, bytes).await?;
    Ok(blob_ref)
}

/// Unix permission bits for `meta`, or 0 where the platform doesn't
/// model them.
fn mode_of(meta: &std::fs::Metadata) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode()
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        0
    }
}

/// Render a relative path with `/` separators for the manifest.
fn rel_to_string(rel: &Path) -> String {
    rel.components()
        .filter_map(|c| match c {
            Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// Walk `root` and store every file, building the manifest entries.
/// Iterative (explicit stack) to avoid boxing an async recursion.
async fn walk_tree(
    adapter: &dyn BlobAdapter,
    root: &Path,
    entries: &mut Vec<DirEntry>,
) -> Result<(), DirError> {
    let mut stack: Vec<PathBuf> = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for ent in std::fs::read_dir(&dir).map_err(io)? {
            let ent = ent.map_err(io)?;
            let path = ent.path();
            let rel = path.strip_prefix(root).map_err(io)?;
            let rel_str = rel_to_string(rel);
            if rel_str.is_empty() {
                continue;
            }
            // `symlink_metadata` so a symlink isn't followed.
            let meta = std::fs::symlink_metadata(&path).map_err(io)?;
            let ft = meta.file_type();
            if ft.is_symlink() {
                let target = std::fs::read_link(&path).map_err(io)?;
                entries.push(DirEntry {
                    path: rel_str,
                    node: DirNode::Symlink {
                        target: target.to_string_lossy().into_owned(),
                    },
                });
            } else if ft.is_dir() {
                entries.push(DirEntry {
                    path: rel_str,
                    node: DirNode::Dir {
                        mode: mode_of(&meta),
                    },
                });
                stack.push(path);
            } else if ft.is_file() {
                let bytes = std::fs::read(&path).map_err(io)?;
                let blob_ref = store_bytes(adapter, &bytes).await?;
                entries.push(DirEntry {
                    path: rel_str,
                    node: DirNode::File {
                        blob: blob_ref.encode(),
                        size: bytes.len() as u64,
                        mode: mode_of(&meta),
                    },
                });
            }
            // Other node kinds (sockets, fifos, devices) are skipped.
        }
    }
    Ok(())
}

/// Walk the directory tree at `root`, store every file as its own
/// content-addressed blob, store a [`DirManifest`] of the tree shape,
/// and return the root [`BlobRef`] (the manifest blob) representing the
/// whole directory.
pub async fn store_dir(adapter: &dyn BlobAdapter, root: &Path) -> Result<BlobRef, DirError> {
    let mut entries = Vec::new();
    walk_tree(adapter, root, &mut entries).await?;
    // Sort by path so directories precede their children on
    // reconstruction and the manifest is deterministic.
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    let manifest = DirManifest { entries };
    let manifest_bytes =
        postcard::to_allocvec(&manifest).map_err(|e| DirError::Manifest(e.to_string()))?;
    store_bytes(adapter, &manifest_bytes).await
}

/// Join `rel` (a manifest path) onto `dest`, rejecting any component
/// that would escape `dest` (absolute, `..`, drive/root prefix). The
/// returned path is always within `dest`.
fn safe_join(dest: &Path, rel: &str) -> Result<PathBuf, DirError> {
    let mut out = dest.to_path_buf();
    for seg in rel.split('/') {
        if seg.is_empty() || seg == "." {
            continue;
        }
        // Reject traversal and any segment the OS would treat as a
        // root / prefix component.
        if seg == ".." {
            return Err(DirError::UnsafePath(rel.to_string()));
        }
        let comp = Path::new(seg);
        if comp.components().any(|c| {
            matches!(
                c,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        }) {
            return Err(DirError::UnsafePath(rel.to_string()));
        }
        out.push(seg);
    }
    Ok(out)
}

/// Apply Unix `mode` to `path` (no-op on non-Unix / mode 0).
fn apply_mode(path: &Path, mode: u32) -> Result<(), DirError> {
    #[cfg(unix)]
    {
        if mode != 0 {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).map_err(io)?;
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
    }
    Ok(())
}

/// Create a symlink at `link` pointing to `target` (best-effort on
/// platforms requiring privileges).
fn make_symlink(target: &str, link: &Path) -> Result<(), DirError> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link).map_err(io)
    }
    #[cfg(windows)]
    {
        // Heuristic: if the resolved target is an existing dir, make a
        // dir symlink, else a file symlink. Requires privilege; surface
        // the error rather than silently skipping.
        let resolved = link.parent().map(|p| p.join(target));
        let is_dir = resolved.as_deref().map(Path::is_dir).unwrap_or(false);
        if is_dir {
            std::os::windows::fs::symlink_dir(target, link).map_err(io)
        } else {
            std::os::windows::fs::symlink_file(target, link).map_err(io)
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (target, link);
        Err(DirError::Io("symlinks unsupported on this platform".into()))
    }
}

/// Reconstruct the directory tree rooted at `manifest_ref` under
/// `dest`, fetching each leaf blob through `adapter` with the default
/// concurrency.
pub async fn fetch_dir(
    adapter: &dyn BlobAdapter,
    manifest_ref: &BlobRef,
    dest: &Path,
) -> Result<(), DirError> {
    fetch_dir_with_concurrency(adapter, manifest_ref, dest, DEFAULT_DIR_FETCH_CONCURRENCY).await
}

/// [`fetch_dir`] with an explicit in-flight leaf-fetch concurrency.
pub async fn fetch_dir_with_concurrency(
    adapter: &dyn BlobAdapter,
    manifest_ref: &BlobRef,
    dest: &Path,
    concurrency: usize,
) -> Result<(), DirError> {
    use futures::stream::{self, StreamExt};

    let manifest_bytes = adapter.fetch(manifest_ref).await?;
    let manifest: DirManifest =
        postcard::from_bytes(&manifest_bytes).map_err(|e| DirError::Manifest(e.to_string()))?;

    std::fs::create_dir_all(dest).map_err(io)?;

    // Pass 1 — directories, shallow-to-deep (sorted path order), so a
    // parent exists before its children.
    for entry in &manifest.entries {
        if let DirNode::Dir { mode } = &entry.node {
            let p = safe_join(dest, &entry.path)?;
            std::fs::create_dir_all(&p).map_err(io)?;
            apply_mode(&p, *mode)?;
        }
    }

    // Pass 2 — files, fetched as independent per-file substrate
    // operations with bounded-high concurrency. Each leaf routes
    // through the adapter (cross-peer on a mesh adapter).
    let files: Vec<(PathBuf, Vec<u8>, u32)> = manifest
        .entries
        .iter()
        .filter_map(|e| match &e.node {
            DirNode::File { blob, mode, .. } => {
                Some((safe_join(dest, &e.path), blob.clone(), *mode))
            }
            _ => None,
        })
        .map(|(p, blob, mode)| p.map(|p| (p, blob, mode)))
        .collect::<Result<_, _>>()?;

    let concurrency = concurrency.max(1);
    let results: Vec<Result<(), DirError>> = stream::iter(files)
        .map(|(path, blob_bytes, mode)| async move {
            let blob_ref = BlobRef::decode(&blob_bytes)
                .map_err(DirError::Blob)?
                .ok_or_else(|| DirError::Manifest("file entry carried a non-blob ref".into()))?;
            let bytes = adapter.fetch(&blob_ref).await?;
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(io)?;
            }
            std::fs::write(&path, &bytes).map_err(io)?;
            apply_mode(&path, mode)?;
            Ok(())
        })
        .buffer_unordered(concurrency)
        .collect()
        .await;
    for r in results {
        r?;
    }

    // Pass 3 — symlinks, after their targets exist.
    for entry in &manifest.entries {
        if let DirNode::Symlink { target } = &entry.node {
            let p = safe_join(dest, &entry.path)?;
            make_symlink(target, &p)?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_join_rejects_traversal_and_absolute() {
        let dest = Path::new("/tmp/dest");
        assert!(safe_join(dest, "../escape").is_err());
        assert!(safe_join(dest, "a/../../escape").is_err());
        // A normal nested path is fine and stays under dest.
        let ok = safe_join(dest, "a/b/c.txt").unwrap();
        assert!(ok.starts_with(dest));
        assert!(ok.ends_with("c.txt"));
    }

    #[test]
    fn rel_to_string_uses_forward_slashes() {
        let p: PathBuf = ["a", "b", "c.txt"].iter().collect();
        assert_eq!(rel_to_string(&p), "a/b/c.txt");
    }

    #[test]
    fn manifest_round_trips_postcard() {
        let m = DirManifest {
            entries: vec![
                DirEntry {
                    path: "d".into(),
                    node: DirNode::Dir { mode: 0o755 },
                },
                DirEntry {
                    path: "d/f.txt".into(),
                    node: DirNode::File {
                        blob: vec![1, 2, 3],
                        size: 3,
                        mode: 0o644,
                    },
                },
                DirEntry {
                    path: "d/link".into(),
                    node: DirNode::Symlink {
                        target: "f.txt".into(),
                    },
                },
            ],
        };
        let decoded: DirManifest =
            postcard::from_bytes(&postcard::to_allocvec(&m).unwrap()).unwrap();
        assert_eq!(m, decoded);
    }

    /// Local round trip: store a small tree through a single in-memory
    /// `MeshBlobAdapter`, fetch it back to a new dir, and compare every
    /// file byte-for-byte plus the directory shape.
    #[tokio::test]
    async fn store_then_fetch_dir_round_trip_local() {
        use super::super::blob::MeshBlobAdapter;
        use crate::adapter::net::redex::Redex;
        use std::sync::Arc;

        let tmp = std::env::temp_dir().join(format!("net-dir-test-{}", std::process::id()));
        let src = tmp.join("src");
        let dst = tmp.join("dst");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(src.join("a/b")).unwrap();
        std::fs::write(src.join("root.txt"), b"root file").unwrap();
        std::fs::write(src.join("a/one.bin"), vec![7u8; 5000]).unwrap();
        std::fs::write(src.join("a/b/two.txt"), b"nested deeper").unwrap();
        // Duplicate content — content addressing should dedup.
        std::fs::write(src.join("a/dup.txt"), b"root file").unwrap();

        let redex = Arc::new(Redex::new());
        let adapter = Arc::new(MeshBlobAdapter::new("dir-test", redex));

        let root_ref = store_dir(adapter.as_ref(), &src).await.expect("store_dir");
        fetch_dir(adapter.as_ref(), &root_ref, &dst)
            .await
            .expect("fetch_dir");

        for rel in ["root.txt", "a/one.bin", "a/b/two.txt", "a/dup.txt"] {
            let want = std::fs::read(src.join(rel)).unwrap();
            let got = std::fs::read(dst.join(rel)).unwrap();
            assert_eq!(want, got, "file {rel} must match byte-for-byte");
        }
        assert!(dst.join("a/b").is_dir(), "nested dirs reconstructed");

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
