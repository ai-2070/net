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
        )
        .err()
        .expect("an explicit deadline over the provider cap must be refused");
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
        )
        .expect("admitted a");
    let open_b = s13::ss_request("svc.b", 0, b"open");
    let call_b = fold_b
        .lock()
        .apply_inbound_admitted(
            &s13::inbound(0x51, 0xA, 0x1111, s13::request_frame(0x1111, 43, &open_b)),
            s13::synthetic_admitted(),
            &lifetime,
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
