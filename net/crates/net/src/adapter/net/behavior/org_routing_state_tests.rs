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
//! - **§9 refusal pass-through** — a refusal is the REGISTRY's verdict,
//!   reported verbatim and memoized nowhere: a full family or node refuses, a
//!   demand that needs none of the exhausted resource warms, and recovery
//!   takes exactly one moved input, never a cache invalidation.

#![allow(clippy::disallowed_methods)]

use super::*;
use crate::adapter::net::behavior::org::{current_timestamp, OrgKeypair};
use crate::adapter::net::behavior::org_grant::{GrantRights, GrantTargetScope, OrgAudienceSecret};
use crate::adapter::net::behavior::org_grant_registry::{
    validate_consumer_record, PreparedInstall,
};
use crate::adapter::net::behavior::org_routing::RegistryWork;
use crate::adapter::net::behavior::org_routing_registry::{
    DemandHandle, GrantArtifactFence, GrantMovementFence, NodeOrgRoutingRegistry, RegistryMetrics,
    ScopedDiscoveryAuthorityStamp, ScopedSourceFacts, SlotSource, SourceCommitPin, SourceFacts,
    SourceSnapshot, SourceToken, MAX_NODE_SLOTS,
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
        PreparedInstall::Ready(slot) => ConsumerGrantSnapshot::finish_install(*slot, install_seq)
            // Synthetic snapshots stamp their transition explicitly: the
            // publication seam is not in play here, and an unstamped snapshot
            // would order against every other one by accident.
            .stamped(GrantMovementFence::Publication(install_seq)),
        PreparedInstall::Noop => panic!("the witness installs a fresh grant"),
    }
}

/// Remove `grant_id`, stamping the removal as publication `revision`.
fn remove(
    snapshot: &ConsumerGrantSnapshot,
    grant_id: &[u8; 32],
    revision: u64,
) -> ConsumerGrantSnapshot {
    snapshot
        .without(grant_id)
        .expect("the record was there")
        .stamped(GrantMovementFence::Publication(revision))
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

    // A DIFFERENT, PROPERLY SIGNED grant reusing the id, under a fresh audience
    // handle. Its scope is not leased, because the installed record's handle is
    // the other one.
    //
    // Built through `issue_reusing_id` rather than by editing a clone's binding:
    // an edited clone is a grant no validator would accept, so a witness driven
    // by one can pass while the production path — where a same-id rotation is a
    // genuinely different signed authority that installs through
    // `validate_consumer_record` — is never exercised at all.
    let (rotated, _successor_secret) = issue_reusing_id(capability, grant.grant_id);
    assert_eq!(rotated.grant_id, grant.grant_id, "the id is REUSED");
    assert_ne!(
        rotated.discovery.as_ref().expect("binding").audience_handle,
        grant.discovery.as_ref().expect("binding").audience_handle,
        "under a different handle"
    );

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
///   is satisfied by the defect. What does NOT clean up is the REGISTRY refusal
///   a transient over-spend provokes, which the refusal metric records and a
///   correct run never produces.
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
        "no rival was refused at the registry"
    );

    // And the family still has room for its 64th demand.
    assert_eq!(
        state.route_handle(&cap("nrpc:after"), &leased),
        RouteLookup::Warm,
        "a transient over-spend would have been refused above"
    );
    assert_eq!(state.handles(), 64);
}

// -------------------------------------------- §9: refusal is pass-through

/// A spent family budget refuses, and the verdict is the REGISTRY's: every
/// cold attempt asks it again, because no local record can know what the next
/// derived set will cost.
///
/// Dies to: memoizing the refusal — the removed family-global cache answered
/// later, differently-shaped sets from an earlier refusal, having never asked
/// the registry (§9).
#[test]
fn a_spent_family_budget_refuses_from_the_registry_every_time() {
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

    for i in 0..3u32 {
        assert_eq!(
            state.route_handle(&cap(&format!("nrpc:over{i}")), &leased),
            RouteLookup::Cold(ColdReason::Refused(DemandRefused::FamilyAtCapacity)),
        );
    }
    assert_eq!(
        f.metrics.refused_family_at_capacity(),
        3,
        "every attempt is the registry's refusal, not a replayed one"
    );
    // A refusal is bounded degradation, never a dead family: warmed entries
    // keep serving.
    assert_eq!(
        state.route_handle(&cap("nrpc:c0"), &leased),
        RouteLookup::Warm
    );
}

/// **The residual-capacity schedule, kept executable.** A refusal of a WIDE
/// demand set says nothing about a NARROW one that still fits: at 62 of 64 a
/// width-3 capability is refused, and a width-1 capability must still warm.
///
/// This is the exact cross-capability regression that justified removing the
/// family-global refusal record — one wide refusal answered every later
/// capability for the family's lifetime, having never asked the registry, so
/// two spendable handles became permanently unreachable. The no-cache design
/// makes the behavior structurally likely, not guaranteed, which is why the
/// schedule stays a witness: it dies to ANY reintroduced family-wide refusal
/// record, whatever its key.
#[test]
fn a_wide_refusal_does_not_poison_residual_capacity() {
    let f = fixture();
    let wide = cap("nrpc:wide");
    let mut grants = Vec::new();
    let mut leased = ConsumerGrantSnapshot::empty();
    for i in 0..2u32 {
        let (grant, secret) = issue(wide, GrantRights::DISCOVER);
        leased = lease(&leased, &grant, secret.expect("secret"), u64::from(i) + 1);
        grants.push(grant);
    }
    let state = f.state(credentials(grants));

    // 62 of 64 spent: two handles remain.
    fill_to_total(&state, &leased, 62);
    let entries = state.entries();

    // `wide` needs Owner + two leased audiences = 3, and only 2 remain.
    assert_eq!(
        state.route_handle(&wide, &leased),
        RouteLookup::Cold(ColdReason::Refused(DemandRefused::FamilyAtCapacity)),
        "a width-3 set does not fit in two handles"
    );
    assert_eq!(state.handles(), 62, "and it retained none of them");
    assert_eq!(state.entries(), entries, "and published no entry");

    // The two spare handles are STILL SPENDABLE by a DIFFERENT capability.
    assert_eq!(
        state.route_handle(&cap("nrpc:narrow"), &leased),
        RouteLookup::Warm,
        "a wide refusal must not poison residual capacity"
    );
    assert_eq!(state.handles(), 63);
    assert_eq!(state.entries(), entries + 1);
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

/// A full node refuses `NodeAtCapacity` on every attempt, and ONE retirement
/// is what frees the next one — no cache invalidation in between, because
/// there is no cache.
///
/// Dies to: memoizing the refusal on the node capacity generation — the
/// removed family-global cache answered every capability's next miss from one
/// capability's refusal, and a generation that stands still suppressed demand
/// whose shape had since changed (§9).
#[test]
fn a_full_node_refuses_every_attempt_until_a_retirement() {
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
    for attempt in 1..=3u64 {
        assert_eq!(
            state.route_handle(&capability, &leased),
            RouteLookup::Cold(ColdReason::Refused(DemandRefused::NodeAtCapacity))
        );
        assert_eq!(
            f.metrics.refused_node_at_capacity(),
            attempt,
            "every attempt is the registry's refusal, not a replayed one"
        );
    }

    // Free one slot: the very next attempt succeeds on the freed capacity.
    held.pop();
    assert_eq!(f.registry.retained_slots(), MAX_NODE_SLOTS - 1);
    assert_eq!(
        state.route_handle(&capability, &leased),
        RouteLookup::Warm,
        "one retirement is the whole recovery story"
    );
    assert_eq!(state.entries(), 1);
}

/// A demand that narrows to slots that ALREADY EXIST warms on a full node
/// with no retirement at all: `NodeAtCapacity` is a statement about CREATING
/// slots, never about sharing ones another family retains.
///
/// This is the schedule the removed cache got wrong (§9): a family refused for
/// `Owner + Grant` whose Grant lease is then removed derives `Owner` alone —
/// a key another family already retains, so a full node is no obstacle to it
/// — but nothing retired, so the cached generation stood still and suppressed
/// an attempt the registry would have granted.
#[test]
fn a_narrowed_demand_warms_on_a_full_node_without_a_retirement() {
    let f = fixture();
    let capability = cap("nrpc:read");
    let (grant, secret) = issue(capability, GrantRights::DISCOVER);
    let leased = lease(
        &ConsumerGrantSnapshot::empty(),
        &grant,
        secret.expect("DISCOVER mints a secret"),
        1,
    );

    // The node is FULL, and one of the slots filling it is the very Owner key
    // this family will need. That is what makes the narrowed set free of node
    // capacity: it creates nothing, so being full cannot refuse it.
    let mut held = Vec::new();
    let mut fillers = Vec::new();
    for chunk in 0..4u32 {
        let filler = f.registry.new_family().expect("family");
        for i in 0..64u32 {
            let key = if (chunk, i) == (0, 0) {
                owner_key(&capability)
            } else {
                SlotKey {
                    scope: PrivateAudienceScope::new(CapabilityAudienceScope::Grant {
                        grant_id: [0x5A; 32],
                        audience_handle: [0x5A; 32],
                    })
                    .expect("private"),
                    capability: cap(&format!("nrpc:fill{}", chunk * 64 + i)),
                }
            };
            held.push(filler.demand(key).expect("fill"));
        }
        fillers.push(filler);
    }
    assert_eq!(f.registry.retained_slots(), MAX_NODE_SLOTS);

    let state = f.state(credentials(vec![grant.clone()]));
    assert_eq!(
        state.route_handle(&capability, &leased),
        RouteLookup::Cold(ColdReason::Refused(DemandRefused::NodeAtCapacity)),
        "Owner + Grant needs a slot a full node cannot create"
    );
    let generation = f.registry.node_capacity_generation();

    // The Grant lease goes away. NOTHING retires: the node is still full and the
    // capacity generation stands exactly where the refusal left it.
    let narrowed = remove(&leased, &grant.grant_id, 2);
    assert_eq!(
        state.route_handle(&capability, &narrowed),
        RouteLookup::Warm,
        "the narrowed set creates no slot, so the full node cannot refuse it"
    );
    assert_eq!(
        f.registry.node_capacity_generation(),
        generation,
        "and it warmed without a single retirement"
    );
    assert_eq!(state.entries(), 1);
    assert_eq!(state.handles(), 1, "Owner alone; the Grant scope is gone");
    assert_eq!(
        f.registry.retained_slots(),
        MAX_NODE_SLOTS,
        "it SHARES the owner slot rather than creating a 257th"
    );
    drop(held);
}

/// An exhausted identity space refuses a set that needs a NEW slot — the
/// registry's verdict, reported verbatim on every attempt.
#[test]
fn identity_exhaustion_refuses_a_set_that_needs_a_new_slot() {
    let f = fixture();
    let state = f.state(credentials(Vec::new()));
    let leased = ConsumerGrantSnapshot::empty();

    f.registry.exhaust_ids_for_test();
    assert_eq!(
        state.route_handle(&cap("nrpc:read"), &leased),
        RouteLookup::Cold(ColdReason::Refused(DemandRefused::IdSpaceExhausted))
    );
    assert_eq!(f.metrics.refused_id_space_exhausted(), 1);
}

/// The control, and the exact wrongness the removed terminal cache had (§9):
/// exhaustion means "cannot MINT another slot identity", never "cannot acquire
/// an existing slot". A capability whose whole demand set is already retained
/// through another family allocates no identity and must warm.
///
/// Dies to: a family-global terminal flag, which promoted one capability's
/// marginal identity failure into a family-wide routing verdict for demand
/// that needed no identity at all.
#[test]
fn identity_exhaustion_does_not_refuse_a_set_of_existing_slots() {
    let f = fixture();
    let capability = cap("nrpc:read");
    let holder = f.registry.new_family().expect("family");
    let _preheld = holder.demand(owner_key(&capability)).expect("pre-retains");
    let state = f.state(credentials(Vec::new()));
    let leased = ConsumerGrantSnapshot::empty();

    f.registry.exhaust_ids_for_test();
    // An unrelated capability that DOES need a new slot is refused first, so a
    // family-global terminal record — were one to exist — would be armed.
    assert_eq!(
        state.route_handle(&cap("nrpc:unrelated"), &leased),
        RouteLookup::Cold(ColdReason::Refused(DemandRefused::IdSpaceExhausted))
    );
    assert_eq!(
        state.route_handle(&capability, &leased),
        RouteLookup::Warm,
        "no identity is allocated for a slot that already exists"
    );
    assert_eq!(state.handles(), 1);
    assert_eq!(
        f.registry.retained_slots(),
        1,
        "it SHARES the pre-retained slot rather than creating one"
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
    let identities = f.registry.allocated_ids_for_test();

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
    // And NO IDENTITY was consumed. This is the half a handle/slot/entry check
    // cannot see: acquiring per key in a loop allocates an incarnation for each
    // new slot BEFORE a later key discovers the refusal, and the prefix it then
    // drops gives the handles and the slots back — but never the identity. The
    // monotone id space is finite and terminal, so a refusal that burns one is a
    // permanent cost paid for retaining nothing, and repeated refusals against a
    // full family would drain it (W-A5's property, at the STATE layer).
    assert_eq!(
        f.registry.allocated_ids_for_test(),
        identities,
        "a refused entry consumes no identity either"
    );
}

/// Warm grantless capabilities until the family holds exactly `target` handles.
fn fill_to_total(state: &OrgRoutingState, leased: &ConsumerGrantSnapshot, target: usize) {
    let mut seed = 0u32;
    while state.handles() < target {
        assert_eq!(
            state.route_handle(&cap(&format!("nrpc:top{seed}")), leased),
            RouteLookup::Warm
        );
        seed += 1;
    }
    assert_eq!(state.handles(), target);
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
    let removed = remove(&leased, &grant.grant_id, 100);
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
    let original_secret = original_secret.expect("DISCOVER mints a secret");
    let original_handle = original.discovery.expect("binding").audience_handle;
    let rotated_handle = rotated.discovery.expect("binding").audience_handle;
    assert_ne!(original_handle, rotated_handle);
    assert_eq!(original.grant_id, rotated.grant_id);

    // The family holds BOTH signed grants; only one of them can be leased at a
    // time, because the registry keys installed records by grant id.
    let leased = lease(
        &ConsumerGrantSnapshot::empty(),
        &original,
        original_secret,
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
        &remove(&leased, &original.grant_id, 100),
        &rotated,
        successor_secret,
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
        &remove(&leased, &original.grant_id, 100),
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
    let before = state.mutate_acquisitions();

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
            "every rival adopts the winner's re-derivation"
        );
    }
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
    // The release responsibility was TRANSFERRED exactly once: the live entry
    // owes its whole set, and no rival left a second set owing the same keys.
    assert_eq!(
        state
            .warm(&capability)
            .expect("warm")
            .demands()
            .held_for_test(),
        vec![owner_key(&capability), grant_key(&grant)],
        "the surviving set owes exactly what it names"
    );
    // And the family still spends its last handle.
    assert_eq!(
        state.route_handle(&cap("nrpc:after"), &leased),
        RouteLookup::Warm,
        "a duplicate transfer would have been refused above"
    );
    assert_eq!(state.handles(), 64);
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

    // Every later attempt is the registry's refusal again — reported, never
    // replayed — and each one still changes nothing.
    assert_eq!(
        state.route_handle(&capability, &leased),
        RouteLookup::Cold(ColdReason::Refused(DemandRefused::FamilyAtCapacity))
    );
    assert_eq!(state.handles(), 64);
}

// ---------------- §4.3: replacement is charged on the PROJECTED footprint

/// Retain distinct node slots under throwaway families until the node holds
/// exactly `target`, returning the handles that keep them alive.
fn fill_node_to(f: &Fixture, target: usize) -> Vec<DemandHandle> {
    let mut held = Vec::new();
    let mut family = f.registry.new_family().expect("family");
    let mut in_family = 0usize;
    let mut seed = 0u32;
    while f.registry.retained_slots() < target {
        if in_family == MAX_HANDLES_PER_FAMILY {
            family = f.registry.new_family().expect("family");
            in_family = 0;
        }
        held.push(
            family
                .demand(SlotKey {
                    scope: PrivateAudienceScope::new(CapabilityAudienceScope::Grant {
                        grant_id: [0xF1; 32],
                        audience_handle: [0xF1; 32],
                    })
                    .expect("private"),
                    capability: cap(&format!("nrpc:nodefill{seed}")),
                })
                .expect("fill"),
        );
        in_family += 1;
        seed += 1;
    }
    assert_eq!(f.registry.retained_slots(), target);
    held
}

/// **The reviewer's schedule.** A family at its hard bound whose capability
/// LOSES an audience projects to `64 - 2 + 1 = 63` and must succeed.
///
/// Dies to: acquiring the replacement set beside the superseded one and dropping
/// it afterwards. That charges the transient GROSS peak — every replacement
/// handle while every superseded handle is still charged — so this asks for 66
/// and is refused at a bound its final footprint sits comfortably inside. The
/// entry is then stuck: it can never shed the obsolete scope, because shedding
/// it requires capacity the shedding itself would free.
#[test]
fn a_narrowing_replacement_at_the_family_bound_is_charged_net() {
    let f = fixture();
    let changing = cap("nrpc:changing");
    let (grant, secret) = issue(changing, GrantRights::DISCOVER);
    let leased = lease(
        &ConsumerGrantSnapshot::empty(),
        &grant,
        secret.expect("secret"),
        1,
    );
    let state = f.state(credentials(vec![grant.clone()]));

    assert_eq!(state.route_handle(&changing, &leased), RouteLookup::Warm);
    assert_eq!(state.handles(), 2, "Owner + one leased audience");
    fill_to_total(&state, &leased, 64);
    assert_eq!(state.handles(), 64, "the family is EXACTLY at its bound");

    // The audience goes away. Projected: 64 - 2 + 1 = 63.
    let removed = remove(&leased, &grant.grant_id, 100);
    assert_eq!(
        state.route_handle(&changing, &removed),
        RouteLookup::Warm,
        "replacement capacity must be charged net of the entry it supersedes"
    );
    assert_eq!(state.handles(), 63);
    assert_eq!(
        state.warm(&changing).expect("warm").demanded(),
        &[owner_key(&changing)],
        "and the obsolete Grant scope is gone"
    );
    assert_eq!(f.metrics.refused_family_at_capacity(), 0);
}

/// A SAME-WIDTH rotation at the hard bound: `64 - 2 + 2 = 64` succeeds.
///
/// The case a "credit the old set first" shortcut gets wrong in the other
/// direction, and the one that proves the intersection is preserved rather than
/// released and re-taken: only the Grant scope moves, and the Owner scope's
/// reference is never given up, so the family never dips below its bound and
/// never rises above it.
#[test]
fn a_same_width_rotation_at_the_family_bound_is_charged_net() {
    let f = fixture();
    let changing = cap("nrpc:changing");
    let (old_grant, old_secret) = issue(changing, GrantRights::DISCOVER);
    let (new_grant, new_secret) = issue(changing, GrantRights::DISCOVER);
    let leased = lease(
        &ConsumerGrantSnapshot::empty(),
        &old_grant,
        old_secret.expect("secret"),
        1,
    );
    let state = f.state(credentials(vec![old_grant.clone(), new_grant.clone()]));

    assert_eq!(state.route_handle(&changing, &leased), RouteLookup::Warm);
    fill_to_total(&state, &leased, 64);
    assert_eq!(state.handles(), 64);
    let slots_before = f.registry.retained_slots();

    // Rotate the audience: the old lease is removed and the successor installed.
    let rotated = lease(
        &remove(&leased, &old_grant.grant_id, 100),
        &new_grant,
        new_secret.expect("secret"),
        2,
    );
    assert_eq!(
        state.route_handle(&changing, &rotated),
        RouteLookup::Warm,
        "a same-width rotation neither grows nor shrinks the footprint"
    );

    assert_eq!(state.handles(), 64, "still exactly at the bound");
    assert_eq!(
        state.warm(&changing).expect("warm").demanded(),
        &[owner_key(&changing), grant_key(&new_grant)],
        "the successor audience is retained"
    );
    assert!(
        !state
            .warm(&changing)
            .expect("warm")
            .demanded()
            .contains(&grant_key(&old_grant)),
        "and the superseded audience is not"
    );
    assert_eq!(
        f.registry.retained_slots(),
        slots_before,
        "one slot retired as one was created"
    );
    assert_eq!(f.metrics.refused_family_at_capacity(), 0);
}

/// At the NODE bound, a rotation whose old exact slot RETIRES projects to
/// `256 - 1 + 1 = 256` and succeeds.
///
/// Dies to: judging the new slot against a transient `256 + 1`. The old Grant
/// slot is this family's alone, so the replacement retires it and the node's
/// final retained count is unchanged — refusing here would strand every rotation
/// on a full node forever, since no rotation can free the slot it is replacing
/// without being allowed to replace it.
#[test]
fn a_rotation_at_the_node_bound_retires_the_slot_it_transfers() {
    let f = fixture();
    let changing = cap("nrpc:changing");
    let (old_grant, old_secret) = issue(changing, GrantRights::DISCOVER);
    let (new_grant, new_secret) = issue(changing, GrantRights::DISCOVER);
    let leased = lease(
        &ConsumerGrantSnapshot::empty(),
        &old_grant,
        old_secret.expect("secret"),
        1,
    );
    let state = f.state(credentials(vec![old_grant.clone(), new_grant.clone()]));
    assert_eq!(state.route_handle(&changing, &leased), RouteLookup::Warm);

    let _fillers = fill_node_to(&f, MAX_NODE_SLOTS);
    assert_eq!(f.registry.retained_slots(), MAX_NODE_SLOTS);

    let rotated = lease(
        &remove(&leased, &old_grant.grant_id, 100),
        &new_grant,
        new_secret.expect("secret"),
        2,
    );
    assert_eq!(
        state.route_handle(&changing, &rotated),
        RouteLookup::Warm,
        "the old exact slot retires as the new one is created"
    );
    assert_eq!(
        f.registry.retained_slots(),
        MAX_NODE_SLOTS,
        "the node's final retained count is unchanged"
    );
    assert_eq!(
        state.warm(&changing).expect("warm").demanded(),
        &[owner_key(&changing), grant_key(&new_grant)]
    );
    assert_eq!(f.metrics.refused_node_at_capacity(), 0);
}

/// The CONTROL for the case above: when the old exact slot is SHARED, it does
/// not retire, so the replacement genuinely needs a 257th slot and must refuse.
///
/// Without this, "credit every old-only slot" passes the retiring case and
/// silently admits a slot past the node bound whenever another family still
/// wants the one being replaced.
#[test]
fn a_rotation_at_the_node_bound_refuses_when_the_old_slot_is_shared() {
    let f = fixture();
    let changing = cap("nrpc:changing");
    let (old_grant, old_secret) = issue(changing, GrantRights::DISCOVER);
    let (new_grant, new_secret) = issue(changing, GrantRights::DISCOVER);
    let leased = lease(
        &ConsumerGrantSnapshot::empty(),
        &old_grant,
        old_secret.expect("secret"),
        1,
    );
    let state = f.state(credentials(vec![old_grant.clone(), new_grant.clone()]));
    assert_eq!(state.route_handle(&changing, &leased), RouteLookup::Warm);

    // A SECOND owner of the very slot the rotation would give up.
    let sharer = f.registry.new_family().expect("family");
    let _shared = sharer
        .demand(grant_key(&old_grant))
        .expect("the slot is already retained, so this shares it");

    let _fillers = fill_node_to(&f, MAX_NODE_SLOTS);
    let handles_before = state.handles();
    let entries_before = state.entries();

    let rotated = lease(
        &remove(&leased, &old_grant.grant_id, 100),
        &new_grant,
        new_secret.expect("secret"),
        2,
    );
    assert_eq!(
        state.route_handle(&changing, &rotated),
        RouteLookup::Cold(ColdReason::Refused(DemandRefused::NodeAtCapacity)),
        "the shared old slot frees nothing, so the new one is genuinely a 257th"
    );

    assert_eq!(f.registry.retained_slots(), MAX_NODE_SLOTS, "no slot moved");
    assert_eq!(state.handles(), handles_before, "no handle moved");
    assert_eq!(state.entries(), entries_before);
    assert_eq!(
        state.warm(&changing).expect("still retained").demanded(),
        &[owner_key(&changing), grant_key(&old_grant)],
        "and the superseded entry is exactly what it was"
    );
}

/// Identity exhaustion during a replacement that needs a NEW slot refuses with
/// total no effect, and the old complete entry survives.
#[test]
fn identity_exhaustion_during_a_replacement_refuses_with_no_effect() {
    let f = fixture();
    let changing = cap("nrpc:changing");
    let (old_grant, old_secret) = issue(changing, GrantRights::DISCOVER);
    let (new_grant, new_secret) = issue(changing, GrantRights::DISCOVER);
    let leased = lease(
        &ConsumerGrantSnapshot::empty(),
        &old_grant,
        old_secret.expect("secret"),
        1,
    );
    let state = f.state(credentials(vec![old_grant.clone(), new_grant.clone()]));
    assert_eq!(state.route_handle(&changing, &leased), RouteLookup::Warm);
    let handles_before = state.handles();
    let slots_before = f.registry.retained_slots();

    f.registry.exhaust_ids_for_test();

    let rotated = lease(
        &remove(&leased, &old_grant.grant_id, 100),
        &new_grant,
        new_secret.expect("secret"),
        2,
    );
    assert_eq!(
        state.route_handle(&changing, &rotated),
        RouteLookup::Cold(ColdReason::Refused(DemandRefused::IdSpaceExhausted)),
    );
    assert_eq!(f.metrics.refused_id_space_exhausted(), 1);
    assert_eq!(state.handles(), handles_before, "nothing was released");
    assert_eq!(f.registry.retained_slots(), slots_before);
    assert_eq!(
        state.warm(&changing).expect("still retained").demanded(),
        &[owner_key(&changing), grant_key(&old_grant)],
        "the old complete entry survives an exhausted replacement"
    );
}

/// The CONTROL: a replacement that needs no fresh identity still succeeds after
/// exhaustion.
///
/// Without it, "refuse every replacement once exhausted" passes the witness
/// above. A narrowing replacement creates no slot, so the terminal identity
/// space has nothing to say about it — and refusing would leave the family
/// permanently unable to shed an obsolete scope.
#[test]
fn a_narrowing_replacement_needs_no_identity_after_exhaustion() {
    let f = fixture();
    let changing = cap("nrpc:changing");
    let (grant, secret) = issue(changing, GrantRights::DISCOVER);
    let leased = lease(
        &ConsumerGrantSnapshot::empty(),
        &grant,
        secret.expect("secret"),
        1,
    );
    let state = f.state(credentials(vec![grant.clone()]));
    assert_eq!(state.route_handle(&changing, &leased), RouteLookup::Warm);
    assert_eq!(state.handles(), 2);

    f.registry.exhaust_ids_for_test();

    let removed = remove(&leased, &grant.grant_id, 100);
    assert_eq!(
        state.route_handle(&changing, &removed),
        RouteLookup::Warm,
        "shedding a scope creates no slot, so exhaustion cannot refuse it"
    );
    assert_eq!(state.handles(), 1);
    assert_eq!(
        state.warm(&changing).expect("warm").demanded(),
        &[owner_key(&changing)]
    );
    assert_eq!(f.metrics.refused_id_space_exhausted(), 0);
}

// ------------------ §4.4/4.6: stale readers, ownership, ordering, races

/// A replacement refused at a full node succeeds once ANOTHER family stops
/// sharing the old slot — even though nothing retired and the node capacity
/// generation never moved.
///
/// The projection genuinely changes when a shared reference goes away: the
/// old slot becomes this family's alone, so giving it up now credits the node
/// bound. (The removed replacement cache gated this retry on the
/// reference-release generation; without any cache the next attempt simply
/// asks the registry, whose projection is current by construction.)
#[test]
fn a_shared_old_replacement_succeeds_when_the_sharer_releases() {
    let f = fixture();
    let capability = cap("nrpc:changing");
    let (old_grant, old_secret) = issue(capability, GrantRights::DISCOVER);
    let (new_grant, new_secret) = issue(capability, GrantRights::DISCOVER);
    let leased = lease(
        &ConsumerGrantSnapshot::empty(),
        &old_grant,
        old_secret.expect("secret"),
        1,
    );
    let state = f.state(credentials(vec![old_grant.clone(), new_grant.clone()]));
    assert_eq!(state.route_handle(&capability, &leased), RouteLookup::Warm);

    // Another family SHARES the exact slot the rotation would give up.
    let sharer = f.registry.new_family().expect("family");
    let shared = sharer.demand(grant_key(&old_grant)).expect("shares");
    let _fillers = fill_node_to(&f, MAX_NODE_SLOTS);
    let generation = f.registry.node_capacity_generation();

    let rotated = lease(
        &remove(&leased, &old_grant.grant_id, 100),
        &new_grant,
        new_secret.expect("secret"),
        101,
    );
    assert_eq!(
        state.route_handle(&capability, &rotated),
        RouteLookup::Cold(ColdReason::Refused(DemandRefused::NodeAtCapacity)),
        "the shared old slot frees nothing, so the new one is a 257th"
    );

    // The sharer lets go. NOTHING retires — the slot is still ours — and the
    // node capacity generation stands still.
    drop(shared);
    assert_eq!(
        f.registry.node_capacity_generation(),
        generation,
        "no retirement, so no capacity movement"
    );
    assert_eq!(
        state.route_handle(&capability, &rotated),
        RouteLookup::Warm,
        "but the projection changed, and the replacement must be retried"
    );
}

/// An under-lock miss RACE between an older and a newer snapshot leaves the
/// NEWEST exact set retained (§4.4 + §4.6 together).
///
/// Dies to: the existence-only recheck (shared with W-M1). The older caller
/// enters the mutation path after the newer one published, and must neither
/// adopt nor overwrite it.
#[test]
fn the_under_lock_miss_race_retains_the_newest_snapshot() {
    let f = fixture();
    let capability = cap("nrpc:read");
    let (grant, secret) = issue(capability, GrantRights::DISCOVER);
    let state = f.state(credentials(vec![grant.clone()]));

    let older = ConsumerGrantSnapshot::empty().stamped(GrantMovementFence::Publication(3));
    let newer = lease(&older, &grant, secret.expect("secret"), 8);

    // The NEWER caller wins the race and publishes.
    assert_eq!(state.acquire(&capability, &newer), RouteLookup::Warm);
    assert_eq!(
        state.warm(&capability).expect("warm").demanded(),
        &[owner_key(&capability), grant_key(&grant)]
    );

    // The OLDER caller arrives at the mutation path afterwards.
    assert_eq!(
        state.acquire(&capability, &older),
        RouteLookup::Cold(ColdReason::SnapshotSuperseded),
        "the loser must not act on a view the family has moved past"
    );
    assert_eq!(
        state.warm(&capability).expect("warm").demanded(),
        &[owner_key(&capability), grant_key(&grant)],
        "and the newest exact set is what stays retained"
    );
    assert_eq!(state.handles(), 2);
}

/// The under-lock miss recheck must validate currency for THIS caller's
/// snapshot, not merely that some entry exists (§4.6).
///
/// Dies to: rechecking `index.get(capability).is_some()` and returning `Warm`.
/// The loser of a miss race then adopts the winner's entry without ever asking
/// whether it is current for its own snapshot, and reports `Warm` for a set
/// missing a newly installed audience.
#[test]
fn the_under_lock_miss_recheck_validates_the_callers_snapshot() {
    let f = fixture();
    let capability = cap("nrpc:read");
    let (grant, secret) = issue(capability, GrantRights::DISCOVER);
    let state = f.state(credentials(vec![grant.clone()]));

    // T1's snapshot: the audience is NOT installed.
    let older = ConsumerGrantSnapshot::empty();
    assert_eq!(state.route_handle(&capability, &older), RouteLookup::Warm);
    assert_eq!(state.warm(&capability).expect("warm").demands().len(), 1);

    // T2's snapshot is NEWER and has the audience. Entering the mutation path
    // directly is what puts it past the lock-free check, exactly as the
    // concurrency witnesses do.
    let newer = lease(&older, &grant, secret.expect("secret"), 5);
    assert_eq!(state.acquire(&capability, &newer), RouteLookup::Warm);
    assert_eq!(
        state.warm(&capability).expect("warm").demanded(),
        &[owner_key(&capability), grant_key(&grant)],
        "the loser of the race must not adopt an entry stale for ITS snapshot"
    );
}

/// The same, for a REMOVAL the racing caller can see and the published entry
/// cannot.
#[test]
fn the_under_lock_miss_recheck_sees_a_removal() {
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

    let removed = remove(&leased, &grant.grant_id, 100);
    assert_eq!(state.acquire(&capability, &removed), RouteLookup::Warm);
    assert_eq!(
        state.warm(&capability).expect("warm").demanded(),
        &[owner_key(&capability)]
    );
}

/// And for a ROTATION.
#[test]
fn the_under_lock_miss_recheck_sees_a_rotation() {
    let f = fixture();
    let capability = cap("nrpc:read");
    let (old_grant, old_secret) = issue(capability, GrantRights::DISCOVER);
    let (new_grant, new_secret) = issue(capability, GrantRights::DISCOVER);
    let leased = lease(
        &ConsumerGrantSnapshot::empty(),
        &old_grant,
        old_secret.expect("secret"),
        1,
    );
    let state = f.state(credentials(vec![old_grant.clone(), new_grant.clone()]));
    assert_eq!(state.route_handle(&capability, &leased), RouteLookup::Warm);

    let rotated = lease(
        &remove(&leased, &old_grant.grant_id, 100),
        &new_grant,
        new_secret.expect("secret"),
        101,
    );
    assert_eq!(state.acquire(&capability, &rotated), RouteLookup::Warm);
    assert_eq!(
        state.warm(&capability).expect("warm").demanded(),
        &[owner_key(&capability), grant_key(&new_grant)]
    );
}

/// A STALLED OLDER snapshot cannot overwrite a newer retained demand set
/// (§4.4).
///
/// Dies to: ordering snapshots by lock acquisition instead of by their published
/// transition. The older caller observes the newer entry as "stale for me" and
/// would otherwise reinstate the very authority the later transition withdrew.
#[test]
fn a_stalled_older_install_cannot_overwrite_a_newer_removal() {
    let f = fixture();
    let capability = cap("nrpc:read");
    let (grant, secret) = issue(capability, GrantRights::DISCOVER);
    let state = f.state(credentials(vec![grant.clone()]));

    // The OLDER view still has the audience installed.
    let older = lease(
        &ConsumerGrantSnapshot::empty(),
        &grant,
        secret.expect("secret"),
        5,
    );
    // The NEWER view removed it.
    let newer = remove(&older, &grant.grant_id, 9);

    assert_eq!(state.route_handle(&capability, &newer), RouteLookup::Warm);
    assert_eq!(
        state.warm(&capability).expect("warm").demanded(),
        &[owner_key(&capability)]
    );

    assert_eq!(
        state.route_handle(&capability, &older),
        RouteLookup::Cold(ColdReason::SnapshotSuperseded),
        "a stalled older snapshot must neither be served nor act"
    );
    assert_eq!(
        state.warm(&capability).expect("warm").demanded(),
        &[owner_key(&capability)],
        "and the newer retained set is untouched"
    );
    assert_eq!(state.handles(), 1);
}

/// The mirror: a stalled older REMOVAL cannot overwrite a newer install.
#[test]
fn a_stalled_older_removal_cannot_overwrite_a_newer_install() {
    let f = fixture();
    let capability = cap("nrpc:read");
    let (grant, secret) = issue(capability, GrantRights::DISCOVER);
    let state = f.state(credentials(vec![grant.clone()]));

    let installed = lease(
        &ConsumerGrantSnapshot::empty(),
        &grant,
        secret.expect("secret"),
        9,
    );
    // An OLDER transition in which the audience was absent.
    let older_absent = ConsumerGrantSnapshot::empty().stamped(GrantMovementFence::Publication(5));

    assert_eq!(
        state.route_handle(&capability, &installed),
        RouteLookup::Warm
    );
    assert_eq!(state.handles(), 2);

    assert_eq!(
        state.route_handle(&capability, &older_absent),
        RouteLookup::Cold(ColdReason::SnapshotSuperseded),
    );
    assert_eq!(state.handles(), 2, "the newer install survives");
    assert_eq!(
        state.warm(&capability).expect("warm").demanded(),
        &[owner_key(&capability), grant_key(&grant)]
    );
}

/// A TERMINAL snapshot outranks every ordinary publication.
#[test]
fn a_terminal_snapshot_cannot_be_overwritten_by_an_ordinary_one() {
    let f = fixture();
    let capability = cap("nrpc:read");
    let (grant, secret) = issue(capability, GrantRights::DISCOVER);
    let state = f.state(credentials(vec![grant.clone()]));

    let installed = lease(
        &ConsumerGrantSnapshot::empty(),
        &grant,
        secret.expect("secret"),
        5,
    );
    // The terminal withdrawal: the publication-identity space is spent.
    let terminal = installed
        .without(&grant.grant_id)
        .expect("the record was there")
        .stamped(GrantMovementFence::Terminal);

    assert_eq!(
        state.route_handle(&capability, &terminal),
        RouteLookup::Warm
    );
    assert_eq!(
        state.warm(&capability).expect("warm").demanded(),
        &[owner_key(&capability)]
    );

    assert_eq!(
        state.route_handle(&capability, &installed),
        RouteLookup::Cold(ColdReason::SnapshotSuperseded),
        "no ordinary publication can follow a terminal one"
    );
    assert_eq!(state.handles(), 1);
}

/// TWO DIFFERENT snapshots claiming ONE transition identity is an invariant
/// breach, and is refused rather than acted on.
#[test]
fn two_snapshots_at_one_revision_are_refused_as_an_invariant_breach() {
    let f = fixture();
    let capability = cap("nrpc:read");
    let (grant, secret) = issue(capability, GrantRights::DISCOVER);
    let state = f.state(credentials(vec![grant.clone()]));

    let absent = ConsumerGrantSnapshot::empty().stamped(GrantMovementFence::Publication(7));
    assert_eq!(state.route_handle(&capability, &absent), RouteLookup::Warm);
    assert_eq!(state.handles(), 1);

    // A DIFFERENT snapshot claiming the SAME transition. The publication seam
    // allocates each identity once, so this cannot happen — and if it does, the
    // two disagree about what that transition published.
    let forged = lease(
        &ConsumerGrantSnapshot::empty(),
        &grant,
        secret.expect("secret"),
        1,
    )
    .stamped(GrantMovementFence::Publication(7));
    assert_eq!(
        state.route_handle(&capability, &forged),
        RouteLookup::Cold(ColdReason::SnapshotSuperseded),
        "one transition identity, two contents — fail closed"
    );
    assert_eq!(state.handles(), 1, "and nothing moved");
}

/// An UNRELATED newer movement advances freshness without churning demand, and
/// the older relevant snapshot still cannot regress afterwards.
#[test]
fn unrelated_newer_movement_advances_freshness_without_churn() {
    let f = fixture();
    let capability = cap("nrpc:read");
    let (grant, secret) = issue(capability, GrantRights::DISCOVER);
    let (unrelated, unrelated_secret) = issue(cap("nrpc:other"), GrantRights::DISCOVER);
    let state = f.state(credentials(vec![grant.clone(), unrelated.clone()]));

    let older = lease(
        &ConsumerGrantSnapshot::empty(),
        &grant,
        secret.expect("secret"),
        5,
    );
    assert_eq!(state.route_handle(&capability, &older), RouteLookup::Warm);
    let acquisitions = state.mutate_acquisitions();
    let handles = state.handles();

    // A newer publication that changes nothing about THIS capability.
    let newer = lease(&older, &unrelated, unrelated_secret.expect("secret"), 9);
    assert_eq!(state.route_handle(&capability, &newer), RouteLookup::Warm);
    assert_eq!(
        state.mutate_acquisitions(),
        acquisitions,
        "an irrelevant movement re-derives nothing and takes no lock"
    );
    assert_eq!(state.handles(), handles);

    // Freshness advanced anyway, so the older snapshot can no longer act.
    assert_eq!(
        state.route_handle(&capability, &older),
        RouteLookup::Cold(ColdReason::SnapshotSuperseded),
        "freshness must advance even when demand does not"
    );
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
