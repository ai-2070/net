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
    /// Make the next socket read fail with `ConnectionReset`.
    inject_conn_reset: AtomicBool,
    /// Park the loop in a long sleep so a caller can exercise the
    /// **timeout** arm of [`RtcDriverHandle::shutdown_and_join`]
    /// against a task that will not cooperate (H1).
    stall_loop: AtomicBool,
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

    fn pump_paused(&self) -> bool {
        self.pause_pump.load(Ordering::Acquire)
    }

    fn write_forced_false(&self) -> bool {
        self.force_write_false.load(Ordering::Acquire)
    }

    fn take_conn_reset(&self) -> bool {
        self.inject_conn_reset.swap(false, Ordering::AcqRel)
    }

    fn drop_this_ingress(&self) -> bool {
        let n = self.ingress_drop_one_in.load(Ordering::Acquire);
        if n == 0 {
            return false;
        }
        let seen = self.ingress_seen.fetch_add(1, Ordering::Relaxed) + 1;
        seen.is_multiple_of(n)
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
    /// The driver task, so shutdown can **join** it. Without this the
    /// task (and its bound UDP socket) outlived the node that created
    /// it for as long as the runtime ran — a successor could not
    /// rebind an explicit RTC port, and a shut-down node with
    /// `serve_stun` kept answering (R3-B).
    task: Arc<parking_lot::Mutex<Option<tokio::task::JoinHandle<()>>>>,
    /// Teardown **completion**, published by the task's own guard
    /// (H1). `task` is ownership of the join, not completion of it:
    /// a second caller that found `None` used to return while the
    /// driver was still running. Every caller now waits on this.
    done: tokio::sync::watch::Sender<bool>,
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
        if let Some(handle) = self.task.lock().take() {
            handle.abort();
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
        let handle = self.task.lock().take();
        let Some(handle) = handle else {
            // Someone else owns the join. Wait for the task's own
            // completion signal, not for that caller's return.
            self.await_teardown().await;
            return;
        };
        // Cancel-safety: if this future is dropped mid-await the
        // handle goes back where it came from, so a later
        // `shutdown_detached` can still abort the task.
        let mut slot = JoinSlot {
            home: Arc::clone(&self.task),
            handle: Some(handle),
        };
        let joined = {
            let handle = slot.handle.as_mut().expect("just set");
            tokio::time::timeout(Duration::from_secs(2), handle).await
        };
        if joined.is_err() {
            tracing::debug!("rtc driver did not exit in time; aborting");
            let handle = slot.handle.as_mut().expect("still held");
            handle.abort();
            // The join the abort is not: without this the method
            // returned while cancellation — and the socket's
            // release — was still pending.
            let _ = handle.await;
        }
        // Joined: the task is gone, so drop the handle rather than
        // returning it.
        let _ = slot.handle.take();
        // The guard publishes completion from inside the task; on a
        // runtime that never polls the aborted task again this is
        // the backstop, and it is idempotent.
        let _ = self.done.send(true);
    }

    /// Park until the driver task's teardown guard has run.
    async fn await_teardown(&self) {
        let mut rx = self.done.subscribe();
        if *rx.borrow() {
            return;
        }
        // `changed()` errors only if every sender is gone, which
        // cannot happen while this handle holds one.
        while rx.changed().await.is_ok() {
            if *rx.borrow() {
                return;
            }
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
}

/// The driver task.
pub struct RtcDriver;

impl RtcDriver {
    /// Bind the RTC socket and spawn the driver.
    ///
    /// The socket is the node's second: §6 rules out demultiplexing
    /// RTC traffic on the Net socket, so there is no discriminator to
    /// get wrong and no way for RTC traffic to reach the Net ingress.
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
        let socket = UdpSocket::bind(config.resolved_bind_addr(net_bind_addr)).await?;
        let local_addr = socket.local_addr()?;
        let advertised = config.public_addr.unwrap_or(local_addr);

        let transport = Arc::new(RtcTransport::new(&config, Arc::clone(&stats)));
        let (signal_tx, signal_rx) = mpsc::channel(64);
        let shutdown = Arc::new(AtomicBool::new(false));
        #[cfg(any(test, feature = "fixtures"))]
        let hooks = Arc::new(RtcTestHooks::default());

        let (done_tx, _done_rx) = tokio::sync::watch::channel(false);
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
            task: Arc::new(parking_lot::Mutex::new(Some(task))),
            done: done_tx,
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
    let mut sessions = &mut table.sessions;
    let socket = table
        .socket
        .as_ref()
        .expect("the socket is taken only by teardown");
    let mut buf = vec![0u8; RECV_BUF];

    while !shutdown.load(Ordering::Acquire) {
        // H1 witness support: a driver that will not notice the
        // shutdown flag, so `shutdown_and_join` has to take its
        // timeout/abort arm.
        #[cfg(any(test, feature = "fixtures"))]
        if hooks.loop_stalled() {
            tokio::time::sleep(Duration::from_secs(60)).await;
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
                &mut sessions,
                advertised,
                &socket,
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
                    &mut sessions,
                    &socket,
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
                        &socket,
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
        reap(&mut sessions, &transport, &stats, &closed);

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
                    &mut sessions,
                    advertised,
                    &socket,
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
    /// released**, and only then completion published. A joiner
    /// woken by `done` therefore always finds the port free.
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
        let _ = self.done.send(true);
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
                let _ = socket.send_to(&t.contents, t.destination).await;
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
                    if hooks.drop_this_ingress() {
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
        // notification channel cannot be allowed to stall the driver,
        // and the failure detector remains the backstop.
        let _ = closed.try_send(session.id);
    }
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
        if let Some(response) = stun::binding_response(datagram, source) {
            let _ = socket.send_to(&response, source).await;
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
    let target = sessions
        .iter()
        .find(|(_, s)| s.rtc.accepts(&input))
        .map(|(slot, _)| *slot);
    let Some(slot) = target else {
        if config.serve_stun && stun::is_binding_request(datagram) {
            if let Some(response) = stun::binding_response(datagram, source) {
                let _ = socket.send_to(&response, source).await;
            }
        }
        return;
    };
    let Some(session) = sessions.get_mut(&slot) else {
        return;
    };
    if session.rtc.handle_input(input).is_err() {
        session.closed = true;
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
        timeout: Instant::now(),
        open: false,
        closed: false,
        open_by: Instant::now() + config.ice_deadline,
        open_waiters: Vec::new(),
    })
}
