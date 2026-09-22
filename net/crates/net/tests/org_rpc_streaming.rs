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
#[path = "org_rpc_streaming/s13.rs"]
mod s13;
#[path = "org_rpc_streaming/s14.rs"]
mod s14;
#[path = "org_rpc_streaming/s15.rs"]
mod s15;

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
#[expect(
    clippy::too_many_arguments,
    reason = "test helper assembling the full AdmissionContext; a params struct would only \
              rename the arguments and hide which verified facts the caller must supply"
)]
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

// ============================================================================
// Slice 1.3 — fold ownership + lifetime (contract 4, §2.1/§2.2/§2.6/§2.8,
// ledger C5–C9). The six NAMED witnesses of the 1.3 acceptance list plus
// three regression witnesses for the observable Q3 changes (C6, C7) and the
// §2.1 credential clamp. Unit-witness idiom: the folds are driven directly
// (or through a REAL `ServeHandle`) and every effect is observed at the
// fold's maps, the protected records' handles, and the emitter seam.
// ============================================================================

use net::adapter::net::cortex::rpc::{
    RpcServerStreamingFold, RpcStreamingRequestFold, StreamDeadlineBound, StreamLifetimePolicy,
    StreamTerminalReason,
};
use std::sync::atomic::AtomicBool;
use std::time::Duration;

/// C5 — a late chunk from a REPLACED session (same node, same wire
/// origin, same call id) misses the map entirely: the stream sees
/// nothing, and only the live incarnation's chunks reach the handler.
/// The 4-tuple key shape is observed at `sender_keys()`/`in_flight_keys()`.
#[tokio::test]
async fn late_chunk_from_replaced_session_is_dropped() {
    const NODE: u64 = 0xA;
    const SESSION_A: u64 = 0x51;
    const SESSION_B: u64 = 0x52; // the REPLACED session
    const ORIGIN: u64 = 0x1111;
    const CALL: u64 = 42;

    let collected = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let (emit, captured) = s13::capturing_emitter();
    let mut fold = RpcStreamingRequestFold::new(
        Arc::new(s13::CollectBodies {
            collected: Arc::clone(&collected),
        }),
        emit,
    );

    // The call opens on session A.
    let open = s13::cs_request("svc", 0, b"open");
    fold.apply_inbound(&s13::inbound(
        SESSION_A,
        NODE,
        ORIGIN,
        s13::request_frame(ORIGIN, CALL, &open),
    ))
    .unwrap();

    // A LATE chunk from the REPLACED session B — same (node, origin,
    // call). It must miss the map: no delivery, no state change.
    let late = s13::chunk_payload(CALL, b"late", false);
    fold.apply_inbound(&s13::inbound(
        SESSION_B,
        NODE,
        ORIGIN,
        s13::chunk_frame(ORIGIN, CALL, &late),
    ))
    .unwrap();

    // The live session's own final chunk + END.
    let live = s13::chunk_payload(CALL, b"live", true);
    fold.apply_inbound(&s13::inbound(
        SESSION_A,
        NODE,
        ORIGIN,
        s13::chunk_frame(ORIGIN, CALL, &live),
    ))
    .unwrap();

    // THE frame-admission observation: the handler's stream holds ONLY
    // the live incarnation's bodies.
    assert!(
        s13::wait_for(Duration::from_secs(10), || { collected.lock().len() >= 2 }).await,
        "the live call must complete within the bound",
    );
    assert_eq!(
        collected
            .lock()
            .iter()
            .map(|b| b.to_vec())
            .collect::<Vec<_>>(),
        vec![b"open".to_vec(), b"live".to_vec()],
        "the handler must see ONLY the live incarnation's chunks — the replaced \
         session's late chunk must be dropped, not admitted into the stream",
    );
    assert!(
        s13::wait_for(Duration::from_secs(10), || !captured.lock().is_empty()).await,
        "the call's terminal must land within the bound",
    );
    {
        let frames = captured.lock().clone();
        assert_eq!(frames.len(), 1, "exactly one terminal response");
        assert_eq!(
            frames[0].body.as_ref(),
            b"openlive".as_slice(),
            "the terminal body joins only the live incarnation's chunks",
        );
    }
    assert!(fold.in_flight_keys().is_empty());
    assert!(fold.sender_keys().is_empty());

    // C5's key SHAPE, observed on a fresh live call: the maps are keyed
    // by the four-part `(from_node, session_id, origin, call_id)`.
    let open2 = s13::cs_request("svc", 0, b"open");
    fold.apply_inbound(&s13::inbound(
        SESSION_A,
        NODE,
        ORIGIN,
        s13::request_frame(ORIGIN, CALL + 1, &open2),
    ))
    .unwrap();
    let live_key: s13::CallKey = (NODE, SESSION_A, ORIGIN, CALL + 1);
    assert_eq!(
        fold.sender_keys(),
        vec![live_key],
        "C5: the upload sender is keyed by (from_node, session_id, origin, call_id)",
    );
    assert!(fold.in_flight_keys().contains(&live_key));
    fold.apply_inbound(&s13::inbound(
        SESSION_A,
        NODE,
        ORIGIN,
        s13::cancel_frame(ORIGIN, CALL + 1),
    ))
    .unwrap();
}

/// §2.1 bound 1 — an OMITTED deadline is filled by the provider default
/// (Q1: 300 s, exact), and an idle protected stream EXPIRES at that
/// deadline with a typed `Timeout` terminal (the default never caps an
/// explicit request — see its sibling witness).
#[tokio::test]
async fn omitted_deadline_gets_default_and_expires_idle() {
    // Part 1 — the Q1 default fills an omitted deadline, exactly.
    let (emit, _captured) = s13::capturing_async_emitter();
    let dropped = Arc::new(AtomicBool::new(false));
    let started = Arc::new(AtomicUsize::new(0));
    let mut fold = RpcServerStreamingFold::new(
        Arc::new(s13::ParkForever {
            dropped: Arc::clone(&dropped),
            started: Arc::clone(&started),
        }),
        emit,
    );
    let clock = ClockSample::now();
    let lifetime = s13::lifetime(StreamLifetimePolicy::q1_defaults(), &[], clock);
    let open = s13::ss_request("svc", 0, b"open"); // deadline_ns == 0 ⇒ omitted
    let call = fold
        .apply_inbound_admitted(
            &s13::inbound(0x51, 0xA, 0x1111, s13::request_frame(0x1111, 42, &open)),
            s13::synthetic_admitted(),
            &lifetime,
            None,
        )
        .expect("an omitted deadline is filled by the default, never refused");
    assert_eq!(
        call.deadline_end_ns(),
        clock.wall_ns + 300 * s13::SEC,
        "omitted ⇒ exactly the Q1 300 s provider default",
    );
    assert_eq!(call.deadline_bound(), StreamDeadlineBound::Deadline);
    call.retire(StreamTerminalReason::Cancelled); // clean up the parked record

    // Part 2 — an idle protected stream expires at its (tiny) default
    // deadline with one `Timeout` terminal, and the handler future is
    // dropped. Fresh handler state: the flags below belong to THIS
    // record only.
    let dropped = Arc::new(AtomicBool::new(false));
    let started = Arc::new(AtomicUsize::new(0));
    let (emit, captured) = s13::capturing_async_emitter();
    let mut fold = RpcServerStreamingFold::new(
        Arc::new(s13::ParkForever {
            dropped: Arc::clone(&dropped),
            started: Arc::clone(&started),
        }),
        emit,
    );
    let lifetime = s13::lifetime(s13::tiny_policy(), &[], ClockSample::now());
    let open = s13::ss_request("svc", 0, b"open");
    let call = fold
        .apply_inbound_admitted(
            &s13::inbound(0x51, 0xA, 0x1111, s13::request_frame(0x1111, 43, &open)),
            s13::synthetic_admitted(),
            &lifetime,
            None,
        )
        .expect("admits");
    assert!(
        s13::wait_for(Duration::from_secs(10), || {
            started.load(std::sync::atomic::Ordering::SeqCst) == 1
        })
        .await,
        "the handler must be entered before expiry is observed",
    );
    assert!(
        s13::wait_for(Duration::from_secs(30), || {
            matches!(call.terminal(), Some(StreamTerminalReason::Timeout))
        })
        .await,
        "an idle call must expire at the resolved default with a Timeout terminal",
    );
    assert!(
        s13::wait_for(Duration::from_secs(10), || {
            call.emission().is_some() && captured.lock().len() == 1
        })
        .await,
        "the terminal is handed off exactly once, after pump stop",
    );
    let frames = captured.lock().clone();
    assert_eq!(frames.len(), 1, "exactly one terminal frame");
    assert_eq!(frames[0].status, RpcStatus::Timeout);
    assert!(
        s13::wait_for(Duration::from_secs(10), || {
            dropped.load(std::sync::atomic::Ordering::SeqCst)
        })
        .await,
        "retirement dropped the handler future",
    );
    assert!(fold.in_flight_keys().is_empty());
}

/// §2.1 bound 2 — an EXPLICIT deadline over `max_live` (Q1: 3600 s) is
/// REFUSED, never clamped, with the typed `DeadlineExceedsPolicy`, and
/// with ZERO effects: no handler, no in-flight entry, no protected
/// record, no emitted frame.
#[tokio::test]
async fn requested_deadline_over_cap_is_refused_with_zero_effects() {
    let calls = Arc::new(AtomicUsize::new(0));
    let (emit, captured) = s13::capturing_async_emitter();
    let mut fold = RpcServerStreamingFold::new(
        Arc::new(s13::CountingStream {
            calls: Arc::clone(&calls),
        }),
        emit,
    );
    let clock = ClockSample::now();
    let lifetime = s13::lifetime(StreamLifetimePolicy::q1_defaults(), &[], clock);
    let requested_end = clock.wall_ns + 7200 * s13::SEC; // over the 3600 s cap
    let open = s13::ss_request("svc", requested_end, b"open");
    let refused = fold
        .apply_inbound_admitted(
            &s13::inbound(0x51, 0xA, 0x1111, s13::request_frame(0x1111, 42, &open)),
            s13::synthetic_admitted(),
            &lifetime,
            None,
        )
        .expect_err("an explicit deadline over the provider cap must be refused");
    assert!(
        matches!(refused, AdmissionDenied::DeadlineExceedsPolicy),
        "an explicit deadline over the provider cap is refused with DeadlineExceedsPolicy, got {refused:?}",
    );
    // Zero effects — the plan's strong reading: handler entry AND fold
    // state AND emission.
    assert!(
        fold.in_flight_keys().is_empty(),
        "a refused opening registers no call",
    );
    assert!(
        fold.protected_owners().is_empty(),
        "a refused opening owns no record",
    );
    assert!(
        captured.lock().is_empty(),
        "the fold emits nothing on refusal (the bridge owes exactly one bounded denial)",
    );
    fixture::assert_handler_stays_dark(&calls, "an over-cap deadline refusal").await;
}

/// §2.1 bound 1 (complement) — an explicit deadline WITHIN the cap is
/// honoured VERBATIM: a 900 s request stays 900 s and is never silently
/// clamped to the 300 s default.
#[tokio::test]
async fn requested_deadline_within_cap_is_honoured() {
    let (emit, _captured) = s13::capturing_async_emitter();
    let dropped = Arc::new(AtomicBool::new(false));
    let started = Arc::new(AtomicUsize::new(0));
    let mut fold = RpcServerStreamingFold::new(
        Arc::new(s13::ParkForever {
            dropped: Arc::clone(&dropped),
            started: Arc::clone(&started),
        }),
        emit,
    );
    let clock = ClockSample::now();
    let lifetime = s13::lifetime(StreamLifetimePolicy::q1_defaults(), &[], clock);
    let requested_end = clock.wall_ns + 900 * s13::SEC; // within the 3600 s cap
    let open = s13::ss_request("svc", requested_end, b"open");
    let call = fold
        .apply_inbound_admitted(
            &s13::inbound(0x51, 0xA, 0x1111, s13::request_frame(0x1111, 42, &open)),
            s13::synthetic_admitted(),
            &lifetime,
            None,
        )
        .expect("a within-cap explicit deadline must be admitted");
    assert_eq!(
        call.deadline_end_ns(),
        requested_end,
        "the requested 900 s end is honoured verbatim",
    );
    assert_ne!(
        call.deadline_end_ns(),
        clock.wall_ns + 300 * s13::SEC,
        "the provider default must NEVER cap an explicit request",
    );
    assert_eq!(call.deadline_bound(), StreamDeadlineBound::Deadline);
    call.retire(StreamTerminalReason::Cancelled); // clean up the parked record
}

/// §2.2 — a pump parked on a zero-credit semaphore is retired at the
/// deadline: producer finished is NOT terminal while the drain is
/// blocked, the semaphore is closed and the pump aborted + joined, the
/// queued chunks are discarded, and EXACTLY ONE terminal (`Timeout`)
/// lands after pump stop.
#[tokio::test]
async fn pump_parked_on_zero_credit_is_retired_at_deadline_with_one_terminal() {
    let returned = Arc::new(AtomicUsize::new(0));
    let (emit, captured) = s13::capturing_async_emitter();
    let mut fold = RpcServerStreamingFold::new(
        Arc::new(s13::EmitAndReturn {
            chunks: vec![b"a", b"b"],
            returned: Arc::clone(&returned),
        }),
        emit,
    );
    // A 1.5 s default: room to observe "not terminal after the handler
    // returned", short enough to expire well within the wait bounds.
    let policy = StreamLifetimePolicy {
        default_live_ns: 1500 * 1_000_000,
        max_live_ns: 30 * s13::SEC,
    };
    let lifetime = s13::lifetime(policy, &[], ClockSample::now());
    // Zero initial credit: the pump parks in `acquire` on the first chunk.
    let open = s13::ss_request_windowed("svc", 0, 0);
    let call = fold
        .apply_inbound_admitted(
            &s13::inbound(0x51, 0xA, 0x1111, s13::request_frame(0x1111, 42, &open)),
            s13::synthetic_admitted(),
            &lifetime,
            None,
        )
        .expect("admits");

    // The handler finished and queued both chunks under zero credit.
    assert!(
        s13::wait_for(Duration::from_secs(10), || returned
            .load(std::sync::atomic::Ordering::SeqCst)
            == 1)
        .await,
        "the handler must run and return",
    );
    assert!(
        call.is_live(),
        "producer finished is NOT terminal (§2.2) — the credit-blocked drain is owned, not abandoned",
    );
    assert_eq!(
        captured.lock().len(),
        0,
        "zero credit ⇒ nothing was published",
    );

    // The deadline retires the call (§2.2's order) and the terminal is
    // typed `Timeout` — one terminal, after pump stop, queue discarded.
    assert!(
        s13::wait_for(Duration::from_secs(30), || {
            matches!(call.terminal(), Some(StreamTerminalReason::Timeout))
        })
        .await,
        "the parked pump must be retired at the deadline with a Timeout terminal",
    );
    assert!(
        s13::wait_for(Duration::from_secs(10), || {
            call.emission().is_some() && captured.lock().len() == 1
        })
        .await,
        "the terminal is handed off exactly once, after pump stop",
    );
    {
        let frames = captured.lock().clone();
        assert_eq!(
            frames.len(),
            1,
            "exactly one terminal frame — the queued chunks are discarded on Timeout",
        );
        assert_eq!(frames[0].status, RpcStatus::Timeout);
    }
    assert!(fold.in_flight_keys().is_empty(), "ownership released");
    // …and it stays exactly one terminal.
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert_eq!(captured.lock().len(), 1, "exactly one terminal, ever",);
}

/// Q3/C9 — dropping a `ServeHandle` retires that registration's live
/// PROTECTED streams (`ServeHandleDropped`), while a SIBLING
/// registration's stream survives and completes normally.
#[tokio::test]
async fn serve_handle_drop_retires_live_stream_and_sibling_survives() {
    let node = fixture::build_node_with(EntityKeypair::from_bytes([0x61u8; 32])).await;
    let release_a = Arc::new(tokio::sync::Notify::new());
    let release_b = Arc::new(tokio::sync::Notify::new());
    let dropped_a = Arc::new(AtomicBool::new(false));
    let dropped_b = Arc::new(AtomicBool::new(false));
    let ran_a = Arc::new(AtomicUsize::new(0));
    let ran_b = Arc::new(AtomicUsize::new(0));

    let handle_a = node
        .serve_rpc_streaming(
            "svc.a",
            Arc::new(s13::ParkUntilReleased {
                release: Arc::clone(&release_a),
                dropped: Arc::clone(&dropped_a),
                ran: Arc::clone(&ran_a),
            }),
        )
        .expect("register svc.a");
    let handle_b = node
        .serve_rpc_streaming(
            "svc.b",
            Arc::new(s13::ParkUntilReleased {
                release: Arc::clone(&release_b),
                dropped: Arc::clone(&dropped_b),
                ran: Arc::clone(&ran_b),
            }),
        )
        .expect("register svc.b");
    let fold_a = handle_a
        .streaming_fold_for_test()
        .expect("the SS fold is reachable through the handle");
    let fold_b = handle_b.streaming_fold_for_test().unwrap();

    // Two live protected records, one per registration.
    let lifetime = s13::lifetime(StreamLifetimePolicy::q1_defaults(), &[], ClockSample::now());
    let open_a = s13::ss_request("svc.a", 0, b"open");
    let call_a = fold_a
        .lock()
        .apply_inbound_admitted(
            &s13::inbound(0x51, 0xA, 0x1111, s13::request_frame(0x1111, 42, &open_a)),
            s13::synthetic_admitted(),
            &lifetime,
            None,
        )
        .expect("admitted a");
    let open_b = s13::ss_request("svc.b", 0, b"open");
    let call_b = fold_b
        .lock()
        .apply_inbound_admitted(
            &s13::inbound(0x51, 0xA, 0x1111, s13::request_frame(0x1111, 43, &open_b)),
            s13::synthetic_admitted(),
            &lifetime,
            None,
        )
        .expect("admitted b");
    assert!(call_a.is_live() && call_b.is_live());
    assert_eq!(
        fold_a.lock().in_flight_keys(),
        vec![(0xA, 0x51, 0x1111, 42)]
    );
    assert_eq!(
        fold_b.lock().in_flight_keys(),
        vec![(0xA, 0x51, 0x1111, 43)]
    );
    // Both handler futures must be POLLED before the drop: a retire that
    // wins before the supervisor's first poll never enters the handler
    // at all (§3 — zero handler effects), which would make the drop
    // flags below vacuous.
    assert!(
        s13::wait_for(Duration::from_secs(10), || {
            ran_a.load(std::sync::atomic::Ordering::SeqCst) == 1
                && ran_b.load(std::sync::atomic::Ordering::SeqCst) == 1
        })
        .await,
        "both handlers must be entered before the drop",
    );

    // Drop ONE ServeHandle: only ITS registration's protected records
    // retire (Q3: protected-only on handle drop).
    drop(handle_a);
    assert!(
        s13::wait_for(Duration::from_secs(30), || {
            matches!(
                call_a.terminal(),
                Some(StreamTerminalReason::ServeHandleDropped)
            )
        })
        .await,
        "the dropped registration's stream must retire with ServeHandleDropped",
    );
    assert!(
        s13::wait_for(Duration::from_secs(10), || {
            fold_a.lock().in_flight_keys().is_empty()
        })
        .await,
        "the retired call's state is reclaimed",
    );
    assert!(
        s13::wait_for(Duration::from_secs(10), || {
            dropped_a.load(std::sync::atomic::Ordering::SeqCst)
        })
        .await,
        "retirement dropped the handler future",
    );

    // The SIBLING is untouched and completes normally afterwards.
    assert!(
        call_b.is_live(),
        "the sibling's stream must survive the drop"
    );
    assert_eq!(
        fold_b.lock().in_flight_keys(),
        vec![(0xA, 0x51, 0x1111, 43)]
    );
    assert_eq!(ran_b.load(std::sync::atomic::Ordering::SeqCst), 1);
    release_b.notify_one();
    assert!(
        s13::wait_for(Duration::from_secs(10), || {
            matches!(call_b.terminal(), Some(StreamTerminalReason::Completed(_)))
        })
        .await,
        "the sibling completes normally",
    );
    drop(handle_b);
}

/// §2.1 bound 3 — credential validity CLAMPS an otherwise-legal request
/// and the expiry terminal is an authority lapse
/// (`AdmissionDenied(Denied)`), never a plain `Timeout`; an exact tie
/// goes to the credential bound.
#[tokio::test]
async fn credential_clamp_expiry_is_admission_denied_not_timeout() {
    let dropped = Arc::new(AtomicBool::new(false));
    let started = Arc::new(AtomicUsize::new(0));
    let (emit, captured) = s13::capturing_async_emitter();
    let mut fold = RpcServerStreamingFold::new(
        Arc::new(s13::ParkForever {
            dropped: Arc::clone(&dropped),
            started: Arc::clone(&started),
        }),
        emit,
    );
    let clock = ClockSample::now();
    let policy = StreamLifetimePolicy {
        default_live_ns: 400 * 1_000_000,
        max_live_ns: 30 * s13::SEC,
    };
    // Requested 20 s, credential validity ends at +600 ms: the clamp wins.
    let credential_end = clock.wall_ns + 600 * 1_000_000;
    let ends = [Some(credential_end)];
    let lifetime = s13::lifetime(policy, &ends, clock);
    let open = s13::ss_request("svc", clock.wall_ns + 20 * s13::SEC, b"open");
    let call = fold
        .apply_inbound_admitted(
            &s13::inbound(0x51, 0xA, 0x1111, s13::request_frame(0x1111, 42, &open)),
            s13::synthetic_admitted(),
            &lifetime,
            None,
        )
        .expect("admits (the clamp is not a refusal)");
    assert_eq!(
        call.deadline_end_ns(),
        credential_end,
        "credential validity clamps"
    );
    assert_eq!(call.deadline_bound(), StreamDeadlineBound::Credential);
    assert!(
        s13::wait_for(Duration::from_secs(30), || {
            matches!(
                call.terminal(),
                Some(StreamTerminalReason::CredentialExpired)
            )
        })
        .await,
        "the clamp's expiry is an authority lapse, not a timeout",
    );
    assert!(
        s13::wait_for(Duration::from_secs(10), || captured.lock().len() == 1).await,
        "exactly one terminal frame",
    );
    let frames = captured.lock().clone();
    assert_eq!(frames.len(), 1);
    assert_eq!(
        frames[0].status,
        RpcStatus::AdmissionDenied,
        "credential expiry emits AdmissionDenied(Denied), never Timeout",
    );
    assert_eq!(
        frames[0].body.as_ref(),
        [0u8].as_slice(),
        "the frozen coarse byte set: 0 = Denied",
    );

    // The exact tie goes to the credential bound (§2.1: at that instant
    // the authority is gone; a plain timeout would understate it).
    let (emit2, _captured2) = s13::capturing_async_emitter();
    let mut fold2 = RpcServerStreamingFold::new(
        Arc::new(s13::ParkForever {
            dropped: Arc::clone(&dropped),
            started: Arc::clone(&started),
        }),
        emit2,
    );
    let clock = ClockSample::now();
    let requested_end = clock.wall_ns + 20 * s13::SEC;
    let tie = [Some(requested_end)];
    let lifetime = s13::lifetime(policy, &tie, clock);
    let open = s13::ss_request("svc", requested_end, b"open");
    let call2 = fold2
        .apply_inbound_admitted(
            &s13::inbound(0x51, 0xA, 0x1111, s13::request_frame(0x1111, 43, &open)),
            s13::synthetic_admitted(),
            &lifetime,
            None,
        )
        .expect("admits");
    assert_eq!(
        call2.deadline_bound(),
        StreamDeadlineBound::Credential,
        "an exact tie reports credential expiry, not timeout",
    );
    call2.retire(StreamTerminalReason::Cancelled); // clean up
}

/// C6 + C7 (Q3 shared repairs, public SS) — a PUBLIC server-streaming
/// call with a NONZERO `deadline_ns` is now enforced: the handler is
/// stopped at the deadline and the terminal is typed `Timeout`
/// (`deadline_ns == 0` keeps meaning "no deadline" — pinned by the
/// unchanged estate).
#[tokio::test]
async fn public_ss_nonzero_deadline_expires_with_typed_timeout() {
    let dropped = Arc::new(AtomicBool::new(false));
    let started = Arc::new(AtomicUsize::new(0));
    let (emit, captured) = s13::capturing_async_emitter();
    let mut fold = RpcServerStreamingFold::new(
        Arc::new(s13::ParkForever {
            dropped: Arc::clone(&dropped),
            started: Arc::clone(&started),
        }),
        emit,
    );
    let wall_now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let open = s13::ss_request("svc", wall_now + 400 * 1_000_000, b"open");
    fold.apply_inbound(&s13::inbound(
        0x51,
        0xA,
        0x1111,
        s13::request_frame(0x1111, 42, &open),
    ))
    .unwrap();
    assert!(
        s13::wait_for(Duration::from_secs(10), || {
            started.load(std::sync::atomic::Ordering::SeqCst) == 1
        })
        .await,
        "the handler must be entered before expiry is observed",
    );
    assert!(
        s13::wait_for(Duration::from_secs(30), || {
            captured
                .lock()
                .first()
                .is_some_and(|f| f.status == RpcStatus::Timeout)
        })
        .await,
        "a public SS handler that ignores its expired deadline must stop with a Timeout terminal",
    );
    let frames = captured.lock().clone();
    assert_eq!(frames.len(), 1, "exactly one terminal");
    assert!(
        s13::wait_for(Duration::from_secs(10), || {
            dropped.load(std::sync::atomic::Ordering::SeqCst)
        })
        .await,
        "the handler future was stopped at the deadline",
    );
    assert!(fold.in_flight_keys().is_empty());
}

/// C7 (Q3 shared repair, public client-streaming) — a deadline expiry's
/// terminal is typed `Timeout` (the pre-C7 source classified it through
/// the CANCEL-wins override).
#[tokio::test]
async fn public_client_stream_deadline_expiry_is_typed_timeout() {
    let dropped = Arc::new(AtomicBool::new(false));
    let started = Arc::new(AtomicUsize::new(0));
    let (emit, captured) = s13::capturing_emitter();
    let mut fold = RpcStreamingRequestFold::new(
        Arc::new(s13::CsParkForever {
            dropped: Arc::clone(&dropped),
            started: Arc::clone(&started),
        }),
        emit,
    );
    let wall_now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let open = s13::cs_request("svc", wall_now + 400 * 1_000_000, b"open");
    fold.apply_inbound(&s13::inbound(
        0x51,
        0xA,
        0x1111,
        s13::request_frame(0x1111, 42, &open),
    ))
    .unwrap();
    assert!(
        s13::wait_for(Duration::from_secs(10), || {
            started.load(std::sync::atomic::Ordering::SeqCst) == 1
        })
        .await,
        "the handler must be entered before expiry is observed",
    );
    assert!(
        s13::wait_for(Duration::from_secs(30), || {
            captured
                .lock()
                .first()
                .is_some_and(|f| f.status == RpcStatus::Timeout)
        })
        .await,
        "a client-streaming deadline expiry must be typed Timeout, not Internal/Cancelled",
    );
    let frames = captured.lock().clone();
    assert_eq!(frames.len(), 1, "exactly one terminal");
    assert!(
        s13::wait_for(Duration::from_secs(10), || {
            dropped.load(std::sync::atomic::Ordering::SeqCst)
        })
        .await,
        "the handler future was stopped at the deadline",
    );
}

// ============================================================================
// Slice 1.4 — registry + revocation (contract 3, §2.3/§2.7/§3, ledger Q1/Q4).
// The nine NAMED witnesses drive the PRODUCTION seams — the shared §3
// helper (`admit_protected_opening`), the registry's reserve/install where
// the interleaving is the property, and the lease-carrying fold seam —
// against REAL `OrgRevocationStore` fixtures (AV-9: scratch dirs left
// behind). Retirement is observed at `retire_reason()` (the synchronous
// §2.3 boundary) and at the supervisor's `terminal()`.
// ============================================================================

use bytes::Bytes;
use net::adapter::net::behavior::org::OrgRevocationBundle;
use net::adapter::net::cortex::rpc::{
    CommitVerdict, OpeningRequest, RegistryPhase, SessionIdentity, VerifiedCallFacts,
};
use net::adapter::net::mesh_rpc::OpeningRefusal;
use std::collections::BTreeMap;

/// §2.3's quantified floor-raise boundary + the generation-only
/// requalification's victim: a raise for THIS member retires its blocked
/// stream BEFORE `apply_bundle` returns to its caller, with one typed
/// `Revoked` terminal.
#[tokio::test]
async fn floor_raise_retires_blocked_stream_before_publish_returns() {
    let server = fixture::build_node_with(EntityKeypair::from_bytes([0x61u8; 32])).await;
    let (org_b, _auth, _dir) = s14::install_authority_owned(&server, "s14-w1");
    let caller_kp = EntityKeypair::from_bytes([0x24u8; 32]);
    let intent = fixture::owner_delegated_intent_gen(
        caller_kp.clone(),
        &org_b,
        server.entity_id().clone(),
        s14::SERVICE,
        1,
    );
    let reg = s14::owner_reg(1);
    let replay = AdmissionReplayGuard::with_defaults();
    let session = s14::synthetic_session();
    let clock = ClockSample::now();
    let (frame, admitted, lease) = s14::admit_ss(
        &server,
        &intent,
        &reg,
        &replay,
        clock,
        42,
        0x1111,
        &session,
        [0xA1u8; 32],
        Some(0), // zero initial credit — the pump parks: a BLOCKED stream
        b"open",
    )
    .expect("the opening admits");

    let returned = Arc::new(AtomicUsize::new(0));
    let dropped = Arc::new(AtomicBool::new(false));
    let (emit, captured) = s13::capturing_async_emitter();
    let mut fold = RpcServerStreamingFold::new(
        Arc::new(s14::QueueChunksAndReturn {
            chunks: vec![b"a", b"b"],
            returned: Arc::clone(&returned),
            dropped: Arc::clone(&dropped),
        }),
        emit,
    );
    let lifetime = s13::lifetime(StreamLifetimePolicy::q1_defaults(), &[], clock);
    let call = s14::open_ss_call(
        &mut fold, &frame, admitted, lease, &session, 0x1111, &lifetime,
    );
    assert!(
        s13::wait_for(Duration::from_secs(10), || returned
            .load(std::sync::atomic::Ordering::SeqCst)
            == 1)
        .await,
        "the handler must queue its chunks",
    );
    assert!(
        call.is_live(),
        "the credit-blocked drain is owned, not abandoned (§2.2)",
    );

    // The floor for THIS member (generation 1 → floor 2) publishes through
    // the REAL store — every subscriber runs synchronously inside
    // `apply_bundle`, before it returns.
    s14::raise_floor(
        &server.org_revocation_store().expect("installed store"),
        &org_b,
        caller_kp.entity_id().clone(),
        2,
    );
    let retired_at_publish_return = call.retire_reason();
    assert!(
        s13::wait_for(Duration::from_secs(30), || call.retire_reason().is_some()).await,
        "the blocked stream must be retired by the raise",
    );
    assert_eq!(
        retired_at_publish_return,
        Some(StreamTerminalReason::Revoked),
        "the retirement landed BEFORE the publication returned to its caller",
    );
    s14::assert_terminal(&call, StreamTerminalReason::Revoked, "floor raise").await;
    assert!(
        s13::wait_for(Duration::from_secs(10), || {
            call.emission().is_some() && captured.lock().len() == 1
        })
        .await,
        "exactly one terminal after pump stop",
    );
    {
        let frames = captured.lock().clone();
        assert_eq!(frames.len(), 1, "exactly one terminal frame");
        assert_eq!(
            frames[0].status,
            RpcStatus::AdmissionDenied,
            "a Revoked stream emits AdmissionDenied",
        );
        assert_eq!(
            frames[0].body.as_ref(),
            [0u8].as_slice(),
            "the frozen coarse byte set: 0 = Denied",
        );
    }
    assert!(
        s13::wait_for(Duration::from_secs(10), || dropped
            .load(std::sync::atomic::Ordering::SeqCst))
        .await,
        "retirement dropped the handler future",
    );
    assert!(fold.in_flight_keys().is_empty(), "ownership released");
}

/// §2.3's requalification (never whole-stamp equality): a floor raise for
/// one member moves EVERY captured stamp's generation, and a SIBLING
/// stream of ANOTHER org survives it — its next item commits after the
/// publication, with its captured generation refreshed.
#[tokio::test]
async fn sibling_stream_of_other_org_sends_next_item_after_publication() {
    let server = fixture::build_node_with(EntityKeypair::from_bytes([0x62u8; 32])).await;
    let (org_b, _auth, _dir) = s14::install_authority_owned(&server, "s14-w2");
    let org_a = net::adapter::net::behavior::org::OrgKeypair::from_bytes([0x7Au8; 32]);
    let provider = server.entity_id().clone();
    // The raised victim: an org-B member (generation 1).
    let caller_b = EntityKeypair::from_bytes([0x25u8; 32]);
    let intent_b = fixture::owner_delegated_intent_gen(
        caller_b.clone(),
        &org_b,
        provider.clone(),
        s14::SERVICE,
        1,
    );
    // The sibling of ANOTHER org: an org-A member with a B→A INVOKE grant.
    let caller_a = EntityKeypair::from_bytes([0x26u8; 32]);
    let intent_a = fixture::cross_org_intent(
        caller_a.clone(),
        &org_a,
        &org_b,
        provider.clone(),
        s14::SERVICE,
    );
    let reg_b = s14::owner_reg(1);
    let reg_a = s14::granted_reg(2);
    let replay = AdmissionReplayGuard::with_defaults();
    let session = s14::synthetic_session();
    let clock = ClockSample::now();

    let (frame_b, admitted_b, lease_b) = s14::admit_ss(
        &server,
        &intent_b,
        &reg_b,
        &replay,
        clock,
        42,
        0x1111,
        &session,
        [0xA1u8; 32],
        None,
        b"open",
    )
    .expect("the org-B opening admits");
    let (frame_a, admitted_a, lease_a) = s14::admit_ss(
        &server,
        &intent_a,
        &reg_a,
        &replay,
        clock,
        43,
        0x1112,
        &session,
        [0xA1u8; 32],
        None,
        b"open",
    )
    .expect("the cross-org opening admits");
    let key_a = s14::call_key(caller_a.entity_id(), 43);
    let incarnation_a = lease_a.incarnation;
    let registry = s14::registry_of(&server);

    let holder_b = Arc::new(s14::SinkHolder::new());
    let holder_a = Arc::new(s14::SinkHolder::new());
    let (emit_b, _cap_b) = s13::capturing_async_emitter();
    let (emit_a, captured_a) = s13::capturing_async_emitter();
    let mut fold_b = RpcServerStreamingFold::new(holder_b.clone(), emit_b);
    let mut fold_a = RpcServerStreamingFold::new(holder_a.clone(), emit_a);
    let lifetime = s13::lifetime(StreamLifetimePolicy::q1_defaults(), &[], clock);
    let call_b = s14::open_ss_call(
        &mut fold_b,
        &frame_b,
        admitted_b,
        lease_b,
        &session,
        0x1111,
        &lifetime,
    );
    let call_a = s14::open_ss_call(
        &mut fold_a,
        &frame_a,
        admitted_a,
        lease_a,
        &session,
        0x1112,
        &lifetime,
    );
    assert!(
        s13::wait_for(Duration::from_secs(10), || holder_a
            .started
            .load(std::sync::atomic::Ordering::SeqCst)
            == 1
            && holder_b.started.load(std::sync::atomic::Ordering::SeqCst) == 1)
        .await,
        "both handlers entered",
    );
    let sink_a = holder_a.sink.lock().take().expect("the sibling's sink");
    sink_a
        .send_wait(Bytes::from_static(b"one"))
        .await
        .expect("item 1 queues before the publication");
    let stamp_before = registry
        .captured_view(&key_a)
        .expect("the sibling's captured view");

    // The raise for (org_b, caller_b): BOTH captured stamps' generation
    // moves — only the victim may retire.
    s14::raise_floor(
        &server.org_revocation_store().expect("installed store"),
        &org_b,
        caller_b.entity_id().clone(),
        2,
    );
    assert_eq!(
        call_b.retire_reason(),
        Some(StreamTerminalReason::Revoked),
        "the raised member's stream retires at the publication boundary",
    );
    assert!(
        call_a.retire_reason().is_none(),
        "the SIBLING of another org must NOT retire on a foreign raise",
    );

    // …and the sibling SENDS ITS NEXT ITEM after the publication: the
    // commit-point requalification refreshes its captured generation
    // instead of retiring it.
    sink_a
        .send_wait(Bytes::from_static(b"two"))
        .await
        .expect("the sibling stream of another org sends its next item after publication");
    let stamp_after = registry
        .captured_view(&key_a)
        .expect("the sibling's refreshed view");
    assert_ne!(
        stamp_before.store_generation, stamp_after.store_generation,
        "the commit refreshed the captured generation (GenerationOnly requalification)",
    );
    assert_eq!(
        registry.commit_check(&key_a, incarnation_a),
        CommitVerdict::Proceed,
        "after the refresh the sibling's view is current again",
    );
    assert!(call_a.is_live(), "the sibling is untouched");
    assert!(
        s13::wait_for(Duration::from_secs(30), || call_b.terminal().is_some()).await,
        "the victim's terminal lands",
    );
    s14::assert_terminal(&call_b, StreamTerminalReason::Revoked, "victim").await;
    let _ = sink_a;
    call_a.retire(StreamTerminalReason::Cancelled);
    drop(holder_a);
    drop(holder_b);
    let _ = captured_a;
}

/// §2.3/§D3 — a poisoned store fails closed: further items are refused
/// and the affected call retires (`AuthorityUnavailable`), and the
/// empty-slice authority wake retires ALL protected streams. Recovery
/// does not resume them.
#[tokio::test]
async fn poisoned_store_retires_all_protected_streams() {
    let server = fixture::build_node_with(EntityKeypair::from_bytes([0x63u8; 32])).await;
    let (org_b, _auth, _dir) = s14::install_authority_owned(&server, "s14-w3");
    let store = server.org_revocation_store().expect("installed store");
    let caller_kp = EntityKeypair::from_bytes([0x24u8; 32]);
    let intent = fixture::owner_delegated_intent_gen(
        caller_kp.clone(),
        &org_b,
        server.entity_id().clone(),
        s14::SERVICE,
        1,
    );
    let reg = s14::owner_reg(1);
    let replay = AdmissionReplayGuard::with_defaults();
    let session = s14::synthetic_session();
    let clock = ClockSample::now();
    let lifetime = s13::lifetime(StreamLifetimePolicy::q1_defaults(), &[], clock);

    // Two live protected streams: one actively sending, one idle.
    let mut calls = Vec::new();
    let mut sinks = Vec::new();
    for (call_id, origin) in [(42u64, 0x1111u64), (43, 0x1112)] {
        let (frame, admitted, lease) = s14::admit_ss(
            &server,
            &intent,
            &reg,
            &replay,
            clock,
            call_id,
            origin,
            &session,
            [0xA1u8; 32],
            None,
            b"open",
        )
        .expect("the opening admits");
        let holder = Arc::new(s14::SinkHolder::new());
        let (emit, _captured) = s13::capturing_async_emitter();
        let mut fold = RpcServerStreamingFold::new(holder.clone(), emit);
        let call = s14::open_ss_call(
            &mut fold, &frame, admitted, lease, &session, origin, &lifetime,
        );
        assert!(
            s13::wait_for(Duration::from_secs(10), || holder
                .started
                .load(std::sync::atomic::Ordering::SeqCst)
                == 1)
            .await,
            "the handler entered",
        );
        sinks.push(holder.sink.lock().take().expect("sink"));
        calls.push((call, fold));
    }
    let call_active = calls[0].0.clone();
    let call_idle = calls[1].0.clone();

    store.mark_poisoned_for_test();
    assert!(store.is_poisoned(), "the store is poisoned");

    // (a) the ACTIVE stream's next item is refused at the §2.3 commit
    // boundary (poisoned ⇒ `Unusable`) and its call retires — "further
    // items refused, terminal emitted".
    let refused = sinks[0].send_wait(Bytes::from_static(b"x")).await;
    assert!(refused.is_err(), "a poisoned store refuses further items");
    assert_eq!(
        call_active.retire_reason(),
        Some(StreamTerminalReason::AuthorityUnavailable),
        "the affected call retires fail-closed",
    );

    // (b) the empty-slice authority wake (the recovery notify — the real
    // store's `mark_poisoned_for_test` marks without waking; see finding
    // F-S1.4-3) retires ALL protected streams — the idle one included —
    // before the call returns.
    let empty = OrgRevocationBundle::try_issue(&org_b, &BTreeMap::new()).expect("empty bundle");
    store.apply_bundle(&empty).expect("recovery wake");
    let idle_at_wake_return = call_idle.retire_reason();
    assert_eq!(
        idle_at_wake_return,
        Some(StreamTerminalReason::AuthorityUnavailable),
        "the empty-slice wake retires ALL protected streams before it returns",
    );
    s14::assert_terminal(
        &call_idle,
        StreamTerminalReason::AuthorityUnavailable,
        "idle stream",
    )
    .await;
    s14::assert_terminal(
        &call_active,
        StreamTerminalReason::AuthorityUnavailable,
        "active stream",
    )
    .await;

    // Recovery does not resume them: every later send is refused and the
    // records stay terminal.
    assert!(sinks[1].send_wait(Bytes::from_static(b"y")).await.is_err());
    assert!(!call_idle.is_live() && !call_active.is_live());
}

/// §2.3's store/authority replacement path: a replacement retires every
/// record captured under the old `(authority_ptr, store_ptr)` BEFORE the
/// install returns, and the registry RE-SUBSCRIBES to the new store (its
/// raises still retire; the old store's do not).
#[tokio::test]
async fn store_replacement_retires_all_and_resubscribes() {
    let server = fixture::build_node_with(EntityKeypair::from_bytes([0x64u8; 32])).await;
    let (org_b, auth1, _dir1) = s14::install_authority_owned(&server, "s14-w4a");
    let store1 = auth1.revocation.clone();
    let caller_kp = EntityKeypair::from_bytes([0x24u8; 32]);
    let intent = fixture::owner_delegated_intent_gen(
        caller_kp.clone(),
        &org_b,
        server.entity_id().clone(),
        s14::SERVICE,
        1,
    );
    let reg = s14::owner_reg(1);
    let replay = AdmissionReplayGuard::with_defaults();
    let session = s14::synthetic_session();
    let clock = ClockSample::now();
    let lifetime = s13::lifetime(StreamLifetimePolicy::q1_defaults(), &[], clock);

    let open = |call_id: u64, origin: u64| {
        let (frame, admitted, lease) = s14::admit_ss(
            &server,
            &intent,
            &reg,
            &replay,
            clock,
            call_id,
            origin,
            &session,
            [0xA1u8; 32],
            None,
            b"open",
        )
        .expect("the opening admits");
        let holder = Arc::new(s14::SinkHolder::new());
        let (emit, _captured) = s13::capturing_async_emitter();
        let mut fold = RpcServerStreamingFold::new(holder.clone(), emit);
        let call = s14::open_ss_call(
            &mut fold, &frame, admitted, lease, &session, origin, &lifetime,
        );
        (call, fold, holder)
    };
    let (call1, _fold1, _holder1) = open(42, 0x1111);

    // REPLACE the authority (same owner org, fresh store on disk): the
    // mesh.rs install-site hook rebinds the registry — records captured
    // under the OLD pair retire before this returns.
    let (_org, auth2, _dir2) = s14::install_authority_owned(&server, "s14-w4b");
    let store2 = auth2.revocation.clone();
    assert_eq!(
        call1.retire_reason(),
        Some(StreamTerminalReason::AuthorityUnavailable),
        "records captured under the old (authority_ptr, store_ptr) retire \
         before the install call returns",
    );
    s14::assert_terminal(
        &call1,
        StreamTerminalReason::AuthorityUnavailable,
        "replaced-store record",
    )
    .await;

    // RESUBSCRIBED: a raise on the NEW store still retires.
    let (call2, _fold2, _holder2) = open(43, 0x1112);
    s14::raise_floor(&store2, &org_b, caller_kp.entity_id().clone(), 2);
    assert_eq!(
        call2.retire_reason(),
        Some(StreamTerminalReason::Revoked),
        "the registry is subscribed to the NEW store",
    );

    // …and the OLD store's raises no longer reach the registry at all.
    let caller2 = EntityKeypair::from_bytes([0x27u8; 32]);
    let intent2 = fixture::owner_delegated_intent_gen(
        caller2.clone(),
        &org_b,
        server.entity_id().clone(),
        s14::SERVICE,
        1,
    );
    let (frame3, admitted3, lease3) = s14::admit_ss(
        &server,
        &intent2,
        &reg,
        &replay,
        clock,
        44,
        0x1113,
        &session,
        [0xA1u8; 32],
        None,
        b"open",
    )
    .expect("the post-replacement opening admits");
    let holder3 = Arc::new(s14::SinkHolder::new());
    let (emit3, _captured3) = s13::capturing_async_emitter();
    let mut fold3 = RpcServerStreamingFold::new(holder3.clone(), emit3);
    let call3 = s14::open_ss_call(
        &mut fold3, &frame3, admitted3, lease3, &session, 0x1113, &lifetime,
    );
    s14::raise_floor(&store1, &org_b, caller2.entity_id().clone(), 2);
    assert!(
        call3.retire_reason().is_none(),
        "the REPLACED store's raises no longer reach the registry (its \
         subscription died with the replacement)",
    );
    call3.retire(StreamTerminalReason::Cancelled);
    drop(holder3);
}

/// §2.4 session retirement — the `install_peer_locked` displaced branch
/// retires EXACTLY the replaced session's calls; a successor call on the
/// new establishment — even reusing the same `(caller, call_id)` — is
/// unaffected.
#[tokio::test]
async fn session_replacement_retires_old_call() {
    let server = fixture::build_node_with(EntityKeypair::from_bytes([0x65u8; 32])).await;
    let caller_node = fixture::build_node_with(EntityKeypair::from_bytes([0x07u8; 32])).await;
    // A QUIESCENT pair (no announcements, no dispatch loops): a
    // same-static re-handshake rotates immediately — the rotation gate
    // defers only a live-and-BUSY session (open streams / unacked data).
    fixture::connect_no_start(&caller_node, &server).await;
    let (org_b, _auth, _dir) = s14::install_authority_owned(&server, "s14-w5");
    let caller_kp = EntityKeypair::from_bytes([0x24u8; 32]);
    let intent = fixture::owner_delegated_intent_gen(
        caller_kp.clone(),
        &org_b,
        server.entity_id().clone(),
        s14::SERVICE,
        1,
    );
    let reg = s14::owner_reg(1);
    let replay = AdmissionReplayGuard::with_defaults();
    let clock = ClockSample::now();
    let lifetime = s13::lifetime(StreamLifetimePolicy::q1_defaults(), &[], clock);

    // The record binds the REAL session identity: the exact establishment
    // (full handshake hash) the displaced branch will name.
    let session = SessionIdentity {
        peer: caller_node.node_id(),
        session_id: server
            .peer_session_id(caller_node.node_id())
            .expect("live session"),
        establishment: server.peer_session_binding(caller_node.node_id()),
    };
    let (frame, admitted, lease) = s14::admit_ss(
        &server,
        &intent,
        &reg,
        &replay,
        clock,
        42,
        0x1111,
        &session,
        session
            .establishment
            .expect("a real session carries its binding"),
        None,
        b"open",
    )
    .expect("the opening admits");
    let holder = Arc::new(s14::SinkHolder::new());
    let (emit, _captured) = s13::capturing_async_emitter();
    let mut fold = RpcServerStreamingFold::new(holder.clone(), emit);
    let call = s14::open_ss_call(
        &mut fold, &frame, admitted, lease, &session, 0x1111, &lifetime,
    );
    assert!(
        s13::wait_for(Duration::from_secs(10), || holder
            .started
            .load(std::sync::atomic::Ordering::SeqCst)
            == 1)
        .await,
        "the handler entered before the replacement",
    );

    // RE-HANDSHAKE: the displaced branch retires the old session's calls
    // before the transition returns.
    fixture::connect_no_start(&caller_node, &server).await;
    assert_eq!(
        call.retire_reason(),
        Some(StreamTerminalReason::SessionReplaced),
        "the displaced session's call retires at the replacement boundary",
    );
    s14::assert_terminal(&call, StreamTerminalReason::SessionReplaced, "old call").await;

    // The SUCCESSOR call — same `(caller, call_id)` on the NEW
    // establishment — is unaffected and completes normally.
    assert!(
        s13::wait_for(Duration::from_secs(10), || s14::registry_of(&server)
            .record_count()
            == 0)
        .await,
        "the retired call's record is reclaimed before the key is reused",
    );
    let session2 = SessionIdentity {
        peer: caller_node.node_id(),
        session_id: server
            .peer_session_id(caller_node.node_id())
            .expect("live session"),
        establishment: server.peer_session_binding(caller_node.node_id()),
    };
    assert_ne!(
        session2.session_id, session.session_id,
        "a re-handshake is a new establishment",
    );
    let release = Arc::new(tokio::sync::Notify::new());
    let dropped2 = Arc::new(AtomicBool::new(false));
    let ran2 = Arc::new(AtomicUsize::new(0));
    // The SAME call id is reused — outside the replay guard's window
    // (the monotonic jump of W6's trick), so the guard cannot be what
    // decides this: the successor exists and the old call's late cleanup
    // cannot touch it.
    let clock_late = ClockSample {
        wall_ns: clock.wall_ns,
        monotonic: clock.monotonic + Duration::from_secs(400),
    };
    let (frame2, admitted2, lease2) = s14::admit_ss(
        &server,
        &intent,
        &reg,
        &replay,
        clock_late,
        42, // the SAME call id
        0x1111,
        &session2,
        session2.establishment.expect("binding"),
        None,
        b"open",
    )
    .expect("the successor opening admits on the new session");
    let (emit2, captured2) = s13::capturing_async_emitter();
    let mut fold2 = RpcServerStreamingFold::new(
        Arc::new(s13::ParkUntilReleased {
            release: Arc::clone(&release),
            dropped: Arc::clone(&dropped2),
            ran: Arc::clone(&ran2),
        }),
        emit2,
    );
    let call2 = s14::open_ss_call(
        &mut fold2, &frame2, admitted2, lease2, &session2, 0x1111, &lifetime,
    );
    assert!(
        s13::wait_for(Duration::from_secs(10), || ran2
            .load(std::sync::atomic::Ordering::SeqCst)
            == 1)
        .await,
        "the successor's handler entered",
    );
    assert!(call2.is_live(), "the successor call is unaffected");
    release.notify_one();
    assert!(
        s13::wait_for(Duration::from_secs(10), || matches!(
            call2.terminal(),
            Some(StreamTerminalReason::Completed(_))
        ))
        .await,
        "the successor completes normally",
    );
    assert_eq!(
        captured2.lock().len(),
        2,
        "the successor's own chunk + its own terminal",
    );
}

/// §3/D4 — a live call id cannot be reused even AFTER the replay guard's
/// window has lapsed for it: `reserve` refuses `ActiveCallOwned` before
/// decode (the guard is never consulted), and the same reuse reaches
/// ADMITTED — not `Replay` — once the call is gone, proving the refusal
/// was active ownership, not replay retention.
#[tokio::test]
async fn active_call_id_reuse_after_replay_window_is_refused() {
    let server = fixture::build_node_with(EntityKeypair::from_bytes([0x66u8; 32])).await;
    let (org_b, _auth, _dir) = s14::install_authority_owned(&server, "s14-w6");
    let caller_kp = EntityKeypair::from_bytes([0x24u8; 32]);
    let intent = fixture::owner_delegated_intent_gen(
        caller_kp.clone(),
        &org_b,
        server.entity_id().clone(),
        s14::SERVICE,
        1,
    );
    let reg = s14::owner_reg(1);
    let replay = AdmissionReplayGuard::with_defaults();
    let session = s14::synthetic_session();
    let clock0 = ClockSample::now();
    // The documented clock pairing (admission_clock.rs): freshness reads
    // `wall_ns`, replay RETENTION derives from `monotonic`. A monotonic
    // jump past `proof_expiry + 300 s` is exactly "after the replay
    // window" while the wall-clock freshness checks stay satisfied.
    let clock_late = ClockSample {
        wall_ns: clock0.wall_ns,
        monotonic: clock0.monotonic + Duration::from_secs(400),
    };

    // #1 — the call admits and STAYS LIVE (its lease is held).
    let (_frame, _admitted, lease1) = s14::admit_ss(
        &server,
        &intent,
        &reg,
        &replay,
        clock0,
        42,
        0x1111,
        &session,
        [0xA1u8; 32],
        None,
        b"open",
    )
    .expect("the first opening admits");
    assert_eq!(replay.len(), 1, "one guard entry — the first proof's");

    // #2 — the SAME `(caller, call_id)` arriving after the replay window
    // (monotonic +400 s > expiry + 300 s) is REFUSED while the call is
    // live — `ActiveCallOwned` from `reserve`, before decode, with the
    // guard never consulted.
    let refused = s14::admit_ss(
        &server,
        &intent,
        &reg,
        &replay,
        clock_late,
        42,
        0x1111,
        &session,
        [0xA1u8; 32],
        None,
        b"open",
    )
    .expect_err("an ACTIVE call id cannot be reused after the replay window");
    assert!(
        matches!(
            refused,
            OpeningRefusal::Denied(AdmissionDenied::ActiveCallOwned)
        ),
        "the reuse is refused as ActiveCallOwned, before decode — got {refused:?}",
    );
    assert_eq!(
        replay.len(),
        1,
        "the refused reuse never reached the replay guard",
    );

    // #3 — the positive control: the same reuse, the same lapsed window,
    // once the call is GONE, ADMITS (the guard's expired entry is
    // overwritten — not `Replay`): the #2 refusal was purely active
    // ownership.
    drop(lease1);
    let (_frame, _admitted, lease3) = s14::admit_ss(
        &server,
        &intent,
        &reg,
        &replay,
        clock_late,
        42,
        0x1111,
        &session,
        [0xA1u8; 32],
        None,
        b"open",
    )
    .expect("the same reuse admits once the call is gone — the replay window had lapsed");
    drop(lease3);
}

/// §3 step 4 — a raise landing between `reserve` and `install` denies the
/// install (`Revoked`) with ZERO effects: the acting-org quota is never
/// charged, no fold effect happens, and the bridge's rollback frees
/// the key for reuse.
#[tokio::test]
async fn raise_between_reserve_and_install_denies_with_zero_effects() {
    let server = fixture::build_node_with(EntityKeypair::from_bytes([0x67u8; 32])).await;
    let (org_b, _auth, _dir) = s14::install_authority_owned(&server, "s14-w7");
    let caller_kp = EntityKeypair::from_bytes([0x24u8; 32]);
    let registry = s14::registry_of(&server);
    let session = s14::synthetic_session();
    let key = s14::call_key(caller_kp.entity_id(), 42);
    let clock = ClockSample::now();

    // Reserve (the model's `raise_between_reserve_and_install_denies_with_zero_effects`).
    let mut reservation = registry
        .reserve(OpeningRequest {
            key: key.clone(),
            session: session.clone(),
            session_generation: Some(1),
            registration: 1,
            shape: RpcCallShape::ServerStreaming,
            now_ns: clock.wall_ns,
        })
        .expect("reserve");
    assert_eq!(registry.active_node(), 1, "the provisional slot is held");
    assert_eq!(
        registry.authority_epoch(),
        reservation.epoch_at_reserve,
        "no notification yet",
    );

    // The raise lands BETWEEN reserve and install — through the real
    // store's publish and the mesh.rs-installed raise subscription.
    s14::raise_floor(
        &server.org_revocation_store().expect("installed store"),
        &org_b,
        caller_kp.entity_id().clone(),
        2,
    );
    assert_eq!(
        registry.authority_epoch(),
        reservation.epoch_at_reserve + 1,
        "the notified callback moved the epoch",
    );

    let denied = registry
        .install(
            &mut reservation,
            VerifiedCallFacts {
                acting_org: org_b.org_id(),
                member: caller_kp.entity_id().clone(),
                member_generation: 1,
                deadline: None,
            },
            clock.wall_ns,
        )
        .expect_err("install must deny after the raise");
    assert_eq!(
        denied,
        AdmissionDenied::Revoked,
        "the §2.3 requalification refuses this member",
    );
    // ZERO effects: no fold effects are possible (no lease exists), the
    // org quota is never charged, and the record is terminal.
    assert_eq!(
        registry.active_for_org(&org_b.org_id()),
        0,
        "org quota never charged"
    );
    assert_eq!(registry.phase(&key), Some(RegistryPhase::Terminal));

    // The bridge's reservation guard owns the rollback; the key frees.
    drop(reservation);
    assert_eq!(registry.active_node(), 0);
    assert_eq!(registry.removals(&key, 1), 1, "exactly one removal");
    assert!(registry.record_count() == 0);
    let second = registry.reserve(OpeningRequest {
        key,
        session,
        session_generation: Some(1),
        registration: 1,
        shape: RpcCallShape::ServerStreaming,
        now_ns: clock.wall_ns,
    });
    assert!(second.is_ok(), "the key is reusable after the rollback");
}

/// §2.7 — queued bytes over the per-call budget park `send_wait` (only on
/// satisfiable bounds) and retirement WAKES it with the closed-sink
/// refusal: the wait is interruptible by retire, exactly once.
#[tokio::test]
async fn queued_bytes_over_call_budget_park_send_wait_and_wake_on_retire() {
    let server = fixture::build_node_with(EntityKeypair::from_bytes([0x68u8; 32])).await;
    // TINY byte budgets (the Q1 defaults are provider-configurable
    // engineering defaults — the accounting semantics are what is under
    // test): 24 B per call per direction.
    s14::set_tiny_byte_registry(&server);
    let (org_b, _auth, _dir) = s14::install_authority_owned(&server, "s14-w8");
    let caller_kp = EntityKeypair::from_bytes([0x24u8; 32]);
    let intent = fixture::owner_delegated_intent_gen(
        caller_kp.clone(),
        &org_b,
        server.entity_id().clone(),
        s14::SERVICE,
        1,
    );
    let reg = s14::owner_reg(1);
    let replay = AdmissionReplayGuard::with_defaults();
    let session = s14::synthetic_session();
    let clock = ClockSample::now();
    let (frame, admitted, lease) = s14::admit_ss(
        &server,
        &intent,
        &reg,
        &replay,
        clock,
        42,
        0x1111,
        &session,
        [0xA1u8; 32],
        Some(0), // zero credit: the pump never releases queued bytes
        b"open",
    )
    .expect("the opening admits");
    let holder = Arc::new(s14::SinkHolder::new());
    let (emit, _captured) = s13::capturing_async_emitter();
    let mut fold = RpcServerStreamingFold::new(holder.clone(), emit);
    let lifetime = s13::lifetime(StreamLifetimePolicy::q1_defaults(), &[], clock);
    let call = s14::open_ss_call(
        &mut fold, &frame, admitted, lease, &session, 0x1111, &lifetime,
    );
    assert!(
        s13::wait_for(Duration::from_secs(10), || holder
            .started
            .load(std::sync::atomic::Ordering::SeqCst)
            == 1)
        .await,
        "the handler entered",
    );
    let sink = holder.sink.lock().take().expect("sink");

    // Three 8 B items fill the 24 B call budget (the zero-credit pump
    // keeps every permit charged).
    for _ in 0..3 {
        sink.send_wait(Bytes::from_static(b"12345678"))
            .await
            .expect("an in-budget item queues");
    }
    // The fourth PARKS on the full call budget — a satisfiable bound.
    let parked = tokio::spawn(async move { sink.send_wait(Bytes::from_static(b"12345678")).await });
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert!(
        !parked.is_finished(),
        "the item over the call budget must PARK in send_wait",
    );

    // Retire wakes it — interruptible by retirement is the §2.7 contract.
    call.retire(StreamTerminalReason::Cancelled);
    let woke = tokio::time::timeout(Duration::from_secs(10), parked)
        .await
        .expect("the parked send_wait must wake on retire")
        .expect("join");
    assert!(
        woke.is_err(),
        "the woken send_wait reports the closed sink (the chunk was not sent)",
    );
    s14::assert_terminal(&call, StreamTerminalReason::Cancelled, "retired call").await;
}

/// Q3/C9 (Main's carve) — node shutdown retires every live protected
/// stream of THAT node with its typed terminal, and a SIBLING node's
/// streams survive and complete normally.
#[tokio::test]
async fn node_shutdown_retires_live_protected_streams() {
    use net::adapter::Adapter as _;

    let node_a = fixture::build_node_with(EntityKeypair::from_bytes([0x69u8; 32])).await;
    let node_b = fixture::build_node_with(EntityKeypair::from_bytes([0x6Au8; 32])).await;
    let clock = ClockSample::now();
    let lifetime = s13::lifetime(StreamLifetimePolicy::q1_defaults(), &[], clock);
    let replay = AdmissionReplayGuard::with_defaults();
    let session = s14::synthetic_session();

    // Node A: a live protected stream under its own authority/store.
    let (org_a, _auth_a, _dir_a) = s14::install_authority_owned(&node_a, "s14-w9a");
    let caller_a = EntityKeypair::from_bytes([0x24u8; 32]);
    let intent_a = fixture::owner_delegated_intent_gen(
        caller_a.clone(),
        &org_a,
        node_a.entity_id().clone(),
        s14::SERVICE,
        1,
    );
    let reg = s14::owner_reg(1);
    let (frame_a, admitted_a, lease_a) = s14::admit_ss(
        &node_a,
        &intent_a,
        &reg,
        &replay,
        clock,
        42,
        0x1111,
        &session,
        [0xA1u8; 32],
        None,
        b"open",
    )
    .expect("the opening admits on A");
    let holder_a = Arc::new(s14::SinkHolder::new());
    let (emit_a, _captured_a) = s13::capturing_async_emitter();
    let mut fold_a = RpcServerStreamingFold::new(holder_a.clone(), emit_a);
    let call_a = s14::open_ss_call(
        &mut fold_a,
        &frame_a,
        admitted_a,
        lease_a,
        &session,
        0x1111,
        &lifetime,
    );

    // Node B: the SIBLING node's live protected stream (its own
    // authority/store and registry).
    let (org_b, _auth_b, _dir_b) = s14::install_authority_owned(&node_b, "s14-w9b");
    let caller_b = EntityKeypair::from_bytes([0x25u8; 32]);
    let intent_b = fixture::owner_delegated_intent_gen(
        caller_b.clone(),
        &org_b,
        node_b.entity_id().clone(),
        s14::SERVICE,
        1,
    );
    let (frame_b, admitted_b, lease_b) = s14::admit_ss(
        &node_b,
        &intent_b,
        &reg,
        &replay,
        clock,
        42,
        0x1111,
        &session,
        [0xA1u8; 32],
        None,
        b"open",
    )
    .expect("the opening admits on B");
    let release_b = Arc::new(tokio::sync::Notify::new());
    let dropped_b = Arc::new(AtomicBool::new(false));
    let ran_b = Arc::new(AtomicUsize::new(0));
    let (emit_b, captured_b) = s13::capturing_async_emitter();
    let mut fold_b = RpcServerStreamingFold::new(
        Arc::new(s13::ParkUntilReleased {
            release: Arc::clone(&release_b),
            dropped: Arc::clone(&dropped_b),
            ran: Arc::clone(&ran_b),
        }),
        emit_b,
    );
    let call_b = s14::open_ss_call(
        &mut fold_b,
        &frame_b,
        admitted_b,
        lease_b,
        &session,
        0x1111,
        &lifetime,
    );
    assert!(
        s13::wait_for(Duration::from_secs(10), || holder_a
            .started
            .load(std::sync::atomic::Ordering::SeqCst)
            == 1
            && ran_b.load(std::sync::atomic::Ordering::SeqCst) == 1)
        .await,
        "both handlers entered before the shutdown",
    );

    // Node A shuts down (the `Adapter::shutdown` hook): its live
    // protected streams reach their typed terminal.
    node_a.shutdown().await.expect("shutdown");
    let retired_at_shutdown_return = call_a.retire_reason();
    assert!(
        s13::wait_for(Duration::from_secs(30), || call_a.retire_reason().is_some()).await,
        "the live protected stream must be retired by node shutdown",
    );
    assert_eq!(
        retired_at_shutdown_return,
        Some(StreamTerminalReason::ServeHandleDropped),
        "the retirement landed before the shutdown call returned",
    );
    s14::assert_terminal(
        &call_a,
        StreamTerminalReason::ServeHandleDropped,
        "shutdown-retired stream",
    )
    .await;

    // The SIBLING node's stream survives and completes normally.
    assert!(call_b.is_live(), "the sibling node's stream survives");
    release_b.notify_one();
    assert!(
        s13::wait_for(Duration::from_secs(10), || matches!(
            call_b.terminal(),
            Some(StreamTerminalReason::Completed(_))
        ))
        .await,
        "the sibling node's stream completes normally",
    );
    assert_eq!(
        captured_b.lock().len(),
        2,
        "the sibling's own chunk + its own terminal",
    );
    drop(fold_a);
    drop(fold_b);
}

// ============================================================================
// Slice 1.5 — bridge wiring + routing (contract 5). These witnesses drive the
// REAL serve bridge (`ServeHandle::inject_inbound_for_test` = the dispatcher's
// exact hand-off), so §3 admission and the fold drive run in one bridge
// iteration, and observe every response at REAL wire endpoints (the NC2
// probe idiom from `nrpc_streaming_gate.rs:268-305`).
// ============================================================================

use net::adapter::net::behavior::org_grant::{GrantRights, GrantTargetScope, OrgCapabilityGrant};
use net::adapter::net::{ChannelName, ChannelPublisher, PublishConfig};
use std::sync::atomic::Ordering;

/// A FORBIDDEN stream opening causes ZERO handler effects. A handler
/// counter alone is not proof (the plan): observe handler entry, sink
/// sends, grant mutations (the wire grant/terminal channels) AND the fold
/// call-key maps (`in_flight_keys` / `sender_keys`).
#[tokio::test]
async fn forbidden_stream_opening_causes_zero_handler_effects() {
    let server = fixture::build_node_with(EntityKeypair::from_bytes([0x71u8; 32])).await;
    let caller = s15::caller_keypair(0x24);
    let caller = fixture::build_node_with(caller).await;
    let bystander = fixture::build_node_with(EntityKeypair::from_bytes([0x73u8; 32])).await;
    s15::connect_three(&caller, &server, &bystander).await;
    let (org_b, _auth, _dir) = s14::install_authority_owned(&server, "s15-w1");

    let entries = Arc::new(AtomicUsize::new(0));
    let sends = Arc::new(AtomicUsize::new(0));
    let serve = server
        .serve_rpc_owner_scoped_streaming(
            "svc",
            Arc::new(s15::CountingSS {
                entries: entries.clone(),
                sends: sends.clone(),
            }),
            Arc::new(|_| true),
        )
        .expect("serve owner-scoped streaming");

    // Wire observation channels: the caller's own reply channel carries
    // every frame the provider emits for the call (the output-emission and
    // grant-mutation channels); a roster subscriber (the bystander) proves
    // nothing leaks beyond the authenticated peer.
    let caller_origin = caller.origin_hash();
    let reply_channel = ChannelName::new(&format!("svc.replies.{caller_origin:016x}")).unwrap();
    let (caller_disp, caller_seen) = s15::recorder();
    assert!(caller
        .register_rpc_inbound(reply_channel.hash(), caller_disp)
        .is_some());
    let (bystander_disp, bystander_seen) = s15::recorder();
    assert!(bystander
        .register_rpc_inbound(reply_channel.hash(), bystander_disp)
        .is_some());
    bystander
        .subscribe_channel(server.node_id(), reply_channel.clone())
        .await
        .expect("bystander subscribes to the caller's reply channel");
    let _pub = ChannelPublisher::new(reply_channel.clone(), PublishConfig::default());

    // FORBIDDEN: a structurally perfect owner-delegated proof carrying a
    // CROSS-ORG capability grant — §1.5's mode checks forbid the SHAPE at an
    // OwnerDelegated registration ("an unexpected one is malformed",
    // `AdmissionDenied::UnexpectedCapabilityGrant`) with the signature and
    // every credential valid.
    let caller_kp = s15::caller_keypair(0x24);
    let mut intent =
        fixture::owner_delegated_intent(caller_kp, &org_b, server.entity_id().clone(), "svc");
    let cap = CapabilityAuthorityId::for_tag("nrpc:svc");
    let (grant, _audience) = OrgCapabilityGrant::try_issue(
        &org_b,
        org_b.org_id(),
        cap,
        GrantRights::INVOKE,
        GrantTargetScope::ExactNode(server.entity_id().clone()),
        3600,
    )
    .expect("capability grant");
    intent.capability_grant = Some(grant);
    let binding = server
        .peer_session_binding(caller.node_id())
        .expect("the live session carries its binding (1.1a)");
    let session_id = server
        .peer_session_id(caller.node_id())
        .expect("the live session id");
    let (frame, _req) = s14::mint_ss_opening(&intent, binding, 42, caller_origin, None, b"open");
    assert!(
        serve.inject_inbound_for_test(s13::inbound(
            session_id,
            caller.node_id(),
            caller_origin,
            frame
        )),
        "the bridge accepted the frame",
    );

    // Exactly ONE wire frame: the typed denial (0x0009 + the single coarse
    // `Denied` byte). No chunk, no grant, no other terminal — the forbidden
    // opening produced NO output emission and NO grant mutation.
    assert!(
        s13::wait_for(Duration::from_secs(10), || caller_seen.lock().len() == 1).await,
        "the caller is told exactly one denial",
    );
    assert!(
        s15::is_denied_byte(&caller_seen.lock()[0], 0),
        "the single frame is AdmissionDenied + the coarse Denied byte",
    );
    fixture::assert_handler_stays_dark(&entries, "the forbidden opening's handler stayed dark")
        .await;
    assert_eq!(
        sends.load(Ordering::SeqCst),
        0,
        "the handler's SINK SENDS stayed at zero (output emission observed separately)",
    );
    // …and the frame count did not move during the darkness window.
    assert_eq!(
        caller_seen.lock().len(),
        1,
        "the denial is the ONLY frame — no chunks, no grants, no extra terminals",
    );
    s15::assert_stays_empty(
        &bystander_seen,
        Duration::from_millis(200),
        "the roster subscriber for the caller's reply channel",
    )
    .await;

    // The fold call-key maps: no in-flight token, no flow/grant semaphore
    // (`grant mutations` at the fold), no protected record — and the §3
    // reservation rolled back.
    let fold = serve
        .streaming_fold_for_test()
        .expect("the streaming fold handle");
    let key = (caller.node_id(), session_id, caller_origin, 42u64);
    {
        let fold = fold.lock();
        assert!(
            fold.in_flight_keys().is_empty(),
            "in_flight_keys(): a forbidden opening creates no in-flight state",
        );
        assert_eq!(
            fold.flow_control_permits(key),
            None,
            "no flow/grant semaphore is ever installed for a forbidden opening",
        );
    }
    let registry = s14::registry_of(&server);
    assert_eq!(
        registry.record_count(),
        0,
        "the §3 reservation rolled back — no registry record survives",
    );
    assert_eq!(registry.active_node(), 0, "no active-call quota charged");

    // The input-delivery maps (`sender_keys` — the CS/DX folds' `senders`)
    // for the SAME opening shape: the CS and duplex bridges refuse it at
    // their shape check, cleanly before any call state (their protected
    // forms do not exist at this stage — E1.8).
    let cs_entries = Arc::new(AtomicUsize::new(0));
    let dx_entries = Arc::new(AtomicUsize::new(0));
    let cs = server
        .serve_rpc_client_stream(
            "svc-cs",
            Arc::new(s15::CountingCS {
                entries: cs_entries.clone(),
            }),
        )
        .expect("serve client-streaming");
    let dx = server
        .serve_rpc_duplex(
            "svc-dx",
            Arc::new(s15::CountingDX {
                entries: dx_entries.clone(),
            }),
        )
        .expect("serve duplex");
    for (handle, svc, call_id) in [(&cs, "svc-cs", 43u64), (&dx, "svc-dx", 44u64)] {
        let frame = s15::plain_ss_opening(svc, call_id, caller_origin, b"open");
        assert!(
            handle.inject_inbound_for_test(s13::inbound(
                session_id,
                caller.node_id(),
                caller_origin,
                frame
            )),
            "{svc}: the bridge accepted the frame",
        );
    }
    fixture::assert_handler_stays_dark(&cs_entries, "the CS handler stayed dark").await;
    fixture::assert_handler_stays_dark(&dx_entries, "the duplex handler stayed dark").await;
    let cs_fold = cs.request_fold_for_test().expect("the CS fold handle");
    {
        let cs_fold = cs_fold.lock();
        assert!(
            cs_fold.in_flight_keys().is_empty(),
            "CS in_flight_keys(): the forbidden opening creates no input-delivery state",
        );
        assert!(
            cs_fold.sender_keys().is_empty(),
            "CS sender_keys(): the forbidden opening creates no request sender",
        );
    }
    let dx_fold = dx.duplex_fold_for_test().expect("the duplex fold handle");
    {
        let dx_fold = dx_fold.lock();
        assert!(
            dx_fold.in_flight_keys().is_empty(),
            "duplex in_flight_keys(): the forbidden opening creates no input-delivery state",
        );
        assert!(
            dx_fold.sender_keys().is_empty(),
            "duplex sender_keys(): the forbidden opening creates no request sender",
        );
    }
}

/// The streaming NC2 witness that did not exist: a PROTECTED
/// server-streaming denial is unicast ONLY to the authenticated session
/// peer — never fanned out to the claimed origin's reply-channel roster
/// where a same-origin bystander (or a forged-origin victim) listens. The
/// bystander probe from `nrpc_streaming_gate.rs:268-305`, adapted to
/// `serve_rpc_owner_scoped_streaming`.
#[tokio::test]
async fn streaming_denial_is_not_fanned_out_to_the_reply_roster() {
    let server = fixture::build_node_with(EntityKeypair::from_bytes([0x72u8; 32])).await;
    let caller = fixture::build_node_with(s15::caller_keypair(0x25)).await;
    let bystander = fixture::build_node_with(EntityKeypair::from_bytes([0x74u8; 32])).await;
    s15::connect_three(&caller, &server, &bystander).await;
    let (_org_b, _auth, _dir) = s14::install_authority_owned(&server, "s15-w2");

    let entries = Arc::new(AtomicUsize::new(0));
    let sends = Arc::new(AtomicUsize::new(0));
    let serve = server
        .serve_rpc_owner_scoped_streaming(
            "svc",
            Arc::new(s15::CountingSS {
                entries: entries.clone(),
                sends: sends.clone(),
            }),
            Arc::new(|_| true),
        )
        .expect("serve owner-scoped streaming");

    // The probe's shape: the bystander subscribes to the CALLER's reply
    // channel and records anything delivered on it.
    let caller_origin = caller.origin_hash();
    let reply_channel = ChannelName::new(&format!("svc.replies.{caller_origin:016x}")).unwrap();
    let (caller_disp, caller_seen) = s15::recorder();
    assert!(caller
        .register_rpc_inbound(reply_channel.hash(), caller_disp)
        .is_some());
    let (bystander_disp, bystander_seen) = s15::recorder();
    assert!(bystander
        .register_rpc_inbound(reply_channel.hash(), bystander_disp)
        .is_some());
    bystander
        .subscribe_channel(server.node_id(), reply_channel.clone())
        .await
        .expect("bystander subscribes to the caller's reply channel");
    let _pub = ChannelPublisher::new(reply_channel.clone(), PublishConfig::default());

    let session_id = server
        .peer_session_id(caller.node_id())
        .expect("the live session id");

    // Leg (a) — the probe's exact shape: a denied opening on the caller's
    // REAL session (no org credential presented). The caller is told
    // exactly once; the bystander sees NOTHING.
    let frame = s15::plain_ss_opening("svc", 42, caller_origin, b"open");
    assert!(
        serve.inject_inbound_for_test(s13::inbound(
            session_id,
            caller.node_id(),
            caller_origin,
            frame
        )),
        "the bridge accepted the frame",
    );
    assert!(
        s13::wait_for(Duration::from_secs(10), || caller_seen.lock().len() == 1).await,
        "a denied streaming caller must be told, not left hanging (§T7)",
    );
    assert!(
        s15::is_denied_byte(&caller_seen.lock()[0], 0),
        "the streaming denial is AdmissionDenied + the coarse Denied byte",
    );
    s15::assert_stays_empty(
        &bystander_seen,
        Duration::from_secs(1),
        "the bystander for a denial on the caller's live session",
    )
    .await;
    assert_eq!(
        caller_seen.lock().len(),
        1,
        "the denial is exactly one frame, delivered only to the authenticated peer",
    );

    // Leg (b) — the NC2 REFLECTION trigger: the same no-proof opening
    // claiming the caller's origin but arriving from an UNROUTABLE peer
    // (no session, no entity pin). Its denial has no authenticated
    // destination: `DirectOnly` DROPS it — it must never be roster-fanned
    // onto the claimed origin's reply channel where the bystander sits.
    // (This is the reachable trigger `response_route_fallback`'s doc names:
    // `PeerPublishOutcome::NoSession` at send time. Flipping `DirectOnly`
    // to `RosterOnStaleDirect` at the production site makes the bystander
    // RECEIVE the terminal — the required inverse red.)
    let ghost = 0xDEAD_BEEF_BAAD_F00Du64;
    let frame = s15::plain_ss_opening("svc", 43, caller_origin, b"open");
    assert!(
        serve.inject_inbound_for_test(s13::inbound(0, ghost, caller_origin, frame)),
        "the bridge accepted the frame",
    );
    s15::assert_stays_empty(
        &bystander_seen,
        Duration::from_secs(1),
        "the bystander for a denial whose direct route is gone",
    )
    .await;
    assert_eq!(
        caller_seen.lock().len(),
        1,
        "the unroutable denial reached NOBODY's roster (dropped, not reflected)",
    );

    // Zero handler effects on both legs.
    fixture::assert_handler_stays_dark(&entries, "both denials kept the handler dark").await;
    assert_eq!(sends.load(Ordering::SeqCst), 0, "zero sink sends");
    let registry = s14::registry_of(&server);
    assert_eq!(registry.record_count(), 0, "no registry record survives");
}

/// Q4 + the §3 step-10/11 order: a VALID proof the provider policy VETOES
/// denies before effects (zero items, zero handler entry) — and the replay
/// slot stays CONSUMED: the identical re-submission is refused at the replay
/// insert (step 10, BEFORE the policy at step 11) without reaching the
/// policy again.
#[tokio::test]
async fn provider_policy_veto_denies_before_effects() {
    let server = fixture::build_node_with(EntityKeypair::from_bytes([0x75u8; 32])).await;
    let caller = fixture::build_node_with(s15::caller_keypair(0x26)).await;
    let bystander = fixture::build_node_with(EntityKeypair::from_bytes([0x76u8; 32])).await;
    s15::connect_three(&caller, &server, &bystander).await;
    let (org_b, _auth, _dir) = s14::install_authority_owned(&server, "s15-w3");

    let entries = Arc::new(AtomicUsize::new(0));
    let sends = Arc::new(AtomicUsize::new(0));
    let policy_calls = Arc::new(AtomicUsize::new(0));
    let policy_calls_for_policy = Arc::clone(&policy_calls);
    let serve = server
        .serve_rpc_owner_scoped_streaming(
            "svc",
            Arc::new(s15::CountingSS {
                entries: entries.clone(),
                sends: sends.clone(),
            }),
            Arc::new(move |_| {
                policy_calls_for_policy.fetch_add(1, Ordering::SeqCst);
                false
            }),
        )
        .expect("serve owner-scoped streaming");

    let caller_origin = caller.origin_hash();
    let reply_channel = ChannelName::new(&format!("svc.replies.{caller_origin:016x}")).unwrap();
    let (caller_disp, caller_seen) = s15::recorder();
    assert!(caller
        .register_rpc_inbound(reply_channel.hash(), caller_disp)
        .is_some());
    let (bystander_disp, bystander_seen) = s15::recorder();
    assert!(bystander
        .register_rpc_inbound(reply_channel.hash(), bystander_disp)
        .is_some());
    bystander
        .subscribe_channel(server.node_id(), reply_channel.clone())
        .await
        .expect("bystander subscribes to the caller's reply channel");
    let _pub = ChannelPublisher::new(reply_channel.clone(), PublishConfig::default());

    // A VALID owner-delegated proof: every credential, the binding and the
    // session fence pass — only the provider-local application veto (step
    // 11, LAST) refuses it.
    let intent = fixture::owner_delegated_intent(
        s15::caller_keypair(0x26),
        &org_b,
        server.entity_id().clone(),
        "svc",
    );
    let binding = server
        .peer_session_binding(caller.node_id())
        .expect("the live session carries its binding (1.1a)");
    let session_id = server
        .peer_session_id(caller.node_id())
        .expect("the live session id");
    let (frame, _req) = s14::mint_ss_opening(&intent, binding, 42, caller_origin, None, b"open");
    assert!(
        serve.inject_inbound_for_test(s13::inbound(
            session_id,
            caller.node_id(),
            caller_origin,
            frame.clone()
        )),
        "the bridge accepted the frame",
    );
    assert!(
        s13::wait_for(Duration::from_secs(10), || caller_seen.lock().len() == 1).await,
        "the vetoed caller is told",
    );
    assert!(
        s15::is_denied_byte(&caller_seen.lock()[0], 0),
        "the veto is AdmissionDenied + the coarse Denied byte",
    );
    assert_eq!(
        policy_calls.load(Ordering::SeqCst),
        1,
        "the provider policy saw the VERIFIED proof exactly once (it runs last)",
    );
    fixture::assert_handler_stays_dark(&entries, "the vetoed opening's handler stayed dark").await;
    assert_eq!(sends.load(Ordering::SeqCst), 0, "zero items");
    s15::assert_stays_empty(
        &bystander_seen,
        Duration::from_millis(200),
        "the roster subscriber for the vetoed call",
    )
    .await;
    let registry = s14::registry_of(&server);
    assert_eq!(
        registry.record_count(),
        0,
        "Q4: the active reservation is RELEASED on veto",
    );
    assert_eq!(registry.active_node(), 0, "no active-call quota charged");

    // Q4: "release the active reservation on veto but RETAIN the replay
    // record" — the IDENTICAL re-submission (same call id, same signed
    // bytes) is refused at the replay insert (step 10) BEFORE the policy
    // (step 11): the vetoed proof is not repeatedly reusable, and the
    // policy is never consulted again for it.
    assert!(
        serve.inject_inbound_for_test(s13::inbound(
            session_id,
            caller.node_id(),
            caller_origin,
            frame
        )),
        "the bridge accepted the re-submission",
    );
    assert!(
        s13::wait_for(Duration::from_secs(10), || caller_seen.lock().len() == 2).await,
        "the re-submission is denied too",
    );
    assert!(
        s15::is_denied_byte(&caller_seen.lock()[1], 0),
        "the re-submission denial is the same coarse Denied byte",
    );
    assert_eq!(
        policy_calls.load(Ordering::SeqCst),
        1,
        "the replay slot stayed CONSUMED — the vetoed proof never reaches the policy again \
         (step 10's insert precedes step 11's veto)",
    );
    fixture::assert_handler_stays_dark(&entries, "the re-submitted opening's handler stayed dark")
        .await;
    assert_eq!(
        sends.load(Ordering::SeqCst),
        0,
        "zero items on re-submission"
    );
    assert_eq!(
        caller_seen.lock().len(),
        2,
        "exactly one denial per attempt — zero items either way",
    );
    assert_eq!(
        registry.record_count(),
        0,
        "the re-submission's reservation rolled back as well",
    );
}

// ===========================================================================
// S1_R — the repair round's acceptance witnesses (S1Review HOLD closure).
// The completion-drain twins ride REAL wire endpoints (the s15 idiom) so the
// observation is DELIVERY at the authenticated receiving endpoint — the F-7
// gap was exactly that the success path only ever captured at emit seams.
// ===========================================================================

/// S1_R Rows 1+2+7 (same-org authority mode). A protected server-streaming
/// call through the REAL bridge: a handler queues ≥2 CONTENT-LABELLED items
/// under ZERO credit and RETURNS (producer finished is not terminal — the
/// drain is owned), a valid `STREAM_GRANT` arrives, the items publish IN
/// ORDER (body bytes + wire sequence — never counted), and then EXACTLY ONE
/// terminal frame asserts the success-terminal wire shape at the
/// AUTHENTICATED RECEIVING ENDPOINT (status `Ok` + `nrpc-streaming: end`).
///
/// Red witnesses (the S1_R brief Appendix inverses): **A2b** (post-close
/// chunks consumed but not published at the supervisor pump) reddens "the
/// same-org queued items publish IN ORDER" below; **A2c** (`end` → `continue`
/// on the `Completed(Ok)` terminal arm) reddens "the same-org terminal
/// frame's exact wire content" below.
#[tokio::test]
async fn completed_stream_drains_queued_items_in_order_with_content_and_end_terminal() {
    use net::adapter::net::cortex::rpc::{HEADER_NRPC_STREAMING, HEADER_NRPC_STREAMING_END};

    let server = fixture::build_node_with(EntityKeypair::from_bytes([0x81u8; 32])).await;
    let caller_kp = s15::caller_keypair(0x27);
    let caller = fixture::build_node_with(caller_kp.clone()).await;
    fixture::bring_up(&caller, &server).await;
    let (org_b, _auth, _dir) = s14::install_authority_owned(&server, "s1r-r1");

    // The handler queues both content-labelled items under zero credit and
    // RETURNS — the response pump parks at its first credit acquire until
    // the grant (§2.2: the credit-blocked drain is owned, not abandoned).
    let returned = Arc::new(AtomicUsize::new(0));
    let dropped = Arc::new(AtomicBool::new(false));
    let serve = server
        .serve_rpc_owner_scoped_streaming(
            s14::SERVICE,
            Arc::new(s14::QueueChunksAndReturn {
                chunks: vec![b"item-A-CONTENT-17", b"item-B-content-29"],
                returned: Arc::clone(&returned),
                dropped: Arc::clone(&dropped),
            }),
            Arc::new(|_| true),
        )
        .expect("serve owner-scoped streaming");

    // The AUTHENTICATED RECEIVING ENDPOINT: the caller's own reply channel
    // carries every frame the provider emits for the call.
    let caller_origin = caller.origin_hash();
    let reply_channel =
        ChannelName::new(&format!("{}.replies.{caller_origin:016x}", s14::SERVICE)).unwrap();
    let (caller_disp, caller_seen) = s15::recorder();
    assert!(caller
        .register_rpc_inbound(reply_channel.hash(), caller_disp)
        .is_some());

    // A VALID owner-delegated streaming opening over the REAL session (full
    // handshake binding, 1.1a), with a ZERO initial credit window.
    let intent = fixture::owner_delegated_intent(
        caller_kp,
        &org_b,
        server.entity_id().clone(),
        s14::SERVICE,
    );
    let binding = server
        .peer_session_binding(caller.node_id())
        .expect("the live session carries its binding (1.1a)");
    let session_id = server
        .peer_session_id(caller.node_id())
        .expect("the live session id");
    let (frame, _req) = s14::mint_ss_opening(&intent, binding, 42, caller_origin, Some(0), b"open");
    assert!(
        serve.inject_inbound_for_test(s13::inbound(
            session_id,
            caller.node_id(),
            caller_origin,
            frame
        )),
        "the bridge accepted the opening",
    );
    assert!(
        s13::wait_for(Duration::from_secs(10), || returned
            .load(Ordering::SeqCst)
            == 1)
        .await,
        "the handler queued its items under zero credit and returned",
    );
    s15::assert_stays_empty(
        &caller_seen,
        Duration::from_millis(200),
        "zero credit publishes nothing before the grant",
    )
    .await;

    // A valid STREAM_GRANT arrives — exactly the two credits the queued
    // items need.
    assert!(
        serve.inject_inbound_for_test(s13::inbound(
            session_id,
            caller.node_id(),
            caller_origin,
            s13::grant_frame(caller_origin, 42, 2),
        )),
        "the bridge accepted the STREAM_GRANT",
    );

    // Rows 1+2+7's content observation: BOTH queued items publish IN ORDER
    // — asserted by body bytes and wire sequence, never by counting.
    assert!(
        s13::wait_for(Duration::from_secs(30), || caller_seen
            .lock()
            .len()
            >= 2)
        .await,
        "the same-org queued items publish IN ORDER after the grant — a post-close \
         discard of queued chunks leaves the caller with its terminal only",
    );
    {
        let seen = caller_seen.lock();
        assert_eq!(
            (
                s15::response_of(&seen[0]).body.as_ref(),
                s15::response_of(&seen[1]).body.as_ref(),
            ),
            (
                b"item-A-CONTENT-17".as_slice(),
                b"item-B-content-29".as_slice()
            ),
            "the same-org items publish IN ORDER — body bytes and wire sequence",
        );
    }

    // …then EXPLICIT COMPLETION whose terminal frame carries the exact wire
    // content: status Ok + the `nrpc-streaming: end` marker.
    assert!(
        s13::wait_for(Duration::from_secs(30), || caller_seen
            .lock()
            .len()
            >= 3)
        .await,
        "the explicit-completion terminal follows the items at the authenticated \
         receiving endpoint",
    );
    {
        let seen = caller_seen.lock();
        assert_eq!(seen.len(), 3, "exactly the two items and ONE terminal frame");
        let terminal = s15::response_of(&seen[2]);
        assert_eq!(
            (terminal.status, terminal.headers, terminal.body.as_ref()),
            (
                RpcStatus::Ok,
                vec![(
                    HEADER_NRPC_STREAMING.to_string(),
                    HEADER_NRPC_STREAMING_END.to_vec()
                )],
                b"".as_slice(),
            ),
            "the same-org terminal frame's exact wire content is status Ok + the \
             `nrpc-streaming: end` marker",
        );
    }
    // …and it stays exactly one terminal, ever.
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert_eq!(caller_seen.lock().len(), 3, "exactly one terminal frame, ever");
}

/// S1_R Row 7's CROSS-ORG twin (the granted / other-org authority intent
/// shape). Same observations as
/// [`completed_stream_drains_queued_items_in_order_with_content_and_end_terminal`]
/// under `OrgAdmission::CrossOrgGranted`: ≥2 content-labelled items queued
/// under zero credit by a returning handler, a valid `STREAM_GRANT`, the
/// items IN ORDER (body bytes + sequence), then EXACTLY ONE terminal frame
/// with the exact wire content (status `Ok` + `nrpc-streaming: end`) at the
/// authenticated receiving endpoint.
///
/// Red witnesses (the S1_R brief Appendix inverses): **A2b** reddens "the
/// cross-org queued items publish IN ORDER" below; **A2c** reddens "the
/// cross-org terminal frame's exact wire content" below.
#[tokio::test]
async fn cross_org_completed_stream_drains_correlated_items_with_end_terminal() {
    use net::adapter::net::cortex::rpc::{HEADER_NRPC_STREAMING, HEADER_NRPC_STREAMING_END};

    let server = fixture::build_node_with(EntityKeypair::from_bytes([0x82u8; 32])).await;
    let caller_kp = s15::caller_keypair(0x29);
    let caller = fixture::build_node_with(caller_kp.clone()).await;
    fixture::bring_up(&caller, &server).await;
    let (org_b, _auth, _dir) = s14::install_authority_owned(&server, "s1r-r7");
    // The CROSS-ORG authority shape: the caller is a member of org A with a
    // B→A `INVOKE` capability grant for the exact provider.
    let org_a = OrgKeypair::from_bytes([0x7Au8; 32]);

    let returned = Arc::new(AtomicUsize::new(0));
    let dropped = Arc::new(AtomicBool::new(false));
    let serve = server
        .serve_rpc_granted_streaming(
            s14::SERVICE,
            Arc::new(s14::QueueChunksAndReturn {
                chunks: vec![b"cross-item-ALPHA-31", b"cross-item-BETA-47"],
                returned: Arc::clone(&returned),
                dropped: Arc::clone(&dropped),
            }),
            Arc::new(|_| true),
        )
        .expect("serve granted streaming");

    let caller_origin = caller.origin_hash();
    let reply_channel =
        ChannelName::new(&format!("{}.replies.{caller_origin:016x}", s14::SERVICE)).unwrap();
    let (caller_disp, caller_seen) = s15::recorder();
    assert!(caller
        .register_rpc_inbound(reply_channel.hash(), caller_disp)
        .is_some());

    let intent = fixture::cross_org_intent(
        caller_kp,
        &org_a,
        &org_b,
        server.entity_id().clone(),
        s14::SERVICE,
    );
    let binding = server
        .peer_session_binding(caller.node_id())
        .expect("the live session carries its binding (1.1a)");
    let session_id = server
        .peer_session_id(caller.node_id())
        .expect("the live session id");
    let (frame, _req) = s14::mint_ss_opening(&intent, binding, 43, caller_origin, Some(0), b"open");
    assert!(
        serve.inject_inbound_for_test(s13::inbound(
            session_id,
            caller.node_id(),
            caller_origin,
            frame
        )),
        "the bridge accepted the cross-org opening",
    );
    assert!(
        s13::wait_for(Duration::from_secs(10), || returned
            .load(Ordering::SeqCst)
            == 1)
        .await,
        "the cross-org handler queued its items under zero credit and returned",
    );
    s15::assert_stays_empty(
        &caller_seen,
        Duration::from_millis(200),
        "zero credit publishes nothing before the grant",
    )
    .await;

    assert!(
        serve.inject_inbound_for_test(s13::inbound(
            session_id,
            caller.node_id(),
            caller_origin,
            s13::grant_frame(caller_origin, 43, 2),
        )),
        "the bridge accepted the STREAM_GRANT",
    );

    // The cross-org items — correlated to THIS call — publish IN ORDER.
    assert!(
        s13::wait_for(Duration::from_secs(30), || caller_seen
            .lock()
            .len()
            >= 2)
        .await,
        "the cross-org queued items publish IN ORDER after the grant — a post-close \
         discard of queued chunks leaves the caller with its terminal only",
    );
    {
        let seen = caller_seen.lock();
        assert_eq!(
            (
                s15::response_of(&seen[0]).body.as_ref(),
                s15::response_of(&seen[1]).body.as_ref(),
            ),
            (
                b"cross-item-ALPHA-31".as_slice(),
                b"cross-item-BETA-47".as_slice()
            ),
            "the cross-org items publish IN ORDER — body bytes and wire sequence",
        );
    }

    assert!(
        s13::wait_for(Duration::from_secs(30), || caller_seen
            .lock()
            .len()
            >= 3)
        .await,
        "the explicit-completion terminal follows the cross-org items at the \
         authenticated receiving endpoint",
    );
    {
        let seen = caller_seen.lock();
        assert_eq!(seen.len(), 3, "exactly the two items and ONE terminal frame");
        let terminal = s15::response_of(&seen[2]);
        assert_eq!(
            (terminal.status, terminal.headers, terminal.body.as_ref()),
            (
                RpcStatus::Ok,
                vec![(
                    HEADER_NRPC_STREAMING.to_string(),
                    HEADER_NRPC_STREAMING_END.to_vec()
                )],
                b"".as_slice(),
            ),
            "the cross-org terminal frame's exact wire content is status Ok + the \
             `nrpc-streaming: end` marker",
        );
    }
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert_eq!(caller_seen.lock().len(), 3, "exactly one terminal frame, ever");
}

/// S1_R Row 5 (F-5's closure) — receiver-side attribution across SESSION
/// REPLACEMENT: a response arriving after the replacement reaches the LIVE
/// session's endpoint, and NOTHING reaches the REPLACED session's endpoint —
/// both asserted at the receiving endpoint's own attribution
/// (`RpcInboundEvent::session_id`, the AEAD-verified receiving incarnation).
/// The response is emitted strictly AFTER the replacement (a parked PUBLIC
/// handler's explicit post-replacement send; protected calls retire at the
/// replacement boundary and their terminal races the peer transition).
///
/// Red witness demanded by the row: the S1_R brief Appendix inverse **A5**
/// (the streaming emitter's `receiving_session_id` forced to `0` at
/// `mesh_rpc.rs`). The receipt outcome for this witness is recorded in
/// `docs/internal/spikes/org-streaming/S1_REPORT.md` §3.
#[tokio::test]
async fn response_after_session_replacement_reaches_only_the_live_session() {
    // The pair's roles are asymmetric ON PURPOSE: `accept()` is only legal
    // before `start()` (the responder side has no per-pending registry), so
    // the RESPONDER (the server) never starts and every re-handshake is
    // legal — while the CALLER runs its dispatch loop so the receiving
    // endpoint can actually RECORD deliveries. The provider pins the caller
    // exactly as a signature-verified direct announcement would
    // (`test_pin_peer_entity`, first-write-wins like the dispatch pin).
    let server = fixture::build_node_with(EntityKeypair::from_bytes([0x83u8; 32])).await;
    let caller_kp = s15::caller_keypair(0x28);
    let caller = fixture::build_node_with(caller_kp.clone()).await;
    fixture::connect_no_start(&caller, &server).await;
    server.test_pin_peer_entity(caller.node_id(), caller.entity_id().clone());
    caller.start();

    // A PUBLIC server-streaming call that PARKS with its sink handed out —
    // every response is the witness's explicit post-replacement decision.
    let holder = Arc::new(s14::SinkHolder::new());
    let serve = server
        .serve_rpc_streaming(s14::SERVICE, holder.clone())
        .expect("serve public streaming");

    let caller_origin = caller.origin_hash();
    let reply_channel =
        ChannelName::new(&format!("{}.replies.{caller_origin:016x}", s14::SERVICE)).unwrap();
    let (caller_disp, caller_seen) = s15::recorder();
    assert!(caller
        .register_rpc_inbound(reply_channel.hash(), caller_disp)
        .is_some());

    // The call opens on the ORIGINAL session.
    let session1 = server
        .peer_session_id(caller.node_id())
        .expect("the live session id");
    let replaced_endpoint = caller
        .peer_session_id(server.node_id())
        .expect("the caller-side receiving incarnation");
    let open = s13::ss_request(s14::SERVICE, 0, b"open");
    assert!(
        serve.inject_inbound_for_test(s13::inbound(
            session1,
            caller.node_id(),
            caller_origin,
            s13::request_frame(caller_origin, 42, &open),
        )),
        "the bridge accepted the opening",
    );
    assert!(
        s13::wait_for(Duration::from_secs(10), || holder
            .started
            .load(Ordering::SeqCst)
            == 1)
        .await,
        "the call is live on the original session",
    );
    s15::assert_stays_empty(
        &caller_seen,
        Duration::from_millis(200),
        "the parked call emits nothing before the replacement",
    )
    .await;

    // RE-HANDSHAKE: a new establishment replaces the session (public calls
    // survive it — only PROTECTED calls retire at the boundary).
    fixture::connect_no_start(&caller, &server).await;
    let live_endpoint = caller
        .peer_session_id(server.node_id())
        .expect("the live session id");
    assert_ne!(
        live_endpoint, replaced_endpoint,
        "a re-handshake is a new establishment",
    );
    assert!(
        !holder.dropped.load(Ordering::SeqCst),
        "the public call survives the replacement (only protected calls retire)",
    );

    // THE RESPONSE — emitted strictly AFTER the replacement.
    let sink = holder.sink.lock().take().expect("the parked handler's sink");
    sink.send(Bytes::from_static(b"post-replacement-CONTENT"));
    assert!(
        s13::wait_for(Duration::from_secs(30), || !caller_seen
            .lock()
            .is_empty())
        .await,
        "the post-replacement response arrives at the receiving endpoint",
    );
    let seen = caller_seen.lock().clone();
    assert!(
        seen.iter()
            .any(|ev| s15::response_of(ev).body.as_ref() == b"post-replacement-CONTENT"),
        "the response is the post-replacement item the live handler sent",
    );
    // Receiver-side attribution on BOTH endpoints.
    for ev in &seen {
        assert_eq!(
            ev.session_id, live_endpoint,
            "the LIVE session's endpoint receives the post-replacement response",
        );
    }
    assert!(
        !seen.iter().any(|ev| ev.session_id == replaced_endpoint),
        "the REPLACED session's endpoint receives nothing",
    );
}
