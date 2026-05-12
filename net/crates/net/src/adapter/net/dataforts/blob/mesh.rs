//! `MeshBlobAdapter` — mesh-native blob storage adapter that uses
//! [`Redex`] as the underlying content-addressed store.
//!
//! Each blob chunk (or whole Small blob) is stored as a single-event
//! `RedexFile` at channel name `dataforts/blob/<hex32>` where `hex32`
//! is the chunk's BLAKE3 hash. Content-addressing makes the storage
//! layer trivially deduplicated — two writes of identical bytes
//! resolve to the same channel and are idempotent.
//!
//! The adapter is registered under the `mesh://` URI scheme. The URI
//! itself is opaque to the adapter (the content hash is the
//! authoritative address); operators conventionally pass
//! `mesh://<hex32>` for human-readable wire traces, but any
//! `mesh://*` URI works.
//!
//! # Manifest dispatch
//!
//! - [`BlobRef::Small`] — bytes live in a single chunk file. `store`
//!   writes the file, `fetch` reads it back.
//! - [`BlobRef::Manifest`] — `store` decomposes the input via
//!   [`chunk_payload`], writes each chunk as its own content-addressed
//!   `RedexFile`, and verifies the supplied chunk list against the
//!   recomputed chunks. `fetch` walks the manifest's `chunks` field
//!   and concatenates each chunk's bytes. `fetch_range` uses
//!   [`byte_range_to_chunks`] to only read the chunks the requested
//!   byte range covers.
//!
//! # What this adapter is NOT (yet, v0.2 PR-2a)
//!
//! - **Replication wiring is opt-in but un-tested in this PR.** The
//!   adapter constructor takes an optional [`ReplicationConfig`];
//!   when supplied, every per-chunk `RedexFile` opens with that
//!   config. Cross-node replication of blob chunks is therefore
//!   already plumbed through to RedEX's existing replication runtime
//!   — but the e2e mesh integration (a peer fetching a blob via
//!   `causal:<hex>` advertisement) lands in a follow-up.
//! - **No GC / refcount / pinning.** PR-4's scope per the plan.
//! - **No `blob-storage-unhealthy` health-gate tag emission.**
//!   Adapter doesn't advertise capabilities itself — that surface
//!   lands with the capability extension in PR-2b.
//! - **`stat::replicas_observed`** comes back as `0` until the
//!   mesh-side advertisement integration lands; `replica_target`
//!   reflects the operator's `ReplicationConfig::factor` when set.

use std::sync::Arc;

use async_trait::async_trait;

use super::adapter::{BlobAdapter, BlobStat};
use super::blob_ref::{
    byte_range_to_chunks, chunk_payload, BlobRef, ChunkRef, ChunkedPayload, Encoding,
};
use super::error::BlobError;
use crate::adapter::net::channel::ChannelName;
use crate::adapter::net::redex::{Redex, RedexFileConfig, ReplicationConfig};

/// Per-chunk storage channel prefix. Each blob chunk lives at
/// `dataforts/blob/<hex32>` keyed on its BLAKE3 hash.
const CHUNK_CHANNEL_PREFIX: &str = "dataforts/blob/";

/// `mesh://`-scheme adapter that stores chunks as content-addressed
/// [`RedexFile`](crate::adapter::net::redex::RedexFile)s. See the
/// module-level docs for the dispatch shape.
#[derive(Clone)]
pub struct MeshBlobAdapter {
    id: String,
    redex: Arc<Redex>,
    /// Whether per-chunk files persist to disk. Defaults to `false`
    /// (in-memory chunks; chunks vanish on process restart). Set
    /// via [`Self::with_persistent`] in production deployments.
    /// Requires `Redex::with_persistent_dir(...)` to have been
    /// configured on the underlying handle — without it, the
    /// per-chunk open surfaces a typed `RedexError`.
    persistent: bool,
    /// Optional per-chunk replication config. `None` keeps chunks
    /// single-node; `Some(_)` arms each per-chunk file with the
    /// existing RedEX replication runtime. Wiring `Redex::enable_replication(mesh)`
    /// is the operator's responsibility — without it, chunks open
    /// with replication set but the runtime fails to spawn (typed
    /// `RedexError`).
    replication: Option<ReplicationConfig>,
}

impl MeshBlobAdapter {
    /// Construct a mesh-native adapter rooted at `redex`. Chunks are
    /// stored as in-memory `RedexFile`s by default — call
    /// [`Self::with_persistent`] to write to disk (requires the
    /// underlying `Redex` to be configured with a persistent dir),
    /// and / or [`Self::with_replication`] to opt every per-chunk
    /// file into the cross-node replication runtime.
    pub fn new(id: impl Into<String>, redex: Arc<Redex>) -> Self {
        Self {
            id: id.into(),
            redex,
            persistent: false,
            replication: None,
        }
    }

    /// Opt every per-chunk file into disk persistence. Default is
    /// in-memory; switch on for production deployments that want
    /// blob chunks to survive process restart.
    pub fn with_persistent(mut self, persistent: bool) -> Self {
        self.persistent = persistent;
        self
    }

    /// Per-chunk replication config applied to every newly-opened
    /// chunk file. Requires `Redex::enable_replication(mesh)` to
    /// have been called on the underlying handle; the per-chunk
    /// open surfaces a typed `RedexError` if not.
    pub fn with_replication(mut self, cfg: ReplicationConfig) -> Self {
        self.replication = Some(cfg);
        self
    }

    /// Channel name for a given chunk hash. Pure function; safe to
    /// inline.
    fn chunk_channel(hash: &[u8; 32]) -> ChannelName {
        let mut name = String::with_capacity(CHUNK_CHANNEL_PREFIX.len() + 64);
        name.push_str(CHUNK_CHANNEL_PREFIX);
        for b in hash {
            use std::fmt::Write;
            let _ = write!(name, "{:02x}", b);
        }
        ChannelName::new(&name).expect("hex-formatted name under reserved prefix is always valid")
    }

    /// `RedexFileConfig` template applied to every chunk open. The
    /// operator opts into disk persistence via [`Self::with_persistent`]
    /// and into cross-node replication via [`Self::with_replication`].
    fn chunk_file_config(&self) -> RedexFileConfig {
        let mut cfg = RedexFileConfig::new().with_persistent(self.persistent);
        if let Some(rep) = self.replication.clone() {
            cfg = cfg.with_replication(Some(rep));
        }
        cfg
    }

    /// Store a single chunk. Idempotent — if the chunk file already
    /// holds content (re-store of identical bytes against the same
    /// content-address), this is a no-op. Verifies the bytes hash
    /// to the supplied hash before writing.
    async fn store_chunk(&self, hash: &[u8; 32], bytes: &[u8]) -> Result<(), BlobError> {
        // Defensive: verify the supplied bytes hash to the supplied
        // hash. The substrate-side `store` already verified at the
        // top of the call; this is a second-pass guard in case
        // this helper is called from a non-substrate path.
        let computed: [u8; 32] = blake3::hash(bytes).into();
        if computed != *hash {
            return Err(BlobError::HashMismatch {
                expected: *hash,
                actual: computed,
            });
        }
        let channel = Self::chunk_channel(hash);
        let cfg = self.chunk_file_config();
        let file = self
            .redex
            .open_file(&channel, cfg)
            .map_err(|e| BlobError::Backend(format!("mesh blob: open chunk file: {}", e)))?;
        // Idempotent-store gate: content-addressed, so if any bytes
        // are already there they must be byte-for-byte equal. Skip
        // the append to avoid stacking duplicates in the RedEX file.
        if !file.is_empty() {
            return Ok(());
        }
        file.append(bytes)
            .map_err(|e| BlobError::Backend(format!("mesh blob: append chunk: {}", e)))?;
        Ok(())
    }

    /// Fetch a single chunk by hash. Returns `BlobError::NotFound`
    /// when the chunk file is absent or empty.
    async fn fetch_chunk(&self, hash: &[u8; 32]) -> Result<Vec<u8>, BlobError> {
        let channel = Self::chunk_channel(hash);
        let cfg = self.chunk_file_config();
        let file = self
            .redex
            .open_file(&channel, cfg)
            .map_err(|e| BlobError::Backend(format!("mesh blob: open chunk file: {}", e)))?;
        let len = file.len() as u64;
        if len == 0 {
            return Err(BlobError::NotFound(format!("mesh://{}", hex32(hash))));
        }
        // Chunks are content-addressed single-event files; read seq 0.
        // Future variations (heat-sourced replicas with multi-event
        // append history) would walk the chain — out of scope here.
        let events = file.read_range(0, len);
        let first = events
            .into_iter()
            .next()
            .ok_or_else(|| BlobError::NotFound(format!("mesh://{}", hex32(hash))))?;
        let bytes = first.payload.to_vec();
        // Defense-in-depth verification — a corrupted on-disk chunk
        // shouldn't propagate silently. The substrate verifies
        // `BlobRef`-level hashes at higher layers, but per-chunk
        // verify catches the manifest-fan-out case where any single
        // bad chunk corrupts the assembled output.
        let computed: [u8; 32] = blake3::hash(&bytes).into();
        if computed != *hash {
            return Err(BlobError::HashMismatch {
                expected: *hash,
                actual: computed,
            });
        }
        Ok(bytes)
    }
}

#[async_trait]
impl BlobAdapter for MeshBlobAdapter {
    fn adapter_id(&self) -> &str {
        &self.id
    }

    fn accepted_schemes(&self) -> &[&str] {
        &["mesh"]
    }

    async fn store(&self, blob_ref: &BlobRef, bytes: &[u8]) -> Result<(), BlobError> {
        match blob_ref {
            BlobRef::Small { hash, size, .. } => {
                // Size guard — caller may have stamped a wrong size
                // before publishing. Reject rather than silently
                // accept truncated content.
                if *size != bytes.len() as u64 {
                    return Err(BlobError::Backend(format!(
                        "mesh blob: Small size mismatch: declared {}, actual {}",
                        size,
                        bytes.len()
                    )));
                }
                self.store_chunk(hash, bytes).await
            }
            BlobRef::Manifest {
                chunks,
                total_size,
                encoding,
                ..
            } => {
                // Reject ReedSolomon at v0.2 — the encoding tag is
                // reserved on the wire for forward-compat; the
                // store path doesn't actually compute parity chunks.
                if !matches!(encoding, Encoding::Replicated) {
                    return Err(BlobError::Backend(format!(
                        "mesh blob: encoding {:?} is reserved for v0.3 and \
                         not supported by the v0.2 store path",
                        encoding
                    )));
                }
                if *total_size != bytes.len() as u64 {
                    return Err(BlobError::Backend(format!(
                        "mesh blob: Manifest total_size mismatch: declared {}, actual {}",
                        total_size,
                        bytes.len()
                    )));
                }
                // Re-chunk the input and verify the resulting hash
                // list matches what the BlobRef advertises. A
                // caller that constructed a Manifest by hand with
                // hashes that don't match the bytes can't poison
                // the store.
                let recomputed = chunk_payload(bytes)?;
                let recomputed_chunks: Vec<(ChunkRef, &[u8])> = match recomputed {
                    ChunkedPayload::Chunked { chunks, .. } => chunks,
                    ChunkedPayload::Inline { payload, hash } => {
                        // Caller advertised a Manifest but the
                        // payload fits in a Small. Surface as an
                        // explicit mismatch — the BlobRef and the
                        // bytes disagree on shape.
                        let _ = (payload, hash);
                        return Err(BlobError::Backend(
                            "mesh blob: Manifest with payload ≤ chunk threshold; \
                             caller should have produced BlobRef::Small"
                                .to_owned(),
                        ));
                    }
                };
                if recomputed_chunks.len() != chunks.len() {
                    return Err(BlobError::Backend(format!(
                        "mesh blob: Manifest chunk count mismatch: declared {}, actual {}",
                        chunks.len(),
                        recomputed_chunks.len()
                    )));
                }
                for (i, (recomputed_chunk, chunk_bytes)) in recomputed_chunks.iter().enumerate() {
                    if recomputed_chunk.hash != chunks[i].hash {
                        return Err(BlobError::Backend(format!(
                            "mesh blob: chunk {} hash mismatch", i,
                        )));
                    }
                    if recomputed_chunk.size != chunks[i].size {
                        return Err(BlobError::Backend(format!(
                            "mesh blob: chunk {} size mismatch", i,
                        )));
                    }
                    self.store_chunk(&recomputed_chunk.hash, chunk_bytes).await?;
                }
                Ok(())
            }
        }
    }

    async fn fetch(&self, blob_ref: &BlobRef) -> Result<Vec<u8>, BlobError> {
        match blob_ref {
            BlobRef::Small { hash, .. } => self.fetch_chunk(hash).await,
            BlobRef::Manifest {
                chunks,
                total_size,
                ..
            } => {
                let mut out = Vec::with_capacity(*total_size as usize);
                for chunk in chunks {
                    let chunk_bytes = self.fetch_chunk(&chunk.hash).await?;
                    if chunk_bytes.len() as u64 != chunk.size as u64 {
                        return Err(BlobError::Backend(format!(
                            "mesh blob: chunk {} fetched size {} != declared {}",
                            hex32(&chunk.hash),
                            chunk_bytes.len(),
                            chunk.size
                        )));
                    }
                    out.extend_from_slice(&chunk_bytes);
                }
                Ok(out)
            }
        }
    }

    async fn fetch_range(
        &self,
        blob_ref: &BlobRef,
        range: std::ops::Range<u64>,
    ) -> Result<Vec<u8>, BlobError> {
        if range.start > range.end {
            return Err(BlobError::Backend(format!(
                "mesh blob: range.start ({}) > range.end ({})",
                range.start, range.end
            )));
        }
        let len = range.end - range.start;
        if len == 0 {
            return Ok(Vec::new());
        }
        match blob_ref {
            BlobRef::Small { hash, size, .. } => {
                if range.end > *size {
                    return Err(BlobError::Backend(format!(
                        "mesh blob: range.end {} exceeds Small size {}",
                        range.end, size
                    )));
                }
                let bytes = self.fetch_chunk(hash).await?;
                Ok(bytes[range.start as usize..range.end as usize].to_vec())
            }
            BlobRef::Manifest { .. } => {
                let requests = byte_range_to_chunks(blob_ref, range.start, range.end)?;
                let mut out = Vec::with_capacity(len as usize);
                let chunks = blob_ref.chunks();
                for req in requests {
                    let chunk = &chunks[req.chunk_index];
                    let chunk_bytes = self.fetch_chunk(&chunk.hash).await?;
                    out.extend_from_slice(
                        &chunk_bytes[req.start_in_chunk as usize..req.end_in_chunk as usize],
                    );
                }
                Ok(out)
            }
        }
    }

    async fn exists(&self, blob_ref: &BlobRef) -> Result<bool, BlobError> {
        match blob_ref {
            BlobRef::Small { hash, .. } => self.chunk_exists(hash),
            BlobRef::Manifest { chunks, .. } => {
                for chunk in chunks {
                    if !self.chunk_exists(&chunk.hash)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
        }
    }

    async fn delete(&self, _blob_ref: &BlobRef) -> Result<(), BlobError> {
        // PR-2a defers refcount-aware delete to PR-4. For now this
        // is a no-op — the GC sweep is what reclaims chunks, and
        // it's keyed on the refcount source list (chain folds /
        // CortEX indexes / out-of-band scanner) that lands later.
        // Returning `Ok(())` here matches the default trait impl;
        // we override only to make the layering explicit + to give
        // PR-4 a single place to wire in.
        Ok(())
    }

    async fn stat(&self, blob_ref: &BlobRef) -> Result<BlobStat, BlobError> {
        // v0.2 PR-2a: size + replica_target + encoding are
        // observable; replicas_observed lands with the capability
        // extension in PR-2b; last_seen_unix_ms lands with the GC
        // scanner in PR-4.
        let replica_target = self.replication.as_ref().map(|c| c.factor);
        Ok(BlobStat {
            size: blob_ref.size(),
            replicas_observed: 0,
            replica_target,
            last_seen_unix_ms: None,
            encoding: blob_ref.encoding(),
        })
    }
}

impl MeshBlobAdapter {
    /// Local-storage existence probe — checks the chunk file is open
    /// with non-zero length. Sync; the `BlobAdapter::exists` async
    /// wrapper above just routes here.
    fn chunk_exists(&self, hash: &[u8; 32]) -> Result<bool, BlobError> {
        let channel = Self::chunk_channel(hash);
        let cfg = self.chunk_file_config();
        let file = self
            .redex
            .open_file(&channel, cfg)
            .map_err(|e| BlobError::Backend(format!("mesh blob: open chunk file: {}", e)))?;
        Ok(!file.is_empty())
    }

    /// Flush every chunk file referenced by `blob_ref` to disk.
    /// Used by `publish_with_blob` (see
    /// `super::publish_with_blob`) under
    /// [`BlobDurability::DurableOnLocal`](crate::adapter::net::dataforts::BlobDurability::DurableOnLocal)
    /// to satisfy "blob survives local node restart" before the
    /// publish step. No-op for `BestEffort`; `ReplicatedTo(n)`
    /// composes this with a wait-for-replicas poll above.
    ///
    /// Iterates `BlobRef::Small` as a single chunk; iterates
    /// `BlobRef::Manifest` over every `ChunkRef`. Each chunk's
    /// underlying `RedexFile::sync` runs sequentially — the call
    /// order is stable but partial-progress on error means some
    /// chunks may have been flushed before the failure point.
    /// Surface as `BlobError::Backend` for the operator to
    /// retry / inspect.
    pub async fn sync_blob(&self, blob_ref: &BlobRef) -> Result<(), BlobError> {
        let hashes: Vec<[u8; 32]> = match blob_ref {
            BlobRef::Small { hash, .. } => vec![*hash],
            BlobRef::Manifest { chunks, .. } => chunks.iter().map(|c| c.hash).collect(),
        };
        for hash in hashes {
            let channel = Self::chunk_channel(&hash);
            // `get_file` returns `None` if no file is registered;
            // a sync of a not-yet-stored chunk is a layering bug,
            // surface a typed error.
            let file = self.redex.get_file(&channel).ok_or_else(|| {
                BlobError::NotFound(format!(
                    "mesh blob: chunk {} not stored locally — sync_blob \
                     requires prior store",
                    hex32(&hash)
                ))
            })?;
            file.sync()
                .map_err(|e| BlobError::Backend(format!("mesh blob: chunk sync: {}", e)))?;
        }
        Ok(())
    }
}

fn hex32(bytes: &[u8; 32]) -> String {
    let mut s = String::with_capacity(64);
    for b in bytes {
        use std::fmt::Write;
        let _ = write!(s, "{:02x}", b);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::super::blob_ref::BLOB_CHUNK_SIZE_BYTES;
    use super::*;

    fn make_adapter() -> MeshBlobAdapter {
        let redex = Arc::new(Redex::new());
        MeshBlobAdapter::new("mesh-test", redex)
    }

    /// BLAKE3 a payload + wrap as a `BlobRef::Small`.
    fn small_ref_for(payload: &[u8]) -> BlobRef {
        let hash: [u8; 32] = blake3::hash(payload).into();
        BlobRef::small(format!("mesh://{}", hex32(&hash)), hash, payload.len() as u64)
    }

    #[tokio::test]
    async fn store_fetch_small_round_trip() {
        let adapter = make_adapter();
        let payload = b"the small blob payload".to_vec();
        let blob = small_ref_for(&payload);

        adapter.store(&blob, &payload).await.unwrap();
        let fetched = adapter.fetch(&blob).await.unwrap();
        assert_eq!(fetched, payload);
    }

    #[tokio::test]
    async fn store_is_idempotent_for_identical_bytes() {
        let adapter = make_adapter();
        let payload = b"idempotent".to_vec();
        let blob = small_ref_for(&payload);

        adapter.store(&blob, &payload).await.unwrap();
        // Second store of identical content must succeed — content-
        // addressed storage is naturally idempotent.
        adapter.store(&blob, &payload).await.unwrap();
        let fetched = adapter.fetch(&blob).await.unwrap();
        assert_eq!(fetched, payload);
    }

    #[tokio::test]
    async fn store_rejects_size_mismatch_on_small() {
        let adapter = make_adapter();
        let payload = b"truth".to_vec();
        let hash: [u8; 32] = blake3::hash(&payload).into();
        // Caller stamps a wrong size on the BlobRef.
        let lying = BlobRef::small("mesh://lie", hash, 999);
        let err = adapter.store(&lying, &payload).await.unwrap_err();
        assert!(matches!(err, BlobError::Backend(_)));
    }

    #[tokio::test]
    async fn store_rejects_bytes_that_dont_hash_to_advertised() {
        let adapter = make_adapter();
        let advertised: &[u8] = b"truth";
        let attempted: &[u8] = b"a lie";
        let hash: [u8; 32] = blake3::hash(advertised).into();
        let blob = BlobRef::small("mesh://tamper", hash, attempted.len() as u64);
        let err = adapter.store(&blob, attempted).await.unwrap_err();
        // Either HashMismatch (from store_chunk verify) or Backend
        // (size mismatch fires first if sizes differ); both are
        // acceptable as long as the store rejects.
        assert!(matches!(
            err,
            BlobError::HashMismatch { .. } | BlobError::Backend(_)
        ));
    }

    #[tokio::test]
    async fn fetch_missing_returns_not_found() {
        let adapter = make_adapter();
        let blob = BlobRef::small("mesh://ghost", [0xFF; 32], 0);
        let err = adapter.fetch(&blob).await.unwrap_err();
        assert!(matches!(err, BlobError::NotFound(_)));
    }

    #[tokio::test]
    async fn exists_reports_correctly() {
        let adapter = make_adapter();
        let payload = b"existential".to_vec();
        let blob = small_ref_for(&payload);
        assert!(!adapter.exists(&blob).await.unwrap());
        adapter.store(&blob, &payload).await.unwrap();
        assert!(adapter.exists(&blob).await.unwrap());
    }

    #[tokio::test]
    async fn store_fetch_manifest_multi_chunk() {
        let adapter = make_adapter();
        // Payload large enough to chunk: 4 MiB + a bit.
        let payload: Vec<u8> = (0..(BLOB_CHUNK_SIZE_BYTES as usize + 100))
            .map(|i| (i % 251) as u8)
            .collect();
        // Drive chunking via the pure-logic helper, then build the
        // BlobRef::Manifest the same way an honest caller would.
        let chunked = chunk_payload(&payload).unwrap();
        let chunk_refs: Vec<ChunkRef> = match chunked {
            ChunkedPayload::Chunked { chunks, .. } => {
                chunks.into_iter().map(|(r, _)| r).collect()
            }
            ChunkedPayload::Inline { .. } => panic!("expected Chunked for >4MiB payload"),
        };
        let blob = BlobRef::manifest("mesh://multi", Encoding::Replicated, chunk_refs).unwrap();

        adapter.store(&blob, &payload).await.unwrap();
        let fetched = adapter.fetch(&blob).await.unwrap();
        assert_eq!(fetched, payload);
    }

    #[tokio::test]
    async fn fetch_range_against_manifest_returns_correct_slice() {
        let adapter = make_adapter();
        let payload: Vec<u8> = (0..(BLOB_CHUNK_SIZE_BYTES as usize * 2 + 500))
            .map(|i| (i % 251) as u8)
            .collect();
        let chunked = chunk_payload(&payload).unwrap();
        let chunk_refs: Vec<ChunkRef> = match chunked {
            ChunkedPayload::Chunked { chunks, .. } => {
                chunks.into_iter().map(|(r, _)| r).collect()
            }
            _ => panic!("expected Chunked"),
        };
        let blob = BlobRef::manifest("mesh://range", Encoding::Replicated, chunk_refs).unwrap();
        adapter.store(&blob, &payload).await.unwrap();

        // Pick a range that spans the first / second chunk boundary.
        let start = BLOB_CHUNK_SIZE_BYTES - 100;
        let end = BLOB_CHUNK_SIZE_BYTES + 100;
        let fetched = adapter.fetch_range(&blob, start..end).await.unwrap();
        assert_eq!(fetched, payload[start as usize..end as usize]);
    }

    #[tokio::test]
    async fn fetch_range_against_small() {
        let adapter = make_adapter();
        let payload = b"hello world, mesh blob adapter".to_vec();
        let blob = small_ref_for(&payload);
        adapter.store(&blob, &payload).await.unwrap();

        let fetched = adapter.fetch_range(&blob, 6..11).await.unwrap();
        assert_eq!(fetched, b"world");
    }

    #[tokio::test]
    async fn store_rejects_reed_solomon_encoding() {
        let adapter = make_adapter();
        let payload: Vec<u8> = vec![0xAA; BLOB_CHUNK_SIZE_BYTES as usize + 1];
        let chunk_refs: Vec<ChunkRef> = match chunk_payload(&payload).unwrap() {
            ChunkedPayload::Chunked { chunks, .. } => {
                chunks.into_iter().map(|(r, _)| r).collect()
            }
            _ => panic!("expected Chunked"),
        };
        let blob = BlobRef::manifest(
            "mesh://rs",
            Encoding::ReedSolomon { k: 4, m: 2 },
            chunk_refs,
        )
        .unwrap();
        let err = adapter.store(&blob, &payload).await.unwrap_err();
        // ReedSolomon is reserved for v0.3 — store rejects.
        assert!(matches!(err, BlobError::Backend(_)));
    }

    #[tokio::test]
    async fn stat_returns_size_plus_metadata() {
        let adapter = make_adapter();
        let payload = b"observable".to_vec();
        let blob = small_ref_for(&payload);
        adapter.store(&blob, &payload).await.unwrap();

        let stat = adapter.stat(&blob).await.unwrap();
        assert_eq!(stat.size, payload.len() as u64);
        assert!(stat.replicas_observed == 0); // PR-2b lands the capability count
        assert_eq!(stat.replica_target, None); // None — no replication configured
        assert_eq!(stat.encoding, None); // Small has no encoding
    }

    #[tokio::test]
    async fn stat_surfaces_replica_target_when_replication_set() {
        // We can't actually exercise replication without a mesh —
        // but we can pin that the `replica_target` field reflects
        // the operator's config when set.
        use crate::adapter::net::redex::PlacementStrategy;
        let redex = Arc::new(Redex::new());
        let rep = ReplicationConfig {
            factor: 3,
            placement: PlacementStrategy::Standard,
            ..ReplicationConfig::default()
        };
        let adapter = MeshBlobAdapter::new("mesh-rep", redex).with_replication(rep);
        let blob = BlobRef::small("mesh://x", [0; 32], 0);
        let stat = adapter.stat(&blob).await.unwrap();
        assert_eq!(stat.replica_target, Some(3));
    }

    #[tokio::test]
    async fn delete_is_noop_in_pr2a() {
        // PR-2a's delete is a no-op pending the PR-4 refcount work.
        // Pin the contract so a future change is visible.
        let adapter = make_adapter();
        let blob = BlobRef::small("mesh://x", [0; 32], 0);
        adapter.delete(&blob).await.unwrap();
    }

    #[tokio::test]
    async fn manifest_store_rejects_size_mismatch() {
        let adapter = make_adapter();
        let real_payload: Vec<u8> = vec![0xAA; BLOB_CHUNK_SIZE_BYTES as usize + 1];
        let chunk_refs: Vec<ChunkRef> = match chunk_payload(&real_payload).unwrap() {
            ChunkedPayload::Chunked { chunks, .. } => {
                chunks.into_iter().map(|(r, _)| r).collect()
            }
            _ => panic!("expected Chunked"),
        };
        let blob = BlobRef::manifest("mesh://x", Encoding::Replicated, chunk_refs).unwrap();
        // Try storing a payload of the wrong size.
        let fake_payload: Vec<u8> = vec![0xBB; 500];
        let err = adapter.store(&blob, &fake_payload).await.unwrap_err();
        assert!(matches!(err, BlobError::Backend(_)));
    }

    #[tokio::test]
    async fn manifest_store_rejects_chunk_hash_mismatch() {
        let adapter = make_adapter();
        // Build a chunk list pointing at bogus hashes, then try to
        // store the *correct* bytes against it. Should reject
        // because the recomputed chunk hashes don't match.
        let payload: Vec<u8> = vec![0xCC; BLOB_CHUNK_SIZE_BYTES as usize + 1];
        let bogus_chunks = vec![
            ChunkRef {
                hash: [0; 32],
                size: BLOB_CHUNK_SIZE_BYTES as u32,
            },
            ChunkRef {
                hash: [1; 32],
                size: 1,
            },
        ];
        let blob =
            BlobRef::manifest("mesh://x", Encoding::Replicated, bogus_chunks).unwrap();
        let err = adapter.store(&blob, &payload).await.unwrap_err();
        assert!(matches!(err, BlobError::Backend(_)));
    }
}
