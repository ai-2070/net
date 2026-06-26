//! The scheduler-bridge driver — the loop that wires the projections to
//! the live subsystems.
//!
//! [`SchedulerBridgeDriver`] owns the live handles and does the I/O the
//! [`SchedulerBridge`] facade deliberately does not:
//!   - [`tick`](SchedulerBridgeDriver::tick): each pass, feed
//!     `project_liveness_from_snapshot(snapshot)` →
//!     `MeshNode::set_liveness_down`, and publish the merged desired
//!     daemon intents into the MeshOS loop;
//!   - [`on_running`](SchedulerBridgeDriver::on_running) /
//!     [`on_released`](SchedulerBridgeDriver::on_released): the claim-
//!     lifecycle hooks the step-driver calls;
//!   - [`lifecycle_observer`](SchedulerBridgeDriver::lifecycle_observer):
//!     a `DaemonLifecycleObserver` applying Projection 3 — fan it in
//!     beside the MeshOS sink with [`fan_out_lifecycle`] (the registry
//!     holds a single observer slot) so daemon signals reach both the
//!     MeshOS loop and the workflow.
//!
//! This is the one place that imports the live runtime types (MeshNode,
//! MeshOsHandle, WorkflowAdapter, the lifecycle observer) — appropriate,
//! since the bridge is the integration point (LD 5 keeps `workflow` /
//! `gang` and `meshos` from importing each other; the bridge sees all).

use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use tokio::sync::Notify;
use tokio::task::JoinHandle;

use crate::adapter::net::behavior::fold::{IslandId, IslandQuery, NodeId};
use crate::adapter::net::behavior::meshos::event_loop::{MeshOsHandle, MeshOsSnapshotReader};
use crate::adapter::net::behavior::meshos::{
    DaemonIntentUpdate, DaemonLifecycleSignal, DaemonRef, MeshOsEvent,
};
use crate::adapter::net::compute::{DaemonLifecycleEvent, DaemonLifecycleObserver};
use crate::adapter::net::cortex::workflow::{ActiveClaim, TaskId, WorkflowAdapter};
use crate::adapter::net::MeshNode;

use super::lifecycle::LifecycleTransition;
use super::liveness::project_liveness_from_snapshot;
use super::runtime::SchedulerBridge;

/// Resolve a claim's island to its current host via the node's
/// `IslandTopology` fold — the `resolve_host` closure Projection 2 needs.
fn resolve_island_host(mesh: &MeshNode, island: IslandId) -> Option<NodeId> {
    mesh.island_fold()
        .query(IslandQuery::Get(island))
        .first()
        .map(|(_, record)| record.host)
}

/// Translate a registry `DaemonLifecycleEvent` into the `(daemon, signal)`
/// the bridge consumes — the same mapping `MeshOsDaemonLifecycleSink`
/// uses, so the two sinks agree on what each event means.
fn event_to_signal(event: DaemonLifecycleEvent) -> (DaemonRef, DaemonLifecycleSignal) {
    match event {
        DaemonLifecycleEvent::Registered { id, name, at } => (
            DaemonRef { id, name },
            DaemonLifecycleSignal::Started { at },
        ),
        DaemonLifecycleEvent::Unregistered { id, name, at } => (
            DaemonRef { id, name },
            DaemonLifecycleSignal::ExitedCleanly { at },
        ),
        DaemonLifecycleEvent::Crashed {
            id,
            name,
            at,
            reason,
        } => (
            DaemonRef { id, name },
            DaemonLifecycleSignal::Crashed { at, reason },
        ),
        DaemonLifecycleEvent::HealthChanged {
            id,
            name,
            at,
            health,
        } => (
            DaemonRef { id, name },
            DaemonLifecycleSignal::HealthChanged { at, health },
        ),
        DaemonLifecycleEvent::SaturationChanged {
            id,
            name,
            at,
            saturation,
        } => (
            DaemonRef { id, name },
            DaemonLifecycleSignal::SaturationChanged { at, saturation },
        ),
    }
}

/// Applies Projection 3 to the workflow as daemon signals arrive. On each
/// signal it computes the transition and writes it to the task chain —
/// `FailStep` calls `WorkflowAdapter::fail`, driving the task `Failed`,
/// which fires the `AfterTerminal` failure policy, and releases the held
/// claim from the registry.
struct BridgeLifecycleObserver {
    bridge: Arc<Mutex<SchedulerBridge>>,
    workflow: Arc<WorkflowAdapter>,
}

impl DaemonLifecycleObserver for BridgeLifecycleObserver {
    fn observe(&self, event: DaemonLifecycleEvent) {
        let (daemon, signal) = event_to_signal(event);
        let transition = {
            let state = self.workflow.state();
            let guard = state.read();
            self.bridge
                .lock()
                .lifecycle_transition(&signal, &daemon, &guard)
        };
        match transition {
            Some(LifecycleTransition::ConfirmRunning(task)) => {
                let _ = self.workflow.start(task);
            }
            Some(LifecycleTransition::FailStep(task)) => {
                let _ = self.workflow.fail(task);
                self.bridge.lock().on_released(task);
            }
            None => {}
        }
    }
}

/// Fans a daemon lifecycle signal out to several observers. The
/// `DaemonRegistry` holds a single observer slot, so install one of these
/// to reach both the existing MeshOS sink AND the bridge's observer.
struct FanOutLifecycleObserver {
    observers: Vec<Arc<dyn DaemonLifecycleObserver>>,
}

impl DaemonLifecycleObserver for FanOutLifecycleObserver {
    fn observe(&self, event: DaemonLifecycleEvent) {
        for obs in &self.observers {
            obs.observe(event.clone());
        }
    }
}

/// Compose several lifecycle observers into one — install the result on
/// the `DaemonRegistry` (which holds a single observer slot) so a daemon
/// signal reaches every observer (e.g. the MeshOS sink + the bridge's
/// [`SchedulerBridgeDriver::lifecycle_observer`]).
pub fn fan_out_lifecycle(
    observers: Vec<Arc<dyn DaemonLifecycleObserver>>,
) -> Arc<dyn DaemonLifecycleObserver> {
    Arc::new(FanOutLifecycleObserver { observers })
}

/// What one [`tick`](SchedulerBridgeDriver::tick) did — for diagnostics
/// and tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TickReport {
    /// `DaemonIntentUpdate`s published this tick.
    pub published: usize,
    /// Hosts marked down (fed to `set_liveness_down`).
    pub down: usize,
}

/// The driver loop facade — owns the live handles and drives the bridge.
pub struct SchedulerBridgeDriver {
    bridge: Arc<Mutex<SchedulerBridge>>,
    workflow: Arc<WorkflowAdapter>,
    mesh: Arc<MeshNode>,
    handle: MeshOsHandle,
    snapshot: MeshOsSnapshotReader,
    /// Stop signal for the [`spawn`](SchedulerBridgeDriver::spawn)ed loop.
    /// `notify_one` so a shutdown raised while the loop is mid-`tick`
    /// stores a permit and is never lost.
    shutdown: Arc<Notify>,
}

impl SchedulerBridgeDriver {
    /// Build a driver over the live handles. Starts with an empty
    /// `ClaimRegistry`.
    pub fn new(
        workflow: Arc<WorkflowAdapter>,
        mesh: Arc<MeshNode>,
        handle: MeshOsHandle,
        snapshot: MeshOsSnapshotReader,
    ) -> Self {
        Self {
            bridge: Arc::new(Mutex::new(SchedulerBridge::new())),
            workflow,
            mesh,
            handle,
            snapshot,
            shutdown: Arc::new(Notify::new()),
        }
    }

    /// Record a claim — call on `StepGate::Running(claim)`.
    pub fn on_running(&self, task: TaskId, claim: ActiveClaim) {
        self.bridge.lock().on_running(task, claim);
    }

    /// Release a claim — call on `release_step`. Returns the released
    /// claim, if any.
    pub fn on_released(&self, task: TaskId) -> Option<ActiveClaim> {
        self.bridge.lock().on_released(task)
    }

    /// One driver pass: apply liveness from the snapshot, then publish the
    /// merged desired daemon intents (Projection 1 + 2). Publishes are
    /// non-blocking and drop on a wedged / closed loop (like the MeshOS
    /// sinks); returns what the pass did.
    pub fn tick(&self) -> TickReport {
        // Projection 4: snapshot → down-set → set_liveness_down. Borrow
        // the snapshot through its `Arc` (`load`) rather than deep-cloning
        // it (`read`): the projection only reads `.peers`, and the guard is
        // dropped as soon as the delta is computed, so the borrow is short.
        let delta = {
            let snapshot = self.snapshot.load();
            project_liveness_from_snapshot(&snapshot)
        };
        let down = delta.down.len();
        self.mesh
            .set_liveness_down(delta.down.into_iter().collect());

        // Projection 1 + 2: publish the merged desired daemon intents.
        let intents: Vec<DaemonIntentUpdate> = {
            let state = self.workflow.state();
            let guard = state.read();
            let mesh = &self.mesh;
            self.bridge
                .lock()
                .desired_intents(&guard, |island| resolve_island_host(mesh, island))
        };
        let mut published = 0;
        for intent in intents {
            if self
                .handle
                .try_publish(MeshOsEvent::DaemonIntentUpdate(intent))
                .is_ok()
            {
                published += 1;
            }
        }
        TickReport { published, down }
    }

    /// A `DaemonLifecycleObserver` applying Projection 3 to the workflow.
    /// Fan it in beside the MeshOS sink with [`fan_out_lifecycle`].
    pub fn lifecycle_observer(&self) -> Arc<dyn DaemonLifecycleObserver> {
        Arc::new(BridgeLifecycleObserver {
            bridge: Arc::clone(&self.bridge),
            workflow: Arc::clone(&self.workflow),
        })
    }

    /// Spawn the periodic driver loop: call [`tick`](Self::tick) every
    /// `interval` until [`shutdown`](Self::shutdown) is signalled. Returns
    /// the task handle — await it after `shutdown()` for a clean stop.
    ///
    /// The driver bridges three subsystems (`MeshNode` / `MeshOsRuntime` /
    /// `WorkflowAdapter`) with no single natural owner, so its lifetime is
    /// the caller's: tie `shutdown()` to your own teardown rather than to
    /// any one subsystem's. The first tick fires immediately; missed ticks
    /// are skipped (no burst catch-up).
    pub fn spawn(self: Arc<Self>, interval: Duration) -> JoinHandle<()> {
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    _ = ticker.tick() => {
                        self.tick();
                    }
                    _ = self.shutdown.notified() => break,
                }
            }
        })
    }

    /// Signal the [`spawn`](Self::spawn)ed loop to stop. `notify_one`
    /// stores a permit if the loop is mid-`tick`, so the signal is never
    /// lost; await the returned `JoinHandle` for a clean shutdown.
    pub fn shutdown(&self) {
        self.shutdown.notify_one();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Instant;

    use super::*;

    #[test]
    fn event_to_signal_mirrors_the_meshos_sink_mapping() {
        let at = Instant::now();
        let (d, s) = event_to_signal(DaemonLifecycleEvent::Registered {
            id: 7,
            name: "task/1".into(),
            at,
        });
        assert_eq!(
            d,
            DaemonRef {
                id: 7,
                name: "task/1".into()
            }
        );
        assert_eq!(s, DaemonLifecycleSignal::Started { at });

        let (_, s) = event_to_signal(DaemonLifecycleEvent::Unregistered {
            id: 7,
            name: "task/1".into(),
            at,
        });
        assert_eq!(s, DaemonLifecycleSignal::ExitedCleanly { at });

        let (_, s) = event_to_signal(DaemonLifecycleEvent::Crashed {
            id: 7,
            name: "task/1".into(),
            at,
            reason: "oom".into(),
        });
        assert_eq!(
            s,
            DaemonLifecycleSignal::Crashed {
                at,
                reason: "oom".into(),
            }
        );
    }

    /// Counting observer for the fan-out test.
    struct Counter(Arc<AtomicUsize>);
    impl DaemonLifecycleObserver for Counter {
        fn observe(&self, _event: DaemonLifecycleEvent) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[test]
    fn fan_out_forwards_to_every_observer() {
        let a = Arc::new(AtomicUsize::new(0));
        let b = Arc::new(AtomicUsize::new(0));
        let fan = fan_out_lifecycle(vec![
            Arc::new(Counter(a.clone())),
            Arc::new(Counter(b.clone())),
        ]);
        fan.observe(DaemonLifecycleEvent::Registered {
            id: 1,
            name: "x".into(),
            at: Instant::now(),
        });
        assert_eq!(a.load(Ordering::Relaxed), 1);
        assert_eq!(b.load(Ordering::Relaxed), 1);
    }
}
