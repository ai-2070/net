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
//! Tokens cross the NAPI boundary as opaque `Buffer`s (the 169-byte
//! serialized `PermissionToken`). The TS SDK wraps them in a `Token`
//! class that parses fields client-side — NAPI exposes one
//! [`parse_token`] helper to keep the wire format in a single place.

use napi::bindgen_prelude::*;
use napi_derive::napi;
use std::sync::Arc;

use net::adapter::net::identity::{
    EntityId, EntityKeypair, IdentityState, IdentityStateError, PermissionToken, TokenCache,
    TokenError, TokenScope,
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
        TokenError::Revoked => "revoked",
        TokenError::InvalidFormat => "invalid_format",
        TokenError::ReadOnly => "read_only",
        TokenError::ZeroTtl => "zero_ttl",
        TokenError::TtlTooLong => "ttl_too_long",
    }
}

fn map_token_err(e: TokenError) -> Error {
    token_err(token_error_kind(&e))
}

/// Issuer-state failures get their own stable `kind` discriminators
/// under the `identity:` prefix, so a caller can tell "you rotated
/// backwards" from "that file is not identity state" without parsing
/// prose. `Display` carries the numbers.
fn map_state_err(e: IdentityStateError) -> Error {
    let kind = match e {
        IdentityStateError::InvalidLength { .. } => "invalid_state_length",
        IdentityStateError::UnsupportedVersion { .. } => "unsupported_state_version",
        IdentityStateError::GenerationWentBackwards { .. } => "generation_went_backwards",
        IdentityStateError::GenerationExhausted => "generation_exhausted",
    };
    identity_err(format!("{kind}: {e}"))
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
            // WILDCARD authorizes the token's actions on *every*
            // channel, regardless of its `channel_hash`. It was absent
            // here, so a wildcard grant could not be issued from this
            // binding at all, and a Rust-issued one crossing the wire
            // had the bit dropped on parse — misrepresenting the
            // credential's authority to the very caller deciding
            // whether to trust it.
            "wildcard" => TokenScope::WILDCARD,
            other => {
                return Err(identity_err(format!(
                    "unknown scope {:?}; expected publish | subscribe | admin |                      delegate | wildcard",
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
    // Rendered last so the common scopes keep their existing order in
    // fixtures; the set is what matters, not the sequence.
    if scope.contains(TokenScope::WILDCARD) {
        out.push("wildcard".into());
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
    /// This issuer's credential epoch, stamped onto every token
    /// `issueToken` mints. See `atGeneration` for the rotation rules;
    /// the encoding is core's, shared with every other binding.
    generation: u32,
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
    /// 169-byte serialized token as a Buffer; hand it to the
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
        let token = PermissionToken::try_issue_with_generation(
            &self.keypair,
            self.generation,
            subject_id,
            scope_bits,
            channel_hash,
            u64::from(ttl_seconds),
            delegation_depth,
        )
        .map_err(map_token_err)?;
        Ok(Buffer::from(token.to_bytes()))
    }

    /// This issuer's current credential epoch.
    ///
    /// Every token `issueToken` mints carries it, and a verifier
    /// rejects that token once its revocation floor for this entity
    /// exceeds it.
    #[napi(getter)]
    pub fn issuer_generation(&self) -> u32 {
        self.generation
    }

    /// The same key at a later generation.
    ///
    /// Returns a **new** `Identity`; this one is unchanged. Rotation is
    /// therefore explicit at the call site rather than something that
    /// happens to a caller mid-issuance.
    ///
    /// `next === issuerGeneration` is accepted and idempotent at every
    /// generation including `2^32 - 1`, so re-applying a persisted
    /// generation on restart is never an error. Going backwards throws.
    ///
    /// There is no generation above `2^32 - 1` to name, so an issuer
    /// there can re-apply but not advance; past that, rotate the key.
    ///
    /// ## Rotation order
    ///
    /// 1. build the generation-N identity here;
    /// 2. persist `toStateBytes()` atomically and durably;
    /// 3. distribute verifier floor N;
    /// 4. start issuing from the returned identity.
    ///
    /// Never publish floor N before step 2 lands. A crash in between
    /// leaves an issuer that has announced a floor it has no durable
    /// state to satisfy — it can mint nothing a verifier accepts, and
    /// only a key rotation gets it back.
    #[napi]
    pub fn at_generation(&self, next: u32) -> Result<Identity> {
        let generation =
            IdentityState::check_rotation(self.generation, next).map_err(map_state_err)?;
        Ok(Self {
            keypair: self.keypair.clone(),
            cache: self.cache.clone(),
            generation,
        })
    }

    /// Serialize the full issuer state — version, seed, generation.
    ///
    /// **Secret material**, exactly like `toBytes`: these bytes contain
    /// the ed25519 signing seed. Encrypt at rest and write atomically;
    /// a torn write here is an issuer that cannot come back.
    #[napi]
    pub fn to_state_bytes(&self) -> Buffer {
        Buffer::from(
            IdentityState {
                seed: *self.keypair.secret_bytes(),
                generation: self.generation,
            }
            .to_bytes()
            .to_vec(),
        )
    }

    /// Restore an issuer — key *and* generation — from
    /// `toStateBytes()`.
    ///
    /// This is the restart path for anything that rotates. `fromBytes`
    /// / `fromSeed` restore the key only and come back at generation
    /// zero, which for a rotated issuer means below its own published
    /// floor.
    #[napi(factory)]
    pub fn from_state_bytes(bytes: Buffer) -> Result<Self> {
        let state = IdentityState::from_bytes(bytes.as_ref()).map_err(map_state_err)?;
        Ok(Self::wrap_at(
            EntityKeypair::from_bytes(state.seed),
            state.generation,
        ))
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
        Self::wrap_at(kp, 0)
    }

    /// Key-only construction has no epoch to restore, so `wrap` starts
    /// at zero. `fromStateBytes` is the path that carries one.
    fn wrap_at(kp: EntityKeypair, generation: u32) -> Self {
        Self {
            keypair: Arc::new(kp),
            cache: Arc::new(TokenCache::new()),
            generation,
        }
    }

    /// Build a matching SDK-level `Identity` by cloning out the
    /// seed and re-constructing. Used by sibling NAPI modules
    /// (`compute`'s `DaemonRuntime::spawn`, the `delegation` /
    /// `enrollment` surfaces) that feed the identity into the SDK.
    ///
    /// The token cache does NOT carry over — the SDK creates a
    /// fresh `TokenCache` inside its own `Identity`. For
    /// `DaemonRuntime` use this is fine; daemons don't consult
    /// the cache at spawn time.
    #[cfg(any(feature = "compute", feature = "delegation"))]
    pub(crate) fn to_sdk_identity(&self) -> net_sdk::Identity {
        let id = net_sdk::Identity::from_seed(*self.keypair.secret_bytes());
        // Carry the epoch across too. `from_seed` restores the key at
        // generation zero, and handing the SDK a zero-generation copy
        // of a rotated issuer would have it mint below its own floor.
        // `at_generation` only fails going backwards or at the
        // ceiling; from zero, neither applies.
        id.at_generation(self.generation).unwrap_or(id)
    }

    /// Wrap a shared `EntityKeypair` Arc in a fresh `Identity` handle
    /// (fresh token cache). Used by the delegation / enrollment modules
    /// to hand back opaque child / device identities — the private seed
    /// stays inside Rust (bridge doctrine H8).
    #[cfg(feature = "delegation")]
    pub(crate) fn from_keypair_arc(keypair: Arc<EntityKeypair>) -> Self {
        Self {
            keypair,
            cache: Arc::new(TokenCache::new()),
            // A freshly-derived child or device identity has no
            // rotation history of its own.
            generation: 0,
        }
    }

    /// The raw `EntityId` (the 32-byte ed25519 public key), for sibling
    /// NAPI modules that need the typed id rather than the `Buffer`
    /// projection of [`Self::entity_id`].
    #[cfg(feature = "delegation")]
    pub(crate) fn entity_id_ref(&self) -> &EntityId {
        self.keypair.entity_id()
    }

    /// The private 32-byte seed, for the delegation module's
    /// child-identity KDF. `pub(crate)` and never surfaced to JS —
    /// the derivation happens inside Rust (H8).
    #[cfg(feature = "delegation")]
    pub(crate) fn secret_seed(&self) -> &[u8; 32] {
        self.keypair.secret_bytes()
    }

    /// Clone out the inner `EntityKeypair`. Used by the MeshOS
    /// binding's `register_daemon` which takes an owned keypair
    /// (the supervisor reads the `origin_hash` as the daemon's
    /// substrate id). `EntityKeypair: Clone` so this is cheap;
    /// the secret bytes never leave the binding.
    #[cfg(feature = "meshos")]
    pub(crate) fn keypair_clone(&self) -> EntityKeypair {
        (*self.keypair).clone()
    }
}

// =========================================================================
// TokenInfo POJO + free functions — wire-format helpers
// =========================================================================

/// Parsed token view. All byte fields are 32 bytes except `signature`
/// (64 bytes). `not_before` / `not_after` / `channel_hash` are
/// `BigInt` to avoid JS number-precision loss (canonical
/// `channel_hash` is 64-bit; the wire `NetHeader` fast-path hint
/// is the low 16 bits of this value). `scope` is the decoded
/// string array.
#[napi(object)]
pub struct TokenInfo {
    pub issuer: Buffer,
    pub subject: Buffer,
    pub scope: Vec<String>,
    pub channel_hash: BigInt,
    pub not_before: BigInt,
    pub not_after: BigInt,
    pub delegation_depth: u8,
    /// Issuer generation this token was minted under.
    ///
    /// `RevocationRegistry` rejects tokens below the issuer's
    /// monotonic floor. Without this field an operator could see that
    /// a credential was refused but not that its generation was the
    /// reason, which is the one thing that explains the refusal.
    pub issuer_generation: u32,
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
        channel_hash: BigInt::from(token.channel_hash),
        not_before: BigInt::from(token.not_before),
        not_after: BigInt::from(token.not_after),
        delegation_depth: token.delegation_depth,
        issuer_generation: token.issuer_generation,
        nonce: BigInt::from(token.nonce),
        signature: Buffer::from(token.signature.to_vec()),
    })
}

/// Verify a detached ed25519 signature against a 32-byte entity id.
///
/// The verifying half of [`Self::sign`]. Every binding exposed signing
/// and none exposed verification for an arbitrary message, so a
/// signature produced here could only be checked from Rust — and the
/// binding tests asserted the signature's *length* rather than a
/// round trip, which passes for any 64 bytes.
///
/// Strict verification (`verify_strict`): the malleable `(R, S + L)`
/// variant is rejected, so one logical message cannot appear under two
/// byte encodings.
///
/// Returns `true` when the signature is valid for this exact
/// `(entityId, message)` pair, `false` when it is not. Throws only on
/// a malformed argument — a wrong-length entity id or signature — so
/// a `false` means "this did not verify", never "you called it wrong".
#[napi]
pub fn verify_signature(entity_id: Buffer, message: Buffer, signature: Buffer) -> Result<bool> {
    let id = buffer_to_entity_id(&entity_id)?;
    let sig: [u8; 64] = signature.as_ref().try_into().map_err(|_| {
        identity_err(format!(
            "signature must be exactly 64 bytes, got {}",
            signature.len()
        ))
    })?;
    Ok(id.verify_bytes(message.as_ref(), &sig).is_ok())
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
    // The child is issued by `signer`, so it carries the signer's
    // generation — not the parent's. The floor it is checked against
    // is the signer's.
    let child = parent
        .delegate_with_generation(
            &signer.keypair,
            signer.issuer_generation(),
            subject_id,
            restricted,
        )
        .map_err(map_token_err)?;
    Ok(Buffer::from(child.to_bytes()))
}

/// Hash a channel name to its canonical 64-bit substrate identifier
/// (matches `PermissionToken::channel_hash`). The wire `NetHeader`
/// fast-path hint is the low 16 bits of this value. Returned as
/// `BigInt` because the canonical value is a full xxh3_64 (a
/// 53-bit-truncating cast through a JS `number` is unsafe). Exposed
/// so TS callers can compare their channel-name against a parsed
/// token's `channel_hash` without reaching for a library.
#[napi]
pub fn channel_hash(channel: String) -> Result<BigInt> {
    channel_to_hash(&channel).map(BigInt::from)
}
