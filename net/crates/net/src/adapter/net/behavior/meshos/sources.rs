//! Source converters — adapt subsystem-native signals into
//! [`super::event::MeshOsEvent`] and publish through a
//! [`super::event_loop::MeshOsHandle`].
//!
//! Locked decision #1: existing reactors become *event
//! sources*, not independent reactors. This module is where
//! each subsystem's signal stream attaches to the canonical
//! event loop.
//!
//! First converter: [`MeshOsDaemonLifecycleSink`] — adapts the
//! [`crate::adapter::net::compute::DaemonLifecycleObserver`]
//! trait to MeshOS event publishing. Install via
//! `DaemonRegistry::set_lifecycle_observer(sink)`; the
//! registry's register / replace / unregister paths fire
//! through it; the sink translates each
//! [`DaemonLifecycleEvent`] to the matching
//! [`super::event::MeshOsEvent::DaemonLifecycle`] and pushes
//! it onto the loop's channel.
//!
//! On `try_publish` failure (queue full / loop closed), the
//! sink either drops the event or records to a leaky counter
//! — never blocks, never panics. The drop counter is sampled
//! via [`MeshOsDaemonLifecycleSink::dropped_count`] for
//! diagnostics.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::adapter::net::compute::{DaemonLifecycleEvent, DaemonLifecycleObserver};
use crate::adapter::net::redex::{ReplicaTransitionEvent, ReplicaTransitionObserver};

use super::event::{DaemonLifecycleSignal, DaemonRef, MeshOsEvent, NodeId, ReplicaUpdate};
use super::event_loop::{MeshOsHandle, MeshOsHandleError};

/// Adapts compute-side lifecycle events into MeshOS events. Hold
/// behind `Arc` (the trait is `Send + Sync + 'static`); install
/// on `DaemonRegistry` via `set_lifecycle_observer`.
#[derive(Debug)]
pub struct MeshOsDaemonLifecycleSink {
    handle: MeshOsHandle,
    dropped: AtomicU64,
}

impl MeshOsDaemonLifecycleSink {
    /// Construct a sink that publishes to `handle`. Cheap —
    /// just clones the handle's sender.
    pub fn new(handle: MeshOsHandle) -> Self {
        Self {
            handle,
            dropped: AtomicU64::new(0),
        }
    }

    /// Total events the sink couldn't publish (queue full or
    /// loop closed). Increments on every drop; reads via
    /// `Relaxed`.
    pub fn dropped_count(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

impl DaemonLifecycleObserver for MeshOsDaemonLifecycleSink {
    fn observe(&self, event: DaemonLifecycleEvent) {
        let mesh_event = match event {
            DaemonLifecycleEvent::Registered { id, name, at } => MeshOsEvent::DaemonLifecycle {
                daemon: DaemonRef { id, name },
                signal: DaemonLifecycleSignal::Started { at },
            },
            DaemonLifecycleEvent::Unregistered { id, name, at } => MeshOsEvent::DaemonLifecycle {
                daemon: DaemonRef { id, name },
                signal: DaemonLifecycleSignal::ExitedCleanly { at },
            },
            DaemonLifecycleEvent::Crashed {
                id,
                name,
                at,
                reason,
            } => MeshOsEvent::DaemonLifecycle {
                daemon: DaemonRef { id, name },
                signal: DaemonLifecycleSignal::Crashed { at, reason },
            },
            DaemonLifecycleEvent::HealthChanged {
                id,
                name,
                at,
                health,
            } => MeshOsEvent::DaemonLifecycle {
                daemon: DaemonRef { id, name },
                signal: DaemonLifecycleSignal::HealthChanged { at, health },
            },
            DaemonLifecycleEvent::SaturationChanged {
                id,
                name,
                at,
                saturation,
            } => MeshOsEvent::DaemonLifecycle {
                daemon: DaemonRef { id, name },
                signal: DaemonLifecycleSignal::SaturationChanged { at, saturation },
            },
        };

        match self.handle.try_publish(mesh_event) {
            Ok(()) => {}
            Err(MeshOsHandleError::QueueFull) | Err(MeshOsHandleError::LoopClosed) => {
                self.dropped.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

/// Adapts replication-coordinator transitions into MeshOS
/// events. Hold behind `Arc`; install on each `ReplicationCoordinator`
/// via `set_transition_observer`. Same drop-on-overflow posture
/// as [`MeshOsDaemonLifecycleSink`].
///
/// `this_node` is the local node's identity; the sink fills it
/// into every emitted `ReplicaUpdate::{Added, Removed}` so
/// MeshOS reconcile sees consistent holder identities.
#[derive(Debug)]
pub struct MeshOsReplicaTransitionSink {
    handle: MeshOsHandle,
    this_node: NodeId,
    dropped: AtomicU64,
}

impl MeshOsReplicaTransitionSink {
    /// Construct a sink publishing to `handle`. `this_node` is
    /// the local node's id, filled into emitted
    /// `ReplicaUpdate { holder: this_node }` payloads.
    pub fn new(handle: MeshOsHandle, this_node: NodeId) -> Self {
        Self {
            handle,
            this_node,
            dropped: AtomicU64::new(0),
        }
    }

    /// Total events the sink couldn't publish.
    pub fn dropped_count(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

impl ReplicaTransitionObserver for MeshOsReplicaTransitionSink {
    fn observe(&self, event: ReplicaTransitionEvent) {
        let mesh_event = match event {
            ReplicaTransitionEvent::BecameHolder { origin_hash, .. } => {
                MeshOsEvent::ReplicaUpdate(ReplicaUpdate::Added {
                    chain: origin_hash,
                    holder: self.this_node,
                })
            }
            ReplicaTransitionEvent::Idled { origin_hash, .. } => {
                MeshOsEvent::ReplicaUpdate(ReplicaUpdate::Removed {
                    chain: origin_hash,
                    holder: self.this_node,
                })
            }
            ReplicaTransitionEvent::LeaderChanged { origin_hash, .. } => {
                MeshOsEvent::ReplicaLeaderUpdate {
                    chain: origin_hash,
                    leader: Some(self.this_node),
                }
            }
        };

        match self.handle.try_publish(mesh_event) {
            Ok(()) => {}
            Err(MeshOsHandleError::QueueFull) | Err(MeshOsHandleError::LoopClosed) => {
                self.dropped.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

/// One-line wire-up: install the replica-transition sink on a
/// `ReplicationCoordinator`. Returns the prior observer, if
/// any. `this_node` rides into every emitted `ReplicaUpdate`.
pub fn attach_to_replication_coordinator(
    coord: &crate::adapter::net::redex::ReplicationCoordinator,
    handle: MeshOsHandle,
    this_node: NodeId,
) -> Option<Arc<dyn ReplicaTransitionObserver>> {
    coord.set_transition_observer(Some(Arc::new(MeshOsReplicaTransitionSink::new(
        handle, this_node,
    ))))
}

/// Helper: install the sink on a `DaemonRegistry` in one call.
/// Returns the prior observer (if any), matching
/// `DaemonRegistry::set_lifecycle_observer`'s return type.
///
/// Lives here (not on the registry) so the wiring concern
/// stays in the meshos module — the registry doesn't need to
/// know about MeshOS specifics.
pub fn attach_to_daemon_registry(
    registry: &crate::adapter::net::compute::DaemonRegistry,
    handle: MeshOsHandle,
) -> Option<Arc<dyn DaemonLifecycleObserver>> {
    registry.set_lifecycle_observer(Some(Arc::new(MeshOsDaemonLifecycleSink::new(handle))))
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;
    use super::super::config::MeshOsConfig;
    use super::super::event_loop::MeshOsLoop;
    use crate::adapter::net::compute::DaemonHealth;

    fn fast_cfg() -> MeshOsConfig {
        MeshOsConfig {
            this_node: 1,
            tick_interval: std::time::Duration::from_millis(10),
            event_queue_capacity: 8,
            action_queue_capacity: 8,
            backpressure: Default::default(),
            locality: Default::default(),
            maintenance: Default::default(),
            scheduler: Default::default(),
        }
    }

    #[tokio::test]
    async fn registered_event_publishes_started_signal() {
        let (loop_, handle, _, _) = MeshOsLoop::new(fast_cfg());
        let sink = MeshOsDaemonLifecycleSink::new(handle.clone());
        let task = tokio::spawn(loop_.run());

        sink.observe(DaemonLifecycleEvent::Registered {
            id: 42,
            name: "telemetry".into(),
            at: Instant::now(),
        });

        // Shut down to harvest.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        handle.publish(MeshOsEvent::Shutdown).await.unwrap();
        let _ = task.await;

        assert_eq!(sink.dropped_count(), 0);
    }

    #[tokio::test]
    async fn dropped_count_increments_when_loop_is_closed() {
        let (loop_, handle, _, _) = MeshOsLoop::new(fast_cfg());
        let sink = MeshOsDaemonLifecycleSink::new(handle.clone());
        // Tear the loop down first.
        handle.publish(MeshOsEvent::Shutdown).await.unwrap();
        let _ = loop_.run().await;
        drop(handle);

        // Publishing into the dead handle now drops.
        sink.observe(DaemonLifecycleEvent::Crashed {
            id: 1,
            name: "telemetry".into(),
            at: Instant::now(),
            reason: "test".into(),
        });
        assert_eq!(sink.dropped_count(), 1);
    }

    #[tokio::test]
    async fn all_lifecycle_variants_translate_to_daemon_lifecycle_event() {
        // Compile-side coverage: every DaemonLifecycleEvent
        // variant maps to a MeshOsEvent::DaemonLifecycle with
        // the matching signal arm. Run all five and confirm
        // dropped_count stays at 0 (i.e. every publish
        // succeeded).
        let (loop_, handle, _, _) = MeshOsLoop::new(fast_cfg());
        let sink = MeshOsDaemonLifecycleSink::new(handle.clone());
        let task = tokio::spawn(loop_.run());

        let now = Instant::now();
        sink.observe(DaemonLifecycleEvent::Registered {
            id: 1,
            name: "a".into(),
            at: now,
        });
        sink.observe(DaemonLifecycleEvent::Unregistered {
            id: 1,
            name: "a".into(),
            at: now,
        });
        sink.observe(DaemonLifecycleEvent::Crashed {
            id: 1,
            name: "a".into(),
            at: now,
            reason: "x".into(),
        });
        sink.observe(DaemonLifecycleEvent::HealthChanged {
            id: 1,
            name: "a".into(),
            at: now,
            health: DaemonHealth::Healthy,
        });
        sink.observe(DaemonLifecycleEvent::SaturationChanged {
            id: 1,
            name: "a".into(),
            at: now,
            saturation: 0.5,
        });

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        handle.publish(MeshOsEvent::Shutdown).await.unwrap();
        let _ = task.await;

        assert_eq!(sink.dropped_count(), 0);
    }

    #[tokio::test]
    async fn became_holder_event_publishes_replica_added() {
        const THIS_NODE: NodeId = 100;
        let (loop_, handle, _, _) = MeshOsLoop::new(fast_cfg());
        let sink = MeshOsReplicaTransitionSink::new(handle.clone(), THIS_NODE);
        let task = tokio::spawn(loop_.run());

        sink.observe(ReplicaTransitionEvent::BecameHolder {
            origin_hash: 0xCAFE,
            at: Instant::now(),
        });

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        handle.publish(MeshOsEvent::Shutdown).await.unwrap();
        let _ = task.await;
        assert_eq!(sink.dropped_count(), 0);
    }

    #[tokio::test]
    async fn idled_event_publishes_replica_removed() {
        const THIS_NODE: NodeId = 100;
        let (loop_, handle, _, _) = MeshOsLoop::new(fast_cfg());
        let sink = MeshOsReplicaTransitionSink::new(handle.clone(), THIS_NODE);
        let task = tokio::spawn(loop_.run());

        sink.observe(ReplicaTransitionEvent::Idled {
            origin_hash: 0xBEEF,
            at: Instant::now(),
        });

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        handle.publish(MeshOsEvent::Shutdown).await.unwrap();
        let _ = task.await;
        assert_eq!(sink.dropped_count(), 0);
    }

    #[tokio::test]
    async fn leader_changed_event_publishes_replica_leader_update() {
        const THIS_NODE: NodeId = 100;
        let (loop_, handle, _, _) = MeshOsLoop::new(fast_cfg());
        let sink = MeshOsReplicaTransitionSink::new(handle.clone(), THIS_NODE);
        let task = tokio::spawn(loop_.run());

        sink.observe(ReplicaTransitionEvent::LeaderChanged {
            origin_hash: 0xC0FFEE,
            at: Instant::now(),
        });

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        handle.publish(MeshOsEvent::Shutdown).await.unwrap();
        let _ = task.await;
        assert_eq!(sink.dropped_count(), 0);
    }

    #[tokio::test]
    async fn replica_sink_drops_increment_when_loop_is_closed() {
        const THIS_NODE: NodeId = 100;
        let (loop_, handle, _, _) = MeshOsLoop::new(fast_cfg());
        let sink = MeshOsReplicaTransitionSink::new(handle.clone(), THIS_NODE);
        handle.publish(MeshOsEvent::Shutdown).await.unwrap();
        let _ = loop_.run().await;
        drop(handle);
        sink.observe(ReplicaTransitionEvent::BecameHolder {
            origin_hash: 1,
            at: Instant::now(),
        });
        assert_eq!(sink.dropped_count(), 1);
    }

    #[tokio::test]
    async fn attach_to_daemon_registry_installs_the_observer() {
        use crate::adapter::net::compute::DaemonRegistry;
        let (loop_, handle, _, _) = MeshOsLoop::new(fast_cfg());
        let registry = DaemonRegistry::new();
        // Consume the loop into a task so the handle's
        // mpsc receiver stays live for the observer-install
        // path (try_publish would surface a closed channel
        // otherwise).
        let task = tokio::spawn(loop_.run());
        assert!(!registry.has_lifecycle_observer());
        let _prior = attach_to_daemon_registry(&registry, handle.clone());
        assert!(registry.has_lifecycle_observer());
        handle.publish(MeshOsEvent::Shutdown).await.unwrap();
        let _ = task.await;
    }
}
