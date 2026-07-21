//! OSDK S2 witnesses — `serve_org` registration, the visibility mapping it
//! implies, and the `OrgCaller` projection.
//!
//! Tier note: registration, visibility projection, and emission are exercised
//! against the real node; the admitted-handler path is witnessed structurally
//! here and live end-to-end in S3.

use net::adapter::net::behavior::capability::CapabilitySet;
use net::adapter::net::identity::EntityKeypair;

use super::serve::{OrgAccess, OrgCaller};
use super::tests::{cap, mesh_with_authority, org_a, org_b};
use super::types::*;
use crate::mesh::Mesh;

#[derive(serde::Serialize, serde::Deserialize)]
struct Ping {
    n: u32,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Pong {
    n: u32,
}

/// Whether the provider's own fold carries the exact tag — the possession
/// precheck a protected dispatch requires. Evaluates NO legacy allow-lists.
fn locally_capable(mesh: &Mesh, tag: &str) -> bool {
    net::adapter::net::behavior::fold::capability_bridge::has_local_capability(
        mesh.node().capability_fold(),
        mesh.node().node_id(),
        tag,
    )
}

/// Whether the PLAINTEXT announcement every peer can read carries `tag`.
fn plaintext_has(mesh: &Mesh, tag: &str) -> bool {
    mesh.node()
        .local_announcement_for_test()
        .map(|a| a.capabilities.has_tag(tag))
        .unwrap_or(false)
}

/// Emission is rebuilt asynchronously; poll briefly rather than racing it.
async fn converged(_mesh: &Mesh, mut check: impl FnMut() -> bool) -> bool {
    for _ in 0..100 {
        if check() {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    false
}

/// Register a no-op protected service.
fn serve(mesh: &Mesh, service: &str, access: OrgAccess) -> crate::mesh_rpc::ServeHandle {
    mesh.serve_org(
        service,
        access,
        |_caller: OrgCaller, req: Ping| async move { Ok(Pong { n: req.n }) },
    )
    .expect("serve_org registers")
}

// ---------------------------------------------------------------------------
// Access implies visibility — the whole point of the two-variant enum
// ---------------------------------------------------------------------------

/// `SameOrg` registers OwnerDelegated admission AND owner-scoped ENCRYPTED
/// discovery: the tag must be absent from the plaintext announcement every peer
/// can read, and present only inside a scoped envelope.
///
/// Note the local fold DOES carry the tag — §2.4a requires it, or the
/// provider's own possession precheck could never admit a protected dispatch.
/// Privacy is about what ships, not about what the provider knows.
#[tokio::test]
async fn same_org_is_private_by_default() {
    let a = org_a();
    let (mesh, _identity, dir) = mesh_with_authority("serve-same-org", Some(&a)).await;
    mesh.node()
        .set_owner_cert_emission(true)
        .expect("enable owner-cert emission");

    let _private = serve(&mesh, "internal.reindex", OrgAccess::SameOrg);
    let _public = mesh
        .serve_rpc_typed(
            "open",
            crate::mesh_rpc::Codec::Json,
            |req: Ping| async move { Ok::<_, String>(Pong { n: req.n }) },
        )
        .expect("public serve");
    mesh.node()
        .announce_capabilities(CapabilitySet::new())
        .await
        .expect("announce");

    assert!(
        converged(&mesh, || {
            mesh.node().announcement_scoped_for_send_for_test().len() == 1
                && plaintext_has(&mesh, "nrpc:open")
                && !plaintext_has(&mesh, "nrpc:internal.reindex")
        })
        .await,
        "one owner envelope ships; plaintext keeps the public tag and drops the private one"
    );
    // The provider still possesses the capability locally.
    assert!(locally_capable(&mesh, "nrpc:internal.reindex"));
    let _ = std::fs::remove_dir_all(&dir);
}

/// `Granted` registers CrossOrgGranted admission AND grant-audience ENCRYPTED
/// discovery — the tag never reaches the plaintext plane.
#[tokio::test]
async fn granted_is_private_by_default() {
    let a = org_a();
    let (mesh, _identity, dir) = mesh_with_authority("serve-granted", Some(&a)).await;
    mesh.node()
        .set_owner_cert_emission(true)
        .expect("enable owner-cert emission");

    let _private = serve(&mesh, "customer.read", OrgAccess::Granted);
    let _public = mesh
        .serve_rpc_typed(
            "open",
            crate::mesh_rpc::Codec::Json,
            |req: Ping| async move { Ok::<_, String>(Pong { n: req.n }) },
        )
        .expect("public serve");
    mesh.node()
        .announce_capabilities(CapabilitySet::new())
        .await
        .expect("announce");

    assert!(
        converged(&mesh, || {
            plaintext_has(&mesh, "nrpc:open") && !plaintext_has(&mesh, "nrpc:customer.read")
        })
        .await,
        "a Granted service never appears in the plaintext announcement"
    );
    assert!(locally_capable(&mesh, "nrpc:customer.read"));
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// Registration preconditions and the provisioning contract
// ---------------------------------------------------------------------------

/// A protected registration without an installed node authority is refused
/// loudly — there is no unprotected fallback.
#[tokio::test]
async fn serve_org_requires_an_installed_node_authority() {
    let (mesh, _identity, _dir) = mesh_with_authority("serve-no-authority", None).await;
    let err = mesh
        .serve_org(
            "internal.reindex",
            OrgAccess::SameOrg,
            |_c: OrgCaller, r: Ping| async move { Ok(Pong { n: r.n }) },
        )
        .err()
        .expect("must refuse");
    assert!(
        matches!(
            err,
            crate::mesh_rpc::ServeError::ProtectedAuthorityRequired(_)
        ),
        "got {err:?}"
    );
}

/// The locked provisioning contract: a `Granted` service registers BEFORE any
/// provider audience exists. Admission protection is active immediately; the
/// service is merely undiscoverable until an audience is installed, and
/// installing one then makes it emittable. Failing the registration instead
/// would break valid startup ordering and dynamic grant installation.
#[tokio::test]
async fn granted_registration_precedes_provider_audience_installation() {
    let (a, b) = (org_a(), org_b());
    // This node is owned by B and serves a capability B grants to A.
    let (mesh, _identity, dir) = mesh_with_authority("serve-provisioning", Some(&b)).await;
    mesh.node()
        .set_owner_cert_emission(true)
        .expect("enable owner-cert emission");
    let provider_entity = mesh.node().entity_id().clone();

    // Registration succeeds with ZERO provider audiences installed.
    let _h = serve(&mesh, "customer.read", OrgAccess::Granted);
    assert_eq!(
        mesh.node().provider_grant_audiences_len_for_test(),
        0,
        "no audience yet"
    );
    mesh.node()
        .announce_capabilities(CapabilitySet::new())
        .await
        .expect("announce");
    assert!(
        converged(&mesh, || {
            mesh.node()
                .announcement_scoped_for_send_for_test()
                .is_empty()
                && !plaintext_has(&mesh, "nrpc:customer.read")
        })
        .await,
        "before provisioning: nothing ships, publicly or privately"
    );

    // Now provision: B issues a grant to A covering this provider.
    let (grant, secret) = OrgCapabilityGrant::try_issue(
        &b,
        a.org_id(),
        cap("nrpc:customer.read"),
        GrantRights::INVOKE.union(GrantRights::DISCOVER),
        GrantTargetScope::ExactNode(provider_entity),
        3600,
    )
    .expect("grant");
    mesh.node()
        .install_provider_grant_audience(grant, secret.expect("secret"))
        .expect("provider audience installs after registration");
    assert_eq!(mesh.node().provider_grant_audiences_len_for_test(), 1);

    mesh.node()
        .announce_capabilities(CapabilitySet::new())
        .await
        .expect("re-announce");
    assert!(
        converged(&mesh, || {
            mesh.node().announcement_scoped_for_send_for_test().len() == 1
                && !plaintext_has(&mesh, "nrpc:customer.read")
        })
        .await,
        "after provisioning: exactly one granted envelope ships, still no plaintext"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A second registration of the same service is refused rather than silently
/// replacing the live one (E0.1 non-destructive registration, through the
/// facade).
#[tokio::test]
async fn a_duplicate_registration_is_refused_without_disturbing_the_first() {
    let a = org_a();
    let (mesh, _identity, dir) = mesh_with_authority("serve-duplicate", Some(&a)).await;
    let _first = serve(&mesh, "internal.reindex", OrgAccess::SameOrg);

    let err = mesh
        .serve_org(
            "internal.reindex",
            OrgAccess::SameOrg,
            |_c: OrgCaller, r: Ping| async move { Ok(Pong { n: r.n }) },
        )
        .err()
        .expect("duplicate must be refused");
    assert!(
        matches!(err, crate::mesh_rpc::ServeError::AlreadyServing(_)),
        "got {err:?}"
    );
    assert!(
        locally_capable(&mesh, "nrpc:internal.reindex"),
        "the original registration is untouched"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// OrgCaller — an exact projection, nothing invented
// ---------------------------------------------------------------------------

/// Every field comes from the canonical `Admitted`, unmodified. If `Admitted`
/// ever grows or renames a field, this fails to compile rather than silently
/// drifting.
#[test]
fn org_caller_is_an_exact_projection_of_admitted() {
    let s = EntityKeypair::generate().entity_id().clone();
    let p = EntityKeypair::generate().entity_id().clone();
    let admitted = Admitted {
        caller: s.clone(),
        acting_org: org_a().org_id(),
        provider_org: org_b().org_id(),
        provider: p.clone(),
        capability: cap("nrpc:customer.read"),
    };

    let caller = OrgCaller::from(&admitted);
    assert_eq!(caller.entity, admitted.caller);
    assert_eq!(caller.acting_org, admitted.acting_org);
    assert_eq!(caller.provider_org, admitted.provider_org);
    assert_eq!(caller.provider, admitted.provider);
    assert_eq!(caller.capability, admitted.capability);
    assert!(!caller.is_same_org(), "A acting on a B-owned provider");

    let same = OrgCaller::from(&Admitted {
        acting_org: org_a().org_id(),
        provider_org: org_a().org_id(),
        ..admitted
    });
    assert!(same.is_same_org());
}
