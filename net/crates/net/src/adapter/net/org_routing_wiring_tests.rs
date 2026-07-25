//! OLB-2B-E3c: the production routing-plane wiring witnesses.
//!
//! Included from `mesh.rs`, so it sees the node's private fields — which is what
//! lets these assert on the real drain lease and the real task handle rather than
//! on a proxy.

use super::*;
use crate::adapter::net::behavior::org_grant::CapabilityAuthorityId;
use crate::adapter::net::behavior::org_routing_registry::{
    PrivateAudienceScope, SlotKey, SlotSource, SourceCommitPin, SourceSnapshot,
};
use crate::adapter::net::behavior::org_scoped_ingest::CapabilityAudienceScope;
use crate::adapter::net::behavior::org_scoped_store::{
    PrivateCapabilityProvider, PrivateDiscoveryDrains, PrivateDiscoveryStream,
};

async fn node() -> Arc<MeshNode> {
    let addr: SocketAddr = "127.0.0.1:0".parse().expect("addr");
    let cfg = MeshNodeConfig::new(addr, [0x77u8; 32]);
    Arc::new(
        MeshNode::new(EntityKeypair::generate(), cfg)
            .await
            .expect("MeshNode::new"),
    )
}

fn owner_scope(seed: u8) -> PrivateAudienceScope {
    PrivateAudienceScope::new(CapabilityAudienceScope::Owner {
        org_id: crate::adapter::net::behavior::org::OrgId::from_bytes([seed; 32]),
        audience_handle: [seed; 32],
    })
    .expect("owner scopes are private")
}

fn slot(seed: u8, tag: &str) -> SlotKey {
    SlotKey {
        scope: owner_scope(seed),
        capability: CapabilityAuthorityId::for_tag(tag),
    }
}

/// Poll until `f` holds, yielding to the runtime between attempts.
async fn until(mut f: impl FnMut() -> bool) -> bool {
    for _ in 0..2000 {
        if f() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    false
}

/// (1) Node startup mints EXACTLY ONE global destructive drain, a second
/// production start is refused loudly rather than silently fencing, and the
/// OWNER lease is never claimed.
#[tokio::test]
async fn startup_mints_exactly_one_global_drain_and_leaves_owner_unclaimed() {
    let node = node().await;
    {
        let probe = PrivateDiscoveryDrains::new(node.scoped_discovery.clone());
        assert!(
            probe.mint(PrivateDiscoveryStream::Global).is_some(),
            "an unstarted node holds no global lease"
        );
    }

    node.start();
    assert!(
        until(|| node.routing_task.lock().is_some()).await,
        "the supervisor task must be recorded for joining"
    );
    assert!(
        until(|| node.org_routing_supervision_counts().0 >= 1).await,
        "an incarnation must start"
    );

    {
        let rival = PrivateDiscoveryDrains::new(node.scoped_discovery.clone());
        assert!(
            rival.mint(PrivateDiscoveryStream::Global).is_none(),
            "the running actor holds the exclusive global drain"
        );
        assert!(
            rival.mint(PrivateDiscoveryStream::Owner).is_some(),
            "the owner stream stays unclaimed for the leader track"
        );
    }

    // A duplicate start is refused BEFORE constructing a supervisor. The drain
    // lease would refuse the second claim anyway — but a supervisor that fails to
    // mint fences the SHARED routing health and overwrites `routing_task`, so
    // without the latch a stray second start would black-hole a perfectly healthy
    // routing plane AND lose the handle shutdown must join.
    assert!(until(|| node.org_routing_ready()).await, "healthy first");
    let started = node.org_routing_supervision_counts().0;
    node.start_org_routing_supervisor();
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        node.org_routing_supervision_counts().0,
        started,
        "there is exactly one production supervisor construction path"
    );
    assert!(
        node.org_routing_ready(),
        "a refused duplicate start must not fence the live routing plane"
    );

    let _ = node.shutdown().await;
    assert!(
        node.routing_task.lock().is_none(),
        "the handle shutdown joined was the LIVE one, not a duplicate's"
    );
}

/// (2) The private-discovery dirty stream reaches the REAL registry: the actor
/// drives `DirtyApply`, health advances only on a completed recapture, and a
/// demanded slot is actually reconstructed through the production source.
#[tokio::test]
async fn the_dirty_stream_reaches_the_real_registry() {
    let node = node().await;
    node.start();

    assert!(
        until(|| node.org_routing_ready()).await,
        "the mint's RebuildAll must complete a recapture and publish Healthy"
    );

    // First demand is REGISTRY work — private discovery cannot signal it.
    let family = node.org_routing_family().expect("family");
    let key = slot(1, "nrpc:e3c");
    let held = family.demand(key.clone()).expect("demand");

    assert!(
        until(|| node.org_routing_slots() == (1, 0)).await,
        "the actor must reconcile the demanded slot, not merely count wakes"
    );
    let facts = node
        .org_routing_base_facts(&key)
        .expect("rebuilt through the production source");
    assert_eq!(
        facts.source_generation,
        node.scoped_discovery.lock().revision(),
        "facts carry the query-visible generation they were committed against"
    );
    assert!(
        node.org_routing_reconciliation_counts()[0] >= 1,
        "a real installation happened"
    );
    assert_eq!(node.org_routing_unserved_scope_count(), 0);

    drop(held);
    let _ = node.shutdown().await;
}

/// (3) Shutdown signals the supervisor, AWAITS it, and only then returns — so
/// the actor has fenced and the exclusive drain has dropped. A successor mint is
/// possible only after that join.
#[tokio::test]
async fn shutdown_joins_the_routing_task_before_returning() {
    let node = node().await;
    node.start();
    assert!(until(|| node.org_routing_ready()).await, "healthy");

    {
        let rival = PrivateDiscoveryDrains::new(node.scoped_discovery.clone());
        assert!(
            rival.mint(PrivateDiscoveryStream::Global).is_none(),
            "the lease is held while the actor runs"
        );
    }

    let _ = node.shutdown().await;

    assert!(
        node.routing_task.lock().is_none(),
        "shutdown must consume the routing task handle"
    );
    assert!(
        !node.org_routing_ready(),
        "the actor fenced routing health on its way out"
    );
    let rival = PrivateDiscoveryDrains::new(node.scoped_discovery.clone());
    assert!(
        rival.mint(PrivateDiscoveryStream::Global).is_some(),
        "the drain dropped BEFORE shutdown returned, so a successor can mint"
    );
}

type BuildHook = Box<dyn Fn() + Send + Sync>;

/// Delegates entirely to the production [`ScopedSlotSource`]; the only
/// difference is a pause inside reconstruction, so a rival mutation can be landed
/// exactly there.
struct PausingSource {
    inner: ScopedSlotSource,
    during_build: Arc<parking_lot::Mutex<Option<BuildHook>>>,
}

struct PausingSnapshot {
    inner: Box<dyn SourceSnapshot>,
    during_build: Arc<parking_lot::Mutex<Option<BuildHook>>>,
}

impl SourceSnapshot for PausingSnapshot {
    fn generation(&self) -> u64 {
        self.inner.generation()
    }
    fn providers(&self, key: &SlotKey) -> Vec<PrivateCapabilityProvider> {
        let hook = self.during_build.lock().take();
        if let Some(hook) = hook {
            hook();
        }
        self.inner.providers(key)
    }
}

impl SlotSource for PausingSource {
    fn snapshot(&self, keys: &[SlotKey]) -> Box<dyn SourceSnapshot> {
        Box::new(PausingSnapshot {
            inner: self.inner.snapshot(keys),
            during_build: self.during_build.clone(),
        })
    }
    fn pin_if_current(&self, expected_generation: u64) -> Option<Box<dyn SourceCommitPin + '_>> {
        self.inner.pin_if_current(expected_generation)
    }
}

/// (4) Production snapshot discipline, driven through the REAL
/// [`ScopedSlotSource`]: a mutation completes while reconstruction is paused —
/// which it could not do if the publication gate were held across the quantum —
/// the stale snapshot's commit pin then refuses, nothing installs, the exact
/// selected identity is requeued, registry work is marked, and the successor
/// pass installs facts at the current generation.
#[tokio::test]
async fn a_mutation_during_reconstruction_defeats_the_production_commit_pin() {
    use crate::adapter::net::behavior::org_routing::{
        ApplyOutcome, ApplyRequest, DirtyApply, RegistryWork,
    };
    use crate::adapter::net::behavior::org_routing_registry::NodeOrgRoutingRegistry;
    use crate::adapter::net::behavior::org_scoped_store::{
        DirtyCapabilities, PrivateDiscoveryChangeBatch,
    };

    let node = node().await;
    let scoped = node.scoped_discovery.clone();
    let publication = node.scoped_publication.clone();
    let during_build: Arc<parking_lot::Mutex<Option<BuildHook>>> = Arc::default();

    let work: Arc<RegistryWork> = Arc::default();
    let registry = NodeOrgRoutingRegistry::new(
        Arc::new(PausingSource {
            inner: ScopedSlotSource {
                scoped_discovery: scoped.clone(),
                publication: publication.clone(),
                org_revocation: node.org_revocation.clone(),
                unserved_scope: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            },
            during_build: during_build.clone(),
        }),
        work.clone(),
        Arc::default(),
    );
    registry.activate_incarnation(1);

    let family = registry.new_family().expect("family");
    let key = slot(2, "nrpc:pinned");
    let _held = family.demand(key.clone()).expect("demand");

    let landed = Arc::new(AtomicBool::new(false));
    {
        let scoped = scoped.clone();
        let publication = publication.clone();
        let landed = landed.clone();
        *during_build.lock() = Some(Box::new(move || {
            // A REAL mutation through the REAL publication gate. If the registry
            // held that gate across reconstruction this would DEADLOCK rather
            // than complete.
            publication.gated_commit(&scoped, |s| {
                s.advance_query_visible_generation_for_test(CapabilityAuthorityId::for_tag(
                    "nrpc:other",
                ))
            });
            landed.store(true, Ordering::Release);
        }));
    }

    let before = scoped.lock().revision();
    let outcome = registry.apply(
        1,
        ApplyRequest {
            batch: PrivateDiscoveryChangeBatch {
                generation: before,
                dirty: DirtyCapabilities::Clean,
            },
            registry_work: true,
        },
    );

    assert!(
        landed.load(Ordering::Acquire),
        "the rival mutation must have completed DURING reconstruction"
    );
    assert!(
        scoped.lock().revision() > before,
        "and must have advanced the query-visible generation"
    );
    assert_eq!(
        outcome,
        ApplyOutcome::Superseded,
        "the stale snapshot's commit pin must refuse"
    );
    assert!(
        registry.base_facts(&key).is_none(),
        "nothing from the stale snapshot installed"
    );
    assert_eq!(
        registry.pending_slots(),
        1,
        "the exact selected identity was requeued"
    );

    let current = scoped.lock().revision();
    let outcome = registry.apply(
        1,
        ApplyRequest {
            batch: PrivateDiscoveryChangeBatch {
                generation: current,
                dirty: DirtyCapabilities::Clean,
            },
            registry_work: true,
        },
    );
    assert!(
        matches!(outcome, ApplyOutcome::Current { .. }),
        "the successor pass completes"
    );
    assert_eq!(
        registry
            .base_facts(&key)
            .expect("installed")
            .source_generation,
        current,
        "at the CURRENT generation"
    );
}

/// The production source answers EXACTLY one authority scope, and COUNTS a scope
/// it does not serve rather than answering it as "no providers".
#[tokio::test]
async fn the_production_source_is_scope_exact_and_counts_unserved_scopes() {
    let node = node().await;
    let source = ScopedSlotSource {
        scoped_discovery: node.scoped_discovery.clone(),
        publication: node.scoped_publication.clone(),
        org_revocation: node.org_revocation.clone(),
        unserved_scope: node.routing_unserved_scope.clone(),
    };

    let grant_key = SlotKey {
        scope: PrivateAudienceScope::new(CapabilityAudienceScope::Grant {
            grant_id: [9u8; 32],
            audience_handle: [9u8; 32],
        })
        .expect("grant scopes are private"),
        capability: CapabilityAuthorityId::for_tag("nrpc:g"),
    };
    let owner_key = slot(3, "nrpc:o");

    let snapshot = source.snapshot(&[owner_key.clone(), grant_key.clone()]);
    assert!(snapshot.providers(&owner_key).is_empty());
    assert!(snapshot.providers(&grant_key).is_empty());
    assert_eq!(
        node.org_routing_unserved_scope_count(),
        1,
        "the unserved grant scope is COUNTED, not silently empty"
    );

    // The snapshot holds no source lock: the gate is free while it is alive.
    let generation = snapshot.generation();
    assert!(
        source.pin_if_current(generation.wrapping_add(1)).is_none(),
        "a generation the source has left is refused"
    );
    assert!(
        source.pin_if_current(generation).is_some(),
        "the live generation is accepted while the snapshot is still held"
    );
}
