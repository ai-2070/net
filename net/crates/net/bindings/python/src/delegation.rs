//! PyO3 surface for delegated agent identity (`HERMES_INTEGRATION_PLAN.md`
//! Phase 3).
//!
//! Thin wrappers over `net_sdk::delegation`: a [`PyDelegationChain`]
//! (`root → machine → gateway → subagent`), a shared
//! [`PyRevocationRegistry`], and [`derive_child_identity`] to derive a
//! stable child `Identity` handle from a parent.
//!
//! **H8 (no key material, ever).** Every function here takes and returns
//! opaque `Identity` handles and *public* entity-ids / chain bytes.
//! Private ed25519 seeds never cross into Python: `derive_child_identity`
//! derives the child keypair inside Rust and hands back a handle; the
//! chain is a blob of ed25519-signed public tokens. The one and only
//! implementation of derivation + verification lives in the Rust SDK
//! (bridge doctrine H2) — this file just forwards.

use std::sync::Arc;
use std::time::Duration;

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use pyo3::types::PyBytes;

use net::adapter::net::identity::{EntityId, EntityKeypair, TokenCache};
use net_sdk::delegation::{
    derive_child_seed, DelegationChain, RevocationRegistry, DEFAULT_DELEGATION_DEPTH,
};
use net_sdk::revocation::RevocationStore;
use net_sdk::Identity as SdkIdentity;

use crate::identity::{identity_err, token_err, Identity};

/// The well-known channel every gateway delegation binds to (never
/// actually published to). Exposed as a module constant so Python callers
/// and tests can reference the exact string.
pub const GATEWAY_DELEGATION_CHANNEL: &str = net_sdk::delegation::GATEWAY_DELEGATION_CHANNEL;

/// Re-derive the SDK `Identity` (keypair + cache) from a Python `Identity`
/// handle. Stays inside the crate — the seed bytes are read from the
/// opaque handle and never surface to Python.
fn to_sdk(id: &Identity) -> SdkIdentity {
    SdkIdentity::from_seed(*id.keypair.secret_bytes())
}

fn entity_id_from_bytes(bytes: &[u8]) -> PyResult<EntityId> {
    if bytes.len() != 32 {
        return Err(identity_err(format!(
            "entity_id must be 32 bytes, got {}",
            bytes.len()
        )));
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(bytes);
    Ok(EntityId::from_bytes(arr))
}

/// Derive a stable child `Identity` handle from `parent` under `label`.
///
/// Deterministic (blake3 KDF over the parent seed), so a machine / gateway
/// identity is reproducible across restarts from the root alone — no extra
/// persistence, and every process that holds the root derives the same
/// child. The returned handle owns its keypair; the private seed is never
/// exposed (H8). `label` namespaces siblings, e.g. `"machine:hostA"` vs
/// `"gateway:hostA:hermes"`.
#[pyfunction]
pub fn derive_child_identity(parent: &Identity, label: &str) -> Identity {
    let child_seed = derive_child_seed(parent.keypair.secret_bytes(), label);
    Identity {
        keypair: Arc::new(EntityKeypair::from_bytes(child_seed)),
        cache: Arc::new(TokenCache::new()),
    }
}

/// Shared per-issuer revocation floor. Bumping an issuer's floor
/// invalidates every outstanding delegation from that issuer — including
/// delegated children — the moment `verify` next runs.
///
/// One registry is shared by a gateway and all its subagents (Arc-backed,
/// clone is a handle to the same store), so a revoke is observed by every
/// chain that verifies against it.
#[pyclass(name = "RevocationRegistry", skip_from_py_object)]
#[derive(Clone)]
pub struct PyRevocationRegistry {
    pub(crate) inner: Arc<RevocationRegistry>,
}

#[pymethods]
impl PyRevocationRegistry {
    /// A fresh registry — every issuer's floor is implicitly 0.
    #[new]
    fn new() -> Self {
        Self {
            inner: Arc::new(RevocationRegistry::new()),
        }
    }

    /// Set `issuer`'s floor to `generation`. Any token from `issuer` with
    /// `issuer_generation < generation` is rejected on the next verify.
    /// Monotonic: a value <= the current floor is a no-op.
    fn revoke_below(&self, issuer: &[u8], generation: u32) -> PyResult<()> {
        let id = entity_id_from_bytes(issuer)?;
        self.inner.revoke_below(&id, generation);
        Ok(())
    }

    /// Convenience: revoke every generation-0 delegation from `issuer`
    /// (bumps the floor to 1). Revoking the *machine* identity kills its
    /// gateway chain and, transitively, that gateway's subagents — while a
    /// different machine's chain is untouched.
    fn revoke(&self, issuer: &[u8]) -> PyResult<()> {
        self.revoke_below(issuer, 1)
    }

    /// The current revocation floor for `issuer` (0 if never revoked).
    fn floor(&self, issuer: &[u8]) -> PyResult<u32> {
        let id = entity_id_from_bytes(issuer)?;
        Ok(self.inner.floor(&id))
    }

    /// Reload the machine-shared revocation floors at `path` into this registry
    /// (monotonic — floors only ever rise, so re-loading is idempotent and
    /// composes with any in-process `revoke`). A missing store file is a no-op
    /// (nothing revoked yet); an unreadable or corrupt store raises.
    ///
    /// This is what lets a *caller's* self-check observe an operator's
    /// `net identity revoke` — written to the same file a `net wrap --owner-root`
    /// provider honors — so a revoked chain fails `DelegationChain.verify` on the
    /// caller side too, not only when the provider re-verifies. Use
    /// [`default_revocation_store_path`] (or the `NET_MESH_REVOCATION_STORE`
    /// override) for `path`.
    fn load_from_store(&self, py: Python<'_>, path: &str) -> PyResult<()> {
        let owned = path.to_string();
        let store = py
            .detach(|| RevocationStore::load(&owned))
            .map_err(|e| PyRuntimeError::new_err(format!("revocation store: {e}")))?;
        store.apply_to(&self.inner);
        Ok(())
    }
}

/// The per-user default revocation-store path — the same file a
/// `net wrap --owner-root` provider honors and `net identity revoke` writes —
/// or `None` if neither a data-local nor a home directory resolves. Pass it (or
/// the `NET_MESH_REVOCATION_STORE` override) to
/// [`RevocationRegistry.load_from_store`](PyRevocationRegistry::load_from_store)
/// so a caller-side check observes an operator revocation.
#[pyfunction]
pub fn default_revocation_store_path() -> Option<String> {
    net_sdk::revocation::default_revocation_store_path().map(|p| p.to_string_lossy().into_owned())
}

/// A `root → … → leaf` delegation chain that attributes a capability
/// invocation to the terminal agent identity.
///
/// Built with [`Self::derive_gateway`] and extended per-task with
/// [`Self::extend_to_subagent`]. Verify it against a
/// [`PyRevocationRegistry`] with [`Self::verify`]; serialize with
/// [`Self::to_bytes`] to carry it.
#[pyclass(name = "DelegationChain", skip_from_py_object)]
#[derive(Clone)]
pub struct PyDelegationChain {
    inner: DelegationChain,
}

#[pymethods]
impl PyDelegationChain {
    /// Build a `root → machine → gateway` chain from opaque `Identity`
    /// handles. `root` and `machine` sign their own delegations; only
    /// `gateway`'s public entity-id is used (its keypair stays with the
    /// gateway). `ttl_seconds` is the grant lifetime; the whole chain
    /// expires together. `max_depth` (default 4) leaves room for subagent
    /// hops.
    #[staticmethod]
    #[pyo3(signature = (root, machine, gateway, ttl_seconds, max_depth=None))]
    fn derive_gateway(
        root: &Identity,
        machine: &Identity,
        gateway: &Identity,
        ttl_seconds: u64,
        max_depth: Option<u8>,
    ) -> PyResult<Self> {
        let root_sdk = to_sdk(root);
        let machine_sdk = to_sdk(machine);
        let depth = max_depth.unwrap_or(DEFAULT_DELEGATION_DEPTH);
        let chain = DelegationChain::derive_gateway(
            &root_sdk,
            &machine_sdk,
            gateway.keypair.entity_id(),
            Duration::from_secs(ttl_seconds),
            depth,
        )
        .map_err(token_err)?;
        Ok(Self { inner: chain })
    }

    /// Extend this chain with a `… → subagent` link, signed by the current
    /// leaf's owner (`leaf_signer`, whose entity-id must equal the chain's
    /// current leaf subject — e.g. the gateway extending to a subagent).
    /// The subagent link drops the delegate right but keeps invoke
    /// authority, so its own calls verify and are individually
    /// attributable. Returns a new chain; the original is unchanged.
    fn extend_to_subagent(&self, leaf_signer: &Identity, subagent: &[u8]) -> PyResult<Self> {
        let signer_sdk = to_sdk(leaf_signer);
        let sub_id = entity_id_from_bytes(subagent)?;
        let chain = self
            .inner
            .extend_to_subagent(&signer_sdk, &sub_id)
            .map_err(token_err)?;
        Ok(Self { inner: chain })
    }

    /// `True` if the chain still authorizes an invocation by `presenter`,
    /// anchored at `root`, honoring `registry`. Returns `False` (never
    /// raises) when the chain is expired, revoked, rooted elsewhere, or
    /// presented by the wrong identity — so a caller can gate a `check_fn`
    /// on it directly. `skew_seconds` tolerates clock drift (0 = strict).
    #[pyo3(signature = (presenter, root, registry, skew_seconds=0))]
    fn verify(
        &self,
        presenter: &[u8],
        root: &[u8],
        registry: &PyRevocationRegistry,
        skew_seconds: u64,
    ) -> PyResult<bool> {
        let presenter_id = entity_id_from_bytes(presenter)?;
        let root_id = entity_id_from_bytes(root)?;
        Ok(self
            .inner
            .verify(&presenter_id, &root_id, &registry.inner, skew_seconds)
            .is_ok())
    }

    /// The terminal (leaf) subject entity-id — the agent this chain
    /// attributes to (the gateway, or a subagent after
    /// [`Self::extend_to_subagent`]).
    #[getter]
    fn leaf<'py>(&self, py: Python<'py>) -> Bound<'py, PyBytes> {
        PyBytes::new(py, self.inner.leaf().as_bytes())
    }

    /// The root issuer entity-id the chain anchors at.
    #[getter]
    fn root<'py>(&self, py: Python<'py>) -> Bound<'py, PyBytes> {
        PyBytes::new(py, self.inner.root().as_bytes())
    }

    /// The subject entity-id of each link, root-to-leaf.
    fn subjects<'py>(&self, py: Python<'py>) -> Vec<Bound<'py, PyBytes>> {
        self.inner
            .subjects()
            .iter()
            .map(|s| PyBytes::new(py, s.as_bytes()))
            .collect()
    }

    /// Serialize to wire bytes (a `TokenChain` blob) for carriage on an
    /// invoke or hand-off to another process.
    fn to_bytes<'py>(&self, py: Python<'py>) -> Bound<'py, PyBytes> {
        PyBytes::new(py, &self.inner.to_bytes())
    }

    /// Parse a serialized chain. Raises `TokenError(kind="invalid_format")`
    /// on an empty chain, too many links, or trailing garbage.
    #[staticmethod]
    fn from_bytes(data: &[u8]) -> PyResult<Self> {
        Ok(Self {
            inner: DelegationChain::from_bytes(data).map_err(token_err)?,
        })
    }

    /// Number of delegation links (2 for a bare gateway chain, +1 per
    /// subagent hop).
    fn __len__(&self) -> usize {
        self.inner.len()
    }

    fn __repr__(&self) -> String {
        format!(
            "DelegationChain(links={}, leaf=0x{})",
            self.inner.len(),
            hex::encode(self.inner.leaf().as_bytes())
        )
    }
}
