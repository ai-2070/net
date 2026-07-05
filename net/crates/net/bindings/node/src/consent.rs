// `#[napi]` exports to JS leave items "unused" from Rust's POV, so
// clippy's dead-code analysis doesn't apply to this module. Suppress
// at file scope.
#![allow(dead_code)]

//! NAPI surface for the local consent surface (`MCP_BRIDGE_SDK_PLAN.md`
//! P2) — thin wrappers over `net_sdk::consent` / `net_sdk::pins`, the
//! surfaces that graduated out of the MCP bridge in P0.
//!
//! Doctrine #1 applies with force: **no logic in bindings**. Identity
//! canonicalization, the consent decision, and above all the pin store's
//! atomic-save + cross-process lock protocol live in the one Rust
//! implementation; this module marshals arguments and results.
//!
//! Two shapes follow:
//!
//! - Capability ids cross the boundary as their `provider/capability`
//!   display **string**; the core parses (and therefore canonicalizes)
//!   them, so `0x2a/echo` and `42/echo` key the same records. The
//!   [`CapabilityId`] class is the parse / inspect / canonicalize helper.
//! - [`PinStore`] is a *path-scoped handle*, not an open snapshot. Its
//!   methods are Promise-returning: every read loads a fresh snapshot and
//!   every mutation runs a full locked `PinStore::mutate` transaction on
//!   napi's tokio runtime, so JS can never do an unlocked
//!   read-modify-write, hold a stale snapshot across a save, or open the
//!   store file directly. The same file the `net mcp pin` CLI and a
//!   running `net mcp serve` shim use is honored bidirectionally.
//!
//! Decisions and states cross as the structured enums' stable string
//! forms (`"pending"`/`"approved"`, `"allowed"`/`"requires_approval"`) —
//! computed in Rust, never re-derived in JS. The approval split is
//! preserved verbatim: `request` only ever writes a *pending* record (the
//! model-callable verb); `approve`/`reject` are the operator verbs an
//! embedding agent runtime must keep out of its model loop.
//!
//! Errors: store I/O / corruption throws with a `pins:` prefix; a
//! malformed capability id throws with a `consent:` prefix — the same
//! stable-prefix convention the identity module uses.

use napi::bindgen_prelude::*;
use napi_derive::napi;
use parking_lot::Mutex;

use net_sdk::consent::{
    CapabilityId as CoreCapabilityId, ConsentPolicy as CoreConsentPolicy, CredentialStatus,
};
use net_sdk::pins::{PinState, PinStore as CorePinStore, PinStoreError};

const ERR_CONSENT_PREFIX: &str = "consent:";
const ERR_PINS_PREFIX: &str = "pins:";

fn consent_err(msg: impl std::fmt::Display) -> Error {
    Error::from_reason(format!("{ERR_CONSENT_PREFIX} {msg}"))
}

fn pins_err(e: PinStoreError) -> Error {
    Error::from_reason(format!("{ERR_PINS_PREFIX} {e}"))
}

/// Parse (and canonicalize) a `provider/capability` display string into a
/// core id, mapping a parse failure to the `consent:` prefix.
fn parse_cap(cap_id: &str) -> Result<CoreCapabilityId> {
    CoreCapabilityId::parse(cap_id).map_err(consent_err)
}

/// The stable string form of a pin state.
fn pin_state_str(state: PinState) -> &'static str {
    match state {
        PinState::Pending => "pending",
        PinState::Approved => "approved",
    }
}

/// One pin record — the JS shape `{ capId, state }`.
#[napi(object)]
pub struct PinRecordJs {
    /// The capability's `provider/capability` display id.
    pub cap_id: String,
    /// `"pending"` or `"approved"`.
    pub state: String,
}

/// A capability's canonical identity: `provider/capability`. Construction
/// and parsing canonicalize the provider (whitespace, `0x`-hex node ids),
/// so a pin or consent record keyed through this type can never miss a
/// differently spelled twin. Purely a parse / inspect helper — the
/// consent and pin APIs take the `provider/capability` string directly
/// and canonicalize internally.
#[napi]
pub struct CapabilityId {
    inner: CoreCapabilityId,
}

#[napi]
impl CapabilityId {
    /// Build from parts. The provider is canonicalized.
    #[napi(constructor)]
    pub fn new(provider: String, capability: String) -> Self {
        Self {
            inner: CoreCapabilityId::new(provider, capability),
        }
    }

    /// Parse the `provider/capability` display form (splits on the FIRST
    /// `/`; the capability half may itself contain `/`). Throws
    /// `consent: ...` on a missing or empty half.
    #[napi(factory)]
    pub fn parse(s: String) -> Result<Self> {
        Ok(Self {
            inner: parse_cap(&s)?,
        })
    }

    /// The provider node qualifier (canonical spelling).
    #[napi(getter)]
    pub fn provider(&self) -> String {
        self.inner.provider.clone()
    }

    /// The capability / tool name.
    #[napi(getter)]
    pub fn capability(&self) -> String {
        self.inner.capability.clone()
    }

    /// The `provider/capability` display / wire form.
    #[napi]
    pub fn display(&self) -> String {
        self.inner.display()
    }

    #[napi(js_name = "toString")]
    pub fn to_js_string(&self) -> String {
        self.inner.display()
    }
}

/// Does a wire-declared credential status require local consent before the
/// capability may be invoked? Implements the core trust boundary: a wire
/// `"none"` is NOT trusted (it gates like `"unknown"`), so even `"none"`
/// (and any unrecognised value) returns `true` — a discovered capability
/// can only ever over-gate, never bypass consent.
#[napi]
pub fn credential_requires_consent(status: String) -> bool {
    CredentialStatus::from_wire(&status).requires_consent()
}

/// The consumer-side consent gate: a config allowlist plus a set of pinned
/// capabilities, deciding per capability + wire credential status. The
/// decision logic is the SDK's — this class only carries state.
#[napi]
pub struct ConsentPolicy {
    // The core policy takes `&mut self` for `allow`/`pin`/`unpin`, but the
    // NAPI surface exposes `&self` methods, so serialize through a mutex —
    // matching the `redis_dedup` binding's shape (no contention in the
    // common single-threaded-JS case).
    inner: Mutex<CoreConsentPolicy>,
}

#[napi]
impl ConsentPolicy {
    /// An empty policy: with no entries, EVERY discovered capability
    /// requires approval (a wire credential status — including `"none"` —
    /// is never trusted).
    #[napi(constructor)]
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(CoreConsentPolicy::new()),
        }
    }

    /// Allowlist a capability (operator config) — a standing pre-approval.
    #[napi]
    pub fn allow(&self, cap_id: String) -> Result<()> {
        self.inner.lock().allow(parse_cap(&cap_id)?);
        Ok(())
    }

    /// Record an approved pin.
    #[napi]
    pub fn pin(&self, cap_id: String) -> Result<()> {
        self.inner.lock().pin(parse_cap(&cap_id)?);
        Ok(())
    }

    /// Remove a pin.
    #[napi]
    pub fn unpin(&self, cap_id: String) -> Result<()> {
        self.inner.lock().unpin(&parse_cap(&cap_id)?);
        Ok(())
    }

    /// Is the capability pinned?
    #[napi]
    pub fn is_pinned(&self, cap_id: String) -> Result<bool> {
        Ok(self.inner.lock().is_pinned(&parse_cap(&cap_id)?))
    }

    /// The pinned capabilities' display ids, sorted.
    #[napi]
    pub fn pinned(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.inner.lock().pinned().map(|id| id.display()).collect();
        ids.sort();
        ids
    }

    /// Decide whether the capability, with the given wire credential
    /// status, may be invoked: `"allowed"` or `"requires_approval"`. The
    /// decision is the SDK enum's stable string form — never re-derive it.
    #[napi]
    pub fn decide(&self, cap_id: String, credential_status: String) -> Result<String> {
        let id = parse_cap(&cap_id)?;
        let requires = self
            .inner
            .lock()
            .decide(&id, &credential_status)
            .requires_approval();
        Ok(if requires {
            "requires_approval".to_string()
        } else {
            "allowed".to_string()
        })
    }

    /// Convenience: does invoking the capability require approval the
    /// operator has not granted?
    #[napi]
    pub fn requires_approval(&self, cap_id: String, credential_status: String) -> Result<bool> {
        let id = parse_cap(&cap_id)?;
        Ok(self.inner.lock().requires_approval(&id, &credential_status))
    }
}

impl Default for ConsentPolicy {
    fn default() -> Self {
        Self::new()
    }
}

/// The persistent, machine-shared pin store — a *path-scoped handle*, with
/// Promise-returning methods.
///
/// Reads load a fresh snapshot of the file; every mutation runs a full
/// load→apply→save transaction under the SDK's cross-process advisory
/// lock on napi's tokio runtime, so a concurrent `net mcp pin` CLI
/// invocation, a running `net mcp serve` shim, or another handle can never
/// be clobbered by a stale snapshot.
///
/// `request` is the model-callable verb (only ever writes a *pending*
/// record); `approve` / `reject` are operator verbs — keep them out of any
/// model loop.
#[napi]
pub struct PinStore {
    path: std::path::PathBuf,
}

#[napi]
impl PinStore {
    /// A handle on the pin store file at `path`. The file need not exist
    /// yet — a missing store reads as empty and is created on the first
    /// mutation.
    #[napi(constructor)]
    pub fn new(path: String) -> Self {
        Self {
            path: std::path::PathBuf::from(path),
        }
    }

    /// The store's file path.
    #[napi(getter)]
    pub fn path(&self) -> String {
        self.path.display().to_string()
    }

    /// Record a pin **request** (the model-callable verb). Writes a
    /// `"pending"` record if none exists; an existing record — pending or
    /// approved — is left untouched (a request never upgrades a pin).
    /// Resolves to the resulting state.
    #[napi]
    pub async fn request(&self, cap_id: String) -> Result<String> {
        let id = parse_cap(&cap_id)?;
        let path = self.path.clone();
        CorePinStore::mutate(path, move |s| pin_state_str(s.request(&id)).to_string())
            .await
            .map_err(pins_err)
    }

    /// **Approve** a pin (operator verb; creates the record if absent).
    /// Resolves to whether this changed the stored state.
    #[napi]
    pub async fn approve(&self, cap_id: String) -> Result<bool> {
        let id = parse_cap(&cap_id)?;
        let path = self.path.clone();
        CorePinStore::mutate(path, move |s| s.approve(&id))
            .await
            .map_err(pins_err)
    }

    /// **Reject / remove** a pin entirely (operator verb). Resolves to
    /// whether a record was removed.
    #[napi]
    pub async fn reject(&self, cap_id: String) -> Result<bool> {
        let id = parse_cap(&cap_id)?;
        let path = self.path.clone();
        CorePinStore::mutate(path, move |s| s.remove(&id))
            .await
            .map_err(pins_err)
    }

    /// Is the capability approved (fresh snapshot)?
    #[napi]
    pub async fn is_approved(&self, cap_id: String) -> Result<bool> {
        let id = parse_cap(&cap_id)?;
        let path = self.path.clone();
        let store = CorePinStore::load(path).await.map_err(pins_err)?;
        Ok(store.is_approved(&id))
    }

    /// The capability's state — `"pending"`, `"approved"`, or `null`.
    #[napi]
    pub async fn state(&self, cap_id: String) -> Result<Option<String>> {
        let id = parse_cap(&cap_id)?;
        let path = self.path.clone();
        let store = CorePinStore::load(path).await.map_err(pins_err)?;
        Ok(store.state(&id).map(|s| pin_state_str(s).to_string()))
    }

    /// Every approved capability's display id, sorted.
    #[napi]
    pub async fn approved(&self) -> Result<Vec<String>> {
        let path = self.path.clone();
        let store = CorePinStore::load(path).await.map_err(pins_err)?;
        let mut ids: Vec<String> = store.approved().iter().map(|id| id.display()).collect();
        ids.sort();
        Ok(ids)
    }

    /// Every pending capability's display id, sorted.
    #[napi]
    pub async fn pending(&self) -> Result<Vec<String>> {
        let path = self.path.clone();
        let store = CorePinStore::load(path).await.map_err(pins_err)?;
        let mut ids: Vec<String> = store.pending().iter().map(|id| id.display()).collect();
        ids.sort();
        Ok(ids)
    }

    /// All records as `{ capId, state }` objects, sorted by capId.
    #[napi]
    pub async fn list(&self) -> Result<Vec<PinRecordJs>> {
        let path = self.path.clone();
        let store = CorePinStore::load(path).await.map_err(pins_err)?;
        let mut rows: Vec<PinRecordJs> = store
            .list()
            .into_iter()
            .map(|(id, state)| PinRecordJs {
                cap_id: id.display(),
                state: pin_state_str(state).to_string(),
            })
            .collect();
        rows.sort_by(|a, b| a.cap_id.cmp(&b.cap_id));
        Ok(rows)
    }
}
