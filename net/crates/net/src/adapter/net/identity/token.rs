//! Permission tokens for Net authorization.
//!
//! Tokens are ed25519-signed, delegatable, and expirable. They authorize
//! an entity to perform specific actions (publish, subscribe, admin) on
//! specific channels. L2 (Channels & Authorization) enforces these at
//! subscription time, not per-packet.

use dashmap::DashMap;
use ed25519_dalek::Signature;
use std::time::{SystemTime, UNIX_EPOCH};

use super::entity::{EntityId, EntityKeypair};

/// Actions a token can authorize.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TokenScope {
    bits: u32,
}

impl TokenScope {
    /// No permissions.
    pub const NONE: Self = Self { bits: 0 };
    /// Publish events to a channel.
    pub const PUBLISH: Self = Self { bits: 0b0001 };
    /// Subscribe to events from a channel.
    pub const SUBSCRIBE: Self = Self { bits: 0b0010 };
    /// Administrative access (create/delete channels, manage tokens).
    pub const ADMIN: Self = Self { bits: 0b0100 };
    /// Can delegate this token to other entities.
    pub const DELEGATE: Self = Self { bits: 0b1000 };
    /// Wildcard over channels: authorizes the token's actions on *every*
    /// channel, regardless of the token's `channel_hash` field. Must be
    /// set explicitly by the issuer — the previous "`channel_hash == 0`
    /// means wildcard" overload is no longer honored, so a legitimate
    /// channel whose 16-bit xxh3 happens to hash to 0 cannot
    /// accidentally be authorized as a universal grant.
    pub const WILDCARD: Self = Self { bits: 0b1_0000 };
    /// Full access (all actions on a single channel). Does NOT include
    /// [`Self::WILDCARD`] — callers that want cross-channel access
    /// must opt in explicitly.
    pub const ALL: Self = Self { bits: 0b1111 };

    /// Create a scope from raw bits.
    #[inline]
    pub const fn from_bits(bits: u32) -> Self {
        Self { bits }
    }

    /// Get the raw bits.
    #[inline]
    pub const fn bits(self) -> u32 {
        self.bits
    }

    /// Check if this scope includes another.
    ///
    /// A scope never "contains" `NONE`: the bit-mask identity
    /// `(self.bits & 0) == 0` would otherwise return true for every
    /// token, so a caller that builds `action: TokenScope` from
    /// external input — e.g. a wire `u32` masked into a smaller
    /// subset — that happens to mask to `NONE` would receive a
    /// blanket `true` against any token. Short-circuit `NONE` so the
    /// caller's "do they have permission X" question rejects the
    /// no-op action.
    #[inline]
    pub const fn contains(self, other: Self) -> bool {
        if other.bits == 0 {
            return false;
        }
        (self.bits & other.bits) == other.bits
    }

    /// Restrict this scope to only include permissions in `other`.
    #[inline]
    pub const fn intersect(self, other: Self) -> Self {
        Self {
            bits: self.bits & other.bits,
        }
    }

    /// Combine with another scope.
    #[inline]
    pub const fn union(self, other: Self) -> Self {
        Self {
            bits: self.bits | other.bits,
        }
    }

    /// Optional channel hash filter. If set, token only applies to
    /// channels matching this hash.
    pub fn with_channel(self, channel_hash: u16) -> ScopedToken {
        ScopedToken {
            scope: self,
            channel_hash: Some(channel_hash),
        }
    }
}

/// A scope bound to an optional channel.
#[derive(Debug, Clone, Copy)]
pub struct ScopedToken {
    pub scope: TokenScope,
    pub channel_hash: Option<u16>,
}

/// A signed, delegatable permission token.
///
/// Wire format (159 bytes):
/// ```text
/// issuer:           32 bytes (EntityId)
/// subject:          32 bytes (EntityId)
/// scope:             4 bytes (u32)
/// channel_hash:      2 bytes (u16, 0 = all channels)
/// not_before:        8 bytes (u64 unix timestamp)
/// not_after:         8 bytes (u64 unix timestamp)
/// delegation_depth:  1 byte  (u8)
/// nonce:             8 bytes (u64)
/// --- signed above ---
/// signature:        64 bytes (ed25519)
/// ```
#[derive(Clone)]
pub struct PermissionToken {
    /// Who issued this token.
    pub issuer: EntityId,
    /// Who this token authorizes.
    pub subject: EntityId,
    /// What actions are permitted.
    pub scope: TokenScope,
    /// Channel restriction (0 = all channels).
    pub channel_hash: u16,
    /// Valid from (unix timestamp seconds).
    pub not_before: u64,
    /// Valid until (unix timestamp seconds).
    pub not_after: u64,
    /// How many times this token can be re-delegated.
    pub delegation_depth: u8,
    /// Unique nonce for revocation.
    pub nonce: u64,
    /// Ed25519 signature over all preceding fields.
    pub signature: [u8; 64],
}

impl PermissionToken {
    /// Size of the signed payload (everything before the signature).
    const SIGNED_PAYLOAD_SIZE: usize = 32 + 32 + 4 + 2 + 8 + 8 + 1 + 8; // 95 bytes

    /// Total serialized size.
    pub const WIRE_SIZE: usize = Self::SIGNED_PAYLOAD_SIZE + 64; // 159 bytes

    /// Issue a new token.
    ///
    /// `duration_secs` is clamped: a value that would overflow
    /// `now + duration_secs` saturates `not_after` at `u64::MAX`,
    /// producing a functionally-never-expiring token rather than
    /// wrapping the timestamp or panicking. Callers who want to
    /// reject pathological TTLs should range-check at the SDK
    /// layer.
    ///
    /// **Panics** if `issuer_keypair` is public-only (the migration-
    /// source path zeroizes its keypair after `ActivateAck`, leaving
    /// such a keypair). FFI callers and any path that may receive a
    /// public-only keypair must use [`Self::try_issue`] instead;
    /// `issue` is preserved as a convenience wrapper for callers
    /// (notably tests) that own a freshly-generated keypair and
    /// know it has its signing half.
    pub fn issue(
        issuer_keypair: &EntityKeypair,
        subject: EntityId,
        scope: TokenScope,
        channel_hash: u16,
        duration_secs: u64,
        delegation_depth: u8,
    ) -> Self {
        // Match each `try_issue` failure to a precise panic message.
        // A blanket `.expect("...public-only keypair...")` would
        // mis-blame any future variant (today: `ZeroTtl`) on the
        // ReadOnly path, leading whoever sees the panic to start
        // chasing a key-loading bug for what is actually a
        // `duration_secs == 0` callsite.
        match Self::try_issue(
            issuer_keypair,
            subject,
            scope,
            channel_hash,
            duration_secs,
            delegation_depth,
        ) {
            Ok(token) => token,
            Err(TokenError::ReadOnly) => {
                panic!("PermissionToken::issue called with a public-only keypair — use try_issue")
            }
            Err(TokenError::ZeroTtl) => {
                panic!("PermissionToken::issue called with duration_secs == 0 — use try_issue")
            }
            Err(e) => panic!("PermissionToken::issue failed: {e:?} — use try_issue"),
        }
    }

    /// Fallible counterpart to [`Self::issue`]: returns
    /// [`TokenError::ReadOnly`] when the issuer keypair lacks its
    /// signing half (post-migration / public-only keypair) instead
    /// of panicking. The FFI bindings route through this function
    /// so a panic doesn't unwind across `extern "C"` into
    /// C/Go-cgo/NAPI/PyO3 callers — undefined behaviour.
    pub fn try_issue(
        issuer_keypair: &EntityKeypair,
        subject: EntityId,
        scope: TokenScope,
        channel_hash: u16,
        duration_secs: u64,
        delegation_depth: u8,
    ) -> Result<Self, TokenError> {
        // A TTL of 0 produces a token with
        // `not_after == not_before`. The signature verifies but
        // `is_valid()` rejects it as `Expired` immediately
        // (`is_valid` uses strict `now >= not_after`, so a token
        // with `not_after == now` is born expired). The caller
        // mints something unusable with no diagnostic. Reject at
        // issue time so the bug surfaces as a typed error rather
        // than a silent "every check fails on the receiver".
        if duration_secs == 0 {
            return Err(TokenError::ZeroTtl);
        }
        let now = current_timestamp();
        // Abort on `getrandom` failure rather than
        // panic-unwinding through the FFI boundary. Token nonces
        // need uniqueness (replay-distinct re-issues), and a
        // predictable nonce + signed payload would let an attacker
        // re-mint identical-looking tokens — termination is the
        // only safe response.
        let mut nonce_bytes = [0u8; 8];
        if let Err(e) = getrandom::fill(&mut nonce_bytes) {
            eprintln!(
                "FATAL: PermissionToken nonce getrandom failure ({e:?}); aborting to avoid predictable token nonce"
            );
            std::process::abort();
        }
        let nonce = u64::from_le_bytes(nonce_bytes);

        let mut token = Self {
            issuer: issuer_keypair.entity_id().clone(),
            subject,
            scope,
            channel_hash,
            not_before: now,
            not_after: now.saturating_add(duration_secs),
            delegation_depth,
            nonce,
            signature: [0u8; 64],
        };

        let payload = token.signed_payload();
        // Use `try_sign` to surface a public-only keypair as
        // `TokenError::ReadOnly` instead of panicking.
        let sig = issuer_keypair
            .try_sign(&payload)
            .map_err(|_| TokenError::ReadOnly)?;
        token.signature = sig.to_bytes();
        Ok(token)
    }

    /// Verify the token's signature against the issuer's public key.
    pub fn verify(&self) -> Result<(), TokenError> {
        let payload = self.signed_payload();
        let sig = Signature::from_bytes(&self.signature);
        self.issuer
            .verify(&payload, &sig)
            .map_err(|_| TokenError::InvalidSignature)
    }

    /// Check if the token is currently valid (signature + time bounds).
    ///
    /// Both bounds are **inclusive-expiry**: the token is live while
    /// `not_before <= now < not_after`. At `now == not_after` the
    /// token is already expired. The cache sweep
    /// (`TokenCache::evict_expired`) has always used this convention
    /// (`retain(|t| t.not_after > now)` drops boundary entries);
    /// the earlier `is_valid` / `is_expired` wording accidentally
    /// treated `not_after` as the last valid second, giving every
    /// token a one-second bonus over what the sweep believed.
    /// Aligning everything on strict "< not_after" removes the
    /// off-by-one and makes the token lifetime exactly
    /// `duration_secs` seconds as `issue()` promises.
    pub fn is_valid(&self) -> Result<(), TokenError> {
        self.verify()?;
        let now = current_timestamp();
        if now < self.not_before {
            return Err(TokenError::NotYetValid);
        }
        if now >= self.not_after {
            return Err(TokenError::Expired);
        }
        Ok(())
    }

    /// Pure time-bound check: `true` iff the host wall-clock has
    /// reached `not_after`. Deliberately **does not** touch the
    /// signature — callers wanting end-to-end validity use
    /// [`Self::is_valid`], and signature integrity alone is
    /// [`Self::verify`]. This separation matters because a
    /// tampered-but-expired token is still expired, and every
    /// binding's `token_is_expired` helper documents itself as a
    /// pure time check.
    ///
    /// Boundary: `now == not_after` ⇒ expired (matches
    /// [`Self::is_valid`] and the cache's eviction convention).
    pub fn is_expired(&self) -> bool {
        current_timestamp() >= self.not_after
    }

    /// Check if this token authorizes a specific action on a channel.
    ///
    /// Returns `true` iff the token's `scope` contains the requested
    /// `action` AND either:
    ///
    /// - the token has the [`TokenScope::WILDCARD`] bit set (authorized
    ///   on every channel regardless of `channel_hash`), OR
    /// - the token's `channel_hash` matches the supplied `channel`.
    ///
    /// The previous convention — `channel_hash == 0` meaning "wildcard,
    /// all channels" — is no longer honored. A legitimate channel
    /// whose 16-bit xxh3 hashes to 0 (1 in 65 536) would otherwise
    /// accidentally turn a narrowly-scoped token into a universal
    /// grant, which an attacker able to register channel names could
    /// brute-force since xxh3 is non-cryptographic.
    pub fn authorizes(&self, action: TokenScope, channel: u16) -> bool {
        if !self.scope.contains(action) {
            return false;
        }
        if self.scope.contains(TokenScope::WILDCARD) {
            return true;
        }
        self.channel_hash == channel
    }

    /// Delegate this token to another entity with restricted scope.
    ///
    /// Returns `None` if delegation is not allowed (depth exhausted or
    /// DELEGATE not in scope).
    ///
    /// The child's `not_after` is copied from the parent verbatim,
    /// NOT derived from `parent.not_after - now`. The subtract-then-
    /// re-read-clock approach lost multiple seconds of validity
    /// when the parent was near expiry — the child's `issue()` call
    /// re-reads `current_timestamp()` and computes
    /// `now + (parent.not_after - previous_now)`, which rounds down
    /// by the wall-clock delta between the two reads. Copying
    /// `not_after` avoids the double-read and guarantees the
    /// child's lifetime is `parent.not_after - child.not_before`
    /// exactly.
    pub fn delegate(
        &self,
        signer: &EntityKeypair,
        new_subject: EntityId,
        restricted_scope: TokenScope,
    ) -> Result<Self, TokenError> {
        // Validate the parent token first
        self.is_valid()?;

        // Check delegation is allowed
        if self.delegation_depth == 0 {
            return Err(TokenError::DelegationExhausted);
        }
        if !self.scope.contains(TokenScope::DELEGATE) {
            return Err(TokenError::DelegationNotAllowed);
        }
        // Verify the signer is the subject of this token
        if signer.entity_id() != &self.subject {
            return Err(TokenError::NotAuthorized);
        }

        // New scope is intersection of current scope and requested scope
        let new_scope = self.scope.intersect(restricted_scope);

        // Issue a child whose `not_after` matches the parent's.
        // `issue()` stamps `not_before = now`, so the child's
        // effective lifetime is `parent.not_after - now` — the
        // same quantity as before, but computed against a single
        // clock read instead of two. Avoids the near-zero-lifetime
        // bug when the parent is near expiry.
        let now = current_timestamp();
        // Abort on `getrandom` failure rather than
        // panic-unwinding through the FFI boundary. Token nonces
        // need uniqueness (replay-distinct re-issues), and a
        // predictable nonce + signed payload would let an attacker
        // re-mint identical-looking tokens — termination is the
        // only safe response.
        let mut nonce_bytes = [0u8; 8];
        if let Err(e) = getrandom::fill(&mut nonce_bytes) {
            eprintln!(
                "FATAL: PermissionToken nonce getrandom failure ({e:?}); aborting to avoid predictable token nonce"
            );
            std::process::abort();
        }
        let nonce = u64::from_le_bytes(nonce_bytes);

        let mut child = Self {
            issuer: signer.entity_id().clone(),
            subject: new_subject,
            scope: new_scope,
            channel_hash: self.channel_hash,
            not_before: now,
            not_after: self.not_after,
            delegation_depth: self.delegation_depth - 1,
            nonce,
            signature: [0u8; 64],
        };
        let payload = child.signed_payload();
        // Use `try_sign` so a public-only `signer` (post-migration
        // zeroize) surfaces as `TokenError::ReadOnly` instead of
        // panicking — same shape as `try_issue`.
        // The `delegate` signature already returns
        // `Result<Self, TokenError>`, so callers naturally observe
        // the new variant without an API change.
        let sig = signer
            .try_sign(&payload)
            .map_err(|_| TokenError::ReadOnly)?;
        child.signature = sig.to_bytes();
        Ok(child)
    }

    /// Serialize the fields that are covered by the signature into
    /// a fixed-size stack buffer. The struct's signed-payload size
    /// is a compile-time constant (95 bytes), so we don't need a
    /// heap allocation per verify — the previous `Vec::with_capacity`
    /// allocated and freed 95 bytes on every signature check, which
    /// is the hottest path on every authenticated mesh packet.
    /// Returning `[u8; SIGNED_PAYLOAD_SIZE]` keeps the layout
    /// identical to the heap version (the existing callers'
    /// `&payload` still resolves to a `&[u8]`).
    fn signed_payload(&self) -> [u8; Self::SIGNED_PAYLOAD_SIZE] {
        let mut buf = [0u8; Self::SIGNED_PAYLOAD_SIZE];
        let mut off = 0;
        buf[off..off + 32].copy_from_slice(self.issuer.as_bytes());
        off += 32;
        buf[off..off + 32].copy_from_slice(self.subject.as_bytes());
        off += 32;
        buf[off..off + 4].copy_from_slice(&self.scope.bits().to_le_bytes());
        off += 4;
        buf[off..off + 2].copy_from_slice(&self.channel_hash.to_le_bytes());
        off += 2;
        buf[off..off + 8].copy_from_slice(&self.not_before.to_le_bytes());
        off += 8;
        buf[off..off + 8].copy_from_slice(&self.not_after.to_le_bytes());
        off += 8;
        buf[off] = self.delegation_depth;
        off += 1;
        buf[off..off + 8].copy_from_slice(&self.nonce.to_le_bytes());
        buf
    }

    /// Serialize to wire format.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(Self::WIRE_SIZE);
        buf.extend_from_slice(&self.signed_payload());
        buf.extend_from_slice(&self.signature);
        buf
    }

    /// Deserialize from wire format.
    ///
    /// Rejects buffers whose length is anything other than exactly
    /// [`Self::WIRE_SIZE`]. Previously this method only guarded the
    /// lower bound, silently accepting concatenated or trailing-
    /// garbage payloads — which weakened the wire-format contract
    /// and let malformed blobs parse as valid tokens. Callers
    /// framing tokens inside a larger message must slice to exactly
    /// `WIRE_SIZE` before calling this.
    pub fn from_bytes(data: &[u8]) -> Result<Self, TokenError> {
        if data.len() != Self::WIRE_SIZE {
            return Err(TokenError::InvalidFormat);
        }

        let issuer = EntityId::from_bytes(data[0..32].try_into().unwrap());
        let subject = EntityId::from_bytes(data[32..64].try_into().unwrap());
        let scope = TokenScope::from_bits(u32::from_le_bytes(data[64..68].try_into().unwrap()));
        let channel_hash = u16::from_le_bytes(data[68..70].try_into().unwrap());
        let not_before = u64::from_le_bytes(data[70..78].try_into().unwrap());
        let not_after = u64::from_le_bytes(data[78..86].try_into().unwrap());
        let delegation_depth = data[86];
        let nonce = u64::from_le_bytes(data[87..95].try_into().unwrap());
        let mut signature = [0u8; 64];
        signature.copy_from_slice(&data[95..159]);

        Ok(Self {
            issuer,
            subject,
            scope,
            channel_hash,
            not_before,
            not_after,
            delegation_depth,
            nonce,
            signature,
        })
    }
}

impl std::fmt::Debug for PermissionToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PermissionToken")
            .field("issuer", &self.issuer)
            .field("subject", &self.subject)
            .field("scope", &format!("{:04b}", self.scope.bits()))
            .field("channel_hash", &format!("{:04x}", self.channel_hash))
            .field("delegation_depth", &self.delegation_depth)
            .field("nonce", &self.nonce)
            .finish()
    }
}

/// Soft cap on the number of `(subject, channel_hash)` slots in a
/// [`TokenCache`]. Set well above any realistic deployment (a node
/// with 65 K distinct subject-channel pairs is itself an outlier)
/// while bounding the memory cost of a peer-driven flood (BUG
/// #146): pre-cap, `insert`/`insert_unchecked` admitted unlimited
/// novel keys and a peer issuing or replaying signed tokens grew
/// the cache linearly in `(subject × channel)` cardinality.
/// Existing entries always refresh; only NEW slot keys are
/// rejected at the cap. `evict_expired` reclaims slots as their
/// tokens lapse, so admission resumes once memory pressure eases.
pub const MAX_TOKEN_SLOTS: usize = 65_536;

/// Soft cap on the number of distinct-scope tokens stored within a
/// single `(subject, channel_hash)` slot. `TokenScope` is a u32
/// bitfield, so up to 2^32 distinct values are theoretically
/// possible — in practice issuers compose from a small set of
/// {PUBLISH, SUBSCRIBE, ADMIN, DELEGATE, WILDCARD}, so 32 is far
/// past real usage while bounding within-slot growth.
pub const MAX_TOKENS_PER_SLOT: usize = 32;

/// Fast permission lookup cache.
///
/// Keyed by `(subject EntityId, channel_hash)`. Each slot holds a
/// **list** of tokens — previous versions kept a single token per
/// slot, which silently dropped tokens when the same subject needed
/// multiple distinct scopes on the same channel (e.g. one PUBLISH
/// token and one SUBSCRIBE token). On insert the incoming token
/// replaces any existing entry with an **identical scope bitfield**
/// so a refresh doesn't stack duplicates, but tokens with different
/// scopes coexist.
///
/// Entries are not evicted automatically — callers should check
/// `is_valid()` on retrieved tokens, or call [`Self::evict_expired`]
/// on a cadence.
///
/// Capacity is bounded by [`MAX_TOKEN_SLOTS`] (slot count) and
/// [`MAX_TOKENS_PER_SLOT`] (tokens-with-distinct-scope per slot).
pub struct TokenCache {
    tokens: DashMap<([u8; 32], u16), Vec<PermissionToken>>,
}

impl TokenCache {
    /// Create an empty token cache.
    pub fn new() -> Self {
        Self {
            tokens: DashMap::new(),
        }
    }

    /// Insert a token into the cache after verifying its signature.
    ///
    /// Returns an error if the token's signature is invalid. This prevents
    /// self-signed or tampered tokens from being cached.
    ///
    /// Tokens with distinct scope bitfields for the same
    /// `(subject, channel_hash)` are stored side-by-side.
    /// A new token with the same scope as an existing entry
    /// **replaces** the existing one — latest-issued wins so
    /// refreshing via re-issue doesn't leak growth.
    pub fn insert(&self, token: PermissionToken) -> Result<(), TokenError> {
        token.verify()?;
        self.insert_unchecked(token);
        Ok(())
    }

    /// Insert a token without verification (for trusted internal use).
    ///
    /// Only use this when the token is known to be valid (e.g., just issued locally).
    ///
    /// WILDCARD-scoped tokens are always stored under the dedicated
    /// wildcard slot (`channel_hash = 0`) regardless of the token's
    /// own `channel_hash` field — that slot is where `check()` looks
    /// for a cross-channel fallback. Non-wildcard tokens live in
    /// their exact `channel_hash` slot.
    ///
    /// Bounded by [`MAX_TOKEN_SLOTS`] and
    /// [`MAX_TOKENS_PER_SLOT`]. When the slot cap is hit, novel
    /// keys are silently dropped (existing slot keys still
    /// refresh); when the within-slot cap is hit, novel scope
    /// bitfields are silently dropped (existing-scope refresh
    /// still wins). `evict_expired` reclaims slots as tokens
    /// lapse, restoring admission.
    pub fn insert_unchecked(&self, token: PermissionToken) {
        let slot_channel = if token.scope.contains(TokenScope::WILDCARD) {
            0
        } else {
            token.channel_hash
        };
        let key = (*token.subject.as_bytes(), slot_channel);

        // Slot cap: only refuse NOVEL keys at the cap so existing
        // peers' token refreshes still work under flood pressure.
        // The cap is enforced AFTER releasing the per-shard entry
        // lock — calling `self.tokens.len()` while holding the
        // entry's write guard would deadlock on our own shard
        // (DashMap's `len` walks every shard's lock). We accept a
        // brief observable overshoot — the inserted token is valid
        // and short-lived between `insert` and `remove` — in
        // exchange for guaranteed convergence. Pre-fix, a parallel
        // `contains_key` + `len` pre-check let N callers all see
        // `len < cap` and overshoot by N, with no rollback.
        let inserted_novel_key = {
            let mut entry = self.tokens.entry(key).or_default();
            let was_empty = entry.is_empty();
            // Replace any existing token with exactly the same scope;
            // otherwise push so distinct-scope tokens coexist.
            if let Some(slot) = entry.iter_mut().find(|t| t.scope == token.scope) {
                *slot = token;
            } else if entry.len() < MAX_TOKENS_PER_SLOT {
                // Within-slot cap: drop novel-scope tokens when the
                // slot is already at capacity. Refresh of an existing
                // scope still hits the branch above, so this only
                // fires on attempts to stack a new scope.
                entry.push(token);
            }
            was_empty
        };

        // Post-insert rollback: if we just admitted a fresh slot
        // key and the cache is now over the soft cap, remove the
        // slot we inserted. Concurrent racers all hit this branch
        // and converge to `len() <= MAX_TOKEN_SLOTS`.
        if inserted_novel_key && self.tokens.len() > MAX_TOKEN_SLOTS {
            self.tokens.remove(&key);
        }
    }

    /// Check if an entity is authorized for an action on a channel.
    ///
    /// Returns `Ok(())` if any cached token for this subject grants
    /// `action`, else an error. Walks the exact-channel slot first,
    /// then the wildcard (`channel_hash = 0`) slot. Within a slot,
    /// any valid token that authorizes the requested action wins —
    /// an expired or otherwise-invalid token in the same slot is
    /// ignored, not blocking.
    pub fn check(
        &self,
        subject: &EntityId,
        action: TokenScope,
        channel_hash: u16,
    ) -> Result<(), TokenError> {
        // Try exact channel match first
        if let Some(slot) = self.tokens.get(&(*subject.as_bytes(), channel_hash)) {
            if slot
                .value()
                .iter()
                .any(|t| t.is_valid().is_ok() && t.authorizes(action, channel_hash))
            {
                return Ok(());
            }
        }
        // Try wildcard (channel_hash = 0)
        if let Some(slot) = self.tokens.get(&(*subject.as_bytes(), 0)) {
            if slot
                .value()
                .iter()
                .any(|t| t.is_valid().is_ok() && t.authorizes(action, channel_hash))
            {
                return Ok(());
            }
        }
        Err(TokenError::NotAuthorized)
    }

    /// Fetch any cached token for `(subject, channel_hash)`. Exact
    /// match only — the wildcard (`channel_hash = 0`) entry is a
    /// separate key. Returns the first valid token in the slot; if
    /// none are valid, returns any entry (so callers can still
    /// inspect for debugging). Callers that need a specific scope
    /// should use [`Self::check`] instead.
    pub fn get(&self, subject: &EntityId, channel_hash: u16) -> Option<PermissionToken> {
        let slot = self.tokens.get(&(*subject.as_bytes(), channel_hash))?;
        let tokens = slot.value();
        // Prefer a currently-valid token; otherwise fall back to
        // the first entry so callers like `net_identity_lookup_token`
        // can still inspect it.
        tokens
            .iter()
            .find(|t| t.is_valid().is_ok())
            .or_else(|| tokens.first())
            .cloned()
    }

    /// Remove expired tokens.
    pub fn evict_expired(&self) {
        let now = current_timestamp();
        self.tokens.retain(|_, slot| {
            slot.retain(|t| t.not_after > now);
            !slot.is_empty()
        });
    }

    /// Total number of cached tokens across all slots.
    ///
    /// A slot is keyed by `(subject, channel_hash)` and can hold
    /// multiple tokens with distinct scopes (e.g. one `PUBLISH` and
    /// one `SUBSCRIBE` for the same peer-on-channel). An earlier
    /// storage change from a single `PermissionToken` per slot to
    /// a `Vec<PermissionToken>` left this method returning the
    /// slot count instead of the token count — FFI / binding
    /// metrics that surfaced "tokens cached" silently undercounted
    /// whenever a slot carried more than one scope. Sum the slot
    /// lengths so the number matches the observable cache
    /// contents.
    pub fn len(&self) -> usize {
        self.tokens.iter().map(|e| e.value().len()).sum()
    }

    /// Check if cache is empty.
    ///
    /// `evict_expired` already drops empty slots, and
    /// `insert_unchecked` never creates one, so a zero slot-count
    /// and a zero token-count coincide in practice — but checking
    /// the slot count keeps `is_empty()` O(1) instead of walking
    /// every slot.
    pub fn is_empty(&self) -> bool {
        self.tokens.is_empty()
    }
}

impl Default for TokenCache {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for TokenCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenCache")
            .field("count", &self.tokens.len())
            .finish()
    }
}

/// Errors from token operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenError {
    /// Token signature is invalid.
    InvalidSignature,
    /// Token is not yet valid (before not_before).
    NotYetValid,
    /// Token has expired (after not_after).
    Expired,
    /// Delegation depth exhausted.
    DelegationExhausted,
    /// DELEGATE scope not present in token.
    DelegationNotAllowed,
    /// No valid token found for the requested action.
    NotAuthorized,
    /// Wire format is too short or malformed.
    InvalidFormat,
    /// Issuer/signer keypair is public-only (post-migration zeroize
    /// or other read-only construction). The caller's signing
    /// operation is not possible.
    ReadOnly,
    /// `duration_secs == 0` was passed to [`PermissionToken::try_issue`].
    ///
    /// Pre-fix, a TTL of 0 produced a token with
    /// `not_after == not_before`, which every receiver immediately
    /// rejects as `Expired`. The signature verifies but every
    /// authorization check fails — silently. Reject at issue
    /// time so the caller learns about the misuse instead of
    /// minting an unusable token.
    ZeroTtl,
}

impl std::fmt::Display for TokenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidSignature => write!(f, "invalid token signature"),
            Self::NotYetValid => write!(f, "token not yet valid"),
            Self::Expired => write!(f, "token expired"),
            Self::DelegationExhausted => write!(f, "delegation depth exhausted"),
            Self::DelegationNotAllowed => write!(f, "delegation not allowed by scope"),
            Self::NotAuthorized => write!(f, "not authorized"),
            Self::InvalidFormat => write!(f, "invalid token format"),
            Self::ReadOnly => write!(f, "signer keypair is public-only"),
            Self::ZeroTtl => write!(f, "token TTL must be > 0 seconds"),
        }
    }
}

impl std::error::Error for TokenError {}

/// Current unix timestamp in seconds.
fn current_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_issue_and_verify() {
        let issuer = EntityKeypair::generate();
        let subject = EntityKeypair::generate();

        let token = PermissionToken::issue(
            &issuer,
            subject.entity_id().clone(),
            TokenScope::PUBLISH
                .union(TokenScope::SUBSCRIBE)
                .union(TokenScope::WILDCARD),
            0, // channel_hash ignored for WILDCARD tokens
            3600,
            0,
        );

        assert!(token.verify().is_ok());
        assert!(token.is_valid().is_ok());
    }

    /// A TTL of 0 must surface as `TokenError::ZeroTtl`,
    /// not silently mint a born-expired token. Pre-fix, the
    /// caller got a token whose signature verified but every
    /// authorization check failed at the receiver — no diagnostic
    /// to the issuer.
    #[test]
    fn try_issue_rejects_zero_ttl() {
        let issuer = EntityKeypair::generate();
        let subject = EntityKeypair::generate();

        let err = PermissionToken::try_issue(
            &issuer,
            subject.entity_id().clone(),
            TokenScope::PUBLISH,
            0,
            0, // ttl = 0 seconds — the bug
            0,
        )
        .unwrap_err();
        assert_eq!(err, TokenError::ZeroTtl, "expected ZeroTtl, got {:?}", err);
    }

    /// `issue` is the panicking convenience wrapper around
    /// `try_issue`. When `try_issue` rejects on a *non*-`ReadOnly`
    /// reason (today: `ZeroTtl`), the panic message must name the
    /// real cause — not the canned "called with a public-only
    /// keypair" string. Pre-fix the wrapper did
    /// `.expect("...public-only keypair...")` unconditionally, so a
    /// `duration_secs == 0` panic mis-blamed key loading and sent
    /// whoever saw the panic chasing the wrong bug.
    #[test]
    #[should_panic(expected = "duration_secs == 0")]
    fn issue_zero_ttl_panic_message_blames_ttl_not_keypair() {
        let issuer = EntityKeypair::generate();
        let subject = EntityKeypair::generate();
        let _ = PermissionToken::issue(
            &issuer,
            subject.entity_id().clone(),
            TokenScope::PUBLISH,
            0,
            0, // the bug — must panic with a TTL-flavored message
            0,
        );
    }

    /// Companion to the above: when the *real* cause is a
    /// public-only keypair, the wrapper still panics with the
    /// long-standing `ReadOnly`-flavored message, so existing
    /// callsites that grep on it keep working.
    #[test]
    #[should_panic(expected = "public-only keypair")]
    fn issue_public_only_keypair_panic_message_blames_keypair() {
        let full = EntityKeypair::generate();
        let issuer = EntityKeypair::public_only(full.entity_id().clone());
        let subject = EntityKeypair::generate();
        let _ = PermissionToken::issue(
            &issuer,
            subject.entity_id().clone(),
            TokenScope::PUBLISH,
            0,
            3600,
            0,
        );
    }

    /// TTL of 1 second is the lowest valid
    /// value; must mint a token that is `is_valid()` immediately.
    #[test]
    fn try_issue_accepts_one_second_ttl() {
        let issuer = EntityKeypair::generate();
        let subject = EntityKeypair::generate();

        let token = PermissionToken::try_issue(
            &issuer,
            subject.entity_id().clone(),
            TokenScope::PUBLISH,
            0,
            1, // 1 second — minimum valid
            0,
        )
        .expect("ttl=1 must mint cleanly (boundary)");
        assert!(token.is_valid().is_ok());
    }

    #[test]
    fn test_tampered_token() {
        let issuer = EntityKeypair::generate();
        let subject = EntityKeypair::generate();

        let mut token = PermissionToken::issue(
            &issuer,
            subject.entity_id().clone(),
            TokenScope::PUBLISH,
            0,
            3600,
            0,
        );

        // Tamper with scope
        token.scope = TokenScope::ADMIN;
        assert!(token.verify().is_err());
    }

    #[test]
    fn test_expired_token() {
        let issuer = EntityKeypair::generate();
        let subject = EntityKeypair::generate();

        // Mint with the minimum valid TTL (1 second), then
        // backdate `not_after` to the past and re-sign — this is
        // how we test expiry semantics now that try_issue rejects
        // `duration_secs == 0` outright.
        let mut token = PermissionToken::issue(
            &issuer,
            subject.entity_id().clone(),
            TokenScope::PUBLISH,
            0,
            1,
            0,
        );
        token.not_after = 0;
        let payload = token.signed_payload();
        token.signature = issuer.sign(&payload).to_bytes();

        assert!(token.verify().is_ok(), "signature is valid");
        // `not_after = 0` and the inclusive-expiry convention
        // (`now >= not_after`) says "expired" — `is_expired` and
        // `is_valid` must agree at the boundary.
        assert!(
            token.is_expired(),
            "backdated token must report expired under inclusive-expiry",
        );
        assert!(
            matches!(token.is_valid(), Err(TokenError::Expired)),
            "is_valid must agree with is_expired at the boundary",
        );
    }

    /// Regression for a cubic-flagged P3: `is_valid` / `is_expired`
    /// used strict `>` against `not_after`, so the boundary second
    /// (`now == not_after`) still counted as valid. The cache's
    /// `evict_expired` has always used the inclusive convention
    /// (drops at the boundary), so tokens survived one second longer
    /// in the "hot" caller-facing checks than in the sweep — a
    /// quiet mismatch that also gave every `issue(duration=N)` an
    /// effective lifetime of `N+1` seconds. This test pins the
    /// inclusive boundary: at `not_after` exactly, expired.
    #[test]
    fn is_valid_and_is_expired_agree_at_not_after_boundary() {
        let issuer = EntityKeypair::generate();
        let subject = EntityKeypair::generate();

        let mut token = PermissionToken::issue(
            &issuer,
            subject.entity_id().clone(),
            TokenScope::PUBLISH,
            0,
            3600,
            0,
        );
        // Force the boundary deterministically — set not_after to
        // the current wall-clock second and re-sign so `is_valid`
        // still passes its signature check.
        token.not_after = current_timestamp();
        let payload = token.signed_payload();
        token.signature = issuer.sign(&payload).to_bytes();

        assert!(
            token.is_expired(),
            "is_expired must return true at now == not_after (inclusive)",
        );
        assert!(
            matches!(token.is_valid(), Err(TokenError::Expired)),
            "is_valid must agree: Expired at now == not_after",
        );

        // And the cache eviction path must also drop it — same
        // boundary convention.
        let cache = TokenCache::new();
        cache.insert_unchecked(token);
        cache.evict_expired();
        assert_eq!(
            cache.len(),
            0,
            "evict_expired must drop a boundary token — all three code paths \
             (is_valid, is_expired, evict_expired) must agree on the boundary",
        );
    }

    /// Regression for a cubic-flagged bug that hit every FFI binding:
    /// `token_is_expired` used to call `is_valid()` and match on
    /// `Err(Expired)`, which short-circuited on signature failure.
    /// A tampered + expired token therefore returned `false` ("not
    /// expired") even though the wall-clock was past `not_after`.
    /// `is_expired()` must be a pure time check, independent of the
    /// signature.
    #[test]
    fn is_expired_ignores_signature_tampering() {
        let issuer = EntityKeypair::generate();
        let subject = EntityKeypair::generate();

        // Fresh token — not expired.
        let mut token = PermissionToken::issue(
            &issuer,
            subject.entity_id().clone(),
            TokenScope::PUBLISH,
            0,
            3600,
            0,
        );
        assert!(!token.is_expired(), "fresh token is not expired");

        // Construct the bug scenario: backdate `not_after` into the
        // past AND flip a byte in the signature. In practice a
        // tampered packet would arrive over the wire; here we
        // mutate in place so the test doesn't depend on sleeps.
        // Both mutations land outside what `verify()` recomputes —
        // not_after is part of the signed payload, so verify() is
        // already going to fail; the point is that `is_expired()`
        // doesn't care.
        token.not_after = 0;
        token.signature[0] ^= 0xFF;

        // Signature fails (expected).
        assert!(
            token.verify().is_err(),
            "mutated payload / signature must fail verify",
        );

        // Pre-fix pattern: `matches!(is_valid(), Err(Expired))`.
        // `is_valid()` short-circuits on the signature failure and
        // returns `Err(InvalidSignature)`, so the match returns
        // false — this is exactly the bug Cubic flagged.
        assert!(
            !matches!(token.is_valid(), Err(TokenError::Expired)),
            "captures the pre-fix pattern: is_valid() short-circuits \
             on signature, never reaches the time check",
        );

        // Post-fix: `is_expired()` compares time directly and
        // reports `true` regardless of signature state.
        assert!(
            token.is_expired(),
            "is_expired() must be a pure time check, independent \
             of signature validity",
        );
    }

    #[test]
    fn test_channel_filter() {
        let issuer = EntityKeypair::generate();
        let subject = EntityKeypair::generate();

        let token = PermissionToken::issue(
            &issuer,
            subject.entity_id().clone(),
            TokenScope::PUBLISH,
            0xABCD, // specific channel
            3600,
            0,
        );

        assert!(token.authorizes(TokenScope::PUBLISH, 0xABCD));
        assert!(!token.authorizes(TokenScope::PUBLISH, 0x1234)); // wrong channel
        assert!(!token.authorizes(TokenScope::SUBSCRIBE, 0xABCD)); // wrong action
    }

    #[test]
    fn test_wildcard_channel() {
        let issuer = EntityKeypair::generate();
        let subject = EntityKeypair::generate();

        // Wildcard tokens must explicitly opt in via the WILDCARD
        // scope bit — the old "channel_hash == 0 implies wildcard"
        // overload no longer applies.
        let token = PermissionToken::issue(
            &issuer,
            subject.entity_id().clone(),
            TokenScope::PUBLISH.union(TokenScope::WILDCARD),
            0,
            3600,
            0,
        );

        assert!(token.authorizes(TokenScope::PUBLISH, 0xABCD));
        assert!(token.authorizes(TokenScope::PUBLISH, 0x1234));
        assert!(token.authorizes(TokenScope::PUBLISH, 0));
    }

    #[test]
    fn test_regression_channel_hash_zero_is_not_wildcard() {
        // Regression (MEDIUM, BUGS.md): a token with `channel_hash = 0`
        // but no WILDCARD scope bit must NOT authorize arbitrary
        // channels. A legitimate channel whose 16-bit xxh3 happens
        // to hash to 0 (1 in 65 536) would otherwise turn a narrowly-
        // scoped token into a universal grant — and since xxh3 is
        // non-cryptographic, an attacker able to register names
        // could brute-force such a collision.
        let issuer = EntityKeypair::generate();
        let subject = EntityKeypair::generate();

        let token = PermissionToken::issue(
            &issuer,
            subject.entity_id().clone(),
            TokenScope::PUBLISH, // no WILDCARD
            0,                   // channel_hash 0 — pretend some channel hashed here
            3600,
            0,
        );

        // Token authorizes channel 0 only (exact match), not other channels.
        assert!(token.authorizes(TokenScope::PUBLISH, 0));
        assert!(
            !token.authorizes(TokenScope::PUBLISH, 0xABCD),
            "channel_hash=0 without WILDCARD must not grant access to arbitrary channels"
        );
        assert!(
            !token.authorizes(TokenScope::PUBLISH, 0x1234),
            "channel_hash=0 without WILDCARD must not grant access to arbitrary channels"
        );
    }

    #[test]
    fn test_delegation() {
        let root = EntityKeypair::generate();
        let node_a = EntityKeypair::generate();
        let node_b = EntityKeypair::generate();

        // Root issues to A with delegation depth 2
        let token_a = PermissionToken::issue(
            &root,
            node_a.entity_id().clone(),
            TokenScope::ALL,
            0,
            3600,
            2,
        );
        assert!(token_a.is_valid().is_ok());

        // A delegates to B with restricted scope
        let token_b = token_a
            .delegate(
                &node_a,
                node_b.entity_id().clone(),
                TokenScope::PUBLISH.union(TokenScope::DELEGATE),
            )
            .unwrap();

        assert!(token_b.is_valid().is_ok());
        assert_eq!(token_b.delegation_depth, 1);
        assert!(token_b.authorizes(TokenScope::PUBLISH, 0));
        assert!(!token_b.authorizes(TokenScope::ADMIN, 0)); // restricted away
    }

    #[test]
    fn test_delegation_depth_exhausted() {
        let root = EntityKeypair::generate();
        let node_a = EntityKeypair::generate();
        let node_b = EntityKeypair::generate();

        let token = PermissionToken::issue(
            &root,
            node_a.entity_id().clone(),
            TokenScope::ALL,
            0,
            3600,
            0, // no delegation
        );

        let result = token.delegate(&node_a, node_b.entity_id().clone(), TokenScope::PUBLISH);
        assert_eq!(result.unwrap_err(), TokenError::DelegationExhausted);
    }

    #[test]
    fn test_delegation_wrong_signer() {
        let root = EntityKeypair::generate();
        let node_a = EntityKeypair::generate();
        let node_b = EntityKeypair::generate();
        let imposter = EntityKeypair::generate();

        let token = PermissionToken::issue(
            &root,
            node_a.entity_id().clone(),
            TokenScope::ALL,
            0,
            3600,
            1,
        );

        // Imposter tries to delegate A's token
        let result = token.delegate(&imposter, node_b.entity_id().clone(), TokenScope::PUBLISH);
        assert_eq!(result.unwrap_err(), TokenError::NotAuthorized);
    }

    #[test]
    fn test_serialization_roundtrip() {
        let issuer = EntityKeypair::generate();
        let subject = EntityKeypair::generate();

        let token = PermissionToken::issue(
            &issuer,
            subject.entity_id().clone(),
            TokenScope::PUBLISH.union(TokenScope::SUBSCRIBE),
            0xBEEF,
            3600,
            3,
        );

        let bytes = token.to_bytes();
        assert_eq!(bytes.len(), PermissionToken::WIRE_SIZE);

        let parsed = PermissionToken::from_bytes(&bytes).unwrap();
        assert!(parsed.verify().is_ok());
        assert_eq!(parsed.issuer, token.issuer);
        assert_eq!(parsed.subject, token.subject);
        assert_eq!(parsed.scope.bits(), token.scope.bits());
        assert_eq!(parsed.channel_hash, 0xBEEF);
        assert_eq!(parsed.delegation_depth, 3);
        assert_eq!(parsed.nonce, token.nonce);
    }

    /// `TokenScope::contains(NONE)` must return `false` — the bit
    /// identity `(bits & 0) == 0` is unconditionally true, so any
    /// token would otherwise "contain" the no-op action. A caller
    /// that builds `action: TokenScope` from external input (e.g. a
    /// wire `u32` masked into a smaller subset) would then receive
    /// a blanket `true` against any token whenever the masked input
    /// happened to land on `NONE`.
    #[test]
    fn token_scope_does_not_contain_none() {
        // Any defined scope must NOT contain NONE.
        for s in [
            TokenScope::PUBLISH,
            TokenScope::SUBSCRIBE,
            TokenScope::ADMIN,
            TokenScope::DELEGATE,
            TokenScope::WILDCARD,
            TokenScope::ALL,
            TokenScope::PUBLISH.union(TokenScope::SUBSCRIBE),
        ] {
            assert!(
                !s.contains(TokenScope::NONE),
                "scope {:?} must not contain NONE",
                s.bits(),
            );
        }
        // Even NONE itself does not "contain" NONE — the question is
        // "do you authorize this action," and the no-op action is
        // never authorized.
        assert!(
            !TokenScope::NONE.contains(TokenScope::NONE),
            "NONE.contains(NONE) must be false (no token authorizes the no-op action)",
        );

        // Sanity: contains is still correct for non-NONE arguments.
        assert!(TokenScope::ALL.contains(TokenScope::PUBLISH));
        assert!(!TokenScope::PUBLISH.contains(TokenScope::ADMIN));
        assert!(TokenScope::PUBLISH
            .union(TokenScope::SUBSCRIBE)
            .contains(TokenScope::SUBSCRIBE));
    }

    #[test]
    fn test_token_cache() {
        let issuer = EntityKeypair::generate();
        let subject = EntityKeypair::generate();

        let cache = TokenCache::new();

        let token = PermissionToken::issue(
            &issuer,
            subject.entity_id().clone(),
            TokenScope::PUBLISH,
            0xABCD,
            3600,
            0,
        );
        let _ = cache.insert(token);

        assert_eq!(cache.len(), 1);

        // Should find the token
        assert!(cache
            .check(subject.entity_id(), TokenScope::PUBLISH, 0xABCD)
            .is_ok());

        // Wrong channel
        assert!(cache
            .check(subject.entity_id(), TokenScope::PUBLISH, 0x1234)
            .is_err());

        // Wrong action
        assert!(cache
            .check(subject.entity_id(), TokenScope::ADMIN, 0xABCD)
            .is_err());

        // Unknown entity
        let unknown = EntityKeypair::generate();
        assert!(cache
            .check(unknown.entity_id(), TokenScope::PUBLISH, 0xABCD)
            .is_err());
    }

    #[test]
    fn test_token_cache_wildcard() {
        let issuer = EntityKeypair::generate();
        let subject = EntityKeypair::generate();

        let cache = TokenCache::new();

        // Wildcard token: explicit WILDCARD scope bit.
        let token = PermissionToken::issue(
            &issuer,
            subject.entity_id().clone(),
            TokenScope::PUBLISH.union(TokenScope::WILDCARD),
            0,
            3600,
            0,
        );
        let _ = cache.insert(token);

        // Should match any channel
        assert!(cache
            .check(subject.entity_id(), TokenScope::PUBLISH, 0xABCD)
            .is_ok());
        assert!(cache
            .check(subject.entity_id(), TokenScope::PUBLISH, 0x1234)
            .is_ok());
    }

    // ---- Regression tests for Cubic AI findings ----

    #[test]
    fn test_regression_wildcard_fallback_not_blocked_by_expired_channel_token() {
        // Regression: token.is_valid()? short-circuited on an expired
        // channel-specific token, preventing the wildcard fallback from
        // being reached.
        let issuer = EntityKeypair::generate();
        let subject = EntityKeypair::generate();
        let cache = TokenCache::new();

        // Insert an expired channel-specific token. Mint with
        // TTL=1 (try_issue rejects 0), then backdate not_after to
        // force expiry.
        let mut expired_token = PermissionToken::issue(
            &issuer,
            subject.entity_id().clone(),
            TokenScope::PUBLISH,
            0xABCD,
            1,
            0,
        );
        // Force expiry by setting not_after to the past
        expired_token.not_after = 0;
        // Re-sign with the modified field
        let payload = expired_token.signed_payload();
        expired_token.signature = issuer.sign(&payload).to_bytes();
        cache.insert_unchecked(expired_token);

        // Insert a valid wildcard token (explicit WILDCARD scope bit).
        let wildcard_token = PermissionToken::issue(
            &issuer,
            subject.entity_id().clone(),
            TokenScope::PUBLISH.union(TokenScope::WILDCARD),
            0,
            3600,
            0,
        );
        cache.insert_unchecked(wildcard_token);

        // The wildcard should be reached despite the expired channel token
        assert!(
            cache
                .check(subject.entity_id(), TokenScope::PUBLISH, 0xABCD)
                .is_ok(),
            "wildcard fallback must not be blocked by expired channel-specific token"
        );
    }

    #[test]
    fn test_regression_delegate_rejects_expired_parent() {
        // Regression: delegate() minted child tokens from an invalid parent
        // because it never called is_valid() on the parent.
        let root = EntityKeypair::generate();
        let node_a = EntityKeypair::generate();
        let node_b = EntityKeypair::generate();

        let mut token = PermissionToken::issue(
            &root,
            node_a.entity_id().clone(),
            TokenScope::ALL,
            0,
            3600,
            2,
        );
        // Force expiry
        token.not_after = 0;
        let payload = token.signed_payload();
        token.signature = root.sign(&payload).to_bytes();

        let result = token.delegate(&node_a, node_b.entity_id().clone(), TokenScope::PUBLISH);
        assert_eq!(
            result.unwrap_err(),
            TokenError::Expired,
            "delegation from expired parent must be rejected"
        );
    }

    #[test]
    fn test_regression_insert_rejects_tampered_token() {
        // Regression: insert() accepted self-signed/tampered tokens
        // because it did not verify the signature.
        let issuer = EntityKeypair::generate();
        let subject = EntityKeypair::generate();

        let mut token = PermissionToken::issue(
            &issuer,
            subject.entity_id().clone(),
            TokenScope::PUBLISH,
            0,
            3600,
            0,
        );
        // Tamper: change scope after signing
        token.scope = TokenScope::ADMIN;

        let cache = TokenCache::new();
        assert!(
            cache.insert(token).is_err(),
            "insert must reject tampered token"
        );
        assert_eq!(cache.len(), 0, "tampered token must not be cached");
    }

    // ========================================================================
    // Cubic-flagged P1/P2 regressions
    // ========================================================================

    /// Regression for a cubic-flagged P1: TokenCache used to key on
    /// `(subject, channel_hash)` and store a single token per slot,
    /// so inserting a SUBSCRIBE token after a PUBLISH token
    /// silently overwrote the earlier one. Both must coexist.
    #[test]
    fn cache_coexists_tokens_of_different_scopes_for_same_channel() {
        let issuer = EntityKeypair::generate();
        let subject = EntityKeypair::generate();
        let channel = 0xABCD;

        let publish_tok = PermissionToken::issue(
            &issuer,
            subject.entity_id().clone(),
            TokenScope::PUBLISH,
            channel,
            3600,
            0,
        );
        let subscribe_tok = PermissionToken::issue(
            &issuer,
            subject.entity_id().clone(),
            TokenScope::SUBSCRIBE,
            channel,
            3600,
            0,
        );

        let cache = TokenCache::new();
        cache.insert(publish_tok).expect("insert publish");
        cache.insert(subscribe_tok).expect("insert subscribe");

        // Both authorizations must pass — the second insert used to
        // clobber the first because the cache was keyed without
        // considering scope.
        assert!(
            cache
                .check(subject.entity_id(), TokenScope::PUBLISH, channel)
                .is_ok(),
            "publish auth lost after subscribe insert",
        );
        assert!(
            cache
                .check(subject.entity_id(), TokenScope::SUBSCRIBE, channel)
                .is_ok(),
            "subscribe auth lost",
        );
    }

    /// Regression for a cubic-flagged P2: after the storage change
    /// from `PermissionToken` to `Vec<PermissionToken>` per slot,
    /// `TokenCache::len()` kept returning `self.tokens.len()` —
    /// the slot count, not the token count. FFI / binding metrics
    /// silently undercounted whenever a slot held more than one
    /// scope. This test exercises the multi-scope case: two tokens
    /// share a slot, so a slot count of 1 coexists with a token
    /// count of 2 — `len()` must report 2.
    #[test]
    fn cache_len_reports_total_tokens_not_slot_count() {
        let issuer = EntityKeypair::generate();
        let subject = EntityKeypair::generate();
        let channel = 0xFEED;

        let cache = TokenCache::new();
        assert_eq!(cache.len(), 0);

        // Two tokens, same (subject, channel) slot, different scopes
        // — coexist in one Vec per `insert_unchecked`.
        cache
            .insert(PermissionToken::issue(
                &issuer,
                subject.entity_id().clone(),
                TokenScope::PUBLISH,
                channel,
                3600,
                0,
            ))
            .expect("insert publish");
        cache
            .insert(PermissionToken::issue(
                &issuer,
                subject.entity_id().clone(),
                TokenScope::SUBSCRIBE,
                channel,
                3600,
                0,
            ))
            .expect("insert subscribe");

        assert_eq!(
            cache.len(),
            2,
            "len() must sum per-slot Vec lengths — two scopes in one slot means two tokens",
        );

        // A third token with a different channel lives in its own
        // slot, bumping both slot count and token count to 2 / 3.
        cache
            .insert(PermissionToken::issue(
                &issuer,
                subject.entity_id().clone(),
                TokenScope::PUBLISH,
                0xBEEF,
                3600,
                0,
            ))
            .expect("insert publish-other");
        assert_eq!(
            cache.len(),
            3,
            "len() after a second slot must reflect 3 tokens total, not 2 slots",
        );
    }

    /// Regression for the other half of the cache semantic: issuing
    /// a SECOND token with the same scope as an existing one
    /// should **replace** it, not stack. Otherwise repeated refreshes
    /// leak linear memory.
    #[test]
    fn cache_same_scope_reinsert_replaces_not_stacks() {
        let issuer = EntityKeypair::generate();
        let subject = EntityKeypair::generate();
        let channel = 0xABCD;

        let cache = TokenCache::new();
        for _ in 0..10 {
            let tok = PermissionToken::issue(
                &issuer,
                subject.entity_id().clone(),
                TokenScope::SUBSCRIBE,
                channel,
                3600,
                0,
            );
            cache.insert(tok).expect("insert");
        }
        // All ten had scope=SUBSCRIBE. The cache should hold one
        // entry total (the most recent), not ten.
        assert_eq!(
            cache.len(),
            1,
            "repeated inserts with the same scope must replace, not stack",
        );
    }

    /// Regression for a cubic-flagged P2: `from_bytes` used to
    /// accept any buffer ≥ WIRE_SIZE, silently ignoring trailing
    /// bytes. Concatenated / corrupted payloads must fail cleanly.
    #[test]
    fn from_bytes_rejects_trailing_garbage() {
        let issuer = EntityKeypair::generate();
        let subject = EntityKeypair::generate();
        let tok = PermissionToken::issue(
            &issuer,
            subject.entity_id().clone(),
            TokenScope::PUBLISH,
            0,
            3600,
            0,
        );
        let mut bytes = tok.to_bytes();
        assert_eq!(bytes.len(), PermissionToken::WIRE_SIZE);
        // Fresh bytes parse fine.
        assert!(PermissionToken::from_bytes(&bytes).is_ok());

        // Append trailing garbage — parser must now refuse.
        bytes.push(0xFF);
        assert!(
            matches!(
                PermissionToken::from_bytes(&bytes),
                Err(TokenError::InvalidFormat)
            ),
            "trailing byte must reject as InvalidFormat",
        );

        // Truncate by one — also refused (already was, but lock in).
        let truncated = &tok.to_bytes()[..PermissionToken::WIRE_SIZE - 1];
        assert!(matches!(
            PermissionToken::from_bytes(truncated),
            Err(TokenError::InvalidFormat)
        ));
    }

    /// Regression for a cubic-flagged P1: `issue()` used unchecked
    /// `now + duration_secs`, which panics in debug builds on
    /// large TTL. Saturating add yields a never-expiring token
    /// instead of crashing.
    #[test]
    fn issue_with_huge_ttl_saturates_rather_than_panics() {
        let issuer = EntityKeypair::generate();
        let subject = EntityKeypair::generate();
        let tok = PermissionToken::issue(
            &issuer,
            subject.entity_id().clone(),
            TokenScope::PUBLISH,
            0,
            u64::MAX,
            0,
        );
        assert_eq!(
            tok.not_after,
            u64::MAX,
            "TTL=u64::MAX must saturate, not wrap or panic",
        );
        assert!(!tok.is_expired());
        // Signature is still valid.
        assert!(tok.verify().is_ok());
    }

    /// Regression for a cubic-flagged P2: `delegate()` computed the
    /// child's TTL as `parent.not_after - current_timestamp()` and
    /// then passed that duration back through `issue()`, which
    /// re-reads `current_timestamp()`. When the parent was close
    /// to expiry the double-read shaved meaningful lifetime off
    /// the child — in the worst case, a child token born already
    /// expired. The fix copies `parent.not_after` directly.
    #[test]
    fn delegate_preserves_parent_not_after() {
        let a = EntityKeypair::generate();
        let b = EntityKeypair::generate();
        let c = EntityKeypair::generate();

        let parent = PermissionToken::issue(
            &a,
            b.entity_id().clone(),
            TokenScope::PUBLISH.union(TokenScope::DELEGATE),
            0,
            3600,
            2,
        );

        let child = parent
            .delegate(&b, c.entity_id().clone(), TokenScope::PUBLISH)
            .expect("delegate");

        assert_eq!(
            child.not_after, parent.not_after,
            "child's not_after must equal parent's, not some smaller value \
             derived from a second clock read",
        );
        // child.not_before was stamped by the child's own clock
        // read, so it's ≥ parent.not_before — which is correct.
        assert!(child.not_before >= parent.not_before);
        assert!(child.verify().is_ok());
    }

    // ========================================================================
    // TEST_COVERAGE_PLAN §P2-9 — TokenCache concurrency safety.
    //
    // The cache is a DashMap, so entry-level writes are atomic,
    // but the mesh-side usage pattern runs insert / check /
    // evict_expired on the same entry from three different
    // tokio tasks under load. These tests pin: no panic, no
    // torn reads, terminal state coherent.
    // ========================================================================

    /// Concurrent `insert_unchecked` (authorize) + `check`
    /// (authorize-gate) + `evict_expired` (sweep) on the same
    /// subject+channel must not panic or produce an inconsistent
    /// terminal state. The observer thread's `check` must always
    /// return a deterministic `Ok(())` or `Err(NotAuthorized)`
    /// — never a corrupted DashMap state (which would manifest
    /// as a panic inside `iter().any(...)`).
    #[test]
    fn concurrent_insert_check_evict_is_panic_free() {
        use std::sync::{Arc, Barrier};
        use std::thread;

        let cache = Arc::new(TokenCache::new());
        let issuer = EntityKeypair::generate();
        let subject_kp = EntityKeypair::generate();
        let subject_id = subject_kp.entity_id().clone();
        let channel_hash = 0xABCDu16;
        let iters = 500u32;
        // Start barrier — without it thread scheduling can let
        // the evictor run its whole loop before the inserter
        // even starts, trivializing the race.
        let start = Arc::new(Barrier::new(3));

        // Inserter: re-issue + replace the token on each
        // iteration. Each insert overwrites the previous entry
        // (same scope → `insert_unchecked`'s `iter_mut().find`
        // path replaces rather than pushes).
        let inserter = {
            let cache = cache.clone();
            let issuer = issuer.clone();
            let subject_id = subject_id.clone();
            let start = start.clone();
            thread::spawn(move || {
                start.wait();
                for _ in 0..iters {
                    let token = PermissionToken::issue(
                        &issuer,
                        subject_id.clone(),
                        TokenScope::SUBSCRIBE,
                        channel_hash,
                        300,
                        0,
                    );
                    cache.insert_unchecked(token);
                }
            })
        };

        // Checker: gate queries fire on the hot path. Must not
        // panic, must return a deterministic Result.
        let checker = {
            let cache = cache.clone();
            let subject_id = subject_id.clone();
            let start = start.clone();
            thread::spawn(move || {
                start.wait();
                for _ in 0..iters {
                    let _ = cache.check(&subject_id, TokenScope::SUBSCRIBE, channel_hash);
                }
            })
        };

        // Evictor: periodic sweep. `evict_expired` walks every
        // slot and retains only not-yet-expired tokens; with
        // 300 s TTLs and a sub-second test, no tokens expire so
        // no entries should actually be removed, but the retain
        // closure must run safely against the writer.
        let evictor = {
            let cache = cache.clone();
            let start = start.clone();
            thread::spawn(move || {
                start.wait();
                for _ in 0..iters {
                    cache.evict_expired();
                }
            })
        };

        inserter.join().expect("inserter panicked");
        checker.join().expect("checker panicked");
        evictor.join().expect("evictor panicked");

        // Terminal state: exactly one token present for this
        // (subject, channel_hash, SUBSCRIBE) slot. The inserter
        // replaced on every iteration — the final token must
        // be valid, and `check` must return Ok(()) against it.
        assert!(
            cache
                .check(&subject_id, TokenScope::SUBSCRIBE, channel_hash)
                .is_ok(),
            "terminal check must succeed — the last insert's token is unexpired",
        );
        assert_eq!(
            cache.len(),
            1,
            "exactly one token should remain (same-scope replace path); got {}",
            cache.len(),
        );
    }

    /// A token that expires mid-test must be dropped by a
    /// concurrent `evict_expired`. The checker's `check` must
    /// return `Ok(())` while the token is still valid and
    /// consistently `Err(NotAuthorized)` after eviction, never
    /// a panic from a retain that ran mid-iter.
    #[test]
    fn evict_expired_races_with_check_without_panic() {
        use std::sync::{Arc, Barrier};
        use std::thread;
        use std::time::Duration;

        let cache = Arc::new(TokenCache::new());
        let issuer = EntityKeypair::generate();
        let subject_kp = EntityKeypair::generate();
        let subject_id = subject_kp.entity_id().clone();
        let channel_hash = 0xBEEFu16;

        // Short-lived token: 1 s TTL. Insert it then let it
        // expire naturally during the race.
        let token = PermissionToken::issue(
            &issuer,
            subject_id.clone(),
            TokenScope::PUBLISH,
            channel_hash,
            1, // 1-second TTL
            0,
        );
        cache.insert_unchecked(token);
        assert!(
            cache
                .check(&subject_id, TokenScope::PUBLISH, channel_hash)
                .is_ok(),
            "pre-expiry check should succeed",
        );

        let start = Arc::new(Barrier::new(2));
        let checker = {
            let cache = cache.clone();
            let subject_id = subject_id.clone();
            let start = start.clone();
            thread::spawn(move || {
                start.wait();
                for _ in 0..2_000 {
                    // Outcome may transition from Ok → Err
                    // exactly once during this loop as the TTL
                    // elapses. Either result is valid; panic
                    // is not.
                    let _ = cache.check(&subject_id, TokenScope::PUBLISH, channel_hash);
                }
            })
        };
        let evictor = {
            let cache = cache.clone();
            let start = start.clone();
            thread::spawn(move || {
                start.wait();
                for _ in 0..2_000 {
                    cache.evict_expired();
                }
            })
        };

        // Wait for TTL to elapse. `current_timestamp` is
        // second-resolution, so 1.5 s of wall clock guarantees
        // `not_after` < `now`.
        thread::sleep(Duration::from_millis(1_500));

        checker.join().expect("checker panicked");
        evictor.join().expect("evictor panicked");

        // Terminal: a fresh evict + check — the token's TTL
        // has expired and the evictor swept at least once since,
        // so check must return NotAuthorized.
        cache.evict_expired();
        match cache.check(&subject_id, TokenScope::PUBLISH, channel_hash) {
            Err(TokenError::NotAuthorized) => {}
            other => panic!("expected NotAuthorized after TTL + evict; got {other:?}"),
        }
    }

    // ========================================================================
    // TokenCache must bound slot growth and within-slot growth
    // ========================================================================

    /// Helper: build a token whose subject is the bytes of `subject_seed`
    /// padded into an EntityId, on `channel_hash`. We bypass the
    /// `insert(...)` signature-verify path by issuing real tokens —
    /// this is a fast-enough way to get many distinct subjects.
    fn issue_token_for(seed: u64, channel_hash: u16, scope: TokenScope) -> PermissionToken {
        let issuer = EntityKeypair::generate();
        // EntityKeypair::generate uses entropy; we just need many
        // distinct subjects, so a per-iteration generate is fine
        // (the test caps iteration counts).
        let _ = seed;
        let subject = EntityKeypair::generate();
        PermissionToken::issue(
            &issuer,
            subject.entity_id().clone(),
            scope,
            channel_hash,
            3600,
            0,
        )
    }

    /// Once `MAX_TOKEN_SLOTS` distinct slot keys are present,
    /// further `insert_unchecked` calls with NOVEL keys must NOT
    /// admit a new slot. Existing-slot refresh paths still work
    /// (covered by `replays_existing_subject_when_slot_cap_is_full`
    /// below). Pre-fix the cache grew linearly with peer-supplied
    /// `(subject, channel_hash)` cardinality.
    ///
    /// Setup uses a single subject and varies `channel_hash` so
    /// the slot keys differ — this avoids spending O(slots) ed25519
    /// keypair generations.
    #[test]
    fn insert_unchecked_drops_novel_slot_when_at_max_token_slots() {
        let issuer = EntityKeypair::generate();
        let subject = EntityKeypair::generate();
        let cache = TokenCache::new();

        // Fill the cache to capacity using the same subject with
        // varying channel_hash. `MAX_TOKEN_SLOTS` is 65_536 and
        // channel_hash is u16 (also 65_536 distinct values), so
        // we can pack the cache exactly to capacity. Building
        // 65_536 PermissionTokens would do 65_536 ed25519 signs,
        // which is too slow for a unit test — instead we
        // pre-build one template token and clone-with-mutated
        // channel_hash. The signature stops being valid after the
        // mutation, but `insert_unchecked` skips verify, so the
        // cache shape under test is identical to the real path.
        let template = PermissionToken::issue(
            &issuer,
            subject.entity_id().clone(),
            TokenScope::PUBLISH,
            0,
            3600,
            0,
        );
        // u16 has exactly MAX_TOKEN_SLOTS (= 65_536) distinct
        // values; `0..MAX_TOKEN_SLOTS as u16` would be `0..0` (cast
        // overflow), so iterate inclusively instead.
        for ch in 0u32..MAX_TOKEN_SLOTS as u32 {
            let mut t = template.clone();
            t.channel_hash = ch as u16;
            cache.insert_unchecked(t);
        }
        // Note: u16 has exactly MAX_TOKEN_SLOTS distinct values, so
        // the cache should hold exactly MAX_TOKEN_SLOTS slots now.
        // (We can't iterate beyond u16::MAX; this is a deliberate
        // alignment with the cap.)
        let len_before_overflow = cache.tokens.len();
        assert_eq!(
            len_before_overflow, MAX_TOKEN_SLOTS,
            "test setup: cache must be filled to capacity",
        );

        // A NOVEL slot key — different subject, channel_hash=0 — must
        // be dropped at the cap.
        let other_subject = EntityKeypair::generate();
        let novel = PermissionToken::issue(
            &issuer,
            other_subject.entity_id().clone(),
            TokenScope::PUBLISH,
            0,
            3600,
            0,
        );
        cache.insert_unchecked(novel);

        assert_eq!(
            cache.tokens.len(),
            MAX_TOKEN_SLOTS,
            "novel slot must be rejected at MAX_TOKEN_SLOTS cap",
        );
    }

    /// At capacity, refreshing an EXISTING slot key (same subject +
    /// same channel_hash) must still succeed — we only refuse novel
    /// keys. Pins that the cap doesn't accidentally lock out
    /// legitimate token refreshes once a peer-driven flood has filled
    /// the cache.
    #[test]
    fn insert_unchecked_replays_existing_subject_when_slot_cap_is_full() {
        let issuer = EntityKeypair::generate();
        let subject = EntityKeypair::generate();
        let cache = TokenCache::new();

        // Fill to capacity.
        let template = PermissionToken::issue(
            &issuer,
            subject.entity_id().clone(),
            TokenScope::PUBLISH,
            0,
            3600,
            0,
        );
        // u16 has exactly MAX_TOKEN_SLOTS (= 65_536) distinct
        // values; `0..MAX_TOKEN_SLOTS as u16` would be `0..0` (cast
        // overflow), so iterate inclusively instead.
        for ch in 0u32..MAX_TOKEN_SLOTS as u32 {
            let mut t = template.clone();
            t.channel_hash = ch as u16;
            cache.insert_unchecked(t);
        }
        assert_eq!(cache.tokens.len(), MAX_TOKEN_SLOTS);

        // Refresh an existing slot (subject + channel_hash=42 already
        // present). Must succeed — same scope replaces same scope.
        let mut refresh = template.clone();
        refresh.channel_hash = 42;
        refresh.nonce = 9999;
        cache.insert_unchecked(refresh);

        assert_eq!(cache.tokens.len(), MAX_TOKEN_SLOTS, "slot count unchanged");
        let slot = cache
            .tokens
            .get(&(*subject.entity_id().as_bytes(), 42))
            .unwrap();
        assert_eq!(slot.value().len(), 1, "still one token in slot");
        assert_eq!(slot.value()[0].nonce, 9999, "refresh replaced the token");
    }

    /// Within a single slot, novel scope bitfields stack up to
    /// `MAX_TOKENS_PER_SLOT`; beyond that the new-scope path drops
    /// silently. Refreshing an existing scope still wins.
    #[test]
    fn insert_unchecked_caps_within_slot_token_count() {
        let issuer = EntityKeypair::generate();
        let subject = EntityKeypair::generate();
        let cache = TokenCache::new();

        // Pack the slot with MAX_TOKENS_PER_SLOT distinct-scope
        // tokens. We use the bitfield directly via from_bits to
        // produce many distinct scope values cheaply. Each token
        // stays at the same (subject, channel_hash) so they share
        // a slot.
        let channel = 0xCAFE;
        let template = PermissionToken::issue(
            &issuer,
            subject.entity_id().clone(),
            TokenScope::PUBLISH,
            channel,
            3600,
            0,
        );
        for i in 0..MAX_TOKENS_PER_SLOT as u32 {
            let mut t = template.clone();
            // Vary the high bits so each scope value is distinct
            // AND has the WILDCARD bit (0b1_0000 / 0x10) consistently
            // *un*set — otherwise tokens with WILDCARD set would
            // route to `slot_channel = 0` and split off into a
            // different slot, dodging the within-slot cap test.
            // Shift `i` past the WILDCARD bit so it never
            // accidentally lights up.
            t.scope = TokenScope::from_bits(0x10_0000 | (i << 8));
            cache.insert_unchecked(t);
        }
        let slot_before = cache
            .tokens
            .get(&(*subject.entity_id().as_bytes(), channel))
            .unwrap();
        assert_eq!(
            slot_before.value().len(),
            MAX_TOKENS_PER_SLOT,
            "test setup: slot must be packed to within-slot cap",
        );
        drop(slot_before); // release DashMap ref before the next op

        // A token with a NOVEL scope bitfield must be dropped.
        // Use a value that ALSO doesn't set the WILDCARD bit so it
        // routes to the same slot as the packed entries.
        let novel_scope_bits = 0x20_0000u32;
        let mut over = template.clone();
        over.scope = TokenScope::from_bits(novel_scope_bits);
        cache.insert_unchecked(over);
        let slot_after = cache
            .tokens
            .get(&(*subject.entity_id().as_bytes(), channel))
            .unwrap();
        assert_eq!(
            slot_after.value().len(),
            MAX_TOKENS_PER_SLOT,
            "novel scope must be rejected at MAX_TOKENS_PER_SLOT",
        );
        assert!(
            slot_after
                .value()
                .iter()
                .all(|t| t.scope.bits() != novel_scope_bits),
            "the dropped scope must not be present in the slot",
        );

        // Refresh of an EXISTING scope still wins, even at cap.
        let _ = issue_token_for; // silence unused warning if future tests don't need it
        drop(slot_after);
        // Refresh the i=0 entry — its scope was 0x10_0000.
        let mut refresh = template.clone();
        refresh.scope = TokenScope::from_bits(0x10_0000);
        refresh.nonce = 1111;
        cache.insert_unchecked(refresh);
        let slot_after_refresh = cache
            .tokens
            .get(&(*subject.entity_id().as_bytes(), channel))
            .unwrap();
        let refreshed = slot_after_refresh
            .value()
            .iter()
            .find(|t| t.scope.bits() == 0x10_0000)
            .expect("scope 0x10_0000 must still be present");
        assert_eq!(
            refreshed.nonce, 1111,
            "refresh-of-existing-scope must succeed at cap"
        );
    }

    /// Concurrent novel-key inserts must NOT overshoot
    /// `MAX_TOKEN_SLOTS`. Pre-fix the path was:
    ///
    /// ```ignore
    /// if !contains_key(&key) && len() >= cap { return; }
    /// entry(key).or_default()...
    /// ```
    ///
    /// N threads could all observe `len() < cap` simultaneously and
    /// each go on to `or_default()` a fresh entry — overshoot
    /// proportional to N (bounded only by concurrency, NOT by
    /// `DashMap` shard count as the prior comment claimed). Under a
    /// peer-driven token flood across a multi-core daemon, this
    /// uncaps the cache.
    ///
    /// Prefill the DashMap directly (bypassing the expensive
    /// PermissionToken::issue ed25519-sign + clone pipeline) to
    /// `MAX_TOKEN_SLOTS - SLACK`, then run `THREADS` concurrent
    /// `insert_unchecked` calls each carrying a distinct novel key.
    /// After the dust settles, the cache must hold at most
    /// `MAX_TOKEN_SLOTS` — the slack lets a few inserts succeed
    /// (correct) while the rest must roll back (the gate).
    #[test]
    fn insert_unchecked_does_not_overshoot_under_concurrent_novel_inserts() {
        use std::sync::Arc;
        use std::thread;

        const SLACK: usize = 4;
        const THREADS: usize = 32;

        let issuer = EntityKeypair::generate();
        let subject = EntityKeypair::generate();
        let cache = Arc::new(TokenCache::new());

        // Build one template token (one ed25519 sign) and seed
        // `MAX_TOKEN_SLOTS - SLACK` slot keys directly into the
        // backing `DashMap`. We aren't testing what the prefill
        // path does — we're testing the cap gate against a single
        // wave of concurrent novel inserts, so prefill speed
        // matters, not prefill semantics.
        let template = PermissionToken::issue(
            &issuer,
            subject.entity_id().clone(),
            TokenScope::PUBLISH,
            0,
            3600,
            0,
        );
        let prefill = MAX_TOKEN_SLOTS - SLACK;
        for ch in 0u32..prefill as u32 {
            let mut t = template.clone();
            t.channel_hash = ch as u16;
            cache
                .tokens
                .insert((*subject.entity_id().as_bytes(), ch as u16), vec![t]);
        }
        assert_eq!(cache.tokens.len(), prefill);

        // Each thread inserts a unique novel key (different subject
        // bytes — synthesized directly so we don't pay an ed25519
        // generate per thread). The race is around the gate's
        // len()-check vs entry-insert.
        let barrier = Arc::new(std::sync::Barrier::new(THREADS));
        let mut handles = Vec::with_capacity(THREADS);
        for tid in 0..THREADS {
            let cache = Arc::clone(&cache);
            let mut novel = template.clone();
            // Synthesize a novel subject by mutating the bytes —
            // identity verification is bypassed by `insert_unchecked`.
            let mut subj_bytes = *subject.entity_id().as_bytes();
            subj_bytes[0] ^= (tid as u8).wrapping_add(1);
            subj_bytes[1] ^= ((tid >> 8) as u8).wrapping_add(1);
            novel.subject = EntityId::from_bytes(subj_bytes);
            novel.channel_hash = (prefill + tid) as u16;
            let barrier = Arc::clone(&barrier);
            handles.push(thread::spawn(move || {
                barrier.wait();
                cache.insert_unchecked(novel);
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        // The strong invariant: NEVER exceed cap. Pre-fix this would
        // overshoot to up to `prefill + THREADS = cap - SLACK + THREADS`.
        let final_len = cache.tokens.len();
        assert!(
            final_len <= MAX_TOKEN_SLOTS,
            "cache overshot cap under concurrent novel inserts: {final_len} > {MAX_TOKEN_SLOTS}",
        );
        // Sanity: at least the prefill survives — concurrent inserts
        // must never remove pre-existing slots.
        assert!(
            final_len >= prefill,
            "prefill leaked: {final_len} < {prefill}",
        );
    }

    // ========================================================================
    // try_issue / delegate must NOT panic on public-only keypair
    // ========================================================================

    /// `try_issue` returns `TokenError::ReadOnly` instead of
    /// panicking when the issuer keypair is public-only (e.g.
    /// post-migration zeroize). FFI bindings route through this
    /// to avoid panic-unwinding across `extern "C"`.
    #[test]
    fn try_issue_returns_read_only_on_public_only_keypair() {
        let full = EntityKeypair::generate();
        // Build a public-only sibling that shares the same entity_id.
        let public_only = EntityKeypair::public_only(full.entity_id().clone());
        assert!(public_only.try_sign(b"x").is_err());

        let subject = EntityKeypair::generate();
        let result = PermissionToken::try_issue(
            &public_only,
            subject.entity_id().clone(),
            TokenScope::PUBLISH,
            0,
            3600,
            0,
        );
        assert!(
            matches!(result, Err(TokenError::ReadOnly)),
            "try_issue must surface public-only keypair as ReadOnly, got {:?}",
            result.map(|_| "Ok"),
        );
    }

    /// `delegate` likewise surfaces a public-only signer as
    /// `TokenError::ReadOnly`. The original `delegate` already
    /// returns `Result`, so no API change was needed — only the
    /// internal `sign` call was switched to `try_sign`.
    #[test]
    fn delegate_returns_read_only_on_public_only_signer() {
        let issuer = EntityKeypair::generate();
        let subject_full = EntityKeypair::generate();
        let target = EntityKeypair::generate();

        let parent = PermissionToken::issue(
            &issuer,
            subject_full.entity_id().clone(),
            TokenScope::PUBLISH.union(TokenScope::DELEGATE),
            0xCAFE,
            3600,
            3,
        );

        // Subject becomes public-only (post-migration zeroize).
        let subject_pub = EntityKeypair::public_only(subject_full.entity_id().clone());
        let result = parent.delegate(
            &subject_pub,
            target.entity_id().clone(),
            TokenScope::PUBLISH,
        );
        assert!(
            matches!(result, Err(TokenError::ReadOnly)),
            "delegate must surface public-only signer as ReadOnly, got {:?}",
            result.map(|_| "Ok"),
        );
    }

    /// `try_issue` succeeds with a full keypair — pins the success
    /// path so a future tightening doesn't accidentally over-reject.
    #[test]
    fn try_issue_succeeds_with_full_keypair() {
        let issuer = EntityKeypair::generate();
        let subject = EntityKeypair::generate();
        let token = PermissionToken::try_issue(
            &issuer,
            subject.entity_id().clone(),
            TokenScope::PUBLISH,
            0,
            3600,
            0,
        )
        .expect("try_issue must succeed with a full keypair");
        assert!(token.verify().is_ok());
    }
}
