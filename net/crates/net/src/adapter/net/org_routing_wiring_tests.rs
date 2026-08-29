//! OLB-2B-E3c: the production routing-plane wiring witnesses.
//!
//! Included from `mesh.rs`, so it sees the node's private fields — which is what
//! lets these assert on the real drain lease and the real task handle rather than
//! on a proxy.

use super::*;
use crate::adapter::net::behavior::org_grant::CapabilityAuthorityId;
use crate::adapter::net::behavior::org_routing_registry::{
    GrantArtifactFence, GrantMovementFence, PrivateAudienceScope, ScopedDiscoveryAuthorityStamp,
    ScopedSourceFacts, SlotKey, SlotSource, SourceCommitPin, SourceFacts, SourceSnapshot,
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

/// Sample the reconciliation counters at a proven ACTOR-IDLE point.
///
/// A witness that asserts an exact delta has to know its baseline is a completed
/// state. Sampling straight after an `until(..artifact exists..)` does not: the
/// artifact becomes visible before the pass that installed it finishes
/// accounting, so a warm-up install can land on the far side of the sample and
/// show up as the transition's own work.
///
/// Equal counter samples do not fix that either, and the earlier form of this
/// helper — three equal samples 20ms apart — was the same vacuous gate in a
/// longer shape. Counters that stopped moving are evidence of a SCHEDULING GAP:
/// a pass spends its reconstruction phase holding no lock and touching no
/// metric, for as long as the sampler is willing to call that quiet. What is
/// needed is an ITERATION BOUNDARY, and only the actor can report one.
///
/// So this drives the barrier rather than waiting to be convinced by it:
///
/// ```text
/// mark registry work   -> wake the actor exactly as first demand wakes it
/// wait for a pass      -> an iteration boundary, reported by the actor itself
/// again                -> so the second pass STARTED after the first finished
/// re-read              -> counters unchanged across the window, nothing pending
/// ```
///
/// Twice, not once. One wake produces exactly one pass, so a single probe cannot
/// distinguish the pass it woke from one that was already in flight and would
/// have run anyway. After two boundaries, whatever was in flight when this call
/// began has completed and at least one whole pass has run inside the window.
///
/// The probe cannot manufacture the work it is measuring: a pass woken with
/// nothing pending selects nothing, builds nothing and settles. And if it does
/// find work owed — a re-queue, a restarted recapture — the counters move, the
/// loop notices, and it asks again rather than returning a baseline that is
/// still warming.
async fn quiesced_counts(node: &MeshNode) -> [u64; 7] {
    for _ in 0..100 {
        let before = node.org_routing_reconciliation_counts();
        for _ in 0..2 {
            let passes = node.org_routing_actor_passes();
            node.mark_org_routing_work();
            assert!(
                until(|| node.org_routing_actor_passes() > passes).await,
                "the routing actor never completed a pass, so no exact delta is \
                 measurable"
            );
        }
        let (_retained, pending) = node.org_routing_slots();
        if pending == 0 && node.org_routing_reconciliation_counts() == before {
            return before;
        }
    }
    panic!("the routing actor never quiesced, so no exact delta is measurable");
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
    fn providers(&self, key: &SlotKey) -> ScopedSourceFacts {
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
    fn pin_if_current(
        &self,
        _keys: &[SlotKey],
        expected: &SourceToken,
    ) -> Option<Box<dyn SourceCommitPin + '_>> {
        let hook = self.before_pin.lock().take();
        if let Some(hook) = hook {
            hook();
        }
        let pin = self.inner.pin_if_current(_keys, expected)?;
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
                session_routing: node.session_routing.clone(),
                scoped_discovery: scoped.clone(),
                publication: publication.clone(),
                org_revocation: node.org_revocation.clone(),
                consumer_grants: node.consumer_grant_audiences.clone(),
                consumer_grant_gate: node.consumer_grant_gate.clone(),
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
        registry.base_facts_unvalidated(&key).is_none(),
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
            .base_facts_unvalidated(&key)
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
        session_routing: node.session_routing.clone(),
        scoped_discovery: node.scoped_discovery.clone(),
        publication: node.scoped_publication.clone(),
        org_revocation: node.org_revocation.clone(),
        consumer_grants: node.consumer_grant_audiences.clone(),
        consumer_grant_gate: node.consumer_grant_gate.clone(),
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
        matches!(snapshot.providers(&owner_key).facts, SourceFacts::Served(ref p) if p.is_empty()),
        "an owner scope with no rows is SERVED with exact empty evidence"
    );
    assert!(
        matches!(snapshot.providers(&grant_key).facts, SourceFacts::Unserved),
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
            .pin_if_current(&[], &SourceToken::new(vec![u64::MAX]))
            .is_none(),
        "a token the source has left is refused"
    );
    assert!(
        source.pin_if_current(&[], &token).is_some(),
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
        session_routing: node.session_routing.clone(),
        scoped_discovery: node.scoped_discovery.clone(),
        publication: node.scoped_publication.clone(),
        org_revocation: node.org_revocation.clone(),
        consumer_grants: node.consumer_grant_audiences.clone(),
        consumer_grant_gate: node.consumer_grant_gate.clone(),
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
        source.pin_if_current(&[], &token).is_some(),
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
        source.pin_if_current(&[], &token).is_none(),
        "a snapshot taken under the OLD revocation authority cannot commit"
    );

    // Re-snapshot under the now-current authority: it commits again.
    let fresh = source.snapshot(std::slice::from_ref(&key));
    let fresh_token = fresh.token();
    assert!(source.pin_if_current(&[], &fresh_token).is_some());

    // POISON alone defeats it too: an unusable floor view is unusable authority,
    // so every scope becomes UNSERVED rather than served unfiltered.
    store.mark_poisoned_for_test();
    assert!(
        source.pin_if_current(&[], &fresh_token).is_none(),
        "poisoning the revocation authority invalidates a snapshot taken before it"
    );
    let poisoned = source.snapshot(std::slice::from_ref(&key));
    assert!(
        matches!(poisoned.providers(&key).facts, SourceFacts::Unserved),
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
        authority: ScopedDiscoveryAuthorityStamp::Owner,
        grant_fence: GrantArtifactFence::Publication(0),
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

/// A scratch directory for one witness's revocation sidecar.
///
/// **It deliberately does not delete itself, and the path carries a
/// process-unique sequence number.** Both halves are load-bearing on unix, and
/// neither is tidiness.
///
/// `OrgRevocationStore` keys its PROCESS-GLOBAL core registry by the `.lock`
/// sidecar's FILE IDENTITY — `BackingId::FileId { device, inode }` on unix —
/// so that two path aliases of one sidecar share one live view (AV-9). That is
/// correct, and it is also why a witness must never free a sidecar inode while
/// its core can still be joined: Linux reuses a freed inode immediately, so the
/// NEXT witness's sidecar can land on it, derive the SAME `BackingId`, and
/// `join_or_create_core` will join the finished witness's still-live core by
/// design — inheriting its floors, its poison bit and its
/// generation-exhaustion latch.
///
/// That aliasing is exactly how a witness which never touched poison reads its
/// own freshly-installed facts as cold, and how a witness holding a brand-new
/// store finds its generation already exhausted. It is scheduling-dependent,
/// so it presents as four witnesses failing in varying combinations rather
/// than as one deterministic break — which is what it did on the OLB-2B.3c
/// step-2 candidate, on Linux only, while every one of them passed locally.
///
/// Keeping the sidecar alive for the life of the process makes the aliasing
/// UNREACHABLE rather than unlikely; the sequence number additionally makes
/// two scratch paths in one process impossible. The residue is a few bytes of
/// JSON per witness under the OS temp directory.
struct Scratch(std::path::PathBuf);

impl Scratch {
    fn path(&self) -> &std::path::Path {
        &self.0
    }
    fn new(tag: &str, node: &MeshNode) -> Self {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        // Entity component restored for collision isolation; truncated because
        // the full id pushes the authority `.lock` path past the Windows limit.
        let entity = format!("{}", node.entity_id());
        let path = std::env::temp_dir().join(format!(
            "olb-{tag}-{}-{seq}-{}",
            std::process::id(),
            &entity[..entity.len().min(8)]
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("scratch dir");
        Self(path)
    }
    fn store(&self) -> Arc<crate::adapter::net::behavior::org_revocation::OrgRevocationStore> {
        let store = Arc::new(
            crate::adapter::net::behavior::org_revocation::OrgRevocationStore::init(
                self.0.join("revocation.json"),
                crate::adapter::net::behavior::org_revocation::ProvisioningExpectation::MayBeFresh,
            )
            .expect("init revocation store"),
        );
        // The fixture's assumption, asserted rather than assumed: this store is
        // FRESH. If the sidecar's inode was recycled from a directory some
        // still-live core is registered under, `join_or_create_core` hands back
        // that core instead — already poisoned, already advanced, or already
        // generation-exhausted — and every downstream assertion in the witness
        // becomes a lie about the plane under test.
        //
        // Failing HERE names the cause. Left unasserted, the same aliasing
        // surfaces as a witness reading its own freshly-installed facts as
        // cold, or as `apply` returning `Superseded` with no visible reason,
        // in whichever unrelated test happened to be scheduled next.
        let generation = store.barriered_generation().expect(
            "a freshly created store cannot be generation-exhausted; if it is, this \
                     store JOINED another test's live core through a recycled sidecar inode",
        );
        assert_eq!(
            generation.get(),
            0,
            "a freshly created store must be at generation 0; a nonzero one means this store \
             joined another test's live core through a recycled sidecar inode"
        );
        assert!(
            !store.is_poisoned(),
            "a freshly created store cannot be poisoned; a poisoned one means this store \
             joined another test's live core through a recycled sidecar inode"
        );
        store
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
        session_routing: node.session_routing.clone(),
        scoped_discovery: node.scoped_discovery.clone(),
        publication: node.scoped_publication.clone(),
        org_revocation: node.org_revocation.clone(),
        consumer_grants: node.consumer_grant_audiences.clone(),
        consumer_grant_gate: node.consumer_grant_gate.clone(),
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
        .pin_if_current(&[], &snapshot.token())
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
        source.pin_if_current(&[], &snapshot.token()).is_none(),
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
            authority: ScopedDiscoveryAuthorityStamp::Owner,
            grant_fence: GrantArtifactFence::Publication(0),
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
            authority: ScopedDiscoveryAuthorityStamp::Owner,
            grant_fence: GrantArtifactFence::Publication(0),
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
        session_routing: node.session_routing.clone(),
        scoped_discovery: node.scoped_discovery.clone(),
        publication: node.scoped_publication.clone(),
        org_revocation: node.org_revocation.clone(),
        consumer_grants: node.consumer_grant_audiences.clone(),
        consumer_grant_gate: node.consumer_grant_gate.clone(),
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
        matches!(snapshot.providers(&key).facts, SourceFacts::Unserved),
        "an exhausted authority serves nothing"
    );
    assert!(
        source.pin_if_current(&[], &snapshot.token()).is_none(),
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
        session_routing: node.session_routing.clone(),
        scoped_discovery: node.scoped_discovery.clone(),
        publication: node.scoped_publication.clone(),
        org_revocation: node.org_revocation.clone(),
        consumer_grants: node.consumer_grant_audiences.clone(),
        consumer_grant_gate: node.consumer_grant_gate.clone(),
        authority: node.routing_authority.clone(),
        settle_gap_hook: parking_lot::Mutex::new(None),
        unserved_scope: node.routing_unserved_scope.clone(),
    };
    let key = slot(14, "nrpc:gen-exhausted");

    let healthy = source.snapshot(std::slice::from_ref(&key));
    assert!(
        matches!(healthy.providers(&key).facts, SourceFacts::Served(_)),
        "precondition: a usable authority serves the scope"
    );
    let healthy_token = healthy.token();
    assert!(source.pin_if_current(&[], &healthy_token).is_some());

    store.saturate_generation_for_test();
    store.republish_for_test();
    assert_eq!(
        store.barriered_generation().err(),
        Some(crate::adapter::net::behavior::org_revocation::GenerationExhausted)
    );

    let snapshot = source.snapshot(std::slice::from_ref(&key));
    assert!(
        matches!(snapshot.providers(&key).facts, SourceFacts::Unserved),
        "an exhausted publication generation serves NOTHING"
    );
    assert!(
        source.pin_if_current(&[], &healthy_token).is_none(),
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
    let warm = registry.base_facts_unvalidated(&key).expect("reconciled");
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
        registry.base_facts_unvalidated(&key).is_none(),
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
    assert!(registry.base_facts_unvalidated(&late).is_none());
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
        session_routing: node.session_routing.clone(),
        scoped_discovery: node.scoped_discovery.clone(),
        publication: node.scoped_publication.clone(),
        org_revocation: node.org_revocation.clone(),
        consumer_grants: node.consumer_grant_audiences.clone(),
        consumer_grant_gate: node.consumer_grant_gate.clone(),
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
                session_routing: node.session_routing.clone(),
                scoped_discovery: node.scoped_discovery.clone(),
                publication: node.scoped_publication.clone(),
                org_revocation: node.org_revocation.clone(),
                consumer_grants: node.consumer_grant_audiences.clone(),
                consumer_grant_gate: node.consumer_grant_gate.clone(),
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

/// (11g) The scoped CHANGE GENERATION latches terminally instead of wrapping
/// (review-pass-3 §12).
///
/// `SourceEpoch::generation` is the routing plane's coherence token, and it is
/// the only counter in the identity set a remote peer influences at all — every
/// accepted scoped ingest advances one. It used to `wrapping_add`, which promised
/// the single property the whole stamp discipline rests on (two different states
/// never share an identity) and did not deliver it.
#[tokio::test]
async fn an_exhausted_scoped_generation_fences_the_routing_source() {
    use crate::adapter::net::behavior::org_routing_registry::SourceLiveness;

    let node = node().await;
    let source = ScopedSlotSource {
        session_routing: node.session_routing.clone(),
        scoped_discovery: node.scoped_discovery.clone(),
        publication: node.scoped_publication.clone(),
        org_revocation: node.org_revocation.clone(),
        consumer_grants: node.consumer_grant_audiences.clone(),
        consumer_grant_gate: node.consumer_grant_gate.clone(),
        authority: node.routing_authority.clone(),
        settle_gap_hook: parking_lot::Mutex::new(None),
        unserved_scope: node.routing_unserved_scope.clone(),
    };
    let key = slot(31, "nrpc:generation-ceiling");

    let healthy = source.snapshot(std::slice::from_ref(&key));
    let healthy_token = healthy.token();
    assert!(
        matches!(healthy.providers(&key).facts, SourceFacts::Served(_)),
        "precondition: a usable generation serves the scope"
    );
    drop(healthy);
    assert_eq!(source.liveness(), SourceLiveness::Live);

    // Park one advance below the ceiling and drive a real mutation over it.
    node.scoped_publication
        .gated_commit(&node.scoped_discovery, |state| {
            state.park_revisions_at_ceiling_for_test();
            state.advance_query_visible_generation_for_test(CapabilityAuthorityId::for_tag(
                "nrpc:generation-ceiling",
            ));
        });

    assert!(
        node.scoped_discovery.lock().generations_exhausted(),
        "the advance past the ceiling LATCHES rather than wrapping to 0"
    );
    assert_eq!(
        node.scoped_discovery.lock().revision(),
        u64::MAX,
        "and parks on the terminal sentinel"
    );
    assert_eq!(
        source.liveness(),
        SourceLiveness::Terminal,
        "a generation that can no longer distinguish states is terminal authority"
    );

    let fenced = source.snapshot(std::slice::from_ref(&key));
    assert!(
        matches!(fenced.providers(&key).facts, SourceFacts::Unserved),
        "an exhausted change generation serves NOTHING"
    );
    let fenced_token = fenced.token();
    drop(fenced);
    assert!(
        source.pin_if_current(&[], &healthy_token).is_none(),
        "a token minted under the usable generation no longer commits"
    );
    assert!(
        source.pin_if_current(&[], &fenced_token).is_none(),
        "and neither does one minted UNDER the exhaustion — two exhausted samples \
         must never compare equal-and-current"
    );
}

/// (11h) The three terminal exhaustion latches reach ONE operator-facing surface
/// (review-pass-3 §13(a)).
///
/// `generation_exhausted_for_metrics` was written for exactly this and wired to
/// nothing — its only reference was its own unit test — so production
/// observability for a terminal latch was a single `tracing::error!` at the
/// moment it happened, and a node inspected afterwards showed nothing at all.
#[tokio::test]
async fn terminal_exhaustion_is_visible_on_a_metrics_surface() {
    let node = node().await;
    let scratch = Scratch::new("exhaustion-surface", &node);
    let store = scratch.store();
    node.install_org_revocation_store(store.clone())
        .expect("install");
    assert_eq!(
        node.org_authority_exhaustion(),
        (false, false, false),
        "precondition: nothing is exhausted"
    );

    store.saturate_generation_for_test();
    store.republish_for_test();
    assert!(
        node.org_authority_exhaustion().0,
        "the revocation publication generation latch is readable after the fact"
    );

    node.routing_authority
        .exhausted
        .store(true, Ordering::Release);
    node.scoped_publication
        .gated_commit(&node.scoped_discovery, |state| {
            state.park_revisions_at_ceiling_for_test();
            state.advance_query_visible_generation_for_test(CapabilityAuthorityId::for_tag(
                "nrpc:exhaustion-surface",
            ));
        });
    assert_eq!(
        node.org_authority_exhaustion(),
        (true, true, true),
        "and each plane reports its OWN latch — they are separate identity spaces"
    );
}

/// (11i) `start` publishes EVERY background task handle before it returns
/// (review-pass-3 §16).
///
/// The handles used to be pushed from inside a `tokio::spawn`, which is the exact
/// pattern the routing supervisor's own comment names as unsafe for
/// deterministic teardown — and which was closed for the routing task alone. A
/// fast shutdown could observe an empty vector and return while the exact-expiry
/// timer was still free to run one more `gated_commit` sweep and watch
/// publication. Benign for state, since everything is `Arc`-held and
/// self-terminating and the shutdown wake is armed before the flag check, but it
/// made every shutdown-ordering witness racy.
///
/// Asserted with NO intervening await, which is what makes it a witness: the
/// spawned-push version passes any test that yields first.
#[tokio::test]
async fn start_publishes_every_background_handle_before_returning() {
    let node = node().await;
    assert!(
        node.tasks.lock().is_empty(),
        "precondition: an unstarted node owns no background tasks"
    );

    node.start();
    let published = node.tasks.lock().len();
    assert!(
        published >= 10,
        "every background handle must be joinable the instant `start` returns, \
         with no scheduler round-trip in between (published {published})"
    );

    let _ = node.shutdown().await;
    assert!(
        node.tasks.lock().is_empty(),
        "and shutdown consumed exactly the handles start published"
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
            authority: ScopedDiscoveryAuthorityStamp::Owner,
            grant_fence: GrantArtifactFence::Publication(0),
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
        .base_facts_unvalidated(&key)
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

/// (11e) The INCARNATION FENCE at the node read seam (review-pass-3 §5).
///
/// This is the headline "incarnation fencing" property and nothing witnessed it:
/// replacing `.allows(facts.actor_incarnation).then_some(facts)` with
/// `Some(facts)` left every test in `org_routing.rs`, `org_routing_registry.rs`
/// and this module green. It matters because the registry genuinely still HOLDS
/// a dead incarnation's facts — `deactivate_incarnation` clears `live_actor`
/// only, and invalidation waits for a successor — so during restart backoff and
/// both terminal fenced states this check is the only thing between those
/// artifacts and a caller.
///
/// Everything else about the facts is deliberately CURRENT here: same authority
/// epoch, same floors, not `Unserved`, not expired. The health fence is the sole
/// variable.
#[tokio::test]
async fn the_read_seam_fences_a_dead_incarnations_facts() {
    use crate::adapter::net::behavior::org_routing::RoutingHealth;
    use crate::adapter::net::behavior::org_routing_registry::{
        SlotBaseFacts, SourceEpoch, SourceFacts,
    };

    let node = node().await;
    let key = slot(29, "nrpc:incarnation-fence");
    let facts = Arc::new(SlotBaseFacts {
        authority: ScopedDiscoveryAuthorityStamp::Owner,
        grant_fence: GrantArtifactFence::Publication(0),
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
    });
    node.routing_registry
        .install_facts_for_test(key.clone(), facts.clone());

    node.routing_health
        .store(Arc::new(RoutingHealth::Healthy { incarnation: 1 }));
    assert!(
        node.org_routing_base_facts(&key).is_some(),
        "precondition: under its OWN live incarnation the artifact serves"
    );

    for (health, why) in [
        (
            RoutingHealth::Fenced,
            "a fenced plane must serve nothing, however current the artifact is",
        ),
        (
            RoutingHealth::Rebuilding { incarnation: 1 },
            "nor may a rebuilding one serve what its own incarnation already built",
        ),
        (
            RoutingHealth::Healthy { incarnation: 2 },
            "and a SUCCESSOR incarnation does not inherit its predecessor's artifacts",
        ),
    ] {
        node.routing_health.store(Arc::new(health));
        assert!(node.org_routing_base_facts(&key).is_none(), "{why}");
        assert!(
            node.routing_registry.base_facts_unvalidated(&key).is_some(),
            "…and the raw accessor still returns it, which is why the SEAM has to fence"
        );
    }
}

/// (11f) The read seam's authority sample is COHERENT, not merely well-ordered
/// (review-pass-3 §6).
///
/// The `None -> store` install is the case that bites, because the two sides of
/// the sample are indistinguishable across it: with no store the view is
/// `(poisoned = false, floor_generation = 0)`, and a freshly installed store
/// publishes at generation 0 too — the NORMAL first-install values, not a
/// coincidence. So a sample that loaded the epoch BEFORE the advance and the
/// floors AFTER the publish would find facts stamped `(R, 0, false)` matching
/// live store B in every compared field, and serve A-era facts as
/// B-authoritative.
///
/// Pass 2 verified the WRITE side and concluded `(B, R)` is unobservable, which
/// is true — but only because the read side happened to sample floors first. No
/// comment marked that order as load-bearing and no test could catch it being
/// swapped. The seqlock re-check removes the ordering from the argument: the
/// install lands inside the sample here, and it is the RE-CHECK that catches it.
#[tokio::test]
async fn the_read_seams_authority_sample_cannot_straddle_a_store_install() {
    use crate::adapter::net::behavior::org_routing_registry::{
        SlotBaseFacts, SourceEpoch, SourceFacts,
    };

    let node = node().await;
    node.routing_health.store(Arc::new(
        crate::adapter::net::behavior::org_routing::RoutingHealth::Healthy { incarnation: 1 },
    ));
    // No store installed: the live view is (false, 0) — the value a fresh store
    // also publishes.
    assert!(node.org_revocation.load().is_none());
    let key = slot(30, "nrpc:coherent-sample");
    let facts = Arc::new(SlotBaseFacts {
        authority: ScopedDiscoveryAuthorityStamp::Owner,
        grant_fence: GrantArtifactFence::Publication(0),
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
    });
    node.routing_registry
        .install_facts_for_test(key.clone(), facts.clone());
    assert!(
        node.org_routing_base_facts(&key).is_some(),
        "precondition: warm under the pre-install authority"
    );

    // The production movement, landing INSIDE the sample: epoch first, store
    // second, both under the authority gate.
    let scratch = Scratch::new("coherent-sample", &node);
    let landed = Arc::new(AtomicBool::new(false));
    {
        let inner = node.clone();
        let store = scratch.store();
        let landed = landed.clone();
        *node.routing_sample_gap_hook.lock() = Some(Arc::new(move || {
            inner
                .install_org_revocation_store(store.clone())
                .expect("install");
            landed.store(true, Ordering::Release);
        }));
    }

    let served = node.org_routing_base_facts(&key);
    assert!(landed.load(Ordering::Acquire), "the install did land");
    assert!(
        served.is_none(),
        "a sample straddling the install must never serve A-era facts as B-authoritative"
    );
    assert_ne!(
        facts.epoch.authority,
        node.routing_authority.epoch(),
        "and the movement really did change the identity the facts were stamped with"
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
            authority: ScopedDiscoveryAuthorityStamp::Owner,
            grant_fence: GrantArtifactFence::Publication(0),
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
            authority: ScopedDiscoveryAuthorityStamp::Owner,
            grant_fence: GrantArtifactFence::Publication(0),
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
        node.routing_registry.base_facts_unvalidated(&key).is_none(),
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
            session_routing: node.session_routing.clone(),
            scoped_discovery: node.scoped_discovery.clone(),
            publication: node.scoped_publication.clone(),
            org_revocation: node.org_revocation.clone(),
            consumer_grants: node.consumer_grant_audiences.clone(),
            consumer_grant_gate: node.consumer_grant_gate.clone(),
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
            registry
                .base_facts_unvalidated(key)
                .expect("built")
                .epoch
                .authority,
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
        session_routing: node.session_routing.clone(),
        scoped_discovery: node.scoped_discovery.clone(),
        publication: node.scoped_publication.clone(),
        org_revocation: node.org_revocation.clone(),
        consumer_grants: node.consumer_grant_audiences.clone(),
        consumer_grant_gate: node.consumer_grant_gate.clone(),
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
                    session_routing: node.session_routing.clone(),
                    scoped_discovery: node.scoped_discovery.clone(),
                    publication: node.scoped_publication.clone(),
                    org_revocation: node.org_revocation.clone(),
                    consumer_grants: node.consumer_grant_audiences.clone(),
                    consumer_grant_gate: node.consumer_grant_gate.clone(),
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
        source.pin_if_current(&[], &token).is_some(),
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
            authority: ScopedDiscoveryAuthorityStamp::Owner,
            grant_fence: GrantArtifactFence::Publication(0),
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
    let metrics: Arc<crate::adapter::net::behavior::org_routing_registry::RegistryMetrics> =
        Arc::default();
    let registry = NodeOrgRoutingRegistry::new(
        Arc::new(PausingSource {
            inner: ScopedSlotSource {
                session_routing: node.session_routing.clone(),
                scoped_discovery: node.scoped_discovery.clone(),
                publication: node.scoped_publication.clone(),
                org_revocation: node.org_revocation.clone(),
                consumer_grants: node.consumer_grant_audiences.clone(),
                consumer_grant_gate: node.consumer_grant_gate.clone(),
                authority: node.routing_authority.clone(),
                settle_gap_hook: parking_lot::Mutex::new(None),
                unserved_scope: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            },
            during_build: during_build.clone(),
            after_pin: after_pin.clone(),
            before_pin: Arc::default(),
        }),
        work.clone(),
        metrics.clone(),
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
    // review-pass-3 §8: counted as AUTHORITY movement under the pin, not as
    // actor-lifecycle churn. The actor was live throughout — attributing this to
    // `stale_actor_rejections` steered operators at supervisor restarts during
    // what is really revocation-publication pressure.
    assert_eq!(
        metrics.settlements_refused(),
        1,
        "the refusal is counted, and counted as a settlement refusal"
    );
    assert_eq!(
        metrics.stale_actor_rejections(),
        0,
        "and NOT as actor-lifecycle churn"
    );

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
            authority: ScopedDiscoveryAuthorityStamp::Owner,
            grant_fence: GrantArtifactFence::Publication(0),
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
        node.routing_registry
            .base_facts_unvalidated(&stale_key)
            .is_none(),
        "the retired-authority facts are invalidated"
    );
    let survivor = node
        .routing_registry
        .base_facts_unvalidated(&fresh_key)
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
///
/// REPAIRED after an independent RED pass (Kyra, 2026-07-27, at `80bb06b5a`)
/// showed this witness green under a mutation that released the publication pin
/// before the settlement — i.e. it did not prove the property it claims. The
/// acknowledgement fired immediately BEFORE `live.write()`, so with the pin
/// wrongly released the publisher could signal, acquire the lock, and have this
/// observer read `published` before the publisher stored it. Elapsed time was
/// already ruled out as evidence; "about to attempt" turns out to be no better,
/// for the same reason.
///
/// The acknowledgement now fires only after a `try_write` has ACTUALLY FAILED,
/// which is the one thing scheduling cannot fake: it means a holder is provably
/// there at that instant, so the publisher is definitively blocked when the
/// negative assertion below runs. Under the same mutation `try_write` succeeds,
/// no acknowledgement is ever sent, and this witness fails at its wait.
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
        session_routing: node.session_routing.clone(),
        scoped_discovery: node.scoped_discovery.clone(),
        publication: node.scoped_publication.clone(),
        org_revocation: node.org_revocation.clone(),
        consumer_grants: node.consumer_grant_audiences.clone(),
        consumer_grant_gate: node.consumer_grant_gate.clone(),
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
    store.arm_publish_contended_hook(Arc::new(move || {
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
            // Wait for PROVEN contention: the publisher's `try_write` failed, so
            // the settlement pin is demonstrably holding `live` right now.
            // Neither an interval nor an "about to attempt" signal is evidence —
            // both also occur when the barrier is absent.
            reached.lock().recv_timeout(Duration::from_secs(10)).expect(
                "the publisher's try_write must FAIL, proving the settlement pin \
                     holds the publication barrier; no signal means the barrier was \
                     released before the settlement",
            );
            // Sound only because of the line above: a failed `try_write` means the
            // publisher is blocked inside `publish`, so it cannot yet have stored
            // this flag.
            assert!(
                !published.load(Ordering::Acquire),
                "a floor publication landed between the validation and the \
                 settlement — they are separately interleavable"
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
        session_routing: node.session_routing.clone(),
        scoped_discovery: node.scoped_discovery.clone(),
        publication: node.scoped_publication.clone(),
        org_revocation: node.org_revocation.clone(),
        consumer_grants: node.consumer_grant_audiences.clone(),
        consumer_grant_gate: node.consumer_grant_gate.clone(),
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
                session_routing: node.session_routing.clone(),
                scoped_discovery: node.scoped_discovery.clone(),
                publication: node.scoped_publication.clone(),
                org_revocation: node.org_revocation.clone(),
                consumer_grants: node.consumer_grant_audiences.clone(),
                consumer_grant_gate: node.consumer_grant_gate.clone(),
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
        .base_facts_unvalidated(&key)
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
    let stranded = registry.base_facts_unvalidated(&key).expect("reconciled");
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
        registry.base_facts_unvalidated(&key).is_none(),
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
    let successor = registry
        .base_facts_unvalidated(&key)
        .expect("reconciled again");
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
    let warm = registry.base_facts_unvalidated(&key).expect("reconciled");
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
        registry.base_facts_unvalidated(&key).is_none(),
        "the pre-poison reconstruction was RETIRED, not left reconciled"
    );
    assert_eq!(registry.pending_slots(), 1, "re-queuing the exact slot");

    // And the successor converges to Option A's steady state.
    assert!(matches!(
        registry.apply(1, request),
        ApplyOutcome::Current { .. }
    ));
    let successor = registry
        .base_facts_unvalidated(&key)
        .expect("reconciled again");
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
            authority: ScopedDiscoveryAuthorityStamp::Owner,
            grant_fence: GrantArtifactFence::Publication(0),
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
        node.routing_registry
            .base_facts_unvalidated(&key_a)
            .is_none(),
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
        node.routing_registry
            .base_facts_unvalidated(&key_b)
            .is_none(),
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
        grant_publication: 0,
        grant_publications_spent: false,
        rows: [(
            key.clone(),
            (
                vec![row(3, 300), row(9, 900), row(5, 500)],
                ScopedDiscoveryAuthorityStamp::Owner,
                u64::MAX,
            ),
        )]
        .into_iter()
        .collect(),
    };

    let SourceFacts::Served(providers) = snapshot.providers(&key).facts else {
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

// ---- OLB-2C: the org authority swap is published INSIDE the routing epoch ----

/// A distinct `NodeAuthority` `Arc` for this node under `org`, via the real
/// adoption ceremony into a fresh tempdir.
fn adopt_authority(
    node: &MeshNode,
    org: &crate::adapter::net::behavior::org::OrgKeypair,
    tag: &str,
) -> Arc<crate::adapter::net::behavior::org_authority::NodeAuthority> {
    use crate::adapter::net::behavior::org::OrgMembershipCert;
    use crate::adapter::net::behavior::org_authority::NodeAuthority;
    let entity = node.entity_id().clone();
    let cert = OrgMembershipCert::try_issue(org, entity.clone(), 1, 3600).expect("issue cert");
    // Deliberately SHORT: the ceremony writes `<dir>/authority.lock`, and a
    // full entity id in the path overruns the Windows path limit — the failure
    // surfaces as a bare "cannot find the path specified", not as a length
    // error. A process-scoped sequence keeps it unique without the length.
    static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("net-olb2c-{tag}-{}-{seq}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    Arc::new(NodeAuthority::adopt(&dir, cert, &entity, 0, None).expect("adopt authority"))
}

/// OLB-2C. The load-bearing one: at the instant the routing epoch's publication
/// completes — still under the authority gate — the node authority the
/// transaction installs is ALREADY visible.
///
/// Before OLB-2C, `install_node_authority_inner` published `node_authority`
/// after `install_org_revocation_store_locked` had returned, which means after
/// the authority gate was released. The epoch therefore advanced, and the store
/// became query-visible, while the authority half of the very same transaction
/// was still the old object. That is the same defect class E3c closed for the
/// store itself, and it is invisible from outside the gate because the window is
/// a few instructions wide — hence the observation runs INSIDE it.
#[tokio::test]
async fn an_authority_install_publishes_the_authority_under_its_own_epoch() {
    let node = node().await;
    let org = crate::adapter::net::behavior::org::OrgKeypair::from_bytes([0x2cu8; 32]);

    // First install establishes a baseline authority so the transition under
    // test is a genuine A -> B replacement rather than None -> A.
    node.install_node_authority(adopt_authority(&node, &org, "a"))
        .expect("install A");
    let a = node.node_authority().expect("A is installed");

    let next = adopt_authority(&node, &org, "b");

    // The OTHER half of the ordering: at the instant AFTER the epoch advance and
    // BEFORE the publication, nothing this transaction publishes may be visible
    // yet. Without it, a mutation that publishes the authority BEFORE the advance
    // leaves every other assertion satisfied — the post-publication observer sees
    // the new authority either way. Publishing early is the hazard
    // `move_routing_authority` documents: a reader observes the new object under
    // the OLD epoch identity and serves it as old-authoritative.
    let early = Arc::new(AtomicBool::new(false));
    {
        let early = early.clone();
        let premature = next.clone();
        let weak = Arc::downgrade(&node);
        *node.routing_authority.pre_publish_hook.lock() = Some(Arc::new(move |_epoch| {
            if let Some(node) = weak.upgrade() {
                if node
                    .node_authority()
                    .is_some_and(|live| Arc::ptr_eq(&live, &premature))
                {
                    early.store(true, Ordering::Release);
                }
            }
        }));
    }

    // Sample the authority the node exposes at the publication instant.
    let observed: Arc<parking_lot::Mutex<Vec<(u64, bool)>>> =
        Arc::new(parking_lot::Mutex::new(Vec::new()));
    {
        let sink = observed.clone();
        let weak = Arc::downgrade(&node);
        // The expected authority is captured as the `Arc` itself, not as a raw
        // pointer: a `*const` is not `Sync`, and `Arc::ptr_eq` is the identity
        // comparison the install path itself uses.
        let expected = next.clone();
        *node.routing_authority.post_publish_hook.lock() = Some(Arc::new(move |epoch| {
            let Some(node) = weak.upgrade() else {
                return;
            };
            let live_is_next = node
                .node_authority()
                .is_some_and(|live| Arc::ptr_eq(&live, &expected));
            sink.lock().push((epoch, live_is_next));
        }));
    }

    node.install_node_authority(next.clone())
        .expect("install B");
    *node.routing_authority.post_publish_hook.lock() = None;
    *node.routing_authority.pre_publish_hook.lock() = None;

    assert!(
        !early.load(Ordering::Acquire),
        "the replacement authority was already visible BEFORE the epoch advance          — a reader in that window observes it under the OLD epoch identity"
    );
    let observed = observed.lock().clone();
    assert_eq!(
        observed.len(),
        1,
        "the complete authority+store transaction must publish under exactly ONE \
         epoch advance, not one per half"
    );
    assert!(
        observed[0].1,
        "at the publication instant the new authority must already be live; \
         observing the OLD one here means the authority swap escaped the epoch \
         that names it"
    );
    assert!(
        !Arc::ptr_eq(&a, &node.node_authority().expect("B is installed")),
        "the transition under test must actually have replaced the authority"
    );
}

/// OLB-2C, the other half: the transaction is ONE authority movement, so it
/// advances the routing epoch exactly once and retires facts stamped before it.
///
/// A cached routing artifact built under authority A must not survive into B.
/// Routing does not read the authority object today — it filters by revocation
/// floors — so this asserts the epoch/retirement protocol rather than a change
/// in which providers are served, which is exactly the property 2B.3's warmed
/// proof path will depend on.
#[tokio::test]
async fn an_authority_install_advances_the_routing_epoch_exactly_once() {
    let node = node().await;
    let org = crate::adapter::net::behavior::org::OrgKeypair::from_bytes([0x2du8; 32]);

    node.install_node_authority(adopt_authority(&node, &org, "c"))
        .expect("install A");
    let before = node.routing_authority.epoch();

    let advances: Arc<std::sync::atomic::AtomicU64> =
        Arc::new(std::sync::atomic::AtomicU64::new(0));
    {
        let counter = advances.clone();
        *node.routing_authority.post_publish_hook.lock() = Some(Arc::new(move |_| {
            counter.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        }));
    }
    node.install_node_authority(adopt_authority(&node, &org, "d"))
        .expect("install B");
    *node.routing_authority.post_publish_hook.lock() = None;

    assert_eq!(
        advances.load(std::sync::atomic::Ordering::Acquire),
        1,
        "an authority+store install is ONE authority movement; a second advance \
         means the two halves were published as separate ordered units"
    );
    let after = node.routing_authority.epoch();
    assert!(
        after > before,
        "the routing epoch must move: {before} -> {after}"
    );
}

/// OLB-2C. A REFUSED install publishes neither half.
///
/// The one-owner preflight rejects a foreign org before any publication, so the
/// authority must not move and the epoch must not advance — a failed
/// transaction that still bumped the epoch would retire every retained routing
/// fact for nothing.
#[tokio::test]
async fn a_refused_authority_install_publishes_neither_half() {
    let node = node().await;
    let org = crate::adapter::net::behavior::org::OrgKeypair::from_bytes([0x2eu8; 32]);
    node.install_node_authority(adopt_authority(&node, &org, "e"))
        .expect("install A");
    let installed = node.node_authority().expect("A is installed");
    let before = node.routing_authority.epoch();

    let foreign = crate::adapter::net::behavior::org::OrgKeypair::from_bytes([0x9fu8; 32]);
    node.install_node_authority(adopt_authority(&node, &foreign, "f"))
        .expect_err("a foreign owner org must be refused at the one-owner preflight");

    assert!(
        Arc::ptr_eq(
            &installed,
            &node.node_authority().expect("A is still installed")
        ),
        "a refused install must not publish its authority"
    );
    assert_eq!(
        node.routing_authority.epoch(),
        before,
        "a refused install must not advance the routing epoch"
    );
}

/// OLB-2C, the branch the changed-store witness does NOT reach: an authority
/// rotation over the SAME revocation store `Arc`.
///
/// A **direct structural branch witness**, and labelled as such deliberately.
/// Today's production constructor gives every `NodeAuthority` its own store, so
/// `authority_changed` and `store_changed` move together and this branch is
/// unreachable end-to-end — it exists fail-closed. An independent RED pass
/// (Kyra, 2026-07-27) showed the consequence: mutating this branch to publish
/// the authority OUTSIDE `move_routing_authority` left
/// `an_authority_install_publishes_the_authority_under_its_own_epoch` green,
/// because that witness only ever exercises the changed-store branch. So the
/// branch was code with no evidence behind it.
///
/// This drives `install_org_revocation_store_locked` directly with the exact
/// installed store `Arc`, which is the only way to reach the branch at all.
/// **When a production workflow can rotate authority over one store, that
/// workflow owes its own end-to-end witness — this one does not stand in for it.**
#[tokio::test]
async fn an_authority_rotation_over_the_same_store_still_publishes_inside_the_epoch() {
    let node = node().await;
    let org = crate::adapter::net::behavior::org::OrgKeypair::from_bytes([0x2fu8; 32]);

    node.install_node_authority(adopt_authority(&node, &org, "g"))
        .expect("install A");
    // The EXACT installed `Arc` — passing it back is what makes the helper take
    // its `Arc::ptr_eq` no-visible-change path.
    let installed_store = node
        .org_revocation
        .load_full()
        .expect("A's store is installed");
    let replacement = adopt_authority(&node, &org, "h");

    let advances = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let visible_in_callback = Arc::new(AtomicBool::new(false));
    // The OTHER half of the ordering: at the instant AFTER the epoch advance and
    // BEFORE the publication, nothing this transaction publishes may be visible
    // yet. Without it, a mutation that publishes the authority BEFORE the advance
    // leaves every other assertion satisfied — the post-publication observer sees
    // the new authority either way. Publishing early is the hazard
    // `move_routing_authority` documents: a reader observes the new object under
    // the OLD epoch identity and serves it as old-authoritative.
    let early = Arc::new(AtomicBool::new(false));
    {
        let early = early.clone();
        let premature = replacement.clone();
        let weak = Arc::downgrade(&node);
        *node.routing_authority.pre_publish_hook.lock() = Some(Arc::new(move |_epoch| {
            if let Some(node) = weak.upgrade() {
                if node
                    .node_authority()
                    .is_some_and(|live| Arc::ptr_eq(&live, &premature))
                {
                    early.store(true, Ordering::Release);
                }
            }
        }));
    }
    {
        let advances = advances.clone();
        let visible = visible_in_callback.clone();
        let expected = replacement.clone();
        let weak = Arc::downgrade(&node);
        *node.routing_authority.post_publish_hook.lock() = Some(Arc::new(move |_epoch| {
            advances.fetch_add(1, Ordering::AcqRel);
            if let Some(node) = weak.upgrade() {
                if node
                    .node_authority()
                    .is_some_and(|live| Arc::ptr_eq(&live, &expected))
                {
                    visible.store(true, Ordering::Release);
                }
            }
        }));
    }

    let before = node.routing_authority.epoch();
    let publish = {
        let replacement = replacement.clone();
        let slot = &node.node_authority;
        move || slot.store(Some(replacement.clone()))
    };
    // Under `org_install`, as every production caller of the `_locked` helper is.
    let store_changed = {
        let _install = node.org_install.lock();
        node.install_org_revocation_store_locked(
            installed_store.clone(),
            false,
            None,
            Some(&publish as &(dyn Fn() + Sync)),
        )
        .expect("re-installing the exact same store is accepted")
    };
    *node.routing_authority.post_publish_hook.lock() = None;
    *node.routing_authority.pre_publish_hook.lock() = None;

    assert!(
        !early.load(Ordering::Acquire),
        "the replacement authority was already visible BEFORE the epoch advance          — a reader in that window observes it under the OLD epoch identity"
    );
    assert!(
        !store_changed,
        "precondition: the same store `Arc` is not a visible store change — \
         without this the test would silently be exercising the OTHER branch"
    );
    // Cannot fail on any reachable path — the same `Arc` is both the input and
    // what the other branch would store — so it is kept as a PRECONDITION marker
    // for the reader, not counted as evidence.
    assert!(
        Arc::ptr_eq(
            &installed_store,
            &node
                .org_revocation
                .load_full()
                .expect("store still installed")
        ),
        "precondition marker: the store is pointer-identical on this branch"
    );
    assert_eq!(
        advances.load(Ordering::Acquire),
        1,
        "an authority-only rotation must still be ONE routing epoch transaction: \
         0 means the authority was published outside the epoch entirely"
    );
    assert!(
        visible_in_callback.load(Ordering::Acquire),
        "at the publication instant the replacement authority must already be \
         live; observing the old one means the swap escaped the epoch naming it"
    );
    assert!(
        node.routing_authority.epoch() > before,
        "the routing epoch must advance even though no store changed"
    );
    assert!(
        Arc::ptr_eq(&replacement, &node.node_authority().expect("authority")),
        "the rotation must actually have landed"
    );
}

// ---- OLB-2B.3a: the lock-free per-slot publication cell ------------------

/// OLB-2B.3a. A handle's lock-free read observes exactly what the REAL actor
/// installation publishes — the same artifact `Arc` the locked registry seam
/// returns.
///
/// Drives `DirtyApply::apply` so the phase-5 installation runs for real. An
/// earlier version of this witness used `install_facts_for_test`, which stores
/// into the cell that already exists, and was therefore blind to the defect that
/// matters most here: a production install that REPLACES the cell
/// (`slot.facts = Arc::new(ArcSwapOption::from(..))` instead of
/// `slot.facts.store(..)`) would leave every live `DemandHandle` holding the old
/// cell — silently cold forever — while the locked seam happily reported the new
/// artifact. The two seams diverging is the whole hazard of publishing through a
/// cloned cell, so the witness must compare them after a real install
/// (Kyra, 2B.3a review).
#[tokio::test]
async fn a_handles_lockfree_read_observes_the_registrys_published_artifact() {
    use crate::adapter::net::behavior::org_routing::{
        ApplyOutcome, ApplyRequest, DirtyApply, RegistryWork,
    };
    use crate::adapter::net::behavior::org_routing_registry::NodeOrgRoutingRegistry;
    use crate::adapter::net::behavior::org_scoped_store::{
        DirtyCapabilities, PrivateDiscoveryChangeBatch,
    };

    let node = node().await;
    let scratch = Scratch::new("lockfree-cell", &node);
    node.install_org_revocation_store(scratch.store())
        .expect("install");

    let source = ScopedSlotSource {
        session_routing: node.session_routing.clone(),
        scoped_discovery: node.scoped_discovery.clone(),
        publication: node.scoped_publication.clone(),
        org_revocation: node.org_revocation.clone(),
        consumer_grants: node.consumer_grant_audiences.clone(),
        consumer_grant_gate: node.consumer_grant_gate.clone(),
        authority: node.routing_authority.clone(),
        settle_gap_hook: parking_lot::Mutex::new(None),
        unserved_scope: Arc::new(std::sync::atomic::AtomicU64::new(0)),
    };
    let work: Arc<RegistryWork> = Arc::default();
    let registry = NodeOrgRoutingRegistry::new(Arc::new(source), work, Arc::default());
    registry.activate_incarnation(1);

    let family = registry.new_family().expect("family");
    let key = slot(31, "nrpc:lockfree");
    let held = family.demand(key.clone()).expect("demand");

    assert!(
        held.base_facts_unvalidated().is_none(),
        "cold before the actor has installed anything"
    );

    // The REAL installation path: first demand queued the slot, so one apply
    // pass reconstructs and installs it through phase 5 under the commit pin.
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
        "the install pass must settle: {outcome:?}"
    );

    let via_registry = registry
        .base_facts_unvalidated(&key)
        .expect("the actor installed an artifact");
    let via_handle = held
        .base_facts_unvalidated()
        .expect("the handle must observe the actor's install; `None` here means                  the handle holds a cell the production install no longer writes to");
    assert!(
        Arc::ptr_eq(&via_handle, &via_registry),
        "the lock-free and locked seams must return the SAME artifact — divergence          means the install replaced the cell instead of storing into it, leaving          every live handle permanently and silently cold"
    );

    // Invalidation must reach the handle through the same cell.
    registry.invalidate_if_stale(&key, &via_registry);
    assert!(
        held.base_facts_unvalidated().is_none(),
        "an invalidation must be visible through the handle, or the warmed path          would keep serving retired facts"
    );
    assert!(
        registry.base_facts_unvalidated(&key).is_none(),
        "and both seams must agree about the invalidation too"
    );
}

// ---- review 2026-07-29 §2: the Grant stamp is keyed by the WHOLE scope ----

/// A node adopted into one org with exactly one consumer Grant installed.
/// Returns the node, the grant id, and the audience handle the INSTALLED record
/// carries — read off the record rather than the secret, because the installed
/// record is what the source compares against.
async fn consumer_with_installed_grant(tag: &str) -> (Arc<MeshNode>, [u8; 32], [u8; 32]) {
    use crate::adapter::net::behavior::org::OrgKeypair;
    use crate::adapter::net::behavior::org_grant::{
        GrantRights, GrantTargetScope, OrgCapabilityGrant,
    };
    let node = node().await;
    let org = OrgKeypair::from_bytes([0xa1u8; 32]);
    let issuer = OrgKeypair::from_bytes([0xa2u8; 32]);
    node.install_node_authority(adopt_authority(&node, &org, tag))
        .expect("install authority");

    let provider = EntityKeypair::generate();
    let (grant, secret) = OrgCapabilityGrant::try_issue(
        &issuer,
        org.org_id(),
        CapabilityAuthorityId::for_tag("nrpc:pair-key"),
        GrantRights::DISCOVER,
        GrantTargetScope::ExactNode(provider.entity_id().clone()),
        3600,
    )
    .expect("issue grant");
    let secret = secret.expect("a DISCOVER grant mints a secret");
    let grant_id = grant.grant_id;
    node.install_consumer_grant_audience(grant, secret)
        .expect("install consumer grant");

    let handle = *node
        .consumer_grant_audiences
        .load()
        .get(&grant_id)
        .expect("the grant is installed")
        .audience_handle();
    (node, grant_id, handle)
}

/// W-G5b. A Grant scope whose audience handle is NOT the installed one reads
/// `Unserved` — even when a SIBLING key in the same batch shares its grant id
/// and does match.
///
/// The two are different slots by construction: `SlotKey` carries the whole
/// scope, and an audience-secret rotation leaves both live at once. The stamp
/// map was keyed by `grant_id` alone, so its dedup short-circuit fired before
/// the per-key handle comparison — whichever of the pair the batch reached first
/// decided the other's fate. With the installed scope first (the ordering
/// asserted below, and one of the two `SlotKey`-ascending orders production
/// produces) the stale scope fetched the installed stamp and was SERVED,
/// stamped as current, and passed the read seam.
///
/// Dies to keying `stamps` — or the Grant arm's lookup — by `grant_id` alone.
/// A single-key witness cannot catch it: with one key the handle comparison IS
/// reached, which is why W-G5 as originally specified passes either way.
#[tokio::test]
async fn a_stale_audience_handle_is_unserved_beside_its_installed_sibling() {
    let (node, grant_id, installed_handle) = consumer_with_installed_grant("pairkey").await;
    let source = ScopedSlotSource {
        session_routing: node.session_routing.clone(),
        scoped_discovery: node.scoped_discovery.clone(),
        publication: node.scoped_publication.clone(),
        org_revocation: node.org_revocation.clone(),
        consumer_grants: node.consumer_grant_audiences.clone(),
        consumer_grant_gate: node.consumer_grant_gate.clone(),
        authority: node.routing_authority.clone(),
        settle_gap_hook: parking_lot::Mutex::new(None),
        unserved_scope: node.routing_unserved_scope.clone(),
    };

    let grant_slot = |audience_handle: [u8; 32]| SlotKey {
        scope: PrivateAudienceScope::new(CapabilityAudienceScope::Grant {
            grant_id,
            audience_handle,
        })
        .expect("grant scopes are private"),
        capability: CapabilityAuthorityId::for_tag("nrpc:pair-key"),
    };
    let installed_key = grant_slot(installed_handle);
    // A handle the installed record does NOT carry — a rotated-away audience.
    let mut stale_handle = installed_handle;
    stale_handle[0] ^= 0xff;
    let stale_key = grant_slot(stale_handle);
    assert_ne!(
        installed_handle, stale_handle,
        "precondition: the two scopes must differ in the handle ONLY"
    );

    // The installed scope FIRST — the order under which the id-keyed dedup let
    // the stale one borrow this key's stamp. `grant_installations_for` walks
    // `keys` in slice order, so this is the exact adversarial ordering rather
    // than a hope about how the pair sorts.
    let snapshot = source.snapshot(&[installed_key.clone(), stale_key.clone()]);
    assert!(
        matches!(
            snapshot.providers(&installed_key).facts,
            SourceFacts::Served(_)
        ),
        "precondition: the scope whose handle IS installed must be served, or \
         this witness proves nothing about the sibling"
    );
    assert!(
        matches!(snapshot.providers(&stale_key).facts, SourceFacts::Unserved),
        "a scope whose audience handle is not the installed one has NO evidence \
         — serving it here hands the caller rows the installed handle authorizes \
         under a scope the node has rotated away from"
    );

    // And the reverse ordering, which the old shape happened to get right: the
    // property must hold for BOTH, or it is an accident of iteration order.
    let reversed = source.snapshot(&[stale_key.clone(), installed_key.clone()]);
    assert!(
        matches!(reversed.providers(&stale_key).facts, SourceFacts::Unserved),
        "and the outcome must not depend on which sibling the batch reaches first"
    );
    assert!(
        matches!(
            reversed.providers(&installed_key).facts,
            SourceFacts::Served(_)
        ),
        "the installed scope stays served under either ordering"
    );
}

/// W-G7 (review 2026-07-29 §1). A consumer-Grant REMOVAL cannot occupy the gap
/// between the commit pin's validation and the settlement.
///
/// The pin re-derives the Grant identity vector, which covers snapshot → pin.
/// What it did not cover was pin → settlement: `ScopedCommitPin` held only the
/// authority and publication gates, and `settle_if_current` re-verifies only the
/// revocation store. Consumer-Grant mutation is serialized by its own gate,
/// which neither of those reaches — so a removal landing in that window let
/// phase 5 install facts stamped with a WITHDRAWN installation and still return
/// `Current`. The read seam refuses those facts, so nothing withdrawn is ever
/// served; what the defect produced was a currentness claim that was false, over
/// a slot nothing then requeues.
///
/// The acknowledgement below fires ONLY after the remover's `try_lock` has
/// actually FAILED — the one thing scheduling cannot fake, because it means the
/// pin is provably holding the gate at that instant. Under the mutation (drop
/// the gate from `ScopedCommitPin`) the `try_lock` SUCCEEDS, no acknowledgement
/// is ever sent, and this witness fails at its wait instead of passing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_consumer_grant_removal_cannot_occupy_the_gap_between_validation_and_settlement() {
    use crate::adapter::net::behavior::org_routing::{
        ApplyOutcome, ApplyRequest, DirtyApply, RegistryWork,
    };
    use crate::adapter::net::behavior::org_routing_registry::NodeOrgRoutingRegistry;
    use crate::adapter::net::behavior::org_scoped_store::{
        DirtyCapabilities, PrivateDiscoveryChangeBatch,
    };

    let (node, grant_id, installed_handle) = consumer_with_installed_grant("wg7").await;
    // Adopting the authority installed its revocation store. Without one,
    // `settle_if_current` returns before the gap hook can fire at all, and this
    // witness would pass vacuously.
    assert!(
        node.org_revocation.load().is_some(),
        "precondition: a store must be installed or the settlement gap is unreachable"
    );

    let source = ScopedSlotSource {
        session_routing: node.session_routing.clone(),
        scoped_discovery: node.scoped_discovery.clone(),
        publication: node.scoped_publication.clone(),
        org_revocation: node.org_revocation.clone(),
        consumer_grants: node.consumer_grant_audiences.clone(),
        consumer_grant_gate: node.consumer_grant_gate.clone(),
        authority: node.routing_authority.clone(),
        settle_gap_hook: parking_lot::Mutex::new(None),
        unserved_scope: node.routing_unserved_scope.clone(),
    };
    let key = SlotKey {
        scope: PrivateAudienceScope::new(CapabilityAudienceScope::Grant {
            grant_id,
            audience_handle: installed_handle,
        })
        .expect("grant scopes are private"),
        capability: CapabilityAuthorityId::for_tag("nrpc:pair-key"),
    };

    let removed = Arc::new(AtomicBool::new(false));
    let entered = Arc::new(AtomicBool::new(false));
    let remover: Arc<parking_lot::Mutex<Option<std::thread::JoinHandle<()>>>> = Arc::default();
    let (reached_tx, reached_rx) = std::sync::mpsc::sync_channel::<()>(1);
    // `Receiver` is Send but not Sync, and the gap hook is an `Fn`.
    let reached = Arc::new(parking_lot::Mutex::new(reached_rx));
    // Bounded completion signal, so neither the negative assertion below nor the
    // join afterwards can hang the suite instead of failing it.
    let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
    node.consumer_grant_gate
        .arm_contended_hook(Arc::new(move || {
            let _ = reached_tx.try_send(());
        }));
    {
        let node = node.clone();
        let removed = removed.clone();
        let entered = entered.clone();
        let remover = remover.clone();
        let reached = reached.clone();
        *source.settle_gap_hook.lock() = Some(Arc::new(move || {
            entered.store(true, Ordering::Release);
            // Spawned HERE, not before the quantum: a removal that lands before
            // the pin legitimately defeats it, the quantum reports `Superseded`,
            // and this gap is never entered. The window under test only exists
            // once the pin has been granted.
            let mover = node.clone();
            let landed = removed.clone();
            let done = done_tx.clone();
            *remover.lock() = Some(std::thread::spawn(move || {
                mover.remove_consumer_grant_audience(&grant_id);
                landed.store(true, Ordering::Release);
                let _ = done.send(());
            }));
            // Wait for PROVEN contention: the remover's `try_lock` failed, so the
            // commit pin is demonstrably holding the consumer-Grant gate right
            // now. Neither an interval nor an "about to attempt" signal is
            // evidence — both also occur when the barrier is absent.
            reached.lock().recv_timeout(Duration::from_secs(10)).expect(
                "the remover's try_lock must FAIL, proving the commit pin holds \
                 the consumer-Grant gate; no signal means Grant movement is free \
                 to land between the validation and the settlement",
            );
            // Sound only because of the line above: a failed `try_lock` means the
            // remover is blocked inside the gate, so it cannot yet have run.
            assert!(
                !removed.load(Ordering::Acquire),
                "a consumer-Grant removal landed between the validation and the \
                 settlement — the installation the pin validated is already \
                 withdrawn by the time the facts stamped with it are installed"
            );
            assert!(
                node.consumer_grant_audiences
                    .load()
                    .get(&grant_id)
                    .is_some(),
                "and the snapshot the settlement stamps against must still name \
                 the installation the pin validated"
            );
        }));
    }

    let work: Arc<RegistryWork> = Arc::default();
    let registry = NodeOrgRoutingRegistry::new(Arc::new(source), work, Arc::default());
    registry.activate_incarnation(1);
    let family = registry.new_family().expect("family");
    let held = family.demand(key.clone()).expect("demand");

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
        "the install pass must settle: {outcome:?}"
    );
    assert!(
        entered.load(Ordering::Acquire),
        "the settlement gap must actually have been entered, or this witness \
         asserted nothing"
    );
    assert!(
        registry.base_facts_unvalidated(&key).is_some(),
        "precondition: the quantum installed an artifact under the live grant"
    );

    // The pin dies with the quantum, so the removal completes: the gate DELAYS
    // Grant movement for the length of one installation, it does not refuse it.
    done_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("the removal must proceed once the pin releases the gate");
    if let Some(handle) = remover.lock().take() {
        handle.join().expect("remover thread");
    }
    assert!(
        node.consumer_grant_audiences
            .load()
            .get(&grant_id)
            .is_none(),
        "the removal must actually have removed the grant"
    );
    drop(held);
}

// ---- the Grant-currentness witness group (design §12) ---------------------
//
// Every witness below drives the PRODUCTION source, the production phase-5
// installation and the production read seam. `install_facts_for_test` is
// deliberately not used: it stores into a cell that already exists, and the
// 2B.3a review showed that a witness built on it is blind to the class of defect
// where the production path and the seam under test diverge.

/// The two orgs and the provider the Grant witnesses share.
///
/// The grant is always issued by a DIFFERENT org than the node's own: a grant
/// from one's own org would be the owner plane, and would witness nothing about
/// the Grant plane's authority.
struct GrantFixture {
    node: Arc<MeshNode>,
    /// The consumer's own org — the grantee.
    org: crate::adapter::net::behavior::org::OrgKeypair,
    /// The issuing org.
    issuer: crate::adapter::net::behavior::org::OrgKeypair,
    provider: EntityKeypair,
}

async fn grant_fixture(tag: &str) -> GrantFixture {
    use crate::adapter::net::behavior::org::{OrgKeypair, OrgMembershipCert};
    use crate::adapter::net::behavior::org_authority::NodeAuthority;
    use crate::adapter::net::identity::MAX_TOKEN_CLOCK_SKEW_SECS;
    let node = node().await;
    let org = OrgKeypair::from_bytes([0xa1u8; 32]);
    let issuer = OrgKeypair::from_bytes([0xa2u8; 32]);

    // Adopted at the FULL skew tolerance, unlike `adopt_authority`'s zero.
    //
    // Install validation uses this skew; the routing source and read seam use
    // `MAX_TOKEN_CLOCK_SKEW_SECS`. Matching them is what lets a witness install a
    // grant whose `not_after` has already passed but which is still valid within
    // tolerance — the only way to place an installed Grant's EFFECTIVE deadline
    // (`not_after + skew`) a few seconds out instead of five minutes.
    let entity = node.entity_id().clone();
    let cert = OrgMembershipCert::try_issue(&org, entity.clone(), 1, 3600).expect("issue cert");
    static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("net-grantfx-{tag}-{}-{seq}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let authority = NodeAuthority::adopt(&dir, cert, &entity, MAX_TOKEN_CLOCK_SKEW_SECS, None)
        .expect("adopt authority");
    node.install_node_authority(Arc::new(authority))
        .expect("install authority");
    GrantFixture {
        node,
        org,
        issuer,
        provider: EntityKeypair::generate(),
    }
}

/// A byte-identical copy of a secret — install CONSUMES the original, and a
/// witness that reinstalls the same grant has to present the same bytes twice.
/// The temporary key-bearing buffer is scrubbed after decode (OA2-F hygiene bar).
fn copy_secret(
    s: &crate::adapter::net::behavior::org_grant::OrgAudienceSecret,
) -> crate::adapter::net::behavior::org_grant::OrgAudienceSecret {
    use crate::adapter::net::behavior::org_grant::OrgAudienceSecret;
    let mut buf = s.encode_config();
    let copy = OrgAudienceSecret::decode_config(&buf).expect("copy secret");
    for b in buf.iter_mut() {
        // SAFETY: `b` is a valid mutable reference into the owned array.
        unsafe { std::ptr::write_volatile(b, 0) };
    }
    copy
}

impl GrantFixture {
    /// Mint an issuer→own-org DISCOVER grant over the fixture's provider.
    ///
    /// `grant_id: Some(id)` REUSES that id, which is what a same-ID replacement
    /// needs; `None` allocates a fresh one. `bounds: Some((nbf, exp))` sets the
    /// validity window explicitly, which is what an expiry witness needs;
    /// `None` is a comfortably-valid hour.
    fn mint(
        &self,
        tag: &str,
        grant_id: Option<[u8; 32]>,
        bounds: Option<(u64, u64)>,
    ) -> (
        crate::adapter::net::behavior::org_grant::OrgCapabilityGrant,
        crate::adapter::net::behavior::org_grant::OrgAudienceSecret,
    ) {
        use crate::adapter::net::behavior::org::current_timestamp;
        use crate::adapter::net::behavior::org_grant::{
            GrantRights, GrantTargetScope, OrgAudienceSecret, OrgCapabilityGrant,
        };
        let target = GrantTargetScope::ExactNode(self.provider.entity_id().clone());
        let cap = CapabilityAuthorityId::for_tag(tag);
        let Some(grant_id) = grant_id else {
            let (grant, secret) = OrgCapabilityGrant::try_issue(
                &self.issuer,
                self.org.org_id(),
                cap,
                GrantRights::DISCOVER,
                target,
                bounds.map_or(3600, |(_, exp)| exp.saturating_sub(current_timestamp())),
            )
            .expect("issue grant");
            return (grant, secret.expect("a DISCOVER grant mints a secret"));
        };
        // Reusing an id means minting the audience binding separately and
        // issuing at explicit bounds — `try_issue` always allocates a fresh id.
        let now = current_timestamp();
        let (secret, binding) = OrgAudienceSecret::mint(grant_id);
        let (not_before, not_after) = bounds.unwrap_or((now.saturating_sub(60), now + 3600));
        let grant = OrgCapabilityGrant::issue_at(
            &self.issuer,
            grant_id,
            self.org.org_id(),
            cap,
            GrantRights::DISCOVER,
            target,
            Some(binding),
            not_before,
            not_after,
            // Nonce varies the canonical bytes, so a same-id replacement is a
            // genuinely DIFFERENT signed authority rather than a re-issue that
            // happens to be byte-identical.
            u64::from(grant_id[0]) ^ not_after,
        );
        (grant, secret)
    }

    /// Install a consumer grant and return `(grant_id, installed audience
    /// handle)`. The handle is read off the INSTALLED RECORD, because that is
    /// what the source and the read seam compare against.
    fn install(
        &self,
        grant: crate::adapter::net::behavior::org_grant::OrgCapabilityGrant,
        secret: crate::adapter::net::behavior::org_grant::OrgAudienceSecret,
    ) -> ([u8; 32], [u8; 32]) {
        let grant_id = grant.grant_id;
        self.node
            .install_consumer_grant_audience(grant, secret)
            .expect("install consumer grant");
        let handle = *self
            .node
            .consumer_grant_audiences
            .load()
            .get(&grant_id)
            .expect("the grant is installed")
            .audience_handle();
        (grant_id, handle)
    }

    fn key(&self, grant_id: [u8; 32], audience_handle: [u8; 32], tag: &str) -> SlotKey {
        SlotKey {
            scope: PrivateAudienceScope::new(CapabilityAudienceScope::Grant {
                grant_id,
                audience_handle,
            })
            .expect("grant scopes are private"),
            capability: CapabilityAuthorityId::for_tag(tag),
        }
    }

    /// Warm `key` through the REAL actor path on the node's OWN registry, and
    /// hand back the handle that keeps the slot retained.
    ///
    /// One `apply` pass: the demand queues the slot, phase 3 reconstructs it
    /// through the production source, and phase 5 installs it beneath the commit
    /// pin. Anything short of that would not produce a production stamp.
    fn warm(
        &self,
        key: &SlotKey,
    ) -> crate::adapter::net::behavior::org_routing_registry::DemandHandle {
        use crate::adapter::net::behavior::org_routing::{
            ApplyOutcome, ApplyRequest, DirtyApply, RoutingHealth,
        };
        use crate::adapter::net::behavior::org_scoped_store::{
            DirtyCapabilities, PrivateDiscoveryChangeBatch,
        };
        self.node
            .routing_health
            .store(Arc::new(RoutingHealth::Healthy { incarnation: 1 }));
        self.node.routing_registry.activate_incarnation(1);
        let family = self.node.org_routing_family().expect("family");
        let held = family.demand(key.clone()).expect("demand");
        let outcome = self.node.routing_registry.apply(
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
            "the warming pass must settle, or nothing below is warm: {outcome:?}"
        );
        assert!(
            self.node.org_routing_base_facts(key).is_some(),
            "precondition: the slot must be WARM before the transition under test"
        );
        held
    }

    /// Drive the actor for `key` and require only that an ARTIFACT exists —
    /// not that it reads.
    ///
    /// A Grant scope whose audience handle is not the installed one reconstructs
    /// `Unserved`, which is a real retained artifact but reads cold. [`Self::warm`]
    /// would reject it at its own precondition, so a witness about what
    /// invalidation SPARES needs this weaker form.
    fn retain(
        &self,
        key: &SlotKey,
    ) -> crate::adapter::net::behavior::org_routing_registry::DemandHandle {
        use crate::adapter::net::behavior::org_routing::{
            ApplyOutcome, ApplyRequest, DirtyApply, RoutingHealth,
        };
        use crate::adapter::net::behavior::org_scoped_store::{
            DirtyCapabilities, PrivateDiscoveryChangeBatch,
        };
        self.node
            .routing_health
            .store(Arc::new(RoutingHealth::Healthy { incarnation: 1 }));
        self.node.routing_registry.activate_incarnation(1);
        let family = self.node.org_routing_family().expect("family");
        let held = family.demand(key.clone()).expect("demand");
        let outcome = self.node.routing_registry.apply(
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
            "the retaining pass must settle: {outcome:?}"
        );
        assert!(
            self.node
                .routing_registry
                .base_facts_unvalidated(key)
                .is_some(),
            "precondition: the slot must hold an ARTIFACT before the transition"
        );
        held
    }

    /// The production source over this node, for witnesses that assert on
    /// capture rather than on the read seam.
    fn source(&self) -> ScopedSlotSource {
        ScopedSlotSource {
            session_routing: self.node.session_routing.clone(),
            scoped_discovery: self.node.scoped_discovery.clone(),
            publication: self.node.scoped_publication.clone(),
            org_revocation: self.node.org_revocation.clone(),
            consumer_grants: self.node.consumer_grant_audiences.clone(),
            consumer_grant_gate: self.node.consumer_grant_gate.clone(),
            authority: self.node.routing_authority.clone(),
            settle_gap_hook: parking_lot::Mutex::new(None),
            unserved_scope: self.node.routing_unserved_scope.clone(),
        }
    }
}

/// W-G3. A same-ID replacement cannot reauthorize facts captured under the
/// PREVIOUS installed Grant.
///
/// Two adversarial cases, and they are distinguished by DIFFERENT components:
///
/// - **case 1 — the byte-identical grant, reinstalled.** Same `grant_id`, same
///   signature, same audience handle. The installation identity is the ONLY
///   thing that differs, so it is the only thing that can retire the artifact;
/// - **case 2 — a differently signed grant reusing the id.** A fresh audience
///   binding and nonce make it a different signed authority under the same name.
///
/// **Case 1 must reinstall the retained grant and a byte-identical copy of its
/// secret**, not re-mint one. An earlier version of this witness called
/// `mint(.., Some(grant_id), ..)`, whose explicit-id branch mints a fresh
/// audience secret and nonce — so it produced a different signature and a
/// different handle, and asserted so itself. That made case 1 a second copy of
/// case 2: the artifact was retired by the signature and handle checks, and the
/// installation-identity comparison was never exercised. Kyra's independent
/// mutation run proved it — with `record.install_seq() == *install_seq` removed
/// from the read seam, the entire 54-witness gate stayed green (2026-07-29).
///
/// The assertions below therefore pin what is held EQUAL as hard as what
/// differs: a case-1 that does not assert equal signature and equal handle
/// cannot tell the two cases apart.
#[tokio::test]
async fn a_same_id_grant_replacement_cannot_reauthorize_captured_facts() {
    let f = grant_fixture("wg3").await;
    let (grant, secret) = f.mint("nrpc:wg3", None, None);
    // Retain the EXACT grant and a byte-identical copy of the secret: install
    // consumes both, and case 1 has to present the same bytes a second time.
    let retained_grant = grant.clone();
    let retained_secret = copy_secret(&secret);
    let signature = grant.signature;
    let (grant_id, handle) = f.install(grant, secret);
    let key = f.key(grant_id, handle, "nrpc:wg3");
    let _held = f.warm(&key);

    let warm = f.node.org_routing_base_facts(&key).expect("warm");
    let ScopedDiscoveryAuthorityStamp::Grant {
        grant_id: stamped_id,
        install_seq: first_install_seq,
        ..
    } = warm.authority
    else {
        panic!("precondition: the artifact must carry a GRANT stamp");
    };
    assert_eq!(
        stamped_id, grant_id,
        "precondition: the stamp names the installed grant"
    );

    // --- case 1: the BYTE-IDENTICAL grant, removed and reinstalled ----------
    assert!(
        f.node.remove_consumer_grant_audience(&grant_id),
        "precondition: the grant was installed"
    );
    let (reinstalled_id, reinstalled_handle) = f.install(retained_grant, retained_secret);

    // Everything the currentness relation binds, EXCEPT the installation
    // identity, is held equal — asserted, not assumed. Without these three the
    // case cannot claim to isolate the identity.
    let installed = f.node.consumer_grant_audiences.load();
    let record = installed.get(&grant_id).expect("reinstalled");
    assert_eq!(reinstalled_id, grant_id, "case 1: the SAME grant id");
    assert_eq!(
        record.grant().signature,
        signature,
        "case 1: the SAME signed authority — byte-identical, not a re-issue"
    );
    assert_eq!(
        reinstalled_handle, handle,
        "case 1: the SAME audience handle"
    );
    assert_ne!(
        record.install_seq(),
        first_install_seq,
        "case 1: and a DIFFERENT installation identity — the only thing that \
         differs, and therefore the only thing that can retire the artifact"
    );
    drop(installed);

    assert!(
        f.node.org_routing_base_facts(&key).is_none(),
        "facts captured under the PREVIOUS installation must not survive a \
         remove-then-reinstall of the byte-identical grant: the signature and \
         the handle are unchanged, so ONLY the non-aliasing installation \
         identity can tell the two installations apart"
    );

    // --- case 2: a DIFFERENTLY SIGNED grant reusing the id ------------------
    // Re-warm under the current installation first, so case 2 retires something
    // that was genuinely warm rather than inheriting case 1's cold slot.
    let _held2 = f.warm(&key);
    assert!(
        f.node.org_routing_base_facts(&key).is_some(),
        "precondition: re-warmed under the reinstalled grant"
    );

    let (distinct, distinct_secret) = f.mint("nrpc:wg3-other", Some(grant_id), None);
    assert_eq!(distinct.grant_id, grant_id, "case 2: the SAME grant id");
    assert_ne!(
        distinct.signature, signature,
        "case 2: a DIFFERENT signed authority under that id"
    );
    assert!(f.node.remove_consumer_grant_audience(&grant_id));
    f.install(distinct, distinct_secret);
    assert!(
        f.node.org_routing_base_facts(&key).is_none(),
        "a DISTINCT signed grant reusing the id must not reauthorize facts \
         captured under the grant it replaced"
    );
}

/// W-G4. The signed Grant identity is part of the currentness relation, on its
/// own.
///
/// **A direct comparison witness, and labelled as such deliberately.** Equal
/// installation identity with a different signature is not production-reachable
/// — `install_seq` is strictly monotone, so any replacement that changes the
/// signature also changes the identity. W-G3 covers the reachable composite;
/// this covers the component, by holding `install_seq` and the handle EQUAL to
/// the installed record and moving only the signature.
///
/// Without it, dropping the signature from the comparison leaves W-G3 green:
/// W-G3's case 2 also moves the installation identity, so the identity check
/// alone still retires the artifact. That is exactly the redundancy that hides a
/// missing component.
#[tokio::test]
async fn the_signed_grant_identity_is_part_of_scope_currentness() {
    use crate::adapter::net::behavior::org::current_timestamp;
    let f = grant_fixture("wg4").await;
    let (grant, secret) = f.mint("nrpc:wg4", None, None);
    let (grant_id, handle) = f.install(grant, secret);

    let installed = f.node.consumer_grant_audiences.load();
    let record = installed.get(&grant_id).expect("installed");
    let live = ScopedDiscoveryAuthorityStamp::Grant {
        grant_id,
        install_seq: record.install_seq(),
        grant_signature: record.grant().signature,
        audience_handle: handle,
    };
    let now = current_timestamp();
    assert!(
        f.node.scope_authority_is_current(&live, now),
        "precondition: the exact installed authority is current"
    );

    // EVERY other component held equal — same id, same installation identity,
    // same handle — so only the signature can be what fails.
    let ScopedDiscoveryAuthorityStamp::Grant {
        grant_signature, ..
    } = live
    else {
        panic!("constructed as a Grant stamp");
    };
    let mut forged = grant_signature;
    forged[0] ^= 0xff;
    let tampered = ScopedDiscoveryAuthorityStamp::Grant {
        grant_id,
        install_seq: record.install_seq(),
        grant_signature: forged,
        audience_handle: handle,
    };
    assert!(
        !f.node.scope_authority_is_current(&tampered, now),
        "a stamp whose signed authority differs from the installed one is NOT \
         current, even with the installation identity and handle equal — the \
         signature binds the whole canonical grant, and dropping it from the \
         comparison is invisible to every reachable-path witness"
    );
}

/// W-G5. The audience handle is part of the authority stamp and the currentness
/// comparison, for a SINGLE key.
///
/// Distinct from `a_stale_audience_handle_is_unserved_beside_its_installed_sibling`
/// (W-G5b), which kills sibling aliasing through a `grant_id`-keyed stamp map.
/// This kills removal of the handle from the comparison itself, and asserts it
/// at BOTH seams the handle is checked at:
///
/// - capture: a lone key whose handle is not the installed one must not be
///   served, with no sibling present to alias through;
/// - the read seam: a stamp whose handle differs from the installed record's is
///   not current, with every other component held equal.
#[tokio::test]
async fn the_audience_handle_is_part_of_scope_currentness() {
    use crate::adapter::net::behavior::org::current_timestamp;
    let f = grant_fixture("wg5").await;
    let (grant, secret) = f.mint("nrpc:wg5", None, None);
    let (grant_id, handle) = f.install(grant, secret);

    let mut other = handle;
    other[0] ^= 0xff;

    // --- capture: ONE key, no sibling to alias through ----------------------
    let source = f.source();
    let stale_key = f.key(grant_id, other, "nrpc:wg5");
    let snapshot = source.snapshot(std::slice::from_ref(&stale_key));
    assert!(
        matches!(snapshot.providers(&stale_key).facts, SourceFacts::Unserved),
        "a lone Grant key whose audience handle is not the installed one has NO \
         evidence — this is the single-key case, with nothing else in the batch"
    );
    drop(snapshot);
    let installed_key = f.key(grant_id, handle, "nrpc:wg5");
    let ok = source.snapshot(std::slice::from_ref(&installed_key));
    assert!(
        matches!(ok.providers(&installed_key).facts, SourceFacts::Served(_)),
        "precondition: the same key with the INSTALLED handle is served, so the \
         refusal above is about the handle and not about the fixture"
    );
    drop(ok);

    // --- the read seam: every other component held equal --------------------
    let installed = f.node.consumer_grant_audiences.load();
    let record = installed.get(&grant_id).expect("installed");
    let live = ScopedDiscoveryAuthorityStamp::Grant {
        grant_id,
        install_seq: record.install_seq(),
        grant_signature: record.grant().signature,
        audience_handle: handle,
    };
    let wrong_handle = ScopedDiscoveryAuthorityStamp::Grant {
        grant_id,
        install_seq: record.install_seq(),
        grant_signature: record.grant().signature,
        audience_handle: other,
    };
    let now = current_timestamp();
    assert!(
        f.node.scope_authority_is_current(&live, now),
        "precondition: the exact installed authority is current"
    );
    assert!(
        !f.node.scope_authority_is_current(&wrong_handle, now),
        "a stamp whose audience handle differs from the installed record's is \
         NOT current — the cached path must never compare less than the live \
         query, which checks the handle as defence in depth"
    );
}

/// W-G6. A consumer-Grant INSTALLATION between source capture and commit
/// refuses publication.
///
/// Drives the production reconstruction and the production phase-5 path:
/// `PausingSource` delegates every method to the real [`ScopedSlotSource`] and
/// only adds a pause inside `providers()`, which is exactly the capture→commit
/// window. The install landed there changes the batch's installation vector, so
/// the token the pin re-derives no longer matches the one the snapshot minted.
///
/// Dies to omitting the Grant identities from `SourceToken`: without them the
/// re-derived token compares equal, the pin is granted, and the quantum
/// publishes facts for a batch whose Grant authority moved underneath it.
#[tokio::test]
async fn a_grant_install_between_capture_and_commit_refuses_publication() {
    use crate::adapter::net::behavior::org_routing::{
        ApplyOutcome, ApplyRequest, DirtyApply, RegistryWork, RoutingHealth,
    };
    use crate::adapter::net::behavior::org_routing_registry::NodeOrgRoutingRegistry;
    use crate::adapter::net::behavior::org_scoped_store::{
        DirtyCapabilities, PrivateDiscoveryChangeBatch,
    };

    let f = grant_fixture("wg6").await;
    // A is installed up front and is the key the quantum selects.
    let (grant_a, secret_a) = f.mint("nrpc:wg6", None, None);
    let (id_a, handle_a) = f.install(grant_a, secret_a);
    let key_a = f.key(id_a, handle_a, "nrpc:wg6");

    // B is NOT installed at capture. Its key is selected in the same batch, so
    // installing it mid-capture moves THIS batch's vector — which is the
    // movement the pin must catch, as opposed to the unrelated movement W-G8
    // proves it must ignore.
    let (grant_b, secret_b) = f.mint("nrpc:wg6-b", None, None);
    let handle_b = secret_b.audience_handle;
    let id_b = grant_b.grant_id;
    let key_b = f.key(id_b, handle_b, "nrpc:wg6-b");

    let during_build: Arc<parking_lot::Mutex<Option<BuildHook>>> = Arc::default();
    let source = PausingSource {
        inner: f.source(),
        during_build: during_build.clone(),
        after_pin: Arc::default(),
        before_pin: Arc::default(),
    };

    let landed = Arc::new(AtomicBool::new(false));
    {
        let node = f.node.clone();
        let landed = landed.clone();
        let pending = parking_lot::Mutex::new(Some((grant_b, secret_b)));
        *during_build.lock() = Some(Box::new(move || {
            let Some((grant, secret)) = pending.lock().take() else {
                return;
            };
            node.install_consumer_grant_audience(grant, secret)
                .expect("the mid-capture install must itself succeed");
            landed.store(true, Ordering::Release);
        }));
    }

    let work: Arc<RegistryWork> = Arc::default();
    let registry = NodeOrgRoutingRegistry::new(Arc::new(source), work, Arc::default());
    f.node
        .routing_health
        .store(Arc::new(RoutingHealth::Healthy { incarnation: 1 }));
    registry.activate_incarnation(1);
    let family = registry.new_family().expect("family");
    let held_a = family.demand(key_a.clone()).expect("demand a");
    let held_b = family.demand(key_b.clone()).expect("demand b");

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
        landed.load(Ordering::Acquire),
        "the mid-capture install must actually have run, or this witness \
         asserted nothing"
    );
    assert!(
        matches!(outcome, ApplyOutcome::Superseded),
        "an installation between capture and commit must defeat the pin: \
         {outcome:?}"
    );
    assert!(
        registry.base_facts_unvalidated(&key_a).is_none(),
        "and NOTHING may be published — not even the key whose own Grant did \
         not move, because the batch it was captured with is no longer current"
    );
    assert!(
        registry.base_facts_unvalidated(&key_b).is_none(),
        "least of all the key whose Grant arrived mid-capture"
    );
    drop(held_a);
    drop(held_b);
}

/// W-G8. Movement of an UNRELATED Grant preserves the exact unaffected slot.
///
/// The inverse-direction proof for W-G6. The batch-wide installation vector in
/// `SourceToken` protects one capture→commit transaction; the per-key stamp in
/// `SlotBaseFacts` protects one cached artifact. If the transaction-wide value
/// had leaked into the artifact stamp, every Grant install or removal anywhere
/// on the node would cold every cached Grant artifact — and, because the owner
/// plane is stamped `Owner` and compared trivially, the damage would be silent
/// and Grant-only.
///
/// Dies to replacing the per-key stamp check with a global "some Grant moved"
/// bit.
#[tokio::test]
async fn unrelated_grant_movement_preserves_the_exact_slot() {
    let f = grant_fixture("wg8").await;
    let (grant_a, secret_a) = f.mint("nrpc:wg8-a", None, None);
    let (id_a, handle_a) = f.install(grant_a, secret_a);
    let key_a = f.key(id_a, handle_a, "nrpc:wg8-a");
    let _held = f.warm(&key_a);

    let warm = f
        .node
        .org_routing_base_facts(&key_a)
        .expect("precondition: A's slot is warm");

    // A second, entirely unrelated grant: different id, different audience,
    // different capability. Nothing about A's artifact depends on it.
    let (grant_b, secret_b) = f.mint("nrpc:wg8-b", None, None);
    let (id_b, _handle_b) = f.install(grant_b, secret_b);
    assert_ne!(id_a, id_b, "precondition: the two grants are unrelated");

    let after_install = f
        .node
        .org_routing_base_facts(&key_a)
        .expect("installing an unrelated Grant must not cold A's slot");
    assert!(
        Arc::ptr_eq(&warm, &after_install),
        "and must not merely leave it readable — it must be the EXACT same \
         artifact, not a silently rebuilt one"
    );

    // Removal is the other direction of movement, and the one a global bit
    // would be most tempting to implement.
    assert!(
        f.node.remove_consumer_grant_audience(&id_b),
        "precondition: B was installed"
    );
    let after_removal = f
        .node
        .org_routing_base_facts(&key_a)
        .expect("removing an unrelated Grant must not cold A's slot either");
    assert!(
        Arc::ptr_eq(&warm, &after_removal),
        "still the exact same artifact"
    );

    // And the control: moving A's OWN grant must cold it, or the assertions
    // above would also pass on a seam that checks nothing at all.
    assert!(f.node.remove_consumer_grant_audience(&id_a));
    assert!(
        f.node.org_routing_base_facts(&key_a).is_none(),
        "control: moving the slot's OWN Grant must cold it — without this the \
         witness above is satisfied by a comparison that never fails"
    );
}

/// W-G13. An installed Grant's own deadline retires its cached facts through
/// the PRODUCTION expiry path — **including, and especially, with ZERO
/// providers.**
///
/// The empty-provider case is the point, not an edge. With no provider rows the
/// artifact has no deadline from its rows: `row_expiry` is `u64::MAX`. So a
/// source that derives expiry from provider rows alone publishes an artifact
/// that claims never to expire while the authority behind it expires shortly —
/// and NOTHING else in the system can say otherwise. The installed Grant is not
/// a scoped row, so it reaches neither `ScopedDiscoveryState::next_visible_expiry`
/// nor the exact-expiry timer it drives.
///
/// That was the state at `df32cbd7d`. The read seam refused an expired Grant, so
/// nothing withdrawn was ever served — but retirement was READER-TRIGGERED, and
/// with no reader the artifact sat retained indefinitely with nothing armed to
/// notice. Kyra's independent review named the gap and required the deadline to
/// reach a production deadline/wake path (2026-07-29, finding 2). Scope item 12.
///
/// This drives the whole edge against a LIVE production supervisor:
///
/// 1. an installed, currently valid DISCOVER Grant with ZERO providers;
/// 2. warmed through the real actor and `DirtyApply::apply`;
/// 3. `Served(empty)` — proven-empty evidence, not `Unserved`;
/// 4. the artifact's deadline IS the Grant's effective deadline, and the
///    registry arms to exactly that;
/// 5. the read seam refuses it past that instant (asserted at an explicit clock,
///    so this half needs no waiting at all);
/// 6. the deadline passes and the ACTOR's own arm fires — no reader touches the
///    slot, and no scoped mutation wakes it;
/// 7. the requeued rebuild settles `Unserved`;
/// 8. shutdown joins every spawned task.
///
/// **On the timing.** The effective deadline is `not_after + MAX_TOKEN_CLOCK_SKEW_SECS`,
/// and the skew is 300 s — so a grant issued to expire "soon" still arms five
/// minutes out. Instead the grant is issued with `not_after` already PAST but
/// still inside the skew tolerance, which is a legitimate installable state and
/// places the effective deadline a few seconds away. That is why this witness
/// needs neither a mocked clock nor a paused runtime: `start_paused` freezes
/// time and a live node never goes idle, so its auto-advance never fires and the
/// sleeps simply never complete. Every wait below is bounded and polls real
/// state; none is used as ordering evidence.
///
/// Dies to deriving the artifact deadline from provider rows alone, and to
/// omitting the installed-Grant deadline from the source.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_installed_grants_expiry_colds_its_facts_with_zero_providers() {
    use crate::adapter::net::behavior::org::current_timestamp;
    use crate::adapter::net::behavior::org_routing_registry::SourceFacts;
    use crate::adapter::net::identity::MAX_TOKEN_CLOCK_SKEW_SECS;

    let f = grant_fixture("wg13").await;
    let base = current_timestamp();
    // `not_after` already past, still valid within tolerance — see the note
    // above. The effective deadline lands `LEAD` seconds out.
    const LEAD: u64 = 4;
    let not_after = base + LEAD - MAX_TOKEN_CLOCK_SKEW_SECS;
    let effective_deadline = not_after + MAX_TOKEN_CLOCK_SKEW_SECS;
    assert_eq!(
        effective_deadline,
        base + LEAD,
        "precondition: the arm is seconds away, not five minutes"
    );
    // An explicit id, because only that branch of `mint` issues at EXPLICIT
    // bounds — the fresh-id branch derives a TTL from `now`, which is zero for a
    // `not_after` already in the past.
    let (grant, secret) = f.mint(
        "nrpc:wg13",
        Some([0x13u8; 32]),
        Some((base.saturating_sub(3600), not_after)),
    );
    let (grant_id, handle) = f.install(grant, secret);
    let key = f.key(grant_id, handle, "nrpc:wg13");
    // A SECOND slot under the same grant, for the read-seam probe only — see
    // the note at that probe for why it cannot share the actor's slot.
    let probe_key = f.key(grant_id, handle, "nrpc:wg13-probe");

    // The real production supervisor. ZERO providers are announced.
    f.node.start();
    assert!(
        until(|| f.node.org_routing_ready()).await,
        "precondition: the actor must reach Healthy before anything is demanded"
    );

    let family = f.node.org_routing_family().expect("family");
    let held = family.demand(key.clone()).expect("demand");
    let held_probe = family.demand(probe_key.clone()).expect("demand probe");
    assert!(
        until(|| f.node.org_routing_base_facts(&key).is_some()
            && f.node.org_routing_base_facts(&probe_key).is_some())
        .await,
        "precondition: the actor must warm BOTH slots under a currently-valid Grant"
    );

    let warm = f.node.org_routing_base_facts(&key).expect("warm");
    assert!(
        matches!(&warm.providers, SourceFacts::Served(p) if p.is_empty()),
        "an installed current Grant with no providers is SERVED with exact empty \
         evidence, not Unserved"
    );
    assert_eq!(
        warm.earliest_expiry, effective_deadline,
        "THE point of this witness: with ZERO provider rows the artifact's only \
         possible deadline is its AUTHORITY's. Derived from rows alone this is \
         u64::MAX — an artifact claiming never to expire while the Grant behind \
         it expires in seconds, with nothing armed to notice"
    );
    assert_eq!(
        f.node.routing_registry.next_artifact_deadline(),
        Some(effective_deadline),
        "and the registry must ARM to it — this is what the actor sleeps on, so \
         a deadline that never reaches here wakes nobody"
    );

    // The read-seam half, at an explicit clock and on the PROBE slot: no
    // waiting, and it isolates "the seam refuses" from "the actor retires",
    // which are different claims about different mechanisms.
    //
    // On the PROBE slot specifically, because `org_routing_base_facts_at`
    // INVALIDATES what it refuses. Probing the actor's own slot would make a
    // reader the thing that retired the artifact, and every actor-arm assertion
    // below would then pass with no actor arm at all — the witness would be
    // contaminating its own evidence.
    assert!(
        f.node
            .org_routing_base_facts_at(&probe_key, effective_deadline.saturating_sub(1))
            .is_some(),
        "precondition: still current one second before the effective deadline"
    );
    assert!(
        f.node
            .org_routing_base_facts_at(&probe_key, effective_deadline)
            .is_none(),
        "an installed-but-expired Grant authorizes NOTHING, and the boundary is          exact: `now >= not_after + skew`, matching `check_time_bounds_at`"
    );
    assert_eq!(
        f.node.routing_registry.next_artifact_deadline(),
        Some(effective_deadline),
        "and the actor's OWN slot is still armed after that probe — proof the          read-seam half is not what retires it below"
    );

    // --- the ACTOR's own arm ------------------------------------------------
    // Nothing below reads the slot or mutates scoped discovery. The only thing
    // that can retire this artifact is the actor waking on the deadline it
    // armed to.
    assert!(
        until(|| f.node.routing_registry.next_artifact_deadline().is_none()).await,
        "the actor must wake on its OWN deadline arm and retire the expired \
         artifact; a deadline still armed here means nothing consumed it"
    );
    assert!(
        until(|| {
            f.node
                .routing_registry
                .base_facts_unvalidated(&key)
                .is_some_and(|facts| matches!(facts.providers, SourceFacts::Unserved))
        })
        .await,
        "and the requeued rebuild must settle UNSERVED: past its deadline the \
         installed Grant authorizes nothing, so the scope has no evidence at all"
    );
    assert!(
        f.node.org_routing_base_facts(&key).is_none(),
        "the read seam is cold — and now AGREES with the retained set rather \
         than being the only thing that knew"
    );

    // NO SPIN. A deadline arm that fires, retires, and finds the same deadline
    // waiting is a busy actor — the failure mode this arm most easily
    // introduces. It converges because the rebuild installs `Unserved`, which
    // carries no deadline; assert the convergence rather than trusting it.
    let settled = f.node.org_routing_reconciliation_counts();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        f.node.org_routing_reconciliation_counts()[0],
        settled[0],
        "the actor must be QUIESCENT once the expired artifact is retired — a          climbing install count means the deadline arm re-fires on what it just          rebuilt"
    );
    assert!(
        f.node.routing_registry.next_artifact_deadline().is_none(),
        "and nothing is armed: `Unserved` carries no deadline, which is what          makes the retirement terminal rather than cyclic"
    );

    drop(held);
    drop(held_probe);
    let _ = f.node.shutdown().await;
    assert!(
        f.node.routing_task.lock().is_none(),
        "every spawned task must be joined"
    );
}

// ---- OLB-2B.3c-pre step 3: the consumer-Grant wake edge (items 10/11) ------
//
// The gap these close, from design §0.1 — the one the boundary design named as a
// defect it "would have specified into existence":
//
//   INSTALL after an Unserved publication -> nothing moves -> the slot stays
//                                            Unserved indefinitely [availability]
//   REMOVE after a Served publication     -> cached facts stay retained until a
//                                            reader happens by      [promptness]
//
// The read seam cannot close either. For INSTALL it returns cold on `Unserved`
// WITHOUT invalidating, so it never requeues; for REMOVE it only acts when
// someone reads. Neither is reachable from `ScopedDiscoveryState::revision`,
// because a Grant transition does not mutate the scoped store at all (item 11).

/// W-W1. Installing a consumer Grant WAKES routing, so a slot that reconstructed
/// `Unserved` becomes warm — with no scoped mutation of any kind.
///
/// This is the availability half of design §0.1. The slot is demanded and warmed
/// BEFORE the grant exists, so the actor installs an `Unserved` artifact for it;
/// nothing about the scoped store then changes. Only the install notification can
/// make it serveable.
///
/// Dies to dropping the install-path notification.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn installing_a_consumer_grant_wakes_the_affected_grant_slot() {
    use crate::adapter::net::behavior::org_routing_registry::SourceFacts;

    let f = grant_fixture("ww1").await;
    // Mint but do NOT install: the scope is known, the authority is not present.
    let (grant, secret) = f.mint("nrpc:ww1", Some([0xd1u8; 32]), None);
    let grant_id = grant.grant_id;
    let handle = secret.audience_handle;
    let key = f.key(grant_id, handle, "nrpc:ww1");

    f.node.start();
    assert!(
        until(|| f.node.org_routing_ready()).await,
        "precondition: the actor must reach Healthy"
    );
    let family = f.node.org_routing_family().expect("family");
    let held = family.demand(key.clone()).expect("demand");

    // Warmed to UNSERVED: the grant is not installed, so the source has no
    // evidence for this scope.
    assert!(
        until(|| {
            f.node
                .routing_registry
                .base_facts_unvalidated(&key)
                .is_some_and(|facts| matches!(facts.providers, SourceFacts::Unserved))
        })
        .await,
        "precondition: with no installed Grant the slot reconstructs UNSERVED"
    );
    assert!(
        f.node.org_routing_base_facts(&key).is_none(),
        "precondition: and reads cold — note WITHOUT invalidating, which is why \
         no reader can ever rescue this slot"
    );
    let scoped_before = f.node.scoped_discovery.lock().revision();

    // The install. Nothing else.
    f.install(grant, secret);

    assert!(
        until(|| {
            f.node
                .routing_registry
                .base_facts_unvalidated(&key)
                .is_some_and(|facts| matches!(facts.providers, SourceFacts::Served(_)))
        })
        .await,
        "installing the Grant must WAKE routing and re-serve the slot; without \
         the install notification nothing moves and it stays Unserved forever"
    );
    assert!(
        f.node.org_routing_base_facts(&key).is_some(),
        "and the read seam must now serve it"
    );
    assert_eq!(
        f.node.scoped_discovery.lock().revision(),
        scoped_before,
        "and the scoped revision must NOT have moved — item 11: a Grant \
         transition does not mutate the scoped store, so the scoped revision \
         cannot be what triggered this"
    );

    drop(held);
    let _ = f.node.shutdown().await;
}

/// W-W2. Removing a consumer Grant WAKES routing, so the warm artifact is
/// retired and rebuilt `Unserved` — with no reader touching the slot.
///
/// The promptness half of design §0.1. `scope_authority_is_current` already
/// refuses a withdrawn Grant, so nothing withdrawn is ever SERVED; what was
/// missing is that the retained set kept disagreeing with what a reader would be
/// told, indefinitely, if no reader arrived.
///
/// Nothing below calls `org_routing_base_facts` between the removal and the
/// assertion, so a reader cannot be what retired it.
///
/// Dies to dropping the removal-path notification.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn removing_a_consumer_grant_wakes_the_affected_grant_slot() {
    use crate::adapter::net::behavior::org_routing_registry::SourceFacts;

    let f = grant_fixture("ww2").await;
    let (grant, secret) = f.mint("nrpc:ww2", None, None);
    let (grant_id, handle) = f.install(grant, secret);
    let key = f.key(grant_id, handle, "nrpc:ww2");

    f.node.start();
    assert!(until(|| f.node.org_routing_ready()).await, "healthy");
    let family = f.node.org_routing_family().expect("family");
    let held = family.demand(key.clone()).expect("demand");
    assert!(
        until(|| f.node.org_routing_base_facts(&key).is_some()).await,
        "precondition: the slot is warm under the installed Grant"
    );
    let scoped_before = f.node.scoped_discovery.lock().revision();

    assert!(
        f.node.remove_consumer_grant_audience(&grant_id),
        "precondition: the grant was installed"
    );

    assert!(
        until(|| {
            f.node
                .routing_registry
                .base_facts_unvalidated(&key)
                .is_some_and(|facts| matches!(facts.providers, SourceFacts::Unserved))
        })
        .await,
        "removing the Grant must WAKE routing and rebuild the slot as Unserved, \
         with no reader involved; without the removal notification the stale \
         artifact stays retained until something happens to read it"
    );
    assert_eq!(
        f.node.scoped_discovery.lock().revision(),
        scoped_before,
        "and again NOT via the scoped revision (item 11)"
    );

    drop(held);
    let _ = f.node.shutdown().await;
}

/// W-W3. The wake is EXACT: moving one Grant leaves another Grant's artifact and
/// the whole Owner plane untouched.
///
/// The counterpart to W-G8, one layer down. W-G8 pins that the per-key STAMP does
/// not carry batch-wide state; this pins that the INVALIDATION does not either.
/// "Invalidate everything on any Grant movement" would satisfy W-W1 and W-W2
/// perfectly and make routine grant churn globally destructive.
///
/// Dies to widening `invalidate_grant_scope` to all retained slots, and to
/// matching the Owner plane.
#[tokio::test]
async fn consumer_grant_movement_wakes_only_the_affected_scope() {
    let f = grant_fixture("ww3").await;
    let (grant_a, secret_a) = f.mint("nrpc:ww3-a", None, None);
    let (id_a, handle_a) = f.install(grant_a, secret_a);
    let key_a = f.key(id_a, handle_a, "nrpc:ww3-a");
    let owner_key = slot(44, "nrpc:ww3-owner");

    // Warm both planes through the real actor path.
    let _held_a = f.warm(&key_a);
    let _held_owner = f.warm(&owner_key);
    let warm_a = f.node.org_routing_base_facts(&key_a).expect("A warm");
    let warm_owner = f
        .node
        .org_routing_base_facts(&owner_key)
        .expect("owner warm");

    // Move an entirely unrelated grant: install, then remove.
    let (grant_b, secret_b) = f.mint("nrpc:ww3-b", None, None);
    let (id_b, _handle_b) = f.install(grant_b, secret_b);
    assert_ne!(id_a, id_b, "precondition: unrelated grants");

    assert!(
        f.node
            .org_routing_base_facts(&key_a)
            .is_some_and(|live| Arc::ptr_eq(&live, &warm_a)),
        "after B install: A's EXACT artifact must survive unrelated Grant \
         movement — not merely be readable, but be the same artifact"
    );
    assert!(
        f.node
            .org_routing_base_facts(&owner_key)
            .is_some_and(|live| Arc::ptr_eq(&live, &warm_owner)),
        "after B install: and the Owner plane must be untouched — it has no \
         consumer Grant to move"
    );

    assert!(f.node.remove_consumer_grant_audience(&id_b));
    assert!(
        f.node
            .org_routing_base_facts(&key_a)
            .is_some_and(|live| Arc::ptr_eq(&live, &warm_a)),
        "after B removal: still A's exact artifact"
    );
    assert!(
        f.node
            .org_routing_base_facts(&owner_key)
            .is_some_and(|live| Arc::ptr_eq(&live, &warm_owner)),
        "after B removal: Owner still untouched"
    );

    // Control: moving A's OWN grant must cold A and still spare Owner. Without
    // this the assertions above are satisfied by an invalidation that never runs.
    assert!(f.node.remove_consumer_grant_audience(&id_a));
    assert!(
        f.node
            .routing_registry
            .base_facts_unvalidated(&key_a)
            .is_none(),
        "control: moving the slot's OWN Grant must retire its artifact"
    );
    assert!(
        f.node
            .org_routing_base_facts(&owner_key)
            .is_some_and(|live| Arc::ptr_eq(&live, &warm_owner)),
        "control: and even then the Owner plane is spared"
    );
}

/// W-W4. At the notification instant, the gate is ALREADY RELEASED and the
/// snapshot ALREADY carries the transition.
///
/// Item 10 is an ordering requirement, and both halves have real failure modes:
///
/// - **gate still held** — `pin_if_current` takes the consumer-Grant gate and
///   THEN the registry lock, so notifying under the gate acquires those two in
///   the opposite order. A lock-order inversion is not something to discover in
///   production;
/// - **snapshot not yet published** — the actor's recapture reads
///   `consumer_grant_audiences` lock-free. A wake that arrives before publication
///   can reconstruct against the OLD snapshot and install exactly the staleness
///   the wake exists to clear. A late wake is merely late; an early one is wrong.
///
/// Both are checked inside the window, because neither is observable outside it.
///
/// **On the gate half:** a `debug_assert!` in `note_consumer_grant_movement`
/// checks the same thing and, being upstream of this hook, is what actually
/// fires in a debug build — so under the "notify under the guard" mutation the
/// panic comes from there, not from the assertion below. That is deliberate
/// defence in depth: the `debug_assert` is the primary and costs nothing in
/// release, and the assertion below is the backstop if it is ever deleted. Both
/// name item 10.
///
/// Dies to moving the notification inside the guard (RED: the `debug_assert`),
/// and to notifying before the publication (RED: the assertion below).
#[tokio::test]
async fn a_grant_movement_notification_runs_after_publication_and_after_release() {
    let f = grant_fixture("ww4").await;

    let observations: Arc<parking_lot::Mutex<Vec<(bool, bool)>>> = Arc::default();
    {
        let sink = observations.clone();
        let weak = Arc::downgrade(&f.node);
        f.node.arm_grant_movement_hook(Arc::new(move |movement| {
            let Some(node) = weak.upgrade() else {
                return;
            };
            // A parking_lot mutex is not reentrant, so `try_lock` fails when THIS
            // thread holds it — which is exactly the case being ruled out.
            let gate_released = node.consumer_grant_gate.mu.try_lock().is_some();
            let published = node
                .consumer_grant_audiences
                .load()
                .get(&movement.grant_id)
                .is_some();
            sink.lock().push((gate_released, published));
        }));
    }

    // --- the INSTALL transition: published means PRESENT ---------------------
    let (grant, secret) = f.mint("nrpc:ww4", None, None);
    let (grant_id, _handle) = f.install(grant, secret);
    {
        let seen = observations.lock().clone();
        assert_eq!(seen.len(), 1, "one notification for one install");
        assert!(
            seen[0].0,
            "the consumer-Grant gate must be RELEASED at the notification \
             instant — holding it here is the gate -> registry inversion of the \
             commit pin's own order"
        );
        assert!(
            seen[0].1,
            "and the new snapshot must ALREADY be published — a wake that \
             precedes publication lets the actor rebuild against the old \
             snapshot and reinstall the staleness it was woken to clear"
        );
    }

    // --- the REMOVAL transition: published means ABSENT ---------------------
    observations.lock().clear();
    assert!(f.node.remove_consumer_grant_audience(&grant_id));
    let seen = observations.lock().clone();
    assert_eq!(seen.len(), 1, "one notification for one removal");
    assert!(seen[0].0, "gate released on the removal path too");
    assert!(
        !seen[0].1,
        "and the removal must ALREADY be published — the snapshot must no longer \
         carry the grant when routing is told it moved"
    );
}

/// W-W5. An outcome that publishes NOTHING wakes nothing.
///
/// Three non-publishing outcomes, each a distinct reason: an idempotent install,
/// a removal of a grant that is not there, and a removal under a STALE lease. The
/// last is the one with teeth — a caller holding a superseded lease could
/// otherwise churn the exact slots the CURRENT installation is serving, on
/// demand.
///
/// Mirrors the `assert_no_effect` discipline from step 1: the negative claim is
/// the interesting one, and it is only provable against a counter.
#[tokio::test]
async fn a_non_publishing_grant_outcome_wakes_nothing() {
    use crate::adapter::net::behavior::org_grant_registry::ConsumerAudienceLease;

    let f = grant_fixture("ww5").await;
    let (grant, secret) = f.mint("nrpc:ww5", None, None);
    let retained = grant.clone();
    let retained_secret = copy_secret(&secret);
    let (grant_id, _handle) = f.install(grant, secret);

    let after_install = f.node.consumer_grant_movements_for_test();
    assert_eq!(after_install, 1, "precondition: the real install woke once");

    // 1. Idempotent install — valid, publishes nothing.
    f.node
        .install_consumer_grant_audience(retained, retained_secret)
        .expect("idempotent install is valid");
    assert_eq!(
        f.node.consumer_grant_movements_for_test(),
        after_install,
        "an IDEMPOTENT install publishes nothing and must wake nothing"
    );

    // 2. A stale lease — same grant id, superseded installation.
    let stale = ConsumerAudienceLease::new(grant_id, u64::MAX);
    assert!(
        !f.node.remove_consumer_grant_audience_if_current(&stale),
        "precondition: a stale lease owns nothing"
    );
    assert_eq!(
        f.node.consumer_grant_movements_for_test(),
        after_install,
        "a STALE lease publishes nothing and must wake nothing — otherwise a \
         superseded lease holder can churn the live installation's slots at will"
    );

    // 3. Removing a grant that is not installed.
    assert!(!f.node.remove_consumer_grant_audience(&[0xffu8; 32]));
    assert_eq!(
        f.node.consumer_grant_movements_for_test(),
        after_install,
        "a no-op removal publishes nothing and must wake nothing"
    );

    // And the control: a REAL removal does wake, so the counter is live.
    assert!(f.node.remove_consumer_grant_audience(&grant_id));
    assert_eq!(
        f.node.consumer_grant_movements_for_test(),
        after_install + 1,
        "control: a real publication DOES wake — without this the assertions \
         above pass against a counter that never moves"
    );
}

/// W-W6. A DELAYED notification for installation N cannot retire an artifact
/// built under N+1.
///
/// A notification can be arbitrarily delayed between its publication and its
/// registry work — the gate is released in between, precisely so it is not held
/// across the registry lock. So an OBSOLETE transition can arrive after a newer
/// installation has already been published, notified, rebuilt and warmed:
///
/// ```text
/// A: remove N    publish absence, release gate, [stalled here]
/// B: install N+1                 publish, notify, actor warms the N+1 artifact
/// A: [resumes]   invalidate      <- must NOT touch the N+1 artifact
/// ```
///
/// Clearing by `grant_id` alone destroys the successor. It never resurrects
/// withdrawn authority — the read seam stays fail-closed — but it lets an
/// obsolete transition retire CURRENT work, which is the defect class
/// `invalidate_if_stale` already guards one layer up: a delayed invalidator must
/// not delete a successor.
///
/// The decision has to hold at the registry mutation boundary, not merely before
/// it: a "is this snapshot still current?" check taken before the lock can be
/// invalidated by a publication landing between the check and the clear. This
/// witness therefore asserts on the ARTIFACT, by pointer identity.
///
/// Found by Kyra's independent review of `fa0b9ddd5` (P1). Dies to comparing
/// `grant_id` alone — i.e. to the implementation at that head.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_delayed_grant_notification_cannot_retire_a_successor_installation() {
    let f = grant_fixture("ww6").await;
    let (grant, secret) = f.mint("nrpc:ww6", Some([0xa6u8; 32]), None);
    // Retained so the reinstall below is BYTE-IDENTICAL: same signature, same
    // handle, same id. Only the installation identity separates N from N+1,
    // which is exactly what the invalidation must key on.
    let retained_grant = grant.clone();
    let retained_secret = copy_secret(&secret);
    let (grant_id, handle) = f.install(grant, secret);
    let key = f.key(grant_id, handle, "nrpc:ww6");

    f.node.start();
    assert!(until(|| f.node.org_routing_ready()).await, "healthy");
    let family = f.node.org_routing_family().expect("family");
    let held = family.demand(key.clone()).expect("demand");
    assert!(
        until(|| f.node.org_routing_base_facts(&key).is_some()).await,
        "precondition: warm under installation N"
    );

    // Park the REMOVAL's notification after publication and gate release, before
    // its registry work. The install's notification must pass through freely, or
    // there is no successor to protect.
    //
    // The shared one-shot latch, NOT a predicate on the movement. It was written
    // that way because the first repair's `superseded_through` gave remove(N) and
    // install(N+1) the same value; that mechanism is gone — the fence is a
    // publication generation now, and the two DO differ — but the latch stays,
    // because "distinguishable today" is not a property a future fence change
    // preserves. The removal's notification is guaranteed first: nothing installs
    // until `reached` is observed. The captured fence is not read here; what this
    // witness asserts about the transition is the artifact it must not destroy.
    let (reached_rx, release_tx, _parked_fence) = park_first_grant_notification(&f.node);

    let remover = {
        let node = f.node.clone();
        std::thread::spawn(move || node.remove_consumer_grant_audience(&grant_id))
    };
    reached_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("the removal must reach its notification and park there");

    // The removal has PUBLISHED absence. Reinstall the byte-identical grant: it
    // takes a fresh installation identity, notifies, and the actor rebuilds.
    f.node
        .install_consumer_grant_audience(retained_grant, retained_secret)
        .expect("reinstall");
    assert!(
        until(|| f.node.org_routing_base_facts(&key).is_some()).await,
        "the successor installation must warm the slot again"
    );
    let successor = f
        .node
        .org_routing_base_facts(&key)
        .expect("successor artifact");
    let counts_before = f.node.org_routing_reconciliation_counts();

    // Now let the obsolete removal notification run.
    let _ = release_tx.send(());
    assert!(
        remover.join().expect("remover thread"),
        "precondition: the removal itself did publish"
    );

    assert!(
        f.node
            .routing_registry
            .base_facts_unvalidated(&key)
            .is_some_and(|live| Arc::ptr_eq(&live, &successor)),
        "the delayed notification for the SUPERSEDED installation must leave the \
         successor artifact exactly as it was — pointer identity, not merely \
         eventual readability"
    );
    let counts_after = f.node.org_routing_reconciliation_counts();
    assert_eq!(
        counts_after[2], counts_before[2],
        "and must invalidate nothing: an obsolete transition retiring current \
         work is the defect, even though the read seam would stay fail-closed"
    );
    assert_eq!(
        counts_after[0], counts_before[0],
        "and must not have re-queued it either — a rebuild here means the \
         successor was needlessly cold-pathed by an obsolete transition"
    );

    drop(held);
    let _ = f.node.shutdown().await;
}

/// W-W7. A Grant transition churns only its OWN audience scope.
///
/// Three controls, each a different way the selection could be too broad:
///
/// - the same grant id under a STALE audience handle — the rotated-away scope,
///   which this transition says nothing about;
/// - an unrelated grant entirely;
/// - the Owner plane, which has no consumer Grant at all.
///
/// All three must keep their EXACT artifact.
///
/// **The same grant id under a different CAPABILITY is deliberately NOT spared**,
/// and that is a decision rather than an oversight. The Grant source answers any
/// capability under an installed `(grant_id, audience_handle)` with
/// `Served(empty)` rather than `Unserved`, whatever the grant's own capability
/// scope says — verified directly: a slot for a capability the grant does not
/// cover reconstructs as `Served(0 providers)` and is stamped `Grant`. So the
/// grant's movement genuinely does affect it, and sparing it would leave a
/// Grant-stamped artifact behind after removal and a permanently `Unserved` slot
/// after install — the two defects this whole edge exists to close, reintroduced
/// for a subset. The assertion below pins the current behaviour so that if the
/// source is ever narrowed to refuse uncovered capabilities, this fails loudly
/// and the invalidation is narrowed in the same change.
///
/// Found by Kyra's independent review of `fa0b9ddd5` (P2). Dies to selecting by
/// `grant_id` alone.
#[tokio::test]
async fn consumer_grant_movement_preserves_same_id_unaffected_scopes() {
    let f = grant_fixture("ww7").await;
    let (grant, secret) = f.mint("nrpc:ww7", Some([0xa7u8; 32]), None);
    let (grant_id, handle) = f.install(grant, secret);

    // The scope that moves.
    let moved = f.key(grant_id, handle, "nrpc:ww7");
    // Same grant id, a rotated-away audience handle: a different slot entirely.
    let mut stale_handle = handle;
    stale_handle[0] ^= 0xff;
    let stale_scope = f.key(grant_id, stale_handle, "nrpc:ww7");
    // Same grant id and handle, a different capability — see the note above.
    let other_capability = f.key(grant_id, handle, "nrpc:ww7-other-capability");
    // An unrelated grant, and the Owner plane.
    let (grant_b, secret_b) = f.mint("nrpc:ww7-b", None, None);
    let (id_b, handle_b) = f.install(grant_b, secret_b);
    let unrelated = f.key(id_b, handle_b, "nrpc:ww7-b");
    let owner = slot(45, "nrpc:ww7-owner");

    let _h1 = f.warm(&moved);
    // RETAINED, not warm: a rotated-away handle reconstructs `Unserved`, which is
    // a real artifact that reads cold. That is precisely the shape this control
    // needs — an artifact the transition must not touch.
    let _h2 = f.retain(&stale_scope);
    let _h3 = f.warm(&other_capability);
    let _h4 = f.warm(&unrelated);
    let _h5 = f.warm(&owner);

    let before_stale = f.node.routing_registry.base_facts_unvalidated(&stale_scope);
    let before_unrelated = f
        .node
        .org_routing_base_facts(&unrelated)
        .expect("unrelated warm");
    let before_owner = f.node.org_routing_base_facts(&owner).expect("owner warm");
    assert!(
        f.node.org_routing_base_facts(&other_capability).is_some(),
        "precondition: an uncovered capability under an installed Grant is \
         SERVED (empty), which is why it is treated as affected below"
    );

    // Move the exact installed scope.
    assert!(f.node.remove_consumer_grant_audience(&grant_id));

    assert!(
        f.node
            .routing_registry
            .base_facts_unvalidated(&moved)
            .is_none(),
        "precondition: the scope that actually moved IS retired"
    );
    assert!(
        f.node
            .org_routing_base_facts(&unrelated)
            .is_some_and(|live| Arc::ptr_eq(&live, &before_unrelated)),
        "an unrelated Grant keeps its EXACT artifact"
    );
    assert!(
        f.node
            .org_routing_base_facts(&owner)
            .is_some_and(|live| Arc::ptr_eq(&live, &before_owner)),
        "and the Owner plane is untouched"
    );
    match before_stale {
        Some(before) => assert!(
            f.node
                .routing_registry
                .base_facts_unvalidated(&stale_scope)
                .is_some_and(|live| Arc::ptr_eq(&live, &before)),
            "the SAME grant id under a rotated-away audience handle is a \
             different scope: this transition says nothing about it, so its \
             exact artifact must survive"
        ),
        None => panic!("precondition: the stale-handle slot must be retained"),
    }
    assert!(
        f.node
            .routing_registry
            .base_facts_unvalidated(&other_capability)
            .is_none(),
        "and the same scope under a different capability IS retired — see this \
         witness's doc: the source serves it, so the grant's movement affects \
         it. If the source is ever narrowed to refuse uncovered capabilities, \
         THIS assertion is the one that must fail and force the invalidation to \
         be narrowed with it"
    );
}

/// W-W8. A delayed INSTALL notification cannot retire the newer ABSENCE artifact
/// a later removal produced.
///
/// The mirror of W-W6, and the permutation W-W6 does not reach:
///
/// ```text
/// W-W6:  delayed removal N   -> install N+1  -> preserve the Grant-stamped N+1
/// W-W8:  delayed install  N  -> remove  N    -> preserve the Owner-stamped absence
/// ```
///
/// It was missed because of a comment I wrote and believed: "an `Unserved`
/// reconstruction names NO installation, so it cannot be the successor". That is
/// false. When the later state is ABSENCE, the `Unserved` artifact IS the exact
/// successor — and an ordering derived from `install_seq` cannot see it, because
/// an absence has no installation identity to be ordered by. Kyra's
/// production-path probe against `7348529fb` failed on precisely the pointer
/// identity asserted below.
///
/// The fence is therefore a consumer-Grant PUBLICATION generation, which orders
/// installs and removals alike, and every reconstruction carries the generation
/// it observed — including `Unserved` ones, which is the whole point.
///
/// Dies to ordering by `install_seq`, and to treating an `Owner`-stamped artifact
/// as never-a-successor.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_delayed_install_notification_cannot_retire_a_successor_removal_artifact() {
    use crate::adapter::net::behavior::org_routing_registry::SourceFacts;

    let f = grant_fixture("ww8").await;
    let (grant, secret) = f.mint("nrpc:ww8", Some([0xa8u8; 32]), None);
    let grant_id = grant.grant_id;
    let handle = secret.audience_handle;
    let key = f.key(grant_id, handle, "nrpc:ww8");

    f.node.start();
    assert!(until(|| f.node.org_routing_ready()).await, "healthy");
    let family = f.node.org_routing_family().expect("family");

    // Park the FIRST notification — the install's. The shared one-shot latch
    // rather than a predicate on the movement, for the same reason as W-W6: the
    // transitions are not reliably distinguishable by value, and the install's
    // notification is guaranteed first because nothing removes until `reached` is
    // observed. The captured fence is not read here, for the same reason it is
    // not in W-W6.
    let (reached_rx, release_tx, _parked_fence) = park_first_grant_notification(&f.node);

    let installer = {
        let node = f.node.clone();
        std::thread::spawn(move || {
            node.install_consumer_grant_audience(grant, secret)
                .expect("install")
        })
    };
    reached_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("the install must reach its notification and park there");

    // The install has PUBLISHED but has not yet notified. Demand the slot NOW:
    // first demand enqueues it and marks work on its own, which is the only
    // reason this slot can warm while the install's own wake is parked — and it
    // is also precisely the "a demand arriving after publication is safe" case
    // design §2A.2 calls out.
    let held = family.demand(key.clone()).expect("demand");
    assert!(
        until(|| f.node.org_routing_base_facts(&key).is_some()).await,
        "the slot must warm under the installed grant before it is removed"
    );

    // Now remove it. Its notification passes through the latch freely, clears the
    // artifact above, and the actor rebuilds the slot as a NEWER absence.
    assert!(
        f.node.remove_consumer_grant_audience(&grant_id),
        "precondition: the removal published"
    );
    assert!(
        until(|| {
            f.node
                .routing_registry
                .base_facts_unvalidated(&key)
                .is_some_and(|facts| matches!(facts.providers, SourceFacts::Unserved))
        })
        .await,
        "the removal must leave a newer UNSERVED artifact — retained, and read \
         cold"
    );
    let successor = f
        .node
        .routing_registry
        .base_facts_unvalidated(&key)
        .expect("successor absence artifact");
    let counts_before = f.node.org_routing_reconciliation_counts();

    // Release the obsolete INSTALL notification.
    let _ = release_tx.send(());
    installer.join().expect("installer thread");

    assert!(
        f.node
            .routing_registry
            .base_facts_unvalidated(&key)
            .is_some_and(|live| Arc::ptr_eq(&live, &successor)),
        "the obsolete INSTALL notification must preserve the exact newer \
         Unserved artifact produced by the later removal — an absence IS a \
         successor, and ordering by installation identity cannot see it"
    );
    let counts_after = f.node.org_routing_reconciliation_counts();
    assert_eq!(
        counts_after[2], counts_before[2],
        "and must invalidate nothing"
    );
    assert_eq!(
        counts_after[0], counts_before[0],
        "and must not have re-queued it either"
    );

    drop(held);
    let _ = f.node.shutdown().await;
}

/// Park the FIRST consumer-Grant notification and hand back the levers.
///
/// A one-shot latch rather than a predicate on the movement: successive
/// transitions are not reliably distinguishable by value, and every witness that
/// uses this arranges for the transition it cares about to be the first.
///
/// Returns `(reached, release, fence)` — wait on `reached` to know the
/// notification is parked (published, gate released, registry untouched), read
/// `fence` to see what the parked transition actually carries, then send on
/// `release` to let it proceed.
///
/// The fence is captured rather than inferred. A witness that arranges a
/// terminal withdrawal and then asserts only on artifacts is really asserting
/// about its own setup: if the transition turned out to be an ordinary
/// `Publication`, several of these witnesses would still pass while testing a
/// different cell of the fence matrix than the one they name
/// (Kyra, review of `010c718ea`).
fn park_first_grant_notification(
    node: &Arc<MeshNode>,
) -> (
    std::sync::mpsc::Receiver<()>,
    std::sync::mpsc::SyncSender<()>,
    Arc<parking_lot::Mutex<Option<GrantMovementFence>>>,
) {
    let (reached_tx, reached_rx) = std::sync::mpsc::sync_channel::<()>(1);
    let (release_tx, release_rx) = std::sync::mpsc::sync_channel::<()>(1);
    let release_rx = Arc::new(parking_lot::Mutex::new(release_rx));
    let parked = Arc::new(AtomicBool::new(false));
    let fence = Arc::new(parking_lot::Mutex::new(None));
    let seen = fence.clone();
    node.arm_grant_movement_hook(Arc::new(move |movement| {
        if parked.swap(true, Ordering::AcqRel) {
            return;
        }
        // Recorded BEFORE the park, so it is readable while the notification is
        // still held.
        *seen.lock() = Some(movement.fence);
        let _ = reached_tx.try_send(());
        release_rx
            .lock()
            .recv_timeout(Duration::from_secs(10))
            .expect("the witness must release the parked notification");
    }));
    (reached_rx, release_tx, fence)
}

/// W-W9. A delayed notification preserves the artifact from its OWN publication.
///
/// The equality arm, and it was unwitnessed: Kyra ran all 62 witnesses under the
/// inverse mutation `<` → `<=` and every one passed. W-W6 and W-W8 cover only
/// `artifact.publication > movement.publication`; neither says anything about
/// `==`.
///
/// The permutation is ordinary, not exotic — it is precisely the "a demand
/// arriving after publication is safe" case design §2A.2 names:
///
/// ```text
/// install P publishes
/// -> its notification parks
/// -> a demand arrives AFTER the publication
/// -> first demand warms the exact Grant artifact at P
/// -> notification P resumes  ->  must preserve its own artifact
/// ```
///
/// Under `<=` the notification destroys the artifact its own publication
/// produced and re-queues it: pure cold-path churn, self-inflicted, on every
/// install that races a demand.
///
/// Dies to `<` → `<=`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_delayed_install_notification_preserves_its_own_publication_artifact() {
    let f = grant_fixture("ww9").await;
    let (grant, secret) = f.mint("nrpc:ww9", Some([0xa9u8; 32]), None);
    let grant_id = grant.grant_id;
    let handle = secret.audience_handle;
    let key = f.key(grant_id, handle, "nrpc:ww9");

    f.node.start();
    assert!(until(|| f.node.org_routing_ready()).await, "healthy");
    let family = f.node.org_routing_family().expect("family");

    let (reached, release, parked_fence) = park_first_grant_notification(&f.node);
    let installer = {
        let node = f.node.clone();
        std::thread::spawn(move || {
            node.install_consumer_grant_audience(grant, secret)
                .expect("install")
        })
    };
    reached
        .recv_timeout(Duration::from_secs(10))
        .expect("the install must park at its notification");
    assert!(
        matches!(
            *parked_fence.lock(),
            Some(GrantMovementFence::Publication(_))
        ),
        "precondition: the parked transition is an ORDINARY publication — this \
         witness is about the equality arm, not the terminal row"
    );

    // Published, not yet notified. First demand enqueues the slot on its own, so
    // it warms under THIS publication.
    let held = family.demand(key.clone()).expect("demand");
    assert!(
        until(|| f.node.org_routing_base_facts(&key).is_some()).await,
        "the demand must warm the slot under the just-published grant"
    );
    let own = f.node.org_routing_base_facts(&key).expect("own artifact");
    let counts_before = f.node.org_routing_reconciliation_counts();

    let _ = release.send(());
    installer.join().expect("installer thread");

    assert!(
        f.node
            .routing_registry
            .base_facts_unvalidated(&key)
            .is_some_and(|live| Arc::ptr_eq(&live, &own)),
        "a notification must preserve the artifact its OWN publication produced \
         — the comparison is STRICTLY less than, and `<=` would have every \
         install that races a demand cold-path its own work"
    );
    let counts_after = f.node.org_routing_reconciliation_counts();
    assert_eq!(counts_after[2], counts_before[2], "and invalidate nothing");
    assert_eq!(counts_after[0], counts_before[0], "and re-queue nothing");

    drop(held);
    let _ = f.node.shutdown().await;
}

/// W-W10. The same equality arm for an ABSENCE artifact.
///
/// Separate from W-W9 deliberately. The premise that failed at `7348529fb` was
/// authority-state-SPECIFIC — it treated `Owner`-stamped artifacts as a class
/// that could never be a successor — so closing the equality arm only for
/// `Grant`-stamped artifacts would leave a future authority-specific branch free
/// to reopen exactly that gap. Both arms are pinned.
///
/// Dies to `<` → `<=`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_delayed_removal_notification_preserves_its_own_absence_artifact() {
    use crate::adapter::net::behavior::org_routing_registry::SourceFacts;

    let f = grant_fixture("ww10").await;
    let (grant, secret) = f.mint("nrpc:ww10", Some([0xaau8; 32]), None);
    let (grant_id, handle) = f.install(grant, secret);
    let key = f.key(grant_id, handle, "nrpc:ww10");

    f.node.start();
    assert!(until(|| f.node.org_routing_ready()).await, "healthy");
    let family = f.node.org_routing_family().expect("family");

    let (reached, release, parked_fence) = park_first_grant_notification(&f.node);
    let remover = {
        let node = f.node.clone();
        std::thread::spawn(move || node.remove_consumer_grant_audience(&grant_id))
    };
    reached
        .recv_timeout(Duration::from_secs(10))
        .expect("the removal must park at its notification");
    assert!(
        matches!(
            *parked_fence.lock(),
            Some(GrantMovementFence::Publication(_))
        ),
        "precondition: the parked withdrawal is an ORDINARY publication. W-W15 \
         is the same schedule at the LAST live identity, where the artifact \
         side is terminal instead"
    );

    // Absence is published. First demand enqueues and the slot reconstructs
    // UNSERVED under THIS publication.
    let held = family.demand(key.clone()).expect("demand");
    assert!(
        until(|| {
            f.node
                .routing_registry
                .base_facts_unvalidated(&key)
                .is_some_and(|facts| matches!(facts.providers, SourceFacts::Unserved))
        })
        .await,
        "the demand must reconstruct the slot as Unserved under the published \
         absence"
    );
    let own = f
        .node
        .routing_registry
        .base_facts_unvalidated(&key)
        .expect("own absence artifact");
    let counts_before = f.node.org_routing_reconciliation_counts();

    let _ = release.send(());
    assert!(
        remover.join().expect("remover thread"),
        "precondition: the removal published"
    );

    assert!(
        f.node
            .routing_registry
            .base_facts_unvalidated(&key)
            .is_some_and(|live| Arc::ptr_eq(&live, &own)),
        "a removal notification must preserve the ABSENCE artifact its own \
         publication produced, exactly as an install preserves its Grant one — \
         the ordering is over transitions, not over authority states"
    );
    let counts_after = f.node.org_routing_reconciliation_counts();
    assert_eq!(counts_after[2], counts_before[2], "and invalidate nothing");
    assert_eq!(counts_after[0], counts_before[0], "and re-queue nothing");

    drop(held);
    let _ = f.node.shutdown().await;
}

/// W-W11. A successful lease-conditional removal is a full transition.
///
/// W-W5 drives `remove_consumer_grant_audience_if_current` only on its STALE
/// branch, where nothing publishes. The successful branch is the one that
/// publishes absence and constructs a movement, and Kyra's review of
/// `91f1c2e11` noted that a branch-local omission there would escape every other
/// witness — all of which drive the unconditional surface.
///
/// Both surfaces now share one `withdraw_consumer_grant`, so the exposure is
/// structural rather than duplicated. The witness is kept anyway, because
/// "they share a helper today" is not a property a future edit preserves.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_current_lease_removal_publishes_wakes_and_fences() {
    use crate::adapter::net::behavior::org_grant_registry::ConsumerAudienceInstall;
    use crate::adapter::net::behavior::org_routing_registry::SourceFacts;

    let f = grant_fixture("ww11").await;
    let (grant, secret) = f.mint("nrpc:ww11", Some([0xabu8; 32]), None);
    let grant_id = grant.grant_id;
    let handle = secret.audience_handle;
    let lease = match f
        .node
        .install_consumer_grant_audience_leased(grant, secret)
        .expect("install")
    {
        ConsumerAudienceInstall::Installed(lease) => lease,
        ConsumerAudienceInstall::AlreadyPresent => panic!("a fresh install must yield a lease"),
    };
    let key = f.key(grant_id, handle, "nrpc:ww11");

    f.node.start();
    assert!(until(|| f.node.org_routing_ready()).await, "healthy");
    let family = f.node.org_routing_family().expect("family");
    let held = family.demand(key.clone()).expect("demand");
    assert!(
        until(|| f.node.org_routing_base_facts(&key).is_some()).await,
        "precondition: warm under the leased installation"
    );
    let movements_before = f.node.consumer_grant_movements_for_test();

    // The CURRENT lease: this is the branch W-W5 never reaches.
    assert!(
        f.node.remove_consumer_grant_audience_if_current(&lease),
        "a current lease must withdraw its own installation"
    );
    assert_eq!(
        f.node.consumer_grant_movements_for_test(),
        movements_before + 1,
        "and that withdrawal is routing movement — exactly one notification"
    );
    assert!(
        f.node
            .consumer_grant_audiences
            .load()
            .get(&grant_id)
            .is_none(),
        "and absence is published"
    );

    // Woken and rebuilt with no reader involved.
    assert!(
        until(|| {
            f.node
                .routing_registry
                .base_facts_unvalidated(&key)
                .is_some_and(|facts| matches!(facts.providers, SourceFacts::Unserved))
        })
        .await,
        "the conditional removal must wake routing and rebuild the slot as \
         Unserved, without a reader"
    );

    drop(held);
    let _ = f.node.shutdown().await;
}

/// W-W12. Publication-identity exhaustion refuses an INSTALL fail-closed, and
/// publishes nothing.
///
/// The counter that orders transitions is the same class of authority identity
/// as the installation counter, and this branch's rule for those is explicit:
/// never wrap, latch and fail closed. The first version broke it — an unchecked
/// `fetch_add(1) + 1` AFTER the snapshot store, which at the ceiling panicked in
/// debug and aliased to zero in release, with the new snapshot ALREADY VISIBLE
/// and no notification delivered. Kyra's deterministic probe against
/// `91f1c2e11` observed exactly that: `panicked=true generation=0
/// partially_published=true`.
///
/// A zero generation is the worst available alias: every retained artifact then
/// compares as newer, so nothing would ever be invalidated again.
///
/// Asserts the whole non-publishing outcome — typed refusal, no snapshot change,
/// not even a transient publication, no identity movement, no routing wake.
#[tokio::test]
async fn publication_exhaustion_refuses_an_install_without_publishing() {
    use crate::adapter::net::behavior::org_grant_registry::GrantAudienceInstallError;

    let f = grant_fixture("ww12").await;
    let (grant, secret) = f.mint("nrpc:ww12", Some([0xacu8; 32]), None);

    f.node.exhaust_consumer_grant_publications_for_test();
    let snapshot_before = f.node.consumer_grant_audiences.load_full();
    let identity_before = f.node.consumer_grant_publication_for_test();
    let movements_before = f.node.consumer_grant_movements_for_test();
    let published_before = f.node.consumer_grant_publications_for_test();

    let err = f
        .node
        .install_consumer_grant_audience(grant, secret)
        .expect_err("an exhausted publication space must refuse an install");
    assert_eq!(
        err,
        GrantAudienceInstallError::IdSpaceExhausted,
        "the refusal must be TYPED. It shares the public variant with installation-identity exhaustion deliberately: a NEW variant on that public enum would be a source-breaking API change, which scope item 16 excludes. The precise space is carried in the log, and the counter assertions below are what distinguish the two behaviourally"
    );

    assert!(
        Arc::ptr_eq(
            &snapshot_before,
            &f.node.consumer_grant_audiences.load_full()
        ),
        "NO PARTIAL PUBLICATION: the exact snapshot from before must still be \
         installed. The defect was that content became visible and only THEN \
         failed"
    );
    assert_eq!(
        f.node.consumer_grant_publications_for_test(),
        published_before,
        "and nothing was published even transiently"
    );
    assert_eq!(
        f.node.consumer_grant_publication_for_test(),
        identity_before,
        "and the identity did not advance, wrap, or saturate"
    );
    assert_ne!(
        f.node.consumer_grant_publication_for_test(),
        0,
        "least of all alias to zero, against which every retained artifact \
         compares as newer and nothing is ever invalidated again"
    );
    assert_eq!(
        f.node.consumer_grant_movements_for_test(),
        movements_before,
        "and a refusal is not routing movement"
    );
}

/// W-W13. Terminal withdrawal retires the artifact produced at the LAST LIVE
/// identity, and touches nothing else.
///
/// Two properties, and the first one is why this witness was rebuilt.
///
/// **Non-aliasing.** The dangerous boundary is the last live identity itself:
///
/// ```text
/// the final installation publishes at MAX-1
/// -> its Served artifact is stamped MAX-1
/// -> withdrawal cannot allocate another identity
/// -> an implementation that REUSED MAX-1 would compare MAX-1 < MAX-1 = false
/// -> the stale Served artifact survives its own withdrawal
/// ```
///
/// The earlier version of this witness warmed at publication 1 and only then
/// jumped the counter, so `1 < MAX-1` cleared even under the alias and it stayed
/// green. Kyra demonstrated that directly. The counter is therefore positioned
/// at `MAX-2` here so the installation genuinely commits `MAX-1` and the
/// artifact genuinely carries it.
///
/// **Scope exactness.** `Terminal` is the one fence that clears every
/// `Publication` artifact it selects without comparing generations, which makes
/// it the easiest place to be accidentally global. (It is not "clear
/// everything": it preserves `TerminalAbsence` — W-W14.) Three controls —
/// same grant id under another handle, an unrelated grant, and the Owner plane —
/// must keep their EXACT artifacts.
///
/// The same-id/other-handle control is **warmed BEFORE the counter is
/// positioned**, and that ordering is the whole reason it is evidence. Warmed
/// after, it reconstructs absent under a spent space and is stamped
/// `TerminalAbsence` — so a terminal predicate widened to `grant_id` alone would
/// select it, the fence would preserve it anyway, and the exact Arc, the
/// invalidation count and the requeue count would all be unchanged. The witness
/// stayed green against precisely the narrow, security-relevant mutation it
/// claimed to catch, because the artifact side silently rescued the selection
/// side (Kyra, review of `010c718ea`). Warmed first, it carries an ordinary
/// `Publication` and erroneous selection becomes observable.
///
/// **Capability-wide within the exact scope.** Two capabilities under the same
/// `(grant_id, audience_handle)` are demanded and both must be retired. W-W7
/// pins capability-wide selection for ORDINARY movement only, so a terminal-only
/// narrowing to a single capability would otherwise evade every witness.
///
/// Dies to reusing `Publication(MAX - 1)` in place of `Terminal`, to a
/// terminal-only widening of the scope predicate (including `grant_id`-only),
/// and to a terminal-only capability narrowing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn terminal_withdrawal_retires_the_last_live_identity_and_nothing_else() {
    use crate::adapter::net::behavior::org_routing_registry::{GrantArtifactFence, SourceFacts};

    let f = grant_fixture("ww13").await;

    // The unrelated control installs FIRST, while the identity space is still
    // live. After the installation under test there is none left — which is the
    // whole situation being tested, so the ordering is forced.
    let (grant_b, secret_b) = f.mint("nrpc:ww13-b", Some([0xaeu8; 32]), None);
    let (id_b, handle_b) = f.install(grant_b, secret_b);
    let unrelated = f.key(id_b, handle_b, "nrpc:ww13-b");
    let owner = slot(46, "nrpc:ww13-owner");

    // Minted but NOT yet installed: the scope keys have to exist before the
    // counter moves, so the same-id/other-handle control can be warmed while an
    // ordinary publication identity is still being handed out.
    let (grant, secret) = f.mint("nrpc:ww13", Some([0xadu8; 32]), None);
    let grant_id = grant.grant_id;
    let handle = secret.audience_handle;
    let key = f.key(grant_id, handle, "nrpc:ww13");
    // A SECOND capability under the EXACT same (grant_id, audience_handle).
    let key_alt = f.key(grant_id, handle, "nrpc:ww13-alt");
    let mut other_handle = handle;
    other_handle[0] ^= 0xff;
    let same_id_other_handle = f.key(grant_id, other_handle, "nrpc:ww13");

    f.node.start();
    assert!(until(|| f.node.org_routing_ready()).await, "healthy");
    let family = f.node.org_routing_family().expect("family");

    // --- the controls warm while the space is still LIVE ---------------------
    let held_h = family
        .demand(same_id_other_handle.clone())
        .expect("demand h");
    let held_b = family.demand(unrelated.clone()).expect("demand b");
    let held_o = family.demand(owner.clone()).expect("demand owner");
    assert!(
        until(|| {
            [&same_id_other_handle, &unrelated, &owner]
                .iter()
                .all(|k| f.node.routing_registry.base_facts_unvalidated(k).is_some())
        })
        .await,
        "precondition: every control holds an artifact"
    );
    let before_h = f
        .node
        .routing_registry
        .base_facts_unvalidated(&same_id_other_handle)
        .expect("h");
    assert!(
        matches!(before_h.grant_fence, GrantArtifactFence::Publication(_)),
        "precondition, and the one this witness was rebuilt for: the \
         same-id/other-handle control must carry an ORDINARY publication. \
         Stamped TerminalAbsence it would be preserved by the fence even when \
         wrongly selected, and a grant_id-only terminal predicate would pass \
         unnoticed"
    );

    // One short of spent: the NEXT reservation is the last live identity.
    f.node
        .set_consumer_grant_publications_for_test(u64::MAX - 2);
    let (grant_id_installed, handle_installed) = f.install(grant, secret);
    assert_eq!(
        (grant_id_installed, handle_installed),
        (grant_id, handle),
        "precondition: the installed record carries the scope the keys above \
         were built from"
    );
    assert_eq!(
        f.node.consumer_grant_publication_for_test(),
        u64::MAX - 1,
        "precondition: the installation commits the LAST live identity — the \
         whole point of this witness is that the artifact carries exactly the \
         value a broken terminal fence would reuse"
    );

    let held = family.demand(key.clone()).expect("demand");
    let held_alt = family.demand(key_alt.clone()).expect("demand alt");
    assert!(
        until(|| {
            [&key, &key_alt]
                .iter()
                .all(|k| f.node.routing_registry.base_facts_unvalidated(k).is_some())
        })
        .await,
        "precondition: both capabilities under the exact scope hold artifacts"
    );

    for (label, k) in [
        ("the granted capability", &key),
        ("a second capability under the same exact scope", &key_alt),
    ] {
        let warm = f
            .node
            .routing_registry
            .base_facts_unvalidated(k)
            .expect("warm");
        assert_eq!(
            warm.grant_fence,
            GrantArtifactFence::Publication(u64::MAX - 1),
            "{label}: must be stamped with the LAST LIVE identity, which is the \
             value a reusing implementation would compare against itself"
        );
    }
    let before_b = f
        .node
        .routing_registry
        .base_facts_unvalidated(&unrelated)
        .expect("b");
    let before_o = f
        .node
        .routing_registry
        .base_facts_unvalidated(&owner)
        .expect("owner");
    assert!(
        f.node
            .routing_registry
            .base_facts_unvalidated(&same_id_other_handle)
            .is_some_and(|live| Arc::ptr_eq(&live, &before_h)),
        "precondition: the INSTALL is exact too, so the control still holds the \
         ordinary-publication artifact it warmed with"
    );
    // Sampled once the warm-up has provably stopped moving, so every delta
    // below is attributable to the withdrawal alone.
    let counts_before = quiesced_counts(&f.node).await;

    // Withdrawal at exhaustion: no identity can be allocated, so the movement is
    // TERMINAL.
    assert!(
        f.node.remove_consumer_grant_audience(&grant_id),
        "an exhausted publication space must NOT block revocation"
    );
    assert_eq!(
        f.node.consumer_grant_publication_for_test(),
        u64::MAX - 1,
        "and must not advance past the last live value — `u64::MAX` stays \
         reserved as the terminal marker"
    );

    assert!(
        until(|| {
            [&key, &key_alt].iter().all(|k| {
                f.node
                    .routing_registry
                    .base_facts_unvalidated(k)
                    .is_some_and(|facts| matches!(facts.providers, SourceFacts::Unserved))
            })
        })
        .await,
        "the withdrawal must retire BOTH capability slots stamped MAX-1 and \
         rebuild them as Unserved. An implementation that reused \
         Publication(MAX-1) here would compare it against itself and leave the \
         stale Served artifacts in place; one narrowed to the grant's own \
         capability would leave the second slot Grant-stamped after removal"
    );

    // --- scope exactness: nothing else moved --------------------------------
    for (label, key, before) in [
        (
            "same grant id, other audience handle",
            &same_id_other_handle,
            &before_h,
        ),
        ("an unrelated grant", &unrelated, &before_b),
        ("the Owner plane", &owner, &before_o),
    ] {
        assert!(
            f.node
                .routing_registry
                .base_facts_unvalidated(key)
                .is_some_and(|live| Arc::ptr_eq(&live, before)),
            "{label}: must keep its EXACT artifact. `Terminal` skips the \
             generation comparison entirely, so it is the easiest fence to \
             widen by accident"
        );
    }

    // NO SPIN, and EXACT deltas.
    let settled = f.node.org_routing_reconciliation_counts();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let quiet = f.node.org_routing_reconciliation_counts();
    assert_eq!(
        quiet[0], settled[0],
        "the actor must be QUIESCENT after a terminal fence"
    );
    assert_eq!(
        quiet[2],
        counts_before[2] + 2,
        "EXACTLY the two capability slots under the exact scope were \
         invalidated. `>=` was the earlier assertion and it is vacuous — it \
         passes at zero, which is the retirement failure this witness exists to \
         catch, and it passes at five, which is the scope widening"
    );
    assert_eq!(
        quiet[0],
        counts_before[0] + 2,
        "and exactly those two were rebuilt — no control was re-queued"
    );

    drop(held);
    drop(held_alt);
    drop(held_h);
    drop(held_b);
    drop(held_o);
    let _ = f.node.shutdown().await;
}

/// W-W14. A terminal withdrawal preserves the ABSENCE artifact its own
/// publication produced.
///
/// The terminal counterpart of W-W9/W-W10. Terminal withdrawal clears
/// unconditionally by publication — which is right for everything that existed
/// BEFORE it — but the artifact reconstructed AFTER its publication is its own
/// successor and must survive:
///
/// ```text
/// terminal withdrawal publishes absence
/// -> its notification parks
/// -> a demand arrives and reconstructs the scope as Unserved
/// -> notification resumes  ->  must preserve that exact artifact
/// ```
///
/// It cannot resurrect authority — nothing can install after exhaustion — but it
/// would have a notification retire the artifact produced from its own
/// transition, which breaks same-publication preservation and the "a demand
/// arriving after publication is safe" rule just as surely as `<=` did.
///
/// This is why terminal absence is a distinct artifact fence rather than a
/// generation: at exhaustion the last live identity may equal the artifact's, so
/// no numeric comparison could tell "before this withdrawal" from "after" it.
///
/// Dies to `TerminalAbsence => true` — i.e. to `Terminal` clearing everything.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_delayed_terminal_notification_preserves_its_own_absence_artifact() {
    use crate::adapter::net::behavior::org_routing_registry::{GrantArtifactFence, SourceFacts};

    let f = grant_fixture("ww14").await;
    f.node
        .set_consumer_grant_publications_for_test(u64::MAX - 2);
    let (grant, secret) = f.mint("nrpc:ww14", Some([0xafu8; 32]), None);
    let (grant_id, handle) = f.install(grant, secret);
    let key = f.key(grant_id, handle, "nrpc:ww14");

    f.node.start();
    assert!(until(|| f.node.org_routing_ready()).await, "healthy");
    let family = f.node.org_routing_family().expect("family");

    let (reached, release, parked_fence) = park_first_grant_notification(&f.node);
    let remover = {
        let node = f.node.clone();
        std::thread::spawn(move || node.remove_consumer_grant_audience(&grant_id))
    };
    reached
        .recv_timeout(Duration::from_secs(10))
        .expect("the terminal withdrawal must park at its notification");
    assert_eq!(
        *parked_fence.lock(),
        Some(GrantMovementFence::Terminal),
        "precondition, asserted DIRECTLY rather than inferred from the setup: \
         the parked movement is the TERMINAL row of the matrix. Deriving it \
         from `set_consumer_grant_publications_for_test` plus an install would \
         let a reservation change silently turn this into an ordinary \
         publication, which W-W10 already covers"
    );

    // Absence is published and the space is spent. First demand enqueues, and the
    // scope reconstructs TERMINALLY absent.
    let held = family.demand(key.clone()).expect("demand");
    assert!(
        until(|| {
            f.node
                .routing_registry
                .base_facts_unvalidated(&key)
                .is_some_and(|facts| matches!(facts.providers, SourceFacts::Unserved))
        })
        .await,
        "the demand must reconstruct the scope as Unserved under the published \
         absence"
    );
    let own = f
        .node
        .routing_registry
        .base_facts_unvalidated(&key)
        .expect("own terminal absence artifact");
    assert_eq!(
        own.grant_fence,
        GrantArtifactFence::TerminalAbsence,
        "precondition: an absent Grant scope reconstructed under a SPENT identity \
         space is terminal — and it is a distinct fence, not a number, precisely \
         because no number could order it against the withdrawal that caused it"
    );
    let counts_before = f.node.org_routing_reconciliation_counts();

    let _ = release.send(());
    assert!(
        remover.join().expect("remover thread"),
        "precondition: the withdrawal published"
    );

    assert!(
        f.node
            .routing_registry
            .base_facts_unvalidated(&key)
            .is_some_and(|live| Arc::ptr_eq(&live, &own)),
        "a terminal withdrawal must preserve the absence artifact its OWN \
         publication produced — clearing everything unconditionally retires \
         current work that nothing owes"
    );
    let counts_after = f.node.org_routing_reconciliation_counts();
    assert_eq!(counts_after[2], counts_before[2], "and invalidate nothing");
    assert_eq!(counts_after[0], counts_before[0], "and re-queue nothing");

    drop(held);
    let _ = f.node.shutdown().await;
}

/// W-W15. The FINAL ORDINARY removal preserves the terminal absence it caused.
///
/// The fourth cell of the fence matrix, and the only one that was documented but
/// not mutation-pinned:
///
/// ```text
///                 artifact:  Publication(a)      TerminalAbsence
///  movement:
///  Publication(p)            clear iff a < p     preserve      <- HERE
///  Terminal                  clear               preserve
/// ```
///
/// It is reachable, and it is not a relabelling of any other witness. The
/// removal that SPENDS the space is an ordinary publication — it reserves the
/// last live identity, `MAX-1`, and commits it — so its notification carries
/// `Publication(MAX-1)`, not `Terminal`. But a demand landing after that
/// publication reconstructs the scope absent under a space that is now spent,
/// which is `TerminalAbsence`. W-W10 covers `Publication × Publication` at an
/// ordinary generation; W-W14 covers `Terminal × TerminalAbsence`. Neither
/// reaches an ordinary movement meeting a terminal artifact.
///
/// ```text
/// counter at MAX-2
/// -> remove publishes absence and commits MAX-1   (the last ordinary publication)
/// -> its notification parks
/// -> a demand arrives, reconstructs Unserved, space spent -> TerminalAbsence
/// -> notification Publication(MAX-1) resumes  ->  must preserve that artifact
/// ```
///
/// Kyra found this by running the committed gate under the inverse mutation:
/// `(TerminalAbsence, Publication(_)) => false` changed to `=> true`, and all 68
/// witnesses passed. A temporary production-path probe of this schedule then
/// went RED under the mutation and GREEN on restore (review of `010c718ea`).
///
/// Dies to `(TerminalAbsence, Publication(_))` preserving → clearing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_final_ordinary_removal_preserves_the_terminal_absence_it_caused() {
    use crate::adapter::net::behavior::org_routing_registry::{GrantArtifactFence, SourceFacts};

    let f = grant_fixture("ww15").await;
    let (grant, secret) = f.mint("nrpc:ww15", Some([0xb0u8; 32]), None);
    let (grant_id, handle) = f.install(grant, secret);
    let key = f.key(grant_id, handle, "nrpc:ww15");

    // Positioned AFTER the install, so it is the REMOVAL that takes the last
    // live identity. That is what makes this an ordinary movement against a
    // terminal artifact rather than W-W14's terminal movement.
    f.node
        .set_consumer_grant_publications_for_test(u64::MAX - 2);

    f.node.start();
    assert!(until(|| f.node.org_routing_ready()).await, "healthy");
    let family = f.node.org_routing_family().expect("family");

    let (reached, release, parked_fence) = park_first_grant_notification(&f.node);
    let remover = {
        let node = f.node.clone();
        std::thread::spawn(move || node.remove_consumer_grant_audience(&grant_id))
    };
    reached
        .recv_timeout(Duration::from_secs(10))
        .expect("the final ordinary removal must park at its notification");
    assert_eq!(
        *parked_fence.lock(),
        Some(GrantMovementFence::Publication(u64::MAX - 1)),
        "precondition, and the distinction this witness rests on: the parked \
         movement is an ORDINARY publication at the last live identity, NOT \
         Terminal. Withdrawal reserves before it publishes, and MAX-1 was still \
         available"
    );
    assert_eq!(
        f.node.consumer_grant_publication_for_test(),
        u64::MAX - 1,
        "and the space is now spent: the next reservation would land on the \
         reserved terminal marker"
    );

    // Absence is published and the space is spent. First demand enqueues, and
    // the scope reconstructs TERMINALLY absent.
    let held = family.demand(key.clone()).expect("demand");
    assert!(
        until(|| {
            f.node
                .routing_registry
                .base_facts_unvalidated(&key)
                .is_some_and(|facts| matches!(facts.providers, SourceFacts::Unserved))
        })
        .await,
        "the demand must reconstruct the scope as Unserved under the published \
         absence"
    );
    let own = f
        .node
        .routing_registry
        .base_facts_unvalidated(&key)
        .expect("own terminal absence artifact");
    assert_eq!(
        own.grant_fence,
        GrantArtifactFence::TerminalAbsence,
        "precondition: reconstructed absent under a SPENT space, so the artifact \
         is terminal even though the movement that caused it was ordinary"
    );
    let counts_before = f.node.org_routing_reconciliation_counts();

    let _ = release.send(());
    assert!(
        remover.join().expect("remover thread"),
        "precondition: the removal published"
    );

    assert!(
        f.node
            .routing_registry
            .base_facts_unvalidated(&key)
            .is_some_and(|live| Arc::ptr_eq(&live, &own)),
        "an ordinary notification must preserve the TERMINAL absence its own \
         publication produced. Nothing can supersede that artifact — no \
         installation can follow a spent space — so clearing it is pure \
         self-inflicted churn on a scope that can never be served again"
    );
    let counts_after = f.node.org_routing_reconciliation_counts();
    assert_eq!(counts_after[2], counts_before[2], "and invalidate nothing");
    assert_eq!(counts_after[0], counts_before[0], "and re-queue nothing");

    drop(held);
    let _ = f.node.shutdown().await;
}

/// W-W16. Terminal withdrawals are SEQUENTIAL, not a global state change.
///
/// Exhaustion is a property of the identity space; terminal absence is a
/// property of ONE scope's reconstruction. That distinction is the entire reason
/// `TerminalAbsence` lives on the artifact instead of being a global
/// `publication == u64::MAX` marker, and it only has teeth if a second grant can
/// still be withdrawn AFTER the first has gone terminal:
///
/// ```text
/// A and B both installed, space then spent
/// -> A withdraws terminally      A: TerminalAbsence   B: Publication(_)  <- still
/// -> B withdraws terminally      B's Served artifact cleared, B: TerminalAbsence
///                                A's terminal absence untouched
/// ```
///
/// The assertion that B is still `Publication` before its own withdrawal is the
/// load-bearing one. A regression that terminalized every artifact once the
/// space was spent would leave B stamped `TerminalAbsence` while B is still
/// SERVED — and B's own later withdrawal would then be unable to clear it,
/// leaving a stale Served artifact for a grant that is gone. That is authority
/// resurrection, reached by a route no single-grant witness can see.
///
/// Dies to stamping `TerminalAbsence` on served scopes under a spent space, and
/// to `Terminal` movement selecting scopes outside its exact `(grant_id,
/// audience_handle)`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_second_installed_grant_still_withdraws_terminally_after_the_first() {
    use crate::adapter::net::behavior::org_routing_registry::{GrantArtifactFence, SourceFacts};

    let f = grant_fixture("ww16").await;
    let (grant_a, secret_a) = f.mint("nrpc:ww16-a", Some([0xb1u8; 32]), None);
    let (id_a, handle_a) = f.install(grant_a, secret_a);
    let (grant_b, secret_b) = f.mint("nrpc:ww16-b", Some([0xb2u8; 32]), None);
    let (id_b, handle_b) = f.install(grant_b, secret_b);
    let key_a = f.key(id_a, handle_a, "nrpc:ww16-a");
    let key_b = f.key(id_b, handle_b, "nrpc:ww16-b");

    // Both installed, THEN the space is spent: no further installation is
    // possible and both withdrawals below are terminal.
    f.node.exhaust_consumer_grant_publications_for_test();

    f.node.start();
    assert!(until(|| f.node.org_routing_ready()).await, "healthy");
    let family = f.node.org_routing_family().expect("family");
    let held_a = family.demand(key_a.clone()).expect("demand a");
    let held_b = family.demand(key_b.clone()).expect("demand b");
    assert!(
        until(|| {
            [&key_a, &key_b].iter().all(|k| {
                f.node
                    .routing_registry
                    .base_facts_unvalidated(k)
                    .is_some_and(|facts| matches!(facts.providers, SourceFacts::Served(_)))
            })
        })
        .await,
        "precondition: both grants are installed and both scopes are Served"
    );
    for (label, k) in [("A", &key_a), ("B", &key_b)] {
        let facts = f
            .node
            .routing_registry
            .base_facts_unvalidated(k)
            .expect("warm");
        assert!(
            matches!(facts.grant_fence, GrantArtifactFence::Publication(_)),
            "{label}: a scope with an INSTALLED grant carries an ordinary \
             publication even though the identity space is spent. Exhaustion is \
             a property of the space; terminal absence is a property of a \
             reconstruction that found nothing"
        );
    }
    let before_b = f
        .node
        .routing_registry
        .base_facts_unvalidated(&key_b)
        .expect("b");

    // --- A withdraws terminally ---------------------------------------------
    assert!(
        f.node.remove_consumer_grant_audience(&id_a),
        "an exhausted space must not block revocation"
    );
    assert!(
        until(|| {
            f.node
                .routing_registry
                .base_facts_unvalidated(&key_a)
                .is_some_and(|facts| matches!(facts.providers, SourceFacts::Unserved))
        })
        .await,
        "A's Served artifact must be retired and rebuilt absent"
    );
    let a_absent = f
        .node
        .routing_registry
        .base_facts_unvalidated(&key_a)
        .expect("a");
    assert_eq!(
        a_absent.grant_fence,
        GrantArtifactFence::TerminalAbsence,
        "A reconstructed absent under a spent space, so its absence is terminal"
    );
    assert!(
        f.node
            .routing_registry
            .base_facts_unvalidated(&key_b)
            .is_some_and(|live| Arc::ptr_eq(&live, &before_b)),
        "B keeps its EXACT artifact through A's terminal withdrawal"
    );
    assert!(
        matches!(
            f.node
                .routing_registry
                .base_facts_unvalidated(&key_b)
                .expect("b")
                .grant_fence,
            GrantArtifactFence::Publication(_)
        ),
        "and it is still an ORDINARY publication. Stamped TerminalAbsence here, \
         B's own withdrawal below could not clear it — a grant that is gone \
         would keep a Served artifact"
    );

    // --- B withdraws terminally, afterwards ---------------------------------
    assert!(
        f.node.remove_consumer_grant_audience(&id_b),
        "a second terminal withdrawal must also be possible"
    );
    assert!(
        until(|| {
            f.node
                .routing_registry
                .base_facts_unvalidated(&key_b)
                .is_some_and(|facts| matches!(facts.providers, SourceFacts::Unserved))
        })
        .await,
        "B's Served artifact must be retired by B's OWN terminal withdrawal"
    );
    assert_eq!(
        f.node
            .routing_registry
            .base_facts_unvalidated(&key_b)
            .expect("b")
            .grant_fence,
        GrantArtifactFence::TerminalAbsence,
        "and B rebuilds terminally absent in its turn"
    );
    assert!(
        f.node
            .routing_registry
            .base_facts_unvalidated(&key_a)
            .is_some_and(|live| Arc::ptr_eq(&live, &a_absent)),
        "while A's terminal absence is untouched by it"
    );

    drop(held_a);
    drop(held_b);
    let _ = f.node.shutdown().await;
}

// ---------------------------------------------------------------------------
// OLB-2B.3c step 2 — the session/direct-state projection and the actor build
// cycle, driven through the PRODUCTION node.
// ---------------------------------------------------------------------------

/// A completed Noise handshake's initiator-side session keys.
fn fresh_session_keys() -> crate::adapter::net::crypto::SessionKeys {
    use crate::adapter::net::crypto::{NoiseHandshake, StaticKeypair};
    let psk = [0x42u8; 32];
    let responder_kp = StaticKeypair::generate();
    let mut initiator =
        NoiseHandshake::initiator(&psk, &responder_kp.public).expect("noise initiator");
    let mut responder = NoiseHandshake::responder(&psk, &responder_kp).expect("noise responder");
    let msg1 = initiator.write_message(&[]).expect("msg1");
    responder.read_message(&msg1).expect("read msg1");
    let msg2 = responder.write_message(&[]).expect("msg2");
    initiator.read_message(&msg2).expect("read msg2");
    initiator.into_session_keys().expect("session keys")
}

/// Install a live peer session directly into the node's peer table.
///
/// The production installers reach it through `install_peer_transition`, which
/// needs a completed handshake AND a socket; these witnesses are about what the
/// PROJECTION makes of the peer table, so they seed it and then drive the same
/// republication every production transition drives. The witness that asserts
/// on the TRANSITION itself uses `install_direct`, the real path.
fn seed_peer(node: &MeshNode, node_id: u64, direct: bool) -> u64 {
    let keys = fresh_session_keys();

    let addr: SocketAddr = format!("10.9.0.{}:9000", (node_id % 250) + 1)
        .parse()
        .expect("addr");
    let session = Arc::new(NetSession::new(keys, addr, 4, false));
    let session_id = session.session_id();
    node.peers.insert(
        node_id,
        PeerInfo {
            node_id,
            transport: if direct {
                PeerTransport::Direct { owned_addr: addr }
            } else {
                PeerTransport::Routed {
                    relay_addr: addr,
                    adjacent_relay_identity: None,
                }
            },
            session,
            remote_static_pub: [0u8; 32],
            last_initiator_ephemeral: None,
        },
    );
    session_id
}

/// The eligibility the production projection currently reports for `entity`.
fn eligibility_of(
    node: &MeshNode,
    entity: &EntityId,
) -> crate::adapter::net::behavior::org_routing_registry::DirectEligibility {
    use crate::adapter::net::behavior::org_routing_registry::SessionEligibility;
    node.session_routing.published.load().eligibility(entity)
}

/// (S2-1) The production join: a pinned entity with ONE live session is
/// annotated with that session's identity and its directness, and an entity
/// with no session is COLD.
///
/// The control is the third entity: without it, an implementation that
/// annotates nothing at all passes every assertion about the first — the
/// "comparison that never fails" shape this process has been rescued from
/// twice.
#[tokio::test]
async fn a_live_peer_session_annotates_the_provider_it_resolves_to() {
    use crate::adapter::net::behavior::org_routing_registry::DirectEligibility;
    let node = node().await;
    let direct_entity = EntityId::from_bytes([0x21u8; 32]);
    let relayed_entity = EntityId::from_bytes([0x22u8; 32]);
    let sessionless = EntityId::from_bytes([0x23u8; 32]);

    let direct_session = seed_peer(&node, 0x2001, true);
    let relayed_session = seed_peer(&node, 0x2002, false);
    node.test_pin_peer_entity(0x2001, direct_entity.clone());
    node.test_pin_peer_entity(0x2002, relayed_entity.clone());

    assert_eq!(
        eligibility_of(&node, &direct_entity),
        DirectEligibility::Direct {
            node_id: 0x2001,
            session_id: direct_session,
        },
        "an adjacent session is DIRECT and names the exact session it is"
    );
    assert_eq!(
        eligibility_of(&node, &relayed_entity),
        DirectEligibility::Relayed {
            node_id: 0x2002,
            session_id: relayed_session,
        },
        "a relayed session is annotated, and annotated as relayed — directness \
         is a property of the transport, not of having a session at all"
    );
    assert_eq!(
        eligibility_of(&node, &sessionless),
        DirectEligibility::Cold,
        "and an entity with no session is COLD"
    );
}

/// (S2-2) An entity claimed by TWO live sessions is never guessed.
///
/// There is no answer to "which session would a call use", and picking one
/// would make a routing decision out of a map iteration order. Absence — which
/// reads back as `Cold` — is the only honest answer.
///
/// Dies to: last-write-wins on the entity key, which every other projection
/// witness survives.
#[tokio::test]
async fn an_entity_claimed_by_two_live_sessions_is_never_annotated() {
    use crate::adapter::net::behavior::org_routing_registry::DirectEligibility;
    let node = node().await;
    let contested = EntityId::from_bytes([0x31u8; 32]);
    let clean = EntityId::from_bytes([0x32u8; 32]);

    seed_peer(&node, 0x3001, true);
    seed_peer(&node, 0x3002, true);
    seed_peer(&node, 0x3003, true);
    node.test_pin_peer_entity(0x3001, contested.clone());
    node.test_pin_peer_entity(0x3002, contested.clone());
    // The control: an entity with exactly one claimant in the SAME
    // republication must still be annotated, or "annotate nothing when
    // anything is ambiguous" would pass.
    node.test_pin_peer_entity(0x3003, clean.clone());

    assert_eq!(
        eligibility_of(&node, &contested),
        DirectEligibility::Cold,
        "two live sessions claim this provider: there is no determinate \
         session, so there is no annotation"
    );
    assert!(
        matches!(
            eligibility_of(&node, &clean),
            DirectEligibility::Direct {
                node_id: 0x3003,
                ..
            }
        ),
        "while an unambiguous provider in the same projection is annotated"
    );
}

/// (S2-3) A pinned entity whose node has NO live session is cold, and a live
/// session with no pinned entity annotates nothing.
///
/// The `NodeId -> EntityId` map is mutable and is consulted only while the
/// projection is built. A row that survived without a live peer record would
/// be attaching a session to a provider on the strength of a mapping alone.
#[tokio::test]
async fn a_pin_without_a_live_session_annotates_nothing() {
    use crate::adapter::net::behavior::org_routing_registry::DirectEligibility;
    let node = node().await;
    let orphan = EntityId::from_bytes([0x41u8; 32]);
    node.test_pin_peer_entity(0x4001, orphan.clone());
    assert_eq!(
        eligibility_of(&node, &orphan),
        DirectEligibility::Cold,
        "a mapping is not a session"
    );

    // And the inverse: a live session whose node id is unpinned contributes no
    // row at all, because there is no provider identity to key one by.
    seed_peer(&node, 0x4002, true);
    node.note_session_transition();
    assert_eq!(
        node.session_routing.published.load().rows.len(),
        0,
        "an unpinned session names no provider, so it annotates none"
    );
}

/// (S2-4) A transition republishes the projection and advances the generation
/// as ONE unit, and an idempotent pin cannot change what the projection says.
///
/// The generation lives inside the published value, so a witness can assert
/// they moved together rather than inferring it.
#[tokio::test]
async fn a_republication_moves_the_projection_and_its_generation_together() {
    let node = node().await;
    let entity = EntityId::from_bytes([0x51u8; 32]);
    seed_peer(&node, 0x5001, true);

    let before = node.org_routing_session_publications();
    node.test_pin_peer_entity(0x5001, entity.clone());
    let after_pin = node.org_routing_session_publications();
    assert_eq!(after_pin, before + 1, "a real pin publishes exactly once");
    let published = node.session_routing.published.load_full();
    assert_eq!(
        Some(published.generation),
        node.org_routing_session_generation(),
        "the generation names the projection that is actually visible"
    );
    assert_eq!(published.rows.len(), 1);

    // A repeat pin for the SAME binding is first-write-wins: whatever the
    // republication does, it cannot change what the projection SAYS.
    node.test_pin_peer_entity(0x5001, entity.clone());
    let republished = node.session_routing.published.load_full();
    assert_eq!(
        republished.rows, published.rows,
        "an idempotent pin cannot change the projection's content"
    );
    assert!(
        republished.generation >= published.generation,
        "and the generation is monotone across it"
    );
}

/// (S2-5) **End to end.** The production actor publishes a pool beside the
/// facts it derived it from; a peer transition then retires exactly the pool,
/// preserves the facts, re-queues the slot, and the actor republishes the pool
/// under the NEW session generation.
///
/// This is the whole build cycle against the real node: real source, real
/// commit pin, real supervisor, real session projection.
#[tokio::test]
async fn a_session_transition_retires_the_pool_and_the_actor_republishes_it() {
    let node = node().await;
    node.start();
    assert!(
        until(|| node.org_routing_ready()).await,
        "precondition: the actor must reach Healthy before anything is demanded"
    );

    let key = slot(60, "nrpc:s2e2e");
    let family = node.org_routing_family().expect("family");
    let held = family.demand(key.clone()).expect("demand");
    assert!(
        until(|| node
            .routing_registry
            .unsensed_pool_unvalidated(&key)
            .is_some())
        .await,
        "the production actor publishes a pool beside the facts it built"
    );

    let facts = node.org_routing_base_facts(&key).expect("facts");
    let pool = node
        .org_routing_unsensed_pool(&key)
        .expect("validated pool");
    assert!(
        pool.derives_from(&facts),
        "the validated read proves the pairing rather than assuming it"
    );
    let generation = pool.session_generation();
    assert_eq!(
        Some(generation),
        node.org_routing_session_generation(),
        "and the pool was computed under the LIVE session view"
    );

    // Baseline at a PROVEN-IDLE point, so the deltas below are the
    // transition's own work and not a warm-up pass's (HOLD item 4).
    let _ = quiesced_counts(&node).await;
    let before = node.org_routing_pool_counts();

    // A real session transition.
    seed_peer(&node, 0x6001, true);
    node.test_pin_peer_entity(0x6001, EntityId::from_bytes([0x61u8; 32]));
    let moved = node
        .org_routing_session_generation()
        .expect("a live generation");
    assert!(moved > generation, "the transition advanced the generation");

    assert!(
        until(|| node
            .routing_registry
            .unsensed_pool_unvalidated(&key)
            .is_some_and(|p| p.session_generation() == moved))
        .await,
        "the actor republishes the pool under the new session view"
    );
    assert!(
        node.org_routing_base_facts(&key).is_some(),
        "and the discovery facts were preserved throughout — session movement \
         is not a reason to re-reconstruct the source"
    );
    // EXACT deltas, at another proven-idle point. `>=` would have accepted a
    // transition that churned every retained pool on the node -- the breadth
    // failure the pool-only invalidator is scoped to avoid, and precisely the
    // assertion an over-broad implementation passes.
    let _ = quiesced_counts(&node).await;
    let after = node.org_routing_pool_counts();
    assert_eq!(
        after[1] - before[1],
        1,
        "the transition retired EXACTLY the one superseded pool this node holds"
    );
    assert_eq!(
        after[0] - before[0],
        1,
        "and the actor republished EXACTLY one pool for it"
    );
    assert_eq!(
        after[2] - before[2],
        0,
        "with no build refused: the rebuild ran after the projection settled"
    );

    drop(held);
    let _ = node.shutdown().await;
}

/// (S2-6) The VALIDATED pool read refuses a pool that does not name the facts
/// beside it, AND a pool computed under a superseded session view.
///
/// Both are cold, and neither invalidates anything: the facts are valid and
/// the rebuild is already owed. The two arms are asserted separately because
/// each hides a distinct defect — the publication order makes a mismatched
/// pair unreachable in production, while a superseded view is the ORDINARY
/// state of a pool between a session transition and the rebuild that follows
/// it.
#[tokio::test]
async fn the_validated_pool_read_refuses_a_mispaired_or_superseded_pool() {
    let node = node().await;
    node.start();
    assert!(until(|| node.org_routing_ready()).await, "precondition");

    // One session transition BEFORE anything warms, so the live generation is
    // past the one `for_test` stamps. Without it the superseded arm would
    // compare two zeroes and pass on a coincidence.
    seed_peer(&node, 0x6101, true);
    node.test_pin_peer_entity(0x6101, EntityId::from_bytes([0x62u8; 32]));

    let key = slot(61, "nrpc:s2read");
    let family = node.org_routing_family().expect("family");
    let held = family.demand(key.clone()).expect("demand");
    assert!(
        until(|| node.org_routing_unsensed_pool(&key).is_some()).await,
        "precondition: a validated pool must exist to be refused"
    );
    let facts = node.org_routing_base_facts(&key).expect("facts");

    // A pool naming a DIFFERENT artifact with identical content: pointer
    // identity is the artifact's identity, so a content comparison would
    // accept this and a pointer comparison must not.
    //
    // Built at the LIVE session generation, deliberately. Hand it generation 0
    // and the session check refuses it as well, so dropping the PAIRING check
    // leaves this arm green — which is precisely what mutation M-Z18 found
    // (design §18.0c). At the live generation the pairing check is the only
    // thing that can refuse it, which is what makes this arm evidence.
    let live_generation = node
        .org_routing_session_generation()
        .expect("a live generation");
    let foreign = Arc::new(
        crate::adapter::net::behavior::org_routing_registry::ScopedUnsensedRoutePool::for_test_at_session(
            Arc::new((*facts).clone()),
            live_generation,
        ),
    );
    assert!(
        !foreign.derives_from(&facts),
        "precondition: this pool names a DIFFERENT artifact…"
    );
    assert_eq!(
        foreign.session_generation(),
        live_generation,
        "…while carrying the live session view, so only the pairing check can \
         refuse it"
    );
    node.routing_registry
        .install_unsensed_pool_for_test(&key, foreign);
    assert!(
        node.org_routing_unsensed_pool(&key).is_none(),
        "a pool that does not name the published facts is COLD"
    );
    assert!(
        node.org_routing_base_facts(&key).is_some(),
        "and refusing it invalidates nothing — the facts are current"
    );

    // The SUPERSEDED-view arm. This pool names the EXACT published facts, so
    // the pairing check passes; what it does not carry is the live session
    // generation, and a pool whose annotations describe sessions that may be
    // gone is not usable.
    let superseded = Arc::new(
        crate::adapter::net::behavior::org_routing_registry::ScopedUnsensedRoutePool::for_test(
            facts.clone(),
        ),
    );
    assert!(
        superseded.derives_from(&facts),
        "precondition: this pool DOES name the published facts, so only the \
         session check can refuse it"
    );
    assert_ne!(
        Some(superseded.session_generation()),
        node.org_routing_session_generation(),
        "precondition: and it was computed under an older view"
    );
    node.routing_registry
        .install_unsensed_pool_for_test(&key, superseded);
    assert!(
        node.org_routing_unsensed_pool(&key).is_none(),
        "a pool from a superseded session view is COLD"
    );
    assert!(
        node.org_routing_base_facts(&key).is_some(),
        "and refusing THAT invalidates nothing either"
    );

    drop(held);
    let _ = node.shutdown().await;
}

/// (S2-7) A terminally exhausted session-generation space fences the pool
/// plane: nothing publishes, every retained pool is retired once, the
/// validated read is cold, and the actor does not spin.
///
/// Fail-closed and irreversible, on the same discipline as the routing
/// authority epoch and the consumer-Grant publication identity — a wrapped
/// generation would let a pool built under one view compare current against an
/// unrelated later one.
#[tokio::test]
async fn an_exhausted_session_generation_fences_the_pool_plane() {
    let node = node().await;
    node.start();
    assert!(until(|| node.org_routing_ready()).await, "precondition");

    let key = slot(62, "nrpc:s2spent");
    let family = node.org_routing_family().expect("family");
    let held = family.demand(key.clone()).expect("demand");
    assert!(
        until(|| node.org_routing_unsensed_pool(&key).is_some()).await,
        "precondition: a pool must exist for the exhaustion to retire"
    );

    // Position the counter so the NEXT reservation would reach the reserved
    // terminal marker, then drive one real transition.
    node.session_routing.currentness.set_for_test(u64::MAX - 1);
    let publications_before = node.org_routing_session_publications();
    let retired_before = node.org_routing_pool_counts()[1];
    seed_peer(&node, 0x7001, true);
    node.test_pin_peer_entity(0x7001, EntityId::from_bytes([0x71u8; 32]));

    assert_eq!(
        node.org_routing_session_generation(),
        None,
        "the space is terminally spent"
    );
    // RESERVE PRECEDES THE STORE, so a transition that cannot allocate a
    // generation publishes NOTHING — the same discipline the consumer-Grant
    // publication identity carries (W-W12).
    //
    // This assertion is what makes the CHECKED reservation observable at all.
    // Without it the witness passes against a `reserve` that wraps: at
    // `MAX - 1` a wrapping add lands exactly on the reserved terminal marker,
    // `generation()` maps that marker to `None` anyway, and every other
    // assertion here — fenced plane, pools retired, no spin — holds
    // identically. The difference is that the wrapping version has already
    // STORED a projection stamped with the marker and counted a publication
    // for it, which is a partial publication of a transition that was refused.
    // Mutation M-Z14 survived on exactly that gap (design §18.0c).
    assert_eq!(
        node.org_routing_session_publications(),
        publications_before,
        "a transition that cannot allocate a generation must publish no \
         projection at all"
    );
    assert!(
        until(|| node
            .routing_registry
            .unsensed_pool_unvalidated(&key)
            .is_none())
        .await,
        "every retained pool is retired once, rather than left published under \
         a view nothing can witness"
    );
    // "retires ONCE" as an EXACT delta, not as eventual absence (HOLD item 4).
    // Absence alone is also produced by retiring the same pool again on every
    // subsequent pass, which is the spin this arm exists to exclude.
    let _ = quiesced_counts(&node).await;
    assert_eq!(
        node.org_routing_pool_counts()[1] - retired_before,
        1,
        "exactly ONE retirement for the one pool this node held; a spinning \
         terminal arm would keep counting them"
    );
    assert!(
        node.org_routing_unsensed_pool(&key).is_none(),
        "and the validated read is cold"
    );
    assert!(
        until(|| node.org_routing_base_facts(&key).is_some()).await,
        "discovery is unaffected: the facts plane keeps reconciling"
    );

    // No spin. The evidence is an ITERATION BOUNDARY with nothing owed on the
    // far side of it: `quiesced_counts` drives two complete passes and panics
    // if the plane never settles, which is exactly what a self-sustaining wake
    // would produce.
    let published_before = node.org_routing_pool_counts()[0];
    let _ = quiesced_counts(&node).await;
    let (_retained, pending) = node.org_routing_slots();
    assert_eq!(
        pending, 0,
        "the actor parks rather than spinning on a plane it can never publish"
    );
    assert_eq!(
        node.org_routing_pool_counts()[0],
        published_before,
        "and nothing is published under a spent generation"
    );

    drop(held);
    let _ = node.shutdown().await;
}

/// (S2-8) A peer transition that publishes NOTHING publishes no session
/// generation, through the real installer.
///
/// `install_direct` with an `expected_prior_session_id` that is not the live
/// incarnation is a lost compare-and-swap: it mutates no peer state at all.
/// The republication is gated on that outcome, so nothing moves — no
/// generation, no projection, no pool invalidation, no actor wake.
///
/// Dies to: republishing unconditionally at the transition site. That mutation
/// leaves every other witness in this file green while making every refused
/// rotation, every replay drop and every lost upgrade churn the pool plane of
/// the whole node.
#[tokio::test]
async fn a_lost_peer_install_publishes_no_session_generation() {
    let node = node().await;
    let addr: SocketAddr = "10.9.9.1:9000".parse().expect("addr");
    let other: SocketAddr = "10.9.9.2:9000".parse().expect("addr");

    // The control, first: a transition that DOES own its peer record
    // republishes exactly once. Without it, "never republish" passes.
    let before = node.org_routing_session_publications();
    let installed = node.install_direct(0x8001, addr, fresh_session_keys(), None);
    assert!(
        installed.owned,
        "precondition: this transition owned the peer"
    );
    assert_eq!(
        node.org_routing_session_publications(),
        before + 1,
        "a transition that published a session publishes a generation"
    );
    let generation = node.org_routing_session_generation();
    let published = node.org_routing_session_publications();

    // A compare-and-swap against a session that is not the live one. The
    // installer bails before touching `peers`, so there is no transition to
    // notify anyone about.
    let lost = node.install_direct(
        0x8001,
        other,
        fresh_session_keys(),
        Some(0xdead_beef_dead_beef),
    );
    assert!(!lost.owned, "precondition: the compare-and-swap must lose");
    assert_eq!(
        node.org_routing_session_publications(),
        published,
        "a non-publishing transition publishes no session generation"
    );
    assert_eq!(
        node.org_routing_session_generation(),
        generation,
        "and the live generation does not move"
    );
}

/// (S2-9) **HOLD item 1 — the sample→revalidate interleaving.** A peer
/// replacement landing INSIDE the projection build cannot make that build
/// publish a generation naming the session it replaced.
///
/// The build samples peer `N`'s session `S0`, is paused there, `S1` is
/// installed over it, and the build is released. The `NodeId → EntityId`
/// mapping never moves, so the identity half of the stability check passes —
/// which is exactly why the INCARNATION half has to exist. Without it the
/// build publishes a fresh generation whose row names `S0`, and a 2B.3d
/// consumer preselects a session that is already gone.
///
/// Dies to: dropping the incarnation re-read (session id or directness) from
/// `build_session_rows`. The seven other projection witnesses all survive that
/// mutation, because none of them replaces a peer mid-build.
#[tokio::test]
async fn a_peer_replaced_mid_build_cannot_publish_its_old_session() {
    use crate::adapter::net::behavior::org_routing_registry::DirectEligibility;
    let node = node().await;
    let entity = EntityId::from_bytes([0x81u8; 32]);
    const N: u64 = 0x8001;

    let s0 = seed_peer(&node, N, true);
    node.test_pin_peer_entity(N, entity.clone());
    assert_eq!(
        eligibility_of(&node, &entity),
        DirectEligibility::Direct {
            node_id: N,
            session_id: s0,
        },
        "precondition: the projection names S0"
    );

    // Arm the one-shot interleaving: when the build has sampled N's peer and
    // is about to revalidate it, replace the live session.
    let replaced: Arc<parking_lot::Mutex<Option<u64>>> = Arc::new(parking_lot::Mutex::new(None));
    {
        let node_for_hook = node.clone();
        let replaced = replaced.clone();
        node.session_routing
            .observe_build_for_test(Arc::new(move |sampled| {
                if sampled != N {
                    return;
                }
                // `seed_peer` overwrites the peer entry, so the map now holds a
                // different session id under the same node id — the exact shape
                // a re-handshake or a direct-path upgrade produces.
                let s1 = seed_peer(&node_for_hook, N, true);
                *replaced.lock() = Some(s1);
            }));
    }

    // Drive one republication straight through the production notifier.
    node.note_session_transition();

    let s1 = replaced
        .lock()
        .take()
        .expect("the interleaving must have run");
    assert_ne!(s1, s0, "precondition: the replacement is a NEW incarnation");

    // THE PROPERTY: nothing published names S0.
    let published = node.session_routing.published.load_full();
    let row = published.rows.get(&entity).copied();
    assert!(
        !matches!(row, Some(DirectEligibility::Direct { session_id, .. })
                     | Some(DirectEligibility::Relayed { session_id, .. })
                  if session_id == s0),
        "a build that sampled S0 must not publish it once S1 is installed; \
         published {row:?} with S0 = {s0}"
    );
    // And the pool plane cannot name it either, because no pool can be built
    // from a projection that does not contain it.
    assert!(
        !matches!(eligibility_of(&node, &entity),
                  DirectEligibility::Direct { session_id, .. }
                  | DirectEligibility::Relayed { session_id, .. } if session_id == s0),
        "and the eligibility a pool would be annotated from names no S0 either"
    );

    // CONVERGENCE: the next republication — the one the real replacement
    // transition performs — names S1. Without this the witness would accept a
    // plane that refuses forever.
    node.note_session_transition();
    assert_eq!(
        eligibility_of(&node, &entity),
        DirectEligibility::Direct {
            node_id: N,
            session_id: s1,
        },
        "the rebuild after the replacement names the LIVE incarnation"
    );
}

/// (S2-10) The inverse control for S2-9: the same interleaving over a STABLE
/// peer publishes the row.
///
/// Without it, "never publish a row whose peer was observed twice" satisfies
/// S2-9 perfectly — and would make every provider on the node permanently
/// cold, which is the strictly-worse implementation this pair exists to
/// separate from the correct one.
#[tokio::test]
async fn a_stable_peer_observed_twice_still_publishes_its_row() {
    use crate::adapter::net::behavior::org_routing_registry::DirectEligibility;
    let node = node().await;
    let entity = EntityId::from_bytes([0x91u8; 32]);
    const N: u64 = 0x9001;

    let s0 = seed_peer(&node, N, true);
    node.test_pin_peer_entity(N, entity.clone());

    // The same observation point, firing over a peer nothing touches.
    let observed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    {
        let observed = observed.clone();
        node.session_routing
            .observe_build_for_test(Arc::new(move |sampled| {
                if sampled == N {
                    observed.store(true, Ordering::Release);
                }
            }));
    }
    node.note_session_transition();
    assert!(
        observed.load(Ordering::Acquire),
        "precondition: the interleaving point must actually have been reached, \
         or this control proves nothing"
    );

    assert_eq!(
        eligibility_of(&node, &entity),
        DirectEligibility::Direct {
            node_id: N,
            session_id: s0,
        },
        "a peer that did not move is annotated with the session it has"
    );
}

/// (S2-11) A republication becomes visible in ONE store, and the generation
/// travels inside the value it names.
///
/// What this proves, exactly — and no more: every observation a reader can take
/// while a republication is in flight is WHOLLY the old value, rows and
/// generation together, right up to the last instant before the store. It does
/// not schedule a reader *during* the store, because there is no during: the
/// projection and its generation live in one `Arc` and become visible in one
/// atomic swap, which is what makes a torn pair unrepresentable rather than
/// merely unobserved. That structural fact is the claim; this witness pins the
/// two windows either side of it, with EXACT row content rather than
/// cardinality — two projections with different rows and the same count would
/// otherwise be indistinguishable.
#[tokio::test]
async fn a_projection_and_its_generation_are_never_observed_torn() {
    use crate::adapter::net::behavior::org_routing_registry::DirectEligibility;
    type Rows = std::collections::BTreeMap<EntityId, DirectEligibility>;
    let node = node().await;
    let entity = EntityId::from_bytes([0xa1u8; 32]);
    const N: u64 = 0xa001;
    seed_peer(&node, N, true);
    node.test_pin_peer_entity(N, entity.clone());
    // A SECOND entity, pinned but sessionless, so the rebuild about to run
    // changes the row set rather than only the generation.
    let arriving = EntityId::from_bytes([0xa2u8; 32]);
    const M: u64 = 0xa002;
    node.test_pin_peer_entity(M, arriving.clone());
    seed_peer(&node, M, false);

    // Every sample a reader can take while the next republication is in flight:
    // one from inside the build, one from the last instant before the store.
    type Samples = Arc<parking_lot::Mutex<Vec<(&'static str, u64, Rows)>>>;
    let samples: Samples = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let sample = {
        let node_for_hook = node.clone();
        let samples = samples.clone();
        move |where_: &'static str| {
            // The lock-free reader: ONE load, so the generation and the rows it
            // names come from one value.
            let seen = node_for_hook.session_routing.published.load_full();
            samples
                .lock()
                .push((where_, seen.generation, seen.rows.clone()));
        }
    };
    {
        let sample = sample.clone();
        node.session_routing
            .observe_build_for_test(Arc::new(move |_| sample("mid-build")));
    }
    {
        let sample = sample.clone();
        node.session_routing
            .observe_revalidated_for_test(Arc::new(move || sample("pre-store")));
    }

    let before = node.session_routing.published.load_full();
    node.note_session_transition();
    let after = node.session_routing.published.load_full();
    assert_ne!(
        after.rows, before.rows,
        "precondition: this republication must actually change the row set, or \
         old and new are indistinguishable and the witness proves nothing"
    );

    let samples = samples.lock().clone();
    let mut seen_where: Vec<&str> = samples.iter().map(|(w, _, _)| *w).collect();
    seen_where.sort_unstable();
    seen_where.dedup();
    assert_eq!(
        seen_where,
        vec!["mid-build", "pre-store"],
        "both observation points must have been reached, or this witness proves \
         nothing"
    );
    for (where_, generation, rows) in samples {
        assert_eq!(
            (generation, &rows),
            (before.generation, &before.rows),
            "a reader at {where_} observed neither the old projection nor the \
             old generation: the two do not travel together"
        );
    }
    assert!(
        after.generation > before.generation,
        "and the republication did advance the generation"
    );
}

/// (S2-12) **HOLD item 2 — the peer shard is RELEASED before the send awaits.**
///
/// The defect: `send_subprotocol_to_node` and `send_to_peer_node` held the
/// `DashMap` peer guard across `socket.send_to(..).await`, and used it after. A
/// send that pends therefore wedged that shard for as long as it pended — no
/// peer replacement, no eviction, no other reader of the same shard — and
/// because the capability-announcement path funnels through the first of them,
/// an exported FFI entry point reaching it synchronously turned a pending
/// socket into an indefinite hang of the whole peer plane.
///
/// **Why this is driven from an observation point and not from a slow send.**
/// A UDP `send_to` to a black-hole address does not pend: the datagram is
/// accepted by the socket and the call returns at once. So a witness that just
/// sends somewhere unroutable and then checks that a replacement succeeds
/// proves nothing at all — the send is already over. The property is about the
/// window between releasing the guard and awaiting, so it is observed exactly
/// there: the hook fires at that instant, and from it a SEPARATE thread takes
/// the same shard for WRITING under a bounded join.
///
/// Dies to: restoring the guard across the await. The bounded join then expires
/// — the writer is blocked on the shard the send is holding — instead of
/// completing in microseconds.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_send_in_flight_retains_no_peer_shard() {
    let node = node().await;
    const N: u64 = 0xb001;
    let _s0 = seed_peer(&node, N, true);

    // Fired with the guard released and the await not yet started. From here a
    // writer must be able to take the very shard the send used.
    let wrote = Arc::new(std::sync::atomic::AtomicBool::new(false));
    {
        let node_for_hook = node.clone();
        let wrote = wrote.clone();
        node.observe_send_guard_release_for_test(Arc::new(move || {
            let node_for_thread = node_for_hook.clone();
            let writer = std::thread::spawn(move || {
                // A WRITE on the same shard: `insert` needs the write half, so a
                // retained read guard blocks it outright.
                seed_peer(&node_for_thread, N, false);
            });
            // Bounded rather than a bare join, so the mutated build fails
            // loudly instead of hanging the suite.
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            while !writer.is_finished() {
                if std::time::Instant::now() >= deadline {
                    panic!(
                        "a peer-map WRITE could not proceed while a send was in \
                         flight: the send is retaining its peer shard across the \
                         await"
                    );
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            writer.join().expect("writer thread");
            wrote.store(true, Ordering::Release);
        }));
    }

    let sent = node
        .send_subprotocol_to_node(N, SUBPROTOCOL_CAPABILITY_ANN, b"probe")
        .await;
    assert!(
        wrote.load(Ordering::Acquire),
        "the observation point must have been reached, or this witness proves \
         nothing"
    );
    // The send's own outcome is not the property — a black-hole address may
    // succeed or fail at the socket. What matters is that it got here at all.
    let _ = sent;

    // And the announcement path is usable afterwards: it is not queued behind a
    // shard the send never gave back.
    assert!(
        tokio::time::timeout(
            Duration::from_secs(10),
            node.announce_capabilities(
                crate::adapter::net::behavior::capability::CapabilitySet::new()
            )
        )
        .await
        .is_ok(),
        "AnnounceCapabilities must not be wedged behind a completed send"
    );
}

/// Re-insert `node_id`'s peer with the OPPOSITE transport, keeping the very
/// same `NetSession`.
///
/// The session id is deliberately unchanged: that is what makes the DIRECTNESS
/// half of the incarnation re-check observable on its own. A replacement that
/// also minted a new session would be caught by the session-id comparison, so
/// it could never separate the two components.
fn flip_peer_transport(node: &MeshNode, node_id: u64) -> bool {
    let Some(existing) = node.peers.get(&node_id).map(|p| {
        (
            p.session.clone(),
            p.transport,
            p.remote_static_pub,
            p.last_initiator_ephemeral,
        )
    }) else {
        return false;
    };
    let (session, transport, remote_static_pub, last_initiator_ephemeral) = existing;
    let addr = transport.send_addr();
    let flipped = match transport {
        PeerTransport::Direct { .. } => PeerTransport::Routed {
            relay_addr: addr,
            adjacent_relay_identity: None,
        },
        PeerTransport::Routed { .. } => PeerTransport::Direct { owned_addr: addr },
    };
    node.peers.insert(
        node_id,
        PeerInfo {
            node_id,
            transport: flipped,
            session,
            remote_static_pub,
            last_initiator_ephemeral,
        },
    );
    true
}

/// (S2-13) The DIRECTNESS half of the incarnation re-check, on its own.
///
/// The peer's transport flips mid-build while its session id stays identical,
/// so the session-id comparison cannot catch it. Without the directness
/// component the build publishes `Direct` for a provider that is now only
/// reachable through a relay — an annotation that would have 2B.3d preselect a
/// route the peer no longer owns, which is the "directness annotates and never
/// creates authority" line read the wrong way round.
///
/// Dies to: comparing only `session_id` in the incarnation re-check. S2-9
/// survives that mutation, because its replacement mints a new session id.
#[tokio::test]
async fn a_transport_flipped_mid_build_cannot_publish_its_old_directness() {
    use crate::adapter::net::behavior::org_routing_registry::DirectEligibility;
    let node = node().await;
    let entity = EntityId::from_bytes([0xc1u8; 32]);
    const N: u64 = 0xc001;

    let s0 = seed_peer(&node, N, true);
    node.test_pin_peer_entity(N, entity.clone());
    assert_eq!(
        eligibility_of(&node, &entity),
        DirectEligibility::Direct {
            node_id: N,
            session_id: s0,
        },
        "precondition: the projection names a DIRECT S0"
    );

    let flipped = Arc::new(std::sync::atomic::AtomicBool::new(false));
    {
        let node_for_hook = node.clone();
        let flipped = flipped.clone();
        node.session_routing
            .observe_build_for_test(Arc::new(move |sampled| {
                if sampled != N {
                    return;
                }
                assert!(
                    flip_peer_transport(&node_for_hook, N),
                    "the interleaving must actually have flipped the transport"
                );
                flipped.store(true, Ordering::Release);
            }));
    }
    node.note_session_transition();
    assert!(
        flipped.load(Ordering::Acquire),
        "precondition: the interleaving must have run"
    );

    // The session id did NOT change, so only the directness component can
    // refuse this row.
    let published = node.session_routing.published.load_full();
    assert_ne!(
        published.rows.get(&entity).copied(),
        Some(DirectEligibility::Direct {
            node_id: N,
            session_id: s0,
        }),
        "a build that sampled DIRECT must not publish it once the transport is \
         relayed, even though the session id never moved"
    );

    // CONVERGENCE: the rebuild names the transport the peer actually has.
    node.note_session_transition();
    assert_eq!(
        eligibility_of(&node, &entity),
        DirectEligibility::Relayed {
            node_id: N,
            session_id: s0,
        },
        "and the rebuild annotates it RELAYED, on the same session"
    );
}

/// (S2-14) **HOLD-2 item 2 — the forwarding primitive holds no peer-map
/// guard.**
///
/// Every fan-out (pingwave, public capability, scoped capability) now sends from
/// the OWNED snapshot [`snapshot_peers`] returns, so the property is a property
/// of one primitive rather than of five call sites remembering the rule.
///
/// The witness carries its own negative control, which is what gives it teeth: a
/// retained `DashMap` iterator — the exact shape the three forwarding loops had
/// — provably blocks a peer-map writer, and the snapshot provably does not. A
/// witness with only the second half would pass against either implementation.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_peer_snapshot_retains_no_shard_and_drops_the_ingress_peer() {
    let node = node().await;
    const A: u64 = 0xe001;
    const B: u64 = 0xe002;
    let _session_a = seed_peer(&node, A, true);
    let _session_b = seed_peer(&node, B, false);

    // A peer-map WRITE, on demand, from another thread. Returns whether it was
    // still blocked after a bounded wait, and the handle to join once whatever
    // was blocking it is gone.
    let write_attempt = || -> (bool, std::thread::JoinHandle<()>) {
        let node = node.clone();
        let writer = std::thread::spawn(move || {
            seed_peer(&node, A, false);
        });
        let until = std::time::Instant::now() + Duration::from_millis(300);
        while !writer.is_finished() {
            if std::time::Instant::now() >= until {
                return (true, writer);
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        (false, writer)
    };

    // THE NEGATIVE CONTROL: guards alive, writer blocked.
    let control = {
        let held: Vec<_> = node.peers.iter().collect();
        assert!(!held.is_empty(), "precondition: the iterator holds guards");
        let (blocked, writer) = write_attempt();
        assert!(
            blocked,
            "a retained peer-map iterator must block a writer — without this the \
             positive half below proves nothing"
        );
        drop(held);
        writer
    };
    control.join().expect("control writer thread");

    // THE PROPERTY: the snapshot is owned, so the same writer proceeds.
    let snapshot = snapshot_peers(&node.peers, Some(B));
    assert_eq!(
        snapshot.len(),
        1,
        "the ingress peer is excluded and every other peer is present"
    );
    let owned_session = snapshot[0].session.session_id();
    let (blocked, writer) = write_attempt();
    assert!(!blocked, "a live snapshot must retain no peer shard");
    writer.join().expect("writer thread");
    assert_ne!(
        node.peers
            .get(&A)
            .map(|p| p.session.session_id())
            .expect("peer A"),
        owned_session,
        "precondition: that write REPLACED the session the snapshot holds"
    );
    assert_eq!(
        snapshot[0].session.session_id(),
        owned_session,
        "and the snapshot owns the session it was taken with — the replacement \
         cannot retroactively change what this fan-out is sending on"
    );
}

/// (S2-16) **HOLD-2 backstop — the announce lock is acquired under a bound.**
///
/// An exported, synchronous entry point must not block unboundedly on an
/// internal lock: the caller is on the far side of `extern "C"` and has no
/// cancellation, so whatever the acquire waits for, it waits for on the caller's
/// thread forever. §18.0g traces the failure this was found by — a wakeup lost
/// between two statically-linked copies of this crate in one process, where the
/// mutex is free and the parked waiter is never woken.
///
/// A `tokio::time::timeout` around the announce future cannot supply this bound,
/// which is why the bound is on the ACQUIRE: `Mutex::lock()` is not an await
/// point, so the timer never runs on the thread stuck inside it.
///
/// Both halves matter. The refusal proves the bound exists; the acquire-on-
/// release proves the bound did not turn ordinary contention into a refusal.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_announce_lock_is_acquired_under_a_bound_and_refuses_past_it() {
    let node = node().await;

    // THE BOUND: a holder that outlives the deadline yields a refusal, not a
    // wait. Driven through the parameterized form so the witness does not sit
    // out the production deadline; production passes the constant.
    let held = node.announce_mu.lock();
    let started = std::time::Instant::now();
    assert!(
        node.lock_announce_mu_within(Duration::from_millis(150))
            .is_none(),
        "an acquire that cannot complete within its bound must REFUSE"
    );
    let waited = started.elapsed();
    assert!(
        waited < Duration::from_secs(5),
        "and it must refuse promptly — waited {waited:?}"
    );

    drop(held);

    // THE CONTROL: ordinary contention still acquires. Without this, "always
    // refuse" would pass the half above and silently disable announcing.
    // Sequenced by a channel rather than a sleep, so the acquire below really
    // does start while another thread holds the lock.
    let (acquired_tx, acquired_rx) = std::sync::mpsc::channel();
    let holder = {
        let node = node.clone();
        std::thread::spawn(move || {
            let guard = node.announce_mu.lock();
            acquired_tx.send(()).expect("signal");
            std::thread::sleep(Duration::from_millis(100));
            drop(guard);
        })
    };
    acquired_rx.recv().expect("the holder must have acquired");
    assert!(
        node.lock_announce_mu_within(Duration::from_secs(10))
            .is_some(),
        "a lock released inside the bound must be acquired, not refused"
    );
    holder.join().expect("holder thread");
}

/// (S2-15) **HOLD-2 item 1 — the revalidated→published window.** The
/// authoritative peer state cannot move between a build's last revalidation and
/// its store.
///
/// This is the schedule the incarnation re-read cannot close, and the reason the
/// re-read alone was not the repair. The re-read closes `sample → replace →
/// revalidate`. It says nothing about `revalidate → replace → publish`: there,
/// the replacement takes the separate peer-transition guard, mutates `peers`,
/// releases it, and only then queues on the session gate — so the build ahead of
/// it stores a projection under a FRESH generation naming a session `peers` has
/// already dropped, and a 2B.3d consumer reading in that window preselects a
/// dead session. A second best-effort re-read would only move the window.
///
/// So the mutation and the publication are ONE unit
/// ([`commit_peer_transition`]), and this witness observes exactly that: it
/// pauses a build after every row has been revalidated and before anything is
/// stored, has a SEPARATE thread attempt the real replacement through
/// `install_direct`, and proves the replacement cannot land while the window is
/// open — then that it lands, and republishes, the moment it closes.
///
/// **The contention is proven, not inferred.** An earlier shape spawned the
/// replacement, polled peer state for 300 ms, and read `!handle.is_finished()`.
/// That is satisfied just as well by a thread the scheduler had not started
/// yet, so it could not tell BLOCKED from LATE — and "late" would pass against
/// an implementation with no gate on the path at all: nothing in it required
/// the replacement to have TOUCHED the gate. The witness now arms the
/// production pre-lock seam
/// (`observe_transition_attempt`, fired immediately before
/// `commit_peer_transition` takes the session gate) and waits for its ack
/// before asserting anything: when the assertions run, the replacement is
/// known to be at the real gate. Armed from inside the window, so the build
/// under test already owns that gate and the one-shot can only be consumed by
/// the replacement below.
///
/// Dies to: publishing from outside the gate the mutation takes — i.e. mutating
/// first and notifying afterwards, which is the shape this replaces. The
/// replacement then lands mid-window and `stable` is false. S2-9 and S2-13
/// survive that mutation, because both drive their interleaving from a raw seed
/// that takes no gate at all.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_peer_replaced_after_revalidation_cannot_publish_its_old_session() {
    use crate::adapter::net::behavior::org_routing_registry::DirectEligibility;
    let node = node().await;
    let entity = EntityId::from_bytes([0xd1u8; 32]);
    const N: u64 = 0xd001;
    let addr: SocketAddr = "10.9.8.1:9000".parse().expect("addr");
    let replacement_addr: SocketAddr = "10.9.8.2:9000".parse().expect("addr");

    // Seeded through the REAL installer, so the state under test was published
    // by the production transition rather than written past it.
    assert!(
        node.install_direct(N, addr, fresh_session_keys(), None)
            .owned,
        "precondition: the seed install owned the peer"
    );
    node.test_pin_peer_entity(N, entity.clone());
    let s0 = node
        .peers
        .get(&N)
        .map(|p| p.session.session_id())
        .expect("the seeded peer");
    assert_eq!(
        eligibility_of(&node, &entity),
        DirectEligibility::Direct {
            node_id: N,
            session_id: s0,
        },
        "precondition: the projection names S0"
    );

    let fired = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stable = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let blocked = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let at_gate = Arc::new(std::sync::atomic::AtomicBool::new(false));
    #[allow(clippy::type_complexity)]
    let replacer: Arc<parking_lot::Mutex<Option<std::thread::JoinHandle<bool>>>> =
        Arc::new(parking_lot::Mutex::new(None));
    {
        let node_for_hook = node.clone();
        let (fired, stable, blocked, at_gate, replacer) = (
            fired.clone(),
            stable.clone(),
            blocked.clone(),
            at_gate.clone(),
            replacer.clone(),
        );
        node.session_routing
            .observe_revalidated_for_test(Arc::new(move || {
                fired.store(true, Ordering::Release);
                // Arm the pre-lock seam BEFORE spawning, so the replacement
                // cannot slip past it. This build holds the gate right now, so
                // the only transition that can consume the one-shot is the one
                // spawned below.
                let (reached_tx, reached_rx) = std::sync::mpsc::sync_channel::<()>(1);
                node_for_hook
                    .session_routing
                    .observe_transition_attempt_for_test(Arc::new(move || {
                        // `try_send` — the seam must never block the
                        // production path it rides.
                        let _ = reached_tx.try_send(());
                    }));
                let node_for_thread = node_for_hook.clone();
                let handle = std::thread::spawn(move || {
                    node_for_thread
                        .install_direct(N, replacement_addr, fresh_session_keys(), None)
                        .owned
                });
                // THE ACK. Until this returns, "not finished" would be
                // ambiguous between blocked and not-yet-started; after it, the
                // replacement is at the production session gate with nothing
                // else held.
                at_gate.store(
                    reached_rx.recv_timeout(Duration::from_secs(10)).is_ok(),
                    Ordering::Release,
                );
                // For as long as this window is open the authoritative peer
                // state must not move. Polled rather than sampled once, so a
                // replacement landing late in the window is caught too.
                let until = std::time::Instant::now() + Duration::from_millis(300);
                let mut held = true;
                while std::time::Instant::now() < until {
                    if node_for_hook.peers.get(&N).map(|p| p.session.session_id()) != Some(s0) {
                        held = false;
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                stable.store(held, Ordering::Release);
                // And it is BLOCKED, not merely slow: it reached the gate (the
                // ack above) and has still not finished.
                blocked.store(!handle.is_finished(), Ordering::Release);
                // Handed out rather than joined — joining here would wait on a
                // thread that cannot proceed until this window closes.
                *replacer.lock() = Some(handle);
            }));
    }

    let publications_before = node.org_routing_session_publications();
    let generation_before = node
        .org_routing_session_generation()
        .expect("a live generation");
    node.note_session_transition();
    assert!(
        fired.load(Ordering::Acquire),
        "precondition: the revalidated→published window must have been reached"
    );

    // The contention precondition: the replacement really did reach the
    // production session gate. Everything below is about a thread that is
    // queued there, and this is what says so.
    assert!(
        at_gate.load(Ordering::Acquire),
        "the replacement never reached the pre-lock seam ahead of \
         `session_routing.gate.lock()` — without that, `!is_finished()` below \
         would be satisfied by a thread that had merely not started yet"
    );

    // THE PROPERTY.
    assert!(
        stable.load(Ordering::Acquire),
        "a peer replacement landed between the build's revalidation and its \
         store: the projection about to be published names a session `peers` no \
         longer holds"
    );
    assert!(
        blocked.load(Ordering::Acquire),
        "the replacement must have been BLOCKED for the whole window, not \
         merely late"
    );

    // CONVERGENCE: the moment the window closes the replacement proceeds, and
    // publishes its own generation.
    let handle = replacer.lock().take().expect("the replacement thread");
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !handle.is_finished() {
        assert!(
            std::time::Instant::now() < deadline,
            "the replacement never proceeded after the window closed"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(
        handle.join().expect("replacement thread"),
        "the replacement transition owned the peer"
    );
    let s1 = node
        .peers
        .get(&N)
        .map(|p| p.session.session_id())
        .expect("the replaced peer");
    assert_ne!(s1, s0, "precondition: the replacement is a NEW incarnation");
    assert_eq!(
        node.org_routing_session_publications(),
        publications_before + 2,
        "exactly two publications, in order: this build, then the replacement's \
         own — which is what makes the generation naming S1 strictly newer than \
         the one naming S0"
    );
    assert_eq!(
        node.org_routing_session_generation(),
        Some(generation_before + 2),
        "and the generation advanced once per publication"
    );
    assert_eq!(
        eligibility_of(&node, &entity),
        DirectEligibility::Direct {
            node_id: N,
            session_id: s1,
        },
        "the live projection names the LIVE incarnation"
    );
}

// ---- OLB-2B.3d-pre: the coherent cold-plan capture -------------------------
//
// The capture is the cold plan's first step, and its whole value is coherence:
// one instant, one revocation view, one consumer-grant view, one store critical
// section, one authority identity. Every witness below drives the PRODUCTION
// seam (`MeshNode::org_cold_discovery` / `org_cold_authority`) on a real node —
// the per-row content is witnessed against the live plane seams in the SDK,
// where real envelopes can be ingested; these four assert the properties only
// the node's private state can show.

/// (C1) ONE store critical section spans every plane of a capture.
///
/// The contention form, not a counter: the hook fires with the scoped-store lock
/// HELD, between the owner plane and the grant planes, and proves a rival
/// `try_lock` FAILS there. Per-plane lock acquisitions — the shape this replaces,
/// where a store mutation can land between two planes of one plan — would let
/// that `try_lock` succeed.
///
/// Carries its own negative control: the same `try_lock` succeeds once the
/// capture has returned, so the positive half cannot be satisfied by a lock that
/// is simply never available.
#[tokio::test]
async fn a_cold_capture_holds_one_store_section_across_every_plane() {
    let node = node().await;
    let org = crate::adapter::net::behavior::org::OrgKeypair::from_bytes([0xc1u8; 32]);
    node.install_node_authority(adopt_authority(&node, &org, "cold-section"))
        .expect("install authority");

    let observed = Arc::new(AtomicBool::new(false));
    let contended = Arc::new(AtomicBool::new(false));
    {
        let scoped = node.scoped_discovery.clone();
        let observed = observed.clone();
        let contended = contended.clone();
        *node.cold_capture_plane_gap_hook.lock() = Some(Arc::new(move || {
            observed.store(true, Ordering::Release);
            contended.store(scoped.try_lock().is_none(), Ordering::Release);
        }));
    }

    let capture = node
        .org_cold_discovery(&CapabilityAuthorityId::for_tag("nrpc:cold.section"), &[])
        .expect("an adopted node captures");
    assert!(
        observed.load(Ordering::Acquire),
        "the between-planes window was never entered, so this witness asserted \
         nothing"
    );
    assert!(
        contended.load(Ordering::Acquire),
        "a store mutation could occupy the gap between two planes of ONE capture"
    );
    assert!(
        node.scoped_discovery.try_lock().is_some(),
        "control: the store lock is free once the capture returns, so the \
         positive half above is about the capture and not about an unavailable \
         lock"
    );
    assert!(
        capture.owner_providers().is_empty(),
        "no envelope was ingested, so the owner plane is empty — the capture's \
         CONTENT is witnessed in the SDK against the live seams"
    );
}

/// (C2) An authority installation that lands INSIDE a capture is never captured
/// across.
///
/// The seqlock the read seam already uses, applied to a wider read: the capture
/// re-checks the routing epoch after reading the view, so an install occupying
/// the window is detected and that attempt is discarded. Dropping the re-check
/// would return a stamp naming the PRE-install epoch — an identity the final
/// comparison would then confirm against a view the successor has replaced.
///
/// The install runs on another thread and is JOINED inside the window, so the
/// interleaving is deterministic rather than raced. It fires in the lock-free
/// window on purpose: the install commits floor reconciliation through the
/// scoped store, so it cannot complete inside the capture's store section at
/// all.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_authority_install_inside_a_cold_capture_is_never_captured_across() {
    let node = node().await;
    let org = crate::adapter::net::behavior::org::OrgKeypair::from_bytes([0xc2u8; 32]);
    node.install_node_authority(adopt_authority(&node, &org, "cold-straddle-a"))
        .expect("install authority");
    let before = node.routing_authority.epoch();

    // Fires ONCE (the hook is taken), so the retry runs cleanly.
    let successor = adopt_authority(&node, &org, "cold-straddle-b");
    let installed = Arc::new(AtomicBool::new(false));
    {
        let node2 = node.clone();
        let installed = installed.clone();
        *node.cold_capture_authority_gap_hook.lock() = Some(Arc::new(move || {
            let node3 = node2.clone();
            let successor = successor.clone();
            let handle = std::thread::spawn(move || {
                node3
                    .install_node_authority(successor)
                    .expect("same-org renewal is accepted");
            });
            handle.join().expect("installer thread");
            installed.store(true, Ordering::Release);
        }));
    }

    let capture = node
        .org_cold_discovery(&CapabilityAuthorityId::for_tag("nrpc:cold.straddle"), &[])
        .expect("the retry captures coherently");
    assert!(
        installed.load(Ordering::Acquire),
        "the window was never entered, so this witness asserted nothing"
    );
    let after = node.routing_authority.epoch();
    assert_ne!(
        after, before,
        "precondition: the install inside the window moved the routing epoch"
    );
    assert_eq!(
        capture.stamp().epoch(),
        after,
        "the capture must carry the epoch it actually completed under, never the \
         one it started under"
    );
    assert!(
        node.org_cold_authority_is_current(capture.stamp()),
        "and the returned stamp compares CURRENT immediately afterwards"
    );
}

/// (C3) A node with no installed authority refuses the capture, with the
/// adjacent control that adopting one admits it.
///
/// Fail-closed: without authority there is no private-discovery authority to
/// plan under, and reporting an empty provider set would misattribute a local
/// configuration failure to the network.
#[tokio::test]
async fn an_unadopted_node_refuses_the_cold_capture() {
    use crate::adapter::net::behavior::org_cold_plan::OrgColdRefusal;
    let node = node().await;
    let capability = CapabilityAuthorityId::for_tag("nrpc:cold.unadopted");
    assert_eq!(
        node.org_cold_discovery(&capability, &[]).err(),
        Some(OrgColdRefusal::NoNodeAuthority),
        "an un-adopted node cannot capture private-discovery authority"
    );
    assert_eq!(
        node.org_cold_authority().err(),
        Some(OrgColdRefusal::NoNodeAuthority),
        "and neither can the authority-only shape the exported path uses"
    );
}

/// (C4) A spent authority-epoch space refuses the capture rather than capturing
/// under an identity that can no longer be compared.
///
/// At saturation every later authority receives the same identity, so a stamp
/// taken there would compare EQUAL across an unrelated replacement — exactly the
/// aliasing the routing plane already fences.
#[tokio::test]
async fn a_spent_authority_epoch_refuses_the_cold_capture() {
    use crate::adapter::net::behavior::org_cold_plan::OrgColdRefusal;
    let node = node().await;
    let org = crate::adapter::net::behavior::org::OrgKeypair::from_bytes([0xc4u8; 32]);
    node.install_node_authority(adopt_authority(&node, &org, "cold-spent"))
        .expect("install authority");
    let capability = CapabilityAuthorityId::for_tag("nrpc:cold.spent");
    assert!(
        node.org_cold_discovery(&capability, &[]).is_ok(),
        "precondition: the capture works before the epoch space is spent"
    );

    node.routing_authority
        .epoch
        .store(u64::MAX, Ordering::Release);
    {
        let _gate = node.routing_authority.lock_gate();
        assert_eq!(
            node.routing_authority.advance(),
            AuthorityAdvance::NewlyExhausted,
            "precondition: the epoch space is now terminal"
        );
    }

    assert_eq!(
        node.org_cold_discovery(&capability, &[]).err(),
        Some(OrgColdRefusal::IncoherentAuthority),
        "a spent epoch space cannot witness currentness, so the capture fails \
         closed"
    );
    assert_eq!(
        node.org_cold_authority().err(),
        Some(OrgColdRefusal::IncoherentAuthority),
        "and so does the authority-only shape"
    );
}
