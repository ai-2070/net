//! What the leaf refused, counted.
//!
//! Every drop in this crate is a *named* drop. That is not
//! bookkeeping for its own sake: S0c found that an over-cap payload
//! was dropped by `NetHeader::validate` "with no log and no counter —
//! the channel looks dead" (plan §7 bullet on `MAX_PAYLOAD_SIZE`), and
//! a leaf that drops non-forwarding traffic silently reproduces
//! exactly that failure mode one layer up. If the dispatcher declines
//! a packet, some counter here moves.
//!
//! `Cell`, not atomics: the leaf runs on the browser main thread
//! (S0b — `RTCPeerConnection` is undefined in workers) and the
//! [`ControlPlane`](crate::control_plane::ControlPlane) trait is
//! deliberately not `Send`. An atomic here would buy nothing and cost
//! a lock-prefixed instruction on the per-packet path.

use core::cell::Cell;

/// One reason a packet or event did not become an observable event.
///
/// The enum is the argument to `LeafCounters::drop`, so a new drop
/// site cannot be added without naming its reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropReason {
    /// A routing envelope whose `dest_id` is not this leaf. **Not an
    /// error** — non-forwarding is the leaf's role (plan §7,
    /// "Non-forwarding is a role, not a TTL"), so this is the
    /// expected disposition and it is counted rather than logged.
    NotAddressedToUs,
    /// A routing envelope that arrived already expired.
    RoutingExpired,
    /// A `subprotocol_id` this leaf has no arm for. Mixed-version
    /// degradation, same as the native dispatcher's
    /// unknown-subprotocol guard.
    UnknownSubprotocol,
    /// The bytes did not parse as a Net packet (or as the payload the
    /// subprotocol declares).
    Unparsable,
    /// The packet's AEAD counter was outside the replay window.
    Replay,
    /// The packet named a session this leaf does not hold.
    NoSession,
    /// A fire-and-forget stream skipped past this sequence. Dropping
    /// is the contract: fire-and-forget never stalls the consumer.
    FireAndForgetGap,
    /// A reliable stream's reorder buffer was full, so the oldest
    /// held sequence was released early and its gap abandoned.
    ReorderBufferFull,
    /// A fragment arrived for a reassembly this leaf refused to open,
    /// because `MAX_OUTSTANDING_REASSEMBLIES`
    /// were already in flight.
    ReassemblyRefused,
    /// A partial reassembly aged out before its last fragment came.
    ReassemblyExpired,
    /// A fragment that contradicted its group (overlapping offset,
    /// inconsistent total, or a piece past the declared end).
    ReassemblyInconsistent,
    /// A signed announcement whose signature did not verify. It is
    /// never ingested: `query` answers from verified state only.
    AnnouncementUnverified,
    /// A `0x0D02` signalling envelope that failed verification or
    /// replay checks.
    SignalRejected,
    /// A stream sequence that was already delivered — a retransmit
    /// arriving after its original, or a replayed packet. Distinct
    /// from [`DropReason::Replay`], which is the AEAD counter
    /// window: this one is the per-stream reorder buffer refusing to
    /// deliver the same sequence twice.
    DuplicateSequence,
    /// An nRPC frame whose `call_id` matches nothing in the call
    /// table — a late reply to a call that already ended, or one
    /// whose peer, session incarnation or reply route is not the
    /// triple the pending call was issued under.
    UnknownCall,
    /// A stream sequence so far beyond the next expected one that
    /// admitting it would let one authenticated packet retire an
    /// arbitrary span of the stream's sequence space.
    SequenceGapTooLarge,
    /// A reliable stream whose retransmits were exhausted, or that
    /// the peer reset. Terminal for that stream.
    StreamFailed,
}

impl DropReason {
    /// The stable string the JSON event and the report use.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotAddressedToUs => "not_addressed_to_us",
            Self::RoutingExpired => "routing_expired",
            Self::UnknownSubprotocol => "unknown_subprotocol",
            Self::Unparsable => "unparsable",
            Self::Replay => "replay",
            Self::NoSession => "no_session",
            Self::FireAndForgetGap => "fire_and_forget_gap",
            Self::ReorderBufferFull => "reorder_buffer_full",
            Self::ReassemblyRefused => "reassembly_refused",
            Self::ReassemblyExpired => "reassembly_expired",
            Self::ReassemblyInconsistent => "reassembly_inconsistent",
            Self::AnnouncementUnverified => "announcement_unverified",
            Self::SignalRejected => "signal_rejected",
            Self::DuplicateSequence => "duplicate_sequence",
            Self::UnknownCall => "unknown_call",
            Self::SequenceGapTooLarge => "sequence_gap_too_large",
            Self::StreamFailed => "stream_failed",
        }
    }

    /// Every reason, in declaration order. Used by the snapshot so a
    /// new variant appears in the JSON without a second edit.
    pub const ALL: [Self; 17] = [
        Self::NotAddressedToUs,
        Self::RoutingExpired,
        Self::UnknownSubprotocol,
        Self::Unparsable,
        Self::Replay,
        Self::NoSession,
        Self::FireAndForgetGap,
        Self::ReorderBufferFull,
        Self::ReassemblyRefused,
        Self::ReassemblyExpired,
        Self::ReassemblyInconsistent,
        Self::AnnouncementUnverified,
        Self::SignalRejected,
        Self::DuplicateSequence,
        Self::UnknownCall,
        Self::SequenceGapTooLarge,
        Self::StreamFailed,
    ];
}

/// The leaf's counters. One per node.
#[derive(Debug, Default)]
pub struct LeafCounters {
    drops: [Cell<u64>; DropReason::ALL.len()],
    packets_in: Cell<u64>,
    packets_out: Cell<u64>,
    fragments_out: Cell<u64>,
    reassembled: Cell<u64>,
    ice_attempted: Cell<u64>,
    ice_direct: Cell<u64>,
    ice_failed: Cell<u64>,
    udp_blocked: Cell<u64>,
}

impl LeafCounters {
    /// A fresh set, all zero.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one drop.
    #[inline]
    pub fn drop_for(&self, reason: DropReason) {
        let slot = &self.drops[reason as usize];
        slot.set(slot.get().saturating_add(1));
    }

    /// Record `n` drops for one reason in constant time.
    ///
    /// A sequence gap is `n` lost packets, and the aggregate must
    /// say so — but an authenticated peer picks the gap's size, so
    /// the receive path must not pay a loop iteration per absent
    /// sequence. One saturating add, whatever `n` is.
    #[inline]
    pub fn drop_n(&self, reason: DropReason, n: u64) {
        let slot = &self.drops[reason as usize];
        slot.set(slot.get().saturating_add(n));
    }

    /// How many drops have been recorded for `reason`.
    #[inline]
    pub fn drops(&self, reason: DropReason) -> u64 {
        self.drops[reason as usize].get()
    }

    /// Total drops across every reason.
    pub fn total_drops(&self) -> u64 {
        self.drops.iter().map(Cell::get).sum()
    }

    /// One inbound packet accepted off the transport (before dispatch).
    #[inline]
    pub fn packet_in(&self) {
        bump(&self.packets_in);
    }

    /// One outbound packet handed to the transport.
    #[inline]
    pub fn packet_out(&self) {
        bump(&self.packets_out);
    }

    /// One outbound packet that was a fragment of a larger payload.
    #[inline]
    pub fn fragment_out(&self) {
        bump(&self.fragments_out);
    }

    /// One inbound payload completed from fragments.
    #[inline]
    pub fn reassembled(&self) {
        bump(&self.reassembled);
    }

    /// §10 `ice_attempted`.
    #[inline]
    pub fn ice_attempted(&self) {
        bump(&self.ice_attempted);
    }

    /// §10 `ice_direct`.
    #[inline]
    pub fn ice_direct(&self) {
        bump(&self.ice_direct);
    }

    /// §10 `ice_failed`.
    #[inline]
    pub fn ice_failed(&self) {
        bump(&self.ice_failed);
    }

    /// §10 `udp_blocked` — moved only when the narrower cause was
    /// actually established (see
    /// [`UdpBlockedEvidence`](crate::error::UdpBlockedEvidence)).
    #[inline]
    pub fn udp_blocked(&self) {
        bump(&self.udp_blocked);
    }

    /// Every counter as a JSON object. u64s are decimal **strings**:
    /// these are counters a page may render, and `JSON.parse` rounds
    /// integers above 2^53.
    pub fn to_json(&self) -> String {
        let mut out = String::from("{");
        out.push_str(&format!("\"packets_in\":\"{}\"", self.packets_in.get()));
        out.push_str(&format!(",\"packets_out\":\"{}\"", self.packets_out.get()));
        out.push_str(&format!(
            ",\"fragments_out\":\"{}\"",
            self.fragments_out.get()
        ));
        out.push_str(&format!(",\"reassembled\":\"{}\"", self.reassembled.get()));
        out.push_str(&format!(
            ",\"ice_attempted\":\"{}\"",
            self.ice_attempted.get()
        ));
        out.push_str(&format!(",\"ice_direct\":\"{}\"", self.ice_direct.get()));
        out.push_str(&format!(",\"ice_failed\":\"{}\"", self.ice_failed.get()));
        out.push_str(&format!(",\"udp_blocked\":\"{}\"", self.udp_blocked.get()));
        out.push_str(",\"drops\":{");
        for (i, reason) in DropReason::ALL.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str(&format!(
                "\"{}\":\"{}\"",
                reason.as_str(),
                self.drops(*reason)
            ));
        }
        out.push_str("}}");
        out
    }
}

#[inline]
fn bump(cell: &Cell<u64>) {
    cell.set(cell.get().saturating_add(1));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_reason_has_its_own_slot() {
        let c = LeafCounters::new();
        for reason in DropReason::ALL {
            c.drop_for(reason);
        }
        for reason in DropReason::ALL {
            assert_eq!(c.drops(reason), 1, "{reason:?} shares a slot");
        }
        assert_eq!(c.total_drops(), DropReason::ALL.len() as u64);
    }

    #[test]
    fn reason_names_are_unique() {
        let mut names: Vec<&str> = DropReason::ALL.iter().map(|r| r.as_str()).collect();
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        assert_eq!(before, names.len(), "two reasons share a wire name");
    }

    #[test]
    fn the_json_snapshot_carries_every_reason_as_a_string() {
        let c = LeafCounters::new();
        c.drop_for(DropReason::NotAddressedToUs);
        let json = c.to_json();
        for reason in DropReason::ALL {
            assert!(json.contains(reason.as_str()), "{reason:?} missing: {json}");
        }
        assert!(json.contains("\"not_addressed_to_us\":\"1\""), "{json}");
    }
}
