//! Durable accepted-floor log (NET_CLI_PLAN_V3 §6.1a, item 2).
//!
//! A verifier that forgets an accepted revocation floor on restart would
//! re-admit the credentials it revoked. This store keeps every floor fact
//! (subtree [`SubnetRevocationFloor`] and [`SubnetSubjectFloor`]) the
//! registry **accepted** — every fact that created its key or advanced its
//! revision watermark, whether or not it raised a generation (SUBNET-07) —
//! in acceptance order, as the exact signed wire bytes. Replaying the log
//! in that order through the same root-anchored verifiers reproduces the
//! registry exactly — per-right subject generations set by different
//! facts, and the revision watermarks that make older facts stale.
//!
//! # One owner for acceptance order and durable order (SUBNET-03)
//!
//! [`SubnetFloorStore::commit`] runs the registry acceptance and the log
//! append under ONE lock. Were they independent, two concurrent facts
//! could be accepted in one order and logged in the other, and a replay
//! would then reject the earlier-accepted fact as stale — losing an
//! acknowledged revocation on restart.
//!
//! # Failed writes are pending, not forgotten (SUBNET-04)
//!
//! The registry is updated first (floors only remove authority, so the
//! stricter in-memory state is kept even when the disk refuses). A fact
//! whose write fails stays PENDING: every later commit — including a retry
//! of the very same fact, which the registry now sees as unchanged —
//! re-attempts the write before reporting success, and
//! [`SubnetFloorStore::is_durable`] reports `false` until it lands. A
//! failed write surfaces as [`SubnetAuthError::StateNotPersisted`]. It
//! lives in [`EnrollmentStorage`]: one protected snapshot with atomic
//! replace and an exclusive owner lock.
//!
//! [`SubnetRevocationFloor`]: super::auth::SubnetRevocationFloor
//! [`SubnetSubjectFloor`]: super::auth::SubnetSubjectFloor

use std::path::Path;

use parking_lot::Mutex;

use super::auth::{FloorAcceptance, SubnetAuthError};
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
    /// Durably logged facts, in acceptance order.
    facts: Vec<Vec<u8>>,
    /// Accepted facts whose write has not landed yet, in acceptance
    /// order after `facts`, each with whether it changed enforceable
    /// state.
    pending: Vec<PendingFact>,
    /// An accepted fact could not even be queued (the log is at
    /// [`MAX_LOGGED_FLOORS`], or the fact is oversized), so the registry
    /// holds state no replay of this log reproduces. Sticky: the store
    /// never again claims to be durable.
    lost: bool,
    /// Test-only fault injection: refuse every snapshot write.
    #[cfg(any(test, feature = "fixtures"))]
    fail_writes: bool,
}

struct PendingFact {
    bytes: Vec<u8>,
    enforceable: bool,
}

/// What one [`SubnetFloorStore::commit`] did on the durable side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FloorCommit {
    /// What the registry acceptance reported.
    pub acceptance: FloorAcceptance,
    /// This exact fact was already accepted by an earlier call whose
    /// write failed and was still pending when this call began.
    pub was_pending: bool,
    /// That pending earlier acceptance changed enforceable state.
    pub was_pending_enforceable: bool,
    /// Every accepted fact — this one and all before it — is durably
    /// logged now.
    pub durable: bool,
}

impl std::fmt::Debug for SubnetFloorStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let inner = self.inner.lock();
        f.debug_struct("SubnetFloorStore")
            .field("facts", &inner.facts.len())
            .field("pending", &inner.pending.len())
            .field("lost", &inner.lost)
            .finish()
    }
}

impl Inner {
    /// Write `facts ++ pending` as one snapshot; on success the pending
    /// facts become logged. Returns whether everything accepted is now
    /// durable.
    fn flush(&mut self) -> bool {
        if !self.pending.is_empty() && self.write_allowed() {
            let mut all = self.facts.clone();
            all.extend(self.pending.iter().map(|p| p.bytes.clone()));
            if self.storage.replace(&encode(&all)).is_ok() {
                self.facts = all;
                self.pending.clear();
            }
        }
        self.pending.is_empty() && !self.lost
    }

    #[cfg(any(test, feature = "fixtures"))]
    fn write_allowed(&self) -> bool {
        !self.fail_writes
    }

    #[cfg(not(any(test, feature = "fixtures")))]
    fn write_allowed(&self) -> bool {
        true
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
                inner: Mutex::new(Inner {
                    storage,
                    facts,
                    pending: Vec::new(),
                    lost: false,
                    #[cfg(any(test, feature = "fixtures"))]
                    fail_writes: false,
                }),
            },
            replay,
        ))
    }

    /// Durably append one accepted floor fact's wire bytes. On failure
    /// nothing is logged and the caller must not report the change
    /// committed.
    ///
    /// Raw append, outside any registry acceptance: the mesh commits
    /// floors through [`Self::commit`], which is what keeps acceptance
    /// order and log order one order.
    pub fn append(&self, fact: &[u8]) -> Result<(), SubnetAuthError> {
        let mut inner = self.inner.lock();
        if inner.facts.len() + inner.pending.len() >= MAX_LOGGED_FLOORS
            || fact.len() > MAX_FACT_BYTES
            || !inner.write_allowed()
        {
            return Err(SubnetAuthError::StateNotPersisted);
        }
        let mut all = inner.facts.clone();
        all.extend(inner.pending.iter().map(|p| p.bytes.clone()));
        all.push(fact.to_vec());
        if inner.storage.replace(&encode(&all)).is_err() {
            return Err(SubnetAuthError::StateNotPersisted);
        }
        inner.facts = all;
        inner.pending.clear();
        Ok(())
    }

    /// Accept one floor fact into the registry and log it, as ONE
    /// ordered step (SUBNET-03).
    ///
    /// `accept` is the registry mutation for `fact`; it runs under this
    /// store's lock, so the order facts are accepted in IS the order they
    /// are logged in. An accepted fact is queued and the queue is written;
    /// a write that fails leaves the fact pending for the next commit to
    /// re-attempt (SUBNET-04), so a retry of the same fact — which the
    /// registry no longer sees as a change — still persists it.
    ///
    /// `accept`'s error is returned untouched, with nothing queued.
    /// Otherwise the caller must treat `!durable` as
    /// [`SubnetAuthError::StateNotPersisted`] for a fact that was
    /// accepted or was pending, and must still invalidate whatever an
    /// enforceable change covers — the in-memory registry already
    /// enforces it.
    pub fn commit(
        &self,
        fact: &[u8],
        accept: impl FnOnce() -> Result<FloorAcceptance, SubnetAuthError>,
    ) -> Result<FloorCommit, SubnetAuthError> {
        let mut inner = self.inner.lock();
        let acceptance = accept()?;
        let prior = inner.pending.iter().find(|p| p.bytes == fact);
        let was_pending = prior.is_some();
        let was_pending_enforceable = prior.is_some_and(|p| p.enforceable);
        if acceptance.accepted {
            if inner.facts.len() + inner.pending.len() >= MAX_LOGGED_FLOORS
                || fact.len() > MAX_FACT_BYTES
            {
                inner.lost = true;
            } else {
                inner.pending.push(PendingFact {
                    bytes: fact.to_vec(),
                    enforceable: acceptance.changed,
                });
            }
        }
        let durable = inner.flush();
        Ok(FloorCommit {
            acceptance,
            was_pending,
            was_pending_enforceable,
            durable,
        })
    }

    /// Whether every fact the registry accepted through [`Self::commit`]
    /// is durably logged — what a readback may attest as `persisted`.
    /// Takes the commit lock, so a commit in flight is either wholly
    /// before or wholly after the answer.
    pub fn is_durable(&self) -> bool {
        let inner = self.inner.lock();
        inner.pending.is_empty() && !inner.lost
    }

    /// Test-only fault injection: while set, every snapshot write fails
    /// as a refused disk would.
    #[cfg(any(test, feature = "fixtures"))]
    pub fn set_fail_writes(&self, fail: bool) {
        self.inner.lock().fail_writes = fail;
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

    const ENFORCING: FloorAcceptance = FloorAcceptance {
        accepted: true,
        changed: true,
    };
    const UNCHANGED: FloorAcceptance = FloorAcceptance {
        accepted: false,
        changed: false,
    };

    /// SUBNET-04: a fact whose write failed stays pending; retrying the
    /// SAME fact — which the registry now reports unchanged — persists
    /// it, and durability is reported honestly in between.
    #[test]
    fn a_failed_write_is_persisted_by_retrying_the_same_fact() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("floors");
        let (store, _) = SubnetFloorStore::open_or_create(&dir).unwrap();
        assert!(store.is_durable());

        store.set_fail_writes(true);
        let c = store.commit(&[4, 1], || Ok(ENFORCING)).unwrap();
        assert!(!c.durable, "the refused write is not durable");
        assert!(!c.was_pending);
        assert!(!store.is_durable(), "readback must not claim persisted");
        // Still failing: the retry is still not durable, and remembers
        // that the pending acceptance was enforceable.
        let c = store.commit(&[4, 1], || Ok(UNCHANGED)).unwrap();
        assert!(!c.durable && c.was_pending && c.was_pending_enforceable);

        store.set_fail_writes(false);
        let c = store.commit(&[4, 1], || Ok(UNCHANGED)).unwrap();
        assert!(c.durable, "the retry persists the pending fact");
        assert!(c.was_pending && c.was_pending_enforceable);
        assert!(store.is_durable());
        assert_eq!(store.len(), 1, "logged exactly once");
        drop(store);
        let (_, replay) = SubnetFloorStore::open_or_create(&dir).unwrap();
        assert_eq!(replay, vec![vec![4, 1]], "and it survives a restart");
    }

    /// SUBNET-04: an unrelated later commit also flushes the pending fact,
    /// ahead of itself — acceptance order is kept.
    #[test]
    fn a_later_commit_flushes_pending_facts_in_acceptance_order() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("floors");
        let (store, _) = SubnetFloorStore::open_or_create(&dir).unwrap();
        store.set_fail_writes(true);
        assert!(!store.commit(&[4, 1], || Ok(ENFORCING)).unwrap().durable);
        store.set_fail_writes(false);
        assert!(store.commit(&[5, 2], || Ok(ENFORCING)).unwrap().durable);
        drop(store);
        let (_, replay) = SubnetFloorStore::open_or_create(&dir).unwrap();
        assert_eq!(replay, vec![vec![4, 1], vec![5, 2]]);
    }

    /// SUBNET-03: registry acceptance runs under the commit lock, so no
    /// other commit can be accepted — or logged — between one fact's
    /// acceptance and its log append. Log order is acceptance order.
    #[test]
    fn acceptance_and_log_append_are_one_ordered_step() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("floors");
        let (store, _) = SubnetFloorStore::open_or_create(&dir).unwrap();
        let store = Arc::new(store);
        let second_accepted = Arc::new(AtomicBool::new(false));

        let (s, flag) = (store.clone(), second_accepted.clone());
        let mut second = None;
        store
            .commit(&[4, 1], || {
                // Revision 1 is accepted; a concurrent revision 2 now
                // races to be accepted and logged first.
                second = Some(std::thread::spawn(move || {
                    s.commit(&[5, 2], || {
                        flag.store(true, Ordering::SeqCst);
                        Ok(ENFORCING)
                    })
                    .unwrap()
                }));
                std::thread::sleep(std::time::Duration::from_millis(200));
                assert!(
                    !second_accepted.load(Ordering::SeqCst),
                    "a second acceptance ran inside the first commit"
                );
                Ok(ENFORCING)
            })
            .unwrap();
        second.unwrap().join().unwrap();
        drop(store);
        let (_, replay) = SubnetFloorStore::open_or_create(&dir).unwrap();
        assert_eq!(
            replay,
            vec![vec![4, 1], vec![5, 2]],
            "logged in the order accepted"
        );
    }

    /// A refused acceptance queues nothing.
    #[test]
    fn a_refused_acceptance_logs_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let (store, _) = SubnetFloorStore::open_or_create(&tmp.path().join("floors")).unwrap();
        assert_eq!(
            store.commit(&[4, 1], || Err(SubnetAuthError::IssuerNotAuthorized)),
            Err(SubnetAuthError::IssuerNotAuthorized)
        );
        assert!(store.is_durable() && store.is_empty());
    }
}
