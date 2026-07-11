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
    /// Sender-local monotonic sequence number. Receivers gate on it
    /// per `(sender, dest)` via [`WithdrawalSeqGate`] so a delayed /
    /// duplicated older withdrawal is discarded instead of tearing
    /// down state the sender has since re-advertised.
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

/// Inbound-withdrawal ordering gate: admit only strictly-newer
/// `seq`s per authenticated `(sender, dest)` pair, so a delayed or
/// duplicated OLDER withdrawal can't tear down a route the sender
/// has since re-advertised (a withdraw/re-withdraw pair reordered
/// in flight would otherwise apply in the wrong order). Withdraw
/// vs. pingwave re-advertise has no shared counter — that residual
/// window is anti-entropy-repaired and out of scope here.
///
/// A sender restart resets its seq counter to 0, which this gate
/// would read as "stale" — callers must [`Self::forget_sender`] on
/// re-handshake so each session incarnation starts clean.
#[derive(Debug, Default)]
pub struct WithdrawalSeqGate {
    seen: dashmap::DashMap<(u64, u64), u64>,
}

impl WithdrawalSeqGate {
    /// Hard bound on tracked pairs. Exceeding it clears the map —
    /// the cost of forgetting is at worst admitting one stale
    /// duplicate (repaired by anti-entropy), which beats unbounded
    /// growth from (peer × dest) churn.
    const MAX_ENTRIES: usize = 8192;

    /// Empty gate — nothing admitted yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// `true` iff `seq` is strictly newer than the last admitted
    /// seq for this `(sender, dest)` (or the pair is unseen);
    /// records it when admitted.
    pub fn admit(&self, sender: u64, dest: u64, seq: u64) -> bool {
        if self.seen.len() > Self::MAX_ENTRIES {
            self.seen.clear();
        }
        let mut admitted = false;
        self.seen
            .entry((sender, dest))
            .and_modify(|last| {
                if seq > *last {
                    *last = seq;
                    admitted = true;
                }
            })
            .or_insert_with(|| {
                admitted = true;
                seq
            });
        admitted
    }

    /// Drop every entry authored by `sender` — called on
    /// (re-)handshake and on dead-peer eviction so a fresh session
    /// incarnation's reset seq counter isn't mistaken for stale.
    pub fn forget_sender(&self, sender: u64) {
        self.seen.retain(|(s, _), _| *s != sender);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seq_gate_admits_strictly_newer_only() {
        let gate = WithdrawalSeqGate::new();
        assert!(gate.admit(1, 9, 5), "first sighting admits");
        assert!(!gate.admit(1, 9, 5), "duplicate seq rejected");
        assert!(!gate.admit(1, 9, 3), "older seq rejected");
        assert!(gate.admit(1, 9, 6), "newer seq admits");
        // Independent pairs don't interfere.
        assert!(gate.admit(1, 8, 0), "different dest is a fresh pair");
        assert!(gate.admit(2, 9, 0), "different sender is a fresh pair");
    }

    #[test]
    fn seq_gate_forget_sender_resets_only_that_sender() {
        let gate = WithdrawalSeqGate::new();
        assert!(gate.admit(1, 9, 10));
        assert!(gate.admit(2, 9, 10));
        gate.forget_sender(1);
        assert!(
            gate.admit(1, 9, 0),
            "forgotten sender's reset counter admits again"
        );
        assert!(
            !gate.admit(2, 9, 0),
            "other senders' history must survive the purge"
        );
    }

    #[test]
    fn seq_gate_survives_overflow_clear() {
        let gate = WithdrawalSeqGate::new();
        for dest in 0..=(WithdrawalSeqGate::MAX_ENTRIES as u64) {
            assert!(gate.admit(1, dest, 1));
        }
        // Past the bound the map clears; the gate keeps functioning
        // (a post-clear duplicate is admitted once — documented
        // trade-off — then gated again).
        assert!(gate.admit(1, 0, 1), "post-clear sighting admits");
        assert!(!gate.admit(1, 0, 1), "gating resumes after the clear");
    }

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
