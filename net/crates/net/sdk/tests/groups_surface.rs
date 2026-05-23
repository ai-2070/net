//! Rust SDK surface tests for `ReplicaGroup` / `ForkGroup` /
//! `StandbyGroup`. Covers Stage 1 of `SDK_GROUPS_SURFACE_PLAN.md`
//! — spawn, route, scale, failure/recovery, error paths.

#![cfg(feature = "groups")]

use std::sync::Arc;

use bytes::Bytes;

use net::adapter::net::behavior::capability::{CapabilityAnnouncement, CapabilitySet};
use net::adapter::net::behavior::loadbalance::{RequestContext, Strategy};
use net::adapter::net::compute::DaemonError as CoreDaemonError;
use net::adapter::net::identity::EntityId;
use net::adapter::net::state::causal::CausalEvent;
use net_sdk::capabilities::CapabilityFilter;
use net_sdk::compute::{DaemonHostConfig, DaemonRuntime, MeshDaemon};
use net_sdk::groups::{
    ForkGroup, ForkGroupConfig, GroupError, GroupHealth, ReplicaGroup, ReplicaGroupConfig,
    StandbyGroup, StandbyGroupConfig,
};
use net_sdk::mesh::MeshBuilder;

const PSK: [u8; 32] = [0x42u8; 32];

// ---- Fixtures ---------------------------------------------------------

struct NoopDaemon;

impl MeshDaemon for NoopDaemon {
    fn name(&self) -> &str {
        "noop"
    }
    fn requirements(&self) -> CapabilityFilter {
        CapabilityFilter::default()
    }
    fn process(&mut self, _event: &CausalEvent) -> Result<Vec<Bytes>, CoreDaemonError> {
        Ok(vec![])
    }
}

/// Build a started `DaemonRuntime` with the given number of
/// synthetic peer nodes indexed in the capability graph, so the
/// scheduler has enough candidates for `place_with_spread`.
async fn runtime_with_peers(extra_peers: usize) -> DaemonRuntime {
    let mesh = MeshBuilder::new("127.0.0.1:0", &PSK)
        .unwrap()
        .build()
        .await
        .expect("build mesh");

    // Inject synthetic capability announcements so the scheduler
    // has N+1 candidates (local + `extra_peers`). The core's
    // replica_group tests use the same pattern because
    // `place_with_spread` needs multiple distinct node IDs.
    let filler_eid = EntityId::from_bytes([0u8; 32]);
    for i in 0..extra_peers {
        let node_id = 0x1000_0000_0000_0000u64 + (i as u64 + 1);
        mesh.inner()
            .test_inject_capability_announcement(CapabilityAnnouncement::new(
                node_id,
                filler_eid.clone(),
                1,
                CapabilitySet::new(),
            ));
    }

    let rt = DaemonRuntime::new(Arc::new(mesh));
    rt.register_factory("noop", || Box::new(NoopDaemon))
        .expect("register factory");
    rt.start().await.expect("start runtime");
    rt
}

fn replica_config(n: u8, seed: u8) -> ReplicaGroupConfig {
    ReplicaGroupConfig {
        replica_count: n,
        group_seed: [seed; 32],
        lb_strategy: Strategy::RoundRobin,
        host_config: DaemonHostConfig::default(),
    }
}

// ---- ReplicaGroup tests -----------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replica_group_spawn_registers_members_and_reports_health() {
    let rt = runtime_with_peers(3).await;
    let group =
        ReplicaGroup::spawn(&rt, "noop", replica_config(3, 0x11)).expect("spawn replica group");

    assert_eq!(group.replica_count(), 3);
    assert_eq!(group.healthy_count(), 3);
    assert_eq!(group.health(), GroupHealth::Healthy);
    assert_eq!(rt.daemon_count(), 3);
    let replicas = group.replicas();
    assert_eq!(replicas.len(), 3);
    // Deterministic origin_hashes — same seed always produces the
    // same set of replica identities.
    for r in &replicas {
        assert!(r.healthy);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replica_group_route_event_returns_live_origin() {
    let rt = runtime_with_peers(3).await;
    let group =
        ReplicaGroup::spawn(&rt, "noop", replica_config(3, 0x22)).expect("spawn replica group");

    // Route 30 requests via consistent-hash on distinct keys — we
    // only care that every routed origin is one of the group's
    // live member origin_hashes.
    let live: std::collections::HashSet<u64> =
        group.replicas().iter().map(|m| m.origin_hash).collect();
    for i in 0..30u64 {
        let ctx = RequestContext::new().with_routing_key(format!("req-{i}"));
        let origin = group.route_event(&ctx).expect("route");
        assert!(
            live.contains(&origin),
            "route returned {:#x}; not in live set {:?}",
            origin,
            live
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replica_group_scale_up_and_down_tracks_daemon_count() {
    let rt = runtime_with_peers(5).await;
    let group = ReplicaGroup::spawn(&rt, "noop", replica_config(2, 0x33)).expect("spawn");
    assert_eq!(rt.daemon_count(), 2);

    group.scale_to(5).expect("scale up");
    assert_eq!(group.replica_count(), 5);
    assert_eq!(rt.daemon_count(), 5);

    group.scale_to(1).expect("scale down");
    assert_eq!(group.replica_count(), 1);
    assert_eq!(rt.daemon_count(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replica_group_spawn_on_not_ready_runtime_errors() {
    // Build a runtime but DON'T call start().
    let mesh = MeshBuilder::new("127.0.0.1:0", &PSK)
        .unwrap()
        .build()
        .await
        .expect("build mesh");
    let rt = DaemonRuntime::new(Arc::new(mesh));
    rt.register_factory("noop", || Box::new(NoopDaemon))
        .expect("register");

    let err = ReplicaGroup::spawn(&rt, "noop", replica_config(2, 0x44))
        .expect_err("spawn before start must error");
    assert!(matches!(err, GroupError::NotReady));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replica_group_unknown_kind_errors_with_factory_not_found() {
    let rt = runtime_with_peers(2).await;
    let err = ReplicaGroup::spawn(&rt, "never-registered", replica_config(2, 0x55))
        .expect_err("unknown kind must error");
    match err {
        GroupError::FactoryNotFound(k) => assert_eq!(k, "never-registered"),
        other => panic!("expected FactoryNotFound, got {other:?}"),
    }
}

// ---- ForkGroup tests --------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fork_group_forks_produce_unique_origins_with_verifiable_lineage() {
    let rt = runtime_with_peers(4).await;
    let parent_origin: u64 = 0xabcd_ef01;
    let fork_seq: u64 = 42;
    let group = ForkGroup::fork(
        &rt,
        "noop",
        parent_origin,
        fork_seq,
        ForkGroupConfig {
            fork_count: 3,
            lb_strategy: Strategy::RoundRobin,
            host_config: DaemonHostConfig::default(),
        },
    )
    .expect("fork");

    assert_eq!(group.fork_count(), 3);
    assert_eq!(group.parent_origin(), parent_origin);
    assert_eq!(group.fork_seq(), fork_seq);

    let members = group.members();
    let mut origins: Vec<u64> = members.iter().map(|m| m.origin_hash).collect();
    origins.sort_unstable();
    origins.dedup();
    assert_eq!(origins.len(), 3, "each fork must have a unique origin_hash");

    assert!(group.verify_lineage());

    let records = group.fork_records();
    assert_eq!(records.len(), 3);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fork_group_spawn_on_not_ready_runtime_errors() {
    let mesh = MeshBuilder::new("127.0.0.1:0", &PSK)
        .unwrap()
        .build()
        .await
        .expect("build mesh");
    let rt = DaemonRuntime::new(Arc::new(mesh));
    rt.register_factory("noop", || Box::new(NoopDaemon))
        .expect("register");
    let err = ForkGroup::fork(
        &rt,
        "noop",
        0x1234,
        1,
        ForkGroupConfig {
            fork_count: 2,
            lb_strategy: Strategy::RoundRobin,
            host_config: DaemonHostConfig::default(),
        },
    )
    .expect_err("fork on not-ready runtime");
    assert!(matches!(err, GroupError::NotReady));
}

// ---- StandbyGroup tests -----------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn standby_group_spawn_member_zero_is_active() {
    let rt = runtime_with_peers(3).await;
    let group = StandbyGroup::spawn(
        &rt,
        "noop",
        StandbyGroupConfig {
            member_count: 3,
            group_seed: [0x77; 32],
            host_config: DaemonHostConfig::default(),
        },
    )
    .expect("spawn standby");

    assert_eq!(group.member_count(), 3);
    assert_eq!(group.standby_count(), 2);
    assert_eq!(group.active_index(), 0);
    assert!(group.active_healthy());
    assert!(group.active_origin() != 0);
    assert_eq!(group.buffered_event_count(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn standby_group_member_count_below_two_is_rejected() {
    let rt = runtime_with_peers(3).await;
    let err = StandbyGroup::spawn(
        &rt,
        "noop",
        StandbyGroupConfig {
            member_count: 1,
            group_seed: [0x88; 32],
            host_config: DaemonHostConfig::default(),
        },
    )
    .expect_err("member_count=1 must error");
    // Surfaces as GroupError::Core wrapping InvalidConfig.
    match err {
        GroupError::Core(_) => {}
        other => panic!("expected GroupError::Core, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn standby_group_unknown_kind_errors() {
    let rt = runtime_with_peers(3).await;
    let err = StandbyGroup::spawn(
        &rt,
        "never-registered",
        StandbyGroupConfig {
            member_count: 2,
            group_seed: [0x99; 32],
            host_config: DaemonHostConfig::default(),
        },
    )
    .expect_err("unknown kind");
    match err {
        GroupError::FactoryNotFound(k) => assert_eq!(k, "never-registered"),
        other => panic!("expected FactoryNotFound, got {other:?}"),
    }
}

// Regression: the replay buffer must grow automatically on
// `DaemonRuntime::deliver(active_origin, event)` — no caller-side
// `on_event_delivered` pairing required. Before the fix,
// `StandbyGroup` relied on the caller to make both calls; a
// forgotten buffering call silently dropped events on failover.
// The fix installs a post-delivery observer at spawn so every
// successful delivery to the active's origin automatically feeds
// the buffer.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn standby_group_buffers_events_without_manual_hook() {
    use ::net::adapter::net::state::causal::{CausalEvent, CausalLink};
    use bytes::Bytes;

    let rt = runtime_with_peers(3).await;
    let group = StandbyGroup::spawn(
        &rt,
        "noop",
        StandbyGroupConfig {
            member_count: 3,
            group_seed: [0xAAu8; 32],
            host_config: DaemonHostConfig::default(),
        },
    )
    .expect("spawn standby");

    let active = group.active_origin();
    assert_eq!(group.buffered_event_count(), 0, "buffer starts empty");

    // Deliver three events through the plain `rt.deliver` path —
    // NO `group.on_event_delivered` call. The auto-wired observer
    // must catch every one.
    for i in 1..=3u64 {
        let event = CausalEvent {
            link: CausalLink {
                origin_hash: active,
                horizon_encoded: 0,
                sequence: i,
                parent_hash: 0,
            },
            payload: Bytes::from(format!("tick-{i}")),
            received_at: 0,
        };
        rt.deliver(active, &event).expect("deliver");
    }

    assert_eq!(
        group.buffered_event_count(),
        3,
        "expected 3 buffered events from automatic observer",
    );
}
