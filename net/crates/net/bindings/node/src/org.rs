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
        let client = self
            .inner
            .load_full()
            .ok_or_else(|| Error::from_reason("org:closed: this OrgClient has been closed"))?;
        let body = bytes::Bytes::from(request.to_vec());
        let reply = client.call_bytes(&service, body).await.map_err(org_error)?;
        Ok(Buffer::from(reply.to_vec()))
    }

    /// The organization this client acts for, as 32 raw bytes.
    #[napi(getter)]
    pub fn acting_org(&self) -> Result<Buffer> {
        let client = self
            .inner
            .load_full()
            .ok_or_else(|| Error::from_reason("org:closed: this OrgClient has been closed"))?;
        Ok(Buffer::from(client.acting_org().as_bytes().to_vec()))
    }

    /// The entity this client calls as, as 32 raw bytes.
    #[napi(getter)]
    pub fn caller(&self) -> Result<Buffer> {
        let client = self
            .inner
            .load_full()
            .ok_or_else(|| Error::from_reason("org:closed: this OrgClient has been closed"))?;
        Ok(Buffer::from(client.caller().as_bytes().to_vec()))
    }

    /// Release the client: drops the consumer-audience lease and this client's
    /// node reference. Idempotent.
    ///
    /// Call this before `mesh.shutdown()` — see the module docs. In-flight
    /// calls that already snapshotted the client complete normally; calls
    /// started after this return `org:closed`.
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
type OrgHandlerTsfn = ThreadsafeFunction<OrgRequest, Promise<Buffer>, OrgRequest, Status, false>;

/// Handle for a served organization service. `close()` unregisters.
#[napi]
pub struct OrgServeHandle {
    inner: parking_lot::Mutex<Option<net_sdk::mesh_rpc::ServeHandle>>,
}

#[napi]
impl OrgServeHandle {
    /// Unregister the service. Idempotent. In-flight handlers run to
    /// completion.
    #[napi]
    pub fn close(&self) {
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

    let handle = net_sdk::org::serve_org_bytes_node(
        node,
        &service,
        access,
        move |caller: net_sdk::org::OrgCaller, body: bytes::Bytes| {
            let tsfn = tsfn.clone();
            async move { dispatch_to_js(tsfn, caller, body, timeout).await }
        },
    )
    .map_err(|e| Error::from_reason(format!("org:serve_failed: {e}")))?;

    Ok(OrgServeHandle {
        inner: parking_lot::Mutex::new(Some(handle)),
    })
}

/// The two-stage TSFN bridge, following `mesh_rpc.rs`'s RPC handler exactly:
/// stage 1 waits for JS to return a Promise, stage 2 awaits it. Both are
/// bounded, `NonBlocking` is always used, and a dropped receiver is swallowed
/// (napi-rs escalates an unhandled one to a fatal process exit).
async fn dispatch_to_js(
    tsfn: Arc<OrgHandlerTsfn>,
    caller: net_sdk::org::OrgCaller,
    body: bytes::Bytes,
    timeout: Duration,
) -> std::result::Result<bytes::Bytes, net_sdk::org::OrgHandlerError> {
    let arg = OrgRequest {
        caller: OrgCaller {
            entity: Buffer::from(caller.entity.as_bytes().to_vec()),
            acting_org: Buffer::from(caller.acting_org.as_bytes().to_vec()),
            provider_org: Buffer::from(caller.provider_org.as_bytes().to_vec()),
            provider: Buffer::from(caller.provider.as_bytes().to_vec()),
            capability: Buffer::from(caller.capability.as_bytes().to_vec()),
            is_same_org: caller.is_same_org(),
        },
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

    // Stage 1 — JS returns a Promise.
    let promise = match tokio::time::timeout(timeout, rx).await {
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

    // Stage 2 — await it.
    match promise.await {
        Ok(buf) => Ok(bytes::Bytes::from(buf.to_vec())),
        Err(e) => Err(net_sdk::org::OrgHandlerError::Application {
            code: ORG_HANDLER_ERROR,
            message: format!("org handler rejected: {e}"),
        }),
    }
}
