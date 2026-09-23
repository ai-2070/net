//! OSDK S2 — `serve_org`: the provider verb.
//!
//! ```ignore
//! mesh.serve_org("customer.read", OrgAccess::Granted,
//!     |caller: OrgCaller, request: GetCustomer| async move {
//!         read_customer(caller, request).await
//!     })?;
//! ```
//!
//! # Access implies visibility
//!
//! Protected services are PRIVATE by default — one choice, not an
//! admission × visibility matrix:
//!
//! ```text
//! OrgAccess::SameOrg → OwnerDelegated admission + OwnerScoped encrypted
//!                      discovery   (core serve_rpc_owner_scoped)
//! OrgAccess::Granted → CrossOrgGranted admission + GrantedAudience encrypted
//!                      discovery   (core serve_rpc_granted)
//! ```
//!
//! Protected-but-publicly-discoverable registration stays available through the
//! low-level `MeshNode::serve_rpc_protected`; it is not a facade concern until a
//! consumer asks for it.
//!
//! # Provider policy IS the handler
//!
//! `serve_org` installs the trivial always-true proof policy and makes the
//! decision in the handler body, where `OrgCaller` carries the verified facts.
//! The step-11 proof-policy hook (which sees the whole `OrgCallProof`, including
//! its grant id) remains fully available on the low-level serve APIs.
//!
//! # Provisioning is separate, and registration never waits for it
//!
//! `serve_org(.., Granted, ..)` may register BEFORE a matching provider audience
//! exists. Admission protection is active immediately; the service is simply
//! encrypted and undiscoverable until `install_provider_grant_audience` is
//! called, which triggers a coherent re-announce. Failing the registration
//! instead would break valid startup ordering and dynamic grant installation.

use std::future::Future;
use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use net::adapter::net::behavior::org_admission::Admitted;
use net::adapter::net::cortex::{
    RequestStream, RpcClientStreamingHandler, RpcDuplexHandler, RpcResponseSink,
    RpcStreamingContext, RpcStreamingHandler,
};
use net::adapter::net::identity::EntityId;
use net::adapter::net::MeshNode;

use super::types::{CapabilityAuthorityId, OrgId};
use crate::mesh::Mesh;
use crate::mesh_rpc::{
    Codec, RequestStreamTyped, ResponseSinkTyped, RpcContext, RpcHandler, RpcHandlerError,
    RpcResponsePayload, RpcStatus, ServeError, ServeHandle, NRPC_TYPED_BAD_REQUEST,
    NRPC_TYPED_HANDLER_ERROR,
};

/// Who may call a protected service — the facade's name for the canonical
/// admission mode, paired with the encrypted discovery it implies.
///
/// There is no third variant: a service that is not org-protected is not an
/// `OrgAccess` service and keeps `serve_rpc` / `serve_rpc_typed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrgAccess {
    /// Members of THIS node's own organization, acting under a dispatcher
    /// grant. Canonically [`OrgAdmission::OwnerDelegated`], announced only
    /// inside the encrypted owner audience.
    ///
    /// [`OrgAdmission::OwnerDelegated`]: super::types::OrgAdmission::OwnerDelegated
    SameOrg,
    /// Members of another organization holding a capability grant THIS node's
    /// owner issued. Canonically [`OrgAdmission::CrossOrgGranted`], announced
    /// only inside the encrypted per-grant audiences.
    ///
    /// [`OrgAdmission::CrossOrgGranted`]: super::types::OrgAdmission::CrossOrgGranted
    Granted,
}

/// The provider-verified facts about an admitted call.
///
/// An exact projection of the canonical [`Admitted`] — the same five fields,
/// nothing added and nothing renamed into a new authority object. Every field
/// was verified by `verify_org_admission` before the handler ran; none is
/// caller-claimed.
///
/// A handler needing headers, packet metadata, or proof-level policy (including
/// the grant id) uses the low-level protected serve API instead — the common
/// handler deliberately never sees `RpcContext` or proof bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrgCaller {
    /// The acting entity — the caller S.
    pub entity: EntityId,
    /// The organization S acted for.
    pub acting_org: OrgId,
    /// This provider's owner organization.
    pub provider_org: OrgId,
    /// This exact provider node.
    pub provider: EntityId,
    /// The capability that was invoked.
    pub capability: CapabilityAuthorityId,
}

impl From<&Admitted> for OrgCaller {
    fn from(a: &Admitted) -> Self {
        Self {
            entity: a.caller.clone(),
            acting_org: a.acting_org,
            provider_org: a.provider_org,
            provider: a.provider.clone(),
            capability: a.capability,
        }
    }
}

impl OrgCaller {
    /// Whether this call came from THIS provider's own organization.
    pub fn is_same_org(&self) -> bool {
        self.acting_org == self.provider_org
    }
}

/// What a raw org handler may return on failure (OSDK-L R1).
///
/// The typed [`Mesh::serve_org`] maps a handler's `Err(String)` onto
/// `Application { code: NRPC_TYPED_HANDLER_ERROR }` and a decode failure onto
/// `Application { code: NRPC_TYPED_BAD_REQUEST }`. A language binding needs the
/// same expressiveness — Node throws `nrpc:app_error:0x<code>:<body>`, Go
/// returns `AppError(code, body)` — so the raw seam carries the code instead of
/// flattening every failure into one status.
///
/// Neither variant is ever an admission denial: `0x0009` is the admission
/// engine's word, and a handler cannot counterfeit it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OrgHandlerError {
    /// An application-level rejection carrying a status the caller sees.
    Application {
        /// Application status code (the `0x8000..=0xFFFF` band by convention).
        code: u16,
        /// Diagnostic body.
        message: String,
    },
    /// An internal failure — surfaced as a server error, not an app status.
    Internal(String),
}

impl From<OrgHandlerError> for RpcHandlerError {
    fn from(e: OrgHandlerError) -> Self {
        match e {
            OrgHandlerError::Application { code, message } => {
                RpcHandlerError::Application { code, message }
            }
            OrgHandlerError::Internal(message) => RpcHandlerError::Internal(message),
        }
    }
}

impl Mesh {
    /// Serve a protected, privately-discoverable service (OSDK §4).
    ///
    /// `access` selects both who may call and how the service is announced —
    /// see the module docs. The handler receives the provider-verified
    /// [`OrgCaller`] and the decoded request; returning `Err(String)` surfaces
    /// as an application error, never as an admission denial (`0x0009` is the
    /// admission engine's word, and the facade does not counterfeit it).
    ///
    /// Requires an installed node authority — a protected registration without
    /// one is refused loudly by the core.
    pub fn serve_org<Req, Resp, F, Fut>(
        &self,
        service: &str,
        access: OrgAccess,
        handler: F,
    ) -> Result<ServeHandle, ServeError>
    where
        Req: serde::de::DeserializeOwned + Send + Sync + 'static,
        Resp: serde::Serialize + Send + Sync + 'static,
        F: Fn(OrgCaller, Req) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Resp, String>> + Send + 'static,
    {
        let codec = Codec::Json;
        let inner = Arc::new(handler);
        // The typed verb IS the raw one plus JSON — one dispatch path, and the
        // codec layer is provably marshaling.
        self.serve_org_bytes(service, access, move |caller, body: Bytes| {
            let inner = inner.clone();
            async move {
                let req: Req = codec
                    .decode(&body)
                    .map_err(|e| OrgHandlerError::Application {
                        code: NRPC_TYPED_BAD_REQUEST,
                        message: format!("org handler: bad request body: {e}"),
                    })?;
                let resp =
                    inner(caller, req)
                        .await
                        .map_err(|message| OrgHandlerError::Application {
                            code: NRPC_TYPED_HANDLER_ERROR,
                            message,
                        })?;
                let out = codec.encode(&resp).map_err(|e| {
                    OrgHandlerError::Internal(format!("org handler: response encode: {e}"))
                })?;
                Ok(Bytes::from(out))
            }
        })
    }

    /// [`serve_org`](Self::serve_org) without the codec — bytes in, bytes out
    /// (OSDK-L R1).
    ///
    /// Exists for the same reason as
    /// [`call_bytes`](crate::org::OrgClient::call_bytes): a generic closure
    /// cannot cross an FFI boundary, so this is the seam every language binding
    /// registers against. The handler still receives the provider-verified
    /// [`OrgCaller`] — dropping it here would defeat the point of the verb.
    ///
    /// Access still implies visibility, and the trivial proof policy is still
    /// installed; nothing about admission changes by removing the codec.
    pub fn serve_org_bytes<F, Fut>(
        &self,
        service: &str,
        access: OrgAccess,
        handler: F,
    ) -> Result<ServeHandle, ServeError>
    where
        F: Fn(OrgCaller, Bytes) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Bytes, OrgHandlerError>> + Send + 'static,
    {
        serve_org_bytes_node(self.node().clone(), service, access, handler)
    }

    /// Serve a protected, privately-discoverable service whose response is a
    /// STREAM (OSDK §4; §4.3). The handler receives the provider-verified
    /// [`OrgCaller`], the decoded request, and a [`ResponseSinkTyped`] it
    /// emits items through; returning `Err(String)` surfaces as an
    /// application error, never as an admission denial. Everything else —
    /// access implies visibility, the trivial proof policy, registration
    /// before provisioning — is [`serve_org`](Self::serve_org)'s contract,
    /// unchanged.
    pub fn serve_org_streaming<Req, Resp, F, Fut>(
        &self,
        service: &str,
        access: OrgAccess,
        handler: F,
    ) -> Result<ServeHandle, ServeError>
    where
        Req: serde::de::DeserializeOwned + Send + Sync + 'static,
        Resp: serde::Serialize + Send + Sync + 'static,
        F: Fn(OrgCaller, Req, ResponseSinkTyped<Resp>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), String>> + Send + 'static,
    {
        let inner = Arc::new(handler);
        // The typed verb IS the bytes row plus JSON — one dispatch path, and
        // the codec layer is provably just marshaling.
        self.serve_org_streaming_bytes(
            service,
            access,
            move |caller, body: Bytes, sink: RpcResponseSink| {
                let inner = inner.clone();
                async move {
                    let req: Req =
                        Codec::Json
                            .decode(&body)
                            .map_err(|e| OrgHandlerError::Application {
                                code: NRPC_TYPED_BAD_REQUEST,
                                message: format!("org streaming handler: bad request body: {e}"),
                            })?;
                    let sink = ResponseSinkTyped::from_raw(sink, Codec::Json);
                    inner(caller, req, sink)
                        .await
                        .map_err(|message| OrgHandlerError::Application {
                            code: NRPC_TYPED_HANDLER_ERROR,
                            message,
                        })
                }
            },
        )
    }

    /// [`serve_org_streaming`](Self::serve_org_streaming) without the codec —
    /// bytes in, raw sink out (OSDK-L R1). The handler still receives the
    /// provider-verified [`OrgCaller`].
    pub fn serve_org_streaming_bytes<F, Fut>(
        &self,
        service: &str,
        access: OrgAccess,
        handler: F,
    ) -> Result<ServeHandle, ServeError>
    where
        F: Fn(OrgCaller, Bytes, RpcResponseSink) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), OrgHandlerError>> + Send + 'static,
    {
        serve_org_streaming_bytes_node(self.node().clone(), service, access, handler)
    }

    /// Serve a protected, privately-discoverable service with a STREAM OF
    /// REQUESTS and one typed response (OSDK §4; §4.3). The handler receives
    /// the provider-verified [`OrgCaller`] and a [`RequestStreamTyped`] to
    /// drain; its return value is the typed terminal response.
    pub fn serve_org_client_stream<Req, Resp, F, Fut>(
        &self,
        service: &str,
        access: OrgAccess,
        handler: F,
    ) -> Result<ServeHandle, ServeError>
    where
        Req: serde::de::DeserializeOwned + Send + Sync + Unpin + 'static,
        Resp: serde::Serialize + Send + Sync + 'static,
        F: Fn(OrgCaller, RequestStreamTyped<Req>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Resp, String>> + Send + 'static,
    {
        let inner = Arc::new(handler);
        self.serve_org_client_stream_bytes(
            service,
            access,
            move |caller, requests: RequestStream| {
                let inner = inner.clone();
                async move {
                    let requests = RequestStreamTyped::from_raw(requests, Codec::Json);
                    let resp = inner(caller, requests).await.map_err(|message| {
                        OrgHandlerError::Application {
                            code: NRPC_TYPED_HANDLER_ERROR,
                            message,
                        }
                    })?;
                    let out = Codec::Json.encode(&resp).map_err(|e| {
                        OrgHandlerError::Internal(format!(
                            "org client-stream handler: response encode: {e}"
                        ))
                    })?;
                    Ok(Bytes::from(out))
                }
            },
        )
    }

    /// [`serve_org_client_stream`](Self::serve_org_client_stream) without the
    /// codec — raw request stream in, bytes terminal out (OSDK-L R1).
    pub fn serve_org_client_stream_bytes<F, Fut>(
        &self,
        service: &str,
        access: OrgAccess,
        handler: F,
    ) -> Result<ServeHandle, ServeError>
    where
        F: Fn(OrgCaller, RequestStream) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Bytes, OrgHandlerError>> + Send + 'static,
    {
        serve_org_client_stream_bytes_node(self.node().clone(), service, access, handler)
    }

    /// Serve a protected, privately-discoverable service BIDIRECTIONALLY
    /// (OSDK §4; §4.3): the handler receives the provider-verified
    /// [`OrgCaller`], a [`RequestStreamTyped`] to drain, and a
    /// [`ResponseSinkTyped`] to emit through.
    pub fn serve_org_duplex<Req, Resp, F, Fut>(
        &self,
        service: &str,
        access: OrgAccess,
        handler: F,
    ) -> Result<ServeHandle, ServeError>
    where
        Req: serde::de::DeserializeOwned + Send + Sync + Unpin + 'static,
        Resp: serde::Serialize + Send + Sync + 'static,
        F: Fn(OrgCaller, RequestStreamTyped<Req>, ResponseSinkTyped<Resp>) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: Future<Output = Result<(), String>> + Send + 'static,
    {
        let inner = Arc::new(handler);
        self.serve_org_duplex_bytes(
            service,
            access,
            move |caller, requests: RequestStream, sink: RpcResponseSink| {
                let inner = inner.clone();
                async move {
                    let requests = RequestStreamTyped::from_raw(requests, Codec::Json);
                    let sink = ResponseSinkTyped::from_raw(sink, Codec::Json);
                    inner(caller, requests, sink).await.map_err(|message| {
                        OrgHandlerError::Application {
                            code: NRPC_TYPED_HANDLER_ERROR,
                            message,
                        }
                    })
                }
            },
        )
    }

    /// [`serve_org_duplex`](Self::serve_org_duplex) without the codec — raw
    /// request stream and sink (OSDK-L R1).
    pub fn serve_org_duplex_bytes<F, Fut>(
        &self,
        service: &str,
        access: OrgAccess,
        handler: F,
    ) -> Result<ServeHandle, ServeError>
    where
        F: Fn(OrgCaller, RequestStream, RpcResponseSink) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), OrgHandlerError>> + Send + 'static,
    {
        serve_org_duplex_bytes_node(self.node().clone(), service, access, handler)
    }
}

/// Register a protected service on a NODE — the one implementation of the serve
/// pipeline (OSDK-L N4).
///
/// [`Mesh::serve_org_bytes`] delegates here, and so does every language
/// binding, for the same reason [`OrgClient::bind_node`] exists: bindings hold
/// `Arc<MeshNode>`, and neither fabricating a throwaway [`Mesh`] nor pinning a
/// permanent one is acceptable.
///
/// `#[doc(hidden)]` — applications use `mesh.serve_org(..)`; this is the
/// binding seam, not a second public way to register a service.
#[doc(hidden)]
pub fn serve_org_bytes_node<F, Fut>(
    node: Arc<MeshNode>,
    service: &str,
    access: OrgAccess,
    handler: F,
) -> Result<ServeHandle, ServeError>
where
    F: Fn(OrgCaller, Bytes) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<Bytes, OrgHandlerError>> + Send + 'static,
{
    let raw = Arc::new(OrgBytesHandler {
        inner: Arc::new(handler),
    });
    auto_register_org_channels(&node, service);
    // The trivial proof policy: v1 decides in the handler, with the verified
    // facts in hand. The low-level API keeps the step-11 seam for providers
    // that must refuse before the replay insert.
    let policy: net::adapter::net::org_admission_gate::OrgProviderPolicy = Arc::new(|_| true);
    match access {
        OrgAccess::SameOrg => node.serve_rpc_owner_scoped(service, raw, policy),
        OrgAccess::Granted => node.serve_rpc_granted(service, raw, policy),
    }
}

/// Register the request/reply channels a served nRPC service needs, through the
/// NODE's registry so the SDK and binding paths register identically.
///
/// Delegates to
/// [`net::adapter::net::ChannelConfigRegistry::install_rpc_service_defaults`], the
/// single implementation shared with `Mesh::serve_rpc*` and the aggregator.
///
/// It did not, until R9. This function was a third copy of the registration and
/// it had received neither H2 nor H3: it used the REPLACING `insert`, so serving
/// an org service silently destroyed an ACL the operator had registered for
/// `<service>.requests` beforehand; and its `<service>.replies.` prefix carried
/// no origin binding, so any mesh peer could hold a live subscription to another
/// caller's reply channel and receive that caller's response bodies whenever the
/// server's direct route missed and the response fell back to roster fan-out.
/// Both were fixed for `serve_rpc` and both were still open here.
///
/// A node built without a channel registry (possible via the bare
/// `MeshNode::new` path) simply skips this — the same tolerance the SDK's
/// `auto_register_rpc_channels` has, since a missing registry means channel ACLs
/// are not in play for that node.
///
/// `pub(crate)`: the subnet-exported serve seam registers through the
/// SAME implementation (SUBNET_AUTH_SDK_PLAN.md §3.5 — promoted, not
/// copied).
pub(crate) fn auto_register_org_channels(_node: &MeshNode, _service: &str) {
    // Deliberately empty. Every org serve path reaches a core seam —
    // `serve_rpc_owner_scoped`, `serve_rpc_granted` and
    // `serve_rpc_subnet_exported` all land in `serve_rpc_unary_impl`,
    // which installs the policy itself now. Pre-registering here would
    // make this a second owner of a requirement that already has one,
    // and a second owner is exactly how this path drifted before: it
    // carried a replacing insert that destroyed operator ACLs and an
    // unbound reply prefix, long after both were fixed elsewhere.
}

/// Wrap a facade byte closure in the one `RpcHandler` bridge —
/// `pub(crate)` so the subnet-exported serve seam projects `OrgCaller`
/// and classifies handler failures through the SAME implementation
/// rather than a second copy.
pub(crate) fn org_bytes_handler<F, Fut>(handler: F) -> Arc<OrgBytesHandler<F>>
where
    F: Fn(OrgCaller, Bytes) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<Bytes, OrgHandlerError>> + Send + 'static,
{
    Arc::new(OrgBytesHandler {
        inner: Arc::new(handler),
    })
}

/// Bridges the facade's `Fn(OrgCaller, Bytes) -> Future` closure to the raw
/// `RpcHandler` trait, forwarding the verified admission facts the existing
/// typed wrapper discards.
///
/// This is the ONLY `RpcHandler` impl in the org facade: the typed verb wraps a
/// codec around a closure and registers through here, so there is exactly one
/// place where admission facts are projected and one place where a handler
/// failure is classified.
pub(crate) struct OrgBytesHandler<F> {
    inner: Arc<F>,
}

#[async_trait]
impl<F, Fut> RpcHandler for OrgBytesHandler<F>
where
    F: Fn(OrgCaller, Bytes) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<Bytes, OrgHandlerError>> + Send + 'static,
{
    async fn call(&self, ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
        let caller = project_caller(ctx.org_admission.as_ref())?;

        let body = (self.inner)(caller, ctx.payload.body.clone()).await?;
        Ok(RpcResponsePayload {
            status: RpcStatus::Ok,
            headers: vec![],
            body,
        })
    }
}

// ===========================================================================
// OSDK §3 — the streaming provider rows (§4.3's serve rows).
//
// One projection, one classification: every bridge here (and the unary one
// above) calls [`project_caller`] and maps `OrgHandlerError` through the one
// `From`, so the four shapes cannot drift apart on attribution or failure
// framing. Each typed verb is its bytes row plus JSON — one dispatch path per
// shape. The trivial proof policy (`|_| true`) is the facade's in every row:
// the provider veto stays the caller's extension point on the low-level API.
// ===========================================================================

/// Project the verified admission facts into the handler-facing type — the
/// ONE place `Admitted` becomes `OrgCaller`.
///
/// The gate dispatches a protected registration ONLY after
/// `verify_org_admission` returned `Admitted`, so `None` here is an invariant
/// violation, not a caller error: refuse loudly rather than panic, and never
/// fabricate attribution to keep going.
fn project_caller(admitted: Option<&Admitted>) -> Result<OrgCaller, RpcHandlerError> {
    admitted
        .map(OrgCaller::from)
        .ok_or_else(|| RpcHandlerError::Application {
            code: NRPC_TYPED_HANDLER_ERROR,
            message: "org handler reached without verified admission".to_string(),
        })
}

/// Bridges the facade's `Fn(OrgCaller, Bytes, RpcResponseSink)` closure to
/// the raw [`RpcStreamingHandler`] trait — the server-streaming row's one
/// projection point.
pub(crate) struct OrgStreamingBytesHandler<F> {
    inner: Arc<F>,
}

#[async_trait]
impl<F, Fut> RpcStreamingHandler for OrgStreamingBytesHandler<F>
where
    F: Fn(OrgCaller, Bytes, RpcResponseSink) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<(), OrgHandlerError>> + Send + 'static,
{
    async fn call(&self, ctx: RpcContext, sink: RpcResponseSink) -> Result<(), RpcHandlerError> {
        let caller = project_caller(ctx.org_admission.as_ref())?;
        (self.inner)(caller, ctx.payload.body.clone(), sink).await?;
        Ok(())
    }
}

/// Bridges the facade's `Fn(OrgCaller, RequestStream)` closure to the raw
/// [`RpcClientStreamingHandler`] trait — the client-streaming row's one
/// projection point.
pub(crate) struct OrgClientStreamBytesHandler<F> {
    inner: Arc<F>,
}

#[async_trait]
impl<F, Fut> RpcClientStreamingHandler for OrgClientStreamBytesHandler<F>
where
    F: Fn(OrgCaller, RequestStream) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<Bytes, OrgHandlerError>> + Send + 'static,
{
    async fn call(
        &self,
        ctx: RpcStreamingContext,
        requests: RequestStream,
    ) -> Result<RpcResponsePayload, RpcHandlerError> {
        let caller = project_caller(ctx.org_admission.as_ref())?;
        let body = (self.inner)(caller, requests).await?;
        Ok(RpcResponsePayload {
            status: RpcStatus::Ok,
            headers: vec![],
            body,
        })
    }
}

/// Bridges the facade's `Fn(OrgCaller, RequestStream, RpcResponseSink)`
/// closure to the raw [`RpcDuplexHandler`] trait — the duplex row's one
/// projection point.
pub(crate) struct OrgDuplexBytesHandler<F> {
    inner: Arc<F>,
}

#[async_trait]
impl<F, Fut> RpcDuplexHandler for OrgDuplexBytesHandler<F>
where
    F: Fn(OrgCaller, RequestStream, RpcResponseSink) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<(), OrgHandlerError>> + Send + 'static,
{
    async fn call(
        &self,
        ctx: RpcStreamingContext,
        requests: RequestStream,
        responses: RpcResponseSink,
    ) -> Result<(), RpcHandlerError> {
        let caller = project_caller(ctx.org_admission.as_ref())?;
        (self.inner)(caller, requests, responses).await?;
        Ok(())
    }
}

/// Register a protected streaming service on a NODE — the one implementation
/// of the server-streaming serve pipeline, mirroring
/// [`serve_org_bytes_node`] (same discipline: the trivial proof policy,
/// access implies visibility).
///
/// `#[doc(hidden)]` — applications use `mesh.serve_org_streaming(..)`; this
/// is the binding seam.
#[doc(hidden)]
pub fn serve_org_streaming_bytes_node<F, Fut>(
    node: Arc<MeshNode>,
    service: &str,
    access: OrgAccess,
    handler: F,
) -> Result<ServeHandle, ServeError>
where
    F: Fn(OrgCaller, Bytes, RpcResponseSink) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<(), OrgHandlerError>> + Send + 'static,
{
    let raw = Arc::new(OrgStreamingBytesHandler {
        inner: Arc::new(handler),
    });
    auto_register_org_channels(&node, service);
    let policy: net::adapter::net::org_admission_gate::OrgProviderPolicy = Arc::new(|_| true);
    match access {
        OrgAccess::SameOrg => node.serve_rpc_owner_scoped_streaming(service, raw, policy),
        OrgAccess::Granted => node.serve_rpc_granted_streaming(service, raw, policy),
    }
}

/// [`serve_org_streaming_bytes_node`] for the client-streaming shape.
///
/// `#[doc(hidden)]` — the binding seam.
#[doc(hidden)]
pub fn serve_org_client_stream_bytes_node<F, Fut>(
    node: Arc<MeshNode>,
    service: &str,
    access: OrgAccess,
    handler: F,
) -> Result<ServeHandle, ServeError>
where
    F: Fn(OrgCaller, RequestStream) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<Bytes, OrgHandlerError>> + Send + 'static,
{
    let raw = Arc::new(OrgClientStreamBytesHandler {
        inner: Arc::new(handler),
    });
    auto_register_org_channels(&node, service);
    let policy: net::adapter::net::org_admission_gate::OrgProviderPolicy = Arc::new(|_| true);
    match access {
        OrgAccess::SameOrg => node.serve_rpc_owner_scoped_client_stream(service, raw, policy),
        OrgAccess::Granted => node.serve_rpc_granted_client_stream(service, raw, policy),
    }
}

/// [`serve_org_streaming_bytes_node`] for the duplex shape.
///
/// `#[doc(hidden)]` — the binding seam.
#[doc(hidden)]
pub fn serve_org_duplex_bytes_node<F, Fut>(
    node: Arc<MeshNode>,
    service: &str,
    access: OrgAccess,
    handler: F,
) -> Result<ServeHandle, ServeError>
where
    F: Fn(OrgCaller, RequestStream, RpcResponseSink) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<(), OrgHandlerError>> + Send + 'static,
{
    let raw = Arc::new(OrgDuplexBytesHandler {
        inner: Arc::new(handler),
    });
    auto_register_org_channels(&node, service);
    let policy: net::adapter::net::org_admission_gate::OrgProviderPolicy = Arc::new(|_| true);
    match access {
        OrgAccess::SameOrg => node.serve_rpc_owner_scoped_duplex(service, raw, policy),
        OrgAccess::Granted => node.serve_rpc_granted_duplex(service, raw, policy),
    }
}
