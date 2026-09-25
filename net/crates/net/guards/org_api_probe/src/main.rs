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

use futures::StreamExt;
use net::adapter::net::behavior::org_admission::{AdmissionContext, AdmissionDenied, OrgAdmission};
use net::adapter::net::cortex::rpc::{
    RequestStream, RpcCancellationToken, RpcResponseSink, RpcStreamingContext, TraceContext,
};
use net::adapter::net::identity::EntityId;
use net::adapter::net::mesh_rpc::{
    CallOptions, ClientStreamCallRaw, DuplexCallRaw, RoutingPolicy, RpcStream,
};

use net_sdk::mesh::Mesh;
use net_sdk::mesh_rpc::{
    CallOptionsTyped, ClientStreamCallTyped, Codec, DuplexCallTyped, RequestStreamTyped,
    ResponseSinkTyped, RpcError, RpcStreamTyped, ServeError, ServeHandle,
};
use net_sdk::org::{
    serve_org_client_stream_bytes_node, serve_org_duplex_bytes_node,
    serve_org_streaming_bytes_node, CapabilityAuthorityId, CoarseAdmissionReason, OrgAccess,
    OrgCaller, OrgClient, OrgClientStreamCall, OrgCredentials, OrgDuplexCall, OrgDuplexSink,
    OrgDuplexStream, OrgHandlerError, OrgId, OrgProofIntent, OrgRevocationState, OrgSdkError,
    OrgStream, OrgStreamRaw,
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

// ============================================================================
// `net_sdk::org` Stage 3 — the streaming verb facade (spec 4.3's rows).
// ============================================================================

/// `OrgClient::{call_streaming, call_streaming_bytes,
/// call_streaming_bytes_deadline, call_client_stream,
/// call_client_stream_bytes_deadline, call_duplex,
/// call_duplex_bytes_deadline}` — the §4.3 caller rows and their
/// `*_bytes_deadline` binding seams (`deadline_ms == 0` ⇒ the facade default
/// 300 s, never "none"). Every handle's shape is pinned by its annotated
/// return position. Never invoked.
async fn pin_org_streaming_calls(
    client: &OrgClient,
    request: &Req,
) -> Result<
    (
        OrgStream<Resp>,
        OrgStreamRaw,
        RpcStream,
        OrgClientStreamCall<Req, Resp>,
        ClientStreamCallRaw,
        OrgDuplexCall<Req, Resp>,
        DuplexCallRaw,
    ),
    OrgSdkError,
> {
    let typed: OrgStream<Resp> = client.call_streaming::<Req, Resp>("svc", request).await?;
    let raw: OrgStreamRaw = client.call_streaming_bytes("svc", Bytes::new()).await?;
    let raw_stream: RpcStream = client
        .call_streaming_bytes_deadline("svc", Bytes::new(), 0, 0)
        .await?;
    let cs: OrgClientStreamCall<Req, Resp> = client.call_client_stream::<Req, Resp>("svc").await?;
    let raw_cs: ClientStreamCallRaw = client
        .call_client_stream_bytes_deadline("svc", 0, 0)
        .await?;
    let dx: OrgDuplexCall<Req, Resp> = client.call_duplex::<Req, Resp>("svc").await?;
    let raw_dx: DuplexCallRaw = client.call_duplex_bytes_deadline("svc", 0, 0).await?;
    Ok((typed, raw, raw_stream, cs, raw_cs, dx, raw_dx))
}

/// `Mesh::{serve_org_streaming, serve_org_streaming_bytes,
/// serve_org_client_stream, serve_org_client_stream_bytes, serve_org_duplex,
/// serve_org_duplex_bytes}` — the §4.3 serve rows over their byte rows. Each
/// verb is referenced as a VALUE (name + generic arity pinned) and then
/// applied with an annotated handler closure: `OrgCaller` FIRST, then the
/// typed handle or the raw piece — the parameter order is the pin.
fn pin_org_streaming_serves(
    mesh: &Mesh,
) -> Result<
    (
        ServeHandle,
        ServeHandle,
        ServeHandle,
        ServeHandle,
        ServeHandle,
        ServeHandle,
    ),
    ServeError,
> {
    let serve_ss = Mesh::serve_org_streaming::<Req, Resp, _, _>;
    let typed_ss = serve_ss(
        mesh,
        "svc.ss",
        OrgAccess::SameOrg,
        |_caller: OrgCaller, _req: Req, sink: ResponseSinkTyped<Resp>| async move {
            sink.send(&0)?;
            Ok::<(), String>(())
        },
    )?;
    let serve_ss_bytes = Mesh::serve_org_streaming_bytes::<_, _>;
    let raw_ss = serve_ss_bytes(
        mesh,
        "svc.ss.raw",
        OrgAccess::SameOrg,
        |_caller: OrgCaller, _body: Bytes, _sink: RpcResponseSink| async move {
            Ok::<(), OrgHandlerError>(())
        },
    )?;
    let serve_cs = Mesh::serve_org_client_stream::<Req, Resp, _, _>;
    let typed_cs = serve_cs(
        mesh,
        "svc.cs",
        OrgAccess::Granted,
        |_caller: OrgCaller, _requests: RequestStreamTyped<Req>| async move { Ok::<Resp, String>(0) },
    )?;
    let serve_cs_bytes = Mesh::serve_org_client_stream_bytes::<_, _>;
    let raw_cs = serve_cs_bytes(
        mesh,
        "svc.cs.raw",
        OrgAccess::SameOrg,
        |_caller: OrgCaller, _requests: RequestStream| async move {
            Ok::<Bytes, OrgHandlerError>(Bytes::new())
        },
    )?;
    let serve_dx = Mesh::serve_org_duplex::<Req, Resp, _, _>;
    let typed_dx = serve_dx(
        mesh,
        "svc.dx",
        OrgAccess::SameOrg,
        |_caller: OrgCaller, _requests: RequestStreamTyped<Req>, sink: ResponseSinkTyped<Resp>| async move {
            sink.send(&0)?;
            Ok::<(), String>(())
        },
    )?;
    let serve_dx_bytes = Mesh::serve_org_duplex_bytes::<_, _>;
    let raw_dx = serve_dx_bytes(
        mesh,
        "svc.dx.raw",
        OrgAccess::SameOrg,
        |_caller: OrgCaller, _requests: RequestStream, _sink: RpcResponseSink| async move {
            Ok::<(), OrgHandlerError>(())
        },
    )?;
    Ok((typed_ss, raw_ss, typed_cs, raw_cs, typed_dx, raw_dx))
}

/// `net_sdk::org::{serve_org_streaming_bytes_node,
/// serve_org_client_stream_bytes_node, serve_org_duplex_bytes_node}` — the
/// binding seams, referenced as values (generic arity pinned) and applied
/// once each with annotated closures.
fn pin_org_streaming_node_seams(
    node: std::sync::Arc<net::adapter::net::MeshNode>,
) -> Result<(ServeHandle, ServeHandle, ServeHandle), ServeError> {
    let ss_node = serve_org_streaming_bytes_node::<_, _>;
    let ss = ss_node(
        node.clone(),
        "svc",
        OrgAccess::SameOrg,
        |_caller: OrgCaller, _body: Bytes, _sink: RpcResponseSink| async move {
            Ok::<(), OrgHandlerError>(())
        },
    )?;
    let cs_node = serve_org_client_stream_bytes_node::<_, _>;
    let cs = cs_node(
        node.clone(),
        "svc",
        OrgAccess::Granted,
        |_caller: OrgCaller, _requests: RequestStream| async move {
            Ok::<Bytes, OrgHandlerError>(Bytes::new())
        },
    )?;
    let dx_node = serve_org_duplex_bytes_node::<_, _>;
    let dx = dx_node(
        node,
        "svc",
        OrgAccess::SameOrg,
        |_caller: OrgCaller, _requests: RequestStream, _sink: RpcResponseSink| async move {
            Ok::<(), OrgHandlerError>(())
        },
    )?;
    Ok((ss, cs, dx))
}

/// The §4.3 wrapping types: `OrgStream`, `OrgStreamRaw`, `OrgClientStreamCall`
/// and `OrgDuplexCall` (with its `into_split` halves `OrgDuplexSink` /
/// `OrgDuplexStream`). Their STREAM ITEMS are the frozen error vocabulary
/// (`Result<_, OrgSdkError>` throughout) and their handle methods are pinned
/// by annotated awaits. Never invoked.
async fn pin_org_stream_wrappers(
    mut typed: OrgStream<Resp>,
    mut raw: OrgStreamRaw,
    mut cs: OrgClientStreamCall<Req, Resp>,
    dx: OrgDuplexCall<Req, Resp>,
) {
    let _item: Option<Result<Resp, OrgSdkError>> = typed.next().await;
    let _raw_item: Option<Result<Bytes, OrgSdkError>> = raw.next().await;
    let _send: Result<(), OrgSdkError> = cs.send(&Req::new()).await;
    let _finish: Result<Resp, OrgSdkError> = cs.finish().await;
    let (mut sink, mut stream): (OrgDuplexSink<Req>, OrgDuplexStream<Resp>) = dx.into_split();
    let _duplex_item: Option<Result<Resp, OrgSdkError>> = stream.next().await;
    let _sink_send: Result<(), OrgSdkError> = sink.send(&Req::new()).await;
    let _sink_close: Result<(), OrgSdkError> = sink.finish_sending().await;
}

/// The probe body — moved verbatim out of `main` so the test harness can
/// EXECUTE it (TESTS-6: CI runs `cargo check` + `cargo test` on this crate,
/// and a runtime assert sealed inside a never-run `main` is not a witness).
fn probe() {
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
    let unrun: [*const (); 11] = [
        pin_org_bind as *const (),
        pin_org_calls as *const (),
        pin_org_serve as *const (),
        pin_org_sdk_error as *const (),
        pin_org_proof_intent as *const (),
        pin_typed_streaming_serves as *const (),
        pin_typed_streaming_calls as *const (),
        pin_org_streaming_calls as *const (),
        pin_org_streaming_serves as *const (),
        pin_org_streaming_node_seams as *const (),
        pin_org_stream_wrappers as *const (),
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

fn main() {
    probe();
}

#[cfg(test)]
mod repair_witnesses {
    //! TESTS-5 + TESTS-6 — the probe's two missing halves.
    //!
    //! TESTS-6: every runtime claim this crate makes now EXECUTES. CI pinned
    //! the probe with `cargo check` alone, which compiles the pins — and the
    //! one `assert!` inside [`super::pin_rpc_streaming_context`] — without
    //! ever running them; the behavioral claim ("the constructor fabricates
    //! no admitted org facts") rested on a never-executed assert.
    //!
    //! TESTS-5: `MANIFEST` (the pin inventory) is BOUND to the pins in
    //! `main.rs` in both directions, with a pin-count floor. Before this, the
    //! two drifted silently in one direction already — names pinned here and
    //! absent from `MANIFEST`. The binding is LEXICAL, in the honest sense
    //! `check-roster.py` documents for itself:
    //!
    //! * code → manifest: every `net::…` / `net_sdk::…` path this file names
    //!   (brace-group `use` lists expanded, comments and string literals
    //!   stripped) must be covered by a MANIFEST line — the exact name, a
    //!   `Type::method` pin under it, or the item a pinned `::Variant` path
    //!   extends. This is the direction the filed defect broke in.
    //! * manifest → code: every MANIFEST line must still be reached by the
    //!   pinned code — an extracted path that is the name itself or a
    //!   `…::Variant` extension of it, or the name's final segment standing
    //!   in the code as a whole word outside comments and strings (a middle
    //!   `a::leaf::b` path segment does NOT count, and a turbofish
    //!   `Type::method::<…>` is not a segment continuation, so `net_sdk::
    //!   org::…` cannot witness a pin named `…::org` while `Mesh::org` is
    //!   witnessed by its real value reference). A method pin whose spelling
    //!   shares a word with unrelated code could still pass this direction —
    //!   that is the stated limit of a lexical check, not a claim of
    //!   parse-level equality.
    //! * floor: the inventory's size is floored at the post-repair count, so
    //!   a deletion that keeps both sides "in sync" still reddens.

    use super::*;
    use std::collections::BTreeSet;

    const MANIFEST: &str = include_str!("../MANIFEST");
    const SOURCE: &str = include_str!("main.rs");

    /// The inventory's size after the TESTS-5 repair: the 66 names the review
    /// counted, plus the eight this file pinned and `MANIFEST` missed
    /// (`MeshNode`, `Bytes`, the `RequestStream`/`RpcResponseSink` pair and
    /// the `ClientStreamCallRaw`/`DuplexCallRaw`/`RpcStream` trio, and the
    /// `net_sdk::org::OrgRevocationState` spelling of the floors type). Any
    /// future shrink must pass this floor deliberately.
    const PIN_COUNT_FLOOR: usize = 74;

    #[test]
    fn manifest_lists_exactly_the_names_this_probe_pins() {
        let code = strip_comments_and_strings(SOURCE);
        let pinned = extract_net_paths(&code);
        let manifest: Vec<&str> = MANIFEST
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .collect();

        let mut seen = BTreeSet::new();
        for m in &manifest {
            assert!(
                seen.insert(*m),
                "MANIFEST lists `{m}` more than once — the inventory is one name per line"
            );
        }

        // code -> manifest: a name pinned here must be named there.
        for p in &pinned {
            assert!(
                manifest.iter().any(|m| {
                    *m == p.as_str()
                        || m.starts_with(&format!("{p}::"))
                        || p.starts_with(&format!("{m}::"))
                }),
                "main.rs pins `{p}` but MANIFEST names neither it nor a method/variant \
                 of it — add `{p}` to MANIFEST (the filed TESTS-5 defect is exactly \
                 this direction)"
            );
        }

        // manifest -> code: a name inventoried there must still be pinned
        // here — either an extracted path reaches it (the exact name or a
        // `…::Variant` extension of it), or its final segment stands in the
        // code as a real name (see `leaf_witnessed`, and its stated limits).
        for m in &manifest {
            let extended = pinned
                .iter()
                .any(|p| p.as_str() == *m || p.starts_with(&format!("{m}::")));
            assert!(
                extended || leaf_witnessed(&code, m),
                "MANIFEST names `{m}` but no pinned code reaches or mentions its name — \
                 restore the pin or remove the line from MANIFEST"
            );
        }
    }

    #[test]
    fn manifest_holds_the_pin_count_floor() {
        let count = MANIFEST
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .count();
        assert!(
            count >= PIN_COUNT_FLOOR,
            "MANIFEST holds {count} pins, floor is {PIN_COUNT_FLOOR} — a mass \
             deletion that stays self-consistent must not pass; lower the floor \
             deliberately or restore the pins"
        );
    }

    /// TESTS-6's named behavioral claim (ledger C1 / Q2): the stable
    /// `RpcStreamingContext` constructor creates NO admitted org facts —
    /// admission facts originate at the verifier.
    #[test]
    fn rpc_streaming_context_new_fabricates_no_admitted_org_facts() {
        let context = pin_rpc_streaming_context();
        assert!(
            context.org_admission.is_none(),
            "RpcStreamingContext::new must create no admitted org facts",
        );
    }

    /// TESTS-6: the whole probe body executes — every construction, every
    /// exhaustive match, and the runtime asserts sealed inside the pins.
    #[test]
    fn the_probe_body_executes_in_the_test_harness() {
        probe();
    }

    /// Comments, string literals and CHAR literals blanked (contents removed,
    /// delimiters and newlines kept, so offsets stay honest). `//` inside a
    /// string does not start a comment, `"` inside a comment does not start a
    /// string, and `'x'` / `'\''` char literals — which this very file
    /// contains — do not smuggle a `"` into string state (a lifetime like
    /// `'a` passes through untouched).
    fn strip_comments_and_strings(src: &str) -> String {
        let b = src.as_bytes();
        let mut out = String::with_capacity(src.len());
        let mut i = 0;
        while i < b.len() {
            if b[i] == b'\''
                && (src[i..].starts_with("'\\")
                    || (i + 2 < b.len() && b[i + 2] == b'\'' && b[i + 1] != b'\n'))
            {
                out.push(' ');
                i += 1;
                if i < b.len() && b[i] == b'\\' {
                    i += 1;
                }
                if i < b.len() {
                    i += 1;
                }
                if i < b.len() && b[i] == b'\'' {
                    i += 1;
                }
                continue;
            }
            match b[i] {
                b'"' => {
                    out.push('"');
                    i += 1;
                    while i < b.len() {
                        match b[i] {
                            b'\\' => {
                                out.push(' ');
                                i += 2;
                            }
                            b'"' => {
                                out.push('"');
                                i += 1;
                                break;
                            }
                            _ => {
                                if b[i] == b'\n' {
                                    out.push('\n');
                                } else {
                                    out.push(' ');
                                }
                                i += 1;
                            }
                        }
                    }
                }
                b'/' if i + 1 < b.len() && b[i + 1] == b'/' => {
                    while i < b.len() && b[i] != b'\n' {
                        i += 1;
                    }
                }
                _ => {
                    out.push(src[i..].chars().next().expect("non-empty remainder"));
                    i += src[i..]
                        .chars()
                        .next()
                        .expect("non-empty remainder")
                        .len_utf8();
                }
            }
        }
        out
    }

    /// Every `net::…` / `net_sdk::…` path in the stripped source, with
    /// brace-group `use` lists (`prefix::{a, b}`) expanded to `prefix::a`,
    /// `prefix::b`.
    fn extract_net_paths(code: &str) -> BTreeSet<String> {
        let b = code.as_bytes();
        let mut out = BTreeSet::new();
        let mut i = 0;
        while i < b.len() {
            if !(b[i].is_ascii_alphabetic() || b[i] == b'_') {
                i += 1;
                continue;
            }
            let start = i;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                i += 1;
            }
            let mut segs: Vec<&str> = vec![&code[start..i]];
            loop {
                if !code[i..].starts_with("::") {
                    break;
                }
                let after = &code[i + 2..];
                if after.starts_with('{') {
                    let end = after.find('}').expect("balanced use brace group");
                    for entry in after[1..end].split(',') {
                        let entry = entry.trim();
                        if entry.is_empty() {
                            continue;
                        }
                        let first = entry.split("::").next().expect("non-empty entry");
                        let p = format!("{}::{first}", segs.join("::"));
                        if p.starts_with("net::") || p.starts_with("net_sdk::") {
                            out.insert(p);
                        }
                    }
                    i += 2 + end + 1;
                    segs.clear();
                    break;
                }
                let seg_start = i + 2;
                let mut j = seg_start;
                while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'_') {
                    j += 1;
                }
                if j == seg_start {
                    break; // turbofish `::<…>` or dangling `::`: the path ends
                }
                segs.push(&code[seg_start..j]);
                i = j;
            }
            if !segs.is_empty() {
                let p = segs.join("::");
                if p.starts_with("net::") || p.starts_with("net_sdk::") {
                    out.insert(p);
                }
            }
        }
        out
    }

    /// The manifest line's final segment appears in the pinned code as a
    /// whole word — as a final path segment, a method reference/call, or a
    /// bare name — but NOT as a middle `a::leaf::b` segment (so `net_sdk::
    /// org::OrgId` cannot witness a pin named `…::org`). A turbofish
    /// `Type::method::<…>` is NOT a middle segment: what follows the `::`
    /// must be an identifier for the segment to continue.
    fn leaf_witnessed(code: &str, manifest_line: &str) -> bool {
        let leaf = manifest_line.rsplit("::").next().expect("non-empty name");
        let bytes = code.as_bytes();
        let mut from = 0;
        while let Some(off) = code[from..].find(leaf) {
            let at = from + off;
            let before_ok =
                at == 0 || !(bytes[at - 1].is_ascii_alphanumeric() || bytes[at - 1] == b'_');
            let after = at + leaf.len();
            let after_ok = after >= bytes.len()
                || !(bytes[after].is_ascii_alphanumeric() || bytes[after] == b'_');
            let continues_segment = code[after..].strip_prefix("::").is_some_and(|rest| {
                rest.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
            });
            let middle_segment = code[..at].ends_with("::") && continues_segment;
            if before_ok && after_ok && !middle_segment {
                return true;
            }
            from = after;
        }
        false
    }
}
