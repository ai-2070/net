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

use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use net::adapter::net::behavior::org_admission::Admitted;
use net::adapter::net::identity::EntityId;

use super::types::{CapabilityAuthorityId, OrgId};
use crate::mesh::Mesh;
use crate::mesh_rpc::{
    Codec, RpcContext, RpcHandler, RpcHandlerError, RpcResponsePayload, RpcStatus, ServeError,
    ServeHandle, NRPC_TYPED_BAD_REQUEST, NRPC_TYPED_HANDLER_ERROR,
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
        Fut: std::future::Future<Output = Result<Resp, String>> + Send + 'static,
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
        Fut: std::future::Future<Output = Result<Bytes, OrgHandlerError>> + Send + 'static,
    {
        let raw = Arc::new(OrgBytesHandler {
            inner: Arc::new(handler),
        });
        self.auto_register_rpc_channels(service);
        // The trivial proof policy: v1 decides in the handler, with the verified
        // facts in hand. The low-level API keeps the step-11 seam for providers
        // that must refuse before the replay insert.
        let policy: net::adapter::net::org_admission_gate::OrgProviderPolicy = Arc::new(|_| true);
        match access {
            OrgAccess::SameOrg => self.node().serve_rpc_owner_scoped(service, raw, policy),
            OrgAccess::Granted => self.node().serve_rpc_granted(service, raw, policy),
        }
    }
}

/// Bridges the facade's `Fn(OrgCaller, Bytes) -> Future` closure to the raw
/// `RpcHandler` trait, forwarding the verified admission facts the existing
/// typed wrapper discards.
///
/// This is the ONLY `RpcHandler` impl in the org facade: the typed verb wraps a
/// codec around a closure and registers through here, so there is exactly one
/// place where admission facts are projected and one place where a handler
/// failure is classified.
struct OrgBytesHandler<F> {
    inner: Arc<F>,
}

#[async_trait]
impl<F, Fut> RpcHandler for OrgBytesHandler<F>
where
    F: Fn(OrgCaller, Bytes) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<Bytes, OrgHandlerError>> + Send + 'static,
{
    async fn call(&self, ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
        // The gate dispatches a protected registration ONLY after
        // `verify_org_admission` returned `Admitted`, so `None` here is an
        // invariant violation, not a caller error. Refuse loudly rather than
        // panic, and never fabricate attribution to keep going.
        let Some(admitted) = ctx.org_admission.as_ref() else {
            return Err(RpcHandlerError::Application {
                code: NRPC_TYPED_HANDLER_ERROR,
                message: "org handler reached without verified admission".to_string(),
            });
        };
        let caller = OrgCaller::from(admitted);

        let body = (self.inner)(caller, ctx.payload.body.clone()).await?;
        Ok(RpcResponsePayload {
            status: RpcStatus::Ok,
            headers: vec![],
            body,
        })
    }
}
