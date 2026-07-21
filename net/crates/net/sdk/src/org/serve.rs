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
        let typed = Arc::new(OrgTypedHandler {
            codec: Codec::Json,
            inner: Arc::new(handler),
            _req: std::marker::PhantomData::<Req>,
            _resp: std::marker::PhantomData::<Resp>,
        });
        self.auto_register_rpc_channels(service);
        // The trivial proof policy: v1 decides in the handler, with the verified
        // facts in hand. The low-level API keeps the step-11 seam for providers
        // that must refuse before the replay insert.
        let policy: net::adapter::net::org_admission_gate::OrgProviderPolicy = Arc::new(|_| true);
        match access {
            OrgAccess::SameOrg => self.node().serve_rpc_owner_scoped(service, typed, policy),
            OrgAccess::Granted => self.node().serve_rpc_granted(service, typed, policy),
        }
    }
}

/// Bridges the facade's `Fn(OrgCaller, Req) -> Future` closure to the raw
/// `RpcHandler` trait, forwarding the verified admission facts the existing
/// typed wrapper discards.
struct OrgTypedHandler<Req, Resp, F> {
    codec: Codec,
    inner: Arc<F>,
    _req: std::marker::PhantomData<Req>,
    _resp: std::marker::PhantomData<Resp>,
}

#[async_trait]
impl<Req, Resp, F, Fut> RpcHandler for OrgTypedHandler<Req, Resp, F>
where
    Req: serde::de::DeserializeOwned + Send + Sync + 'static,
    Resp: serde::Serialize + Send + Sync + 'static,
    F: Fn(OrgCaller, Req) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<Resp, String>> + Send + 'static,
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

        let req: Req = match self.codec.decode(&ctx.payload.body) {
            Ok(r) => r,
            Err(e) => {
                return Err(RpcHandlerError::Application {
                    code: NRPC_TYPED_BAD_REQUEST,
                    message: format!("org handler: bad request body: {e}"),
                })
            }
        };

        let resp =
            (self.inner)(caller, req)
                .await
                .map_err(|message| RpcHandlerError::Application {
                    code: NRPC_TYPED_HANDLER_ERROR,
                    message,
                })?;

        let body = self
            .codec
            .encode(&resp)
            .map_err(|e| RpcHandlerError::Internal(format!("org handler: response encode: {e}")))?;
        Ok(RpcResponsePayload {
            status: RpcStatus::Ok,
            headers: vec![],
            body: body.into(),
        })
    }
}
