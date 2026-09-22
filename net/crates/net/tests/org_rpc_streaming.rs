//! Stage 1 — core protected server-streaming witnesses
//! (`ORG_SCOPED_STREAMING_PLAN.md`, pinned brief `spikes/org-streaming/S1_BRIEF.md`).
//!
//! Slice 1.2 — streaming proof (contract 2): `OrgStreamCallProof` /
//! `StreamCallBinding` admission through the shape-aware verifier, the
//! §1.4 frozen-old-provider refusal, and the §1.3 session-binding fence.

#![cfg(all(feature = "net", feature = "cortex", feature = "fixtures"))]

#[path = "org_rpc_streaming/fixture.rs"]
mod fixture;
#[path = "org_rpc_streaming/frozen_85ecc77c9.rs"]
mod frozen_85ecc77c9;

use std::sync::atomic::AtomicUsize;
use std::sync::Arc;

use net::adapter::net::behavior::admission_clock::ClockSample;
use net::adapter::net::behavior::org::OrgKeypair;
use net::adapter::net::behavior::org_admission::{
    verify_org_admission, AdmissionContext, AdmissionDenied, CoarseAdmissionReason, OrgAdmission,
};
use net::adapter::net::behavior::org_admission_replay::AdmissionReplayGuard;
use net::adapter::net::behavior::org_call::{
    OrgCallProof, OrgStreamCallProof, RpcCallShape, STREAM_CALL_KIND_SERVER_STREAMING,
};
use net::adapter::net::behavior::org_grant::CapabilityAuthorityId;
use net::adapter::net::behavior::org_revocation::OrgRevocationState;
use net::adapter::net::cortex::{RpcRequestPayload, RpcStatus, FLAG_RPC_STREAMING_RESPONSE};
use net::adapter::net::identity::EntityKeypair;
use net::adapter::net::mesh_rpc::{test_sign_admission_proof, OrgProofIntent};
use net::adapter::net::org_admission_gate::org_request_digest;

use frozen_85ecc77c9::UnaryAdmission as FrozenProviderAdm;

const CALL_ID: u64 = 42;
const SERVICE: &str = "svc";

fn cap() -> CapabilityAuthorityId {
    CapabilityAuthorityId::for_tag(&format!("nrpc:{SERVICE}"))
}

/// Mint "the new caller's bytes" through the REAL shared caller-side mint
/// helper — an [`OrgStreamCallProof`] for a server-streaming opening bound to
/// `session_binding` — plus the request it was minted over.
fn mint_stream_opening(
    intent: &OrgProofIntent,
    session_binding: [u8; 32],
    call_id: u64,
) -> (Vec<u8>, RpcRequestPayload) {
    let req = fixture::opening_request(SERVICE, FLAG_RPC_STREAMING_RESPONSE, b"open");
    let (_name, bytes) = test_sign_admission_proof(
        intent,
        call_id,
        &req,
        RpcCallShape::ServerStreaming,
        Some(session_binding),
    )
    .expect("mint the streaming proof header");
    (bytes, req)
}

/// The provider-side context for a server-streaming opening (registered
/// shape SS; SS payload flags; the receiving session's binding).
fn stream_ctx<'a>(
    mode: OrgAdmission,
    caller: &'a net::adapter::net::identity::EntityId,
    provider: &'a net::adapter::net::identity::EntityId,
    provider_owner_org: net::adapter::net::behavior::org::OrgId,
    call_id: u64,
    digest: [u8; 32],
    registered_shape: RpcCallShape,
    session_binding: Option<[u8; 32]>,
    floors: &'a OrgRevocationState,
) -> AdmissionContext<'a> {
    AdmissionContext::new(
        mode,
        caller,
        provider,
        provider_owner_org,
        cap(),
        call_id,
        digest,
        registered_shape,
        false,
        true,
        session_binding,
        floors,
        0,
    )
}

/// §1.2 — a well-formed same-org (owner-delegated) stream opening verifies
/// through the shape-aware verifier with full four-party attribution.
#[test]
fn stream_opening_admits_same_org() {
    let org_b = OrgKeypair::from_bytes([0x42u8; 32]);
    let caller_kp = EntityKeypair::from_bytes([0x24u8; 32]);
    let provider = net::adapter::net::identity::EntityId::from_bytes([0x99u8; 32]);
    let intent =
        fixture::owner_delegated_intent(caller_kp.clone(), &org_b, provider.clone(), SERVICE);
    let (bind_open, _bind_other) = fixture::session_bindings();

    let (bytes, req) = mint_stream_opening(&intent, bind_open, CALL_ID);
    let digest = org_request_digest(&req).expect("request digest");
    let floors = OrgRevocationState::empty();
    let caller_entity = caller_kp.entity_id().clone();
    let ctx = stream_ctx(
        OrgAdmission::OwnerDelegated,
        &caller_entity,
        &provider,
        org_b.org_id(),
        CALL_ID,
        digest,
        RpcCallShape::ServerStreaming,
        Some(bind_open),
        &floors,
    );
    let replay = AdmissionReplayGuard::with_defaults();

    let admitted = verify_org_admission(
        &ctx,
        &[&bytes],
        &replay,
        ClockSample::now(),
        || true,
        |_| true,
    )
    .expect("a well-formed same-org stream opening admits");
    assert_eq!(admitted.caller, *caller_kp.entity_id());
    assert_eq!(admitted.acting_org, org_b.org_id());
    assert_eq!(admitted.provider_org, org_b.org_id());
    assert_eq!(admitted.provider, provider);
    assert_eq!(admitted.capability, cap());
}

/// §1.2 — a well-formed cross-org stream opening (membership + dispatcher
/// from org A, a B→A INVOKE grant) verifies.
#[test]
fn stream_opening_admits_cross_org() {
    let org_a = OrgKeypair::from_bytes([0x77u8; 32]);
    let org_b = OrgKeypair::from_bytes([0x42u8; 32]);
    let caller_kp = EntityKeypair::from_bytes([0x24u8; 32]);
    let provider = net::adapter::net::identity::EntityId::from_bytes([0x99u8; 32]);
    let intent =
        fixture::cross_org_intent(caller_kp.clone(), &org_a, &org_b, provider.clone(), SERVICE);
    let (bind_open, _bind_other) = fixture::session_bindings();

    let (bytes, req) = mint_stream_opening(&intent, bind_open, CALL_ID);
    let digest = org_request_digest(&req).expect("request digest");
    let floors = OrgRevocationState::empty();
    let caller_entity = caller_kp.entity_id().clone();
    let ctx = stream_ctx(
        OrgAdmission::CrossOrgGranted,
        &caller_entity,
        &provider,
        org_b.org_id(),
        CALL_ID,
        digest,
        RpcCallShape::ServerStreaming,
        Some(bind_open),
        &floors,
    );
    let replay = AdmissionReplayGuard::with_defaults();

    let admitted = verify_org_admission(
        &ctx,
        &[&bytes],
        &replay,
        ClockSample::now(),
        || true,
        |_| true,
    )
    .expect("a well-formed cross-org stream opening admits");
    assert_eq!(admitted.caller, *caller_kp.entity_id());
    assert_eq!(admitted.acting_org, org_a.org_id());
    assert_eq!(admitted.provider_org, org_b.org_id());
}

/// §1.2 — a well-formed FULL streaming proof signed under the UNARY domain
/// is `BindingInvalid` on a streaming registration — DISTINCT from a
/// truncated unary-format proof, which the strict streaming decoder refuses
/// as `MalformedProof`.
#[test]
fn unary_context_proof_is_binding_invalid_on_stream_registration() {
    let org_b = OrgKeypair::from_bytes([0x42u8; 32]);
    let caller_kp = EntityKeypair::from_bytes([0x24u8; 32]);
    let provider = net::adapter::net::identity::EntityId::from_bytes([0x99u8; 32]);
    let intent =
        fixture::owner_delegated_intent(caller_kp.clone(), &org_b, provider.clone(), SERVICE);
    let (bind_open, _bind_other) = fixture::session_bindings();
    let req = fixture::opening_request(SERVICE, FLAG_RPC_STREAMING_RESPONSE, b"open");
    let digest = org_request_digest(&req).expect("request digest");

    // The caller computes the UNARY binding ("net-org-call-v1") over the 11
    // unary fields and signs THAT — then presents the result in FULL
    // streaming format (kind + session_binding present, well-formed bytes).
    // Expiry from one clock sample, well inside the 30 s TTL ceiling.
    let expiry = ClockSample::now().wall_ns + 20_000_000_000;
    let unary_proof = OrgCallProof::sign_for_call(
        &intent.caller,
        intent.membership.clone(),
        intent.dispatcher.clone(),
        intent.capability_grant.clone(),
        intent.acting_org,
        intent.provider_owner_org,
        intent.provider.clone(),
        CALL_ID,
        intent.capability,
        expiry,
        digest,
    );
    let full = OrgStreamCallProof {
        caller_membership: unary_proof.caller_membership.clone(),
        dispatcher_grant: unary_proof.dispatcher_grant.clone(),
        capability_grant: unary_proof.capability_grant.clone(),
        proof_expires_at_unix_ns: unary_proof.proof_expires_at_unix_ns,
        call_binding_sig: unary_proof.call_binding_sig,
        kind: STREAM_CALL_KIND_SERVER_STREAMING,
        session_binding: bind_open,
    };
    let bytes = full.encode().expect("encode the full streaming proof");

    let floors = OrgRevocationState::empty();
    let caller_entity = caller_kp.entity_id().clone();
    let ctx = stream_ctx(
        OrgAdmission::OwnerDelegated,
        &caller_entity,
        &provider,
        org_b.org_id(),
        CALL_ID,
        digest,
        RpcCallShape::ServerStreaming,
        Some(bind_open),
        &floors,
    );
    let replay = AdmissionReplayGuard::with_defaults();
    assert_eq!(
        verify_org_admission(
            &ctx,
            &[&bytes],
            &replay,
            ClockSample::now(),
            || true,
            |_| true
        ),
        Err(AdmissionDenied::BindingInvalid),
        "a unary-domain signature must never authorize a streaming opening",
    );

    // DISTINCTNESS: a TRUNCATED unary-format proof (the 5-field encoding
    // alone — no kind, no session_binding) fails the strict full-consumption
    // streaming decoder as `MalformedProof`, a different typed reason.
    let truncated = unary_proof.encode().expect("encode the unary proof");
    assert_eq!(
        verify_org_admission(
            &ctx,
            &[&truncated],
            &replay,
            ClockSample::now(),
            || true,
            |_| true
        ),
        Err(AdmissionDenied::MalformedProof),
        "a truncated unary-format proof is malformed on a streaming registration, \
         not merely binding-invalid",
    );
}

/// §1.5 step 4 (preserved) — a streaming proof on a UNARY registration is
/// the typed `StreamingUnsupported` refusal (coarse `NotSupported`), never
/// admitted under a unary binding.
#[test]
fn stream_proof_on_unary_registration_is_not_supported() {
    let org_b = OrgKeypair::from_bytes([0x42u8; 32]);
    let caller_kp = EntityKeypair::from_bytes([0x24u8; 32]);
    let provider = net::adapter::net::identity::EntityId::from_bytes([0x99u8; 32]);
    let intent =
        fixture::owner_delegated_intent(caller_kp.clone(), &org_b, provider.clone(), SERVICE);
    let (bind_open, _bind_other) = fixture::session_bindings();
    let (bytes, req) = mint_stream_opening(&intent, bind_open, CALL_ID);
    let digest = org_request_digest(&req).expect("request digest");

    let floors = OrgRevocationState::empty();
    let caller_entity = caller_kp.entity_id().clone();
    // The registration is UNARY; the call's flags say server-streaming.
    let ctx = stream_ctx(
        OrgAdmission::OwnerDelegated,
        &caller_entity,
        &provider,
        org_b.org_id(),
        CALL_ID,
        digest,
        RpcCallShape::Unary,
        Some(bind_open),
        &floors,
    );
    let replay = AdmissionReplayGuard::with_defaults();
    assert_eq!(
        verify_org_admission(
            &ctx,
            &[&bytes],
            &replay,
            ClockSample::now(),
            || true,
            |_| true
        ),
        Err(AdmissionDenied::StreamingUnsupported),
    );
    assert_eq!(
        AdmissionDenied::StreamingUnsupported.coarse(),
        CoarseAdmissionReason::NotSupported,
        "the refusal must be typed NotSupported on the wire",
    );
}

/// §1.4 — the FROZEN old provider (`85ecc77c9`: `OrgCallProof`,
/// `verify_org_admission`, `serve_rpc_protected`, vendored verbatim in
/// [`frozen_85ecc77c9`]) refuses the NEW caller's stream-proof bytes with
/// the typed `NotSupported` — the prefix design holds. If the frozen path
/// yields anything else, the prefix design WITHDRAWS (§1.4: report, never
/// patch).
#[tokio::test]
async fn frozen_old_provider_refuses_stream_proof_with_not_supported() {
    use frozen_85ecc77c9::old_org_admission as old_adm;
    use frozen_85ecc77c9::{frozen_opening, FrozenProvider, FrozenYield};

    let server = fixture::build_node_with(EntityKeypair::from_bytes([0x99u8; 32])).await;
    let (org_b, _dir) = fixture::install_authority(&server, "frozen-old");
    let caller_kp = EntityKeypair::from_bytes([0x24u8; 32]);
    let provider = server.entity_id().clone();
    let intent =
        fixture::owner_delegated_intent(caller_kp.clone(), &org_b, provider.clone(), SERVICE);
    let (bind_open, _bind_other) = fixture::session_bindings();

    // The vendored `serve_rpc_protected` is EXECUTED: both frozen
    // registration gates and the happy registration.
    let handler = Arc::new(fixture::DarkHandler {
        calls: Arc::new(AtomicUsize::new(0)),
    });
    let policy: net::adapter::net::org_admission_gate::OrgProviderPolicy = Arc::new(|_| true);

    let no_authority = FrozenProvider::new(server.clone(), false);
    assert!(
        matches!(
            no_authority.serve_rpc_protected(
                SERVICE,
                handler.clone(),
                old_adm::OrgAdmission::OwnerDelegated,
                policy.clone(),
            ),
            Err(net::adapter::net::mesh_rpc::ServeError::ProtectedAuthorityRequired(_)),
        ),
        "frozen E1.1 gate: no authority ⇒ refused up front",
    );

    let frozen = FrozenProvider::new(server.clone(), true);
    assert!(
        matches!(
            frozen.serve_rpc_protected(
                SERVICE,
                handler.clone(),
                old_adm::OrgAdmission::PublicAuthenticated,
                policy.clone(),
            ),
            Err(net::adapter::net::mesh_rpc::ServeError::InvalidProtectedRegistration(_)),
        ),
        "frozen gate: PublicAuthenticated is refused by the vendored body",
    );
    let _handle = frozen
        .serve_rpc_protected(
            SERVICE,
            handler,
            old_adm::OrgAdmission::OwnerDelegated,
            policy,
        )
        .expect("the frozen provider registers a protected unary service");
    assert!(
        matches!(
            frozen.registration().as_ref(),
            Some((service, FrozenProviderAdm::Protected { .. })) if service.as_str() == SERVICE,
        ),
        "the vendored registration captured UnaryAdmission::Protected",
    );

    // The NEW caller's bytes: the real shared mint helper, streaming shape,
    // bound to the session.
    let (bytes, req) = mint_stream_opening(&intent, bind_open, CALL_ID);
    let digest = org_request_digest(&req).expect("request digest");
    let caller_entity = caller_kp.entity_id().clone();
    let server_entity = server.entity_id().clone();
    let floors = OrgRevocationState::empty();
    let frozen_ctx = old_adm::AdmissionContext {
        mode: old_adm::OrgAdmission::OwnerDelegated,
        authenticated_caller: &caller_entity,
        provider: &server_entity,
        provider_owner_org: org_b.org_id(),
        invoked_capability: cap(),
        call_id: CALL_ID,
        request_digest: digest,
        // Overwritten by the verbatim frozen flag derivation inside
        // `frozen_opening` (the frozen path owns this decision).
        is_unary: true,
        floors: &floors,
        skew_secs: 0,
    };

    let replay = AdmissionReplayGuard::with_defaults();
    match frozen_opening(
        &req,
        frozen_ctx,
        &[&bytes],
        &replay,
        ClockSample::now(),
        |_| true,
    ) {
        FrozenYield::Denied { denied, response } => {
            assert_eq!(
                denied,
                old_adm::AdmissionDenied::StreamingUnsupported,
                "the frozen path must carry the typed StreamingUnsupported refusal",
            );
            assert_eq!(
                denied.coarse(),
                old_adm::CoarseAdmissionReason::NotSupported,
                "the frozen coarse mapping must be typed NotSupported",
            );
            assert_eq!(response.status, RpcStatus::AdmissionDenied);
            assert_eq!(
                response.body.as_ref(),
                [old_adm::CoarseAdmissionReason::NotSupported.to_wire()].as_slice(),
                "the frozen wire body is exactly the NotSupported coarse byte",
            );
        }
        other => panic!(
            "STOP (§1.4): the frozen path yielded {:?}, not typed NotSupported — \
             the prefix design WITHDRAWS; report it, do not patch",
            yield_debug(&other),
        ),
    }
}

fn yield_debug(y: &frozen_85ecc77c9::FrozenYield) -> String {
    match y {
        frozen_85ecc77c9::FrozenYield::Admitted(a) => format!("Admitted({a:?})"),
        frozen_85ecc77c9::FrozenYield::Denied { denied, response } => {
            format!(
                "Denied({denied:?}, status {:?}, body {:?})",
                response.status, response.body
            )
        }
    }
}

/// §1.3 — an opening bound to session 1, replayed on session 2, is the typed
/// `SessionBindingMismatch` — NOT `Replay` — proving the session fence runs
/// before the replay insert.
#[test]
fn replayed_opening_on_new_session_is_session_binding_mismatch() {
    let org_b = OrgKeypair::from_bytes([0x42u8; 32]);
    let caller_kp = EntityKeypair::from_bytes([0x24u8; 32]);
    let provider = net::adapter::net::identity::EntityId::from_bytes([0x99u8; 32]);
    let intent =
        fixture::owner_delegated_intent(caller_kp.clone(), &org_b, provider.clone(), SERVICE);
    let (bind_s1, bind_s2) = fixture::session_bindings();

    // The opening is minted against session 1's binding.
    let (bytes, req) = mint_stream_opening(&intent, bind_s1, CALL_ID);
    let digest = org_request_digest(&req).expect("request digest");
    let floors = OrgRevocationState::empty();
    let caller_entity = caller_kp.entity_id().clone();
    let replay = AdmissionReplayGuard::with_defaults();

    // First arrival on session 1: admitted.
    let ctx_s1 = stream_ctx(
        OrgAdmission::OwnerDelegated,
        &caller_entity,
        &provider,
        org_b.org_id(),
        CALL_ID,
        digest,
        RpcCallShape::ServerStreaming,
        Some(bind_s1),
        &floors,
    );
    verify_org_admission(
        &ctx_s1,
        &[&bytes],
        &replay,
        ClockSample::now(),
        || true,
        |_| true,
    )
    .expect("the opening admits on the session it binds");

    // The SAME frame on the REPLACED session (session 2's binding): the
    // session fence fires BEFORE the replay insert — the typed reason is
    // SessionBindingMismatch, not Replay.
    let ctx_s2 = stream_ctx(
        OrgAdmission::OwnerDelegated,
        &caller_entity,
        &provider,
        org_b.org_id(),
        CALL_ID,
        digest,
        RpcCallShape::ServerStreaming,
        Some(bind_s2),
        &floors,
    );
    assert_eq!(
        verify_org_admission(
            &ctx_s2,
            &[&bytes],
            &replay,
            ClockSample::now(),
            || true,
            |_| true
        ),
        Err(AdmissionDenied::SessionBindingMismatch),
        "a replayed opening on a new session must surface SessionBindingMismatch, not Replay",
    );
}
