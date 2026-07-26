//! OLB-2B: the node-owned private-discovery routing consumer — supervisor,
//! incarnation fencing, and restart policy.
//!
//! This module is the sole consumer of the GLOBAL private-discovery change stream
//! (OLB mints global only; the owner stream stays unclaimed for the provider-free
//! leader track). It lives beside `org_scoped_store` rather than inside it, which
//! is exactly the arrangement OLB-2B-E1's `pub(crate) drain()` exists to permit:
//! the capability is unforgeable outside its home module, yet consumable here.
//!
//! # Faults are explicit, because production aborts on panic
//!
//! `[profile.release]` sets `panic = "abort"`. A real panic in a release build
//! therefore kills the process: tokio returns no panic `JoinError`, no `Drop`
//! guard runs, and no in-process supervisor restarts anything. Supervision here is
//! consequently built on EXPLICIT [`ActorFault`] returns, which return normally
//! through the fence guard, resolve the inline incarnation future, back off, and
//! restart (Kyra OLB-2B-E2).
//!
//! A true panic remains process-fatal by design, and that is safe for route
//! currentness: no in-process caller survives the abort, and the external restart
//! constructs a fresh actor whose mint forces a complete `RebuildAll` recapture.

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
/// Health is a coarse, GLOBAL signal. It is never sufficient on its own: every
/// retained artifact must additionally carry its actor incarnation, its slot
/// incarnation, and the private-discovery source generation it was built from.
/// Health says "this actor is in a usable posture", not "this route is current".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RoutingHealth {
    /// Routes built by this incarnation are usable.
    Healthy { incarnation: u64 },
    /// A COMPLETE recapture is in progress: no route from this incarnation is
    /// trustworthy yet. Entered only for a full rebuild, never for ordinary
    /// per-capability movement.
    Rebuilding { incarnation: u64 },
    /// NO cached route is usable — before the first incarnation, between
    /// incarnations, and permanently after crash-loop exhaustion or an abnormal
    /// terminal failure. A call must take the fresh current-authority cold path,
    /// or fail locally before proof/send.
    Fenced,
}

impl RoutingHealth {
    /// Whether a cached route stamped by `incarnation` may be used. Fenced and
    /// mid-recapture states are unusable, and so is a route from any incarnation
    /// other than the live one — which is what stops detached work from a dead run
    /// being trusted after a successor starts.
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

/// A RECOVERABLE actor failure, reported explicitly rather than by panicking —
/// see the module docs on `panic = "abort"`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ActorFault {
    /// Operator-facing cause.
    pub reason: String,
}

/// What one application attempt achieved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ApplyOutcome {
    /// A COMPLETE conditional installation succeeded against the named source
    /// generation: every slot the request covered is now current. Only this
    /// outcome may advance health.
    Current { source_generation: u64 },
    /// One bounded quantum completed, but work the request covered REMAINS
    /// (Kyra OLB-2B-E3b). A full recapture spanning several quanta reports this
    /// until the last one, so health stays in `Rebuilding` rather than going
    /// `Healthy` with slots still outstanding. The consumer has re-queued the
    /// remainder authoritatively and marked, so the actor is woken again.
    Progress { source_generation: u64 },
    /// The source moved while this attempt was building, so its result was
    /// discarded. Health must NOT advance from an obsolete attempt; the actor
    /// stays in recapture and re-attempts on the next wake.
    ///
    /// CONTRACT: reporting `Superseded` asserts that a corresponding wake is
    /// pending or eventual — the source movement that invalidated the attempt must
    /// itself have advanced the change watch. The actor parks after every
    /// application (never spins), so an implementation that returns `Superseded`
    /// with no accompanying wake strands its own recapture. OLB-2B-E3 will need an
    /// internal registry-work wake in the actor's wait set for demand insertion and
    /// slot-incarnation movement, neither of which advances the private-discovery
    /// watch.
    Superseded,
    /// A recoverable failure: the actor exits through the synchronous fence and
    /// the supervisor applies its restart policy.
    ///
    /// Never constructed by the bounded routing registry, whose every refusal is
    /// deterministic and non-fatal — but the actor must still handle it, because
    /// the `DirtyApply` contract admits implementors that CAN fail recoverably.
    /// Deleting it would delete the restart policy's only trigger.
    #[allow(dead_code)]
    Fault(ActorFault),
}

/// Pending REGISTRY work, independent of private-discovery movement (OLB-2B-E3).
///
/// Private-discovery movement is not sufficient to wake everything the actor must
/// reconcile: first demand insertion, slot-incarnation movement, last-reference
/// retirement, and other retained-work invalidation change what must be built
/// WITHOUT advancing the private-discovery watch. This is the second source in the
/// actor's wait set.
///
/// `pending` is AUTHORITATIVE and the notification is only a hint. That split is
/// what makes it correct under coalescing and under a wake that arrives BEFORE the
/// actor parks: many marks collapse into one flag, and the actor consumes the flag
/// rather than trusting that it saw a notification.
#[derive(Default)]
pub(crate) struct RegistryWork {
    pending: AtomicBool,
    notify: Notify,
}

impl RegistryWork {
    /// Record that reconciliation is owed and hint the actor. Coalescing is the
    /// point: a burst of demand insertions is one pending flag.
    pub(crate) fn mark(&self) {
        self.pending.store(true, Ordering::Release);
        self.notify.notify_waiters();
    }

    /// Consume the pending flag, reporting whether work was owed.
    fn take(&self) -> bool {
        self.pending.swap(false, Ordering::AcqRel)
    }
}

/// What woke this application attempt. Carries BOTH trigger domains, so a demand
/// wake with a clean source batch still reconciles rather than being skipped, and
/// first demand does not have to masquerade as a node-wide `RebuildAll`
/// (Kyra OLB-2B-E3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ApplyRequest {
    /// The drained private-discovery delta. May be `Clean` when only registry work
    /// is owed.
    pub batch: PrivateDiscoveryChangeBatch,
    /// Whether registry work was pending for this pass.
    pub registry_work: bool,
}

/// Applies one reconciliation pass. Implemented by the bounded routing registry in
/// OLB-2B-E3.
///
/// Takes `&self`, NOT `&mut self`, and the shared handle carries no outer mutex
/// (Kyra OLB-2B-E2): the implementation owns its own bounded synchronization, so
/// it can snapshot retained slot identities, RELEASE its registry lock, query the
/// scoped source, decode/build/sort entirely off-lock, then reacquire and
/// conditionally install. An outer mutex spanning that work would hold a lock
/// across scoped-state access and heavy reconstruction.
pub(crate) trait DirtyApply: Send + Sync + 'static {
    /// Apply one reconciliation pass. Must not hold any lock across scoped-state
    /// access, decoding, sorting, projection, or reconciliation.
    fn apply(&self, incarnation: u64, request: ApplyRequest) -> ApplyOutcome;

    /// This incarnation is now the live one. Called once at incarnation start,
    /// BEFORE any application, so the consumer can bind its work authority to the
    /// actual actor lifecycle rather than to a high-water counter (Kyra
    /// OLB-2B-E3b).
    fn activate_incarnation(&self, _incarnation: u64) {}

    /// This incarnation is over. Called on EVERY exit path — clean, fault, or
    /// cancellation — from the actor's own fence guard, so a dead actor loses its
    /// authority synchronously.
    fn deactivate_incarnation(&self, _incarnation: u64) {}
}

/// The shared applier — lock-free at this seam.
pub(crate) type SharedApply = Arc<dyn DirtyApply>;

/// Why an actor incarnation stopped.
#[derive(Debug, PartialEq, Eq)]
enum ActorExit {
    /// The node is shutting down — do not restart.
    Shutdown,
    /// The change watch closed WITHOUT a shutdown in progress.
    ///
    /// This is abnormal, not teardown. The authoritative state is still alive —
    /// the actor still holds `Arc<Mutex<ScopedDiscoveryState>>` — so all this
    /// proves is that the publication SENDER disappeared. Invalidations have
    /// stopped while discovery can still change, which is a silent-staleness
    /// hazard and must be loud.
    SourceClosedUnexpected,
    /// A recoverable failure; the supervisor applies its restart policy.
    Fault(ActorFault),
}

/// Fences routing health when an incarnation ends, on EVERY exit path — an
/// ordinary return, a fault return, or (where the profile unwinds) a panic.
///
/// Fencing lives in the actor's own stack rather than in the supervisor's
/// observation of the join handle, so it is SYNCHRONOUS with the incarnation's
/// death: there is no window in which the actor is finished but its routes still
/// look usable (Kyra OLB-2B-E2).
struct IncarnationFence {
    health: SharedRoutingHealth,
    /// Revoked in the same guard, so a dead actor loses its registry work
    /// authority at exactly the moment its routes stop being trusted.
    apply: SharedApply,
    incarnation: u64,
}

impl Drop for IncarnationFence {
    fn drop(&mut self) {
        self.health.store(Arc::new(RoutingHealth::Fenced));
        self.apply.deactivate_incarnation(self.incarnation);
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

/// Observation points for the deterministic actor witnesses.
///
/// Gated `any(test, fixtures)`, NOT `fixtures` alone. The supervisor witnesses
/// need these hooks, and CI's gating `--lib` job does not enable `fixtures` — so
/// a fixtures-only gate silently drops every one of them out of the job that
/// gates in-source units, while leaving `#[cfg(test)]` seams they use looking
/// dead (Kyra OLB-2B-E3c).
#[cfg(any(test, feature = "fixtures"))]
#[derive(Default)]
pub(crate) struct ActorHooks {
    /// EVERY health publication, in order.
    ///
    /// A final `Fenced` cannot distinguish "never published Healthy after
    /// shutdown" from "published Healthy and then fenced a microsecond later" —
    /// the states are identical afterwards. Recording the transitions is the only
    /// way to witness the ABSENCE of a transient publication (Kyra OLB-2B-E3c).
    pub(crate) health_transitions: parking_lot::Mutex<Vec<RoutingHealth>>,
    /// Fired after each drain, with the batch the incarnation observed.
    #[allow(clippy::type_complexity)]
    pub(crate) drained:
        parking_lot::Mutex<Option<Arc<dyn Fn(u64, &PrivateDiscoveryChangeBatch) + Send + Sync>>>,
}

#[cfg(any(test, feature = "fixtures"))]
impl ActorHooks {
    fn note_health(&self, state: &RoutingHealth) {
        self.health_transitions.lock().push(*state);
    }

    fn fire_drained(&self, incarnation: u64, batch: &PrivateDiscoveryChangeBatch) {
        if let Some(hook) = self.drained.lock().clone() {
            hook(incarnation, batch);
        }
    }
}

/// Everything one incarnation needs. Owned, so the whole set moves into the
/// incarnation FUTURE and drops when that future resolves or is dropped — which is
/// what makes the future resolving sufficient proof that the drain was released,
/// and what makes cancelling the supervisor release it too.
struct Incarnation {
    drain: PrivateDiscoveryDrain,
    changed: tokio::sync::watch::Receiver<u64>,
    health: SharedRoutingHealth,
    id: u64,
    apply: SharedApply,
    work: Arc<RegistryWork>,
    shutdown: Arc<AtomicBool>,
    shutdown_notify: Arc<Notify>,
    #[cfg(any(test, feature = "fixtures"))]
    hooks: Arc<ActorHooks>,
}

/// Drive one actor incarnation: drain the global change stream and apply it.
///
/// The wake protocol mirrors the exact-expiry timer's, for the same reason: a
/// `Notified` captures the notify-waiters epoch when it is CONSTRUCTED, so
/// checking the shutdown flag before constructing it loses a shutdown landing in
/// the gap and parks the task forever (Kyra OLB-2A.3.2). Constructed and enabled
/// BEFORE the flag load, re-armed every iteration.
///
/// Health transitions are deliberately NARROW:
///
/// - `RebuildAll` — a complete recapture, so publish global `Rebuilding` and
///   advance to `Healthy` only once a current installation succeeded;
/// - `Caps(set)` — ordinary movement, so global health is left ALONE. Fencing
///   every warmed route because one unrelated capability moved would make routine
///   churn globally disruptive; invalidating the matching retained slots is the
///   registry's job;
/// - `Clean` — no transition at all;
/// - `Superseded` — never advances health, because the attempt was obsolete.
async fn run_incarnation(mut it: Incarnation) -> ActorExit {
    // Claim work authority, then fence on every exit path — the guard revokes it.
    it.apply.activate_incarnation(it.id);
    let _fence = IncarnationFence {
        health: it.health.clone(),
        apply: it.apply.clone(),
        incarnation: it.id,
    };

    // Set when a full recapture was superseded, so a subsequent WOKEN pass still
    // completes one rather than leaving health stuck in `Rebuilding`.
    let mut owed_recapture = false;

    loop {
        let shutdown_signal = it.shutdown_notify.notified();
        tokio::pin!(shutdown_signal);
        shutdown_signal.as_mut().enable();
        if it.shutdown.load(Ordering::Acquire) {
            return ActorExit::Shutdown;
        }

        // Arm the registry-work wake BEFORE consuming its flag, on the same
        // discipline as shutdown: a `mark` landing in the gap is then either
        // observed by the `take` below or leaves this signal ready.
        let work_signal = it.work.notify.notified();
        tokio::pin!(work_signal);
        work_signal.as_mut().enable();
        let registry_work = it.work.take();

        // Mark the current version seen BEFORE draining, so a mutation landing
        // during the drain or the apply is never missed — it either lands in this
        // batch or leaves `changed()` ready for the trailing pass.
        it.changed.borrow_and_update();

        let mut batch = it.drain.drain();
        #[cfg(any(test, feature = "fixtures"))]
        it.hooks.fire_drained(it.id, &batch);

        // An owed complete recapture SUBSUMES whatever this pass drained, whether
        // that is `Clean` or a `Caps` delta (Kyra OLB-2B-E2).
        //
        // Promoting only on `Clean` loses the recapture in the normal case: an
        // attempt is superseded precisely BECAUSE the source moved during it, and
        // that movement dirties capabilities, so the waking batch is `Caps(..)`.
        // Applying it as `Caps` would report `Current`, leave the recapture still
        // owed, and — with no further movement to wake anything — strand the actor
        // in `Rebuilding` indefinitely.
        if owed_recapture {
            batch.dirty = DirtyCapabilities::RebuildAll;
        }

        let full = matches!(batch.dirty, DirtyCapabilities::RebuildAll);
        // A pass is quiet only when NEITHER trigger domain has anything owed. A
        // registry-work wake with a clean source batch must still reconcile —
        // first demand and slot lifecycle move nothing in private discovery.
        let quiet = matches!(batch.dirty, DirtyCapabilities::Clean) && !registry_work;

        if !quiet {
            if full {
                // A complete recapture: nothing from this incarnation is
                // trustworthy until it finishes.
                let state = RoutingHealth::Rebuilding { incarnation: it.id };
                #[cfg(any(test, feature = "fixtures"))]
                it.hooks.note_health(&state);
                it.health.store(Arc::new(state));
            }
            match it.apply.apply(
                it.id,
                ApplyRequest {
                    batch,
                    registry_work,
                },
            ) {
                ApplyOutcome::Current { .. } => {
                    // Recheck shutdown BEFORE publishing. `apply` is synchronous
                    // and can be long; a shutdown landing inside it would
                    // otherwise be followed by this incarnation resurrecting
                    // `Healthy` over a node that is tearing down, and the fence
                    // only lands once the loop reaches its next park (Kyra
                    // OLB-2B-E3c). Health must never move forward after shutdown
                    // has been observed, on the explicit path or under Drop.
                    if it.shutdown.load(Ordering::Acquire) {
                        return ActorExit::Shutdown;
                    }
                    if full {
                        let state = RoutingHealth::Healthy { incarnation: it.id };
                        #[cfg(any(test, feature = "fixtures"))]
                        it.hooks.note_health(&state);
                        it.health.store(Arc::new(state));
                        owed_recapture = false;
                    }
                    // `Caps` leaves global health untouched by design.
                }
                ApplyOutcome::Progress { .. } => {
                    // A bounded quantum finished but the recapture epoch is still
                    // open. Health must NOT advance — publishing Healthy here would
                    // advertise a set in which later slots were never rebuilt by
                    // this incarnation (Kyra OLB-2B-E3b).
                    owed_recapture = owed_recapture || full;
                }
                ApplyOutcome::Superseded => {
                    // Obsolete result: publish nothing, and make sure a recapture
                    // still completes. Per the `Superseded` contract the wake that
                    // invalidated this attempt — source movement OR registry work —
                    // is pending or eventual, so the retry is driven by real
                    // movement rather than by spinning.
                    owed_recapture = owed_recapture || full;
                }
                ApplyOutcome::Fault(fault) => return ActorExit::Fault(fault),
            }
        }

        if !quiet {
            // A REAL cooperative yield between quanta (Kyra OLB-2B-E3b).
            // `DirtyApply::apply` is synchronous and bounded, so a hot demand
            // family replenishes its pending work and marks again; the select
            // below would then be immediately ready every iteration. An
            // already-ready `Notify` inside `select!` is not by itself a
            // guaranteed scheduler yield, so without this a continuously-ready
            // quantum chain could starve shutdown, starve source movement, and let
            // one family monopolize the actor.
            tokio::task::yield_now().await;
        }

        // ALWAYS park here, including after an application. The trailing pass is
        // preserved without looping: `borrow_and_update` ran BEFORE the drain, so
        // movement during the drain or the apply leaves `changed()` already ready
        // and this returns immediately.
        //
        // Looping directly instead would busy-spin whenever an applier reports
        // `Superseded` persistently — each pass would synthesize another
        // `RebuildAll` and never yield. Parking makes the retry rate the rate of
        // actual source movement, which is exactly what `Superseded` reports.
        tokio::select! {
            _ = &mut work_signal => {}
            changed_result = it.changed.changed() => {
                if changed_result.is_err() {
                    // The sender is gone. Only teardown if a shutdown is actually
                    // in progress; otherwise this is abnormal — the authoritative
                    // state is still alive and can still change while nothing
                    // invalidates.
                    return if it.shutdown.load(Ordering::Acquire) {
                        ActorExit::Shutdown
                    } else {
                        ActorExit::SourceClosedUnexpected
                    };
                }
            }
            _ = &mut shutdown_signal => return ActorExit::Shutdown,
        }
    }
}

/// Observable supervisor counters, shared with the node so metrics survive the
/// supervisor being consumed by [`RoutingSupervisor::run`].
#[derive(Default)]
pub(crate) struct RoutingMetrics {
    incarnations: AtomicU64,
    source_closed_unexpected: AtomicU64,
}

impl RoutingMetrics {
    /// How many incarnations have been started.
    pub(crate) fn incarnations_started(&self) -> u64 {
        self.incarnations.load(Ordering::Acquire)
    }

    /// How many times the change source closed abnormally, with no shutdown in
    /// progress.
    pub(crate) fn source_closed_unexpected(&self) -> u64 {
        self.source_closed_unexpected.load(Ordering::Acquire)
    }

    /// Allocate the next incarnation id, or `None` on overflow. Checked, because
    /// reusing an identifier would let a stale artifact pass the fence test.
    fn next_incarnation(&self) -> Option<u64> {
        let mut current = self.incarnations.load(Ordering::Acquire);
        loop {
            let next = current.checked_add(1)?;
            match self.incarnations.compare_exchange_weak(
                current,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Some(next),
                Err(actual) => current = actual,
            }
        }
    }

    /// Test seam: preset the counter to exercise overflow.
    #[cfg(test)]
    fn set_incarnations_for_test(&self, value: u64) {
        self.incarnations.store(value, Ordering::Release);
    }
}

/// The node-owned supervisor: the ONLY mint authority for the global
/// private-discovery stream, and the only thing that starts an incarnation.
///
/// NOT `Clone`, and [`Self::run`] CONSUMES it, so duplicate execution of one
/// supervisor is unrepresentable rather than merely refused — a re-entrant `run`
/// could otherwise bypass backoff and the terminal crash-loop posture (Kyra
/// OLB-2B-E2). Recovery from the terminal state is a node restart, which
/// constructs a new supervisor through the single audited node-owned path.
pub(crate) struct RoutingSupervisor {
    mint: PrivateDiscoveryDrains,
    health: SharedRoutingHealth,
    metrics: Arc<RoutingMetrics>,
}

impl RoutingSupervisor {
    pub(crate) fn new(
        mint: PrivateDiscoveryDrains,
        health: SharedRoutingHealth,
        metrics: Arc<RoutingMetrics>,
    ) -> Self {
        Self {
            mint,
            health,
            metrics,
        }
    }

    /// Run the supervision loop until shutdown, an abnormal terminal failure, or
    /// crash-loop exhaustion.
    ///
    /// Restart discipline, in order:
    ///
    /// 1. mint the drain — refusing LOUDLY and fencing if the stream is held or
    ///    stranded, rather than running a drainless actor;
    /// 2. run ONE incarnation INLINE, so the supervisor future structurally owns
    ///    it;
    /// 3. on an explicit [`ActorFault`], apply capped backoff within a bounded
    ///    rolling window;
    /// 4. on exhaustion, stay `Fenced` permanently rather than spinning.
    ///
    /// # Why inline rather than a spawned task
    ///
    /// A `tokio::spawn`ed incarnation is DETACHED when its `JoinHandle` drops, so
    /// cancelling or dropping the supervisor would leave the actor alive — still
    /// holding the exclusive drain, still applying, still publishing health, and
    /// outliving the node-owned supervisor (Kyra OLB-2B-E2). Awaiting the handle
    /// only covers cancellation of the CHILD, never of the parent.
    ///
    /// Running inline makes ownership structural:
    ///
    /// ```text
    /// supervisor future owns the incarnation future
    ///   → which owns the drain and the fence guard
    ///   → so dropping/cancelling the supervisor drops the incarnation
    ///   → the fence runs and the drain releases its lease
    ///   → no detached child survives
    /// ```
    ///
    /// It also makes the join handle unnecessary for predecessor completion:
    /// `run_incarnation` resolving IS the proof, after which its locals drop, the
    /// fence fires, and the lease frees before any successor is minted. Explicit
    /// [`ActorFault`] supervision is what removes the original reason to spawn —
    /// there is no panic to isolate that a release build would not turn into an
    /// abort anyway.
    ///
    /// The shutdown flag is checked before AND after every mint and interrupts the
    /// backoff, so a fault racing shutdown cannot spawn a replacement.
    pub(crate) async fn run(
        self,
        changed: tokio::sync::watch::Receiver<u64>,
        apply: SharedApply,
        work: Arc<RegistryWork>,
        shutdown: Arc<AtomicBool>,
        shutdown_notify: Arc<Notify>,
        #[cfg(any(test, feature = "fixtures"))] hooks: Arc<ActorHooks>,
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

            let Some(id) = self.metrics.next_incarnation() else {
                drop(drain);
                self.fence();
                tracing::error!(
                    "org routing: incarnation counter exhausted; routing stays fenced \
                     rather than reusing an identifier"
                );
                return;
            };

            // Re-checked AFTER the mint: the flag may have been set while claiming,
            // and a replacement must not outlive the node.
            if shutdown.load(Ordering::Acquire) {
                drop(drain);
                self.fence();
                return;
            }

            // INLINE: the supervisor future owns this one. Resolving is itself the
            // proof the predecessor finished — its locals drop here, firing the
            // fence and releasing the lease before any successor is minted.
            let exit = run_incarnation(Incarnation {
                drain,
                changed: changed.clone(),
                health: self.health.clone(),
                id,
                apply: apply.clone(),
                work: work.clone(),
                shutdown: shutdown.clone(),
                shutdown_notify: shutdown_notify.clone(),
                #[cfg(any(test, feature = "fixtures"))]
                hooks: hooks.clone(),
            })
            .await;

            let fault = match exit {
                ActorExit::Shutdown => {
                    self.fence();
                    return;
                }
                ActorExit::SourceClosedUnexpected => {
                    // Abnormal and terminal: cloning the same closed receiver
                    // cannot recover, so this consumes no restart budget — but it
                    // is NOT normal teardown and must be loud.
                    self.metrics
                        .source_closed_unexpected
                        .fetch_add(1, Ordering::AcqRel);
                    self.fence();
                    tracing::error!(
                        incarnation = id,
                        "org routing: the private-discovery change source closed with no \
                         shutdown in progress; invalidations have stopped while discovery \
                         can still change. Routing stays fenced."
                    );
                    return;
                }
                ActorExit::Fault(fault) => fault,
            };

            let now = tokio::time::Instant::now();
            faults.retain(|at| now.duration_since(*at) < RESTART_WINDOW);
            faults.push(now);
            if faults.len() > MAX_RESTARTS_IN_WINDOW {
                // Crash loop: a deterministic fault. Stay fenced rather than retry;
                // recovery needs operator action or a node restart.
                self.fence();
                tracing::error!(
                    faults = faults.len(),
                    reason = %fault.reason,
                    "org routing: actor crash-loop budget exhausted; routing stays \
                     fenced until the node is restarted"
                );
                return;
            }
            let shift = u32::try_from(faults.len()).unwrap_or(u32::MAX).min(16);
            let backoff =
                RESTART_BACKOFF_CAP.min(RESTART_BACKOFF_BASE.saturating_mul(1u32 << (shift - 1)));
            tracing::warn!(
                incarnation = id,
                ?backoff,
                reason = %fault.reason,
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

    fn fence(&self) {
        self.health.store(Arc::new(RoutingHealth::Fenced));
    }
}

/// OLB-2B-E2 witnesses.
///
/// Restart/crash-loop claims are driven by EXPLICIT [`ActorFault`], never by
/// panics — release builds abort, so a panic-based witness would prove nothing
/// about production (Kyra OLB-2B-E2).
#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::net::behavior::capability::CapabilitySet;
    use crate::adapter::net::behavior::org::OrgId;
    use crate::adapter::net::behavior::org_scoped_ingest::{
        CapabilityAudienceScope, PreparedScopedCapability, VerifiedScopedCapability,
    };
    use crate::adapter::net::behavior::org_scoped_store::ScopedDiscoveryState;
    use crate::adapter::net::identity::EntityId;

    /// `(incarnation, source delta, registry-work flag)` per application.
    type Applied = Arc<parking_lot::Mutex<Vec<(u64, DirtyCapabilities, bool)>>>;
    type Decide = Box<dyn Fn(u64, &ApplyRequest) -> ApplyOutcome + Send + Sync>;

    /// Records every application and returns a scripted outcome. Note the `&self`
    /// seam: no outer mutex spans the application.
    struct ScriptedApply {
        seen: Applied,
        decide: Decide,
    }

    impl DirtyApply for ScriptedApply {
        fn apply(&self, incarnation: u64, request: ApplyRequest) -> ApplyOutcome {
            self.seen.lock().push((
                incarnation,
                request.batch.dirty.clone(),
                request.registry_work,
            ));
            (self.decide)(incarnation, &request)
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
        metrics: Arc<RoutingMetrics>,
        seen: Applied,
        work: Arc<RegistryWork>,
        shutdown: Arc<AtomicBool>,
        notify: Arc<Notify>,
        hooks: Arc<ActorHooks>,
        tx: tokio::sync::watch::Sender<u64>,
        rx: tokio::sync::watch::Receiver<u64>,
    }

    fn harness() -> Harness {
        let state = Arc::new(parking_lot::Mutex::new(ScopedDiscoveryState::new()));
        state.lock().ingest(owner_record(3), 0);
        let (tx, rx) = tokio::sync::watch::channel(0u64);
        Harness {
            state,
            health: new_routing_health(),
            metrics: Arc::default(),
            seen: Arc::default(),
            work: Arc::default(),
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
                self.metrics.clone(),
            )
        }

        fn applier(&self, decide: Decide) -> SharedApply {
            Arc::new(ScriptedApply {
                seen: self.seen.clone(),
                decide,
            })
        }

        /// Always-current applier.
        fn ok_applier(&self) -> SharedApply {
            self.applier(Box::new(|_, r| ApplyOutcome::Current {
                source_generation: r.batch.generation,
            }))
        }

        fn spawn(&self, sup: RoutingSupervisor, apply: SharedApply) -> tokio::task::JoinHandle<()> {
            let (rx, work, shutdown, notify, hooks) = (
                self.rx.clone(),
                self.work.clone(),
                self.shutdown.clone(),
                self.notify.clone(),
                self.hooks.clone(),
            );
            tokio::spawn(async move { sup.run(rx, apply, work, shutdown, notify, hooks).await })
        }

        fn stop(&self) {
            self.shutdown.store(true, Ordering::Release);
            self.notify.notify_waiters();
        }

        fn health(&self) -> RoutingHealth {
            **self.health.load()
        }

        /// Whether the global drain lease is currently free.
        fn lease_free(&self) -> bool {
            PrivateDiscoveryDrains::new(self.state.clone())
                .mint(PrivateDiscoveryStream::Global)
                .is_some()
        }
    }

    async fn settle() {
        for _ in 0..32 {
            tokio::task::yield_now().await;
        }
    }

    /// The supervisor is the SOLE mint authority: if the global stream is already
    /// held, it fences and starts no actor, rather than running a drainless one.
    #[tokio::test(start_paused = true)]
    async fn a_held_stream_fences_instead_of_starting_a_drainless_actor() {
        let h = harness();
        // Start NON-fenced so the fence below is load-bearing.
        h.health
            .store(Arc::new(RoutingHealth::Healthy { incarnation: 99 }));
        let squatter = PrivateDiscoveryDrains::new(h.state.clone());
        let _held = squatter
            .mint(PrivateDiscoveryStream::Global)
            .expect("squatter holds it");

        h.supervisor()
            .run(
                h.rx.clone(),
                h.ok_applier(),
                h.work.clone(),
                h.shutdown.clone(),
                h.notify.clone(),
                h.hooks.clone(),
            )
            .await;

        assert_eq!(h.metrics.incarnations_started(), 0, "no actor was started");
        assert_eq!(h.health(), RoutingHealth::Fenced);
    }

    /// Shutdown landing INSIDE a synchronous apply is never followed by a
    /// `Healthy` publication.
    ///
    /// `apply` is synchronous and can be long, so a shutdown can land in the
    /// middle of one that goes on to report `Current`. Publishing `Healthy` from
    /// that pass resurrects health over a node that is tearing down, and the
    /// fence only lands once the loop reaches its next park.
    ///
    /// Asserted on the TRANSITION LOG, not the final state: a final `Fenced` is
    /// identical whether or not a transient `Healthy` was published in between,
    /// so only the recorded sequence can witness its absence (Kyra OLB-2B-E3c).
    #[tokio::test(start_paused = true)]
    async fn shutdown_inside_apply_is_never_followed_by_healthy() {
        let h = harness();
        let shutdown = h.shutdown.clone();
        let notify = h.notify.clone();
        // The mint drives a full RebuildAll; shutdown lands inside that apply.
        let applier = h.applier(Box::new(move |_, r| {
            shutdown.store(true, Ordering::Release);
            notify.notify_waiters();
            ApplyOutcome::Current {
                source_generation: r.batch.generation,
            }
        }));
        let run = h.spawn(h.supervisor(), applier);
        settle().await;
        run.await.expect("supervisor joins");

        let transitions = h.hooks.health_transitions.lock().clone();
        assert!(
            transitions.contains(&RoutingHealth::Rebuilding { incarnation: 1 }),
            "the recapture still announced itself: {transitions:?}"
        );
        assert!(
            !transitions
                .iter()
                .any(|state| matches!(state, RoutingHealth::Healthy { .. })),
            "Healthy must NEVER be published after shutdown was observed:              {transitions:?}"
        );
        assert_eq!(h.health(), RoutingHealth::Fenced, "and the exit fences");
    }

    /// A full recapture publishes `Rebuilding` then `Healthy` only after a CURRENT
    /// installation.
    #[tokio::test(start_paused = true)]
    async fn a_recapture_reports_healthy_only_after_current_installation() {
        let h = harness();
        let run = h.spawn(h.supervisor(), h.ok_applier());
        settle().await;

        assert_eq!(h.health(), RoutingHealth::Healthy { incarnation: 1 });
        assert_eq!(
            h.seen.lock().as_slice(),
            &[(1, DirtyCapabilities::RebuildAll, false)],
            "the first batch is the mint's complete recapture"
        );

        h.stop();
        run.await.expect("supervisor joins");
        assert_eq!(h.health(), RoutingHealth::Fenced, "exit fences");
    }

    /// A SUPERSEDED attempt never publishes `Healthy` — the reconstruction was
    /// obsolete, so health must not advance from it.
    #[tokio::test(start_paused = true)]
    async fn a_superseded_recapture_never_publishes_healthy() {
        let h = harness();
        let apply = h.applier(Box::new(|_, _| ApplyOutcome::Superseded));
        let run = h.spawn(h.supervisor(), apply);
        settle().await;

        assert_eq!(
            h.health(),
            RoutingHealth::Rebuilding { incarnation: 1 },
            "an obsolete attempt leaves the actor in recapture, never Healthy"
        );

        h.stop();
        let _ = tokio::time::timeout(Duration::from_secs(5), run).await;
    }

    /// An owed recapture SURVIVES the `Caps` wake that superseded it.
    ///
    /// This is the realistic supersede path, not a contrived one: an attempt is
    /// superseded precisely BECAUSE the source moved during it, and that movement
    /// dirties capabilities — so the batch that wakes the actor is `Caps(..)`, not
    /// `Clean`. Promoting only on `Clean` would apply that delta, report `Current`,
    /// leave the recapture owed, and strand the actor in `Rebuilding` forever with
    /// nothing left to wake it.
    #[tokio::test(start_paused = true)]
    async fn an_owed_recapture_survives_the_caps_wake_that_superseded_it() {
        let h = harness();
        let attempts = Arc::new(AtomicU64::new(0));
        let apply = {
            let (attempts, state, tx) = (attempts.clone(), h.state.clone(), h.tx.clone());
            h.applier(Box::new(move |_, r| {
                if attempts.fetch_add(1, Ordering::AcqRel) == 0 {
                    // The source moves DURING the recapture — which is WHY it is
                    // superseded — dirtying a capability and waking the actor.
                    state.lock().ingest(owner_record(4), 0);
                    let _ = tx.send(1);
                    ApplyOutcome::Superseded
                } else {
                    ApplyOutcome::Current {
                        source_generation: r.batch.generation,
                    }
                }
            }))
        };
        let run = h.spawn(h.supervisor(), apply);
        settle().await;

        assert_eq!(
            h.seen.lock().as_slice(),
            &[
                (1, DirtyCapabilities::RebuildAll, false),
                (1, DirtyCapabilities::RebuildAll, false)
            ],
            "the second attempt must receive RebuildAll — the owed recapture \
             subsumes the Caps delta that woke the actor"
        );
        assert_eq!(
            h.health(),
            RoutingHealth::Healthy { incarnation: 1 },
            "the recapture completed and health recovered"
        );
        assert_eq!(attempts.load(Ordering::Acquire), 2, "exactly two attempts");

        h.stop();
        run.await.expect("supervisor joins");
    }

    /// Ordinary `Caps` movement does NOT toggle global health: fencing every warmed
    /// route because one unrelated capability moved would make routine churn
    /// globally disruptive. Per-slot invalidation is the registry's job (E3).
    #[tokio::test(start_paused = true)]
    async fn caps_movement_leaves_global_health_alone() {
        let h = harness();
        // Sample health DURING each application: asserting only the final state
        // would pass even if `Caps` toggled Rebuilding->Healthy around the work.
        let during: Arc<parking_lot::Mutex<Vec<(DirtyCapabilities, RoutingHealth)>>> =
            Arc::default();
        let apply = {
            let (during, health) = (during.clone(), h.health.clone());
            h.applier(Box::new(move |_, r| {
                during.lock().push((r.batch.dirty.clone(), **health.load()));
                ApplyOutcome::Current {
                    source_generation: r.batch.generation,
                }
            }))
        };
        let run = h.spawn(h.supervisor(), apply);
        settle().await;
        assert_eq!(h.health(), RoutingHealth::Healthy { incarnation: 1 });

        // Ordinary movement: a second provider dirties one capability.
        h.state.lock().ingest(owner_record(4), 0);
        let _ = h.tx.send(1);
        settle().await;

        let during = during.lock().clone();
        let caps_health = during
            .iter()
            .find(|(d, _)| matches!(d, DirtyCapabilities::Caps(_)))
            .map(|(_, health)| *health)
            .expect("a Caps batch was applied");
        assert_eq!(
            caps_health,
            RoutingHealth::Healthy { incarnation: 1 },
            "ordinary Caps movement must not globally fence warmed routes while it \
             rebuilds — per-slot invalidation is the registry's job"
        );
        // And the full recapture DID enter Rebuilding, so the distinction is real.
        let full_health = during
            .iter()
            .find(|(d, _)| matches!(d, DirtyCapabilities::RebuildAll))
            .map(|(_, health)| *health)
            .expect("a RebuildAll batch was applied");
        assert_eq!(
            full_health,
            RoutingHealth::Rebuilding { incarnation: 1 },
            "a complete recapture DOES publish global Rebuilding"
        );

        h.stop();
        run.await.expect("supervisor joins");
    }

    /// An explicit fault fences, and the supervisor runs a SUCCESSOR only after the
    /// predecessor resolved — the successor recaptures completely, so the delta the
    /// dead incarnation consumed is not lost.
    #[tokio::test(start_paused = true)]
    async fn a_fault_fences_then_a_successor_recaptures() {
        let h = harness();
        let apply = h.applier(Box::new(|inc, r| {
            if inc == 1 {
                ApplyOutcome::Fault(ActorFault {
                    reason: ("injected").into(),
                })
            } else {
                ApplyOutcome::Current {
                    source_generation: r.batch.generation,
                }
            }
        }));
        let run = h.spawn(h.supervisor(), apply);
        settle().await;
        tokio::time::advance(Duration::from_secs(1)).await;
        settle().await;

        assert_eq!(h.metrics.incarnations_started(), 2, "exactly one successor");
        assert_eq!(h.health(), RoutingHealth::Healthy { incarnation: 2 });
        assert_eq!(
            h.seen.lock().as_slice(),
            &[
                (1, DirtyCapabilities::RebuildAll, false),
                (2, DirtyCapabilities::RebuildAll, false)
            ],
            "the successor recaptured completely"
        );

        h.stop();
        run.await.expect("supervisor joins");
    }

    /// Fencing is SYNCHRONOUS with an abnormal exit: throughout the restart
    /// backoff — before any successor exists — health is already `Fenced`.
    ///
    /// Load-bearing because the incarnation reaches `Rebuilding` before it faults,
    /// so without the actor-stack fence health would still read `Rebuilding{1}`.
    #[tokio::test(start_paused = true)]
    async fn an_abnormal_exit_fences_synchronously_during_backoff() {
        let h = harness();
        let apply = h.applier(Box::new(|_, _| {
            ApplyOutcome::Fault(ActorFault {
                reason: ("injected").into(),
            })
        }));
        let run = h.spawn(h.supervisor(), apply);
        settle().await;

        assert_eq!(
            h.health(),
            RoutingHealth::Fenced,
            "a dead incarnation fences immediately, before any successor"
        );
        assert_eq!(h.metrics.incarnations_started(), 1, "still in backoff");

        h.stop();
        let _ = tokio::time::timeout(Duration::from_secs(5), run).await;
    }

    /// Cancelling the SUPERVISOR drops the incarnation with it: the fence runs and
    /// the exclusive drain lease is released.
    ///
    /// This is the detached-child hazard: a spawned incarnation whose `JoinHandle`
    /// is dropped keeps running, holding the drain and publishing health while
    /// outliving its supervisor. Inline ownership makes that unrepresentable.
    #[tokio::test(start_paused = true)]
    async fn cancelling_the_supervisor_drops_the_incarnation_and_frees_the_lease() {
        let h = harness();
        let run = h.spawn(h.supervisor(), h.ok_applier());
        settle().await;
        assert_eq!(h.health(), RoutingHealth::Healthy { incarnation: 1 });
        assert!(!h.lease_free(), "the live incarnation holds the lease");

        run.abort();
        let _ = run.await;
        settle().await;

        assert_eq!(
            h.health(),
            RoutingHealth::Fenced,
            "cancelling the supervisor fences: no orphan keeps routes usable"
        );
        assert!(
            h.lease_free(),
            "the orphaned incarnation did not survive holding the exclusive drain"
        );
    }

    /// A closed watch with NO shutdown in progress is abnormal, not teardown: the
    /// authoritative state is still alive, so invalidations have stopped while
    /// discovery can still change. Loud, terminal, one incarnation, no spin.
    #[tokio::test(start_paused = true)]
    async fn a_closed_watch_without_shutdown_is_loud_and_terminal() {
        let mut h = harness();
        let run = h.spawn(h.supervisor(), h.ok_applier());
        settle().await;

        // Close the channel while shutdown is still FALSE.
        let (tx, rx) = tokio::sync::watch::channel(0u64);
        drop(std::mem::replace(&mut h.tx, tx));
        drop(std::mem::replace(&mut h.rx, rx));

        let joined = tokio::time::timeout(Duration::from_secs(5), run).await;
        assert!(joined.is_ok(), "terminal, and no busy loop");
        assert!(
            !h.shutdown.load(Ordering::Acquire),
            "this was NOT a shutdown"
        );
        assert_eq!(
            h.metrics.source_closed_unexpected(),
            1,
            "the abnormal closure is observable"
        );
        assert_eq!(
            h.metrics.incarnations_started(),
            1,
            "no restart against a permanently closed receiver"
        );
        assert_eq!(h.health(), RoutingHealth::Fenced);
    }

    /// The same closure DURING shutdown is ordinary teardown: no abnormal counter.
    #[tokio::test(start_paused = true)]
    async fn a_closed_watch_during_shutdown_is_normal_teardown() {
        let mut h = harness();
        let run = h.spawn(h.supervisor(), h.ok_applier());
        settle().await;

        h.shutdown.store(true, Ordering::Release);
        let (tx, rx) = tokio::sync::watch::channel(0u64);
        drop(std::mem::replace(&mut h.tx, tx));
        drop(std::mem::replace(&mut h.rx, rx));

        let joined = tokio::time::timeout(Duration::from_secs(5), run).await;
        assert!(joined.is_ok());
        assert_eq!(
            h.metrics.source_closed_unexpected(),
            0,
            "teardown is not an abnormal closure"
        );
        assert_eq!(h.health(), RoutingHealth::Fenced);
    }

    /// Shutdown while the actor is parked stops the supervisor and fences.
    #[tokio::test(start_paused = true)]
    async fn shutdown_while_parked_stops_and_fences() {
        let h = harness();
        let run = h.spawn(h.supervisor(), h.ok_applier());
        settle().await;

        h.stop();
        let joined = tokio::time::timeout(Duration::from_secs(5), run).await;
        assert!(joined.is_ok(), "a parked actor still observes shutdown");
        assert_eq!(h.metrics.incarnations_started(), 1);
        assert_eq!(h.health(), RoutingHealth::Fenced);
    }

    /// Shutdown landing DURING restart backoff starts no replacement.
    #[tokio::test(start_paused = true)]
    async fn shutdown_during_backoff_starts_no_replacement() {
        let h = harness();
        let apply = h.applier(Box::new(|inc, r| {
            if inc == 1 {
                ApplyOutcome::Fault(ActorFault {
                    reason: ("injected").into(),
                })
            } else {
                ApplyOutcome::Current {
                    source_generation: r.batch.generation,
                }
            }
        }));
        let run = h.spawn(h.supervisor(), apply);
        settle().await;
        h.stop();

        let joined = tokio::time::timeout(Duration::from_secs(5), run).await;
        assert!(joined.is_ok(), "backoff is interruptible by shutdown");
        assert_eq!(
            h.metrics.incarnations_started(),
            1,
            "no replacement after shutdown"
        );
        assert_eq!(h.health(), RoutingHealth::Fenced);
    }

    /// A deterministic fault exhausts the bounded restart budget and lands in the
    /// terminal crash-loop state: permanently fenced, no further incarnations, no
    /// tight retry loop.
    #[tokio::test(start_paused = true)]
    async fn a_deterministic_fault_exhausts_the_restart_budget_and_stays_fenced() {
        let h = harness();
        let apply = h.applier(Box::new(|_, _| {
            ApplyOutcome::Fault(ActorFault {
                reason: ("deterministic").into(),
            })
        }));
        let run = h.spawn(h.supervisor(), apply);

        let joined = tokio::time::timeout(Duration::from_secs(600), run).await;
        assert!(
            joined.is_ok(),
            "the supervisor gives up rather than spinning"
        );
        assert_eq!(
            h.metrics.incarnations_started() as usize,
            MAX_RESTARTS_IN_WINDOW + 1,
            "exactly the budgeted attempts, then stop"
        );
        assert_eq!(
            h.health(),
            RoutingHealth::Fenced,
            "crash-loop exhaustion is fail-closed"
        );
    }

    /// Incarnation ids are CHECKED: exhaustion fences and terminates rather than
    /// wrapping and reusing an identifier a stale artifact could match.
    #[tokio::test(start_paused = true)]
    async fn incarnation_overflow_fences_rather_than_reusing_an_id() {
        let h = harness();
        h.metrics.set_incarnations_for_test(u64::MAX);
        h.health
            .store(Arc::new(RoutingHealth::Healthy { incarnation: 7 }));

        h.supervisor()
            .run(
                h.rx.clone(),
                h.ok_applier(),
                h.work.clone(),
                h.shutdown.clone(),
                h.notify.clone(),
                h.hooks.clone(),
            )
            .await;

        assert_eq!(h.health(), RoutingHealth::Fenced);
        assert!(h.lease_free(), "the refused mint released its claim");
    }

    // ----- OLB-2B-E3a: the registry-work wake seam -----

    /// A registry-work wake with a CLEAN source batch still reconciles.
    ///
    /// First demand insertion, slot-incarnation movement, and last-reference
    /// retirement change what must be built without moving private discovery at
    /// all. Skipping such a pass as "quiet" would strand first demand until some
    /// unrelated source movement happened to wake the actor.
    #[tokio::test(start_paused = true)]
    async fn registry_work_reconciles_even_with_a_clean_source_batch() {
        let h = harness();
        let run = h.spawn(h.supervisor(), h.ok_applier());
        settle().await;
        let after_recapture = h.seen.lock().len();

        // No source movement whatsoever — only registry work.
        h.work.mark();
        settle().await;

        let seen = h.seen.lock().clone();
        assert_eq!(
            seen.len(),
            after_recapture + 1,
            "the registry-work wake produced exactly one reconciliation pass"
        );
        let (_, dirty, work) = seen.last().expect("a pass").clone();
        assert_eq!(
            dirty,
            DirtyCapabilities::Clean,
            "the source really was clean — this pass is work-driven only"
        );
        assert!(work, "and the pass carries the registry-work trigger");

        h.stop();
        run.await.expect("supervisor joins");
    }

    /// The pending flag is AUTHORITATIVE, not the notification: a burst of marks
    /// coalesces into ONE reconciliation, and a mark landing before the actor parks
    /// is still observed rather than lost.
    #[tokio::test(start_paused = true)]
    async fn registry_work_coalesces_and_is_never_lost() {
        let h = harness();
        let run = h.spawn(h.supervisor(), h.ok_applier());
        settle().await;
        let baseline = h.seen.lock().len();

        // A burst: many marks, no awaits between them.
        for _ in 0..8 {
            h.work.mark();
        }
        settle().await;

        let seen = h.seen.lock().clone();
        assert_eq!(
            seen.len(),
            baseline + 1,
            "eight marks coalesce into one pass, not eight: {seen:?}"
        );
        assert!(
            seen.last().expect("a pass").2,
            "the coalesced pass carries the work trigger"
        );

        // The flag was consumed, so a quiet actor stays quiet.
        settle().await;
        assert_eq!(
            h.seen.lock().len(),
            baseline + 1,
            "a consumed flag does not re-trigger"
        );

        h.stop();
        run.await.expect("supervisor joins");
    }

    /// Registry work marked BEFORE the supervisor starts is not lost.
    ///
    /// `notify_waiters` retains no permit, so the notification itself is gone by
    /// the time an actor exists. The authoritative pending flag is what survives,
    /// and the first pass consumes it — arriving alongside the mint's RebuildAll.
    #[tokio::test(start_paused = true)]
    async fn registry_work_marked_before_start_is_not_lost() {
        let h = harness();
        // Marked with NO actor in existence: the notification cannot be delivered.
        h.work.mark();

        let run = h.spawn(h.supervisor(), h.ok_applier());
        settle().await;

        let seen = h.seen.lock().clone();
        assert_eq!(
            seen.first().expect("a first pass"),
            &(1, DirtyCapabilities::RebuildAll, true),
            "the first pass carries the mint's RebuildAll AND the pre-start work \
             flag: {seen:?}"
        );

        h.stop();
        run.await.expect("supervisor joins");
    }

    /// Work marked DURING an application is consumed by a later pass, exactly once.
    ///
    /// The mark lands after this pass already took the flag, so it must not be
    /// folded into the in-flight application (which never saw it) nor dropped.
    #[tokio::test(start_paused = true)]
    async fn registry_work_marked_during_an_application_is_consumed_exactly_once() {
        let h = harness();
        let marked = Arc::new(AtomicBool::new(false));
        let apply = {
            let (marked, work) = (marked.clone(), h.work.clone());
            h.applier(Box::new(move |_, r| {
                // During the FIRST application only, a registry mutation queues
                // bounded work and wakes the actor.
                if !marked.swap(true, Ordering::AcqRel) {
                    work.mark();
                }
                ApplyOutcome::Current {
                    source_generation: r.batch.generation,
                }
            }))
        };
        let run = h.spawn(h.supervisor(), apply);
        settle().await;

        let seen = h.seen.lock().clone();
        assert_eq!(
            seen.len(),
            2,
            "exactly one follow-up pass for the queued work: {seen:?}"
        );
        assert_eq!(
            seen[0],
            (1, DirtyCapabilities::RebuildAll, false),
            "the in-flight pass never saw the mark it had already passed"
        );
        assert_eq!(
            seen[1],
            (1, DirtyCapabilities::Clean, true),
            "the queued work is consumed by a later pass, with a clean source"
        );

        h.stop();
        run.await.expect("supervisor joins");
    }

    /// A registry-work pass does NOT masquerade as a node-wide rebuild: the source
    /// delta it carries is whatever was really drained, and global health is not
    /// disturbed.
    #[tokio::test(start_paused = true)]
    async fn registry_work_neither_fakes_a_rebuild_nor_fences() {
        let h = harness();
        let run = h.spawn(h.supervisor(), h.ok_applier());
        settle().await;
        assert_eq!(h.health(), RoutingHealth::Healthy { incarnation: 1 });

        h.work.mark();
        settle().await;

        let (_, dirty, _) = h.seen.lock().last().expect("a pass").clone();
        assert_ne!(
            dirty,
            DirtyCapabilities::RebuildAll,
            "first demand must not be synthesized into a node-wide RebuildAll"
        );
        assert_eq!(
            h.health(),
            RoutingHealth::Healthy { incarnation: 1 },
            "registry work does not globally fence warmed routes"
        );

        h.stop();
        run.await.expect("supervisor joins");
    }

    /// `allows` is the fence contract the warmed-call path will consult: only the
    /// LIVE incarnation's routes are usable, and health alone is never
    /// source-currentness.
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

    /// UNWIND-ONLY. Release builds set `panic = "abort"`, so this proves nothing
    /// about production supervision — it only pins that the fence guard also covers
    /// an unwinding panic where the profile permits one. Production restart is
    /// driven by [`ActorFault`], witnessed above.
    #[cfg(panic = "unwind")]
    #[tokio::test(start_paused = true)]
    async fn unwind_only_a_panicking_apply_still_runs_the_fence_guard() {
        let h = harness();
        let apply = h.applier(Box::new(|_, _| panic!("unwind-only fault")));
        let health = h.health.clone();
        let (rx, work, shutdown, notify, hooks) = (
            h.rx.clone(),
            h.work.clone(),
            h.shutdown.clone(),
            h.notify.clone(),
            h.hooks.clone(),
        );
        let sup = h.supervisor();
        let run =
            tokio::spawn(async move { sup.run(rx, apply, work, shutdown, notify, hooks).await });
        let outcome = run.await;

        assert!(outcome.is_err(), "the panic propagated (unwind profile)");
        assert_eq!(
            **health.load(),
            RoutingHealth::Fenced,
            "the actor-stack fence ran during the unwind"
        );
    }
}
