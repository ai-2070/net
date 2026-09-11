//! The admission side of the RTC transport: per-peer reserved queues.
//!
//! This is the half that mesh tasks touch. It holds no `str0m::Rtc`
//! and never blocks: [`RtcTransport::submit`] is synchronous and
//! total, and every refusal happens here, before the packet is
//! anybody's responsibility. Past a successful `submit` the driver
//! owns the packet (§2, and S0b's retain-and-retry finding).
//!
//! Two gates, in this order:
//!
//! 1. **The hard bound** — reserved slots and reserved bytes
//!    (`send_queue_packets` / `send_queue_bytes`). Exact, taken under
//!    the queue lock, so a concurrent submit cannot overshoot it.
//! 2. **The advisory** — the `buffered_amount` the driver published
//!    before its last write to this peer. It may refuse *earlier* than
//!    the hard bound; it can never define the bound, because it is
//!    only refreshed while the driver is writing to that peer (S0b
//!    §4b: an idle-then-burst peer reads a stale zero).

use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::Arc;

use bytes::Bytes;
use dashmap::DashMap;
use parking_lot::Mutex;

use super::config::RtcConfig;
use super::stats::RtcStats;
use super::RtcPeerId;

/// Why a submission was refused. Every variant is an admission-time
/// decision: nothing here can happen after acceptance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RtcSubmitError {
    /// Reserved packet slots exhausted.
    QueueFull,
    /// Reserved byte budget exhausted.
    BytesFull,
    /// The driver's published `buffered_amount` is over the advisory
    /// threshold.
    AdvisoryOver,
    /// No such peer: never opened, already closed, or a handle whose
    /// generation is spent.
    UnknownPeer,
}

impl std::fmt::Display for RtcSubmitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::QueueFull => write!(f, "rtc admission: reserved queue slots exhausted"),
            Self::BytesFull => write!(f, "rtc admission: reserved queue bytes exhausted"),
            Self::AdvisoryOver => write!(f, "rtc admission: buffered-amount advisory exceeded"),
            Self::UnknownPeer => write!(f, "rtc admission: no open channel for this peer"),
        }
    }
}

impl std::error::Error for RtcSubmitError {}

impl From<RtcSubmitError> for std::io::Error {
    /// Every admission refusal is `WouldBlock`, which is what makes
    /// the sink's RTC half indistinguishable from the UDP half at the
    /// call sites: `WouldBlock` is backpressure, per the row's
    /// disposition, never a transport error.
    fn from(e: RtcSubmitError) -> Self {
        let kind = match e {
            RtcSubmitError::UnknownPeer => std::io::ErrorKind::NotConnected,
            _ => std::io::ErrorKind::WouldBlock,
        };
        std::io::Error::new(kind, e)
    }
}

/// One peer's reserved queue.
#[derive(Debug)]
struct PeerSlot {
    generation: u32,
    /// Queue and its byte count under one lock: the reservation has to
    /// be atomic with the push or two concurrent submits can both see
    /// room for the last slot.
    queue: Mutex<(std::collections::VecDeque<Bytes>, usize)>,
    /// Last `buffered_amount` the driver published for this peer.
    /// Advisory: stale by construction between the driver's writes.
    published_buffered: AtomicUsize,
    /// Set when the channel closes; the slot stays until the driver
    /// reaps it so a late submit is refused rather than resurrecting
    /// a dead channel.
    closed: std::sync::atomic::AtomicBool,
}

/// The mesh-side handle on the RTC transport.
#[derive(Debug)]
pub struct RtcTransport {
    slots: DashMap<u32, PeerSlot>,
    next_slot: AtomicU32,
    stats: Arc<RtcStats>,
    send_queue_packets: usize,
    send_queue_bytes: usize,
    buffered_amount_advisory: usize,
}

impl RtcTransport {
    /// Build the admission side from the operator's config.
    pub fn new(config: &RtcConfig, stats: Arc<RtcStats>) -> Self {
        Self {
            slots: DashMap::new(),
            next_slot: AtomicU32::new(0),
            stats,
            send_queue_packets: config.send_queue_packets,
            send_queue_bytes: config.send_queue_bytes,
            buffered_amount_advisory: config.buffered_amount_advisory,
        }
    }

    /// The shared counters.
    #[inline]
    pub fn stats(&self) -> &Arc<RtcStats> {
        &self.stats
    }

    /// Open a slot for a channel the driver has just brought up.
    ///
    /// `generation` starts at 0 for a fresh slot and is one past the
    /// closed incarnation when a slot is reused, so a handle captured
    /// before a close never addresses its successor.
    pub fn open_peer(&self) -> RtcPeerId {
        let slot = self.next_slot.fetch_add(1, Ordering::Relaxed);
        let generation = 0;
        self.slots.insert(
            slot,
            PeerSlot {
                generation,
                queue: Mutex::new((std::collections::VecDeque::new(), 0)),
                published_buffered: AtomicUsize::new(0),
                closed: std::sync::atomic::AtomicBool::new(false),
            },
        );
        RtcPeerId { slot, generation }
    }

    /// Admission. Synchronous, total, and the only refusal point.
    ///
    /// On `Ok(())` the packet is the driver's: it is retained across
    /// `Channel::write` returning `Ok(false)` and is only ever lost if
    /// the channel closes with it still queued, which is counted
    /// ([`RtcStats::discarded_at_close`]).
    pub fn submit(&self, packet: &[u8], to: RtcPeerId) -> Result<(), RtcSubmitError> {
        let Some(slot) = self.slots.get(&to.slot) else {
            self.stats.note_refused_unknown_peer();
            return Err(RtcSubmitError::UnknownPeer);
        };
        if slot.generation != to.generation || slot.closed.load(Ordering::Acquire) {
            self.stats.note_refused_unknown_peer();
            return Err(RtcSubmitError::UnknownPeer);
        }

        // The advisory first: it is the cheaper read, and refusing
        // early is exactly what it is for.
        if slot.published_buffered.load(Ordering::Relaxed) > self.buffered_amount_advisory {
            self.stats.note_refused_advisory();
            return Err(RtcSubmitError::AdvisoryOver);
        }

        let mut guard = slot.queue.lock();
        let (queue, bytes) = &mut *guard;
        if queue.len() >= self.send_queue_packets {
            drop(guard);
            self.stats.note_refused_slots();
            return Err(RtcSubmitError::QueueFull);
        }
        if *bytes + packet.len() > self.send_queue_bytes {
            drop(guard);
            self.stats.note_refused_bytes();
            return Err(RtcSubmitError::BytesFull);
        }
        queue.push_back(Bytes::copy_from_slice(packet));
        *bytes += packet.len();
        drop(guard);
        self.stats.note_accepted();
        Ok(())
    }

    /// Driver side: take the next packet for `slot`, if any.
    pub(super) fn pop(&self, slot: u32) -> Option<Bytes> {
        let entry = self.slots.get(&slot)?;
        let mut guard = entry.queue.lock();
        let (queue, bytes) = &mut *guard;
        let packet = queue.pop_front()?;
        *bytes = bytes.saturating_sub(packet.len());
        Some(packet)
    }

    /// Driver side: publish a fresh `buffered_amount` reading.
    pub(super) fn publish_buffered(&self, slot: u32, amount: usize) {
        if let Some(entry) = self.slots.get(&slot) {
            entry.published_buffered.store(amount, Ordering::Relaxed);
        }
        self.stats.observe_buffered(amount);
    }

    /// The reading admission is currently judging this peer against.
    pub fn published_buffered(&self, id: RtcPeerId) -> Option<usize> {
        let entry = self.slots.get(&id.slot)?;
        (entry.generation == id.generation)
            .then(|| entry.published_buffered.load(Ordering::Relaxed))
    }

    /// How many packets are waiting for the driver.
    pub fn queued_packets(&self, id: RtcPeerId) -> usize {
        self.slots
            .get(&id.slot)
            .filter(|e| e.generation == id.generation)
            .map(|e| e.queue.lock().0.len())
            .unwrap_or(0)
    }

    /// How many queued bytes are outstanding.
    pub fn queued_bytes(&self, id: RtcPeerId) -> usize {
        self.slots
            .get(&id.slot)
            .filter(|e| e.generation == id.generation)
            .map(|e| e.queue.lock().1)
            .unwrap_or(0)
    }

    /// Is this handle still addressable?
    pub fn is_open(&self, id: RtcPeerId) -> bool {
        self.slots
            .get(&id.slot)
            .is_some_and(|e| e.generation == id.generation && !e.closed.load(Ordering::Acquire))
    }

    /// Close a peer: refuse further submissions immediately, discard
    /// whatever is still queued, and count it.
    ///
    /// Returns the number of packets discarded — the only place an
    /// admitted packet is lost, which is why it is a return value and
    /// a counter rather than a log line.
    pub fn close_peer(&self, id: RtcPeerId, retained: usize) -> u64 {
        let Some(entry) = self.slots.get(&id.slot) else {
            return 0;
        };
        if entry.generation != id.generation {
            return 0;
        }
        entry.closed.store(true, Ordering::Release);
        let discarded = {
            let mut guard = entry.queue.lock();
            let (queue, bytes) = &mut *guard;
            let n = queue.len();
            queue.clear();
            *bytes = 0;
            n
        };
        drop(entry);
        // The slot object stays with `closed` set until reuse so a
        // late submit is refused; reuse bumps the generation, which is
        // what makes an id unrepeatable.
        let discarded = discarded as u64 + retained as u64;
        if discarded > 0 {
            self.stats.note_discarded_at_close_n(discarded);
        }
        discarded
    }

    /// Reuse a closed slot for a new channel, at the next generation.
    pub fn reopen_peer(&self, slot: u32) -> Option<RtcPeerId> {
        let mut entry = self.slots.get_mut(&slot)?;
        if !entry.closed.load(Ordering::Acquire) {
            return None;
        }
        entry.generation = entry.generation.wrapping_add(1);
        entry.closed.store(false, Ordering::Release);
        entry.published_buffered.store(0, Ordering::Relaxed);
        let generation = entry.generation;
        Some(RtcPeerId { slot, generation })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn transport(packets: usize, bytes: usize, advisory: usize) -> RtcTransport {
        let config = RtcConfig {
            send_queue_packets: packets,
            send_queue_bytes: bytes,
            buffered_amount_advisory: advisory,
            ..RtcConfig::new()
        };
        RtcTransport::new(&config, Arc::new(RtcStats::default()))
    }

    #[test]
    fn the_reserved_slot_bound_refuses_at_admission_and_queues_nothing() {
        let t = transport(2, 1 << 20, usize::MAX);
        let id = t.open_peer();
        assert!(t.submit(b"a", id).is_ok());
        assert!(t.submit(b"b", id).is_ok());
        assert_eq!(t.submit(b"c", id), Err(RtcSubmitError::QueueFull));
        assert_eq!(
            t.queued_packets(id),
            2,
            "a refusal must not enqueue: nothing is accepted then dropped"
        );
        assert_eq!(t.stats().accepted(), 2);
        assert_eq!(t.stats().admission_refused_slots(), 1);
    }

    #[test]
    fn the_reserved_byte_bound_is_exact_and_independent_of_the_slot_bound() {
        let t = transport(1024, 8, usize::MAX);
        let id = t.open_peer();
        assert!(t.submit(&[0u8; 8], id).is_ok());
        assert_eq!(t.submit(b"x", id), Err(RtcSubmitError::BytesFull));
        assert_eq!(t.queued_bytes(id), 8);
        assert_eq!(t.stats().admission_refused_bytes(), 1);
    }

    /// The advisory refuses *earlier* than the hard bound — and only
    /// the hard bound is a bound. This is the staleness property in
    /// miniature: the reading is whatever the driver last published.
    #[test]
    fn the_advisory_refuses_before_the_hard_bound_is_reached() {
        let t = transport(1024, 1 << 20, 1024);
        let id = t.open_peer();
        assert!(t.submit(b"fits", id).is_ok());
        t.publish_buffered(id.slot, 2048);
        assert_eq!(t.submit(b"nope", id), Err(RtcSubmitError::AdvisoryOver));
        assert_eq!(t.queued_packets(id), 1);

        // …and decays back to admitting once the driver publishes a
        // reading under the threshold.
        t.publish_buffered(id.slot, 0);
        assert!(t.submit(b"again", id).is_ok());
        assert_eq!(t.stats().admission_refused_advisory(), 1);
    }

    #[test]
    fn a_closed_peer_refuses_and_reports_what_it_discarded() {
        let t = transport(16, 1 << 20, usize::MAX);
        let id = t.open_peer();
        assert!(t.submit(b"one", id).is_ok());
        assert!(t.submit(b"two", id).is_ok());

        // One packet is with the driver, retained across an `Ok(false)`.
        let discarded = t.close_peer(id, 1);
        assert_eq!(discarded, 3, "two queued plus the retained one");
        assert_eq!(t.stats().discarded_at_close(), 3);
        assert_eq!(t.submit(b"late", id), Err(RtcSubmitError::UnknownPeer));
        assert!(!t.is_open(id));
    }

    #[test]
    fn a_reused_slot_gets_a_new_generation_and_the_old_handle_stays_dead() {
        let t = transport(16, 1 << 20, usize::MAX);
        let first = t.open_peer();
        t.close_peer(first, 0);
        let second = t.reopen_peer(first.slot).expect("slot reopens");

        assert_eq!(second.slot, first.slot);
        assert_ne!(
            second.generation, first.generation,
            "a spent id must never address the session that reuses its slot"
        );
        assert!(t.submit(b"new", second).is_ok());
        assert_eq!(
            t.submit(b"stale", first),
            Err(RtcSubmitError::UnknownPeer),
            "the pre-close handle is refused, not silently routed to the successor"
        );
    }

    #[test]
    fn popping_returns_packets_in_order_and_frees_the_byte_reservation() {
        let t = transport(16, 16, usize::MAX);
        let id = t.open_peer();
        assert!(t.submit(b"aaaa", id).is_ok());
        assert!(t.submit(b"bbbb", id).is_ok());
        assert_eq!(t.queued_bytes(id), 8);

        assert_eq!(t.pop(id.slot).as_deref(), Some(&b"aaaa"[..]));
        assert_eq!(
            t.queued_bytes(id),
            4,
            "the reservation must be released as the driver takes packets, or the \
             bound ratchets shut"
        );
        assert_eq!(t.pop(id.slot).as_deref(), Some(&b"bbbb"[..]));
        assert_eq!(t.pop(id.slot), None);
    }

    /// Concurrency: the reservation is taken under the queue lock, so
    /// N threads racing the last slots cannot overshoot the bound.
    #[test]
    fn concurrent_submits_never_exceed_the_reserved_slots() {
        let t = Arc::new(transport(64, 1 << 20, usize::MAX));
        let id = t.open_peer();
        let mut handles = Vec::new();
        for _ in 0..8 {
            let t = Arc::clone(&t);
            handles.push(std::thread::spawn(move || {
                for _ in 0..64 {
                    let _ = t.submit(b"x", id);
                }
            }));
        }
        for h in handles {
            h.join().expect("thread");
        }
        assert_eq!(t.queued_packets(id), 64);
        assert_eq!(t.stats().accepted(), 64);
        assert_eq!(t.stats().admission_refused_slots(), 8 * 64 - 64);
    }
}
