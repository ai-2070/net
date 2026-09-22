//! Pins the public org + typed-streaming API that
//! `docs/internal/plans/ORG_SCOPED_STREAMING_PLAN.md` will change, from
//! OUTSIDE the workspace (Stage 0, slice 0.2).
//!
//! Every item below is named, constructed, destructured or matched
//! exhaustively by an external consumer, so a source-breaking change to it
//! fails THIS crate's `cargo check` even when every in-workspace caller was
//! updated in the same commit. `MANIFEST` beside this file lists the same
//! names one per line.
//!
//! What each pin is for — the Compatibility-ledger rows of the plan
//! (`ORG_SCOPED_STREAMING_PLAN.md:813-836`) that are expected to break it:
//!
//! * **C1 (realized in Stage 1 slice 1.3)** — [`pin_rpc_streaming_context`]:
//!   the `RpcStreamingContext { .. }` literal. Adding `org_admission` and
//!   `#[non_exhaustive]` (approved Q2) makes this literal illegal from
//!   outside the defining crate. The named-break doctrine applied: the
//!   literal STOPPED compiling and this pin was UPDATED to the stable
//!   constructor in the same commit as the break (not deleted).
//! * **C3** — [`pin_admission_context`]: the `AdmissionContext { .. }`
//!   literal, including `is_unary: true`. Replacing `is_unary` with
//!   `shape: RpcCallShape` and adding `#[non_exhaustive]` (approved Q7) breaks
//!   both the field and the literal. Expected to FAIL in Stage 1 by design.
//! * **C4** — [`pin_admission_denied`]: an exhaustive external `match` over
//!   `AdmissionDenied`. The seven new denial variants and `#[non_exhaustive]`
//!   break it. Expected to FAIL in Stage 1 by design.
//! * **C11** — [`pin_call_options`]: the `CallOptions { .. }` literal,
//!   including `org_proof_intent`. C11 only WIDENS what the streaming callers
//!   accept, so this literal is expected to keep compiling; it is here because
//!   any field added to `CallOptions` for the streaming work would be a
//!   silent-to-the-workspace break of it.
//!
//! Everything else is a no-break pin: it must keep compiling across all five
//! stages, and a diff that changes one of these signatures has to justify
//! itself here first.
//!
//! Nothing runs a mesh. The verbs that need a live node are named as values
//! and never invoked — the same device as
//! `../../fixtures_off_probe/src/main.rs:18`, except that these casts sit
//! beside fully type-annotated call expressions, so parameter and return types
//! ARE pinned (a bare `as *const ()` cast pins only the name).

use net::adapter::net::behavior::org_admission::{AdmissionContext, AdmissionDenied, OrgAdmission};
use net::adapter::net::cortex::rpc::{RpcCancellationToken, RpcStreamingContext, TraceContext};
use net::adapter::net::identity::EntityId;
use net::adapter::net::mesh_rpc::{CallOptions, RoutingPolicy};

use net_sdk::mesh::Mesh;
use net_sdk::mesh_rpc::{
    CallOptionsTyped, ClientStreamCallTyped, Codec, DuplexCallTyped, RequestStreamTyped,
    ResponseSinkTyped, RpcError, RpcStreamTyped, ServeError, ServeHandle,
};
use net_sdk::org::{
    CapabilityAuthorityId, CoarseAdmissionReason, OrgAccess, OrgCaller, OrgClient, OrgCredentials,
    OrgHandlerError, OrgId, OrgProofIntent, OrgRevocationState, OrgSdkError,
};
use net_sdk::Bytes;

/// Request/response types for every typed pin. Deliberately `std` types with
/// blanket serde impls: the probe pins the mesh API, and a `#[derive]` here
/// would add a `serde` dependency (and its lockfile drift) for nothing.
type Req = String;
type Resp = u64;

// ============================================================================
// `net_sdk::org` — the verb facade.
// ============================================================================

/// `Mesh::org(..) -> OrgClient`, as a typed function pointer: the coercion
/// fails if the receiver, the credential parameter or the result type moves.
fn pin_org_bind() -> fn(&Mesh, OrgCredentials) -> Result<OrgClient, OrgSdkError> {
    Mesh::org
}

/// `OrgClient::{call, call_bytes, call_exported}`. Never awaited — building
/// the future is what type-checks the signatures.
async fn pin_org_calls(client: &OrgClient) -> Result<(u64, Bytes, u64), OrgSdkError> {
    let request: Req = "probe".to_string();

    let typed: Resp = client
        .call::<Req, Resp>("probe.org.unary", &request)
        .await?;
    let raw: Bytes = client
        .call_bytes("probe.org.unary", Bytes::from_static(b"probe"))
        .await?;
    let exported: Resp = client
        .call_exported::<Req, Resp>("probe.org.exported", &request)
        .await?;

    Ok((typed, raw, exported))
}

/// `Mesh::serve_org` and `Mesh::serve_org_bytes`, plus `OrgCaller` both
/// destructured and re-built by struct literal — a new `OrgCaller` field
/// breaks this function twice.
fn pin_org_serve(mesh: &Mesh) -> Result<(ServeHandle, ServeHandle), ServeError> {
    let typed = mesh.serve_org::<Req, Resp, _, _>(
        "probe.org.unary",
        OrgAccess::SameOrg,
        |caller: OrgCaller, request: Req| async move {
            let OrgCaller {
                entity,
                acting_org,
                provider_org,
                provider,
                capability,
            } = caller;
            let rebuilt = OrgCaller {
                entity,
                acting_org,
                provider_org,
                provider,
                capability,
            };
            Ok::<Resp, String>(request.len() as u64 + u64::from(rebuilt.is_same_org()))
        },
    )?;

    let raw = mesh.serve_org_bytes(
        "probe.org.bytes",
        OrgAccess::Granted,
        |caller: OrgCaller, body: Bytes| async move {
            if body.is_empty() {
                return Err(OrgHandlerError::Application {
                    code: 0x8001,
                    message: format!("empty body from {:?}", caller.acting_org),
                });
            }
            Ok::<Bytes, OrgHandlerError>(body)
        },
    )?;

    Ok((typed, raw))
}

/// `OrgAccess`, matched exhaustively — a third access mode breaks here.
fn pin_org_access(access: OrgAccess) -> &'static str {
    match access {
        OrgAccess::SameOrg => "same-org",
        OrgAccess::Granted => "granted",
    }
}

/// `OrgHandlerError`, matched exhaustively down to the struct-variant fields.
fn pin_org_handler_error(error: OrgHandlerError) -> String {
    match error {
        OrgHandlerError::Application { code, message } => format!("app:{code:#06x}:{message}"),
        OrgHandlerError::Internal(message) => format!("internal:{message}"),
    }
}

/// `OrgSdkError`, matched exhaustively — a new error kind breaks here.
fn pin_org_sdk_error(error: OrgSdkError) -> String {
    match error {
        OrgSdkError::Credentials(e) => format!("credentials:{e}"),
        OrgSdkError::Discovery(e) => format!("discovery:{e}"),
        OrgSdkError::AdmissionDenied(reason) => {
            format!("admission_denied:{}", pin_coarse_admission_reason(reason))
        }
        OrgSdkError::Rpc(e) => format!("rpc:{e}"),
    }
}

/// `OrgProofIntent`, destructured by reference — the seventh entry in the
/// plan's list of public types whose shape may not change without a named
/// break (`ORG_SCOPED_STREAMING_PLAN.md:762-765`). By reference because a
/// literal would need real key and certificate material; destructuring still
/// fails on any added, removed or renamed field.
fn pin_org_proof_intent(intent: &OrgProofIntent) -> u64 {
    let OrgProofIntent {
        caller,
        membership,
        dispatcher,
        capability_grant,
        acting_org,
        provider_owner_org,
        provider,
        capability,
        proof_ttl_secs,
    } = intent;
    let _ = (caller, membership, dispatcher, capability_grant);
    let _ = (acting_org, provider_owner_org, provider, capability);
    *proof_ttl_secs
}

/// `CoarseAdmissionReason`, matched exhaustively. Stage 1 keeps the three
/// buckets (C4's new variants map onto them), so this pin must NOT break.
fn pin_coarse_admission_reason(reason: CoarseAdmissionReason) -> &'static str {
    match reason {
        CoarseAdmissionReason::Denied => "denied",
        CoarseAdmissionReason::NotSupported => "not_supported",
        CoarseAdmissionReason::Unavailable => "unavailable",
    }
}

/// **Ledger C4.** `AdmissionDenied`, matched exhaustively from outside the
/// crate. The enum is not `#[non_exhaustive]` today
/// (`src/adapter/net/behavior/org_admission.rs:85-86`), which is exactly what
/// makes this match legal — and what makes Stage 1's seven new variants plus
/// `#[non_exhaustive]` a source break a downstream consumer can see.
fn pin_admission_denied(denied: AdmissionDenied) -> &'static str {
    match denied {
        AdmissionDenied::NotOrgProtected => "not_org_protected",
        AdmissionDenied::MissingHeader => "missing_header",
        AdmissionDenied::MultipleHeaders => "multiple_headers",
        AdmissionDenied::MalformedProof => "malformed_proof",
        AdmissionDenied::StreamingUnsupported => "streaming_unsupported",
        AdmissionDenied::MemberBindingMismatch => "member_binding_mismatch",
        AdmissionDenied::ActingOrgMismatch => "acting_org_mismatch",
        AdmissionDenied::UnexpectedCapabilityGrant => "unexpected_capability_grant",
        AdmissionDenied::MissingCapabilityGrant => "missing_capability_grant",
        AdmissionDenied::ForeignIssuer => "foreign_issuer",
        AdmissionDenied::GranteeMismatch => "grantee_mismatch",
        AdmissionDenied::InsufficientRights => "insufficient_rights",
        AdmissionDenied::CapabilityMismatch => "capability_mismatch",
        AdmissionDenied::TargetNotCovered => "target_not_covered",
        AdmissionDenied::DispatcherGrantScope => "dispatcher_grant_scope",
        AdmissionDenied::DispatcherGrantInvalid => "dispatcher_grant_invalid",
        AdmissionDenied::MembershipInvalid => "membership_invalid",
        AdmissionDenied::MembershipRevoked => "membership_revoked",
        AdmissionDenied::CapabilityGrantInvalid => "capability_grant_invalid",
        AdmissionDenied::ProofExpired => "proof_expired",
        AdmissionDenied::BindingInvalid => "binding_invalid",
        AdmissionDenied::ProviderAuthorityUnavailable => "provider_authority_unavailable",
        AdmissionDenied::AuthorityChanged => "authority_changed",
        AdmissionDenied::Replay => "replay",
        AdmissionDenied::CallIdCollision => "call_id_collision",
        AdmissionDenied::ReplayCapacity => "replay_capacity",
        AdmissionDenied::PerCallerReplayCapacity => "per_caller_replay_capacity",
        AdmissionDenied::PerOrganizationReplayCapacity => "per_organization_replay_capacity",
        AdmissionDenied::ExternalPoolReplayCapacity => "external_pool_replay_capacity",
        AdmissionDenied::ProviderPolicyRejected => "provider_policy_rejected",
        // C4's seven Stage 1 variants (realized in slice 1.2).
        AdmissionDenied::ShapeMismatch => "shape_mismatch",
        AdmissionDenied::SessionBindingMismatch => "session_binding_mismatch",
        AdmissionDenied::DeadlineExceedsPolicy => "deadline_exceeds_policy",
        AdmissionDenied::ActiveCallOwned => "active_call_owned",
        AdmissionDenied::ActiveStreamCapacity => "active_stream_capacity",
        AdmissionDenied::Revoked => "revoked",
        AdmissionDenied::ResourceExhausted => "resource_exhausted",
        // `#[non_exhaustive]` (C4/Q7) — a future variant is not a compile
        // break downstream any more; this arm is the acknowledged cost of the
        // approved break, and its string is the pinned fallback.
        _ => "future_variant",
    }
}

// ============================================================================
// `net_sdk::mesh_rpc` — the six typed streaming entry points.
// ============================================================================

/// `serve_rpc_streaming_typed`, `serve_rpc_client_stream_typed` and
/// `serve_rpc_duplex_typed`: each referenced as a VALUE first (so the name and
/// its generic arity are pinned), then applied to a fully annotated handler
/// closure (so the parameter order, the handler shape and the result type are
/// pinned too).
fn pin_typed_streaming_serves(
    mesh: &Mesh,
) -> Result<(ServeHandle, ServeHandle, ServeHandle), ServeError> {
    let serve_rpc_streaming_typed = Mesh::serve_rpc_streaming_typed::<Req, Resp, _, _>;
    let server_streaming: ServeHandle = serve_rpc_streaming_typed(
        mesh,
        "probe.stream.ss",
        Codec::Json,
        |request: Req, sink: ResponseSinkTyped<Resp>| async move {
            sink.send(&(request.len() as u64))?;
            Ok::<(), String>(())
        },
    )?;

    let serve_rpc_client_stream_typed = Mesh::serve_rpc_client_stream_typed::<Req, Resp, _, _>;
    let client_streaming: ServeHandle = serve_rpc_client_stream_typed(
        mesh,
        "probe.stream.cs",
        Codec::Json,
        |_requests: RequestStreamTyped<Req>| async move { Ok::<Resp, String>(0) },
    )?;

    let serve_rpc_duplex_typed = Mesh::serve_rpc_duplex_typed::<Req, Resp, _, _>;
    let duplex: ServeHandle = serve_rpc_duplex_typed(
        mesh,
        "probe.stream.dx",
        Codec::Json,
        |_requests: RequestStreamTyped<Req>, sink: ResponseSinkTyped<Resp>| async move {
            sink.send(&1)?;
            Ok::<(), String>(())
        },
    )?;

    Ok((server_streaming, client_streaming, duplex))
}

/// `call_streaming_typed`, `call_client_stream_typed` and `call_duplex_typed`,
/// referenced as values and then applied with annotated arguments and
/// annotated handle types. Never awaited.
async fn pin_typed_streaming_calls(
    mesh: &Mesh,
    target_node_id: u64,
    opts: CallOptionsTyped,
) -> Result<
    (
        RpcStreamTyped<Resp>,
        ClientStreamCallTyped<Req, Resp>,
        DuplexCallTyped<Req, Resp>,
    ),
    RpcError,
> {
    let request: Req = "probe".to_string();

    let call_streaming_typed = Mesh::call_streaming_typed::<Req, Resp>;
    let server_streaming: RpcStreamTyped<Resp> = call_streaming_typed(
        mesh,
        target_node_id,
        "probe.stream.ss",
        &request,
        opts.clone(),
    )
    .await?;

    let call_client_stream_typed = Mesh::call_client_stream_typed::<Req, Resp>;
    let client_streaming: ClientStreamCallTyped<Req, Resp> =
        call_client_stream_typed(mesh, target_node_id, "probe.stream.cs", opts.clone()).await?;

    let call_duplex_typed = Mesh::call_duplex_typed::<Req, Resp>;
    let duplex: DuplexCallTyped<Req, Resp> =
        call_duplex_typed(mesh, target_node_id, "probe.stream.dx", opts).await?;

    Ok((server_streaming, client_streaming, duplex))
}

// ============================================================================
// Core public types the Compatibility ledger names. These literals are the
// break: no `..Default::default()` anywhere, so every field is spelled out and
// every added, removed or renamed field is a compile error here.
// ============================================================================

/// **Ledger C11 neighbourhood.** `CallOptions { .. }` by struct literal, and
/// the `net_sdk::mesh_rpc` re-export asserted to be the same type.
fn pin_call_options() -> CallOptions {
    let options = CallOptions {
        deadline: None,
        routing_policy: RoutingPolicy::RoundRobin,
        filter_unhealthy: true,
        trace_context: Some(pin_trace_context()),
        max_in_flight_per_target: 64,
        stream_window_initial: Some(8),
        request_window_initial: Some(8),
        request_headers: vec![("nrpc-probe".to_string(), b"1".to_vec())],
        cancel_token: None,
        org_proof_intent: None,
    };
    // The facade re-export and the core type must stay one type, not two.
    let options: net_sdk::mesh_rpc::CallOptions = options;
    options
}

/// **Ledger C1 (realized in Stage 1 slice 1.3).** The
/// `RpcStreamingContext { .. }` by struct literal — the exact
/// construction Q2's `org_admission` field plus `#[non_exhaustive]`
/// makes impossible from outside the defining crate
/// (`src/adapter/net/cortex/rpc.rs`) — stopped compiling when Q2
/// landed. This is the named break's migration path, UPDATED not
/// deleted: the stable constructor (the only external construction the
/// break leaves), which creates NO admitted org facts.
fn pin_rpc_streaming_context() -> RpcStreamingContext {
    let context = RpcStreamingContext::new(
        0,
        1,
        0,
        vec![("nrpc-probe".to_string(), b"1".to_vec())],
        RpcCancellationToken::new(),
        Some(pin_trace_context()),
    );
    // The constructor's contract (Q2): it never fabricates verified
    // attribution — admission facts originate at the verifier.
    assert!(
        context.org_admission.is_none(),
        "RpcStreamingContext::new must create no admitted org facts",
    );
    let context: net_sdk::mesh_rpc::RpcStreamingContext = context;
    context
}

/// **Ledger C3 (realized in Stage 1 slice 1.2).** The `AdmissionContext { .. }`
/// literal — including the `is_unary: bool` field — stopped compiling when Q7
/// landed: `is_unary` became `shape: RpcCallShape` and the struct became
/// `#[non_exhaustive]`. This is the named break's migration path, UPDATED not
/// deleted: the stable constructor (the only external construction the break
/// leaves), with the shape term the constructor derives from (registration
/// shape, payload flags).
fn pin_admission_context<'a>(
    authenticated_caller: &'a EntityId,
    provider: &'a EntityId,
    floors: &'a OrgRevocationState,
) -> AdmissionContext<'a> {
    AdmissionContext::new(
        OrgAdmission::OwnerDelegated,
        authenticated_caller,
        provider,
        OrgId::from_bytes([7u8; 32]),
        CapabilityAuthorityId::for_tag("nrpc:probe.org.unary"),
        1,
        [0u8; 32],
        net::adapter::net::behavior::org_call::RpcCallShape::Unary,
        false,
        false,
        None,
        floors,
        30,
    )
}

/// `OrgAdmission`, matched exhaustively: the canonical mode enum behind
/// `OrgAccess`.
fn pin_org_admission(mode: OrgAdmission) -> &'static str {
    match mode {
        OrgAdmission::PublicAuthenticated => "public_authenticated",
        OrgAdmission::OwnerDelegated => "owner_delegated",
        OrgAdmission::CrossOrgGranted => "cross_org_granted",
    }
}

/// `TraceContext { .. }` by struct literal — it rides inside both ledger
/// literals above, so a change to it breaks them silently unless it is spelled
/// out somewhere.
fn pin_trace_context() -> TraceContext {
    TraceContext {
        traceparent: "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01".to_string(),
        tracestate: String::new(),
    }
}

fn main() {
    // Constructed for real, not merely type-checked: no mesh, no runtime, no
    // node. These are the literals ledger rows C1/C3 break.
    let options = pin_call_options();
    let context = pin_rpc_streaming_context();

    let authenticated_caller = EntityId([1u8; 32]);
    let provider = EntityId([2u8; 32]);
    let floors = OrgRevocationState::empty();
    let admission = pin_admission_context(&authenticated_caller, &provider, &floors);

    // The exhaustive matches, exercised.
    let matched = [
        pin_org_access(OrgAccess::SameOrg),
        pin_org_access(OrgAccess::Granted),
        pin_coarse_admission_reason(CoarseAdmissionReason::NotSupported),
        pin_admission_denied(AdmissionDenied::StreamingUnsupported),
        pin_org_admission(OrgAdmission::OwnerDelegated),
    ];
    let handler_error = pin_org_handler_error(OrgHandlerError::Internal("probe".to_string()));

    // The verbs that need a live node: named as values, never invoked. The
    // casts pin only the names; the annotated call expressions inside each
    // function are what pin the signatures.
    let unrun: [*const (); 7] = [
        pin_org_bind as *const (),
        pin_org_calls as *const (),
        pin_org_serve as *const (),
        pin_org_sdk_error as *const (),
        pin_org_proof_intent as *const (),
        pin_typed_streaming_serves as *const (),
        pin_typed_streaming_calls as *const (),
    ];

    println!(
        "org+streaming API probe: 3 ledger surfaces constructed \
         (CallOptions literal, stream_window_initial={:?}; RpcStreamingContext \
         constructor, call_id={}, org_admission.is_none()={}; AdmissionContext \
         constructor, shape={:?}), {} exhaustive matches ({}), \
         {} signatures pinned without running a node",
        options.stream_window_initial,
        context.call_id,
        context.org_admission.is_none(),
        admission.shape,
        matched.len(),
        handler_error,
        unrun.len(),
    );
}
