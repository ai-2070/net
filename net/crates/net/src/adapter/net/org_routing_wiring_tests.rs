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
        facts.epoch.generation,
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
    /// Fires once the COMMIT PIN is already held — the window no routing gate can
    /// exclude, where the revocation store publishes through its own
    /// synchronization.
    after_pin: Arc<parking_lot::Mutex<Option<BuildHook>>>,
    /// Fires BEFORE the pin is attempted — the probe→settle window of a pass
    /// that selected nothing, which has no reconstruction to pause inside.
    before_pin: Arc<parking_lot::Mutex<Option<BuildHook>>>,
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
        let hook = self.before_pin.lock().take();
        if let Some(hook) = hook {
            hook();
        }
        let pin = self.inner.pin_if_current(expected)?;
        let hook = self.after_pin.lock().take();
        if let Some(hook) = hook {
            hook();
        }
        Some(pin)
    }
    fn liveness(&self) -> crate::adapter::net::behavior::org_routing_registry::SourceLiveness {
        self.inner.liveness()
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
                authority: node.routing_authority.clone(),
                settle_gap_hook: parking_lot::Mutex::new(None),
                unserved_scope: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            },
            during_build: during_build.clone(),
            after_pin: Arc::default(),
            before_pin: Arc::default(),
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
            .epoch
            .generation,
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
        authority: node.routing_authority.clone(),
        settle_gap_hook: parking_lot::Mutex::new(None),
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

/// (6) Shutdown genuinely OVERLAPS registration and cannot return while it is
/// unresolved.
///
/// The one-start latch admits a single starter but does not ORDER it against
/// shutdown. This runs the real schedule: startup is parked after `tokio::spawn`
/// and before the handle is published, a concurrent shutdown attempts the same
/// slot, and shutdown must NOT be able to return until registration resolves.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shutdown_overlapping_registration_cannot_return_unresolved() {
    let node = node().await;

    let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel::<()>(1);
    let release = Arc::new((parking_lot::Mutex::new(false), parking_lot::Condvar::new()));
    {
        let release = release.clone();
        *node.routing_spawn_pause_hook.lock() = Some(Arc::new(move || {
            let _ = entered_tx.try_send(());
            let (lock, cv) = &*release;
            let mut go = lock.lock();
            while !*go {
                cv.wait(&mut go);
            }
        }));
    }

    // Startup runs on a blocking thread and parks inside the window.
    let starter = {
        let node = node.clone();
        tokio::task::spawn_blocking(move || node.start_org_routing_supervisor())
    };
    tokio::task::spawn_blocking(move || entered_rx.recv())
        .await
        .expect("join")
        .expect("startup must reach the spawn/publication window");

    // Concurrent shutdown, racing for the same slot.
    let shutdown_returned = Arc::new(AtomicBool::new(false));
    let shutting = {
        let node = node.clone();
        let flag = shutdown_returned.clone();
        tokio::spawn(async move {
            let _ = node.shutdown().await;
            flag.store(true, Ordering::Release);
        })
    };

    // Wait until shutdown has provably REACHED the routing-task slot and found
    // it held — not for an elapsed interval, which would also "pass" on a
    // scheduler that never got there (Kyra OLB-2B-E3c).
    let blocked = {
        let node = node.clone();
        tokio::task::spawn_blocking(move || {
            for _ in 0..20_000 {
                if node.routing_join_blocked_for_test() {
                    return true;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            false
        })
    }
    .await
    .expect("join");
    assert!(blocked, "shutdown never reached the routing-task slot");
    assert!(
        !shutdown_returned.load(Ordering::Acquire),
        "shutdown returned while routing registration was still unresolved"
    );

    // Resolve registration; shutdown may now proceed.
    {
        let (lock, cv) = &*release;
        *lock.lock() = true;
        cv.notify_all();
    }
    starter.await.expect("starter");
    shutting.await.expect("shutdown task");

    assert!(shutdown_returned.load(Ordering::Acquire));
    assert!(
        node.routing_task.lock().is_none(),
        "shutdown took and joined the exact handle"
    );
    assert!(!node.org_routing_ready(), "health is fenced");
    let rival = PrivateDiscoveryDrains::new(node.scoped_discovery.clone());
    assert!(
        rival.mint(PrivateDiscoveryStream::Global).is_some(),
        "no post-shutdown incarnation survives holding the drain"
    );
}

/// The other order: shutdown wins the slot first, so startup spawns nothing.
#[tokio::test]
async fn startup_that_loses_to_shutdown_spawns_nothing() {
    let node = node().await;
    node.shutdown_flag_for_test();
    node.start_org_routing_supervisor();
    assert!(
        node.routing_task.lock().is_none(),
        "startup that observes shutdown under the slot lock must spawn nothing"
    );
    let rival = PrivateDiscoveryDrains::new(node.scoped_discovery.clone());
    assert!(
        rival.mint(PrivateDiscoveryStream::Global).is_some(),
        "and must not have claimed the exclusive drain"
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
        authority: node.routing_authority.clone(),
        settle_gap_hook: parking_lot::Mutex::new(None),
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
    // Through the PRODUCTION install path — the single swap site, which is what
    // advances routing authority.
    node.install_org_revocation_store(store.clone())
        .expect("install revocation store");
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
        epoch: crate::adapter::net::behavior::org_routing_registry::SourceEpoch {
            generation: node.scoped_discovery.lock().revision(),
            authority: node.routing_authority.epoch(),
            floor_generation: 0,
            poisoned: false,
        },
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

/// A scratch directory that cleans itself up.
struct Scratch(std::path::PathBuf);

impl Scratch {
    fn path(&self) -> &std::path::Path {
        &self.0
    }
    fn new(tag: &str, node: &MeshNode) -> Self {
        // Entity component restored for collision isolation; truncated because
        // the full id pushes the authority `.lock` path past the Windows limit.
        let entity = format!("{}", node.entity_id());
        let path = std::env::temp_dir().join(format!(
            "olb-{tag}-{}-{}",
            std::process::id(),
            &entity[..entity.len().min(8)]
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("scratch dir");
        Self(path)
    }
    fn store(&self) -> Arc<crate::adapter::net::behavior::org_revocation::OrgRevocationStore> {
        Arc::new(
            crate::adapter::net::behavior::org_revocation::OrgRevocationStore::init(
                self.0.join("revocation.json"),
                crate::adapter::net::behavior::org_revocation::ProvisioningExpectation::MayBeFresh,
            )
            .expect("init revocation store"),
        )
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Arm the authority gate's contention signal and return a receiver that fires
/// the instant a rival is ABOUT to block on it.
///
/// Deterministic by construction: the hook runs only when `try_lock` found the
/// gate held, immediately before the blocking acquisition. Inferring contention
/// from elapsed time would pass on a slow scheduler that never reached the lock
/// at all (Kyra OLB-2B-E3c).
fn arm_authority_contention(node: &MeshNode) -> std::sync::mpsc::Receiver<()> {
    let (tx, rx) = std::sync::mpsc::sync_channel::<()>(4);
    *node.routing_authority.contention_hook.lock() = Some(Arc::new(move || {
        let _ = tx.try_send(());
    }));
    rx
}

/// (11) The PRODUCTION store swap cannot become visible while a commit pin is
/// alive.
///
/// Not the helper — `install_org_revocation_store`, the real path. Publishing the
/// store first and bumping routing authority afterwards would let the new store
/// be query-visible while routing still serves the old authority, for as long as
/// a pin is held.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_production_store_swap_cannot_publish_while_a_commit_pin_is_alive() {
    let node = node().await;
    let source = ScopedSlotSource {
        scoped_discovery: node.scoped_discovery.clone(),
        publication: node.scoped_publication.clone(),
        org_revocation: node.org_revocation.clone(),
        authority: node.routing_authority.clone(),
        settle_gap_hook: parking_lot::Mutex::new(None),
        unserved_scope: node.routing_unserved_scope.clone(),
    };
    let key = slot(7, "nrpc:pinned-authority");

    let scratch = Scratch::new("swap", &node);
    let store = scratch.store();
    let blocked = arm_authority_contention(&node);

    let snapshot = source.snapshot(std::slice::from_ref(&key));
    let pin = source
        .pin_if_current(&snapshot.token())
        .expect("nothing has moved");

    let installed = Arc::new(AtomicBool::new(false));
    let rival = {
        let node = node.clone();
        let installed = installed.clone();
        let store = store.clone();
        tokio::task::spawn_blocking(move || {
            node.install_org_revocation_store(store)
                .expect("install revocation store");
            installed.store(true, Ordering::Release);
        })
    };

    // Wait for the rival to REACH the blocked acquisition, not for a timeout.
    tokio::task::spawn_blocking(move || blocked.recv_timeout(Duration::from_secs(10)))
        .await
        .expect("join")
        .expect("the installer must block on the authority gate");

    assert!(
        !installed.load(Ordering::Acquire),
        "the production install completed while a commit pin was alive"
    );
    assert!(
        node.org_revocation.load().is_none(),
        "and the new store must NOT be query-visible yet"
    );

    drop(pin);
    rival.await.expect("rival");
    assert!(installed.load(Ordering::Acquire));
    assert!(node.org_revocation.load().is_some());

    assert!(
        source.pin_if_current(&snapshot.token()).is_none(),
        "a token from the retired authority must be refused"
    );
}

/// (11b) A floor publication that has become authoritative but whose subscriber
/// has not run yet still colds facts built against the previous floors.
///
/// The floor generation moves INSIDE the revocation store, before the subscriber
/// that advances the routing epoch is notified. Retaining it in the stamped epoch
/// is what closes that gap.
#[tokio::test]
async fn facts_built_against_superseded_floors_read_cold() {
    use crate::adapter::net::behavior::org_routing_registry::{
        SlotBaseFacts, SourceEpoch, SourceFacts,
    };

    let node = node().await;
    let scratch = Scratch::new("floors", &node);
    node.install_org_revocation_store(scratch.store())
        .expect("install");
    node.routing_health.store(Arc::new(
        crate::adapter::net::behavior::org_routing::RoutingHealth::Healthy { incarnation: 1 },
    ));

    let key = slot(11, "nrpc:floored-read");
    let live_floor = node
        .org_revocation
        .load()
        .as_ref()
        .expect("store")
        .barriered_generation()
        .expect("not exhausted")
        .get();

    node.routing_registry.install_facts_for_test(
        key.clone(),
        Arc::new(SlotBaseFacts {
            providers: SourceFacts::Served(Arc::from([] as [PrivateCapabilityProvider; 0])),
            epoch: SourceEpoch {
                generation: node.scoped_discovery.lock().revision(),
                authority: node.routing_authority.epoch(),
                floor_generation: live_floor,
                poisoned: false,
            },
            actor_incarnation: 1,
            slot_incarnation: 1,
            earliest_expiry: u64::MAX,
        }),
    );
    assert!(
        node.org_routing_base_facts(&key).is_some(),
        "precondition: coherent facts are served"
    );

    // Same authority epoch, same scoped revision — only the floors moved.
    node.routing_registry.install_facts_for_test(
        key.clone(),
        Arc::new(SlotBaseFacts {
            providers: SourceFacts::Served(Arc::from([] as [PrivateCapabilityProvider; 0])),
            epoch: SourceEpoch {
                generation: node.scoped_discovery.lock().revision(),
                authority: node.routing_authority.epoch(),
                floor_generation: live_floor.wrapping_sub(1),
                poisoned: false,
            },
            actor_incarnation: 1,
            slot_incarnation: 1,
            earliest_expiry: u64::MAX,
        }),
    );
    assert!(
        node.org_routing_base_facts(&key).is_none(),
        "facts built against superseded floors must read COLD even though the \
         authority epoch and scoped revision are unchanged"
    );
    assert_eq!(node.org_routing_slots().1, 1, "and the slot is re-queued");
}

/// (11c) An exhausted authority epoch FENCES routing rather than aliasing.
///
/// At saturation every later authority would receive the same identity, so a
/// token minted under one store would commit under an unrelated replacement.
#[tokio::test]
async fn an_exhausted_authority_epoch_fences_rather_than_aliasing() {
    let node = node().await;
    let source = ScopedSlotSource {
        scoped_discovery: node.scoped_discovery.clone(),
        publication: node.scoped_publication.clone(),
        org_revocation: node.org_revocation.clone(),
        authority: node.routing_authority.clone(),
        settle_gap_hook: parking_lot::Mutex::new(None),
        unserved_scope: node.routing_unserved_scope.clone(),
    };
    let key = slot(12, "nrpc:exhausted");

    node.routing_authority
        .epoch
        .store(u64::MAX, Ordering::Release);
    assert!(!node.routing_authority.is_exhausted());

    {
        let _gate = node.routing_authority.lock_gate();
        assert_eq!(
            node.routing_authority.advance(),
            AuthorityAdvance::NewlyExhausted,
            "the terminal transition is reported to exactly one caller"
        );
        assert_eq!(
            node.routing_authority.advance(),
            AuthorityAdvance::AlreadyExhausted,
            "and never a second time"
        );
    }
    assert!(
        node.routing_authority.is_exhausted(),
        "advancing past the ceiling must FENCE, not saturate"
    );
    assert_eq!(
        node.routing_authority.epoch(),
        u64::MAX,
        "and must not have handed out a reused identity"
    );

    let snapshot = source.snapshot(std::slice::from_ref(&key));
    assert!(
        matches!(snapshot.providers(&key), SourceFacts::Unserved),
        "an exhausted authority serves nothing"
    );
    assert!(
        source.pin_if_current(&snapshot.token()).is_none(),
        "and commits nothing"
    );
}

/// (11c2) An exhausted revocation publication generation is unusable authority.
///
/// The store's generation can no longer distinguish floor views, so it can no
/// longer witness currentness — routing must fail closed on it exactly as it does
/// on poison, rather than trusting a frozen discriminator.
#[tokio::test]
async fn an_exhausted_store_generation_makes_every_scope_unserved() {
    let node = node().await;
    let scratch = Scratch::new("gen-exhausted", &node);
    let store = scratch.store();
    node.install_org_revocation_store(store.clone())
        .expect("install");

    let source = ScopedSlotSource {
        scoped_discovery: node.scoped_discovery.clone(),
        publication: node.scoped_publication.clone(),
        org_revocation: node.org_revocation.clone(),
        authority: node.routing_authority.clone(),
        settle_gap_hook: parking_lot::Mutex::new(None),
        unserved_scope: node.routing_unserved_scope.clone(),
    };
    let key = slot(14, "nrpc:gen-exhausted");

    let healthy = source.snapshot(std::slice::from_ref(&key));
    assert!(
        matches!(healthy.providers(&key), SourceFacts::Served(_)),
        "precondition: a usable authority serves the scope"
    );
    let healthy_token = healthy.token();
    assert!(source.pin_if_current(&healthy_token).is_some());

    store.saturate_generation_for_test();
    store.republish_for_test();
    assert_eq!(
        store.barriered_generation().err(),
        Some(crate::adapter::net::behavior::org_revocation::GenerationExhausted)
    );

    let snapshot = source.snapshot(std::slice::from_ref(&key));
    assert!(
        matches!(snapshot.providers(&key), SourceFacts::Unserved),
        "an exhausted publication generation serves NOTHING"
    );
    assert!(
        source.pin_if_current(&healthy_token).is_none(),
        "and a token minted under the usable authority no longer commits"
    );
}

/// (11c3) The TRANSITION to terminal authority exhaustion, driven through the
/// production movement path: it retires every retained fact — including the
/// `authority == u64::MAX` stamps that `invalidate_authority_older_than(MAX)`
/// structurally spares — clears the queue instead of promising unfulfillable
/// work, and fences `org_routing_ready` INDEPENDENTLY of supervisor health
/// (E3c blockers §2).
///
/// Before this closure, `advance()` latched silently: the movement invalidated
/// nothing (nothing is older than MAX), health stayed `Healthy`, readiness
/// stayed true, and any queued slot re-queued + re-marked itself through the
/// refused commit pin forever.
#[tokio::test]
async fn terminal_exhaustion_retires_max_stamped_facts_and_fences_readiness() {
    use crate::adapter::net::behavior::org_routing::{ApplyOutcome, ApplyRequest, DirtyApply};
    use crate::adapter::net::behavior::org_scoped_store::{
        DirtyCapabilities, PrivateDiscoveryChangeBatch,
    };

    let node = node().await;
    let scratch = Scratch::new("authority-terminal", &node);
    node.install_org_revocation_store(scratch.store())
        .expect("install");
    node.routing_health.store(Arc::new(
        crate::adapter::net::behavior::org_routing::RoutingHealth::Healthy { incarnation: 1 },
    ));
    let registry = node.routing_registry.clone();
    registry.activate_incarnation(1);

    // Park the epoch at the ceiling, then warm a slot: its facts carry the
    // `authority == u64::MAX` stamp no strictly-older invalidation can name.
    node.routing_authority
        .epoch
        .store(u64::MAX, Ordering::Release);
    let family = registry.new_family().expect("family");
    let key = slot(26, "nrpc:authority-terminal");
    let _held = family.demand(key.clone()).expect("demand");
    let request = ApplyRequest {
        batch: PrivateDiscoveryChangeBatch {
            generation: 0,
            dirty: DirtyCapabilities::Clean,
        },
        registry_work: true,
    };
    assert!(matches!(
        registry.apply(1, request.clone()),
        ApplyOutcome::Current { .. }
    ));
    let warm = registry.base_facts(&key).expect("reconciled");
    assert_eq!(
        warm.epoch.authority,
        u64::MAX,
        "precondition: MAX-stamped facts are retained"
    );
    assert!(
        node.org_routing_base_facts(&key).is_some(),
        "precondition: and warm"
    );
    assert!(node.org_routing_ready(), "precondition: ready");

    // The PRODUCTION movement: a store install advances routing authority; at
    // the ceiling that movement IS the terminal transition.
    let replacement = Scratch::new("authority-terminal-b", &node);
    node.install_org_revocation_store(replacement.store())
        .expect("replacement install");

    assert!(node.routing_authority.is_exhausted(), "terminally fenced");
    assert_eq!(
        node.routing_authority.epoch(),
        u64::MAX,
        "no identity was reused"
    );
    assert!(
        registry.base_facts(&key).is_none(),
        "the MAX-stamped fact was retired SYNCHRONOUSLY — no reader involved"
    );
    assert_eq!(
        registry.pending_slots(),
        0,
        "and nothing was re-queued: a rebuild could never install again"
    );
    assert!(
        matches!(
            **node.routing_health.load(),
            crate::adapter::net::behavior::org_routing::RoutingHealth::Healthy { .. }
        ),
        "supervisor health alone still says Healthy…"
    );
    assert!(
        !node.org_routing_ready(),
        "…so readiness must fence on exhaustion INDEPENDENTLY of health"
    );
    assert!(
        node.org_routing_base_facts(&key).is_none(),
        "reads are cold"
    );

    // Work demanded AFTER the transition parks instead of spinning: the failed
    // commit pin is TERMINAL, so the pass discards without re-queueing or
    // re-marking itself awake.
    let late = slot(26, "nrpc:authority-terminal-late");
    let _late_held = family.demand(late.clone()).expect("late demand");
    assert_eq!(registry.pending_slots(), 1, "the late demand queues once");
    assert_eq!(
        registry.apply(1, request),
        ApplyOutcome::Superseded,
        "nothing settles under a terminal authority"
    );
    assert_eq!(
        registry.pending_slots(),
        0,
        "and the pass DISCARDS rather than re-queues — no terminal spin"
    );
    assert!(registry.base_facts(&late).is_none());
}

/// (11c4) The STORE-GENERATION arm of the exhaustion fence (review-pass-3 §1):
/// an exhausted publication generation makes settlement impossible, so the pass
/// must park rather than spin — and because the condition is RECOVERABLE, it must
/// park without losing the queue.
///
/// This arm is not the authority arm and cannot be fenced by it. The folded
/// `(poisoned = true, floor_generation = 0)` view is SELF-CONSISTENT across
/// passes, so `pin_if_current` ACCEPTS every time; only `ScopedCommitPin::matches`
/// refuses, at the phase-5 settlement. Before this closure that branch re-queued
/// AND re-marked, so the actor re-snapshotted, re-built, re-pinned and re-failed
/// at `yield_now` rate forever, with no source movement required at all.
#[tokio::test]
async fn an_exhausted_store_generation_parks_apply_without_spinning_and_recovers() {
    use crate::adapter::net::behavior::org_routing::{ApplyOutcome, ApplyRequest, DirtyApply};
    use crate::adapter::net::behavior::org_routing_registry::SourceLiveness;
    use crate::adapter::net::behavior::org_scoped_store::{
        DirtyCapabilities, PrivateDiscoveryChangeBatch,
    };

    let node = node().await;
    let scratch = Scratch::new("store-generation-fence", &node);
    let store = scratch.store();
    node.install_org_revocation_store(store.clone())
        .expect("install");
    node.routing_health.store(Arc::new(
        crate::adapter::net::behavior::org_routing::RoutingHealth::Healthy { incarnation: 1 },
    ));
    let registry = node.routing_registry.clone();
    registry.activate_incarnation(1);

    let family = registry.new_family().expect("family");
    let key = slot(27, "nrpc:store-generation-fence");
    let _held = family.demand(key.clone()).expect("demand");
    let request = ApplyRequest {
        batch: PrivateDiscoveryChangeBatch {
            generation: 0,
            dirty: DirtyCapabilities::Clean,
        },
        registry_work: true,
    };
    assert!(matches!(
        registry.apply(1, request.clone()),
        ApplyOutcome::Current { .. }
    ));
    assert!(
        node.org_routing_base_facts(&key).is_some(),
        "precondition: the slot is warm through the production seam"
    );

    // Park the store's publication generation at the ceiling and republish, so
    // the LIVE view is the exhausted one.
    store.saturate_generation_for_test();
    store.republish_for_test();
    assert_eq!(
        store.barriered_generation().err(),
        Some(crate::adapter::net::behavior::org_revocation::GenerationExhausted)
    );

    let source = ScopedSlotSource {
        scoped_discovery: node.scoped_discovery.clone(),
        publication: node.scoped_publication.clone(),
        org_revocation: node.org_revocation.clone(),
        authority: node.routing_authority.clone(),
        settle_gap_hook: parking_lot::Mutex::new(None),
        unserved_scope: node.routing_unserved_scope.clone(),
    };
    assert_eq!(
        source.liveness(),
        SourceLiveness::Fenced,
        "recoverable, not terminal: a replacement install retires it"
    );
    assert!(
        !node.routing_authority.is_exhausted(),
        "and the AUTHORITY latch is untouched — this is the other arm"
    );

    // Re-queue the slot and clear the wake flag, so what the pass does to it is
    // the only thing the assertions below can be reading.
    registry.invalidate_for_test(&key);
    node.routing_work.take_for_test();

    for pass in 0..3 {
        assert_eq!(
            registry.apply(1, request.clone()),
            ApplyOutcome::Superseded,
            "pass {pass}: nothing settles against an exhausted publication generation"
        );
        assert_eq!(
            registry.pending_slots(),
            1,
            "pass {pass}: the identity stays OWED — the fence is recoverable"
        );
        assert!(
            !node.routing_work.take_for_test(),
            "pass {pass}: and the pass must NOT re-arm itself — that is the livelock"
        );
    }
    assert!(
        node.org_routing_base_facts(&key).is_none(),
        "service is cold while the fence holds"
    );

    // RECOVERY: a replacement store install swaps the store and advances the
    // routing epoch as one movement. That movement owns the wake.
    let replacement = Scratch::new("store-generation-fence-b", &node);
    node.install_org_revocation_store(replacement.store())
        .expect("replacement install");
    assert_eq!(
        source.liveness(),
        SourceLiveness::Live,
        "the fence lifts with the store that raised it"
    );
    assert!(
        node.routing_work.take_for_test(),
        "and the movement supplied the wake the parked pass deliberately did not"
    );

    assert!(
        matches!(
            registry.apply(1, request),
            ApplyOutcome::Current { .. } | ApplyOutcome::Progress { .. }
        ),
        "the preserved queue is what lets the successor pass rebuild"
    );
    assert!(
        node.org_routing_base_facts(&key).is_some(),
        "and service is warm again — the fence cost promptness, not the slot"
    );
}

/// (28c) An EMPTY registry whose authority moves inside the probe→settle window
/// is re-driven rather than stranded (E3c blockers §3, review-pass-3 §2).
///
/// The empty-selection pass is the COMMON production path, not a corner: every
/// node retains zero routing slots until the warmed-call consumer lands. Its two
/// `Superseded` returns used to mark nothing, and the compensator that covers the
/// non-empty paths — `invalidate_authority_older_than`, which marks only when
/// `pending` ends non-empty — is a guaranteed no-op with zero retained slots. An
/// authority-only movement advances no scoped watch either, so the actor set
/// `owed_recapture`, parked, and left health at `Rebuilding` indefinitely.
///
/// Three legs, because the fix has to hold on both refusal sites AND must not
/// re-open the exhaustion livelock the fences just closed.
#[tokio::test]
async fn an_empty_registry_whose_authority_moves_under_the_probe_is_redriven() {
    use crate::adapter::net::behavior::org_routing::{
        ApplyOutcome, ApplyRequest, DirtyApply, RegistryWork,
    };
    use crate::adapter::net::behavior::org_routing_registry::NodeOrgRoutingRegistry;
    use crate::adapter::net::behavior::org_scoped_store::{
        DirtyCapabilities, PrivateDiscoveryChangeBatch,
    };

    let node = node().await;
    let scratch = Scratch::new("empty-selection", &node);
    node.install_org_revocation_store(scratch.store())
        .expect("install");

    let before_pin: Arc<parking_lot::Mutex<Option<BuildHook>>> = Arc::default();
    let after_pin: Arc<parking_lot::Mutex<Option<BuildHook>>> = Arc::default();
    let work: Arc<RegistryWork> = Arc::default();
    let registry = NodeOrgRoutingRegistry::new(
        Arc::new(PausingSource {
            inner: ScopedSlotSource {
                scoped_discovery: node.scoped_discovery.clone(),
                publication: node.scoped_publication.clone(),
                org_revocation: node.org_revocation.clone(),
                authority: node.routing_authority.clone(),
                settle_gap_hook: parking_lot::Mutex::new(None),
                unserved_scope: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            },
            during_build: Arc::default(),
            after_pin: after_pin.clone(),
            before_pin: before_pin.clone(),
        }),
        work.clone(),
        Arc::default(),
    );
    registry.activate_incarnation(1);

    // NO demand: zero retained slots, zero pending. Every pass below takes the
    // empty-selection branch.
    assert_eq!(registry.retained_slots(), 0);
    assert_eq!(registry.pending_slots(), 0);
    let request = ApplyRequest {
        batch: PrivateDiscoveryChangeBatch {
            generation: 0,
            dirty: DirtyCapabilities::Clean,
        },
        registry_work: true,
    };
    assert!(
        matches!(
            registry.apply(1, request.clone()),
            ApplyOutcome::Current { .. }
        ),
        "baseline: an undisturbed empty pass settles Current"
    );

    // --- leg 1: movement between the probe token and the pin ---
    let replacement = Scratch::new("empty-selection-b", &node);
    let replacement_store = replacement.store();
    let moved = Arc::new(AtomicBool::new(false));
    {
        let node = node.clone();
        let replacement_store = replacement_store.clone();
        let moved = moved.clone();
        *before_pin.lock() = Some(Box::new(move || {
            // The production movement: a store install advances the routing
            // epoch. It retracts nothing scoped, so it advances no scoped watch,
            // and with zero retained slots its own invalidation marks nothing.
            node.install_org_revocation_store(replacement_store.clone())
                .expect("replacement install");
            moved.store(true, Ordering::Release);
        }));
    }
    work.take_for_test();
    assert_eq!(
        registry.apply(1, request.clone()),
        ApplyOutcome::Superseded,
        "the pin must refuse a token minted under the retired authority"
    );
    assert!(moved.load(Ordering::Acquire), "the authority did move");
    assert!(
        work.take_for_test(),
        "the empty-selection PIN refusal must mark: nothing else will wake this actor"
    );
    assert!(
        matches!(
            registry.apply(1, request.clone()),
            ApplyOutcome::Current { .. }
        ),
        "and the re-driven pass settles — which is what lets the supervisor publish Healthy"
    );

    // --- leg 2: movement between the pin and the settlement ---
    // Floors publish through the store's own synchronization, which no routing
    // gate excludes, so this is the one window a held pin cannot close by itself.
    let published = Arc::new(AtomicBool::new(false));
    {
        let replacement_store = replacement_store.clone();
        let published = published.clone();
        *after_pin.lock() = Some(Box::new(move || {
            replacement_store.republish_for_test();
            published.store(true, Ordering::Release);
        }));
    }
    work.take_for_test();
    assert_eq!(
        registry.apply(1, request.clone()),
        ApplyOutcome::Superseded,
        "a floor publication under the pin must not settle an empty pass either"
    );
    assert!(published.load(Ordering::Acquire), "the floor did move");
    assert!(
        work.take_for_test(),
        "the empty-selection SETTLE refusal must mark for the same reason"
    );
    assert!(matches!(
        registry.apply(1, request.clone()),
        ApplyOutcome::Current { .. }
    ));

    // --- leg 3: the composition constraint ---
    // The marks above are conditioned on MOVEMENT. Against an unsettleable
    // source nothing has moved and nothing will, so marking would reproduce
    // exactly the probe→refuse→mark→probe spin the exhaustion fences close —
    // on an empty registry, forever.
    replacement_store.saturate_generation_for_test();
    replacement_store.republish_for_test();
    work.take_for_test();
    for pass in 0..3 {
        assert_eq!(
            registry.apply(1, request.clone()),
            ApplyOutcome::Superseded,
            "pass {pass}: an exhausted publication generation cannot settle"
        );
        assert!(
            !work.take_for_test(),
            "pass {pass}: and must NOT self-wake — the empty path spins hardest of all"
        );
    }
}

/// (11d) A delayed reader that finds ITS artifact stale must not delete a newer
/// one installed in the meantime.
#[tokio::test]
async fn a_delayed_reader_does_not_delete_a_newer_artifact() {
    use crate::adapter::net::behavior::org_routing_registry::{
        SlotBaseFacts, SourceEpoch, SourceFacts,
    };

    let node = node().await;
    let key = slot(13, "nrpc:delayed-reader");
    let facts = |authority: u64| {
        Arc::new(SlotBaseFacts {
            providers: SourceFacts::Served(Arc::from([] as [PrivateCapabilityProvider; 0])),
            epoch: SourceEpoch {
                generation: node.scoped_discovery.lock().revision(),
                authority,
                floor_generation: 0,
                poisoned: false,
            },
            actor_incarnation: 1,
            slot_incarnation: 1,
            earliest_expiry: u64::MAX,
        })
    };

    let stale = facts(node.routing_authority.epoch().wrapping_sub(1));
    node.routing_registry
        .install_facts_for_test(key.clone(), stale.clone());

    // Reconciliation installs a CURRENT artifact before the delayed reader acts.
    let current = facts(node.routing_authority.epoch());
    node.routing_registry
        .install_facts_for_test(key.clone(), current.clone());

    // The delayed reader now acts on the artifact IT observed.
    node.routing_registry.invalidate_if_stale(&key, &stale);

    let live = node
        .routing_registry
        .base_facts(&key)
        .expect("the current artifact survives");
    assert!(
        Arc::ptr_eq(&live, &current),
        "a delayed reader must not delete a newer artifact"
    );
    assert_eq!(
        node.org_routing_slots().1,
        0,
        "and must not re-queue work that was already done"
    );
}

/// (12) Poisoning revocation authority COLDS already-cached facts and re-queues
/// the exact slot.
///
/// Poison after installation moves no epoch and retracts no scoped row, so
/// nothing would rebuild the slot on its own. The read seam is what must catch
/// it.
#[tokio::test]
async fn poisoning_authority_colds_already_cached_facts() {
    use crate::adapter::net::behavior::org_routing_registry::{
        SlotBaseFacts, SourceEpoch, SourceFacts,
    };

    let node = node().await;
    let key = slot(8, "nrpc:poisoned");
    node.routing_health.store(Arc::new(
        crate::adapter::net::behavior::org_routing::RoutingHealth::Healthy { incarnation: 1 },
    ));

    let scratch = std::env::temp_dir().join(format!(
        "net-olb2b-e3c-poison-{}-{}",
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

    node.routing_registry.install_facts_for_test(
        key.clone(),
        Arc::new(SlotBaseFacts {
            providers: SourceFacts::Served(Arc::from([] as [PrivateCapabilityProvider; 0])),
            epoch: SourceEpoch {
                generation: node.scoped_discovery.lock().revision(),
                authority: node.routing_authority.epoch(),
                floor_generation: 0,
                poisoned: false,
            },
            actor_incarnation: 1,
            slot_incarnation: 1,
            earliest_expiry: u64::MAX,
        }),
    );
    assert!(
        node.org_routing_base_facts(&key).is_some(),
        "precondition: the cached facts are served"
    );

    store.mark_poisoned_for_test();

    assert!(
        node.org_routing_base_facts(&key).is_none(),
        "poisoned authority must COLD already-cached facts"
    );
    assert_eq!(
        node.org_routing_slots().1,
        1,
        "and re-queue the exact slot so the actor rebuilds it"
    );

    node.org_revocation.store(None);
    drop(store);
    let _ = std::fs::remove_dir_all(&scratch);
}

/// (13) Authority-only movement invalidates every retained fact and wakes the
/// registry, even though it touches no scoped state.
#[tokio::test]
async fn authority_only_movement_invalidates_and_requeues_everything() {
    use crate::adapter::net::behavior::org_routing_registry::{
        SlotBaseFacts, SourceEpoch, SourceFacts,
    };

    let node = node().await;
    node.routing_health.store(Arc::new(
        crate::adapter::net::behavior::org_routing::RoutingHealth::Healthy { incarnation: 1 },
    ));
    let key = slot(9, "nrpc:authority-move");
    let scoped_before = node.scoped_discovery.lock().revision();

    node.routing_registry.install_facts_for_test(
        key.clone(),
        Arc::new(SlotBaseFacts {
            providers: SourceFacts::Served(Arc::from([] as [PrivateCapabilityProvider; 0])),
            epoch: SourceEpoch {
                generation: scoped_before,
                authority: node.routing_authority.epoch(),
                floor_generation: 0,
                poisoned: false,
            },
            actor_incarnation: 1,
            slot_incarnation: 1,
            earliest_expiry: u64::MAX,
        }),
    );
    assert!(node.org_routing_base_facts(&key).is_some());

    move_routing_authority(&node.routing_authority, &node.routing_registry, || {});

    assert_eq!(
        node.scoped_discovery.lock().revision(),
        scoped_before,
        "authority movement touched NO scoped state - there is no scoped wake"
    );
    // Read the RAW registry, not the node read seam: the read seam would cold
    // this lazily on its own, which cannot distinguish "invalidated by the
    // movement" from "invalidated the first time someone happened to look".
    // Blocker 2 is specifically about the actor learning WITHOUT a read.
    assert!(
        node.routing_registry.base_facts(&key).is_none(),
        "authority movement must SYNCHRONOUSLY invalidate every retained fact"
    );
    assert_eq!(
        node.org_routing_slots().1,
        1,
        "and re-queue it, so the actor rebuilds without waiting for a reader"
    );
    assert!(
        node.org_routing_base_facts(&key).is_none(),
        "and it reads cold"
    );
}

/// (14) A recapture spanning two authority epochs never settles `Current`.
///
/// Driven through a real `apply()`: quantum 1 installs under authority A, the
/// authority moves with NO scoped movement, and the next pass must not report a
/// complete installation over the mixed set.
#[tokio::test]
async fn a_recapture_across_two_authority_epochs_does_not_settle_current() {
    use crate::adapter::net::behavior::org_routing::{
        ApplyOutcome, ApplyRequest, DirtyApply, RegistryWork,
    };
    use crate::adapter::net::behavior::org_routing_registry::NodeOrgRoutingRegistry;
    use crate::adapter::net::behavior::org_scoped_store::{
        DirtyCapabilities, PrivateDiscoveryChangeBatch,
    };

    let node = node().await;
    let work: Arc<RegistryWork> = Arc::default();
    let registry = NodeOrgRoutingRegistry::new(
        Arc::new(ScopedSlotSource {
            scoped_discovery: node.scoped_discovery.clone(),
            publication: node.scoped_publication.clone(),
            org_revocation: node.org_revocation.clone(),
            authority: node.routing_authority.clone(),
            settle_gap_hook: parking_lot::Mutex::new(None),
            unserved_scope: node.routing_unserved_scope.clone(),
        }),
        work.clone(),
        Arc::default(),
    );
    registry.activate_incarnation(1);

    // More than one quantum, so the recapture epoch stays OPEN across passes.
    // That is where mixed-authority settlement is actually reachable: outside an
    // epoch, ordinary work legitimately leaves unrelated slots at older epochs
    // and must still settle Current (the narrow-settlement rule).
    let mut family = registry.new_family().expect("family");
    let mut held = Vec::new();
    let mut keys = Vec::new();
    for index in 0..70 {
        if index > 0 && index % 64 == 0 {
            family = registry.new_family().expect("family");
        }
        let key = slot(10, &format!("nrpc:epoch-{index}"));
        held.push(family.demand(key.clone()).expect("demand"));
        keys.push(key);
    }

    let request = |dirty| ApplyRequest {
        batch: PrivateDiscoveryChangeBatch {
            generation: 0,
            dirty,
        },
        registry_work: true,
    };

    // Quantum 1 under authority A: incomplete, so the epoch stays open.
    let outcome = registry.apply(1, request(DirtyCapabilities::RebuildAll));
    assert!(
        matches!(outcome, ApplyOutcome::Progress { .. }),
        "one quantum cannot finish 70 slots: {outcome:?}"
    );
    let authority_a = node.routing_authority.epoch();
    let scoped = node.scoped_discovery.lock().revision();

    // Authority moves MID-EPOCH. No scoped movement whatsoever.
    {
        let _gate = node.routing_authority.lock_gate();
        let _ = node.routing_authority.advance();
    }
    assert_eq!(
        node.scoped_discovery.lock().revision(),
        scoped,
        "scoped revision is IDENTICAL across the two authority epochs"
    );
    assert_ne!(node.routing_authority.epoch(), authority_a);

    // The next quantum finishes the remainder under authority B — but the epoch
    // must NOT settle, because the first quantum's facts carry authority A.
    let outcome = registry.apply(1, request(DirtyCapabilities::RebuildAll));
    assert!(
        matches!(outcome, ApplyOutcome::Progress { .. }),
        "a recapture must not settle Current over mixed-authority facts:          {outcome:?}"
    );
    assert!(
        registry.pending_slots() > 0,
        "the authority-A slots are re-queued"
    );

    // Draining the rest completes it under ONE authority.
    let mut outcome = registry.apply(1, request(DirtyCapabilities::RebuildAll));
    for _ in 0..4 {
        if matches!(outcome, ApplyOutcome::Current { .. }) {
            break;
        }
        outcome = registry.apply(1, request(DirtyCapabilities::RebuildAll));
    }
    assert!(
        matches!(outcome, ApplyOutcome::Current { .. }),
        "and completes once every slot shares one authority: {outcome:?}"
    );
    let live = node.routing_authority.epoch();
    for key in &keys {
        assert_eq!(
            registry.base_facts(key).expect("built").epoch.authority,
            live,
            "one authority across the whole retained set"
        );
    }
}

/// (20) Store A sampled before a swap can never be combined with store B's
/// routing epoch.
///
/// Off-gate, `snapshot()` could read the epoch after a publisher advanced it but
/// the store before the swap landed — producing A's floors under B's epoch. If A
/// and B happen to share a floor generation the token then compares equal to live
/// B, and A-derived rows install as B-authoritative. Sampling both under the
/// authority gate is what forecloses it: the snapshot blocks for the whole
/// publication rather than straddling it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_snapshot_cannot_straddle_a_store_publication() {
    let node = node().await;
    let source = ScopedSlotSource {
        scoped_discovery: node.scoped_discovery.clone(),
        publication: node.scoped_publication.clone(),
        org_revocation: node.org_revocation.clone(),
        authority: node.routing_authority.clone(),
        settle_gap_hook: parking_lot::Mutex::new(None),
        unserved_scope: node.routing_unserved_scope.clone(),
    };
    let key = slot(15, "nrpc:straddle");
    let scratch = Scratch::new("straddle", &node);

    // Hold the authority gate, then race a snapshot against it: the snapshot must
    // BLOCK rather than sample half of the transition.
    let blocked = arm_authority_contention(&node);
    // Held on a BLOCKING thread, not across an await: the guard is not Send and
    // holding it across a suspension point would be a real defect, not a lint.
    let (release_tx, release_rx) = std::sync::mpsc::sync_channel::<()>(1);
    let (done_tx, done_rx) = std::sync::mpsc::sync_channel::<()>(1);
    let epoch_before = node.routing_authority.epoch();
    let holder = {
        let node = node.clone();
        let store = scratch.store();
        tokio::task::spawn_blocking(move || {
            let _held = node.routing_authority.lock_gate();
            let _ = done_tx.send(());
            let _ = release_rx.recv();
            // Complete the transition under the held gate, exactly as the
            // publisher does: epoch first, then the store.
            let _ = node.routing_authority.advance();
            node.org_revocation.store(Some(store));
        })
    };
    tokio::task::spawn_blocking(move || done_rx.recv_timeout(Duration::from_secs(10)))
        .await
        .expect("join")
        .expect("the holder must take the gate");

    let sampled = Arc::new(AtomicBool::new(false));
    let sampler = {
        let sampled = sampled.clone();
        let source_epoch = Arc::new(parking_lot::Mutex::new(None));
        let out = source_epoch.clone();
        let node = node.clone();
        let key = key.clone();
        (
            tokio::task::spawn_blocking(move || {
                let src = ScopedSlotSource {
                    scoped_discovery: node.scoped_discovery.clone(),
                    publication: node.scoped_publication.clone(),
                    org_revocation: node.org_revocation.clone(),
                    authority: node.routing_authority.clone(),
                    settle_gap_hook: parking_lot::Mutex::new(None),
                    unserved_scope: node.routing_unserved_scope.clone(),
                };
                let snap = src.snapshot(std::slice::from_ref(&key));
                *out.lock() = Some(snap.token());
                sampled.store(true, Ordering::Release);
            }),
            source_epoch,
        )
    };

    // BOUNDED: a missing contention signal must FAIL the witness, not hang it.
    tokio::task::spawn_blocking(move || blocked.recv_timeout(Duration::from_secs(10)))
        .await
        .expect("join")
        .expect("the snapshot must block on the authority gate");
    assert!(
        !sampled.load(Ordering::Acquire),
        "a snapshot sampled authority while a publication held the gate"
    );

    let _ = release_tx.send(());
    holder.await.expect("holder");
    assert_ne!(epoch_before, node.routing_authority.epoch());

    sampler.0.await.expect("sampler");
    let token = sampler.1.lock().clone().expect("token");

    // Whatever it sampled is COHERENT: it commits against the live authority.
    assert!(
        source.pin_if_current(&token).is_some(),
        "the snapshot must have sampled one side of the transition, not a mix"
    );
}

/// (21) A lock-free cached read never observes the replacement store with the
/// retired epoch.
///
/// The publisher advances the epoch BEFORE publishing the store, so the window is
/// conservatively cold rather than permissively stale: a reader sees (A, R) or
/// (B, R+1), never (B, R).
#[tokio::test]
async fn the_epoch_advances_before_the_store_becomes_visible() {
    use crate::adapter::net::behavior::org_routing_registry::{
        SlotBaseFacts, SourceEpoch, SourceFacts,
    };

    let node = node().await;
    node.routing_health.store(Arc::new(
        crate::adapter::net::behavior::org_routing::RoutingHealth::Healthy { incarnation: 1 },
    ));
    let key = slot(16, "nrpc:epoch-first");
    let retired = node.routing_authority.epoch();

    node.routing_registry.install_facts_for_test(
        key.clone(),
        Arc::new(SlotBaseFacts {
            providers: SourceFacts::Served(Arc::from([] as [PrivateCapabilityProvider; 0])),
            epoch: SourceEpoch {
                generation: node.scoped_discovery.lock().revision(),
                authority: retired,
                floor_generation: 0,
                poisoned: false,
            },
            actor_incarnation: 1,
            slot_incarnation: 1,
            earliest_expiry: u64::MAX,
        }),
    );
    assert!(node.org_routing_base_facts(&key).is_some());

    // Observe the ordering directly: inside the publish closure the epoch has
    // ALREADY moved, so no reader can pair the new store with the retired epoch.
    let scratch = Scratch::new("epoch-first", &node);
    let store = scratch.store();
    let observed = Arc::new(AtomicBool::new(false));
    {
        let node2 = node.clone();
        let observed = observed.clone();
        let store = store.clone();
        move_routing_authority(&node.routing_authority, &node.routing_registry, || {
            assert_ne!(
                node2.routing_authority.epoch(),
                retired,
                "the epoch must advance BEFORE the store becomes visible"
            );
            assert!(
                node2.org_revocation.load().is_none(),
                "precondition: the store is not visible yet"
            );
            node2.org_revocation.store(Some(store));
            observed.store(true, Ordering::Release);
        });
    }
    assert!(observed.load(Ordering::Acquire));
    assert!(
        node.org_routing_base_facts(&key).is_none(),
        "facts stamped with the retired epoch read cold across the transition"
    );
}

/// (22) In-place floor publication underneath a live commit pin cannot settle
/// `Current`.
///
/// The gates exclude scoped mutation and node-mediated authority movement; they
/// cannot exclude the revocation store's own publication. The pin therefore
/// re-verifies floors and poison before settlement, so a quantum whose authority
/// moved reports `Superseded` and re-queues instead of publishing a completed
/// recapture.
#[tokio::test]
async fn a_floor_publication_under_the_pin_cannot_settle_current() {
    use crate::adapter::net::behavior::org_routing::{
        ApplyOutcome, ApplyRequest, DirtyApply, RegistryWork,
    };
    use crate::adapter::net::behavior::org_routing_registry::NodeOrgRoutingRegistry;
    use crate::adapter::net::behavior::org_scoped_store::{
        DirtyCapabilities, PrivateDiscoveryChangeBatch,
    };

    let node = node().await;
    let scratch = Scratch::new("pin-floor", &node);
    let store = scratch.store();
    node.install_org_revocation_store(store.clone())
        .expect("install");

    let during_build: Arc<parking_lot::Mutex<Option<BuildHook>>> = Arc::default();
    let after_pin: Arc<parking_lot::Mutex<Option<BuildHook>>> = Arc::default();
    let work: Arc<RegistryWork> = Arc::default();
    let registry = NodeOrgRoutingRegistry::new(
        Arc::new(PausingSource {
            inner: ScopedSlotSource {
                scoped_discovery: node.scoped_discovery.clone(),
                publication: node.scoped_publication.clone(),
                org_revocation: node.org_revocation.clone(),
                authority: node.routing_authority.clone(),
                settle_gap_hook: parking_lot::Mutex::new(None),
                unserved_scope: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            },
            during_build: during_build.clone(),
            after_pin: after_pin.clone(),
            before_pin: Arc::default(),
        }),
        work.clone(),
        Arc::default(),
    );
    registry.activate_incarnation(1);

    let family = registry.new_family().expect("family");
    let key = slot(17, "nrpc:pin-floor");
    let _held = family.demand(key.clone()).expect("demand");

    let request = ApplyRequest {
        batch: PrivateDiscoveryChangeBatch {
            generation: 0,
            dirty: DirtyCapabilities::Clean,
        },
        registry_work: true,
    };

    // Baseline: with authority still, this settles Current.
    assert!(matches!(
        registry.apply(1, request.clone()),
        ApplyOutcome::Current { .. }
    ));

    // Publish a floor THROUGH THE REAL STORE while the COMMIT PIN IS ALREADY
    // HELD. Publishing earlier would be caught by `pin_if_current`'s token check
    // and would prove nothing about the pin's own guarantee — this is the window
    // Kyra's schedule names, and only the final complete-vector validation closes
    // it.
    registry.invalidate_for_test(&key);
    let published = Arc::new(AtomicBool::new(false));
    {
        let store = store.clone();
        let published = published.clone();
        *after_pin.lock() = Some(Box::new(move || {
            store.republish_for_test();
            published.store(true, Ordering::Release);
        }));
    }

    let outcome = registry.apply(1, request.clone());
    assert!(
        published.load(Ordering::Acquire),
        "the floor must have moved"
    );
    assert_eq!(
        outcome,
        ApplyOutcome::Superseded,
        "a quantum whose floor authority moved must NOT settle Current"
    );
    assert_eq!(registry.pending_slots(), 1, "and must re-queue");

    // Settled again once nothing is moving.
    assert!(matches!(
        registry.apply(1, request),
        ApplyOutcome::Current { .. }
    ));
}

/// (23) Authority invalidation cannot delete facts installed under the SUCCESSOR
/// authority.
///
/// The publisher releases the authority gate before invalidating, so a
/// reconciliation under the new authority can land in between. Unconditional
/// invalidation would delete that work and re-queue it, turning a just-returned
/// `Current` into immediately-owed work.
#[tokio::test]
async fn authority_invalidation_spares_successor_facts() {
    use crate::adapter::net::behavior::org_routing_registry::{
        SlotBaseFacts, SourceEpoch, SourceFacts,
    };

    let node = node().await;
    node.routing_health.store(Arc::new(
        crate::adapter::net::behavior::org_routing::RoutingHealth::Healthy { incarnation: 1 },
    ));
    let stale_key = slot(18, "nrpc:retired");
    let fresh_key = slot(18, "nrpc:successor");

    let retired = node.routing_authority.epoch();
    {
        let _gate = node.routing_authority.lock_gate();
        let _ = node.routing_authority.advance();
    }
    let live = node.routing_authority.epoch();
    assert_ne!(retired, live);

    let facts = |authority: u64| {
        Arc::new(SlotBaseFacts {
            providers: SourceFacts::Served(Arc::from([] as [PrivateCapabilityProvider; 0])),
            epoch: SourceEpoch {
                generation: node.scoped_discovery.lock().revision(),
                authority,
                floor_generation: 0,
                poisoned: false,
            },
            actor_incarnation: 1,
            slot_incarnation: 1,
            earliest_expiry: u64::MAX,
        })
    };
    node.routing_registry
        .install_facts_for_test(stale_key.clone(), facts(retired));
    let successor = facts(live);
    node.routing_registry
        .install_facts_for_test(fresh_key.clone(), successor.clone());

    node.routing_registry.invalidate_authority_older_than(live);

    assert!(
        node.routing_registry.base_facts(&stale_key).is_none(),
        "the retired-authority facts are invalidated"
    );
    let survivor = node
        .routing_registry
        .base_facts(&fresh_key)
        .expect("successor facts survive");
    assert!(
        Arc::ptr_eq(&survivor, &successor),
        "invalidation must not delete work done under the SUCCESSOR authority"
    );
}

/// (24) A floor publication cannot occupy the gap between the final validation
/// and the settlement.
///
/// The earlier floor witness publishes BEFORE the validation, which only proves
/// that completed movement is detected. This one occupies the gap Kyra named: the
/// hook fires INSIDE `settle_if_current`, after the validation has succeeded and
/// before the settlement runs. A publication launched there must not be able to
/// land, because `Current` is what causes the supervisor to publish `Healthy` —
/// sampling and then settling as two steps would make that claim false with
/// nothing left to detect it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_publication_cannot_occupy_the_gap_between_validation_and_settlement() {
    use crate::adapter::net::behavior::org_routing::{
        ApplyOutcome, ApplyRequest, DirtyApply, RegistryWork,
    };
    use crate::adapter::net::behavior::org_routing_registry::NodeOrgRoutingRegistry;
    use crate::adapter::net::behavior::org_scoped_store::{
        DirtyCapabilities, PrivateDiscoveryChangeBatch,
    };

    let node = node().await;
    let scratch = Scratch::new("settle-gap", &node);
    let store = scratch.store();
    node.install_org_revocation_store(store.clone())
        .expect("install");

    let source = ScopedSlotSource {
        scoped_discovery: node.scoped_discovery.clone(),
        publication: node.scoped_publication.clone(),
        org_revocation: node.org_revocation.clone(),
        authority: node.routing_authority.clone(),
        settle_gap_hook: parking_lot::Mutex::new(None),
        unserved_scope: Arc::new(std::sync::atomic::AtomicU64::new(0)),
    };

    let published = Arc::new(AtomicBool::new(false));
    let entered = Arc::new(AtomicBool::new(false));
    let publisher: Arc<parking_lot::Mutex<Option<std::thread::JoinHandle<()>>>> = Arc::default();
    let (reached_tx, reached_rx) = std::sync::mpsc::sync_channel::<()>(1);
    // `Receiver` is Send but not Sync, and the gap hook is an `Fn`.
    let reached = Arc::new(parking_lot::Mutex::new(reached_rx));
    // Bounded completion signal, so neither the negative assertion below nor the
    // join afterwards can hang the suite instead of failing it.
    let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
    store.arm_publish_blocking_hook(Arc::new(move || {
        let _ = reached_tx.try_send(());
    }));
    {
        let store = store.clone();
        let published = published.clone();
        let entered = entered.clone();
        let publisher = publisher.clone();
        let reached = reached.clone();
        *source.settle_gap_hook.lock() = Some(Arc::new(move || {
            entered.store(true, Ordering::Release);
            let store = store.clone();
            let landed = published.clone();
            let done = done_tx.clone();
            *publisher.lock() = Some(std::thread::spawn(move || {
                store.republish_for_test();
                landed.store(true, Ordering::Release);
                let _ = done.send(());
            }));
            // Wait for the publisher to REACH the blocked `live.write()`, not for
            // an interval — elapsed time also "passes" on a scheduler that never
            // got there (Kyra OLB-2B-E3c).
            reached
                .lock()
                .recv_timeout(Duration::from_secs(10))
                .expect("the publisher must reach the blocked live.write()");
            assert!(
                !published.load(Ordering::Acquire),
                "a floor publication landed between the validation and the                  settlement — they are separately interleavable"
            );
        }));
    }

    let work: Arc<RegistryWork> = Arc::default();
    let registry = NodeOrgRoutingRegistry::new(Arc::new(source), work.clone(), Arc::default());
    registry.activate_incarnation(1);

    let family = registry.new_family().expect("family");
    let key = slot(19, "nrpc:settle-gap");
    let _held = family.demand(key.clone()).expect("demand");
    let outcome = registry.apply(
        1,
        ApplyRequest {
            batch: PrivateDiscoveryChangeBatch {
                generation: 0,
                dirty: DirtyCapabilities::Clean,
            },
            registry_work: true,
        },
    );

    assert!(
        entered.load(Ordering::Acquire),
        "the gap must have been entered"
    );
    done_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("the publication must land once the barrier is released");
    if let Some(handle) = publisher.lock().take() {
        handle.join().expect("publisher");
    }
    assert!(
        published.load(Ordering::Acquire),
        "the publication must land once the barrier is released"
    );
    assert!(
        matches!(outcome, ApplyOutcome::Current { .. }),
        "the settlement was protected, so it is sound: {outcome:?}"
    );
}

/// (25) Poison cannot occupy the gap between the final validation and the
/// settlement.
///
/// Poison is a path-registry write that `live` does not order, so the
/// publication barrier alone leaves the validation and the settlement
/// independently interleavable. The pin therefore holds the store's POISON GATE
/// too, and a poison transition launched inside the gap must block — `Current` is
/// what causes the supervisor to publish `Healthy` (Kyra OLB-2B-E3c).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn poison_cannot_occupy_the_gap_between_validation_and_settlement() {
    use crate::adapter::net::behavior::org_routing::{
        ApplyOutcome, ApplyRequest, DirtyApply, RegistryWork,
    };
    use crate::adapter::net::behavior::org_routing_registry::NodeOrgRoutingRegistry;
    use crate::adapter::net::behavior::org_scoped_store::{
        DirtyCapabilities, PrivateDiscoveryChangeBatch,
    };

    let node = node().await;
    let scratch = Scratch::new("settle-poison", &node);
    let store = scratch.store();
    node.install_org_revocation_store(store.clone())
        .expect("install");

    let source = ScopedSlotSource {
        scoped_discovery: node.scoped_discovery.clone(),
        publication: node.scoped_publication.clone(),
        org_revocation: node.org_revocation.clone(),
        authority: node.routing_authority.clone(),
        settle_gap_hook: parking_lot::Mutex::new(None),
        unserved_scope: Arc::new(std::sync::atomic::AtomicU64::new(0)),
    };

    let poisoned = Arc::new(AtomicBool::new(false));
    let entered = Arc::new(AtomicBool::new(false));
    let contender: Arc<parking_lot::Mutex<Option<std::thread::JoinHandle<()>>>> = Arc::default();
    // Fired by the store itself ONLY when the contender's `try_lock` OBSERVED
    // the poison gate held — i.e. it actually met the pin's exclusion (E3c
    // blockers §3). The previous form acknowledged before the acquisition was
    // even attempted, so it proved only that the contender was scheduled: a
    // slow scheduler could satisfy the wait and leave the negative assertion
    // below to pass vacuously with the gate protection broken. If the mark
    // ever stops taking the gate — or the pin stops holding it — this ack
    // never fires and the wait fails loudly.
    let (at_gate_tx, at_gate_rx) = std::sync::mpsc::sync_channel::<()>(1);
    let at_gate = Arc::new(parking_lot::Mutex::new(at_gate_rx));
    store.arm_poison_contended_hook(Arc::new(move || {
        let _ = at_gate_tx.try_send(());
    }));
    let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
    let done_rx = Arc::new(parking_lot::Mutex::new(done_rx));
    {
        let store = store.clone();
        let poisoned = poisoned.clone();
        let entered = entered.clone();
        let contender = contender.clone();
        let at_gate = at_gate.clone();
        let done_rx = done_rx.clone();
        *source.settle_gap_hook.lock() = Some(Arc::new(move || {
            entered.store(true, Ordering::Release);
            let contender_store = store.clone();
            let landed = poisoned.clone();
            let done = done_tx.clone();
            *contender.lock() = Some(std::thread::spawn(move || {
                contender_store.mark_poisoned_for_test();
                landed.store(true, Ordering::Release);
                let _ = done.send(());
            }));
            at_gate
                .lock()
                .recv_timeout(Duration::from_secs(10))
                .expect("the contender must OBSERVE the poison gate held");
            // Deterministic rather than time-bounded: the ack above proves the
            // contender was blocked at the HELD gate, and the gate cannot drop
            // before this hook returns — so any ungated route to completion
            // would already have been taken.
            assert!(
                done_rx.lock().try_recv().is_err(),
                "poison landed between the validation and the settlement — they \
                 are separately interleavable"
            );
            assert!(
                !poisoned.load(Ordering::Acquire),
                "poison landed between the validation and the settlement — they \
                 are separately interleavable"
            );
            // The FACT itself, not the contender's return: a mark that mutated
            // the registry before taking the gate would leave `done` unsent
            // (still blocked) while the poison is already live inside the gap
            // — exactly what the two proxies above cannot see.
            assert!(
                !store.is_poisoned(),
                "the poison FACT landed inside the validation-settlement gap — \
                 the mark mutated the registry before taking the gate"
            );
        }));
    }

    let work: Arc<RegistryWork> = Arc::default();
    let registry = NodeOrgRoutingRegistry::new(Arc::new(source), work.clone(), Arc::default());
    registry.activate_incarnation(1);

    let family = registry.new_family().expect("family");
    let key = slot(20, "nrpc:settle-poison");
    let _held = family.demand(key.clone()).expect("demand");
    let outcome = registry.apply(
        1,
        ApplyRequest {
            batch: PrivateDiscoveryChangeBatch {
                generation: 0,
                dirty: DirtyCapabilities::Clean,
            },
            registry_work: true,
        },
    );

    assert!(
        entered.load(Ordering::Acquire),
        "the gap must have been entered"
    );
    done_rx
        .lock()
        .recv_timeout(Duration::from_secs(10))
        .expect("poison must land once the pin is released");
    if let Some(handle) = contender.lock().take() {
        handle.join().expect("contender");
    }
    assert!(
        poisoned.load(Ordering::Acquire),
        "poison must land once the pin is released"
    );
    assert!(
        matches!(outcome, ApplyOutcome::Current { .. }),
        "the settlement was protected, so it is sound: {outcome:?}"
    );
}

/// (25b) Poison completing BEFORE the validation is detected: the pass reports
/// `Superseded` and re-queues rather than settling.
#[tokio::test]
async fn poison_before_the_validation_is_detected() {
    use crate::adapter::net::behavior::org_routing::{
        ApplyOutcome, ApplyRequest, DirtyApply, RegistryWork,
    };
    use crate::adapter::net::behavior::org_routing_registry::NodeOrgRoutingRegistry;
    use crate::adapter::net::behavior::org_scoped_store::{
        DirtyCapabilities, PrivateDiscoveryChangeBatch,
    };

    let node = node().await;
    let scratch = Scratch::new("poison-early", &node);
    let store = scratch.store();
    node.install_org_revocation_store(store.clone())
        .expect("install");

    let after_pin: Arc<parking_lot::Mutex<Option<BuildHook>>> = Arc::default();
    let work: Arc<RegistryWork> = Arc::default();
    let registry = NodeOrgRoutingRegistry::new(
        Arc::new(PausingSource {
            inner: ScopedSlotSource {
                scoped_discovery: node.scoped_discovery.clone(),
                publication: node.scoped_publication.clone(),
                org_revocation: node.org_revocation.clone(),
                authority: node.routing_authority.clone(),
                settle_gap_hook: parking_lot::Mutex::new(None),
                unserved_scope: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            },
            during_build: Arc::default(),
            after_pin: after_pin.clone(),
            before_pin: Arc::default(),
        }),
        work.clone(),
        Arc::default(),
    );
    registry.activate_incarnation(1);

    let family = registry.new_family().expect("family");
    let key = slot(21, "nrpc:poison-early");
    let _held = family.demand(key.clone()).expect("demand");
    let request = ApplyRequest {
        batch: PrivateDiscoveryChangeBatch {
            generation: 0,
            dirty: DirtyCapabilities::Clean,
        },
        registry_work: true,
    };
    assert!(matches!(
        registry.apply(1, request.clone()),
        ApplyOutcome::Current { .. }
    ));
    registry.invalidate_for_test(&key);

    let landed = Arc::new(AtomicBool::new(false));
    {
        let store = store.clone();
        let landed = landed.clone();
        *after_pin.lock() = Some(Box::new(move || {
            store.mark_poisoned_for_test();
            landed.store(true, Ordering::Release);
        }));
    }

    let outcome = registry.apply(1, request);
    assert!(landed.load(Ordering::Acquire), "poison must have landed");
    assert_eq!(
        outcome,
        ApplyOutcome::Superseded,
        "poison completing before the validation must NOT settle Current"
    );
    assert_eq!(registry.pending_slots(), 1, "and must re-queue");
}

/// (25c) Under STEADY poison, a retry settles `Current` over an entirely
/// `Unserved` source — and every slot reads cold.
///
/// This pins the contract deliberately (Kyra's Option A). `Current` means "the
/// registry has completely reconciled the CURRENT source state", including an
/// unusable one; it does not mean routes are usable. Usability is per-slot and
/// lives in `org_routing_base_facts`, which returns cold for `Unserved`. The
/// alternative — treating poison as a fence that can never settle — would retry
/// forever under steady poison, which is why it is not the design.
#[tokio::test]
async fn steady_poison_settles_current_over_an_unserved_source() {
    use crate::adapter::net::behavior::org_routing::{ApplyOutcome, ApplyRequest, DirtyApply};
    use crate::adapter::net::behavior::org_scoped_store::{
        DirtyCapabilities, PrivateDiscoveryChangeBatch,
    };

    let node = node().await;
    let scratch = Scratch::new("poison-steady", &node);
    let store = scratch.store();
    node.install_org_revocation_store(store.clone())
        .expect("install");
    store.mark_poisoned_for_test();

    // The NODE's own registry, not a look-alike built beside it: the cold-read
    // assertion below goes through `node.org_routing_base_facts`, which reads
    // `node.routing_registry`. Driving a separate registry would make that
    // assertion pass because the node's registry never heard of the slot.
    node.routing_health.store(Arc::new(
        crate::adapter::net::behavior::org_routing::RoutingHealth::Healthy { incarnation: 1 },
    ));
    let registry = node.routing_registry.clone();
    registry.activate_incarnation(1);

    let family = registry.new_family().expect("family");
    let key = slot(22, "nrpc:poison-steady");
    let _held = family.demand(key.clone()).expect("demand");

    let outcome = registry.apply(
        1,
        ApplyRequest {
            batch: PrivateDiscoveryChangeBatch {
                generation: 0,
                dirty: DirtyCapabilities::Clean,
            },
            registry_work: true,
        },
    );
    assert!(
        matches!(outcome, ApplyOutcome::Current { .. }),
        "steady poison must CONVERGE, not retry forever: {outcome:?}"
    );
    assert_eq!(registry.pending_slots(), 0, "nothing is owed");
    let facts = registry
        .base_facts(&key)
        .expect("the slot IS reconciled — with unusable-source facts");
    assert!(
        matches!(
            facts.providers,
            crate::adapter::net::behavior::org_routing_registry::SourceFacts::Unserved
        ),
        "a poisoned authority can speak for no scope"
    );
    assert!(
        facts.epoch.poisoned,
        "and the facts are STAMPED with the poisoned authority, so a later \
         recovery is detectable"
    );
    assert!(
        node.org_routing_base_facts(&key).is_none(),
        "but it reads COLD: reconciled is not usable"
    );
    assert_eq!(
        registry.pending_slots(),
        0,
        "and a cold read under STEADY poison re-queues nothing — the epoch \
         comparison catches transitions, not the steady state"
    );
}

/// (27) A PRODUCTION poison clear cannot land inside a live [`PublicationPin`].
///
/// The mirror of (25), and the half that was missing. The recovery paths used to
/// call the raw path-registry helper, so a pin that had validated
/// `poisoned == true` could watch the clear land between its validation and its
/// settlement and report `Current` for an authority that was already gone.
///
/// The schedule is the reachable one, not a synthetic one: recovery publishes
/// the durable view FIRST (nothing is pinned yet, so it lands), and only then
/// reaches the clear. The pin is taken in exactly that window.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_recovery_poison_clear_cannot_land_under_a_publication_pin() {
    use crate::adapter::net::behavior::org_revocation::OrgRevocationStore;

    let node = node().await;
    let scratch = Scratch::new("poison-clear", &node);
    let store = scratch.store();
    store.mark_poisoned_for_test();
    assert!(store.is_poisoned(), "precondition: the path is poisoned");

    // PLACEMENT hook: fired before the clear attempts the acquisition — i.e.
    // after the recovery's republish has already landed. It rendezvouses so
    // the pin is taken BEFORE the acquisition is attempted. Placement is not
    // contention evidence; that is the CONTENDED ack below (E3c blockers §3).
    let (at_gate_tx, at_gate_rx) = std::sync::mpsc::sync_channel::<()>(1);
    let (pinned_tx, pinned_rx) = std::sync::mpsc::channel::<()>();
    let pinned_rx = parking_lot::Mutex::new(pinned_rx);
    store.arm_poison_blocking_hook(Arc::new(move || {
        let _ = at_gate_tx.try_send(());
        let _ = pinned_rx.lock().recv_timeout(Duration::from_secs(10));
    }));
    // Fired ONLY when the clear's `try_lock` observed the pin's gate hold —
    // the acknowledgement the negative assertion below is sequenced after.
    let (contended_tx, contended_rx) = std::sync::mpsc::sync_channel::<()>(1);
    store.arm_poison_contended_hook(Arc::new(move || {
        let _ = contended_tx.try_send(());
    }));

    let (done_tx, done_rx) = std::sync::mpsc::channel::<bool>();
    let path = scratch.path().join("revocation.json");
    let recovery = std::thread::spawn(move || {
        let recovered = OrgRevocationStore::open_existing(&path);
        let _ = done_tx.send(recovered.is_ok());
    });

    at_gate_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("the recovery must REACH the poison gate");
    // The republish has landed; take the pin the way a settlement does.
    let pin = store.pin_publication();
    let _ = pinned_tx.send(());
    // Only after the clear OBSERVED the pin's gate hold is there a contention
    // to assert about (E3c blockers §3): the previous 250ms bounded wait could
    // pass vacuously with the recovery still unscheduled, protection broken.
    contended_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("the recovery's clear must OBSERVE the pin's poison-gate hold");
    assert!(
        done_rx.try_recv().is_err(),
        "a production recovery cleared poison while a publication pin was alive \
         — the clear bypasses the poison gate"
    );
    assert!(
        store.is_poisoned(),
        "poison must be held immobile in BOTH directions under the pin"
    );
    drop(pin);
    assert!(
        done_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("the clear must land once the pin drops"),
        "the recovery itself must succeed"
    );
    assert!(!store.is_poisoned(), "recovery clears the poison");
    recovery.join().expect("recovery thread");
}

/// (28) A real recovery RETIRES the `Unserved` reconstruction it left behind,
/// wakes routing without waiting for a reader, and lets the successor serve.
///
/// This is the other half of Option A. `Current` over an unusable source is only
/// an acceptable steady state if leaving it is observable. A recovery
/// republishes the same durable view, so it raises no floor and
/// `StoreCore::notify` is silent — without an explicit authority wake the
/// registry stays reconciled to obsolete `Unserved` facts indefinitely.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_poison_recovery_retires_the_unserved_reconstruction_it_left() {
    use crate::adapter::net::behavior::org_revocation::OrgRevocationStore;
    use crate::adapter::net::behavior::org_routing::{ApplyOutcome, ApplyRequest, DirtyApply};
    use crate::adapter::net::behavior::org_routing_registry::SourceFacts;
    use crate::adapter::net::behavior::org_scoped_store::{
        DirtyCapabilities, PrivateDiscoveryChangeBatch,
    };

    let node = node().await;
    let scratch = Scratch::new("poison-recover", &node);
    let store = scratch.store();
    node.install_org_revocation_store(store.clone())
        .expect("install");
    store.mark_poisoned_for_test();
    node.routing_health.store(Arc::new(
        crate::adapter::net::behavior::org_routing::RoutingHealth::Healthy { incarnation: 1 },
    ));

    let registry = node.routing_registry.clone();
    registry.activate_incarnation(1);
    let family = registry.new_family().expect("family");
    let key = slot(23, "nrpc:poison-recover");
    let _held = family.demand(key.clone()).expect("demand");
    let request = ApplyRequest {
        batch: PrivateDiscoveryChangeBatch {
            generation: 0,
            dirty: DirtyCapabilities::Clean,
        },
        registry_work: true,
    };

    assert!(
        matches!(
            registry.apply(1, request.clone()),
            ApplyOutcome::Current { .. }
        ),
        "steady poison converges (Option A)"
    );
    let stranded = registry.base_facts(&key).expect("reconciled");
    assert!(
        matches!(stranded.providers, SourceFacts::Unserved),
        "precondition: the reconstruction serves nothing"
    );
    assert!(stranded.epoch.poisoned, "precondition: stamped as poisoned");
    assert!(
        node.org_routing_base_facts(&key).is_none(),
        "precondition: it reads cold"
    );
    let authority_before = node.routing_authority.epoch();

    // REAL recovery through the production open path: prove the entry durable,
    // republish through the shared core, clear the poison. BOUNDED (E3c
    // blockers §4): the open serializes behind the interprocess lock and the
    // poison gate, so a regression that wedges either would otherwise hang
    // this witness rather than fail it.
    let path = scratch.path().join("revocation.json");
    let recovered = tokio::time::timeout(
        Duration::from_secs(10),
        tokio::task::spawn_blocking(move || OrgRevocationStore::open_existing(&path)),
    )
    .await
    .expect("bounded: the recovery open must complete")
    .expect("join")
    .expect("recovery must succeed");
    assert!(!store.is_poisoned(), "recovery clears the poison");
    assert!(
        recovered.shares_core_with(&store),
        "and it recovers the SAME live core the node installed"
    );

    // The proactive wake: a recovery raises no floor, so this is the only signal
    // routing gets — and it must not depend on anyone reading first.
    assert!(
        node.routing_authority.epoch() > authority_before,
        "a poison recovery must move routing authority even though it raised no \
         floor"
    );
    assert!(
        registry.base_facts(&key).is_none(),
        "and it must RETIRE the Unserved reconstruction, not leave it reconciled"
    );
    assert_eq!(
        registry.pending_slots(),
        1,
        "re-queuing the exact slot, so the actor rebuilds rather than leaving a \
         hole"
    );

    // And the successor reconciliation is built over the recovered authority.
    assert!(
        matches!(registry.apply(1, request), ApplyOutcome::Current { .. }),
        "the successor quantum settles"
    );
    let successor = registry.base_facts(&key).expect("reconciled again");
    assert!(
        !successor.epoch.poisoned,
        "the successor is stamped against the RECOVERED authority"
    );
    assert!(
        matches!(successor.providers, SourceFacts::Served(_)),
        "and the source speaks for the scope again"
    );
    assert!(
        node.org_routing_base_facts(&key).is_some(),
        "so the slot reads warm — recovery is observable end to end"
    );
}

/// (28b) The MARK counterpart of (28): a durability poison that raises NO floor
/// still wakes routing — proactively, with no reader involved — and the
/// untouched slots converge to `Current` over an `Unserved` source.
///
/// The schedule is the reachable one, all on production paths. A post-rename
/// failure leaves the LIVE view ahead of what disk can prove (the directory
/// entry may resolve to the old bytes — that rollback is the uncertainty
/// PostRename names). Recovery clears the poison but only rereads; disk stays
/// behind. A bundle that then raises the DISK state to a level the live view
/// already enforces post-rename-fails with an EMPTY raise set: `notify` is
/// silent, and before this closure the mark produced no wake at all — unlike
/// the recovery clear, which always did.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_poison_mark_that_raises_no_floor_wakes_routing_without_a_reader() {
    use crate::adapter::net::behavior::org::{OrgId, OrgKeypair, OrgRevocationBundle};
    use crate::adapter::net::behavior::org_revocation::{OrgRevocationError, OrgRevocationStore};
    use crate::adapter::net::behavior::org_routing::{ApplyOutcome, ApplyRequest, DirtyApply};
    use crate::adapter::net::behavior::org_routing_registry::SourceFacts;
    use crate::adapter::net::behavior::org_scoped_store::{
        DirtyCapabilities, PrivateDiscoveryChangeBatch,
    };

    let node = node().await;
    let scratch = Scratch::new("poison-mark", &node);
    let store = scratch.store();
    node.install_org_revocation_store(store.clone())
        .expect("install");
    node.routing_health.store(Arc::new(
        crate::adapter::net::behavior::org_routing::RoutingHealth::Healthy { incarnation: 1 },
    ));
    let registry = node.routing_registry.clone();
    registry.activate_incarnation(1);
    let family = registry.new_family().expect("family");
    let key = slot(27, "nrpc:poison-mark");
    let _held = family.demand(key.clone()).expect("demand");
    let request = ApplyRequest {
        batch: PrivateDiscoveryChangeBatch {
            generation: 0,
            dirty: DirtyCapabilities::Clean,
        },
        registry_work: true,
    };
    assert!(matches!(
        registry.apply(1, request.clone()),
        ApplyOutcome::Current { .. }
    ));
    assert!(
        node.org_routing_base_facts(&key).is_some(),
        "precondition: warm over a healthy authority"
    );

    let floor_org = OrgKeypair::from_bytes([0x42u8; 32]);
    let floor_member = EntityId::from_bytes([0x24u8; 32]);
    let org_id: OrgId = floor_org.org_id();
    let bundle = |floor: u32| {
        let mut floors = std::collections::BTreeMap::new();
        floors.insert(floor_member.clone(), floor);
        OrgRevocationBundle::try_issue(&floor_org, &floors).expect("issue")
    };

    // A post-rename failure whose bundle DOES raise: the live view advances,
    // disk keeps the old bytes, the path poisons — and the non-empty raise
    // wakes routing through `notify` alone, exactly once (no double bump).
    let epoch_before = node.routing_authority.epoch();
    let err = {
        store.arm_forced_post_rename_for_test();
        store
            .apply_bundle(&bundle(5))
            .expect_err("durability must be uncertain")
    };
    assert!(matches!(
        err,
        OrgRevocationError::DurabilityUncertain { .. }
    ));
    assert!(store.is_poisoned(), "the raising mark poisoned the path");
    assert_eq!(
        node.routing_authority.epoch(),
        epoch_before + 1,
        "a RAISING mark wakes through `notify` — and exactly once"
    );
    assert_eq!(
        store.floor_for(&org_id, &floor_member),
        5,
        "the live view is ahead of what disk can prove"
    );
    assert!(matches!(
        registry.apply(1, request.clone()),
        ApplyOutcome::Current { .. }
    ));

    // REAL recovery through the production open path: poison clears, the
    // locked reread republishes DISK's older state — and the live view must
    // not weaken (per-key max), so disk remains behind it.
    let path = scratch.path().join("revocation.json");
    let recovered = tokio::time::timeout(
        Duration::from_secs(10),
        tokio::task::spawn_blocking(move || OrgRevocationStore::open_existing(&path)),
    )
    .await
    .expect("bounded: the recovery open must complete")
    .expect("join")
    .expect("recovery must succeed");
    assert!(recovered.shares_core_with(&store), "same live core");
    assert!(!store.is_poisoned(), "recovery clears the poison");
    assert_eq!(
        store.floor_for(&org_id, &floor_member),
        5,
        "recovery rereads older disk bytes but never weakens the live view"
    );
    assert!(matches!(
        registry.apply(1, request.clone()),
        ApplyOutcome::Current { .. }
    ));
    let warm = registry.base_facts(&key).expect("reconciled");
    assert!(
        !warm.epoch.poisoned,
        "precondition: warm again, stamped against the recovered authority"
    );

    // THE mark under witness: the bundle raises disk (0 → 3) but not the
    // already-ahead live view (5), so `raised` is EMPTY and `notify` says
    // nothing.
    let authority_before = node.routing_authority.epoch();
    let err = {
        store.arm_forced_post_rename_for_test();
        store
            .apply_bundle(&bundle(3))
            .expect_err("durability must be uncertain again")
    };
    assert!(matches!(
        err,
        OrgRevocationError::DurabilityUncertain { .. }
    ));
    assert!(store.is_poisoned(), "the empty-raise mark landed");
    assert_eq!(
        store.floor_for(&org_id, &floor_member),
        5,
        "and raised no floor"
    );

    // The proactive wake, with NO reader involved:
    assert_eq!(
        node.routing_authority.epoch(),
        authority_before + 1,
        "an empty-raise MARK owes the same wake as the recovery clear"
    );
    assert!(
        registry.base_facts(&key).is_none(),
        "the pre-poison reconstruction was RETIRED, not left reconciled"
    );
    assert_eq!(registry.pending_slots(), 1, "re-queuing the exact slot");

    // And the successor converges to Option A's steady state.
    assert!(matches!(
        registry.apply(1, request),
        ApplyOutcome::Current { .. }
    ));
    let successor = registry.base_facts(&key).expect("reconciled again");
    assert!(
        matches!(successor.providers, SourceFacts::Unserved),
        "Current over an Unserved source"
    );
    assert!(
        successor.epoch.poisoned,
        "stamped against the poisoned authority, so a later recovery is \
         detectable"
    );
    assert!(
        node.org_routing_base_facts(&key).is_none(),
        "and reads cold"
    );
}

/// (29) The LAZY half, isolated: a READER retires an `Unserved` reconstruction
/// whose poisoned authority has recovered, with no wake involved at all — and
/// re-queues nothing while the poison is steady.
///
/// The store is placed in the node's slot WITHOUT a raise subscription, so the
/// proactive wake (28) cannot be what repairs this. And the second artifact
/// differs from the live authority in the POISON BIT ALONE — same authority
/// epoch, same floor generation — so the retirement is attributable to the epoch
/// comparison and nothing else.
#[tokio::test]
async fn a_reader_retires_unserved_facts_once_their_poison_clears() {
    use crate::adapter::net::behavior::org_revocation::OrgRevocationStore;
    use crate::adapter::net::behavior::org_routing_registry::{
        SlotBaseFacts, SourceEpoch, SourceFacts,
    };

    let node = node().await;
    node.routing_health.store(Arc::new(
        crate::adapter::net::behavior::org_routing::RoutingHealth::Healthy { incarnation: 1 },
    ));
    let scratch = Scratch::new("poison-lazy", &node);
    let store = scratch.store();
    // Deliberately NOT `install_org_revocation_store`: no raise subscription, so
    // nothing can wake routing and only the read seam can repair this.
    node.org_revocation.store(Some(store.clone()));
    store.mark_poisoned_for_test();

    let poisoned_facts = |floor_generation: u64| {
        Arc::new(SlotBaseFacts {
            providers: SourceFacts::Unserved,
            epoch: SourceEpoch {
                generation: node.scoped_discovery.lock().revision(),
                authority: node.routing_authority.epoch(),
                floor_generation,
                poisoned: true,
            },
            actor_incarnation: 1,
            slot_incarnation: 1,
            earliest_expiry: u64::MAX,
        })
    };

    let key_a = slot(24, "nrpc:poison-lazy-steady");
    let live_floor = store.barriered_generation().expect("not exhausted").get();
    node.routing_registry
        .install_facts_for_test(key_a.clone(), poisoned_facts(live_floor));
    assert!(
        node.org_routing_base_facts(&key_a).is_none(),
        "an unusable source reads cold"
    );
    assert_eq!(
        node.org_routing_slots().1,
        0,
        "and STEADY poison re-queues nothing: reading live poison as a staleness \
         predicate would churn this slot on every read, forever"
    );

    // Real recovery through the production open path, bounded like (28)'s
    // (E3c blockers §4).
    let path = scratch.path().join("revocation.json");
    let _recovered = tokio::time::timeout(
        Duration::from_secs(10),
        tokio::task::spawn_blocking(move || OrgRevocationStore::open_existing(&path)),
    )
    .await
    .expect("bounded: the recovery open must complete")
    .expect("join")
    .expect("recovery must succeed");
    assert!(!store.is_poisoned(), "recovery clears the poison");

    assert!(
        node.org_routing_base_facts(&key_a).is_none(),
        "the obsolete reconstruction still reads cold"
    );
    assert!(
        node.routing_registry.base_facts(&key_a).is_none(),
        "but the reader RETIRED it rather than leaving it reconciled forever"
    );
    assert_eq!(node.org_routing_slots().1, 1, "re-queuing the exact slot");

    // Poison bit ALONE: same authority epoch, same (recovered) floor generation.
    let key_b = slot(24, "nrpc:poison-lazy-isolated");
    let recovered_floor = store.barriered_generation().expect("not exhausted").get();
    node.routing_registry
        .install_facts_for_test(key_b.clone(), poisoned_facts(recovered_floor));
    assert!(node.org_routing_base_facts(&key_b).is_none());
    assert!(
        node.routing_registry.base_facts(&key_b).is_none(),
        "a fact differing from the live authority ONLY in the poison bit must \
         still be retired"
    );
    assert_eq!(node.org_routing_slots().1, 2, "and re-queued");

    node.org_revocation.store(None);
}

/// (26) A terminally exhausted publication generation stops owner-certified
/// emission entirely — no owner certificate AND no scoped envelope.
///
/// Representing "no store" and "store exhausted" as one `None` made them compare
/// EQUAL, so the seqlock's raw equality passed and the cached certified
/// announcement kept shipping.
#[tokio::test]
async fn an_exhausted_store_generation_stops_certified_emission() {
    let node = node().await;
    let org = crate::adapter::net::behavior::org::OrgKeypair::generate();
    let entity = node.entity_id().clone();
    let cert = crate::adapter::net::behavior::org::OrgMembershipCert::try_issue(
        &org,
        entity.clone(),
        1,
        3600,
    )
    .expect("cert");
    let scratch = Scratch::new("send-exhausted", &node);
    let authority = crate::adapter::net::behavior::org_authority::NodeAuthority::adopt(
        scratch.path(),
        cert,
        &entity,
        0,
        None,
    )
    .expect("adopt");
    node.install_node_authority(Arc::new(authority))
        .expect("install authority");
    let _ = node.set_owner_cert_emission(true);

    let store = scratch.store();
    node.install_org_revocation_store(store.clone())
        .expect("install store");

    // There must BE an announcement for the send path to certify.
    node.announce_capabilities(crate::adapter::net::behavior::capability::CapabilitySet::default())
        .await
        .expect("announce");

    let now = crate::adapter::net::behavior::org::current_timestamp();
    let live_authority = node.node_authority().expect("authority installed");
    assert!(
        node.owner_cert_under(&live_authority, now).is_some(),
        "precondition: the send path would certify this announcement"
    );
    assert!(
        node.announcement_bytes_for_send_for_test().is_some(),
        "precondition: there is something to send"
    );

    store.saturate_generation_for_test();
    store.republish_for_test();
    assert!(
        store.barriered_generation().is_err(),
        "terminally exhausted"
    );

    // Either acceptable terminal behaviour: suppress the send, or emit only the
    // public cert-free form. Never a certificate, never a scoped envelope.
    assert!(
        node.owner_cert_under(&live_authority, now).is_none(),
        "terminal exhaustion must not construct an owner certificate — the floor          comparison it rests on can no longer be shown current"
    );
    // WEAK, and deliberately labelled so: this node establishes no owner-scoped
    // service, so the set is empty before exhaustion too. It is a regression
    // guard against exhaustion ever ADDING envelopes, not a proof that existing
    // ones are suppressed. Proving that needs an owner-audience credential and a
    // scoped service, which belongs with the scoped-emission witnesses rather
    // than here — recorded as a known evidence gap.
    assert!(
        node.announcement_scoped_for_send_for_test().is_empty(),
        "terminal exhaustion must not emit owner/grant-scoped envelopes"
    );

    // And the seqlock alias itself: two exhausted stamps must not compare equal
    // -and-current, which is what kept the CACHED certified announcement shipping.
    let exhausted_stamp = node.security_stamp();
    assert!(
        !exhausted_stamp.is_current(&exhausted_stamp),
        "an exhausted send stamp must never compare current, even against itself"
    );
}

/// (30) The defensive per-provider dedup keeps the NEWEST generation.
///
/// `Vec::dedup_by` passes elements in reverse slice order and removes its first
/// argument, so the FIRST of each run survives. With the tiebreak sorted
/// ascending that was the OLDEST announcement — a silently-stale route rather
/// than a refused one. Unreachable through
/// `find_scope_exact_private_providers`, which keys on `(scope, provider)` under
/// exact scope equality and so yields one row per provider; this drives the
/// snapshot directly so the defense is actually exercised (review-pass-2 §5).
#[tokio::test]
async fn duplicate_provider_rows_collapse_to_the_newest_generation() {
    let node = node().await;
    let key = slot(25, "nrpc:dedup");
    let provider = node.entity_id().clone();
    let org = crate::adapter::net::behavior::org::OrgId::from_bytes([25u8; 32]);
    let row = |generation: u64, expires_at: u64| PrivateCapabilityProvider {
        provider: provider.clone(),
        owner_org: org,
        expires_at,
        generation,
    };

    // Deliberately supplied OUT of order, so the assertion cannot pass merely
    // because the input happened to be sorted the right way.
    let snapshot = ScopedSourceSnapshot {
        token: SourceToken::default(),
        rows: [(key.clone(), vec![row(3, 300), row(9, 900), row(5, 500)])]
            .into_iter()
            .collect(),
    };

    let SourceFacts::Served(providers) = snapshot.providers(&key) else {
        panic!("a captured scope must reconstruct as Served");
    };
    assert_eq!(providers.len(), 1, "one row per provider survives");
    assert_eq!(
        providers[0].generation, 9,
        "the dedup must keep the NEWEST announcement, not the oldest"
    );
    assert_eq!(
        providers[0].expires_at, 900,
        "and the surviving row must be that announcement's, not a mix"
    );
}
