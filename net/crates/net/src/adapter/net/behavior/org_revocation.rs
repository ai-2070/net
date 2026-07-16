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
//! the operator are inputs, never this file. Reloads are serialized
//! by an in-process lock. (`net node adopt` also touches the file,
//! but adoption happens before the node runs.)

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
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
    /// Filesystem failure while reading or durably writing.
    Io {
        /// The path being read or written.
        path: String,
        /// The underlying I/O error.
        reason: String,
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
        }
    }
}

impl std::error::Error for OrgRevocationError {}

/// The node-local persisted revocation maxima plus its published
/// live view. See the module docs for the locked reload order and
/// failure semantics.
///
/// The store is org-agnostic (keys are `(OrgId, EntityId)`); WHICH
/// bundles get fed to [`Self::apply_bundle`] is the caller's trust
/// decision — in OA-1 the adopt/startup wiring feeds only the
/// node's owner-org bundle.
pub struct OrgRevocationStore {
    path: PathBuf,
    /// Serializes merge→persist→publish sequences.
    reload: Mutex<()>,
    /// The published live view. Always equal to the last
    /// successfully persisted state — never ahead of the disk.
    live: RwLock<Arc<OrgRevocationState>>,
}

impl OrgRevocationStore {
    /// Adopt-time entry point: load the existing file if present
    /// (re-adoption preserves maxima — monotonicity survives even
    /// an operator re-running adopt), otherwise durably create an
    /// empty state file.
    pub fn init(path: impl Into<PathBuf>) -> Result<Self, OrgRevocationError> {
        let path = path.into();
        let state = match std::fs::read(&path) {
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
            reload: Mutex::new(()),
            live: RwLock::new(Arc::new(state)),
        })
    }

    /// Startup entry point: the file MUST exist and parse. Missing
    /// or corrupt → loud typed error; protected verification never
    /// starts against silently weaker floors.
    pub fn open_existing(path: impl Into<PathBuf>) -> Result<Self, OrgRevocationError> {
        let path = path.into();
        let bytes = match std::fs::read(&path) {
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
            reload: Mutex::new(()),
            live: RwLock::new(Arc::new(state)),
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

    /// Apply an operator bundle under the locked reload order.
    ///
    /// Returns the number of floors raised. `Ok(0)` means the
    /// bundle was valid but every floor was at or below the
    /// persisted maxima — nothing written, nothing republished
    /// (a lower bundle never rolls back).
    ///
    /// On ANY error the persisted last-good state and the live view
    /// are both untouched.
    pub fn apply_bundle(&self, bundle: &OrgRevocationBundle) -> Result<usize, OrgRevocationError> {
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

        // 2. Merge maxima with the PERSISTED state (the live view
        //    mirrors it by construction — never ahead of disk).
        let mut merged = (*self.snapshot()).clone();
        let raised = merged.merge_bundle(bundle);
        if raised == 0 {
            return Ok(0);
        }

        // 3. Atomically persist the merged maxima. Failure here
        //    leaves the old file (rename is atomic) and the old
        //    live view — a floor the disk could forget is never
        //    enforced.
        write_atomic(&self.path, &merged.to_file_bytes()?)?;

        // 4. ONLY THEN publish the new live view.
        *self.live.write() = Arc::new(merged);
        Ok(raised)
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

/// Durable atomic write: temp file (owner-only on Unix) → fsync
/// temp → atomic rename → fsync parent directory. Unlike the sdk's
/// `RevocationStore`, the parent-dir fsync is a hard requirement
/// here (plan §1.5 locked order): if the directory entry isn't
/// durable, the caller must not publish state the filesystem could
/// forget.
///
/// `pub(crate)`: the org-authority scaffolding (`org_authority.rs`)
/// writes its sibling config files with the same discipline.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), OrgRevocationError> {
    let io = |e: std::io::Error| OrgRevocationError::Io {
        path: path.display().to_string(),
        reason: e.to_string(),
    };

    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(io)?;
        }
    }

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
        f.write_all(bytes).map_err(io)?;
        f.flush().map_err(io)?;
        f.sync_all().map_err(io)?;
    }
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        io(e)
    })?;

    // POSIX: rename() updates the directory entry in memory only; a
    // crash before the directory fsyncs can revert to the old file
    // (BUG #93 lineage, mirrors redex/disk.rs). Required, not
    // best-effort — see the function docs.
    #[cfg(unix)]
    {
        let dir = match path.parent() {
            Some(p) if !p.as_os_str().is_empty() => p,
            _ => Path::new("."),
        };
        let dirf = std::fs::File::open(dir).map_err(io)?;
        dirf.sync_all().map_err(io)?;
    }
    Ok(())
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
        assert_eq!(raised, 1);
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
        assert_eq!(raised, 0, "lower floor must not merge");
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
        // undurable 9 is never enforced.
        assert_eq!(store.floor_for(&org().org_id(), &member()), 5);
    }
}
