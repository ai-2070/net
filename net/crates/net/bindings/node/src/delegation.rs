//! NAPI surface for delegated agent identity (`HERMES_INTEGRATION_PLAN.md`
//! Phase 3) — the Node twin of the Python `delegation.rs`.
//!
//! Thin wrappers over `net_sdk::delegation`: a [`DelegationChain`]
//! (`root → machine → gateway → subagent`), a shared
//! [`RevocationRegistry`], and [`derive_child_identity`] to derive a
//! stable child `Identity` handle from a parent.
//!
//! **H8 (no key material, ever).** Every function here takes and returns
//! opaque `Identity` handles and *public* entity-ids / chain bytes.
//! Private ed25519 seeds never cross into JS: `deriveChildIdentity`
//! derives the child keypair inside Rust and hands back a handle; the
//! chain is a blob of ed25519-signed public tokens. The one and only
//! implementation of derivation + verification lives in the Rust SDK
//! (bridge doctrine H2) — this file just forwards.

#![cfg(feature = "delegation")]
// napi-derive registers these items via a generated `extern "C"` table the
// dead-code lint can't trace under the test profile.
#![allow(dead_code)]

use napi::bindgen_prelude::*;
use napi_derive::napi;
use std::sync::Arc;
use std::time::Duration;

use net::adapter::net::identity::{EntityId, EntityKeypair};
use net_sdk::delegation::{
    derive_child_seed, DelegationChain as SdkDelegationChain,
    RevocationRegistry as SdkRevocationRegistry, DEFAULT_DELEGATION_DEPTH,
};
use net_sdk::revocation::RevocationStore;

use crate::identity::{token_err_for, Identity};

/// The well-known channel every gateway delegation binds to (never
/// actually published to). Exported so JS callers and tests can
/// reference the exact string.
#[napi]
pub const GATEWAY_DELEGATION_CHANNEL: &str = net_sdk::delegation::GATEWAY_DELEGATION_CHANNEL;

fn delegation_err(msg: impl std::fmt::Display) -> Error {
    Error::from_reason(format!("delegation: {msg}"))
}

/// Decode a 32-byte entity-id `Buffer` into the typed `EntityId`.
/// Shared with the enrollment module (same shape as the Python
/// `entity_id_from_bytes`).
pub(crate) fn entity_id_from_buffer(buf: &Buffer) -> Result<EntityId> {
    let bytes: &[u8] = buf.as_ref();
    if bytes.len() != 32 {
        return Err(delegation_err(format!(
            "entity_id must be 32 bytes, got {}",
            bytes.len()
        )));
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(bytes);
    Ok(EntityId::from_bytes(arr))
}

/// Resolve a JS `BigInt` argument to a `u64`, naming the argument in the
/// rejection. Validation itself is the one shared implementation
/// ([`crate::common::bigint_u64`]). Shared with the enrollment / a2a
/// modules.
pub(crate) fn u64_arg(name: &str, value: BigInt) -> Result<u64> {
    crate::common::bigint_u64(value).map_err(|e| delegation_err(format!("{name}: {}", e.reason)))
}

/// Derive a stable child `Identity` handle from `parent` under `label`.
///
/// Deterministic (blake3 KDF over the parent seed), so a machine / gateway
/// identity is reproducible across restarts from the root alone — no extra
/// persistence, and every process that holds the root derives the same
/// child. The returned handle owns its keypair; the private seed is never
/// exposed (H8). `label` namespaces siblings, e.g. `"machine:hostA"` vs
/// `"gateway:hostA:hermes"`.
#[napi]
pub fn derive_child_identity(parent: &Identity, label: String) -> Identity {
    let child_seed = derive_child_seed(parent.secret_seed(), &label);
    Identity::from_keypair_arc(Arc::new(EntityKeypair::from_bytes(child_seed)))
}

/// The per-user default revocation-store path — the same file a
/// `net wrap --owner-root` provider honors and `net identity revoke`
/// writes — or `null` if neither a data-local nor a home directory
/// resolves. Pass it (or the `NET_MESH_REVOCATION_STORE` override) to
/// [`RevocationRegistry::load_from_store`] so a caller-side check
/// observes an operator revocation.
#[napi]
pub fn default_revocation_store_path() -> Option<String> {
    net_sdk::revocation::default_revocation_store_path().map(|p| p.to_string_lossy().into_owned())
}

/// Shared per-issuer revocation floor. Bumping an issuer's floor
/// invalidates every outstanding delegation from that issuer — including
/// delegated children — the moment `verify` next runs.
///
/// One registry is shared by a gateway and all its subagents (Arc-backed),
/// so a revoke is observed by every chain that verifies against it.
#[napi]
pub struct RevocationRegistry {
    pub(crate) inner: Arc<SdkRevocationRegistry>,
}

#[napi]
impl RevocationRegistry {
    /// A fresh registry — every issuer's floor is implicitly 0.
    #[napi(constructor)]
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(SdkRevocationRegistry::new()),
        }
    }

    /// Set `issuer`'s floor to `generation`. Any token from `issuer` with
    /// `issuer_generation < generation` is rejected on the next verify.
    /// Monotonic: a value <= the current floor is a no-op.
    #[napi]
    pub fn revoke_below(&self, issuer: Buffer, generation: u32) -> Result<()> {
        let id = entity_id_from_buffer(&issuer)?;
        self.inner.revoke_below(&id, generation);
        Ok(())
    }

    /// Convenience: revoke every generation-0 delegation from `issuer`
    /// (bumps the floor to 1). Revoking the *machine* identity kills its
    /// gateway chain and, transitively, that gateway's subagents — while a
    /// different machine's chain is untouched.
    #[napi]
    pub fn revoke(&self, issuer: Buffer) -> Result<()> {
        self.revoke_below(issuer, 1)
    }

    /// The current revocation floor for `issuer` (0 if never revoked).
    #[napi]
    pub fn floor(&self, issuer: Buffer) -> Result<u32> {
        let id = entity_id_from_buffer(&issuer)?;
        Ok(self.inner.floor(&id))
    }

    /// Reload the machine-shared revocation floors at `path` into this
    /// registry (monotonic — floors only ever rise, so re-loading is
    /// idempotent and composes with any in-process `revoke`). A missing
    /// store file is a no-op (nothing revoked yet); an unreadable or
    /// corrupt store rejects.
    ///
    /// This is what lets a *caller's* self-check observe an operator's
    /// `net identity revoke` — written to the same file a
    /// `net wrap --owner-root` provider honors — so a revoked chain fails
    /// [`DelegationChain::verify`] on the caller side too, not only when
    /// the provider re-verifies. Use [`default_revocation_store_path`]
    /// (or the `NET_MESH_REVOCATION_STORE` override) for `path`.
    #[napi]
    pub async fn load_from_store(&self, path: String) -> Result<()> {
        let store = RevocationStore::load(&path)
            .map_err(|e| delegation_err(format!("revocation store: {e}")))?;
        store.apply_to(&self.inner);
        Ok(())
    }
}

/// A `root → … → leaf` delegation chain that attributes a capability
/// invocation to the terminal agent identity.
///
/// Built with [`Self::derive_gateway`] and extended per-task with
/// [`Self::extend_to_subagent`]. Verify it against a
/// [`RevocationRegistry`] with [`Self::verify`]; serialize with
/// [`Self::to_bytes`] to carry it.
#[napi]
pub struct DelegationChain {
    inner: SdkDelegationChain,
}

impl DelegationChain {
    /// Wrap an SDK chain — used by the enrollment surface (`JoinOutcome`,
    /// `OperatorEnrollment.approve`) to hand back a `DelegationChain` handle.
    pub(crate) fn from_inner(inner: SdkDelegationChain) -> Self {
        Self { inner }
    }

    /// A clone of the underlying SDK chain — used by `DeviceEnrollment` to
    /// bundle a chain handed in from JS.
    pub(crate) fn inner_chain(&self) -> SdkDelegationChain {
        self.inner.clone()
    }
}

#[napi]
impl DelegationChain {
    /// Build a `root → machine → gateway` chain from opaque `Identity`
    /// handles. `root` and `machine` sign their own delegations; only
    /// `gateway`'s public entity-id is used (its keypair stays with the
    /// gateway). `ttlSeconds` is the grant lifetime; the whole chain
    /// expires together. `maxDepth` (default 4) leaves room for subagent
    /// hops.
    #[napi(factory)]
    pub fn derive_gateway(
        root: &Identity,
        machine: &Identity,
        gateway: &Identity,
        ttl_seconds: u32,
        max_depth: Option<u8>,
    ) -> Result<DelegationChain> {
        let root_sdk = root.to_sdk_identity();
        let machine_sdk = machine.to_sdk_identity();
        let depth = max_depth.unwrap_or(DEFAULT_DELEGATION_DEPTH);
        let chain = SdkDelegationChain::derive_gateway(
            &root_sdk,
            &machine_sdk,
            gateway.entity_id_ref(),
            Duration::from_secs(u64::from(ttl_seconds)),
            depth,
        )
        .map_err(token_err_for)?;
        Ok(Self { inner: chain })
    }

    /// Parse a serialized chain. Throws `token: invalid_format` on an
    /// empty chain, too many links, or trailing garbage.
    #[napi(factory)]
    pub fn from_bytes(data: Buffer) -> Result<DelegationChain> {
        Ok(Self {
            inner: SdkDelegationChain::from_bytes(data.as_ref()).map_err(token_err_for)?,
        })
    }

    /// Extend this chain with a `… → subagent` link, signed by the current
    /// leaf's owner (`leafSigner`, whose entity-id must equal the chain's
    /// current leaf subject — e.g. the gateway extending to a subagent).
    /// The subagent link drops the delegate right but keeps invoke
    /// authority, so its own calls verify and are individually
    /// attributable. Returns a new chain; the original is unchanged.
    #[napi]
    pub fn extend_to_subagent(
        &self,
        leaf_signer: &Identity,
        subagent: Buffer,
    ) -> Result<DelegationChain> {
        let signer_sdk = leaf_signer.to_sdk_identity();
        let sub_id = entity_id_from_buffer(&subagent)?;
        let chain = self
            .inner
            .extend_to_subagent(&signer_sdk, &sub_id)
            .map_err(token_err_for)?;
        Ok(Self { inner: chain })
    }

    /// `true` if the chain still authorizes an invocation by `presenter`,
    /// anchored at `root`, honoring `registry`. Returns `false` (never
    /// throws) when the chain is expired, revoked, rooted elsewhere, or
    /// presented by the wrong identity — so a caller can gate a check on
    /// it directly. `skewSeconds` tolerates clock drift (default 0 =
    /// strict).
    #[napi]
    pub fn verify(
        &self,
        presenter: Buffer,
        root: Buffer,
        registry: &RevocationRegistry,
        skew_seconds: Option<u32>,
    ) -> Result<bool> {
        let presenter_id = entity_id_from_buffer(&presenter)?;
        let root_id = entity_id_from_buffer(&root)?;
        Ok(self
            .inner
            .verify(
                &presenter_id,
                &root_id,
                &registry.inner,
                u64::from(skew_seconds.unwrap_or(0)),
            )
            .is_ok())
    }

    /// The terminal (leaf) subject entity-id — the agent this chain
    /// attributes to (the gateway, or a subagent after
    /// [`Self::extend_to_subagent`]).
    #[napi(getter)]
    pub fn leaf(&self) -> Buffer {
        Buffer::from(self.inner.leaf().as_bytes().to_vec())
    }

    /// The root issuer entity-id the chain anchors at.
    #[napi(getter)]
    pub fn root(&self) -> Buffer {
        Buffer::from(self.inner.root().as_bytes().to_vec())
    }

    /// The subject entity-id of each link, root-to-leaf.
    #[napi]
    pub fn subjects(&self) -> Vec<Buffer> {
        self.inner
            .subjects()
            .iter()
            .map(|s| Buffer::from(s.as_bytes().to_vec()))
            .collect()
    }

    /// Serialize to wire bytes (a `TokenChain` blob) for carriage on an
    /// invoke or hand-off to another process.
    #[napi]
    pub fn to_bytes(&self) -> Buffer {
        Buffer::from(self.inner.to_bytes())
    }

    /// Number of delegation links (2 for a bare gateway chain, +1 per
    /// subagent hop).
    #[napi(getter)]
    pub fn length(&self) -> u32 {
        self.inner.len() as u32
    }
}
