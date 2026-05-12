// `#[napi]` exports functions to JS but leaves them "unused" from
// Rust's POV, so clippy's dead-code analysis doesn't apply to this
// module. Suppress at file scope.
#![allow(dead_code)]

//! NAPI surface for the `Identity` handle — ed25519 keypair + token
//! cache.
//!
//! Pure-compute: no network state. Exposing this before the mesh
//! integration (Stage C) unblocks callers who want to issue / verify
//! tokens in ahead-of-time flows (e.g., minting tokens at a central
//! issuer and distributing them out of band).
//!
//! Tokens cross the NAPI boundary as opaque `Buffer`s (the 159-byte
//! serialized `PermissionToken`). The TS SDK wraps them in a `Token`
//! class that parses fields client-side — NAPI exposes one
//! [`parse_token`] helper to keep the wire format in a single place.

use napi::bindgen_prelude::*;
use napi_derive::napi;
use std::sync::Arc;

use net::adapter::net::identity::{
    EntityId, EntityKeypair, PermissionToken, TokenCache, TokenError, TokenScope,
};

// =========================================================================
// Error prefixes — stable strings that the TS layer dispatches on
// =========================================================================

const ERR_IDENTITY_PREFIX: &str = "identity:";
const ERR_TOKEN_PREFIX: &str = "token:";

fn identity_err(msg: impl Into<String>) -> Error {
    Error::from_reason(format!("{} {}", ERR_IDENTITY_PREFIX, msg.into()))
}

fn token_err(kind: &str) -> Error {
    // `kind` is one of: invalid_signature | not_yet_valid | expired |
    // delegation_exhausted | delegation_not_allowed | not_authorized |
    // invalid_format | read_only. Kept as a stable discriminator so
    // the TS layer can build a `.kind`-tagged exception without
    // parsing prose.
    Error::from_reason(format!("{} {}", ERR_TOKEN_PREFIX, kind))
}

fn token_error_kind(e: &TokenError) -> &'static str {
    match e {
        TokenError::InvalidSignature => "invalid_signature",
        TokenError::NotYetValid => "not_yet_valid",
        TokenError::Expired => "expired",
        TokenError::DelegationExhausted => "delegation_exhausted",
        TokenError::DelegationNotAllowed => "delegation_not_allowed",
        TokenError::NotAuthorized => "not_authorized",
        TokenError::InvalidFormat => "invalid_format",
        TokenError::ReadOnly => "read_only",
        TokenError::ZeroTtl => "zero_ttl",
    }
}

fn map_token_err(e: TokenError) -> Error {
    token_err(token_error_kind(&e))
}

/// Public helper for crate-internal callers (mesh subscribe path)
/// that need to classify a `TokenError` with the same `token: <kind>`
/// prefix the rest of this module uses. Keeps the `kind` strings
/// single-sourced.
pub(crate) fn token_err_for(e: TokenError) -> Error {
    map_token_err(e)
}

// =========================================================================
// Scope parsing — string array ↔ TokenScope bitfield
// =========================================================================

fn parse_scope(scopes: Vec<String>) -> Result<TokenScope> {
    let mut acc = TokenScope::NONE;
    for s in &scopes {
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

// =========================================================================
// Channel-name hashing — keep the hash function in one place
// =========================================================================

fn channel_to_hash(channel: &str) -> Result<net::adapter::net::ChannelHash> {
    let name = net::adapter::net::ChannelName::new(channel)
        .map_err(|e| identity_err(format!("invalid channel name: {}", e)))?;
    Ok(name.hash())
}

fn buffer_to_entity_id(buf: &Buffer) -> Result<EntityId> {
    let bytes: &[u8] = buf.as_ref();
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

// =========================================================================
// Identity NAPI class
// =========================================================================

/// ed25519 keypair + local token cache. See the module docs for the
/// persistence model (seed out via `toBytes`, back in via
/// `fromBytes` / `fromSeed`).
#[napi]
pub struct Identity {
    keypair: Arc<EntityKeypair>,
    cache: Arc<TokenCache>,
}

#[napi]
impl Identity {
    /// Generate a fresh ed25519 identity. Treat every call as creating
    /// a new entity; persist via [`Self::to_bytes`] if you want
    /// stable ids across restarts.
    #[napi(factory)]
    pub fn generate() -> Self {
        Self::wrap(EntityKeypair::generate())
    }

    /// Load from a caller-owned 32-byte ed25519 seed.
    #[napi(factory)]
    pub fn from_seed(seed: Buffer) -> Result<Self> {
        let bytes: &[u8] = seed.as_ref();
        if bytes.len() != 32 {
            return Err(identity_err(format!(
                "seed must be 32 bytes, got {}",
                bytes.len()
            )));
        }
        let mut arr = [0u8; 32];
        arr.copy_from_slice(bytes);
        Ok(Self::wrap(EntityKeypair::from_bytes(arr)))
    }

    /// Alias for [`Self::from_seed`] — provided for API parity with
    /// the Rust SDK where `Identity::to_bytes` / `from_bytes` round-
    /// trip. Today the persisted form IS the 32-byte seed.
    #[napi(factory)]
    pub fn from_bytes(bytes: Buffer) -> Result<Self> {
        Self::from_seed(bytes)
    }

    /// Serialize the identity as its 32-byte seed. Token cache entries
    /// are runtime-only; re-install long-lived grants via
    /// [`Self::install_token`] after reload.
    #[napi]
    pub fn to_bytes(&self) -> Buffer {
        Buffer::from(self.keypair.secret_bytes().to_vec())
    }

    /// Ed25519 public key. 32 bytes.
    #[napi(getter)]
    pub fn entity_id(&self) -> Buffer {
        Buffer::from(self.keypair.entity_id().as_bytes().to_vec())
    }

    /// Derived 64-bit origin hash used in packet headers.
    #[napi(getter)]
    pub fn origin_hash(&self) -> BigInt {
        BigInt::from(self.keypair.origin_hash())
    }

    /// Derived 64-bit node id used for routing / addressing.
    #[napi(getter)]
    pub fn node_id(&self) -> BigInt {
        BigInt::from(self.keypair.node_id())
    }

    /// Sign arbitrary bytes. Returns 64 bytes (ed25519 signature).
    #[napi]
    pub fn sign(&self, message: Buffer) -> Buffer {
        let sig = self.keypair.sign(message.as_ref());
        Buffer::from(sig.to_bytes().to_vec())
    }

    /// Issue a scoped permission token to `subject`. Returns the
    /// 159-byte serialized token as a Buffer; hand it to the
    /// subscriber who will then call `installToken(bytes)`.
    ///
    /// `scope` is a subset of `["publish", "subscribe", "admin",
    /// "delegate"]`. `delegation_depth = 0` disallows further
    /// re-delegation.
    #[napi]
    pub fn issue_token(
        &self,
        subject: Buffer,
        scope: Vec<String>,
        channel: String,
        ttl_seconds: u32,
        delegation_depth: u8,
    ) -> Result<Buffer> {
        let subject_id = buffer_to_entity_id(&subject)?;
        let scope_bits = parse_scope(scope)?;
        let channel_hash = channel_to_hash(&channel)?;
        // Route through `try_issue` so a `ttl_seconds=0`
        // surfaces as `TokenError::ZeroTtl` (mapped to NAPI
        // Error here) rather than minting a born-expired token
        // that every receiver rejects with no diagnostic to the
        // issuer.
        let token = PermissionToken::try_issue(
            &self.keypair,
            subject_id,
            scope_bits,
            channel_hash,
            u64::from(ttl_seconds),
            delegation_depth,
        )
        .map_err(map_token_err)?;
        Ok(Buffer::from(token.to_bytes()))
    }

    /// Install a token this node received from another issuer. The
    /// signature is verified before insert; a tampered or
    /// truncated token throws `token: invalid_signature` /
    /// `token: invalid_format`.
    #[napi]
    pub fn install_token(&self, token: Buffer) -> Result<()> {
        let parsed = PermissionToken::from_bytes(token.as_ref()).map_err(map_token_err)?;
        self.cache.insert(parsed).map_err(map_token_err)
    }

    /// Look up a cached token by `(subject, channel)`. Returns
    /// `undefined` if no exact-channel token is cached.
    #[napi]
    pub fn lookup_token(&self, subject: Buffer, channel: String) -> Result<Option<Buffer>> {
        let subject_id = buffer_to_entity_id(&subject)?;
        let channel_hash = channel_to_hash(&channel)?;
        Ok(self
            .cache
            .get(&subject_id, channel_hash)
            .map(|t| Buffer::from(t.to_bytes())))
    }

    /// Number of cached tokens. Testing aid.
    #[napi(getter)]
    pub fn token_cache_len(&self) -> u32 {
        self.cache.len() as u32
    }

    fn wrap(kp: EntityKeypair) -> Self {
        Self {
            keypair: Arc::new(kp),
            cache: Arc::new(TokenCache::new()),
        }
    }

    /// Build a matching SDK-level `Identity` by cloning out the
    /// seed and re-constructing. Used by sibling NAPI modules
    /// (currently: `compute`'s `DaemonRuntime::spawn`) that feed
    /// the identity into the SDK's compute surface.
    ///
    /// The token cache does NOT carry over — the SDK creates a
    /// fresh `TokenCache` inside its own `Identity`. For
    /// `DaemonRuntime` use this is fine; daemons don't consult
    /// the cache at spawn time.
    #[cfg(feature = "compute")]
    pub(crate) fn to_sdk_identity(&self) -> net_sdk::Identity {
        net_sdk::Identity::from_seed(*self.keypair.secret_bytes())
    }
}

// =========================================================================
// TokenInfo POJO + free functions — wire-format helpers
// =========================================================================

/// Parsed token view. All byte fields are 32 bytes except `signature`
/// (64 bytes). `not_before` / `not_after` are unix seconds as
/// `BigInt` to avoid JS number-precision loss. `scope` is the decoded
/// string array; `channel_hash` is the canonical 32-bit substrate
/// identifier (used for ACL/storage/config keys; the wire
/// `NetHeader` fast-path hint is the low 16 bits of this value).
#[napi(object)]
pub struct TokenInfo {
    pub issuer: Buffer,
    pub subject: Buffer,
    pub scope: Vec<String>,
    pub channel_hash: u32,
    pub not_before: BigInt,
    pub not_after: BigInt,
    pub delegation_depth: u8,
    pub nonce: BigInt,
    pub signature: Buffer,
}

/// Parse a serialized `PermissionToken`. Throws `token:
/// invalid_format` on bad length / structure; the signature is NOT
/// verified here (use [`verify_token`] or `installToken` for that).
#[napi]
pub fn parse_token(bytes: Buffer) -> Result<TokenInfo> {
    let token = PermissionToken::from_bytes(bytes.as_ref()).map_err(map_token_err)?;
    Ok(TokenInfo {
        issuer: Buffer::from(token.issuer.as_bytes().to_vec()),
        subject: Buffer::from(token.subject.as_bytes().to_vec()),
        scope: scope_to_strings(token.scope),
        channel_hash: token.channel_hash,
        not_before: BigInt::from(token.not_before),
        not_after: BigInt::from(token.not_after),
        delegation_depth: token.delegation_depth,
        nonce: BigInt::from(token.nonce),
        signature: Buffer::from(token.signature.to_vec()),
    })
}

/// Verify a serialized token's signature. Returns `true` on valid.
/// Time-bound validity is a separate check — use [`token_is_expired`]
/// for that.
#[napi]
pub fn verify_token(bytes: Buffer) -> Result<bool> {
    let token = PermissionToken::from_bytes(bytes.as_ref()).map_err(map_token_err)?;
    Ok(token.verify().is_ok())
}

/// `true` if the token's `not_after` has passed. Uses the host
/// wall-clock; cross-check against trusted time if that matters.
/// Pure time check — a tampered-but-expired token still reports
/// true. Use `verifyToken` for signature integrity.
#[napi]
pub fn token_is_expired(bytes: Buffer) -> Result<bool> {
    let token = PermissionToken::from_bytes(bytes.as_ref()).map_err(map_token_err)?;
    Ok(token.is_expired())
}

/// Delegate a token to a new subject. The `parent_bytes` token must
/// have `delegation_depth > 0` and include the `delegate` scope; the
/// `signer` identity must be the subject of the parent token.
#[napi]
pub fn delegate_token(
    signer: &Identity,
    parent_bytes: Buffer,
    new_subject: Buffer,
    restricted_scope: Vec<String>,
) -> Result<Buffer> {
    let parent = PermissionToken::from_bytes(parent_bytes.as_ref()).map_err(map_token_err)?;
    let subject_id = buffer_to_entity_id(&new_subject)?;
    let restricted = parse_scope(restricted_scope)?;
    let child = parent
        .delegate(&signer.keypair, subject_id, restricted)
        .map_err(map_token_err)?;
    Ok(Buffer::from(child.to_bytes()))
}

/// Hash a channel name to its canonical 32-bit substrate identifier
/// (matches `PermissionToken::channel_hash`). The wire `NetHeader`
/// fast-path hint is the low 16 bits of this value. Exposed so TS
/// callers can compare their channel-name against a parsed token's
/// `channel_hash` without reaching for a library.
#[napi]
pub fn channel_hash(channel: String) -> Result<u32> {
    channel_to_hash(&channel)
}
