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
/// capability announcement — both are mesh-state broadcasts.
///
/// Mixed-version degradation: a node that does not know this id
/// drops the packet at the dispatch loop's unknown-subprotocol guard
/// (`mesh.rs`, just before the standard event path) and keeps its
/// pre-RT-5 behavior — routes age out via `sweep_stale`. Note that
/// guard is itself an RT-5-era addition: binaries built *before it*
/// had no catch-all and would instead mis-handle an unknown
/// subprotocol frame as an opaque application event. A true
/// mixed-version deployment therefore needs peers new enough to have
/// the guard, not merely new enough to have this constant.
pub const SUBPROTOCOL_ROUTE_WITHDRAW: u16 = 0x0C01;

// 0x0C02/0x0C03 (sensing interest / readiness attestation) are
// sensing-owned and live in `super::sensing::wire` — committed there
// per the SENSING_INTEREST_COALESCING_PLAN review-7 sign-off.

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
    seen: dashmap::DashMap<(u64, u64), SeqEntry>,
    /// Monotonic access clock; each [`Self::admit`] stamps the
    /// touched entry with the next tick so overflow eviction can drop
    /// the least-recently-active pairs.
    tick: std::sync::atomic::AtomicU64,
}

/// Per-`(sender, dest)` gate state: the last admitted `seq` plus the
/// access `tick` of the most recent sighting (for LRU eviction).
#[derive(Debug)]
struct SeqEntry {
    seq: u64,
    touch: u64,
}

impl WithdrawalSeqGate {
    /// Hard bound that triggers eviction.
    const MAX_ENTRIES: usize = 8192;
    /// Post-eviction target: overflow drops the least-recently-touched
    /// entries down to this mark rather than clearing the whole map.
    const LOW_WATER: usize = 6144;

    /// Empty gate — nothing admitted yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// `true` iff `seq` is strictly newer than the last admitted
    /// seq for this `(sender, dest)` (or the pair is unseen);
    /// records it (and refreshes its access tick) when seen.
    pub fn admit(&self, sender: u64, dest: u64, seq: u64) -> bool {
        // Refresh/insert the incoming pair FIRST — apply its ordering
        // check against any existing entry and stamp it with the
        // newest tick — and only THEN bound the map. Evicting first
        // could drop this very pair's history (if it were the LRU
        // victim) right before the insert, so even an OLDER seq would
        // be reinserted as new and wrongly admitted (cubic P1). By
        // sighting first, the incoming pair becomes the most-recently-
        // touched and is never evicted by its own admit. Every
        // sighting (admitted or not) refreshes the tick, since a pair
        // still receiving withdrawals is active.
        let touch = self.tick.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut admitted = false;
        self.seen
            .entry((sender, dest))
            .and_modify(|e| {
                e.touch = touch;
                if seq > e.seq {
                    e.seq = seq;
                    admitted = true;
                }
            })
            .or_insert_with(|| {
                admitted = true;
                SeqEntry { seq, touch }
            });
        self.evict_if_over_capacity();
        admitted
    }

    /// Drop only the least-recently-touched entries down to
    /// [`Self::LOW_WATER`] when the map exceeds [`Self::MAX_ENTRIES`].
    ///
    /// The previous implementation cleared the WHOLE map on overflow,
    /// which forgot the ordering of every tracked `(sender, dest)`
    /// pair at once — a single overflow then let a delayed OLDER
    /// withdrawal for any route slip through and tear down a route the
    /// sender had since re-advertised (cubic review P2). Evicting only
    /// the idle tail preserves the ordering of the recently-active
    /// pairs, which are exactly the ones an in-flight reorder can
    /// threaten. O(n) but only on the rare overflow.
    fn evict_if_over_capacity(&self) {
        if self.seen.len() <= Self::MAX_ENTRIES {
            return;
        }
        let mut touches: Vec<u64> = self.seen.iter().map(|e| e.value().touch).collect();
        if touches.len() <= Self::LOW_WATER {
            return;
        }
        // The cutoff is the `LOW_WATER`-th newest touch; ticks are
        // unique, so retaining `touch >= cutoff` keeps exactly the
        // newest `LOW_WATER` pairs.
        let cutoff_idx = touches.len() - Self::LOW_WATER;
        touches.select_nth_unstable(cutoff_idx);
        let cutoff = touches[cutoff_idx];
        self.seen.retain(|_, e| e.touch >= cutoff);
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
    fn seq_gate_survives_overflow_eviction() {
        let gate = WithdrawalSeqGate::new();
        // dest 0 is inserted first, so it's the least-recently-touched
        // and gets evicted when the map overflows.
        for dest in 0..=(WithdrawalSeqGate::MAX_ENTRIES as u64) {
            assert!(gate.admit(1, dest, 1));
        }
        // The next admit trips the bound and evicts the idle tail
        // (including the long-untouched dest 0). The gate keeps
        // functioning: an evicted pair is admitted once, then gated.
        assert!(gate.admit(1, 0, 1), "evicted pair's sighting admits");
        assert!(!gate.admit(1, 0, 1), "gating resumes after the eviction");
    }

    #[test]
    fn seq_gate_overflow_preserves_recently_active_ordering() {
        // cubic P2: a whole-map clear on overflow would forget EVERY
        // pair's ordering and let a delayed OLDER withdrawal tear down
        // a re-established route. Bounded eviction must keep the
        // recently-active pairs' ordering intact.
        let gate = WithdrawalSeqGate::new();
        // Fill to the bound with filler pairs.
        for dest in 0..(WithdrawalSeqGate::MAX_ENTRIES as u64) {
            gate.admit(2, dest, 1);
        }
        // A hot pair, touched last → most-recently-active, seq 100.
        assert!(gate.admit(1, 7, 100));
        // One more insert tips over the bound and evicts the OLDEST
        // (filler) entries; the just-touched hot pair must survive.
        assert!(gate.admit(2, WithdrawalSeqGate::MAX_ENTRIES as u64, 1));
        // The hot pair's ordering is intact — a delayed older
        // withdrawal is still rejected (a whole-map clear would have
        // wrongly admitted it), while a genuinely newer one admits.
        assert!(
            !gate.admit(1, 7, 50),
            "recently-active pair's ordering must survive overflow eviction",
        );
        assert!(gate.admit(1, 7, 101), "a genuinely newer seq still admits");
    }

    #[test]
    fn seq_gate_admit_survives_the_overflow_it_triggers() {
        // cubic P1: the pair a withdrawal names is refreshed BEFORE
        // eviction victims are chosen, so an admit that trips the
        // capacity bound never evicts its own pair — the pair keeps
        // its ordering instead of being dropped and reinserted as a
        // fresh entry that would admit an older seq.
        let gate = WithdrawalSeqGate::new();
        // Fill exactly to the bound with filler pairs.
        for dest in 0..(WithdrawalSeqGate::MAX_ENTRIES as u64) {
            gate.admit(2, dest, 1);
        }
        // This pair's admit tips over the bound and triggers eviction;
        // it must survive (just sighted → newest) and then gate a
        // later stale withdrawal for itself.
        assert!(gate.admit(1, 7, 100), "overflow-triggering pair admitted");
        assert!(
            !gate.admit(1, 7, 50),
            "the pair that triggered the overflow kept its ordering",
        );
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
