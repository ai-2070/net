//! OLB-2B.3b witnesses for the clone-family routing state.
//!
//! Four groups, each pinning one thing the design settles:
//!
//! - **§4 demand-set derivation** — Owner plus the audiences the family
//!   ACTUALLY LEASED for DISCOVER, and nothing else (W-N1, W-N2);
//! - **§4.2 demand-set currency** — the leased set moves under a warmed entry,
//!   and the entry follows it: installation, removal, rotation, and the refusal
//!   that must change nothing (W-L1..W-L5);
//! - **§8 lock-free lookup** — both zero-lock forms, an end-to-end counter and a
//!   real contention witness against the actual mutex;
//! - **§9 refusal policy** — spent-from-a-width, retryable-on-a-generation, and
//!   terminal, each with the control that stops a collapsed implementation
//!   passing (W-R1, W-R2).

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

/// A B→A DISCOVER grant over `capability` REUSING `grant_id`, with a freshly
/// minted audience binding.
///
/// Properly signed, so it installs through the same validation as any other: a
/// same-id replacement is a genuinely different signed authority, not a forgery
/// the record validator would reject. `try_issue` always allocates a fresh id,
/// which is why this drops to `issue_at`.
fn issue_reusing_id(
    capability: CapabilityAuthorityId,
    grant_id: [u8; 32],
) -> (Arc<OrgCapabilityGrant>, OrgAudienceSecret) {
    let now = current_timestamp();
    let (secret, binding) = OrgAudienceSecret::mint(grant_id);
    let grant = OrgCapabilityGrant::issue_at(
        &provider_org(),
        grant_id,
        consumer_org().org_id(),
        capability,
        GrantRights::DISCOVER,
        GrantTargetScope::AnyNodeOwnedBy(provider_org().org_id()),
        Some(binding),
        now.saturating_sub(60),
        now + 3600,
        // Vary the canonical bytes so this is not a byte-identical re-issue.
        u64::from(grant_id[0]) ^ now,
    );
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

// ------------------------------- §9: residual capacity under mixed widths

/// Fill `state` to exactly `handles` demands with grantless capabilities.
fn fill_to(state: &OrgRoutingState, leased: &ConsumerGrantSnapshot, handles: u32) {
    for i in 0..handles {
        assert_eq!(
            state.route_handle(&cap(&format!("nrpc:fill{i}")), leased),
            RouteLookup::Warm
        );
    }
    assert_eq!(state.handles(), handles as usize);
}

/// Two leased DISCOVER grants for each of `capabilities`, so every one of them
/// is a WIDTH-3 demand set (Owner + two audiences).
fn wide_credentials(
    capabilities: &[CapabilityAuthorityId],
) -> (FamilyDiscoveryCredentials, ConsumerGrantSnapshot) {
    let mut grants = Vec::new();
    let mut leased = ConsumerGrantSnapshot::empty();
    let mut seq = 0u64;
    for capability in capabilities {
        for _ in 0..2 {
            let (grant, secret) = issue(*capability, GrantRights::DISCOVER);
            seq += 1;
            leased = lease(&leased, &grant, secret.expect("secret"), seq);
            grants.push(grant);
        }
    }
    (credentials(grants), leased)
}

/// **The residual-capacity property.** A refusal of a WIDE demand set says
/// nothing about a NARROW one that still fits.
///
/// Dies to: one `family_at_capacity: bool` for the whole family. Demand sets
/// have variable width, so "this family was refused" is not "this family is
/// full" — at 62 of 64 a width-3 capability is refused and a width-1 one has
/// room. The flag suppressed every later set for the family's LIFETIME, having
/// never asked the registry, so the two spare handles became unreachable.
#[test]
fn a_wide_refusal_does_not_poison_residual_capacity() {
    let f = fixture();
    let wide = cap("nrpc:wide");
    let (credentials, leased) = wide_credentials(&[wide]);
    let state = f.state(credentials);

    // 62 of 64 spent: two handles remain.
    fill_to(&state, &leased, 62);
    let entries = state.entries();

    // `wide` needs Owner + two leased audiences = 3, and only 2 remain.
    assert_eq!(
        state.route_handle(&wide, &leased),
        RouteLookup::Cold(ColdReason::Refused(DemandRefused::FamilyAtCapacity)),
        "a width-3 set does not fit in two handles"
    );
    assert_eq!(state.handles(), 62, "and it retained none of them");
    assert_eq!(state.entries(), entries, "and published no entry");

    // The two spare handles are STILL SPENDABLE. This is the assertion the
    // family-global flag failed.
    assert_eq!(
        state.route_handle(&cap("nrpc:narrow"), &leased),
        RouteLookup::Warm,
        "a wide refusal must not globally poison residual capacity"
    );
    assert_eq!(state.handles(), 63);
    assert_eq!(state.entries(), entries + 1);
}

/// The other half, and the control that stops the repair becoming a licence to
/// hammer the registry: a refusal at width W settles every set of width >= W
/// LOCALLY, without asking the registry again.
///
/// Dies to: dropping the width record entirely (always attempt), which re-derives
/// and re-takes the node-wide registry lock on every later cold call — the waste
/// the sticky arm existed to prevent. Also dies to recording the WIDEST refused
/// width instead of the narrowest, which re-opens sets already proven not to fit.
#[test]
fn a_refusal_settles_every_wider_set_without_the_registry() {
    let f = fixture();
    let first = cap("nrpc:wide1");
    let second = cap("nrpc:wide2");
    let (credentials, leased) = wide_credentials(&[first, second]);
    let state = f.state(credentials);

    fill_to(&state, &leased, 62);
    assert_eq!(
        state.route_handle(&first, &leased),
        RouteLookup::Cold(ColdReason::Refused(DemandRefused::FamilyAtCapacity))
    );
    assert_eq!(f.metrics.refused_family_at_capacity(), 1);

    // A DIFFERENT capability of the same width. Provably cannot fit, so the
    // registry must not be asked.
    for _ in 0..32 {
        assert_eq!(
            state.route_handle(&second, &leased),
            RouteLookup::Cold(ColdReason::Refused(DemandRefused::FamilyAtCapacity))
        );
    }
    assert_eq!(
        f.metrics.refused_family_at_capacity(),
        1,
        "every set at or above the refused width is settled locally"
    );

    // Spending the residual capacity and then being refused at width 1 lowers
    // the record to its floor, after which everything is settled locally — the
    // old flag's exact behaviour, reached only once it is actually true.
    assert_eq!(
        state.route_handle(&cap("nrpc:narrow0"), &leased),
        RouteLookup::Warm
    );
    assert_eq!(
        state.route_handle(&cap("nrpc:narrow1"), &leased),
        RouteLookup::Warm
    );
    assert_eq!(state.handles(), 64, "the budget is now genuinely spent");
    assert_eq!(
        state.route_handle(&cap("nrpc:narrow2"), &leased),
        RouteLookup::Cold(ColdReason::Refused(DemandRefused::FamilyAtCapacity))
    );
    assert_eq!(f.metrics.refused_family_at_capacity(), 2);
    for _ in 0..32 {
        assert_eq!(
            state.route_handle(&cap("nrpc:narrow3"), &leased),
            RouteLookup::Cold(ColdReason::Refused(DemandRefused::FamilyAtCapacity))
        );
    }
    assert_eq!(
        f.metrics.refused_family_at_capacity(),
        2,
        "the registry is asked at most once per DISTINCT narrower width",
    );
}

// --------------------------- §4.2: the demand set is current, not first-seen

/// The retained Grant scopes of a warmed entry, for assertions about exactly
/// WHICH audience an entry holds.
fn retained_grant_scopes(
    state: &OrgRoutingState,
    capability: &CapabilityAuthorityId,
) -> Vec<([u8; 32], [u8; 32])> {
    state
        .warm(capability)
        .expect("warm")
        .demanded()
        .iter()
        .filter_map(|key| match key.scope.scope() {
            CapabilityAudienceScope::Grant {
                grant_id,
                audience_handle,
            } => Some((*grant_id, *audience_handle)),
            _ => None,
        })
        .collect()
}

/// **The lifecycle property.** An audience leased AFTER a capability warmed
/// joins that capability's demand set.
///
/// Dies to: returning the warmed entry on any hit without checking its lease
/// currency. `route_handle` accepts a `ConsumerGrantSnapshot` that changes under
/// it, so "actually leased" cannot mean "whatever happened to be installed on
/// the first miss" — the entry would keep answering with an Owner-only authority
/// set the family has since outgrown, which reads downstream as a route that
/// legitimately found no granted provider.
#[test]
fn a_newly_leased_audience_joins_a_warmed_entry() {
    let f = fixture();
    let capability = cap("nrpc:read");
    let (grant, secret) = issue(capability, GrantRights::DISCOVER);
    let state = f.state(credentials(vec![grant.clone()]));

    // The grant is held, but its audience secret is NOT installed. W-N1: Owner
    // only.
    let unleased = ConsumerGrantSnapshot::empty();
    assert_eq!(
        state.route_handle(&capability, &unleased),
        RouteLookup::Warm
    );
    assert_eq!(
        state.warm(&capability).expect("warm").demands().len(),
        1,
        "W-N1 — an uninstalled audience is not a demand"
    );
    assert_eq!(state.handles(), 1);

    // The exact audience is now leased.
    let leased = lease(&unleased, &grant, secret.expect("secret"), 1);
    assert_eq!(state.route_handle(&capability, &leased), RouteLookup::Warm);

    let entry = state.warm(&capability).expect("warm");
    assert_eq!(
        entry.demands().len(),
        2,
        "Owner + the newly leased Grant audience"
    );
    assert_eq!(
        entry.demanded(),
        &[owner_key(&capability), grant_key(&grant)],
        "and it is the EXACT scope that was leased"
    );
    assert_eq!(state.handles(), 2, "the superseded set was released");
    assert_eq!(state.entries(), 1, "one entry, re-derived — not two");
    assert_eq!(f.registry.retained_slots(), 2);
}

/// Removing the lease drops the scope it authorized: an obsolete audience is not
/// retained as part of the current complete demand set.
///
/// Dies to: checking only for NEWLY leased scopes (the "missing" direction), which
/// leaves a removed audience retained forever — a required contributor the source
/// can no longer serve, holding family and node budget for authority the family
/// no longer has.
#[test]
fn a_removed_audience_leaves_a_warmed_entry() {
    let f = fixture();
    let capability = cap("nrpc:read");
    let (grant, secret) = issue(capability, GrantRights::DISCOVER);
    let leased = lease(
        &ConsumerGrantSnapshot::empty(),
        &grant,
        secret.expect("secret"),
        1,
    );
    let state = f.state(credentials(vec![grant.clone()]));

    assert_eq!(state.route_handle(&capability, &leased), RouteLookup::Warm);
    assert_eq!(state.handles(), 2);
    assert_eq!(
        retained_grant_scopes(&state, &capability),
        vec![(
            grant.grant_id,
            grant.discovery.expect("binding").audience_handle
        )]
    );

    // The lease is removed.
    let removed = leased
        .without(&grant.grant_id)
        .expect("the record was there");
    assert_eq!(state.route_handle(&capability, &removed), RouteLookup::Warm);

    assert_eq!(
        state.warm(&capability).expect("warm").demanded(),
        &[owner_key(&capability)],
        "Owner alone — the obsolete Grant scope is gone"
    );
    assert!(
        retained_grant_scopes(&state, &capability).is_empty(),
        "an obsolete scope is not retained as the current complete demand set"
    );
    assert_eq!(state.handles(), 1, "and its handle was released");
    assert_eq!(
        f.registry.retained_slots(),
        1,
        "the last reference retired the node slot"
    );
}

/// A ROTATED audience — same `grant_id`, different handle — replaces the scope it
/// supersedes and cannot alias it.
///
/// Dies to: comparing lease currency on `grant_id` alone. The id is still
/// installed, so an id-keyed check calls the rotated-away scope current and the
/// family keeps demanding an audience that no longer exists while never demanding
/// the one that does. This is the aliasing the source seam closed at `cbbd448b3`,
/// arriving on the lifecycle side.
#[test]
fn a_rotated_audience_replaces_the_scope_it_supersedes() {
    let f = fixture();
    let capability = cap("nrpc:read");
    let (original, original_secret) = issue(capability, GrantRights::DISCOVER);
    // A genuinely signed replacement REUSING the id, over a different audience.
    let (rotated, successor_secret) = issue_reusing_id(capability, original.grant_id);
    let original_secret = Some(original_secret.expect("DISCOVER mints a secret"));
    let successor_secret = Some(successor_secret);
    let original_handle = original.discovery.expect("binding").audience_handle;
    let rotated_handle = rotated.discovery.expect("binding").audience_handle;
    assert_ne!(original_handle, rotated_handle);
    assert_eq!(original.grant_id, rotated.grant_id);

    // The family holds BOTH signed grants; only one of them can be leased at a
    // time, because the registry keys installed records by grant id.
    let leased = lease(
        &ConsumerGrantSnapshot::empty(),
        &original,
        original_secret.expect("secret"),
        1,
    );
    let state = f.state(credentials(vec![original.clone(), rotated.clone()]));

    assert_eq!(state.route_handle(&capability, &leased), RouteLookup::Warm);
    assert_eq!(
        retained_grant_scopes(&state, &capability),
        vec![(original.grant_id, original_handle)],
        "the ORIGINAL audience is what is leased"
    );

    // Rotate: the id is removed and re-installed under the successor's handle.
    let rotated_lease = lease(
        &leased
            .without(&original.grant_id)
            .expect("the record was there"),
        &rotated,
        successor_secret.expect("secret"),
        2,
    );
    assert_eq!(
        state.route_handle(&capability, &rotated_lease),
        RouteLookup::Warm
    );

    assert_eq!(
        retained_grant_scopes(&state, &capability),
        vec![(original.grant_id, rotated_handle)],
        "the id is unchanged, so ONLY the whole-scope comparison can see this"
    );
    assert_eq!(state.handles(), 2, "Owner + exactly one audience");
    assert_eq!(
        f.registry.retained_slots(),
        2,
        "the rotated-away slot retired; it did not accumulate beside its successor"
    );
}

/// A scope rotated away under an id the family still holds is NOT retained.
///
/// The narrow case the two-direction check exists for: the family holds ONLY the
/// original grant, so nothing new becomes demandable and the "missing" direction
/// stays silent. Only the OBSOLETE direction can notice, and only if it compares
/// the whole scope — the id is still installed.
///
/// Dies to: comparing `leased.get(grant_id).is_some()` in the obsolete
/// direction. The id is present, so the check passes, and the family goes on
/// demanding an audience that no longer exists — a required contributor the
/// source can only answer `Unserved`, held for the family's lifetime.
#[test]
fn a_rotated_away_scope_is_not_retained_under_its_own_id() {
    let f = fixture();
    let capability = cap("nrpc:read");
    let (original, original_secret) = issue(capability, GrantRights::DISCOVER);
    let original_handle = original.discovery.expect("binding").audience_handle;
    let leased = lease(
        &ConsumerGrantSnapshot::empty(),
        &original,
        original_secret.expect("secret"),
        1,
    );
    // The family's credentials hold the ORIGINAL only.
    let state = f.state(credentials(vec![original.clone()]));

    assert_eq!(state.route_handle(&capability, &leased), RouteLookup::Warm);
    assert_eq!(
        retained_grant_scopes(&state, &capability),
        vec![(original.grant_id, original_handle)]
    );
    assert_eq!(state.handles(), 2);

    // The id is re-installed under a DIFFERENT audience the family has no grant
    // for. Nothing becomes newly demandable; the retained scope simply died.
    let (successor, successor_secret) = issue_reusing_id(capability, original.grant_id);
    assert_ne!(
        successor.discovery.expect("binding").audience_handle,
        original_handle
    );
    let rotated = lease(
        &leased
            .without(&original.grant_id)
            .expect("the record was there"),
        &successor,
        successor_secret,
        2,
    );
    assert!(
        rotated.get(&original.grant_id).is_some(),
        "precondition: the ID is still installed — an id-keyed check sees nothing wrong"
    );

    assert_eq!(state.route_handle(&capability, &rotated), RouteLookup::Warm);
    assert!(
        retained_grant_scopes(&state, &capability).is_empty(),
        "the rotated-away scope is not retained under its own id"
    );
    assert_eq!(
        state.warm(&capability).expect("warm").demanded(),
        &[owner_key(&capability)]
    );
    assert_eq!(state.handles(), 1);
    assert_eq!(f.registry.retained_slots(), 1);
}

/// Concurrent re-derivations against one lease movement spend ONE demand set.
///
/// The lifecycle twin of `concurrent_misses_acquire_one_demand_set`, and it
/// drives `rederive` directly for the same reason that one drives `acquire`:
/// through `route_handle` the outcome is the scheduler's, because a rival that
/// arrives after the winner has published finds the entry current and returns
/// before reaching the section under test.
///
/// It asserts on the family BUDGET, not the end state: a duplicate re-derivation
/// is self-cleaning — the second publication displaces the first and the
/// displaced handle releases — so the final count is the same either way. What
/// does not clean up is the capacity refusal a redundant acquisition provokes.
///
/// Dies to: dropping the under-`mutate` re-check in `rederive`.
#[test]
fn concurrent_rederivations_spend_one_demand_set() {
    let f = fixture();
    let capability = cap("nrpc:read");
    let (grant, secret) = issue(capability, GrantRights::DISCOVER);
    let state = Arc::new(f.state(credentials(vec![grant.clone()])));

    let unleased = ConsumerGrantSnapshot::empty();
    assert_eq!(
        state.route_handle(&capability, &unleased),
        RouteLookup::Warm
    );
    // 62 of 64 spent, one of which is this capability's Owner demand. The
    // width-2 upgrade fits exactly once (62 + 2 = 64) and a second concurrent
    // one provably does not (63 + 2 = 65).
    for i in 0..61u32 {
        assert_eq!(
            state.route_handle(&cap(&format!("nrpc:fill{i}")), &unleased),
            RouteLookup::Warm
        );
    }
    assert_eq!(state.handles(), 62);

    let leased = Arc::new(lease(&unleased, &grant, secret.expect("secret"), 1));
    let stale = state.warm(&capability).expect("warm");
    let before = state.mutate_acquisitions();

    let start = Arc::new(std::sync::Barrier::new(4));
    let threads: Vec<_> = (0..4)
        .map(|_| {
            let state = state.clone();
            let start = start.clone();
            let leased = leased.clone();
            let stale = stale.clone();
            std::thread::spawn(move || {
                start.wait();
                state.rederive(&capability, &leased, stale)
            })
        })
        .collect();
    for t in threads {
        assert_eq!(
            t.join().expect("join"),
            RouteLookup::Warm,
            "every rival adopts the winner's re-derivation"
        );
    }
    drop(stale);

    assert_eq!(
        state.mutate_acquisitions() - before,
        4,
        "all four genuinely entered the mutation section"
    );
    assert_eq!(
        f.metrics.refused_family_at_capacity(),
        0,
        "a duplicate re-derivation would have overrun the budget and been refused"
    );
    assert_eq!(state.handles(), 63, "Owner + the new audience, once");
    assert_eq!(state.entries(), 62, "one entry per capability, re-derived");
    assert_eq!(
        state.warm(&capability).expect("warm").demanded(),
        &[owner_key(&capability), grant_key(&grant)]
    );
}

/// A re-derivation the family cannot afford changes NOTHING, and says so.
///
/// Two properties in one schedule because they are the same decision: the new set
/// is acquired BEFORE the old one is released, so a refusal leaves the entry
/// exactly as it was; and the refusal is REPORTED rather than papered over with
/// the stale entry, because answering `Warm` with a Grant plane known to be
/// incomplete is the silent authority narrowing §4.1 exists to prevent.
///
/// Dies to: releasing the superseded set before acquiring the replacement (the
/// entry is destroyed by a refusal), and to returning `Warm` from the stale entry
/// when the re-derivation is refused.
#[test]
fn a_refused_rederivation_leaves_the_entry_exactly_as_it_was() {
    let f = fixture();
    let capability = cap("nrpc:read");
    let (grant, secret) = issue(capability, GrantRights::DISCOVER);
    let state = f.state(credentials(vec![grant.clone()]));

    let unleased = ConsumerGrantSnapshot::empty();
    assert_eq!(
        state.route_handle(&capability, &unleased),
        RouteLookup::Warm
    );
    // Spend the family's whole budget, so the width-2 upgrade cannot fit.
    for i in 0..63u32 {
        assert_eq!(
            state.route_handle(&cap(&format!("nrpc:c{i}")), &unleased),
            RouteLookup::Warm
        );
    }
    assert_eq!(state.handles(), 64);
    let entries = state.entries();
    let slots = f.registry.retained_slots();

    let leased = lease(&unleased, &grant, secret.expect("secret"), 1);
    assert_eq!(
        state.route_handle(&capability, &leased),
        RouteLookup::Cold(ColdReason::Refused(DemandRefused::FamilyAtCapacity)),
        "the upgrade is refused, and refusal is what the caller is told"
    );

    assert_eq!(
        state.handles(),
        64,
        "nothing was released for a refused set"
    );
    assert_eq!(state.entries(), entries);
    assert_eq!(f.registry.retained_slots(), slots);
    assert_eq!(
        state.warm(&capability).expect("still retained").demanded(),
        &[owner_key(&capability)],
        "the entry is exactly what it was, still retained, still Owner-only"
    );

    // And the registry is not asked again for a width already proven not to fit.
    let asked = f.metrics.refused_family_at_capacity();
    for _ in 0..16 {
        assert_eq!(
            state.route_handle(&capability, &leased),
            RouteLookup::Cold(ColdReason::Refused(DemandRefused::FamilyAtCapacity))
        );
    }
    assert_eq!(f.metrics.refused_family_at_capacity(), asked);
}

/// Capacity a SUPERSESSION frees is spendable again.
///
/// The one path on which a family's handle count can FALL: re-derivation
/// publishes a narrower set and releases the wider one it replaced. The width
/// record's soundness rests on the count never falling, so this is exactly where
/// it has to be forgotten.
///
/// Dies to: keeping the width record across a supersession. The freed handles
/// are real and the registry would grant them, but the family never asks — the
/// same unreachable-residual-capacity defect the width model repaired, arriving
/// through the back door.
#[test]
fn capacity_freed_by_a_supersession_is_spendable_again() {
    let f = fixture();
    let wide = cap("nrpc:wide");
    let (grant, secret) = issue(wide, GrantRights::DISCOVER);
    let leased = lease(
        &ConsumerGrantSnapshot::empty(),
        &grant,
        secret.expect("secret"),
        1,
    );
    // A second width-2 capability, so there is something to be refused and then
    // admitted once the supersession frees a handle.
    let other = cap("nrpc:other");
    let (other_grant, other_secret) = issue(other, GrantRights::DISCOVER);
    let leased = lease(&leased, &other_grant, other_secret.expect("secret"), 2);
    let state = f.state(credentials(vec![grant.clone(), other_grant.clone()]));

    // `wide` is width 2 (Owner + the leased audience); 61 grantless fillers take
    // the family to 63 of 64.
    assert_eq!(state.route_handle(&wide, &leased), RouteLookup::Warm);
    for i in 0..61u32 {
        assert_eq!(
            state.route_handle(&cap(&format!("nrpc:f{i}")), &leased),
            RouteLookup::Warm
        );
    }
    assert_eq!(state.handles(), 63);
    assert_eq!(
        state.route_handle(&other, &leased),
        RouteLookup::Cold(ColdReason::Refused(DemandRefused::FamilyAtCapacity)),
        "63 + 2 does not fit"
    );

    // Remove `wide`'s lease. Its entry supersedes down to Owner alone, which
    // FREES a handle — the one path on which this family's count falls.
    let removed = leased
        .without(&grant.grant_id)
        .expect("the record was there");
    assert_eq!(state.route_handle(&wide, &removed), RouteLookup::Warm);
    assert_eq!(state.handles(), 62, "the superseded audience was released");

    // `other` is still width 2 under `removed`, and 62 + 2 = 64 now fits.
    assert_eq!(
        state.route_handle(&other, &removed),
        RouteLookup::Warm,
        "capacity freed by a supersession must be reachable again"
    );
    assert_eq!(state.handles(), 64);
}

/// The currency check is on the LOCK-FREE path: a warmed entry that is still
/// current takes `mutate` zero times, WITH a leased Grant audience in it.
///
/// Dies to: performing the currency check under the mutation lock. The existing
/// zero-lock witnesses run a grantless family, whose check has nothing to
/// compare — this one has a real Grant plane and still takes no lock.
#[test]
fn a_current_warmed_entry_with_a_leased_audience_takes_no_lock() {
    let f = fixture();
    let capability = cap("nrpc:read");
    let (grant, secret) = issue(capability, GrantRights::DISCOVER);
    let leased = lease(
        &ConsumerGrantSnapshot::empty(),
        &grant,
        secret.expect("secret"),
        1,
    );
    let state = f.state(credentials(vec![grant]));

    assert_eq!(state.route_handle(&capability, &leased), RouteLookup::Warm);
    let after_miss = state.mutate_acquisitions();
    assert_eq!(after_miss, 1);

    for _ in 0..1_000 {
        assert_eq!(state.route_handle(&capability, &leased), RouteLookup::Warm);
    }
    assert_eq!(
        state.mutate_acquisitions(),
        after_miss,
        "a current entry is answered without the mutation lock, Grant plane and all"
    );
}

/// The per-capability index and the full credential scan derive the SAME set.
///
/// The index exists so the warmed currency check never walks the family's whole
/// credential set. It is a filter of that set, not a second admission rule, and
/// this is what says so: every capability, both sources, same answer.
///
/// Dies to: indexing on anything but the capability — including "helpfully"
/// pre-filtering on rights or lease state, which would move a decision out of
/// `classify` and let the two derivations disagree.
#[test]
fn the_capability_index_derives_exactly_what_the_full_scan_derives() {
    let f = fixture();
    let read = cap("nrpc:read");
    let write = cap("nrpc:write");
    let mut grants = Vec::new();
    let mut leased = ConsumerGrantSnapshot::empty();

    // TWO leased DISCOVER grants for `read`, so dropping ANY ONE of them from
    // the index changes the derived set — an index holding only the first grant
    // per capability, or only the last, is a different answer and not a subset
    // that happens to look the same. Beside them an INVOKE-only grant and an
    // unleased DISCOVER grant, which contribute nothing from either source.
    let mut seq = 0u64;
    for _ in 0..2 {
        let (grant, secret) = issue(read, GrantRights::DISCOVER);
        seq += 1;
        leased = lease(&leased, &grant, secret.expect("secret"), seq);
        grants.push(grant);
    }
    grants.push(issue(read, GrantRights::INVOKE).0);
    grants.push(issue(read, GrantRights::DISCOVER).0);
    let (leased_write, secret) = issue(write, GrantRights::DISCOVER);
    leased = lease(&leased, &leased_write, secret.expect("secret"), seq + 1);
    grants.push(leased_write);

    let credentials = credentials(grants);
    let state = f.state(credentials.clone());

    for capability in [read, write, cap("nrpc:absent")] {
        assert_eq!(
            state.demand_set(&capability, &leased),
            demand_set_for(&credentials, &capability, &leased),
            "the index is a filter of the credential set, never a second rule"
        );
    }
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
