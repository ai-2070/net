//! Durable accepted-floor log (NET_CLI_PLAN_V3 §6.1a, item 2).
//!
//! A verifier that forgets an accepted revocation floor on restart would
//! re-admit the credentials it revoked. This store keeps every floor fact
//! (subtree [`SubnetRevocationFloor`] and [`SubnetSubjectFloor`]) that
//! **changed** enforceable state, in acceptance order, as the exact signed
//! wire bytes. Replaying the log in that order through the same
//! root-anchored verifiers reproduces the registry exactly — including
//! per-right subject generations set by different facts, which a
//! "latest fact per key" store would lose.
//!
//! The log is written before the apply that changed state reports success;
//! a failed write surfaces as [`SubnetAuthError::StateNotPersisted`] (the
//! in-memory registry keeps the stricter state — floors only remove
//! authority). It lives in [`EnrollmentStorage`]: one protected snapshot
//! with atomic replace and an exclusive owner lock.
//!
//! [`SubnetRevocationFloor`]: super::auth::SubnetRevocationFloor
//! [`SubnetSubjectFloor`]: super::auth::SubnetSubjectFloor

use std::path::Path;

use parking_lot::Mutex;

use super::auth::SubnetAuthError;
use crate::adapter::net::behavior::enrollment_storage::{EnrollmentStorage, StorageError};

const MAGIC: [u8; 4] = *b"NMSF";
const VERSION: u16 = 1;
const CHECKSUM_CONTEXT: &str = "net-mesh subnet floor store v1";
/// Upper bound on logged floor facts. Floors are rare operator actions;
/// a full log refuses further persistence rather than growing unbounded.
pub const MAX_LOGGED_FLOORS: usize = 65_536;
/// Largest single logged fact (tag + the larger floor artifact).
const MAX_FACT_BYTES: usize = 1 + 190;

/// Why the floor store could not be opened.
#[derive(Debug, thiserror::Error)]
pub enum FloorStoreError {
    /// The protected storage refused (busy, insecure, I/O).
    #[error("subnet floor store: {0}")]
    Storage(#[from] StorageError),
    /// The snapshot failed integrity or structure checks.
    #[error("subnet floor store is corrupt")]
    Corrupt,
}

/// The node-owned durable floor log.
pub struct SubnetFloorStore {
    inner: Mutex<Inner>,
}

struct Inner {
    storage: EnrollmentStorage,
    facts: Vec<Vec<u8>>,
}

impl std::fmt::Debug for SubnetFloorStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SubnetFloorStore")
            .field("facts", &self.inner.lock().facts.len())
            .finish()
    }
}

impl SubnetFloorStore {
    /// Open the store at `dir` (creating an empty one if absent) and
    /// return it with the logged facts, in acceptance order, for the
    /// caller to replay before any admission.
    pub fn open_or_create(dir: &Path) -> Result<(Self, Vec<Vec<u8>>), FloorStoreError> {
        let (storage, facts) = if dir.exists() {
            let storage = EnrollmentStorage::open(dir)?;
            let facts = decode(&storage.read()?)?;
            (storage, facts)
        } else {
            (EnrollmentStorage::create(dir, &encode(&[]))?, Vec::new())
        };
        let replay = facts.clone();
        Ok((
            Self {
                inner: Mutex::new(Inner { storage, facts }),
            },
            replay,
        ))
    }

    /// Durably append one accepted floor fact's wire bytes. On failure
    /// nothing is logged and the caller must not report the change
    /// committed.
    pub fn append(&self, fact: &[u8]) -> Result<(), SubnetAuthError> {
        let mut inner = self.inner.lock();
        if inner.facts.len() >= MAX_LOGGED_FLOORS || fact.len() > MAX_FACT_BYTES {
            return Err(SubnetAuthError::StateNotPersisted);
        }
        inner.facts.push(fact.to_vec());
        let snapshot = encode(&inner.facts);
        if inner.storage.replace(&snapshot).is_err() {
            inner.facts.pop();
            return Err(SubnetAuthError::StateNotPersisted);
        }
        Ok(())
    }

    /// Number of logged facts.
    pub fn len(&self) -> usize {
        self.inner.lock().facts.len()
    }

    /// Whether nothing is logged.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

// MAGIC | u16 VERSION | u32 count | (u16 len | fact)* | blake3 checksum[32]
fn encode(facts: &[Vec<u8>]) -> Vec<u8> {
    let mut out = Vec::with_capacity(10 + facts.len() * (2 + MAX_FACT_BYTES) + 32);
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&(facts.len() as u32).to_le_bytes());
    for fact in facts {
        out.extend_from_slice(&(fact.len() as u16).to_le_bytes());
        out.extend_from_slice(fact);
    }
    let sum = blake3::derive_key(CHECKSUM_CONTEXT, &out);
    out.extend_from_slice(&sum);
    out
}

fn decode(bytes: &[u8]) -> Result<Vec<Vec<u8>>, FloorStoreError> {
    let body_len = bytes
        .len()
        .checked_sub(32)
        .ok_or(FloorStoreError::Corrupt)?;
    let (body, sum) = bytes.split_at(body_len);
    if blake3::derive_key(CHECKSUM_CONTEXT, body) != sum {
        return Err(FloorStoreError::Corrupt);
    }
    let take = |off: &mut usize, n: usize| -> Result<&[u8], FloorStoreError> {
        let end = off.checked_add(n).ok_or(FloorStoreError::Corrupt)?;
        let s = body.get(*off..end).ok_or(FloorStoreError::Corrupt)?;
        *off = end;
        Ok(s)
    };
    let mut off = 0;
    if take(&mut off, 4)? != MAGIC {
        return Err(FloorStoreError::Corrupt);
    }
    let version = take(&mut off, 2)?;
    if u16::from_le_bytes([version[0], version[1]]) != VERSION {
        return Err(FloorStoreError::Corrupt);
    }
    let count = take(&mut off, 4)?;
    let count = u32::from_le_bytes([count[0], count[1], count[2], count[3]]) as usize;
    if count > MAX_LOGGED_FLOORS {
        return Err(FloorStoreError::Corrupt);
    }
    let mut facts = Vec::with_capacity(count);
    for _ in 0..count {
        let len = take(&mut off, 2)?;
        let len = u16::from_le_bytes([len[0], len[1]]) as usize;
        if len == 0 || len > MAX_FACT_BYTES {
            return Err(FloorStoreError::Corrupt);
        }
        facts.push(take(&mut off, len)?.to_vec());
    }
    if off != body.len() {
        return Err(FloorStoreError::Corrupt);
    }
    Ok(facts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_log_round_trips_in_order_and_refuses_tampering() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("floors");
        let (store, replay) = SubnetFloorStore::open_or_create(&dir).unwrap();
        assert!(replay.is_empty());
        store.append(&[4, 1, 2, 3]).unwrap();
        store.append(&[5, 9, 9]).unwrap();
        drop(store);
        let (store, replay) = SubnetFloorStore::open_or_create(&dir).unwrap();
        assert_eq!(replay, vec![vec![4, 1, 2, 3], vec![5, 9, 9]]);
        assert_eq!(store.len(), 2);
        assert!(
            store.append(&[0u8; MAX_FACT_BYTES + 1]).is_err(),
            "oversized facts are refused, not truncated"
        );
        drop(store);

        let mut bad = encode(&[vec![4, 1]]);
        bad[12] ^= 1;
        assert!(matches!(decode(&bad), Err(FloorStoreError::Corrupt)));
        assert!(matches!(decode(&[1, 2, 3]), Err(FloorStoreError::Corrupt)));
    }
}
