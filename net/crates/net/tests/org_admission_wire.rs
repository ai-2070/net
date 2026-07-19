//! OA2-F4 / §2.6 — grant + admission-wire closure witnesses over the PUBLIC
//! API. Both grant types round-trip through their canonical wire bytes and
//! verify, and a cross-org call proof carrying a DISCOVER grant leaks no raw
//! discovery key into the `net-org-admission` header value. Pairs with the
//! lib-unit byte-scan in `behavior::org_call` and the installed-secret witness
//! in `behavior::org_grant`.

#![cfg(feature = "net")]

use net::adapter::net::behavior::org::{OrgKeypair, OrgMembershipCert};
use net::adapter::net::behavior::org_call::OrgCallProof;
use net::adapter::net::behavior::org_grant::{
    audience_key_commitment, CapabilityAuthorityId, DispatcherScope, GrantRights, GrantTargetScope,
    OrgCapabilityGrant, OrgDispatcherGrant,
};
use net::adapter::net::identity::{EntityId, EntityKeypair};

fn org_a() -> OrgKeypair {
    OrgKeypair::from_bytes([0x77u8; 32])
}
fn org_b() -> OrgKeypair {
    OrgKeypair::from_bytes([0x42u8; 32])
}
fn caller() -> EntityKeypair {
    EntityKeypair::from_bytes([0x24u8; 32])
}
fn provider() -> EntityId {
    EntityId::from_bytes([0x99u8; 32])
}
fn cap() -> CapabilityAuthorityId {
    CapabilityAuthorityId::for_tag("nrpc:oa2-echo")
}

#[test]
fn grants_round_trip_through_wire_bytes_and_verify() {
    // A -> S dispatcher grant.
    let dispatcher = OrgDispatcherGrant::try_issue(
        &org_a(),
        caller().entity_id().clone(),
        DispatcherScope::Exact(cap()),
        3600,
    )
    .expect("dispatcher");
    let decoded =
        OrgDispatcherGrant::from_bytes(&dispatcher.to_bytes()).expect("decode dispatcher");
    assert_eq!(decoded, dispatcher);
    decoded.verify().expect("dispatcher verifies");

    // B -> A capability grant WITH discover.
    let (grant, secret) = OrgCapabilityGrant::try_issue(
        &org_b(),
        org_a().org_id(),
        cap(),
        GrantRights::INVOKE.union(GrantRights::DISCOVER),
        GrantTargetScope::ExactNode(provider()),
        3600,
    )
    .expect("cap grant");
    assert!(secret.is_some(), "DISCOVER mints an audience secret");
    let decoded = OrgCapabilityGrant::from_bytes(&grant.to_bytes()).expect("decode cap grant");
    assert_eq!(decoded, grant);
    decoded.verify().expect("cap grant verifies");
}

#[test]
fn encoded_admission_header_carries_no_discovery_key() {
    let membership = OrgMembershipCert::try_issue(&org_a(), caller().entity_id().clone(), 1, 3600)
        .expect("cert");
    let dispatcher = OrgDispatcherGrant::try_issue(
        &org_a(),
        caller().entity_id().clone(),
        DispatcherScope::Exact(cap()),
        3600,
    )
    .expect("dispatcher");
    let (grant, secret) = OrgCapabilityGrant::try_issue(
        &org_b(),
        org_a().org_id(),
        cap(),
        GrantRights::INVOKE.union(GrantRights::DISCOVER),
        GrantTargetScope::ExactNode(provider()),
        3600,
    )
    .expect("cap grant");
    let secret = secret.expect("discover secret");
    let discovery_key = *secret.discovery_key();

    // Any far-future ns expiry — encoding does not verify freshness.
    let expiry = 4_000_000_000_000_000_000u64;
    let proof = OrgCallProof::sign_for_call(
        &caller(),
        membership,
        dispatcher,
        Some(grant),
        org_a().org_id(),
        org_b().org_id(),
        provider(),
        7,
        cap(),
        expiry,
        [0x11u8; 32],
    );
    // The encoded proof IS the `net-org-admission` header value on the wire.
    let header_value = proof.encode().expect("encode proof");

    assert!(
        !header_value.windows(32).any(|w| w == discovery_key),
        "raw discovery key leaked into the net-org-admission header value",
    );
    let commitment = audience_key_commitment(&discovery_key);
    assert!(
        header_value.windows(32).any(|w| w == commitment),
        "the safe commitment rides in the header value",
    );
}
