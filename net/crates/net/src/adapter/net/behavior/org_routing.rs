//! OLB-2B: the node-owned private-discovery routing consumer — supervisor,
//! incarnation fencing, and restart policy.
//!
//! This module is the sole consumer of the GLOBAL private-discovery change stream
//! (OLB mints global only; the owner stream stays unclaimed for the provider-free
//! leader track). It lives beside `org_scoped_store` rather than inside it, which
//! is exactly the arrangement OLB-2B-E1's `pub(crate) drain()` exists to permit:
//! the capability is unforgeable outside its home module, yet consumable here.
//!
//! What this slice owns:
//!
//! - **Sole mint authority.** The supervisor holds the node's
//!   [`PrivateDiscoveryDrains`]. Nothing else mints, and an actor cannot exist
//!   without the capability because the drain is moved into it.
//! - **Incarnation fencing.** Every actor run has a monotonic incarnation id, and
//!   its exit — clean OR abnormal — fences routing health SYNCHRONOUSLY, before a
//!   successor can exist, so work from a dead incarnation is never trusted.
//! - **Restart policy.** Supervised automatic restart with capped exponential
//!   backoff and bounded attempts in a rolling window, terminating in a
//!   fail-closed crash-loop state (Kyra OLB-2B, Q1).

// E2-ONLY. This module is exercised by its witnesses but not yet spawned by
// `MeshNode`: per Kyra's Q2 the production node must not run a lifecycle-only
// counter sink, so the node wiring lands in E3 together with the bounded routing
// registry that is the real consumer. THIS ALLOW IS REMOVED IN E3 — if it is
// still here, the real consumer never landed.
#![allow(dead_code)]

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Notify;

use super::org_scoped_store::{
    DirtyCapabilities, PrivateDiscoveryChangeBatch, PrivateDiscoveryDrain, PrivateDiscoveryDrains,
    PrivateDiscoveryStream,
};

/// Whether cached routing state may be trusted.
///
/// Consulted by every warmed call once route consumption lands (OLB-2B.3). Until
/// then it is published and witnessed here, which is where the fence is DEFINED:
/// a later slice must not have to invent it while also wiring the call path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RoutingHealth {
    /// Routes built by this incarnation are usable.
    Healthy { incarnation: u64 },
    /// A recapture is in progress; routes from this incarnation are not complete.
    Rebuilding { incarnation: u64 },
    /// NO cached route is usable — before the first incarnation, between
    /// incarnations, and permanently after crash-loop exhaustion. A call must take
    /// the fresh current-authority cold path, or fail locally before proof/send.
    Fenced,
}

impl RoutingHealth {
    /// Whether a cached route stamped by `incarnation` may be used. Fenced and
    /// mid-rebuild states are unusable, and so is a route from any incarnation
    /// other than the live one — which is what stops detached work from a dead run
    /// being trusted after a successor starts.
    #[allow(dead_code)] // Consumed by OLB-2B.3's warmed-call path.
    pub(crate) fn allows(&self, incarnation: u64) -> bool {
        matches!(self, RoutingHealth::Healthy { incarnation: live } if *live == incarnation)
    }
}

/// The node's published routing health.
pub(crate) type SharedRoutingHealth = Arc<arc_swap::ArcSwap<RoutingHealth>>;

/// A fresh health cell, fenced until an incarnation says otherwise.
pub(crate) fn new_routing_health() -> SharedRoutingHealth {
    Arc::new(arc_swap::ArcSwap::from_pointee(RoutingHealth::Fenced))
}

/// Applies a drained change batch. Implemented by the bounded routing registry in
/// OLB-2B-E3; the actor is written against this seam so the supervisor and the
/// registry can be reviewed apart.
pub(crate) trait DirtyApply: Send + 'static {
    /// Apply one drained batch. Runs OFF the scoped-state lock.
    fn apply(&mut self, incarnation: u64, batch: PrivateDiscoveryChangeBatch);
}

/// The shared applier. Held across incarnations so a successor continues against
/// the same registry rather than a fresh one.
pub(crate) type SharedApply = Arc<parking_lot::Mutex<Box<dyn DirtyApply>>>;

/// Why an actor incarnation stopped.
#[derive(Debug, PartialEq, Eq)]
enum ActorExit {
    /// The node is shutting down — do not restart.
    Shutdown,
    /// The change watch closed: the scoped-discovery source is gone (the node is
    /// being torn down). Terminal, and NOT a fault — restarting would spin against
    /// a source that no longer exists.
    SourceGone,
}

/// Fences routing health when an incarnation ends, on EVERY exit path including a
/// panic unwind.
///
/// Fencing lives in the actor's own stack rather than in the supervisor's
/// observation of the join handle, so it is SYNCHRONOUS with the incarnation's
/// death: there is no window in which the task is dead but its routes still look
/// usable (Kyra OLB-2B-E2).
struct IncarnationFence {
    health: SharedRoutingHealth,
}

impl Drop for IncarnationFence {
    fn drop(&mut self) {
        self.health.store(Arc::new(RoutingHealth::Fenced));
    }
}

/// Capped exponential backoff between restarts.
const RESTART_BACKOFF_BASE: Duration = Duration::from_millis(100);
/// Ceiling for that backoff — a deterministic fault must not spin.
const RESTART_BACKOFF_CAP: Duration = Duration::from_secs(30);
/// Restarts tolerated inside [`RESTART_WINDOW`] before the crash-loop state.
const MAX_RESTARTS_IN_WINDOW: usize = 5;
/// The rolling window the restart budget is counted over.
const RESTART_WINDOW: Duration = Duration::from_secs(300);

/// Fixtures-only observation points for the deterministic supervisor witnesses.
#[cfg(feature = "fixtures")]
#[derive(Default)]
pub(crate) struct ActorHooks {
    /// Fired after each drain, with the batch the incarnation observed.
    #[allow(clippy::type_complexity)]
    pub(crate) drained:
        parking_lot::Mutex<Option<Arc<dyn Fn(u64, &PrivateDiscoveryChangeBatch) + Send + Sync>>>,
    /// Fired once per incarnation at start-up. Returning `true` panics that
    /// incarnation, which is how the restart/fencing witnesses inject faults.
    #[allow(clippy::type_complexity)]
    pub(crate) panic_incarnation:
        parking_lot::Mutex<Option<Arc<dyn Fn(u64) -> bool + Send + Sync>>>,
}

#[cfg(feature = "fixtures")]
impl ActorHooks {
    fn fire_drained(&self, incarnation: u64, batch: &PrivateDiscoveryChangeBatch) {
        if let Some(hook) = self.drained.lock().clone() {
            hook(incarnation, batch);
        }
    }

    fn should_panic(&self, incarnation: u64) -> bool {
        self.panic_incarnation
            .lock()
            .clone()
            .is_some_and(|hook| hook(incarnation))
    }
}

/// Everything one incarnation needs. Owned, so the whole set moves into the task
/// and drops with it — which is what makes awaiting the join handle sufficient
/// proof that the drain has been released.
struct Incarnation {
    drain: PrivateDiscoveryDrain,
    changed: tokio::sync::watch::Receiver<u64>,
    health: SharedRoutingHealth,
    id: u64,
    apply: SharedApply,
    shutdown: Arc<AtomicBool>,
    shutdown_notify: Arc<Notify>,
    #[cfg(feature = "fixtures")]
    hooks: Arc<ActorHooks>,
}

/// Drive one actor incarnation: drain the global change stream and apply it.
///
/// The wake protocol mirrors the exact-expiry timer's, for the same reason: a
/// `Notified` captures the notify-waiters epoch when it is CONSTRUCTED, so
/// checking the shutdown flag before constructing it loses a shutdown landing in
/// the gap and parks the task forever (Kyra OLB-2A.3.2). Constructed and enabled
/// BEFORE the flag load, re-armed every iteration.
async fn run_incarnation(mut it: Incarnation) -> ActorExit {
    // Fences on every exit path, including a panic unwind.
    let _fence = IncarnationFence {
        health: it.health.clone(),
    };

    #[cfg(feature = "fixtures")]
    if it.hooks.should_panic(it.id) {
        panic!("injected routing-actor fault (incarnation {})", it.id);
    }

    loop {
        let shutdown_signal = it.shutdown_notify.notified();
        tokio::pin!(shutdown_signal);
        shutdown_signal.as_mut().enable();
        if it.shutdown.load(Ordering::Acquire) {
            return ActorExit::Shutdown;
        }

        // Mark the current version seen BEFORE draining, so a mutation landing
        // during the drain or the apply is never missed — it either lands in this
        // batch or leaves `changed()` ready for the trailing pass.
        it.changed.borrow_and_update();

        it.health
            .store(Arc::new(RoutingHealth::Rebuilding { incarnation: it.id }));
        let batch = it.drain.drain();
        #[cfg(feature = "fixtures")]
        it.hooks.fire_drained(it.id, &batch);
        let idle = matches!(batch.dirty, DirtyCapabilities::Clean);
        if !idle {
            // Off the scoped-state lock: `drain` released it before returning.
            it.apply.lock().apply(it.id, batch);
        }
        it.health
            .store(Arc::new(RoutingHealth::Healthy { incarnation: it.id }));

        if !idle {
            // Coalesced trailing pass: movement during the apply left the watch
            // ready, so loop straight back rather than sleeping on it.
            continue;
        }

        tokio::select! {
            changed_result = it.changed.changed() => {
                if changed_result.is_err() {
                    // The publication sender is gone: the scoped-discovery source
                    // no longer exists. Terminal, and not a fault — restarting
                    // would spin against nothing.
                    return ActorExit::SourceGone;
                }
            }
            _ = &mut shutdown_signal => return ActorExit::Shutdown,
        }
    }
}

/// The node-owned supervisor: the ONLY mint authority for the global
/// private-discovery stream, and the only thing that starts an incarnation.
pub(crate) struct RoutingSupervisor {
    mint: PrivateDiscoveryDrains,
    health: SharedRoutingHealth,
    incarnations: AtomicU64,
}

impl RoutingSupervisor {
    pub(crate) fn new(mint: PrivateDiscoveryDrains, health: SharedRoutingHealth) -> Self {
        Self {
            mint,
            health,
            incarnations: AtomicU64::new(0),
        }
    }

    /// The live health view, for the node and for witnesses.
    #[allow(dead_code)] // Consumed by OLB-2B.3's warmed-call path.
    pub(crate) fn health(&self) -> SharedRoutingHealth {
        self.health.clone()
    }

    /// How many incarnations have been started. Observability for the witnesses
    /// and, later, the restart counter.
    #[allow(dead_code)]
    pub(crate) fn incarnations_started(&self) -> u64 {
        self.incarnations.load(Ordering::Acquire)
    }

    /// Run the supervision loop until shutdown, the source disappearing, or
    /// crash-loop exhaustion.
    ///
    /// Restart discipline, in order:
    ///
    /// 1. mint the drain — refusing LOUDLY and fencing if the stream is held or
    ///    stranded, rather than running a drainless actor;
    /// 2. spawn ONE incarnation and await its join handle, so a successor is
    ///    minted only after the predecessor task has fully resolved and, with it,
    ///    its drain has dropped and released the lease;
    /// 3. on a fault, apply capped backoff within a bounded rolling window;
    /// 4. on exhaustion, stay `Fenced` permanently rather than spinning.
    ///
    /// The shutdown flag is checked before AND after every mint, so a panic racing
    /// shutdown cannot spawn a replacement that outlives the node.
    pub(crate) async fn run(
        &self,
        changed: tokio::sync::watch::Receiver<u64>,
        apply: SharedApply,
        shutdown: Arc<AtomicBool>,
        shutdown_notify: Arc<Notify>,
        #[cfg(feature = "fixtures")] hooks: Arc<ActorHooks>,
    ) {
        let mut faults: Vec<tokio::time::Instant> = Vec::new();

        loop {
            if shutdown.load(Ordering::Acquire) {
                self.fence();
                return;
            }

            let Some(drain) = self.mint.mint(PrivateDiscoveryStream::Global) else {
                // Held or stranded (a leaked handle never returns its lease). Never
                // proceed drainless: fence and stop, loudly.
                self.fence();
                tracing::error!(
                    "org routing: the global private-discovery drain is unavailable; \
                     routing stays fenced and no actor is started"
                );
                return;
            };

            let id = self.incarnations.fetch_add(1, Ordering::AcqRel) + 1;
            // Re-checked AFTER the mint: the flag may have been set while claiming,
            // and a replacement must not outlive the node.
            if shutdown.load(Ordering::Acquire) {
                drop(drain);
                self.fence();
                return;
            }

            let handle = tokio::spawn(run_incarnation(Incarnation {
                drain,
                changed: changed.clone(),
                health: self.health.clone(),
                id,
                apply: apply.clone(),
                shutdown: shutdown.clone(),
                shutdown_notify: shutdown_notify.clone(),
                #[cfg(feature = "fixtures")]
                hooks: hooks.clone(),
            }));

            // Join before any re-mint: when this resolves the task is finished and
            // its drain has dropped, so the lease is free for a successor.
            match handle.await {
                Ok(ActorExit::Shutdown) | Ok(ActorExit::SourceGone) => {
                    self.fence();
                    return;
                }
                Err(join_error) if join_error.is_cancelled() => {
                    // Aborted (node teardown): not a fault to retry.
                    self.fence();
                    return;
                }
                Err(_panicked) => {
                    let now = tokio::time::Instant::now();
                    faults.retain(|at| now.duration_since(*at) < RESTART_WINDOW);
                    faults.push(now);
                    if faults.len() > MAX_RESTARTS_IN_WINDOW {
                        // Crash loop: a deterministic fault. Stay fenced rather
                        // than retry; recovery needs operator action.
                        self.fence();
                        tracing::error!(
                            faults = faults.len(),
                            "org routing: actor crash-loop budget exhausted; routing \
                             stays fenced until the node is restarted"
                        );
                        return;
                    }
                    let shift = u32::try_from(faults.len()).unwrap_or(u32::MAX).min(16);
                    let backoff = RESTART_BACKOFF_CAP
                        .min(RESTART_BACKOFF_BASE.saturating_mul(1u32 << (shift - 1)));
                    tracing::warn!(
                        incarnation = id,
                        ?backoff,
                        "org routing: actor incarnation faulted; restarting after backoff"
                    );
                    // Backoff stays interruptible by shutdown, on the same
                    // arm-before-check discipline.
                    let shutdown_signal = shutdown_notify.notified();
                    tokio::pin!(shutdown_signal);
                    shutdown_signal.as_mut().enable();
                    if shutdown.load(Ordering::Acquire) {
                        self.fence();
                        return;
                    }
                    tokio::select! {
                        _ = tokio::time::sleep(backoff) => {}
                        _ = &mut shutdown_signal => {
                            self.fence();
                            return;
                        }
                    }
                }
            }
        }
    }

    fn fence(&self) {
        self.health.store(Arc::new(RoutingHealth::Fenced));
    }
}

/// OLB-2B-E2 witnesses: sole-mint authority, synchronous incarnation fencing on
/// abnormal exit, join-before-remint, shutdown during wait and during backoff,
/// closed-watch handling, and deterministic crash-loop exhaustion.
#[cfg(all(test, feature = "fixtures"))]
mod tests {
    use super::*;
    use crate::adapter::net::behavior::capability::CapabilitySet;
    use crate::adapter::net::behavior::org::OrgId;
    use crate::adapter::net::behavior::org_scoped_ingest::{
        CapabilityAudienceScope, PreparedScopedCapability, VerifiedScopedCapability,
    };
    use crate::adapter::net::behavior::org_scoped_store::ScopedDiscoveryState;
    use crate::adapter::net::identity::EntityId;

    type Applied = Arc<parking_lot::Mutex<Vec<(u64, DirtyCapabilities)>>>;

    /// Records every applied batch with the incarnation that applied it.
    struct RecordingApply {
        seen: Applied,
    }

    impl DirtyApply for RecordingApply {
        fn apply(&mut self, incarnation: u64, batch: PrivateDiscoveryChangeBatch) {
            self.seen.lock().push((incarnation, batch.dirty));
        }
    }

    fn owner_record(seed: u8) -> PreparedScopedCapability {
        let descriptor = CapabilitySet::new().add_tag("nrpc:x").to_bytes_compact();
        PreparedScopedCapability::prepare(VerifiedScopedCapability::for_test(
            CapabilityAudienceScope::Owner {
                org_id: OrgId::from_bytes([1u8; 32]),
                audience_handle: [0x11u8; 32],
            },
            EntityId::from_bytes([seed; 32]),
            OrgId::from_bytes([1u8; 32]),
            1,
            10_000,
            5,
            None,
            descriptor,
        ))
    }

    struct Harness {
        state: Arc<parking_lot::Mutex<ScopedDiscoveryState>>,
        health: SharedRoutingHealth,
        seen: Applied,
        apply: SharedApply,
        shutdown: Arc<AtomicBool>,
        notify: Arc<Notify>,
        hooks: Arc<ActorHooks>,
        tx: tokio::sync::watch::Sender<u64>,
        rx: tokio::sync::watch::Receiver<u64>,
    }

    fn harness() -> Harness {
        let state = Arc::new(parking_lot::Mutex::new(ScopedDiscoveryState::new()));
        state.lock().ingest(owner_record(3), 0);
        let seen: Applied = Arc::default();
        let apply: SharedApply = Arc::new(parking_lot::Mutex::new(Box::new(RecordingApply {
            seen: seen.clone(),
        })));
        let (tx, rx) = tokio::sync::watch::channel(0u64);
        Harness {
            state,
            health: new_routing_health(),
            seen,
            apply,
            shutdown: Arc::new(AtomicBool::new(false)),
            notify: Arc::new(Notify::new()),
            hooks: Arc::default(),
            tx,
            rx,
        }
    }

    impl Harness {
        fn supervisor(&self) -> RoutingSupervisor {
            RoutingSupervisor::new(
                PrivateDiscoveryDrains::new(self.state.clone()),
                self.health.clone(),
            )
        }

        fn spawn(&self, sup: Arc<RoutingSupervisor>) -> tokio::task::JoinHandle<()> {
            let (rx, apply, shutdown, notify, hooks) = (
                self.rx.clone(),
                self.apply.clone(),
                self.shutdown.clone(),
                self.notify.clone(),
                self.hooks.clone(),
            );
            tokio::spawn(async move { sup.run(rx, apply, shutdown, notify, hooks).await })
        }

        fn stop(&self) {
            self.shutdown.store(true, Ordering::Release);
            self.notify.notify_waiters();
        }

        fn health(&self) -> RoutingHealth {
            **self.health.load()
        }
    }

    async fn settle() {
        for _ in 0..32 {
            tokio::task::yield_now().await;
        }
    }

    /// The supervisor is the SOLE mint authority: if the global stream is already
    /// held, it refuses to start an actor and fences, rather than running a
    /// drainless one that would silently never reconcile.
    #[tokio::test(start_paused = true)]
    async fn a_held_stream_fences_instead_of_starting_a_drainless_actor() {
        let h = harness();
        // Start from a NON-fenced state so the fence below is load-bearing rather
        // than trivially already true.
        h.health
            .store(Arc::new(RoutingHealth::Healthy { incarnation: 99 }));
        let squatter = PrivateDiscoveryDrains::new(h.state.clone());
        let _held = squatter
            .mint(PrivateDiscoveryStream::Global)
            .expect("squatter holds it");

        let sup = h.supervisor();
        sup.run(
            h.rx.clone(),
            h.apply.clone(),
            h.shutdown.clone(),
            h.notify.clone(),
            h.hooks.clone(),
        )
        .await;

        assert_eq!(sup.incarnations_started(), 0, "no actor was started");
        assert_eq!(h.health(), RoutingHealth::Fenced);
    }

    /// Ordinary operation: the incarnation drains, applies, and publishes Healthy —
    /// and its first batch is the mint's RebuildAll recapture.
    #[tokio::test(start_paused = true)]
    async fn an_incarnation_applies_its_recapture_and_reports_healthy() {
        let h = harness();
        let sup = Arc::new(h.supervisor());
        let run = h.spawn(sup.clone());
        settle().await;

        assert_eq!(
            h.health(),
            RoutingHealth::Healthy { incarnation: 1 },
            "a settled incarnation publishes Healthy"
        );
        assert_eq!(
            h.seen.lock().as_slice(),
            &[(1, DirtyCapabilities::RebuildAll)],
            "the first batch is the mint's complete recapture"
        );

        h.stop();
        run.await.expect("supervisor joins");
        assert_eq!(h.health(), RoutingHealth::Fenced, "exit fences");
    }

    /// A panicking incarnation fences health, and the supervisor mints a SUCCESSOR
    /// only after the predecessor task resolved — proving the lease was released by
    /// the join. The successor recaptures completely, so the delta the dead
    /// incarnation consumed is not lost.
    #[tokio::test(start_paused = true)]
    async fn a_panicked_incarnation_fences_then_a_successor_recaptures() {
        let h = harness();
        *h.hooks.panic_incarnation.lock() = Some(Arc::new(|id| id == 1));

        let sup = Arc::new(h.supervisor());
        let run = h.spawn(sup.clone());
        settle().await;
        tokio::time::advance(Duration::from_secs(1)).await;
        settle().await;

        assert_eq!(sup.incarnations_started(), 2, "exactly one successor");
        assert_eq!(
            h.health(),
            RoutingHealth::Healthy { incarnation: 2 },
            "the successor is live and healthy"
        );
        assert_eq!(
            h.seen.lock().as_slice(),
            &[(2, DirtyCapabilities::RebuildAll)],
            "the successor recaptured completely; incarnation 1 applied nothing"
        );

        h.stop();
        run.await.expect("supervisor joins");
    }

    /// A CLOSED watch means the scoped-discovery source is gone. That is terminal
    /// and NOT a fault: the supervisor stops fenced instead of restarting against a
    /// source that no longer exists, which would spin.
    #[tokio::test(start_paused = true)]
    async fn a_closed_watch_stops_the_supervisor_without_restarting() {
        let mut h = harness();
        let sup = Arc::new(h.supervisor());
        let run = h.spawn(sup.clone());
        settle().await;

        // Close the channel: drop the sender and every receiver.
        let (tx, rx) = tokio::sync::watch::channel(0u64);
        let dead_tx = std::mem::replace(&mut h.tx, tx);
        let dead_rx = std::mem::replace(&mut h.rx, rx);
        drop(dead_tx);
        drop(dead_rx);

        let joined = tokio::time::timeout(Duration::from_secs(5), run).await;
        assert!(joined.is_ok(), "the supervisor must stop, not spin");
        assert_eq!(sup.incarnations_started(), 1, "no restart on a gone source");
        assert_eq!(h.health(), RoutingHealth::Fenced);
    }

    /// Shutdown while the actor is parked stops the supervisor and fences.
    #[tokio::test(start_paused = true)]
    async fn shutdown_while_parked_stops_and_fences() {
        let h = harness();
        let sup = Arc::new(h.supervisor());
        let run = h.spawn(sup.clone());
        settle().await;

        h.stop();
        let joined = tokio::time::timeout(Duration::from_secs(5), run).await;
        assert!(joined.is_ok(), "a parked actor still observes shutdown");
        assert_eq!(sup.incarnations_started(), 1);
        assert_eq!(h.health(), RoutingHealth::Fenced);
    }

    /// Shutdown landing DURING restart backoff spawns no replacement — a panic
    /// racing shutdown must not leave an actor outliving the node.
    #[tokio::test(start_paused = true)]
    async fn shutdown_during_backoff_spawns_no_replacement() {
        let h = harness();
        *h.hooks.panic_incarnation.lock() = Some(Arc::new(|id| id == 1));

        let sup = Arc::new(h.supervisor());
        let run = h.spawn(sup.clone());
        settle().await;
        h.stop();

        let joined = tokio::time::timeout(Duration::from_secs(5), run).await;
        assert!(joined.is_ok(), "backoff is interruptible by shutdown");
        assert_eq!(
            sup.incarnations_started(),
            1,
            "no replacement was spawned after shutdown"
        );
        assert_eq!(h.health(), RoutingHealth::Fenced);
    }

    /// A deterministic fault exhausts the bounded restart budget and lands in the
    /// terminal crash-loop state: permanently fenced, no further incarnations, and
    /// no tight retry loop.
    #[tokio::test(start_paused = true)]
    async fn a_deterministic_fault_exhausts_the_restart_budget_and_stays_fenced() {
        let h = harness();
        *h.hooks.panic_incarnation.lock() = Some(Arc::new(|_| true));

        let sup = Arc::new(h.supervisor());
        let run = h.spawn(sup.clone());

        let joined = tokio::time::timeout(Duration::from_secs(600), run).await;
        assert!(
            joined.is_ok(),
            "the supervisor gives up rather than spinning"
        );
        assert_eq!(
            sup.incarnations_started() as usize,
            MAX_RESTARTS_IN_WINDOW + 1,
            "exactly the budgeted attempts, then stop"
        );
        assert_eq!(
            h.health(),
            RoutingHealth::Fenced,
            "crash-loop exhaustion is fail-closed"
        );
    }

    /// Fencing is SYNCHRONOUS with an abnormal exit, not deferred to the
    /// supervisor's observation of the join handle. An incarnation that dies
    /// mid-cycle leaves health `Fenced` immediately — including throughout the
    /// restart backoff, BEFORE any successor exists — so a warmed call in that
    /// window can never find a usable-looking route from a dead run.
    ///
    /// Load-bearing: the incarnation reaches `Rebuilding` before it dies, so
    /// without the actor-side fence health would still read `Rebuilding{1}` here
    /// (the supervisor's own fence does not run on the fault path — it goes to
    /// backoff).
    #[tokio::test(start_paused = true)]
    async fn an_abnormal_exit_fences_synchronously_during_backoff() {
        let h = harness();
        // Die mid-cycle, after the drain and after publishing Rebuilding.
        *h.hooks.drained.lock() = Some(Arc::new(|id, _| {
            if id == 1 {
                panic!("injected mid-cycle fault");
            }
        }));
        // Freeze the successor at its start so we observe the between-incarnations
        // window rather than a recovered state.
        *h.hooks.panic_incarnation.lock() = Some(Arc::new(|id| {
            if id == 2 {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            false
        }));

        let sup = Arc::new(h.supervisor());
        let run = h.spawn(sup.clone());
        settle().await;

        assert_eq!(
            h.health(),
            RoutingHealth::Fenced,
            "a dead incarnation fences immediately, before any successor"
        );
        assert_eq!(
            sup.incarnations_started(),
            1,
            "still in backoff — no successor yet"
        );

        h.stop();
        let _ = tokio::time::timeout(Duration::from_secs(5), run).await;
    }

    /// `allows` is the fence contract the warmed-call path will consult: only the
    /// LIVE incarnation's routes are usable.
    #[test]
    fn only_the_live_incarnations_routes_are_usable() {
        assert!(RoutingHealth::Healthy { incarnation: 7 }.allows(7));
        assert!(
            !RoutingHealth::Healthy { incarnation: 8 }.allows(7),
            "a dead incarnation's routes are never trusted after a successor starts"
        );
        assert!(!RoutingHealth::Rebuilding { incarnation: 7 }.allows(7));
        assert!(!RoutingHealth::Fenced.allows(7));
    }
}
