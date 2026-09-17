//! The RTC driver: one task, one socket, every `str0m::Rtc`.
//!
//! Lifted from S0b's loop, which ran ~1 000 sessions against a real
//! headless Chromium in both ICE roles. The invariants it validated,
//! restated as this module's rules:
//!
//! 1. **Nothing outside this task ever touches an `Rtc`.** There is no
//!    lock around one, because there is no second toucher.
//! 2. **Single mutation, then a complete drain.** Every
//!    `handle_input` / `sdp_api` mutation is followed by
//!    [`Session::drain`] to `Output::Timeout` before the next one.
//! 3. **One `Channel::write` per drain.** str0m's contract; it is also
//!    why RTC packets are never batched in the scheduler drain.
//! 4. **Retain and retry.** A packet `write` refuses with `Ok(false)`
//!    is kept by the driver and re-offered. S0b's first version
//!    dropped it: the queue then never backs up, admission never
//!    refuses, and the loss is invisible — one run silently lost
//!    19 476 packets while reporting one refusal.
//! 5. **The driver never blocks.** A blocked driver stalls every
//!    peer's `poll_output`, so a full ingress input drops and counts
//!    ([`RtcStats::ingress_dropped`]) rather than waiting.
//! 6. **`ConnectionReset` is swallowed and counted.** On Windows an
//!    ICMP port-unreachable from a peer that went away fails the
//!    *next* `recv_from` with `WSAECONNRESET`; treating it as fatal
//!    kills every session on the socket.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, oneshot};

use str0m::change::{SdpAnswer, SdpOffer, SdpPendingOffer};
use str0m::channel::{ChannelConfig, ChannelId, Reliability};
use str0m::net::{Protocol, Receive};
use str0m::{Candidate, Event, Input, Output, Rtc};

use super::config::RtcConfig;
use super::stats::RtcStats;
use super::stun;
use super::transport::RtcTransport;
use super::RtcPeerId;

/// Longest the driver parks in `recv_from` before servicing queues and
/// timeouts. S0b's value: short enough that the outbound pump stays
/// responsive, long enough to not spin.
const MAX_POLL_INTERVAL: Duration = Duration::from_millis(5);

/// Maximum datagram the RTC socket reads.
const RECV_BUF: usize = 2048;

/// Longest the **STUN-only** socket parks in `recv_from` before
/// re-checking the shutdown flag.
///
/// Two orders of magnitude slacker than [`MAX_POLL_INTERVAL`]
/// because it paces nothing: that socket has no queues to pump and
/// no session timers to fire, so this only bounds how long a
/// signal-only [`RtcDriverHandle::shutdown`] leaves the port bound.
/// A joining shutdown waits for the task's own exit, which is at
/// most one interval away.
const STUN_POLL_INTERVAL: Duration = Duration::from_millis(250);

/// Longest a joining shutdown waits for a task to exit on its own
/// before aborting it — and, for a caller that does not own the
/// join, longest it parks before re-checking whether the handle
/// came back to it.
///
/// The same number for both because it is the same fact: how long
/// this driver is prepared to wait for a socket to be released. It
/// is not a deadline on the release, which is recorded and
/// permanent; it bounds how long a caller blocks before escalating.
const TASK_JOIN_TIMEOUT: Duration = Duration::from_secs(2);

/// **Bounded service policy** (R6): the most `Channel::write`s one
/// peer may take in one outer turn of the driver loop.
///
/// The earlier claim — "one write per peer per iteration" — was not
/// what the code did: `pump_peer` looped until the peer's queue was
/// empty, its write refused, or its channel closed, so a
/// continuously-replenished producer could hold the phase and delay
/// every other peer's writes, the timer pass and the socket read.
/// str0m's mutate-then-drain ordering is preserved inside the
/// quantum: each write is still followed by a complete drain, and the
/// quantum only decides when the driver moves on.
const WRITE_QUANTUM_PER_TURN: usize = 8;

/// Same idea for signalling: a drain-until-empty loop over a channel
/// somebody else is filling is not a bound.
const SIGNAL_QUANTUM_PER_TURN: usize = 16;

/// One signalling instruction for the driver.
///
/// Stage 3 has no signalling subprotocol — `0x0D02` is Stage 4's — so
/// these are delivered in-process by the loopback harness. The shapes
/// are the ones a real signalling path would carry.
#[derive(Debug)]
pub enum RtcSignal {
    /// Create a session, add the DataChannel, and produce an offer.
    CreateOffer {
        /// Offer SDP and the handle the channel will open under.
        reply: oneshot::Sender<Result<(RtcPeerId, String), String>>,
    },
    /// Create a session from a remote offer and produce an answer.
    AcceptOffer {
        /// The remote offer's SDP.
        offer_sdp: String,
        /// Answer SDP and the handle the channel will open under.
        reply: oneshot::Sender<Result<(RtcPeerId, String), String>>,
    },
    /// Apply the answer to a pending offer.
    AcceptAnswer {
        /// Which session.
        peer: RtcPeerId,
        /// The remote answer's SDP.
        answer_sdp: String,
        /// Completion.
        reply: oneshot::Sender<Result<(), String>>,
    },
    /// Add a remote ICE candidate.
    RemoteCandidate {
        /// Which session.
        peer: RtcPeerId,
        /// Candidate in SDP form.
        candidate: String,
        /// Completion.
        reply: oneshot::Sender<Result<(), String>>,
    },
    /// Stage 4b: report the pair ICE actually selected.
    SelectedPair {
        /// Which session.
        peer: RtcPeerId,
        /// `(local, remote, how the remote address was learned)`.
        reply: oneshot::Sender<Option<(std::net::SocketAddr, std::net::SocketAddr, &'static str)>>,
    },
    /// Wait until the DataChannel is open (or the deadline passes).
    AwaitOpen {
        /// Which session.
        peer: RtcPeerId,
        /// Completion; `Err` on timeout or close.
        reply: oneshot::Sender<Result<(), String>>,
    },
    /// Close a session; the remainder of its queue is discarded and
    /// counted.
    Close {
        /// Which session.
        peer: RtcPeerId,
    },
}

/// Test-only fault injection on the driver.
///
/// Every hook here exists because the property it exercises cannot
/// be produced on demand from outside: a paused drain, a
/// `Channel::write` that refuses after a passing precheck, DataChannel
/// loss with `maxRetransmits: 0`, and an ICMP-induced
/// `ConnectionReset` are all environmental. They are
/// `cfg(any(test, feature = "fixtures"))`, so no production build can
/// reach them.
#[cfg(any(test, feature = "fixtures"))]
#[derive(Debug, Default)]
pub struct RtcTestHooks {
    /// Stop the outbound pump without closing anything: queued
    /// packets stay queued and the advisory reading goes stale,
    /// which is exactly the state the reserved-bytes bound has to
    /// hold on its own.
    pause_pump: AtomicBool,
    /// Treat every `Channel::write` as `Ok(false)` — a
    /// post-acceptance refusal after a passing precheck. Exercises
    /// retain-and-retry without needing a saturated peer.
    force_write_false: AtomicBool,
    /// Drop one in N inbound DataChannel messages (0 = no loss).
    /// With `maxRetransmits: 0` there is no SCTP recovery, so this
    /// is what makes `reliability.rs` the only recovery mechanism.
    ingress_drop_one_in: std::sync::atomic::AtomicU64,
    /// Counter for the loss injector.
    ingress_seen: std::sync::atomic::AtomicU64,
    /// Drop the Nth inbound DataChannel message, counting from the
    /// moment it is armed (0 = disabled).
    ///
    /// Deterministic where [`Self::ingress_drop_one_in`] is
    /// periodic, and per-PACKET where
    /// [`Self::raw_egress_drop_at`] is per-datagram. That last
    /// difference is the one that matters for a fragment group: one
    /// 8 KiB Net packet is carried by ~7 DTLS datagrams, so the raw
    /// egress injector cannot name a PACKET at all — arming it at 3
    /// loses a chunk of the first piece, not the third piece. This
    /// drops a whole `Event::ChannelData`, which is exactly one Net
    /// packet, so "the third piece" is a thing a witness can say.
    ingress_drop_at: std::sync::atomic::AtomicU64,
    /// Inbound messages seen since [`Self::ingress_drop_at`] was
    /// armed, so a receipt can state that the drop happened rather
    /// than that it was requested.
    ingress_seen_since_arm: std::sync::atomic::AtomicU64,
    /// Drop the inbound Net packet whose header carries a given
    /// `fragment_offset`, stored as **offset + 1** so that `0` is
    /// "disabled" and the struct keeps its derived `Default`. Offset
    /// 0 is a legitimate target — it is the group's head — so it
    /// cannot double as the sentinel.
    ///
    /// # Why this exists beside `ingress_drop_at`
    ///
    /// `ingress_drop_at` names an ARRIVAL ORDINAL, and an ordinal is
    /// only the piece a witness means if nothing else arrives in
    /// between. Credit grants, ACKs and the peer's own traffic share
    /// this channel, so "the third inbound message" is the third
    /// fragment on one machine and something else on another. That
    /// is not a flake to retry: the instrument was naming a position
    /// in a sequence it does not control.
    ///
    /// `a_lost_middle_fragment_is_retransmitted_and_the_payload_arrives_once`
    /// passed on Windows and failed on the Linux runner with "held 0
    /// bytes", which its own message reads as the HEAD having been
    /// lost — the ordinal had selected a different packet. This
    /// injector names the piece by its own identity instead, so the
    /// witness drops the middle fragment on every machine.
    ingress_drop_frag_offset: std::sync::atomic::AtomicU32,
    /// Fragments the offset-targeted injector actually dropped,
    /// counted since it was armed.
    ingress_frag_dropped: std::sync::atomic::AtomicU64,
    /// Drop the Nth RAW outbound datagram carrying DTLS
    /// application data — i.e. below SCTP (0 = disabled).
    ///
    /// # Why this exists beside `ingress_drop_one_in`
    ///
    /// The two injectors lose a packet at **different layers**, and
    /// only one of them can say anything about SCTP.
    ///
    /// `ingress_drop_one_in` discards an `Event::ChannelData` —
    /// SCTP has already delivered it, so the loss is above SCTP and
    /// is terminal no matter how the DataChannel was negotiated.
    /// That is what makes it the right tool for "`reliability.rs`
    /// is the only recovery mechanism".
    ///
    /// This one discards the datagram on the socket, before the
    /// peer's SCTP ever sees the chunk. A reliable channel
    /// retransmits it; a `{ordered: false, maxRetransmits: 0}`
    /// channel does not. It therefore DISTINGUISHES the two
    /// negotiations, which the other hook cannot — and that
    /// difference is the whole of the Stage 5 §11.8 mDNS finding:
    /// Noise `msg1`/`msg2` are `build_handshake` packets outside
    /// the reliable-stream machinery, so SCTP is their only
    /// recovery, and a harness that switched SCTP recovery off made
    /// a single lost datagram terminal.
    ///
    /// Deterministic, never probabilistic: it drops the **Nth**
    /// qualifying datagram counted from the moment it is armed, so
    /// a receipt taken with it is reproducible. Only DTLS
    /// `application_data` records (content type `0x17`) are
    /// counted; STUN checks and the DTLS handshake are left alone,
    /// because losing those exercises ICE and DTLS recovery rather
    /// than SCTP's.
    raw_egress_drop_at: std::sync::atomic::AtomicU64,
    /// Qualifying datagrams seen since [`Self::set_raw_egress_drop_at`].
    raw_egress_seen: std::sync::atomic::AtomicU64,
    /// Make the next socket read fail with `ConnectionReset`.
    inject_conn_reset: AtomicBool,
    /// Park the loop in a long sleep so a caller can exercise the
    /// **timeout** arm of [`RtcDriverHandle::shutdown_and_join`]
    /// against a task that will not cooperate (H1).
    stall_loop: AtomicBool,
    /// Wedge the driver loop in a **synchronous** sleep, in
    /// milliseconds. `0` disables it.
    ///
    /// Distinct from [`Self::stall_loop`], and the distinction is
    /// the whole point: that one parks the loop at an `await`, where
    /// `abort()` lands. This one blocks the worker thread, where it
    /// does not — which is what a task in a blocking syscall looks
    /// like, and the case that turned a bounded shutdown into an
    /// unbounded one.
    block_loop_ms: std::sync::atomic::AtomicU64,
}

#[cfg(any(test, feature = "fixtures"))]
impl RtcTestHooks {
    /// Pause or resume the outbound pump.
    pub fn set_pump_paused(&self, paused: bool) {
        self.pause_pump.store(paused, Ordering::Release);
    }

    /// Force every `Channel::write` to report `Ok(false)`.
    pub fn set_force_write_false(&self, forced: bool) {
        self.force_write_false.store(forced, Ordering::Release);
    }

    /// Drop one in `n` inbound DataChannel messages; `0` disables.
    pub fn set_ingress_drop_one_in(&self, n: u64) {
        self.ingress_drop_one_in.store(n, Ordering::Release);
    }

    /// Arm the deterministic ABOVE-SCTP injector: drop the `nth`
    /// inbound DataChannel message, counting from this call. `0`
    /// disables and resets the counter.
    ///
    /// One message is one Net packet, which is what makes this the
    /// instrument for "lose exactly the third piece of this
    /// fragment group". A drop here is terminal for SCTP — it has
    /// already delivered — so whatever arrives afterwards was
    /// recovered by `reliability.rs` and nothing else.
    pub fn set_ingress_drop_at(&self, nth: u64) {
        self.ingress_seen_since_arm.store(0, Ordering::Release);
        self.ingress_drop_at.store(nth, Ordering::Release);
    }

    /// Inbound messages counted since [`Self::set_ingress_drop_at`].
    pub fn ingress_counted(&self) -> u64 {
        self.ingress_seen_since_arm.load(Ordering::Acquire)
    }

    /// Arm the ingress injector on a fragment's OWN identity: drop
    /// the inbound Net packet whose header carries this
    /// `fragment_offset`. `None` disables it.
    ///
    /// **Fires exactly once, then disarms.** A retransmission of the
    /// lost piece carries the same offset, so an injector that
    /// stayed armed would eat the recovery the witness exists to
    /// observe — the payload would never arrive and the failure
    /// would look like a broken reassembler rather than a greedy
    /// instrument. This is not hypothetical: the first version of
    /// this injector did exactly that.
    ///
    /// Prefer this to [`Self::set_ingress_drop_at`] whenever the
    /// witness means "that piece" rather than "that arrival". An
    /// ordinal is only the intended piece if nothing else shares the
    /// channel, and credit grants, ACKs and the peer's own traffic
    /// do share it — the ordinal form selected the group's head on a
    /// Linux runner and its middle piece on Windows for the very
    /// witness this was added for.
    pub fn set_ingress_drop_fragment_offset(&self, offset: Option<u16>) {
        self.ingress_frag_dropped.store(0, Ordering::Release);
        self.ingress_drop_frag_offset.store(
            offset.map_or(0, |o| u32::from(o).saturating_add(1)),
            Ordering::Release,
        );
    }

    /// Fragments dropped by
    /// [`Self::set_ingress_drop_fragment_offset`] since it was armed,
    /// so a witness can assert the loss HAPPENED rather than that it
    /// was requested — the difference between an injector that fired
    /// and one that silently matched nothing.
    pub fn ingress_fragment_dropped(&self) -> u64 {
        self.ingress_frag_dropped.load(Ordering::Acquire)
    }

    /// Arm the PRE-SCTP injector: drop the `nth` outbound datagram
    /// that carries DTLS application data, counting from this call.
    /// `0` disables and resets the counter.
    ///
    /// A different instrument from
    /// [`Self::set_ingress_drop_one_in`]: this one loses the
    /// datagram below SCTP, so a reliable DataChannel recovers it
    /// and an unreliable one does not. That difference is the whole
    /// point — a drop above SCTP is terminal either way and cannot
    /// distinguish the two channel configurations.
    pub fn set_raw_egress_drop_at(&self, nth: u64) {
        self.raw_egress_seen.store(0, Ordering::Release);
        self.raw_egress_drop_at.store(nth, Ordering::Release);
    }

    /// How many qualifying datagrams the injector has counted since
    /// it was armed — so a receipt can state that the drop really
    /// happened rather than that it was merely requested.
    pub fn raw_egress_counted(&self) -> u64 {
        self.raw_egress_seen.load(Ordering::Acquire)
    }

    /// Make the driver's next socket read surface a
    /// `ConnectionReset`.
    pub fn inject_conn_reset(&self) {
        self.inject_conn_reset.store(true, Ordering::Release);
    }

    /// Park the driver loop, so shutdown must abort it.
    pub fn set_stall_loop(&self, stalled: bool) {
        self.stall_loop.store(stalled, Ordering::Release);
    }

    fn loop_stalled(&self) -> bool {
        self.stall_loop.load(Ordering::Acquire)
    }

    /// Wedge the driver loop in a synchronous sleep of `ms`, where
    /// `abort()` cannot reach it. `0` disables.
    pub fn set_block_loop_ms(&self, ms: u64) {
        self.block_loop_ms.store(ms, Ordering::Release);
    }

    fn loop_blocked_for(&self) -> Option<Duration> {
        let ms = self.block_loop_ms.load(Ordering::Acquire);
        (ms > 0).then(|| Duration::from_millis(ms))
    }

    fn pump_paused(&self) -> bool {
        self.pause_pump.load(Ordering::Acquire)
    }

    fn write_forced_false(&self) -> bool {
        self.force_write_false.load(Ordering::Acquire)
    }

    fn take_conn_reset(&self) -> bool {
        self.inject_conn_reset.swap(false, Ordering::AcqRel)
    }

    fn drop_this_ingress(&self, data: &[u8]) -> bool {
        // The injectors keep independent counters, so arming one
        // does not perturb another's schedule, and an unarmed
        // injector counts nothing.
        let mut drop = false;
        let nth = self.ingress_drop_at.load(Ordering::Acquire);
        if nth != 0 {
            let seen = self.ingress_seen_since_arm.fetch_add(1, Ordering::Relaxed) + 1;
            drop = seen == nth;
        }
        // Targeted by the piece's own identity, and it fires ONCE.
        // The header is plaintext — it is the AAD the payload is
        // sealed against — so this reads the offset without touching
        // the ciphertext, and a datagram that is not a parseable Net
        // packet simply does not match.
        //
        // Disarming on the first match is the whole point: a
        // retransmission of the lost piece carries the SAME
        // `fragment_offset`, so an injector that stayed armed would
        // eat the recovery it exists to observe and the group would
        // never complete. `compare_exchange` rather than a store, so
        // two pieces arriving on different driver tasks cannot both
        // see themselves as the first.
        let armed = self.ingress_drop_frag_offset.load(Ordering::Acquire);
        if armed != 0 {
            let want = armed - 1;
            if let Some(header) = net_wire::protocol::NetHeader::from_bytes(data) {
                if header.frag_flags & net_wire::protocol::FRAG_FRAGMENTED != 0
                    && u32::from(header.fragment_offset) == want
                    && self
                        .ingress_drop_frag_offset
                        .compare_exchange(armed, 0, Ordering::AcqRel, Ordering::Acquire)
                        .is_ok()
                {
                    self.ingress_frag_dropped.fetch_add(1, Ordering::Relaxed);
                    drop = true;
                }
            }
        }
        let n = self.ingress_drop_one_in.load(Ordering::Acquire);
        if n != 0 {
            let seen = self.ingress_seen.fetch_add(1, Ordering::Relaxed) + 1;
            drop = drop || seen.is_multiple_of(n);
        }
        drop
    }

    /// Whether this raw outbound datagram is the armed one.
    ///
    /// Only DTLS `application_data` records (content type `0x17`)
    /// are counted: a STUN check or a DTLS handshake record lost
    /// here would exercise ICE or DTLS recovery, which is a
    /// different claim.
    fn drop_this_raw_egress(&self, datagram: &[u8]) -> bool {
        let nth = self.raw_egress_drop_at.load(Ordering::Acquire);
        if nth == 0 || datagram.first() != Some(&0x17) {
            return false;
        }
        let seen = self.raw_egress_seen.fetch_add(1, Ordering::Relaxed) + 1;
        seen == nth
    }
}

/// A pausable seam between "the RTC Noise exchange completed" and
/// "the install commits" (H2).
///
/// The install-race witnesses need that gap to be an observable
/// point, not a timing accident: arm the pause, start the exchange,
/// wait until the installer has *reached* it, interfere (close the
/// endpoint, install a competing incarnation, open a stream on the
/// incumbent), then release. Unarmed — which is every production
/// build path — it is one relaxed load.
#[cfg(any(test, feature = "fixtures"))]
#[derive(Debug, Default)]
pub struct RtcInstallPause {
    /// How many more arriving installs to park. A bound rather than
    /// a flag because a witness usually parks *one* attempt and
    /// then needs a second, competing attempt to run to completion.
    park_budget: std::sync::atomic::AtomicU32,
    reached: tokio::sync::Notify,
    release: tokio::sync::Notify,

    arrivals: std::sync::atomic::AtomicU32,
}

#[cfg(any(test, feature = "fixtures"))]
impl RtcInstallPause {
    /// Park the **next** install that reaches the seam; later ones
    /// pass straight through.
    pub fn arm_once(&self) {
        self.park_budget.store(1, Ordering::Release);
    }

    /// Let parked installs through and stop parking new ones.
    pub fn release(&self) {
        self.park_budget.store(0, Ordering::Release);
        self.release.notify_waiters();
    }

    /// How many installs have reached the seam.
    pub fn arrivals(&self) -> u32 {
        self.arrivals.load(Ordering::Acquire)
    }

    /// Wait until an install is parked at the seam.
    pub async fn wait_until_reached(&self) {
        loop {
            let notified = self.reached.notified();
            if self.arrivals() > 0 {
                return;
            }
            notified.await;
        }
    }

    pub(crate) async fn wait_if_armed(&self) {
        let taken = self
            .park_budget
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n > 0).then(|| n - 1)
            })
            .is_ok();
        if !taken {
            return;
        }
        let released = self.release.notified();
        self.arrivals.fetch_add(1, Ordering::AcqRel);
        self.reached.notify_waiters();
        released.await;
    }
}

/// Mesh-side handle on the driver.
#[derive(Debug, Clone)]
pub struct RtcDriverHandle {
    signals: mpsc::Sender<RtcSignal>,
    transport: Arc<RtcTransport>,
    stats: Arc<RtcStats>,
    local_addr: SocketAddr,
    shutdown: Arc<AtomicBool>,
    /// The driver task and its **release**, so shutdown can join it.
    /// Without this the task (and its bound UDP socket) outlived the
    /// node that created it for as long as the runtime ran — a
    /// successor could not rebind an explicit RTC port, and a
    /// shut-down node with `serve_stun` kept answering (R3-B).
    release: TaskRelease,
    /// The STUN-only endpoint, when `RtcConfig::stun_addr`
    /// configured one: the address to announce and the task
    /// answering on it.
    stun: Option<StunEndpoint>,
    #[cfg(any(test, feature = "fixtures"))]
    hooks: Arc<RtcTestHooks>,
}

impl RtcDriverHandle {
    /// Test-only fault injection.
    #[cfg(any(test, feature = "fixtures"))]
    #[inline]
    pub fn hooks(&self) -> &Arc<RtcTestHooks> {
        &self.hooks
    }

    /// The admission side.
    #[inline]
    pub fn transport(&self) -> &Arc<RtcTransport> {
        &self.transport
    }

    /// The shared counters.
    #[inline]
    pub fn stats(&self) -> &Arc<RtcStats> {
        &self.stats
    }

    /// The address the RTC socket is bound to (§6: its own socket).
    #[inline]
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// The address the **STUN-only** socket is bound to, when
    /// [`RtcConfig::stun_addr`] configured one.
    ///
    /// Resolved post-bind, so a `:0` bind reports a real port and
    /// the announcement never has to guess an adjacent one. `None`
    /// when no second socket was configured, which is what "off
    /// unless configured" means at the emission point.
    ///
    /// Always a different socket from [`Self::local_addr`], which is
    /// the point: a leaf pointing `iceServers` at its peer's RTC
    /// endpoint has libwebrtc eat that peer's connectivity checks.
    /// The operator's `stun_public_addr` override is applied by the
    /// announcer; this is the bind's own truth.
    #[inline]
    pub fn stun_local_addr(&self) -> Option<SocketAddr> {
        self.stun.as_ref().map(|stun| stun.local_addr)
    }

    /// Send a signalling instruction to the driver.
    pub async fn signal(&self, signal: RtcSignal) -> Result<(), String> {
        self.signals
            .send(signal)
            .await
            .map_err(|_| "rtc driver is gone".to_string())
    }

    /// Create an offer and the session behind it.
    pub async fn create_offer(&self) -> Result<(RtcPeerId, String), String> {
        let (tx, rx) = oneshot::channel();
        self.signal(RtcSignal::CreateOffer { reply: tx }).await?;
        rx.await
            .map_err(|_| "driver dropped the reply".to_string())?
    }

    /// Accept a remote offer, producing an answer.
    pub async fn accept_offer(&self, offer_sdp: String) -> Result<(RtcPeerId, String), String> {
        let (tx, rx) = oneshot::channel();
        self.signal(RtcSignal::AcceptOffer {
            offer_sdp,
            reply: tx,
        })
        .await?;
        rx.await
            .map_err(|_| "driver dropped the reply".to_string())?
    }

    /// Apply an answer to a pending offer.
    pub async fn accept_answer(&self, peer: RtcPeerId, answer_sdp: String) -> Result<(), String> {
        let (tx, rx) = oneshot::channel();
        self.signal(RtcSignal::AcceptAnswer {
            peer,
            answer_sdp,
            reply: tx,
        })
        .await?;
        rx.await
            .map_err(|_| "driver dropped the reply".to_string())?
    }

    /// Add a remote ICE candidate.
    pub async fn remote_candidate(&self, peer: RtcPeerId, candidate: String) -> Result<(), String> {
        let (tx, rx) = oneshot::channel();
        self.signal(RtcSignal::RemoteCandidate {
            peer,
            candidate,
            reply: tx,
        })
        .await?;
        rx.await
            .map_err(|_| "driver dropped the reply".to_string())?
    }

    /// Wait for the DataChannel to open.
    pub async fn await_open(&self, peer: RtcPeerId) -> Result<(), String> {
        let (tx, rx) = oneshot::channel();
        self.signal(RtcSignal::AwaitOpen { peer, reply: tx })
            .await?;
        rx.await
            .map_err(|_| "driver dropped the reply".to_string())?
    }

    /// **Stage 4b (§6).** The candidate pair ICE selected for
    /// `peer`, as `(local, remote, learned)`, once traffic is
    /// flowing on it. `learned` is `"signalled"` when the remote
    /// address arrived as a candidate over signalling, and
    /// `"peer-reflexive"` when it did not — the address was learned
    /// from the peer's own inbound binding request, which is the
    /// mechanism §6 names for a browser that hides its host IPs
    /// behind `<uuid>.local`.
    ///
    /// Reported from what the ICE stack is transmitting to: str0m
    /// 0.23.1 exposes no nominated-pair accessor and emits no event
    /// for one, so "the address it sends to" is the observable that
    /// exists. `None` before anything has been sent.
    pub async fn selected_pair(
        &self,
        peer: RtcPeerId,
    ) -> Option<(std::net::SocketAddr, std::net::SocketAddr, &'static str)> {
        let (tx, rx) = oneshot::channel();
        self.signal(RtcSignal::SelectedPair { peer, reply: tx })
            .await
            .ok()?;
        rx.await.ok().flatten()
    }

    /// Close a session.
    pub async fn close(&self, peer: RtcPeerId) -> Result<(), String> {
        self.signal(RtcSignal::Close { peer }).await
    }

    /// Ask the driver to stop after its current iteration.
    ///
    /// Signal-only: use [`Self::shutdown_and_join`] when the caller
    /// needs the socket to be released by the time it returns.
    pub fn shutdown(&self) {
        self.shutdown.store(true, Ordering::Release);
    }

    /// Stop the driver from a destructor: signal, then abort the
    /// task.
    ///
    /// `Drop` cannot await, and detaching the task — which is what
    /// dropping the handle used to do — left it bound (R3-B). The
    /// abort itself no longer skips cleanup: the task's teardown
    /// guard runs on cancellation, closing every slot, counting what
    /// was queued and making the transport terminal (H1), so a
    /// retained handle cannot submit into a dead driver.
    pub fn shutdown_detached(&self) {
        self.shutdown.store(true, Ordering::Release);
        self.release.abort();
        if let Some(stun) = &self.stun {
            stun.release.abort();
        }
    }

    /// Stop the driver and wait until the task is **gone**, so the
    /// RTC socket is closed and the transport is terminal when this
    /// returns.
    ///
    /// Bounded: a driver wedged in a syscall is aborted rather than
    /// hanging the caller's shutdown — and then awaited, because
    /// `abort()` requests cancellation, it does not perform it (H1).
    /// Concurrent callers all wait for the same completion, and a
    /// caller cancelled mid-join returns the handle rather than
    /// detaching the task.
    pub async fn shutdown_and_join(&self) {
        self.shutdown.store(true, Ordering::Release);
        self.release.join().await;
        // The STUN-only socket is a second binding under the same
        // R3-B obligation: a shut-down anchor must not keep
        // answering, and a successor must be able to rebind an
        // explicit STUN port.
        if let Some(stun) = &self.stun {
            stun.release.join().await;
        }
    }
}

/// A spawned task whose **release is a recorded fact**, not the
/// return of whichever caller happened to own its join handle.
///
/// Two tasks hold a bound UDP socket — the driver loop and the
/// STUN-only loop — and both are under the same R3-B obligation: by
/// the time a joining shutdown returns, the port must be free. The
/// mechanism is one because the obligation is one; the two owners
/// stay separately observable through [`RtcDriverHandle`] and
/// [`StunEndpoint`].
///
/// # Why completion is published, and published DURABLY
///
/// Ownership of the join is not completion of it: a second caller
/// that finds the handle taken must wait for the **task**, not for
/// the first caller's return. So the task's own teardown guard
/// records release, and every caller reads that record.
///
/// It is recorded with `send_replace`, which stores the value
/// whether or not anyone is subscribed. `send` does not: it returns
/// `Err` and **leaves the value untouched** when there are no live
/// receivers, and this driver retains none — the spawn-time receiver
/// is dropped immediately. A completion published while nobody was
/// listening was therefore lost, and a subscription taken *after*
/// the socket and the task were gone read `false` and waited for a
/// change that could never come again.
#[derive(Debug, Clone)]
struct TaskRelease {
    task: Arc<parking_lot::Mutex<Option<tokio::task::JoinHandle<()>>>>,
    done: tokio::sync::watch::Sender<bool>,
    /// What the task is, for the escalation log line.
    what: &'static str,
}

impl TaskRelease {
    /// Take ownership of a spawned task and the release channel its
    /// own guard publishes on.
    fn new(
        task: tokio::task::JoinHandle<()>,
        done: tokio::sync::watch::Sender<bool>,
        what: &'static str,
    ) -> Self {
        Self {
            task: Arc::new(parking_lot::Mutex::new(Some(task))),
            done,
            what,
        }
    }

    /// Signal-free abort, for a destructor.
    ///
    /// **The handle is left where it is.** Aborting through a
    /// borrow rather than taking it keeps a later [`Self::join`]
    /// able to own the join and record release — which matters most
    /// for a task aborted *before its first poll*, because such a
    /// task never runs its guard and so never publishes anything
    /// itself. Taking the handle here made that release
    /// unobservable for ever.
    fn abort(&self) {
        if let Some(task) = self.task.lock().as_ref() {
            task.abort();
        }
    }

    /// Wait until the task is gone and its socket released.
    ///
    /// Bounded: a task wedged in a syscall is aborted rather than
    /// hanging the caller's shutdown — and then awaited, because
    /// `abort()` requests cancellation, it does not perform it
    /// (H1). Concurrent callers all reach the same recorded
    /// release, and a caller cancelled mid-join returns the handle
    /// rather than detaching the task.
    async fn join(&self) {
        loop {
            // The recorded fact, first: repeated and late joins are
            // the ordinary case once a node has shut down, and
            // there is nothing left to own by then.
            if *self.done.borrow() {
                return;
            }
            let taken = self.task.lock().take();
            let Some(handle) = taken else {
                // Another caller owns the join. Wait for the task's
                // own release rather than for that caller's return
                // — and re-check, because a joiner cancelled
                // mid-await puts the handle back (`JoinSlot`) and
                // would otherwise leave nobody to escalate.
                let mut rx = self.done.subscribe();
                if *rx.borrow_and_update() {
                    return;
                }
                if tokio::time::timeout(TASK_JOIN_TIMEOUT, rx.changed())
                    .await
                    .is_err()
                {
                    continue;
                }
                continue;
            };
            // Cancel-safety: if this future is dropped mid-await the
            // handle goes back where it came from, so a later
            // `abort` can still cancel the task and a later `join`
            // can still own it.
            let mut slot = JoinSlot {
                home: Arc::clone(&self.task),
                handle: Some(handle),
            };
            // `slot.handle` is `Some` here by construction — it is
            // set immediately above and taken only after this block.
            if let Some(handle) = slot.handle.as_mut() {
                if tokio::time::timeout(TASK_JOIN_TIMEOUT, &mut *handle)
                    .await
                    .is_err()
                {
                    tracing::debug!(task = self.what, "rtc task did not exit in time; aborting");
                    handle.abort();
                    // The join the abort is not: without waiting
                    // here the method returned while cancellation —
                    // and the socket's release — was still pending.
                    //
                    // **Bounded, because `abort()` is cooperative.**
                    // It takes effect at an await point, so a task
                    // wedged in a SYNCHRONOUS call — a blocking
                    // syscall, a `std::sync` wait, a test's blocking
                    // hook — never reaches one and the cancellation
                    // never lands. An unbounded wait here therefore
                    // did not protect the release; it converted "a
                    // task that cannot be cancelled" into "shutdown
                    // never returns", in production as well as under
                    // test. The wait is what the release record is
                    // for; when it cannot be had, saying so is
                    // strictly better than hanging the caller.
                }
            }
            // Joined: the task is gone, so drop the handle rather
            // than returning it, and record the release. The guard
            // records it too; a task aborted before its first poll
            // never ran one, which is why this is not a mere
            // backstop. Idempotent either way.
            let _ = slot.handle.take();
            self.done.send_replace(true);
            return;
        }
    }
}

/// Returns a taken [`tokio::task::JoinHandle`] to its home if the
/// joining future is cancelled (H1).
///
/// Without it a cancelled joiner detached the task: later shutdowns
/// found `None`, and `shutdown_detached` had nothing left to abort.
struct JoinSlot {
    home: Arc<parking_lot::Mutex<Option<tokio::task::JoinHandle<()>>>>,
    handle: Option<tokio::task::JoinHandle<()>>,
}

impl Drop for JoinSlot {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            *self.home.lock() = Some(handle);
        }
    }
}

/// The STUN-only endpoint: the address its socket bound, and the
/// task that answers binding requests on it.
///
/// Its own task rather than a second arm of the driver loop. That
/// loop's read is also its pacing — one bounded `recv_from` between
/// signalling, pumping, timers and reaping — so folding a second
/// socket into it would change *when* the RTC socket is read. The
/// diagnostic `UdpBlocked` probe targets that socket, and its
/// behaviour has to stay byte-identical.
#[derive(Debug, Clone)]
struct StunEndpoint {
    /// Post-bind, so a `:0` configuration yields a real port.
    local_addr: SocketAddr,
    /// The answering task and its recorded release — the same
    /// mechanism the driver task uses, for the same reason: a
    /// caller that finds the join taken must wait for the SOCKET to
    /// be free, not for another caller to return.
    release: TaskRelease,
}

/// One driver-owned session.
struct Session {
    rtc: Rtc,
    id: RtcPeerId,
    cid: Option<ChannelId>,
    pending: Option<SdpPendingOffer>,
    /// A packet `Channel::write` refused; retained, never dropped.
    retry: Option<Bytes>,
    timeout: Instant,
    open: bool,
    closed: bool,
    /// Deadline for reaching an open DataChannel.
    open_by: Instant,
    /// Waiters parked on [`RtcSignal::AwaitOpen`].
    open_waiters: Vec<oneshot::Sender<Result<(), String>>>,
    /// **Stage 4b (§6, the mDNS question).** The destination str0m
    /// is currently transmitting to — after nomination, that is the
    /// remote half of the selected candidate pair. str0m 0.23.1
    /// exposes no nominated-pair accessor and emits no event for
    /// it, so this is the observable that exists: what the ICE
    /// stack actually sends to.
    last_transmit: Option<std::net::SocketAddr>,
    /// Remote candidate addresses this session was *told* about
    /// through signalling. An address in `last_transmit` that is
    /// NOT in here was learned from the peer's own inbound binding
    /// request — i.e. peer-reflexive, which is precisely the answer
    /// §6 asks for when a browser hides its host IPs behind
    /// `<uuid>.local`.
    signalled_remotes: Vec<std::net::SocketAddr>,
}

/// The driver task.
pub struct RtcDriver;

impl RtcDriver {
    /// Bind the RTC socket and spawn the driver.
    ///
    /// The socket is the node's second: §6 rules out demultiplexing
    /// RTC traffic on the Net socket, so there is no discriminator to
    /// get wrong and no way for RTC traffic to reach the Net ingress.
    ///
    /// A **third, STUN-only** socket is bound when
    /// [`RtcConfig::stun_addr`] names one, so this anchor can
    /// announce a STUN endpoint that is not its own ICE address. It
    /// is additional: the RTC socket's behaviour, including the
    /// `serve_stun` responder the diagnostic `UdpBlocked` probe
    /// targets, is unchanged.
    ///
    /// Refuses **before binding anything** when the configuration
    /// announces a STUN endpoint no socket will serve or puts one
    /// UDP endpoint in both roles ([`RtcConfig::validate`]), and
    /// again **after binding** against the pair this anchor will
    /// actually advertise ([`RtcConfig::resolved_endpoint_conflict`])
    /// — which is the only check a `:0` bind can be held to. An
    /// anchor must not come up announcing a pairing that cannot
    /// work, because the peer's only symptom is an ICE timeout with
    /// no diagnostic.
    pub async fn spawn(
        config: RtcConfig,
        net_bind_addr: SocketAddr,
        stats: Arc<RtcStats>,
        ingress: mpsc::Sender<(Bytes, RtcPeerId)>,
        // R3-E: every channel the driver closes is announced here, so
        // the mesh can run its ordinary peer-removal transaction.
        // Without it a reaped channel left the peer, its addresses and
        // its routes installed until some unrelated failure detector
        // noticed.
        closed: mpsc::Sender<RtcPeerId>,
    ) -> std::io::Result<RtcDriverHandle> {
        if let Some(conflict) = config.validate() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                conflict,
            ));
        }
        let socket = UdpSocket::bind(config.resolved_bind_addr(net_bind_addr)).await?;
        let local_addr = socket.local_addr()?;
        let advertised = config.advertised_rtc_addr(local_addr);

        let transport = Arc::new(RtcTransport::new(&config, Arc::clone(&stats)));
        let (signal_tx, signal_rx) = mpsc::channel(64);
        let shutdown = Arc::new(AtomicBool::new(false));
        #[cfg(any(test, feature = "fixtures"))]
        let hooks = Arc::new(RtcTestHooks::default());

        // A configured STUN bind that cannot be taken is fatal
        // here: the alternative is an anchor that announces an
        // endpoint it is not listening on.
        //
        // Bound, then CHECKED, then served: the resolved pair is
        // known only once both sockets exist, and a refusal must
        // leave nothing spawned. Both sockets are dropped by the
        // early return below.
        let stun_bound = match config.stun_addr {
            Some(bind) => {
                let stun_socket = UdpSocket::bind(bind).await?;
                let bound = stun_socket.local_addr()?;
                Some((stun_socket, bound))
            }
            None => None,
        };
        if let Some(conflict) = config
            .resolved_endpoint_conflict(local_addr, stun_bound.as_ref().map(|(_, bound)| *bound))
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                conflict,
            ));
        }
        let stun = stun_bound.map(|(stun_socket, bound)| {
            // No receiver is retained on purpose: release is a
            // RECORDED value, not a notification that needs a
            // listener — see `TaskRelease`.
            let (stun_done, _) = tokio::sync::watch::channel(false);
            let stun_task = tokio::spawn(stun_only_loop(
                stun_socket,
                Arc::clone(&shutdown),
                stun_done.clone(),
            ));
            StunEndpoint {
                // The bind's own truth. `stun_public_addr` is the
                // announcer's override, applied where the
                // announcement is built — one resolution rule, in
                // one place.
                local_addr: bound,
                release: TaskRelease::new(stun_task, stun_done, "rtc stun socket"),
            }
        });

        let (done_tx, _) = tokio::sync::watch::channel(false);
        let task = tokio::spawn(driver_loop(
            done_tx.clone(),
            config,
            socket,
            advertised,
            Arc::clone(&transport),
            Arc::clone(&stats),
            ingress,
            closed,
            signal_rx,
            Arc::clone(&shutdown),
            #[cfg(any(test, feature = "fixtures"))]
            Arc::clone(&hooks),
        ));

        Ok(RtcDriverHandle {
            signals: signal_tx,
            transport: Arc::clone(&transport),
            stats: Arc::clone(&stats),
            local_addr,
            shutdown: Arc::clone(&shutdown),
            release: TaskRelease::new(task, done_tx, "rtc driver"),
            stun,
            #[cfg(any(test, feature = "fixtures"))]
            hooks,
        })
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "the driver owns every piece of RTC state by design; bundling them into a struct would only move the argument list"
)]
async fn driver_loop(
    done: tokio::sync::watch::Sender<bool>,
    config: RtcConfig,
    socket: UdpSocket,
    advertised: SocketAddr,
    transport: Arc<RtcTransport>,
    stats: Arc<RtcStats>,
    ingress: mpsc::Sender<(Bytes, RtcPeerId)>,
    closed: mpsc::Sender<RtcPeerId>,
    mut signals: mpsc::Receiver<RtcSignal>,
    shutdown: Arc<AtomicBool>,
    #[cfg(any(test, feature = "fixtures"))] hooks: Arc<RtcTestHooks>,
) {
    // H1: the session table lives inside a guard whose `Drop` owns
    // teardown, so the **same** cleanup runs whether the loop exits
    // cooperatively or the task is aborted. Previously the cleanup
    // was the loop's tail: an abort — which is what `Drop` on the
    // node does — released the socket while leaving slots open,
    // queued packets uncounted and `submit` returning `Ok(())`.
    let mut table = SessionTable {
        sessions: HashMap::new(),
        socket: Some(socket),
        transport: Arc::clone(&transport),
        stats: Arc::clone(&stats),
        closed: closed.clone(),
        done,
    };
    let sessions = &mut table.sessions;
    // Taken only by teardown, which runs after this borrow ends.
    let Some(socket) = table.socket.as_ref() else {
        return;
    };
    let mut buf = vec![0u8; RECV_BUF];

    while !shutdown.load(Ordering::Acquire) {
        // H1 witness support: a driver that will not notice the
        // shutdown flag, so `shutdown_and_join` has to take its
        // timeout/abort arm.
        #[cfg(any(test, feature = "fixtures"))]
        if hooks.loop_stalled() {
            tokio::time::sleep(Duration::from_secs(60)).await;
        }
        // The same support for the case `abort()` cannot reach: a
        // worker thread blocked outside the async machinery, which
        // is what a task in a blocking syscall is.
        #[cfg(any(test, feature = "fixtures"))]
        if let Some(blocked) = hooks.loop_blocked_for() {
            std::thread::sleep(blocked);
        }
        // --- 1. signalling: each is one mutation plus a full drain ---
        //
        // A *disconnected* channel is terminal (R3-B): every sender is
        // gone, so no further instruction can arrive and the loop has
        // nothing left to serve. Treating it as "momentarily empty",
        // as this did, is what kept an orphaned driver — and its
        // socket — alive for the runtime's lifetime.
        let mut signals_served = 0usize;
        while signals_served < SIGNAL_QUANTUM_PER_TURN {
            signals_served += 1;
            let signal = match signals.try_recv() {
                Ok(signal) => signal,
                Err(mpsc::error::TryRecvError::Empty) => break,
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    shutdown.store(true, Ordering::Release);
                    break;
                }
            };
            handle_signal(
                &config,
                sessions,
                advertised,
                socket,
                &transport,
                &stats,
                &ingress,
                #[cfg(any(test, feature = "fixtures"))]
                &hooks,
                &closed,
                signal,
            )
            .await;
        }
        if shutdown.load(Ordering::Acquire) {
            break;
        }

        // --- 2. outbound pump: one write per drain, retain on refusal -
        let slots: Vec<u32> = sessions.keys().copied().collect();
        #[cfg(any(test, feature = "fixtures"))]
        let pump_paused = hooks.pump_paused();
        #[cfg(not(any(test, feature = "fixtures")))]
        let pump_paused = false;
        if !pump_paused {
            for slot in &slots {
                pump_peer(
                    *slot,
                    sessions,
                    socket,
                    &transport,
                    &stats,
                    &ingress,
                    #[cfg(any(test, feature = "fixtures"))]
                    &hooks,
                )
                .await;
            }
        }

        // --- 3. advisory refresh --------------------------------------
        //
        // For every peer whose queue is non-empty OR whose last
        // published reading is above zero, so an idle peer's stale
        // high reading decays to the truth instead of refusing
        // forever. (S0b §4b: the reading is only refreshed while the
        // driver is writing, which is exactly what made it advisory.)
        for slot in &slots {
            let Some(session) = sessions.get_mut(slot) else {
                continue;
            };
            let Some(cid) = session.cid else { continue };
            let stale_high = transport
                .published_buffered(session.id)
                .is_some_and(|b| b > 0);
            let queued = transport.queued_packets(session.id) > 0;
            if !(stale_high || queued) {
                continue;
            }
            if let Some(mut channel) = session.rtc.channel(cid) {
                let amount = channel.buffered_amount();
                transport.publish_buffered(session.id.slot, amount);
            }
        }

        // --- 4. timeouts ----------------------------------------------
        let now = Instant::now();
        for slot in &slots {
            let due = sessions.get(slot).is_some_and(|s| s.timeout <= now);
            if due {
                if let Some(session) = sessions.get_mut(slot) {
                    if session.rtc.handle_input(Input::Timeout(now)).is_err() {
                        session.closed = true;
                    }
                    drain_session(
                        session,
                        socket,
                        &transport,
                        &stats,
                        &ingress,
                        #[cfg(any(test, feature = "fixtures"))]
                        &hooks,
                    )
                    .await;
                }
            }
            // ICE/DataChannel establishment deadline.
            if let Some(session) = sessions.get_mut(slot) {
                if !session.open && session.open_by <= now {
                    session.closed = true;
                }
            }
        }

        // --- 5. reap ---------------------------------------------------
        reap(sessions, &transport, &stats, &closed);

        // H3: re-offer closes the mesh never received. Still
        // non-blocking — a notification that cannot be delivered now
        // goes back on its slot and is offered again next turn.
        for id in transport.pending_evictions() {
            // R-A: the mark is cleared only once the notification
            // has been ACCEPTED. A refusal leaves every remaining
            // mark standing — the previous loop cleared them all up
            // front and abandoned the rest on the first refusal.
            if closed.try_send(id).is_err() {
                break;
            }
            transport.clear_pending_eviction(id);
            stats.note_close_notify_redelivered();
        }

        // --- 6. one bounded socket read --------------------------------
        let wait = sessions
            .values()
            .map(|s| s.timeout)
            .min()
            .map(|t| t.saturating_duration_since(Instant::now()))
            .unwrap_or(MAX_POLL_INTERVAL)
            .min(MAX_POLL_INTERVAL)
            .max(Duration::from_micros(500));

        // R6: the injected reset is fed to the **same** read-result
        // handler a real `recv_from` error goes through. The earlier
        // hook incremented the counter on its own branch, so deleting
        // the production `ConnectionReset` arm would not have failed
        // the witness that claimed to cover it.
        #[cfg(any(test, feature = "fixtures"))]
        let injected_reset = hooks.take_conn_reset();
        #[cfg(not(any(test, feature = "fixtures")))]
        let injected_reset = false;
        let read = if injected_reset {
            Ok(Err(std::io::Error::new(
                std::io::ErrorKind::ConnectionReset,
                "injected ICMP port-unreachable",
            )))
        } else {
            tokio::time::timeout(wait, socket.recv_from(&mut buf)).await
        };

        match read {
            Ok(Ok((n, source))) => {
                receive(
                    &config,
                    sessions,
                    advertised,
                    socket,
                    &transport,
                    &stats,
                    &ingress,
                    #[cfg(any(test, feature = "fixtures"))]
                    &hooks,
                    &buf[..n],
                    source,
                )
                .await;
            }
            // See rule 6: an ICMP port-unreachable about a peer that
            // went away says nothing about this socket's health.
            Ok(Err(e)) if e.kind() == std::io::ErrorKind::ConnectionReset => {
                stats.note_udp_conn_reset();
            }
            Ok(Err(e)) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Ok(Err(e)) => {
                tracing::debug!(error = %e, "rtc socket read error");
            }
            Err(_) => {} // read timeout: fall through to the pump
        }
    }

    // Teardown is `SessionTable::drop`, below — one implementation
    // for the cooperative exit and the aborted one.
    drop(table);
}

/// The STUN-only socket's loop: answer uncredentialed binding
/// requests, and carry nothing else.
///
/// No session ever reads or writes this socket — it is not passed to
/// any `Rtc`, never appears as a host candidate, and never receives
/// a session's outbound packet. That is the whole point of it: a
/// leaf can point `iceServers` here without libwebrtc's
/// `UDPPort::OnReadPacket` consuming the anchor's connectivity
/// checks as STUN-server responses.
///
/// A **credentialed** binding request (an ICE connectivity check) is
/// ignored: no session on this socket could have negotiated those
/// credentials, and an unauthenticated response is something the
/// asking ICE agent must discard anyway.
async fn stun_only_loop(
    socket: UdpSocket,
    shutdown: Arc<AtomicBool>,
    done: tokio::sync::watch::Sender<bool>,
) {
    // Same shape as `SessionTable` (H1): release the port and
    // publish completion from a guard, so an aborted task does it
    // too.
    let guard = StunSocket {
        socket: Some(socket),
        done,
    };
    let Some(socket) = guard.socket.as_ref() else {
        return;
    };
    let mut buf = vec![0u8; RECV_BUF];

    while !shutdown.load(Ordering::Acquire) {
        match tokio::time::timeout(STUN_POLL_INTERVAL, socket.recv_from(&mut buf)).await {
            Ok(Ok((n, source))) => {
                let datagram = &buf[..n];
                if stun::is_binding_request(datagram) && !stun::has_username(datagram) {
                    answer_binding_request(socket, datagram, source).await;
                }
            }
            // Rule 6: an ICMP port-unreachable about a client that
            // went away says nothing about this socket's health, and
            // on Windows it fails the *next* `recv_from`.
            Ok(Err(e)) if e.kind() == std::io::ErrorKind::ConnectionReset => {}
            Ok(Err(e)) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Ok(Err(e)) => {
                tracing::debug!(error = %e, "rtc stun socket read error");
            }
            Err(_) => {} // read timeout: re-check the shutdown flag
        }
    }

    drop(guard);
}

/// Releases the STUN-only socket and **records** its release,
/// whether [`stun_only_loop`] exits cooperatively or is aborted.
struct StunSocket {
    socket: Option<UdpSocket>,
    done: tokio::sync::watch::Sender<bool>,
}

impl Drop for StunSocket {
    fn drop(&mut self) {
        drop(self.socket.take());
        // `send_replace`, not `send`: this driver retains no
        // receiver, and `send` neither notifies nor STORES when
        // there is none — so the release of a socket nobody was
        // watching was lost, and a join taken afterwards read
        // `false` for ever. See `TaskRelease`.
        self.done.send_replace(true);
    }
}

/// The driver's session table plus the teardown its `Drop` owns
/// (H1).
///
/// Every remaining session's backlog is discarded and counted, each
/// close is announced to the mesh, and the transport is made
/// terminal so no historical handle can admit another packet. The
/// socket is a separate binding in `driver_loop`, dropped after
/// this one, so the transport contract is settled before the port
/// is free.
struct SessionTable {
    sessions: HashMap<u32, Session>,
    /// The RTC socket, owned here so teardown order is explicit:
    /// slots closed and counted, transport terminal, **socket
    /// released**, and only then release recorded. A joiner woken
    /// by `done` therefore always finds the port free.
    socket: Option<UdpSocket>,
    transport: Arc<RtcTransport>,
    stats: Arc<RtcStats>,
    closed: mpsc::Sender<RtcPeerId>,
    done: tokio::sync::watch::Sender<bool>,
}

impl Drop for SessionTable {
    fn drop(&mut self) {
        for session in self.sessions.values() {
            let retained = usize::from(session.retry.is_some());
            if retained > 0 {
                self.stats.note_unretained();
            }
            self.transport.close_peer(session.id, retained);
            let _ = self.closed.try_send(session.id);
        }
        // Slots the driver never had a `Session` for — an offer that
        // never opened — are closed here too, and the transport
        // becomes terminal.
        self.transport.shutdown_terminal();
        drop(self.socket.take());
        // Recorded, not merely signalled: see `StunSocket::drop`.
        self.done.send_replace(true);
    }
}

/// Pump one peer: at most one `Channel::write` per drain.
async fn pump_peer(
    slot: u32,
    sessions: &mut HashMap<u32, Session>,
    socket: &UdpSocket,
    transport: &Arc<RtcTransport>,
    stats: &Arc<RtcStats>,
    ingress: &mpsc::Sender<(Bytes, RtcPeerId)>,
    #[cfg(any(test, feature = "fixtures"))] hooks: &Arc<RtcTestHooks>,
) {
    // The quantum: a busy peer gets a bounded share of this turn, not
    // the whole of it (R6).
    for _ in 0..WRITE_QUANTUM_PER_TURN {
        let Some(session) = sessions.get_mut(&slot) else {
            return;
        };
        if !session.open || session.closed {
            return;
        }
        let Some(cid) = session.cid else { return };

        let packet = match session.retry.take() {
            Some(p) => {
                stats.note_unretained();
                p
            }
            None => match transport.pop(slot) {
                Some(p) => p,
                None => return,
            },
        };

        // Publish the reading this write is about to invalidate: that
        // is what bounds the advisory's staleness to one packet
        // (S0b §4b measured max 8 089 B, mean 3 235 B).
        let Some(mut channel) = session.rtc.channel(cid) else {
            // The channel vanished between drains; the packet is
            // still ours, so retain it for the close accounting.
            stats.note_retained();
            session.retry = Some(packet);
            session.closed = true;
            return;
        };
        let amount = channel.buffered_amount();
        transport.publish_buffered(session.id.slot, amount);

        #[cfg(any(test, feature = "fixtures"))]
        let forced_false = hooks.write_forced_false();
        #[cfg(not(any(test, feature = "fixtures")))]
        let forced_false = false;
        let wrote = if forced_false {
            // The injected post-acceptance refusal: the precheck
            // above passed, so this is the path that must retain
            // rather than drop.
            Ok(false)
        } else {
            channel.write(true, &packet)
        };
        match wrote {
            Ok(true) => {
                stats.note_written();
            }
            Ok(false) => {
                // Rule 4: retain, stop this peer's pump for the
                // iteration, and let admission do the refusing.
                stats.note_write_false();
                stats.note_retained();
                session.retry = Some(packet);
                drain_session(
                    session,
                    socket,
                    transport,
                    stats,
                    ingress,
                    #[cfg(any(test, feature = "fixtures"))]
                    hooks,
                )
                .await;
                return;
            }
            Err(e) => {
                tracing::debug!(error = %e, "rtc channel write failed; closing session");
                stats.note_retained();
                session.retry = Some(packet);
                session.closed = true;
                return;
            }
        }
        drain_session(
            session,
            socket,
            transport,
            stats,
            ingress,
            #[cfg(any(test, feature = "fixtures"))]
            hooks,
        )
        .await;
        if sessions.get(&slot).is_some_and(|s| s.closed) {
            return;
        }
    }
}

/// Rule 2: drain to `Output::Timeout` after every mutation.
async fn drain_session(
    session: &mut Session,
    socket: &UdpSocket,
    transport: &Arc<RtcTransport>,
    stats: &Arc<RtcStats>,
    ingress: &mpsc::Sender<(Bytes, RtcPeerId)>,
    #[cfg(any(test, feature = "fixtures"))] hooks: &Arc<RtcTestHooks>,
) {
    loop {
        match session.rtc.poll_output() {
            Ok(Output::Timeout(t)) => {
                session.timeout = t;
                return;
            }
            Ok(Output::Transmit(t)) => {
                session.last_transmit = Some(t.destination);
                // Injected PRE-SCTP loss: the datagram never leaves
                // the socket, so the peer's SCTP never sees the
                // chunk. A reliable DataChannel retransmits it; a
                // `maxRetransmits: 0` one does not — which is the
                // difference the `ChannelData` injector above
                // cannot express, because by then SCTP has already
                // delivered.
                #[cfg(any(test, feature = "fixtures"))]
                if hooks.drop_this_raw_egress(&t.contents) {
                    continue;
                }
                // The send result is NOT discarded. A datagram the
                // kernel refuses — no route, EPERM from a firewall,
                // an address the socket's namespace cannot reach —
                // is indistinguishable at the peer from a packet
                // that was never generated, and str0m will happily
                // go on nominating a pair whose every transmit
                // failed. That is the one boundary between "the
                // engine answered" and "the answer left the host".
                // Boundaries 5 and 6, per transaction: a STUN reply
                // the engine generated, and whether it actually left
                // the host. The transaction id ties it back to the
                // request logged on ingress, so one check can be
                // followed across every boundary without a browser
                // counter in the argument. Non-STUN egress (DTLS,
                // SCTP) is not traced: it only exists once ICE has
                // already succeeded.
                let reply_tid = (t.contents.len() >= 12
                    && t.contents[4..8] == [0x21, 0x12, 0xa4, 0x42])
                .then(|| {
                    (
                        u16::from_be_bytes([t.contents[0], t.contents[1]]),
                        u32::from_be_bytes([
                            t.contents[8],
                            t.contents[9],
                            t.contents[10],
                            t.contents[11],
                        ]),
                    )
                });
                match socket.send_to(&t.contents, t.destination).await {
                    Err(e) => tracing::debug!(
                        destination = %t.destination,
                        bytes = t.contents.len(),
                        error = %e,
                        "an RTC datagram could not be sent"
                    ),
                    Ok(sent) => {
                        if let Some((kind, tid)) = reply_tid {
                            tracing::debug!(
                                destination = %t.destination,
                                stun_type = format!("{kind:04x}"),
                                tid = format!("{tid:08x}"),
                                bytes = sent,
                                "a STUN message left the socket"
                            );
                        }
                    }
                }
            }
            Ok(Output::Event(event)) => match event {
                Event::ChannelOpen(cid, _label) => {
                    session.cid = Some(cid);
                    session.open = true;
                    for waiter in session.open_waiters.drain(..) {
                        let _ = waiter.send(Ok(()));
                    }
                }
                Event::ChannelData(data) => {
                    // Injected DataChannel loss. With
                    // `maxRetransmits: 0` SCTP will not recover it,
                    // which is the point: `reliability.rs` is the
                    // only mechanism that can.
                    #[cfg(any(test, feature = "fixtures"))]
                    if hooks.drop_this_ingress(&data.data) {
                        continue;
                    }
                    // Rule 5: never block the driver. A full input
                    // drops and counts.
                    match ingress.try_send((Bytes::from(data.data), session.id)) {
                        Ok(()) => stats.note_ingress_delivered(),
                        Err(mpsc::error::TrySendError::Full(_)) => stats.note_ingress_dropped(),
                        Err(mpsc::error::TrySendError::Closed(_)) => {
                            // The receive loop is gone; nothing left
                            // to deliver to.
                            session.closed = true;
                        }
                    }
                }
                Event::ChannelClose(_) => {
                    session.open = false;
                    session.closed = true;
                }
                Event::Connected => {}
                _ => {}
            },
            Err(e) => {
                tracing::debug!(error = %e, "rtc poll_output failed; closing session");
                session.closed = true;
                let _ = transport;
                return;
            }
        }
    }
}

/// Remove dead sessions, bumping the generation and counting whatever
/// was still owed.
fn reap(
    sessions: &mut HashMap<u32, Session>,
    transport: &Arc<RtcTransport>,
    stats: &Arc<RtcStats>,
    closed: &mpsc::Sender<RtcPeerId>,
) {
    let dead: Vec<u32> = sessions
        .iter()
        .filter(|(_, s)| s.closed || !s.rtc.is_alive())
        .map(|(slot, _)| *slot)
        .collect();
    for slot in dead {
        let Some(mut session) = sessions.remove(&slot) else {
            continue;
        };
        let retained = usize::from(session.retry.take().is_some());
        if retained > 0 {
            stats.note_unretained();
        }
        transport.close_peer(session.id, retained);
        for waiter in session.open_waiters.drain(..) {
            let _ = waiter.send(Err("rtc session closed before the channel opened".into()));
        }
        // R3-E: tell the mesh, which owns peer removal. Bounded and
        // non-blocking like every other driver output — a full
        // notification channel cannot be allowed to stall the
        // driver. H3: a `try_send` that fails is no longer a
        // discarded fact. The close is recorded on the slot and
        // re-offered on a later turn, so an exact lifetime's removal
        // does not depend on the failure detector noticing.
        if closed.try_send(session.id).is_err() {
            transport.mark_pending_eviction(session.id);
            stats.note_close_notify_deferred();
        }
    }
}

/// The bare STUN responder: answer one binding request, reporting
/// the sender's own address back to it. Returns whether a response
/// went out.
///
/// One implementation, shared by the RTC socket's dispatch and the
/// STUN-only socket, so the two cannot drift. Counting is the
/// caller's: `RtcStats::stun_binding_requests` is specifically "a
/// peer aimed at our published `rtc_addr`" (R8), which the
/// STUN-only socket's traffic is not.
async fn answer_binding_request(socket: &UdpSocket, datagram: &[u8], source: SocketAddr) -> bool {
    // `None` when the datagram is not a binding request, so no
    // caller can accidentally answer arbitrary traffic.
    let Some(response) = stun::binding_response(datagram, source) else {
        return false;
    };
    let _ = socket.send_to(&response, source).await;
    true
}

/// Route one datagram: STUN first (it is not str0m's), then the
/// session that claims it.
#[expect(
    clippy::too_many_arguments,
    reason = "one call site; the alternative is a parameter struct that exists only for this call"
)]
async fn receive(
    config: &RtcConfig,
    sessions: &mut HashMap<u32, Session>,
    advertised: SocketAddr,
    socket: &UdpSocket,
    transport: &Arc<RtcTransport>,
    stats: &Arc<RtcStats>,
    ingress: &mpsc::Sender<(Bytes, RtcPeerId)>,
    #[cfg(any(test, feature = "fixtures"))] hooks: &Arc<RtcTestHooks>,
    datagram: &[u8],
    source: SocketAddr,
) {
    // R4-A: an **uncredentialed** binding request is a gathering
    // request and nothing else — no session negotiated it, so there
    // is nothing to intercept. A request carrying `USERNAME` is an
    // ICE connectivity check and belongs to whichever session
    // negotiated those credentials; it goes to `Rtc::accepts` first,
    // and reaches the bare responder only if no session claims it.
    if config.serve_stun && stun::is_binding_request(datagram) && !stun::has_username(datagram) {
        if answer_binding_request(socket, datagram, source).await {
            // R8: a peer aiming at our published `rtc_addr` is
            // observable here, and nowhere else — an ICE check
            // carries `USERNAME` and never reaches this arm, and the
            // STUN-only socket is a different published endpoint.
            stats.note_stun_binding_request();
        }
        return;
    }

    let Ok(contents) = datagram.try_into() else {
        return;
    };
    let input = Input::Receive(
        Instant::now(),
        Receive {
            proto: Protocol::Udp,
            source,
            destination: advertised,
            contents,
        },
    );

    // R4-A: **sessions first, STUN second.** An ICE connectivity
    // check *is* a STUN Binding Request, so answering every Binding
    // Request before asking the sessions meant a node with
    // `serve_stun` intercepted its own (and its peers') ICE checks
    // and could never finish a session. `Rtc::accepts` is the real
    // discriminator: it matches the request's ICE credentials
    // (USERNAME/MESSAGE-INTEGRITY) against the session that
    // negotiated them. Only a request **no session claims** is an
    // unsolicited gathering request, and only that one is answered by
    // the bare responder.
    // R4-A trace, per TRANSACTION. Kyra's boundary list for the
    // Chromium NAT rows: request received → parsed → accepted by the
    // correct live session → authenticated response generated → sent.
    // Each boundary below records its own outcome, keyed by the
    // transaction's first four bytes, so one check can be followed
    // end to end without a browser counter in the argument and
    // without a credential in the log. The id prefix is opaque: it
    // identifies, it does not authenticate.
    let credentialed_check = stun::is_binding_request(datagram) && stun::has_username(datagram);
    let tid = if credentialed_check && datagram.len() >= 12 {
        Some(u32::from_be_bytes([
            datagram[8],
            datagram[9],
            datagram[10],
            datagram[11],
        ]))
    } else {
        None
    };

    let target = sessions
        .iter()
        .find(|(_, s)| s.rtc.accepts(&input))
        .map(|(slot, _)| *slot);
    let Some(slot) = target else {
        if config.serve_stun && stun::is_binding_request(datagram) {
            // A binding request carrying USERNAME that NO session
            // claims is the interesting case: it is an ICE check
            // addressed to credentials this anchor does not
            // recognise, and answering it as a gathering request
            // tells the peer nothing it can use. Name it — a check
            // that is silently unclaimed is indistinguishable, from
            // the peer's side, from a dropped packet.
            if stun::has_username(datagram) {
                tracing::debug!(
                    %source,
                    sessions = sessions.len(),
                    "an ICE connectivity check no session claimed"
                );
            }
            if answer_binding_request(socket, datagram, source).await {
                stats.note_stun_binding_request();
            }
        }
        return;
    };
    let Some(session) = sessions.get_mut(&slot) else {
        return;
    };
    // Boundary 3: which live session claimed it. `accepts` matches the
    // request's ICE credentials, so this names the session that owns
    // them — the thing a "no such dialog" string could not.
    if let Some(tid) = tid {
        tracing::debug!(
            %source,
            slot,
            tid = format!("{tid:08x}"),
            "ICE check claimed by a live session"
        );
    }
    // Boundary 4: the engine's verdict on the parsed datagram.
    if let Err(e) = session.rtc.handle_input(input) {
        tracing::debug!(%source, slot, error = %e, "str0m refused a datagram; closing the session");
        session.closed = true;
    } else if let Some(tid) = tid {
        tracing::debug!(slot, tid = format!("{tid:08x}"), "str0m accepted the check");
    }
    // Boundary 5 and 6 are inside `drain_session`: it is what polls
    // the response out of the engine and puts it on the socket, and
    // it reports a refused send.
    drain_session(
        session,
        socket,
        transport,
        stats,
        ingress,
        #[cfg(any(test, feature = "fixtures"))]
        hooks,
    )
    .await;
}

#[expect(
    clippy::too_many_arguments,
    reason = "the signalling handler mutates the same state set the loop owns; a struct would only rename the arguments"
)]
async fn handle_signal(
    config: &RtcConfig,
    sessions: &mut HashMap<u32, Session>,
    advertised: SocketAddr,
    socket: &UdpSocket,
    transport: &Arc<RtcTransport>,
    stats: &Arc<RtcStats>,
    ingress: &mpsc::Sender<(Bytes, RtcPeerId)>,
    #[cfg(any(test, feature = "fixtures"))] hooks: &Arc<RtcTestHooks>,
    closed: &mpsc::Sender<RtcPeerId>,
    signal: RtcSignal,
) {
    match signal {
        RtcSignal::CreateOffer { reply } => {
            if sessions.len() >= config.max_peers {
                let _ = reply.send(Err("rtc: max_peers reached".into()));
                return;
            }
            let mut session = match new_session(config, transport, advertised) {
                Ok(s) => s,
                Err(e) => {
                    let _ = reply.send(Err(format!("rtc: {e}")));
                    return;
                }
            };
            let mut api = session.rtc.sdp_api();
            // §3: one DataChannel, unordered, zero retransmits — the
            // reliability that matters is `reliability.rs`'s, and two
            // retransmit mechanisms in series is worse than one.
            api.add_channel_with_config(ChannelConfig {
                label: "net".to_string(),
                ordered: false,
                reliability: Reliability::MaxRetransmits { retransmits: 0 },
                negotiated: None,
                protocol: String::new(),
            });
            match api.apply() {
                Some((offer, pending)) => {
                    session.pending = Some(pending);
                    let id = session.id;
                    let sdp = offer.to_sdp_string();
                    drain_session(
                        &mut session,
                        socket,
                        transport,
                        stats,
                        ingress,
                        #[cfg(any(test, feature = "fixtures"))]
                        hooks,
                    )
                    .await;
                    sessions.insert(id.slot, session);
                    let _ = reply.send(Ok((id, sdp)));
                }
                None => {
                    transport.close_peer(session.id, 0);
                    let _ = reply.send(Err("rtc: no changes to apply".into()));
                }
            }
        }
        RtcSignal::AcceptOffer { offer_sdp, reply } => {
            if sessions.len() >= config.max_peers {
                let _ = reply.send(Err("rtc: max_peers reached".into()));
                return;
            }
            let offer = match SdpOffer::from_sdp_string(&offer_sdp) {
                Ok(o) => o,
                Err(e) => {
                    let _ = reply.send(Err(format!("rtc: bad offer: {e}")));
                    return;
                }
            };
            let mut session = match new_session(config, transport, advertised) {
                Ok(s) => s,
                Err(e) => {
                    let _ = reply.send(Err(format!("rtc: {e}")));
                    return;
                }
            };
            match session.rtc.sdp_api().accept_offer(offer) {
                Ok(answer) => {
                    let id = session.id;
                    let sdp = answer.to_sdp_string();
                    drain_session(
                        &mut session,
                        socket,
                        transport,
                        stats,
                        ingress,
                        #[cfg(any(test, feature = "fixtures"))]
                        hooks,
                    )
                    .await;
                    sessions.insert(id.slot, session);
                    let _ = reply.send(Ok((id, sdp)));
                }
                Err(e) => {
                    transport.close_peer(session.id, 0);
                    let _ = reply.send(Err(format!("rtc: accept_offer: {e}")));
                }
            }
        }
        RtcSignal::AcceptAnswer {
            peer,
            answer_sdp,
            reply,
        } => {
            let Some(session) = session_for(sessions, peer) else {
                let _ = reply.send(Err(STALE_HANDLE.into()));
                return;
            };
            let answer = match SdpAnswer::from_sdp_string(&answer_sdp) {
                Ok(a) => a,
                Err(e) => {
                    let _ = reply.send(Err(format!("rtc: bad answer: {e}")));
                    return;
                }
            };
            let Some(pending) = session.pending.take() else {
                let _ = reply.send(Err("rtc: no pending offer".into()));
                return;
            };
            match session.rtc.sdp_api().accept_answer(pending, answer) {
                Ok(()) => {
                    drain_session(
                        session,
                        socket,
                        transport,
                        stats,
                        ingress,
                        #[cfg(any(test, feature = "fixtures"))]
                        hooks,
                    )
                    .await;
                    let _ = reply.send(Ok(()));
                }
                Err(e) => {
                    session.closed = true;
                    let _ = reply.send(Err(format!("rtc: accept_answer: {e}")));
                }
            }
        }
        RtcSignal::SelectedPair { peer, reply } => {
            // An unnamed socket cannot report a local half; the
            // unspecified address says "not known" without panicking
            // on a path an operator is only observing.
            let socket_addr = socket.local_addr().unwrap_or(std::net::SocketAddr::new(
                std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED),
                0,
            ));
            let answer = session_for(sessions, peer).and_then(|session| {
                let remote = session.last_transmit?;
                let learned = if session.signalled_remotes.contains(&remote) {
                    "signalled"
                } else {
                    "peer-reflexive"
                };
                Some((socket_addr, remote, learned))
            });
            let _ = reply.send(answer);
        }
        RtcSignal::RemoteCandidate {
            peer,
            candidate,
            reply,
        } => {
            let Some(session) = session_for(sessions, peer) else {
                let _ = reply.send(Err(STALE_HANDLE.into()));
                return;
            };
            match Candidate::from_sdp_string(&candidate) {
                Ok(c) => {
                    if let Some(addr) = c.addr().into() {
                        let addr: std::net::SocketAddr = addr;
                        if !session.signalled_remotes.contains(&addr) {
                            session.signalled_remotes.push(addr);
                        }
                    }
                    session.rtc.add_remote_candidate(c);
                    drain_session(
                        session,
                        socket,
                        transport,
                        stats,
                        ingress,
                        #[cfg(any(test, feature = "fixtures"))]
                        hooks,
                    )
                    .await;
                    let _ = reply.send(Ok(()));
                }
                Err(e) => {
                    let _ = reply.send(Err(format!("rtc: bad candidate: {e}")));
                }
            }
        }
        RtcSignal::AwaitOpen { peer, reply } => {
            match session_for(sessions, peer) {
                Some(session) if session.open => {
                    let _ = reply.send(Ok(()));
                }
                Some(session) => session.open_waiters.push(reply),
                None => {
                    let _ = reply.send(Err(STALE_HANDLE.into()));
                }
            };
        }
        RtcSignal::Close { peer } => {
            // R3-C: a handle from a previous lifetime of this slot
            // must not be able to evict its successor. With slots
            // recycled (R3-D) that is no longer theoretical.
            if let Some(session) = session_for(sessions, peer) {
                session.closed = true;
            }
            reap(sessions, transport, stats, closed);
        }
    }
}

/// What every signal arm says to a handle that is not the live
/// incarnation of its slot.
const STALE_HANDLE: &str = "rtc: unknown session (stale or wrong-generation handle)";

/// Resolve a signal's handle to the session **only** when the handle
/// names the live incarnation. Looking up by `slot` alone let a
/// wrong-generation `Close` kill a live successor and a
/// wrong-generation `AwaitOpen` report someone else's open (R3-C).
fn session_for(sessions: &mut HashMap<u32, Session>, peer: RtcPeerId) -> Option<&mut Session> {
    sessions.get_mut(&peer.slot).filter(|s| s.id == peer)
}

fn new_session(
    config: &RtcConfig,
    transport: &Arc<RtcTransport>,
    advertised: SocketAddr,
) -> Result<Session, super::transport::RtcError> {
    let mut rtc = Rtc::new(Instant::now());
    if let Ok(candidate) = Candidate::host(advertised, "udp") {
        rtc.add_local_candidate(candidate);
    }
    // R3-D: this is the recycling allocator now. It can refuse, and a
    // refusal is a real answer — silently reusing an identity is what
    // it exists to prevent.
    let id = transport.open_peer()?;
    Ok(Session {
        rtc,
        id,
        cid: None,
        pending: None,
        retry: None,
        last_transmit: None,
        signalled_remotes: Vec::new(),
        timeout: Instant::now(),
        open: false,
        closed: false,
        open_by: Instant::now() + config.ice_deadline,
        open_waiters: Vec::new(),
    })
}
