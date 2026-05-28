//! PyO3 surface for the `Identity` handle — ed25519 keypair +
//! token cache.
//!
//! Mirrors `bindings/node/src/identity.rs` one-for-one on the
//! error-prefix convention (`"identity: ..."` /
//! `"token: <kind>"`) and the opaque-bytes token boundary.
//! Tokens cross as Python `bytes`; `CapabilitySet` / `SubnetId` /
//! etc. live in sibling modules.

use std::sync::Arc;

use pyo3::exceptions::PyException;
use pyo3::prelude::*;
use pyo3::types::PyDict;

use net::adapter::net::identity::{
    EntityId, EntityKeypair, PermissionToken, TokenCache, TokenError as CoreTokenError, TokenScope,
};

// =========================================================================
// Exceptions — matches TS `IdentityError` / `TokenError(kind)`
// =========================================================================

pyo3::create_exception!(
    _net,
    IdentityError,
    PyException,
    "Raised for malformed inputs at the identity layer (wrong seed \
     length, invalid entity id, unknown scope, etc.). Token-validity \
     failures raise `TokenError` instead."
);

pyo3::create_exception!(
    _net,
    TokenError,
    PyException,
    "Raised when a token fails validation. The message has the form \
     `token: <kind>` where `<kind>` is one of `invalid_signature` | \
     `not_yet_valid` | `expired` | `delegation_exhausted` | \
     `delegation_not_allowed` | `not_authorized` | `invalid_format` | \
     `read_only`."
);

pub(crate) fn token_error_kind(e: &CoreTokenError) -> &'static str {
    match e {
        CoreTokenError::InvalidSignature => "invalid_signature",
        CoreTokenError::NotYetValid => "not_yet_valid",
        CoreTokenError::Expired => "expired",
        CoreTokenError::DelegationExhausted => "delegation_exhausted",
        CoreTokenError::DelegationNotAllowed => "delegation_not_allowed",
        CoreTokenError::NotAuthorized => "not_authorized",
        CoreTokenError::InvalidFormat => "invalid_format",
        CoreTokenError::ReadOnly => "read_only",
        CoreTokenError::ZeroTtl => "zero_ttl",
        CoreTokenError::TtlTooLong => "ttl_too_long",
    }
}

/// Encode a `CoreTokenError` as a Python-level `TokenError`. The
/// `kind` discriminator is the message suffix after `"token: "`;
/// programmatic callers parse it via
/// `str(e).removeprefix("token: ")`.
pub(crate) fn token_err(e: CoreTokenError) -> PyErr {
    PyErr::new::<TokenError, _>(format!("token: {}", token_error_kind(&e)))
}

pub(crate) fn identity_err(msg: impl Into<String>) -> PyErr {
    PyErr::new::<IdentityError, _>(format!("identity: {}", msg.into()))
}

// =========================================================================
// Scope parsing
// =========================================================================

fn parse_scope(scopes: &[String]) -> PyResult<TokenScope> {
    let mut acc = TokenScope::NONE;
    for s in scopes {
        acc = acc.union(match s.as_str() {
            "publish" => TokenScope::PUBLISH,
            "subscribe" => TokenScope::SUBSCRIBE,
            "admin" => TokenScope::ADMIN,
            "delegate" => TokenScope::DELEGATE,
            other => {
                return Err(identity_err(format!(
                    "unknown scope {:?}; expected publish | subscribe | admin | delegate",
                    other
                )));
            }
        });
    }
    Ok(acc)
}

fn scope_to_strings(scope: TokenScope) -> Vec<String> {
    let mut out = Vec::new();
    if scope.contains(TokenScope::PUBLISH) {
        out.push("publish".into());
    }
    if scope.contains(TokenScope::SUBSCRIBE) {
        out.push("subscribe".into());
    }
    if scope.contains(TokenScope::ADMIN) {
        out.push("admin".into());
    }
    if scope.contains(TokenScope::DELEGATE) {
        out.push("delegate".into());
    }
    out
}

fn bytes_to_entity_id(bytes: &[u8]) -> PyResult<EntityId> {
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

fn channel_to_hash(channel: &str) -> PyResult<net::adapter::net::ChannelHash> {
    let name = net::adapter::net::ChannelName::new(channel)
        .map_err(|e| identity_err(format!("invalid channel name: {}", e)))?;
    Ok(name.hash())
}

// =========================================================================
// Identity pyclass
// =========================================================================

/// ed25519 keypair + local token cache. Cheap to clone (both
/// inner members are `Arc`).
#[pyclass(from_py_object)]
#[derive(Clone)]
pub struct Identity {
    pub(crate) keypair: Arc<EntityKeypair>,
    pub(crate) cache: Arc<TokenCache>,
}

impl Identity {
    fn wrap(kp: EntityKeypair) -> Self {
        Self {
            keypair: Arc::new(kp),
            cache: Arc::new(TokenCache::new()),
        }
    }

    /// Convert this Python `Identity` into the SDK `Identity` type
    /// consumed by `net-sdk::compute`. Used by the compute feature's
    /// `DaemonRuntime::spawn` / migration methods — the SDK reads
    /// the 32-byte ed25519 seed through its own handle, so we hand
    /// it a fresh `net_sdk::Identity` built from the seed bytes we
    /// already hold.
    #[cfg(feature = "compute")]
    pub(crate) fn to_sdk_identity(&self) -> net_sdk::Identity {
        net_sdk::Identity::from_seed(*self.keypair.secret_bytes())
    }
}

#[pymethods]
impl Identity {
    /// Generate a fresh ed25519 identity.
    #[staticmethod]
    fn generate() -> Self {
        Self::wrap(EntityKeypair::generate())
    }

    /// Load from a caller-owned 32-byte ed25519 seed.
    #[staticmethod]
    fn from_seed(seed: &[u8]) -> PyResult<Self> {
        if seed.len() != 32 {
            return Err(identity_err(format!(
                "seed must be 32 bytes, got {}",
                seed.len()
            )));
        }
        let mut arr = [0u8; 32];
        arr.copy_from_slice(seed);
        Ok(Self::wrap(EntityKeypair::from_bytes(arr)))
    }

    /// Alias for `from_seed` — the persisted form IS the 32-byte seed.
    #[staticmethod]
    fn from_bytes(bytes: &[u8]) -> PyResult<Self> {
        Self::from_seed(bytes)
    }

    /// Serialize as the 32-byte ed25519 seed. Treat as secret.
    fn to_bytes(&self) -> Vec<u8> {
        self.keypair.secret_bytes().to_vec()
    }

    /// Ed25519 public key. 32 bytes.
    #[getter]
    fn entity_id(&self) -> Vec<u8> {
        self.keypair.entity_id().as_bytes().to_vec()
    }

    /// Derived 64-bit origin hash used in packet headers.
    #[getter]
    fn origin_hash(&self) -> u64 {
        self.keypair.origin_hash()
    }

    /// Derived 64-bit node id used for routing/addressing.
    #[getter]
    fn node_id(&self) -> u64 {
        self.keypair.node_id()
    }

    /// Sign arbitrary bytes. Returns 64 bytes (ed25519 signature).
    fn sign(&self, message: &[u8]) -> Vec<u8> {
        self.keypair.sign(message).to_bytes().to_vec()
    }

    /// Issue a scoped permission token to `subject`.
    ///
    /// `scope` is a list of `'publish' | 'subscribe' | 'admin' |
    /// 'delegate'`. `delegation_depth = 0` forbids further
    /// re-delegation.
    #[pyo3(signature = (subject, scope, channel, ttl_seconds, delegation_depth=0))]
    fn issue_token(
        &self,
        subject: &[u8],
        scope: Vec<String>,
        channel: &str,
        ttl_seconds: u32,
        delegation_depth: u8,
    ) -> PyResult<Vec<u8>> {
        let subject_id = bytes_to_entity_id(subject)?;
        let scope_bits = parse_scope(&scope)?;
        let channel_hash = channel_to_hash(channel)?;
        // Route through `try_issue` so `ttl_seconds=0`
        // surfaces as `TokenError::ZeroTtl` rather than minting a
        // born-expired token.
        let token = PermissionToken::try_issue(
            &self.keypair,
            subject_id,
            scope_bits,
            channel_hash,
            u64::from(ttl_seconds),
            delegation_depth,
        )
        .map_err(token_err)?;
        Ok(token.to_bytes())
    }

    /// Install a token received from another issuer. Signature
    /// verification happens on insert; malformed or tampered
    /// tokens raise `TokenError`.
    fn install_token(&self, token: &[u8]) -> PyResult<()> {
        let parsed = PermissionToken::from_bytes(token).map_err(token_err)?;
        self.cache.insert(parsed).map_err(token_err)
    }

    /// Look up a cached token by `(subject, channel)`. Returns
    /// `None` if no exact-channel token is cached.
    fn lookup_token(&self, subject: &[u8], channel: &str) -> PyResult<Option<Vec<u8>>> {
        let subject_id = bytes_to_entity_id(subject)?;
        let channel_hash = channel_to_hash(channel)?;
        Ok(self
            .cache
            .get(&subject_id, channel_hash)
            .map(|t| t.to_bytes()))
    }

    /// Number of cached tokens (testing aid).
    #[getter]
    fn token_cache_len(&self) -> u32 {
        self.cache.len() as u32
    }

    fn __repr__(&self) -> String {
        format!(
            "Identity(entity_id=0x{}, node_id={:#x})",
            hex::encode(self.keypair.entity_id().as_bytes()),
            self.keypair.node_id()
        )
    }
}

// =========================================================================
// Module-level functions
// =========================================================================

/// Parse a serialized `PermissionToken`. Returns a dict with
/// `issuer`, `subject`, `scope`, `channel_hash`, `not_before`,
/// `not_after`, `delegation_depth`, `nonce`, `signature`.
/// Raises `TokenError(kind="invalid_format")` on bad length / layout.
#[pyfunction]
pub fn parse_token<'py>(py: Python<'py>, token: &[u8]) -> PyResult<Bound<'py, PyDict>> {
    let parsed = PermissionToken::from_bytes(token).map_err(token_err)?;
    let out = PyDict::new(py);
    out.set_item("issuer", parsed.issuer.as_bytes().to_vec())?;
    out.set_item("subject", parsed.subject.as_bytes().to_vec())?;
    out.set_item("scope", scope_to_strings(parsed.scope))?;
    out.set_item("channel_hash", parsed.channel_hash)?;
    out.set_item("not_before", parsed.not_before)?;
    out.set_item("not_after", parsed.not_after)?;
    out.set_item("delegation_depth", parsed.delegation_depth)?;
    out.set_item("nonce", parsed.nonce)?;
    out.set_item("signature", parsed.signature.to_vec())?;
    Ok(out)
}

/// Verify a serialized token's ed25519 signature. `True` on
/// valid; `False` on tampered / wrong-subject. Time-bound validity
/// is a separate check — use `token_is_expired` for that.
#[pyfunction]
pub fn verify_token(token: &[u8]) -> PyResult<bool> {
    let parsed = PermissionToken::from_bytes(token).map_err(token_err)?;
    Ok(parsed.verify().is_ok())
}

/// `True` if the token's `not_after` has passed (host wall-clock).
/// Pure time check — a tampered-but-expired token still reports
/// true. Use :func:`verify_token` for signature integrity.
#[pyfunction]
pub fn token_is_expired(token: &[u8]) -> PyResult<bool> {
    let parsed = PermissionToken::from_bytes(token).map_err(token_err)?;
    Ok(parsed.is_expired())
}

/// Delegate a token to a new subject. The `parent` token must
/// include the `delegate` scope and have `delegation_depth > 0`;
/// the `signer` must be the subject of the parent token.
#[pyfunction]
pub fn delegate_token(
    signer: &Identity,
    parent: &[u8],
    new_subject: &[u8],
    restricted_scope: Vec<String>,
) -> PyResult<Vec<u8>> {
    let parent_tok = PermissionToken::from_bytes(parent).map_err(token_err)?;
    let subject_id = bytes_to_entity_id(new_subject)?;
    let restricted = parse_scope(&restricted_scope)?;
    let child = parent_tok
        .delegate(&signer.keypair, subject_id, restricted)
        .map_err(token_err)?;
    Ok(child.to_bytes())
}

/// Hash a channel name to its canonical 64-bit substrate identifier
/// (used for ACL / config / storage keys; the wire `NetHeader`
/// fast-path hint is the low 16 bits of this value). Python's
/// `int` is arbitrary-precision so the full u64 round-trips
/// without truncation.
#[pyfunction]
pub fn channel_hash(channel: &str) -> PyResult<u64> {
    channel_to_hash(channel)
}
