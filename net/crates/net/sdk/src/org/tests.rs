//! OSDK S0 witnesses — credential construction, the binding relation, and the
//! consumer-audience lease lifecycle.
//!
//! Tier note (mirroring OA-4): these are local/unit and node-level witnesses.
//! They prove the facade refuses what the substrate would refuse and that
//! installed ingest authority tracks live credential possession. Live
//! two-node admission is S1/S3.

use std::sync::Arc;

use net::adapter::net::identity::EntityKeypair;
use net::adapter::net::{MeshNode, MeshNodeConfig};

use super::credentials::OrgCredentials;
use super::error::OrgCredentialError;
use super::types::*;
use crate::identity::Identity;
use crate::mesh::Mesh;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

pub(super) fn org_a() -> OrgKeypair {
    OrgKeypair::from_bytes([0xA1u8; 32])
}
pub(super) fn org_b() -> OrgKeypair {
    OrgKeypair::from_bytes([0xB2u8; 32])
}
fn org_c() -> OrgKeypair {
    OrgKeypair::from_bytes([0xC3u8; 32])
}

pub(super) fn cap(tag: &str) -> CapabilityAuthorityId {
    CapabilityAuthorityId::for_tag(tag)
}

/// Membership + dispatcher for `member` acting for `org`, both wide open.
pub(super) fn belonging(
    org: &OrgKeypair,
    member: &net::adapter::net::identity::EntityId,
) -> (OrgMembershipCert, OrgDispatcherGrant) {
    let cert = OrgMembershipCert::try_issue(org, member.clone(), 1, 3600).expect("cert");
    let grant =
        OrgDispatcherGrant::try_issue(org, member.clone(), DispatcherScope::Any, 3600).expect("dg");
    (cert, grant)
}

/// A DISCOVER|INVOKE grant from `issuer` to `grantee_org` over `capability`.
pub(super) fn discover_grant(
    issuer: &OrgKeypair,
    grantee_org: OrgId,
    capability: CapabilityAuthorityId,
    ttl: u64,
) -> (OrgCapabilityGrant, OrgAudienceSecret) {
    let (grant, secret) = OrgCapabilityGrant::try_issue(
        issuer,
        grantee_org,
        capability,
        GrantRights::INVOKE.union(GrantRights::DISCOVER),
        GrantTargetScope::AnyNodeOwnedBy(issuer.org_id()),
        ttl,
    )
    .expect("grant");
    (grant, secret.expect("discover mints a secret"))
}

/// A mesh with a durable identity and (optionally) an adopted node authority
/// owned by `owner`. Returns the mesh and the authority dir to clean up.
pub(super) async fn mesh_with_authority(
    tag: &str,
    owner: Option<&OrgKeypair>,
) -> (Mesh, Identity, std::path::PathBuf) {
    let identity = Identity::generate();
    let mesh = Mesh::builder("127.0.0.1:0", &[0x51u8; 32])
        .expect("builder")
        .identity(identity.clone())
        .build()
        .await
        .expect("mesh");
    let dir = std::env::temp_dir().join(format!(
        "net-osdk-s0-{tag}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    if let Some(owner) = owner {
        let entity = identity.entity_id().clone();
        let cert = OrgMembershipCert::try_issue(owner, entity.clone(), 1, 3600).expect("cert");
        let authority = NodeAuthority::adopt(&dir, cert, &entity, 0, None).expect("adopt");
        mesh.node()
            .install_node_authority(Arc::new(authority))
            .expect("install authority");
    }
    (mesh, identity, dir)
}

/// A bare adopted node (no SDK mesh) — used to install a consumer audience
/// through the LOW-LEVEL API so the lease's non-owning path can be witnessed.
async fn adopted_node(
    tag: &str,
    owner: &OrgKeypair,
) -> (Arc<MeshNode>, EntityKeypair, std::path::PathBuf) {
    let kp = EntityKeypair::generate();
    let cfg = MeshNodeConfig::new("127.0.0.1:0".parse().expect("addr"), [0x52u8; 32]);
    let node = Arc::new(MeshNode::new(kp.clone(), cfg).await.expect("node"));
    let entity = node.entity_id().clone();
    let cert = OrgMembershipCert::try_issue(owner, entity.clone(), 1, 3600).expect("cert");
    let dir = std::env::temp_dir().join(format!("net-osdk-s0-low-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let authority = NodeAuthority::adopt(&dir, cert, &entity, 0, None).expect("adopt");
    node.install_node_authority(Arc::new(authority))
        .expect("install");
    (node, kp, dir)
}

// ---------------------------------------------------------------------------
// 1. Construction — structural relationships and signatures only
// ---------------------------------------------------------------------------

#[test]
fn credentials_accept_a_coherent_set_without_checking_windows() {
    let a = org_a();
    let member = EntityKeypair::generate().entity_id().clone();
    let (cert, dg) = belonging(&a, &member);
    let (grant, secret) = discover_grant(&org_b(), a.org_id(), cap("nrpc:svc"), 3600);

    let creds = OrgCredentials::new(cert, dg, vec![grant], vec![secret]).expect("assembles");
    assert_eq!(creds.acting_org(), a.org_id());
    assert_eq!(creds.member(), &member);
    assert_eq!(creds.grants().len(), 1);
}

#[test]
fn credentials_may_be_assembled_before_their_validity_window() {
    // The ruling's contract: construction is structural. A credential set whose
    // grant is NOT YET valid still assembles — only binding and calling care.
    let a = org_a();
    let member = EntityKeypair::generate().entity_id().clone();
    let (cert, dg) = belonging(&a, &member);
    // `try_issue` mints not_before = now, so use a grant that is already
    // expired instead: same point (a window failure is not a construction
    // failure), and expiry is reachable without clock control.
    let (grant, secret) = discover_grant(&org_b(), a.org_id(), cap("nrpc:svc"), 1);
    std::thread::sleep(std::time::Duration::from_millis(1100));

    OrgCredentials::new(cert, dg, vec![grant], vec![secret])
        .expect("an expired grant still ASSEMBLES — windows are not a construction check");
}

#[test]
fn credentials_reject_a_dispatcher_grant_for_a_different_entity() {
    let a = org_a();
    let member = EntityKeypair::generate().entity_id().clone();
    let other = EntityKeypair::generate().entity_id().clone();
    let cert = OrgMembershipCert::try_issue(&a, member, 1, 3600).expect("cert");
    let dg = OrgDispatcherGrant::try_issue(&a, other, DispatcherScope::Any, 3600).expect("dg");

    let err = OrgCredentials::new(cert, dg, vec![], vec![]).expect_err("must refuse");
    assert!(
        matches!(err, OrgCredentialError::DispatcherBindingMismatch { .. }),
        "got {err:?}"
    );
}

#[test]
fn credentials_reject_disagreeing_acting_orgs() {
    let (a, b) = (org_a(), org_b());
    let member = EntityKeypair::generate().entity_id().clone();
    let cert = OrgMembershipCert::try_issue(&a, member.clone(), 1, 3600).expect("cert");
    // Dispatcher grant signed by a DIFFERENT org for the same entity.
    let dg = OrgDispatcherGrant::try_issue(&b, member, DispatcherScope::Any, 3600).expect("dg");

    let err = OrgCredentials::new(cert, dg, vec![], vec![]).expect_err("must refuse");
    assert!(
        matches!(err, OrgCredentialError::ActingOrgMismatch { .. }),
        "got {err:?}"
    );
}

#[test]
fn credentials_reject_a_grant_issued_to_another_org() {
    let a = org_a();
    let member = EntityKeypair::generate().entity_id().clone();
    let (cert, dg) = belonging(&a, &member);
    // B grants to C, not to A — this wallet holds only grants naming its org.
    let (grant, secret) = discover_grant(&org_b(), org_c().org_id(), cap("nrpc:svc"), 3600);

    let err = OrgCredentials::new(cert, dg, vec![grant], vec![secret]).expect_err("must refuse");
    assert!(
        matches!(err, OrgCredentialError::GrantNotForActingOrg { .. }),
        "got {err:?}"
    );
}

#[test]
fn credentials_reject_an_audience_secret_matching_no_held_grant() {
    let a = org_a();
    let member = EntityKeypair::generate().entity_id().clone();
    let (cert, dg) = belonging(&a, &member);
    let (grant, _secret) = discover_grant(&org_b(), a.org_id(), cap("nrpc:svc"), 3600);
    // A secret from a DIFFERENT grant (fresh audience material per grant).
    let (_other_grant, other_secret) =
        discover_grant(&org_b(), a.org_id(), cap("nrpc:other"), 3600);

    let err =
        OrgCredentials::new(cert, dg, vec![grant], vec![other_secret]).expect_err("must refuse");
    assert!(
        matches!(err, OrgCredentialError::AudienceSecretMismatch { .. }),
        "got {err:?}"
    );
}

#[test]
fn credentials_reject_duplicate_grant_ids() {
    let a = org_a();
    let member = EntityKeypair::generate().entity_id().clone();
    let (cert, dg) = belonging(&a, &member);
    let (grant, secret) = discover_grant(&org_b(), a.org_id(), cap("nrpc:svc"), 3600);
    let dup = grant.clone();

    let err = OrgCredentials::new(cert, dg, vec![grant, dup], vec![secret]).expect_err("refuse");
    assert!(
        matches!(err, OrgCredentialError::DuplicateGrant { .. }),
        "got {err:?}"
    );
}

#[test]
fn credentials_reject_a_tampered_signature() {
    let a = org_a();
    let member = EntityKeypair::generate().entity_id().clone();
    let (mut cert, dg) = belonging(&a, &member);
    cert.signature[0] ^= 0xFF;

    let err = OrgCredentials::new(cert, dg, vec![], vec![]).expect_err("must refuse");
    assert!(
        matches!(err, OrgCredentialError::SignatureInvalid { .. }),
        "got {err:?}"
    );
}

#[test]
fn credentials_debug_is_redacted() {
    let a = org_a();
    let member = EntityKeypair::generate().entity_id().clone();
    let (cert, dg) = belonging(&a, &member);
    let (grant, secret) = discover_grant(&org_b(), a.org_id(), cap("nrpc:svc"), 3600);
    let creds = OrgCredentials::new(cert, dg, vec![grant], vec![secret]).expect("assembles");

    let rendered = format!("{creds:?}");
    assert!(rendered.contains("audience_secrets: 1"), "{rendered}");
    // Counts, never material.
    assert!(!rendered.contains("discovery_key"), "{rendered}");
}

// ---------------------------------------------------------------------------
// 2. Binding — the complete private-discovery identity relation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn bind_refuses_a_mesh_without_a_durable_identity() {
    let a = org_a();
    let mesh = Mesh::builder("127.0.0.1:0", &[0x53u8; 32])
        .expect("builder")
        .build()
        .await
        .expect("mesh");
    let member = mesh.entity_keypair().entity_id().clone();
    let (cert, dg) = belonging(&a, &member);
    let creds = OrgCredentials::new(cert, dg, vec![], vec![]).expect("assembles");

    let err = mesh.org(creds).expect_err("must refuse");
    assert!(
        matches!(
            err,
            crate::org::OrgSdkError::Credentials(OrgCredentialError::PersistentIdentityRequired)
        ),
        "got {err:?}"
    );
}

#[tokio::test]
async fn bind_refuses_without_an_installed_node_authority() {
    let a = org_a();
    let (mesh, identity, _dir) = mesh_with_authority("no-authority", None).await;
    let (cert, dg) = belonging(&a, identity.entity_id());
    let creds = OrgCredentials::new(cert, dg, vec![], vec![]).expect("assembles");

    let err = mesh.org(creds).expect_err("must refuse");
    assert!(
        matches!(
            err,
            crate::org::OrgSdkError::Credentials(OrgCredentialError::NodeAuthorityRequired)
        ),
        "got {err:?}"
    );
}

#[tokio::test]
async fn bind_refuses_when_the_node_authority_belongs_to_another_org() {
    // Node adopted by B; credentials assert membership in A.
    let (mesh, identity, dir) = mesh_with_authority("org-mismatch", Some(&org_b())).await;
    let (cert, dg) = belonging(&org_a(), identity.entity_id());
    let creds = OrgCredentials::new(cert, dg, vec![], vec![]).expect("assembles");

    let err = mesh.org(creds).expect_err("must refuse");
    assert!(
        matches!(
            err,
            crate::org::OrgSdkError::Credentials(
                OrgCredentialError::NodeAuthorityOrgMismatch { .. }
            )
        ),
        "got {err:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn bind_refuses_a_membership_for_a_different_entity() {
    let a = org_a();
    let (mesh, _identity, dir) = mesh_with_authority("member-mismatch", Some(&a)).await;
    // Membership vouching for someone else entirely.
    let stranger = EntityKeypair::generate().entity_id().clone();
    let (cert, dg) = belonging(&a, &stranger);
    let creds = OrgCredentials::new(cert, dg, vec![], vec![]).expect("assembles");

    let err = mesh.org(creds).expect_err("must refuse");
    assert!(
        matches!(
            err,
            crate::org::OrgSdkError::Credentials(OrgCredentialError::MemberBindingMismatch { .. })
        ),
        "got {err:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn bind_accepts_the_complete_relation_and_leases_the_audience() {
    let a = org_a();
    let (mesh, identity, dir) = mesh_with_authority("bind-ok", Some(&a)).await;
    let (cert, dg) = belonging(&a, identity.entity_id());
    let (grant, secret) = discover_grant(&org_b(), a.org_id(), cap("nrpc:svc"), 3600);
    let grant_id = grant.grant_id;
    let creds = OrgCredentials::new(cert, dg, vec![grant], vec![secret]).expect("assembles");

    let client = mesh.org(creds).expect("binds");
    assert_eq!(client.acting_org(), a.org_id());
    assert_eq!(client.caller(), identity.entity_id());
    client.check_current().expect("credentials are current");

    // The audience is installed on the node — without this, private discovery
    // would have nothing to ingest.
    assert_eq!(mesh.node().consumer_grant_audiences_len_for_test(), 1);
    assert_eq!(
        mesh.node().org_audience_leases().entry_for_test(&grant_id),
        Some((1, true)),
        "one reference, owned by this registry"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn bind_refuses_a_grant_that_is_not_currently_installable() {
    // Validity-contract stage 2: an expired DISCOVER grant ASSEMBLES (stage 1)
    // but must fail the BIND loudly, surfacing the canonical registry refusal —
    // rather than binding a client that silently discovers nothing.
    let a = org_a();
    let (mesh, identity, dir) = mesh_with_authority("bind-expired", Some(&a)).await;
    let (cert, dg) = belonging(&a, identity.entity_id());
    let (grant, secret) = discover_grant(&org_b(), a.org_id(), cap("nrpc:svc"), 1);
    let creds = OrgCredentials::new(cert, dg, vec![grant], vec![secret]).expect("assembles");
    std::thread::sleep(std::time::Duration::from_millis(1100));

    let err = mesh.org(creds).expect_err("must refuse");
    match err {
        crate::org::OrgSdkError::Credentials(OrgCredentialError::AudienceInstallRefused {
            source,
            ..
        }) => assert_eq!(source, GrantAudienceInstallError::GrantNotCurrent),
        other => panic!("got {other:?}"),
    }
    assert_eq!(
        mesh.node().consumer_grant_audiences_len_for_test(),
        0,
        "a failed bind leaves the registry as it found it"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// 3. Lease lifecycle — refcount, ownership safety, concurrency
// ---------------------------------------------------------------------------

/// Two clients bound with the same grant install ONE audience; the first drop
/// removes nothing; the last drop removes it; a later bind re-installs.
#[tokio::test]
async fn lease_refcounts_across_clients_and_removes_only_on_the_last_drop() {
    let a = org_a();
    let (mesh, identity, dir) = mesh_with_authority("lease-refcount", Some(&a)).await;
    let (grant, secret) = discover_grant(&org_b(), a.org_id(), cap("nrpc:svc"), 3600);
    let grant_id = grant.grant_id;

    let make = |g: OrgCapabilityGrant, s: OrgAudienceSecret| {
        let (cert, dg) = belonging(&a, identity.entity_id());
        OrgCredentials::new(cert, dg, vec![g], vec![s]).expect("assembles")
    };

    // Second holder needs its own secret instance; re-issue is not possible
    // (fresh audience per grant), so decode a config-encoded copy — exactly how
    // a second process would load the same 0600 file.
    let encoded = secret.encode_config();
    let secret2 = OrgAudienceSecret::decode_config(&encoded).expect("decode");

    let c1 = mesh.org(make(grant.clone(), secret)).expect("bind 1");
    assert_eq!(mesh.node().consumer_grant_audiences_len_for_test(), 1);
    let c2 = mesh.org(make(grant, secret2)).expect("bind 2");
    assert_eq!(
        mesh.node().consumer_grant_audiences_len_for_test(),
        1,
        "the second bind must NOT install a second record"
    );
    assert_eq!(
        mesh.node().org_audience_leases().entry_for_test(&grant_id),
        Some((2, true))
    );

    // A CLONE shares its client's lease — it does not take a reference.
    let c1_clone = c1.clone();
    assert_eq!(
        mesh.node().org_audience_leases().entry_for_test(&grant_id),
        Some((2, true)),
        "cloning a client shares one guard"
    );
    drop(c1_clone);
    drop(c1);
    assert_eq!(
        mesh.node().consumer_grant_audiences_len_for_test(),
        1,
        "dropping one holder must not withdraw the other's ingest authority"
    );
    assert_eq!(
        mesh.node().org_audience_leases().entry_for_test(&grant_id),
        Some((1, true))
    );

    drop(c2);
    assert_eq!(
        mesh.node().consumer_grant_audiences_len_for_test(),
        0,
        "the last holder's drop withdraws the audience"
    );
    assert_eq!(mesh.node().org_audience_leases().len(), 0);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A record installed OUTSIDE the SDK is not ours to remove: the bind observes
/// `AlreadyPresent`, the entry is non-owning, and the final drop leaves the
/// low-level installation in place.
#[tokio::test]
async fn lease_never_removes_an_installation_it_did_not_perform() {
    let a = org_a();
    let (mesh, identity, dir) = mesh_with_authority("lease-not-ours", Some(&a)).await;
    let (grant, secret) = discover_grant(&org_b(), a.org_id(), cap("nrpc:svc"), 3600);
    let grant_id = grant.grant_id;

    // Low-level operator install first.
    let encoded = secret.encode_config();
    let secret_low = OrgAudienceSecret::decode_config(&encoded).expect("decode");
    mesh.node()
        .install_consumer_grant_audience(grant.clone(), secret_low)
        .expect("low-level install");
    assert_eq!(mesh.node().consumer_grant_audiences_len_for_test(), 1);

    let (cert, dg) = belonging(&a, identity.entity_id());
    let creds = OrgCredentials::new(cert, dg, vec![grant], vec![secret]).expect("assembles");
    let client = mesh.org(creds).expect("binds");
    assert_eq!(
        mesh.node().org_audience_leases().entry_for_test(&grant_id),
        Some((1, false)),
        "already present → the lease is NON-OWNING"
    );

    drop(client);
    assert_eq!(
        mesh.node().consumer_grant_audiences_len_for_test(),
        1,
        "the operator's installation survives the SDK client's drop"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A stale lease removes nothing: if the owned installation is replaced under
/// the same grant id, the token no longer matches and the successor survives.
#[tokio::test]
async fn a_stale_lease_token_cannot_remove_a_successor_installation() {
    let a = org_a();
    let (node, _kp, dir) = adopted_node("stale-token", &a).await;
    let (grant, secret) = discover_grant(&org_b(), a.org_id(), cap("nrpc:svc"), 3600);

    let encoded = secret.encode_config();
    let secret2 = OrgAudienceSecret::decode_config(&encoded).expect("decode");

    let lease = match node
        .install_consumer_grant_audience_leased(grant.clone(), secret)
        .expect("install")
    {
        net::adapter::net::behavior::org_grant_registry::ConsumerAudienceInstall::Installed(l) => l,
        other => panic!("expected Installed, got {other:?}"),
    };

    // Someone replaces the record under the SAME grant id (remove-then-install).
    assert!(node.remove_consumer_grant_audience(&grant.grant_id));
    node.install_consumer_grant_audience(grant, secret2)
        .expect("reinstall");
    assert_eq!(node.consumer_grant_audiences_len_for_test(), 1);

    assert!(
        !node.remove_consumer_grant_audience_if_current(&lease),
        "the stale token owns nothing"
    );
    assert_eq!(
        node.consumer_grant_audiences_len_for_test(),
        1,
        "the successor installation survives"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A last-holder drop racing a fresh bind must leave the new client with its
/// audience installed. The lease mutex spans the 0→1 install and the 1→0
/// removal, so the two cannot interleave into a released-but-referenced state.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_final_release_racing_a_new_bind_leaves_the_audience_installed() {
    let a = org_a();
    let (mesh, identity, dir) = mesh_with_authority("lease-race", Some(&a)).await;
    let mesh = Arc::new(mesh);
    let (grant, secret) = discover_grant(&org_b(), a.org_id(), cap("nrpc:svc"), 3600);
    let encoded = secret.encode_config();

    let creds = |s: OrgAudienceSecret| {
        let (cert, dg) = belonging(&a, identity.entity_id());
        OrgCredentials::new(cert, dg, vec![grant.clone()], vec![s]).expect("assembles")
    };

    for _ in 0..24 {
        let first = mesh
            .org(creds(
                OrgAudienceSecret::decode_config(&encoded).expect("decode"),
            ))
            .expect("bind");
        let dropper = {
            let _ = &mesh;
            std::thread::spawn(move || drop(first))
        };
        let second = mesh
            .org(creds(
                OrgAudienceSecret::decode_config(&encoded).expect("decode"),
            ))
            .expect("concurrent bind");
        dropper.join().expect("join");

        assert_eq!(
            mesh.node().consumer_grant_audiences_len_for_test(),
            1,
            "the surviving client must still hold its audience"
        );
        drop(second);
        assert_eq!(mesh.node().consumer_grant_audiences_len_for_test(), 0);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// 4. Naming + path compatibility (compile witnesses)
// ---------------------------------------------------------------------------

/// `net_sdk::org::OrgError` must still name the canonical ISSUANCE error — the
/// facade's error is the distinct `OrgSdkError`, and an existing public type
/// name was not hijacked. Also pins that the pre-split flat paths resolve.
#[test]
fn canonical_paths_are_unchanged_by_the_module_split() {
    fn assert_issuance_error(_: &crate::org::OrgError) {}
    let e = OrgMembershipCert::try_issue(
        &org_a(),
        EntityKeypair::generate().entity_id().clone(),
        1,
        u64::MAX, // over the TTL ceiling → a canonical issuance error
    )
    .expect_err("ttl ceiling");
    assert_issuance_error(&e);

    // Flat and `types::` paths both resolve to the same items.
    let _: crate::org::OrgId = org_a().org_id();
    let _: crate::org::types::OrgId = org_a().org_id();
    let _: crate::org::types::OrgProofIntent;
}

// ---------------------------------------------------------------------------
// 5. The lease registry's ownership scope (found while starting Workstream N)
// ---------------------------------------------------------------------------

/// Two `Mesh` wrappers over ONE node must share the audience refcount.
///
/// The lease guards the NODE's consumer-audience registry, so its refcount has
/// to be keyed to the node. `Mesh::from_node_arc` is public and the Node/Python
/// bindings hold `Arc<MeshNode>` rather than an SDK `Mesh`, so "one Mesh per
/// node" is not an invariant anyone enforces.
///
/// If the registry is per-`Mesh`, the second wrapper's bind sees the record
/// already present, marks its lease NON-OWNING, and then the first wrapper's
/// drop removes the audience out from under a client that is still live —
/// silently breaking its private discovery.
#[tokio::test]
async fn two_mesh_wrappers_over_one_node_share_the_audience_lease() {
    let a = org_a();
    let (mesh1, identity, dir) = mesh_with_authority("lease-scope", Some(&a)).await;
    let node = mesh1.node().clone();
    // A second wrapper over the SAME node — what a binding does.
    let mesh2 = Mesh::from_node_arc(
        node.clone(),
        std::sync::Arc::new(net::adapter::net::ChannelConfigRegistry::new()),
        Some(identity.clone()),
    );

    let (grant, secret) = discover_grant(&org_b(), a.org_id(), cap("nrpc:svc"), 3600);
    let encoded = secret.encode_config();
    let creds = |s: OrgAudienceSecret| {
        let (cert, dg) = belonging(&a, identity.entity_id());
        OrgCredentials::new(cert, dg, vec![grant.clone()], vec![s]).expect("assembles")
    };

    let c1 = mesh1
        .org(creds(secret))
        .expect("bind through the first wrapper");
    let c2 = mesh2
        .org(creds(
            OrgAudienceSecret::decode_config(&encoded).expect("decode"),
        ))
        .expect("bind through the second wrapper");

    assert_eq!(node.consumer_grant_audiences_len_for_test(), 1);

    // The first client goes away; the second is STILL LIVE and still needs its
    // audience to discover anything.
    drop(c1);
    assert_eq!(
        node.consumer_grant_audiences_len_for_test(),
        1,
        "dropping one wrapper's client must not withdraw a live client's ingest          authority — the refcount belongs to the node, not the wrapper"
    );

    drop(c2);
    assert_eq!(
        node.consumer_grant_audiences_len_for_test(),
        0,
        "the last client's drop withdraws it"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
