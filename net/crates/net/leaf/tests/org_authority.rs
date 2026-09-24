//! Cross-implementation conformance witnesses for the ported org
//! proof/admission authority (`net_leaf::org`).
//!
//! Three obligations, in order of how expensive they are to discover
//! in production:
//!
//! 1. **Golden vectors** — the spec's six pinned hex vectors must
//!    reproduce byte-for-byte through the leaf's ISSUE and codec
//!    paths. These are the cross-implementation conformance bar
//!    against the core's `behavior/{org,org_grant}` tests: if these
//!    pass, a credential minted here verifies there and vice versa.
//! 2. **The admission denial matrix** — each refusal surfaces the
//!    EXACT `AdmissionDenied` variant the core engine produces, at
//!    the exact ordered step that owns it.
//! 3. **The decode discipline** — the streaming decoder is strict
//!    (truncation, trailing bytes and unknown kinds all refuse); the
//!    unary decoder is prefix-tolerant exactly as core's frozen one.

use std::collections::BTreeMap;

use bytes::Bytes;

use net_leaf::identity::{hex_lower, unhex, EntityKeypair};
use net_leaf::org::admission::{
    verify_org_admission, AdmissionContext, AdmissionDenied, CoarseAdmissionReason, OrgAdmission,
};
use net_leaf::org::cert::{OrgError, OrgId, OrgKeypair, OrgMembershipCert, OrgRevocationBundle};
use net_leaf::org::digest::org_request_digest;
use net_leaf::org::entity::EntityId;
use net_leaf::org::grant::{
    audience_key_commitment, CapabilityAuthorityId, DispatcherScope, GrantRights, GrantTargetScope,
    GrantedDiscoveryBinding, OrgAudienceSecret, OrgCapabilityGrant, OrgDispatcherGrant,
};
use net_leaf::org::proof::{
    OrgCallProof, OrgStreamCallProof, RpcCallShape, ORG_ADMISSION_HEADER,
    STREAM_CALL_KIND_CLIENT_STREAMING, STREAM_CALL_KIND_SERVER_STREAMING,
};
use net_leaf::org::replay::AdmissionReplayGuard;
use net_leaf::org::revocation::RevocationFacts;
use net_leaf::rpc_wire::RpcRequestPayload;

// ---------------------------------------------------------------------------
// The spec's six golden vectors (ORG_SCOPED_STREAMING_PLAN §4.5 conformance)
// ---------------------------------------------------------------------------

const GOLDEN_CERT_HEX: &str = "2152f8d19b791d24453242e15f2eab6cb7cffa7b6a5ed30097960e069881db12242424242424242424242424242424242424242424242424242424242424242400f15365000000008024356700000000050000008877665544332211b94f91f5cac0026eb101b68e5eed16d9e3f8d516d9b81add1de32ccf7508fe40f597be8370ba6ee871154348a5d2ea86335714277a60e8146de3b5576acd300c";

const GOLDEN_BUNDLE_HEX: &str = "2152f8d19b791d24453242e15f2eab6cb7cffa7b6a5ed30097960e069881db1200f153650000000003000000010101010101010101010101010101010101010101010101010101010101010103000000020202020202020202020202020202020202020202020202020202020202020207000000030303030303030303030303030303030303030303030303030303030303030301000000e9da4985e5d915871c1713e5c6ec815bf97c5c69ac943595f51c9c9faf7d26231220f9aaa5b5f42db19b0cf60a05502549c486a08b7c2495d3f38e7a453b0803";

const GOLDEN_DISPATCHER_GRANT_HEX: &str = "c853ad0f0cd2b619aea92ceec4fd56a24d6499d584ce79257e45cfd8139b60a7242424242424242424242424242424242424242424242424242424242424242401b7cf23907dfe3cad1152c9c5e14bec0bbdd0beeaafaff54ee27d5e5974788bab00f153650000000010ff5365000000008877665544332211ce5ee45cb913ab81f08b3013b3f5d5910558cbdb51febea077bd981f32956bef01ee5fbef4474599bf1e9112fda161ba2ce80660a2e8937878b8db144755d909";

const GOLDEN_CAPABILITY_GRANT_HEX: &str = "11111111111111111111111111111111111111111111111111111111111111112152f8d19b791d24453242e15f2eab6cb7cffa7b6a5ed30097960e069881db12c853ad0f0cd2b619aea92ceec4fd56a24d6499d584ce79257e45cfd8139b60a7b7cf23907dfe3cad1152c9c5e14bec0bbdd0beeaafaff54ee27d5e5974788bab03000000022152f8d19b791d24453242e15f2eab6cb7cffa7b6a5ed30097960e069881db1201abababababababababababababababababababababababababababababababab3338c46839907f71578cf4730fcf8eb0ec586ef8b496b453390fa38e38c33aa700f153650000000010ff53650000000011223344556677889afda595f45d1b61831dcec72c599da2aa168c2a5343c5872d3e650ab5bb70076d5e163081340d6f50eb0407278176a2685afc31e5bb3adc4f93f4e46ff80806";

const GOLDEN_CAPABILITY_ID_HEX: &str =
    "b7cf23907dfe3cad1152c9c5e14bec0bbdd0beeaafaff54ee27d5e5974788bab";

const GOLDEN_COMMITMENT_HEX: &str =
    "3338c46839907f71578cf4730fcf8eb0ec586ef8b496b453390fa38e38c33aa7";

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

const NOW_SECS: u64 = 1_700_000_000;
const NOW_NS: u64 = NOW_SECS * 1_000_000_000;
const NOW_MONO_MS: u64 = 500_000;
const SESSION_BINDING: [u8; 32] = [0x5C; 32];

/// Org B — the provider's owner org.
fn owner() -> OrgKeypair {
    OrgKeypair::from_bytes([0x42; 32])
}

/// Org A — the grantee org.
fn grantee() -> OrgKeypair {
    OrgKeypair::from_bytes([0x77; 32])
}

/// The calling entity S.
fn caller() -> EntityKeypair {
    EntityKeypair::from_secret([0x24; 32])
}

/// The exact provider P.
fn provider() -> EntityKeypair {
    EntityKeypair::from_secret([0x99; 32])
}

fn caller_id() -> EntityId {
    EntityId::from_bytes(*caller().entity_id())
}

fn provider_id() -> EntityId {
    EntityId::from_bytes(*provider().entity_id())
}

/// The invoked capability C.
fn cap() -> CapabilityAuthorityId {
    CapabilityAuthorityId::for_tag("nrpc:oa2-echo")
}

/// A finalized request carrying an admission header (which the
/// digest must strip) and a duplicated application header (whose
/// ORDER the digest must bind).
fn request() -> RpcRequestPayload {
    RpcRequestPayload {
        service: "nrpc:oa2-echo".to_string(),
        deadline_ns: NOW_NS + 5_000_000_000,
        flags: 0,
        headers: vec![
            ("x-trace".to_string(), b"t1".to_vec()),
            (ORG_ADMISSION_HEADER.to_string(), b"proof-header".to_vec()),
            ("x-trace".to_string(), b"t2".to_vec()),
        ],
        body: Bytes::from_static(b"hello"),
    }
}

/// The same request with one body byte flipped.
fn tampered_request() -> RpcRequestPayload {
    let mut req = request();
    req.body = Bytes::from_static(b"hellp");
    req
}

/// Same-org credentials: membership + dispatcher grant from org B,
/// minted through the ported ISSUE functions with caller-supplied
/// time and nonce.
fn same_org_creds() -> (OrgMembershipCert, OrgDispatcherGrant) {
    let owner = owner();
    let cid = caller_id();
    let membership =
        OrgMembershipCert::try_issue(&owner, cid.clone(), 3, 3600, NOW_SECS, 0x11).unwrap();
    let dispatcher = OrgDispatcherGrant::try_issue(
        &owner,
        cid,
        DispatcherScope::Exact(cap()),
        3600,
        NOW_SECS,
        0x12,
    )
    .unwrap();
    (membership, dispatcher)
}

/// Cross-org credentials: membership + dispatcher grant from org A.
fn cross_org_creds() -> (OrgMembershipCert, OrgDispatcherGrant) {
    let grantee = grantee();
    let cid = caller_id();
    let membership =
        OrgMembershipCert::try_issue(&grantee, cid.clone(), 1, 3600, NOW_SECS, 0x21).unwrap();
    let dispatcher = OrgDispatcherGrant::try_issue(
        &grantee,
        cid,
        DispatcherScope::Exact(cap()),
        3600,
        NOW_SECS,
        0x22,
    )
    .unwrap();
    (membership, dispatcher)
}

/// A capability grant B → A through the ported ISSUE function
/// (INVOKE only, no audience material).
fn capability_grant(
    capability: CapabilityAuthorityId,
    target: GrantTargetScope,
) -> OrgCapabilityGrant {
    let owner = owner();
    OrgCapabilityGrant::try_issue(
        &owner,
        grantee().org_id(),
        capability,
        GrantRights::INVOKE,
        target,
        3600,
        [0x11; 32],
        None,
        NOW_SECS,
        0x13,
    )
    .unwrap()
    .0
}

/// A same-org unary proof over `digest`.
fn same_org_proof(digest: [u8; 32], call_id: u64, expires_ns: u64) -> OrgCallProof {
    let (membership, dispatcher) = same_org_creds();
    OrgCallProof::sign_for_call(
        &caller(),
        membership,
        dispatcher,
        None,
        owner().org_id(),
        owner().org_id(),
        provider_id(),
        call_id,
        cap(),
        expires_ns,
        digest,
    )
}

/// A cross-org unary proof carrying `grant`.
fn cross_org_proof(
    grant: OrgCapabilityGrant,
    digest: [u8; 32],
    call_id: u64,
    capability: CapabilityAuthorityId,
) -> OrgCallProof {
    let (membership, dispatcher) = cross_org_creds();
    OrgCallProof::sign_for_call(
        &caller(),
        membership,
        dispatcher,
        Some(grant),
        grantee().org_id(),
        owner().org_id(),
        provider_id(),
        call_id,
        capability,
        NOW_NS + 10_000_000_000,
        digest,
    )
}

/// A same-org streaming proof with an explicit `kind`.
fn stream_proof(kind: u8, digest: [u8; 32], call_id: u64) -> OrgStreamCallProof {
    let (membership, dispatcher) = same_org_creds();
    OrgStreamCallProof::sign_for_stream_call(
        &caller(),
        membership,
        dispatcher,
        None,
        owner().org_id(),
        owner().org_id(),
        provider_id(),
        call_id,
        cap(),
        NOW_NS + 10_000_000_000,
        digest,
        kind,
        SESSION_BINDING,
    )
}

/// A unary-shaped context (registered shape `Unary`).
#[allow(clippy::too_many_arguments)]
fn unary_ctx<'a>(
    mode: OrgAdmission,
    caller: &'a EntityId,
    provider: &'a EntityId,
    owner_org: OrgId,
    call_id: u64,
    digest: [u8; 32],
    facts: &'a RevocationFacts,
) -> AdmissionContext<'a> {
    AdmissionContext::new(
        mode,
        caller,
        provider,
        owner_org,
        cap(),
        call_id,
        digest,
        RpcCallShape::Unary,
        false,
        false,
        None,
        facts,
        0,
    )
}

/// A server-streaming-shaped context (registered shape
/// `ServerStreaming`) binding the given receiving session.
#[allow(clippy::too_many_arguments)]
fn stream_ctx<'a>(
    caller: &'a EntityId,
    provider: &'a EntityId,
    call_id: u64,
    digest: [u8; 32],
    session_binding: Option<[u8; 32]>,
    facts: &'a RevocationFacts,
) -> AdmissionContext<'a> {
    AdmissionContext::new(
        OrgAdmission::OwnerDelegated,
        caller,
        provider,
        owner().org_id(),
        cap(),
        call_id,
        digest,
        RpcCallShape::ServerStreaming,
        false,
        true,
        session_binding,
        facts,
        0,
    )
}

// ---------------------------------------------------------------------------
// (a) Golden vectors — byte identity against the core's pins
// ---------------------------------------------------------------------------

#[test]
fn golden_membership_cert_wire_bytes_match() {
    let org = owner();
    let cert = OrgMembershipCert::issue_at(
        &org,
        EntityId::from_bytes([0x24; 32]),
        5,
        1_700_000_000,
        1_731_536_000,
        0x1122_3344_5566_7788,
    );
    assert_eq!(hex_lower(&cert.to_bytes()), GOLDEN_CERT_HEX);
    let decoded = OrgMembershipCert::from_bytes(&unhex(GOLDEN_CERT_HEX).unwrap()).unwrap();
    assert_eq!(decoded, cert);
    assert_eq!(decoded.to_bytes(), cert.to_bytes());
    decoded.verify().unwrap();
}

#[test]
fn golden_revocation_bundle_wire_bytes_match() {
    let org = owner();
    let mut floors = BTreeMap::new();
    floors.insert(EntityId::from_bytes([1u8; 32]), 3u32);
    floors.insert(EntityId::from_bytes([2u8; 32]), 7u32);
    floors.insert(EntityId::from_bytes([3u8; 32]), 1u32);
    let bundle = OrgRevocationBundle::issue_at(&org, &floors, 1_700_000_000).unwrap();
    assert_eq!(hex_lower(&bundle.to_bytes()), GOLDEN_BUNDLE_HEX);
    let decoded = OrgRevocationBundle::from_bytes(&unhex(GOLDEN_BUNDLE_HEX).unwrap()).unwrap();
    assert_eq!(decoded, bundle);
    assert_eq!(decoded.to_bytes(), bundle.to_bytes());
    decoded.verify().unwrap();
    assert_eq!(decoded.floors().len(), 3);
}

#[test]
fn golden_capability_authority_id_matches() {
    let id = CapabilityAuthorityId::for_tag("nrpc:oa2-echo");
    assert_eq!(hex_lower(id.as_bytes()), GOLDEN_CAPABILITY_ID_HEX);
    assert_eq!(
        CapabilityAuthorityId::from_bytes(
            unhex(GOLDEN_CAPABILITY_ID_HEX).unwrap().try_into().unwrap()
        ),
        id
    );
}

#[test]
fn golden_audience_key_commitment_matches() {
    let commitment = audience_key_commitment(&[0xCD; 32]);
    assert_eq!(hex_lower(&commitment), GOLDEN_COMMITMENT_HEX);
}

#[test]
fn golden_dispatcher_grant_wire_bytes_match() {
    let org_a = grantee();
    let grant = OrgDispatcherGrant::issue_at(
        &org_a,
        EntityId::from_bytes([0x24; 32]),
        DispatcherScope::Exact(CapabilityAuthorityId::for_tag("nrpc:oa2-echo")),
        1_700_000_000,
        1_700_003_600,
        0x1122_3344_5566_7788,
    );
    assert_eq!(hex_lower(&grant.to_bytes()), GOLDEN_DISPATCHER_GRANT_HEX);
    let decoded =
        OrgDispatcherGrant::from_bytes(&unhex(GOLDEN_DISPATCHER_GRANT_HEX).unwrap()).unwrap();
    assert_eq!(decoded, grant);
    assert_eq!(decoded.to_bytes(), grant.to_bytes());
    decoded.verify().unwrap();
}

#[test]
fn golden_capability_grant_wire_bytes_match() {
    let org_b = owner();
    let org_a = grantee();
    let binding = GrantedDiscoveryBinding {
        audience_handle: [0xAB; 32],
        key_commitment: audience_key_commitment(&[0xCD; 32]),
    };
    let grant = OrgCapabilityGrant::issue_at(
        &org_b,
        [0x11; 32],
        org_a.org_id(),
        CapabilityAuthorityId::for_tag("nrpc:oa2-echo"),
        GrantRights::DISCOVER.union(GrantRights::INVOKE),
        GrantTargetScope::AnyNodeOwnedBy(org_b.org_id()),
        Some(binding),
        1_700_000_000,
        1_700_003_600,
        0x8877_6655_4433_2211,
    );
    assert_eq!(hex_lower(&grant.to_bytes()), GOLDEN_CAPABILITY_GRANT_HEX);
    let decoded =
        OrgCapabilityGrant::from_bytes(&unhex(GOLDEN_CAPABILITY_GRANT_HEX).unwrap()).unwrap();
    assert_eq!(decoded, grant);
    assert_eq!(decoded.to_bytes(), grant.to_bytes());
    decoded.verify().unwrap();
    // The structural rule and the commitment both survive the codec.
    assert!(decoded.permits_invoke());
    assert!(decoded.permits_discover());
    let secret_material = OrgAudienceSecret::mint([0x11; 32], {
        let mut rng = [0u8; 64];
        rng[..32].copy_from_slice(&[0xAB; 32]);
        rng[32..].copy_from_slice(&[0xCD; 32]);
        rng
    });
    assert!(secret_material.0.matches_grant(&decoded));
}

// ---------------------------------------------------------------------------
// (b) The admission engine: valid paths and the typed refusal matrix
// ---------------------------------------------------------------------------

#[test]
fn owner_delegated_admission_admits_a_valid_same_org_call() {
    let digest = org_request_digest(&request()).unwrap();
    let caller_id = caller_id();
    let provider_id = provider_id();
    let facts = RevocationFacts::default();
    let guard = AdmissionReplayGuard::with_defaults();
    let proof = same_org_proof(digest, 7, NOW_NS + 10_000_000_000);
    let header = proof.encode().unwrap();
    let ctx = unary_ctx(
        OrgAdmission::OwnerDelegated,
        &caller_id,
        &provider_id,
        owner().org_id(),
        7,
        digest,
        &facts,
    );
    let admitted = verify_org_admission(
        &ctx,
        &[&header],
        &guard,
        NOW_NS,
        NOW_MONO_MS,
        || true,
        |_| true,
    )
    .unwrap();
    assert_eq!(admitted.caller, caller_id);
    assert_eq!(admitted.acting_org, owner().org_id());
    assert_eq!(admitted.provider_org, owner().org_id());
    assert_eq!(admitted.provider, provider_id);
    assert_eq!(admitted.capability, cap());
}

#[test]
fn cross_org_granted_admission_admits_a_valid_granted_call() {
    let digest = org_request_digest(&request()).unwrap();
    let caller_id = caller_id();
    let provider_id = provider_id();
    let facts = RevocationFacts::default();
    let guard = AdmissionReplayGuard::with_defaults();
    let grant = capability_grant(cap(), GrantTargetScope::AnyNodeOwnedBy(owner().org_id()));
    let proof = cross_org_proof(grant, digest, 7, cap());
    let header = proof.encode().unwrap();
    let ctx = unary_ctx(
        OrgAdmission::CrossOrgGranted,
        &caller_id,
        &provider_id,
        owner().org_id(),
        7,
        digest,
        &facts,
    );
    let admitted = verify_org_admission(
        &ctx,
        &[&header],
        &guard,
        NOW_NS,
        NOW_MONO_MS,
        || true,
        |_| true,
    )
    .unwrap();
    assert_eq!(admitted.acting_org, grantee().org_id());
    assert_eq!(admitted.provider_org, owner().org_id());
}

#[test]
fn expired_proof_is_refused_as_proof_expired() {
    let digest = org_request_digest(&request()).unwrap();
    let caller_id = caller_id();
    let provider_id = provider_id();
    let facts = RevocationFacts::default();
    let guard = AdmissionReplayGuard::with_defaults();
    // Signed to expire one second before the sample.
    let proof = same_org_proof(digest, 7, NOW_NS - 1_000_000_000);
    let header = proof.encode().unwrap();
    let ctx = unary_ctx(
        OrgAdmission::OwnerDelegated,
        &caller_id,
        &provider_id,
        owner().org_id(),
        7,
        digest,
        &facts,
    );
    let err = verify_org_admission(
        &ctx,
        &[&header],
        &guard,
        NOW_NS,
        NOW_MONO_MS,
        || true,
        |_| true,
    )
    .unwrap_err();
    assert_eq!(err, AdmissionDenied::ProofExpired);
}

#[test]
fn proof_ttl_beyond_the_ceiling_is_refused_as_proof_expired() {
    let digest = org_request_digest(&request()).unwrap();
    let caller_id = caller_id();
    let provider_id = provider_id();
    let facts = RevocationFacts::default();
    let guard = AdmissionReplayGuard::with_defaults();
    // 31 s ahead: not expired, but past `MAX_ORG_PROOF_TTL_SECS` (30).
    let proof = same_org_proof(digest, 7, NOW_NS + 31_000_000_000);
    let header = proof.encode().unwrap();
    let ctx = unary_ctx(
        OrgAdmission::OwnerDelegated,
        &caller_id,
        &provider_id,
        owner().org_id(),
        7,
        digest,
        &facts,
    );
    let err = verify_org_admission(
        &ctx,
        &[&header],
        &guard,
        NOW_NS,
        NOW_MONO_MS,
        || true,
        |_| true,
    )
    .unwrap_err();
    // The TTL ceiling is a merits refusal, surfaced under the same
    // typed reason as expiry (core's step 8 mapping).
    assert_eq!(err, AdmissionDenied::ProofExpired);
}

#[test]
fn grant_capability_mismatch_is_refused_as_capability_mismatch() {
    let digest = org_request_digest(&request()).unwrap();
    let caller_id = caller_id();
    let provider_id = provider_id();
    let facts = RevocationFacts::default();
    let guard = AdmissionReplayGuard::with_defaults();
    // The grant names a DIFFERENT capability than the one invoked.
    let grant = capability_grant(
        CapabilityAuthorityId::for_tag("nrpc:other-service"),
        GrantTargetScope::AnyNodeOwnedBy(owner().org_id()),
    );
    let proof = cross_org_proof(grant, digest, 7, cap());
    let header = proof.encode().unwrap();
    let ctx = unary_ctx(
        OrgAdmission::CrossOrgGranted,
        &caller_id,
        &provider_id,
        owner().org_id(),
        7,
        digest,
        &facts,
    );
    let err = verify_org_admission(
        &ctx,
        &[&header],
        &guard,
        NOW_NS,
        NOW_MONO_MS,
        || true,
        |_| true,
    )
    .unwrap_err();
    assert_eq!(err, AdmissionDenied::CapabilityMismatch);
}

#[test]
fn grant_target_not_covering_the_provider_is_refused_as_target_not_covered() {
    let digest = org_request_digest(&request()).unwrap();
    let caller_id = caller_id();
    let provider_id = provider_id();
    let facts = RevocationFacts::default();
    let guard = AdmissionReplayGuard::with_defaults();
    // A grant covering a DIFFERENT provider node — "wrong provider".
    let other_provider = EntityId::from_bytes(*EntityKeypair::from_secret([0x55; 32]).entity_id());
    let grant = capability_grant(cap(), GrantTargetScope::ExactNode(other_provider));
    let proof = cross_org_proof(grant, digest, 7, cap());
    let header = proof.encode().unwrap();
    let ctx = unary_ctx(
        OrgAdmission::CrossOrgGranted,
        &caller_id,
        &provider_id,
        owner().org_id(),
        7,
        digest,
        &facts,
    );
    let err = verify_org_admission(
        &ctx,
        &[&header],
        &guard,
        NOW_NS,
        NOW_MONO_MS,
        || true,
        |_| true,
    )
    .unwrap_err();
    assert_eq!(err, AdmissionDenied::TargetNotCovered);
}

#[test]
fn grant_from_a_foreign_org_is_refused_as_foreign_issuer() {
    let digest = org_request_digest(&request()).unwrap();
    let caller_id = caller_id();
    let provider_id = provider_id();
    let facts = RevocationFacts::default();
    let guard = AdmissionReplayGuard::with_defaults();
    // Self-signed by org A: authority ONLY if the provider's owner
    // issued it.
    let grant = OrgCapabilityGrant::try_issue(
        &grantee(),
        grantee().org_id(),
        cap(),
        GrantRights::INVOKE,
        GrantTargetScope::AnyNodeOwnedBy(grantee().org_id()),
        3600,
        [0x33; 32],
        None,
        NOW_SECS,
        0x33,
    )
    .unwrap()
    .0;
    let proof = cross_org_proof(grant, digest, 7, cap());
    let header = proof.encode().unwrap();
    let ctx = unary_ctx(
        OrgAdmission::CrossOrgGranted,
        &caller_id,
        &provider_id,
        owner().org_id(),
        7,
        digest,
        &facts,
    );
    let err = verify_org_admission(
        &ctx,
        &[&header],
        &guard,
        NOW_NS,
        NOW_MONO_MS,
        || true,
        |_| true,
    )
    .unwrap_err();
    assert_eq!(err, AdmissionDenied::ForeignIssuer);
}

#[test]
fn tampered_request_body_is_refused_as_binding_invalid() {
    let digest = org_request_digest(&request()).unwrap();
    let tampered_digest = org_request_digest(&tampered_request()).unwrap();
    assert_ne!(digest, tampered_digest);
    let caller_id = caller_id();
    let provider_id = provider_id();
    let facts = RevocationFacts::default();
    let guard = AdmissionReplayGuard::with_defaults();
    // The proof signs the ORIGINAL request; the provider hashes the
    // tampered body — one flipped byte must fail the binding.
    let proof = same_org_proof(digest, 7, NOW_NS + 10_000_000_000);
    let header = proof.encode().unwrap();
    let ctx = unary_ctx(
        OrgAdmission::OwnerDelegated,
        &caller_id,
        &provider_id,
        owner().org_id(),
        7,
        tampered_digest,
        &facts,
    );
    let err = verify_org_admission(
        &ctx,
        &[&header],
        &guard,
        NOW_NS,
        NOW_MONO_MS,
        || true,
        |_| true,
    )
    .unwrap_err();
    assert_eq!(err, AdmissionDenied::BindingInvalid);
}

#[test]
fn stream_session_binding_mismatch_is_refused() {
    let digest = org_request_digest(&request()).unwrap();
    let caller_id = caller_id();
    let provider_id = provider_id();
    let facts = RevocationFacts::default();
    let guard = AdmissionReplayGuard::with_defaults();
    let proof = stream_proof(STREAM_CALL_KIND_SERVER_STREAMING, digest, 7);
    let header = proof.encode().unwrap();
    // The opening rides SESSION_BINDING; the RECEIVING session is a
    // different one.
    let ctx = stream_ctx(
        &caller_id,
        &provider_id,
        7,
        digest,
        Some([0xEE; 32]),
        &facts,
    );
    let err = verify_org_admission(
        &ctx,
        &[&header],
        &guard,
        NOW_NS,
        NOW_MONO_MS,
        || true,
        |_| true,
    )
    .unwrap_err();
    assert_eq!(err, AdmissionDenied::SessionBindingMismatch);
    // …and a hand-built session (`None`) can never admit a stream.
    let ctx = stream_ctx(&caller_id, &provider_id, 7, digest, None, &facts);
    let err = verify_org_admission(
        &ctx,
        &[&header],
        &guard,
        NOW_NS,
        NOW_MONO_MS,
        || true,
        |_| true,
    )
    .unwrap_err();
    assert_eq!(err, AdmissionDenied::SessionBindingMismatch);
}

#[test]
fn stream_kind_mismatch_is_refused_as_shape_mismatch() {
    let digest = org_request_digest(&request()).unwrap();
    let caller_id = caller_id();
    let provider_id = provider_id();
    let facts = RevocationFacts::default();
    let guard = AdmissionReplayGuard::with_defaults();
    // A kind-2 (client-streaming) proof presented on a kind-1
    // (server-streaming) registration.
    let proof = stream_proof(STREAM_CALL_KIND_CLIENT_STREAMING, digest, 7);
    let header = proof.encode().unwrap();
    let ctx = stream_ctx(
        &caller_id,
        &provider_id,
        7,
        digest,
        Some(SESSION_BINDING),
        &facts,
    );
    let err = verify_org_admission(
        &ctx,
        &[&header],
        &guard,
        NOW_NS,
        NOW_MONO_MS,
        || true,
        |_| true,
    )
    .unwrap_err();
    assert_eq!(err, AdmissionDenied::ShapeMismatch);
}

#[test]
fn replayed_call_id_is_refused_as_replay() {
    let digest = org_request_digest(&request()).unwrap();
    let caller_id = caller_id();
    let provider_id = provider_id();
    let facts = RevocationFacts::default();
    let guard = AdmissionReplayGuard::with_defaults();
    let proof = same_org_proof(digest, 7, NOW_NS + 10_000_000_000);
    let header = proof.encode().unwrap();
    let ctx = unary_ctx(
        OrgAdmission::OwnerDelegated,
        &caller_id,
        &provider_id,
        owner().org_id(),
        7,
        digest,
        &facts,
    );
    verify_org_admission(
        &ctx,
        &[&header],
        &guard,
        NOW_NS,
        NOW_MONO_MS,
        || true,
        |_| true,
    )
    .unwrap();
    // The SAME proof re-presented within its retention window.
    let err = verify_org_admission(
        &ctx,
        &[&header],
        &guard,
        NOW_NS,
        NOW_MONO_MS,
        || true,
        |_| true,
    )
    .unwrap_err();
    assert_eq!(err, AdmissionDenied::Replay);
}

#[test]
fn live_call_id_reuse_with_a_new_binding_is_refused_as_call_id_collision() {
    let digest = org_request_digest(&request()).unwrap();
    let tampered_digest = org_request_digest(&tampered_request()).unwrap();
    let caller_id = caller_id();
    let provider_id = provider_id();
    let facts = RevocationFacts::default();
    let guard = AdmissionReplayGuard::with_defaults();
    let proof = same_org_proof(digest, 7, NOW_NS + 10_000_000_000);
    let header = proof.encode().unwrap();
    let ctx = unary_ctx(
        OrgAdmission::OwnerDelegated,
        &caller_id,
        &provider_id,
        owner().org_id(),
        7,
        digest,
        &facts,
    );
    verify_org_admission(
        &ctx,
        &[&header],
        &guard,
        NOW_NS,
        NOW_MONO_MS,
        || true,
        |_| true,
    )
    .unwrap();
    // A DIFFERENT binding under the SAME live call_id — a caller bug
    // or forged reuse, distinguishable from a replay.
    let proof2 = same_org_proof(tampered_digest, 7, NOW_NS + 10_000_000_000);
    let header2 = proof2.encode().unwrap();
    let ctx2 = unary_ctx(
        OrgAdmission::OwnerDelegated,
        &caller_id,
        &provider_id,
        owner().org_id(),
        7,
        tampered_digest,
        &facts,
    );
    let err = verify_org_admission(
        &ctx2,
        &[&header2],
        &guard,
        NOW_NS,
        NOW_MONO_MS,
        || true,
        |_| true,
    )
    .unwrap_err();
    assert_eq!(err, AdmissionDenied::CallIdCollision);
}

#[test]
fn revocation_floor_above_the_generation_is_refused_as_membership_revoked() {
    let digest = org_request_digest(&request()).unwrap();
    let caller_id = caller_id();
    let provider_id = provider_id();
    let guard = AdmissionReplayGuard::with_defaults();
    // The same-org fixture cert is at generation 3; the fed floor
    // for (org B, caller) is 5 — the cert is revoked. (A cert AT
    // the floor stays alive, which the valid-path tests pin.)
    let mut facts = RevocationFacts::default();
    assert_eq!(
        facts.merge_floors(owner().org_id(), &[(caller_id.clone(), 5)]),
        1
    );
    assert_eq!(facts.floor_for(&owner().org_id(), &caller_id), 5);
    let proof = same_org_proof(digest, 7, NOW_NS + 10_000_000_000);
    let header = proof.encode().unwrap();
    let ctx = unary_ctx(
        OrgAdmission::OwnerDelegated,
        &caller_id,
        &provider_id,
        owner().org_id(),
        7,
        digest,
        &facts,
    );
    let err = verify_org_admission(
        &ctx,
        &[&header],
        &guard,
        NOW_NS,
        NOW_MONO_MS,
        || true,
        |_| true,
    )
    .unwrap_err();
    assert_eq!(err, AdmissionDenied::MembershipRevoked);
}

// ---------------------------------------------------------------------------
// (c) The decode discipline: strict stream, prefix-tolerant unary
// ---------------------------------------------------------------------------

#[test]
fn stream_decode_refuses_trailing_bytes_unknown_kind_and_truncation() {
    let digest = org_request_digest(&request()).unwrap();
    let proof = stream_proof(STREAM_CALL_KIND_SERVER_STREAMING, digest, 7);
    let bytes = proof.encode().unwrap();
    // The well-formed value decodes…
    assert_eq!(OrgStreamCallProof::decode(&bytes).unwrap(), proof);

    // …but ANY trailing byte is refused, never ignored.
    let mut trailing = bytes.clone();
    trailing.push(0x00);
    assert_eq!(
        OrgStreamCallProof::decode(&trailing).unwrap_err(),
        OrgError::InvalidFormat
    );

    // Any truncation — one byte, or the whole 33-byte suffix.
    assert_eq!(
        OrgStreamCallProof::decode(&bytes[..bytes.len() - 1]).unwrap_err(),
        OrgError::InvalidFormat
    );
    assert_eq!(
        OrgStreamCallProof::decode(&bytes[..bytes.len() - 33]).unwrap_err(),
        OrgError::InvalidFormat
    );

    // Unknown kinds (0 and 4+) mint fine but decode refuses.
    for kind in [0u8, 4u8, 255u8] {
        let bad = stream_proof(kind, digest, 8);
        let bad_bytes = bad.encode().unwrap();
        assert_eq!(
            OrgStreamCallProof::decode(&bad_bytes).unwrap_err(),
            OrgError::InvalidFormat
        );
    }
}

#[test]
fn unary_decode_tolerates_the_streaming_suffix_and_round_trips_the_prefix() {
    let digest = org_request_digest(&request()).unwrap();
    let proof = stream_proof(STREAM_CALL_KIND_SERVER_STREAMING, digest, 7);
    let stream_bytes = proof.encode().unwrap();
    let unary_view = proof.unary_prefix();
    let unary_bytes = unary_view.encode().unwrap();

    // The frozen layout relation: kind (1 B) + session binding
    // (32 B raw) = exactly 33 suffix bytes, and the five prefix
    // fields encode byte-identically in both proofs.
    assert_eq!(stream_bytes.len(), unary_bytes.len() + 33);
    assert_eq!(&stream_bytes[..unary_bytes.len()], &unary_bytes[..]);
    assert_eq!(
        stream_bytes[unary_bytes.len()],
        STREAM_CALL_KIND_SERVER_STREAMING
    );
    assert_eq!(&stream_bytes[unary_bytes.len() + 1..], &SESSION_BINDING[..]);

    // The unary decoder consumes the prefix and IGNORES the suffix —
    // the frozen old-provider behavior, reproduced exactly.
    let decoded = OrgCallProof::decode(&stream_bytes).unwrap();
    assert_eq!(decoded, unary_view);
    assert_eq!(decoded.encode().unwrap(), unary_bytes);
}

// ---------------------------------------------------------------------------
// The frozen coarse-reason wire mapping
// ---------------------------------------------------------------------------

#[test]
fn coarse_reasons_map_to_frozen_wire_bytes() {
    use AdmissionDenied as D;
    use CoarseAdmissionReason as C;

    let unavailable = [
        D::ProviderAuthorityUnavailable,
        D::AuthorityChanged,
        D::ReplayCapacity,
        D::PerCallerReplayCapacity,
        D::PerOrganizationReplayCapacity,
        D::ExternalPoolReplayCapacity,
        D::ActiveStreamCapacity,
        D::ResourceExhausted,
    ];
    let denied = [
        D::NotOrgProtected,
        D::MissingHeader,
        D::MultipleHeaders,
        D::MalformedProof,
        D::ShapeMismatch,
        D::SessionBindingMismatch,
        D::DeadlineExceedsPolicy,
        D::ActiveCallOwned,
        D::Revoked,
        D::MemberBindingMismatch,
        D::ActingOrgMismatch,
        D::UnexpectedCapabilityGrant,
        D::MissingCapabilityGrant,
        D::ForeignIssuer,
        D::GranteeMismatch,
        D::InsufficientRights,
        D::CapabilityMismatch,
        D::TargetNotCovered,
        D::DispatcherGrantScope,
        D::DispatcherGrantInvalid,
        D::MembershipInvalid,
        D::MembershipRevoked,
        D::CapabilityGrantInvalid,
        D::ProofExpired,
        D::BindingInvalid,
        D::Replay,
        D::CallIdCollision,
        D::ProviderPolicyRejected,
    ];
    // Every variant is classified exactly once (8 + 1 + 28 = 37).
    for d in unavailable {
        assert_eq!(d.coarse(), C::Unavailable);
        assert_eq!(d.coarse().to_wire(), 2);
    }
    assert_eq!(D::StreamingUnsupported.coarse(), C::NotSupported);
    assert_eq!(D::StreamingUnsupported.coarse().to_wire(), 1);
    for d in denied {
        assert_eq!(d.coarse(), C::Denied);
        assert_eq!(d.coarse().to_wire(), 0);
    }

    // The wire byte round-trips and unknown bytes refuse.
    assert_eq!(C::from_wire(0), Some(C::Denied));
    assert_eq!(C::from_wire(1), Some(C::NotSupported));
    assert_eq!(C::from_wire(2), Some(C::Unavailable));
    assert_eq!(C::from_wire(3), None);
    assert_eq!(C::from_wire(255), None);
}
