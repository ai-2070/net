//! Rust SDK migration surface tests.
//!
//! Stage 2 of `SDK_COMPUTE_SURFACE_PLAN.md` — exercises
//! `DaemonRuntime::start_migration` end-to-end over an encrypted UDP
//! mesh, plus `MigrationHandle::wait` / `cancel` / `phase` on both
//! the happy path and failure modes.
//!
//! Scope: local-source case (source node == the orchestrator). The
//! remote-source case is covered by `three_node_integration.rs` at
//! the core layer.

#![cfg(feature = "compute")]

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use tokio::time::sleep;

use net::adapter::net::compute::DaemonError as CoreDaemonError;
use net::adapter::net::state::causal::CausalEvent;
use net_sdk::capabilities::CapabilityFilter;
use net_sdk::compute::{
    DaemonError, DaemonHostConfig, DaemonRuntime, MeshDaemon, MigrationFailureReason, MigrationOpts,
};
use net_sdk::mesh::{Mesh, MeshBuilder};
use net_sdk::Identity;

const PSK: [u8; 32] = [0x42u8; 32];

// ---- Fixture daemon: stateful counter ---------------------------------

struct CounterDaemon {
    count: u64,
}

impl MeshDaemon for CounterDaemon {
    fn name(&self) -> &str {
        "counter"
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

// ---- Harness: two meshes + runtimes + handshake ------------------------

struct Pair {
    source_rt: DaemonRuntime,
    target_rt: DaemonRuntime,
}

async fn build_pair() -> Pair {
    let a = MeshBuilder::new("127.0.0.1:0", &PSK)
        .unwrap()
        .build()
        .await
        .expect("build a");
    let b = MeshBuilder::new("127.0.0.1:0", &PSK)
        .unwrap()
        .build()
        .await
        .expect("build b");
    handshake(&a, &b).await;
    let source_rt = DaemonRuntime::new(Arc::new(a));
    let target_rt = DaemonRuntime::new(Arc::new(b));
    Pair {
        source_rt,
        target_rt,
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

fn counter_factory() -> impl Fn() -> Box<dyn MeshDaemon> + Send + Sync + 'static {
    || Box::new(CounterDaemon { count: 0 })
}

// ---- Tests -------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn start_migration_requires_ready_runtime() {
    let pair = build_pair().await;
    // Source is Registering — spawn / migrate rejected.
    let err = pair
        .source_rt
        .start_migration(0xDEAD_BEEF, 0, 0)
        .await
        .expect_err("start_migration must fail while Registering");
    assert!(matches!(err, DaemonError::NotReady), "got {err:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn local_source_migration_reaches_complete_and_transfers_state() {
    let pair = build_pair().await;
    let Pair {
        source_rt,
        target_rt,
    } = &pair;

    // Register the same factory on both nodes. Target uses it to
    // reconstruct the daemon after the snapshot arrives.
    source_rt
        .register_factory("counter", counter_factory())
        .unwrap();
    target_rt
        .register_factory("counter", counter_factory())
        .unwrap();

    source_rt.start().await.unwrap();
    target_rt.start().await.unwrap();

    // Start both meshes' receive loops.
    source_rt.mesh().inner().start();
    target_rt.mesh().inner().start();
    sleep(Duration::from_millis(100)).await;

    // Daemon identity — only the source needs to know it. The
    // target reconstructs the keypair from the identity envelope
    // that rides with the snapshot (Stages 5b / 6 of the
    // identity-migration plan). The target pre-registers a factory
    // keyed by origin_hash with a **placeholder** keypair; the
    // envelope overrides it at restore time.
    let identity = Identity::generate();
    let origin_hash = identity.keypair().origin_hash();
    let handle = source_rt
        .spawn("counter", identity.clone(), DaemonHostConfig::default())
        .await
        .expect("spawn on source");
    for i in 1..=3u64 {
        source_rt
            .deliver(handle.origin_hash, &make_event(origin_hash, i, b"tick"))
            .expect("deliver");
    }

    // Target must know the (origin_hash → kind) mapping so the
    // dispatcher can find the factory closure. The keypair in this
    // registration is used only as a fallback when the envelope is
    // absent — under envelope transport it's overridden by the
    // decrypted keypair from the snapshot.
    target_rt
        .register_migration_target_identity("counter", identity, DaemonHostConfig::default())
        .expect("pre-register target identity (envelope overrides keypair at restore)");

    // Kick off the migration: source → target.
    let mig = source_rt
        .start_migration(
            handle.origin_hash,
            source_rt.mesh().inner().node_id(),
            target_rt.mesh().inner().node_id(),
        )
        .await
        .expect("start_migration");
    assert_eq!(mig.origin_hash, handle.origin_hash);

    // Wait for Complete. Migration should finish within a couple
    // seconds on a localhost UDP loop.
    let result = mig.wait_with_timeout(Duration::from_secs(5)).await;
    result.expect("migration reached Complete");

    // Target now holds the daemon. Deliver one more event and
    // assert the counter continued from 3 (not reset to 0).
    let outputs = target_rt
        .deliver(origin_hash, &make_event(origin_hash, 4, b"post-migration"))
        .expect("deliver on target");
    assert_eq!(outputs.len(), 1);
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&outputs[0].payload);
    assert_eq!(
        u64::from_le_bytes(bytes),
        4,
        "counter must continue from the pre-migration state, not reset",
    );

    // Source should no longer host the daemon.
    assert_eq!(source_rt.daemon_count(), 0);
    assert_eq!(target_rt.daemon_count(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn migration_to_unconnected_peer_fails_target_unavailable() {
    let pair = build_pair().await;
    pair.source_rt
        .register_factory("counter", counter_factory())
        .unwrap();
    pair.source_rt.start().await.unwrap();
    pair.source_rt.mesh().inner().start();

    let identity = Identity::generate();
    let origin_hash = identity.keypair().origin_hash();
    let _handle = pair
        .source_rt
        .spawn("counter", identity, DaemonHostConfig::default())
        .await
        .expect("spawn");

    let err = pair
        .source_rt
        .start_migration(
            origin_hash,
            pair.source_rt.mesh().inner().node_id(),
            0xDEAD_BEEF_CAFE_F00D, // no session with this node_id
        )
        .await
        .expect_err("unconnected target must fail");
    match err {
        DaemonError::Migration(e) => {
            let msg = format!("{}", e);
            // Either the envelope-seal prerequisite check fails
            // first (peer X25519 static unknown for the
            // unconnected node), or the subsequent send fails
            // (no peer_addr). Both are correct rejections of a
            // migration to a peer we have no session with. The
            // previous `Ok(None)` fallback in `maybe_seal_local_snapshot`
            // would silently proceed with unsealed bytes and surface
            // only the send-side failure; the current stricter
            // semantic may surface the seal-prerequisite error
            // instead.
            assert!(
                msg.contains("unavailable")
                    || msg.contains("send")
                    || msg.contains("peer X25519 static"),
                "expected target-unreachable-flavored error, got: {msg}",
            );
        }
        other => panic!("expected DaemonError::Migration, got {other:?}"),
    }

    // Orchestrator must have rolled back — no stale migration record.
    let mig = pair.source_rt.start_migration(
        origin_hash,
        pair.source_rt.mesh().inner().node_id(),
        0xBEEF_CAFE_FEED_F00D,
    );
    // A second migration attempt should get the same outcome, not
    // AlreadyMigrating — confirming the rollback.
    let err2 = mig.await.expect_err("second attempt still fails");
    assert!(
        !matches!(
            err2,
            DaemonError::Migration(net_sdk::compute::MigrationError::AlreadyMigrating(_))
        ),
        "orchestrator should have cleaned up after the first failure, not held a \
         stale migration record — got {err2:?}",
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn migration_opts_transport_identity_false_skips_envelope() {
    // Smoke test for the opt-out surface: `MigrationOpts {
    // transport_identity: false }` reaches migration Complete on a
    // well-configured 2-node mesh (both sides share the identity
    // out of band, same as the pre-Stage-5b path). The envelope is
    // deliberately absent; the test's goal is to prove the option
    // plumbs through start_migration_with and doesn't regress the
    // existing flow.
    let pair = build_pair().await;
    pair.source_rt
        .register_factory("counter", counter_factory())
        .unwrap();
    pair.target_rt
        .register_factory("counter", counter_factory())
        .unwrap();
    pair.source_rt.start().await.unwrap();
    pair.target_rt.start().await.unwrap();
    pair.source_rt.mesh().inner().start();
    pair.target_rt.mesh().inner().start();
    sleep(Duration::from_millis(100)).await;

    let identity = Identity::generate();
    let origin_hash = identity.keypair().origin_hash();
    let _handle = pair
        .source_rt
        .spawn("counter", identity.clone(), DaemonHostConfig::default())
        .await
        .expect("spawn");
    pair.target_rt
        .register_migration_target_identity("counter", identity, DaemonHostConfig::default())
        .expect("pre-register");

    let mig = pair
        .source_rt
        .start_migration_with(
            origin_hash,
            pair.source_rt.mesh().inner().node_id(),
            pair.target_rt.mesh().inner().node_id(),
            MigrationOpts {
                transport_identity: false,
                ..MigrationOpts::default()
            },
        )
        .await
        .expect("start_migration_with");

    mig.wait_with_timeout(Duration::from_secs(5))
        .await
        .expect("public-identity migration must reach Complete");
    assert_eq!(pair.target_rt.daemon_count(), 1);
}

/// Target has the `kind` factory registered but has NOT pre-
/// registered a factory for the daemon's specific `origin_hash`
/// (no `register_migration_target_identity` / `expect_migration`).
/// The dispatcher must surface `FactoryNotFound` rather than
/// timing out, so the caller can distinguish a configuration bug
/// from a transient failure.
///
/// Previously named `migration_to_registering_target_surfaces_not_ready`
/// — misleading, since the body never exercised the `NotReady`
/// path. The NotReady readiness predicate is covered separately by
/// [`auto_retry_succeeds_after_target_becomes_ready`],
/// [`auto_retry_gives_up_with_not_ready_timeout`], and
/// [`migration_opts_retry_disabled_surfaces_not_ready_verbatim`].
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn migration_to_target_without_origin_factory_surfaces_factory_not_found() {
    let pair = build_pair().await;
    pair.source_rt
        .register_factory("counter", counter_factory())
        .unwrap();
    // Target registers the `kind` factory but never calls
    // `register_migration_target_identity` / `expect_migration`,
    // so `factories.construct(origin_hash)` returns None →
    // dispatcher emits `FactoryNotFound`.
    pair.target_rt
        .register_factory("counter", counter_factory())
        .unwrap();
    pair.source_rt.start().await.unwrap();
    pair.target_rt.start().await.unwrap();
    pair.source_rt.mesh().inner().start();
    pair.target_rt.mesh().inner().start();
    sleep(Duration::from_millis(100)).await;

    let identity = Identity::generate();
    let origin_hash = identity.keypair().origin_hash();
    let _handle = pair
        .source_rt
        .spawn("counter", identity, DaemonHostConfig::default())
        .await
        .expect("spawn");

    let mig = pair
        .source_rt
        .start_migration(
            origin_hash,
            pair.source_rt.mesh().inner().node_id(),
            pair.target_rt.mesh().inner().node_id(),
        )
        .await
        .expect("start_migration");

    let err = mig
        .wait_with_timeout(Duration::from_secs(3))
        .await
        .expect_err("must fail — target has no factory for this origin");
    match err {
        DaemonError::MigrationFailed(reason) => {
            assert_eq!(
                reason,
                MigrationFailureReason::FactoryNotFound,
                "expected FactoryNotFound structured reason, got {reason:?}",
            );
            assert!(!reason.is_retriable());
        }
        other => panic!("expected MigrationFailed, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn auto_retry_succeeds_after_target_becomes_ready() {
    // Force the target to report `NotReady` via the test-only
    // `simulate_not_ready` hook. Source fires a migration;
    // dispatcher emits `NotReady`; SDK backs off + retries. After
    // ~600 ms we flip the target back to ready; next retry
    // succeeds and the migration completes.
    let pair = build_pair().await;
    let Pair {
        source_rt,
        target_rt,
    } = &pair;
    source_rt
        .register_factory("counter", counter_factory())
        .unwrap();
    target_rt
        .register_factory("counter", counter_factory())
        .unwrap();
    source_rt.start().await.unwrap();
    target_rt.start().await.unwrap();
    source_rt.mesh().inner().start();
    target_rt.mesh().inner().start();
    sleep(Duration::from_millis(100)).await;

    // Flip the target's readiness predicate OFF before the source
    // starts the migration.
    target_rt.simulate_not_ready(true);

    let identity = Identity::generate();
    let origin_hash = identity.keypair().origin_hash();
    let _handle = source_rt
        .spawn("counter", identity, DaemonHostConfig::default())
        .await
        .expect("spawn");
    target_rt
        .expect_migration("counter", origin_hash, DaemonHostConfig::default())
        .expect("expect_migration");

    // Background task: clear the simulate flag after a short
    // delay, so a mid-flight retry attempt lands on a ready target.
    let target_rt_bg = target_rt.clone();
    tokio::spawn(async move {
        sleep(Duration::from_millis(700)).await;
        target_rt_bg.simulate_not_ready(false);
    });

    let start = tokio::time::Instant::now();
    let mig = source_rt
        .start_migration_with(
            origin_hash,
            source_rt.mesh().inner().node_id(),
            target_rt.mesh().inner().node_id(),
            MigrationOpts {
                retry_not_ready: Some(Duration::from_secs(10)),
                ..MigrationOpts::default()
            },
        )
        .await
        .expect("start_migration_with");
    mig.wait_with_timeout(Duration::from_secs(15))
        .await
        .expect("migration should eventually succeed after retries");
    let elapsed = start.elapsed();

    // Must have taken at least one backoff cycle (500 ms+).
    assert!(
        elapsed >= Duration::from_millis(500),
        "migration with NotReady retry should wait for at least one backoff; took {elapsed:?}",
    );
    // But shouldn't burn the whole budget.
    assert!(
        elapsed < Duration::from_secs(10),
        "migration took too long — retry loop likely spun past the target recovery; took {elapsed:?}",
    );

    assert_eq!(target_rt.daemon_count(), 1);
    assert_eq!(source_rt.daemon_count(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn auto_retry_gives_up_with_not_ready_timeout() {
    // Target stays in simulated-not-ready forever. Source should
    // exhaust its retry budget and surface `NotReadyTimeout`
    // carrying the attempt count.
    let pair = build_pair().await;
    let Pair {
        source_rt,
        target_rt,
    } = &pair;
    source_rt
        .register_factory("counter", counter_factory())
        .unwrap();
    target_rt
        .register_factory("counter", counter_factory())
        .unwrap();
    source_rt.start().await.unwrap();
    target_rt.start().await.unwrap();
    source_rt.mesh().inner().start();
    target_rt.mesh().inner().start();
    sleep(Duration::from_millis(100)).await;

    target_rt.simulate_not_ready(true);

    let identity = Identity::generate();
    let origin_hash = identity.keypair().origin_hash();
    let _handle = source_rt
        .spawn("counter", identity, DaemonHostConfig::default())
        .await
        .expect("spawn");
    target_rt
        .expect_migration("counter", origin_hash, DaemonHostConfig::default())
        .expect("expect_migration");

    // Tight retry budget so the test runs in reasonable time.
    let mig = source_rt
        .start_migration_with(
            origin_hash,
            source_rt.mesh().inner().node_id(),
            target_rt.mesh().inner().node_id(),
            MigrationOpts {
                retry_not_ready: Some(Duration::from_millis(1_500)),
                ..MigrationOpts::default()
            },
        )
        .await
        .expect("start_migration_with");
    let err = mig
        .wait_with_timeout(Duration::from_secs(5))
        .await
        .expect_err("must give up");
    match err {
        DaemonError::MigrationFailed(MigrationFailureReason::NotReadyTimeout { attempts }) => {
            assert!(
                attempts >= 2,
                "expected at least 2 attempts against a perpetually-not-ready target, got {attempts}",
            );
        }
        other => panic!("expected NotReadyTimeout, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn expect_migration_envelope_supplies_keypair_with_no_placeholder() {
    // Proves the `expect_migration(kind, origin_hash)` path: target
    // pre-registers with ONLY kind + origin_hash — no placeholder
    // keypair, not even a dummy one. The migration snapshot's
    // identity envelope provides the real keypair at restore.
    //
    // This is the end-state API documented in the
    // `DAEMON_IDENTITY_MIGRATION_PLAN.md` Stage 5b seam —
    // previously `register_migration_target_identity` required a
    // matching-origin-hash identity (i.e. the test had to share
    // the same identity between source and target), which was a
    // confusing API and only worked because the envelope happened
    // to override the pre-registered keypair.
    let pair = build_pair().await;
    let Pair {
        source_rt,
        target_rt,
    } = &pair;
    source_rt
        .register_factory("counter", counter_factory())
        .unwrap();
    target_rt
        .register_factory("counter", counter_factory())
        .unwrap();
    source_rt.start().await.unwrap();
    target_rt.start().await.unwrap();
    source_rt.mesh().inner().start();
    target_rt.mesh().inner().start();
    sleep(Duration::from_millis(100)).await;

    // Source spawns with a real identity; target NEVER sees it.
    let real_identity = Identity::generate();
    let origin_hash = real_identity.keypair().origin_hash();
    let _handle = source_rt
        .spawn("counter", real_identity, DaemonHostConfig::default())
        .await
        .expect("spawn on source");

    // Target pre-registers the migration target with just the
    // origin_hash. No identity required.
    target_rt
        .expect_migration("counter", origin_hash, DaemonHostConfig::default())
        .expect("expect_migration");

    // Migration runs with default opts (transport_identity = true),
    // so the envelope carries the real keypair to the target.
    let mig = source_rt
        .start_migration(
            origin_hash,
            source_rt.mesh().inner().node_id(),
            target_rt.mesh().inner().node_id(),
        )
        .await
        .expect("start_migration");
    mig.wait_with_timeout(Duration::from_secs(5))
        .await
        .expect("migration Complete — envelope supplied the keypair");

    assert_eq!(target_rt.daemon_count(), 1);
    assert_eq!(source_rt.daemon_count(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn expect_migration_without_envelope_fails_cleanly() {
    // Inverse of the above: if the source opts out of identity
    // transport, the placeholder factory on target has no keypair
    // to fall back to. Restore must fail cleanly with an
    // identity-transport error rather than silently synthesizing
    // a wrong keypair and rejecting later.
    let pair = build_pair().await;
    let Pair {
        source_rt,
        target_rt,
    } = &pair;
    source_rt
        .register_factory("counter", counter_factory())
        .unwrap();
    target_rt
        .register_factory("counter", counter_factory())
        .unwrap();
    source_rt.start().await.unwrap();
    target_rt.start().await.unwrap();
    source_rt.mesh().inner().start();
    target_rt.mesh().inner().start();
    sleep(Duration::from_millis(100)).await;

    let real_identity = Identity::generate();
    let origin_hash = real_identity.keypair().origin_hash();
    let _handle = source_rt
        .spawn("counter", real_identity, DaemonHostConfig::default())
        .await
        .expect("spawn");

    target_rt
        .expect_migration("counter", origin_hash, DaemonHostConfig::default())
        .expect("expect_migration");

    let mig = source_rt
        .start_migration_with(
            origin_hash,
            source_rt.mesh().inner().node_id(),
            target_rt.mesh().inner().node_id(),
            MigrationOpts {
                transport_identity: false,
                retry_not_ready: None,
            },
        )
        .await
        .expect("start_migration_with");

    let err = mig
        .wait_with_timeout(Duration::from_secs(3))
        .await
        .expect_err("placeholder + no envelope must fail");
    // The reason is wrapped in the dispatcher as a
    // StateFailed("identity envelope open failed: ...") — we just
    // check it's a MigrationFailed-class error surfacing the
    // typed reason, not an opaque abort.
    match err {
        DaemonError::MigrationFailed(reason) => match reason {
            MigrationFailureReason::StateFailed(_) => {}
            other => panic!("expected StateFailed wrapping, got {other:?}"),
        },
        other => panic!("expected MigrationFailed, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn migration_to_node_without_compute_runtime_surfaces_compute_not_supported() {
    // Source has a `DaemonRuntime`; target has a bare `Mesh` with
    // no runtime. Source starts a migration to the target's node
    // id; the mesh's default handler synthesizes a
    // `ComputeNotSupported` reply and the source surfaces the
    // typed reason promptly (not an opaque timeout).
    let source_mesh = MeshBuilder::new("127.0.0.1:0", &PSK)
        .unwrap()
        .build()
        .await
        .unwrap();
    let bare_target = MeshBuilder::new("127.0.0.1:0", &PSK)
        .unwrap()
        .build()
        .await
        .unwrap();
    handshake(&source_mesh, &bare_target).await;
    let source_rt = DaemonRuntime::new(Arc::new(source_mesh));
    source_rt
        .register_factory("counter", counter_factory())
        .unwrap();
    source_rt.start().await.unwrap();
    source_rt.mesh().inner().start();
    bare_target.inner().start();
    sleep(Duration::from_millis(100)).await;

    let identity = Identity::generate();
    let origin_hash = identity.keypair().origin_hash();
    let _handle = source_rt
        .spawn("counter", identity, DaemonHostConfig::default())
        .await
        .expect("spawn");

    let mig = source_rt
        .start_migration_with(
            origin_hash,
            source_rt.mesh().inner().node_id(),
            bare_target.inner().node_id(),
            MigrationOpts {
                retry_not_ready: None,
                ..MigrationOpts::default()
            },
        )
        .await
        .expect("start_migration_with");

    let start = tokio::time::Instant::now();
    let err = mig
        .wait_with_timeout(Duration::from_secs(5))
        .await
        .expect_err("bare mesh must reject with ComputeNotSupported");
    let elapsed = start.elapsed();
    assert!(
        elapsed < Duration::from_secs(2),
        "ComputeNotSupported is terminal + fast — no retry backoff should be burned; took {elapsed:?}",
    );
    match err {
        DaemonError::MigrationFailed(MigrationFailureReason::ComputeNotSupported) => {}
        other => panic!("expected ComputeNotSupported, got {other:?}"),
    }
}

/// `MigrationOpts { retry_not_ready: None }` is the one-shot
/// variant: any failure (retriable or not) surfaces verbatim; the
/// SDK doesn't reinitiate. This test exercises the terminal branch
/// with a `FactoryNotFound` failure — the caller should see the
/// typed reason within a single poll cycle, not after any backoff.
///
/// Previously named `migration_opts_retry_disabled_surfaces_not_ready_immediately`
/// — misleading, since the body tests `FactoryNotFound`, not
/// `NotReady`. The NotReady-plus-retry-disabled combination is
/// covered by
/// [`migration_opts_retry_disabled_surfaces_not_ready_verbatim`].
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn migration_opts_retry_disabled_surfaces_factory_not_found_immediately() {
    let pair = build_pair().await;
    pair.source_rt
        .register_factory("counter", counter_factory())
        .unwrap();
    pair.target_rt
        .register_factory("counter", counter_factory())
        .unwrap();
    pair.source_rt.start().await.unwrap();
    pair.target_rt.start().await.unwrap();
    pair.source_rt.mesh().inner().start();
    pair.target_rt.mesh().inner().start();
    sleep(Duration::from_millis(100)).await;

    let identity = Identity::generate();
    let origin_hash = identity.keypair().origin_hash();
    let _handle = pair
        .source_rt
        .spawn("counter", identity, DaemonHostConfig::default())
        .await
        .expect("spawn");

    // Target has kind registered but no factory-by-origin — so
    // dispatcher emits FactoryNotFound. Confirm retry disabled
    // returns fast (no backoff overhead).
    let mig = pair
        .source_rt
        .start_migration_with(
            origin_hash,
            pair.source_rt.mesh().inner().node_id(),
            pair.target_rt.mesh().inner().node_id(),
            MigrationOpts {
                retry_not_ready: None,
                ..MigrationOpts::default()
            },
        )
        .await
        .expect("start_migration_with");

    let start = tokio::time::Instant::now();
    let err = mig
        .wait_with_timeout(Duration::from_secs(10))
        .await
        .expect_err("terminal failure must surface");
    let elapsed = start.elapsed();
    assert!(
        elapsed < Duration::from_secs(2),
        "with retry disabled, FactoryNotFound must surface within ~first poll, \
         not after retry backoff — took {elapsed:?}",
    );
    match err {
        DaemonError::MigrationFailed(MigrationFailureReason::FactoryNotFound) => {}
        other => panic!("expected FactoryNotFound, got {other:?}"),
    }
}

/// Real NotReady-with-retry-disabled coverage: target simulates
/// NotReady permanently, but the SDK is called with
/// `retry_not_ready: None`. The caller must see
/// `MigrationFailureReason::NotReady` surfaced **verbatim** (not
/// retried, not promoted to `NotReadyTimeout`).
///
/// Exercises the `retry_deadline.is_none()` branch in `wait_until`:
/// `NotReady` is retriable, but with no budget the SDK must return
/// the raw reason instead of looping.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn migration_opts_retry_disabled_surfaces_not_ready_verbatim() {
    let pair = build_pair().await;
    let Pair {
        source_rt,
        target_rt,
    } = &pair;
    source_rt
        .register_factory("counter", counter_factory())
        .unwrap();
    target_rt
        .register_factory("counter", counter_factory())
        .unwrap();
    source_rt.start().await.unwrap();
    target_rt.start().await.unwrap();
    source_rt.mesh().inner().start();
    target_rt.mesh().inner().start();
    sleep(Duration::from_millis(100)).await;

    // Target stays in simulated-not-ready throughout.
    target_rt.simulate_not_ready(true);

    let identity = Identity::generate();
    let origin_hash = identity.keypair().origin_hash();
    let _handle = source_rt
        .spawn("counter", identity, DaemonHostConfig::default())
        .await
        .expect("spawn");
    target_rt
        .expect_migration("counter", origin_hash, DaemonHostConfig::default())
        .expect("expect_migration");

    let mig = source_rt
        .start_migration_with(
            origin_hash,
            source_rt.mesh().inner().node_id(),
            target_rt.mesh().inner().node_id(),
            MigrationOpts {
                retry_not_ready: None,
                ..MigrationOpts::default()
            },
        )
        .await
        .expect("start_migration_with");

    let start = tokio::time::Instant::now();
    let err = mig
        .wait_with_timeout(Duration::from_secs(5))
        .await
        .expect_err("must surface NotReady, not complete");
    let elapsed = start.elapsed();
    // Fast fail: no retry budget means we must surface within a
    // poll cycle of the first inbound MigrationFailed (~100 ms on
    // localhost). 2 s is defensive.
    assert!(
        elapsed < Duration::from_secs(2),
        "retry-disabled NotReady must surface on the first attempt — took {elapsed:?}",
    );
    match err {
        DaemonError::MigrationFailed(MigrationFailureReason::NotReady) => {}
        DaemonError::MigrationFailed(other) => panic!(
            "expected MigrationFailed(NotReady) verbatim, got MigrationFailed({other:?}) — \
             retry-disabled must NOT promote NotReady to NotReadyTimeout",
        ),
        other => panic!("expected MigrationFailed(NotReady), got {other:?}"),
    }
}

/// Regression (Cubic AI P1): a duplicate `spawn` with the same
/// identity used to corrupt the factory registry for the incumbent.
/// The sequence was:
///
/// 1. First `spawn` — factory_registry[origin] = incumbent's factory.
/// 2. Second `spawn` with same identity — factory_registry.insert()
///    silently clobbered the slot. Then `DaemonRegistry::register`
///    correctly rejected the duplicate host. The error-path rollback
///    then called `factory_registry.remove(origin)` — removing the
///    *now-clobbered* slot.
/// 3. The incumbent daemon stayed live, but its factory entry was
///    gone. Subsequent migration attempts of the incumbent would
///    fail at the source-side snapshot path because the dispatcher
///    couldn't rebuild the daemon's restore inputs.
///
/// This test exercises the real migration path end-to-end after a
/// failed duplicate `spawn`: the atomic register fix (factory
/// insert fails fast on collision, never clobbers) keeps the
/// incumbent's slot intact, so migration still completes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn duplicate_spawn_preserves_migratability() {
    let pair = build_pair().await;
    let Pair {
        source_rt,
        target_rt,
    } = &pair;

    source_rt
        .register_factory("counter", counter_factory())
        .unwrap();
    target_rt
        .register_factory("counter", counter_factory())
        .unwrap();
    source_rt.start().await.unwrap();
    target_rt.start().await.unwrap();
    source_rt.mesh().inner().start();
    target_rt.mesh().inner().start();
    sleep(Duration::from_millis(100)).await;

    let identity = Identity::generate();
    let origin_hash = identity.keypair().origin_hash();

    // First spawn — the incumbent we're going to migrate later.
    let handle = source_rt
        .spawn("counter", identity.clone(), DaemonHostConfig::default())
        .await
        .expect("first spawn on source");
    for i in 1..=2u64 {
        source_rt
            .deliver(handle.origin_hash, &make_event(origin_hash, i, b"pre"))
            .expect("deliver");
    }

    // Duplicate spawn: same identity. Must fail at the atomic
    // factory_registry step, without touching the incumbent's slot.
    let err = source_rt
        .spawn("counter", identity.clone(), DaemonHostConfig::default())
        .await
        .expect_err("duplicate spawn must fail");
    match err {
        DaemonError::Core(CoreDaemonError::ProcessFailed(ref m)) => {
            assert!(m.contains("already registered"), "got {m:?}");
        }
        other => panic!("expected Core(ProcessFailed), got {other:?}"),
    }
    assert_eq!(
        source_rt.daemon_count(),
        1,
        "incumbent must still be in the registry after a rejected duplicate",
    );

    // Target pre-registers so envelope transport works. Pre-fix,
    // this step would succeed here too — the regression manifests
    // on the SOURCE side at snapshot-construction time.
    target_rt
        .register_migration_target_identity(
            "counter",
            identity.clone(),
            DaemonHostConfig::default(),
        )
        .expect("target identity");

    // Now migrate the incumbent. Pre-fix, the source's snapshot path
    // would fail here because the rollback had stripped the factory
    // entry. Post-fix, this completes normally.
    let mig = source_rt
        .start_migration(
            handle.origin_hash,
            source_rt.mesh().inner().node_id(),
            target_rt.mesh().inner().node_id(),
        )
        .await
        .expect("start_migration after failed duplicate spawn");
    mig.wait_with_timeout(Duration::from_secs(5))
        .await
        .expect("migration reaches Complete — incumbent's factory entry survived");

    // State survived: counter was at 2 on source, next event on
    // target yields 3.
    let outputs = target_rt
        .deliver(origin_hash, &make_event(origin_hash, 3, b"post"))
        .expect("deliver on target");
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&outputs[0].payload);
    assert_eq!(
        u64::from_le_bytes(bytes),
        3,
        "incumbent's state must survive a rejected duplicate-spawn attempt",
    );
    assert_eq!(source_rt.daemon_count(), 0);
    assert_eq!(target_rt.daemon_count(), 1);
}

/// Regression (Cubic-AI P1 on mesh.rs self-loopback): the 2-node
/// local-source topology (A = orchestrator = source, B = target)
/// runs the tail of the migration entirely through self-loopback
/// on A:
///
/// ```text
///   A: on_replay_complete  → CutoverNotify    to self   (loopback 1)
///   A: on_cutover          → CleanupComplete  to self   (loopback 2)
///   A: on_cleanup_complete → ActivateTarget   to B      (REMOTE)
///   B: activate            → ActivateAck      to A      (remote)
///   A: on_activate_ack     → orchestrator record removed
/// ```
///
/// The earlier mesh.rs implementation `tokio::spawn`ed each
/// loopback with a fire-and-forget closure that **dropped**
/// `handle_message`'s return value. Loopback 2's output
/// (`ActivateTarget` to B) was discarded, B never ran `activate`,
/// and A's orchestrator record never reached its terminal state —
/// the migration looked complete (phase=Complete stayed pinned)
/// but never fully wound down.
///
/// The fix replaces the fire-and-forget spawn with an in-place
/// BFS queue that preserves every outbound message, including
/// remote-bound follow-ups produced by a loopback chain.
///
/// This test asserts the record is **fully removed** after the
/// migration (post-`ActivateAck`), which is the only observable
/// that distinguishes "phase reached Complete" from "record
/// genuinely cleaned up." A pre-fix run leaves
/// `migration_phase(origin) == Some(Complete)`; post-fix it is
/// `None`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn local_source_migration_drives_full_chain_through_self_loopback() {
    let pair = build_pair().await;
    let Pair {
        source_rt,
        target_rt,
    } = &pair;
    source_rt
        .register_factory("counter", counter_factory())
        .unwrap();
    target_rt
        .register_factory("counter", counter_factory())
        .unwrap();
    source_rt.start().await.unwrap();
    target_rt.start().await.unwrap();
    source_rt.mesh().inner().start();
    target_rt.mesh().inner().start();
    sleep(Duration::from_millis(100)).await;

    let identity = Identity::generate();
    let origin_hash = identity.keypair().origin_hash();
    let handle = source_rt
        .spawn("counter", identity.clone(), DaemonHostConfig::default())
        .await
        .expect("spawn on source");
    target_rt
        .register_migration_target_identity("counter", identity, DaemonHostConfig::default())
        .expect("target identity");

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
        .expect("migration reaches Complete");

    // Give the dispatcher's tail-end ActivateAck a beat to land —
    // `wait` returns as soon as it observes the orchestrator record
    // disappear, which can race with the final `on_activate_ack`
    // removal under current_thread scheduling. 200 ms is plenty.
    sleep(Duration::from_millis(200)).await;

    assert_eq!(
        source_rt.migration_phase(origin_hash),
        None,
        "orchestrator record must be removed by ActivateAck — a pre-fix \
         run would leave phase=Complete pinned because ActivateTarget \
         (produced inside self-loopback) was silently dropped",
    );
    assert_eq!(source_rt.daemon_count(), 0);
    assert_eq!(target_rt.daemon_count(), 1);
}

/// Regression (Cubic-AI P1 on `MigrationHandle::wait`): the doc
/// advertised "block until terminal state" but silently enforced
/// a 60-second ceiling via `wait_with_timeout(60s)` — a migration
/// that legitimately exceeded 60s (large snapshot on a saturated
/// link) would be aborted mid-flight.
///
/// The fix splits the two: `wait()` takes no deadline;
/// `wait_with_timeout(d)` takes one. This test pins virtual time,
/// starts a migration that stalls at phase=Snapshot because the
/// target's receive loop is never started, and advances 120 s of
/// virtual time — well past the old 60 s ceiling. Under the
/// pre-fix code the spawned `wait()` task would have resolved
/// with `Err(StateFailed("timed out"))` somewhere in the advance
/// window. Post-fix, it stays pending.
#[tokio::test(flavor = "current_thread")]
async fn wait_without_timeout_survives_120_virtual_seconds() {
    let pair = build_pair().await;
    let Pair {
        source_rt,
        target_rt,
    } = &pair;
    source_rt
        .register_factory("counter", counter_factory())
        .unwrap();
    target_rt
        .register_factory("counter", counter_factory())
        .unwrap();
    source_rt.start().await.unwrap();
    target_rt.start().await.unwrap();

    // Start ONLY A's receive loop. B's socket still buffers
    // inbound, but nothing is dispatched — so the `SnapshotReady`
    // A sends below never drives B into the restore path, and the
    // orchestrator on A sits at phase=Snapshot indefinitely.
    source_rt.mesh().inner().start();

    let identity = Identity::generate();
    let origin_hash = identity.keypair().origin_hash();
    let handle = source_rt
        .spawn("counter", identity.clone(), DaemonHostConfig::default())
        .await
        .expect("spawn");
    target_rt
        .register_migration_target_identity("counter", identity, DaemonHostConfig::default())
        .expect("target identity");

    let mig = source_rt
        .start_migration(
            handle.origin_hash,
            source_rt.mesh().inner().node_id(),
            target_rt.mesh().inner().node_id(),
        )
        .await
        .expect("start_migration");

    // Sanity: record exists at a pre-terminal phase.
    assert!(
        matches!(
            source_rt.migration_phase(origin_hash),
            Some(MigrationPhase::Snapshot | MigrationPhase::Transfer)
        ),
        "fixture: migration should be stuck at a pre-terminal phase",
    );

    // Pause virtual time, spawn wait(), advance 120 s — past the
    // pre-fix hidden 60 s ceiling. yield_now interleaves the
    // spawn/advance with the waiter's poll loop.
    tokio::time::pause();
    let wait_task = tokio::spawn(mig.wait());
    for _ in 0..5 {
        tokio::task::yield_now().await;
    }
    tokio::time::advance(Duration::from_secs(120)).await;
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }

    assert!(
        !wait_task.is_finished(),
        "wait() must not abort under a hidden timeout — pre-fix, 120s of \
         virtual time would have tripped the 60s ceiling and aborted the \
         migration. Post-fix, wait() blocks until the migration itself \
         reaches a terminal state.",
    );
    wait_task.abort();

    // Resume real time so test teardown can complete without
    // wedging paused sleeps inside the mesh's shutdown path.
    tokio::time::resume();
}

use net_sdk::compute::MigrationPhase;

/// Regression (Cubic-AI P1): `transport_identity: true` is a
/// strict opt-in. When the local mesh cannot supply the target's
/// X25519 static (e.g. this node was the NKpsk0 responder, so
/// `snow` never surfaced the initiator's static), the previous
/// `maybe_seal_local_snapshot` returned `Ok(None)` and
/// `start_migration_with` proceeded with an **unsealed**
/// snapshot — silently downgrading the migration below what the
/// caller opted into.
///
/// The fix treats "peer X25519 unknown" and "daemon keypair
/// absent" as terminal errors under `transport_identity: true`.
/// Callers who want to proceed unsealed in NKpsk0-responder
/// topologies must now set `transport_identity: false`
/// explicitly.
///
/// Fixture: the default `build_pair` makes node A the initiator
/// and node B the responder. On B, `peer_static_x25519(A)`
/// returns `None` (the documented NKpsk0 limitation exercised in
/// `capability_broadcast::peer_static_x25519_returns_peer_noise_pubkey_after_handshake`).
/// Spawning on B and attempting to migrate B→A with
/// `transport_identity: true` must fail with the seal-prerequisite
/// error, not complete silently.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn transport_identity_strict_rejects_when_peer_static_unknown() {
    let pair = build_pair().await;
    // Responder is `target_rt` (B) in the fixture; flip roles
    // so the responder is the migration source.
    let source_rt = &pair.target_rt;
    let target_rt = &pair.source_rt;

    source_rt
        .register_factory("counter", counter_factory())
        .unwrap();
    target_rt
        .register_factory("counter", counter_factory())
        .unwrap();
    source_rt.start().await.unwrap();
    target_rt.start().await.unwrap();
    source_rt.mesh().inner().start();
    target_rt.mesh().inner().start();
    sleep(Duration::from_millis(100)).await;

    let target_node_id = target_rt.mesh().inner().node_id();
    let source_node_id = source_rt.mesh().inner().node_id();

    // Sanity: responder side (source) indeed cannot see the
    // initiator side's (target) X25519 static. If this fails,
    // the fixture doesn't reflect the NKpsk0 limitation and
    // the test below isn't exercising the right scenario.
    assert!(
        source_rt
            .mesh()
            .inner()
            .peer_static_x25519(target_node_id)
            .is_none(),
        "fixture: responder should see None for initiator's X25519 static",
    );

    let identity = Identity::generate();
    let origin_hash = identity.keypair().origin_hash();
    let _handle = source_rt
        .spawn("counter", identity.clone(), DaemonHostConfig::default())
        .await
        .expect("spawn on responder");
    target_rt
        .expect_migration("counter", origin_hash, DaemonHostConfig::default())
        .expect("expect_migration on initiator");

    // Strict opt-in. Pre-fix: `Ok(None)` silent downgrade →
    // migration proceeds unsealed → either completes incorrectly
    // or fails later for a misleading reason. Post-fix: fails
    // here with the seal-prerequisite error.
    let err = source_rt
        .start_migration_with(
            origin_hash,
            source_node_id,
            target_node_id,
            MigrationOpts {
                transport_identity: true,
                retry_not_ready: None,
            },
        )
        .await
        .expect_err(
            "transport_identity: true must reject when peer X25519 static is unknown — \
             silently proceeding unsealed breaks the caller's opt-in guarantee",
        );
    let msg = format!("{err}");
    assert!(
        msg.contains("peer X25519 static"),
        "expected seal-prerequisite error mentioning peer X25519 static, got: {msg}",
    );
    // Post-completion: no daemon landed on target; source still
    // has its own daemon; orchestrator record was rolled back.
    assert_eq!(target_rt.daemon_count(), 0);
    assert_eq!(source_rt.daemon_count(), 1);
}

/// Complement to the strict rejection above: opting out via
/// `transport_identity: false` must still proceed successfully
/// in the NKpsk0-responder topology, using the
/// `register_migration_target_identity` fallback keypair on the
/// target.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn transport_identity_false_proceeds_unsealed_under_nkpsk0_responder_source() {
    let pair = build_pair().await;
    let source_rt = &pair.target_rt;
    let target_rt = &pair.source_rt;

    source_rt
        .register_factory("counter", counter_factory())
        .unwrap();
    target_rt
        .register_factory("counter", counter_factory())
        .unwrap();
    source_rt.start().await.unwrap();
    target_rt.start().await.unwrap();
    source_rt.mesh().inner().start();
    target_rt.mesh().inner().start();
    sleep(Duration::from_millis(100)).await;

    let identity = Identity::generate();
    let origin_hash = identity.keypair().origin_hash();
    let _handle = source_rt
        .spawn("counter", identity.clone(), DaemonHostConfig::default())
        .await
        .expect("spawn");
    // Opt-out requires the target to pre-register the identity
    // so the factory has a keypair to restore under.
    target_rt
        .register_migration_target_identity("counter", identity, DaemonHostConfig::default())
        .expect("target identity");

    let mig = source_rt
        .start_migration_with(
            origin_hash,
            source_rt.mesh().inner().node_id(),
            target_rt.mesh().inner().node_id(),
            MigrationOpts {
                transport_identity: false,
                retry_not_ready: None,
            },
        )
        .await
        .expect("start_migration with opt-out");
    mig.wait_with_timeout(Duration::from_secs(5))
        .await
        .expect("migration reaches Complete under opt-out");
    assert_eq!(source_rt.daemon_count(), 0);
    assert_eq!(target_rt.daemon_count(), 1);
}

/// Regression (Cubic-AI P2 on `recent_failures` cache hygiene): the
/// dispatcher's failure callback populates `recent_failures[origin]`
/// on every inbound `MigrationFailed`. `MigrationHandle::wait`
/// consumes the entry in its None-status branch. But if the caller
/// drops the handle without calling `wait` (or never gets to
/// `wait`), the entry sits forever — and the *next* migration for
/// the same `origin_hash` silently pops it, mis-reporting the fresh
/// migration's outcome as the stale reason.
///
/// Fix: `start_migration_with` clears `recent_failures[origin]` at
/// the top of every new attempt, so no stale reason from a prior
/// abandoned attempt can leak.
///
/// This is a deterministic unit-style check against the cache's
/// public lifecycle, using the hidden `inject_migration_failure` /
/// `peek_migration_failure` test hooks. Exercising the same
/// property end-to-end through an actual abandoned migration races
/// against the dispatcher's handling of the inbound
/// `MigrationFailed`, so this direct shape is more reliable.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn start_migration_clears_stale_failure_cache_entry() {
    let pair = build_pair().await;
    let Pair {
        source_rt,
        target_rt,
    } = &pair;
    source_rt
        .register_factory("counter", counter_factory())
        .unwrap();
    target_rt
        .register_factory("counter", counter_factory())
        .unwrap();
    source_rt.start().await.unwrap();
    target_rt.start().await.unwrap();
    source_rt.mesh().inner().start();
    target_rt.mesh().inner().start();
    sleep(Duration::from_millis(100)).await;

    let identity = Identity::generate();
    let origin_hash = identity.keypair().origin_hash();
    let handle = source_rt
        .spawn("counter", identity.clone(), DaemonHostConfig::default())
        .await
        .expect("spawn");
    target_rt
        .register_migration_target_identity("counter", identity, DaemonHostConfig::default())
        .expect("target identity");

    // Stage a stale failure as if a prior abandoned migration left
    // it behind. No actual losing migration needed.
    source_rt.inject_migration_failure(origin_hash, MigrationFailureReason::NotReady);
    assert!(
        matches!(
            source_rt.peek_migration_failure(origin_hash),
            Some(MigrationFailureReason::NotReady)
        ),
        "fixture: stale failure must be present before start_migration",
    );

    // Call start_migration — must clear the stale entry as its
    // first act, BEFORE any wire work. After this call returns,
    // the fresh migration is in flight; the stale entry is gone.
    let mig = source_rt
        .start_migration(
            handle.origin_hash,
            source_rt.mesh().inner().node_id(),
            target_rt.mesh().inner().node_id(),
        )
        .await
        .expect("start_migration");

    // Window: the fresh migration may complete before this peek
    // runs, in which case its own outcome (success) leaves the
    // slot empty. Either way, the stale `NotReady` must no longer
    // be observable — that's the regression guard.
    let now = source_rt.peek_migration_failure(origin_hash);
    assert!(
        !matches!(now, Some(MigrationFailureReason::NotReady)),
        "start_migration_with must drop stale cache entry; still saw {now:?}",
    );

    // End-to-end: fresh migration should complete normally and
    // `wait` must return Ok. Under the pre-fix code this would
    // surface the stale `NotReady` even though the fresh migration
    // actually succeeded.
    mig.wait_with_timeout(Duration::from_secs(5))
        .await
        .expect("fresh migration reaches Complete without stale failure leaking");
    assert_eq!(target_rt.daemon_count(), 1);
    assert_eq!(source_rt.daemon_count(), 0);
    // Post-completion, the cache must be empty for this origin.
    assert_eq!(source_rt.peek_migration_failure(origin_hash), None);
}

/// Shutdown drains the failure cache — bounds memory footprint
/// after a runtime has been torn down.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_clears_failure_cache() {
    let pair = build_pair().await;
    let rt = &pair.source_rt;
    rt.register_factory("counter", counter_factory()).unwrap();
    rt.start().await.unwrap();

    // Inject a handful of stale entries.
    for origin in 0..16u64 {
        rt.inject_migration_failure(origin, MigrationFailureReason::NotReady);
    }
    assert!(rt.peek_migration_failure(0).is_some());

    rt.shutdown().await.expect("shutdown");

    for origin in 0..16u64 {
        assert_eq!(
            rt.peek_migration_failure(origin),
            None,
            "shutdown must drop cache entry for {origin:#x}",
        );
    }
}

// ---- Helpers -----------------------------------------------------------

fn make_event(origin_hash: u64, seq: u64, payload: &'static [u8]) -> CausalEvent {
    use net::adapter::net::state::causal::CausalLink;
    CausalEvent {
        link: CausalLink {
            origin_hash,
            horizon_encoded: 0,
            sequence: seq,
            parent_hash: 0,
        },
        payload: Bytes::from_static(payload),
        received_at: 0,
    }
}

// ---- Regression pins ---------------------------------------------------

/// Pins the technique used by `DaemonRuntime::build_migration_handler`:
/// capture `Handle::current()` once inside a tokio runtime, then use
/// `handle.spawn(...)` from a non-runtime thread.
///
/// The substrate's migration handler invokes the SDK-supplied
/// `PostRestoreCallback` / `PreCleanupCallback` synchronously and
/// expects the hook to `spawn` the actual work. When the dispatch path
/// is driven from a non-tokio thread (FFI trampoline, Go cgo,
/// synchronous `block_on` callers), bare `tokio::spawn` panics with
/// "there is no reactor running". The fix captures the runtime handle
/// at handler-construction time; this test catches a regression that
/// swaps it back to `tokio::spawn`.
///
/// Tracked in `TestMigration_EndToEndCounterSurvivesAToB` end-to-end on
/// the Go side — this Rust-side pin gives a focused signal so the next
/// `build_migration_handler` refactor catches the regression without
/// waiting for the cgo suite.
#[tokio::test]
async fn captured_handle_spawns_from_non_runtime_thread() {
    let handle = tokio::runtime::Handle::current();
    let (tx, rx) = tokio::sync::oneshot::channel::<u8>();
    let join = std::thread::spawn(move || {
        // No tokio runtime is registered on this OS thread; a bare
        // `tokio::spawn(...)` here would panic. The captured `Handle`
        // routes the spawn onto the runtime that owned `Handle::current()`
        // at capture time.
        handle.spawn(async move {
            let _ = tx.send(42);
        });
    });
    join.join().expect("os thread joined");
    assert_eq!(rx.await.expect("oneshot recv"), 42);
}
