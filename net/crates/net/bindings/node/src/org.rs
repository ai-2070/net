//! OSDK-L Workstream N — the organization capability surface for Node.
//!
//! Two verbs, five concepts, and no way to put a discovery key in a JS
//! `Buffer`. This module is marshaling only: every authority decision already
//! happened in `net_sdk::org`, and anything here that looks like a decision is
//! a bug.
//!
//! # The credential asymmetry (the one thing to understand)
//!
//! Public signed credentials — membership, dispatcher grant, capability grants
//! — cross as canonical wire `Buffer`s, because they are public objects
//! designed to transit. The audience secret does **not**: it is the raw
//! discovery key, and handing it to V8 would put it in garbage-collected memory
//! that is never zeroized, freely copied by the collector, and visible in a heap
//! dump. So JS supplies a **path**, Rust opens and validates the file, and the
//! key's whole lifetime stays on the Rust side.
//!
//! There is deliberately no bytes variant of `audienceSecretPaths`. Adding one
//! would reopen the language-SDK plan's first locked decision.
//!
//! # Lifecycle
//!
//! An `OrgClient` holds an `Arc<MeshNode>` and a consumer-audience lease, so a
//! live one keeps ingest authority installed AND holds a node reference. The
//! teardown order is:
//!
//! ```text
//! orgClient.close()  →  serveHandle.close()  →  await mesh.shutdown()
//! ```
//!
//! Skipping `close()` is not merely untidy: `NetMesh.shutdown()` drains
//! outstanding `Arc<MeshNode>` references for ~250 ms and then REJECTS with
//! "cannot shutdown: outstanding references exist", restoring the node. The
//! node stays usable and a retry after `close()` succeeds — but the first
//! shutdown fails, visibly.

use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwapOption;
use napi::bindgen_prelude::*;
use napi::threadsafe_function::{ThreadsafeFunction, ThreadsafeFunctionCallMode};
use napi_derive::napi;

/// Inputs for [`OrgCredentials::create`].
///
/// `audienceSecretPaths` is `string[]` and not `Buffer[]` by design — see the
/// module docs.
#[napi(object)]
pub struct OrgCredentialsOptions {
    /// Canonical wire bytes of the membership certificate (156 B).
    pub membership: Buffer,
    /// Canonical wire bytes of the dispatcher grant (185 B).
    pub dispatcher: Buffer,
    /// Canonical wire bytes of each held capability grant (318 B each).
    pub grants: Vec<Buffer>,
    /// Filesystem paths to the out-of-band audience-secret files, one per
    /// DISCOVER grant. Rust opens and validates each file; the key never
    /// reaches JS.
    pub audience_secret_paths: Vec<String>,
}

/// A validated organization credential set.
///
/// Consumed by [`OrgClient::bind`]: binding takes ownership, so a second bind
/// from the same instance fails rather than silently sharing state. Construct a
/// new one to bind again.
#[napi]
pub struct OrgCredentials {
    inner: parking_lot::Mutex<Option<net_sdk::org::OrgCredentials>>,
}

#[napi]
impl OrgCredentials {
    /// Validate and assemble a credential set.
    ///
    /// Verifies every signature and structural relation the provider's
    /// admission engine will later re-verify remotely, and loads each audience
    /// secret through the checked loader (which validates the OPENED file:
    /// no symlink following, regular file, owner-only, exact size). Validity
    /// windows are deliberately NOT checked here — credentials are routinely
    /// assembled before the window they will be used in.
    #[napi(factory)]
    pub fn create(options: OrgCredentialsOptions) -> Result<OrgCredentials> {
        let grants: Vec<Vec<u8>> = options.grants.iter().map(|g| g.to_vec()).collect();
        let paths: Vec<std::path::PathBuf> = options
            .audience_secret_paths
            .iter()
            .map(std::path::PathBuf::from)
            .collect();

        let inner = net_sdk::org::OrgCredentials::from_parts(
            &options.membership,
            &options.dispatcher,
            &grants,
            &paths,
        )
        .map_err(|e| org_error(net_sdk::org::OrgSdkError::Credentials(e)))?;

        Ok(OrgCredentials {
            inner: parking_lot::Mutex::new(Some(inner)),
        })
    }

    /// Take the inner set for binding. Second call yields `None`.
    fn take(&self) -> Option<net_sdk::org::OrgCredentials> {
        self.inner.lock().take()
    }
}

/// A credential set bound to a live mesh — the caller half of the facade.
///
/// Close it when done: see the module docs on teardown order.
#[napi]
pub struct OrgClient {
    /// `ArcSwapOption` rather than a plain field so `close()` and an in-flight
    /// `callBytes` cannot race into a half-torn state. A call snapshots the
    /// client first; because clones share one audience lease and one node
    /// reference, a snapshot that wins keeps BOTH alive until it completes,
    /// even if `close()` lands immediately after.
    inner: ArcSwapOption<net_sdk::org::OrgClient>,
}

#[napi]
impl OrgClient {
    /// Bind credentials to a mesh.
    ///
    /// Refuses unless the complete private-discovery identity relation holds:
    /// the node's identity was explicitly configured (an org membership names a
    /// durable entity, so a generated ephemeral keypair is refused), a node
    /// authority is installed, its owner org is the membership's org, and the
    /// membership vouches for this node's entity.
    ///
    /// Consumes `credentials`.
    #[napi(factory)]
    pub fn bind(mesh: &crate::NetMesh, credentials: &OrgCredentials) -> Result<OrgClient> {
        let node = mesh.node_arc_clone()?;
        let creds = credentials.take().ok_or_else(|| {
            Error::from_reason(
                "org:credentials:already_consumed: these OrgCredentials were already bound; \
                 construct a new set to bind again",
            )
        })?;
        let client = net_sdk::org::OrgClient::bind_node(node, creds).map_err(org_error)?;
        Ok(OrgClient {
            inner: ArcSwapOption::from_pointee(client),
        })
    }

    /// Call a protected service — bytes in, bytes out.
    ///
    /// The typed `call` lives in `org.ts` over this, mirroring how
    /// `TypedMeshRpc` sits over the raw nRPC surface.
    ///
    /// Discovers privately, selects one authorized provider, mints a canonical
    /// request-bound proof, and issues ONE exact-target call. Never retries: a
    /// signed proof is bound to one call id, so any second attempt must be a
    /// fresh call the application makes deliberately.
    #[napi]
    pub async fn call_bytes(&self, service: String, request: Buffer) -> Result<Buffer> {
        // Snapshot first: if `close()` lands after this line, the clone keeps
        // the lease and node reference alive until the call completes.
        let client = self.inner.load_full().ok_or_else(|| {
            Error::from_reason("org:credentials:closed: this OrgClient has been closed")
        })?;
        let body = bytes::Bytes::from(request.to_vec());
        let reply = client.call_bytes(&service, body).await.map_err(org_error)?;
        Ok(Buffer::from(reply.to_vec()))
    }

    /// Call a subnet-exported service — bytes in, bytes out
    /// (SSDK §3.6; the typed `callExported` lives in `org.ts` over this).
    ///
    /// Discovers on the PUBLIC plane through the verified ownership
    /// projection (candidate and owner sampled from one snapshot),
    /// derives the same-org/granted relation from the VERIFIED owner,
    /// mints the same canonical proof as `callBytes`, and sends exactly
    /// once — never a retry. Deliberately `callExported`, not
    /// `callSubnet`: the caller names no subnet, joins no subnet, and
    /// receives no subnet context.
    #[napi]
    pub async fn call_exported_bytes(&self, service: String, request: Buffer) -> Result<Buffer> {
        let client = self.inner.load_full().ok_or_else(|| {
            Error::from_reason("org:credentials:closed: this OrgClient has been closed")
        })?;
        let body = bytes::Bytes::from(request.to_vec());
        let reply = client
            .call_exported_bytes(&service, body)
            .await
            .map_err(org_error)?;
        Ok(Buffer::from(reply.to_vec()))
    }

    /// Open a protected STREAMING call — one request in, a stream of
    /// responses out (§4.4's `callStreamingBytes`). Returns the EXISTING
    /// `RpcStream` handle, over the frozen
    /// `call_streaming_bytes_deadline` seam — no second stream type.
    ///
    /// Execution control follows the seam contract exactly:
    /// `deadlineMs == 0` (or omitted) is the facade's default lifetime
    /// (Owner Q1, 300 s) and NEVER "no deadline"; `cancelToken == 0`
    /// (or omitted) is uncancellable. Neither argument is an
    /// authorization input. Reserve a token from the same node's
    /// `MeshRpc.reserveCancelToken()` and fire `MeshRpc.cancelCall(token)`
    /// to retire the call midstream.
    ///
    /// Errors carry the `org:` wire vocabulary — the opening refusal is
    /// `org:admission_denied:<coarse>` / `org:discovery:…`, and a midstream
    /// outcome arrives as the stream's thrown error (`org:rpc:…`, or
    /// `org:admission_denied:<coarse>` on revocation), classifiable with
    /// `classifyOrgError`. Drop or `close()` emits one CANCEL.
    #[napi]
    pub async fn call_streaming_bytes(
        &self,
        service: String,
        request: Buffer,
        deadline_ms: Option<u32>,
        cancel_token: Option<BigInt>,
    ) -> Result<crate::mesh_rpc::RpcStream> {
        let client = self.inner.load_full().ok_or_else(|| {
            Error::from_reason("org:credentials:closed: this OrgClient has been closed")
        })?;
        let cancel_token = match cancel_token {
            Some(t) => crate::common::bigint_u64(t)?,
            None => 0,
        };
        let body = bytes::Bytes::from(request.to_vec());
        let inner = client
            .call_streaming_bytes_deadline(
                &service,
                body,
                u64::from(deadline_ms.unwrap_or(0)),
                cancel_token,
            )
            .await
            .map_err(org_error)?;
        let flow_controlled_cached = inner.flow_controlled();
        Ok(crate::mesh_rpc::RpcStream {
            inner: Arc::new(tokio::sync::Mutex::new(Some(inner))),
            flow_controlled_cached,
            org_errors: true,
        })
    }

    /// Open a protected CLIENT-STREAMING call — a stream of requests in,
    /// one terminal response out (§4.4's `callClientStreamBytes`). Returns
    /// the EXISTING `ClientStreamCall` handle over the frozen
    /// `call_client_stream_bytes_deadline` seam. The opening is lazy: the
    /// initial REQUEST flies at the first `send` (or `finish` on the
    /// degenerate zero-send path), pinned to the provider this verb
    /// selected. `deadlineMs` / `cancelToken` follow the same seam
    /// contract as {@link call_streaming_bytes}.
    #[napi]
    pub async fn call_client_stream_bytes(
        &self,
        service: String,
        deadline_ms: Option<u32>,
        cancel_token: Option<BigInt>,
    ) -> Result<crate::mesh_rpc::ClientStreamCall> {
        let client = self.inner.load_full().ok_or_else(|| {
            Error::from_reason("org:credentials:closed: this OrgClient has been closed")
        })?;
        let cancel_token = match cancel_token {
            Some(t) => crate::common::bigint_u64(t)?,
            None => 0,
        };
        let inner = client
            .call_client_stream_bytes_deadline(
                &service,
                u64::from(deadline_ms.unwrap_or(0)),
                cancel_token,
            )
            .await
            .map_err(org_error)?;
        let call_id_cached = inner.call_id();
        let flow_controlled_cached = inner.flow_controlled();
        Ok(crate::mesh_rpc::ClientStreamCall {
            inner: Arc::new(tokio::sync::Mutex::new(Some(inner))),
            call_id_cached,
            flow_controlled_cached,
            close_notify: Arc::new(tokio::sync::Notify::new()),
            org_errors: true,
        })
    }

    /// Open a protected DUPLEX call and return its two halves directly as
    /// `[sink, stream]` — the EXISTING `DuplexSink` + `DuplexStream`
    /// handles (§4.4's `callDuplexBytes`), split at the verb because
    /// `intoSplit` is synchronous on the core handle. The opening rides
    /// the frozen `call_duplex_bytes_deadline` seam; `deadlineMs` /
    /// `cancelToken` follow the same seam contract as
    /// {@link call_streaming_bytes}. CANCEL fires only when BOTH halves
    /// drop without observing the response stream's terminal frame.
    #[napi]
    pub async fn call_duplex_bytes(
        &self,
        service: String,
        deadline_ms: Option<u32>,
        cancel_token: Option<BigInt>,
    ) -> Result<(crate::mesh_rpc::DuplexSink, crate::mesh_rpc::DuplexStream)> {
        let client = self.inner.load_full().ok_or_else(|| {
            Error::from_reason("org:credentials:closed: this OrgClient has been closed")
        })?;
        let cancel_token = match cancel_token {
            Some(t) => crate::common::bigint_u64(t)?,
            None => 0,
        };
        let inner = client
            .call_duplex_bytes_deadline(&service, u64::from(deadline_ms.unwrap_or(0)), cancel_token)
            .await
            .map_err(org_error)?;
        let call_id_cached = inner.call_id();
        let flow_controlled_cached = inner.flow_controlled();
        let (sink, stream) = inner.into_split();
        Ok((
            crate::mesh_rpc::DuplexSink {
                inner: Arc::new(tokio::sync::Mutex::new(Some(sink))),
                call_id_cached,
                flow_controlled_cached,
                close_notify: Arc::new(tokio::sync::Notify::new()),
                org_errors: true,
            },
            crate::mesh_rpc::DuplexStream {
                inner: Arc::new(tokio::sync::Mutex::new(Some(stream))),
                call_id_cached,
                org_errors: true,
            },
        ))
    }

    /// The organization this client acts for, as 32 raw bytes.
    #[napi(getter)]
    pub fn acting_org(&self) -> Result<Buffer> {
        let client = self.inner.load_full().ok_or_else(|| {
            Error::from_reason("org:credentials:closed: this OrgClient has been closed")
        })?;
        Ok(Buffer::from(client.acting_org().as_bytes().to_vec()))
    }

    /// The entity this client calls as, as 32 raw bytes.
    #[napi(getter)]
    pub fn caller(&self) -> Result<Buffer> {
        let client = self.inner.load_full().ok_or_else(|| {
            Error::from_reason("org:credentials:closed: this OrgClient has been closed")
        })?;
        Ok(Buffer::from(client.caller().as_bytes().to_vec()))
    }

    /// Release the client: drops the consumer-audience lease and this client's
    /// node reference. Idempotent.
    ///
    /// Call this before `mesh.shutdown()` — see the module docs. In-flight
    /// calls that already snapshotted the client complete normally; calls
    /// started after this reject with `org:credentials:closed` (a LOCAL error —
    /// nothing was sent).
    #[napi]
    pub fn close(&self) {
        let _ = self.inner.swap(None);
    }

    /// Whether [`close`](Self::close) has been called.
    #[napi(getter)]
    pub fn is_closed(&self) -> bool {
        self.inner.load().is_none()
    }
}

/// Map an `OrgSdkError` onto the `org:` wire vocabulary `errors.ts` classifies.
///
/// The string comes from `to_wire()` — the single Rust source, pinned by
/// `tests/cross_lang_org/error_vectors.json` — so this binding cannot drift
/// from the contract by inventing its own text.
fn org_error(e: net_sdk::org::OrgSdkError) -> Error {
    Error::from_reason(e.to_wire())
}

// ---------------------------------------------------------------------------
// Provisioning — the operator/startup steps that make the surface usable
// ---------------------------------------------------------------------------

/// Install an adopted node authority from `authorityDir` — the directory
/// `net node adopt` wrote.
///
/// REQUIRED before `OrgClient.bind` can succeed or a `Granted` service can
/// serve: the org facade needs an installed authority. This is node STARTUP
/// (loading already-adopted files), not adoption (the one-time ceremony that
/// mints them — that stays in the `net org` / `net node adopt` CLI). The node's
/// identity must be the one the membership names, so the mesh must have been
/// created with the matching `identitySeed`.
#[napi]
pub fn install_org_authority(mesh: &crate::NetMesh, authority_dir: String) -> Result<()> {
    let node = mesh.node_arc_clone()?;
    net_sdk::org::install_org_authority_node(&node, std::path::Path::new(&authority_dir))
        .map_err(|e| Error::from_reason(e.to_string()))
}

/// Install a provider grant audience so a `Granted` service can seal envelopes:
/// the grant this node's org issued (wire bytes) plus its out-of-band secret
/// (a PATH — the raw key never enters JS, the same asymmetry as credentials).
///
/// A `SameOrg` provider does NOT need this; it seals under the owner audience
/// carried by the installed authority.
#[napi]
pub fn install_provider_grant_audience(
    mesh: &crate::NetMesh,
    grant: Buffer,
    audience_secret_path: String,
) -> Result<()> {
    let node = mesh.node_arc_clone()?;
    net_sdk::org::install_provider_grant_audience_node(
        &node,
        &grant,
        std::path::Path::new(&audience_secret_path),
    )
    .map_err(|e| Error::from_reason(e.to_string()))
}

// ---------------------------------------------------------------------------
// The provider verb
// ---------------------------------------------------------------------------

/// Who may call a protected service, and how it is announced.
///
/// Access implies visibility — both variants are announced ONLY inside an
/// encrypted audience, never on the plaintext plane. Protected-but-publicly-
/// discoverable registration stays on the low-level Rust API.
#[napi(string_enum)]
pub enum OrgAccess {
    /// Members of this node's own organization, acting under a dispatcher
    /// grant. Announced inside the encrypted owner audience.
    SameOrg,
    /// Members of another organization holding a capability grant this node's
    /// owner issued. Announced inside the encrypted per-grant audiences.
    Granted,
}

/// The provider-verified facts about an admitted call.
///
/// An exact projection of the canonical `Admitted` — the same five fields,
/// nothing added. Every one was verified by `verify_org_admission` before the
/// handler ran; none is caller-claimed. Ids are 32 raw bytes.
#[napi(object)]
pub struct OrgCaller {
    /// The acting entity — the caller.
    pub entity: Buffer,
    /// The organization the caller acted for.
    pub acting_org: Buffer,
    /// This provider's owner organization.
    pub provider_org: Buffer,
    /// This exact provider node.
    pub provider: Buffer,
    /// The capability that was invoked.
    pub capability: Buffer,
    /// Whether the call came from this provider's own organization.
    pub is_same_org: bool,
}

/// What the JS handler receives: the verified facts plus the request bytes.
#[napi(object)]
pub struct OrgRequest {
    /// Provider-verified attribution.
    pub caller: OrgCaller,
    /// The raw request body.
    pub request: Buffer,
}

/// Application status for a handler that rejected — the same value the typed
/// nRPC layer uses (`NRPC_TYPED_HANDLER_ERROR`), so a caller routes org handler
/// errors exactly as it routes typed-RPC ones. Deliberately in the
/// application band: a handler cannot counterfeit an admission denial.
const ORG_HANDLER_ERROR: u16 = 0x8001;

/// Handler bridge: `(req: OrgRequest) => Promise<Buffer>`.
///
/// The trailing `false` means NOT callee-handled, so a JS throw surfaces as a
/// `Result::Err` in the callback rather than crashing the process — the
/// invariant every TSFN site in this crate holds.
///
/// `pub(crate)`: the subnet-exported serve (`subnet.rs`) bridges the SAME
/// handler shape through the SAME dispatch, deliberately not a second copy.
pub(crate) type OrgHandlerTsfn =
    ThreadsafeFunction<OrgRequest, Promise<Buffer>, OrgRequest, Status, false>;

/// Handle for a served organization service. `close()` unregisters.
#[napi]
pub struct OrgServeHandle {
    inner: parking_lot::Mutex<Option<net_sdk::mesh_rpc::ServeHandle>>,
    /// The runtime the registration ran on, carried forward — exactly the
    /// role `mesh_rpc::ServeHandle`'s `runtime` field plays (`mesh_rpc.rs`
    /// "ServeHandle"): `close()` runs on the JS thread with no reactor in
    /// context, and dropping the inner handle tears down runtime-bound
    /// resources, so the handle must be able to enter the runtime it
    /// belongs to. Every org-surface serve registers on
    /// [`org_serve_runtime`], so that is the runtime captured here.
    runtime: tokio::runtime::Handle,
}

impl OrgServeHandle {
    /// Wrap a registered serve handle — shared with the subnet-exported
    /// serve, which returns the same RAII shape.
    pub(crate) fn from_handle(handle: net_sdk::mesh_rpc::ServeHandle) -> Self {
        OrgServeHandle {
            inner: parking_lot::Mutex::new(Some(handle)),
            runtime: org_serve_runtime().handle().clone(),
        }
    }
}

#[napi]
impl OrgServeHandle {
    /// Unregister the service. Idempotent. In-flight handlers run to
    /// completion (subject to the handler-drop contract on the serve
    /// verbs — see [`serve_org_streaming`]).
    #[napi]
    pub fn close(&self) {
        // Enter the registration's runtime before dropping the inner
        // handle — mirror of `mesh_rpc::ServeHandle::close`.
        let _enter = self.runtime.enter();
        let _ = self.inner.lock().take();
    }
}

/// Serve a protected, privately-discoverable service.
///
/// The handler receives `{ caller, request }` and returns the response bytes.
/// `access` selects both who may call AND how the service is announced; there
/// is no separate visibility knob, because every combination a common provider
/// should want is one of these two.
///
/// Returning a rejected promise surfaces as an application error, never as an
/// admission denial — `0x0009` is the admission engine's word, and a handler
/// cannot counterfeit it.
///
/// Requires an installed node authority; a protected registration without one
/// is refused loudly.
#[napi]
pub fn serve_org(
    mesh: &crate::NetMesh,
    service: String,
    access: OrgAccess,
    handler: Function<'_, OrgRequest, Promise<Buffer>>,
    handler_timeout_ms: Option<u32>,
) -> Result<OrgServeHandle> {
    let node = mesh.node_arc_clone()?;
    let tsfn: OrgHandlerTsfn = handler
        .build_threadsafe_function()
        .callee_handled::<false>()
        .build()?;
    let tsfn = Arc::new(tsfn);
    // 0 disables the cap, matching `MeshRpc.serve`'s contract.
    let timeout = match handler_timeout_ms {
        Some(0) => Duration::from_secs(u64::from(u32::MAX)),
        Some(ms) => Duration::from_millis(u64::from(ms)),
        None => Duration::from_secs(60),
    };

    let access = match access {
        OrgAccess::SameOrg => net_sdk::org::OrgAccess::SameOrg,
        OrgAccess::Granted => net_sdk::org::OrgAccess::Granted,
    };

    // `serve_org_bytes_node` -> `serve_rpc_*` spawns an inbound-event bridge
    // with a bare `tokio::spawn`, which needs an ambient runtime. This is a
    // SYNC `#[napi]` method, so it runs on the JS thread with no runtime (napi's
    // is only reachable via `env.spawn_future`). Enter our own for the
    // registration; the bridge task then runs on it. Without this, the first
    // live serve panics "there is no reactor running" — the refusal-only
    // `org_binding.test.ts` never reached it; `org_live.test.ts` does.
    let handle = {
        let _rt_guard = org_serve_runtime().enter();
        net_sdk::org::serve_org_bytes_node(
            node,
            &service,
            access,
            move |caller: net_sdk::org::OrgCaller, body: bytes::Bytes| {
                let tsfn = tsfn.clone();
                async move { dispatch_to_js(tsfn, caller, body, timeout).await }
            },
        )
        // A serve REGISTRATION failure is a local provider-setup error, not one
        // of the four org CALL domains — surface it plain (no `org:` prefix), the
        // same convention `install_org_authority` / `install_provider_grant_audience`
        // use, so it never misclassifies as an `unknown`-domain org error.
        .map_err(|e| Error::from_reason(format!("org serve registration failed: {e}")))?
    };

    Ok(OrgServeHandle::from_handle(handle))
}

/// A dedicated multi-thread runtime for the sync `serve_org` registration so the
/// SDK's serve bridge (`serve_rpc_*` -> `tokio::spawn`) has an ambient runtime.
/// napi's own runtime is only reachable from `env.spawn_future`, and org serve
/// is documented as synchronous (a quick register), so a dedicated runtime — the
/// same shape `org-ffi` uses on the C ABI — keeps the API sync. The bridge task
/// lives here for the service's lifetime; the TSFN it calls is threadsafe, so
/// cross-runtime dispatch to the JS thread is sound.
pub(crate) fn org_serve_runtime() -> &'static tokio::runtime::Runtime {
    static RT: std::sync::OnceLock<tokio::runtime::Runtime> = std::sync::OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("net-org-napi-serve")
            .build()
            .expect("build org-serve runtime")
    })
}

/// The `OrgCaller` projection — ONE place that turns the admission-verified
/// facade caller into the JS-visible [`OrgCaller`], shared by the unary
/// bridge and the three streaming bridges so attribution cannot drift per
/// shape. Every field comes from the verified [`net_sdk::org::OrgCaller`]
/// (itself a projection of the canonical `Admitted`); none is caller-claimed
/// and none is derived from routing metadata like `caller_origin`.
fn org_caller_js(caller: &net_sdk::org::OrgCaller) -> OrgCaller {
    OrgCaller {
        entity: Buffer::from(caller.entity.as_bytes().to_vec()),
        acting_org: Buffer::from(caller.acting_org.as_bytes().to_vec()),
        provider_org: Buffer::from(caller.provider_org.as_bytes().to_vec()),
        provider: Buffer::from(caller.provider.as_bytes().to_vec()),
        capability: Buffer::from(caller.capability.as_bytes().to_vec()),
        is_same_org: caller.is_same_org(),
    }
}

/// The two-stage TSFN bridge, following `mesh_rpc.rs`'s streaming handler
/// exactly: stage 1 waits for JS to return a Promise, stage 2 awaits it. BOTH
/// stages share ONE `timeout_at` deadline, so the total handler time can never
/// exceed `handlerTimeoutMs` — a handler that returns a never-resolving promise
/// can no longer wedge a serve-runtime worker forever (an unbounded stage-2
/// `promise.await` previously made the timeout a no-op, since org handlers are
/// always async and all latency lives in stage 2). `NonBlocking` is always
/// used, and a dropped receiver is swallowed (napi-rs escalates an unhandled
/// one to a fatal process exit).
pub(crate) async fn dispatch_to_js(
    tsfn: Arc<OrgHandlerTsfn>,
    caller: net_sdk::org::OrgCaller,
    body: bytes::Bytes,
    timeout: Duration,
) -> std::result::Result<bytes::Bytes, net_sdk::org::OrgHandlerError> {
    let arg = OrgRequest {
        caller: org_caller_js(&caller),
        request: Buffer::from(body.to_vec()),
    };

    let (tx, rx) = tokio::sync::oneshot::channel::<napi::Result<Promise<Buffer>>>();
    let status = tsfn.call_with_return_value(
        arg,
        ThreadsafeFunctionCallMode::NonBlocking,
        move |ret: napi::Result<Promise<Buffer>>, _env| {
            // A dropped receiver means the handler task was cancelled before
            // the JS callback fired — discard silently.
            let _ = tx.send(ret);
            napi::Result::Ok(())
        },
    );
    if status != Status::Ok {
        return Err(net_sdk::org::OrgHandlerError::Internal(format!(
            "TSFN enqueue failed: {status:?}"
        )));
    }

    // One deadline for BOTH stages, so a hung promise is bounded too.
    let deadline = tokio::time::Instant::now() + timeout;

    // Stage 1 — JS returns a Promise.
    let promise = match tokio::time::timeout_at(deadline, rx).await {
        Ok(Ok(Ok(p))) => p,
        Ok(Ok(Err(e))) => {
            return Err(net_sdk::org::OrgHandlerError::Internal(format!(
                "JS org handler threw synchronously: {e}"
            )))
        }
        Ok(Err(_)) => {
            return Err(net_sdk::org::OrgHandlerError::Internal(
                "JS callback channel disconnected before the org handler responded".to_string(),
            ))
        }
        Err(_) => {
            return Err(net_sdk::org::OrgHandlerError::Internal(format!(
                "JS org handler did not respond within {} ms",
                timeout.as_millis()
            )))
        }
    };

    // Stage 2 — await the promise under the SAME deadline, so a never-resolving
    // handler promise cannot hold the request (and its worker) open forever.
    match tokio::time::timeout_at(deadline, promise).await {
        Ok(Ok(buf)) => Ok(bytes::Bytes::from(buf.to_vec())),
        Ok(Err(e)) => Err(net_sdk::org::OrgHandlerError::Application {
            code: ORG_HANDLER_ERROR,
            message: format!("org handler rejected: {e}"),
        }),
        Err(_) => Err(net_sdk::org::OrgHandlerError::Internal(format!(
            "JS org handler promise did not resolve within {} ms",
            timeout.as_millis()
        ))),
    }
}

// ---------------------------------------------------------------------------
// The streaming provider verbs (§4.4) — `serveOrgStreaming` /
// `serveOrgClientStream` / `serveOrgDuplex`, on `org_serve_runtime()`,
// composing the [`org_caller_js`] projection with `mesh_rpc.rs`'s
// `[..]` TSFN argument shape (`StreamingHandlerArgs`' array marshaling).
//
// # Handler-drop contract — the §2.2 / F-S3.1-2 level, STATED EXPLICITLY
//
// Every handler registered here is polled inside the call's retire
// supervisor. When the call retires — caller cancel, deadline, revocation,
// node teardown — the supervisor may DROP the handler future WITHOUT a
// final poll. A JS handler therefore must NOT assume cancellation is a
// handler-side event it will observe: its promise may simply never settle,
// and `try/finally` around the handler body is NOT guaranteed to run.
// Cooperation is best-effort by contract (§2.2 `forced`).
//
// Cancellation is observed through the RETIREMENT OBSERVABLES instead:
//
// - the caller's stream terminates — its final `next()` yields the terminal
//   outcome (`org:rpc:cancelled` / `org:rpc:timeout` / an
//   `org:admission_denied:<coarse>` on revocation), classifiable with
//   `classifyOrgError`;
// - dropping or closing a call handle emits exactly one CANCEL;
// - the `OrgServeHandle` bounds registration (`close()` unregisters).
//
// The response sink / request stream this bridge hands the handler are
// released by the RUST side the moment the handler's promise settles OR the
// supervisor drops the future — never on a handler-side `finally`.
// ---------------------------------------------------------------------------

/// The `[caller, req, sink]` server-streaming handler arguments — the
/// [`org_caller_js`] projection composed with `mesh_rpc.rs`'s
/// `StreamingHandlerArgs` array marshaling (§4.4).
pub struct OrgStreamingHandlerArgs {
    caller: OrgCaller,
    req: Buffer,
    sink: crate::mesh_rpc::JsResponseSink,
}

impl ToNapiValue for OrgStreamingHandlerArgs {
    unsafe fn to_napi_value(
        env: napi::sys::napi_env,
        val: Self,
    ) -> napi::Result<napi::sys::napi_value> {
        // Build a JS array `[caller, req, sink]`, exactly the manual
        // per-element marshaling `mesh_rpc.rs`'s `StreamingHandlerArgs`
        // uses (its `ToNapiValue` comment explains why: hand-written
        // napi impls need explicit element conversion).
        let env_wrapper = napi::Env::from_raw(env);
        let mut arr = env_wrapper.create_array(3)?;
        let caller_val = unsafe { OrgCaller::to_napi_value(env, val.caller)? };
        let req_val = unsafe { Buffer::to_napi_value(env, val.req)? };
        let sink_val = unsafe { crate::mesh_rpc::JsResponseSink::to_napi_value(env, val.sink)? };
        let caller_unknown =
            unsafe { napi::bindgen_prelude::Unknown::from_napi_value(env, caller_val)? };
        let req_unknown = unsafe { napi::bindgen_prelude::Unknown::from_napi_value(env, req_val)? };
        let sink_unknown =
            unsafe { napi::bindgen_prelude::Unknown::from_napi_value(env, sink_val)? };
        arr.set(0, caller_unknown)?;
        arr.set(1, req_unknown)?;
        arr.set(2, sink_unknown)?;
        unsafe { napi::bindgen_prelude::Array::to_napi_value(env, arr) }
    }
}

/// TSFN for server-streaming org handlers. JS side:
/// `(args: [OrgCaller, Buffer, JsResponseSink]) => Promise<Buffer>`. The
/// Promise resolving is the "handler done" signal; the Buffer value is
/// ignored (the fold emits the terminal frame from the bridge's return).
type OrgStreamingHandlerTsfn = ThreadsafeFunction<
    OrgStreamingHandlerArgs,
    Promise<Buffer>,
    OrgStreamingHandlerArgs,
    Status,
    false,
>;

/// The `[caller, stream]` client-streaming handler arguments (§4.4).
pub struct OrgClientStreamHandlerArgs {
    caller: OrgCaller,
    stream: crate::mesh_rpc::JsRequestStream,
}

impl ToNapiValue for OrgClientStreamHandlerArgs {
    unsafe fn to_napi_value(
        env: napi::sys::napi_env,
        val: Self,
    ) -> napi::Result<napi::sys::napi_value> {
        let env_wrapper = napi::Env::from_raw(env);
        let mut arr = env_wrapper.create_array(2)?;
        let caller_val = unsafe { OrgCaller::to_napi_value(env, val.caller)? };
        let stream_val =
            unsafe { crate::mesh_rpc::JsRequestStream::to_napi_value(env, val.stream)? };
        let caller_unknown =
            unsafe { napi::bindgen_prelude::Unknown::from_napi_value(env, caller_val)? };
        let stream_unknown =
            unsafe { napi::bindgen_prelude::Unknown::from_napi_value(env, stream_val)? };
        arr.set(0, caller_unknown)?;
        arr.set(1, stream_unknown)?;
        unsafe { napi::bindgen_prelude::Array::to_napi_value(env, arr) }
    }
}

/// TSFN for client-streaming org handlers. JS side:
/// `(args: [OrgCaller, JsRequestStream]) => Promise<Buffer>` — the resolved
/// Buffer is the terminal response body.
type OrgClientStreamHandlerTsfn = ThreadsafeFunction<
    OrgClientStreamHandlerArgs,
    Promise<Buffer>,
    OrgClientStreamHandlerArgs,
    Status,
    false,
>;

/// The `[caller, stream, sink]` duplex handler arguments (§4.4).
pub struct OrgDuplexHandlerArgs {
    caller: OrgCaller,
    stream: crate::mesh_rpc::JsRequestStream,
    sink: crate::mesh_rpc::JsResponseSink,
}

impl ToNapiValue for OrgDuplexHandlerArgs {
    unsafe fn to_napi_value(
        env: napi::sys::napi_env,
        val: Self,
    ) -> napi::Result<napi::sys::napi_value> {
        let env_wrapper = napi::Env::from_raw(env);
        let mut arr = env_wrapper.create_array(3)?;
        let caller_val = unsafe { OrgCaller::to_napi_value(env, val.caller)? };
        let stream_val =
            unsafe { crate::mesh_rpc::JsRequestStream::to_napi_value(env, val.stream)? };
        let sink_val = unsafe { crate::mesh_rpc::JsResponseSink::to_napi_value(env, val.sink)? };
        let caller_unknown =
            unsafe { napi::bindgen_prelude::Unknown::from_napi_value(env, caller_val)? };
        let stream_unknown =
            unsafe { napi::bindgen_prelude::Unknown::from_napi_value(env, stream_val)? };
        let sink_unknown =
            unsafe { napi::bindgen_prelude::Unknown::from_napi_value(env, sink_val)? };
        arr.set(0, caller_unknown)?;
        arr.set(1, stream_unknown)?;
        arr.set(2, sink_unknown)?;
        unsafe { napi::bindgen_prelude::Array::to_napi_value(env, arr) }
    }
}

/// TSFN for duplex org handlers. JS side:
/// `(args: [OrgCaller, JsRequestStream, JsResponseSink]) => Promise<Buffer>`.
type OrgDuplexHandlerTsfn =
    ThreadsafeFunction<OrgDuplexHandlerArgs, Promise<Buffer>, OrgDuplexHandlerArgs, Status, false>;

/// The one JS-throw/promise-rejection mapping for every org handler shape,
/// mirroring [`dispatch_to_js`]'s: a rejected promise is an APPLICATION
/// error (`ORG_HANDLER_ERROR`, the typed-RPC handler-error band), never an
/// admission denial — `0x0009` is the admission engine's word and a handler
/// cannot counterfeit it. Everything else (a synchronous throw, a dead
/// channel, a timeout) is `Internal`.
fn org_handler_rejection(e: napi::Error) -> net_sdk::org::OrgHandlerError {
    net_sdk::org::OrgHandlerError::Application {
        code: ORG_HANDLER_ERROR,
        message: format!("org handler rejected: {e}"),
    }
}

/// Two-stage TSFN bridge for the server-streaming shape. Mirrors
/// [`dispatch_to_js`] (ONE `timeout_at` deadline across both stages) plus
/// `mesh_rpc.rs`'s `NodeStreamingRpcHandler` sink discipline: the sink slot
/// is retained by the bridge and dropped the moment the handler's promise
/// settles OR this future is dropped (the handler-drop contract above) —
/// never on a V8 GC.
async fn dispatch_streaming_to_js(
    tsfn: Arc<OrgStreamingHandlerTsfn>,
    caller: net_sdk::org::OrgCaller,
    body: bytes::Bytes,
    sink: ::net::adapter::net::cortex::RpcResponseSink,
    timeout: Duration,
) -> std::result::Result<(), net_sdk::org::OrgHandlerError> {
    let sink_slot = Arc::new(parking_lot::Mutex::new(Some(sink)));
    let arg = OrgStreamingHandlerArgs {
        caller: org_caller_js(&caller),
        req: Buffer::from(body.to_vec()),
        sink: crate::mesh_rpc::JsResponseSink {
            inner: sink_slot.clone(),
        },
    };
    let (tx, rx) = tokio::sync::oneshot::channel::<napi::Result<Promise<Buffer>>>();
    let status = tsfn.call_with_return_value(
        arg,
        ThreadsafeFunctionCallMode::NonBlocking,
        move |ret: napi::Result<Promise<Buffer>>, _env| {
            let _ = tx.send(ret);
            napi::Result::Ok(())
        },
    );
    if status != Status::Ok {
        // Drop the sink before bailing — no V8-GC-quantized terminal frame.
        drop(sink_slot.lock().take());
        return Err(net_sdk::org::OrgHandlerError::Internal(format!(
            "TSFN enqueue failed: {status:?}"
        )));
    }
    let deadline = tokio::time::Instant::now() + timeout;
    let promise = match tokio::time::timeout_at(deadline, rx).await {
        Ok(Ok(Ok(p))) => p,
        Ok(Ok(Err(e))) => {
            drop(sink_slot.lock().take());
            return Err(net_sdk::org::OrgHandlerError::Internal(format!(
                "JS org streaming handler threw synchronously: {e}"
            )));
        }
        Ok(Err(_)) => {
            drop(sink_slot.lock().take());
            return Err(net_sdk::org::OrgHandlerError::Internal(
                "JS callback channel disconnected before the org streaming handler responded"
                    .to_string(),
            ));
        }
        Err(_) => {
            drop(sink_slot.lock().take());
            return Err(net_sdk::org::OrgHandlerError::Internal(format!(
                "JS org streaming handler did not respond within {} ms",
                timeout.as_millis()
            )));
        }
    };
    let settled = tokio::time::timeout_at(deadline, promise).await;
    // The handler is done with the sink the instant its promise settles,
    // whichever way it settled (see the handler-drop contract).
    drop(sink_slot.lock().take());
    match settled {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(e)) => Err(org_handler_rejection(e)),
        Err(_) => Err(net_sdk::org::OrgHandlerError::Internal(format!(
            "JS org streaming handler promise did not resolve within {} ms",
            timeout.as_millis()
        ))),
    }
}

/// Build the reused [`crate::mesh_rpc::JsRequestStream`] for an org
/// handler.
///
/// STATED LIMIT, not an accident: the frozen org handler seam
/// (`Fn(OrgCaller, RequestStream, ..)`) hands the binding only the verified
/// [`net_sdk::org::OrgCaller`] and the request stream, and the core
/// `RequestStream` exposes no metadata accessors — so the reused handle's
/// `callerOrigin` / `callId` / `deadlineNs` / `headers` accessors report
/// their documented EMPTY values (`0n` / `[]`) on this surface. Handler
/// attribution rides the `OrgCaller`'s five verified 32-byte facts (which
/// are exact), never the routing hash.
fn org_request_stream(
    requests: ::net::adapter::net::cortex::RequestStream,
) -> crate::mesh_rpc::JsRequestStream {
    crate::mesh_rpc::JsRequestStream {
        inner: Arc::new(tokio::sync::Mutex::new(Some(requests))),
        caller_origin: 0,
        call_id: 0,
        deadline_ns: 0,
        headers: Arc::new(Vec::new()),
    }
}

/// Two-stage TSFN bridge for the client-streaming shape. The resolved
/// Buffer is the call's terminal response body. Request-stream metadata:
/// see [`org_request_stream`].
async fn dispatch_client_stream_to_js(
    tsfn: Arc<OrgClientStreamHandlerTsfn>,
    caller: net_sdk::org::OrgCaller,
    requests: ::net::adapter::net::cortex::RequestStream,
    timeout: Duration,
) -> std::result::Result<bytes::Bytes, net_sdk::org::OrgHandlerError> {
    let arg = OrgClientStreamHandlerArgs {
        caller: org_caller_js(&caller),
        stream: org_request_stream(requests),
    };
    let (tx, rx) = tokio::sync::oneshot::channel::<napi::Result<Promise<Buffer>>>();
    let status = tsfn.call_with_return_value(
        arg,
        ThreadsafeFunctionCallMode::NonBlocking,
        move |ret: napi::Result<Promise<Buffer>>, _env| {
            let _ = tx.send(ret);
            napi::Result::Ok(())
        },
    );
    if status != Status::Ok {
        return Err(net_sdk::org::OrgHandlerError::Internal(format!(
            "TSFN enqueue failed: {status:?}"
        )));
    }
    let deadline = tokio::time::Instant::now() + timeout;
    let promise = match tokio::time::timeout_at(deadline, rx).await {
        Ok(Ok(Ok(p))) => p,
        Ok(Ok(Err(e))) => {
            return Err(net_sdk::org::OrgHandlerError::Internal(format!(
                "JS org client-streaming handler threw synchronously: {e}"
            )))
        }
        Ok(Err(_)) => return Err(net_sdk::org::OrgHandlerError::Internal(
            "JS callback channel disconnected before the org client-streaming handler responded"
                .to_string(),
        )),
        Err(_) => {
            return Err(net_sdk::org::OrgHandlerError::Internal(format!(
                "JS org client-streaming handler did not respond within {} ms",
                timeout.as_millis()
            )))
        }
    };
    match tokio::time::timeout_at(deadline, promise).await {
        Ok(Ok(buf)) => Ok(bytes::Bytes::from(buf.to_vec())),
        Ok(Err(e)) => Err(org_handler_rejection(e)),
        Err(_) => Err(net_sdk::org::OrgHandlerError::Internal(format!(
            "JS org client-streaming handler promise did not resolve within {} ms",
            timeout.as_millis()
        ))),
    }
}

/// Two-stage TSFN bridge for the duplex shape — [`dispatch_streaming_to_js`]'s
/// sink discipline plus [`dispatch_client_stream_to_js`]'s response body
/// handling (duplex returns `Result<(), _>`; the Buffer value is ignored).
async fn dispatch_duplex_to_js(
    tsfn: Arc<OrgDuplexHandlerTsfn>,
    caller: net_sdk::org::OrgCaller,
    requests: ::net::adapter::net::cortex::RequestStream,
    sink: ::net::adapter::net::cortex::RpcResponseSink,
    timeout: Duration,
) -> std::result::Result<(), net_sdk::org::OrgHandlerError> {
    let sink_slot = Arc::new(parking_lot::Mutex::new(Some(sink)));
    let arg = OrgDuplexHandlerArgs {
        caller: org_caller_js(&caller),
        stream: org_request_stream(requests),
        sink: crate::mesh_rpc::JsResponseSink {
            inner: sink_slot.clone(),
        },
    };
    let (tx, rx) = tokio::sync::oneshot::channel::<napi::Result<Promise<Buffer>>>();
    let status = tsfn.call_with_return_value(
        arg,
        ThreadsafeFunctionCallMode::NonBlocking,
        move |ret: napi::Result<Promise<Buffer>>, _env| {
            let _ = tx.send(ret);
            napi::Result::Ok(())
        },
    );
    if status != Status::Ok {
        drop(sink_slot.lock().take());
        return Err(net_sdk::org::OrgHandlerError::Internal(format!(
            "TSFN enqueue failed: {status:?}"
        )));
    }
    let deadline = tokio::time::Instant::now() + timeout;
    let promise = match tokio::time::timeout_at(deadline, rx).await {
        Ok(Ok(Ok(p))) => p,
        Ok(Ok(Err(e))) => {
            drop(sink_slot.lock().take());
            return Err(net_sdk::org::OrgHandlerError::Internal(format!(
                "JS org duplex handler threw synchronously: {e}"
            )));
        }
        Ok(Err(_)) => {
            drop(sink_slot.lock().take());
            return Err(net_sdk::org::OrgHandlerError::Internal(
                "JS callback channel disconnected before the org duplex handler responded"
                    .to_string(),
            ));
        }
        Err(_) => {
            drop(sink_slot.lock().take());
            return Err(net_sdk::org::OrgHandlerError::Internal(format!(
                "JS org duplex handler did not respond within {} ms",
                timeout.as_millis()
            )));
        }
    };
    let settled = tokio::time::timeout_at(deadline, promise).await;
    drop(sink_slot.lock().take());
    match settled {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(e)) => Err(org_handler_rejection(e)),
        Err(_) => Err(net_sdk::org::OrgHandlerError::Internal(format!(
            "JS org duplex handler promise did not resolve within {} ms",
            timeout.as_millis()
        ))),
    }
}

fn org_access(access: OrgAccess) -> net_sdk::org::OrgAccess {
    match access {
        OrgAccess::SameOrg => net_sdk::org::OrgAccess::SameOrg,
        OrgAccess::Granted => net_sdk::org::OrgAccess::Granted,
    }
}

fn org_handler_timeout(handler_timeout_ms: Option<u32>) -> Duration {
    // 0 disables the cap, matching `MeshRpc.serve`'s contract.
    match handler_timeout_ms {
        Some(0) => Duration::from_secs(u64::from(u32::MAX)),
        Some(ms) => Duration::from_millis(u64::from(ms)),
        None => Duration::from_secs(60),
    }
}

/// Serve a protected, privately-discoverable service whose response is a
/// STREAM (§4.4's `serveOrgStreaming`), on [`org_serve_runtime`] over the
/// frozen `serve_org_streaming_bytes_node` seam.
///
/// The handler receives `[caller, req, sink]`: the admission-verified
/// [`OrgCaller`], the raw request body, and the reused
/// `JsResponseSink` to push chunks into. The Promise resolving is the
/// "handler done" signal — the fold emits the terminal frame then (the
/// Buffer value is ignored). Returning a rejected promise surfaces as an
/// application error, never as an admission denial.
///
/// **Handler-drop contract (§2.2 / F-S3.1-2, stated level):** the retire
/// supervisor may drop the handler future WITHOUT a final poll on
/// cancellation/deadline/revocation — do not assume a handler-side
/// cancellation event or a `finally`. Cancellation is observed through the
/// retirement observables (the caller's terminal stream outcome, the one
/// CANCEL from a dropped call handle, this handle's `close()`). The sink is
/// released by Rust when the handler settles or the future drops.
///
/// Requires an installed node authority; `access` selects who may call AND
/// how the service is announced (the unary {@link serveOrg}'s contract,
/// unchanged).
#[napi(
    ts_args_type = "mesh: NetMesh, service: string, access: OrgAccess, handler: (args: [OrgCaller, Buffer, JsResponseSink]) => Promise<Buffer>, handlerTimeoutMs?: number"
)]
pub fn serve_org_streaming(
    mesh: &crate::NetMesh,
    service: String,
    access: OrgAccess,
    handler: Function<'_, OrgStreamingHandlerArgs, Promise<Buffer>>,
    handler_timeout_ms: Option<u32>,
) -> Result<OrgServeHandle> {
    let node = mesh.node_arc_clone()?;
    let tsfn: OrgStreamingHandlerTsfn = handler
        .build_threadsafe_function()
        .callee_handled::<false>()
        .build()?;
    let tsfn = Arc::new(tsfn);
    let timeout = org_handler_timeout(handler_timeout_ms);
    let access = org_access(access);
    let handle = {
        let _rt_guard = org_serve_runtime().enter();
        net_sdk::org::serve_org_streaming_bytes_node(
            node,
            &service,
            access,
            move |caller: net_sdk::org::OrgCaller,
                  body: bytes::Bytes,
                  sink: ::net::adapter::net::cortex::RpcResponseSink| {
                let tsfn = tsfn.clone();
                async move { dispatch_streaming_to_js(tsfn, caller, body, sink, timeout).await }
            },
        )
        .map_err(|e| Error::from_reason(format!("org serve registration failed: {e}")))?
    };
    Ok(OrgServeHandle::from_handle(handle))
}

/// Serve a protected, privately-discoverable service with a STREAM OF
/// REQUESTS and one terminal response (§4.4's `serveOrgClientStream`), on
/// [`org_serve_runtime`] over the frozen `serve_org_client_stream_bytes_node`
/// seam.
///
/// The handler receives `[caller, stream]` and returns (a Promise of) the
/// terminal response body. Drain `stream.next()` until `null` (clean
/// EOF/half-close). The request stream's `callerOrigin`/`callId`/
/// `deadlineNs`/`headers` accessors report their documented EMPTY values on
/// this surface — see [`org_request_stream`]; attribution rides the
/// verified [`OrgCaller`].
///
/// **Handler-drop contract (§2.2 / F-S3.1-2, stated level):** see
/// [`serve_org_streaming`] — the retire supervisor may drop the handler
/// future without a final poll; cancellation is observed through the
/// retirement observables, never assumed as a handler-side event.
#[napi(
    ts_args_type = "mesh: NetMesh, service: string, access: OrgAccess, handler: (args: [OrgCaller, JsRequestStream]) => Promise<Buffer>, handlerTimeoutMs?: number"
)]
pub fn serve_org_client_stream(
    mesh: &crate::NetMesh,
    service: String,
    access: OrgAccess,
    handler: Function<'_, OrgClientStreamHandlerArgs, Promise<Buffer>>,
    handler_timeout_ms: Option<u32>,
) -> Result<OrgServeHandle> {
    let node = mesh.node_arc_clone()?;
    let tsfn: OrgClientStreamHandlerTsfn = handler
        .build_threadsafe_function()
        .callee_handled::<false>()
        .build()?;
    let tsfn = Arc::new(tsfn);
    let timeout = org_handler_timeout(handler_timeout_ms);
    let access = org_access(access);
    let handle = {
        let _rt_guard = org_serve_runtime().enter();
        net_sdk::org::serve_org_client_stream_bytes_node(
            node,
            &service,
            access,
            move |caller: net_sdk::org::OrgCaller,
                  requests: ::net::adapter::net::cortex::RequestStream| {
                let tsfn = tsfn.clone();
                async move { dispatch_client_stream_to_js(tsfn, caller, requests, timeout).await }
            },
        )
        .map_err(|e| Error::from_reason(format!("org serve registration failed: {e}")))?
    };
    Ok(OrgServeHandle::from_handle(handle))
}

/// Serve a protected, privately-discoverable DUPLEX service (§4.4's
/// `serveOrgDuplex`), on [`org_serve_runtime`] over the frozen
/// `serve_org_duplex_bytes_node` seam.
///
/// The handler receives `[caller, stream, sink]`; the Promise resolving is
/// the "handler done" signal (the Buffer value is ignored). Both directions
/// are independent — emit before/after/while draining. The request stream's
/// metadata accessors report their documented EMPTY values on this surface
/// — see [`org_request_stream`].
///
/// **Handler-drop contract (§2.2 / F-S3.1-2, stated level):** see
/// [`serve_org_streaming`] — the retire supervisor may drop the handler
/// future without a final poll; cancellation is observed through the
/// retirement observables, never assumed as a handler-side event. The sink
/// is released by Rust when the handler settles or the future drops.
#[napi(
    ts_args_type = "mesh: NetMesh, service: string, access: OrgAccess, handler: (args: [OrgCaller, JsRequestStream, JsResponseSink]) => Promise<Buffer>, handlerTimeoutMs?: number"
)]
pub fn serve_org_duplex(
    mesh: &crate::NetMesh,
    service: String,
    access: OrgAccess,
    handler: Function<'_, OrgDuplexHandlerArgs, Promise<Buffer>>,
    handler_timeout_ms: Option<u32>,
) -> Result<OrgServeHandle> {
    let node = mesh.node_arc_clone()?;
    let tsfn: OrgDuplexHandlerTsfn = handler
        .build_threadsafe_function()
        .callee_handled::<false>()
        .build()?;
    let tsfn = Arc::new(tsfn);
    let timeout = org_handler_timeout(handler_timeout_ms);
    let access = org_access(access);
    let handle = {
        let _rt_guard = org_serve_runtime().enter();
        net_sdk::org::serve_org_duplex_bytes_node(
            node,
            &service,
            access,
            move |caller: net_sdk::org::OrgCaller,
                  requests: ::net::adapter::net::cortex::RequestStream,
                  sink: ::net::adapter::net::cortex::RpcResponseSink| {
                let tsfn = tsfn.clone();
                async move { dispatch_duplex_to_js(tsfn, caller, requests, sink, timeout).await }
            },
        )
        .map_err(|e| Error::from_reason(format!("org serve registration failed: {e}")))?
    };
    Ok(OrgServeHandle::from_handle(handle))
}

// ---------------------------------------------------------------------------
// Test-only provisioning (`test-helpers`) — the same-org live scenario.
//
// The Rust-minted fixtures (`gen_org_scenario`) cover the GRANTED
// (cross-org) cell only; a same-org streaming live test needs two nodes in
// ONE org sharing §3.4's out-of-band owner audience, which no generator
// writes. This minter is the Node row's equivalent of the Rust live
// fixture's `fast_mesh(.., shared_audience)` provisioning — adoption (the
// operator ceremony) plus the shared audience staging — reachable only from
// a `--features test-helpers` build (the vitest build), exactly like
// `NetMesh::test_inject_synthetic_peer`. It is NOT exported to production
// consumers.
// ---------------------------------------------------------------------------

/// The provider node's identity seed (hex in the manifest) — `EntityKeypair`
/// semantics so `NetMesh.create({ identitySeed })` reconstructs the exact
/// entity the certs name.
#[cfg(feature = "test-helpers")]
const TEST_PROVIDER_SEED: [u8; 32] = [0x41u8; 32];
/// The caller node's identity seed.
#[cfg(feature = "test-helpers")]
const TEST_CALLER_SEED: [u8; 32] = [0x42u8; 32];
/// The single organization both nodes belong to.
#[cfg(feature = "test-helpers")]
const TEST_ORG_SEED: [u8; 32] = [0xA4u8; 32];
/// Validity window for every minted cert/grant — fresh per run.
#[cfg(feature = "test-helpers")]
const TEST_TTL_SECS: u64 = 3600;

#[cfg(feature = "test-helpers")]
fn test_to_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Mint a complete same-org scenario into `outdir` and return its manifest
/// as JSON: two adopted node authorities in ONE org sharing one owner
/// audience (the §3.4 out-of-band pre-staging, without which owner-private
/// discovery could never open the other side's envelopes), plus the caller's
/// membership + wide-open dispatcher grant (no capability grants — same-org
/// admission is `OwnerDelegated`).
///
/// The shared audience is staged by rewriting the caller authority
/// directory's `owner-audience.key` with the provider's adopted audience
/// AFTER both adoptions (adoption mints one when absent and preserves an
/// existing same-org one, but a pre-existing audience would flip its
/// provisioning expectation; the post-adopt rewrite is the exact file state
/// `NodeAuthority::open` — i.e. `installOrgAuthority` — loads).
#[cfg(feature = "test-helpers")]
#[napi]
pub fn test_mint_same_org_scenario(outdir: String) -> Result<String> {
    use net_sdk::org::types::{
        DispatcherScope, NodeAuthority, OrgDispatcherGrant, OrgKeypair, OrgMembershipCert,
        OWNER_AUDIENCE_FILE,
    };

    let outdir = std::path::PathBuf::from(outdir);
    let org = OrgKeypair::from_bytes(TEST_ORG_SEED);
    let provider_kp = ::net::adapter::net::identity::EntityKeypair::from_bytes(TEST_PROVIDER_SEED);
    let caller_kp = ::net::adapter::net::identity::EntityKeypair::from_bytes(TEST_CALLER_SEED);
    let provider_entity = provider_kp.entity_id().clone();
    let caller_entity = caller_kp.entity_id().clone();

    let provider_dir = outdir.join("provider");
    let caller_dir = outdir.join("caller");
    let provider_auth = provider_dir.join("authority");
    let caller_auth = caller_dir.join("authority");
    std::fs::create_dir_all(&provider_dir).map_err(|e| Error::from_reason(e.to_string()))?;
    std::fs::create_dir_all(&caller_dir).map_err(|e| Error::from_reason(e.to_string()))?;

    // The adoption ceremony (`net node adopt`'s exact shape) for both nodes.
    let provider_cert =
        OrgMembershipCert::try_issue(&org, provider_entity.clone(), 1, TEST_TTL_SECS)
            .map_err(|e| Error::from_reason(e.to_string()))?;
    NodeAuthority::adopt(&provider_auth, provider_cert, &provider_entity, 0, None)
        .map_err(|e| Error::from_reason(e.to_string()))?;
    let caller_cert = OrgMembershipCert::try_issue(&org, caller_entity.clone(), 1, TEST_TTL_SECS)
        .map_err(|e| Error::from_reason(e.to_string()))?;
    NodeAuthority::adopt(&caller_auth, caller_cert.clone(), &caller_entity, 0, None)
        .map_err(|e| Error::from_reason(e.to_string()))?;

    // Stage the ONE per-organization owner audience across both authority
    // dirs: read the provider's adopted audience and install it as the
    // caller's (the caller's minted one is replaced). The file already
    // exists owner-only from the adopt above, so a truncating rewrite keeps
    // its checked permissions.
    let shared = NodeAuthority::open(&provider_auth, &provider_entity)
        .map_err(|e| Error::from_reason(e.to_string()))?;
    let audience_bytes = shared.audience.encode_config();
    std::fs::write(caller_auth.join(OWNER_AUDIENCE_FILE), audience_bytes)
        .map_err(|e| Error::from_reason(e.to_string()))?;

    // The caller's credentials — membership + a wide-open dispatcher grant
    // (no capability grants: same-org is OwnerDelegated).
    let dispatcher = OrgDispatcherGrant::try_issue(
        &org,
        caller_entity.clone(),
        DispatcherScope::Any,
        TEST_TTL_SECS,
    )
    .map_err(|e| Error::from_reason(e.to_string()))?;
    std::fs::write(caller_dir.join("membership.bin"), caller_cert.to_bytes())
        .map_err(|e| Error::from_reason(e.to_string()))?;
    std::fs::write(caller_dir.join("dispatcher.bin"), dispatcher.to_bytes())
        .map_err(|e| Error::from_reason(e.to_string()))?;

    let manifest = serde_json::json!({
        "version": 1,
        "description": "test-helpers same-org streaming scenario: provider and \
                        caller in ONE org sharing one owner audience. GENERATED \
                        fresh per run (certs expire) — do not commit.",
        "org_id_hex": test_to_hex(org.org_id().as_bytes()),
        "provider": {
            "seed_hex": test_to_hex(&TEST_PROVIDER_SEED),
            "entity_id_hex": test_to_hex(provider_entity.as_bytes()),
            "authority_dir": "provider/authority",
        },
        "caller": {
            "seed_hex": test_to_hex(&TEST_CALLER_SEED),
            "entity_id_hex": test_to_hex(caller_entity.as_bytes()),
            "authority_dir": "caller/authority",
            "membership_path": "caller/membership.bin",
            "dispatcher_path": "caller/dispatcher.bin",
        },
    });
    let json =
        serde_json::to_string_pretty(&manifest).map_err(|e| Error::from_reason(e.to_string()))?;
    std::fs::write(outdir.join("manifest.json"), &json)
        .map_err(|e| Error::from_reason(e.to_string()))?;
    Ok(json)
}
