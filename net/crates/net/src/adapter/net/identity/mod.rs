//! Layer 1: Trust & Identity for Net.
//!
//! This module provides cryptographic identity, origin binding, and
//! permission tokens for the mesh. All identifiers (node_id, origin_hash)
//! are derived from ed25519 public keys.

mod entity;
mod envelope;
mod origin;
mod token;

pub use entity::{EntityError, EntityId, EntityKeypair};
pub use envelope::{
    EnvelopeError, IdentityEnvelope, IDENTITY_ENVELOPE_SIZE, IDENTITY_ENVELOPE_VERSION,
};
pub use origin::OriginStamp;
pub use token::{
    PermissionToken, RevocationRegistry, TokenCache, TokenError, TokenScope, MAX_TOKENS_PER_SLOT,
    MAX_TOKEN_CLOCK_SKEW_SECS, MAX_TOKEN_SLOTS, MAX_TOKEN_TTL_SECS,
    TOKEN_CLOCK_SKEW_SECS_RECOMMENDED,
};
