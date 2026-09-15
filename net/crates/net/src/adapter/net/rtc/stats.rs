//! `RtcStats` — the counters Stage 3's exit criteria are asserted on.
//!
//! Every one of these exists because something is dropped, refused or
//! swallowed on a path where silence would be indistinguishable from
//! health. S0b's first driver dropped 19 476 packets while reporting
//! one refusal; that is the failure mode this type exists to make
//! impossible.

use std::sync::atomic::{AtomicU64, Ordering};

/// Counters for the RTC transport, shared by the driver, the sink and
/// the ingress path. Cheap relaxed atomics: no counter is read on a
/// decision path, only by tests and diagnostics.
///
/// # Field telemetry: `ice_direct / ice_attempted`
///
/// Plan §10's deployment metric. It is **reported, never gated** —
/// nothing in this crate reads it to make a decision.
///
/// **The denominator is attempts, not sessions.** An *attempt* is one
/// signalling dialog: [`Self::note_ice_attempted`] moves exactly
/// where a dialog is recorded, which is the `Offer` this node sent
/// ([`super::start_dialog`]) or an `Offer` it accepted
/// ([`super::handle_signal`]). So:
///
/// * a caller that retries after a timeout spends **two** attempts;
/// * an ICE restart inside one dialog is still **one** attempt;
/// * a peer that never gets a dialog (no announced Noise key, the
///   dialog budget refused the frame) contributes **nothing** — the
///   refusal is counted at its own gate, not here;
/// * an anchor answering a browser's bootstrap offer counts that
///   dialog too, so on an anchor the denominator includes the
///   browsers that merely arrived;
/// * **the bootstrap dialog counts on the browser's side too.** A
///   leaf spends one dialog reaching its anchor before it can offer
///   a peer anything, so a page that went direct with one peer
///   reports **two** attempts, not one — and a field reader
///   computing `ice_direct / ice_attempted` who is not expecting
///   that will find their ratio a fraction of what they predicted.
///   The bootstrap dialog is an ICE attempt by the same definition
///   as any other; it is simply one nobody pictures.
///
/// **Every count here is PER PARTICIPANT, never per system.** This
/// is one node's own ledger, so each side counts only the dialogs it
/// drove. Two browsers going direct through one anchor is three
/// distinct dialogs in the system — A↔anchor, B↔anchor, A↔B — but
/// **no counter anywhere reports three**: A reports two (its anchor
/// dialog and the peer dialog), B reports two, and the anchor
/// reports two (one bootstrap per leaf). Adding ledgers across nodes
/// double-counts every pair dialog, because both endpoints counted
/// it; a number derived that way matches nothing this surface emits.
///
/// **What the ratio does NOT tell you.** It is not a success rate
/// for sessions: a session can be perfectly healthy and contribute
/// a non-`ice_direct` attempt, because a **relayed session is not a
/// failed one** — the routed path through the anchor is a supported
/// disposition (§9 step 6), and ICE reaching `connected` is
/// explicitly not a health gate. It is also not a per-pair fact: a
/// pair that failed once and succeeded on the retry contributes one
/// `ice_direct` out of two attempts while being, right now, direct.
/// To ask whether a given pair is direct, ask the pair
/// (`peer_endpoint` / `peer_is_direct`), never this ratio.
///
/// **Zero attempts has no ratio.** `0/0` is not `0 %`. Every surface
/// that renders this reports "no attempts" — a null, a `—` — rather
/// than a number that reads as total failure.
///
/// # The partition, and why `ice_pending` exists
///
/// Each counted attempt ends in exactly one terminal outcome:
/// [`Self::ice_direct`], [`Self::ice_relayed`],
/// [`Self::ice_failed`], or — on the **leaf** only —
/// `udp_blocked`. Hence plan §10's identity:
///
/// ```text
/// ice_direct + ice_relayed + ice_failed + udp_blocked == ice_attempted
/// ```
///
/// Two qualifiers, both load-bearing:
///
/// 1. **The identity is only exact once every attempt has
///    TERMINATED.** An attempt in flight has been counted in the
///    denominator and in no outcome yet, so the sum is *less* than
///    `ice_attempted` by the number of live attempts. That residual
///    is [`Self::ice_pending`], and it is exposed rather than left
///    to be discovered: a sum that is off by one because a dialog
///    was still running otherwise reads as a lost count. Assert the
///    identity together with `ice_pending() == 0`, not after a
///    hopeful sleep.
/// 2. **`udp_blocked` is structurally not a native term.** A node
///    that is signalling over UDP cannot have UDP blocked, and this
///    surface has no probe that could establish the narrower cause,
///    so no such counter exists here — rather than a field frozen
///    at zero, which would read as "no UDP blocking observed", a
///    claim this surface is not entitled to make. The term lives on
///    the leaf, where `UdpBlockedEvidence` actually establishes it.
///    Natively the partition is `direct + relayed + failed +
///    pending == attempted`, which is what [`Self::ice_snapshot`]
///    reports.
#[derive(Debug, Default)]
pub struct RtcStats {
    accepted: AtomicU64,
    admission_refused_slots: AtomicU64,
    admission_refused_bytes: AtomicU64,
    admission_refused_advisory: AtomicU64,
    admission_refused_unknown_peer: AtomicU64,
    write_false: AtomicU64,
    written: AtomicU64,
    discarded_at_close: AtomicU64,
    drain_refused: AtomicU64,
    ingress_delivered: AtomicU64,
    ingress_dropped: AtomicU64,
    validate_rejected: AtomicU64,
    udp_conn_reset: AtomicU64,
    max_buffered: AtomicU64,
    retained: AtomicU64,
    admission_refused_forward: AtomicU64,
    admission_refused_transit: AtomicU64,
    admission_refused_route: AtomicU64,
    admission_refused_subscribe: AtomicU64,
    admission_refused_announce: AtomicU64,
    admission_refused_deliver: AtomicU64,
    admission_promoted: AtomicU64,
    close_notify_deferred: AtomicU64,
    close_notify_redelivered: AtomicU64,
    admission_reservation_retired: AtomicU64,
    admission_promotion_orphaned: AtomicU64,
    admission_rejected_outcome: AtomicU64,
    admission_reclaimed: AtomicU64,
    /// The **budget** refusal: a well-formed frame this sender may
    /// not send right now (too many open dialogs, too many frames in
    /// the window). Kept distinct from the two below so a witness
    /// that expects "no refusals" can say which refusal happened.
    signal_over_budget: AtomicU64,
    /// The frame did not decode as an `RtcSignalMsg` at all — bad
    /// postcard, unknown variant, or an SDP over `MAX_SDP_BYTES`.
    signal_malformed: AtomicU64,
    /// The frame was admitted but the engine's bounded queue was
    /// full (or the engine is gone), so it was dropped after
    /// admission.
    signal_engine_full: AtomicU64,
    signal_forwarded: AtomicU64,
    signal_delivered: AtomicU64,
    signal_unknown_dialog: AtomicU64,
    /// Unsolicited STUN binding requests this anchor's own responder
    /// answered (Stage 4b R8). An ICE connectivity check carries
    /// `USERNAME` and belongs to a session, so it is NOT counted
    /// here: this is "somebody used us as their STUN server", which
    /// is what a published `rtc_addr` is for.
    stun_binding_requests: AtomicU64,
    ice_attempted: AtomicU64,
    ice_direct: AtomicU64,
    ice_relayed: AtomicU64,
    ice_failed: AtomicU64,
}

macro_rules! counter {
    ($field:ident, $bump:ident, $doc:expr) => {
        #[doc = $doc]
        #[inline]
        pub fn $field(&self) -> u64 {
            self.$field.load(Ordering::Relaxed)
        }

        #[doc = concat!("Increment [`Self::", stringify!($field), "`].")]
        #[inline]
        pub fn $bump(&self) {
            self.$field.fetch_add(1, Ordering::Relaxed);
        }
    };
}

impl RtcStats {
    counter!(
        accepted,
        note_accepted,
        "Packets admitted into a peer's reserved queue. Once counted here the driver owns the packet."
    );
    counter!(
        admission_refused_slots,
        note_refused_slots,
        "Admission refusals because the reserved packet slots were exhausted — part of the hard bound."
    );
    counter!(
        admission_refused_bytes,
        note_refused_bytes,
        "Admission refusals because the reserved byte budget was exhausted — part of the hard bound."
    );
    counter!(
        admission_refused_advisory,
        note_refused_advisory,
        "Admission refusals because the published `buffered_amount` reading was over the advisory threshold."
    );
    counter!(
        admission_refused_unknown_peer,
        note_refused_unknown_peer,
        "Submissions for a peer the driver does not hold — a closed session, or a handle whose generation is spent."
    );
    counter!(
        write_false,
        note_write_false,
        "`Channel::write` returning `Ok(false)` after a passing precheck. NOT a loss: the packet is retained and retried."
    );
    counter!(written, note_written, "Packets `Channel::write` accepted.");
    counter!(
        discarded_at_close,
        note_discarded_at_close,
        "Packets still queued (or retained for retry) when a channel closed. The only place an admitted packet is lost, and it is counted."
    );
    counter!(
        drain_refused,
        note_drain_refused,
        "Scheduler-drain packets dropped after one re-queue attempt was itself refused."
    );
    counter!(
        ingress_delivered,
        note_ingress_delivered,
        "DataChannel messages handed to the receive loop."
    );
    counter!(
        ingress_dropped,
        note_ingress_dropped,
        "DataChannel messages dropped because the bounded ingress input was full. Never blocks the driver."
    );
    counter!(
        validate_rejected,
        note_validate_rejected,
        "RTC datagrams `NetHeader::validate` rejected — S0c's silent 8 KiB black hole, now audible."
    );
    counter!(
        admission_refused_forward,
        note_admission_refused_forward,
        "Forwarding refused for a provisional adjacent session (§12: no third-party relay before enrollment)."
    );
    counter!(
        admission_refused_transit,
        note_admission_refused_transit,
        "Routed-envelope TRANSIT refused for a provisional adjacent session (F1 specifically). Separate from the other forwarding sites so a witness can tell 'refused to relay onward' from 'refused to re-flood a pingwave'."
    );
    counter!(
        admission_refused_route,
        note_admission_refused_route,
        "Route installation withheld from a provisional session — it holds a session, not discovery participation."
    );
    counter!(
        admission_refused_subscribe,
        note_admission_refused_subscribe,
        "Channel subscription outside the bootstrap allow-list."
    );
    counter!(
        admission_refused_announce,
        note_admission_refused_announce,
        "Capability announcement from a provisional session, neither ingested nor flooded."
    );
    counter!(
        admission_refused_deliver,
        note_admission_refused_deliver,
        "Application delivery outside the bootstrap allow-list, decided after the nRPC envelope was decoded under bounds (§12 step 3)."
    );
    counter!(
        admission_promoted,
        note_admission_promoted,
        "Provisional sessions promoted by the enrollment handler, bound to the exact live session incarnation."
    );
    counter!(
        close_notify_redelivered,
        note_close_notify_redelivered,
        "Deferred closes later accepted by the mesh's notification channel. Every deferred close must eventually appear here: the difference is a close the driver recorded and then lost (R-A)."
    );
    counter!(
        close_notify_deferred,
        note_close_notify_deferred,
        "Channel closes the mesh's notification channel could not take immediately. Recorded on the slot and re-offered on a later driver turn (H3) — not discarded."
    );
    counter!(
        admission_reservation_retired,
        note_admission_reservation_retired,
        "Enrollment reservations retired because their session was evicted or replaced (R2). A late completion for one of these promotes nothing."
    );
    counter!(
        admission_promotion_orphaned,
        note_admission_promotion_orphaned,
        "Enrollment completions that found no reservation of their own (R2) - a retired call, or a response for a call that was never authorized."
    );
    counter!(
        admission_rejected_outcome,
        note_admission_rejected_outcome,
        "Enrollment exchanges whose `JoinOutcome` was Rejected. The session stays provisional — promoting on any response would have admitted a peer by the very message that refused it."
    );
    counter!(
        admission_reclaimed,
        note_admission_reclaimed,
        "Provisional sessions closed and reclaimed: expiry, a breached whole-session bound, or `max_provisional`."
    );
    counter!(
        signal_over_budget,
        note_signal_over_budget,
        "`0x0D02` frames refused by the per-sender dialog/frame budget. A silent drop here is indistinguishable from a peer that never signalled."
    );
    counter!(
        signal_malformed,
        note_signal_malformed,
        "`0x0D02` frames that did not decode as an `RtcSignalMsg`: bad postcard, an unknown variant, or an SDP over `MAX_SDP_BYTES`. Split out from `signal_over_budget` so a refusal names its cause."
    );
    counter!(
        signal_engine_full,
        note_signal_engine_full,
        "`0x0D02` frames admitted by the budget and then dropped because the engine's bounded queue was full (or the engine is gone). Split out from `signal_over_budget`: the sender did nothing wrong, this node did not keep up."
    );
    counter!(
        signal_forwarded,
        note_signal_forwarded,
        "`0x0D02` frames this node forwarded for a pair it is relaying. Counted by `subprotocol_id`, which is cleartext AAD-authenticated header — the SDP is never read (plan §10)."
    );
    counter!(
        signal_unknown_dialog,
        note_signal_unknown_dialog,
        "Signalling frames naming a dialog this node has no live attempt for (R5-A): unknown, already rejected, expired or completed. They reserve nothing — a frame cannot create the attempt it claims to belong to."
    );
    counter!(
        signal_delivered,
        note_signal_delivered,
        "`0x0D02` frames delivered to this node's own signalling handler."
    );
    counter!(
        stun_binding_requests,
        note_stun_binding_request,
        "Unsolicited STUN binding requests answered by this anchor's own responder — a peer using the published `rtc_addr` as its STUN target. ICE checks carry `USERNAME` and belong to a session; they are not counted here."
    );
    // ---------------------------------------------------------------
    // The ICE attempt ledger (plan §10). Read the type-level
    // "field telemetry" section above before changing any of the
    // four terms below: they are a PARTITION of `ice_attempted`,
    // and the natsim conformance matrix asserts that.
    // ---------------------------------------------------------------
    counter!(
        ice_attempted,
        note_ice_attempted,
        "**The denominator.** Direct-path ATTEMPTS: one per signalling dialog this node drove — the `Offer` we sent, or an `Offer` we accepted. Not sessions, not peers, not `connect` calls: a caller that retries spends two attempts, and an ICE restart inside one dialog is still one."
    );
    counter!(
        ice_direct,
        note_ice_direct,
        "Attempts that ended with an INSTALLED `PeerAddr::Rtc` endpoint. Not channel-open: an attempt whose DataChannel opened and whose Noise handshake then failed is `ice_failed`, never this."
    );
    counter!(
        ice_relayed,
        note_ice_relayed,
        "Attempts that reached their own `ice_deadline` with ICE never connected. For a peer dialog that is literally 'the pair stayed on the anchor' — the routed session was never replaced, and that is a normal disposition, not a failure. For an anchor-bootstrap dialog there is no routed fallback to stay on, so here the term means only 'the attempt timed out'."
    );
    counter!(
        ice_failed,
        note_ice_failed,
        "Attempts that ended without a direct path for a reason OTHER than their deadline: the DataChannel opened and the install then did not complete, an `Answer` the local driver could not use, the peer's `Reject`, or a bootstrap dialog the browser abandoned. Split from `ice_relayed` because 'we ran out of time' and 'this could not work' are different operational facts."
    );
    counter!(
        udp_conn_reset,
        note_udp_conn_reset,
        "`ConnectionReset` readings swallowed on the RTC socket (an ICMP port-unreachable about a peer that went away, never a socket fault)."
    );

    /// One coherent read of the ICE attempt ledger, as the Deck
    /// column, `net-mesh anchor stats` and the conformance matrix
    /// consume it.
    ///
    /// **Read the four counters together, not one at a time.** They
    /// are separate relaxed atomics, so four independent
    /// `load`s can straddle a terminating attempt and observe a
    /// denominator that has already counted an outcome the numerator
    /// has not — the classic way a ratio briefly exceeds 1. This
    /// loads the denominator LAST, so the only skew possible is an
    /// outcome that is not yet in `attempted`, which
    /// [`IceStats::pending`] saturates to zero instead of
    /// underflowing.
    #[inline]
    pub fn ice_snapshot(&self) -> IceStats {
        let direct = self.ice_direct();
        let relayed = self.ice_relayed();
        let failed = self.ice_failed();
        let attempted = self.ice_attempted();
        IceStats {
            attempted,
            direct,
            relayed,
            failed,
        }
    }

    /// Attempts counted in the denominator that have not reached any
    /// terminal outcome yet — the residual of plan §10's identity.
    ///
    /// Zero means every attempt has terminated, which is the only
    /// state in which the identity is exact. See the type docs.
    #[inline]
    pub fn ice_pending(&self) -> u64 {
        self.ice_snapshot().pending()
    }

    /// Packets the driver is holding in a retry slot right now —
    /// admitted, popped from the reserved queue, and not yet written.
    ///
    /// A gauge, not a counter, and the reason it exists: the retry
    /// slot is finite storage **outside** the queue reservation
    /// (`pop` releases the slot and bytes before the packet moves
    /// here), so `accepted == written + discarded_at_close + queued`
    /// is *not* the conservation law. This term is the missing one.
    #[inline]
    pub fn retained(&self) -> u64 {
        self.retained.load(Ordering::Relaxed)
    }

    /// Driver side: a packet entered a retry slot.
    #[inline]
    pub(super) fn note_retained(&self) {
        self.retained.fetch_add(1, Ordering::Relaxed);
    }

    /// Driver side: a retry slot was emptied (written, or discarded
    /// at close).
    #[inline]
    pub(super) fn note_unretained(&self) {
        let _ = self
            .retained
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
                Some(v.saturating_sub(1))
            });
    }

    /// Largest `buffered_amount` the driver has observed.
    #[inline]
    pub fn max_buffered(&self) -> u64 {
        self.max_buffered.load(Ordering::Relaxed)
    }

    /// Record an observed `buffered_amount`.
    #[inline]
    pub fn observe_buffered(&self, amount: usize) {
        self.max_buffered
            .fetch_max(amount as u64, Ordering::Relaxed);
    }

    /// Add `n` to [`Self::discarded_at_close`].
    #[inline]
    pub fn note_discarded_at_close_n(&self, n: u64) {
        self.discarded_at_close.fetch_add(n, Ordering::Relaxed);
    }

    /// Total admission refusals, by whichever gate fired.
    #[inline]
    pub fn admission_refused(&self) -> u64 {
        self.admission_refused_slots()
            + self.admission_refused_bytes()
            + self.admission_refused_advisory()
            + self.admission_refused_unknown_peer()
    }
}

/// The ICE attempt ledger, as `DeckClient::ice_stats`,
/// `net-mesh anchor stats` and Deck's ICE column report it:
/// plan §10's `ice_direct / ice_attempted` field telemetry with its
/// denominator and its residual carried alongside, so no renderer
/// has to reconstruct either.
///
/// # The denominator is ATTEMPTS, not sessions
///
/// [`Self::attempted`] counts **direct-path attempts**: one per
/// signalling dialog — the offer this node sent, or an offer it
/// accepted. A caller that retries after a timeout spends two
/// attempts; an ICE restart inside one dialog is one; a peer that
/// never got a dialog contributes nothing; and an anchor answering a
/// browser's bootstrap offer counts that dialog too — as does a
/// leaf's own bootstrap dialog with its anchor, so a page that went
/// direct with one peer reports two attempts rather than one.
///
/// It is **one participant's** ledger, never a system total. Two
/// browsers going direct through one anchor is three dialogs in the
/// system and no counter reports three: each browser reports two,
/// and the anchor reports two. Summing ledgers across nodes
/// double-counts every pair dialog.
///
/// # What the ratio does not tell you
///
/// [`Self::direct_ratio`] is **not a success rate for sessions**. A
/// relayed session is not a failed one: the routed path through an
/// anchor is a supported disposition, and ICE reaching `connected`
/// is deliberately not a health gate. Nor is it a per-pair fact — a
/// pair that failed once and succeeded on the retry shows as one
/// direct out of two attempts while being, right now, direct. Ask
/// the pair, not the ratio, whether a pair is direct.
///
/// # Reading it honestly
///
/// `direct + relayed + failed + pending == attempted` on this
/// surface. [`Self::pending`] is the attempts still in flight — the
/// reason a naive four-term sum can come up short — and plan §10's
/// fourth outcome, `udp_blocked`, is deliberately absent here: a
/// node signalling over UDP cannot have UDP blocked, so this
/// surface carries no such field rather than one frozen at zero
/// that would read as evidence. That term belongs to the browser
/// leaf, which can actually establish it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IceStats {
    /// **The denominator.** Direct-path attempts started: one per
    /// signalling dialog this node drove.
    pub attempted: u64,
    /// Attempts that ended with an installed direct RTC endpoint.
    pub direct: u64,
    /// Attempts that reached their deadline with ICE never
    /// connected. For a peer dialog: the pair stayed on the anchor,
    /// which is a normal disposition and not a failure.
    pub relayed: u64,
    /// Attempts that ended without a direct path for a reason other
    /// than their deadline.
    pub failed: u64,
}

impl IceStats {
    /// Attempts counted in the denominator that have not reached a
    /// terminal outcome yet.
    ///
    /// Saturating on purpose: the four counters are independent
    /// relaxed atomics, so a read that straddles a terminating
    /// attempt can see an outcome the denominator has not caught up
    /// with. That is a zero-length gap, not a negative population.
    #[inline]
    pub fn pending(&self) -> u64 {
        self.attempted
            .saturating_sub(self.direct)
            .saturating_sub(self.relayed)
            .saturating_sub(self.failed)
    }

    /// `ice_direct / ice_attempted`, or `None` when nothing has been
    /// attempted.
    ///
    /// **`None` is not zero.** `0/0` is not `0 %`; a node that has
    /// never attempted a direct path has no direct-path ratio, and
    /// rendering one as `0 %` would report total failure where
    /// nothing has happened. Every surface renders this absence as
    /// an absence.
    #[inline]
    pub fn direct_ratio(&self) -> Option<f64> {
        if self.attempted == 0 {
            return None;
        }
        Some(self.direct as f64 / self.attempted as f64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admission_refusals_sum_across_every_gate() {
        let s = RtcStats::default();
        s.note_refused_slots();
        s.note_refused_bytes();
        s.note_refused_advisory();
        s.note_refused_unknown_peer();
        assert_eq!(s.admission_refused(), 4);
        assert_eq!(s.accepted(), 0, "a refusal is not an acceptance");
    }

    #[test]
    fn the_buffered_high_water_mark_only_rises() {
        let s = RtcStats::default();
        s.observe_buffered(4096);
        s.observe_buffered(128);
        assert_eq!(
            s.max_buffered(),
            4096,
            "a later smaller reading must not erase the peak the advisory is judged against"
        );
    }

    /// The residual saturates rather than underflowing.
    ///
    /// The four ICE counters are independent relaxed atomics, so a
    /// read can straddle a terminating attempt and observe an
    /// outcome the denominator has not caught up with. `pending`
    /// documents that as a zero-length gap; this pins it, because
    /// the alternative on `u64` is a panic in debug and `u64::MAX`
    /// in release — a telemetry row reporting eighteen quintillion
    /// attempts in flight.
    #[test]
    fn the_ice_residual_saturates_instead_of_underflowing() {
        let skewed = IceStats {
            attempted: 2,
            direct: 2,
            relayed: 1,
            failed: 0,
        };
        assert_eq!(skewed.pending(), 0);
    }

    /// The ratio is the ratio, and the absence of one is not a
    /// number. Both directions, because collapsing them is the
    /// misreading this whole surface exists to prevent.
    #[test]
    fn no_attempts_has_no_ratio_and_a_terminated_attempt_has_a_real_one() {
        let empty = IceStats {
            attempted: 0,
            direct: 0,
            relayed: 0,
            failed: 0,
        };
        assert_eq!(empty.direct_ratio(), None, "0/0 is not 0 per cent");
        let ledger = IceStats {
            attempted: 4,
            direct: 3,
            relayed: 1,
            failed: 0,
        };
        assert_eq!(ledger.direct_ratio(), Some(0.75));
        assert_eq!(ledger.pending(), 0);
    }
}
