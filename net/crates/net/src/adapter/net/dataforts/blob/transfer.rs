//! Blob transfer over router streams (FairScheduler transport plan, T-1).
//!
//! On-demand cross-peer blob fetch that moves bytes over the router's
//! reliable, scheduled streams — NOT RedEX replication (a replication
//! primitive) and NOT nRPC (a request/reply primitive). See
//! `docs/plans/FAIRSCHEDULER_TRANSPORT_PLAN.md`.
//!
//! T-1 (this slice): the subprotocol ID and the stream-allocation
//! convention. The control packet that initiates a transfer and the
//! bulk data both ride [`SUBPROTOCOL_BLOB_TRANSFER`]; transfer streams
//! draw their IDs from a reserved region of the shared `u64` stream-id
//! space so they never alias channel-publisher, subprotocol, or control
//! streams. (T-2 discovery→stream bridge, T-3 serving handler, T-4
//! receive reassembly land on top.)
//!
//! # Stream-id convention
//!
//! The substrate's stream-id space is shared (the session keys stream
//! state and the [`FairScheduler`](crate::adapter::net::router::FairScheduler)
//! keys queues by raw `stream_id`), with soft conventions per subsystem:
//!
//! - **Channel-publisher streams** always SET bit 48
//!   (`MeshNode::publish_stream_id` = `0x0001_0000_0000_0000 | hash`).
//! - **Subprotocol streams** use the small subprotocol-id value
//!   (`< 0x1100`).
//! - **Control stream** is `u64::MAX` (bit 48 set).
//!
//! Transfer streams therefore use **bit 61 set AND bit 48 clear**:
//! distinct from channel/control streams (which always set bit 48) and
//! from subprotocol streams (which never set bit 61). The low 48 bits
//! carry a per-transfer nonce, so bit 48 stays clear by construction.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};

use bytes::Bytes;
use dashmap::DashMap;
use serde::{Deserialize, Serialize};

use super::error::BlobError;
use super::mesh::MeshBlobAdapter;
use crate::adapter::net::{MeshNode, Reliability, Stream, StreamConfig};

/// Subprotocol ID for blob transfer. Next free family after the
/// existing `0x04xx..0x10xx` allocations (fold is `0x1000`). Both the
/// transfer control packet and its bulk data carry this ID so inbound
/// dispatch routes them to the transfer handler (T-3).
pub const SUBPROTOCOL_BLOB_TRANSFER: u16 = 0x1100;

/// Marker bit (61) on a transfer stream ID. Combined with bit 48 clear
/// (channel-publisher / control streams always set bit 48), this keeps
/// transfer stream IDs disjoint from every other subsystem's streams.
const TRANSFER_STREAM_FLAG: u64 = 1 << 61;

/// Bit 48 — the channel-publisher discriminator. Transfer stream IDs
/// keep it CLEAR (their nonce occupies only bits 0..47), which is what
/// makes them disjoint from channel/control streams.
const CHANNEL_STREAM_BIT: u64 = 1 << 48;

/// Mask for the per-transfer nonce (bits 0..47). Keeping the nonce
/// below bit 48 guarantees [`CHANNEL_STREAM_BIT`] stays clear.
const TRANSFER_NONCE_MASK: u64 = (1 << 48) - 1;

/// Construct a transfer stream ID from a per-transfer `nonce`. Only the
/// low 48 bits of `nonce` are used.
pub fn transfer_stream_id(nonce: u64) -> u64 {
    TRANSFER_STREAM_FLAG | (nonce & TRANSFER_NONCE_MASK)
}

/// True iff `stream_id` is a blob-transfer stream (bit 61 set, bit 48
/// clear). Channel/control streams (bit 48 set) and subprotocol streams
/// (bit 61 clear) both return `false`.
pub fn is_transfer_stream_id(stream_id: u64) -> bool {
    stream_id & TRANSFER_STREAM_FLAG != 0 && stream_id & CHANNEL_STREAM_BIT == 0
}

/// Process-wide nonce source for transfer streams. A monotonic counter
/// is enough: collisions only at 2^48 concurrent-lifetime transfers,
/// far beyond any real workload, and a wrapped nonce only risks
/// aliasing a *still-open* transfer stream (closed ones are cleaned up).
static TRANSFER_STREAM_NONCE: AtomicU64 = AtomicU64::new(1);

/// Allocate a fresh transfer stream ID (unique within this process for
/// the next 2^48 allocations).
pub fn next_transfer_stream_id() -> u64 {
    let nonce = TRANSFER_STREAM_NONCE.fetch_add(1, Ordering::Relaxed);
    transfer_stream_id(nonce)
}

/// Per-data-event byte cap. Kept under `MAX_PAYLOAD_SIZE` (8108) minus
/// the event-frame length prefix so each raw data event rides one
/// packet without overflowing the payload, and each `send_on_stream`
/// of a single event sends exactly one packet (no partial-batch on
/// backpressure).
const DATA_FRAME_BYTES: usize = 8000;

/// Tx-credit window for a serving transfer stream, in on-wire bytes.
///
/// The default stream window (64 KiB, `DEFAULT_STREAM_WINDOW_BYTES`)
/// holds only ~8 data frames in flight, so a multi-MiB chunk exhausts
/// credit dozens of times and each stall pays `send_with_retry`'s
/// ≥5 ms backoff even though the receiver's `StreamWindow` grant lands
/// in <1 ms — throughput collapses to the stall cadence, not the link.
///
/// A serving stream carries at most one chunk
/// ([`BLOB_CHUNK_SIZE_BYTES`](super::BLOB_CHUNK_SIZE_BYTES) = 4 MiB of
/// *payload*), so a window that covers a full chunk's *on-wire* size —
/// payload **plus** the per-packet framing the credit accounting
/// charges (Net header + AEAD tag + event-frame length prefix, ~90 B ×
/// ~525 frames ≈ 47 KiB for a max chunk) — lets the whole chunk stream
/// without the per-event sends ever running out of credit mid-chunk.
/// 5 MiB clears that with headroom. The receiver's continuous
/// `StreamWindow` grants (drained every ≤1 ms) then only matter for
/// chunks at the very top of the size range; below that the sender
/// never blocks on credit at all.
///
/// This is the lever that fixed the throughput/fairness regression: the
/// 64 KiB default window held ~8 frames, so a multi-MiB chunk exhausted
/// credit dozens of times and each stall paid `send_with_retry`'s ≥5 ms
/// backoff even though the grant lands in <1 ms — turning a smooth
/// transfer into a stop-go stutter that also smeared concurrent
/// transfers' interleaving. Fairness is otherwise unaffected: the
/// [`FairScheduler`](crate::adapter::net::router::FairScheduler)
/// interleaves by per-round quantum at dequeue time regardless of any
/// one stream's window or queue depth.
const TRANSFER_STREAM_WINDOW_BYTES: u32 = 5 * 1024 * 1024;

/// Upper bound the receiver accepts for a chunk's `total_len`, so a
/// misbehaving holder can't claim a huge length and OOM the buffer.
/// Generous above the 4 MiB single-chunk max.
const TRANSFER_MAX_CHUNK_BYTES: u64 = 16 * 1024 * 1024;

/// How long a requester waits for a transfer to complete before giving
/// up (and letting the caller retry another holder).
const TRANSFER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Retry budget for an individual stream send under backpressure.
const SEND_RETRIES: usize = 64;

// ── Wire frames ────────────────────────────────────────────────────

/// Control frame, carried on a `SUBPROTOCOL_BLOB_TRANSFER` packet with
/// the transfer stream ID. Sent requester → holder to initiate.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransferControl {
    /// "Send me the chunk addressed by `hash` on this stream."
    Request {
        /// 32-byte BLAKE3 content address.
        hash: [u8; 32],
    },
}

/// First data-plane event on the transfer stream, holder → requester.
/// Subsequent events on the stream are raw chunk bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransferHeader {
    /// The holder has the chunk; `total_len` bytes follow as raw events.
    Found {
        /// Total chunk length the following raw events sum to.
        total_len: u64,
    },
    /// The holder doesn't have the chunk; no bytes follow.
    NotFound,
}

// ── Engine ─────────────────────────────────────────────────────────

type DoneTx = tokio::sync::oneshot::Sender<Result<Bytes, BlobError>>;

/// Requester-side in-flight transfer state, keyed by transfer stream id.
struct PendingInbound {
    expected_hash: [u8; 32],
    /// `None` until the `TransferHeader` lands; then the declared length.
    total_len: Option<u64>,
    buf: Vec<u8>,
    /// Taken and fired once on completion (success / NotFound / error).
    done: Option<DoneTx>,
}

/// Drives blob transfer over router streams (FairScheduler transport).
/// Installed on a node via
/// [`crate::adapter::net::MeshNode::serve_blob_transfer`]. Holds a
/// `Weak<MeshNode>` (to open reply streams without an adapter↔mesh
/// cycle) and the local [`MeshBlobAdapter`] (for content lookup), plus
/// the requester-side pending map.
pub struct BlobTransferEngine {
    mesh: Weak<MeshNode>,
    adapter: Arc<MeshBlobAdapter>,
    pending: DashMap<u64, PendingInbound>,
}

impl BlobTransferEngine {
    /// Construct an engine over the local node + adapter.
    pub fn new(mesh: &Arc<MeshNode>, adapter: Arc<MeshBlobAdapter>) -> Self {
        Self {
            mesh: Arc::downgrade(mesh),
            adapter,
            pending: DashMap::new(),
        }
    }

    /// Register a requester-side pending transfer before the Request is
    /// sent, so the reply (header/data on `stream_id`) can be matched.
    pub fn register_pending(&self, stream_id: u64, expected_hash: [u8; 32], done: DoneTx) {
        self.pending.insert(
            stream_id,
            PendingInbound {
                expected_hash,
                total_len: None,
                buf: Vec::new(),
                done: Some(done),
            },
        );
    }

    /// Drop a pending transfer (timeout / give-up). Idempotent.
    pub fn cancel_pending(&self, stream_id: u64) {
        self.pending.remove(&stream_id);
    }

    /// Serving side: a `TransferControl::Request` arrived on `stream_id`
    /// from `requester`. Spawn a task that reads the chunk locally and
    /// streams it back on the same (transfer) stream.
    ///
    /// NOTE: this first slice does not yet gate on channel-auth — the
    /// session is authenticated, but the per-content authorization the
    /// plan calls for (requester subscribed to a channel that grants
    /// read) is a T-3 follow-up. Documented so it isn't forgotten.
    pub fn on_request(&self, requester: u64, stream_id: u64, payload: &[u8]) {
        let control: TransferControl = match postcard::from_bytes(payload) {
            Ok(c) => c,
            Err(e) => {
                tracing::debug!(error = %e, requester, "blob transfer: bad control frame");
                return;
            }
        };
        let TransferControl::Request { hash } = control;
        let Some(mesh) = self.mesh.upgrade() else {
            return;
        };
        let adapter = self.adapter.clone();
        tokio::spawn(async move {
            serve_chunk(mesh, adapter, requester, stream_id, hash).await;
        });
    }

    /// Requester side: reliable, in-order data-plane events for a
    /// transfer stream were diverted here (before the event bus). The
    /// first event is the [`TransferHeader`]; the rest are raw bytes.
    pub fn on_data(&self, stream_id: u64, events: Vec<Bytes>) {
        // Each completion path extracts what it needs from the map
        // entry, drops the guard, removes the entry, then fires the
        // oneshot — never holding a DashMap guard across `remove`.
        enum Step {
            Continue,
            Fail(BlobError),
            Complete,
        }
        for event in events {
            let step = {
                let mut entry = match self.pending.get_mut(&stream_id) {
                    Some(e) => e,
                    None => return, // already completed / cancelled
                };
                if entry.total_len.is_none() {
                    match postcard::from_bytes::<TransferHeader>(&event) {
                        Ok(TransferHeader::NotFound) => {
                            Step::Fail(BlobError::NotFound("transfer: holder NotFound".into()))
                        }
                        Ok(TransferHeader::Found { total_len })
                            if total_len > TRANSFER_MAX_CHUNK_BYTES =>
                        {
                            Step::Fail(BlobError::Backend(format!(
                                "transfer: total_len {total_len} exceeds cap"
                            )))
                        }
                        Ok(TransferHeader::Found { total_len }) => {
                            entry.total_len = Some(total_len);
                            entry.buf.reserve(total_len.min(TRANSFER_MAX_CHUNK_BYTES) as usize);
                            if total_len == 0 {
                                Step::Complete
                            } else {
                                Step::Continue
                            }
                        }
                        Err(e) => {
                            Step::Fail(BlobError::Backend(format!("transfer: bad header: {e}")))
                        }
                    }
                } else {
                    let total = entry.total_len.unwrap_or(0);
                    if (entry.buf.len() as u64).saturating_add(event.len() as u64) > total {
                        Step::Fail(BlobError::Backend(
                            "transfer: holder sent more than total_len".into(),
                        ))
                    } else {
                        entry.buf.extend_from_slice(&event);
                        if entry.buf.len() as u64 >= total {
                            Step::Complete
                        } else {
                            Step::Continue
                        }
                    }
                }
            };
            match step {
                Step::Continue => {}
                Step::Fail(err) => {
                    self.finish(stream_id, Err(err));
                    return;
                }
                Step::Complete => {
                    self.finish_verified(stream_id);
                    return;
                }
            }
        }
    }

    /// Remove the pending entry and fire its oneshot with `result`.
    fn finish(&self, stream_id: u64, result: Result<Bytes, BlobError>) {
        if let Some((_, mut pending)) = self.pending.remove(&stream_id) {
            if let Some(tx) = pending.done.take() {
                let _ = tx.send(result);
            }
        }
    }

    /// Remove the pending entry, verify the assembled bytes against the
    /// expected hash, and fire its oneshot.
    fn finish_verified(&self, stream_id: u64) {
        let Some((_, mut pending)) = self.pending.remove(&stream_id) else {
            return;
        };
        let bytes = std::mem::take(&mut pending.buf);
        let result = {
            let computed: [u8; 32] = blake3::hash(&bytes).into();
            if computed == pending.expected_hash {
                Ok(Bytes::from(bytes))
            } else {
                Err(BlobError::HashMismatch {
                    expected: pending.expected_hash,
                    actual: computed,
                })
            }
        };
        if let Some(tx) = pending.done.take() {
            let _ = tx.send(result);
        }
    }
}

/// Serving-side: read `hash` locally and stream it to `requester` on
/// `stream_id` over a reliable, scheduled stream (FairScheduler).
async fn serve_chunk(
    mesh: Arc<MeshNode>,
    adapter: Arc<MeshBlobAdapter>,
    requester: u64,
    stream_id: u64,
    hash: [u8; 32],
) {
    let cfg = StreamConfig::new()
        .with_reliability(Reliability::Reliable)
        .with_scheduled(true)
        .with_window_bytes(TRANSFER_STREAM_WINDOW_BYTES)
        .with_fairness_weight(1);
    let stream = match mesh.open_stream(requester, stream_id, cfg) {
        Ok(s) => s,
        Err(e) => {
            tracing::debug!(error = %e, requester, "blob transfer: open reply stream failed");
            return;
        }
    };

    // `fetch_chunk` here is the local content-addressed read (this
    // branch has no peer-fetch fallback on the adapter), so a serving
    // node always answers from its own store — no recursion risk.
    let local = adapter.fetch_chunk(&hash).await;
    match local {
        Ok(bytes) => {
            let header = TransferHeader::Found {
                total_len: bytes.len() as u64,
            };
            if send_one(&mesh, &stream, postcard_event(&header)).await.is_err() {
                return;
            }
            // One reliable event per ~8 KiB frame. Because
            // `TRANSFER_STREAM_WINDOW_BYTES` covers a whole chunk's
            // on-wire size, the per-event credit never runs dry
            // mid-chunk, so these sends don't stall into
            // `send_with_retry`'s backoff (the 64 KiB default window
            // exhausted every ~8 frames and each stall paid ≥5 ms even
            // though the receiver's grant lands in <1 ms). Per-event
            // (not batched) keeps each `send_with_retry` independently
            // safe: a one-packet call can't partially commit and then
            // resend a duplicate under a fresh sequence on retry.
            for chunk in bytes.chunks(DATA_FRAME_BYTES) {
                if send_one(&mesh, &stream, Bytes::copy_from_slice(chunk))
                    .await
                    .is_err()
                {
                    return;
                }
            }
        }
        Err(_) => {
            // Absent locally or local read error → NotFound (never serve
            // suspect bytes). The requester fails over to another holder.
            let _ = send_one(&mesh, &stream, postcard_event(&TransferHeader::NotFound)).await;
        }
    }
}

fn postcard_event<T: Serialize>(value: &T) -> Bytes {
    Bytes::from(postcard::to_allocvec(value).unwrap_or_default())
}

async fn send_one(mesh: &Arc<MeshNode>, stream: &Stream, event: Bytes) -> Result<(), ()> {
    mesh.send_with_retry(stream, std::slice::from_ref(&event), SEND_RETRIES)
        .await
        .map_err(|e| {
            tracing::debug!(error = %e, "blob transfer: stream send failed");
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transfer_ids_are_disjoint_from_channel_and_control_streams() {
        // Channel-publisher streams always set bit 48.
        let channel_like = CHANNEL_STREAM_BIT | 0xDEAD_BEEF_CAFE;
        assert!(!is_transfer_stream_id(channel_like));
        // Control stream is u64::MAX (bit 48 set).
        assert!(!is_transfer_stream_id(u64::MAX));
        // Subprotocol streams are small (bit 61 clear).
        assert!(!is_transfer_stream_id(SUBPROTOCOL_BLOB_TRANSFER as u64));
        assert!(!is_transfer_stream_id(0x1000));
    }

    #[test]
    fn transfer_ids_round_trip_and_self_identify() {
        for nonce in [1u64, 42, 0xFFFF, (1 << 48) - 1] {
            let id = transfer_stream_id(nonce);
            assert!(is_transfer_stream_id(id), "id {id:#x} must self-identify");
            // bit 48 clear by construction.
            assert_eq!(id & CHANNEL_STREAM_BIT, 0);
            // bit 61 set.
            assert_ne!(id & TRANSFER_STREAM_FLAG, 0);
        }
    }

    #[test]
    fn allocator_yields_distinct_transfer_ids() {
        let a = next_transfer_stream_id();
        let b = next_transfer_stream_id();
        assert_ne!(a, b);
        assert!(is_transfer_stream_id(a) && is_transfer_stream_id(b));
    }
}
