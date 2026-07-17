//! Restart-persistent organization revocation maxima — OA-1 §1.5 of
//! `docs/plans/ORG_CAPABILITY_AUTH_PLAN.md`.
//!
//! An in-memory monotone merge of
//! [`OrgRevocationBundle`](super::org::OrgRevocationBundle) floors
//! is insufficient: if config management replaces the operator's
//! bundle file with an OLDER (still validly signed) bundle and the
//! node restarts, there is no prior maximum left to compare
//! against, and the fleet silently rolls back to weaker floors.
//! The minimum fix — deliberately NOT the deferred WAL/replication
//! system — is one small atomic local file of merged maxima
//! (`revocation-state.json` in the node's authority config
//! directory).
//!
//! # Locked reload order
//!
//! ```text
//! verify incoming bundle signature
//! → merge maxima with PERSISTED state (monotone; lower never wins)
//! → atomically write merged maxima
//!      (write temp → fsync temp → atomic rename → fsync parent dir)
//! → ONLY THEN publish the new live view
//! ```
//!
//! [`OrgRevocationStore::apply_bundle`] implements exactly this
//! order. Failure handling is asymmetric by design:
//!
//! - **Corrupt incoming bundle** → keep the persisted last-good
//!   state, log loudly, return a typed error. Live view untouched.
//! - **Corrupt persisted maxima file** → LOUD startup failure
//!   ([`OrgRevocationStore::open_existing`] refuses) — protected
//!   verification never starts against silently weaker floors. A
//!   *missing* file at startup is equally loud: absence IS silently
//!   weaker floors. Only `net node adopt`
//!   ([`OrgRevocationStore::init`]) may create the file.
//!
//! Unlike the sdk's `RevocationStore`, the parent-directory fsync
//! here is **not** best-effort: the plan's locked order makes it
//! part of the durability boundary, and the live view must not
//! publish a floor the filesystem could forget on crash.
//!
//! # Writer model
//!
//! The node owns its own maxima file; bundle files distributed by
//! the operator are inputs, never this file. Same-file writers —
//! whether a second store instance, a concurrent `net node adopt`,
//! or another process — are ENFORCED serial (review-8 §5): every
//! reload holds an exclusive advisory lock on the stable `.lock`
//! sidecar and rereads the persisted maxima under that lock before
//! merging, so no writer's floors can be rolled out of the file by
//! a staler writer's in-memory snapshot.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};

use super::org::{OrgError, OrgId, OrgRevocationBundle};
use crate::adapter::net::identity::EntityId;

/// Format version of `revocation-state.json`. Bump requires an
/// explicit migration; an unknown version is a loud startup
/// failure, never a silent re-init.
pub const ORG_REVOCATION_STATE_VERSION: u32 = 1;

/// Merged revocation-floor maxima: for each `(org, member)`, the
/// highest `minimum_generation` any verified bundle has ever
/// asserted on this node. Monotone: a merge can only raise floors.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OrgRevocationState {
    floors: BTreeMap<(OrgId, EntityId), u32>,
}

impl OrgRevocationState {
    /// The empty state (fresh adopt).
    pub fn empty() -> Self {
        Self::default()
    }

    /// The current floor for `(org, member)`. Absent keys floor at
    /// 0 — every generation is admissible until a bundle says
    /// otherwise.
    pub fn floor_for(&self, org: &OrgId, member: &EntityId) -> u32 {
        self.floors
            .get(&(*org, member.clone()))
            .copied()
            .unwrap_or(0)
    }

    /// Number of tracked `(org, member)` floors.
    pub fn len(&self) -> usize {
        self.floors.len()
    }

    /// `true` iff no floors are tracked.
    pub fn is_empty(&self) -> bool {
        self.floors.is_empty()
    }

    /// Iterate floors in canonical `(org, member)` order.
    pub fn iter(&self) -> impl Iterator<Item = (&(OrgId, EntityId), &u32)> {
        self.floors.iter()
    }

    /// Monotone merge: raise each `(bundle.org_id, member)` floor
    /// to the bundle's value where higher; lower values never win.
    /// Returns how many floors rose.
    ///
    /// Does NOT verify the bundle — the caller does (the store's
    /// locked order verifies before merging; state-level callers
    /// such as tests must do the same).
    pub fn merge_bundle(&mut self, bundle: &OrgRevocationBundle) -> usize {
        let mut raised = 0;
        for (member, floor) in bundle.floors() {
            let entry = self
                .floors
                .entry((bundle.org_id, member.clone()))
                .or_insert(0);
            if *floor > *entry {
                *entry = *floor;
                raised += 1;
            }
        }
        raised
    }

    /// Serialize to the versioned on-disk JSON form (sorted by the
    /// map's canonical order, so the file is deterministic).
    fn to_file_bytes(&self) -> Result<Vec<u8>, OrgRevocationError> {
        let file = PersistedStateFile {
            version: ORG_REVOCATION_STATE_VERSION,
            floors: self
                .floors
                .iter()
                .map(|((org, member), floor)| PersistedFloor {
                    org: *org,
                    member: member.clone(),
                    floor: *floor,
                })
                .collect(),
        };
        serde_json::to_vec_pretty(&file).map_err(|e| OrgRevocationError::Io {
            path: String::new(),
            reason: format!("serialize revocation state: {e}"),
        })
    }

    /// Strict read of a persisted state file that may not exist
    /// yet: `Ok(None)` when absent, loud typed errors on anything
    /// unparseable. The adoption ceremony uses this to validate
    /// candidate floors BEFORE creating any durable state
    /// (review-8 §7/§8).
    pub fn load_if_exists(path: &Path) -> Result<Option<Self>, OrgRevocationError> {
        match read_regular_nofollow(path) {
            Ok(bytes) => Self::from_file_bytes(&bytes, path).map(Some),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(OrgRevocationError::Io {
                path: path.display().to_string(),
                reason: e.to_string(),
            }),
        }
    }

    /// Strict parse of the on-disk form. Unknown fields, an
    /// unsupported version, or duplicate `(org, member)` keys are
    /// all corruption — loud typed errors, never best-effort
    /// recovery (a "recovered" state could be a weaker one).
    fn from_file_bytes(bytes: &[u8], path: &Path) -> Result<Self, OrgRevocationError> {
        let file: PersistedStateFile =
            serde_json::from_slice(bytes).map_err(|e| OrgRevocationError::CorruptState {
                path: path.display().to_string(),
                detail: e.to_string(),
            })?;
        if file.version != ORG_REVOCATION_STATE_VERSION {
            return Err(OrgRevocationError::UnsupportedVersion {
                path: path.display().to_string(),
                found: file.version,
            });
        }
        let mut floors = BTreeMap::new();
        for entry in file.floors {
            if floors
                .insert((entry.org, entry.member), entry.floor)
                .is_some()
            {
                return Err(OrgRevocationError::CorruptState {
                    path: path.display().to_string(),
                    detail: "duplicate (org, member) floor entry".to_string(),
                });
            }
        }
        Ok(Self { floors })
    }
}

/// On-disk shape of `revocation-state.json`. `deny_unknown_fields`:
/// an entry this node doesn't understand could be a floor it is
/// about to drop — corruption, not forward compatibility.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedStateFile {
    version: u32,
    floors: Vec<PersistedFloor>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedFloor {
    org: OrgId,
    member: EntityId,
    floor: u32,
}

/// Errors from the persisted revocation store.
#[derive(Debug)]
pub enum OrgRevocationError {
    /// The incoming bundle failed signature or structural
    /// verification. Persisted last-good state is retained.
    InvalidBundle(OrgError),
    /// No persisted maxima file at startup. Absence is silently
    /// weaker floors, so startup must not proceed; only
    /// `net node adopt` creates the file.
    MissingState {
        /// Where the state file was expected.
        path: String,
    },
    /// The persisted maxima file exists but cannot be trusted
    /// (parse failure, duplicate keys). LOUD startup failure.
    CorruptState {
        /// The state file's path.
        path: String,
        /// What failed to parse or validate.
        detail: String,
    },
    /// The persisted file's format version is unknown to this
    /// build.
    UnsupportedVersion {
        /// The state file's path.
        path: String,
        /// The version the file declares.
        found: u32,
    },
    /// Filesystem failure while reading or durably writing. When
    /// raised from `apply_bundle` this is always PRE-rename: the
    /// old file and old live view are both intact.
    Io {
        /// The path being read or written.
        path: String,
        /// The underlying I/O error.
        reason: String,
    },
    /// The rename LANDED but the parent-directory fsync failed —
    /// the directory entry may or may not survive a crash, so disk
    /// and memory can no longer be proven synchronized. The store
    /// publishes the merged (never-weaker) live view, then poisons
    /// itself: further applies are refused until restart
    /// re-establishes ground truth from disk (review-8 §13).
    DurabilityUncertain {
        /// The state file's path.
        path: String,
        /// The underlying fsync error.
        reason: String,
    },
    /// A previous apply ended post-rename durability-uncertain
    /// (see [`Self::DurabilityUncertain`]); this store refuses
    /// further reloads until the process restarts.
    Poisoned {
        /// The state file's path.
        path: String,
    },
    /// A running node refused to swap its installed revocation
    /// store for one whose live view is lower on some `(org,
    /// member)` key — an installed floor never lowers (review-8
    /// §4). Reload higher floors through
    /// [`OrgRevocationStore::apply_bundle`] instead of replacing
    /// the store.
    NonMonotonicReplacement {
        /// The candidate store's state-file path.
        path: String,
    },
}

impl std::fmt::Display for OrgRevocationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidBundle(e) => write!(f, "revocation bundle rejected: {e}"),
            Self::MissingState { path } => write!(
                f,
                "revocation state file missing at {path}; refusing to start with \
                 implicitly empty floors — run `net node adopt` to provision"
            ),
            Self::CorruptState { path, detail } => write!(
                f,
                "revocation state file at {path} is corrupt ({detail}); refusing to \
                 start against silently weaker floors"
            ),
            Self::UnsupportedVersion { path, found } => write!(
                f,
                "revocation state file at {path} has unsupported version {found} \
                 (this build supports {ORG_REVOCATION_STATE_VERSION})"
            ),
            Self::Io { path, reason } => write!(f, "revocation state I/O at {path}: {reason}"),
            Self::DurabilityUncertain { path, reason } => write!(
                f,
                "revocation state at {path}: rename landed but the parent-directory \
                 fsync failed ({reason}); disk and memory can no longer be proven \
                 synchronized — store poisoned until restart"
            ),
            Self::Poisoned { path } => write!(
                f,
                "revocation store at {path} is poisoned after a durability-uncertain \
                 write; restart the process to re-establish ground truth from disk"
            ),
            Self::NonMonotonicReplacement { path } => write!(
                f,
                "refusing to replace the installed revocation store with {path}: its \
                 live view is lower on at least one (org, member) floor — an installed \
                 floor never lowers; apply a bundle instead"
            ),
        }
    }
}

impl std::error::Error for OrgRevocationError {}

/// One floor raise observed by [`OrgRevocationStore::apply_bundle`]
/// relative to the store's previously published live view —
/// `(org, member, new_floor)`. Fed to the raise callback so a
/// running node can retract stale ownership projections
/// immediately (review-8 §9).
pub type RaisedFloor = (OrgId, EntityId, u32);

/// Callback invoked after a reload publishes floors higher than the
/// previously enforced view.
type FloorsRaisedCallback = Arc<dyn Fn(&[RaisedFloor]) + Send + Sync>;

/// The node-local persisted revocation maxima plus its published
/// live view. See the module docs for the locked reload order and
/// failure semantics.
///
/// The store is org-agnostic (keys are `(OrgId, EntityId)`); WHICH
/// bundles get fed to [`Self::apply_bundle`] is the caller's trust
/// decision — in OA-1 the adopt/startup wiring feeds only the
/// node's owner-org bundle.
///
/// # Multi-writer safety (review-8 §5)
///
/// Same-file writers (a second store instance, a concurrent
/// `net node adopt`) are serialized through an exclusive advisory
/// lock on a stable `.lock` sidecar, and every reload REREADS the
/// persisted maxima under that lock before merging — an instance's
/// in-memory snapshot is never trusted as the merge base, so a
/// stale writer cannot roll another writer's floors out of the
/// file. Because every writer follows reread-merge-write, the disk
/// state only ever grows, and republishing the reread state can
/// never lower a live view.
pub struct OrgRevocationStore {
    path: PathBuf,
    /// Registry key for the PATH-WIDE durability-uncertainty bit
    /// (review-9): poison is shared by every store instance backed
    /// by the same canonical pathname, not held per object.
    poison_key: PathBuf,
    /// Serializes this instance's merge→persist→publish sequences
    /// (the sidecar file lock serializes across instances).
    reload: Mutex<()>,
    /// The published live view. Never ahead of the durably
    /// persisted state; possibly behind it between reloads when
    /// another writer advanced the file.
    live: RwLock<Arc<OrgRevocationState>>,
    /// Invoked (outside BOTH the file lock and the reload lock —
    /// re-entrant callbacks must not deadlock, review-9) with the
    /// floors a reload raised relative to the previously published
    /// view. The running node uses this to retract stale ownership
    /// projections from the capability fold.
    on_floors_raised: RwLock<Option<FloorsRaisedCallback>>,
}

impl OrgRevocationStore {
    /// Adopt-time entry point: load the existing file if present
    /// (re-adoption preserves maxima — monotonicity survives even
    /// an operator re-running adopt), otherwise durably create an
    /// empty state file. Runs under the interprocess lock so two
    /// concurrent adoptions cannot race the create.
    pub fn init(path: impl Into<PathBuf>) -> Result<Self, OrgRevocationError> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| OrgRevocationError::Io {
                    path: path.display().to_string(),
                    reason: e.to_string(),
                })?;
            }
        }
        let _lock = lock_state_file(&path)?;
        let poison_key = poison_key_for(&path);
        recover_poison_if_needed_locked(&path, &poison_key)?;
        let state = match read_regular_nofollow(&path) {
            Ok(bytes) => OrgRevocationState::from_file_bytes(&bytes, &path)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let state = OrgRevocationState::empty();
                write_atomic(&path, &state.to_file_bytes()?)?;
                state
            }
            Err(e) => {
                return Err(OrgRevocationError::Io {
                    path: path.display().to_string(),
                    reason: e.to_string(),
                })
            }
        };
        Ok(Self {
            path,
            poison_key,
            reload: Mutex::new(()),
            live: RwLock::new(Arc::new(state)),
            on_floors_raised: RwLock::new(None),
        })
    }

    /// Startup entry point: the file MUST exist and parse. Missing
    /// or corrupt → loud typed error; protected verification never
    /// starts against silently weaker floors.
    ///
    /// If the backing path is durability-poisoned (review-9), the
    /// open performs explicit recovery under the interprocess lock
    /// — a reread plus a SUCCESSFUL parent-directory fsync — before
    /// treating the current pathname as authoritative; recovery
    /// failure refuses the open. A fresh instance therefore never
    /// launders path-wide uncertainty.
    pub fn open_existing(path: impl Into<PathBuf>) -> Result<Self, OrgRevocationError> {
        let path = path.into();
        let poison_key = poison_key_for(&path);
        if poison_registry().lock().contains(&poison_key) {
            let _lock = lock_state_file(&path)?;
            recover_poison_if_needed_locked(&path, &poison_key)?;
        }
        let bytes = match read_regular_nofollow(&path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let err = OrgRevocationError::MissingState {
                    path: path.display().to_string(),
                };
                tracing::error!("{err}");
                return Err(err);
            }
            Err(e) => {
                return Err(OrgRevocationError::Io {
                    path: path.display().to_string(),
                    reason: e.to_string(),
                })
            }
        };
        let state = OrgRevocationState::from_file_bytes(&bytes, &path).inspect_err(|err| {
            tracing::error!("{err}");
        })?;
        Ok(Self {
            path,
            poison_key,
            reload: Mutex::new(()),
            live: RwLock::new(Arc::new(state)),
            on_floors_raised: RwLock::new(None),
        })
    }

    /// The backing file path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Snapshot of the published live view.
    pub fn snapshot(&self) -> Arc<OrgRevocationState> {
        self.live.read().clone()
    }

    /// Live floor for `(org, member)`.
    pub fn floor_for(&self, org: &OrgId, member: &EntityId) -> u32 {
        self.snapshot().floor_for(org, member)
    }

    /// `true` while this store's BACKING PATH is
    /// durability-uncertain (review-9: the poison bit is shared by
    /// every instance on the same canonical pathname, not held per
    /// object). Cleared only by explicit recovery — a locked
    /// reread plus a successful parent-directory fsync — performed
    /// by [`Self::open_existing`], [`Self::init`], or the next
    /// [`Self::apply_bundle`].
    pub fn is_poisoned(&self) -> bool {
        poison_registry().lock().contains(&self.poison_key)
    }

    /// Install the raise callback (review-8 §9). Invoked after a
    /// reload publishes floors above the previously enforced view —
    /// including floors learned from OTHER writers via the
    /// under-lock reread, not only the supplied bundle's. The node
    /// wiring uses this to retract stale ownership projections.
    pub fn set_on_floors_raised(&self, callback: impl Fn(&[RaisedFloor]) + Send + Sync + 'static) {
        *self.on_floors_raised.write() = Some(Arc::new(callback));
    }

    /// Apply an operator bundle under the locked reload order
    /// (interprocess-safe, review-8 §5):
    ///
    /// ```text
    /// verify bundle signature
    /// → acquire exclusive lock on the stable `.lock` sidecar
    /// → REREAD the persisted maxima under the lock (load-bearing:
    ///    an in-memory snapshot must never be the merge base)
    /// → monotone merge
    /// → atomically persist iff the disk state changed
    /// → publish the merged live view
    /// → release the lock, notify raise observers
    /// ```
    ///
    /// Returns the floors raised relative to this store's
    /// PREVIOUSLY published view — the supplied bundle's raises
    /// plus any floors another writer advanced on disk since the
    /// last reload. `Ok(empty)` means nothing rose (a lower bundle
    /// never rolls back).
    ///
    /// On pre-rename errors the persisted last-good state and the
    /// live view are both untouched. A POST-rename parent-fsync
    /// failure publishes the merged (never-weaker) view, poisons
    /// the store, and returns
    /// [`OrgRevocationError::DurabilityUncertain`]; further applies
    /// are refused until restart.
    pub fn apply_bundle(
        &self,
        bundle: &OrgRevocationBundle,
    ) -> Result<Vec<RaisedFloor>, OrgRevocationError> {
        // The locked phase returns its outcome so raise observers
        // run AFTER both the file lock and this instance's reload
        // guard have dropped — a callback that re-enters
        // `apply_bundle` on the same store must not deadlock
        // (review-9).
        enum LockedOutcome {
            Applied(Vec<RaisedFloor>),
            DurabilityUncertain(Vec<RaisedFloor>, String),
        }

        let outcome = {
            let _guard = self.reload.lock();

            // 1. Verify the incoming bundle's signature + canonical
            //    structure. A corrupt bundle keeps last-good, loudly.
            if let Err(e) = bundle.verify() {
                let err = OrgRevocationError::InvalidBundle(e);
                tracing::error!(
                    org = %bundle.org_id,
                    "rejecting revocation bundle, keeping last-good persisted floors: {err}"
                );
                return Err(err);
            }

            // 2. Interprocess critical section. If the backing path
            //    is durability-poisoned, attempt explicit recovery
            //    (parent-dir fsync under the lock) — no same-path
            //    apply may silently succeed while uncertainty
            //    remains, whichever instance carries it (review-9).
            let lock = lock_state_file(&self.path)?;
            recover_poison_if_needed_locked(&self.path, &self.poison_key)?;

            // 3. REREAD the persisted maxima under the lock — the
            //    reread is load-bearing: merging from this
            //    instance's live snapshot would let a stale writer
            //    overwrite floors another writer already persisted.
            let disk_bytes =
                read_regular_nofollow(&self.path).map_err(|e| OrgRevocationError::Io {
                    path: self.path.display().to_string(),
                    reason: e.to_string(),
                })?;
            let disk = OrgRevocationState::from_file_bytes(&disk_bytes, &self.path)?;

            // 4. Monotone merge against the reread disk state.
            let mut merged = disk.clone();
            let raised_on_disk = merged.merge_bundle(bundle);

            // 5. Persist iff the disk state changed; the write must
            //    complete before anything is published.
            let mut durability_uncertain: Option<String> = None;
            if raised_on_disk > 0 {
                match write_atomic_phased(&self.path, &merged.to_file_bytes()?) {
                    Ok(()) => {}
                    Err(WritePhase::PreRename(reason)) => {
                        // Old file (rename never happened) and old
                        // live view both intact — a floor the disk
                        // could forget is never enforced.
                        drop(lock);
                        return Err(OrgRevocationError::Io {
                            path: self.path.display().to_string(),
                            reason,
                        });
                    }
                    Err(WritePhase::PostRename(reason)) => {
                        // The rename LANDED; only the directory-entry
                        // durability is uncertain. Still publish the
                        // merged (never-weaker) view below so
                        // enforcement doesn't regress under what the
                        // disk may now hold, but poison the PATH: no
                        // instance may pretend disk and memory are
                        // synchronized until recovery proves the
                        // entry durable.
                        poison_registry().lock().insert(self.poison_key.clone());
                        durability_uncertain = Some(reason);
                    }
                }
            }

            // 6. Publish the merged view (also syncs this instance
            //    up to floors other writers advanced) and release
            //    the lock; notification happens outside.
            let raised = self.publish(merged);
            drop(lock);
            match durability_uncertain {
                None => LockedOutcome::Applied(raised),
                Some(reason) => LockedOutcome::DurabilityUncertain(raised, reason),
            }
        };

        match outcome {
            LockedOutcome::Applied(raised) => {
                self.notify_raised(&raised);
                Ok(raised)
            }
            LockedOutcome::DurabilityUncertain(raised, reason) => {
                let err = OrgRevocationError::DurabilityUncertain {
                    path: self.path.display().to_string(),
                    reason,
                };
                tracing::error!("{err}");
                self.notify_raised(&raised);
                Err(err)
            }
        }
    }

    /// Swap the live view to `next`, returning every floor that
    /// rose relative to the previously published view. `next` is
    /// always a monotone superset under the locked reload order,
    /// so no floor can lower here.
    fn publish(&self, next: OrgRevocationState) -> Vec<RaisedFloor> {
        let mut live = self.live.write();
        let raised: Vec<RaisedFloor> = next
            .iter()
            .filter(|((org, member), floor)| **floor > live.floor_for(org, member))
            .map(|((org, member), floor)| (*org, member.clone(), *floor))
            .collect();
        *live = Arc::new(next);
        raised
    }

    fn notify_raised(&self, raised: &[RaisedFloor]) {
        if raised.is_empty() {
            return;
        }
        let callback = self.on_floors_raised.read().clone();
        if let Some(callback) = callback {
            callback(raised);
        }
    }
}

impl std::fmt::Debug for OrgRevocationStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OrgRevocationStore")
            .field("path", &self.path)
            .field("floors", &self.snapshot().len())
            .finish()
    }
}

/// Which phase of the durable write failed. PRE-rename failures
/// are recoverable (the target file was never touched; the temp is
/// cleaned up). POST-rename failures mean the directory entry may
/// already point at the new bytes while its durability is unproven
/// — the caller must fail closed (review-8 §13).
pub(crate) enum WritePhase {
    /// The target file is untouched; nothing published.
    PreRename(String),
    /// The rename landed; only the parent-directory fsync failed.
    PostRename(String),
}

/// Process-wide durability-uncertainty registry, keyed by the
/// CANONICAL backing path (review-9): the filesystem's uncertainty
/// after a landed-rename/failed-dir-fsync belongs to the directory
/// entry, not to one `OrgRevocationStore` instance. Every store
/// opened on the same pathname shares the poison bit; recovery
/// (a locked reread plus a SUCCESSFUL parent-directory fsync)
/// clears it.
static PATH_POISON: std::sync::OnceLock<Mutex<std::collections::HashSet<PathBuf>>> =
    std::sync::OnceLock::new();

fn poison_registry() -> &'static Mutex<std::collections::HashSet<PathBuf>> {
    PATH_POISON.get_or_init(|| Mutex::new(std::collections::HashSet::new()))
}

/// The registry key for `path`: canonicalized parent (stable — the
/// file itself is replaced by rename) joined with the file name;
/// falls back to the path verbatim when the parent cannot resolve.
fn poison_key_for(path: &Path) -> PathBuf {
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) if !parent.as_os_str().is_empty() => parent
            .canonicalize()
            .map(|p| p.join(name))
            .unwrap_or_else(|_| path.to_path_buf()),
        _ => path.to_path_buf(),
    }
}

/// Open `path` as a REGULAR file without following symlinks
/// (review-9): authority/state data and the stable lock inode must
/// never be attacker-steerable through a planted link, and the
/// permission/type checks must run on the OPENED handle so there is
/// no check-to-use window.
///
/// Unix uses `O_NOFOLLOW` (a symlink final component fails to
/// open); other platforms fall back to a `symlink_metadata`
/// pre-check plus a handle-metadata type check.
pub(crate) fn open_regular_nofollow(path: &Path) -> std::io::Result<std::fs::File> {
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(not(unix))]
    {
        let meta = std::fs::symlink_metadata(path)?;
        if meta.file_type().is_symlink() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "refusing symlink: authority files must be regular files",
            ));
        }
    }
    let file = opts.open(path).map_err(|e| {
        #[cfg(unix)]
        if e.raw_os_error() == Some(libc::ELOOP) {
            return std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "refusing symlink: authority files must be regular files",
            );
        }
        e
    })?;
    // Type check on the opened descriptor — immune to a swap
    // between check and use.
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "refusing non-regular file: authority files must be regular files",
        ));
    }
    Ok(file)
}

/// Read a whole regular file through a no-follow handle.
pub(crate) fn read_regular_nofollow(path: &Path) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let mut file = open_regular_nofollow(path)?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// Acquire the exclusive interprocess lock guarding `path` via its
/// stable `.lock` sidecar (the state file itself is replaced by
/// rename, so it cannot carry the lock). Blocking; released when
/// the returned handle drops. std advisory file locking — same
/// semantics as the sdk revocation store's fs2 sidecar. The sidecar
/// is opened no-follow so a planted symlink cannot redirect the
/// lock inode (review-9).
///
/// `pub(crate)`: the adoption ceremony's final phase holds this
/// lock across its floor re-verification and membership write.
pub(crate) fn lock_state_file(path: &Path) -> Result<std::fs::File, OrgRevocationError> {
    let io = |e: std::io::Error| OrgRevocationError::Io {
        path: path.display().to_string(),
        reason: format!("state lock: {e}"),
    };
    let mut lock_path = path.as_os_str().to_os_string();
    lock_path.push(".lock");
    let mut opts = std::fs::OpenOptions::new();
    opts.create(true).write(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(libc::O_NOFOLLOW);
        opts.mode(0o600);
    }
    let f = opts.open(PathBuf::from(lock_path)).map_err(io)?;
    f.lock().map_err(io)?;
    Ok(f)
}

/// Explicit durability recovery, called with the interprocess lock
/// HELD (review-9): if `poison_key` is registered, prove the
/// directory entry durable with a parent-directory fsync; success
/// clears the path-wide poison, failure refuses with
/// [`OrgRevocationError::Poisoned`]. No same-path operation may
/// silently succeed while uncertainty remains.
fn recover_poison_if_needed_locked(
    path: &Path,
    poison_key: &Path,
) -> Result<(), OrgRevocationError> {
    if !poison_registry().lock().contains(poison_key) {
        return Ok(());
    }
    match fsync_parent_dir(path) {
        Ok(()) => {
            poison_registry().lock().remove(poison_key);
            tracing::warn!(
                path = %path.display(),
                "revocation-state durability uncertainty recovered (parent directory fsynced)"
            );
            Ok(())
        }
        Err(e) => {
            tracing::error!(
                path = %path.display(),
                error = %e,
                "revocation-state durability recovery failed; path remains poisoned"
            );
            Err(OrgRevocationError::Poisoned {
                path: path.display().to_string(),
            })
        }
    }
}

/// fsync the parent directory of `path` (Unix; no-op elsewhere,
/// where the rename primitive carries the metadata guarantee).
/// Split out so the durability-recovery path (review-9) can prove
/// the directory entry durable without rewriting the file.
fn fsync_parent_dir(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        let dir = match path.parent() {
            Some(p) if !p.as_os_str().is_empty() => p,
            _ => Path::new("."),
        };
        std::fs::File::open(dir)?.sync_all()?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

/// Monotone counter qualifying temp names so two writers in one
/// process (or a reused PID) can never collide on a temp inode.
static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// A fresh, unpredictable same-directory temp path:
/// `<file>.tmp.<pid>.<seq>.<rand16hex>`. Appended to the FULL file
/// name (the previous `with_extension` form replaced `.json`,
/// making the name predictable — review-8 §10: a pre-created
/// permissive temp would survive `create(true).truncate(true)`
/// with its original mode).
///
/// Entropy failure is an ERROR, not a silent all-zero suffix
/// (review-9): pid + a process-local sequence do not survive PID
/// reuse, so the random suffix is load-bearing for the
/// unpredictability claim. `create_new` keeps even that failure
/// mode fail-loud, but we don't rely on it.
fn fresh_temp_path(path: &Path) -> Result<PathBuf, WritePhase> {
    let mut rand = [0u8; 8];
    getrandom::fill(&mut rand)
        .map_err(|e| WritePhase::PreRename(format!("temp-name entropy unavailable: {e:?}")))?;
    let mut s = path.as_os_str().to_os_string();
    s.push(format!(
        ".tmp.{}.{}.{}",
        std::process::id(),
        TEMP_SEQ.fetch_add(1, Ordering::Relaxed),
        hex::encode(rand)
    ));
    Ok(PathBuf::from(s))
}

/// Durable atomic write with phase-typed failures: fresh
/// `create_new` temp (owner-only mode applied at creation — never
/// a reused inode) → write → flush → fsync temp → atomic rename →
/// fsync parent directory. The temp file is removed on every
/// pre-rename failure. Unlike the sdk's `RevocationStore`, the
/// parent-dir fsync is a hard requirement here (plan §1.5 locked
/// order).
pub(crate) fn write_atomic_phased(path: &Path, bytes: &[u8]) -> Result<(), WritePhase> {
    let pre = |e: std::io::Error| WritePhase::PreRename(e.to_string());

    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(pre)?;
        }
    }

    // `create_new` + creation-time 0600: an attacker cannot
    // pre-create the (unpredictable) name, and even a collision
    // with a crash-left temp fails loudly instead of truncating a
    // permissive inode. A handful of retries covers the
    // astronomically unlikely name collision.
    let mut tmp = fresh_temp_path(path)?;
    let mut file = None;
    for _ in 0..4 {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        match opts.open(&tmp) {
            Ok(f) => {
                file = Some(f);
                break;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                tmp = fresh_temp_path(path)?;
            }
            Err(e) => return Err(pre(e)),
        }
    }
    let Some(mut f) = file else {
        return Err(WritePhase::PreRename(
            "could not create a fresh temp file after 4 attempts".to_string(),
        ));
    };

    // Any failure before the rename removes the temp so no stale
    // inode accumulates for later reuse.
    let write_result = (|| -> std::io::Result<()> {
        use std::io::Write;
        f.write_all(bytes)?;
        f.flush()?;
        f.sync_all()?;
        Ok(())
    })();
    if let Err(e) = write_result {
        drop(f);
        let _ = std::fs::remove_file(&tmp);
        return Err(pre(e));
    }
    drop(f);

    // Atomic replacement. On Unix, rename(2) atomically replaces
    // an existing destination. On Windows, Rust's std::fs::rename
    // is implemented with MoveFileExW + MOVEFILE_REPLACE_EXISTING,
    // which also replaces an existing destination — no separate
    // ReplaceFileW path is required (std library guarantee since
    // Rust 1.0; see std::fs::rename platform-specific behavior).
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(pre(e));
    }

    // POSIX: rename() updates the directory entry in memory only; a
    // crash before the directory fsyncs can revert to the old file
    // (BUG #93 lineage, mirrors redex/disk.rs). Required, not
    // best-effort — and a failure HERE is post-rename: the caller
    // must treat disk state as unproven (review-8 §13).
    if let Err(e) = fsync_parent_dir(path) {
        return Err(WritePhase::PostRename(e.to_string()));
    }
    Ok(())
}

/// Phase-flattened wrapper for callers whose files carry no
/// published live view (the org-authority config writes): any
/// failure — pre- or post-rename — is an error to surface.
///
/// `pub(crate)`: the org-authority scaffolding (`org_authority.rs`)
/// writes its sibling config files with the same discipline.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), OrgRevocationError> {
    write_atomic_phased(path, bytes).map_err(|phase| match phase {
        WritePhase::PreRename(reason) => OrgRevocationError::Io {
            path: path.display().to_string(),
            reason,
        },
        WritePhase::PostRename(reason) => OrgRevocationError::DurabilityUncertain {
            path: path.display().to_string(),
            reason,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::net::behavior::org::OrgKeypair;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static TEST_DIR_SEQ: AtomicUsize = AtomicUsize::new(0);

    /// Unique per-test scratch dir (house pattern — no tempfile dev-dep).
    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!(
                "net-org-revocation-{}-{}",
                std::process::id(),
                TEST_DIR_SEQ.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&dir).expect("create scratch dir");
            Self(dir)
        }
        fn state_path(&self) -> PathBuf {
            self.0.join("revocation-state.json")
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn org() -> OrgKeypair {
        OrgKeypair::from_bytes([0x42u8; 32])
    }

    fn member() -> EntityId {
        EntityId::from_bytes([0x24u8; 32])
    }

    fn bundle_with_floor(generation: u32) -> OrgRevocationBundle {
        let mut floors = BTreeMap::new();
        floors.insert(member(), generation);
        OrgRevocationBundle::try_issue(&org(), &floors).expect("issue")
    }

    #[test]
    fn init_creates_empty_state_and_open_existing_loads_it() {
        let scratch = Scratch::new();
        let path = scratch.state_path();

        let store = OrgRevocationStore::init(&path).expect("init");
        assert!(store.snapshot().is_empty());
        assert!(path.exists());

        let reopened = OrgRevocationStore::open_existing(&path).expect("open");
        assert!(reopened.snapshot().is_empty());
    }

    #[test]
    fn open_existing_refuses_missing_state() {
        let scratch = Scratch::new();
        let err = OrgRevocationStore::open_existing(scratch.state_path())
            .expect_err("missing file must be loud");
        assert!(matches!(err, OrgRevocationError::MissingState { .. }));
    }

    #[test]
    fn apply_bundle_raises_persists_and_publishes() {
        let scratch = Scratch::new();
        let store = OrgRevocationStore::init(scratch.state_path()).expect("init");

        let raised = store.apply_bundle(&bundle_with_floor(5)).expect("apply");
        assert_eq!(raised, vec![(org().org_id(), member(), 5)]);
        assert_eq!(store.floor_for(&org().org_id(), &member()), 5);

        // Persisted: a fresh open (simulated restart) sees floor 5.
        drop(store);
        let reopened = OrgRevocationStore::open_existing(scratch.state_path()).expect("open");
        assert_eq!(reopened.floor_for(&org().org_id(), &member()), 5);
    }

    /// The OA-1 exit-gate restart witness, verbatim:
    ///
    /// ```text
    /// load floor generation 5 → persist
    /// replace operator bundle with VALID generation 3
    /// restart
    /// → generation 5 remains authoritative
    /// ```
    #[test]
    fn restart_witness_lower_valid_bundle_never_rolls_back() {
        let scratch = Scratch::new();
        let path = scratch.state_path();

        // Load floor generation 5 → persist.
        let store = OrgRevocationStore::init(&path).expect("init");
        store.apply_bundle(&bundle_with_floor(5)).expect("apply 5");
        drop(store);

        // "Replace the operator bundle with VALID generation 3" +
        // restart: the persisted maxima, not the bundle file, is
        // what survives.
        let store = OrgRevocationStore::open_existing(&path).expect("restart");
        let before = std::fs::read(&path).expect("read state");
        let raised = store
            .apply_bundle(&bundle_with_floor(3))
            .expect("valid lower bundle is not an error");
        assert!(raised.is_empty(), "lower floor must not merge");
        assert_eq!(store.floor_for(&org().org_id(), &member()), 5);
        // No-op reload leaves the persisted file byte-identical.
        assert_eq!(std::fs::read(&path).expect("read state"), before);

        // Second restart: generation 5 still authoritative.
        drop(store);
        let store = OrgRevocationStore::open_existing(&path).expect("restart 2");
        assert_eq!(store.floor_for(&org().org_id(), &member()), 5);
    }

    #[test]
    fn corrupt_incoming_bundle_keeps_last_good() {
        let scratch = Scratch::new();
        let store = OrgRevocationStore::init(scratch.state_path()).expect("init");
        store.apply_bundle(&bundle_with_floor(5)).expect("apply 5");

        // Tamper the signature of a higher-generation bundle.
        let mut evil = bundle_with_floor(9);
        evil.signature[0] ^= 1;
        let before = std::fs::read(store.path()).expect("read state");
        let err = store
            .apply_bundle(&evil)
            .expect_err("tampered bundle rejected");
        assert!(matches!(err, OrgRevocationError::InvalidBundle(_)));
        // Live view AND persisted file untouched.
        assert_eq!(store.floor_for(&org().org_id(), &member()), 5);
        assert_eq!(std::fs::read(store.path()).expect("read state"), before);
    }

    #[test]
    fn corrupt_persisted_state_is_loud_at_startup() {
        let scratch = Scratch::new();
        let path = scratch.state_path();
        OrgRevocationStore::init(&path).expect("init");

        std::fs::write(&path, b"{ not json").expect("corrupt");
        let err = OrgRevocationStore::open_existing(&path).expect_err("corrupt is loud");
        assert!(matches!(err, OrgRevocationError::CorruptState { .. }));

        // Unsupported version is equally loud.
        std::fs::write(&path, br#"{"version":99,"floors":[]}"#).expect("write");
        let err = OrgRevocationStore::open_existing(&path).expect_err("version is loud");
        assert!(matches!(
            err,
            OrgRevocationError::UnsupportedVersion { found: 99, .. }
        ));

        // Duplicate (org, member) keys are corruption.
        let org_hex = hex::encode(org().org_id().as_bytes());
        let member_hex = hex::encode(member().as_bytes());
        let dup = format!(
            r#"{{"version":1,"floors":[
                {{"org":"{org_hex}","member":"{member_hex}","floor":1}},
                {{"org":"{org_hex}","member":"{member_hex}","floor":2}}
            ]}}"#
        );
        std::fs::write(&path, dup).expect("write");
        let err = OrgRevocationStore::open_existing(&path).expect_err("dup is loud");
        assert!(matches!(err, OrgRevocationError::CorruptState { .. }));
    }

    #[test]
    fn init_preserves_existing_maxima_on_readopt() {
        let scratch = Scratch::new();
        let path = scratch.state_path();
        let store = OrgRevocationStore::init(&path).expect("init");
        store.apply_bundle(&bundle_with_floor(5)).expect("apply");
        drop(store);

        // Re-running adopt must NOT reset floors to empty.
        let readopted = OrgRevocationStore::init(&path).expect("re-init");
        assert_eq!(readopted.floor_for(&org().org_id(), &member()), 5);
    }

    #[test]
    fn merge_is_per_key_monotone_across_orgs_and_members() {
        let org_a = OrgKeypair::from_bytes([1u8; 32]);
        let org_b = OrgKeypair::from_bytes([2u8; 32]);
        let m1 = EntityId::from_bytes([11u8; 32]);
        let m2 = EntityId::from_bytes([22u8; 32]);

        let mut state = OrgRevocationState::empty();

        let mut floors = BTreeMap::new();
        floors.insert(m1.clone(), 5);
        floors.insert(m2.clone(), 2);
        let a1 = OrgRevocationBundle::try_issue(&org_a, &floors).expect("issue");
        assert_eq!(state.merge_bundle(&a1), 2);

        // Same members under a DIFFERENT org are independent keys.
        let b1 = OrgRevocationBundle::try_issue(&org_b, &floors).expect("issue");
        assert_eq!(state.merge_bundle(&b1), 2);
        assert_eq!(state.floor_for(&org_a.org_id(), &m1), 5);
        assert_eq!(state.floor_for(&org_b.org_id(), &m1), 5);

        // Mixed raise/no-op within one bundle: m1 lower (no-op),
        // m2 higher (raises).
        let mut floors = BTreeMap::new();
        floors.insert(m1.clone(), 3);
        floors.insert(m2.clone(), 7);
        let a2 = OrgRevocationBundle::try_issue(&org_a, &floors).expect("issue");
        assert_eq!(state.merge_bundle(&a2), 1);
        assert_eq!(state.floor_for(&org_a.org_id(), &m1), 5);
        assert_eq!(state.floor_for(&org_a.org_id(), &m2), 7);
        // Unknown keys floor at 0.
        assert_eq!(
            state.floor_for(&org_a.org_id(), &EntityId::from_bytes([99u8; 32])),
            0
        );
    }

    #[cfg(unix)]
    #[test]
    fn persist_failure_never_publishes_the_live_view() {
        let scratch = Scratch::new();
        let path = scratch.state_path();
        let store = OrgRevocationStore::init(&path).expect("init");
        store.apply_bundle(&bundle_with_floor(5)).expect("apply 5");

        // Force the atomic rename to fail: replace the state file
        // with a non-empty DIRECTORY at the same path.
        std::fs::remove_file(&path).expect("remove");
        std::fs::create_dir(&path).expect("dir at path");
        std::fs::write(path.join("occupied"), b"x").expect("occupy");

        let err = store
            .apply_bundle(&bundle_with_floor(9))
            .expect_err("rename onto non-empty dir must fail");
        assert!(matches!(err, OrgRevocationError::Io { .. }));
        // The live view still serves the last DURABLE floor — the
        // undurable 9 is never enforced. Pre-rename failure does
        // NOT poison: nothing on disk changed.
        assert_eq!(store.floor_for(&org().org_id(), &member()), 5);
        assert!(!store.is_poisoned());

        // No temp files left behind by the failed write.
        let leftovers: Vec<_> = std::fs::read_dir(&scratch.0)
            .expect("read scratch")
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp."))
            .collect();
        assert!(leftovers.is_empty(), "stale temps: {leftovers:?}");
    }

    fn bundle_for(member: EntityId, generation: u32) -> OrgRevocationBundle {
        let mut floors = BTreeMap::new();
        floors.insert(member, generation);
        OrgRevocationBundle::try_issue(&org(), &floors).expect("issue")
    }

    /// Review-8 §5 witness: two store instances on one file. B's
    /// in-memory view is stale when it writes; the under-lock
    /// REREAD must preserve A's floor — both maxima survive in the
    /// persisted state and in B's republished live view.
    #[test]
    fn concurrent_store_instances_preserve_all_maxima() {
        let scratch = Scratch::new();
        let path = scratch.state_path();
        let member_x = EntityId::from_bytes([0xAAu8; 32]);
        let member_y = EntityId::from_bytes([0xBBu8; 32]);

        let store_a = OrgRevocationStore::init(&path).expect("init A");
        let store_b = OrgRevocationStore::open_existing(&path).expect("open B");

        // A raises member_x to 5; B has not observed it.
        store_a
            .apply_bundle(&bundle_for(member_x.clone(), 5))
            .expect("A applies x=5");
        assert_eq!(store_b.floor_for(&org().org_id(), &member_x), 0);

        // B (stale snapshot) raises member_y to 7. Without the
        // reread this write would roll x=5 out of the file.
        let raised = store_b
            .apply_bundle(&bundle_for(member_y.clone(), 7))
            .expect("B applies y=7");

        // The persisted state carries BOTH maxima…
        let reopened = OrgRevocationStore::open_existing(&path).expect("reopen");
        assert_eq!(reopened.floor_for(&org().org_id(), &member_x), 5);
        assert_eq!(reopened.floor_for(&org().org_id(), &member_y), 7);
        // …and B's live view synced up to A's floor during the
        // locked reload, reporting the cross-writer raise too.
        assert_eq!(store_b.floor_for(&org().org_id(), &member_x), 5);
        assert!(raised.contains(&(org().org_id(), member_x, 5)));
        assert!(raised.contains(&(org().org_id(), member_y, 7)));
    }

    /// Review-8 §9 plumbing: the raise callback fires with exactly
    /// the raised floors, and never for a no-op (lower) bundle.
    #[test]
    fn raise_callback_fires_only_on_raises() {
        let scratch = Scratch::new();
        let store = OrgRevocationStore::init(scratch.state_path()).expect("init");

        let seen: Arc<Mutex<Vec<RaisedFloor>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = seen.clone();
        store.set_on_floors_raised(move |raised| {
            sink.lock().extend_from_slice(raised);
        });

        store.apply_bundle(&bundle_with_floor(5)).expect("apply 5");
        assert_eq!(*seen.lock(), vec![(org().org_id(), member(), 5)]);

        seen.lock().clear();
        store.apply_bundle(&bundle_with_floor(3)).expect("apply 3");
        assert!(seen.lock().is_empty(), "lower bundle must not notify");
    }

    /// Review-8 §13 + review-9 witness: a POST-rename parent-fsync
    /// failure publishes the merged (never-weaker) view and poisons
    /// the BACKING PATH — every same-path instance refuses until an
    /// explicit recovery (locked reread + successful parent-dir
    /// fsync) proves the directory entry durable.
    #[cfg(unix)]
    #[test]
    fn post_rename_fsync_failure_poisons_the_path_until_recovery() {
        use std::os::unix::fs::PermissionsExt;
        let scratch = Scratch::new();
        let path = scratch.state_path();
        let store = OrgRevocationStore::init(&path).expect("init");
        // A second instance on the SAME path, opened before the
        // failure — path-wide poison must gate it too (review-9).
        let sibling = OrgRevocationStore::open_existing(&path).expect("sibling");
        store.apply_bundle(&bundle_with_floor(5)).expect("apply 5");

        // Write+execute but NO read on the parent: lookups, file
        // reads, temp creation, and the rename all still work, but
        // opening the directory for fsync needs read — the exact
        // post-rename failure.
        std::fs::set_permissions(&scratch.0, std::fs::Permissions::from_mode(0o300))
            .expect("chmod 0300");
        let err = store
            .apply_bundle(&bundle_with_floor(9))
            .expect_err("dir fsync must fail");
        assert!(
            matches!(err, OrgRevocationError::DurabilityUncertain { .. }),
            "got: {err}"
        );

        // Fail-closed in the never-weaker direction: the merged
        // floor IS enforced (the rename landed; the disk may hold
        // it), but no same-path instance may pretend disk and
        // memory are synchronized while recovery is impossible.
        assert_eq!(store.floor_for(&org().org_id(), &member()), 9);
        assert!(store.is_poisoned());
        assert!(
            sibling.is_poisoned(),
            "poison is path-wide, not per instance"
        );
        let err = store
            .apply_bundle(&bundle_with_floor(11))
            .expect_err("originating store refuses while recovery fails");
        assert!(matches!(err, OrgRevocationError::Poisoned { .. }));
        // The SIBLING's no-op-shaped lower apply must equally refuse
        // — this is the review-9 red (it previously returned Ok).
        let err = sibling
            .apply_bundle(&bundle_with_floor(3))
            .expect_err("sibling refuses while the path is uncertain");
        assert!(matches!(err, OrgRevocationError::Poisoned { .. }));
        // A NEWLY OPENED instance cannot launder the uncertainty
        // either: its open attempts recovery, which still fails.
        assert!(
            OrgRevocationStore::open_existing(&path).is_err(),
            "fresh open must not bypass path poison while recovery fails"
        );

        // Once the environment is repaired, the next operation
        // performs explicit recovery (locked reread + successful
        // parent fsync) and clears the uncertainty.
        std::fs::set_permissions(&scratch.0, std::fs::Permissions::from_mode(0o700))
            .expect("chmod back");
        let raised = sibling
            .apply_bundle(&bundle_with_floor(11))
            .expect("recovered apply succeeds");
        assert!(raised.contains(&(org().org_id(), member(), 11)));
        assert!(!store.is_poisoned(), "recovery clears the path-wide bit");
        let reopened = OrgRevocationStore::open_existing(&path).expect("reopen");
        assert_eq!(reopened.floor_for(&org().org_id(), &member()), 11);
    }

    /// Review-9: raise callbacks run OUTSIDE both the file lock and
    /// the instance reload guard — a callback that synchronously
    /// re-enters `apply_bundle` on the same store must not
    /// deadlock.
    #[test]
    fn reentrant_callback_does_not_deadlock() {
        let scratch = Scratch::new();
        let store = Arc::new(OrgRevocationStore::init(scratch.state_path()).expect("init"));

        let reentered = Arc::new(Mutex::new(false));
        let store_for_callback = Arc::downgrade(&store);
        let flag = reentered.clone();
        store.set_on_floors_raised(move |raised| {
            // Re-enter once, from the first raise only.
            if raised.iter().any(|(_, _, floor)| *floor == 5) {
                if let Some(store) = store_for_callback.upgrade() {
                    store
                        .apply_bundle(&bundle_with_floor(7))
                        .expect("re-entrant apply must not deadlock");
                    *flag.lock() = true;
                }
            }
        });

        store.apply_bundle(&bundle_with_floor(5)).expect("apply 5");
        assert!(*reentered.lock(), "callback re-entered apply_bundle");
        assert_eq!(store.floor_for(&org().org_id(), &member()), 7);
    }

    /// Review-9 filesystem policy: state files and the lock sidecar
    /// are opened no-follow — a planted symlink is refused, not
    /// followed.
    #[cfg(unix)]
    #[test]
    fn symlinked_state_and_lock_files_are_refused() {
        let scratch = Scratch::new();
        let path = scratch.state_path();
        let store = OrgRevocationStore::init(&path).expect("init");
        store.apply_bundle(&bundle_with_floor(5)).expect("apply 5");
        drop(store);

        // Symlinked STATE file: reads refuse.
        let real = scratch.0.join("elsewhere.json");
        std::fs::rename(&path, &real).expect("move state");
        std::os::unix::fs::symlink(&real, &path).expect("plant symlink");
        assert!(
            OrgRevocationStore::open_existing(&path).is_err(),
            "symlinked state file must refuse"
        );
        std::fs::remove_file(&path).expect("remove link");
        std::fs::rename(&real, &path).expect("restore state");
        OrgRevocationStore::open_existing(&path).expect("regular file opens");

        // Symlinked LOCK sidecar: locking refuses rather than
        // following the link to a foreign inode.
        let mut lock_path = path.as_os_str().to_os_string();
        lock_path.push(".lock");
        let lock_path = PathBuf::from(lock_path);
        let _ = std::fs::remove_file(&lock_path);
        let foreign = scratch.0.join("foreign.lock");
        std::fs::write(&foreign, b"").expect("foreign lock");
        std::os::unix::fs::symlink(&foreign, &lock_path).expect("plant lock symlink");
        let store = OrgRevocationStore::open_existing(&path).expect("open");
        assert!(
            store.apply_bundle(&bundle_with_floor(9)).is_err(),
            "symlinked lock sidecar must refuse"
        );
    }
}
