//! Rust SDK tests for the daemon channel-re-bind ledger.
//!
//! Stage 1 + Stage 2 of
//! [`DAEMON_CHANNEL_REBIND_PLAN.md`](../../../docs/DAEMON_CHANNEL_REBIND_PLAN.md):
//! subscribe through `DaemonRuntime` records a ledger entry on the
//! source's daemon host; the ledger rides inside
//! `StateSnapshot::bindings_bytes` across the migration wire; the
//! target's restored daemon host exposes the same ledger after
//! restore.
//!
//! Stage 3 (auto-replay during Restore) + Stage 4 (source-side
//! teardown at Cutover) are tracked in the todo list — this file
//! asserts the ledger plumbing works end-to-end, which is the
//! prerequisite for both.

#![cfg(feature = "compute")]

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use tokio::time::sleep;

use net::adapter::net::compute::DaemonError as CoreDaemonError;
use net::adapter::net::state::causal::CausalEvent;
use net_sdk::capabilities::CapabilityFilter;
use net_sdk::compute::{DaemonHostConfig, DaemonRuntime, MeshDaemon, SubscriptionBinding};
use net_sdk::mesh::{Mesh, MeshBuilder};
use net_sdk::{ChannelConfig, ChannelId, ChannelName, Identity, Visibility};

const PSK: [u8; 32] = [0x42u8; 32];

// ---- Fixture: echo daemon ---------------------------------------------

struct EchoCounter {
    count: u64,
}

impl MeshDaemon for EchoCounter {
    fn name(&self) -> &str {
        "echo-counter"
    }
    fn requirements(&self) -> CapabilityFilter {
        CapabilityFilter::default()
    }
    fn process(&mut self, _event: &CausalEvent) -> Result<Vec<Bytes>, CoreDaemonError> {
        self.count += 1;
        Ok(vec![Bytes::copy_from_slice(&self.count.to_le_bytes())])
    }
    fn snapshot(&self) -> Option<Bytes> {
        Some(Bytes::copy_from_slice(&self.count.to_le_bytes()))
    }
    fn restore(&mut self, state: Bytes) -> Result<(), CoreDaemonError> {
        if state.len() != 8 {
            return Err(CoreDaemonError::RestoreFailed(format!(
                "counter needs 8 bytes, got {}",
                state.len()
            )));
        }
        let mut arr = [0u8; 8];
        arr.copy_from_slice(&state);
        self.count = u64::from_le_bytes(arr);
        Ok(())
    }
}

fn echo_factory() -> impl Fn() -> Box<dyn MeshDaemon> + Send + Sync + 'static {
    || Box::new(EchoCounter { count: 0 })
}

// ---- Harness -----------------------------------------------------------

struct Trio {
    source_rt: DaemonRuntime,
    target_rt: DaemonRuntime,
    publisher: Mesh,
}

async fn build_trio() -> Trio {
    let source_mesh = MeshBuilder::new("127.0.0.1:0", &PSK)
        .unwrap()
        .build()
        .await
        .unwrap();
    let target_mesh = MeshBuilder::new("127.0.0.1:0", &PSK)
        .unwrap()
        .build()
        .await
        .unwrap();
    let publisher = MeshBuilder::new("127.0.0.1:0", &PSK)
        .unwrap()
        .build()
        .await
        .unwrap();

    // Full mesh: source↔target, source↔publisher, target↔publisher.
    handshake(&source_mesh, &target_mesh).await;
    handshake(&source_mesh, &publisher).await;
    handshake(&target_mesh, &publisher).await;

    let source_rt = DaemonRuntime::new(Arc::new(source_mesh));
    let target_rt = DaemonRuntime::new(Arc::new(target_mesh));
    Trio {
        source_rt,
        target_rt,
        publisher,
    }
}

async fn handshake(a: &Mesh, b: &Mesh) {
    let addr_b = b.inner().local_addr();
    let pub_b = *b.inner().public_key();
    let nid_b = b.inner().node_id();
    let nid_a = a.inner().node_id();
    let (r1, r2) = tokio::join!(b.inner().accept(nid_a), async {
        sleep(Duration::from_millis(50)).await;
        a.inner().connect(addr_b, &pub_b, nid_b).await
    });
    r1.expect("accept");
    r2.expect("connect");
}

// ---- Stage 1: subscribe records, unsubscribe forgets -----------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn subscribe_through_runtime_records_in_ledger() {
    let trio = build_trio().await;
    let Trio {
        source_rt,
        publisher,
        ..
    } = &trio;
    source_rt
        .register_factory("echo-counter", echo_factory())
        .unwrap();
    source_rt.start().await.unwrap();
    source_rt.mesh().start();
    publisher.start();
    sleep(Duration::from_millis(100)).await;

    // Publisher registers an open channel.
    let channel = ChannelName::new("sensors/temp").unwrap();
    publisher.register_channel(
        ChannelConfig::new(ChannelId::new(channel.clone())).with_visibility(Visibility::Global),
    );

    // Spawn daemon, subscribe it to the publisher's channel.
    let identity = Identity::generate();
    let handle = source_rt
        .spawn("echo-counter", identity, DaemonHostConfig::default())
        .await
        .expect("spawn");

    // Ledger is empty pre-subscribe.
    assert!(source_rt
        .subscriptions(handle.origin_hash)
        .expect("ledger query")
        .is_empty(),);

    source_rt
        .subscribe_channel(
            handle.origin_hash,
            publisher.inner().node_id(),
            channel.clone(),
            None,
        )
        .await
        .expect("subscribe");

    // Ledger now carries the binding.
    let subs = source_rt.subscriptions(handle.origin_hash).unwrap();
    assert_eq!(subs.len(), 1);
    assert_eq!(subs[0].publisher, publisher.inner().node_id());
    assert_eq!(subs[0].channel, channel);
    assert!(subs[0].token_bytes.is_none());

    // Unsubscribe drops the binding.
    source_rt
        .unsubscribe_channel(
            handle.origin_hash,
            publisher.inner().node_id(),
            channel.clone(),
        )
        .await
        .expect("unsubscribe");

    assert!(source_rt
        .subscriptions(handle.origin_hash)
        .expect("ledger")
        .is_empty(),);
}

// ---- Stage 2: ledger rides in `StateSnapshot::bindings_bytes` --------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ledger_rides_snapshot_through_migration() {
    let trio = build_trio().await;
    let Trio {
        source_rt,
        target_rt,
        publisher,
    } = &trio;
    source_rt
        .register_factory("echo-counter", echo_factory())
        .unwrap();
    target_rt
        .register_factory("echo-counter", echo_factory())
        .unwrap();
    source_rt.start().await.unwrap();
    target_rt.start().await.unwrap();
    source_rt.mesh().start();
    target_rt.mesh().start();
    publisher.start();
    sleep(Duration::from_millis(100)).await;

    let channel = ChannelName::new("sensors/lidar").unwrap();
    publisher.register_channel(
        ChannelConfig::new(ChannelId::new(channel.clone())).with_visibility(Visibility::Global),
    );

    let identity = Identity::generate();
    let handle = source_rt
        .spawn(
            "echo-counter",
            identity.clone(),
            DaemonHostConfig::default(),
        )
        .await
        .expect("spawn");

    source_rt
        .subscribe_channel(
            handle.origin_hash,
            publisher.inner().node_id(),
            channel.clone(),
            None,
        )
        .await
        .expect("subscribe");

    // Target pre-registers for the migration by origin_hash only.
    // The envelope on the outbound snapshot carries the real
    // keypair; `expect_migration` stores a placeholder and the
    // dispatcher overrides at restore time.
    target_rt
        .expect_migration(
            "echo-counter",
            handle.origin_hash,
            DaemonHostConfig::default(),
        )
        .expect("expect_migration on target");
    let _ = identity; // not needed on target under envelope transport

    let mig = source_rt
        .start_migration(
            handle.origin_hash,
            source_rt.mesh().inner().node_id(),
            target_rt.mesh().inner().node_id(),
        )
        .await
        .expect("start_migration");
    mig.wait_with_timeout(Duration::from_secs(5))
        .await
        .expect("migration reached Complete");

    // Target's restored daemon host carries the same subscription in
    // its ledger, reconstructed from `snapshot.bindings_bytes`.
    let target_subs = target_rt
        .subscriptions(handle.origin_hash)
        .expect("target ledger query");
    assert_eq!(target_subs.len(), 1);
    let sub = &target_subs[0];
    assert_eq!(sub.publisher, publisher.inner().node_id());
    assert_eq!(sub.channel, channel);
    assert!(sub.token_bytes.is_none());

    // Sanity check the binding struct survived intact.
    let _ = SubscriptionBinding {
        publisher: sub.publisher,
        channel: sub.channel.clone(),
        token_bytes: sub.token_bytes.clone(),
    };
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn auto_replay_rebinds_subscriptions_on_target_after_migration() {
    // Stage 3: after a migration Completes, the target's ledger is
    // rehydrated AND the target's DaemonRuntime has auto-invoked
    // `mesh.subscribe_channel(...)` for every binding so the
    // publisher's roster now includes the target node. We observe
    // this via the publisher's `roster.has_subscriber(channel,
    // target_node_id)` after letting the rebind task run.
    let trio = build_trio().await;
    let Trio {
        source_rt,
        target_rt,
        publisher,
    } = &trio;
    source_rt
        .register_factory("echo-counter", echo_factory())
        .unwrap();
    target_rt
        .register_factory("echo-counter", echo_factory())
        .unwrap();
    source_rt.start().await.unwrap();
    target_rt.start().await.unwrap();
    source_rt.mesh().start();
    target_rt.mesh().start();
    publisher.start();
    sleep(Duration::from_millis(100)).await;

    let channel = ChannelName::new("lab/streams").unwrap();
    publisher.register_channel(
        ChannelConfig::new(ChannelId::new(channel.clone())).with_visibility(Visibility::Global),
    );

    let identity = Identity::generate();
    let handle = source_rt
        .spawn(
            "echo-counter",
            identity.clone(),
            DaemonHostConfig::default(),
        )
        .await
        .expect("spawn");

    source_rt
        .subscribe_channel(
            handle.origin_hash,
            publisher.inner().node_id(),
            channel.clone(),
            None,
        )
        .await
        .expect("source-side subscribe");

    // Publisher initially sees ONLY the source node in the roster.
    let channel_id = ChannelId::new(channel.clone());
    let pre_migration = publisher
        .inner()
        .roster()
        .members(&channel_id)
        .contains(&source_rt.mesh().inner().node_id());
    assert!(
        pre_migration,
        "publisher must see source as subscriber pre-migration"
    );

    target_rt
        .register_migration_target_identity("echo-counter", identity, DaemonHostConfig::default())
        .expect("pre-register target");

    let mig = source_rt
        .start_migration(
            handle.origin_hash,
            source_rt.mesh().inner().node_id(),
            target_rt.mesh().inner().node_id(),
        )
        .await
        .expect("start_migration");
    mig.wait_with_timeout(Duration::from_secs(5))
        .await
        .expect("migration Complete");

    // Give the auto-replay tokio task a beat to finish its
    // subscribe round-trip to the publisher.
    let target_node = target_rt.mesh().inner().node_id();
    let source_node = source_rt.mesh().inner().node_id();
    let arrived = wait_until(Duration::from_secs(2), Duration::from_millis(50), || {
        publisher
            .inner()
            .roster()
            .members(&channel_id)
            .contains(&target_node)
    })
    .await;
    assert!(
        arrived,
        "publisher must see target as subscriber after auto-replay — \
         channel re-bind didn't complete on migration",
    );

    // Stage 4: after Cutover, the source's pre-cleanup callback
    // sent Unsubscribe to the publisher. The publisher's roster
    // should drop the source node within a short window — without
    // the teardown it would linger until session timeout (~30 s).
    let source_gone = wait_until(Duration::from_secs(2), Duration::from_millis(50), || {
        !publisher
            .inner()
            .roster()
            .members(&channel_id)
            .contains(&source_node)
    })
    .await;
    assert!(
        source_gone,
        "publisher's roster must drop the source promptly after \
         Cutover — source-side Unsubscribe teardown didn't fire",
    );
}

async fn wait_until<F: FnMut() -> bool>(total: Duration, step: Duration, mut cond: F) -> bool {
    let deadline = tokio::time::Instant::now() + total;
    loop {
        if cond() {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        sleep(step).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn empty_ledger_produces_no_bindings_bytes_overhead() {
    let mesh = MeshBuilder::new("127.0.0.1:0", &PSK)
        .unwrap()
        .build()
        .await
        .unwrap();
    let rt = DaemonRuntime::new(Arc::new(mesh));
    rt.register_factory("echo-counter", echo_factory()).unwrap();
    rt.start().await.unwrap();

    let handle = rt
        .spawn(
            "echo-counter",
            Identity::generate(),
            DaemonHostConfig::default(),
        )
        .await
        .unwrap();
    let snap = rt
        .snapshot(handle.origin_hash)
        .await
        .expect("snapshot ok")
        .expect("counter is stateful");
    // No subscriptions → empty ledger → empty `bindings_bytes`.
    assert!(
        snap.bindings_bytes.is_empty(),
        "empty ledger must serialize to zero bytes, not a wire trailer",
    );
}
