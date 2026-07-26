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
                authority: node.routing_authority.clone(),
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
    fn new(tag: &str, node: &MeshNode) -> Self {
        let path = std::env::temp_dir().join(format!(
            "net-olb2b-e3c-{tag}-{}-{}",
            std::process::id(),
            node.entity_id()
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
    tokio::task::spawn_blocking(move || blocked.recv())
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
        unserved_scope: node.routing_unserved_scope.clone(),
    };
    let key = slot(12, "nrpc:exhausted");

    node.routing_authority
        .epoch
        .store(u64::MAX, Ordering::Release);
    assert!(!node.routing_authority.is_exhausted());

    {
        let _gate = node.routing_authority.lock_gate();
        node.routing_authority.advance();
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
        node.routing_authority.advance();
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
