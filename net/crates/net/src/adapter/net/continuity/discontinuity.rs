//! Honest discontinuity — chain breaks and entity forks.
//!
//! When a chain breaks (node crash, data loss, corruption), the system
//! creates a new entity with documented lineage via a `ForkRecord`.
//! The mesh knows the original entity is discontinued and a fork has
//! taken its place. No silent recovery — only honest forking.

use xxhash_rust::xxh3::xxh3_64;

use crate::adapter::net::identity::EntityKeypair;
use crate::adapter::net::state::causal::{
    CausalChainBuilder, CausalLink, ChainError, CAUSAL_LINK_SIZE,
};

/// A detected discontinuity in an entity's causal chain.
#[derive(Debug, Clone)]
pub struct Discontinuity {
    /// Entity whose chain broke.
    pub origin_hash: u64,
    /// Last verified event in the original chain.
    pub last_verified: CausalLink,
    /// The event that could not be linked (if available).
    pub failed_link: Option<CausalLink>,
    /// Why the chain broke.
    pub reason: DiscontinuityReason,
    /// When the break was detected (local nanos).
    pub detected_at: u64,
}

/// Why a chain broke.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiscontinuityReason {
    /// Node crashed, state lost between snapshot and head.
    NodeCrash {
        /// Last snapshot sequence before the crash.
        last_snapshot_seq: u64,
    },
    /// Chain validation failed.
    ChainBreak(ChainError),
    /// Conflicting chains from same origin (split brain).
    ConflictingChains {
        /// Sequence where conflict was detected.
        seq: u64,
        /// Hash from chain A.
        hash_a: u64,
        /// Hash from chain B.
        hash_b: u64,
    },
    /// Data corruption detected.
    Corruption,
}

impl std::fmt::Display for DiscontinuityReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NodeCrash { last_snapshot_seq } => {
                write!(f, "node crash (last snapshot at seq {})", last_snapshot_seq)
            }
            Self::ChainBreak(e) => write!(f, "chain break: {}", e),
            Self::ConflictingChains {
                seq,
                hash_a,
                hash_b,
            } => {
                write!(
                    f,
                    "conflicting chains at seq {}: {:#x} vs {:#x}",
                    seq, hash_a, hash_b
                )
            }
            Self::Corruption => write!(f, "data corruption"),
        }
    }
}

/// Record of an entity fork — created when a chain breaks.
///
/// The forked entity gets a new keypair and a new chain. The fork genesis
/// link has a deterministic sentinel `parent_hash` so any node can verify
/// the fork is legitimate.
#[derive(Debug, Clone)]
pub struct ForkRecord {
    /// The original entity's origin hash.
    pub original_origin: u64,
    /// The forked entity's origin hash (new keypair).
    pub forked_origin: u64,
    /// Sequence at which the fork occurred.
    pub fork_seq: u64,
    /// The forked entity's genesis link.
    pub fork_genesis: CausalLink,
    /// Snapshot sequence used to seed the fork (if any).
    pub from_snapshot_seq: Option<u64>,
}

/// Compute the deterministic fork sentinel hash.
///
/// `parent_hash = xxh3(original_origin ++ fork_seq ++ "fork")`
///
/// Any node can verify a fork record by recomputing this sentinel.
pub fn fork_sentinel(original_origin: u64, fork_seq: u64) -> u64 {
    let mut buf = Vec::with_capacity(8 + 8 + 4);
    buf.extend_from_slice(&original_origin.to_le_bytes());
    buf.extend_from_slice(&fork_seq.to_le_bytes());
    buf.extend_from_slice(b"fork");
    xxh3_64(&buf)
}

/// Create a forked entity from a discontinuity.
///
/// Generates a new keypair for the forked entity and creates a genesis
/// link with the deterministic fork sentinel as parent_hash.
///
/// Returns the new keypair, fork record, and chain builder ready to
/// produce events.
pub fn fork_entity(
    original_origin: u64,
    fork_seq: u64,
    from_snapshot_seq: Option<u64>,
) -> (EntityKeypair, ForkRecord, CausalChainBuilder) {
    let new_keypair = EntityKeypair::generate();
    let new_origin = new_keypair.origin_hash();

    // Fork genesis has the sentinel as parent_hash
    let sentinel = fork_sentinel(original_origin, fork_seq);
    let fork_genesis = CausalLink {
        origin_hash: new_origin,
        horizon_encoded: 0,
        sequence: 0,
        parent_hash: sentinel,
    };

    let record = ForkRecord {
        original_origin,
        forked_origin: new_origin,
        fork_seq,
        fork_genesis,
        from_snapshot_seq,
    };

    // Chain builder starts from the fork genesis
    let builder = CausalChainBuilder::from_head(fork_genesis, bytes::Bytes::new());

    (new_keypair, record, builder)
}

impl ForkRecord {
    /// Verify that this fork record is structurally valid.
    ///
    /// Checks:
    /// - Sentinel parent_hash matches the deterministic fork hash
    /// - Fork genesis origin matches forked_origin
    /// - Fork genesis sequence is 0 (genesis)
    /// - Original and forked origins differ
    pub fn verify(&self) -> bool {
        let expected = fork_sentinel(self.original_origin, self.fork_seq);
        self.fork_genesis.parent_hash == expected
            && self.fork_genesis.origin_hash == self.forked_origin
            && self.fork_genesis.sequence == 0
            && self.original_origin != self.forked_origin
    }

    /// Wire size: 8 + 8 + 8 + CAUSAL_LINK_SIZE + 1 + 8 bytes.
    pub const WIRE_SIZE: usize = 8 + 8 + 8 + CAUSAL_LINK_SIZE + 1 + 8;

    /// Serialize to bytes.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(Self::WIRE_SIZE);
        buf.extend_from_slice(&self.original_origin.to_le_bytes());
        buf.extend_from_slice(&self.forked_origin.to_le_bytes());
        buf.extend_from_slice(&self.fork_seq.to_le_bytes());
        buf.extend_from_slice(&self.fork_genesis.to_bytes());
        match self.from_snapshot_seq {
            Some(seq) => {
                buf.push(1);
                buf.extend_from_slice(&seq.to_le_bytes());
            }
            None => {
                buf.push(0);
                buf.extend_from_slice(&0u64.to_le_bytes());
            }
        }
        buf
    }

    /// Deserialize from bytes.
    ///
    /// Rejects buffers whose length is anything other than exactly
    /// [`Self::WIRE_SIZE`]. Silently accepting trailing bytes
    /// weakened the wire-format contract: two concatenated
    /// `ForkRecord`s could parse as the first one alone, and
    /// callers framing this type inside a larger payload could
    /// smuggle extra bytes past the parser.
    ///
    /// Also runs `verify()` before returning, so a
    /// structurally-bogus record (sentinel mismatch, origin collision,
    /// etc.) cannot reach a caller that forgot to call `verify`
    /// themselves. Callers that need the raw bytes-only parse for
    /// diagnostics can use [`Self::from_bytes_unchecked`].
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        let parsed = Self::from_bytes_unchecked(data)?;
        if !parsed.verify() {
            return None;
        }
        Some(parsed)
    }

    /// Deserialize without `verify()`. Useful for diagnostics
    /// (e.g. inspecting a malformed record before deciding what
    /// to do); production callers should prefer [`Self::from_bytes`].
    pub fn from_bytes_unchecked(data: &[u8]) -> Option<Self> {
        if data.len() != Self::WIRE_SIZE {
            return None;
        }
        let original_origin = u64::from_le_bytes(data[0..8].try_into().unwrap());
        let forked_origin = u64::from_le_bytes(data[8..16].try_into().unwrap());
        let fork_seq = u64::from_le_bytes(data[16..24].try_into().unwrap());
        // Field offsets are computed from the prefix lengths above
        // plus `CAUSAL_LINK_SIZE` so a future wire-size change to
        // the causal link is picked up here automatically. Pre-#130
        // these were hand-encoded (16..40 for the link, 40 for the
        // snapshot flag, 41..49 for the snapshot seq) and silently
        // mis-parsed when the link width changed.
        let link_end = 24 + CAUSAL_LINK_SIZE;
        let fork_genesis = CausalLink::from_bytes(&data[24..link_end])?;
        let has_snapshot = data[link_end] != 0;
        let snapshot_seq = u64::from_le_bytes(data[link_end + 1..link_end + 9].try_into().unwrap());
        let from_snapshot_seq = if has_snapshot {
            Some(snapshot_seq)
        } else {
            None
        };

        Some(Self {
            original_origin,
            forked_origin,
            fork_seq,
            fork_genesis,
            from_snapshot_seq,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fork_sentinel_deterministic() {
        let s1 = fork_sentinel(0xAAAA, 42);
        let s2 = fork_sentinel(0xAAAA, 42);
        assert_eq!(s1, s2);
        assert_ne!(s1, 0);
    }

    #[test]
    fn test_fork_sentinel_differs() {
        let s1 = fork_sentinel(0xAAAA, 42);
        let s2 = fork_sentinel(0xBBBB, 42);
        let s3 = fork_sentinel(0xAAAA, 43);
        assert_ne!(s1, s2);
        assert_ne!(s1, s3);
    }

    #[test]
    fn test_fork_entity() {
        let (keypair, record, builder) = fork_entity(0xAAAA, 100, Some(90));

        assert_eq!(record.original_origin, 0xAAAA);
        assert_eq!(record.forked_origin, keypair.origin_hash());
        assert_eq!(record.fork_seq, 100);
        assert_eq!(record.from_snapshot_seq, Some(90));
        assert!(record.verify());

        // Builder should be ready to produce events
        assert_eq!(builder.origin_hash(), keypair.origin_hash());
    }

    #[test]
    fn test_fork_record_roundtrip() {
        let (_, record, _) = fork_entity(0xDEAD, 500, None);

        let bytes = record.to_bytes();
        assert_eq!(bytes.len(), ForkRecord::WIRE_SIZE);

        let parsed = ForkRecord::from_bytes(&bytes).unwrap();
        assert_eq!(parsed.original_origin, record.original_origin);
        assert_eq!(parsed.forked_origin, record.forked_origin);
        assert_eq!(parsed.fork_seq, record.fork_seq);
        assert_eq!(parsed.fork_genesis, record.fork_genesis);
        assert_eq!(parsed.from_snapshot_seq, None);
        assert!(parsed.verify());
    }

    #[test]
    fn test_fork_record_with_snapshot_seq() {
        let (_, record, _) = fork_entity(0xBEEF, 200, Some(150));

        let bytes = record.to_bytes();
        let parsed = ForkRecord::from_bytes(&bytes).unwrap();
        assert_eq!(parsed.from_snapshot_seq, Some(150));
    }

    #[test]
    fn test_tampered_sentinel_fails_verification() {
        let (_, mut record, _) = fork_entity(0xAAAA, 100, None);
        record.fork_genesis.parent_hash = 0xBADBADBAD;
        assert!(!record.verify());
    }

    /// Regression: BUG_REPORT.md #42 — `from_bytes` previously
    /// returned `Some(record)` for any bytes that *parsed* the
    /// wire format, even if `verify()` would have rejected the
    /// record (e.g. tampered sentinel, origin collision). The
    /// caller had to remember to call `verify()` themselves; if
    /// they didn't, an attacker could ship a structurally-bogus
    /// fork record and the consumer would treat it as valid.
    /// The fix calls `verify()` inside `from_bytes`. The
    /// raw-parse path is preserved as `from_bytes_unchecked` for
    /// diagnostic uses.
    #[test]
    fn from_bytes_runs_verify() {
        // Build a valid fork record, tamper its sentinel, and
        // serialize. `from_bytes` must reject; `from_bytes_unchecked`
        // must succeed (returning the bogus record so a diagnostic
        // tool can inspect it).
        let (_, mut record, _) = fork_entity(0xCAFE, 7, None);
        // Tamper: set the parent_hash to something that won't
        // match the fork sentinel.
        record.fork_genesis.parent_hash = 0xDEAD_BEEF;
        let bytes = record.to_bytes();

        assert!(
            ForkRecord::from_bytes(&bytes).is_none(),
            "from_bytes must reject a record whose sentinel \
             doesn't verify (#42)"
        );
        assert!(
            ForkRecord::from_bytes_unchecked(&bytes).is_some(),
            "from_bytes_unchecked still parses the raw bytes for \
             diagnostic use"
        );
    }
}
