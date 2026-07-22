//! OSDK S1 witnesses — the authority decision behind `org.call`.
//!
//! Tier note: these exercise the REAL private-discovery store (envelopes are
//! built by the canonical builders and admitted through the real
//! `verify_scoped_ingest` path), the real credential predicates, and the real
//! intent construction — everything `call` does before the network. The live
//! two-node traversal of `verify_org_admission` is S3.

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
    let (targets, considered) = client
        .authorized_targets(&capability)
        .expect("authority decision");

    assert_eq!(considered, 1, "one owner-private candidate");
    assert_eq!(targets.len(), 1);
    assert_eq!(&targets[0].0, provider.entity_id());
    assert_eq!(targets[0].1, Mode::SameOrg);

    let intent = client.intent_for(capability, targets[0].0.clone(), targets[0].1.clone());
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
    let (targets, considered) = client
        .authorized_targets(&capability)
        .expect("authority decision");
    assert_eq!(considered, 1);
    assert_eq!(targets.len(), 1, "one authorized cross-org target");
    assert_eq!(targets[0].1, Mode::Granted(Box::new(grant.clone())));

    let intent = client.intent_for(capability, targets[0].0.clone(), targets[0].1.clone());
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
    let (targets, considered) = client
        .authorized_targets(&cap("nrpc:public.svc"))
        .expect("authority decision");
    assert_eq!(considered, 0, "a public announcement is not a candidate");
    assert!(targets.is_empty());
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
            .authorized_targets(&cap("nrpc:internal.reindex"))
            .expect("decision")
            .1,
        1
    );
    assert_eq!(
        client
            .authorized_targets(&cap("nrpc:not.declared"))
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

    let (targets, considered) = client
        .authorized_targets(&cap("nrpc:customer.read"))
        .expect("authority decision");
    assert_eq!(considered, 1, "discovery DID resolve the provider");
    assert!(
        targets.is_empty(),
        "but DISCOVER alone is never invocation authority"
    );

    let err = client
        .plan("customer.read")
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
        .authorized_targets(&capability)
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

    let (targets, considered) = client
        .authorized_targets(&capability)
        .expect("authority decision");
    assert_eq!(considered, 1, "resolved");
    assert!(targets.is_empty(), "but not covered by any INVOKE grant");
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
        let (targets, considered) = client
            .authorized_targets(&capability)
            .expect("authority decision");
        assert_eq!(considered, 2);
        assert_eq!(targets.len(), 2);
        assert_eq!(targets[0].0, expected, "lowest entity id wins, every time");
    }
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
    let err = client.plan("internal.reindex").expect_err("unreachable");
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
        .plan("internal.reindex")
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

    let err = client.plan("internal.reindex").expect_err("out of scope");
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
    let err = client.plan("internal.reindex").expect_err("expired");
    assert!(
        matches!(
            err,
            OrgSdkError::Credentials(OrgCredentialError::NotCurrentlyValid { .. })
        ),
        "got {err:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
