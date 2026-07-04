//! The persistent pin store (`MCP_BRIDGE_PLAN.md` Phase 3).
//!
//! A pin is *local client consent* for a capability — for this user profile on
//! this machine — not remote authorization (the wrapper's owner scope always
//! wins on top). Two rules from the plan govern the design:
//!
//! - **The model must not approve its own future access.** `net_request_pin`
//!   only ever writes a *pending* record; moving `pending → approved` happens
//!   exclusively through the operator CLI (`net mcp pin approve`), outside the
//!   model loop. This store has no "approve" path reachable from a request.
//! - **State is shared across shims/hosts on the machine.** The plan puts it
//!   daemon-side; our shim is not a daemon client, so the machine-shared
//!   equivalent is a per-user JSON file every `net mcp serve` and the pin CLI
//!   read/write. Writes are atomic (temp + rename) so a concurrent reader never
//!   sees a half-written file; last-writer-wins on the rare approve/request
//!   race is acceptable for an interactive flow.
//!
//! The store keys on a capability's [`CapabilityId`] display form
//! (`provider/capability`), so a pin is bound to a specific provider — never a
//! bare capability name that could silently repoint.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::backend::CapabilityId;

/// Whether a pin has been approved by the operator, or is still awaiting it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PinState {
    /// Requested (by the model, via `net_request_pin`) but not yet approved.
    /// Grants nothing.
    Pending,
    /// Approved out-of-band by the operator. Satisfies shim-side consent.
    Approved,
}

/// A failure loading or saving the pin store.
#[derive(Debug, thiserror::Error)]
pub enum PinStoreError {
    /// An I/O error reading or writing the store file.
    #[error("pin store I/O error at {path}: {reason}")]
    Io {
        /// The file path involved.
        path: String,
        /// The stringified underlying I/O error.
        reason: String,
    },
    /// The store file exists but does not parse as a pin store.
    #[error("pin store at {path} is corrupt: {reason}")]
    Corrupt {
        /// The file path involved.
        path: String,
        /// Why it failed to parse.
        reason: String,
    },
}

/// The persisted, machine-shared set of pins.
#[derive(Debug, Clone)]
pub struct PinStore {
    path: PathBuf,
    /// Keyed by `CapabilityId` display form for stable, provider-bound records.
    pins: BTreeMap<String, PinState>,
}

// On-disk shape. A struct wrapper (not a bare map) leaves room for a future
// schema version / metadata without a breaking format change.
#[derive(Serialize, Deserialize, Default)]
struct PinFile {
    #[serde(default)]
    pins: Vec<StoredPin>,
}

#[derive(Serialize, Deserialize)]
struct StoredPin {
    cap_id: String,
    state: PinState,
}

impl PinStore {
    /// Load the store at `path`. A missing file is an **empty** store (the
    /// common first-run case), not an error; a present-but-unparseable file is
    /// [`PinStoreError::Corrupt`] so a typo never silently drops pins.
    pub async fn load(path: impl Into<PathBuf>) -> Result<Self, PinStoreError> {
        let path = path.into();
        match tokio::fs::read(&path).await {
            Ok(bytes) => {
                let file: PinFile =
                    serde_json::from_slice(&bytes).map_err(|e| PinStoreError::Corrupt {
                        path: path.display().to_string(),
                        reason: e.to_string(),
                    })?;
                let pins = file.pins.into_iter().map(|p| (p.cap_id, p.state)).collect();
                Ok(Self { path, pins })
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self {
                path,
                pins: BTreeMap::new(),
            }),
            Err(e) => Err(PinStoreError::Io {
                path: path.display().to_string(),
                reason: e.to_string(),
            }),
        }
    }

    /// Persist the store atomically: write a sibling temp file, then rename it
    /// over the target (an atomic replace on both Unix and Windows), so a
    /// concurrent reader sees either the old or the new file, never a partial.
    pub async fn save(&self) -> Result<(), PinStoreError> {
        let io_err = |e: std::io::Error| PinStoreError::Io {
            path: self.path.display().to_string(),
            reason: e.to_string(),
        };

        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() {
                tokio::fs::create_dir_all(parent).await.map_err(io_err)?;
            }
        }

        let file = PinFile {
            pins: self
                .pins
                .iter()
                .map(|(cap_id, state)| StoredPin {
                    cap_id: cap_id.clone(),
                    state: *state,
                })
                .collect(),
        };
        let bytes = serde_json::to_vec_pretty(&file).map_err(|e| PinStoreError::Io {
            path: self.path.display().to_string(),
            reason: format!("serialize pin store: {e}"),
        })?;

        // A per-process-unique temp name so two writers don't clobber each
        // other's temp file mid-write (the final rename is still last-wins).
        let tmp = self
            .path
            .with_extension(format!("tmp.{}", std::process::id()));
        tokio::fs::write(&tmp, &bytes).await.map_err(io_err)?;

        // Restrict to owner-only before the rename — the store records
        // security-sensitive consent decisions, so it must not be world- or
        // group-readable under a permissive umask. The mode travels with the
        // inode through the rename. (Unix only; on Windows the per-user data
        // dir already scopes access via inherited ACLs.)
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            tokio::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))
                .await
                .map_err(io_err)?;
        }

        tokio::fs::rename(&tmp, &self.path).await.map_err(io_err)?;
        Ok(())
    }

    /// The store's file path.
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    /// Record a pin **request** (model-callable). Adds a `Pending` record if
    /// the capability has no record yet; if one already exists (pending or
    /// approved) it is left unchanged — a request never upgrades a pin. Returns
    /// the resulting state.
    pub fn request(&mut self, id: &CapabilityId) -> PinState {
        *self.pins.entry(id.display()).or_insert(PinState::Pending)
    }

    /// **Approve** a pin (operator-only). Creates the record if absent (an
    /// operator may pre-approve). Returns `true` if this changed the state.
    pub fn approve(&mut self, id: &CapabilityId) -> bool {
        let prev = self.pins.insert(id.display(), PinState::Approved);
        prev != Some(PinState::Approved)
    }

    /// **Reject / remove** a pin entirely (operator-only). Returns `true` if a
    /// record was removed.
    pub fn remove(&mut self, id: &CapabilityId) -> bool {
        self.pins.remove(&id.display()).is_some()
    }

    /// Is `id` approved?
    pub fn is_approved(&self, id: &CapabilityId) -> bool {
        self.pins.get(&id.display()) == Some(&PinState::Approved)
    }

    /// The state of `id`, if it has a record.
    pub fn state(&self, id: &CapabilityId) -> Option<PinState> {
        self.pins.get(&id.display()).copied()
    }

    /// Every approved capability (parseable ids only).
    pub fn approved(&self) -> Vec<CapabilityId> {
        self.ids_in(PinState::Approved)
    }

    /// Every pending capability (parseable ids only).
    pub fn pending(&self) -> Vec<CapabilityId> {
        self.ids_in(PinState::Pending)
    }

    /// All records as `(id, state)` (parseable ids only), for `pin list`.
    pub fn list(&self) -> Vec<(CapabilityId, PinState)> {
        self.pins
            .iter()
            .filter_map(|(raw, state)| CapabilityId::parse(raw).ok().map(|id| (id, *state)))
            .collect()
    }

    fn ids_in(&self, want: PinState) -> Vec<CapabilityId> {
        self.pins
            .iter()
            .filter(|(_, s)| **s == want)
            .filter_map(|(raw, _)| CapabilityId::parse(raw).ok())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cap(s: &str) -> CapabilityId {
        CapabilityId::parse(s).unwrap()
    }

    fn store_path() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("pins.json");
        (dir, path)
    }

    #[tokio::test]
    async fn missing_file_loads_empty() {
        let (_dir, path) = store_path();
        let store = PinStore::load(&path).await.unwrap();
        assert!(store.approved().is_empty());
        assert!(store.pending().is_empty());
    }

    #[tokio::test]
    async fn request_creates_pending_and_does_not_grant() {
        let (_dir, path) = store_path();
        let mut store = PinStore::load(&path).await.unwrap();
        let id = cap("b/echo");
        assert_eq!(store.request(&id), PinState::Pending);
        assert!(!store.is_approved(&id), "a request must not grant consent");
        assert_eq!(store.pending(), vec![id.clone()]);
        // A second request does not upgrade it.
        assert_eq!(store.request(&id), PinState::Pending);
    }

    #[tokio::test]
    async fn request_never_upgrades_an_approved_pin() {
        let (_dir, path) = store_path();
        let mut store = PinStore::load(&path).await.unwrap();
        let id = cap("b/echo");
        assert!(store.approve(&id));
        // The model requesting again must not disturb an approved pin.
        assert_eq!(store.request(&id), PinState::Approved);
        assert!(store.is_approved(&id));
    }

    #[tokio::test]
    async fn approve_then_persist_is_visible_to_a_fresh_load() {
        let (_dir, path) = store_path();
        {
            let mut store = PinStore::load(&path).await.unwrap();
            store.request(&cap("b/echo"));
            assert!(store.approve(&cap("b/secret")));
            store.save().await.unwrap();
        }
        // A separate load (another shim / the pin CLI) sees the same state.
        let reloaded = PinStore::load(&path).await.unwrap();
        assert!(reloaded.is_approved(&cap("b/secret")));
        assert!(!reloaded.is_approved(&cap("b/echo")));
        assert_eq!(reloaded.pending(), vec![cap("b/echo")]);
        assert_eq!(reloaded.approved(), vec![cap("b/secret")]);
    }

    #[tokio::test]
    async fn remove_deletes_the_record() {
        let (_dir, path) = store_path();
        let mut store = PinStore::load(&path).await.unwrap();
        let id = cap("b/echo");
        store.approve(&id);
        assert!(store.remove(&id));
        assert!(!store.is_approved(&id));
        assert!(!store.remove(&id), "removing again is a no-op");
    }

    #[tokio::test]
    async fn approve_reports_whether_it_changed_state() {
        let (_dir, path) = store_path();
        let mut store = PinStore::load(&path).await.unwrap();
        let id = cap("b/echo");
        assert!(store.approve(&id), "pending/absent → approved is a change");
        assert!(!store.approve(&id), "already approved → no change");
    }

    #[tokio::test]
    async fn corrupt_file_is_an_error_not_a_silent_reset() {
        let (_dir, path) = store_path();
        tokio::fs::create_dir_all(path.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(&path, b"{ not valid json").await.unwrap();
        let err = PinStore::load(&path).await.unwrap_err();
        assert!(matches!(err, PinStoreError::Corrupt { .. }));
    }

    #[tokio::test]
    async fn list_round_trips_states() {
        let (_dir, path) = store_path();
        let mut store = PinStore::load(&path).await.unwrap();
        store.approve(&cap("b/a"));
        store.request(&cap("b/z"));
        store.save().await.unwrap();
        let reloaded = PinStore::load(&path).await.unwrap();
        let mut listed = reloaded.list();
        listed.sort_by(|a, b| a.0.display().cmp(&b.0.display()));
        assert_eq!(
            listed,
            vec![
                (cap("b/a"), PinState::Approved),
                (cap("b/z"), PinState::Pending),
            ],
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn saved_store_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let (_dir, path) = store_path();
        let mut store = PinStore::load(&path).await.unwrap();
        store.approve(&cap("b/echo"));
        store.save().await.unwrap();
        let mode = tokio::fs::metadata(&path)
            .await
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o777,
            0o600,
            "the pin store records consent decisions and must be owner-only",
        );
    }
}
