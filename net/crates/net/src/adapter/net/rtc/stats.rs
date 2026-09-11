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
    counter!(
        written,
        note_written,
        "Packets `Channel::write` accepted."
    );
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
        udp_conn_reset,
        note_udp_conn_reset,
        "`ConnectionReset` readings swallowed on the RTC socket (an ICMP port-unreachable about a peer that went away, never a socket fault)."
    );

    /// Largest `buffered_amount` the driver has observed.
    #[inline]
    pub fn max_buffered(&self) -> u64 {
        self.max_buffered.load(Ordering::Relaxed)
    }

    /// Record an observed `buffered_amount`.
    #[inline]
    pub fn observe_buffered(&self, amount: usize) {
        self.max_buffered.fetch_max(amount as u64, Ordering::Relaxed);
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
}
