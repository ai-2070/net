//! Durable issuer state: the signing seed plus its credential epoch.
//!
//! `PermissionToken::issuer_generation` is the signing identity's
//! epoch, and a verifier rejects a token once its
//! [`RevocationRegistry`](super::RevocationRegistry) floor for that
//! issuer exceeds it. That only works if the issuer can come back after
//! a restart still knowing which epoch it is on. A bare 32-byte seed
//! cannot carry that: restoring from one returns an issuer at
//! generation zero, which — for an issuer that has already published
//! floor `N` — can no longer mint anything a verifier will accept, and
//! has no way to discover `N`.
//!
//! So the persisted form is versioned and carries both:
//!
//! ```text
//! offset  size  field
//!      0     1  version (currently 1)
//!      1    32  ed25519 seed
//!     33     4  issuer generation, u32 little-endian
//! ```
//!
//! The codec lives in core rather than in any one SDK because every
//! binding persists the same bytes. A state file written by the Rust
//! SDK has to be readable by the Python one; two hand-rolled encoders
//! agreeing today is not the same as one encoder.
//!
//! **These bytes are secret material** — they contain the signing seed,
//! exactly as `Identity::to_bytes` does. Encrypt at rest, and write
//! atomically: a torn write here is an issuer that cannot come back.

/// Size of an encoded [`IdentityState`].
pub const IDENTITY_STATE_SIZE: usize = 1 + 32 + 4;

/// Version byte this build writes.
pub const IDENTITY_STATE_VERSION: u8 = 1;

/// What can go wrong decoding or rotating durable issuer state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentityStateError {
    /// Not [`IDENTITY_STATE_SIZE`] bytes.
    InvalidLength {
        /// What the caller passed.
        got: usize,
    },
    /// Written by a newer library than the one reading it.
    ///
    /// Refusing is the point. A partial parse of credential state is
    /// how an issuer silently comes back on the wrong epoch, and a
    /// wrong epoch is either a dead issuer or a retired credential
    /// walking again.
    UnsupportedVersion {
        /// The version byte found.
        found: u8,
    },
    /// A generation may only move forward. Going back would re-mint at
    /// an epoch the issuer has already retired.
    GenerationWentBackwards {
        /// The issuer's current generation.
        current: u32,
        /// What the caller asked for.
        requested: u32,
    },
    /// `u32::MAX` is the last usable generation. Further rotation means
    /// rotating the identity key itself.
    GenerationExhausted,
}

impl core::fmt::Display for IdentityStateError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::InvalidLength { got } => write!(
                f,
                "identity state must be {IDENTITY_STATE_SIZE} bytes, got {got}"
            ),
            Self::UnsupportedVersion { found } => write!(
                f,
                "identity state version {found} is newer than this library \
                 understands (expected {IDENTITY_STATE_VERSION})"
            ),
            Self::GenerationWentBackwards { current, requested } => write!(
                f,
                "issuer generation may only move forward: at {current}, \
                 asked for {requested}"
            ),
            Self::GenerationExhausted => write!(
                f,
                "issuer generation is exhausted at u32::MAX; rotate the \
                 identity key instead"
            ),
        }
    }
}

impl std::error::Error for IdentityStateError {}

/// A decoded issuer state: signing seed plus credential epoch.
#[derive(Clone, Copy)]
pub struct IdentityState {
    /// The 32-byte ed25519 seed. Secret.
    pub seed: [u8; 32],
    /// The issuer's credential epoch.
    pub generation: u32,
}

impl IdentityState {
    /// Encode to the versioned wire form.
    pub fn to_bytes(&self) -> [u8; IDENTITY_STATE_SIZE] {
        let mut out = [0u8; IDENTITY_STATE_SIZE];
        out[0] = IDENTITY_STATE_VERSION;
        out[1..33].copy_from_slice(&self.seed);
        out[33..37].copy_from_slice(&self.generation.to_le_bytes());
        out
    }

    /// Decode, rejecting anything this build does not fully understand.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, IdentityStateError> {
        if bytes.len() != IDENTITY_STATE_SIZE {
            return Err(IdentityStateError::InvalidLength { got: bytes.len() });
        }
        if bytes[0] != IDENTITY_STATE_VERSION {
            return Err(IdentityStateError::UnsupportedVersion { found: bytes[0] });
        }
        let mut seed = [0u8; 32];
        seed.copy_from_slice(&bytes[1..33]);
        Ok(Self {
            seed,
            generation: u32::from_le_bytes([bytes[33], bytes[34], bytes[35], bytes[36]]),
        })
    }

    /// The generation to rotate to, or why the move is refused.
    ///
    /// `next == current` is accepted and idempotent, so re-applying a
    /// persisted generation on restart is not an error. Going backwards
    /// is refused, and so is any move once `current` is `u32::MAX`.
    ///
    /// Shared by every SDK's `at_generation` so the rules do not have
    /// to be restated — and cannot be restated slightly differently —
    /// in five bindings.
    pub fn check_rotation(current: u32, next: u32) -> Result<u32, IdentityStateError> {
        if current == u32::MAX {
            return Err(IdentityStateError::GenerationExhausted);
        }
        if next < current {
            return Err(IdentityStateError::GenerationWentBackwards {
                current,
                requested: next,
            });
        }
        Ok(next)
    }
}

impl core::fmt::Debug for IdentityState {
    /// Never prints the seed. A `{:?}` on issuer state in a log line
    /// would put the signing key in the log.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("IdentityState")
            .field("seed", &"<redacted>")
            .field("generation", &self.generation)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_seed_and_generation() {
        let state = IdentityState {
            seed: [0xA5; 32],
            generation: 0xDEAD_BEEF,
        };
        let decoded = IdentityState::from_bytes(&state.to_bytes()).unwrap();
        assert_eq!(decoded.seed, state.seed);
        assert_eq!(decoded.generation, state.generation);
    }

    /// The layout is a cross-SDK contract; pin the bytes, not just the
    /// round trip. A round-trip test passes just as happily if every
    /// binding agrees on the *wrong* offsets.
    #[test]
    fn layout_is_pinned() {
        let bytes = IdentityState {
            seed: [0x11; 32],
            generation: 258,
        }
        .to_bytes();
        assert_eq!(bytes.len(), 37);
        assert_eq!(bytes[0], 1, "version");
        assert_eq!(&bytes[1..33], &[0x11u8; 32], "seed");
        assert_eq!(&bytes[33..37], &[0x02, 0x01, 0x00, 0x00], "u32 LE");
    }

    #[test]
    fn rejects_wrong_length_and_future_versions() {
        assert_eq!(
            IdentityState::from_bytes(&[]).unwrap_err(),
            IdentityStateError::InvalidLength { got: 0 }
        );
        // A bare seed is not identity state. Accepting it would
        // reintroduce the generation-zero trap through the versioned
        // door.
        assert_eq!(
            IdentityState::from_bytes(&[0u8; 32]).unwrap_err(),
            IdentityStateError::InvalidLength { got: 32 }
        );
        let mut future = IdentityState {
            seed: [0; 32],
            generation: 0,
        }
        .to_bytes();
        future[0] = 2;
        assert_eq!(
            IdentityState::from_bytes(&future).unwrap_err(),
            IdentityStateError::UnsupportedVersion { found: 2 }
        );
    }

    #[test]
    fn rotation_rules_are_monotonic_and_capped() {
        assert_eq!(IdentityState::check_rotation(3, 4).unwrap(), 4);
        assert_eq!(
            IdentityState::check_rotation(3, 3).unwrap(),
            3,
            "idempotent: re-applying a persisted generation is not an error"
        );
        assert_eq!(
            IdentityState::check_rotation(3, 2).unwrap_err(),
            IdentityStateError::GenerationWentBackwards {
                current: 3,
                requested: 2
            }
        );
        assert_eq!(
            IdentityState::check_rotation(u32::MAX, u32::MAX).unwrap_err(),
            IdentityStateError::GenerationExhausted
        );
    }

    #[test]
    fn debug_never_prints_the_seed() {
        let rendered = format!(
            "{:?}",
            IdentityState {
                seed: [0x7F; 32],
                generation: 1,
            }
        );
        assert!(rendered.contains("<redacted>"));
        assert!(!rendered.contains("127"), "no seed bytes in Debug output");
    }
}
