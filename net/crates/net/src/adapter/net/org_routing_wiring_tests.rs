//! OLB-2B-E3c: the production routing-plane wiring witnesses.
//!
//! Included from `mesh.rs`, so it sees the node's private fields — which is what
//! lets these assert on the real drain lease and the real task handle rather than
//! on a proxy.

use super::*;
use crate::adapter::net::behavior::org_grant::CapabilityAuthorityId;
use crate::adapter::net::behavior::org_routing_registry::{
    PrivateAudienceScope, SlotKey, SlotSource, SourceCommitPin, SourceFacts, SourceSnapshot,
    SourceToken,
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

/// A node that has never been started.
async fn node_unstarted() -> Arc<MeshNode> {
    node().await
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
    fn token(&self) -> SourceToken {
        self.inner.token()
    }
    fn providers(&self, key: &SlotKey) -> SourceFacts {
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
    fn pin_if_current(&self, expected: &SourceToken) -> Option<Box<dyn SourceCommitPin + '_>> {
        self.inner.pin_if_current(expected)
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
    assert!(
        matches!(snapshot.providers(&owner_key), SourceFacts::Served(ref p) if p.is_empty()),
        "an owner scope with no rows is SERVED with exact empty evidence"
    );
    assert!(
        matches!(snapshot.providers(&grant_key), SourceFacts::Unserved),
        "an unsupported scope is UNSERVED, not authoritatively empty"
    );
    assert_eq!(
        node.org_routing_unserved_scope_count(),
        1,
        "the unserved grant scope is COUNTED, not silently empty"
    );

    // The snapshot holds no source lock: the gate is free while it is alive.
    let token = snapshot.token();
    assert!(
        source
            .pin_if_current(&SourceToken::new(vec![u64::MAX]))
            .is_none(),
        "a token the source has left is refused"
    );
    assert!(
        source.pin_if_current(&token).is_some(),
        "the live token is accepted while the snapshot is still held"
    );
}

/// (6) Shutdown cannot slip between the supervisor spawn and the publication of
/// its handle.
///
/// The one-start latch does NOT close this: it admits a single starter, it does
/// not order that starter against shutdown. Without the slot lock spanning the
/// shutdown check, the spawn and the publication, shutdown takes `None`, returns,
/// and an unjoined supervisor outlives teardown.
/// (6) Shutdown cannot slip between the supervisor spawn and the publication of
/// its handle.
///
/// The one-start latch does NOT close this: it admits a single starter, it does
/// not order that starter against shutdown. Without the slot lock spanning the
/// shutdown check, the spawn AND the publication, a shutdown landing in that gap
/// takes `None`, returns, and leaves an unjoined supervisor alive after teardown
/// completed (Kyra OLB-2B-E3c).
///
/// Asserted from INSIDE the window, which is what makes it deterministic: the
/// slot is provably held there, so any joiner must block rather than observe an
/// empty slot. A `try_lock` that succeeded would be exactly the race.
#[tokio::test]
async fn shutdown_cannot_pass_between_routing_spawn_and_handle_publication() {
    let node = node().await;

    let observed = Arc::new(AtomicBool::new(false));
    {
        let node_ref = node.clone();
        let observed = observed.clone();
        *node.routing_spawn_pause_hook.lock() = Some(Arc::new(move || {
            assert!(
                node_ref.routing_task.try_lock().is_none(),
                "the routing-task slot must be HELD across spawn -> publication; \
                 a joiner that could take it here would take None and return \
                 without joining a live supervisor"
            );
            observed.store(true, Ordering::Release);
        }));
    }

    node.start_org_routing_supervisor();
    assert!(
        observed.load(Ordering::Acquire),
        "the spawn/publication window must have been entered"
    );
    assert!(
        node.routing_task.lock().is_some(),
        "the handle is published before the slot is released"
    );

    // And the other order: shutdown wins the slot first, so startup spawns
    // nothing rather than leaving an unjoined supervisor behind.
    let fresh = node_unstarted().await;
    fresh.shutdown_flag_for_test();
    fresh.start_org_routing_supervisor();
    assert!(
        fresh.routing_task.lock().is_none(),
        "startup that observes shutdown under the slot lock must spawn nothing"
    );
    let rival = PrivateDiscoveryDrains::new(fresh.scoped_discovery.clone());
    assert!(
        rival.mint(PrivateDiscoveryStream::Global).is_some(),
        "and must not have claimed the exclusive drain"
    );

    let _ = node.shutdown().await;
    assert!(
        node.routing_task.lock().is_none(),
        "no unjoined routing handle may exist once shutdown has returned"
    );
}

/// (7) Revocation authority is INSIDE the currentness token.
///
/// This is the window a scoped-revision-only check cannot see: a floor (or a
/// whole store) becomes the live authority BEFORE the floor-raise subscriber
/// retracts the scoped rows it invalidates, so `revision()` has not moved while
/// the rows the source would serve are already unauthorized. A scoped-only token
/// installs facts inside that window and lets the callback retract them a moment
/// later (Kyra OLB-2B-E3c).
///
/// Driven at the source seam so the transition is exact: take a snapshot, move
/// ONLY the revocation authority, and prove the commit pin refuses.
#[tokio::test]
async fn revocation_authority_movement_alone_defeats_the_commit_pin() {
    let node = node().await;
    let source = ScopedSlotSource {
        scoped_discovery: node.scoped_discovery.clone(),
        publication: node.scoped_publication.clone(),
        org_revocation: node.org_revocation.clone(),
        unserved_scope: node.routing_unserved_scope.clone(),
    };
    let key = slot(4, "nrpc:floored");

    // Un-adopted: implicit floor 0.
    let snapshot = source.snapshot(std::slice::from_ref(&key));
    let token = snapshot.token();
    let scoped_before = node.scoped_discovery.lock().revision();
    assert!(
        source.pin_if_current(&token).is_some(),
        "nothing has moved yet"
    );

    // A real store becomes the live revocation authority. The SCOPED revision
    // does not move - that is the whole point.
    let scratch = std::env::temp_dir().join(format!(
        "net-olb2b-e3c-rev-{}-{}",
        std::process::id(),
        node.entity_id()
    ));
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).expect("scratch dir");
    let store = Arc::new(
        crate::adapter::net::behavior::org_revocation::OrgRevocationStore::init(
            scratch.join("revocation.json"),
            crate::adapter::net::behavior::org_revocation::ProvisioningExpectation::MayBeFresh,
        )
        .expect("init revocation store"),
    );
    node.org_revocation.store(Some(store.clone()));
    assert_eq!(
        node.scoped_discovery.lock().revision(),
        scoped_before,
        "the scoped revision must NOT move - only revocation authority did"
    );

    assert!(
        source.pin_if_current(&token).is_none(),
        "a snapshot taken under the OLD revocation authority cannot commit"
    );

    // Re-snapshot under the now-current authority: it commits again.
    let fresh = source.snapshot(std::slice::from_ref(&key));
    let fresh_token = fresh.token();
    assert!(source.pin_if_current(&fresh_token).is_some());

    // POISON alone defeats it too: an unusable floor view is unusable authority,
    // so every scope becomes UNSERVED rather than served unfiltered.
    store.mark_poisoned_for_test();
    assert!(
        source.pin_if_current(&fresh_token).is_none(),
        "poisoning the revocation authority invalidates a snapshot taken before it"
    );
    let poisoned = source.snapshot(std::slice::from_ref(&key));
    assert!(
        matches!(poisoned.providers(&key), SourceFacts::Unserved),
        "a poisoned revocation authority serves NOTHING rather than unfiltered rows"
    );

    drop(store);
    node.org_revocation.store(None);
    let _ = std::fs::remove_dir_all(&scratch);
}

/// (8) An unsupported audience scope reads COLD, never as a proven-empty
/// provider set.
///
/// A caller holding only slot facts must be able to tell "this source cannot
/// speak for the scope" from "this source says there are zero providers". The
/// node-global counter is observability; it cannot carry that distinction.
#[tokio::test]
async fn an_unserved_scope_reads_cold_rather_than_authoritatively_empty() {
    let node = node().await;
    node.start();
    assert!(until(|| node.org_routing_ready()).await, "healthy");

    let family = node.org_routing_family().expect("family");
    let grant_key = SlotKey {
        scope: PrivateAudienceScope::new(CapabilityAudienceScope::Grant {
            grant_id: [4u8; 32],
            audience_handle: [4u8; 32],
        })
        .expect("grant scopes are private"),
        capability: CapabilityAuthorityId::for_tag("nrpc:granted"),
    };
    let owner_key = slot(5, "nrpc:owned");
    let _g = family.demand(grant_key.clone()).expect("grant demand");
    let _o = family.demand(owner_key.clone()).expect("owner demand");

    assert!(
        until(|| node.org_routing_slots() == (2, 0)).await,
        "both slots reconcile - an unserved scope still owes no work"
    );

    assert!(
        node.org_routing_base_facts(&grant_key).is_none(),
        "the unserved grant scope reads COLD, not as zero providers"
    );
    assert!(
        node.org_routing_base_facts(&owner_key).is_some(),
        "the served owner scope reads as real evidence"
    );
    assert!(node.org_routing_unserved_scope_count() >= 1);

    let _ = node.shutdown().await;
}

/// (9) Cached facts that cross their expiry read COLD, whether or not the
/// exact-expiry sweep has run.
///
/// The scoped store filters expiry at query time, so its uncached reads are
/// expiry-safe by construction and the timer only governs promptness. A cache
/// preserves that only if its READ seam re-checks the wall clock.
#[tokio::test]
async fn cached_facts_that_crossed_their_expiry_read_cold() {
    use crate::adapter::net::behavior::org_routing_registry::{SlotBaseFacts, SourceFacts};

    let node = node().await;
    let key = slot(6, "nrpc:expiring");
    let now = crate::adapter::net::behavior::org::current_timestamp();

    // Install facts directly, as a quantum that raced the deadline would leave
    // them: valid at capture, expired by the time they are read.
    let expired = Arc::new(SlotBaseFacts {
        providers: SourceFacts::Served(Arc::from([] as [PrivateCapabilityProvider; 0])),
        source_generation: node.scoped_discovery.lock().revision(),
        actor_incarnation: 1,
        slot_incarnation: 1,
        earliest_expiry: now,
    });
    let live = Arc::new(SlotBaseFacts {
        earliest_expiry: now + 3600,
        ..(*expired).clone()
    });

    node.routing_health.store(Arc::new(
        crate::adapter::net::behavior::org_routing::RoutingHealth::Healthy { incarnation: 1 },
    ));

    node.routing_registry
        .install_facts_for_test(key.clone(), live);
    assert!(
        node.org_routing_base_facts(&key).is_some(),
        "an unexpired cached fact is served"
    );

    node.routing_registry
        .install_facts_for_test(key.clone(), expired);
    assert!(
        node.org_routing_base_facts(&key).is_none(),
        "an expired cached fact must NOT be served merely because the exact \
         timer has not swept yet"
    );
}

/// (10) Best-effort `Drop` cannot leave the supervisor freely applying: it
/// fences health synchronously and aborts the task rather than detaching it, so
/// the exclusive drain is released.
#[tokio::test]
async fn drop_fences_and_aborts_rather_than_detaching_the_supervisor() {
    let scoped = {
        let node = node().await;
        node.start();
        assert!(until(|| node.org_routing_ready()).await, "healthy");
        let scoped = node.scoped_discovery.clone();
        {
            let rival = PrivateDiscoveryDrains::new(scoped.clone());
            assert!(
                rival.mint(PrivateDiscoveryStream::Global).is_none(),
                "the lease is held while the actor runs"
            );
        }
        let health = node.routing_health.clone();
        drop(node);
        assert!(
            matches!(
                **health.load(),
                crate::adapter::net::behavior::org_routing::RoutingHealth::Fenced
            ),
            "Drop must fence routing health SYNCHRONOUSLY"
        );
        scoped
    };

    // The abort takes effect at the next suspension point; the drain drops with
    // the cancelled supervisor future.
    assert!(
        until(|| {
            PrivateDiscoveryDrains::new(scoped.clone())
                .mint(PrivateDiscoveryStream::Global)
                .is_some()
        })
        .await,
        "an aborted supervisor must release the exclusive global drain"
    );
}
