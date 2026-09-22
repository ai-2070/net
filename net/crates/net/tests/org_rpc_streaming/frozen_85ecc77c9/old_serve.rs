//! VERBATIM vendoring of the frozen old-provider SERVE surface
//! (`mesh_rpc.rs` at revision `85ecc77c9`):
//!
//! * `MeshNode::serve_rpc_protected` — **lines 3400–3438** (doc + body),
//!   sha256 of extraction
//!   `2b1234f4d377685a39ee0db58218d69c76c8c128fb8bd8b542b4a1a914935292`,
//!   appended verbatim as an inherent method of [`FrozenProvider`];
//! * the frozen per-call shape derivation (`let is_unary = …`) —
//!   **lines 1124–1127**, sha256
//!   `f74300ce94b508da6a439c906f523c4110dd3394d08d5e975e1a28ac3fbc9bf8`;
//! * the frozen denial response shape (`let resp = RpcResponsePayload { … }`)
//!   — **lines 872–876** of `emit_admission_denial`, sha256
//!   `fa275454b5e7293f7e3090ed0775d1193d520b00cbab9e3f68f5b43deccc5960`.
//!
//! Adaptations (named, exhaustive — everything else is byte-identical):
//!
//! 1. Import retargets (`crate::…`/`super::…` → `net::…`/`super::old_…`);
//! 2. the denial-shape block's one `crate::adapter::net::cortex::` prefix is
//!    retargeted to `net::adapter::net::cortex::` (a test crate cannot name
//!    the library's `crate::`);
//! 3. two `Self` methods the vendored `serve_rpc_protected` body calls are
//!    supplied by the shim below (the frozen bodies live inside `MeshNode`
//!    among private types — `RegisteredRpcService`, `RpcResponseJob`,
//!    `RpcOriginNodeCache`, the bridge closure — that cannot compile in a
//!    test crate): `node_authority()` (the frozen E1.1 presence probe) and
//!    `serve_rpc_unary_impl(...)` (the frozen registrar). The registrar shim
//!    makes NO admission decision: it captures the frozen
//!    `UnaryAdmission::Protected` for [`FrozenProvider::registration`] and
//!    returns the `ServeHandle` of the harness node's UNCHANGED v0.4 public
//!    registrar (`MeshNode::serve_rpc`, not touched by Stage 1);
//! 4. [`UnaryAdmission`] here carries only the `Protected { .. }` variant —
//!    the only one the vendored body constructs (the full frozen enum is
//!    `mesh_rpc.rs:6791-6846` at `85ecc77c9`).
//!
//! The vendored items are NEVER substituted by the new implementation: the
//! decode is the frozen prefix-tolerant `OrgCallProof::decode`, the verify is
//! the frozen `is_unary` step 4, and the wire byte comes from the frozen
//! `coarse()` mapping.

#![allow(
    dead_code,
    reason = "verbatim vendoring + shim seams: the frozen registration shape and its \
              captured fields are exercised structurally, not read field-by-field"
)]

use std::sync::Arc;

use net::adapter::net::cortex::RpcHandler;
use net::adapter::net::mesh_rpc::{ServeError, ServeHandle};
use net::adapter::net::org_admission_gate::OrgProviderPolicy;

use super::old_org_admission::OrgAdmission;

/// Adaptation 4 (above): the frozen `UnaryAdmission`'s `Protected` variant —
/// the only one the vendored `serve_rpc_protected` body constructs.
pub enum UnaryAdmission {
    Protected {
        admission: OrgAdmission,
        provider_policy: OrgProviderPolicy,
    },
}

/// The frozen old provider: a registration surface carrying the vendored
/// `serve_rpc_protected` plus the two shim methods its body calls.
pub struct FrozenProvider {
    /// The frozen E1.1 probe's subject: `Some` iff a node authority is
    /// installed (mirrors `MeshNode::node_authority().is_none()`).
    pub authority_installed: bool,
    /// The registration captured by the [`Self::serve_rpc_unary_impl`]
    /// shim — the frozen `UnaryAdmission::Protected` the per-call runner
    /// admits under.
    registration: std::sync::Mutex<Option<(String, UnaryAdmission)>>,
    /// The harness node whose UNCHANGED public registrar supplies the
    /// `ServeHandle` (adaptation 3).
    mesh: Arc<net::adapter::net::MeshNode>,
}

impl FrozenProvider {
    pub fn new(mesh: Arc<net::adapter::net::MeshNode>, authority_installed: bool) -> Arc<Self> {
        Arc::new(Self {
            authority_installed,
            registration: std::sync::Mutex::new(None),
            mesh,
        })
    }

    /// The registration the vendored `serve_rpc_protected` installed.
    pub fn registration(&self) -> std::sync::MutexGuard<'_, Option<(String, UnaryAdmission)>> {
        self.registration.lock().expect("registration lock")
    }

    /// Adaptation 3: the frozen `MeshNode::node_authority` presence probe.
    fn node_authority(&self) -> Option<()> {
        self.authority_installed.then_some(())
    }

    /// Adaptation 3: the frozen `MeshNode::serve_rpc_unary_impl` registrar —
    /// captures the frozen admission shape (no decision made here) and
    /// returns the harness node's unchanged v0.4 public registration handle.
    fn serve_rpc_unary_impl<H: RpcHandler>(
        self: &Arc<Self>,
        service: &str,
        handler: Arc<H>,
        admission: UnaryAdmission,
    ) -> Result<ServeHandle, ServeError> {
        let handle = self.mesh.serve_rpc(service, handler)?;
        *self.registration.lock().expect("registration lock") =
            Some((service.to_string(), admission));
        Ok(handle)
    }
    /// Register a PROTECTED unary RPC handler (E1.1/E1.2). Every call must carry
    /// a `net-org-admission` proof that [`verify_org_admission`] accepts under
    /// `admission` (owner-delegated or cross-org); the captured `provider_policy`
    /// is the final application veto. REQUIRES an installed node authority (else
    /// [`ServeError::ProtectedAuthorityRequired`]) and an org-protected mode
    /// (never `PublicAuthenticated`). Unary only (E1.8). Denied calls receive
    /// [`RpcStatus::AdmissionDenied`] (0x0009 + a coarse reason); the handler
    /// runs ONLY on an admitted call and reads the four-party attribution via
    /// [`RpcContext::org_admission`](crate::adapter::net::cortex::RpcContext).
    ///
    /// [`verify_org_admission`]: crate::adapter::net::behavior::org_admission::verify_org_admission
    pub fn serve_rpc_protected<H: RpcHandler>(
        self: &Arc<Self>,
        service: &str,
        handler: Arc<H>,
        admission: OrgAdmission,
        provider_policy: OrgProviderPolicy,
    ) -> Result<ServeHandle, ServeError> {
        if matches!(admission, OrgAdmission::PublicAuthenticated) {
            return Err(ServeError::InvalidProtectedRegistration(
                "admission mode must be org-protected (OwnerDelegated / CrossOrgGranted), \
                 not PublicAuthenticated"
                    .to_string(),
            ));
        }
        // E1.1: protected registration requires an installed authority — checked
        // up front so the registration below cannot half-succeed then unwind.
        if self.node_authority().is_none() {
            return Err(ServeError::ProtectedAuthorityRequired(service.to_string()));
        }
        self.serve_rpc_unary_impl(
            service,
            handler,
            UnaryAdmission::Protected {
                admission,
                provider_policy,
            },
        )
    }
}

/// The typed frozen yield of one opening: either the four-party [`Admitted`]
/// or the denial together with the EXACT wire response `emit_admission_denial`
/// would enqueue (`RpcStatus::AdmissionDenied` + one coarse byte).
pub enum FrozenYield {
    Admitted(Box<super::old_org_admission::Admitted>),
    Denied {
        denied: super::old_org_admission::AdmissionDenied,
        response: net::adapter::net::cortex::RpcResponsePayload,
    },
}

/// The frozen per-call decision chain for ONE opening frame on the frozen
/// provider: the verbatim `is_unary` derivation (`mesh_rpc.rs:1124-1127` at
/// `85ecc77c9`), the frozen [`super::old_org_admission::verify_org_admission`]
/// fed the frame's admission header, and — on denial — the verbatim denial
/// response shape (`mesh_rpc.rs:872-877` at `85ecc77c9`).
///
/// `ctx` is the frozen `AdmissionContext`; its `is_unary` field is
/// OVERWRITTEN by the verbatim flag derivation below (that derivation is the
/// frozen path's, not the caller's). `policy` is the registered
/// `provider_policy` (the registration's veto — `|_| true` for these
/// witnesses).
pub fn frozen_opening(
    payload: &net::adapter::net::cortex::RpcRequestPayload,
    mut ctx: super::old_org_admission::AdmissionContext<'_>,
    admission_headers: &[&[u8]],
    replay: &net::adapter::net::behavior::org_admission_replay::AdmissionReplayGuard,
    clock: net::adapter::net::behavior::admission_clock::ClockSample,
    policy: impl FnOnce(&super::old_org_call::OrgCallProof) -> bool,
) -> FrozenYield {
    use net::adapter::net::cortex::{
        FLAG_RPC_CLIENT_STREAMING_REQUEST, FLAG_RPC_STREAMING_RESPONSE,
    };

    // ==== VERBATIM mesh_rpc.rs:1124-1127 @85ecc77c9 (adaptation 1: none needed) ====
    // Unary only (E1.8): a streaming flag on a protected REQUEST is a distinct
    // "not supported" denial, never admitted under a unary binding.
    let is_unary =
        payload.flags & (FLAG_RPC_CLIENT_STREAMING_REQUEST | FLAG_RPC_STREAMING_RESPONSE) == 0;
    // ==== end verbatim ====
    ctx.is_unary = is_unary;

    let outcome = super::old_org_admission::verify_org_admission(
        &ctx,
        admission_headers,
        replay,
        clock,
        || true,
        policy,
    );
    match outcome {
        Ok(admitted) => FrozenYield::Admitted(Box::new(admitted)),
        Err(denied) => {
            let coarse = denied.coarse();
            // ==== VERBATIM mesh_rpc.rs:872-876 @85ecc77c9 (adaptation 2: the
            // one `crate::adapter::net::cortex::` prefix retargeted to `net::`) ====
            let resp = net::adapter::net::cortex::RpcResponsePayload {
                status: RpcStatus::AdmissionDenied,
                headers: vec![],
                body: Bytes::copy_from_slice(&[coarse.to_wire()]),
            };
            // ==== end verbatim ====
            FrozenYield::Denied {
                denied,
                response: resp,
            }
        }
    }
}

use bytes::Bytes;
use net::adapter::net::cortex::RpcStatus;
