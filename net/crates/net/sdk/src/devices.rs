//! Machine-shared device registry — the operator's inventory of enrolled
//! devices, backing `mesh.devices()` (Hermes V2 Phase 1).
//!
//! Enrollment ([`crate::enrollment`]) mints a `root → device` delegation; this
//! module is the operator-side *inventory* of who was admitted: each device's
//! entity id, its chosen name + tags, when it enrolled, and whether it's been
//! revoked. It's the display/management surface (`mesh.devices()`,
//! `mesh.revoke`), **not** the enforcement surface.
//!
//! # Inventory, not authority
//!
//! Access enforcement lives in [`crate::revocation::RevocationStore`] (the
//! floors a running `net wrap` provider honors). This registry's `revoked_at`
//! is *metadata for display* — `mesh.revoke(device)` bumps the device's floor
//! in the `RevocationStore` (which actually kills access) **and** stamps
//! `revoked_at` here (so the device list shows it). The two are orthogonal on
//! purpose: an operator could prune a machine from their inventory without
//! touching floors, or revoke a floor for a device never recorded here.
//!
//! # Discipline
//!
//! Mirrors the pin store / revocation store: writes take a cross-process
//! advisory lock on a stable sidecar and land via an atomic temp+rename, so a
//! reader never sees a torn file; reads are lock-free (a reader observes the
//! old or the new file, never a partial). Names / tags / entity ids are not
//! secret — no key material is ever stored here (H8).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use fs2::FileExt;
use serde::{Deserialize, Serialize};

use crate::identity::EntityId;

/// The per-user default device-registry path:
/// `<local data dir>/net-mesh/devices.json`, falling back to `<home>/…` — the
/// single machine-shared file the mesh-management surfaces converge on (bridge
/// SDK doctrine #1). `None` only when neither a data-local nor a home directory
/// resolves.
pub fn default_device_registry_path() -> Option<PathBuf> {
    dirs::data_local_dir()
        .or_else(dirs::home_dir)
        .map(|d| d.join("net-mesh").join("devices.json"))
}

/// Errors from the device registry.
#[derive(Debug, thiserror::Error)]
pub enum DeviceRegistryError {
    /// An I/O error touching the registry or its lock.
    #[error("device registry I/O at {path}: {reason}")]
    Io { path: String, reason: String },
    /// The registry file exists but couldn't be parsed — surfaced (not silently
    /// treated as empty) so a typo never quietly drops the inventory.
    #[error("device registry at {path} is corrupt: {reason}")]
    Corrupt { path: String, reason: String },
}

/// One enrolled device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceRecord {
    /// The device's entity id (its ed25519 public key).
    pub device: EntityId,
    /// The device's chosen name (`pc`, `mac`, …).
    pub name: String,
    /// The device's routing/labeling tags (`region:office`, `gpu:true`).
    pub tags: Vec<String>,
    /// Unix-seconds the device was enrolled.
    pub enrolled_at: u64,
    /// Unix-seconds the device was revoked, or `None` while active. Display
    /// metadata — enforcement is the [`crate::revocation::RevocationStore`]
    /// floor.
    pub revoked_at: Option<u64>,
}

impl DeviceRecord {
    /// A freshly-enrolled (active) device record.
    pub fn new(
        device: EntityId,
        name: impl Into<String>,
        tags: Vec<String>,
        enrolled_at: u64,
    ) -> Self {
        Self {
            device,
            name: name.into(),
            tags,
            enrolled_at,
            revoked_at: None,
        }
    }

    /// Whether the device is marked revoked in the inventory.
    pub fn is_revoked(&self) -> bool {
        self.revoked_at.is_some()
    }
}

/// A snapshot of the machine-shared device inventory.
#[derive(Debug, Clone, Default)]
pub struct DeviceRegistry {
    devices: BTreeMap<[u8; 32], DeviceRecord>,
}

impl DeviceRegistry {
    /// Load the registry at `path`. A missing file is an **empty** registry
    /// (the common case), not an error; a present-but-unparseable file is
    /// [`DeviceRegistryError::Corrupt`]. Lock-free — safe against a concurrent
    /// atomic-rename writer.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, DeviceRegistryError> {
        let path = path.as_ref();
        match std::fs::read(path) {
            Ok(bytes) => {
                let file: DeviceFile =
                    serde_json::from_slice(&bytes).map_err(|e| DeviceRegistryError::Corrupt {
                        path: path.display().to_string(),
                        reason: e.to_string(),
                    })?;
                let mut devices = BTreeMap::new();
                for d in file.devices {
                    let key = decode_entity_hex(&d.device).map_err(|reason| {
                        DeviceRegistryError::Corrupt {
                            path: path.display().to_string(),
                            reason,
                        }
                    })?;
                    devices.insert(
                        key,
                        DeviceRecord {
                            device: EntityId::from_bytes(key),
                            name: d.name,
                            tags: d.tags,
                            enrolled_at: d.enrolled_at,
                            revoked_at: d.revoked_at,
                        },
                    );
                }
                Ok(Self { devices })
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(DeviceRegistryError::Io {
                path: path.display().to_string(),
                reason: e.to_string(),
            }),
        }
    }

    /// All devices, ordered by entity id.
    pub fn list(&self) -> Vec<&DeviceRecord> {
        self.devices.values().collect()
    }

    /// The record for `device`, if enrolled.
    pub fn get(&self, device: &EntityId) -> Option<&DeviceRecord> {
        self.devices.get(device.as_bytes())
    }

    /// Number of enrolled devices (including revoked-but-not-pruned).
    pub fn len(&self) -> usize {
        self.devices.len()
    }

    /// Whether the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.devices.is_empty()
    }

    /// Record (upsert) an enrolled device, persisting to `path` under a
    /// cross-process lock. A re-enroll of an existing device replaces its
    /// record (new name/tags/enrolled_at, active again). Blocking — enrollment
    /// is a rare operator action, not a hot path.
    pub fn record(path: impl AsRef<Path>, record: DeviceRecord) -> Result<(), DeviceRegistryError> {
        with_locked_store(path.as_ref(), |store| {
            store.devices.insert(*record.device.as_bytes(), record);
        })
    }

    /// Mark `device` revoked at `now` (display metadata; enforcement is the
    /// [`crate::revocation::RevocationStore`] floor). Returns whether a matching
    /// record existed. Persisted under a cross-process lock.
    pub fn mark_revoked(
        path: impl AsRef<Path>,
        device: &EntityId,
        now: u64,
    ) -> Result<bool, DeviceRegistryError> {
        let mut found = false;
        with_locked_store(path.as_ref(), |store| {
            if let Some(rec) = store.devices.get_mut(device.as_bytes()) {
                rec.revoked_at = Some(now);
                found = true;
            }
        })?;
        Ok(found)
    }

    /// Remove `device` from the inventory entirely, persisting under a lock.
    /// Returns whether a record existed. (Pruning inventory is orthogonal to
    /// revoking a floor — see the module docs.)
    pub fn remove(path: impl AsRef<Path>, device: &EntityId) -> Result<bool, DeviceRegistryError> {
        let mut found = false;
        with_locked_store(path.as_ref(), |store| {
            found = store.devices.remove(device.as_bytes()).is_some();
        })?;
        Ok(found)
    }

    /// Persist atomically: write a sibling temp file (owner-only on Unix),
    /// fsync, rename over the target — a reader sees the old or the new file,
    /// never a partial.
    fn save_atomic(&self, path: &Path) -> Result<(), DeviceRegistryError> {
        let io = |e: std::io::Error| DeviceRegistryError::Io {
            path: path.display().to_string(),
            reason: e.to_string(),
        };
        let file = DeviceFile {
            devices: self
                .devices
                .values()
                .map(|r| StoredDevice {
                    device: encode_entity_hex(r.device.as_bytes()),
                    name: r.name.clone(),
                    tags: r.tags.clone(),
                    enrolled_at: r.enrolled_at,
                    revoked_at: r.revoked_at,
                })
                .collect(),
        };
        let bytes = serde_json::to_vec_pretty(&file).map_err(|e| DeviceRegistryError::Io {
            path: path.display().to_string(),
            reason: format!("serialize device registry: {e}"),
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

        // Durability (POSIX): fsync the parent directory so the rename survives
        // a crash (mirrors `revocation.rs` / `redex/disk.rs`). Best-effort;
        // Windows needs no directory fsync.
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

/// Load → mutate → atomically save, all under a cross-process lock on the stable
/// sidecar. The shared load-modify-save transaction for every registry writer.
fn with_locked_store(
    path: &Path,
    mutate: impl FnOnce(&mut DeviceRegistry),
) -> Result<(), DeviceRegistryError> {
    let io = |e: std::io::Error| DeviceRegistryError::Io {
        path: path.display().to_string(),
        reason: e.to_string(),
    };

    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(io)?;
        }
    }

    // Lock a STABLE sidecar (not the store file, which the atomic rename
    // replaces) so two writers serialize. Releases when `_lock` drops.
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

    let mut store = DeviceRegistry::load(path)?;
    mutate(&mut store);
    store.save_atomic(path)?;
    Ok(())
}

/// The stable lock sidecar next to `path` (`<path>.lock`), built from the raw
/// `OsString` so a non-UTF-8 path is preserved byte-for-byte.
fn sidecar_lock_path(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_os_string();
    s.push(".lock");
    PathBuf::from(s)
}

#[derive(Serialize, Deserialize, Default)]
struct DeviceFile {
    #[serde(default)]
    devices: Vec<StoredDevice>,
}

#[derive(Serialize, Deserialize)]
struct StoredDevice {
    /// Device entity-id, lowercase hex (64 chars).
    device: String,
    name: String,
    #[serde(default)]
    tags: Vec<String>,
    enrolled_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    revoked_at: Option<u64>,
}

// Entity-id hex codec. Duplicated from `revocation.rs` deliberately: each
// file-backed store stays self-contained (as `revocation.rs` itself hand-rolls
// this rather than pulling `hex`), so the modules don't couple through an
// internal helper.
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
        return Err(format!("device id must be 64 hex chars, got {}", s.len()));
    }
    let mut out = [0u8; 32];
    let bytes = s.as_bytes();
    for (i, slot) in out.iter_mut().enumerate() {
        let hi = (bytes[i * 2] as char)
            .to_digit(16)
            .ok_or_else(|| format!("device id has non-hex char at {}", i * 2))?;
        let lo = (bytes[i * 2 + 1] as char)
            .to_digit(16)
            .ok_or_else(|| format!("device id has non-hex char at {}", i * 2 + 1))?;
        *slot = ((hi << 4) | lo) as u8;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Identity;

    fn dev(name: &str) -> (EntityId, DeviceRecord) {
        let id = Identity::generate().entity_id().clone();
        let rec = DeviceRecord::new(
            id.clone(),
            name,
            vec![format!("region:{name}")],
            1_700_000_000,
        );
        (id, rec)
    }

    #[test]
    fn missing_file_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("devices.json");
        assert!(DeviceRegistry::load(&path).unwrap().is_empty());
    }

    #[test]
    fn record_persists_and_reloads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("devices.json");
        let (id, rec) = dev("pc");

        DeviceRegistry::record(&path, rec.clone()).unwrap();
        let reloaded = DeviceRegistry::load(&path).unwrap();
        assert_eq!(reloaded.len(), 1);
        assert_eq!(reloaded.get(&id), Some(&rec));
        assert!(!reloaded.get(&id).unwrap().is_revoked());
    }

    #[test]
    fn record_upserts_an_existing_device() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("devices.json");
        let (id, rec) = dev("pc");
        DeviceRegistry::record(&path, rec).unwrap();

        // Re-enroll the same device with a new name/tags → replace.
        let updated = DeviceRecord::new(
            id.clone(),
            "workstation",
            vec!["gpu:true".into()],
            1_700_000_100,
        );
        DeviceRegistry::record(&path, updated.clone()).unwrap();

        let reloaded = DeviceRegistry::load(&path).unwrap();
        assert_eq!(reloaded.len(), 1, "upsert, not append");
        assert_eq!(reloaded.get(&id), Some(&updated));
    }

    #[test]
    fn mark_revoked_stamps_and_reports_found() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("devices.json");
        let (id, rec) = dev("mac");
        DeviceRegistry::record(&path, rec).unwrap();

        assert!(DeviceRegistry::mark_revoked(&path, &id, 1_700_000_500).unwrap());
        let reloaded = DeviceRegistry::load(&path).unwrap();
        assert_eq!(reloaded.get(&id).unwrap().revoked_at, Some(1_700_000_500));
        assert!(reloaded.get(&id).unwrap().is_revoked());

        // An unknown device reports not-found and doesn't create a record.
        let (other, _) = dev("ghost");
        assert!(!DeviceRegistry::mark_revoked(&path, &other, 1_700_000_600).unwrap());
        assert_eq!(DeviceRegistry::load(&path).unwrap().len(), 1);
    }

    #[test]
    fn remove_prunes_a_device() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("devices.json");
        let (id, rec) = dev("pc");
        DeviceRegistry::record(&path, rec).unwrap();

        assert!(DeviceRegistry::remove(&path, &id).unwrap());
        assert!(DeviceRegistry::load(&path).unwrap().is_empty());
        // Removing an absent device is a no-op that reports not-found.
        assert!(!DeviceRegistry::remove(&path, &id).unwrap());
    }

    #[test]
    fn list_is_ordered_and_complete() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("devices.json");
        let (_, a) = dev("a");
        let (_, b) = dev("b");
        DeviceRegistry::record(&path, a).unwrap();
        DeviceRegistry::record(&path, b).unwrap();

        let reg = DeviceRegistry::load(&path).unwrap();
        let names: Vec<_> = reg.list().iter().map(|r| r.name.clone()).collect();
        assert_eq!(names.len(), 2);
    }

    #[test]
    fn corrupt_file_is_surfaced() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("devices.json");
        std::fs::write(&path, b"{ not valid json").unwrap();
        assert!(matches!(
            DeviceRegistry::load(&path),
            Err(DeviceRegistryError::Corrupt { .. })
        ));
    }

    #[test]
    fn hex_round_trips() {
        let id = Identity::generate().entity_id().clone();
        let hexed = encode_entity_hex(id.as_bytes());
        assert_eq!(hexed.len(), 64);
        assert_eq!(&decode_entity_hex(&hexed).unwrap(), id.as_bytes());
        assert_eq!(
            &decode_entity_hex(&format!("0x{hexed}")).unwrap(),
            id.as_bytes()
        );
        assert!(decode_entity_hex("deadbeef").is_err());
    }
}
