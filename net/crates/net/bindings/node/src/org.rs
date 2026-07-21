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

use arc_swap::ArcSwapOption;
use napi::bindgen_prelude::*;
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
