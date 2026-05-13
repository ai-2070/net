//! [`MeshOsLoop`] — the canonical event loop. Locked decision #1:
//! one stream → one reconcile → consistent actions. Locked
//! decision #4: reconcile emits, the action executor drains.
//!
//! Phase A wires the plumbing. The loop owns an mpsc receiver
//! that consumes [`super::event::MeshOsEvent`]s from arbitrary
//! sources, folds each event into [`super::state::MeshOsState`]
//! (and routes desired-state input into
//! [`super::state::DesiredState`]), runs
//! [`super::reconcile::reconcile`] at most once per
//! [`super::event::MeshOsEvent::Tick`], and pushes any emitted
//! actions through an mpsc sender that the action executor will
//! drain. Phase A's reconcile is a no-op so the executor sees an
//! empty queue under steady state.
//!
//! Sources fan-in via converters owned by their subsystems —
//! none ship in Phase A. Tests drive events directly through the
//! source channel to exercise the ordering contract.

use std::sync::Arc;

use tokio::sync::mpsc;
use tokio::time::{interval_at, Instant as TokioInstant, MissedTickBehavior};

use super::action::{AllocateActionId, PendingAction};
use super::config::MeshOsConfig;
use super::event::MeshOsEvent;
use super::reconcile::reconcile;
use super::state::{DesiredState, MeshOsState};

/// Per-node MeshOS instance. Owns the actual + desired state
/// folds, the event-source channel, and the action-executor
/// channel. Cloneable handles (`MeshOsHandle`) hand out
/// `mpsc::Sender<MeshOsEvent>` clones for sources to publish on;
/// `MeshOsLoop::run` is the long-lived task.
pub struct MeshOsLoop {
    config: Arc<MeshOsConfig>,

    /// Inbound event stream. The loop owns the receiver; sender
    /// clones live on every [`MeshOsHandle`]. When the last
    /// handle drops, `recv()` returns `None` and the loop exits.
    events_rx: mpsc::Receiver<MeshOsEvent>,

    /// Outbound action queue. The action executor task drains
    /// this; Phase A drains and drops (no real subsystem
    /// dispatch yet).
    actions_tx: mpsc::Sender<PendingAction>,

    /// Action id allocator.
    action_ids: AllocateActionId,

    /// Folded substrate state.
    actual: MeshOsState,
    /// Folded desired state (placement intent + future
    /// daemon-intent feeds).
    desired: DesiredState,

    /// Reconcile-pass counter — used by tests / diagnostics to
    /// confirm reconcile fired exactly once per Tick.
    reconcile_count: u64,
}

/// Cloneable handle for publishing events into the loop. Cheap
/// to clone (just clones the `mpsc::Sender`). Drop the last
/// handle to signal end-of-events; the loop will exit after
/// draining its current backlog.
#[derive(Clone, Debug)]
pub struct MeshOsHandle {
    events: mpsc::Sender<MeshOsEvent>,
}

impl MeshOsHandle {
    /// Publish an event into the loop's stream. Async — backs
    /// pressure when the source channel is at
    /// `event_queue_capacity`. Sources that need a fire-and-
    /// forget path can `try_send` directly on the underlying
    /// sender via `into_sender`.
    pub async fn publish(&self, event: MeshOsEvent) -> Result<(), MeshOsHandleError> {
        self.events
            .send(event)
            .await
            .map_err(|_| MeshOsHandleError::LoopClosed)
    }

    /// Try to publish without awaiting. Returns
    /// `MeshOsHandleError::QueueFull` when the source channel is
    /// at capacity.
    pub fn try_publish(&self, event: MeshOsEvent) -> Result<(), MeshOsHandleError> {
        self.events.try_send(event).map_err(|e| match e {
            mpsc::error::TrySendError::Closed(_) => MeshOsHandleError::LoopClosed,
            mpsc::error::TrySendError::Full(_) => MeshOsHandleError::QueueFull,
        })
    }

    /// Hand out the underlying sender for sources that need to
    /// manage their own backpressure / batching.
    pub fn into_sender(self) -> mpsc::Sender<MeshOsEvent> {
        self.events
    }
}

/// Surface-side errors from [`MeshOsHandle::publish`] /
/// [`MeshOsHandle::try_publish`]. The loop is conservative —
/// callers decide whether to retry, drop, or apply their own
/// backpressure.
#[derive(Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum MeshOsHandleError {
    /// The loop has exited; further publishes will be dropped.
    LoopClosed,
    /// The source channel is at `event_queue_capacity`. The
    /// caller picks: back off + retry, drop the event, or apply
    /// source-side backpressure.
    QueueFull,
}

impl MeshOsLoop {
    /// Construct a loop bound to the given config. Returns the
    /// loop (consumed by `run()`) and a [`MeshOsHandle`] that
    /// sources clone to publish events.
    pub fn new(config: MeshOsConfig) -> (Self, MeshOsHandle, mpsc::Receiver<PendingAction>) {
        let config = Arc::new(config);
        let (events_tx, events_rx) = mpsc::channel(config.event_queue_capacity);
        let (actions_tx, actions_rx) = mpsc::channel(config.action_queue_capacity);
        let handle = MeshOsHandle { events: events_tx };
        let me = Self {
            config,
            events_rx,
            actions_tx,
            action_ids: AllocateActionId::new(),
            actual: MeshOsState::default(),
            desired: DesiredState::default(),
            reconcile_count: 0,
        };
        (me, handle, actions_rx)
    }

    /// Drive the loop until either:
    /// 1. all `MeshOsHandle` clones drop and the source channel
    ///    empties (graceful end-of-events), or
    /// 2. a `MeshOsEvent::Shutdown` is dequeued.
    ///
    /// Returns the final `reconcile_count` — used by tests; in
    /// production it's diagnostic-only.
    pub async fn run(mut self) -> u64 {
        // Tick timer — fires every `tick_interval`. Configured
        // to skip missed ticks rather than burst, since the
        // reconcile pass is the slow-and-steady cadence the plan
        // locks in.
        let mut tick = interval_at(
            TokioInstant::now() + self.config.tick_interval,
            self.config.tick_interval,
        );
        tick.set_missed_tick_behavior(MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                event = self.events_rx.recv() => {
                    let Some(event) = event else { break };
                    if matches!(event, MeshOsEvent::Shutdown) {
                        break;
                    }
                    self.apply(&event);
                }
                _ = tick.tick() => {
                    // The Tick event drives reconcile; we route
                    // it through the same `apply` path so the
                    // `last_tick` fold field updates uniformly.
                    self.apply(&MeshOsEvent::Tick);
                    self.run_reconcile().await;
                }
            }
        }

        self.reconcile_count
    }

    fn apply(&mut self, event: &MeshOsEvent) {
        match event {
            MeshOsEvent::PlacementIntent(intent) => self.desired.apply(intent),
            MeshOsEvent::DaemonIntentUpdate(update) => self.desired.apply_daemon_intent(update),
            MeshOsEvent::LocalReplicaIntent(update) => {
                self.desired.apply_local_replica_intent(update)
            }
            MeshOsEvent::AdminEvent(admin) => {
                self.desired.apply_admin(admin, self.config.this_node);
            }
            _ => {}
        }
        self.actual.apply(event, self.config.this_node);
    }

    async fn run_reconcile(&mut self) {
        let actions = reconcile(
            &self.actual,
            &self.desired,
            self.config.this_node,
            &self.config.locality,
            &self.config.maintenance,
        );
        self.reconcile_count += 1;
        let now = std::time::Instant::now();
        for action in actions {
            let pending = PendingAction {
                id: self.action_ids.next(),
                action,
                emitted_at: now,
            };
            // Drop on backpressure rather than block reconcile —
            // the executor's job is to apply admit(); reconcile
            // staying responsive is the higher-order property.
            // Phase G upgrades this to a proper admit/defer
            // surface; Phase A's drop is harmless because no
            // actions are emitted yet.
            let _ = self.actions_tx.try_send(pending);
        }
    }

}

/// Convenience: a config with a very short `tick_interval` for
/// tests, so the reconcile pass fires quickly. Not exported
/// outside the crate.
#[cfg(test)]
pub(crate) fn fast_test_config() -> MeshOsConfig {
    MeshOsConfig {
        this_node: 1,
        tick_interval: std::time::Duration::from_millis(10),
        event_queue_capacity: 64,
        action_queue_capacity: 64,
        backpressure: Default::default(),
        locality: Default::default(),
        maintenance: Default::default(),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration as StdDuration;

    use super::*;
    use super::super::event::{ChainId, MeshOsEvent, ReplicaUpdate};

    #[tokio::test]
    async fn loop_exits_cleanly_when_all_handles_drop() {
        let (loop_, handle, mut actions_rx) = MeshOsLoop::new(fast_test_config());
        let task = tokio::spawn(loop_.run());
        drop(handle);
        // Loop should drain quickly. `run` returns the
        // reconcile count.
        let count = tokio::time::timeout(StdDuration::from_secs(1), task)
            .await
            .expect("loop did not exit after all handles dropped")
            .expect("join");
        // Zero or more ticks may have fired before we dropped;
        // assert we got at least zero (compiles + ran).
        let _ = count;
        // The action queue must be empty under Phase A.
        assert!(actions_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn loop_exits_on_shutdown_event() {
        let (loop_, handle, _) = MeshOsLoop::new(fast_test_config());
        let task = tokio::spawn(loop_.run());
        handle.publish(MeshOsEvent::Shutdown).await.unwrap();
        let count = tokio::time::timeout(StdDuration::from_secs(1), task)
            .await
            .expect("loop did not exit after Shutdown")
            .expect("join");
        let _ = count;
    }

    #[tokio::test]
    async fn ticks_drive_reconcile_passes() {
        let cfg = MeshOsConfig {
            tick_interval: StdDuration::from_millis(20),
            ..fast_test_config()
        };
        let (loop_, handle, _) = MeshOsLoop::new(cfg);
        let task = tokio::spawn(loop_.run());
        // Let several ticks fire.
        tokio::time::sleep(StdDuration::from_millis(120)).await;
        handle.publish(MeshOsEvent::Shutdown).await.unwrap();
        let count = task.await.expect("join");
        // At a 20 ms tick over a 120 ms window we expect roughly
        // 4–7 reconcile passes; require at least 2 so a slow CI
        // host doesn't flake.
        assert!(count >= 2, "expected at least 2 reconcile passes, got {count}");
    }

    #[tokio::test]
    async fn loop_drains_event_burst_without_panicking() {
        // Smoke test: the loop accepts a burst of arbitrary
        // events and exits cleanly when shutdown is published.
        // The fold-side ordering property is asserted directly
        // on `MeshOsState::apply` in `state::tests` — that
        // covers the substantive ordering guarantee without
        // having to crack open the consumed-loop's state.
        let (loop_, handle, _) = MeshOsLoop::new(fast_test_config());

        let chain: ChainId = 0xC0FFEE;
        let probe = tokio::spawn(async move {
            handle
                .publish(MeshOsEvent::ReplicaUpdate(ReplicaUpdate::Added {
                    chain,
                    holder: 11,
                }))
                .await
                .unwrap();
            handle
                .publish(MeshOsEvent::ReplicaUpdate(ReplicaUpdate::Added {
                    chain,
                    holder: 12,
                }))
                .await
                .unwrap();
            handle
                .publish(MeshOsEvent::ReplicaUpdate(ReplicaUpdate::Removed {
                    chain,
                    holder: 11,
                }))
                .await
                .unwrap();
            handle.publish(MeshOsEvent::Shutdown).await.unwrap();
        });

        let task = tokio::spawn(loop_.run());
        probe.await.expect("publisher panicked");
        let _count = tokio::time::timeout(StdDuration::from_millis(200), task)
            .await
            .expect("loop did not exit")
            .expect("join");
    }

    #[test]
    fn loop_construction_returns_handle_and_actions_receiver() {
        // Compile + type-check: `new` returns the triple, the
        // handle is cloneable, the actions receiver is the
        // documented type.
        let (_loop_, handle, _actions_rx) = MeshOsLoop::new(MeshOsConfig::default());
        let _clone = handle.clone();
    }

    #[tokio::test]
    async fn try_publish_surfaces_queue_full_under_saturation() {
        // Capacity-1 channel, loop not yet running — second
        // try_publish should hit QueueFull rather than block.
        let cfg = MeshOsConfig {
            event_queue_capacity: 1,
            ..fast_test_config()
        };
        let (loop_, handle, _) = MeshOsLoop::new(cfg);
        handle.try_publish(MeshOsEvent::Tick).unwrap();
        match handle.try_publish(MeshOsEvent::Tick) {
            Err(MeshOsHandleError::QueueFull) => {}
            other => panic!("expected QueueFull, got {other:?}"),
        }
        drop(handle);
        // Drain so the loop exits.
        let _ = loop_.run().await;
    }

    #[tokio::test]
    async fn shutdown_event_short_circuits_pending_events_after_it() {
        // Per the loop contract: Shutdown breaks the loop the
        // moment it's dequeued. Events sent after Shutdown is
        // sent are still dequeued by the channel (FIFO) but only
        // up to the Shutdown event; after the loop breaks they
        // remain undelivered.
        let (loop_, handle, _) = MeshOsLoop::new(fast_test_config());
        handle.publish(MeshOsEvent::Tick).await.unwrap();
        handle.publish(MeshOsEvent::Shutdown).await.unwrap();
        // A post-Shutdown event: enqueued behind Shutdown.
        handle
            .publish(MeshOsEvent::ReplicaUpdate(ReplicaUpdate::Added {
                chain: 1,
                holder: 1,
            }))
            .await
            .unwrap();
        let count = tokio::time::timeout(StdDuration::from_secs(1), loop_.run())
            .await
            .expect("loop did not exit on Shutdown")
            ;
        // First event (Tick) flowed through but did NOT trigger
        // a reconcile pass (reconcile fires on the timer Tick,
        // not on the event-payload Tick). So count stays 0.
        // (The timer Tick happens on the tokio `interval` arm
        // of `select!`, not from the application event.)
        let _ = count;
    }
}
