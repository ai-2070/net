//! Layer 1: Trust & Identity for Net.
//!
//! This module provides cryptographic identity, origin binding, and
//! permission tokens for the mesh. All identifiers (node_id, origin_hash)
//! are derived from ed25519 public keys.

mod entity;
mod envelope;
mod origin;
mod proof;
mod state;
mod token;

pub use entity::{EntityError, EntityId, EntityKeypair};
pub use envelope::{
    EnvelopeError, IdentityEnvelope, IDENTITY_ENVELOPE_SIZE, IDENTITY_ENVELOPE_VERSION,
};
pub use origin::OriginStamp;
pub use proof::{
    decode as decode_identity_proof, encode as encode_identity_proof, proof_transcript,
    verify_proof, IdentityChallengeStore, IdentityProofCodecError, IdentityProofMsg,
    IdentityProofReject, IDENTITY_CHALLENGE_TTL, MAX_IDENTITY_CHALLENGES_PER_PEER,
    MAX_IDENTITY_CHALLENGE_PEERS, SUBPROTOCOL_IDENTITY_PROOF, TRANSCRIPT_LEN,
};
pub use state::{IdentityState, IdentityStateError, IDENTITY_STATE_SIZE, IDENTITY_STATE_VERSION};
pub use token::{
    PermissionToken, RevocationRegistry, TokenCache, TokenChain, TokenError, TokenScope,
    MAX_CHAIN_DEPTH, MAX_TOKENS_PER_SLOT, MAX_TOKEN_CLOCK_SKEW_SECS, MAX_TOKEN_SLOTS,
    MAX_TOKEN_TTL_SECS, TOKEN_CLOCK_SKEW_SECS_RECOMMENDED,
};
