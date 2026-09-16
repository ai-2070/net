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
    /// A fragment arrived for a reassembly this leaf refused to open,
    /// because `MAX_OUTSTANDING_REASSEMBLIES`
    /// were already in flight.
    ReassemblyRefused,
    /// A partial reassembly aged out before its last fragment came.
    ReassemblyExpired,
    /// A fragment byte-identical to one the group already holds, on
    /// the same offset and the same stream sequence — a legitimate
    /// retransmission of a piece whose acknowledgement was lost.
    /// The group keeps what it has and the ack is repeated; this
    /// counts the no-op so the recovery is observable.
    ReassemblyDuplicate,
    /// A fragment that contradicted its group: an overlapping or
    /// conflicting offset, an inconsistent total, a piece past the
    /// declared end, delivery metadata differing from the group's
    /// first piece, or a sequence the group cannot own.
    ReassemblyInconsistent,
    /// A signed announcement whose signature did not verify. It is
    /// never ingested: `query` answers from verified state only.
    AnnouncementUnverified,
    /// A `0x0D02` signalling envelope that failed verification or
    /// replay checks.
    SignalRejected,
    /// A `0x0D02` envelope refused because the replay set already
    /// holds [`MAX_REMEMBERED_SIGNALS`] *unexpired* admissions.
    /// Distinct from [`DropReason::SignalRejected`]: nothing is
    /// wrong with this envelope, and the alternative — evicting a
    /// live entry to make room — would trade replay protection for
    /// cardinality.
    ///
    /// [`MAX_REMEMBERED_SIGNALS`]: crate::signal::MAX_REMEMBERED_SIGNALS
    SignalCapacityRefused,
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
    /// A record for a stream whose consumer called `close`. The
    /// receive cursor stays live for the rest of the session — the
    /// peer's transmit sequence was never rewound, so a reopen has
    /// to resume where the peer actually is — but there is no
    /// consumer to deliver to, so the bytes are counted here rather
    /// than queued for one that may never come back.
    StreamClosed,
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
            Self::ReassemblyRefused => "reassembly_refused",
            Self::ReassemblyExpired => "reassembly_expired",
            Self::ReassemblyDuplicate => "reassembly_duplicate",
            Self::ReassemblyInconsistent => "reassembly_inconsistent",
            Self::AnnouncementUnverified => "announcement_unverified",
            Self::SignalRejected => "signal_rejected",
            Self::SignalCapacityRefused => "signal_capacity_refused",
            Self::DuplicateSequence => "duplicate_sequence",
            Self::UnknownCall => "unknown_call",
            Self::SequenceGapTooLarge => "sequence_gap_too_large",
            Self::StreamFailed => "stream_failed",
            Self::StreamClosed => "stream_closed",
        }
    }

    /// Every reason, in declaration order. Used by the snapshot so a
    /// new variant appears in the JSON without a second edit.
    pub const ALL: [Self; 19] = [
        Self::NotAddressedToUs,
        Self::RoutingExpired,
        Self::UnknownSubprotocol,
        Self::Unparsable,
        Self::Replay,
        Self::NoSession,
        Self::FireAndForgetGap,
        Self::ReassemblyRefused,
        Self::ReassemblyExpired,
        Self::ReassemblyDuplicate,
        Self::ReassemblyInconsistent,
        Self::AnnouncementUnverified,
        Self::SignalRejected,
        Self::SignalCapacityRefused,
        Self::DuplicateSequence,
        Self::UnknownCall,
        Self::SequenceGapTooLarge,
        Self::StreamFailed,
        Self::StreamClosed,
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
    ice_relayed: Cell<u64>,
    ice_failed: Cell<u64>,
    udp_blocked: Cell<u64>,
    credit_grants_sent: Cell<u64>,
    credit_grants_received: Cell<u64>,
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

    /// §10 `ice_relayed` — the attempt reached its own deadline with
    /// ICE never connected.
    ///
    /// **Not a failure.** For a peer dialog this is literally "the
    /// pair stayed on the anchor": the routed session was never
    /// replaced and the peer is still reachable, which §9 step 6
    /// makes an expected disposition rather than an error. For the
    /// anchor-bootstrap dialog there is no routed fallback to stay
    /// on, so here it means only "the attempt timed out".
    ///
    /// **The anchor-bootstrap dialog is an attempt too.** A leaf
    /// spends one dialog reaching its anchor before it can offer a
    /// peer anything, so a page that went direct with one peer
    /// reports TWO attempts, not one. A field reader computing
    /// `ice_direct / ice_attempted` who is not expecting that will
    /// find their ratio a fraction of what they predicted.
    ///
    /// These counts are PER PARTICIPANT, never per system: this is
    /// this leaf's own ledger. Two leaves going direct through one
    /// anchor is three dialogs in the system and no counter reports
    /// three — each leaf reports two and the anchor reports two.
    /// Summing ledgers across nodes double-counts every pair dialog.
    ///
    /// Exactly one of `ice_direct`, `ice_relayed`, `ice_failed` and
    /// `udp_blocked` moves per counted attempt, which is what makes
    /// §10's identity
    /// `ice_direct + ice_relayed + ice_failed + udp_blocked ==
    /// ice_attempted` assertable. The residual,
    /// `ice_attempted - (the four)`, is the attempts still IN
    /// FLIGHT — the identity is exact only once that residual is
    /// zero, so assert it together with the sum rather than after a
    /// hopeful wait. At the deadline the evidence takes precedence:
    /// `UdpBlockedEvidence` established means `udp_blocked` and not
    /// this counter.
    #[inline]
    pub fn ice_relayed(&self) {
        bump(&self.ice_relayed);
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

    /// One stream-window frame this leaf put on the wire — the
    /// credit and cumulative ack a sender's window is replenished
    /// and pruned by.
    ///
    /// Counted because "the transfer finished" is not evidence of
    /// replenishment on its own: a window-sized transfer completes
    /// on the implicit initial credit alone. Traffic past the window
    /// plus a non-zero count here is.
    #[inline]
    pub fn credit_grant_sent(&self) {
        bump(&self.credit_grants_sent);
    }

    /// One stream-window frame this leaf applied from its peer —
    /// the replenishment its own send window is spending.
    #[inline]
    pub fn credit_grant_received(&self) {
        bump(&self.credit_grants_received);
    }

    /// Stream-window frames emitted.
    #[inline]
    pub fn credit_grants_sent(&self) -> u64 {
        self.credit_grants_sent.get()
    }

    /// Stream-window frames applied.
    #[inline]
    pub fn credit_grants_received(&self) -> u64 {
        self.credit_grants_received.get()
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
        out.push_str(&format!(",\"ice_relayed\":\"{}\"", self.ice_relayed.get()));
        out.push_str(&format!(",\"ice_failed\":\"{}\"", self.ice_failed.get()));
        out.push_str(&format!(",\"udp_blocked\":\"{}\"", self.udp_blocked.get()));
        out.push_str(&format!(
            ",\"credit_grants_sent\":\"{}\"",
            self.credit_grants_sent.get()
        ));
        out.push_str(&format!(
            ",\"credit_grants_received\":\"{}\"",
            self.credit_grants_received.get()
        ));
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

    /// The RTC transport's own ledger, in the field names the NATIVE
    /// `RtcStats` uses — plan §10's `RtcStats` exposed on the leaf.
    ///
    /// `u64`s are decimal **strings**, the same rule
    /// [`Self::to_json`] follows and for the same reason.
    ///
    /// # One spelling, across anchors and browsers
    ///
    /// Every emitted name exists natively and means the same thing.
    /// There is deliberately no leaf-specific spelling for a fact
    /// both sides record: an operator reading
    /// `ice_direct / ice_attempted` over a mesh of anchors and
    /// browsers is reading ONE metric, and two spellings would make
    /// it two.
    ///
    /// # `not_applicable`, and why it is not a wall of zeros
    ///
    /// 24 native fields have no leaf meaning, and each says so with
    /// its reason instead of reporting `0`. A zero is an
    /// OBSERVATION — "no ingress overflow", "no STUN requests
    /// answered" — and a leaf that has no such mechanism is not
    /// entitled to the claim. Native `RtcStats` makes exactly this
    /// choice in the other direction: it carries no `udp_blocked`
    /// field because a node signalling over UDP cannot have UDP
    /// blocked, "rather than a field frozen at zero, which would
    /// read as 'no UDP blocking observed'".
    ///
    /// `udp_blocked` is therefore the one emitted term with no
    /// native counterpart, and it is emitted here because the leaf
    /// is the side that can actually establish it
    /// ([`crate::error::UdpBlockedEvidence`]).
    ///
    /// # `ice_pending` is derived, on both sides
    ///
    /// Not a field here and not a field natively:
    /// `ice_attempted - (direct + relayed + failed + udp_blocked)`
    /// is the residual, i.e. the attempts still in flight, and the
    /// §10 identity is exact only where it is zero. The arithmetic
    /// is the same on both surfaces, so it is done by the reader
    /// rather than emitted twice.
    pub fn rtc_stats_json(&self, link: &RtcLinkSnapshot) -> String {
        let mut out = String::from("{");
        for (name, value) in [
            ("accepted", link.accepted),
            ("written", link.written),
            ("write_false", link.write_false),
            ("retained", link.retained),
            ("discarded_at_close", link.discarded_at_close),
            ("max_buffered", link.max_buffered),
            ("admission_refused_slots", link.admission_refused_slots),
            ("admission_refused_bytes", link.admission_refused_bytes),
            (
                "admission_refused_advisory",
                link.admission_refused_advisory,
            ),
            (
                "admission_refused_unknown_peer",
                link.admission_refused_unknown_peer,
            ),
            ("ingress_delivered", link.ingress_delivered),
            ("ice_attempted", self.ice_attempted.get()),
            ("ice_direct", self.ice_direct.get()),
            ("ice_relayed", self.ice_relayed.get()),
            ("ice_failed", self.ice_failed.get()),
            ("udp_blocked", self.udp_blocked.get()),
        ] {
            if out.len() > 1 {
                out.push(',');
            }
            out.push_str(&format!("\"{name}\":\"{value}\""));
        }
        out.push_str(",\"not_applicable\":{");
        for (i, (name, reason)) in RTC_STATS_NOT_APPLICABLE.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str(&format!(
                "\"{name}\":{}",
                serde_json::Value::String((*reason).to_string())
            ));
        }
        out.push_str("}}");
        out
    }

    /// The names [`Self::rtc_stats_json`] emits, in emission order.
    ///
    /// Exported so the correspondence test can compare the emitted
    /// set against [`NATIVE_RTC_STATS_FIELDS`] without parsing the
    /// JSON it is asserting about.
    pub const RTC_STATS_EMITTED: [&'static str; 16] = [
        "accepted",
        "written",
        "write_false",
        "retained",
        "discarded_at_close",
        "max_buffered",
        "admission_refused_slots",
        "admission_refused_bytes",
        "admission_refused_advisory",
        "admission_refused_unknown_peer",
        "ingress_delivered",
        "ice_attempted",
        "ice_direct",
        "ice_relayed",
        "ice_failed",
        "udp_blocked",
    ];
}

/// One reading of the browser's RTC link, in the field names the
/// NATIVE `RtcStats` uses.
///
/// Plain `u64`s and no `wasm-bindgen`, so the rendering above is
/// testable on the host. [`crate::rtc::RtcLinkCounters`] is the live
/// side; this is the snapshot it hands over.
///
/// Every field here exists natively under the same spelling
/// (`net/src/adapter/net/rtc/stats.rs`) and means the same thing.
/// That is the point of the type: a leaf-specific spelling for a
/// fact both sides record would make one deployment metric two, and
/// the operator reading `ice_direct / ice_attempted` across a mesh
/// of anchors and browsers is reading one number.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct RtcLinkSnapshot {
    /// Packets admitted into the peer's retained queue. Once
    /// counted the transport owns the packet (§2).
    pub accepted: u64,
    /// Packets `RTCDataChannel.send` took.
    pub written: u64,
    /// `send` throwing after a passing precheck. NOT a loss: the
    /// packet stays at the head of the queue and
    /// `bufferedamountlow` retries it.
    pub write_false: u64,
    /// Packets retained right now — a gauge, not a counter.
    pub retained: u64,
    /// Packets still retained when a channel closed. The only place
    /// an admitted packet is lost, and it is counted.
    pub discarded_at_close: u64,
    /// The largest `bufferedAmount` observed.
    pub max_buffered: u64,
    /// Admission refusals because the reserved packet slots were
    /// exhausted — half of the hard bound.
    pub admission_refused_slots: u64,
    /// Admission refusals because the reserved byte budget was
    /// exhausted — the other half.
    pub admission_refused_bytes: u64,
    /// Admission refusals because the live `bufferedAmount` reading
    /// was at or over the advisory threshold.
    pub admission_refused_advisory: u64,
    /// Sends for a peer this transport cannot address: no link, or
    /// a link whose channel is not open.
    pub admission_refused_unknown_peer: u64,
    /// DataChannel messages handed to the node.
    pub ingress_delivered: u64,
}

/// Every field the native `RtcStats` carries, verbatim.
///
/// The inventory a reader diffs against
/// `net/src/adapter/net/rtc/stats.rs`, and the reason
/// [`LeafCounters::rtc_stats_json`] cannot quietly grow a spelling
/// of its own or quietly forget one: the emitted names plus
/// [`RTC_STATS_NOT_APPLICABLE`] are asserted to be exactly this
/// list, with nothing in both.
pub const NATIVE_RTC_STATS_FIELDS: [&str; 39] = [
    "accepted",
    "admission_refused_slots",
    "admission_refused_bytes",
    "admission_refused_advisory",
    "admission_refused_unknown_peer",
    "write_false",
    "written",
    "discarded_at_close",
    "drain_refused",
    "ingress_delivered",
    "ingress_dropped",
    "validate_rejected",
    "udp_conn_reset",
    "max_buffered",
    "retained",
    "admission_refused_forward",
    "admission_refused_transit",
    "admission_refused_route",
    "admission_refused_subscribe",
    "admission_refused_announce",
    "admission_refused_deliver",
    "admission_promoted",
    "close_notify_deferred",
    "close_notify_redelivered",
    "admission_reservation_retired",
    "admission_promotion_orphaned",
    "admission_rejected_outcome",
    "admission_reclaimed",
    "signal_over_budget",
    "signal_malformed",
    "signal_engine_full",
    "signal_forwarded",
    "signal_delivered",
    "signal_unknown_dialog",
    "stun_binding_requests",
    "ice_attempted",
    "ice_direct",
    "ice_relayed",
    "ice_failed",
];

/// Native `RtcStats` fields a leaf has no meaning for, each with the
/// reason it has none.
///
/// **Said rather than zeroed.** A field frozen at `0` reads as an
/// observation — "no ingress overflow", "no STUN requests answered",
/// "no admission refusals" — and a leaf is not entitled to any of
/// those claims. Native `RtcStats` makes the same choice in the
/// other direction and says so: it carries no `udp_blocked` field,
/// "rather than a field frozen at zero, which would read as 'no UDP
/// blocking observed', a claim this surface is not entitled to
/// make". This is that rule applied to the leaf's 24.
pub const RTC_STATS_NOT_APPLICABLE: [(&str, &str); 24] = [
    (
        "ingress_dropped",
        "the leaf's inbound queue is an unbounded VecDeque the pump drains on the same turn, \
         so there is no bounded input to overflow",
    ),
    (
        "drain_refused",
        "there is no scheduler drain: a refused write leaves the packet at the head of the \
         retained queue and `bufferedamountlow` retries it",
    ),
    (
        "validate_rejected",
        "the leaf's inbound refusals are named one by one in `counters().drops`, by \
         DropReason, rather than collapsed into a single term",
    ),
    (
        "udp_conn_reset",
        "a browser holds no UDP socket, so there is no ICMP port-unreachable reading to \
         swallow",
    ),
    (
        "admission_refused_forward",
        "§12 admission is the ANCHOR's: a leaf relays for nobody",
    ),
    (
        "admission_refused_transit",
        "§12 admission is the ANCHOR's: a leaf carries no routed envelope in transit",
    ),
    (
        "admission_refused_route",
        "§12 admission is the ANCHOR's: a leaf installs no routes for other nodes",
    ),
    (
        "admission_refused_subscribe",
        "§12 admission is the ANCHOR's: a leaf serves no subscriptions",
    ),
    (
        "admission_refused_announce",
        "§12 admission is the ANCHOR's: a leaf floods no announcements",
    ),
    (
        "admission_refused_deliver",
        "§12 admission is the ANCHOR's: a leaf answers no third party's nRPC",
    ),
    (
        "admission_promoted",
        "a leaf enrolls WITH an anchor; it promotes nobody",
    ),
    (
        "close_notify_deferred",
        "the mesh's close-notification channel is native; a leaf's close is synchronous in \
         `PeerLink::drop` and cannot be deferred",
    ),
    (
        "close_notify_redelivered",
        "nothing is deferred, so nothing is redelivered",
    ),
    (
        "admission_reservation_retired",
        "enrollment reservations are the anchor's accounting, not the enrollee's",
    ),
    (
        "admission_promotion_orphaned",
        "enrollment reservations are the anchor's accounting, not the enrollee's",
    ),
    (
        "admission_rejected_outcome",
        "a leaf READS its own JoinOutcome; it adjudicates nobody else's",
    ),
    (
        "admission_reclaimed",
        "provisional sessions are reclaimed by the anchor that admitted them",
    ),
    (
        "signal_over_budget",
        "the per-sender `0x0D02` dialog/frame budget is the forwarding anchor's gate",
    ),
    (
        "signal_malformed",
        "a leaf decodes only envelopes addressed to itself, and an undecodable one is \
         counted as a drop by reason",
    ),
    (
        "signal_engine_full",
        "there is no bounded signalling engine queue: a verified envelope is filed on its \
         dialog synchronously",
    ),
    (
        "signal_forwarded",
        "blind `0x0D02` forwarding is the anchor's, and it is what makes §10's flat \
         application-data counter observable",
    ),
    (
        "signal_delivered",
        "a leaf's delivered envelopes are the verified ones filed on a dialog; the term \
         natively counts frames a NODE handed to its own signalling handler on behalf of \
         pairs it relays for",
    ),
    (
        "signal_unknown_dialog",
        "an envelope for a dialog this leaf is not driving is dropped by \
         `Inner::file_signal` and counted as a drop by reason",
    ),
    (
        "stun_binding_requests",
        "a leaf serves no STUN: it is a client of the address its anchor published",
    ),
];

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

    /// Every native `RtcStats` field is accounted for exactly once:
    /// emitted with a leaf meaning, or declared inapplicable with
    /// the reason it has none. Nothing in both, nothing in neither.
    ///
    /// This is the assertion that keeps "the same field names as
    /// native" true as either side grows a counter. A leaf-only
    /// spelling would show up as an emitted name that is not in the
    /// native inventory; a native field nobody decided about would
    /// show up as missing from both sets - and the second is the
    /// dangerous one, because the alternative to deciding is a
    /// plausible zero.
    #[test]
    fn every_native_rtc_stats_field_is_emitted_or_declared_inapplicable() {
        let mut accounted: Vec<&str> = LeafCounters::RTC_STATS_EMITTED
            .iter()
            .copied()
            // `udp_blocked` is the one emitted term with NO native
            // counterpart, on purpose: native says it has no such
            // field because a node signalling over UDP cannot have
            // UDP blocked, and the leaf is the side whose
            // `UdpBlockedEvidence` can establish it.
            .filter(|name| *name != "udp_blocked")
            .chain(RTC_STATS_NOT_APPLICABLE.iter().map(|(name, _)| *name))
            .collect();
        let overlap: Vec<&str> = LeafCounters::RTC_STATS_EMITTED
            .iter()
            .copied()
            .filter(|name| {
                RTC_STATS_NOT_APPLICABLE
                    .iter()
                    .any(|(other, _)| other == name)
            })
            .collect();
        assert!(
            overlap.is_empty(),
            "these are emitted AND declared inapplicable: {overlap:?}"
        );

        let mut native: Vec<&str> = NATIVE_RTC_STATS_FIELDS.to_vec();
        accounted.sort_unstable();
        native.sort_unstable();
        assert_eq!(
            accounted, native,
            "the leaf's RtcStats surface has drifted from the native field inventory in \
             net/src/adapter/net/rtc/stats.rs"
        );
    }

    /// The inapplicable fields carry a REASON, not a zero.
    #[test]
    fn an_inapplicable_field_says_why_instead_of_reporting_zero() {
        let json = LeafCounters::new().rtc_stats_json(&RtcLinkSnapshot::default());
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
        let na = parsed
            .get("not_applicable")
            .and_then(serde_json::Value::as_object)
            .expect("not_applicable is an object");
        assert_eq!(na.len(), RTC_STATS_NOT_APPLICABLE.len());
        for (name, _) in RTC_STATS_NOT_APPLICABLE {
            let reason = na
                .get(name)
                .and_then(serde_json::Value::as_str)
                .unwrap_or_else(|| panic!("{name} carries no reason: {json}"));
            assert!(reason.len() > 20, "{name}'s reason is not one: {reason:?}");
            // The whole point: the field is absent from the reading
            // itself, so no consumer can read it as an observation.
            assert!(
                parsed.get(name).is_none(),
                "{name} is declared inapplicable AND reported: {json}"
            );
        }
    }

    /// The reading reports the transport's and the ICE ledger's real
    /// values, as decimal strings.
    #[test]
    fn the_rtc_reading_reports_both_halves_as_decimal_strings() {
        let c = LeafCounters::new();
        c.ice_attempted();
        c.ice_attempted();
        c.ice_direct();
        c.udp_blocked();
        let link = RtcLinkSnapshot {
            accepted: 7,
            written: 6,
            write_false: 1,
            retained: 1,
            discarded_at_close: 0,
            // Past 2^53: a `JSON.parse` of a bare number would round
            // it, which is why every value here is a string.
            max_buffered: 9_007_199_254_740_993,
            admission_refused_slots: 2,
            admission_refused_bytes: 3,
            admission_refused_advisory: 4,
            admission_refused_unknown_peer: 5,
            ingress_delivered: 11,
        };
        let parsed: serde_json::Value =
            serde_json::from_str(&c.rtc_stats_json(&link)).expect("valid JSON");
        for (name, want) in [
            ("accepted", "7"),
            ("written", "6"),
            ("write_false", "1"),
            ("retained", "1"),
            ("max_buffered", "9007199254740993"),
            ("admission_refused_slots", "2"),
            ("admission_refused_bytes", "3"),
            ("admission_refused_advisory", "4"),
            ("admission_refused_unknown_peer", "5"),
            ("ingress_delivered", "11"),
            ("ice_attempted", "2"),
            ("ice_direct", "1"),
            ("ice_relayed", "0"),
            ("ice_failed", "0"),
            ("udp_blocked", "1"),
        ] {
            assert_eq!(
                parsed.get(name).and_then(serde_json::Value::as_str),
                Some(want),
                "{name}"
            );
        }
    }
}
