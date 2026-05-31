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
