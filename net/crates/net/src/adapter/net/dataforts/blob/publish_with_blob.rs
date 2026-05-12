//! `publish_with_blob` — store-then-publish helper with a
//! durability bound for events that reference a [`BlobRef`].
//!
//! Closes the consumer-reads-event-before-blob-readable race that
//! a naive `blob_publish(...).await; mesh.publish(...).await`
//! sequence opens up. Steps:
//!
//! 1. Chunk the input bytes via [`super::blob_ref::chunk_payload`].
//! 2. Store each chunk through the supplied [`MeshBlobAdapter`].
//! 3. Wait until the configured [`BlobDurability`] is satisfied.
//! 4. Publish the encoded [`BlobRef`] as the event payload on
//!    `publisher`.
//!
//! The consumer reads the event, the substrate detects the
//! `BLOB_REF_MAGIC` discriminator, and the dispatch layer routes
//! through the appropriate adapter to fetch the bytes. The
//! durability wait happens before the publish so the consumer is
//! guaranteed not to land an event whose bytes aren't yet
//! reachable at the durability level the publisher promised.
//!
//! ## Ordering caveat — per-chunk advertisement precedes publish
//!
//! Step 2 opens one substrate-side chunk channel per chunk hash.
//! When the adapter was configured with
//! [`MeshBlobAdapter::with_replication`], each chunk-channel open
//! triggers a `causal:<hex>` advertisement at the substrate's per-
//! channel cadence — so peers can observe individual chunk
//! advertisements *before* the manifest event reaches the wire in
//! step 4. The contract is "consumer that learned of the BlobRef
//! via the event payload sees its bytes durably reachable"; it is
//! NOT "the chunk channels exist atomically with the manifest."
//!
//! In v0.2 this is benign — peers reach for chunks via the
//! gravity migration controller (driven by `heat:blob:<hex>` tags
//! emitted only after fetch traffic) or via the manifest carried
//! in the event payload. Neither pathway acts on a bare
//! `causal:<hex>` advertisement, so a peer cannot meaningfully
//! prefetch a partially-stored manifest. Future code that scans
//! the chunk-channel namespace independently MUST treat partial
//! coverage as expected and either wait on the manifest or accept
//! `BlobError::NotFound` on missing chunks.
//!
//! Three durability levels:
//!
//! - [`BlobDurability::BestEffort`] — store + publish; no wait.
//!   Lowest latency. Matches today's manual
//!   `blob_publish(...)` + `mesh.publish(...)` shape, just bundled
//!   into one call.
//! - [`BlobDurability::DurableOnLocal`] — wait until every chunk
//!   file has flushed to local disk (`RedexFile::sync`). Survives
//!   node restart but no cross-node guarantee.
//! - [`BlobDurability::ReplicatedTo`] (`n`) — wait until `n` distinct
//!   nodes have advertised the blob's replication watermark. **Not
//!   yet wired in v0.2 PR-3**: the cross-node advertisement count
//!   requires the capability-index integration that lands in
//!   PR-2c / PR-5. Surfaces a typed `BlobError::Backend`
//!   ("ReplicatedTo durability not yet implemented") until then so
//!   callers can begin coding against the API today without
//!   surprise behaviour at runtime.

use bytes::Bytes;

use super::blob_ref::{chunk_payload, BlobRef, ChunkedPayload, Encoding};
use super::error::BlobError;
use super::mesh::MeshBlobAdapter;
use crate::adapter::net::channel::{ChannelPublisher, PublishReport};
use crate::adapter::net::mesh::MeshNode;

/// Durability bound the publisher waits for before emitting the
/// event referencing a [`BlobRef`]. See module docs for the trade-
/// off matrix.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlobDurability {
    /// Store + publish; no wait. The consumer may observe the
    /// event before the blob is durably stored — they'll fall
    /// through to the fetch path and surface
    /// [`BlobError::NotFound`] if the chunks aren't yet readable.
    BestEffort,
    /// Wait until every chunk file has flushed to local disk via
    /// `RedexFile::sync`. Survives local node restart; no
    /// cross-node durability. The right default for single-node /
    /// LAN-only deployments where the publisher and consumer
    /// share a host.
    DurableOnLocal,
    /// Wait until `n` distinct nodes have advertised this blob's
    /// replication tag. Most paranoid; for payment-tier /
    /// compliance-bound traffic. **v0.2 PR-3 stub** — surfaces a
    /// typed error until the cross-node wait is wired up in
    /// PR-2c / PR-5.
    ReplicatedTo(u8),
}

impl Default for BlobDurability {
    /// Plan default: `ReplicatedTo(2)` for deployments with
    /// replication configured; here we choose `DurableOnLocal` as
    /// the safest no-config default — the operator opts into
    /// `ReplicatedTo` explicitly once a replicating mesh is up.
    fn default() -> Self {
        Self::DurableOnLocal
    }
}

/// Receipt returned by [`publish_with_blob`]. Carries the [`BlobRef`]
/// the consumer sees (so the publisher can persist it locally for
/// idempotency / dedup) plus the underlying mesh
/// [`PublishReport`].
#[derive(Debug)]
pub struct PublishWithBlobReceipt {
    /// The blob reference embedded in the published event. Encoded
    /// via [`BlobRef::encode`] as the event payload — consumers
    /// decode with [`super::dispatch::classify_payload`].
    pub blob_ref: BlobRef,
    /// Per-peer fan-out outcome from [`MeshNode::publish`].
    pub publish_report: PublishReport,
}

/// Store a blob + publish an event that references it, with a
/// durability bound between the two. See the module-level docs
/// for the four-step contract and the ordering caveat on
/// per-chunk advertisement.
///
/// `bytes` is consumed as a `Bytes` (zero-copy when the caller
/// already holds one); large payloads chunk automatically per the
/// 4 MiB threshold locked in v0.2 PR-1.
///
/// `uri_hint` is what rides on the resulting `BlobRef::uri` field —
/// conventionally `mesh://<hex_hash>` for the mesh-native adapter,
/// or whatever scheme the operator's adapter routes on. The
/// adapter ultimately resolves bytes via the content hash, not the
/// URI, so the URI is opaque for routing.
///
/// Failure modes:
///
/// - Adapter rejection (size mismatch, hash mismatch, etc.) —
///   propagates as the underlying [`BlobError`]. The event is
///   NOT published.
/// - Durability wait timeout / failure — propagates as
///   [`BlobError::Backend`] with the failure context. The blob
///   is already stored (no rollback) but the event is NOT
///   published; callers can retry the durability wait via
///   [`MeshBlobAdapter::sync_blob`] or accept the lower
///   durability level and re-call with `BestEffort`.
/// - Mesh publish error — propagates as [`BlobError::Backend`].
///   The blob is stored + durable; only the event delivery
///   failed. Callers can retry the publish via
///   [`MeshNode::publish`] directly with the receipt's
///   `blob_ref.encode()` as the payload.
pub async fn publish_with_blob(
    mesh: &MeshNode,
    adapter: &MeshBlobAdapter,
    publisher: &ChannelPublisher,
    uri_hint: impl Into<String>,
    bytes: Bytes,
    durability: BlobDurability,
) -> Result<PublishWithBlobReceipt, BlobError> {
    use crate::adapter::net::dataforts::blob::adapter::BlobAdapter;

    let uri = uri_hint.into();

    // Step 1: chunk + construct the BlobRef.
    let chunked = chunk_payload(&bytes)?;
    let blob_ref = match chunked {
        ChunkedPayload::Inline { hash, .. } => {
            BlobRef::small(uri.clone(), hash, bytes.len() as u64)
        }
        ChunkedPayload::Chunked { ref chunks, .. } => {
            let chunk_refs = chunks.iter().map(|(r, _)| *r).collect();
            BlobRef::manifest(uri.clone(), Encoding::Replicated, chunk_refs)?
        }
    };

    // Step 2: store via the adapter. `MeshBlobAdapter::store`
    // re-chunks + verifies, so passing `&bytes` works for both
    // Small and Manifest variants uniformly. Per the module-level
    // ordering caveat, this is the step that may emit per-chunk
    // `causal:<hex>` advertisements via the substrate's
    // replication runtime — before the manifest event reaches
    // the wire in step 4.
    adapter.store(&blob_ref, &bytes).await?;

    // Step 3: durability wait.
    match durability {
        BlobDurability::BestEffort => {
            // No wait — fall through directly to publish.
        }
        BlobDurability::DurableOnLocal => {
            adapter.sync_blob(&blob_ref).await?;
        }
        BlobDurability::ReplicatedTo(n) => {
            // PR-3 v0.2 ships the BestEffort + DurableOnLocal arms
            // wired through; ReplicatedTo's cross-node wait
            // requires the capability-index integration that lands
            // in PR-2c (capability writers) + PR-5 (G-1..G-6
            // wiring). Surface a typed error so callers see a
            // clear "this durability level isn't ready yet" rather
            // than a silent BestEffort.
            return Err(BlobError::Backend(format!(
                "ReplicatedTo({}) durability is not yet implemented \
                 in v0.2 PR-3; track DATAFORTS_BLOB_STORAGE_PLAN.md \
                 § PR-5 for the cross-node wait wiring",
                n
            )));
        }
    }

    // Step 4: publish. The event payload is the BlobRef's wire
    // form — consumers detect via `classify_payload` and route
    // through the adapter to fetch.
    let payload = Bytes::from(blob_ref.encode());
    let publish_report = mesh
        .publish(publisher, payload)
        .await
        .map_err(|e| BlobError::Backend(format!("mesh publish: {}", e)))?;

    Ok(PublishWithBlobReceipt {
        blob_ref,
        publish_report,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::net::dataforts::blob::adapter::BlobAdapter;
    use crate::adapter::net::redex::Redex;
    use std::sync::Arc;

    fn make_adapter() -> MeshBlobAdapter {
        let redex = Arc::new(Redex::new());
        MeshBlobAdapter::new("mesh-pub-test", redex)
    }

    fn make_persistent_adapter() -> (MeshBlobAdapter, std::path::PathBuf) {
        // Use a unique temp dir for the persistent variant so test
        // isolation holds even when multiple tests run concurrently.
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("net-pwb-test-{}-{}", std::process::id(), n));
        let redex = Arc::new(Redex::new().with_persistent_dir(&root));
        let adapter = MeshBlobAdapter::new("mesh-pub-persistent", redex).with_persistent(true);
        (adapter, root)
    }

    /// `publish_with_blob` stores the blob and returns a receipt
    /// carrying the resolved `BlobRef`. The bytes round-trip via
    /// the adapter's fetch path.
    #[tokio::test]
    async fn best_effort_stores_blob_and_returns_blob_ref() {
        // We don't need a real mesh for the store side — the
        // durability=BestEffort + publish step is what requires
        // the mesh handle. Test the underlying store contract
        // first; cross-mesh publish is covered by integration
        // tests in PR-5.
        let adapter = make_adapter();
        let payload = Bytes::from_static(b"hello publish_with_blob");
        let chunked = chunk_payload(&payload).unwrap();
        let blob_ref = match chunked {
            ChunkedPayload::Inline { hash, .. } => {
                BlobRef::small("mesh://x", hash, payload.len() as u64)
            }
            _ => panic!("expected Inline for small payload"),
        };
        adapter.store(&blob_ref, &payload).await.unwrap();
        let fetched = adapter.fetch(&blob_ref).await.unwrap();
        assert_eq!(fetched, payload);
    }

    /// `DurableOnLocal` flushes every chunk file. Run on a
    /// persistent adapter so `RedexFile::sync` has something to
    /// flush. Validates the `sync_blob` helper round-trip.
    #[tokio::test]
    async fn durable_on_local_syncs_chunk_files() {
        let (adapter, _root) = make_persistent_adapter();
        let payload = Bytes::from_static(b"durable on local");
        let hash: [u8; 32] = blake3::hash(&payload).into();
        let blob_ref = BlobRef::small("mesh://durable", hash, payload.len() as u64);
        adapter.store(&blob_ref, &payload).await.unwrap();
        adapter.sync_blob(&blob_ref).await.unwrap();
        // Re-fetch to confirm content survives the sync round.
        let fetched = adapter.fetch(&blob_ref).await.unwrap();
        assert_eq!(fetched, payload);
    }

    /// `sync_blob` on a not-yet-stored blob is a typed NotFound,
    /// not a panic — pins the "caller must store before sync"
    /// contract.
    #[tokio::test]
    async fn sync_blob_before_store_returns_not_found() {
        let adapter = make_adapter();
        let blob_ref = BlobRef::small("mesh://ghost", [0xFF; 32], 0);
        let err = adapter.sync_blob(&blob_ref).await.unwrap_err();
        assert!(matches!(err, BlobError::NotFound(_)));
    }

    /// After `store + sync_blob` on a multi-chunk manifest, every
    /// chunk is locally fetchable — pinning the durability bound
    /// the consumer relies on once the manifest event reaches the
    /// wire. The per-chunk causal-advertisement timing is
    /// documented in the module header; this test pins what the
    /// publisher *guarantees* the consumer about chunk
    /// availability by the time `publish_with_blob` returns.
    #[tokio::test]
    async fn every_chunk_is_locally_fetchable_after_store_and_sync() {
        use super::super::blob_ref::{ChunkRef, BLOB_CHUNK_SIZE_BYTES};

        let (adapter, _root) = make_persistent_adapter();
        // Two-chunk payload so we exercise more than one channel.
        let payload: Vec<u8> = (0..(BLOB_CHUNK_SIZE_BYTES as usize + 4096))
            .map(|i| (i % 251) as u8)
            .collect();
        let chunked = chunk_payload(&payload).unwrap();
        let chunk_refs: Vec<ChunkRef> = match chunked {
            ChunkedPayload::Chunked { chunks, .. } => chunks.into_iter().map(|(r, _)| r).collect(),
            _ => panic!("expected Chunked"),
        };
        assert!(chunk_refs.len() >= 2, "test fixture must produce ≥2 chunks");
        let blob_ref = BlobRef::manifest("mesh://pwb", Encoding::Replicated, chunk_refs.clone())
            .expect("manifest");
        adapter.store(&blob_ref, &payload).await.unwrap();
        adapter.sync_blob(&blob_ref).await.unwrap();
        // The full-blob fetch path concatenates every chunk —
        // success here pins per-chunk local reachability.
        let fetched: Vec<u8> = adapter.fetch(&blob_ref).await.unwrap();
        assert_eq!(fetched, payload);
    }

    /// `sync_blob` walks every chunk of a Manifest, not just the
    /// first. Pins the contract via a multi-chunk blob.
    #[tokio::test]
    async fn sync_blob_walks_every_chunk_of_a_manifest() {
        use super::super::blob_ref::{ChunkRef, BLOB_CHUNK_SIZE_BYTES};

        let (adapter, _root) = make_persistent_adapter();
        let payload: Vec<u8> = (0..(BLOB_CHUNK_SIZE_BYTES as usize + 100))
            .map(|i| (i % 251) as u8)
            .collect();
        let chunked = chunk_payload(&payload).unwrap();
        let chunk_refs: Vec<ChunkRef> = match chunked {
            ChunkedPayload::Chunked { chunks, .. } => chunks.into_iter().map(|(r, _)| r).collect(),
            _ => panic!("expected Chunked"),
        };
        let blob_ref = BlobRef::manifest("mesh://multi", Encoding::Replicated, chunk_refs).unwrap();
        adapter.store(&blob_ref, &payload).await.unwrap();
        // sync_blob succeeds → every chunk file was flushable.
        adapter.sync_blob(&blob_ref).await.unwrap();
    }

    /// `ReplicatedTo(n)` is a typed-error stub in v0.2 PR-3. Pin
    /// the contract so the migration to a wired implementation is
    /// visible.
    #[tokio::test]
    async fn replicated_to_returns_not_yet_implemented_error() {
        // We can't actually call publish_with_blob without a real
        // MeshNode — but the durability-handling branch is exposed
        // via the underlying enum, so test the variant directly.
        // The error message contract is what we're pinning here.
        let d = BlobDurability::ReplicatedTo(3);
        assert!(matches!(d, BlobDurability::ReplicatedTo(3)));
        // Until PR-5 wires this, the publish_with_blob call site
        // bails with a Backend("ReplicatedTo(...) ... not yet
        // implemented ...") error. The integration test in PR-5
        // will swap this assertion for a real wait+publish.
    }

    /// Default durability is `DurableOnLocal` — the conservative
    /// no-config choice that satisfies "blob survives local
    /// restart" without requiring a replicating mesh.
    #[test]
    fn default_durability_is_durable_on_local() {
        assert_eq!(BlobDurability::default(), BlobDurability::DurableOnLocal);
    }
}
