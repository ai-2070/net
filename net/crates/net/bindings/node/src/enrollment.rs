//! NAPI surface for device enrollment (`HERMES_INTEGRATION_PLAN_V2.md`
//! Phase 1) — the Node twin of the Python `enrollment.rs`.
//!
//! Thin wrappers over `net_sdk::enrollment` / `net_sdk::operator` /
//! `net_sdk::devices`: the invite → join → approve handshake and the
//! operator-side device-lifecycle facade, exposed so a JS operator can
//! mint invites, approve requests, and manage devices, and a JS device can
//! build a signed [`JoinRequest`] and verify the [`JoinOutcome`] it gets
//! back.
//!
//! **H8 (no key material, ever).** [`JoinRequest::create`] and
//! [`OperatorEnrollment`] take opaque `Identity` handles; the private
//! ed25519 seed is read inside Rust and never surfaces to JS. Everything
//! crossing the boundary is a *public* entity-id, an invite string, or
//! signed chain bytes. The one implementation of the handshake lives in
//! the Rust SDK (bridge doctrine H2) — this file forwards.
//!
//! The live mesh bridge (`NetMesh.join` / `renew` / `serveEnrollmentAuto`
//! / `rendezvousString`) rides `net_sdk::mesh_enroll` over the node the
//! binding already holds. Unlike the Python binding (whose `NetMesh` owns
//! a per-instance runtime), napi runs `async fn`s on its process-wide
//! tokio runtime, so the round-trips await there directly.

#![cfg(feature = "delegation")]
// napi-derive registers these items via a generated `extern "C"` table the
// dead-code lint can't trace under the test profile.
#![allow(dead_code)]

use napi::bindgen_prelude::*;
use napi_derive::napi;
use parking_lot::Mutex;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use net::adapter::net::channel::ChannelConfigRegistry;
use net::adapter::net::MeshNode;
use net_sdk::delegation::DEFAULT_DELEGATION_DEPTH;
use net_sdk::devices::DeviceRecord as SdkDeviceRecord;
use net_sdk::enrollment::{
    fingerprint as sdk_fingerprint, DeviceEnrollment as SdkDeviceEnrollment,
    InviteToken as SdkInviteToken, JoinOutcome as SdkJoinOutcome, JoinRequest as SdkJoinRequest,
};
use net_sdk::mesh::Mesh as SdkMesh;
use net_sdk::mesh_rpc::ServeHandle;
use net_sdk::operator::OperatorEnrollment as SdkOperatorEnrollment;
use net_sdk::Identity as SdkIdentity;

use crate::delegation::{entity_id_from_buffer, u64_arg, DelegationChain, RevocationRegistry};
use crate::identity::Identity;
use crate::NetMesh;

fn enroll_err(msg: impl std::fmt::Display) -> Error {
    Error::from_reason(format!("enrollment: {msg}"))
}

/// Rebuild a JS `Identity` handle from an SDK identity — the keypair `Arc`
/// is shared; a fresh token cache is attached. The private seed stays in
/// Rust.
fn to_node_identity(sdk: &SdkIdentity) -> Identity {
    Identity::from_keypair_arc(sdk.keypair().clone())
}

/// A short, human-comparable fingerprint of an entity-id (the 32-byte
/// ed25519 public key), shown on both sides of a join so a human can
/// confirm the mesh identity matches — `A1B2-C3D4-E5F6-0789`.
#[napi]
pub fn fingerprint(entity: Buffer) -> Result<String> {
    Ok(sdk_fingerprint(&entity_id_from_buffer(&entity)?))
}

/// A pre-authorization to *ask* to join a mesh — not a key. Carries the
/// mesh `root`, a `rendezvous` locator, a single-use nonce, and a short
/// TTL.
#[napi]
pub struct InviteToken {
    pub(crate) inner: SdkInviteToken,
}

#[napi]
impl InviteToken {
    /// Parse an invite string (`net-invite:<base64url>`). Throws on a
    /// missing prefix, bad base64, or malformed bytes.
    #[napi(factory)]
    pub fn decode(s: String) -> Result<InviteToken> {
        Ok(Self {
            inner: SdkInviteToken::decode(&s).map_err(enroll_err)?,
        })
    }

    /// Parse canonical wire bytes.
    #[napi(factory)]
    pub fn from_bytes(data: Buffer) -> Result<InviteToken> {
        Ok(Self {
            inner: SdkInviteToken::from_bytes(data.as_ref()).map_err(enroll_err)?,
        })
    }

    /// The copy-paste / QR invite string.
    #[napi]
    pub fn encode(&self) -> String {
        self.inner.encode()
    }

    /// The mesh root entity-id this invite admits into (32 bytes).
    #[napi(getter)]
    pub fn root(&self) -> Buffer {
        Buffer::from(self.inner.root.as_bytes().to_vec())
    }

    /// The rendezvous locator the device dials (opaque transport string).
    #[napi(getter)]
    pub fn rendezvous(&self) -> String {
        self.inner.rendezvous.clone()
    }

    /// Unix-seconds expiry.
    #[napi(getter)]
    pub fn expires_at(&self) -> BigInt {
        BigInt::from(self.inner.expires_at)
    }

    /// The displayed fingerprint of the mesh root — show it to the joiner.
    #[napi]
    pub fn root_fingerprint(&self) -> String {
        self.inner.root_fingerprint()
    }

    /// Whether the invite has expired at `now` (unix secs).
    #[napi]
    pub fn is_expired(&self, now: BigInt) -> Result<bool> {
        Ok(self.inner.is_expired(u64_arg("now", now)?))
    }

    /// Canonical wire bytes.
    #[napi]
    pub fn to_bytes(&self) -> Buffer {
        Buffer::from(self.inner.to_bytes())
    }
}

/// A device's request to join, signed by the device's own key.
#[napi]
pub struct JoinRequest {
    pub(crate) inner: SdkJoinRequest,
}

#[napi]
impl JoinRequest {
    /// Build + sign a request against `invite`. `device` is the opaque
    /// `Identity` handle whose key is being enrolled (H8: seed stays in
    /// Rust).
    #[napi(factory)]
    pub fn create(
        device: &Identity,
        name: String,
        tags: Vec<String>,
        invite: &InviteToken,
    ) -> JoinRequest {
        Self {
            inner: SdkJoinRequest::create(&device.to_sdk_identity(), name, tags, &invite.inner),
        }
    }

    /// Parse canonical wire bytes (does not verify the signature).
    #[napi(factory)]
    pub fn from_bytes(data: Buffer) -> Result<JoinRequest> {
        Ok(Self {
            inner: SdkJoinRequest::from_bytes(data.as_ref()).map_err(enroll_err)?,
        })
    }

    /// `true` if the device's self-signature verifies (it holds its key).
    #[napi]
    pub fn verify_self_signature(&self) -> bool {
        self.inner.verify_self_signature().is_ok()
    }

    /// The device entity-id (32 bytes).
    #[napi(getter)]
    pub fn device(&self) -> Buffer {
        Buffer::from(self.inner.device.as_bytes().to_vec())
    }

    /// The device-chosen name.
    #[napi(getter)]
    pub fn name(&self) -> String {
        self.inner.name.clone()
    }

    /// The device-chosen tags.
    #[napi(getter)]
    pub fn tags(&self) -> Vec<String> {
        self.inner.tags.clone()
    }

    /// Canonical wire bytes.
    #[napi]
    pub fn to_bytes(&self) -> Buffer {
        Buffer::from(self.inner.to_bytes())
    }
}

/// The operator's response to a join request — the payload the enrollment
/// RPC returns to the device.
#[napi]
pub struct JoinOutcome {
    inner: SdkJoinOutcome,
}

#[napi]
impl JoinOutcome {
    /// Parse canonical wire bytes.
    #[napi(factory)]
    pub fn from_bytes(data: Buffer) -> Result<JoinOutcome> {
        Ok(Self {
            inner: SdkJoinOutcome::from_bytes(data.as_ref()).map_err(enroll_err)?,
        })
    }

    /// Canonical wire bytes.
    #[napi]
    pub fn to_bytes(&self) -> Buffer {
        Buffer::from(self.inner.to_bytes())
    }

    /// `true` if the device was admitted.
    #[napi(getter)]
    pub fn is_admitted(&self) -> bool {
        matches!(self.inner, SdkJoinOutcome::Admitted { .. })
    }

    /// The stable reject code (`1..=7`) if rejected, else `null`.
    #[napi(getter)]
    pub fn reject_code(&self) -> Option<u16> {
        match &self.inner {
            SdkJoinOutcome::Rejected { code, .. } => Some(*code),
            SdkJoinOutcome::Admitted { .. } => None,
        }
    }

    /// The human reject message if rejected, else `null`.
    #[napi(getter)]
    pub fn reject_message(&self) -> Option<String> {
        match &self.inner {
            SdkJoinOutcome::Rejected { message, .. } => Some(message.clone()),
            SdkJoinOutcome::Admitted { .. } => None,
        }
    }

    /// Device-side: verify the admitted grant anchors at the invited mesh
    /// root (`inviteRoot`) and binds to this `device`, returning the
    /// `DelegationChain`. Throws if the outcome was a rejection, or the
    /// grant is untrusted (wrong root / wrong device) — defending the
    /// joiner against a rogue operator.
    ///
    /// `&self` despite the `into_` name: a `#[napi]` method can't consume
    /// `self` (the object is JS-owned), and the JS-facing name
    /// deliberately mirrors the SDK's `JoinOutcome::into_chain` (pinned by
    /// the Python binding and callers).
    #[allow(clippy::wrong_self_convention)]
    #[napi]
    pub fn into_chain(&self, device: Buffer, invite_root: Buffer) -> Result<DelegationChain> {
        let device_id = entity_id_from_buffer(&device)?;
        let root_id = entity_id_from_buffer(&invite_root)?;
        let chain = self
            .inner
            .clone()
            .into_chain(&device_id, &root_id)
            .map_err(enroll_err)?;
        Ok(DelegationChain::from_inner(chain))
    }
}

/// One enrolled device in the operator's inventory.
#[napi]
pub struct DeviceRecord {
    inner: SdkDeviceRecord,
}

#[napi]
impl DeviceRecord {
    /// The device entity-id (32 bytes).
    #[napi(getter)]
    pub fn device(&self) -> Buffer {
        Buffer::from(self.inner.device.as_bytes().to_vec())
    }

    #[napi(getter)]
    pub fn name(&self) -> String {
        self.inner.name.clone()
    }

    #[napi(getter)]
    pub fn tags(&self) -> Vec<String> {
        self.inner.tags.clone()
    }

    /// Unix-seconds the device enrolled.
    #[napi(getter)]
    pub fn enrolled_at(&self) -> BigInt {
        BigInt::from(self.inner.enrolled_at)
    }

    /// Unix-seconds the device was revoked, or `null` while active.
    #[napi(getter)]
    pub fn revoked_at(&self) -> Option<BigInt> {
        self.inner.revoked_at.map(BigInt::from)
    }

    #[napi(getter)]
    pub fn is_revoked(&self) -> bool {
        self.inner.is_revoked()
    }
}

/// The operator side: mint invites, approve join requests into
/// `root → device` delegations, and manage the device inventory —
/// composing the enrollment authority + device registry + revocation
/// store for one mesh root.
#[napi]
pub struct OperatorEnrollment {
    inner: Arc<SdkOperatorEnrollment>,
}

impl OperatorEnrollment {
    /// Shared handle to the underlying facade — used by the live
    /// `NetMesh.serveEnrollmentAuto` bridge to hand the coordinator to the
    /// nRPC handler.
    pub(crate) fn arc(&self) -> Arc<SdkOperatorEnrollment> {
        self.inner.clone()
    }
}

#[napi]
impl OperatorEnrollment {
    /// Build a coordinator for the `root` `Identity` handle, with explicit
    /// device-registry and revocation-store paths.
    #[napi(constructor)]
    pub fn new(root: &Identity, registry_path: String, revocation_path: String) -> Self {
        Self {
            inner: Arc::new(SdkOperatorEnrollment::new(
                root.to_sdk_identity(),
                PathBuf::from(registry_path),
                PathBuf::from(revocation_path),
            )),
        }
    }

    /// Build using the per-user default store paths (the same
    /// machine-shared files the CLI and a `net wrap` provider converge
    /// on). Throws if neither path resolves.
    #[napi(factory)]
    pub fn with_default_paths(root: &Identity) -> Result<OperatorEnrollment> {
        SdkOperatorEnrollment::with_default_paths(root.to_sdk_identity())
            .map(|inner| Self {
                inner: Arc::new(inner),
            })
            .ok_or_else(|| enroll_err("no default store paths could be resolved"))
    }

    /// The mesh root entity-id (32 bytes).
    #[napi(getter)]
    pub fn root_id(&self) -> Buffer {
        Buffer::from(self.inner.root_id().as_bytes().to_vec())
    }

    /// The displayed fingerprint of the mesh root.
    #[napi]
    pub fn root_fingerprint(&self) -> String {
        self.inner.root_fingerprint()
    }

    /// Mint an invite for this mesh valid for `ttlSeconds`, tracking it so
    /// a later `approve` can match a request to it. `rendezvous` is the
    /// transport locator devices dial (e.g. `NetMesh.rendezvousString()`).
    #[napi]
    pub fn invite(&self, rendezvous: String, ttl_seconds: u32) -> InviteToken {
        InviteToken {
            inner: self
                .inner
                .invite(rendezvous, Duration::from_secs(u64::from(ttl_seconds))),
        }
    }

    /// Approve an arriving request (auto — invite-as-authorization),
    /// reading the system clock: run the fail-closed checks, record the
    /// device, retire the single-use invite, and return the
    /// `root → device` `DelegationChain`. Rejects on any refusal
    /// (unknown/expired/wrong invite, bad signature). Runs off the JS
    /// thread (store file IO).
    #[napi]
    pub async fn approve(
        &self,
        request: &JoinRequest,
        grant_ttl_seconds: u32,
        max_depth: Option<u8>,
    ) -> Result<DelegationChain> {
        let depth = max_depth.unwrap_or(DEFAULT_DELEGATION_DEPTH);
        let ttl = Duration::from_secs(u64::from(grant_ttl_seconds));
        let enrollment = self
            .inner
            .approve(&request.inner, ttl, depth)
            .map_err(enroll_err)?;
        Ok(DelegationChain::from_inner(enrollment.chain))
    }

    /// The **server-side** handler: turn serialized `JoinRequest` bytes
    /// into serialized `JoinOutcome` bytes (auto — invite-as-
    /// authorization). This is what the enrollment RPC moves; a JS host
    /// can serve enrollment by feeding it received request bytes and
    /// returning the outcome bytes. Never throws — a malformed request or
    /// a rejection is a coded `JoinOutcome`.
    #[napi]
    pub async fn handle_join_request(
        &self,
        request_bytes: Buffer,
        grant_ttl_seconds: u32,
        max_depth: Option<u8>,
    ) -> Buffer {
        let depth = max_depth.unwrap_or(DEFAULT_DELEGATION_DEPTH);
        let ttl = Duration::from_secs(u64::from(grant_ttl_seconds));
        Buffer::from(
            self.inner
                .handle_join_request(request_bytes.as_ref(), ttl, depth),
        )
    }

    /// Revoke a device: raise its revocation floor (kills all current
    /// delegations) and stamp the inventory. Reads the system clock.
    #[napi]
    pub async fn revoke(&self, device: Buffer) -> Result<()> {
        let id = entity_id_from_buffer(&device)?;
        self.inner.revoke(&id).map_err(enroll_err)
    }

    /// The enrolled devices in the inventory.
    #[napi]
    pub async fn devices(&self) -> Result<Vec<DeviceRecord>> {
        let records = self.inner.devices().map_err(enroll_err)?;
        Ok(records
            .into_iter()
            .map(|inner| DeviceRecord { inner })
            .collect())
    }

    /// Prune a device from the inventory entirely (orthogonal to revoking
    /// its floor). Returns whether a record existed.
    #[napi]
    pub async fn forget(&self, device: Buffer) -> Result<bool> {
        let id = entity_id_from_buffer(&device)?;
        self.inner.forget(&id).map_err(enroll_err)
    }

    /// Outstanding (minted, unredeemed, unexpired at `now` unix-secs)
    /// invites.
    #[napi]
    pub fn pending_invites(&self, now: BigInt) -> Result<Vec<InviteToken>> {
        Ok(self
            .inner
            .pending_invites(u64_arg("now", now)?)
            .into_iter()
            .map(|inner| InviteToken { inner })
            .collect())
    }
}

/// A device's **persisted** enrollment — its own key + the
/// `root → device` grant it received — so it survives restarts without
/// re-pairing. The device seed stays in Rust (H8); [`Self::device`] hands
/// back an opaque `Identity`.
#[napi]
pub struct DeviceEnrollment {
    inner: SdkDeviceEnrollment,
}

impl DeviceEnrollment {
    /// The underlying SDK enrollment — used by `NetMesh.renew`.
    pub(crate) fn inner_ref(&self) -> &SdkDeviceEnrollment {
        &self.inner
    }
}

#[napi]
impl DeviceEnrollment {
    /// Bundle a device `Identity` handle with the `root → device` chain it
    /// received from `join`, the operator's `rendezvous` locator (from the
    /// invite, for renewal), and the unix-seconds it enrolled.
    #[napi(constructor)]
    pub fn new(
        device: &Identity,
        chain: &DelegationChain,
        rendezvous: String,
        enrolled_at: BigInt,
    ) -> Result<Self> {
        Ok(Self {
            inner: SdkDeviceEnrollment::new(
                device.to_sdk_identity(),
                chain.inner_chain(),
                rendezvous,
                u64_arg("enrolledAt", enrolled_at)?,
            ),
        })
    }

    /// Load a persisted enrollment from `path`. Resolves `null` if none is
    /// saved yet; rejects on a corrupt file.
    #[napi]
    pub async fn load(path: String) -> Result<Option<DeviceEnrollment>> {
        let loaded = SdkDeviceEnrollment::load(&path).map_err(enroll_err)?;
        Ok(loaded.map(|inner| Self { inner }))
    }

    /// Persist to `path` (`0600`, atomic). Overwrites — e.g. after a
    /// renewal.
    #[napi]
    pub async fn save(&self, path: String) -> Result<()> {
        self.inner.save(&path).map_err(enroll_err)
    }

    /// The device's opaque `Identity` handle (its private seed stays in
    /// Rust) — use it to extend the grant to a gateway.
    #[napi(getter)]
    pub fn device(&self) -> Identity {
        to_node_identity(self.inner.device())
    }

    /// The `root → device` delegation chain.
    #[napi(getter)]
    pub fn chain(&self) -> DelegationChain {
        DelegationChain::from_inner(self.inner.chain().clone())
    }

    /// The operator's rendezvous locator — where the device dials to
    /// renew.
    #[napi(getter)]
    pub fn rendezvous(&self) -> String {
        self.inner.rendezvous().to_string()
    }

    /// The mesh root the grant anchors at (32 bytes).
    #[napi(getter)]
    pub fn root(&self) -> Buffer {
        Buffer::from(self.inner.root().as_bytes().to_vec())
    }

    /// Unix-seconds the device enrolled.
    #[napi(getter)]
    pub fn enrolled_at(&self) -> BigInt {
        BigInt::from(self.inner.enrolled_at())
    }

    /// Unix-seconds the grant expires.
    #[napi(getter)]
    pub fn expires_at(&self) -> BigInt {
        BigInt::from(self.inner.expires_at())
    }

    /// Whether the grant still verifies + is unexpired. Pass a
    /// `RevocationRegistry` (an empty one is fine device-side — the
    /// provider enforces revocation on invoke). `skewSeconds` tolerates
    /// clock drift (default 0 = strict).
    #[napi]
    pub fn is_valid(&self, revocation: &RevocationRegistry, skew_seconds: Option<u32>) -> bool {
        self.inner
            .is_valid(&revocation.inner, u64::from(skew_seconds.unwrap_or(0)))
    }

    /// Whether the grant is within `windowSeconds` of expiry at `now`
    /// (unix secs) — the trigger for silent renewal.
    #[napi]
    pub fn needs_renewal(&self, window_seconds: u32, now: BigInt) -> Result<bool> {
        Ok(self
            .inner
            .needs_renewal(u64::from(window_seconds), u64_arg("now", now)?))
    }
}

// -----------------------------------------------------------------------------
// Live mesh bridge — the `NetMesh` methods that drive the SDK `Mesh` over the
// raw `MeshNode` the binding holds. The wire orchestration lives once in the
// SDK (`net_sdk::mesh_enroll`); this just wraps the node in a `Mesh` and
// forwards (bridge doctrine H2).
// -----------------------------------------------------------------------------

/// Wrap a raw node in an SDK `Mesh` sharing the same live node. A fresh
/// channel registry is fine — nRPC dispatch lives on the node; the registry
/// is auxiliary bookkeeping the served handle keeps alive. Mirrors the
/// Python `mesh_over`; shared with the `a2a` module.
pub(crate) fn mesh_over(node: Arc<MeshNode>, identity: Option<SdkIdentity>) -> SdkMesh {
    SdkMesh::from_node_arc(node, Arc::new(ChannelConfigRegistry::new()), identity)
}

/// Keeps the served enrollment services alive (returned by
/// `NetMesh.serveEnrollmentAuto`). Dropping it or calling
/// [`stop`](Self::stop) unregisters them.
#[napi]
pub struct EnrollmentServeHandle {
    // The `Mesh` holds the channel registry the services registered against
    // and each `ServeHandle` one dispatcher registration (enroll + renew) —
    // all must outlive the services. A `parking_lot::Mutex` because napi
    // hands out `&self`; a `#[napi]` class is GC-finalized, not
    // scope-dropped, so `stop()` is the deterministic release
    // (the `close()` gotcha in `bindings.md`).
    inner: Mutex<Option<(SdkMesh, Vec<ServeHandle>)>>,
}

#[napi]
impl EnrollmentServeHandle {
    /// Stop serving enrollment (unregister the services). Idempotent.
    #[napi]
    pub fn stop(&self) {
        let _ = self.inner.lock().take();
    }

    /// Whether the services are still registered.
    #[napi(getter)]
    pub fn serving(&self) -> bool {
        self.inner.lock().is_some()
    }
}

#[napi]
impl NetMesh {
    /// The invite `rendezvous` locator for this node (addr + Noise pubkey
    /// + node id), to pass to `OperatorEnrollment.invite`. Devices dial it
    /// via `join`. (Requires the `delegation` feature.)
    #[napi]
    pub fn rendezvous_string(&self) -> Result<String> {
        Ok(mesh_over(self.node_arc_clone()?, None).rendezvous_string())
    }

    /// Device-side enrollment: enroll `device`'s key into the mesh named
    /// by the `invite` string, under `name` + `tags`, returning the
    /// verified `root → device` `DelegationChain`. This node must be
    /// `start()`ed and built with `permissiveChannels: true` (the
    /// enrollment nRPC uses dynamic per-caller reply channels the strict
    /// registry rejects). (Requires the `delegation` feature.)
    #[napi]
    pub async fn join(
        &self,
        device: &Identity,
        invite: String,
        name: String,
        tags: Option<Vec<String>>,
    ) -> Result<DelegationChain> {
        let node = self.node_arc_clone()?;
        let mesh = mesh_over(node, Some(device.to_sdk_identity()));
        let chain = mesh
            .join(&invite, name, tags.unwrap_or_default())
            .await
            .map_err(enroll_err)?;
        Ok(DelegationChain::from_inner(chain))
    }

    /// Operator-side: serve the full device lifecycle on this node (auto —
    /// the invite is the authorization): **enroll** (join) + **renew**.
    /// Hold the resolved handle for as long as enrollment should stay
    /// open; call `handle.stop()` before `shutdown()`. This node must be
    /// `start()`ed and built with `permissiveChannels: true`. (Requires
    /// the `delegation` feature.)
    ///
    /// `async` although the registration itself is synchronous: the SDK's
    /// `serve_rpc` spawns a response-drainer task, which needs the tokio
    /// runtime context napi only provides to `async fn`s — a plain sync
    /// method here panics with "no reactor running" (the same reason the
    /// Python binding wraps this in `runtime.enter()`).
    #[napi]
    pub async fn serve_enrollment_auto(
        &self,
        operator: &OperatorEnrollment,
        grant_ttl_seconds: u32,
        max_depth: Option<u8>,
    ) -> Result<EnrollmentServeHandle> {
        let depth = max_depth.unwrap_or(DEFAULT_DELEGATION_DEPTH);
        let ttl = Duration::from_secs(u64::from(grant_ttl_seconds));
        let mesh = mesh_over(self.node_arc_clone()?, None);
        let op = operator.arc();
        let enroll = mesh
            .serve_enrollment_auto(op.clone(), ttl, depth)
            .map_err(enroll_err)?;
        let renew = mesh
            .serve_renewal_auto(op, ttl, depth)
            .map_err(enroll_err)?;
        Ok(EnrollmentServeHandle {
            inner: Mutex::new(Some((mesh, vec![enroll, renew]))),
        })
    }

    /// Device-side renewal: refresh the grant carried by `enrollment` over
    /// the mesh, returning the verified **fresh** `root → device`
    /// `DelegationChain`. This node must be `start()`ed and built with
    /// `permissiveChannels: true`. (Requires the `delegation` feature.)
    #[napi]
    pub async fn renew(&self, enrollment: &DeviceEnrollment) -> Result<DelegationChain> {
        let node = self.node_arc_clone()?;
        let inner = enrollment.inner_ref();
        let mesh = mesh_over(node, Some(inner.device().clone()));
        let renewed = mesh
            .renew(inner.rendezvous(), inner.chain())
            .await
            .map_err(enroll_err)?;
        Ok(DelegationChain::from_inner(renewed))
    }
}
