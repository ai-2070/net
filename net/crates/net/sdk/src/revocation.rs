//! Machine-shared, persistent delegation-revocation floors (Hermes plan
//! Phase 3).
//!
//! The **provider** side of delegation revocation: a running `net wrap`
//! provider's `DelegationGate` (in the `net-mesh-mcp` adapter) honors these
//! floors, so an
//! operator can revoke a delegated gateway's (or machine's) access to their
//! wrapped tools **without restarting the provider**. A floor entry is
//! `issuer entity-id → generation`; any delegation link issued by that issuer
//! with `issuer_generation < floor` is rejected (the same
//! [`RevocationRegistry`] semantics the token layer uses, made persistent +
//! cross-process).
//!
//! Discipline mirrors the pin store: writes go through a cross-process advisory
//! lock on a stable sidecar and land via an atomic temp+rename, so a reader
//! never sees a torn file. **Reads are lock-free** — floors are *monotonic*
//! (only ever raised), and the atomic rename means a reader observes the old or
//! the new file, never a partial; a missed bump just means one more invoke
//! slips through before the next reload, never a resurrected access.
//!
//! This is **local provider state** — "which issuers I refuse to admit". It
//! composes with a future mesh-published root-revocation layer, which would
//! simply *write* into this same store.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use fs2::FileExt;
use serde::{Deserialize, Serialize};

use net::adapter::net::identity::{EntityId, RevocationRegistry};

/// The per-user default revocation-store path:
/// `<local data dir>/net-mesh/delegation-revocations.json`, falling back to
/// `<home>/…` — the single machine-shared file a provider and any revoke tool
/// converge on (bridge-SDK doctrine #1). `None` only when neither a data-local
/// nor a home directory resolves.
pub fn default_revocation_store_path() -> Option<PathBuf> {
    dirs::data_local_dir()
        .or_else(dirs::home_dir)
        .map(|d| d.join("net-mesh").join("delegation-revocations.json"))
}

/// Errors from the revocation store.
#[derive(Debug, thiserror::Error)]
pub enum RevocationStoreError {
    /// An I/O error touching the store or its lock.
    #[error("revocation store I/O at {path}: {reason}")]
    Io { path: String, reason: String },
    /// The store file exists but couldn't be parsed — surfaced (not silently
    /// treated as empty) so a typo never quietly drops revocations.
    #[error("revocation store at {path} is corrupt: {reason}")]
    Corrupt { path: String, reason: String },
}

#[derive(Serialize, Deserialize, Default)]
struct RevocationFile {
    #[serde(default)]
    floors: Vec<StoredFloor>,
    /// Revoked SUBJECTS (lowercase hex entity ids): any chain naming one as a
    /// link subject is refused. Omitted when empty, so a store without one
    /// stays byte-identical to the pre-existing format.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    revoked_subjects: Vec<String>,
}

#[derive(Serialize, Deserialize)]
struct StoredFloor {
    /// Issuer entity-id, lowercase hex (64 chars).
    issuer: String,
    generation: u32,
}

/// A snapshot of the machine-shared revocation floors (`issuer → generation`)
/// and revoked subjects.
///
/// The two answer different questions. A **floor** is per *issuer* and
/// generation-scoped: it kills links that issuer signed below a generation,
/// which is also how an entity rotates past its own outstanding grants and
/// keeps working at the new generation. It never touches a link issued *to*
/// that entity — so raising a device's floor left its own root-issued
/// `root → device` grant valid. A **revoked subject** is the "this device is
/// gone" record: any chain that names it as a link subject is refused,
/// whatever generation signed the link (SDK audit 2026-10-11).
#[derive(Debug, Clone, Default)]
pub struct RevocationStore {
    floors: BTreeMap<[u8; 32], u32>,
    subjects: BTreeSet<[u8; 32]>,
}

impl RevocationStore {
    /// Load the floors at `path`. A missing file is an **empty** store (the
    /// common case), not an error; a present-but-unparseable file is
    /// [`RevocationStoreError::Corrupt`]. Lock-free — safe against a concurrent
    /// atomic-rename writer.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, RevocationStoreError> {
        let path = path.as_ref();
        match std::fs::read(path) {
            Ok(bytes) => {
                let file: RevocationFile =
                    serde_json::from_slice(&bytes).map_err(|e| RevocationStoreError::Corrupt {
                        path: path.display().to_string(),
                        reason: e.to_string(),
                    })?;
                let mut floors = BTreeMap::new();
                for f in file.floors {
                    let key = decode_entity_hex(&f.issuer).map_err(|reason| {
                        RevocationStoreError::Corrupt {
                            path: path.display().to_string(),
                            reason,
                        }
                    })?;
                    // Keep the highest generation if a malformed file duplicates
                    // an issuer — floors only ever rise.
                    let e = floors.entry(key).or_insert(0);
                    if f.generation > *e {
                        *e = f.generation;
                    }
                }
                let mut subjects = BTreeSet::new();
                for hex in &file.revoked_subjects {
                    let key =
                        decode_entity_hex(hex).map_err(|reason| RevocationStoreError::Corrupt {
                            path: path.display().to_string(),
                            reason,
                        })?;
                    subjects.insert(key);
                }
                Ok(Self { floors, subjects })
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(RevocationStoreError::Io {
                path: path.display().to_string(),
                reason: e.to_string(),
            }),
        }
    }

    /// The floor for `issuer` (0 if never revoked).
    pub fn floor(&self, issuer: &EntityId) -> u32 {
        self.floors.get(issuer.as_bytes()).copied().unwrap_or(0)
    }

    /// Apply every stored floor to `registry` (monotonic `revoke_below`), so the
    /// registry a `DelegationGate` (in the `net-mesh-mcp` adapter) verifies
    /// against reflects the persisted revocations.
    pub fn apply_to(&self, registry: &RevocationRegistry) {
        for (bytes, gen) in &self.floors {
            registry.revoke_below(&EntityId::from_bytes(*bytes), *gen);
        }
    }

    /// Whether `subject` has been revoked as a subject (see the type docs).
    pub fn is_subject_revoked(&self, subject: &EntityId) -> bool {
        self.subjects.contains(subject.as_bytes())
    }

    /// The revoked subjects.
    pub fn revoked_subjects(&self) -> impl Iterator<Item = EntityId> + '_ {
        self.subjects.iter().map(|b| EntityId::from_bytes(*b))
    }

    /// Number of revoked issuers (testing/diagnostics).
    pub fn len(&self) -> usize {
        self.floors.len()
    }

    /// Whether the store has no revocations (no floors and no revoked
    /// subjects).
    pub fn is_empty(&self) -> bool {
        self.floors.is_empty() && self.subjects.is_empty()
    }

    /// Revoke `subject` outright: persist it to `path` (cross-process lock,
    /// atomic rename) so every chain naming it as a link subject is refused.
    /// Monotonic and idempotent; there is no un-revoke.
    pub fn revoke_subject(
        path: impl AsRef<Path>,
        subject: &EntityId,
    ) -> Result<(), RevocationStoreError> {
        Self::mutate(path.as_ref(), |store| {
            store.subjects.insert(*subject.as_bytes());
        })
    }

    /// Revoke: raise `issuer`'s floor to at least `generation`, persisting to
    /// `path` under a cross-process lock. Returns the new floor. Blocking — a
    /// revocation is a rare operator action, not a hot path, so a plain blocking
    /// lock is fine (no async-pool-starvation concern the pin store must handle).
    pub fn revoke_below(
        path: impl AsRef<Path>,
        issuer: &EntityId,
        generation: u32,
    ) -> Result<u32, RevocationStoreError> {
        let mut new_floor = 0;
        Self::mutate(path.as_ref(), |store| {
            let entry = store.floors.entry(*issuer.as_bytes()).or_insert(0);
            if generation > *entry {
                *entry = generation;
            }
            new_floor = *entry;
        })?;
        Ok(new_floor)
    }

    /// Load → `change` → save atomically, all under the cross-process lock.
    fn mutate(path: &Path, change: impl FnOnce(&mut Self)) -> Result<(), RevocationStoreError> {
        let io = |e: std::io::Error| RevocationStoreError::Io {
            path: path.display().to_string(),
            reason: e.to_string(),
        };

        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(io)?;
            }
        }

        // Lock a STABLE sidecar (not the store file, which the atomic rename
        // replaces) so two writers serialize. The lock releases when `_lock`
        // drops at the end of this function.
        let lock_path = sidecar_lock_path(path);
        let _lock = {
            let f = std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(false)
                .open(&lock_path)
                .map_err(io)?;
            f.lock_exclusive().map_err(io)?;
            f
        };

        let mut store = Self::load(path)?;
        change(&mut store);
        store.save_atomic(path)
    }

    /// Persist atomically: write a sibling temp file (owner-only on Unix), fsync,
    /// then rename it over the target — a reader sees the old or the new file,
    /// never a partial.
    fn save_atomic(&self, path: &Path) -> Result<(), RevocationStoreError> {
        let io = |e: std::io::Error| RevocationStoreError::Io {
            path: path.display().to_string(),
            reason: e.to_string(),
        };
        let file = RevocationFile {
            floors: self
                .floors
                .iter()
                .map(|(bytes, gen)| StoredFloor {
                    issuer: encode_entity_hex(bytes),
                    generation: *gen,
                })
                .collect(),
            revoked_subjects: self.subjects.iter().map(encode_entity_hex).collect(),
        };
        let bytes = serde_json::to_vec_pretty(&file).map_err(|e| RevocationStoreError::Io {
            path: path.display().to_string(),
            reason: format!("serialize revocation store: {e}"),
        })?;

        let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
        {
            use std::io::Write;
            let mut opts = std::fs::OpenOptions::new();
            opts.write(true).create(true).truncate(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                opts.mode(0o600);
            }
            let mut f = opts.open(&tmp).map_err(io)?;
            f.write_all(&bytes).map_err(io)?;
            f.flush().map_err(io)?;
            f.sync_all().map_err(io)?;
        }
        std::fs::rename(&tmp, path).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            io(e)
        })?;

        // Durability (POSIX): `rename()` only updates the directory entry in
        // memory; without fsyncing the parent directory a crash can revert to
        // the old file, silently losing the just-persisted revocation (BUG #93,
        // mirrors `redex/disk.rs`). Best-effort — a dir-fsync failure doesn't
        // undo the completed rename, and Windows needs no directory fsync.
        #[cfg(unix)]
        {
            let dir = match path.parent() {
                Some(p) if !p.as_os_str().is_empty() => p,
                _ => Path::new("."),
            };
            if let Ok(dirf) = std::fs::File::open(dir) {
                let _ = dirf.sync_all();
            }
        }
        Ok(())
    }
}

/// The stable lock sidecar next to `path` (`<path>.lock`), built from the raw
/// `OsString` so a non-UTF-8 path is preserved byte-for-byte.
fn sidecar_lock_path(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_os_string();
    s.push(".lock");
    PathBuf::from(s)
}

fn encode_entity_hex(bytes: &[u8; 32]) -> String {
    let mut s = String::with_capacity(64);
    for b in bytes {
        s.push(char::from_digit((b >> 4) as u32, 16).expect("nibble is 0..16"));
        s.push(char::from_digit((b & 0x0f) as u32, 16).expect("nibble is 0..16"));
    }
    s
}

fn decode_entity_hex(s: &str) -> Result<[u8; 32], String> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    if s.len() != 64 {
        return Err(format!("issuer must be 64 hex chars, got {}", s.len()));
    }
    let mut out = [0u8; 32];
    let bytes = s.as_bytes();
    for (i, slot) in out.iter_mut().enumerate() {
        let hi = (bytes[i * 2] as char)
            .to_digit(16)
            .ok_or_else(|| format!("issuer has non-hex char at {}", i * 2))?;
        let lo = (bytes[i * 2 + 1] as char)
            .to_digit(16)
            .ok_or_else(|| format!("issuer has non-hex char at {}", i * 2 + 1))?;
        *slot = ((hi << 4) | lo) as u8;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Identity;

    #[test]
    fn hex_round_trips() {
        let id = Identity::generate();
        let mut bytes = [0u8; 32];
        bytes.copy_from_slice(id.entity_id().as_bytes());
        let hexed = encode_entity_hex(&bytes);
        assert_eq!(hexed.len(), 64);
        assert_eq!(decode_entity_hex(&hexed).unwrap(), bytes);
        assert_eq!(decode_entity_hex(&format!("0x{hexed}")).unwrap(), bytes);
        assert!(decode_entity_hex("deadbeef").is_err());
    }

    #[test]
    fn revoke_persists_and_reloads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rev.json");
        let issuer = Identity::generate();

        // Missing file → empty.
        assert!(RevocationStore::load(&path).unwrap().is_empty());

        // Revoke, then a fresh load sees the floor (cross-process shape).
        let new = RevocationStore::revoke_below(&path, issuer.entity_id(), 1).unwrap();
        assert_eq!(new, 1);
        let reloaded = RevocationStore::load(&path).unwrap();
        assert_eq!(reloaded.floor(issuer.entity_id()), 1);
        assert_eq!(reloaded.len(), 1);
    }

    #[test]
    fn revoked_subjects_round_trip_and_keep_floors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rev.json");
        let a = Identity::generate();
        let b = Identity::generate();
        RevocationStore::revoke_below(&path, a.entity_id(), 3).unwrap();
        // A store with no revoked subjects keeps the original file shape.
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(!raw.contains("revoked_subjects"), "{raw}");

        RevocationStore::revoke_subject(&path, b.entity_id()).unwrap();
        RevocationStore::revoke_subject(&path, b.entity_id()).unwrap(); // idempotent
        let store = RevocationStore::load(&path).unwrap();
        assert!(store.is_subject_revoked(b.entity_id()));
        assert!(!store.is_subject_revoked(a.entity_id()));
        assert_eq!(
            store.floor(a.entity_id()),
            3,
            "floors survive the subject write"
        );
        assert_eq!(store.revoked_subjects().count(), 1);
        assert!(!store.is_empty());
    }

    #[test]
    fn revoke_below_is_monotonic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rev.json");
        let issuer = Identity::generate();

        assert_eq!(
            RevocationStore::revoke_below(&path, issuer.entity_id(), 5).unwrap(),
            5
        );
        // A lower generation is a no-op (never un-revokes).
        assert_eq!(
            RevocationStore::revoke_below(&path, issuer.entity_id(), 2).unwrap(),
            5
        );
        assert_eq!(
            RevocationStore::load(&path)
                .unwrap()
                .floor(issuer.entity_id()),
            5
        );
    }

    #[test]
    fn apply_to_bumps_a_registry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rev.json");
        let issuer = Identity::generate();
        RevocationStore::revoke_below(&path, issuer.entity_id(), 3).unwrap();

        let reg = RevocationRegistry::new();
        assert_eq!(reg.floor(issuer.entity_id()), 0);
        RevocationStore::load(&path).unwrap().apply_to(&reg);
        assert_eq!(reg.floor(issuer.entity_id()), 3);
    }
}
