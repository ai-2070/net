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
            ReplicaTransitionEvent::LeaderLost { origin_hash, .. } => {
                MeshOsEvent::ReplicaLeaderUpdate {
                    chain: origin_hash,
                    leader: None,
                }
            }
            ReplicaTransitionEvent::LeaderLostAndIdled { origin_hash, .. } => {
                // Atomic pair: bundled into one MeshOsEvent so a
                // queue-full drop can't split it into a phantom
                // leader + holderless set.
                MeshOsEvent::ReplicaLeaderLostAndRemoved {
                    chain: origin_hash,
                    holder: self.this_node,
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

    use super::super::config::MeshOsConfig;
    use super::super::event_loop::{MeshOsLoop, MeshOsLoopParts};
    use super::*;
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
        let MeshOsLoopParts {
            mesh_loop: loop_,
            handle,
            actions_rx: _,
            reader: _,
        } = MeshOsLoop::new(fast_cfg());
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
        let MeshOsLoopParts {
            mesh_loop: loop_,
            handle,
            actions_rx: _,
            reader: _,
        } = MeshOsLoop::new(fast_cfg());
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
        let MeshOsLoopParts {
            mesh_loop: loop_,
            handle,
            actions_rx: _,
            reader: _,
        } = MeshOsLoop::new(fast_cfg());
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
        let MeshOsLoopParts {
            mesh_loop: loop_,
            handle,
            actions_rx: _,
            reader: _,
        } = MeshOsLoop::new(fast_cfg());
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
        let MeshOsLoopParts {
            mesh_loop: loop_,
            handle,
            actions_rx: _,
            reader: _,
        } = MeshOsLoop::new(fast_cfg());
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
        let MeshOsLoopParts {
            mesh_loop: loop_,
            handle,
            actions_rx: _,
            reader: _,
        } = MeshOsLoop::new(fast_cfg());
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
    async fn leader_lost_event_clears_replica_leader_via_none_update() {
        // When this coordinator steps down, the sink must surface
        // a `ReplicaLeaderUpdate { leader: None }` so MeshOS
        // clears its mirror — otherwise the loop carries a stale
        // `replica_leader[chain] = THIS_NODE` until a different
        // node's LeaderChanged overwrites it.
        const THIS_NODE: NodeId = 100;
        let MeshOsLoopParts {
            mesh_loop: loop_,
            handle,
            actions_rx: _,
            reader,
        } = MeshOsLoop::new(fast_cfg());
        let sink = MeshOsReplicaTransitionSink::new(handle.clone(), THIS_NODE);
        let task = tokio::spawn(loop_.run());
        // Seed the holders set first so the snapshot surfaces a
        // ReplicaSnapshot for the chain; otherwise an absent
        // entry could mask a regression where the fold drops the
        // LeaderLost translation.
        sink.observe(ReplicaTransitionEvent::BecameHolder {
            origin_hash: 0xBADC0DE,
            at: Instant::now(),
        });
        // Promote — leader is now Some(THIS_NODE).
        sink.observe(ReplicaTransitionEvent::LeaderChanged {
            origin_hash: 0xBADC0DE,
            at: Instant::now(),
        });
        // Step down — should publish leader: None.
        sink.observe(ReplicaTransitionEvent::LeaderLost {
            origin_hash: 0xBADC0DE,
            at: Instant::now(),
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        handle.publish(MeshOsEvent::Shutdown).await.unwrap();
        let _ = task.await;
        let snap = reader.read();
        // Holders set has THIS_NODE; leader has been cleared.
        let entry = snap
            .replicas
            .get(&0xBADC0DE)
            .expect("replicas entry exists after BecameHolder");
        assert_eq!(entry.leader, None, "LeaderLost should clear leader");
    }

    #[tokio::test]
    async fn leader_to_idle_lands_as_one_event_clearing_leader_and_removing_holder() {
        // Regression: a Leader → Idle transition used to fire two
        // separate events (Idled + LeaderLost). If the channel
        // were full one could drop, leaving the snapshot with
        // either a phantom leader or a leader-less holder set.
        // The substrate now fires `LeaderLostAndIdled` as one
        // event; the sink translates it to one bundled
        // `MeshOsEvent::ReplicaLeaderLostAndRemoved`.
        const THIS_NODE: NodeId = 100;
        let MeshOsLoopParts {
            mesh_loop: loop_,
            handle,
            actions_rx: _,
            reader,
        } = MeshOsLoop::new(fast_cfg());
        let sink = MeshOsReplicaTransitionSink::new(handle.clone(), THIS_NODE);
        let task = tokio::spawn(loop_.run());
        // Seed: become a holder + leader.
        sink.observe(ReplicaTransitionEvent::BecameHolder {
            origin_hash: 0xDEADBEEF,
            at: Instant::now(),
        });
        sink.observe(ReplicaTransitionEvent::LeaderChanged {
            origin_hash: 0xDEADBEEF,
            at: Instant::now(),
        });
        // Step down all the way to Idle — bundled event.
        sink.observe(ReplicaTransitionEvent::LeaderLostAndIdled {
            origin_hash: 0xDEADBEEF,
            at: Instant::now(),
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        handle.publish(MeshOsEvent::Shutdown).await.unwrap();
        let _ = task.await;
        let snap = reader.read();
        // Either the chain entry is absent (holder set empty —
        // both holder removal and leader clear succeeded), or
        // the entry has no holders AND no leader. A "phantom
        // leader on no holder" or "holder without leader" is the
        // exact regression we're guarding against.
        if let Some(entry) = snap.replicas.get(&0xDEADBEEF) {
            assert!(
                !entry.holders.contains(&THIS_NODE),
                "after LeaderLostAndIdled, THIS_NODE must not be in holders; got {:?}",
                entry,
            );
            assert_eq!(
                entry.leader, None,
                "after LeaderLostAndIdled, leader must be cleared; got {:?}",
                entry,
            );
        }
    }

    #[tokio::test]
    async fn replica_sink_drops_increment_when_loop_is_closed() {
        const THIS_NODE: NodeId = 100;
        let MeshOsLoopParts {
            mesh_loop: loop_,
            handle,
            actions_rx: _,
            reader: _,
        } = MeshOsLoop::new(fast_cfg());
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
        let MeshOsLoopParts {
            mesh_loop: loop_,
            handle,
            actions_rx: _,
            reader: _,
        } = MeshOsLoop::new(fast_cfg());
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
