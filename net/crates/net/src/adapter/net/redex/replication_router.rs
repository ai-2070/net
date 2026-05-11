//! Per-`Redex` replication router — registers every spawned
//! [`ReplicationRuntimeHandle`] by [`ChannelId`] and dispatches
//! inbound `SUBPROTOCOL_REDEX` events from the mesh dispatch loop
//! to the right runtime's inbox.
//!
//! The substrate's mesh-side dispatcher decodes each inbound
//! `SUBPROTOCOL_REDEX` frame into an [`Inbound`] event keyed on
//! [`ChannelId`] (channel-name BLAKE2s), then calls
//! [`ReplicationInboundRouter::try_route`]. This module's
//! [`RedexReplicationRouter`] is the production impl — owns a
//! `DashMap<ChannelId, Arc<ReplicationRuntimeHandle>>` and
//! delegates `try_route` to the named runtime's `try_dispatch`.
//!
//! Lifecycle:
//!
//! - `Redex::enable_replication(mesh)` constructs one router per
//!   `Redex` and installs it on the `MeshNode` via
//!   `set_replication_inbound_router`. Idempotent — the second
//!   call to `enable_replication` is a no-op.
//! - `Redex::open_file` with `RedexFileConfig::replication.is_some()`
//!   spawns a `ReplicationRuntime` and registers its handle
//!   under the channel's [`ChannelId`].
//! - `Redex` drop / explicit `close_file` cancels the runtime +
//!   removes the registration; the router's `try_route` then
//!   returns `Err(inbound)` for that channel (which the mesh
//!   dispatcher drops silently).
//!
//! Routing edge cases:
//!
//! - Unknown channel id — runtime not registered (channel not
//!   opened on this node, or registration was removed during
//!   cleanup): `try_route` returns `Err(inbound)`. Caller (mesh
//!   dispatch) drops silently.
//! - Runtime inbox full — at [`RUNTIME_INBOX_CAPACITY`] (1024 per
//!   plan §3 cardinality budget): `try_route` returns
//!   `Err(inbound)`. Same drop-silently shape; reliable-stream /
//!   heartbeat cycle recovers observable state.

use std::sync::Arc;

use dashmap::DashMap;

use super::replication::ChannelId;
use super::replication_runtime::{
    Inbound, ReplicationInboundRouter, ReplicationRuntimeHandle,
};

/// Per-`Redex` registry of runtime handles, dispatching by
/// channel id. Cheap to clone (everything is Arc) so the same
/// router can be shared between `Redex` (for registration) and
/// `MeshNode` (for inbound dispatch).
#[derive(Default)]
pub struct RedexReplicationRouter {
    runtimes: DashMap<ChannelId, Arc<ReplicationRuntimeHandle>>,
}

impl RedexReplicationRouter {
    /// Construct an empty router.
    pub fn new() -> Self {
        Self {
            runtimes: DashMap::new(),
        }
    }

    /// Register a runtime handle under `channel_id`. Returns the
    /// previously-registered handle if one existed, so the caller
    /// can cancel it cleanly. Re-registration is the
    /// `RedexFileConfig::replication` update path — same channel,
    /// new config, new runtime.
    pub fn register(
        &self,
        channel_id: ChannelId,
        handle: Arc<ReplicationRuntimeHandle>,
    ) -> Option<Arc<ReplicationRuntimeHandle>> {
        self.runtimes.insert(channel_id, handle)
    }

    /// Look up a runtime handle. Cloned `Arc` so the caller can
    /// drive the handle (dispatch events, cancel) without
    /// holding the DashMap shard lock.
    pub fn get(&self, channel_id: &ChannelId) -> Option<Arc<ReplicationRuntimeHandle>> {
        self.runtimes.get(channel_id).map(|e| e.value().clone())
    }

    /// Remove the registration for `channel_id`. Returns the
    /// removed handle, if any, so the caller can cancel + await
    /// its exit deterministically.
    pub fn unregister(
        &self,
        channel_id: &ChannelId,
    ) -> Option<Arc<ReplicationRuntimeHandle>> {
        self.runtimes.remove(channel_id).map(|(_, v)| v)
    }

    /// Number of registered runtimes.
    pub fn len(&self) -> usize {
        self.runtimes.len()
    }

    /// True iff no runtimes are registered.
    pub fn is_empty(&self) -> bool {
        self.runtimes.is_empty()
    }
}

impl ReplicationInboundRouter for RedexReplicationRouter {
    fn try_route(&self, channel_id: ChannelId, inbound: Inbound) -> Result<(), Inbound> {
        match self.runtimes.get(&channel_id) {
            Some(handle) => handle.value().try_dispatch(inbound),
            None => Err(inbound),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::net::channel::ChannelName;
    use crate::adapter::net::redex::replication::{ReplicaRole, SyncHeartbeat};

    fn cid_for(name: &str) -> ChannelId {
        let cn = ChannelName::new(name).unwrap();
        ChannelId::from_name(&cn)
    }

    fn dummy_inbound(channel_id: ChannelId) -> Inbound {
        Inbound::Heartbeat {
            from: 0xAA,
            msg: SyncHeartbeat {
                channel_id,
                tail_seq: 0,
                role: ReplicaRole::Replica,
                wall_clock_ms: 0,
            },
        }
    }

    #[test]
    fn unknown_channel_returns_inbound_back() {
        let router = RedexReplicationRouter::new();
        let cid = cid_for("test/unknown");
        let event = dummy_inbound(cid);
        let result = router.try_route(cid, event);
        assert!(result.is_err(), "unknown channel must reject");
    }

    #[test]
    fn empty_router_reports_empty() {
        let router = RedexReplicationRouter::new();
        assert!(router.is_empty());
        assert_eq!(router.len(), 0);
        assert!(router.get(&cid_for("nothing")).is_none());
    }

    #[test]
    fn unregister_returns_handle_and_drops_registration() {
        // We use a runtime-spawned handle, which requires the
        // tokio runtime. Build a minimal one with `block_on`.
        let rt = tokio::runtime::Runtime::new().unwrap();
        let _guard = rt.enter();
        let cid = cid_for("test/unregister");
        let handle = build_dummy_handle();
        let router = RedexReplicationRouter::new();
        router.register(cid, handle.clone());
        assert_eq!(router.len(), 1);
        let removed = router.unregister(&cid);
        assert!(removed.is_some(), "unregister must return the handle");
        assert!(router.is_empty());
        // Re-routing the same channel now fails (registration
        // removed).
        let result = router.try_route(cid, dummy_inbound(cid));
        assert!(result.is_err());
        // Drain the runtime so the task exits cleanly.
        rt.block_on(handle.cancel());
    }

    #[test]
    fn register_replaces_returns_previous_handle() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let _guard = rt.enter();
        let cid = cid_for("test/replace");
        let first = build_dummy_handle();
        let second = build_dummy_handle();
        let router = RedexReplicationRouter::new();
        assert!(router.register(cid, first.clone()).is_none());
        let previous = router.register(cid, second.clone());
        assert!(
            previous.is_some(),
            "second register must return the prior handle"
        );
        assert_eq!(router.len(), 1, "still one channel — second replaced first");
        rt.block_on(first.cancel());
        rt.block_on(second.cancel());
    }

    #[test]
    fn try_route_to_registered_channel_dispatches() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let _guard = rt.enter();
        let cid = cid_for("test/runtime");
        let handle = build_dummy_handle();
        let router = RedexReplicationRouter::new();
        router.register(cid, handle.clone());
        // A real event flows into the runtime's inbox.
        let result = router.try_route(cid, dummy_inbound(cid));
        assert!(result.is_ok(), "registered channel must route");
        rt.block_on(handle.cancel());
    }

    /// Build a minimal `ReplicationRuntimeHandle` for unit tests.
    /// Uses the channel name `"test/runtime"` and the same shape
    /// as the unit tests in `replication_runtime.rs`.
    fn build_dummy_handle() -> Arc<ReplicationRuntimeHandle> {
        use super::super::file::RedexFile;
        use super::super::manager::Redex;
        use super::super::replication_budget::BandwidthBudget;
        use super::super::replication_config::ReplicationConfig;
        use super::super::replication_coordinator::{
            ChainTagSink, ChannelIdentity, ReplicationCoordinator,
        };
        use super::super::replication_metrics::ReplicationMetricsRegistry;
        use super::super::replication_runtime::{
            spawn_replication_runtime, ReplicationDispatcher, RuntimeInputs,
        };
        use crate::adapter::net::behavior::placement::NodeId;
        use crate::adapter::net::channel::ChannelName;
        use crate::adapter::net::redex::config::RedexFileConfig;
        use crate::error::AdapterError;
        use parking_lot::Mutex;
        use std::time::{Duration, Instant};

        struct NoopSink;
        #[async_trait::async_trait]
        impl ChainTagSink for NoopSink {
            async fn announce_chain(
                &self,
                _origin_hash: u64,
                _tip_seq: u64,
            ) -> Result<(), AdapterError> {
                Ok(())
            }
            async fn withdraw_chain(&self, _origin_hash: u64) -> Result<(), AdapterError> {
                Ok(())
            }
        }
        struct NoopDispatcher;
        #[async_trait::async_trait]
        impl ReplicationDispatcher for NoopDispatcher {
            async fn send_heartbeat(
                &self,
                _target: NodeId,
                _msg: SyncHeartbeat,
            ) -> Result<(), AdapterError> {
                Ok(())
            }
            async fn send_sync_request(
                &self,
                _target: NodeId,
                _msg: super::super::replication::SyncRequest,
            ) -> Result<(), AdapterError> {
                Ok(())
            }
            async fn send_sync_response(
                &self,
                _target: NodeId,
                _msg: super::super::replication::SyncResponse,
            ) -> Result<(), AdapterError> {
                Ok(())
            }
            async fn send_sync_nack(
                &self,
                _target: NodeId,
                _msg: super::super::replication::SyncNack,
            ) -> Result<(), AdapterError> {
                Ok(())
            }
        }

        let cn = ChannelName::new("test/runtime").unwrap();
        let redex = Redex::new();
        let file: RedexFile = redex
            .open_file(&cn, RedexFileConfig::default())
            .unwrap();
        let registry = ReplicationMetricsRegistry::new();
        let coordinator = Arc::new(ReplicationCoordinator::new(
            ChannelIdentity {
                channel_name: "test/runtime".to_string(),
                origin_hash: 0xCAFE_BABE,
            },
            ReplicationConfig::new(),
            Arc::new(NoopSink) as Arc<dyn ChainTagSink>,
            &registry,
        ));
        let inputs = RuntimeInputs {
            channel: ChannelIdentity {
                channel_name: "test/runtime".to_string(),
                origin_hash: 0xCAFE_BABE,
            },
            channel_id: cid_for("test/runtime"),
            self_node_id: 0x10,
            replica_set: vec![0x10, 0x20],
            heartbeat_ms: 60_000, // very slow tick for tests
            wall_clock_provider: Arc::new(|| 0),
            tail_provider: Arc::new(|| 0),
            rtt_lookup: Arc::new(|_| Some(Duration::from_millis(5))),
            file,
        };
        let budget = Arc::new(Mutex::new(BandwidthBudget::new(
            0.5,
            1_000_000,
            Instant::now(),
        )));
        Arc::new(spawn_replication_runtime(
            inputs,
            coordinator,
            Arc::new(NoopDispatcher),
            budget,
        ))
    }
}
