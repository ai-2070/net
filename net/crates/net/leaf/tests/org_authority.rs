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
//! 2. **The admission denial matrix** — every `AdmissionDenied`
//!    variant the leaf's admission pipeline can PRODUCE is surfaced
//!    by a named test at the exact ordered step that owns it, with
//!    the exact variant the core engine produces. The full variant
//!    set is 37; four can never be produced by any net-mesh-leaf code
//!    path and are tracked in the "variant-coverage gap" note below —
//!    the matrix witnesses the other 33, one named test per variant
//!    at its owning step.
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
use net_leaf::org::replay::{
    AdmissionReplayConfig, AdmissionReplayGuard, ReplayOutcome, ReplayPrincipal,
};
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
/// time and nonce. The dispatcher scope is a parameter so the
/// scope-mismatch arm (`DispatcherGrantScope`) and the `Any` arm can
/// both be exercised at their owning step.
fn same_org_creds(scope: DispatcherScope) -> (OrgMembershipCert, OrgDispatcherGrant) {
    let owner = owner();
    let cid = caller_id();
    let membership =
        OrgMembershipCert::try_issue(&owner, cid.clone(), 3, 3600, NOW_SECS, 0x11).unwrap();
    let dispatcher = OrgDispatcherGrant::try_issue(&owner, cid, scope, 3600, NOW_SECS, 0x12).unwrap();
    (membership, dispatcher)
}

/// Cross-org credentials: membership + dispatcher grant from org A
/// (scope-parameterized as [`same_org_creds`]).
fn cross_org_creds(scope: DispatcherScope) -> (OrgMembershipCert, OrgDispatcherGrant) {
    let grantee = grantee();
    let cid = caller_id();
    let membership =
        OrgMembershipCert::try_issue(&grantee, cid.clone(), 1, 3600, NOW_SECS, 0x21).unwrap();
    let dispatcher =
        OrgDispatcherGrant::try_issue(&grantee, cid, scope, 3600, NOW_SECS, 0x22).unwrap();
    (membership, dispatcher)
}

/// A capability grant through the ported ISSUE function. Fully
/// parameterized — issuer, grantee, rights and target — so the
/// grant-side refusal arms (rights, grantee, issuer, scope) can each
/// be fired at their owning step; a DISCOVER-carrying grant brings
/// its audience material exactly as the issue-path rule demands.
fn capability_grant(
    issuer: &OrgKeypair,
    grantee_org: OrgId,
    capability: CapabilityAuthorityId,
    rights: GrantRights,
    target: GrantTargetScope,
) -> OrgCapabilityGrant {
    let audience_random = rights.contains(GrantRights::DISCOVER).then_some([0x44; 64]);
    OrgCapabilityGrant::try_issue(
        issuer,
        grantee_org,
        capability,
        rights,
        target,
        3600,
        [0x11; 32],
        audience_random,
        NOW_SECS,
        0x13,
    )
    .unwrap()
    .0
}

/// A proof over `digest` assembled from arbitrary verified parts —
/// the step-level refusal witnesses use it to mix credentials the
/// convenience builders cannot (a membership naming another member,
/// a membership and dispatcher from different orgs, an expired
/// window). `acting_org` is the signed binding term and must equal
/// `dispatcher.org_id` wherever the witness reaches step 9.
#[allow(clippy::too_many_arguments)]
fn proof_with(
    signer: &EntityKeypair,
    membership: OrgMembershipCert,
    dispatcher: OrgDispatcherGrant,
    grant: Option<OrgCapabilityGrant>,
    acting_org: OrgId,
    digest: [u8; 32],
    call_id: u64,
    expires_ns: u64,
) -> OrgCallProof {
    OrgCallProof::sign_for_call(
        signer,
        membership,
        dispatcher,
        grant,
        acting_org,
        owner().org_id(),
        provider_id(),
        call_id,
        cap(),
        expires_ns,
        digest,
    )
}

/// A same-org unary proof over `digest`.
fn same_org_proof(digest: [u8; 32], call_id: u64, expires_ns: u64) -> OrgCallProof {
    let (membership, dispatcher) = same_org_creds(DispatcherScope::Exact(cap()));
    proof_with(
        &caller(),
        membership,
        dispatcher,
        None,
        owner().org_id(),
        digest,
        call_id,
        expires_ns,
    )
}

/// A cross-org unary proof carrying `grant`.
fn cross_org_proof(
    grant: OrgCapabilityGrant,
    digest: [u8; 32],
    call_id: u64,
    capability: CapabilityAuthorityId,
) -> OrgCallProof {
    let (membership, dispatcher) = cross_org_creds(DispatcherScope::Exact(cap()));
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
    let (membership, dispatcher) = same_org_creds(DispatcherScope::Exact(cap()));
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
    let grant = capability_grant(
        &owner(),
        grantee().org_id(),
        cap(),
        GrantRights::INVOKE,
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
        &owner(),
        grantee().org_id(),
        CapabilityAuthorityId::for_tag("nrpc:other-service"),
        GrantRights::INVOKE,
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
    let grant = capability_grant(
        &owner(),
        grantee().org_id(),
        cap(),
        GrantRights::INVOKE,
        GrantTargetScope::ExactNode(other_provider),
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
    let grant = capability_grant(
        &grantee(),
        grantee().org_id(),
        cap(),
        GrantRights::INVOKE,
        GrantTargetScope::AnyNodeOwnedBy(grantee().org_id()),
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

/// LEAF-18: the digest must bind header ORDER, header VALUES and the
/// deadline — and NOTHING else. Mint and verify share
/// `org_request_digest`, so a pipeline-only witness cannot tell a
/// binding digest from a constant; these discriminators can (a
/// sorting digest ties the reorder, an unstripped admission header
/// breaks the strip equation, an unbound deadline ties the retime).
#[test]
fn the_request_digest_binds_header_order_values_and_the_deadline() {
    let base = org_request_digest(&request()).unwrap();

    // Header ORDER binds: the duplicated `x-trace` entries are
    // distinct positions, not a set.
    let mut reordered = request();
    reordered.headers.swap(0, 2);
    let reordered_digest = org_request_digest(&reordered).unwrap();
    assert_ne!(reordered_digest, base, "header order must bind the digest");

    // Header VALUES bind.
    let mut revalued = request();
    revalued.headers[0].1 = b"t3".to_vec();
    assert_ne!(
        org_request_digest(&revalued).unwrap(),
        base,
        "header values must bind the digest"
    );

    // The deadline binds.
    let mut redeadlined = request();
    redeadlined.deadline_ns += 1;
    assert_ne!(
        org_request_digest(&redeadlined).unwrap(),
        base,
        "the deadline must bind the digest"
    );

    // …and the admission header binds NOTHING: its value is stripped
    // before hashing, so the same call digests identically whatever
    // proof header it carries.
    let mut reproofed = request();
    reproofed.headers[1].1 = b"a-different-proof-header".to_vec();
    assert_eq!(
        org_request_digest(&reproofed).unwrap(),
        base,
        "the admission header must be stripped from the digest"
    );

    // The pipeline discriminates through the real binding: a proof
    // signed over the ORIGINAL request refuses when the request
    // arrives reordered.
    let caller_id = caller_id();
    let provider_id = provider_id();
    let facts = RevocationFacts::default();
    let guard = AdmissionReplayGuard::with_defaults();
    let proof = same_org_proof(base, 7, NOW_NS + 10_000_000_000);
    let header = proof.encode().unwrap();
    let ctx = unary_ctx(
        OrgAdmission::OwnerDelegated,
        &caller_id,
        &provider_id,
        owner().org_id(),
        7,
        reordered_digest,
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

/// LEAF-19: the floor view is raise-only. A later bundle claiming a
/// LOWER floor (or none) must never un-revoke — no counter moves, no
/// epoch bump, and the admission consequence is unchanged.
#[test]
fn a_lower_floor_never_rolls_back_a_raised_one() {
    let caller_id = caller_id();
    let mut facts = RevocationFacts::default();
    assert_eq!(
        facts.merge_floors(owner().org_id(), &[(caller_id.clone(), 5)]),
        1
    );
    let epoch_after_raise = facts.epoch;

    // The un-revocation attempts: floor 4 and floor 0 both lose.
    assert_eq!(
        facts.merge_floors(owner().org_id(), &[(caller_id.clone(), 4)]),
        0
    );
    assert_eq!(
        facts.merge_floors(owner().org_id(), &[(caller_id.clone(), 0)]),
        0
    );
    assert_eq!(facts.floor_for(&owner().org_id(), &caller_id), 5);
    assert_eq!(
        facts.epoch, epoch_after_raise,
        "a lower floor moves no epoch — the stability view must not \
         wobble for a rollback attempt"
    );

    // And the pipeline consequence: the generation-3 cert stays
    // refused — an un-revocation is invisible to admission.
    let digest = org_request_digest(&request()).unwrap();
    let provider_id = provider_id();
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

/// LEAF-19: the floor's strictness, witnessed at the outcome level —
/// "every cert below this generation is revoked" leaves a cert AT the
/// floor alive, and it admits end to end.
#[test]
fn a_membership_at_the_floor_is_still_admitted() {
    let digest = org_request_digest(&request()).unwrap();
    let caller_id = caller_id();
    let provider_id = provider_id();
    let mut facts = RevocationFacts::default();
    // The same-org fixture cert is at generation 3; the floor lands
    // exactly on it.
    assert_eq!(
        facts.merge_floors(owner().org_id(), &[(caller_id.clone(), 3)]),
        1
    );
    assert_eq!(facts.floor_for(&owner().org_id(), &caller_id), 3);
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
    .expect("a cert AT the floor is alive");
    assert_eq!(admitted.caller, caller_id);
}

// ---------------------------------------------------------------------------
// (b2) The step-level denial matrix — one named test per variant the
//      leaf's pipeline can produce, at the step that owns it (LEAF-7)
// ---------------------------------------------------------------------------
//
// Variant-coverage gap (LEAF-7, narrowed claim — the tracked gap the
// closure allows): the enum carries 37 variants and this matrix
// covers the 33 any net-mesh-leaf code path can produce. Four are
// unwitnessable here because the leaf has ZERO construction sites for
// them: `AdmissionDenied::ActiveStreamCapacity`,
// `AdmissionDenied::Revoked`, `AdmissionDenied::ResourceExhausted`
// and `AdmissionDenied::ProviderAuthorityUnavailable`. The situations
// they name surface in the leaf under other types: mid-call
// revocation and byte-budget retirement travel as
// `StreamTerminalReason::Revoked` / `StreamTerminalReason::ResourceExhausted`
// (coarse bytes at the wire), and a poisoned revocation store fails
// the §9.5 stability recheck → `AuthorityChanged` at step 9.5. The
// CORE mints all four (its engine owns the active-stream reserve
// budget and the installed-authority pre-check the leaf has no
// analogue for): `src/adapter/net/cortex/rpc.rs` and
// `src/adapter/net/org_admission_gate.rs` — a different
// `AdmissionDenied` enum, in a different crate. No leaf test can
// produce these four through the real pipeline; asserting the enum
// literal would be a tautology, so the gap is tracked here instead.

/// Charge `guard` with live replay entries — the state the step-10
/// outcome witnesses need before the pipeline consults it.
fn fill_replay(
    guard: &AdmissionReplayGuard,
    caller: &EntityId,
    acting_org: &OrgId,
    call_ids: &[u64],
) {
    for &call_id in call_ids {
        let outcome = guard.admit(
            ReplayPrincipal {
                caller,
                acting_org,
                provider_owner_org: &owner().org_id(),
            },
            call_id,
            [call_id as u8; 32],
            NOW_MONO_MS + 1_000_000,
            NOW_MONO_MS,
        );
        assert_eq!(outcome, ReplayOutcome::Admitted, "the fill must land");
    }
}

#[test]
fn public_authenticated_mode_is_refused_as_not_org_protected() {
    let digest = org_request_digest(&request()).unwrap();
    let caller_id = caller_id();
    let provider_id = provider_id();
    let facts = RevocationFacts::default();
    let guard = AdmissionReplayGuard::with_defaults();
    let proof = same_org_proof(digest, 7, NOW_NS + 10_000_000_000);
    let header = proof.encode().unwrap();
    // Step 1: a non-org-protected mode never reaches the engine.
    let ctx = unary_ctx(
        OrgAdmission::PublicAuthenticated,
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
    assert_eq!(err, AdmissionDenied::NotOrgProtected);
}

#[test]
fn two_admission_headers_are_refused_as_multiple_headers() {
    let digest = org_request_digest(&request()).unwrap();
    let caller_id = caller_id();
    let provider_id = provider_id();
    let facts = RevocationFacts::default();
    let guard = AdmissionReplayGuard::with_defaults();
    let proof = same_org_proof(digest, 7, NOW_NS + 10_000_000_000);
    let header = proof.encode().unwrap();
    let second: &[u8] = b"a-second-proof-header";
    // Step 2: exactly one admission header, or deny — two values are
    // refused before anything decodes.
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
        &[&header, second],
        &guard,
        NOW_NS,
        NOW_MONO_MS,
        || true,
        |_| true,
    )
    .unwrap_err();
    assert_eq!(err, AdmissionDenied::MultipleHeaders);
}

#[test]
fn a_proof_for_another_member_is_refused_as_member_binding_mismatch() {
    let digest = org_request_digest(&request()).unwrap();
    let caller_id = caller_id();
    let provider_id = provider_id();
    let facts = RevocationFacts::default();
    let guard = AdmissionReplayGuard::with_defaults();
    // The proof's membership names a DIFFERENT member than the
    // TOFU-authenticated channel peer — a captured proof replayed by
    // another peer, refused before any signature work (step 5).
    let other_member = EntityId::from_bytes(*EntityKeypair::from_secret([0x25; 32]).entity_id());
    let membership =
        OrgMembershipCert::try_issue(&owner(), other_member, 3, 3600, NOW_SECS, 0x41).unwrap();
    let (_, dispatcher) = same_org_creds(DispatcherScope::Exact(cap()));
    let proof = proof_with(
        &caller(),
        membership,
        dispatcher,
        None,
        owner().org_id(),
        digest,
        7,
        NOW_NS + 10_000_000_000,
    );
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
    assert_eq!(err, AdmissionDenied::MemberBindingMismatch);
}

#[test]
fn membership_and_dispatcher_from_different_orgs_are_refused_as_acting_org_mismatch() {
    let digest = org_request_digest(&request()).unwrap();
    let caller_id = caller_id();
    let provider_id = provider_id();
    let facts = RevocationFacts::default();
    let guard = AdmissionReplayGuard::with_defaults();
    // The acting org is named by the membership (org A); the
    // dispatcher grant must agree and names org B.
    let membership =
        OrgMembershipCert::try_issue(&grantee(), caller_id.clone(), 1, 3600, NOW_SECS, 0x41).unwrap();
    let dispatcher = OrgDispatcherGrant::try_issue(
        &owner(),
        caller_id.clone(),
        DispatcherScope::Exact(cap()),
        3600,
        NOW_SECS,
        0x42,
    )
    .unwrap();
    let proof = proof_with(
        &caller(),
        membership,
        dispatcher,
        None,
        owner().org_id(),
        digest,
        7,
        NOW_NS + 10_000_000_000,
    );
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
    assert_eq!(err, AdmissionDenied::ActingOrgMismatch);
}

#[test]
fn a_capability_grant_on_a_same_org_call_is_refused_as_unexpected_capability_grant() {
    let digest = org_request_digest(&request()).unwrap();
    let caller_id = caller_id();
    let provider_id = provider_id();
    let facts = RevocationFacts::default();
    let guard = AdmissionReplayGuard::with_defaults();
    // `OwnerDelegated` admission carrying a cross-org capability
    // grant: same-org calls have none (step 6).
    let (membership, dispatcher) = same_org_creds(DispatcherScope::Exact(cap()));
    let grant = capability_grant(
        &owner(),
        grantee().org_id(),
        cap(),
        GrantRights::INVOKE,
        GrantTargetScope::AnyNodeOwnedBy(owner().org_id()),
    );
    let proof = proof_with(
        &caller(),
        membership,
        dispatcher,
        Some(grant),
        owner().org_id(),
        digest,
        7,
        NOW_NS + 10_000_000_000,
    );
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
    assert_eq!(err, AdmissionDenied::UnexpectedCapabilityGrant);
}

#[test]
fn a_granted_call_without_its_grant_is_refused_as_missing_capability_grant() {
    let digest = org_request_digest(&request()).unwrap();
    let caller_id = caller_id();
    let provider_id = provider_id();
    let facts = RevocationFacts::default();
    let guard = AdmissionReplayGuard::with_defaults();
    // `CrossOrgGranted` admission carrying NO capability grant
    // (step 6).
    let (membership, dispatcher) = cross_org_creds(DispatcherScope::Exact(cap()));
    let proof = proof_with(
        &caller(),
        membership,
        dispatcher,
        None,
        grantee().org_id(),
        digest,
        7,
        NOW_NS + 10_000_000_000,
    );
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
    assert_eq!(err, AdmissionDenied::MissingCapabilityGrant);
}

#[test]
fn a_grant_for_another_grantee_is_refused_as_grantee_mismatch() {
    let digest = org_request_digest(&request()).unwrap();
    let caller_id = caller_id();
    let provider_id = provider_id();
    let facts = RevocationFacts::default();
    let guard = AdmissionReplayGuard::with_defaults();

    // Arm 1 (step 6, same-org mode): the caller acts for org A while
    // the registration is MY owner's — nobody's same-org call.
    let (membership, dispatcher) = cross_org_creds(DispatcherScope::Exact(cap()));
    let proof = proof_with(
        &caller(),
        membership,
        dispatcher,
        None,
        grantee().org_id(),
        digest,
        7,
        NOW_NS + 10_000_000_000,
    );
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
    assert_eq!(err, AdmissionDenied::GranteeMismatch);

    // Arm 2 (step 6, granted mode): the grant names a THIRD org as
    // grantee, not the caller's verified acting org.
    let third_org = OrgKeypair::from_bytes([0x13; 32]);
    let grant = capability_grant(
        &owner(),
        third_org.org_id(),
        cap(),
        GrantRights::INVOKE,
        GrantTargetScope::AnyNodeOwnedBy(owner().org_id()),
    );
    let proof = cross_org_proof(grant, digest, 8, cap());
    let header = proof.encode().unwrap();
    let ctx = unary_ctx(
        OrgAdmission::CrossOrgGranted,
        &caller_id,
        &provider_id,
        owner().org_id(),
        8,
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
    assert_eq!(err, AdmissionDenied::GranteeMismatch);
}

#[test]
fn a_grant_without_invoke_is_refused_as_insufficient_rights() {
    let digest = org_request_digest(&request()).unwrap();
    let caller_id = caller_id();
    let provider_id = provider_id();
    let facts = RevocationFacts::default();
    let guard = AdmissionReplayGuard::with_defaults();
    // The grant carries DISCOVER only — invoking needs `rights ⊇
    // INVOKE` (step 6).
    let grant = capability_grant(
        &owner(),
        grantee().org_id(),
        cap(),
        GrantRights::DISCOVER,
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
    assert_eq!(err, AdmissionDenied::InsufficientRights);
}

#[test]
fn a_dispatcher_grant_out_of_scope_is_refused_as_dispatcher_grant_scope() {
    let digest = org_request_digest(&request()).unwrap();
    let caller_id = caller_id();
    let provider_id = provider_id();
    let facts = RevocationFacts::default();
    let guard = AdmissionReplayGuard::with_defaults();

    // Arm 1 (step 7): the dispatcher grant empowers a DIFFERENT
    // entity to dispatch.
    let other = EntityId::from_bytes(*EntityKeypair::from_secret([0x25; 32]).entity_id());
    let membership =
        OrgMembershipCert::try_issue(&owner(), caller_id.clone(), 3, 3600, NOW_SECS, 0x41).unwrap();
    let dispatcher = OrgDispatcherGrant::try_issue(
        &owner(),
        other,
        DispatcherScope::Exact(cap()),
        3600,
        NOW_SECS,
        0x42,
    )
    .unwrap();
    let proof = proof_with(
        &caller(),
        membership,
        dispatcher,
        None,
        owner().org_id(),
        digest,
        7,
        NOW_NS + 10_000_000_000,
    );
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
    assert_eq!(err, AdmissionDenied::DispatcherGrantScope);

    // Arm 2 (step 7): the right dispatcher, but an `Exact` scope
    // naming a DIFFERENT capability than the one invoked.
    let (membership, _) = same_org_creds(DispatcherScope::Exact(cap()));
    let dispatcher = OrgDispatcherGrant::try_issue(
        &owner(),
        caller_id.clone(),
        DispatcherScope::Exact(CapabilityAuthorityId::for_tag("nrpc:other-service")),
        3600,
        NOW_SECS,
        0x43,
    )
    .unwrap();
    let proof = proof_with(
        &caller(),
        membership,
        dispatcher,
        None,
        owner().org_id(),
        digest,
        8,
        NOW_NS + 10_000_000_000,
    );
    let header = proof.encode().unwrap();
    let ctx = unary_ctx(
        OrgAdmission::OwnerDelegated,
        &caller_id,
        &provider_id,
        owner().org_id(),
        8,
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
    assert_eq!(err, AdmissionDenied::DispatcherGrantScope);
}

#[test]
fn a_dispatcher_grant_for_any_capability_admits() {
    let digest = org_request_digest(&request()).unwrap();
    let caller_id = caller_id();
    let provider_id = provider_id();
    let facts = RevocationFacts::default();
    let guard = AdmissionReplayGuard::with_defaults();
    // The `Any` arm of the very same step-7 scope check: the org
    // trusts this dispatcher broadly, and the call admits end to end.
    let (membership, dispatcher) = same_org_creds(DispatcherScope::Any);
    let proof = proof_with(
        &caller(),
        membership,
        dispatcher,
        None,
        owner().org_id(),
        digest,
        7,
        NOW_NS + 10_000_000_000,
    );
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
    .expect("a dispatcher scoped Any covers the invoked capability");
    assert_eq!(admitted.caller, caller_id);
    assert_eq!(admitted.capability, cap());
}

#[test]
fn an_expired_membership_is_refused_as_membership_invalid() {
    let digest = org_request_digest(&request()).unwrap();
    let caller_id = caller_id();
    let provider_id = provider_id();
    let facts = RevocationFacts::default();
    let guard = AdmissionReplayGuard::with_defaults();
    // The membership window closed an hour ago (step 8's credential
    // verify/window check).
    let membership = OrgMembershipCert::issue_at(
        &owner(),
        caller_id.clone(),
        3,
        NOW_SECS - 7_200,
        NOW_SECS - 3_600,
        0x41,
    );
    let dispatcher = OrgDispatcherGrant::try_issue(
        &owner(),
        caller_id.clone(),
        DispatcherScope::Exact(cap()),
        3600,
        NOW_SECS,
        0x42,
    )
    .unwrap();
    let proof = proof_with(
        &caller(),
        membership,
        dispatcher,
        None,
        owner().org_id(),
        digest,
        7,
        NOW_NS + 10_000_000_000,
    );
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
    assert_eq!(err, AdmissionDenied::MembershipInvalid);
}

#[test]
fn an_expired_dispatcher_grant_is_refused_as_dispatcher_grant_invalid() {
    let digest = org_request_digest(&request()).unwrap();
    let caller_id = caller_id();
    let provider_id = provider_id();
    let facts = RevocationFacts::default();
    let guard = AdmissionReplayGuard::with_defaults();
    // The membership is fresh; the DISPATCHER grant's window closed
    // an hour ago (step 8).
    let membership =
        OrgMembershipCert::try_issue(&owner(), caller_id.clone(), 3, 3600, NOW_SECS, 0x41).unwrap();
    let dispatcher = OrgDispatcherGrant::issue_at(
        &owner(),
        caller_id.clone(),
        DispatcherScope::Exact(cap()),
        NOW_SECS - 7_200,
        NOW_SECS - 3_600,
        0x42,
    );
    let proof = proof_with(
        &caller(),
        membership,
        dispatcher,
        None,
        owner().org_id(),
        digest,
        7,
        NOW_NS + 10_000_000_000,
    );
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
    assert_eq!(err, AdmissionDenied::DispatcherGrantInvalid);
}

#[test]
fn an_expired_capability_grant_is_refused_as_capability_grant_invalid() {
    let digest = org_request_digest(&request()).unwrap();
    let caller_id = caller_id();
    let provider_id = provider_id();
    let facts = RevocationFacts::default();
    let guard = AdmissionReplayGuard::with_defaults();
    // Membership and dispatcher are fresh (minted inside
    // `cross_org_proof`); the CAPABILITY grant's window closed an
    // hour ago (step 8). Issued at `NOW - 7200` with a 3600 s life:
    // `[NOW-7200, NOW-3600]`.
    let grant = OrgCapabilityGrant::try_issue(
        &owner(),
        grantee().org_id(),
        cap(),
        GrantRights::INVOKE,
        GrantTargetScope::AnyNodeOwnedBy(owner().org_id()),
        3600,
        [0x11; 32],
        None,
        NOW_SECS - 7_200,
        0x13,
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
    assert_eq!(err, AdmissionDenied::CapabilityGrantInvalid);
}

#[test]
fn a_failed_stability_recheck_is_refused_as_authority_changed() {
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
    // Step 9.5: the provider's security view moved mid-admission.
    let err = verify_org_admission(
        &ctx,
        &[&header],
        &guard,
        NOW_NS,
        NOW_MONO_MS,
        || false,
        |_| true,
    )
    .unwrap_err();
    assert_eq!(err, AdmissionDenied::AuthorityChanged);
    // …and the contract on the outcome: the refusal consumed NO
    // replay slot, so a retry from a fresh view admits rather than
    // reading as `Replay`.
    let admitted = verify_org_admission(
        &ctx,
        &[&header],
        &guard,
        NOW_NS,
        NOW_MONO_MS,
        || true,
        |_| true,
    )
    .expect("the stability refusal consumed no replay slot");
    assert_eq!(admitted.caller, caller_id);
}

#[test]
fn a_full_replay_guard_is_refused_as_replay_capacity() {
    let digest = org_request_digest(&request()).unwrap();
    let caller_id = caller_id();
    let provider_id = provider_id();
    let facts = RevocationFacts::default();
    // Every global slot holds a still-live guard (owner-org traffic,
    // which draws on the reserve AND whatever external capacity is
    // idle) — a novel admission fails closed rather than evicting
    // one (step 10).
    let guard = AdmissionReplayGuard::try_new(AdmissionReplayConfig {
        max_entries: 4,
        max_entries_per_caller: 3,
        owner_reserved_entries: 1,
        max_entries_per_external_org: 3,
    })
    .unwrap();
    let a = EntityId::from_bytes([0xA1; 32]);
    let b = EntityId::from_bytes([0xA2; 32]);
    fill_replay(&guard, &a, &owner().org_id(), &[1, 2, 3]);
    fill_replay(&guard, &b, &owner().org_id(), &[1]);
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
    assert_eq!(err, AdmissionDenied::ReplayCapacity);
}

#[test]
fn a_caller_over_quota_is_refused_as_per_caller_replay_capacity() {
    let digest = org_request_digest(&request()).unwrap();
    let caller_id = caller_id();
    let provider_id = provider_id();
    let facts = RevocationFacts::default();
    // THIS caller already holds its maximum retained entries; the
    // denial must name the caller's own quota, not the global guard
    // (step 10).
    let guard = AdmissionReplayGuard::try_new(AdmissionReplayConfig {
        max_entries: 8,
        max_entries_per_caller: 2,
        owner_reserved_entries: 1,
        max_entries_per_external_org: 7,
    })
    .unwrap();
    fill_replay(&guard, &caller_id, &owner().org_id(), &[1, 2]);
    let proof = same_org_proof(digest, 3, NOW_NS + 10_000_000_000);
    let header = proof.encode().unwrap();
    let ctx = unary_ctx(
        OrgAdmission::OwnerDelegated,
        &caller_id,
        &provider_id,
        owner().org_id(),
        3,
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
    assert_eq!(err, AdmissionDenied::PerCallerReplayCapacity);
}

#[test]
fn an_org_over_quota_is_refused_as_per_organization_replay_capacity() {
    let digest = org_request_digest(&request()).unwrap();
    let caller_id = caller_id();
    let provider_id = provider_id();
    let facts = RevocationFacts::default();
    // The external acting org has consumed its aggregate allocation
    // across TWO member identities — the org quota, not any caller's
    // (step 10).
    let guard = AdmissionReplayGuard::try_new(AdmissionReplayConfig {
        max_entries: 8,
        max_entries_per_caller: 3,
        owner_reserved_entries: 1,
        max_entries_per_external_org: 2,
    })
    .unwrap();
    let m1 = EntityId::from_bytes([0xA1; 32]);
    let m2 = EntityId::from_bytes([0xA2; 32]);
    fill_replay(&guard, &m1, &grantee().org_id(), &[1]);
    fill_replay(&guard, &m2, &grantee().org_id(), &[1]);
    let grant = capability_grant(
        &owner(),
        grantee().org_id(),
        cap(),
        GrantRights::INVOKE,
        GrantTargetScope::AnyNodeOwnedBy(owner().org_id()),
    );
    let proof = cross_org_proof(grant, digest, 3, cap());
    let header = proof.encode().unwrap();
    let ctx = unary_ctx(
        OrgAdmission::CrossOrgGranted,
        &caller_id,
        &provider_id,
        owner().org_id(),
        3,
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
    assert_eq!(err, AdmissionDenied::PerOrganizationReplayCapacity);
}

#[test]
fn a_full_external_replay_pool_is_refused_as_external_pool_replay_capacity() {
    let digest = org_request_digest(&request()).unwrap();
    let caller_id = caller_id();
    let provider_id = provider_id();
    let facts = RevocationFacts::default();
    // The external pool is full with NO single org over its quota —
    // three live entries from three distinct external orgs — and the
    // refusal must say so rather than blame the fourth org's own
    // allocation (step 10).
    let guard = AdmissionReplayGuard::try_new(AdmissionReplayConfig {
        max_entries: 4,
        max_entries_per_caller: 3,
        owner_reserved_entries: 1,
        max_entries_per_external_org: 3,
    })
    .unwrap();
    for (seed, call_id) in [(0xA1u8, 1u64), (0xA2, 1), (0xA3, 1)] {
        let member = EntityId::from_bytes([seed; 32]);
        let org = OrgKeypair::from_bytes([seed; 32]).org_id();
        fill_replay(&guard, &member, &org, &[call_id]);
    }
    // The fourth external org's caller: its own quota is untouched,
    // so only the pool can refuse it.
    let org_d = OrgKeypair::from_bytes([0x88; 32]);
    let membership =
        OrgMembershipCert::try_issue(&org_d, caller_id.clone(), 1, 3600, NOW_SECS, 0x41).unwrap();
    let dispatcher = OrgDispatcherGrant::try_issue(
        &org_d,
        caller_id.clone(),
        DispatcherScope::Exact(cap()),
        3600,
        NOW_SECS,
        0x42,
    )
    .unwrap();
    let grant = capability_grant(
        &owner(),
        org_d.org_id(),
        cap(),
        GrantRights::INVOKE,
        GrantTargetScope::AnyNodeOwnedBy(owner().org_id()),
    );
    let proof = proof_with(
        &caller(),
        membership,
        dispatcher,
        Some(grant),
        org_d.org_id(),
        digest,
        3,
        NOW_NS + 10_000_000_000,
    );
    let header = proof.encode().unwrap();
    let ctx = unary_ctx(
        OrgAdmission::CrossOrgGranted,
        &caller_id,
        &provider_id,
        owner().org_id(),
        3,
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
    assert_eq!(err, AdmissionDenied::ExternalPoolReplayCapacity);
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
