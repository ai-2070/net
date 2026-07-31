//! OLB-2B.3b witnesses for the clone-family routing state.
//!
//! Three groups, each pinning one thing the design settles:
//!
//! - **§4 demand-set derivation** — Owner plus the audiences the family
//!   ACTUALLY LEASED for DISCOVER, and nothing else (W-N1, W-N2);
//! - **§8 lock-free lookup** — both zero-lock forms, an end-to-end counter and a
//!   real contention witness against the actual mutex;
//! - **§9 refusal policy** — sticky, retryable-on-a-generation, and terminal,
//!   each with the control that stops a collapsed implementation passing.

#![allow(clippy::disallowed_methods)]

use super::*;
use crate::adapter::net::behavior::org::{current_timestamp, OrgKeypair};
use crate::adapter::net::behavior::org_grant::{GrantRights, GrantTargetScope, OrgAudienceSecret};
use crate::adapter::net::behavior::org_grant_registry::{
    validate_consumer_record, PreparedInstall,
};
use crate::adapter::net::behavior::org_routing::RegistryWork;
use crate::adapter::net::behavior::org_routing_registry::{
    GrantArtifactFence, NodeOrgRoutingRegistry, RegistryMetrics, ScopedDiscoveryAuthorityStamp,
    ScopedSourceFacts, SlotSource, SourceCommitPin, SourceFacts, SourceSnapshot, SourceToken,
    MAX_NODE_SLOTS,
};
use std::time::Duration;

// ----------------------------------------------------------------- fixtures

/// A source that answers nothing.
///
/// These witnesses are about DEMAND — which scopes a family retains, under what
/// budget, and how a miss decides whether to try again. None of them installs a
/// fact, so a source that never serves is not a simplification that hides a
/// property; a serving one would only add an actor the assertions do not read.
struct InertSource;

struct InertSnapshot;

impl SourceSnapshot for InertSnapshot {
    fn token(&self) -> SourceToken {
        SourceToken::new(vec![0])
    }
    fn providers(&self, _key: &SlotKey) -> ScopedSourceFacts {
        ScopedSourceFacts {
            facts: SourceFacts::Unserved,
            authority: ScopedDiscoveryAuthorityStamp::Owner,
            authority_deadline: u64::MAX,
            grant_fence: GrantArtifactFence::Publication(0),
        }
    }
}

impl SlotSource for InertSource {
    fn snapshot(&self, _keys: &[SlotKey]) -> Box<dyn SourceSnapshot> {
        Box::new(InertSnapshot)
    }
    fn pin_if_current(
        &self,
        _keys: &[SlotKey],
        _expected: &SourceToken,
    ) -> Option<Box<dyn SourceCommitPin + '_>> {
        None
    }
}

struct Fixture {
    registry: Arc<NodeOrgRoutingRegistry>,
    metrics: Arc<RegistryMetrics>,
}

fn fixture() -> Fixture {
    let metrics: Arc<RegistryMetrics> = Arc::default();
    let registry = NodeOrgRoutingRegistry::new(
        Arc::new(InertSource),
        Arc::<RegistryWork>::default(),
        metrics.clone(),
    );
    Fixture { registry, metrics }
}

impl Fixture {
    fn state(&self, credentials: FamilyDiscoveryCredentials) -> OrgRoutingState {
        OrgRoutingState::new(self.registry.new_family().expect("family"), credentials)
    }
}

fn consumer_org() -> OrgKeypair {
    OrgKeypair::from_bytes([0xA1; 32])
}

fn provider_org() -> OrgKeypair {
    OrgKeypair::from_bytes([0xB2; 32])
}

fn cap(tag: &str) -> CapabilityAuthorityId {
    CapabilityAuthorityId::for_tag(tag)
}

const OWNER_HANDLE: [u8; 32] = [0x0E; 32];

fn credentials(grants: Vec<Arc<OrgCapabilityGrant>>) -> FamilyDiscoveryCredentials {
    FamilyDiscoveryCredentials {
        acting_org: consumer_org().org_id(),
        owner_audience_handle: OWNER_HANDLE,
        grants,
    }
}

fn owner_key(capability: &CapabilityAuthorityId) -> SlotKey {
    SlotKey {
        scope: PrivateAudienceScope::new(CapabilityAudienceScope::Owner {
            org_id: consumer_org().org_id(),
            audience_handle: OWNER_HANDLE,
        })
        .expect("owner scopes are private"),
        capability: *capability,
    }
}

fn grant_key(grant: &OrgCapabilityGrant) -> SlotKey {
    SlotKey {
        scope: PrivateAudienceScope::new(CapabilityAudienceScope::Grant {
            grant_id: grant.grant_id,
            audience_handle: grant
                .discovery
                .as_ref()
                .expect("a DISCOVER grant carries a binding")
                .audience_handle,
        })
        .expect("grant scopes are private"),
        capability: grant.capability,
    }
}

/// A B→A grant over `capability` with exactly `rights`, plus its out-of-band
/// audience secret when it has one.
fn issue(
    capability: CapabilityAuthorityId,
    rights: GrantRights,
) -> (Arc<OrgCapabilityGrant>, Option<OrgAudienceSecret>) {
    let (grant, secret) = OrgCapabilityGrant::try_issue(
        &provider_org(),
        consumer_org().org_id(),
        capability,
        rights,
        GrantTargetScope::AnyNodeOwnedBy(provider_org().org_id()),
        3600,
    )
    .expect("issue");
    (Arc::new(grant), secret)
}

/// Install `(grant, secret)` into a consumer snapshot — the exact production
/// two-step, so "leased" here means what it means at the source seam.
fn lease(
    snapshot: &ConsumerGrantSnapshot,
    grant: &OrgCapabilityGrant,
    secret: OrgAudienceSecret,
    install_seq: u64,
) -> ConsumerGrantSnapshot {
    let now = current_timestamp();
    let record = validate_consumer_record(grant.clone(), secret, &consumer_org().org_id(), now, 60)
        .expect("consumer record valid");
    match snapshot
        .prepare_install(record, now)
        .expect("room reserved")
    {
        PreparedInstall::Ready(slot) => ConsumerGrantSnapshot::finish_install(*slot, install_seq),
        PreparedInstall::Noop => panic!("the witness installs a fresh grant"),
    }
}

// ------------------------------------------- §4: the exact demand set (W-N)

/// The baseline: Owner plus one leased DISCOVER audience, in that order.
#[test]
fn a_leased_discover_grant_is_a_demand_beside_owner() {
    let capability = cap("nrpc:read");
    let (grant, secret) = issue(capability, GrantRights::DISCOVER.union(GrantRights::INVOKE));
    let leased = lease(
        &ConsumerGrantSnapshot::empty(),
        &grant,
        secret.expect("DISCOVER mints a secret"),
        1,
    );

    let keys = demand_set_for(&credentials(vec![grant.clone()]), &capability, &leased)
        .expect("the family has an owner scope");
    assert_eq!(
        keys,
        vec![owner_key(&capability), grant_key(&grant)],
        "Owner first, then the leased Grant audience"
    );
}

/// **W-N1** — a DISCOVER grant whose audience secret was never installed is NOT
/// a demand.
///
/// Dies to: dropping the installed-lease check, i.e. demanding every DISCOVER
/// grant the family holds. The resulting slot is a required contributor the
/// source can only ever answer `Unserved`, so it consumes family and node budget
/// permanently while contributing nothing (§4).
#[test]
fn a_discover_grant_with_no_installed_audience_is_not_a_demand() {
    let capability = cap("nrpc:read");
    let (unleased, unleased_secret) = issue(capability, GrantRights::DISCOVER);
    let unleased_secret = unleased_secret.expect("DISCOVER mints a secret");
    let (leased_grant, leased_secret) = issue(capability, GrantRights::DISCOVER);
    let leased = lease(
        &ConsumerGrantSnapshot::empty(),
        &leased_grant,
        leased_secret.expect("DISCOVER mints a secret"),
        1,
    );

    let keys = demand_set_for(
        &credentials(vec![unleased.clone(), leased_grant.clone()]),
        &capability,
        &leased,
    )
    .expect("owner scope");

    assert_eq!(
        keys,
        vec![owner_key(&capability), grant_key(&leased_grant)],
        "the unleased DISCOVER grant contributes no demand"
    );
    assert!(
        !keys.contains(&grant_key(&unleased)),
        "and specifically not a permanently-Unserved contributor for it"
    );
    // The REASON matters as much as the outcome, and is asserted where the two
    // exclusions are separable — it must be excluded for being unleased, not for
    // lacking DISCOVER, which it has.
    assert_eq!(
        classify(&unleased, &capability, &consumer_org().org_id(), &leased),
        Some(GrantDemand::NotLeased),
        "DISCOVER it has; the audience it does not"
    );

    // Control: the SAME grant becomes a demand the moment its audience IS
    // installed. Without it, an implementation that simply never demands a
    // Grant scope satisfies every assertion above — the exact "a comparison
    // that never fails" shape W-G8 was rescued from.
    let both = lease(&leased, &unleased, unleased_secret, 2);
    let keys = demand_set_for(
        &credentials(vec![unleased.clone(), leased_grant.clone()]),
        &capability,
        &both,
    )
    .expect("owner scope");
    assert_eq!(keys.len(), 3, "leasing it makes it a demand: {keys:?}");
    assert!(keys.contains(&grant_key(&unleased)));
}

/// **W-N2** — an INVOKE-only grant is NOT a source demand.
///
/// Dies to: classifying by "the family holds a grant for this capability"
/// instead of "the family leased a DISCOVER audience for it" — the mutation that
/// makes `classify` return a scope for a grant with no discovery binding.
///
/// The INVOKE-only grant is not discarded, only undemanded: it stays in the
/// family's credential set, which is where §3.1's INVOKE matching reads it.
#[test]
fn an_invoke_only_grant_is_not_a_source_demand() {
    let capability = cap("nrpc:read");
    let (discover, discover_secret) = issue(capability, GrantRights::DISCOVER);
    let leased = lease(
        &ConsumerGrantSnapshot::empty(),
        &discover,
        discover_secret.expect("DISCOVER mints a secret"),
        1,
    );

    // The ORDINARY INVOKE-only grant, with an id of its own.
    let (invoke_only, secret) = issue(capability, GrantRights::INVOKE);
    assert!(
        secret.is_none() && invoke_only.discovery.is_none(),
        "an INVOKE-only grant carries no discovery binding at all"
    );

    // The rights gate is asserted at the CLASSIFIER, which is the only place the
    // two gates are separable. Downstream they are not: a grant with no binding
    // has no audience to name, so any scope synthesized for it is one the lease
    // check then rejects — W-N1's gate structurally subsumes W-N2's, and a
    // witness that only looked at the demand set would survive removing the
    // rights gate entirely. It did. `GrantDemand` distinguishes the two REASONS
    // precisely so they can be mutated and asserted apart.
    assert_eq!(
        classify(&invoke_only, &capability, &consumer_org().org_id(), &leased),
        Some(GrantDemand::NotDiscovery),
        "excluded for carrying no DISCOVER right — NOT for being unleased"
    );
    assert_eq!(
        classify(&discover, &capability, &consumer_org().org_id(), &leased),
        Some(GrantDemand::Leased(
            discover
                .discovery
                .as_ref()
                .expect("binding")
                .audience_handle
        )),
        "the control: the DISCOVER grant beside it IS a demand"
    );

    let creds = credentials(vec![invoke_only.clone(), discover.clone()]);
    let keys = demand_set_for(&creds, &capability, &leased).expect("owner scope");
    assert_eq!(
        keys,
        vec![owner_key(&capability), grant_key(&discover)],
        "exactly two demands: Owner and the DISCOVER audience"
    );
    assert!(
        creds
            .grants
            .iter()
            .any(|g| g.grant_id == invoke_only.grant_id),
        "and the INVOKE-only grant is RETAINED for the projection to match"
    );

    // It also costs no budget: acquiring the entry spends two handles, not three.
    let f = fixture();
    let state = f.state(creds);
    assert_eq!(state.route_handle(&capability, &leased), RouteLookup::Warm);
    assert_eq!(state.handles(), 2, "Owner + the leased DISCOVER audience");
    assert_eq!(f.registry.retained_slots(), 2);
}

/// A grant for another capability, or naming another grantee, is not this
/// capability's demand.
#[test]
fn demands_are_exact_on_capability_and_grantee() {
    let capability = cap("nrpc:read");
    let (mine, mine_secret) = issue(capability, GrantRights::DISCOVER);
    let (other_cap, other_secret) = issue(cap("nrpc:write"), GrantRights::DISCOVER);
    let leased = lease(
        &ConsumerGrantSnapshot::empty(),
        &mine,
        mine_secret.expect("secret"),
        1,
    );
    let leased = lease(&leased, &other_cap, other_secret.expect("secret"), 2);

    let keys = demand_set_for(
        &credentials(vec![mine.clone(), other_cap.clone()]),
        &capability,
        &leased,
    )
    .expect("owner scope");
    assert_eq!(keys, vec![owner_key(&capability), grant_key(&mine)]);

    // A grant naming a DIFFERENT grantee org is not this family's, even leased.
    let (foreign, foreign_secret) = OrgCapabilityGrant::try_issue(
        &provider_org(),
        OrgKeypair::from_bytes([0xC3; 32]).org_id(),
        capability,
        GrantRights::DISCOVER,
        GrantTargetScope::AnyNodeOwnedBy(provider_org().org_id()),
        3600,
    )
    .map(|(g, s)| (Arc::new(g), s))
    .expect("issue");
    let now = current_timestamp();
    let record = validate_consumer_record(
        (*foreign).clone(),
        foreign_secret.expect("secret"),
        &OrgKeypair::from_bytes([0xC3; 32]).org_id(),
        now,
        60,
    )
    .expect("valid for ITS grantee");
    let leased = match leased.prepare_install(record, now).expect("room") {
        PreparedInstall::Ready(slot) => ConsumerGrantSnapshot::finish_install(*slot, 3),
        PreparedInstall::Noop => panic!("fresh"),
    };
    let keys = demand_set_for(
        &credentials(vec![mine.clone(), foreign]),
        &capability,
        &leased,
    )
    .expect("owner scope");
    assert_eq!(
        keys,
        vec![owner_key(&capability), grant_key(&mine)],
        "another org's grant is not this family's demand"
    );
}

/// A rotated audience does not alias through the grant id.
///
/// The same defect the source seam closed at `cbbd448b3`, arriving on the demand
/// side: an id-keyed lease check would call a rotated-away handle leased and
/// demand a scope no installed record authorizes.
#[test]
fn a_rotated_audience_handle_is_not_leased_under_its_own_id() {
    let capability = cap("nrpc:read");
    let (grant, secret) = issue(capability, GrantRights::DISCOVER);
    let leased = lease(
        &ConsumerGrantSnapshot::empty(),
        &grant,
        secret.expect("secret"),
        1,
    );

    // A DIFFERENT signed grant reusing the id, under a different handle. Its
    // scope is not leased, because the installed record's handle is the other
    // one.
    let mut rotated = (*grant).clone();
    if let Some(binding) = rotated.discovery.as_mut() {
        binding.audience_handle = [0xEE; 32];
    }
    let rotated = Arc::new(rotated);

    let keys = demand_set_for(&credentials(vec![rotated.clone()]), &capability, &leased)
        .expect("owner scope");
    assert_eq!(
        keys,
        vec![owner_key(&capability)],
        "the id is installed, but under a different handle — not leased"
    );
}

// -------------------------------------------- §8: the lock-free read path

/// The end-to-end counter form: a warmed lookup acquires `mutate` ZERO times.
#[test]
fn a_warmed_lookup_takes_no_lock() {
    let capability = cap("nrpc:read");
    let f = fixture();
    let state = f.state(credentials(Vec::new()));
    let leased = ConsumerGrantSnapshot::empty();

    assert_eq!(state.route_handle(&capability, &leased), RouteLookup::Warm);
    let after_miss = state.mutate_acquisitions();
    assert_eq!(after_miss, 1, "the MISS takes it exactly once");

    for _ in 0..1_000 {
        assert!(state.warm(&capability).is_some());
        assert_eq!(state.route_handle(&capability, &leased), RouteLookup::Warm);
    }
    assert_eq!(
        state.mutate_acquisitions(),
        after_miss,
        "and a thousand warmed lookups take it not once more"
    );
}

/// The contention form: hold the REAL mutex, prove a contender cannot take it,
/// and complete a warmed lookup while it is held.
///
/// The counter above cannot distinguish "no lock" from "an uncontended lock";
/// this can. Bounded wait, no retry, and the contender's failure is
/// acknowledged before the lookup runs so the two are genuinely concurrent.
#[test]
fn a_warmed_lookup_completes_while_the_mutation_lock_is_held() {
    let capability = cap("nrpc:read");
    let f = fixture();
    let state = Arc::new(f.state(credentials(Vec::new())));
    let leased = ConsumerGrantSnapshot::empty();
    assert_eq!(state.route_handle(&capability, &leased), RouteLookup::Warm);

    let held = state.mutate_lock_for_test().lock();

    let (tx, rx) = std::sync::mpsc::channel();
    let contender = {
        let state = state.clone();
        std::thread::spawn(move || {
            let blocked = state.mutate_lock_for_test().try_lock().is_none();
            tx.send(blocked).expect("acknowledge");
            blocked
        })
    };
    assert!(
        rx.recv_timeout(Duration::from_secs(5))
            .expect("the contender must report within the bound"),
        "the lock this witness holds must be the REAL one — try_lock has to fail"
    );

    // The lock is genuinely held and genuinely contended. A warmed lookup still
    // completes, because it never goes near it.
    let handle = state.warm(&capability).expect("warm read under contention");
    assert_eq!(handle.capability(), &capability);
    assert_eq!(
        handle.demands().len(),
        1,
        "Owner only, for a grantless family"
    );
    assert_eq!(state.route_handle(&capability, &leased), RouteLookup::Warm);

    drop(held);
    assert!(contender.join().expect("contender joins"));
}

/// Concurrent misses for one capability spend ONE demand set.
///
/// The lock-free hit is checked AGAIN under `mutate` for exactly this reason: the
/// loser of the race must adopt the winner's entry, not acquire a second,
/// duplicate set against the family's budget.
///
/// Two things this witness had to be rebuilt around, both found by running the
/// mutation rather than by reading the test:
///
/// - **It drives `acquire`, the production miss path, not `route_handle`.**
///   Through `route_handle` the outcome is the scheduler's: if the winner
///   publishes before a rival reaches the LOCK-FREE check, the rival returns
///   early and never enters the section under test. Entering `acquire` directly
///   puts all four threads past that check by construction.
/// - **It asserts on the family BUDGET, not on the end-state handle count.** A
///   duplicate acquisition is self-cleaning: the second publication replaces the
///   first index entry, the displaced `CapabilityRouteHandle` drops, and its
///   demands release — so the final count is 1 either way and an assertion on it
///   is satisfied by the defect. What does NOT clean up is the refusal a
///   transient over-spend produces, and `FamilyAtCapacity` is **sticky for the
///   family's lifetime** (§9). One redundant acquisition can therefore poison a
///   family permanently, which is the real harm and the thing to assert.
#[test]
fn concurrent_misses_acquire_one_demand_set() {
    let capability = cap("nrpc:read");
    let f = fixture();
    // THREE demands for the contested capability — Owner plus two leased
    // audiences. A one-demand entry cannot show the defect: the displaced
    // handle releases inside the winner's own `index.store`, so the transient
    // peak never exceeds the budget by more than the single demand that was
    // just reclaimed, and no rival is ever refused. At three, the peak is
    // `held + 3` with the previous entry's 3 still alive, which overruns.
    let mut grants = Vec::new();
    let mut leased = ConsumerGrantSnapshot::empty();
    for i in 0..2u32 {
        let (grant, secret) = issue(capability, GrantRights::DISCOVER);
        leased = lease(&leased, &grant, secret.expect("secret"), u64::from(i) + 1);
        grants.push(grant);
    }
    let state = Arc::new(f.state(credentials(grants)));

    // 60 of 64 handles spent, so the FIRST acquisition fits at 63 and a second
    // concurrent one provably does not.
    for i in 0..60u32 {
        assert_eq!(
            state.route_handle(&cap(&format!("nrpc:fill{i}")), &leased),
            RouteLookup::Warm
        );
    }
    assert_eq!(state.handles(), 60);
    let before = state.mutate_acquisitions();
    let leased = Arc::new(leased);

    let start = Arc::new(std::sync::Barrier::new(4));
    let threads: Vec<_> = (0..4)
        .map(|_| {
            let state = state.clone();
            let start = start.clone();
            let leased = leased.clone();
            std::thread::spawn(move || {
                start.wait();
                state.acquire(&capability, &leased)
            })
        })
        .collect();
    for t in threads {
        assert_eq!(
            t.join().expect("join"),
            RouteLookup::Warm,
            "every rival adopts the winner's entry; none spends the budget again"
        );
    }

    assert_eq!(
        state.mutate_acquisitions() - before,
        4,
        "all four genuinely entered the mutation section"
    );
    assert_eq!(state.entries(), 61, "one new entry, not four");
    assert_eq!(state.handles(), 63, "Owner + two leased audiences, once");
    assert_eq!(f.registry.retained_slots(), 63);
    assert_eq!(
        f.metrics.refused_family_at_capacity(),
        0,
        "no rival was refused, so no sticky refusal was recorded"
    );

    // The family is NOT poisoned: it still has room for its 64th demand.
    assert_eq!(
        state.route_handle(&cap("nrpc:after"), &leased),
        RouteLookup::Warm,
        "a transient over-spend would have set the sticky flag and blocked this"
    );
    assert_eq!(state.handles(), 64);
}

// ------------------------------------------------- §9: the refusal policy

/// `FamilyAtCapacity` is STICKY for the family's lifetime.
///
/// Dies to: dropping the sticky flag, which makes every later cold call
/// re-derive the set and re-take the registry lock to be refused again. Sticky
/// is exact rather than merely conservative here because this state never evicts
/// an entry, so a spent budget stays spent.
#[test]
fn family_capacity_refusal_is_sticky_for_the_family() {
    let f = fixture();
    let state = f.state(credentials(Vec::new()));
    let leased = ConsumerGrantSnapshot::empty();
    for i in 0..64u32 {
        assert_eq!(
            state.route_handle(&cap(&format!("nrpc:c{i}")), &leased),
            RouteLookup::Warm
        );
    }
    assert_eq!(state.handles(), 64, "the budget is exactly spent");
    assert_eq!(state.entries(), 64, "64 grantless capabilities fit");

    for i in 0..32u32 {
        assert_eq!(
            state.route_handle(&cap(&format!("nrpc:over{i}")), &leased),
            RouteLookup::Cold(ColdReason::Refused(DemandRefused::FamilyAtCapacity)),
        );
    }
    assert_eq!(
        f.metrics.refused_family_at_capacity(),
        1,
        "the registry is asked ONCE; every later miss is settled locally"
    );
    // Warmed entries keep working — sticky means no new entries, not a dead
    // family.
    assert_eq!(
        state.route_handle(&cap("nrpc:c0"), &leased),
        RouteLookup::Warm
    );
}

/// Option-A accounting: the family bound is on DEMANDS, not capabilities.
///
/// This is the property the plan text was corrected to state. One leased
/// DISCOVER grant per capability costs two demands, so 32 capabilities fit and
/// the 33rd is refused — under a "64 warmed capabilities" bound it would not be.
#[test]
fn the_family_bound_counts_demands_not_capabilities() {
    let f = fixture();
    let mut grants = Vec::new();
    let mut leased = ConsumerGrantSnapshot::empty();
    for i in 0..33u32 {
        let (grant, secret) = issue(cap(&format!("nrpc:c{i}")), GrantRights::DISCOVER);
        leased = lease(
            &leased,
            &grant,
            secret.expect("DISCOVER mints a secret"),
            u64::from(i) + 1,
        );
        grants.push(grant);
    }
    let state = f.state(credentials(grants));

    for i in 0..32u32 {
        assert_eq!(
            state.route_handle(&cap(&format!("nrpc:c{i}")), &leased),
            RouteLookup::Warm
        );
    }
    assert_eq!(state.handles(), 64, "two demands per capability");
    assert_eq!(state.entries(), 32, "so 32 capabilities, not 64");
    assert_eq!(
        state.route_handle(&cap("nrpc:c32"), &leased),
        RouteLookup::Cold(ColdReason::Refused(DemandRefused::FamilyAtCapacity)),
        "the 33rd needs two more handles and there are none"
    );
    assert!(
        state.entries() <= MAX_CAPABILITY_ENTRIES_PER_FAMILY,
        "the entry ceiling is structural, never separately enforced"
    );
}

/// `NodeAtCapacity` is RETRYABLE, gated on the node capacity generation.
///
/// Dies to: dropping the generation gate, which makes every later miss re-derive
/// and re-take the node-wide registry lock while the node is provably still
/// full. Also dies to gating on `retained_slots()` instead, which a
/// retire-then-demand pair leaves unchanged.
#[test]
fn node_capacity_refusal_retries_only_when_the_generation_moves() {
    let f = fixture();
    let mut held = Vec::new();
    let mut fillers = Vec::new();
    for chunk in 0..4u32 {
        let filler = f.registry.new_family().expect("family");
        for i in 0..64u32 {
            held.push(
                filler
                    .demand(SlotKey {
                        scope: PrivateAudienceScope::new(CapabilityAudienceScope::Grant {
                            grant_id: [0x5A; 32],
                            audience_handle: [0x5A; 32],
                        })
                        .expect("private"),
                        capability: cap(&format!("nrpc:fill{}", chunk * 64 + i)),
                    })
                    .expect("fill"),
            );
        }
        fillers.push(filler);
    }
    assert_eq!(f.registry.retained_slots(), MAX_NODE_SLOTS);

    let state = f.state(credentials(Vec::new()));
    let leased = ConsumerGrantSnapshot::empty();
    let capability = cap("nrpc:read");
    assert_eq!(
        state.route_handle(&capability, &leased),
        RouteLookup::Cold(ColdReason::Refused(DemandRefused::NodeAtCapacity))
    );
    assert_eq!(f.metrics.refused_node_at_capacity(), 1);

    let generation = f.registry.node_capacity_generation();
    for _ in 0..32 {
        assert_eq!(
            state.route_handle(&capability, &leased),
            RouteLookup::Cold(ColdReason::Refused(DemandRefused::NodeAtCapacity))
        );
    }
    assert_eq!(
        f.metrics.refused_node_at_capacity(),
        1,
        "the registry is not asked again while the generation stands still"
    );
    assert_eq!(f.registry.node_capacity_generation(), generation);

    // Free one slot. The generation moves, and the very next miss retries.
    held.pop();
    assert_eq!(f.registry.retained_slots(), MAX_NODE_SLOTS - 1);
    assert_ne!(
        f.registry.node_capacity_generation(),
        generation,
        "a retirement is what moves it"
    );
    assert_eq!(
        state.route_handle(&capability, &leased),
        RouteLookup::Warm,
        "the retry succeeds on the freed capacity"
    );
    assert_eq!(state.entries(), 1);
}

/// `IdSpaceExhausted` is TERMINAL — never retried, not even when the node
/// capacity generation moves.
///
/// The control is the point. Without "and the generation moving changes
/// nothing", a terminal flag and a retryable one are indistinguishable in a
/// witness that only counts refusals, because nothing in it ever moves the
/// signal the retryable class watches.
#[test]
fn identity_exhaustion_is_terminal_and_outranks_a_moving_generation() {
    let f = fixture();
    let state = f.state(credentials(Vec::new()));
    let leased = ConsumerGrantSnapshot::empty();
    let spare = f.registry.new_family().expect("family");
    let parked = spare
        .demand(SlotKey {
            scope: PrivateAudienceScope::new(CapabilityAudienceScope::Grant {
                grant_id: [0x77; 32],
                audience_handle: [0x77; 32],
            })
            .expect("private"),
            capability: cap("nrpc:parked"),
        })
        .expect("parked");

    f.registry.exhaust_ids_for_test();
    assert_eq!(
        state.route_handle(&cap("nrpc:read"), &leased),
        RouteLookup::Cold(ColdReason::Refused(DemandRefused::IdSpaceExhausted))
    );
    assert_eq!(f.metrics.refused_id_space_exhausted(), 1);

    // Retire a slot: the node capacity generation moves, which is exactly the
    // signal `NodeAtCapacity` retries on.
    let before = f.registry.node_capacity_generation();
    drop(parked);
    assert_ne!(f.registry.node_capacity_generation(), before);

    for _ in 0..16 {
        assert_eq!(
            state.route_handle(&cap("nrpc:read"), &leased),
            RouteLookup::Cold(ColdReason::Refused(DemandRefused::IdSpaceExhausted))
        );
        assert_eq!(
            state.route_handle(&cap("nrpc:other"), &leased),
            RouteLookup::Cold(ColdReason::Refused(DemandRefused::IdSpaceExhausted))
        );
    }
    assert_eq!(
        f.metrics.refused_id_space_exhausted(),
        1,
        "terminal means the registry is never asked again, whatever moves"
    );
}

/// A refused entry retains NOTHING — no partial demand, no index entry.
#[test]
fn a_refused_entry_retains_no_partial_demand() {
    let f = fixture();
    let mut grants = Vec::new();
    let mut leased = ConsumerGrantSnapshot::empty();
    for i in 0..2u32 {
        let (grant, secret) = issue(cap("nrpc:wide"), GrantRights::DISCOVER);
        leased = lease(&leased, &grant, secret.expect("secret"), u64::from(i) + 1);
        grants.push(grant);
    }
    let state = f.state(credentials(grants));
    let leased_ref = &leased;

    // Spend 63 of 64 handles on grantless capabilities.
    for i in 0..63u32 {
        assert_eq!(
            state.route_handle(&cap(&format!("nrpc:c{i}")), leased_ref),
            RouteLookup::Warm
        );
    }
    assert_eq!(state.handles(), 63);
    let slots = f.registry.retained_slots();

    // `nrpc:wide` needs three demands (Owner + two leased audiences) and only
    // one handle remains.
    assert_eq!(
        state.route_handle(&cap("nrpc:wide"), leased_ref),
        RouteLookup::Cold(ColdReason::Refused(DemandRefused::FamilyAtCapacity))
    );
    assert_eq!(state.handles(), 63, "not one handle of the set was kept");
    assert_eq!(
        f.registry.retained_slots(),
        slots,
        "and no slot was created"
    );
    assert_eq!(state.entries(), 63, "and no index entry was published");
    assert!(
        state.warm(&cap("nrpc:wide")).is_none(),
        "a refused capability is not warm"
    );
}

/// Dropping the family releases every demand it held, and the last reference
/// retires the node slot.
#[test]
fn dropping_the_state_releases_every_demand() {
    let capability = cap("nrpc:read");
    let f = fixture();
    let (grant, secret) = issue(capability, GrantRights::DISCOVER);
    let leased = lease(
        &ConsumerGrantSnapshot::empty(),
        &grant,
        secret.expect("secret"),
        1,
    );
    let state = f.state(credentials(vec![grant]));
    assert_eq!(state.route_handle(&capability, &leased), RouteLookup::Warm);
    assert_eq!(f.registry.retained_slots(), 2);

    drop(state);
    assert_eq!(
        f.registry.retained_slots(),
        0,
        "ownership is the whole lifecycle — no separate teardown to forget"
    );
    assert_eq!(f.metrics.slots_retired(), 2);
}
