//! Capability-broadcast + route-withdrawal subprotocol identifiers.
//!
//! The capability wire payload is a
//! [`super::capability::CapabilityAnnouncement`] serialized via its
//! `to_bytes` / `from_bytes` codec; the route-withdrawal payload is
//! the [`RouteWithdrawal`] codec below. Dispatch + index integration
//! live on `MeshNode` (see `mesh.rs`); this module is intentionally
//! small so each subprotocol id + its payload type have one shared
//! home.

/// Subprotocol id for `CapabilityAnnouncement` packets. Adjacent to
/// the channel-membership id (0x0A00) and stream-window id (0x0B00)
/// to keep the allocated range contiguous.
pub const SUBPROTOCOL_CAPABILITY_ANN: u16 = 0x0C00;

/// Subprotocol id for [`RouteWithdrawal`] packets (RT-5,
/// REALTIME_ROUTING_AND_DISCOVERY_PLAN). Same 0x0C family as the
/// capability announcement — both are mesh-state broadcasts. Nodes
/// that predate this id drop the packets at subprotocol dispatch and
/// keep their pre-RT-5 behavior (routes age out via `sweep_stale`),
/// which is the designed mixed-version degradation.
pub const SUBPROTOCOL_ROUTE_WITHDRAW: u16 = 0x0C01;

/// Poison-reverse route withdrawal: the SENDER declares "I no longer
/// forward toward `dest`". The `via` leg is implicit and
/// authenticated — it is always the session peer the packet arrived
/// from, never a field an attacker could set — so a receiver drops
/// exactly its `(dest, next_hop = sender)` route and nothing else. A
/// malicious sender can only poison routes that already flow through
/// itself, a power it trivially has by dropping traffic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RouteWithdrawal {
    /// Node the sender can no longer reach.
    pub dest: u64,
    /// Sender-local monotonic sequence number. Not currently used
    /// for ordering on the receive side (a stale withdrawal is
    /// repaired by the next pingwave / anti-entropy tick); carried
    /// so a future receiver can order a withdraw/re-advertise race
    /// without a wire change.
    pub seq: u64,
}

impl RouteWithdrawal {
    /// Wire size: `dest` (8 LE) + `seq` (8 LE).
    pub const SIZE: usize = 16;

    /// Serialize to the fixed 16-byte wire layout.
    pub fn to_bytes(&self) -> [u8; Self::SIZE] {
        let mut buf = [0u8; Self::SIZE];
        buf[..8].copy_from_slice(&self.dest.to_le_bytes());
        buf[8..].copy_from_slice(&self.seq.to_le_bytes());
        buf
    }

    /// Strict parse: exactly [`Self::SIZE`] bytes, anything else is
    /// `None`. No version byte — a future layout change takes a new
    /// subprotocol id, mirroring how old nodes degrade for this one.
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        if data.len() != Self::SIZE {
            return None;
        }
        Some(Self {
            dest: u64::from_le_bytes(data[..8].try_into().ok()?),
            seq: u64::from_le_bytes(data[8..].try_into().ok()?),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_withdrawal_roundtrip() {
        let w = RouteWithdrawal {
            dest: 0xDEAD_BEEF_CAFE_F00D,
            seq: 42,
        };
        assert_eq!(RouteWithdrawal::from_bytes(&w.to_bytes()), Some(w));
    }

    #[test]
    fn route_withdrawal_rejects_wrong_length() {
        assert_eq!(RouteWithdrawal::from_bytes(&[0u8; 15]), None);
        assert_eq!(RouteWithdrawal::from_bytes(&[0u8; 17]), None);
        assert_eq!(RouteWithdrawal::from_bytes(&[]), None);
    }
}
