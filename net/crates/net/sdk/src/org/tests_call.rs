//! OSDK S1 witnesses — the authority decision behind `org.call`.
//!
//! Tier note: these exercise the REAL private-discovery store (envelopes are
//! built by the canonical builders and admitted through the real
//! `verify_scoped_ingest` path), the real credential predicates, and the real
//! intent construction — everything `call` does before the network. The live
//! two-node traversal of `verify_org_admission` is S3.

use std::sync::Arc;

use net::adapter::net::behavior::capability::{CapabilityAnnouncement, CapabilitySet};
use net::adapter::net::behavior::org_scoped_ann::ScopedCapabilityAnnouncement;
use net::adapter::net::identity::EntityKeypair;

use super::call::Mode;
use super::credentials::OrgCredentials;
use super::error::{OrgCredentialError, OrgDiscoveryError};
use super::tests::{belonging, cap, discover_grant, mesh_with_authority, org_a, org_b};
use super::types::*;
use super::{OrgClient, OrgSdkError};
use crate::identity::Identity;
use crate::mesh::Mesh;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// A compact capability descriptor. An owner envelope legitimately carries
/// several tags; a granted envelope carries exactly one (bound to its grant).
fn descriptor(tags: &[&str]) -> Vec<u8> {
    let mut caps = CapabilitySet::new();
    for t in tags {
        caps = caps.add_tag(*t);
    }
    caps.to_bytes_compact()
}

fn far_future() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs()
        + 3600
}

/// Duplicate an audience secret the way a second holder would obtain it: by
/// loading the same 0600 config file.
fn copy_secret(s: &OrgAudienceSecret) -> OrgAudienceSecret {
    OrgAudienceSecret::decode_config(&s.encode_config()).expect("decode config")
}

/// Inject an OWNER-scoped announcement from a same-org provider, through the
/// real ingest path (outer signature, owner cert, audience selection, AEAD,
/// descriptor validation all run).
fn inject_owner_envelope(mesh: &Mesh, owner: &OrgKeypair, provider: &EntityKeypair, tags: &[&str]) {
    let authority = mesh.node().node_authority().expect("authority");
    let cert = OrgMembershipCert::try_issue(owner, provider.entity_id().clone(), 1, 3600)
        .expect("provider cert");
    let env = ScopedCapabilityAnnouncement::build_owner(
        provider,
        owner.org_id(),
        cert,
        authority.audience.audience_handle,
        authority.audience.discovery_key(),
        1,
        far_future(),
        &descriptor(tags),
    )
    .expect("owner envelope");
    mesh.node()
        .ingest_scoped_announcement_for_test(&env.to_bytes());
}

/// Inject a GRANTED envelope from a provider owned by the issuing org.
fn inject_granted_envelope(
    mesh: &Mesh,
    issuer: &OrgKeypair,
    provider: &EntityKeypair,
    grant: &OrgCapabilityGrant,
    secret: &OrgAudienceSecret,
    tag: &str,
) {
    let cert = OrgMembershipCert::try_issue(issuer, provider.entity_id().clone(), 1, 3600)
        .expect("provider cert");
    let env = ScopedCapabilityAnnouncement::build_granted(
        provider,
        issuer.org_id(),
        cert,
        grant.grant_id,
        secret.audience_handle,
        secret.discovery_key(),
        1,
        far_future(),
        &descriptor(&[tag]),
    )
    .expect("granted envelope");
    mesh.node()
        .ingest_scoped_announcement_for_test(&env.to_bytes());
}

fn bind(
    mesh: &Mesh,
    a: &OrgKeypair,
    identity: &Identity,
    held: Vec<(OrgCapabilityGrant, Option<OrgAudienceSecret>)>,
) -> OrgClient {
    let (cert, dg) = belonging(a, identity.entity_id());
    let mut grants = Vec::new();
    let mut secrets = Vec::new();
    for (g, s) in held {
        grants.push(g);
        if let Some(s) = s {
            secrets.push(s);
        }
    }
    let creds = OrgCredentials::new(cert, dg, grants, secrets).expect("assembles");
    mesh.org(creds).expect("binds")
}

// ---------------------------------------------------------------------------
// Intent equality — the facade builds exactly the proof a hand-written caller
// would, for both modes.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn plan_builds_the_canonical_same_org_intent() {
    let a = org_a();
    let (mesh, identity, dir) = mesh_with_authority("plan-same-org", Some(&a)).await;
    let provider = EntityKeypair::generate();
    inject_owner_envelope(
        &mesh,
        &a,
        &provider,
        &["nrpc:internal.reindex", "nrpc:other"],
    );

    let client = bind(&mesh, &a, &identity, vec![]);
    let capability = cap("nrpc:internal.reindex");
    let (candidates, considered) = client
        .authorized_candidates(&capability)
        .expect("authority decision");

    assert_eq!(considered, 1, "one owner-private candidate");
    assert_eq!(candidates.len(), 1);
    assert_eq!(&candidates[0].provider, provider.entity_id());
    assert_eq!(candidates[0].mode, Mode::SameOrg);

    let intent = client.intent_for(&candidates[0]);
    // All nine fields.
    assert_eq!(intent.caller.entity_id(), identity.entity_id());
    assert_eq!(&intent.membership, client.membership());
    assert_eq!(&intent.dispatcher, client.dispatcher());
    assert!(
        intent.capability_grant.is_none(),
        "OwnerDelegated admission refuses an unexpected capability grant"
    );
    assert_eq!(intent.acting_org, a.org_id());
    assert_eq!(intent.provider_owner_org, a.org_id());
    assert_eq!(&intent.provider, provider.entity_id());
    assert_eq!(intent.capability, capability);
    assert_eq!(intent.proof_ttl_secs, 30, "the shared frozen TTL");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn plan_builds_the_canonical_cross_org_intent() {
    let (a, b) = (org_a(), org_b());
    let (mesh, identity, dir) = mesh_with_authority("plan-cross-org", Some(&a)).await;
    let provider = EntityKeypair::generate();
    let (grant, secret) = discover_grant(&b, a.org_id(), cap("nrpc:customer.read"), 3600);
    let secret_copy = copy_secret(&secret);

    let client = bind(&mesh, &a, &identity, vec![(grant.clone(), Some(secret))]);
    inject_granted_envelope(
        &mesh,
        &b,
        &provider,
        &grant,
        &secret_copy,
        "nrpc:customer.read",
    );

    let capability = cap("nrpc:customer.read");
    let (candidates, considered) = client
        .authorized_candidates(&capability)
        .expect("authority decision");
    assert_eq!(considered, 1);
    assert_eq!(candidates.len(), 1, "one authorized cross-org target");
    assert_eq!(candidates[0].mode, Mode::Granted(Box::new(grant.clone())));

    let intent = client.intent_for(&candidates[0]);
    assert_eq!(
        intent.capability_grant.as_ref(),
        Some(&grant),
        "the matched grant rides the proof"
    );
    assert_eq!(intent.acting_org, a.org_id());
    assert_eq!(
        intent.provider_owner_org,
        b.org_id(),
        "the provider's owner org is the grant's ISSUER"
    );
    assert_eq!(&intent.provider, provider.entity_id());
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// Private-only discovery
// ---------------------------------------------------------------------------

/// A capability present ONLY on the plaintext plane is invisible to the facade:
/// no public ownership projection, no plaintext fallback.
#[tokio::test]
async fn the_public_plane_is_never_consulted() {
    let a = org_a();
    let (mesh, identity, dir) = mesh_with_authority("plan-private-only", Some(&a)).await;
    let provider = EntityKeypair::generate();

    let caps = CapabilitySet::new().add_tag("nrpc:public.svc");
    let ann = CapabilityAnnouncement::new(
        provider.entity_id().node_id(),
        provider.entity_id().clone(),
        1,
        caps,
    );
    mesh.node().test_inject_capability_announcement(ann);
    assert!(
        !mesh.node().find_service_nodes("public.svc").is_empty(),
        "the public plane really does carry it"
    );

    let client = bind(&mesh, &a, &identity, vec![]);
    let (candidates, considered) = client
        .authorized_candidates(&cap("nrpc:public.svc"))
        .expect("authority decision");
    assert_eq!(considered, 0, "a public announcement is not a candidate");
    assert!(candidates.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

/// The owner plane announces every owner-scoped tag a provider serves, so a
/// record is not by itself an answer about one capability.
#[tokio::test]
async fn an_owner_record_matches_only_the_capability_it_declares() {
    let a = org_a();
    let (mesh, identity, dir) = mesh_with_authority("plan-owner-tag", Some(&a)).await;
    let provider = EntityKeypair::generate();
    inject_owner_envelope(&mesh, &a, &provider, &["nrpc:internal.reindex"]);

    let client = bind(&mesh, &a, &identity, vec![]);
    assert_eq!(
        client
            .authorized_candidates(&cap("nrpc:internal.reindex"))
            .expect("decision")
            .1,
        1
    );
    assert_eq!(
        client
            .authorized_candidates(&cap("nrpc:not.declared"))
            .expect("decision")
            .1,
        0,
        "a different tag on the same provider is not a candidate"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// Authority relation
// ---------------------------------------------------------------------------

/// DISCOVER resolves the provider; invoking still needs INVOKE. Refused
/// LOCALLY, without spending a provider round trip.
#[tokio::test]
async fn a_discover_only_grant_resolves_but_cannot_invoke() {
    let (a, b) = (org_a(), org_b());
    let (mesh, identity, dir) = mesh_with_authority("plan-discover-only", Some(&a)).await;
    let provider = EntityKeypair::generate();
    let (grant, secret) = OrgCapabilityGrant::try_issue(
        &b,
        a.org_id(),
        cap("nrpc:customer.read"),
        GrantRights::DISCOVER,
        GrantTargetScope::AnyNodeOwnedBy(b.org_id()),
        3600,
    )
    .expect("discover-only grant");
    let secret = secret.expect("discover mints a secret");
    let secret_copy = copy_secret(&secret);

    let client = bind(&mesh, &a, &identity, vec![(grant.clone(), Some(secret))]);
    inject_granted_envelope(
        &mesh,
        &b,
        &provider,
        &grant,
        &secret_copy,
        "nrpc:customer.read",
    );

    let (candidates, considered) = client
        .authorized_candidates(&cap("nrpc:customer.read"))
        .expect("authority decision");
    assert_eq!(considered, 1, "discovery DID resolve the provider");
    assert!(
        candidates.is_empty(),
        "but DISCOVER alone is never invocation authority"
    );

    let err = client
        .plan("customer.read", 0)
        .expect_err("no invoke authority");
    assert!(
        matches!(
            err,
            OrgSdkError::Discovery(OrgDiscoveryError::NoAuthorizedProvider { considered: 1, .. })
        ),
        "got {err:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Two grants that both satisfy the relation are an error, never a silent pick.
#[tokio::test]
async fn overlapping_grants_are_an_ambiguity_error() {
    let (a, b) = (org_a(), org_b());
    let (mesh, identity, dir) = mesh_with_authority("plan-ambiguous", Some(&a)).await;
    let provider = EntityKeypair::generate();
    let capability = cap("nrpc:customer.read");

    // A wide AnyNodeOwnedBy grant (which also carries the discovery audience)
    // and an ExactNode grant for the same capability: both cover this provider.
    let (wide, wide_secret) = discover_grant(&b, a.org_id(), capability, 3600);
    let (exact, none) = OrgCapabilityGrant::try_issue(
        &b,
        a.org_id(),
        capability,
        GrantRights::INVOKE,
        GrantTargetScope::ExactNode(provider.entity_id().clone()),
        3600,
    )
    .expect("exact grant");
    assert!(none.is_none(), "INVOKE-only mints no audience material");
    let wide_copy = copy_secret(&wide_secret);

    let client = bind(
        &mesh,
        &a,
        &identity,
        vec![(wide.clone(), Some(wide_secret)), (exact, None)],
    );
    inject_granted_envelope(
        &mesh,
        &b,
        &provider,
        &wide,
        &wide_copy,
        "nrpc:customer.read",
    );

    let err = client
        .authorized_candidates(&capability)
        .expect_err("must refuse");
    match err {
        OrgSdkError::Credentials(OrgCredentialError::AmbiguousCapabilityGrant {
            grant_ids,
            ..
        }) => assert_eq!(grant_ids.len(), 2),
        other => panic!("got {other:?}"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A grant that does not cover this exact provider does not authorize it.
#[tokio::test]
async fn a_grant_whose_target_scope_excludes_the_provider_does_not_authorize() {
    let (a, b) = (org_a(), org_b());
    let (mesh, identity, dir) = mesh_with_authority("plan-target-scope", Some(&a)).await;
    let provider = EntityKeypair::generate();
    let elsewhere = EntityKeypair::generate();
    let capability = cap("nrpc:customer.read");

    // Discovery audience covers any B node; the INVOKE grant names a DIFFERENT
    // exact node, so this provider resolves but is not invocable.
    let (wide, wide_secret) = OrgCapabilityGrant::try_issue(
        &b,
        a.org_id(),
        capability,
        GrantRights::DISCOVER,
        GrantTargetScope::AnyNodeOwnedBy(b.org_id()),
        3600,
    )
    .expect("discover grant");
    let wide_secret = wide_secret.expect("secret");
    let (exact_other, _) = OrgCapabilityGrant::try_issue(
        &b,
        a.org_id(),
        capability,
        GrantRights::INVOKE,
        GrantTargetScope::ExactNode(elsewhere.entity_id().clone()),
        3600,
    )
    .expect("exact grant elsewhere");
    let wide_copy = copy_secret(&wide_secret);

    let client = bind(
        &mesh,
        &a,
        &identity,
        vec![(wide.clone(), Some(wide_secret)), (exact_other, None)],
    );
    inject_granted_envelope(
        &mesh,
        &b,
        &provider,
        &wide,
        &wide_copy,
        "nrpc:customer.read",
    );

    let (candidates, considered) = client
        .authorized_candidates(&capability)
        .expect("authority decision");
    assert_eq!(considered, 1, "resolved");
    assert!(candidates.is_empty(), "but not covered by any INVOKE grant");
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// Selection, reachability, and the stage-3 recheck
// ---------------------------------------------------------------------------

#[tokio::test]
async fn selection_is_deterministic_lowest_provider_id() {
    let a = org_a();
    let (mesh, identity, dir) = mesh_with_authority("plan-determinism", Some(&a)).await;
    let p1 = EntityKeypair::generate();
    let p2 = EntityKeypair::generate();
    inject_owner_envelope(&mesh, &a, &p1, &["nrpc:internal.reindex"]);
    inject_owner_envelope(&mesh, &a, &p2, &["nrpc:internal.reindex"]);

    let client = bind(&mesh, &a, &identity, vec![]);
    let capability = cap("nrpc:internal.reindex");
    let expected = std::cmp::min(p1.entity_id().clone(), p2.entity_id().clone());

    for _ in 0..5 {
        let (candidates, considered) = client
            .authorized_candidates(&capability)
            .expect("authority decision");
        assert_eq!(considered, 2);
        assert_eq!(candidates.len(), 2);
        assert_eq!(
            candidates[0].provider, expected,
            "lowest entity id wins, every time"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// OLB-1 selection surface: direct reachability now rides each
/// `AuthorizedOrgCandidate`, and `plan` selects the first *directly reachable*
/// authorized provider in deterministic order — not merely the first authorized
/// one. Here the reachable provider sorts LATER, so a selector that ignored
/// reachability (or dropped the flag in the factoring) would pick the wrong
/// provider or wrongly report `ProviderNotDirect`.
#[tokio::test]
async fn selection_prefers_a_direct_provider_over_an_earlier_indirect_one() {
    let a = org_a();
    let (mesh, identity, dir) = mesh_with_authority("plan-direct-pref", Some(&a)).await;
    let p1 = EntityKeypair::generate();
    let p2 = EntityKeypair::generate();
    inject_owner_envelope(&mesh, &a, &p1, &["nrpc:internal.reindex"]);
    inject_owner_envelope(&mesh, &a, &p2, &["nrpc:internal.reindex"]);

    // Pin a live direct session to whichever provider sorts LATER, leaving the
    // lower-EntityId provider authorized but indirect.
    let (lower, higher) = if p1.entity_id() < p2.entity_id() {
        (&p1, &p2)
    } else {
        (&p2, &p1)
    };
    mesh.node()
        .test_pin_peer_entity(higher.entity_id().node_id(), higher.entity_id().clone());

    let client = bind(&mesh, &a, &identity, vec![]);
    let capability = cap("nrpc:internal.reindex");

    // Both authorized, deterministic order, exactly one direct — the later one.
    let (candidates, _) = client
        .authorized_candidates(&capability)
        .expect("authority decision");
    assert_eq!(candidates.len(), 2);
    assert_eq!(
        &candidates[0].provider,
        lower.entity_id(),
        "sorted lowest-first"
    );
    assert!(!candidates[0].direct, "the lower provider has no session");
    assert!(candidates[1].direct, "the higher provider is pinned direct");

    // plan selects the direct provider even though it sorts later.
    let intent = client
        .plan("internal.reindex", 0)
        .expect("a directly reachable provider exists");
    assert_eq!(
        &intent.provider,
        higher.entity_id(),
        "direct reachability beats sort order"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Authorized but unreachable is distinct from nothing authorized: protected
/// RPC is direct-session-only (OA2-E0.3), so the facade does not send a request
/// the provider would deny for relaying.
#[tokio::test]
async fn an_authorized_but_unreachable_provider_is_reported_as_not_direct() {
    let a = org_a();
    let (mesh, identity, dir) = mesh_with_authority("plan-indirect", Some(&a)).await;
    let provider = EntityKeypair::generate();
    inject_owner_envelope(&mesh, &a, &provider, &["nrpc:internal.reindex"]);

    let client = bind(&mesh, &a, &identity, vec![]);
    let err = client.plan("internal.reindex", 0).expect_err("unreachable");
    assert!(
        matches!(
            err,
            OrgSdkError::Discovery(OrgDiscoveryError::ProviderNotDirect { .. })
        ),
        "got {err:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn nothing_discovered_reports_zero_considered() {
    let a = org_a();
    let (mesh, identity, dir) = mesh_with_authority("plan-empty", Some(&a)).await;
    let client = bind(&mesh, &a, &identity, vec![]);

    let err = client
        .plan("internal.reindex", 0)
        .expect_err("nothing to call");
    assert!(
        matches!(
            err,
            OrgSdkError::Discovery(OrgDiscoveryError::NoAuthorizedProvider { considered: 0, .. })
        ),
        "got {err:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_dispatcher_scope_that_excludes_the_capability_refuses_locally() {
    let a = org_a();
    let (mesh, identity, dir) = mesh_with_authority("plan-scope", Some(&a)).await;
    let provider = EntityKeypair::generate();
    inject_owner_envelope(&mesh, &a, &provider, &["nrpc:internal.reindex"]);

    let cert =
        OrgMembershipCert::try_issue(&a, identity.entity_id().clone(), 1, 3600).expect("cert");
    let dg = OrgDispatcherGrant::try_issue(
        &a,
        identity.entity_id().clone(),
        DispatcherScope::Exact(cap("nrpc:something.else")),
        3600,
    )
    .expect("dg");
    let client = mesh
        .org(OrgCredentials::new(cert, dg, vec![], vec![]).expect("assembles"))
        .expect("binds");

    let err = client
        .plan("internal.reindex", 0)
        .expect_err("out of scope");
    assert!(
        matches!(
            err,
            OrgSdkError::Credentials(OrgCredentialError::DispatcherScopeExcludesCapability { .. })
        ),
        "got {err:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Stage 3: an expired membership refuses at CALL time.
///
/// It also pins the stage boundary: binding does NOT check membership or
/// dispatcher windows (only the installability of DISCOVER audiences), so an
/// already-expired membership binds fine and fails on the first call. Waiting
/// out the window BEFORE binding keeps the witness deterministic — extra delay
/// under load can only strengthen it, never make the credential valid again.
#[tokio::test]
async fn an_expired_membership_refuses_at_call_time() {
    let a = org_a();
    let (mesh, identity, dir) = mesh_with_authority("plan-expired", Some(&a)).await;
    let cert = OrgMembershipCert::try_issue(&a, identity.entity_id().clone(), 1, 1).expect("cert");
    let dg =
        OrgDispatcherGrant::try_issue(&a, identity.entity_id().clone(), DispatcherScope::Any, 3600)
            .expect("dg");
    std::thread::sleep(std::time::Duration::from_millis(1100));

    let client = mesh
        .org(OrgCredentials::new(cert, dg, vec![], vec![]).expect("assembles"))
        .expect("an expired membership still BINDS — windows are a call-time check");

    client
        .check_current()
        .expect_err("but the credentials are not current");
    let err = client.plan("internal.reindex", 0).expect_err("expired");
    assert!(
        matches!(
            err,
            OrgSdkError::Credentials(OrgCredentialError::NotCurrentlyValid { .. })
        ),
        "got {err:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// OLB-2A.2: ingesting an owner capability through the real path advances the
/// private-discovery change generations — the read-only poll a reconciler uses to
/// learn that discovery moved, wired end-to-end through the MeshNode accessors.
/// (The destructive delta drain is crate-internal per the OLB-2A closure, so the
/// delta CONTENT is witnessed at the `ScopedDiscoveryState` level in the core.)
#[tokio::test]
async fn ingest_advances_the_private_discovery_generation() {
    let a = org_a();
    let (mesh, _identity, dir) = mesh_with_authority("pd-generation", Some(&a)).await;
    let provider = EntityKeypair::generate();

    assert_eq!(mesh.node().private_discovery_generation(), 0);
    assert_eq!(mesh.node().private_discovery_owner_generation(), 0);

    inject_owner_envelope(&mesh, &a, &provider, &["nrpc:internal.reindex"]);

    assert!(
        mesh.node().private_discovery_generation() >= 1,
        "global generation advanced"
    );
    assert!(
        mesh.node().private_discovery_owner_generation() >= 1,
        "owner generation advanced"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// OLB-2A.3: a scoped-store mutation that changes the visible set WAKES the
/// private-discovery change watch (the push signal a consumer awaits instead of
/// polling), and a no-op (a stale re-ingest) does NOT — the centralized helper
/// only publishes when the generation actually advanced.
#[tokio::test]
async fn a_visible_mutation_wakes_the_watch_and_a_noop_does_not() {
    let a = org_a();
    let (mesh, _identity, dir) = mesh_with_authority("pd-watch", Some(&a)).await;
    let provider = EntityKeypair::generate();

    let mut rx = mesh.node().subscribe_private_discovery_changes();
    assert_eq!(*rx.borrow_and_update(), 0);

    inject_owner_envelope(&mesh, &a, &provider, &["nrpc:internal.reindex"]);
    assert!(
        rx.has_changed().expect("sender alive"),
        "a visible-set change wakes the watch"
    );
    assert!(*rx.borrow_and_update() >= 1);

    // The same envelope again is a stale re-ingest — no visible change, no wake.
    inject_owner_envelope(&mesh, &a, &provider, &["nrpc:internal.reindex"]);
    assert!(
        !rx.has_changed().expect("sender alive"),
        "a no-op mutation must not wake the watch"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// review-pass-3 §17 — candidate order is GLOBAL, not per-plane.
///
/// The OLB-1 sort was red-coupled to nothing: the discovery source
/// (`owner_by_capability: BTreeMap<_, BTreeSet<ScopedKey>>`) already yields
/// ascending-EntityId order WITHIN one scope, and both determinism witnesses put
/// both providers in the SAME owner scope — so deleting the sort left every test
/// passing deterministically. It is load-bearing only ACROSS planes or scopes,
/// which no test constructed; the phase-3 sorted-order reachability sampling was
/// likewise unobservable in any single-threaded test.
///
/// Both relative orderings are driven, so the assertion cannot pass vacuously
/// whichever plane discovery happens to enumerate first: with the sort removed,
/// one of the two arrangements must come back out of order.
#[tokio::test]
async fn candidate_order_is_global_across_the_owner_and_grant_planes() {
    for grant_provider_sorts_lower in [true, false] {
        let (a, b) = (org_a(), org_b());
        let label = if grant_provider_sorts_lower {
            "plan-cross-plane-grant-lo"
        } else {
            "plan-cross-plane-owner-lo"
        };
        let (mesh, identity, dir) = mesh_with_authority(label, Some(&a)).await;
        let tag = "nrpc:customer.read";

        // Pick the pair so the required plane holds the LOWER EntityId.
        let (owner_provider, grant_provider) = loop {
            let owner = EntityKeypair::generate();
            let granted = EntityKeypair::generate();
            if (granted.entity_id() < owner.entity_id()) == grant_provider_sorts_lower {
                break (owner, granted);
            }
        };

        let (grant, secret) = discover_grant(&b, a.org_id(), cap(tag), 3600);
        let secret_copy = copy_secret(&secret);
        let client = bind(&mesh, &a, &identity, vec![(grant.clone(), Some(secret))]);
        inject_owner_envelope(&mesh, &a, &owner_provider, &[tag]);
        inject_granted_envelope(&mesh, &b, &grant_provider, &grant, &secret_copy, tag);

        let (candidates, _) = client
            .authorized_candidates(&cap(tag))
            .expect("authority decision");
        assert_eq!(
            candidates.len(),
            2,
            "{label}: one candidate from each plane"
        );
        assert!(
            candidates.iter().any(|c| matches!(c.mode, Mode::SameOrg)),
            "{label}: the owner plane really is represented"
        );
        assert!(
            candidates
                .iter()
                .any(|c| matches!(c.mode, Mode::Granted(_))),
            "{label}: and so is the grant plane"
        );
        assert!(
            candidates[0].provider.as_bytes() < candidates[1].provider.as_bytes(),
            "{label}: candidates must be in GLOBAL ascending EntityId order — \
             per-plane discovery order is not a total order",
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

// ---------------------------------------------------------------------------
// OLB-2B.3d-pre — the coherent cold plan
//
// The capture's structural properties (one store section, the epoch re-check,
// the two fail-closed refusals) are witnessed on the node in
// `org_routing_wiring_tests`. These four are the ones that need REAL rows and a
// REAL bound client: that the capture serves exactly what the live plane seams
// serve, that each component of the captured authority identity is actually
// compared, and that a moved authority mints nothing.
// ---------------------------------------------------------------------------

/// The capture is a COHERENCE change, not a discovery change: for the same
/// capability and the same held grant it serves exactly the rows the live
/// per-plane seams serve.
///
/// This is the drift guard for having two entry shapes over one store. A capture
/// that skipped the revocation floors, or applied a weaker grant-row predicate,
/// would be MORE permissive than the seam it replaced — and every candidate
/// count in the suite would still look right.
#[tokio::test]
async fn the_cold_capture_serves_exactly_what_the_live_plane_seams_serve() {
    let a = org_a();
    let b = org_b();
    let (mesh, identity, dir) = mesh_with_authority("cold-capture-planes", Some(&a)).await;
    let own = EntityKeypair::generate();
    let foreign = EntityKeypair::generate();
    let tag = "nrpc:customer.read";
    let capability = cap(tag);

    inject_owner_envelope(&mesh, &a, &own, &[tag]);
    let (grant, secret) = discover_grant(&b, a.org_id(), capability, 3600);
    let grant_id = grant.grant_id;
    let secret_copy = copy_secret(&secret);
    // Bind FIRST: a granted envelope is only admissible while the consumer
    // audience is installed, which is what the bind's lease does.
    let client = bind(&mesh, &a, &identity, vec![(grant.clone(), Some(secret))]);
    inject_granted_envelope(&mesh, &b, &foreign, &grant, &secret_copy, tag);

    let capture = client.capture_private(&capability).expect("capture");
    let owner_live: Vec<_> = mesh
        .node()
        .owner_private_capability_providers(&capability)
        .into_iter()
        .map(|p| p.provider)
        .collect();
    let granted_live: Vec<_> = mesh
        .node()
        .granted_capability_providers(&grant_id)
        .into_iter()
        .map(|p| p.provider)
        .collect();
    assert_eq!(owner_live.len(), 1, "precondition: one owner-plane row");
    assert_eq!(granted_live.len(), 1, "precondition: one grant-plane row");
    assert_eq!(
        capture
            .owner_providers()
            .iter()
            .map(|p| p.provider.clone())
            .collect::<Vec<_>>(),
        owner_live,
        "the capture's owner plane must be exactly the live seam's"
    );
    assert_eq!(
        capture
            .granted_providers(&grant_id)
            .iter()
            .map(|p| p.provider.clone())
            .collect::<Vec<_>>(),
        granted_live,
        "the capture's grant plane must be exactly the live seam's — same store \
         query, same floors, same installed-grant predicate"
    );
    assert!(
        capture.granted_providers(&[0x77u8; 32]).is_empty(),
        "a grant the capture was not asked about contributes nothing"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A real revocation floor raise supersedes the capture it preceded, so the plan
/// will not mint under it.
///
/// End-to-end and honest about its coupling: on this path the raise advances the
/// routing epoch as well as the floor generation, so this witness proves the
/// TRANSITION is caught, not which component caught it. The floor generation is
/// witnessed on its own by
/// `org_routing_wiring_tests::a_captured_stamp_compares_the_revocation_floor_generation`,
/// which is where the epoch can be held equal.
///
/// Carries the adjacent control: before the raise the same stamp compares
/// CURRENT, so this cannot be satisfied by a stamp that never matches.
#[tokio::test]
async fn a_raised_revocation_floor_supersedes_the_capture_it_preceded() {
    let a = org_a();
    let (mesh, identity, dir) = mesh_with_authority("cold-floor", Some(&a)).await;
    let provider = EntityKeypair::generate();
    let tag = "nrpc:internal.reindex";
    inject_owner_envelope(&mesh, &a, &provider, &[tag]);
    let client = bind(&mesh, &a, &identity, vec![]);
    let capability = cap(tag);

    let capture = client.capture_private(&capability).expect("capture");
    assert!(
        mesh.node()
            .org_cold_authority_is_current(capture.authority()),
        "control: an untouched authority view compares CURRENT"
    );

    let mut floors = std::collections::BTreeMap::new();
    floors.insert(provider.entity_id().clone(), 2u32);
    let bundle = OrgRevocationBundle::try_issue(&a, &floors).expect("floors");
    mesh.node()
        .node_authority()
        .expect("authority")
        .revocation
        .apply_bundle(&bundle)
        .expect("the org raises a floor");

    assert!(
        !mesh
            .node()
            .org_cold_authority_is_current(capture.authority()),
        "a raised floor must supersede the authority identity captured before it"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A same-id consumer-grant replacement moves the captured authority identity.
///
/// Comparing `grant_id` alone would pass a remove-then-reinstall and a different
/// signed grant reusing the id, which is exactly how a withdrawn discovery
/// authority keeps serving. With the adjacent control: the untouched
/// installation compares CURRENT.
#[tokio::test]
async fn a_captured_stamp_notices_a_consumer_grant_replacement() {
    let a = org_a();
    let b = org_b();
    let (mesh, identity, dir) = mesh_with_authority("cold-grant-move", Some(&a)).await;
    let foreign = EntityKeypair::generate();
    let tag = "nrpc:customer.read";
    let capability = cap(tag);
    let (grant, secret) = discover_grant(&b, a.org_id(), capability, 3600);
    let grant_id = grant.grant_id;
    let secret_copy = copy_secret(&secret);
    let reinstall_secret = copy_secret(&secret);
    let client = bind(&mesh, &a, &identity, vec![(grant.clone(), Some(secret))]);
    inject_granted_envelope(&mesh, &b, &foreign, &grant, &secret_copy, tag);

    let capture = client.capture_private(&capability).expect("capture");
    assert_eq!(
        capture.granted_providers(&grant_id).len(),
        1,
        "precondition: the grant plane carries the provider"
    );
    assert!(
        mesh.node()
            .org_cold_authority_is_current(capture.authority()),
        "control: the untouched installation compares CURRENT"
    );

    // Remove and reinstall the SAME signed grant: a new installation of the same
    // authority, which is the weakest movement the comparison must still see.
    assert!(
        mesh.node().remove_consumer_grant_audience(&grant_id),
        "precondition: the grant was installed"
    );
    mesh.node()
        .install_consumer_grant_audience(grant, reinstall_secret)
        .expect("reinstall the same grant");

    assert!(
        !mesh
            .node()
            .org_cold_authority_is_current(capture.authority()),
        "a reinstallation is a DIFFERENT installation, so the captured identity \
         is superseded"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A derivation whose authority moved before the mint produces NO proof intent.
///
/// The security property of the final coherent comparison, driven through the
/// exact code `plan` runs: capture, move the node's authority, derive. The
/// control derives over an unmoved capture and mints the canonical intent, so
/// "never mints" cannot satisfy this witness.
#[tokio::test]
async fn a_plan_attempt_under_a_moved_authority_mints_nothing() {
    use super::call::PlanAttempt;
    let a = org_a();
    let (mesh, identity, dir) = mesh_with_authority("cold-superseded", Some(&a)).await;
    let provider = EntityKeypair::generate();
    let tag = "nrpc:internal.reindex";
    inject_owner_envelope(&mesh, &a, &provider, &[tag]);
    // Protected RPC is direct-session-only, so the control has to be reachable
    // or selection refuses before the comparison this witness is about.
    mesh.node()
        .test_pin_peer_entity(provider.entity_id().node_id(), provider.entity_id().clone());
    let client = bind(&mesh, &a, &identity, vec![]);
    let capability = cap(tag);

    let control = client.capture_private(&capability).expect("capture");
    match client
        .plan_attempt(&capability, &control, &unsensed())
        .expect("the control derivation succeeds")
    {
        PlanAttempt::Minted(intent) => assert_eq!(
            intent.capability, capability,
            "control: an unmoved capture mints exactly one intent for the \
             capability asked about"
        ),
        PlanAttempt::Superseded { .. } => {
            panic!("control: an unmoved capture must not report itself superseded")
        }
    }

    let capture = client.capture_private(&capability).expect("capture");
    // A same-org renewal is accepted and advances the routing epoch, so the
    // captured identity is genuinely superseded — no test seam involved.
    let entity = identity.entity_id().clone();
    let cert = OrgMembershipCert::try_issue(&a, entity.clone(), 1, 3600).expect("cert");
    let next_dir = dir.join("successor");
    let _ = std::fs::remove_dir_all(&next_dir);
    let successor =
        NodeAuthority::adopt(&next_dir, cert, &entity, 0, None).expect("adopt successor");
    mesh.node()
        .install_node_authority(Arc::new(successor))
        .expect("same-org renewal is accepted");
    assert!(
        !mesh
            .node()
            .org_cold_authority_is_current(capture.authority()),
        "precondition: the renewal superseded the captured identity"
    );

    match client
        .plan_attempt(&capability, &capture, &unsensed())
        .expect("a superseded derivation is not an error")
    {
        PlanAttempt::Superseded { considered } => assert_eq!(
            considered, 1,
            "the superseded arm still reports the candidates it examined, so the \
             eventual refusal carries a real number"
        ),
        PlanAttempt::Minted(_) => {
            panic!("a proof intent was minted under an authority identity that had moved")
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// HOLD-3 (independent review, 2026-08-29) — a superseded view speaks about
// nothing, including its own refusals.
// ---------------------------------------------------------------------------

/// Move the node's authority within its own org: a renewal is accepted and
/// advances the routing epoch, so every capture taken before it is superseded.
/// A planning input with no request-relative budget: these unit witnesses are
/// about authority and candidate derivation, not about sensed ORDER, so they
/// pass the same neutral selection every `call_bytes` does.
fn unsensed() -> super::call::SensedSelection<'static> {
    super::call::SensedSelection::new("net.unit.test", 0)
}

fn renew_authority(mesh: &Mesh, org: &OrgKeypair, identity: &Identity, dir: &std::path::Path) {
    let entity = identity.entity_id().clone();
    let cert = OrgMembershipCert::try_issue(org, entity.clone(), 1, 3600).expect("cert");
    let next = dir.join("successor");
    let _ = std::fs::remove_dir_all(&next);
    let authority = NodeAuthority::adopt(&next, cert, &entity, 0, None).expect("adopt successor");
    mesh.node()
        .install_node_authority(Arc::new(authority))
        .expect("same-org renewal is accepted");
}

/// A `NoAuthorizedProvider` derived under a superseded capture never escapes.
///
/// Movement causes this class: a removed consumer grant or a raised floor empties
/// a plane, so "nothing authorized" is exactly what a stale view reports. Dies to
/// applying `?` to the derivation before the comparison — the stale refusal then
/// escapes with authority and outside the bounded budget.
#[tokio::test]
async fn a_superseded_no_provider_derivation_does_not_escape() {
    let a = org_a();
    let (mesh, identity, dir) = mesh_with_authority("cold-superseded-none", Some(&a)).await;
    let client = bind(&mesh, &a, &identity, vec![]);
    let capability = cap("nrpc:internal.reindex");

    let capture = client.capture_private(&capability).expect("capture");
    // Control: still current, so the exact refusal is preserved verbatim.
    match client.plan_attempt(&capability, &capture, &unsensed()) {
        Err(OrgSdkError::Discovery(OrgDiscoveryError::NoAuthorizedProvider {
            considered, ..
        })) => assert_eq!(considered, 0, "control: nothing discovered, nothing hidden"),
        other => panic!("control: expected NoAuthorizedProvider, got {other:?}"),
    }

    renew_authority(&mesh, &a, &identity, &dir);
    match client
        .plan_attempt(&capability, &capture, &unsensed())
        .expect("a superseded derivation is not an error")
    {
        super::call::PlanAttempt::Superseded { considered } => assert_eq!(considered, 0),
        super::call::PlanAttempt::Minted(_) => panic!("nothing was mintable"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// An `AmbiguousCapabilityGrant` derived under a superseded capture never
/// escapes either — the authority-bearing class movement can CREATE, by
/// installing a second overlapping grant.
#[tokio::test]
async fn a_superseded_ambiguity_derivation_does_not_escape() {
    let (a, b) = (org_a(), org_b());
    let (mesh, identity, dir) = mesh_with_authority("cold-superseded-ambig", Some(&a)).await;
    let provider = EntityKeypair::generate();
    let tag = "nrpc:customer.read";
    let capability = cap(tag);
    let (g1, s1) = discover_grant(&b, a.org_id(), capability, 3600);
    let (g2, s2) = discover_grant(&b, a.org_id(), capability, 3600);
    let s1_copy = copy_secret(&s1);
    let client = bind(
        &mesh,
        &a,
        &identity,
        vec![(g1.clone(), Some(s1)), (g2.clone(), Some(s2))],
    );
    inject_granted_envelope(&mesh, &b, &provider, &g1, &s1_copy, tag);

    let capture = client.capture_private(&capability).expect("capture");
    match client.plan_attempt(&capability, &capture, &unsensed()) {
        Err(OrgSdkError::Credentials(OrgCredentialError::AmbiguousCapabilityGrant {
            grant_ids,
            ..
        })) => assert_eq!(
            grant_ids.len(),
            2,
            "control: the exact ambiguity is reported"
        ),
        other => panic!("control: expected AmbiguousCapabilityGrant, got {other:?}"),
    }

    renew_authority(&mesh, &a, &identity, &dir);
    match client
        .plan_attempt(&capability, &capture, &unsensed())
        .expect("a superseded derivation is not an error")
    {
        super::call::PlanAttempt::Superseded { considered } => assert_eq!(
            considered, 1,
            "the superseded arm reports the candidates it examined"
        ),
        super::call::PlanAttempt::Minted(_) => panic!("an ambiguous plan is never mintable"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// The exported path gates its negative outcomes the same way.
#[tokio::test]
async fn a_superseded_exported_derivation_does_not_escape() {
    let a = org_a();
    let (mesh, identity, dir) = mesh_with_authority("cold-superseded-exported", Some(&a)).await;
    let client = bind(&mesh, &a, &identity, vec![]);
    let capability = cap("nrpc:public.svc");
    let authority = mesh.node().org_cold_authority().expect("authority capture");

    match client.plan_exported_attempt(&capability, "public.svc", &authority) {
        Err(OrgSdkError::Discovery(OrgDiscoveryError::NoAuthorizedProvider {
            considered, ..
        })) => assert_eq!(considered, 0, "control: the public plane is empty"),
        other => panic!("control: expected NoAuthorizedProvider, got {other:?}"),
    }

    renew_authority(&mesh, &a, &identity, &dir);
    match client
        .plan_exported_attempt(&capability, "public.svc", &authority)
        .expect("a superseded derivation is not an error")
    {
        super::call::PlanAttempt::Superseded { considered } => assert_eq!(considered, 0),
        super::call::PlanAttempt::Minted(_) => panic!("nothing was mintable"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Three superseded attempts exhaust the bounded budget and refuse LOCALLY with
/// the capability asked about and the count the last derivation examined.
///
/// Drives the production loop with a capture that is already superseded, so the
/// budget is observable without racing authority movement three times. Dies to an
/// unbounded loop (the witness would hang) and to a budget that reports a
/// different capability or count.
#[tokio::test]
async fn three_superseded_attempts_refuse_locally_with_the_last_count() {
    let a = org_a();
    let (mesh, identity, dir) = mesh_with_authority("cold-exhaustion", Some(&a)).await;
    let provider = EntityKeypair::generate();
    let tag = "nrpc:internal.reindex";
    inject_owner_envelope(&mesh, &a, &provider, &[tag]);
    mesh.node()
        .test_pin_peer_entity(provider.entity_id().node_id(), provider.entity_id().clone());
    let client = bind(&mesh, &a, &identity, vec![]);
    let capability = cap(tag);

    let stale = client.capture_private(&capability).expect("capture");
    renew_authority(&mesh, &a, &identity, &dir);
    assert!(
        !mesh.node().org_cold_authority_is_current(stale.authority()),
        "precondition: every attempt below derives under a superseded capture"
    );

    let attempts = std::cell::Cell::new(0usize);
    let err = client
        .plan_over(&capability, &unsensed(), || {
            attempts.set(attempts.get() + 1);
            Ok(stale.clone())
        })
        .expect_err("a superseded plan never mints");
    assert_eq!(
        attempts.get(),
        3,
        "the re-derivation budget is bounded at 3"
    );
    match err {
        OrgSdkError::Discovery(OrgDiscoveryError::NoAuthorizedProvider {
            capability: reported,
            considered,
        }) => {
            assert_eq!(
                reported,
                super::error::hex_capability(&capability),
                "the refusal names the capability the caller asked about"
            );
            assert_eq!(
                considered, 1,
                "and carries the count the last derivation examined"
            );
        }
        other => panic!("expected a local NoAuthorizedProvider, got {other:?}"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Both capture refusals map onto the EXISTING local vocabulary through the
/// production loop — no new error kind, and no silent widening.
#[tokio::test]
async fn cold_capture_refusals_map_onto_the_existing_vocabulary() {
    use net::adapter::net::behavior::org_cold_plan::OrgColdRefusal;
    let a = org_a();
    let (mesh, identity, dir) = mesh_with_authority("cold-refusal-map", Some(&a)).await;
    let client = bind(&mesh, &a, &identity, vec![]);
    let capability = cap("nrpc:internal.reindex");

    match client.plan_over(&capability, &unsensed(), || {
        Err(OrgColdRefusal::NoNodeAuthority)
    }) {
        Err(OrgSdkError::Credentials(OrgCredentialError::NodeAuthorityRequired)) => {}
        other => panic!("expected NodeAuthorityRequired, got {other:?}"),
    }
    match client.plan_over(&capability, &unsensed(), || {
        Err(OrgColdRefusal::IncoherentAuthority)
    }) {
        Err(OrgSdkError::Discovery(OrgDiscoveryError::NoAuthorizedProvider {
            considered, ..
        })) => assert_eq!(considered, 0, "nothing was coherently discovered"),
        other => panic!("expected NoAuthorizedProvider, got {other:?}"),
    }
    match client.plan_exported_over(&capability, "public.svc", || {
        Err(OrgColdRefusal::NoNodeAuthority)
    }) {
        Err(OrgSdkError::Credentials(OrgCredentialError::NodeAuthorityRequired)) => {}
        other => panic!("expected NodeAuthorityRequired on the exported path, got {other:?}"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// F2 (independent review, 2026-08-29) — the proof intent is constructed only
// AFTER the final currentness comparison, on both paths.
//
// The first repair satisfied every behavioural assertion while minting BEFORE
// the comparison: `Superseded` was returned, no intent escaped, and no test
// could tell. These witnesses close that by counting CONSTRUCTIONS, so the
// sequence itself is observable.
// ---------------------------------------------------------------------------

/// A superseded PRIVATE attempt constructs no proof intent at all, and the
/// positive control constructs exactly one.
///
/// Dies to moving `intent_for` back before the comparison: the superseded arm
/// would then construct an intent it throws away, and the count would rise.
#[tokio::test]
async fn a_superseded_private_attempt_constructs_no_intent() {
    let a = org_a();
    let (mesh, identity, dir) = mesh_with_authority("cold-mint-order", Some(&a)).await;
    let provider = EntityKeypair::generate();
    let tag = "nrpc:internal.reindex";
    inject_owner_envelope(&mesh, &a, &provider, &[tag]);
    mesh.node()
        .test_pin_peer_entity(provider.entity_id().node_id(), provider.entity_id().clone());
    let client = bind(&mesh, &a, &identity, vec![]);
    let capability = cap(tag);

    // Positive control FIRST, so a zero delta below cannot come from a plan that
    // never selects anything.
    let current = client.capture_private(&capability).expect("capture");
    let before = super::call::intents_constructed_on_this_thread();
    match client
        .plan_attempt(&capability, &current, &unsensed())
        .expect("the control derivation succeeds")
    {
        super::call::PlanAttempt::Minted(intent) => {
            assert_eq!(
                intent.capability, capability,
                "control: the canonical intent"
            )
        }
        super::call::PlanAttempt::Superseded { .. } => {
            panic!("control: an unmoved capture must mint")
        }
    }
    assert_eq!(
        super::call::intents_constructed_on_this_thread() - before,
        1,
        "control: a current attempt constructs EXACTLY one intent"
    );

    let stale = client.capture_private(&capability).expect("capture");
    renew_authority(&mesh, &a, &identity, &dir);
    let before = super::call::intents_constructed_on_this_thread();
    match client
        .plan_attempt(&capability, &stale, &unsensed())
        .expect("a superseded derivation is not an error")
    {
        super::call::PlanAttempt::Superseded { considered } => assert_eq!(considered, 1),
        super::call::PlanAttempt::Minted(_) => panic!("a superseded capture must not mint"),
    }
    assert_eq!(
        super::call::intents_constructed_on_this_thread(),
        before,
        "a superseded attempt must construct NO proof intent — not even one it \
         discards: §10 puts the comparison BETWEEN selection and the mint"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The exported attempt has the same sequence: select, compare, then mint.
///
/// Its derivation refuses on an empty public plane, so this witness pairs the
/// superseded arm with a construction count rather than with a minted intent —
/// and asserts the count stays at zero in BOTH the refusing-current and the
/// superseded cases, because neither may construct a proof.
#[tokio::test]
async fn a_superseded_exported_attempt_constructs_no_intent() {
    let a = org_a();
    let (mesh, identity, dir) = mesh_with_authority("cold-mint-order-exported", Some(&a)).await;
    let client = bind(&mesh, &a, &identity, vec![]);
    let capability = cap("nrpc:public.svc");
    let authority = mesh.node().org_cold_authority().expect("authority capture");

    let before = super::call::intents_constructed_on_this_thread();
    assert!(
        client
            .plan_exported_attempt(&capability, "public.svc", &authority)
            .is_err(),
        "control: the empty public plane refuses while the capture is current"
    );
    assert_eq!(
        super::call::intents_constructed_on_this_thread(),
        before,
        "a refusal constructs no intent"
    );

    renew_authority(&mesh, &a, &identity, &dir);
    match client
        .plan_exported_attempt(&capability, "public.svc", &authority)
        .expect("a superseded derivation is not an error")
    {
        super::call::PlanAttempt::Superseded { considered } => assert_eq!(considered, 0),
        super::call::PlanAttempt::Minted(_) => panic!("nothing was mintable"),
    }
    assert_eq!(
        super::call::intents_constructed_on_this_thread(),
        before,
        "and neither does a superseded exported attempt"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// OA-6 — the sensed step inside the authority window, and around the
// deferred candidate semantics
// ---------------------------------------------------------------------------

/// Authority movement DURING nonempty sensed planning is still caught by the
/// FINAL currentness comparison, and mints nothing.
///
/// The older superseded-attempt witnesses move the authority BEFORE
/// `plan_attempt` and carry one candidate, so the sensed step returns
/// immediately for them: a comparison moved earlier would still satisfy them.
/// Here the movement happens INSIDE the sensed step, over two candidates,
/// through a `#[cfg(test)]` seam that fires after reconciliation and before
/// the projection — which is exactly the window a fence-before-sensing
/// ordering would leave open.
#[tokio::test]
async fn authority_movement_inside_sensed_planning_is_still_fenced() {
    let a = org_a();
    let (mesh, identity, dir) = mesh_with_authority("plan-sensed-fence", Some(&a)).await;
    let p1 = EntityKeypair::generate();
    let p2 = EntityKeypair::generate();
    for provider in [&p1, &p2] {
        inject_owner_envelope(&mesh, &a, provider, &["nrpc:internal.reindex"]);
        mesh.node()
            .test_pin_peer_entity(provider.entity_id().node_id(), provider.entity_id().clone());
    }
    let client = bind(&mesh, &a, &identity, vec![]);
    let capability = cap("nrpc:internal.reindex");
    let sensed = super::call::SensedSelection::new("nrpc:internal.reindex", 0);

    // Precondition: TWO pinned same-organization candidates, so the sensed
    // step really runs rather than returning on a single candidate.
    let (candidates, _) = client
        .authorized_candidates(&capability)
        .expect("authority decision");
    assert_eq!(candidates.len(), 2, "the sensed window must be nonempty");
    assert!(candidates.iter().all(|c| c.direct));

    // A capture that is CURRENT when the attempt starts.
    let capture = client.capture_private(&capability).expect("capture");
    let before = super::call::intents_constructed_on_this_thread();

    // The movement lands inside the sensed step.
    // The successor authority is prepared UP FRONT, so the seam itself only
    // installs it: the movement is what must land inside the sensed step, and
    // the ceremony around it is irrelevant to that.
    let successor = {
        let entity = identity.entity_id().clone();
        let cert = OrgMembershipCert::try_issue(&a, entity.clone(), 1, 3600).expect("cert");
        let next = dir.join("successor");
        let _ = std::fs::remove_dir_all(&next);
        Arc::new(NodeAuthority::adopt(&next, cert, &entity, 0, None).expect("adopt successor"))
    };
    let moved = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    {
        let node = mesh.node().clone();
        let moved = moved.clone();
        super::call::set_sensing_planning_seam(Some(std::sync::Arc::new(move || {
            // Once: a seam that renewed on every projection would prove
            // nothing about ordering.
            if moved.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                node.install_node_authority(successor.clone())
                    .expect("same-org renewal is accepted");
            }
        })));
    }
    let attempt = client.plan_attempt(&capability, &capture, &sensed);
    super::call::set_sensing_planning_seam(None);

    assert_eq!(
        moved.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "the seam must have fired inside the sensed step"
    );
    match attempt {
        Ok(super::call::PlanAttempt::Superseded { .. }) => {}
        other => panic!("a capture superseded inside sensed planning must not mint: {other:?}"),
    }
    assert_eq!(
        super::call::intents_constructed_on_this_thread(),
        before,
        "and NOTHING may be constructed for it"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A provider on BOTH planes is one `SameOrg` candidate, a `Granted`-only
/// provider is never ranked and never pruned, and the permutation keeps the
/// complete list.
///
/// The granted-only provider sits in the MIDDLE of the globally sorted list
/// deliberately. That position is what makes the mask load-bearing: a mapping
/// that treated `Granted` as same-organization would move it to the FRONT when
/// ranked and to the BACK when pruned, whereas at the head of the list both
/// mistakes are invisible. Ranking and pruning are covered as separate cases
/// for the same reason.
#[tokio::test]
async fn the_sensed_adapter_never_ranks_or_prunes_a_granted_candidate() {
    let (a, b) = (org_a(), org_b());
    let (mesh, identity, dir) = mesh_with_authority("plan-sensed-modes", Some(&a)).await;

    // Three providers, ordered the way the candidate list is: by provider
    // entity bytes. The MIDDLE one is the granted-only stranger.
    let mut keys = vec![
        EntityKeypair::generate(),
        EntityKeypair::generate(),
        EntityKeypair::generate(),
    ];
    keys.sort_by(|x, y| x.entity_id().as_bytes().cmp(y.entity_id().as_bytes()));
    let dual = keys.remove(0);
    let granted_only = keys.remove(0);
    let owner_only = keys.remove(0);

    inject_owner_envelope(&mesh, &a, &dual, &["nrpc:internal.reindex"]);
    inject_owner_envelope(&mesh, &a, &owner_only, &["nrpc:internal.reindex"]);
    let (grant, secret) = discover_grant(&b, a.org_id(), cap("nrpc:internal.reindex"), 3600);
    let client = bind(
        &mesh,
        &a,
        &identity,
        vec![(grant.clone(), Some(copy_secret(&secret)))],
    );
    for provider in [&granted_only, &dual] {
        inject_granted_envelope(
            &mesh,
            &b,
            provider,
            &grant,
            &secret,
            "nrpc:internal.reindex",
        );
    }
    for provider in [&owner_only, &dual, &granted_only] {
        mesh.node()
            .test_pin_peer_entity(provider.entity_id().node_id(), provider.entity_id().clone());
    }

    let capability = cap("nrpc:internal.reindex");
    let (candidates, considered) = client
        .authorized_candidates(&capability)
        .expect("authority decision");
    assert_eq!(
        candidates.len(),
        3,
        "three distinct providers, the dual-plane one exactly ONCE: {candidates:?}"
    );
    assert_eq!(considered, 3, "considered is the discovery count");
    assert_eq!(
        &candidates[0].provider,
        dual.entity_id(),
        "precondition: the sorted list is dual, granted-only, owner-only"
    );
    assert_eq!(&candidates[1].provider, granted_only.entity_id());
    assert_eq!(&candidates[2].provider, owner_only.entity_id());
    assert!(
        matches!(candidates[0].mode, Mode::SameOrg),
        "owner-first dedup: a provider on both planes is SameOrg"
    );
    assert!(
        matches!(candidates[1].mode, Mode::Granted(_)),
        "and a stranger stays on the granted plane"
    );
    assert!(matches!(candidates[2].mode, Mode::SameOrg));

    let granted_id = granted_only.entity_id().node_id();
    let identity_permutation: Vec<usize> = (0..candidates.len()).collect();

    // CASE 1 - RANKED. Sensing is owner-plane only, so ranking the granted id
    // moves nothing. A mapping that called it same-organization would emit it
    // FIRST.
    assert_eq!(
        super::call::org_sensed_candidate_permutation(&candidates, &[granted_id], &[]),
        identity_permutation,
        "a Granted id cannot be ranked: the input order is kept"
    );

    // CASE 2 - PRUNED. Same reasoning in the other direction: a mapping that
    // called it same-organization would emit it LAST.
    assert_eq!(
        super::call::org_sensed_candidate_permutation(&candidates, &[], &[granted_id]),
        identity_permutation,
        "a Granted id cannot be pruned either"
    );

    // CASE 3 - both at once, which is what the granted plane looks like to a
    // projection that has no row for it at all.
    assert_eq!(
        super::call::org_sensed_candidate_permutation(&candidates, &[granted_id], &[granted_id]),
        identity_permutation,
    );

    // And a real SameOrg provider IS rankable: the last one leads, the granted
    // candidate keeps its relative place, nothing is dropped.
    let owner_id = owner_only.entity_id().node_id();
    let mut reordered = candidates.clone();
    let permutation = super::call::org_sensed_candidate_permutation(&reordered, &[owner_id], &[]);
    assert_eq!(
        permutation,
        vec![2, 0, 1],
        "the sensed SameOrg provider leads"
    );
    super::call::apply_permutation(&mut reordered, permutation);
    assert_eq!(&reordered[0].provider, owner_only.entity_id());
    assert_eq!(reordered.len(), 3, "and nothing was dropped");
    assert!(
        reordered
            .iter()
            .any(|c| &c.provider == granted_only.entity_id()),
        "including the granted candidate, unsensed and unpruned"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Selection still decides AFTER the order: `direct` beats rank, and the
/// not-direct error names the reordered leader rather than the sorted one.
#[tokio::test]
async fn selection_and_its_errors_follow_the_reordered_list() {
    let a = org_a();
    let (mesh, identity, dir) = mesh_with_authority("plan-sensed-select", Some(&a)).await;
    let p1 = EntityKeypair::generate();
    let p2 = EntityKeypair::generate();
    for provider in [&p1, &p2] {
        inject_owner_envelope(&mesh, &a, provider, &["nrpc:internal.reindex"]);
    }
    let (lower, higher) = if p1.entity_id() < p2.entity_id() {
        (&p1, &p2)
    } else {
        (&p2, &p1)
    };
    let client = bind(&mesh, &a, &identity, vec![]);
    let capability = cap("nrpc:internal.reindex");

    // NEITHER is pinned: the error must name the leader of the order actually
    // in force, which is the reordered one.
    let (candidates, considered) = client
        .authorized_candidates(&capability)
        .expect("authority decision");
    let mut reordered = candidates.clone();
    let permutation = super::call::org_sensed_candidate_permutation(
        &reordered,
        &[higher.entity_id().node_id()],
        &[],
    );
    super::call::apply_permutation(&mut reordered, permutation);
    assert_eq!(&reordered[0].provider, higher.entity_id());
    match client.select_candidate(&capability, &reordered, considered) {
        Err(OrgSdkError::Discovery(OrgDiscoveryError::ProviderNotDirect { provider })) => {
            assert_eq!(
                &provider,
                higher.entity_id(),
                "the not-direct error names the order's leader, not the sorted first"
            );
        }
        other => panic!("expected ProviderNotDirect, got {other:?}"),
    }
    // The unreordered list names the OTHER provider, which is what makes the
    // assertion above about the order rather than about the fixture.
    match client.select_candidate(&capability, &candidates, considered) {
        Err(OrgSdkError::Discovery(OrgDiscoveryError::ProviderNotDirect { provider })) => {
            assert_eq!(&provider, lower.entity_id());
        }
        other => panic!("expected ProviderNotDirect, got {other:?}"),
    }

    // Now PIN only the lower provider and rank the higher one first: the
    // pin-binding annotation still decides, so selection takes the pinned one
    // despite the rank. (`direct` means an identity pin here - a predicate
    // that predates this slice; the live-session question is a separate
    // network witness.)
    mesh.node()
        .test_pin_peer_entity(lower.entity_id().node_id(), lower.entity_id().clone());
    let (candidates, considered) = client
        .authorized_candidates(&capability)
        .expect("authority decision");
    let mut reordered = candidates.clone();
    let permutation = super::call::org_sensed_candidate_permutation(
        &reordered,
        &[higher.entity_id().node_id()],
        &[],
    );
    super::call::apply_permutation(&mut reordered, permutation);
    assert_eq!(&reordered[0].provider, higher.entity_id());
    let chosen = client
        .select_candidate(&capability, &reordered, considered)
        .expect("a direct candidate exists");
    assert_eq!(
        &chosen.provider,
        lower.entity_id(),
        "an unpinned sensed leader must not be selected over a pinned one"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The reconciliation trigger, as a decision table over a REAL demand.
///
/// The record certifies the demand it was taken from, so this drives it with
/// actual `OrgSensingCapabilityDemand` values: a record cannot outlive the
/// demand's identity, cannot claim a narrower population settled the first
/// time it sees it, and cannot grow without bound.
#[tokio::test]
async fn the_reconciliation_trigger_certifies_the_installed_demand() {
    use net::adapter::net::behavior::org_sensing_demand::OrgSensingFamily;
    use std::time::{Duration, Instant};

    let a = org_a();
    let (mesh, _identity, dir) = mesh_with_authority("plan-trigger", Some(&a)).await;
    let family = OrgSensingFamily::mint(mesh.node()).expect("mint");
    let capability = cap("nrpc:internal.reindex");
    let schedule = super::client::ConvergenceSchedule::default();
    let floor = Duration::from_secs(2);
    let t0 = Instant::now();

    // No demand at all: converge.
    assert!(schedule.needs_convergence(&capability, &[1, 2], None, t0, floor));

    // A real demand, with nothing discovered - so its population is EMPTY and
    // cannot cover an expectation of two providers.
    let demand = family.retain("nrpc:internal.reindex").expect("retain");
    assert!(demand.population().is_empty(), "nothing is discovered here");
    assert!(schedule.needs_convergence(&capability, &[1, 2], Some(&demand), t0, floor));

    // Certifying it does NOT settle it: the published population is narrower
    // than the expectation, which is exactly the interleaving where a
    // discovery row expired between the caller's capture and core's query.
    schedule.certify(capability, vec![1, 2], &demand, t0);
    assert!(
        !schedule.needs_convergence(&capability, &[1, 2], Some(&demand), t0, floor),
        "but it is floored, not retried on the very next call"
    );
    assert!(
        schedule.needs_convergence(&capability, &[1, 2], Some(&demand), t0 + floor, floor),
        "and it IS retried once the floor has passed"
    );

    // A retry that produces the identical population is a FIXED POINT, not a
    // loop: certify the same pair again and it settles.
    schedule.certify(capability, vec![1, 2], &demand, t0 + floor);
    assert!(
        !schedule.needs_convergence(
            &capability,
            &[1, 2],
            Some(&demand),
            t0 + Duration::from_secs(600),
            floor
        ),
        "a fixed point is not re-run by the passage of time"
    );

    // A CHANGED expectation converges immediately, with no floor.
    assert!(schedule.needs_convergence(&capability, &[1, 2, 3], Some(&demand), t0, floor));
    assert!(schedule.needs_convergence(&capability, &[1], Some(&demand), t0, floor));

    // A REPLACED demand invalidates the record outright, even though the
    // expectation is unchanged: the record certified a different installation.
    let replaced = family.retain("nrpc:internal.reindex").expect("re-retain");
    assert!(
        !std::sync::Arc::ptr_eq(&demand, &replaced),
        "precondition: the convergence published a new demand"
    );
    assert!(
        schedule.needs_convergence(
            &capability,
            &[1, 2],
            Some(&replaced),
            t0 + Duration::from_secs(600),
            floor
        ),
        "a record must never certify a demand it was not taken from"
    );

    // A refusal certifies nothing.
    schedule.certify(capability, vec![1, 2], &replaced, t0);
    schedule.record_refusal(capability, vec![1, 2], t0);
    assert!(schedule.needs_convergence(&capability, &[1, 2], Some(&replaced), t0, floor));

    // And the record set is BOUNDED: many capability names cannot grow it.
    for index in 0..200u32 {
        let other = cap(&format!("nrpc:svc.{index}"));
        schedule.record_refusal(other, vec![1], t0 + Duration::from_millis(index as u64));
    }
    assert!(
        schedule.len() <= 64,
        "the convergence record set must stay bounded, got {}",
        schedule.len()
    );

    drop(demand);
    drop(replaced);
    drop(family);
    let _ = std::fs::remove_dir_all(&dir);
}
