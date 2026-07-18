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
//!
//! # One path, one security view
//!
//! Within a process, every [`OrgRevocationStore`] handle backed by
//! the same NORMALIZED pathname shares one [`StoreCore`] (review-9
//! addendum): one live floor view, one reload/publish transaction
//! lock, one publish generation, and one subscriber registry. A
//! same-path sibling therefore observes a raise the instant it is
//! published — one backing file is never modeled as several
//! independent security views glued to a shared poison boolean.
//! Opens ALWAYS serialize behind the interprocess state lock (no
//! pre-lock poison fast path), and durability recovery rereads and
//! republishes the persisted state through the shared core BEFORE
//! the path-wide poison bit clears.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};

use parking_lot::{Condvar, Mutex, RwLock};
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
    /// the BACKING PATH: same-path operations are refused until
    /// recovery — a locked reread republished through the shared
    /// core plus a SUCCESSFUL parent-directory fsync —
    /// re-establishes ground truth (review-8 §13, review-9). A
    /// restart is one route to that recovery, not the contract.
    DurabilityUncertain {
        /// The state file's path.
        path: String,
        /// The underlying fsync error.
        reason: String,
    },
    /// A previous apply ended post-rename durability-uncertain
    /// (see [`Self::DurabilityUncertain`]) and recovery has not yet
    /// succeeded; same-path reloads and opens are refused until a
    /// locked reread plus a successful parent-directory fsync
    /// clears the uncertainty.
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
    /// R2-4: this backing path is already bound, for the lifetime of a
    /// live core, to a DIFFERENT `.lock` sidecar identity than the one
    /// just opened — the sidecar was recreated or replaced underneath a
    /// core that same-path siblings still hold. Joining under the new
    /// identity would fork the path into two independent security views,
    /// so it is refused loudly.
    BackingIdentityConflict {
        /// The normalized state-file path whose sidecar identity changed.
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
                 synchronized — path poisoned until a locked reread and a successful \
                 parent-directory fsync recover it"
            ),
            Self::Poisoned { path } => write!(
                f,
                "revocation store path {path} is poisoned after a durability-uncertain \
                 write; recovery requires a locked reread republished through the \
                 shared store plus a successful parent-directory fsync (restarting the \
                 process is one route, not the requirement)"
            ),
            Self::NonMonotonicReplacement { path } => write!(
                f,
                "refusing to replace the installed revocation store with {path}: its \
                 live view is lower on at least one (org, member) floor — an installed \
                 floor never lowers; apply a bundle instead"
            ),
            Self::BackingIdentityConflict { path } => write!(
                f,
                "revocation store path {path} is bound to a different .lock sidecar \
                 identity than the one just opened — the sidecar was recreated or \
                 replaced while a same-path core is still live; refusing to fork the \
                 path into two independent security views"
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

/// The process-wide state shared by every [`OrgRevocationStore`]
/// handle backed by one normalized path (review-9 addendum): ONE
/// live view, ONE reload/publish transaction lock, ONE publish
/// generation, ONE subscriber registry. Handles are cheap facades;
/// the core is the security object.
struct StoreCore {
    /// The NORMALIZED backing path — used for reads / writes / the
    /// interprocess lock. NOT the registry key (that is [`BackingId`],
    /// so case-aliases collapse — AV-9).
    path: PathBuf,
    /// Stable backing-file identity (the `.lock` sidecar inode) — the
    /// key in the core and poison registries (AV-9).
    backing_id: BackingId,
    /// Serializes merge→persist→publish transactions in-process
    /// (the sidecar file lock serializes across processes). Also
    /// exposed to the node as [`PublishGuard`] so store
    /// replacement and authority installation can pin the live
    /// view across their check-then-swap sections.
    reload: Mutex<()>,
    /// The one published live view every same-path handle shares.
    /// Never ahead of the durably persisted state.
    live: RwLock<Arc<OrgRevocationState>>,
    /// Bumped on every publish; lets callers order publications.
    generation: AtomicU64,
    /// Raise subscribers, each with a removable token. A REGISTRY,
    /// not a single slot (review-9 addendum): registering a second
    /// observer must never silently steal the first one's
    /// notifications.
    subscribers: RwLock<Vec<(u64, FloorsRaisedCallback)>>,
    /// Token source for [`Self::subscribers`].
    next_subscriber: AtomicU64,
    /// Test-only pause fired ONCE, from inside [`Self::publish`],
    /// AFTER the live-view swap and BEFORE the generation bump, while
    /// `live.write()` is still held. Lets a witness deterministically
    /// occupy the exact "new view installed, old generation still
    /// present" window the barriered readers must not observe.
    /// Always `None` in production (armed only by
    /// [`OrgRevocationStore::arm_publish_pause_for_test`], a
    /// `#[doc(hidden)]` seam mirroring the review-11 `*_paused_for_test`
    /// hooks); the per-publish check is an uncontended `Mutex::take`.
    publish_pause: parking_lot::Mutex<Option<PublishPauseHook>>,
}

/// The one-shot hook a test installs to pause [`StoreCore::publish`]
/// between the view swap and the generation bump.
struct PublishPauseHook {
    /// Signalled once the view is swapped and the pause begins.
    swapped: std::sync::mpsc::Sender<()>,
    /// Blocks the publisher until the test releases it.
    resume: std::sync::mpsc::Receiver<()>,
}

impl StoreCore {
    /// Swap the live view to `next`, returning every floor that
    /// rose relative to the previously published view. `next` is
    /// always a monotone superset under the locked reload order;
    /// the per-key max with the outgoing view makes "an installed
    /// floor never lowers" structural rather than assumed.
    fn publish(&self, mut next: OrgRevocationState) -> Vec<RaisedFloor> {
        let mut live = self.live.write();
        for ((org, member), floor) in live.iter() {
            let entry = next.floors.entry((*org, member.clone())).or_insert(0);
            if *floor > *entry {
                *entry = *floor;
            }
        }
        let raised: Vec<RaisedFloor> = next
            .iter()
            .filter(|((org, member), floor)| **floor > live.floor_for(org, member))
            .map(|((org, member), floor)| (*org, member.clone(), *floor))
            .collect();
        *live = Arc::new(next);
        // Occupy the "new view installed, old generation still present"
        // window while `live` (the write guard) is held, so a witness
        // can prove the barriered readers never observe it. A no-op
        // (one uncontended `Mutex::take`) unless a test armed the hook.
        self.run_publish_pause_hook();
        self.generation.fetch_add(1, Ordering::Release);
        raised
    }

    /// Fire the one-shot publish pause hook if a test installed one.
    /// Runs while the caller holds `live.write()`.
    fn run_publish_pause_hook(&self) {
        if let Some(hook) = self.publish_pause.lock().take() {
            let _ = hook.swapped.send(());
            let _ = hook.resume.recv();
        }
    }

    /// Notify every subscriber of `raised`. Callers invoke this
    /// OUTSIDE both the file lock and the reload lock — re-entrant
    /// callbacks must not deadlock (review-9).
    fn notify(&self, raised: &[RaisedFloor]) {
        if raised.is_empty() {
            return;
        }
        let subscribers: Vec<FloorsRaisedCallback> = self
            .subscribers
            .read()
            .iter()
            .map(|(_, callback)| callback.clone())
            .collect();
        for callback in subscribers {
            callback(raised);
        }
    }

    /// Remove the subscriber registered under `token`. Unknown tokens
    /// are a no-op. Called by [`RaiseSubscription`]'s Drop through a
    /// `Weak<StoreCore>` (R2-2), so a subscription is retired
    /// deterministically by dropping its guard — never dependent on a
    /// facade `Drop` a capture cycle could keep from running.
    fn remove_subscriber(&self, token: u64) {
        self.subscribers.write().retain(|(t, _)| *t != token);
    }
}

/// The exclusion lease shared between one raise subscription's wrapped
/// callback and its [`RaiseSubscription`] guard (R2-3). It is the
/// re-entrancy-safe "in-flight lease drained by teardown" variant: the
/// wrapped callback registers itself as in-flight for the *duration of the
/// user callback* (never holding the lease's own lock across it, so a
/// re-entrant `apply_bundle` cannot self-deadlock), and teardown marks the
/// lease dead and BLOCKS until every in-flight callback has left.
///
/// Guarantees, jointly:
/// - a callback that has passed the liveness check and is mid-mutation
///   keeps teardown blocked until it finishes (no torn retraction);
/// - once teardown has marked the lease dead, no *new* callback body runs
///   — including one already snapshotted by [`StoreCore::notify`] outside
///   the registry lock, or a re-entrant one.
struct SubscriptionLease {
    state: Mutex<LeaseState>,
    /// Signalled when `in_flight` reaches zero, so a draining teardown
    /// wakes exactly when the last in-flight callback leaves.
    drained: Condvar,
}

struct LeaseState {
    /// Set once by teardown; gates every subsequent callback entry.
    dead: bool,
    /// Count of callback bodies currently executing under this lease.
    in_flight: usize,
}

thread_local! {
    /// Leases whose callback body the CURRENT thread is executing (R3-4).
    /// Pushed by the wrapped callback on entry, popped on leave. A guard
    /// dropped from INSIDE its own callback consults this so
    /// [`SubscriptionLease::kill_and_drain`] does not wait for the very
    /// frame that is dropping it (which would self-deadlock). Raw pointers
    /// are only compared for identity and are only ever present while the
    /// callback holds a live `Arc` to that lease, so there is no
    /// use-after-free.
    static ACTIVE_LEASES: std::cell::RefCell<Vec<*const SubscriptionLease>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

impl SubscriptionLease {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(LeaseState {
                dead: false,
                in_flight: 0,
            }),
            drained: Condvar::new(),
        })
    }

    /// Enter the callback body: `true` if admitted (caller MUST pair with
    /// [`Self::leave`]), `false` if the lease is dead (caller returns
    /// without running the user callback). The lease lock is held only for
    /// this check-and-count, never across the user callback itself.
    fn enter(self: &Arc<Self>) -> bool {
        {
            let mut st = self.state.lock();
            if st.dead {
                return false;
            }
            st.in_flight += 1;
        }
        // R3-4: record this thread as executing under this lease, so a
        // self-drop from inside the callback does not drain-wait for its
        // own frame.
        let ptr = Arc::as_ptr(self);
        ACTIVE_LEASES.with(|a| a.borrow_mut().push(ptr));
        true
    }

    /// Leave the callback body, waking a draining teardown if this was the
    /// last in-flight callback.
    fn leave(self: &Arc<Self>) {
        let ptr = Arc::as_ptr(self);
        ACTIVE_LEASES.with(|a| {
            let mut v = a.borrow_mut();
            if let Some(i) = v.iter().rposition(|&p| p == ptr) {
                v.remove(i);
            }
        });
        let mut st = self.state.lock();
        st.in_flight -= 1;
        if st.in_flight == 0 {
            self.drained.notify_all();
        }
    }

    /// Teardown: mark the lease dead so no NEW callback body starts, then —
    /// only when the caller holds none of this lease's own frames — block
    /// until every in-flight callback has left.
    ///
    /// - External teardown (the common case: the guard is dropped from a
    ///   thread that is NOT inside this callback) has `own_frames == 0`, so
    ///   it BLOCKS until `in_flight` reaches zero — the strong guarantee that
    ///   no callback is in flight when the guard's `Drop` returns. `leave`
    ///   signals `drained` at exactly that boundary.
    /// - Self-unsubscription (the guard is dropped from INSIDE one or more of
    ///   this lease's own callback frames, `own_frames > 0`) does NOT wait at
    ///   all. Waiting would be wrong for two independent reasons (R3-4): this
    ///   thread's own frame(s) cannot reach their `LeaveOnDrop` until this drop
    ///   returns, so waiting for them self-deadlocks; and a callback of the
    ///   SAME lease running on ANOTHER thread may be blocked on a user lock
    ///   THIS callback still holds, so waiting for that foreign frame would
    ///   deadlock across threads. Setting `dead` first stops every new
    ///   (including re-entrant) callback; the current frame's `LeaveOnDrop`
    ///   retires the subscription and each in-flight frame — own and foreign —
    ///   retires through its own `LeaveOnDrop` once it finishes. Return
    ///   immediately: non-blocking and non-reentrant.
    fn kill_and_drain(self: &Arc<Self>) {
        let ptr = Arc::as_ptr(self);
        let own_frames = ACTIVE_LEASES.with(|a| a.borrow().iter().filter(|&&p| p == ptr).count());
        let mut st = self.state.lock();
        st.dead = true;
        if own_frames > 0 {
            // Self-unsubscription: never wait (see doc — self- and cross-thread
            // deadlock). `dead` gates new entries; live frames self-retire.
            return;
        }
        while st.in_flight > 0 {
            self.drained.wait(&mut st);
        }
    }
}

/// An externally-owned RAII handle to one raise subscription (R2-2 +
/// R2-3). Dropping it:
/// 1. marks the exclusion lease dead and drains any in-flight callback
///    ([`SubscriptionLease::kill_and_drain`]), then
/// 2. removes the callback from the shared core's registry via a
///    `Weak<StoreCore>`.
///
/// Because removal goes through the `Weak` — not the owning
/// [`OrgRevocationStore`] facade's `Drop` — a
/// `core → callback → Arc<store> → core` capture cycle that keeps the
/// facade alive can no longer strand the callback in the core: whoever
/// holds this guard (the node, a sibling handle) retires the subscription
/// by dropping it.
#[must_use = "dropping the RaiseSubscription immediately unsubscribes and drains the callback"]
pub struct RaiseSubscription {
    core: Weak<StoreCore>,
    token: u64,
    lease: Arc<SubscriptionLease>,
}

impl Drop for RaiseSubscription {
    fn drop(&mut self) {
        // R2-3: block until no callback observed live is still mutating,
        // and stop any snapshotted-but-not-yet-run callback, BEFORE the
        // token is removed.
        self.lease.kill_and_drain();
        // R2-2: retire through the Weak core, independent of the facade.
        if let Some(core) = self.core.upgrade() {
            core.remove_subscriber(self.token);
        }
    }
}

/// A stable identity for a store's backing file, derived from the
/// OPENED `.lock` sidecar's inode (AV-9 item 9). The sidecar is created
/// once and NEVER renamed — only the state file is rename-replaced by
/// `write_atomic`, so the sidecar's inode is stable across every write,
/// and two differently-cased path aliases (`revocation-state.json` vs
/// `REVOCATION-STATE.JSON`) resolve to the SAME sidecar inode on a
/// case-insensitive filesystem. Keying the core and poison registries
/// on this — rather than the literal-cased normalized path — collapses
/// those aliases to one core (shared live view + publish lock) and one
/// poison entry, while `normalize_backing_path` / `open_lock_file`
/// still refuse a symlinked or non-regular final component.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum BackingId {
    /// Filesystem file-identity: the unix `(device, inode)` or windows
    /// `(volume_serial, file_index)` of the OPENED `.lock` sidecar. Two
    /// differently-cased path aliases of one sidecar share this.
    FileId { device: u64, inode: u64 },
    /// Last-resort key on a platform with no file-identity API (or if a Unix
    /// `fstat` fails): the FULL normalized path. R2-4 — never a lossy 64-bit
    /// path hash, so two distinct paths can NEVER collide onto one core or
    /// poison entry (the pre-R2-4 `DefaultHasher` fallback could, at the
    /// ~2^32 birthday bound). Never constructed on Windows: a missing file
    /// identity there fails loud (see [`BackingId::of`]) rather than degrading
    /// to a case-sensitive literal path.
    #[cfg_attr(windows, allow(dead_code))]
    Path(PathBuf),
}

impl BackingId {
    /// Derive the identity from the opened lock sidecar. `path` (already
    /// normalized by the caller) backs the fallback key on a platform with
    /// no file-identity API, or if a Unix `fstat` somehow fails.
    ///
    /// On Windows the identity comes from the stable `GetFileInformationByHandle`
    /// Win32 call: the `std` `MetadataExt::{volume_serial_number, file_index}`
    /// accessors require the unstable `windows_by_handle` feature and do NOT
    /// build on stable Rust. A Windows identity read that fails is a LOUD
    /// error — never a silent degradation to a case-sensitive literal path,
    /// which would reopen AV-9 by letting two differently-cased aliases of one
    /// sidecar key two DISTINCT cores.
    fn of(lock: &std::fs::File, path: &Path) -> Result<Self, OrgRevocationError> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            // `fstat` on an open fd effectively never fails; degrade to the
            // full-path fallback key (R2-4) if it somehow does.
            Ok(match lock.metadata() {
                Ok(meta) => BackingId::FileId {
                    device: meta.dev(),
                    inode: meta.ino(),
                },
                Err(_) => BackingId::Path(path.to_path_buf()),
            })
        }
        #[cfg(windows)]
        {
            // Stable Win32 `(dwVolumeSerialNumber, nFileIndex)` identity; a
            // read failure fails loud rather than degrading to a literal path.
            match windows_file_identity(lock) {
                Ok((device, inode, _links)) => Ok(BackingId::FileId { device, inode }),
                Err(e) => Err(OrgRevocationError::Io {
                    path: path.display().to_string(),
                    reason: format!("state lock: cannot read Windows file identity: {e}"),
                }),
            }
        }
        // Fallback key (R2-4: the FULL normalized path, never a lossy 64-bit
        // hash) on a platform with no file-identity API.
        #[cfg(not(any(unix, windows)))]
        {
            let _ = lock;
            Ok(BackingId::Path(path.to_path_buf()))
        }
    }
}

/// Read the stable Win32 `BY_HANDLE_FILE_INFORMATION` for an open handle and
/// return `(volume serial, file index, hard-link count)`.
///
/// `std`'s equivalents (`MetadataExt::volume_serial_number` / `file_index`,
/// and any link count at all) require the unstable `windows_by_handle`
/// feature and do not build on stable Rust, and there is no Win32 bindings
/// crate in this workspace — so we declare the one call we need directly, the
/// same hand-rolled `extern "system"` idiom the crate already uses elsewhere.
#[cfg(windows)]
fn windows_file_identity(file: &std::fs::File) -> std::io::Result<(u64, u64, u32)> {
    use std::os::windows::io::AsRawHandle;

    // `BY_HANDLE_FILE_INFORMATION`; `FILETIME` is two `DWORD`s. `#[repr(C)]`
    // so the field offsets match the Win32 ABI exactly.
    #[repr(C)]
    #[derive(Default)]
    struct ByHandleFileInformation {
        dw_file_attributes: u32,
        ft_creation_time: [u32; 2],
        ft_last_access_time: [u32; 2],
        ft_last_write_time: [u32; 2],
        dw_volume_serial_number: u32,
        n_file_size_high: u32,
        n_file_size_low: u32,
        n_number_of_links: u32,
        n_file_index_high: u32,
        n_file_index_low: u32,
    }

    extern "system" {
        fn GetFileInformationByHandle(
            h_file: *mut std::ffi::c_void,
            lp_file_information: *mut ByHandleFileInformation,
        ) -> i32;
    }

    let mut info = ByHandleFileInformation::default();
    // SAFETY: `file` owns a valid, open handle for the duration of this call,
    // and `info` is a live, correctly-sized, writable output buffer.
    // `as_raw_handle()` is already `*mut c_void` — the exact `h_file` type.
    let ok = unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) };
    if ok == 0 {
        return Err(std::io::Error::last_os_error());
    }
    let volume = u64::from(info.dw_volume_serial_number);
    let index = (u64::from(info.n_file_index_high) << 32) | u64::from(info.n_file_index_low);
    Ok((volume, index, info.n_number_of_links))
}

/// Process-wide core registry (AV-9 + R2-4). `cores` maps a backing
/// file's stable [`BackingId`] to its live core (`Weak`, so a backing
/// file whose handles all dropped releases its core; the POISON registry
/// is separate precisely because poison must outlive every handle).
///
/// `bindings` (R2-4) maps a normalized backing PATH to the sidecar
/// identity currently bound to it. It is GC'd in lockstep with dead cores
/// (a binding whose id has no live core is dropped), so a *surviving*
/// binding always names a LIVE core — a path resolving to a different
/// identity while its binding survives means the sidecar was recreated or
/// replaced under a still-held core, which
/// [`join_or_create_core`] refuses loudly.
struct CoreRegistry {
    cores: std::collections::HashMap<BackingId, std::sync::Weak<StoreCore>>,
    bindings: std::collections::HashMap<PathBuf, BackingId>,
}

static CORES: std::sync::OnceLock<Mutex<CoreRegistry>> = std::sync::OnceLock::new();

fn core_registry() -> &'static Mutex<CoreRegistry> {
    CORES.get_or_init(|| {
        Mutex::new(CoreRegistry {
            cores: std::collections::HashMap::new(),
            bindings: std::collections::HashMap::new(),
        })
    })
}

/// Join the existing core for `path`, republishing the state just
/// reread from disk through it (a same-path sibling's live view
/// advances BEFORE any poison clears — review-9 addendum), or
/// create a fresh core seeded with that state. The caller MUST
/// hold the interprocess state lock, which is what makes the
/// reread current.
///
/// The republish through an EXISTING core takes that core's
/// `reload` lock (review-11 P1): every `StoreCore::publish` — not
/// only `apply_bundle`'s — must hold `reload`, or a replacement
/// holding [`PublishGuard`] could be racing an opener that
/// publishes a stronger floor between the guard's dominance
/// comparison and its swap. The canonical order is interprocess
/// file lock (already held by the caller) OUTER, `reload` INNER;
/// [`OrgRevocationStore::apply_bundle`] obeys the same order, and
/// a replacement holds only `reload` (never the file lock), so no
/// cycle exists. The registry lock is released before `reload` is
/// acquired so no `registry → reload` nesting can form.
fn join_or_create_core(
    backing_id: BackingId,
    path: &Path,
    disk: OrgRevocationState,
) -> Result<(Arc<StoreCore>, Vec<RaisedFloor>), OrgRevocationError> {
    let mut guard = core_registry().lock();
    // Reborrow as `&mut CoreRegistry` so disjoint field borrows (immutable
    // `cores`, mutable `bindings`) are allowed — a `MutexGuard`'s Deref
    // would otherwise borrow the whole guard.
    let reg = &mut *guard;
    // GC dead cores AND the bindings that named them, in lockstep: after
    // this, every surviving binding points at a LIVE core.
    reg.cores.retain(|_, weak| weak.strong_count() > 0);
    let live_ids = &reg.cores;
    reg.bindings.retain(|_, id| live_ids.contains_key(id));
    // R2-4 binding check: if this path is already bound (to a still-live
    // core) under a DIFFERENT sidecar identity, the sidecar was recreated
    // or replaced underneath that core — refuse loudly rather than fork
    // the path into two independent security views. A legitimate
    // recreation (the old core fully dropped) left no surviving binding,
    // so it falls through and rebinds.
    if let Some(bound) = reg.bindings.get(path) {
        if *bound != backing_id {
            return Err(OrgRevocationError::BackingIdentityConflict {
                path: path.display().to_string(),
            });
        }
    }
    reg.bindings.insert(path.to_path_buf(), backing_id.clone());
    let existing = reg
        .cores
        .get(&backing_id)
        .and_then(std::sync::Weak::upgrade);
    if let Some(core) = existing {
        drop(guard);
        let raised = {
            let _reload = core.reload.lock();
            core.publish(disk)
        };
        return Ok((core, raised));
    }
    // Fresh core: nobody else can observe it until we insert, so
    // its first publish races nothing. Hold the registry lock
    // across the check-and-insert so two openers cannot both create.
    let core = Arc::new(StoreCore {
        path: path.to_path_buf(),
        backing_id: backing_id.clone(),
        reload: Mutex::new(()),
        live: RwLock::new(Arc::new(disk)),
        generation: AtomicU64::new(0),
        subscribers: RwLock::new(Vec::new()),
        next_subscriber: AtomicU64::new(0),
        publish_pause: parking_lot::Mutex::new(None),
    });
    reg.cores.insert(backing_id, Arc::downgrade(&core));
    Ok((core, Vec::new()))
}

/// Exclusive guard over one or two stores' publish transactions.
/// While held, no reload can publish a new live view through the
/// guarded core(s) — from ANY same-path handle, including an
/// opener joining the core (review-11 P1). The node holds this
/// across its replacement dominance comparison and swap, and
/// across authority verification and publication, so the installed
/// floor view cannot rise between a check and the publication that
/// depends on it.
///
/// When two DISTINCT cores must be pinned (a cross-core store
/// replacement or authority install — the topology review-10
/// supports), [`publish_guard_pair`] acquires their `reload` locks
/// in a canonical order (normalized path order) so two nodes
/// performing opposite swaps cannot deadlock ABBA (review-11 P1).
/// Callbacks are never invoked under this guard (raises notify
/// outside the reload lock), so holding it cannot deadlock against
/// notification work.
pub(crate) struct PublishGuard<'a> {
    _guards: Vec<parking_lot::MutexGuard<'a, ()>>,
}

/// Pin BOTH stores' publish transactions in a canonical, ABBA-free
/// order (review-11 P1). Same-core stores dedup to a single lock
/// (parking_lot mutexes are not reentrant, so locking one core
/// twice would self-deadlock). Distinct cores lock in normalized
/// path order, so every caller that pins the same two cores — from
/// any node — acquires them in the same sequence.
pub(crate) fn publish_guard_pair<'a>(
    a: &'a OrgRevocationStore,
    b: &'a OrgRevocationStore,
) -> PublishGuard<'a> {
    if Arc::ptr_eq(&a.core, &b.core) {
        return PublishGuard {
            _guards: vec![a.core.reload.lock()],
        };
    }
    let (first, second) = if a.core.path <= b.core.path {
        (a, b)
    } else {
        (b, a)
    };
    let g1 = first.core.reload.lock();
    let g2 = second.core.reload.lock();
    PublishGuard {
        _guards: vec![g1, g2],
    }
}

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
///
/// Within one process, same-path handles additionally share one
/// [`StoreCore`] — one live view, one publish transaction, one
/// subscriber registry (review-9 addendum). See the module docs.
pub struct OrgRevocationStore {
    /// The shared per-path core.
    ///
    /// R3-4: the facade holds NO subscription of its own. A raise
    /// subscription is always owned EXTERNALLY through the
    /// [`RaiseSubscription`] guard returned by
    /// [`Self::subscribe_floors_raised`] — the node's install path holds
    /// it, a test holds it. The removed `set_on_floors_raised` stored its
    /// guard inside the facade, which a callback capturing `Arc<Self>`
    /// (`core → callback → Arc<store> → own_subscription → …`) could keep
    /// alive forever, so the facade's own drop never ran and the callback
    /// leaked. Whoever holds the external guard breaks that cycle by
    /// dropping it.
    core: Arc<StoreCore>,
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
        let path = normalize_backing_path(&path)?;
        let lock = lock_state_file(&path)?;
        // AV-9: identity is the stable `.lock` inode, so case-aliases
        // share one core + poison entry.
        let backing_id = BackingId::of(&lock, &path)?;
        let was_poisoned = is_poisoned(&backing_id, &path);
        if was_poisoned {
            prove_entry_durable(&path)?;
        }
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
        let (core, raised) = join_or_create_core(backing_id.clone(), &path, state)?;
        if was_poisoned {
            clear_poison(&backing_id, &path);
        }
        drop(lock);
        let store = Self { core };
        store.core.notify(&raised);
        Ok(store)
    }

    /// Startup entry point: the file MUST exist and parse. Missing
    /// or corrupt → loud typed error; protected verification never
    /// starts against silently weaker floors.
    ///
    /// The open ALWAYS serializes behind the interprocess state
    /// lock — there is no pre-lock poison fast path (review-9
    /// addendum): a writer holding the lock may be mid-rename, so
    /// an opener must wait and read the FINAL state, and a poison
    /// bit registered while it waited must gate it. If the path is
    /// durability-poisoned, the open performs explicit recovery
    /// under that lock — a successful parent-directory fsync plus
    /// the reread republished through the shared per-path core
    /// (every live sibling advances) — BEFORE the poison clears;
    /// recovery failure refuses the open. A fresh instance
    /// therefore never launders path-wide uncertainty.
    pub fn open_existing(path: impl Into<PathBuf>) -> Result<Self, OrgRevocationError> {
        let path = normalize_backing_path(&path.into())?;
        let lock = lock_state_file(&path)?;
        // AV-9: stable `.lock` inode identity (case-aliases collapse).
        let backing_id = BackingId::of(&lock, &path)?;
        let was_poisoned = is_poisoned(&backing_id, &path);
        if was_poisoned {
            prove_entry_durable(&path)?;
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
        let (core, raised) = join_or_create_core(backing_id.clone(), &path, state)?;
        if was_poisoned {
            clear_poison(&backing_id, &path);
        }
        drop(lock);
        let store = Self { core };
        store.core.notify(&raised);
        Ok(store)
    }

    /// The backing file path (normalized at construction).
    pub fn path(&self) -> &Path {
        &self.core.path
    }

    /// Snapshot of the published live view.
    pub fn snapshot(&self) -> Arc<OrgRevocationState> {
        self.core.live.read().clone()
    }

    /// Live floor for `(org, member)`.
    pub fn floor_for(&self, org: &OrgId, member: &EntityId) -> u32 {
        self.snapshot().floor_for(org, member)
    }

    /// `true` while this store's BACKING PATH is
    /// durability-uncertain (review-9: the poison bit is shared by
    /// every instance on the same normalized pathname, not held
    /// per object). Cleared only by explicit recovery — a locked
    /// reread republished through the shared core plus a
    /// successful parent-directory fsync — performed by
    /// [`Self::open_existing`], [`Self::init`], or the next
    /// [`Self::apply_bundle`].
    pub fn is_poisoned(&self) -> bool {
        is_poisoned(&self.core.backing_id, &self.core.path)
    }

    /// Test-only: mark this store's backing path poisoned so
    /// [`Self::is_poisoned`] returns true, without forcing a real
    /// fsync failure. Lets a witness exercise the
    /// durability-uncertain admission-denial branch. `#[doc(hidden)]`
    /// (matching the review-9/11 `*_for_test` seams) so integration
    /// tests in a separate crate can reach it; never used in
    /// production paths.
    #[doc(hidden)]
    pub fn mark_poisoned_for_test(&self) {
        mark_poisoned(&self.core.backing_id, &self.core.path);
    }

    /// `true` iff `other` is backed by the same normalized path —
    /// i.e. shares this store's core (live view, publish lock,
    /// subscribers).
    pub fn shares_core_with(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.core, &other.core)
    }

    /// The core's publish generation — bumped once per published
    /// live view. Lets callers order publications relative to
    /// their own critical sections.
    ///
    /// A BARE atomic load: it does NOT cross the live-view lock, so a
    /// publication in progress (view already swapped under
    /// `live.write()`, generation not yet bumped) is observed as the
    /// OLD generation. Admission stamping must use
    /// [`Self::barriered_generation`] / [`Self::snapshot_with_generation`]
    /// instead — see their docs (OA2-E1 Kyra review).
    pub fn publish_generation(&self) -> u64 {
        self.core.generation.load(Ordering::Acquire)
    }

    /// The publish generation read UNDER a `live.read()` barrier
    /// (OA2-E1 Kyra review). [`StoreCore::publish`] swaps the live
    /// view and bumps the generation while holding `live.write()`, so
    /// acquiring a read guard first guarantees no publication is
    /// mid-flight: the returned generation always matches the
    /// currently-visible view. Unlike [`Self::publish_generation`],
    /// this can never return an old generation while a raised floor is
    /// already installed — the interleaving that would let a stale
    /// admission stamp compare "unchanged" and admit against a floor
    /// that has actually risen.
    pub fn barriered_generation(&self) -> u64 {
        let _live = self.core.live.read();
        self.core.generation.load(Ordering::Acquire)
    }

    /// A floor snapshot together with the exact generation it
    /// reflects, both read under ONE `live.read()` guard (OA2-E1
    /// Kyra review). Publication-barriered like
    /// [`Self::barriered_generation`], so the `(snapshot, generation)`
    /// pair is always consistent — no seqlock retry needed.
    pub fn snapshot_with_generation(&self) -> (Arc<OrgRevocationState>, u64) {
        let live = self.core.live.read();
        let generation = self.core.generation.load(Ordering::Acquire);
        (live.clone(), generation)
    }

    /// Test-only (`#[doc(hidden)]`, mirroring the review-11
    /// `*_paused_for_test` seams): arm the one-shot publish pause. The
    /// NEXT [`StoreCore::publish`] (e.g. via [`Self::apply_bundle`])
    /// will, after swapping the live view and while still holding
    /// `live.write()`, signal the returned receiver and then block
    /// until the returned sender is used. Lets a witness sit in the
    /// "new view, old generation" window to prove the send-path /
    /// admission barriered reads never observe it. Not for production.
    #[doc(hidden)]
    pub fn arm_publish_pause_for_test(
        &self,
    ) -> (std::sync::mpsc::Receiver<()>, std::sync::mpsc::Sender<()>) {
        let (swapped_tx, swapped_rx) = std::sync::mpsc::channel();
        let (resume_tx, resume_rx) = std::sync::mpsc::channel();
        *self.core.publish_pause.lock() = Some(PublishPauseHook {
            swapped: swapped_tx,
            resume: resume_rx,
        });
        (swapped_rx, resume_tx)
    }

    /// Pin this store's publish transaction (review-9 addendum):
    /// while the returned guard lives, no reload can publish a new
    /// live view through this store's core, from any same-path
    /// handle. See [`PublishGuard`].
    pub(crate) fn publish_guard(&self) -> PublishGuard<'_> {
        PublishGuard {
            _guards: vec![self.core.reload.lock()],
        }
    }

    /// Number of raise subscribers currently registered on the
    /// shared core (test/metric surface). Used by the review-11 P2
    /// leak witness to prove a dropped node unsubscribed its
    /// callback.
    #[doc(hidden)]
    pub fn subscriber_count(&self) -> usize {
        self.core.subscribers.read().len()
    }

    /// Test-only (AV-10): snapshot the core's subscriber callbacks
    /// EXACTLY as [`StoreCore::notify`] does — clone the callback
    /// `Arc`s outside the registry lock. Lets a witness capture a
    /// callback BEFORE a node teardown and invoke it afterward to prove
    /// the owner-liveness token makes such a late callback inert.
    #[doc(hidden)]
    pub fn snapshot_subscribers_for_test(&self) -> Vec<FloorsRaisedCallback> {
        self.core
            .subscribers
            .read()
            .iter()
            .map(|(_, callback)| callback.clone())
            .collect()
    }

    /// Register `callback` as ONE subscriber in the core's raise
    /// registry and return an externally-owned [`RaiseSubscription`]
    /// RAII guard (review-9 addendum: subscription is a registry, not a
    /// single replaceable slot — a second observer must never silently
    /// steal the first one's notifications). Subscribers fire after a
    /// reload publishes floors above the previously enforced view —
    /// including floors learned from OTHER writers via the under-lock
    /// reread, and raises published by same-path sibling handles.
    ///
    /// The callback is wrapped in an exclusion lease (R2-3): its body
    /// runs only while registered as in-flight, and a teardown draining
    /// the lease blocks until it leaves. Dropping the returned guard
    /// retires the subscription — draining any in-flight callback and
    /// removing it from the core through a `Weak<StoreCore>` (R2-2), so
    /// cleanup never depends on this facade's own drop.
    #[must_use = "dropping the returned guard immediately unsubscribes the callback"]
    pub fn subscribe_floors_raised(
        &self,
        callback: impl Fn(&[RaisedFloor]) + Send + Sync + 'static,
    ) -> RaiseSubscription {
        let lease = SubscriptionLease::new();
        let lease_cb = Arc::clone(&lease);
        let wrapped: FloorsRaisedCallback = Arc::new(move |raised: &[RaisedFloor]| {
            // R2-3: admit under the lease, run the user callback OUTSIDE
            // the lease lock (re-entrant `apply_bundle` and long
            // retractions must not self-deadlock), then leave — even on
            // panic, via the drop guard.
            if !lease_cb.enter() {
                return;
            }
            struct LeaveOnDrop<'a>(&'a Arc<SubscriptionLease>);
            impl Drop for LeaveOnDrop<'_> {
                fn drop(&mut self) {
                    self.0.leave();
                }
            }
            let _leave = LeaveOnDrop(&lease_cb);
            callback(raised);
        });
        let token = self.core.next_subscriber.fetch_add(1, Ordering::Relaxed);
        self.core.subscribers.write().push((token, wrapped));
        RaiseSubscription {
            core: Arc::downgrade(&self.core),
            token,
            lease,
        }
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
    /// failure publishes the merged (never-weaker) view through
    /// the shared core (every same-path sibling advances with it),
    /// poisons the PATH, and returns
    /// [`OrgRevocationError::DurabilityUncertain`]; further
    /// same-path applies are refused until recovery — a locked
    /// reread republished through the core plus a successful
    /// parent-directory fsync — clears the uncertainty.
    pub fn apply_bundle(
        &self,
        bundle: &OrgRevocationBundle,
    ) -> Result<Vec<RaisedFloor>, OrgRevocationError> {
        let path = &self.core.path;

        // The locked phase returns its outcome so raise observers
        // run AFTER both the file lock and the core's reload guard
        // have dropped — a callback that re-enters `apply_bundle`
        // on the same store must not deadlock (review-9).
        enum LockedOutcome {
            Applied(Vec<RaisedFloor>),
            DurabilityUncertain(Vec<RaisedFloor>, String),
        }

        // 1. Verify the incoming bundle's signature + canonical
        //    structure BEFORE taking any lock — a corrupt bundle
        //    keeps last-good, loudly, and touches nothing.
        if let Err(e) = bundle.verify() {
            let err = OrgRevocationError::InvalidBundle(e);
            tracing::error!(
                org = %bundle.org_id,
                "rejecting revocation bundle, keeping last-good persisted floors: {err}"
            );
            return Err(err);
        }

        let outcome = {
            // Canonical lock order (review-11 P1): interprocess file
            // lock OUTER, core `reload` INNER — the SAME order every
            // opener (`join_or_create_core`) uses. Publishing under
            // `reload` is what makes [`PublishGuard`] a real barrier:
            // no publish can land between a replacement's dominance
            // comparison and its swap. A replacement holds only
            // `reload` (never the file lock), so no lock cycle forms.
            let lock = lock_state_file(path)?;
            let _guard = self.core.reload.lock();

            // R3-3: the `.lock` sidecar just opened MUST be the SAME
            // identity this live core was created on. If the sidecar was
            // deleted and recreated (fresh inode) beneath a still-live
            // handle — which the `nlink != 1` refusal does NOT catch, since
            // the replacement has one link — this transaction would lock
            // and publish through a DIFFERENT backing identity than its
            // core's, operating outside its original lock / publication
            // domain (a new opener is refused by `BackingIdentityConflict`,
            // but the existing handle would sail on). Refuse loudly BEFORE
            // any reread / merge / write, so disk and the live view are
            // both untouched.
            let opened_id = BackingId::of(&lock, path)?;
            if opened_id != self.core.backing_id {
                drop(lock);
                return Err(OrgRevocationError::BackingIdentityConflict {
                    path: path.display().to_string(),
                });
            }

            // 2. Interprocess critical section. A poisoned path
            //    must first prove its directory entry durable; the
            //    reread + publish below then republish the ground
            //    truth through the shared core BEFORE the poison
            //    bit clears (review-9 addendum: recovery reloads
            //    live views, it never merely fsyncs).
            let was_poisoned = is_poisoned(&self.core.backing_id, path);
            if was_poisoned {
                prove_entry_durable(path)?;
            }

            // 3. REREAD the persisted maxima under the lock — the
            //    reread is load-bearing: merging from this
            //    instance's live snapshot would let a stale writer
            //    overwrite floors another writer already persisted.
            let disk_bytes = read_regular_nofollow(path).map_err(|e| OrgRevocationError::Io {
                path: path.display().to_string(),
                reason: e.to_string(),
            })?;
            let disk = OrgRevocationState::from_file_bytes(&disk_bytes, path)?;

            // 4. Monotone merge against the reread disk state.
            let mut merged = disk.clone();
            let raised_on_disk = merged.merge_bundle(bundle);

            // 5. Persist iff the disk state changed; the write must
            //    complete before anything is published.
            let mut durability_uncertain: Option<String> = None;
            if raised_on_disk > 0 {
                match write_atomic_phased(path, &merged.to_file_bytes()?) {
                    Ok(()) => {}
                    Err(WritePhase::PreRename(reason)) => {
                        // Old file (rename never happened) and old
                        // live view both intact — a floor the disk
                        // could forget is never enforced.
                        drop(lock);
                        return Err(OrgRevocationError::Io {
                            path: path.display().to_string(),
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
                        mark_poisoned(&self.core.backing_id, path);
                        durability_uncertain = Some(reason);
                    }
                }
            }

            // 6. Publish the merged view through the SHARED core —
            //    every same-path handle's view advances the instant
            //    this lands (review-9 addendum) — then clear any
            //    recovered poison and release the lock;
            //    notification happens outside.
            let raised = self.core.publish(merged);
            if was_poisoned && durability_uncertain.is_none() {
                clear_poison(&self.core.backing_id, path);
            }
            drop(lock);
            match durability_uncertain {
                None => LockedOutcome::Applied(raised),
                Some(reason) => LockedOutcome::DurabilityUncertain(raised, reason),
            }
        };

        match outcome {
            LockedOutcome::Applied(raised) => {
                self.core.notify(&raised);
                Ok(raised)
            }
            LockedOutcome::DurabilityUncertain(raised, reason) => {
                let err = OrgRevocationError::DurabilityUncertain {
                    path: path.display().to_string(),
                    reason,
                };
                tracing::error!("{err}");
                self.core.notify(&raised);
                Err(err)
            }
        }
    }
}

impl std::fmt::Debug for OrgRevocationStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OrgRevocationStore")
            .field("path", &self.core.path)
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
/// NORMALIZED backing path (review-9): the filesystem's uncertainty
/// after a landed-rename/failed-dir-fsync belongs to the directory
/// entry, not to one `OrgRevocationStore` instance. Every store
/// opened on the same pathname shares the poison bit; recovery
/// (a locked reread republished through the shared core plus a
/// SUCCESSFUL parent-directory fsync) clears it. Separate from the
/// core registry because poison must outlive every handle.
static PATH_POISON: std::sync::OnceLock<Mutex<PoisonRegistry>> = std::sync::OnceLock::new();

/// Two poison indexes that must BOTH be consulted (R3-2):
///
/// - `by_id` — the live `.lock` sidecar identity ([`BackingId`]), which
///   is what same-path handles join their core on; and
/// - `by_path` — the CANONICAL state-file path mapped to the SET of every
///   sidecar identity ever poisoned under it. The path key survives `.lock`
///   sidecar replacement: keying poison only on the sidecar identity let a
///   durability-uncertain path be laundered by dropping every handle and
///   recreating the `.lock` (new inode ⇒ new `BackingId` ⇒ `by_id` miss ⇒
///   recovery skipped). The path tombstone closes that — once poisoned, the
///   path stays poisoned across sidecar recreation until explicit recovery,
///   and case-aliases collapse through the actual filesystem
///   (`canonicalize`), not blind case-folding.
///
///   Tracking the id SET per path (not just the path itself) lets recovery
///   retire EVERY stale old `BackingId` for that path in one step (P2
///   hygiene): a sidecar that was unlinked and recreated stranded its old id
///   in `by_id`, and a later store re-using that recycled inode would
///   otherwise trip redundant recovery on the dead id's residue.
#[derive(Default)]
struct PoisonRegistry {
    by_id: std::collections::HashSet<BackingId>,
    by_path: std::collections::HashMap<PathBuf, std::collections::HashSet<BackingId>>,
}

fn poison_registry() -> &'static Mutex<PoisonRegistry> {
    PATH_POISON.get_or_init(|| Mutex::new(PoisonRegistry::default()))
}

/// The case-normalized poison-tombstone key for `normalized_path` (R3-2).
/// `canonicalize` collapses case-aliases through the ACTUAL filesystem
/// identity — not blind ASCII case-folding, which would over-poison two
/// genuinely distinct files on a case-sensitive filesystem. Falls back to
/// the normalized path when the state file does not yet exist (a fresh
/// `init` before creation) or `canonicalize` otherwise fails.
fn poison_path_key(normalized_path: &Path) -> PathBuf {
    std::fs::canonicalize(normalized_path).unwrap_or_else(|_| normalized_path.to_path_buf())
}

/// Poison `normalized_path` under BOTH indexes (R3-2), recording `id` in the
/// path's id set so recovery can retire every id ever poisoned here.
fn mark_poisoned(id: &BackingId, normalized_path: &Path) {
    let key = poison_path_key(normalized_path);
    let mut reg = poison_registry().lock();
    reg.by_id.insert(id.clone());
    reg.by_path.entry(key).or_default().insert(id.clone());
}

/// Poisoned iff EITHER the live sidecar identity OR the canonical state
/// path is tombstoned — so a recreated sidecar (new `BackingId`, same
/// path) is still caught (R3-2).
fn is_poisoned(id: &BackingId, normalized_path: &Path) -> bool {
    let key = poison_path_key(normalized_path);
    let reg = poison_registry().lock();
    reg.by_id.contains(id) || reg.by_path.contains_key(&key)
}

/// Normalize a backing pathname ONCE at store construction
/// (review-9 addendum): the CANONICAL parent joined with the
/// literal final component, with NO verbatim fallback. Aliases of
/// one file (bare vs `./`, relative vs absolute, `..` hops,
/// symlinked parents) land on ONE core and ONE poison entry, so a
/// single backing file never gets independent security views. A
/// path with no final component, or whose parent cannot resolve,
/// is refused.
///
/// The final component is validated for symlink/non-regular
/// ATOMICALLY (review-11 P2): the previous form did
/// `symlink_metadata` then `canonicalize` as two syscalls, and the
/// final component could be swapped to a symlink in between —
/// `canonicalize` would then follow it and key the store to the
/// link's target, which the later no-follow opens could not detect.
/// A no-follow open of the joined path IS the check: it refuses a
/// symlink (`ELOOP`) or non-regular final in one syscall, or
/// reports the file simply does not exist yet (a fresh `init`).
///
/// The parent is canonicalized (resolving parent symlinks and
/// case), so parent-side aliases still collapse; the FINAL
/// component is taken literally rather than canonicalized. In
/// practice the final component is a fixed constant
/// (`revocation-state.json`, `owner-audience.key`), so
/// final-component case aliasing on case-insensitive filesystems
/// is not a real call shape — trading it away removes the TOCTOU.
pub(crate) fn normalize_backing_path(path: &Path) -> Result<PathBuf, OrgRevocationError> {
    let io = |reason: String| OrgRevocationError::Io {
        path: path.display().to_string(),
        reason,
    };
    let Some(file_name) = path.file_name() else {
        return Err(io("backing path has no final component".to_string()));
    };
    let parent = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    let canon_parent = parent
        .canonicalize()
        .map_err(|e| io(format!("cannot canonicalize parent directory: {e}")))?;
    let joined = canon_parent.join(file_name);
    // Atomic final-component validation: the no-follow open refuses
    // a symlink/FIFO/non-regular final in one syscall (no
    // stat→canonicalize gap). NotFound is fine — a fresh store
    // creates the file under exactly this name.
    match open_regular_nofollow(&joined) {
        Ok(_) => Ok(joined),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(joined),
        Err(e) => Err(io(format!(
            "refusing non-regular backing path (symlink/FIFO/other): {e}"
        ))),
    }
}

/// First half of durability recovery, called with the interprocess
/// lock HELD: prove the directory entry durable with a
/// parent-directory fsync. Failure refuses with
/// [`OrgRevocationError::Poisoned`] — no same-path operation may
/// proceed while uncertainty remains. On success the caller MUST
/// reread the state file and republish it through the shared core
/// (so every live sibling advances to ground truth) BEFORE calling
/// [`clear_poison`] — recovery reloads live views; it never merely
/// fsyncs (review-9 addendum).
fn prove_entry_durable(path: &Path) -> Result<(), OrgRevocationError> {
    fsync_parent_dir(path).map_err(|e| {
        tracing::error!(
            path = %path.display(),
            error = %e,
            "revocation-state durability recovery failed; path remains poisoned"
        );
        OrgRevocationError::Poisoned {
            path: path.display().to_string(),
        }
    })
}

/// Second half of durability recovery: clear the path-wide bit
/// after the entry was proven durable AND the reread state was
/// republished through the shared core.
fn clear_poison(id: &BackingId, path: &Path) {
    let key = poison_path_key(path);
    {
        let mut reg = poison_registry().lock();
        // Retire EVERY sidecar identity ever poisoned under this canonical
        // path, not just the recovering one (P2 hygiene): a prior sidecar
        // that was unlinked and recreated left its old `BackingId` stranded
        // in `by_id`, and a later store re-using that recycled inode would
        // otherwise trip redundant recovery on the dead residue.
        if let Some(ids) = reg.by_path.remove(&key) {
            for stale in ids {
                reg.by_id.remove(&stale);
            }
        }
        reg.by_id.remove(id);
    }
    tracing::warn!(
        path = %path.display(),
        "revocation-state durability uncertainty recovered \
         (locked reread republished; parent directory fsynced)"
    );
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
    let opened = opts.open(path);
    // Map the Unix `O_NOFOLLOW` symlink rejection (`ELOOP`) to a clear typed
    // error. Unix-only: `#[cfg(not(unix))]` has no `O_NOFOLLOW`, and mapping
    // there would be an identity map (`|e| e`).
    #[cfg(unix)]
    let opened = opened.map_err(|e| {
        if e.raw_os_error() == Some(libc::ELOOP) {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "refusing symlink: authority files must be regular files",
            )
        } else {
            e
        }
    });
    let file = opened?;
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
/// semantics as the sdk revocation store's fs2 sidecar.
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
    let lock = open_lock_file(&PathBuf::from(lock_path)).map_err(io)?;
    // R2-4: a legitimately-created sidecar has exactly ONE hard link. A
    // link count above one means someone hard-linked this sidecar's inode
    // to a SECOND name — the attack that would otherwise collapse two
    // distinct state paths onto one [`BackingId`] (and thus one core /
    // poison entry). Refuse fail-closed on every platform. `std` exposes the
    // link count on Unix (`nlink`) and on Windows only via the stable Win32
    // `GetFileInformationByHandle` (`nNumberOfLinks`), read here directly.
    #[cfg(unix)]
    let nlink = {
        use std::os::unix::fs::MetadataExt;
        u64::from(lock.metadata().map_err(io)?.nlink())
    };
    #[cfg(windows)]
    let nlink = {
        let (_volume, _index, links) = windows_file_identity(&lock).map_err(io)?;
        u64::from(links)
    };
    #[cfg(any(unix, windows))]
    if nlink != 1 {
        return Err(OrgRevocationError::Io {
            path: path.display().to_string(),
            reason: format!(
                "state lock: refusing .lock sidecar with {nlink} hard links \
                 (expected 1) — a hard-linked sidecar would alias two backing paths"
            ),
        });
    }
    Ok(lock)
}

/// Open-and-lock a lock inode (`.lock` sidecar, ceremony lock)
/// under the full regular-file policy (review-9): no-follow (a
/// planted symlink cannot redirect the lock inode), `O_NONBLOCK`
/// (a planted FIFO fails or returns instead of blocking the open
/// forever), and a type check on the OPENED descriptor — advisory
/// locking a non-regular inode is not a lock on anything this
/// module owns. `O_NONBLOCK` is inert for regular files and does
/// not affect the (deliberately blocking) advisory lock call.
///
/// `pub(crate)`: the adoption ceremony lock
/// (`org_authority::lock_ceremony`) applies the same policy.
pub(crate) fn open_lock_file(lock_path: &Path) -> std::io::Result<std::fs::File> {
    let mut opts = std::fs::OpenOptions::new();
    opts.create(true).write(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        opts.mode(0o600);
    }
    #[cfg(not(unix))]
    {
        // Non-Unix has no O_NOFOLLOW: same symlink precheck as
        // `open_regular_nofollow` (plus the opened-handle type
        // check below).
        if let Ok(meta) = std::fs::symlink_metadata(lock_path) {
            if meta.file_type().is_symlink() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "refusing symlink: lock files must be regular files",
                ));
            }
        }
    }
    let f = opts.open(lock_path)?;
    // Type check on the opened descriptor — immune to a swap
    // between check and use.
    if !f.metadata()?.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "refusing non-regular file: lock files must be regular files",
        ));
    }
    f.lock()?;
    Ok(f)
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
    // an existing destination. On Windows, std::fs::rename is
    // DOCUMENTED to replace an existing destination file (see the
    // std platform-specific behavior notes), so no separate
    // ReplaceFileW path is required for replacement semantics.
    // Crash DURABILITY is a distinct boundary: the parent-dir
    // fsync below is Unix-only, so on non-Unix platforms the
    // durability of the new directory entry is whatever the
    // platform's rename primitive provides — this module's
    // fail-closed poison machinery therefore only arms on Unix,
    // where the fsync can actually be attempted and fail.
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

    /// AV-9 item 9: on a case-INSENSITIVE filesystem, two
    /// differently-cased aliases of one backing file resolve to the
    /// SAME `.lock` inode, so they must collapse to ONE core (shared
    /// live view + publish lock + poison) — not split as the pre-AV-9
    /// literal-cased path key did. On a case-SENSITIVE filesystem the
    /// two names ARE distinct files, so the assertions are skipped (the
    /// fix is correctly a no-op there).
    ///
    /// Red-witness (on a case-insensitive FS): reverting the CORES /
    /// PATH_POISON key from [`BackingId`] to the normalized path makes
    /// the two aliases distinct keys — `shares_core_with` is then false
    /// and this fails.
    #[test]
    fn case_aliased_paths_share_one_core_on_case_insensitive_fs() {
        let scratch = Scratch::new();
        let lower = scratch.0.join("revocation-state.json");
        let upper = scratch.0.join("REVOCATION-STATE.JSON");

        let a = OrgRevocationStore::init(&lower).expect("init lower alias");

        // Probe the filesystem: does the upper-cased alias resolve to
        // the file just created? If not (case-sensitive FS), there is
        // no alias to unify and the fix is a no-op.
        if !upper.exists() {
            return;
        }

        let b = OrgRevocationStore::open_existing(&upper).expect("open upper alias");
        assert!(
            a.shares_core_with(&b),
            "case-aliases on a case-insensitive FS must share ONE core (same .lock inode)",
        );

        // A floor published through one alias is visible through the
        // other immediately (shared live view).
        a.apply_bundle(&bundle_with_floor(5))
            .expect("apply floor via lower alias");
        assert_eq!(
            b.floor_for(&org().org_id(), &member()),
            5,
            "a floor published through one alias must be visible through the other",
        );

        // Poison registered through one alias is visible through the
        // other (shared poison entry).
        a.mark_poisoned_for_test();
        assert!(
            b.is_poisoned(),
            "poison under one alias must be visible through the other",
        );
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

    /// Review-8 §5 + review-9 addendum witness: two store handles
    /// on one file share ONE core — a sibling observes a raise the
    /// instant it publishes (never a stale independent view) — and
    /// the under-lock REREAD keeps every maximum in the persisted
    /// file (the reread still guards CROSS-PROCESS writers, which
    /// cannot share a core).
    #[test]
    fn same_path_handles_share_one_live_view_and_preserve_all_maxima() {
        let scratch = Scratch::new();
        let path = scratch.state_path();
        let member_x = EntityId::from_bytes([0xAAu8; 32]);
        let member_y = EntityId::from_bytes([0xBBu8; 32]);

        let store_a = OrgRevocationStore::init(&path).expect("init A");
        let store_b = OrgRevocationStore::open_existing(&path).expect("open B");
        assert!(
            store_a.shares_core_with(&store_b),
            "same normalized path must join one core"
        );

        // A raises member_x to 5; B's view advances IMMEDIATELY —
        // one backing file is never two security views (review-9
        // addendum).
        store_a
            .apply_bundle(&bundle_for(member_x.clone(), 5))
            .expect("A applies x=5");
        assert_eq!(store_b.floor_for(&org().org_id(), &member_x), 5);

        // B raises member_y to 7; the shared view means only y
        // newly rises, and the persisted file carries BOTH maxima.
        let raised = store_b
            .apply_bundle(&bundle_for(member_y.clone(), 7))
            .expect("B applies y=7");
        assert_eq!(raised, vec![(org().org_id(), member_y.clone(), 7)]);

        let reopened = OrgRevocationStore::open_existing(&path).expect("reopen");
        assert_eq!(reopened.floor_for(&org().org_id(), &member_x), 5);
        assert_eq!(reopened.floor_for(&org().org_id(), &member_y), 7);
        assert!(reopened.shares_core_with(&store_a));
    }

    /// Review-9 addendum: `open_existing` has NO pre-lock poison
    /// fast path — an opener serializes behind the state lock, and
    /// a poison bit registered while it waited gates it. Recovery
    /// rereads the FINAL persisted state and returns it, never a
    /// stale pre-write view.
    #[test]
    fn fresh_open_serializes_behind_the_state_lock_and_recovers_poison() {
        let scratch = Scratch::new();
        let path = scratch.state_path();
        drop(OrgRevocationStore::init(&path).expect("init"));
        let norm = normalize_backing_path(&path).expect("normalize");

        // Writer holds the interprocess lock…
        let lock = lock_state_file(&norm).expect("lock");

        let opener_path = path.clone();
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let opener = std::thread::spawn(move || {
            started_tx.send(()).expect("send started");
            let result = OrgRevocationStore::open_existing(&opener_path);
            done_tx.send(()).expect("send done");
            result
        });
        started_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("opener started");
        // …so the opener must NOT complete while the lock is held.
        assert!(
            done_rx
                .recv_timeout(std::time::Duration::from_millis(300))
                .is_err(),
            "open_existing must serialize behind the state lock"
        );

        // Still under the lock: the writer lands a stronger state
        // and (simulating a failed post-rename parent fsync)
        // registers the path-wide poison.
        let mut stronger = OrgRevocationState::empty();
        stronger.merge_bundle(&bundle_with_floor(9));
        write_atomic(&norm, &stronger.to_file_bytes().expect("bytes")).expect("write");
        mark_poisoned(&BackingId::of(&lock, &norm).expect("backing id"), &norm);

        // Lock releases → the opener proceeds: it must observe the
        // poison, recover (reread + successful parent fsync), and
        // return the FINAL floor — never the pre-write view.
        drop(lock);
        let opened = opener
            .join()
            .expect("join opener")
            .expect("open recovers and succeeds");
        assert_eq!(opened.floor_for(&org().org_id(), &member()), 9);
        assert!(
            !opened.is_poisoned(),
            "successful recovery clears the path-wide bit"
        );
    }

    /// Review-11 P1: an opener publishing through an EXISTING core
    /// obeys the same `PublishGuard` a replacement holds. While the
    /// guard is held, a same-path opener cannot publish its
    /// (stronger) floor — it blocks until the guard drops, so a
    /// replacement's dominance comparison and swap see a frozen
    /// live view. This is the store-level root of the review-10 red
    /// (opener published floor 10 inside a held guard).
    #[test]
    fn opener_cannot_publish_through_a_held_publish_guard() {
        let scratch = Scratch::new();
        let path = scratch.state_path();
        // store_a creates and keeps the core alive at floor 0.
        let store_a = OrgRevocationStore::init(&path).expect("init");

        // A stronger state is already durable on disk (an operator
        // bundle another writer persisted); an opener would read and
        // publish floor 10 through the shared core.
        let norm = normalize_backing_path(&path).expect("normalize");
        let mut stronger = OrgRevocationState::empty();
        stronger.merge_bundle(&bundle_with_floor(10));
        {
            let _lk = lock_state_file(&norm).expect("lock");
            write_atomic(&norm, &stronger.to_file_bytes().expect("bytes")).expect("write");
        }

        // Hold the publish guard (what a replacement holds across
        // dominance→swap).
        let guard = store_a.publish_guard();

        let opener_path = path.clone();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let opener = std::thread::spawn(move || {
            let s = OrgRevocationStore::open_existing(&opener_path).expect("open");
            done_tx.send(()).expect("done");
            s
        });
        // The opener must NOT publish while the guard is held: the
        // shared live view stays at floor 0.
        assert!(
            done_rx
                .recv_timeout(std::time::Duration::from_millis(300))
                .is_err(),
            "opener published inside a held PublishGuard"
        );
        assert_eq!(
            store_a.floor_for(&org().org_id(), &member()),
            0,
            "the guarded live view must not move under an opener"
        );

        // Releasing the guard lets the opener publish; the shared
        // view then advances to 10.
        drop(guard);
        let opened = opener.join().expect("join");
        assert_eq!(opened.floor_for(&org().org_id(), &member()), 10);
        assert_eq!(store_a.floor_for(&org().org_id(), &member()), 10);
    }

    /// Review-9 addendum: the raise-observer registry supports
    /// multiple subscribers — registering a second observer never
    /// steals the first one's notifications, same-path handles'
    /// callbacks all fire, and a token unsubscribes only its own
    /// registration.
    #[test]
    fn multiple_subscribers_on_one_path_all_observe_raises() {
        let scratch = Scratch::new();
        let path = scratch.state_path();
        let store_a = OrgRevocationStore::init(&path).expect("init A");
        let store_b = OrgRevocationStore::open_existing(&path).expect("open B");

        let seen_a: Arc<Mutex<Vec<u32>>> = Arc::new(Mutex::new(Vec::new()));
        let seen_b: Arc<Mutex<Vec<u32>>> = Arc::new(Mutex::new(Vec::new()));
        let seen_tok: Arc<Mutex<Vec<u32>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = seen_a.clone();
        let _sub_a = store_a.subscribe_floors_raised(move |raised| {
            sink.lock().extend(raised.iter().map(|(_, _, f)| *f));
        });
        let sink = seen_b.clone();
        let _sub_b = store_b.subscribe_floors_raised(move |raised| {
            sink.lock().extend(raised.iter().map(|(_, _, f)| *f));
        });
        let sink = seen_tok.clone();
        let subscription = store_a.subscribe_floors_raised(move |raised| {
            sink.lock().extend(raised.iter().map(|(_, _, f)| *f));
        });

        // One raise through A notifies EVERY registration —
        // including B's, which observes the raise through the
        // shared core (previously the review-9 addendum red: only
        // the final `set_on_floors_raised` caller was notified).
        store_a
            .apply_bundle(&bundle_with_floor(5))
            .expect("apply 5");
        assert_eq!(*seen_a.lock(), vec![5]);
        assert_eq!(*seen_b.lock(), vec![5]);
        assert_eq!(*seen_tok.lock(), vec![5]);

        // Dropping the RAII guard removes ONLY that registration.
        drop(subscription);
        store_b
            .apply_bundle(&bundle_with_floor(7))
            .expect("apply 7");
        assert_eq!(*seen_a.lock(), vec![5, 7]);
        assert_eq!(*seen_b.lock(), vec![5, 7]);
        assert_eq!(*seen_tok.lock(), vec![5], "unsubscribed token is silent");
    }

    /// R2-2: `subscribe_floors_raised` hands back an externally-owned
    /// RAII guard; dropping the GUARD retires the subscription even while
    /// the owning store facade is still very much alive — removal goes
    /// through the guard's `Weak<StoreCore>`, not the facade's `Drop`
    /// (which a `core → callback → Arc<store> → core` capture cycle could
    /// keep from ever running).
    ///
    /// Red-witness: making `RaiseSubscription::drop` skip
    /// `core.remove_subscriber` leaves the count at 1.
    #[test]
    fn dropping_the_subscription_guard_unsubscribes_while_the_store_lives() {
        let scratch = Scratch::new();
        let store = OrgRevocationStore::init(scratch.state_path()).expect("init");
        assert_eq!(store.subscriber_count(), 0);
        let subscription = store.subscribe_floors_raised(|_raised| {});
        assert_eq!(
            store.subscriber_count(),
            1,
            "subscribe registers one callback"
        );
        drop(subscription);
        assert_eq!(
            store.subscriber_count(),
            0,
            "dropping the guard unsubscribed while the store handle is still alive",
        );
    }

    /// R2-3: teardown EXCLUDES an in-flight callback. A callback that has
    /// passed the liveness check and is mid-body keeps the subscription's
    /// Drop BLOCKED (draining the exclusion lease) until it leaves — so a
    /// retraction can never be torn in half by a concurrent teardown, and
    /// no new callback starts once teardown has begun.
    ///
    /// Deterministic barrier: the callback signals `entered` (now counted
    /// in-flight) and blocks; a teardown thread drops the guard and must
    /// park in `kill_and_drain`; while parked it provably cannot signal
    /// completion (asserted via a bounded `recv_timeout` that MUST expire);
    /// releasing the callback lets it leave, the drain completes, and only
    /// then does teardown finish.
    ///
    /// Red-witness: dropping the `while in_flight > 0` drain loop in
    /// `kill_and_drain` lets teardown complete while the callback is still
    /// in-flight, so the "must block" `recv_timeout` receives early and the
    /// assertion fails.
    #[test]
    fn teardown_blocks_until_an_in_flight_callback_leaves() {
        use std::sync::mpsc;
        use std::time::Duration;

        let scratch = Scratch::new();
        let store = Arc::new(OrgRevocationStore::init(scratch.state_path()).expect("init"));

        let (entered_tx, entered_rx) = mpsc::channel::<()>();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        // The callback must be `Fn + Send + Sync`; the mpsc endpoints are
        // `!Sync`, so guard them.
        let entered_tx = Mutex::new(entered_tx);
        let release_rx = Mutex::new(release_rx);
        let ran = Arc::new(AtomicUsize::new(0));
        let ran_cb = Arc::clone(&ran);
        let subscription = store.subscribe_floors_raised(move |_raised| {
            ran_cb.fetch_add(1, Ordering::SeqCst);
            entered_tx.lock().send(()).expect("signal entered");
            // Block INSIDE the callback body: the exclusion lease counts
            // this run as in-flight for the whole duration.
            release_rx.lock().recv().expect("await release");
        });

        // Fire a raise on a worker thread so the callback blocks there.
        let store_fire = Arc::clone(&store);
        let fire = std::thread::spawn(move || {
            store_fire
                .apply_bundle(&bundle_with_floor(5))
                .expect("apply 5");
        });

        // The callback is now in-flight (blocked on release).
        entered_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("callback entered");

        // Tear down on another thread; it must PARK in kill_and_drain.
        let (teardown_done_tx, teardown_done_rx) = mpsc::channel::<()>();
        let teardown = std::thread::spawn(move || {
            drop(subscription);
            teardown_done_tx.send(()).expect("signal teardown done");
        });

        // Block proof: while the callback is in-flight, teardown cannot
        // complete — this `recv_timeout` MUST expire.
        assert!(
            teardown_done_rx
                .recv_timeout(Duration::from_millis(300))
                .is_err(),
            "teardown must block while a callback is in-flight",
        );

        // Release the callback → it leaves → the drain wakes → teardown
        // completes.
        release_tx.send(()).expect("release callback");
        teardown_done_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("teardown completes after the callback drains");

        fire.join().expect("fire thread");
        teardown.join().expect("teardown thread");
        assert_eq!(ran.load(Ordering::SeqCst), 1, "callback ran exactly once");
        assert_eq!(
            store.subscriber_count(),
            0,
            "the drained guard removed the subscriber",
        );
    }

    /// R3-4: dropping the externally-owned guard BREAKS the
    /// `core → subscribers → callback → Arc<store> → Arc<core> → core`
    /// capture cycle, so a callback that captures `Arc<store>` no longer
    /// leaks the store. (The removed `set_on_floors_raised` stored its
    /// guard inside the facade, which that same cycle kept alive forever,
    /// so its drop never ran.)
    ///
    /// Red-witness: making `RaiseSubscription::drop` skip
    /// `remove_subscriber` leaves the capturing callback in the core, so
    /// the store never frees and `weak.upgrade()` stays `Some`.
    #[test]
    fn dropping_the_external_guard_breaks_a_store_capturing_cycle() {
        let scratch = Scratch::new();
        let store = Arc::new(OrgRevocationStore::init(scratch.state_path()).expect("init"));
        let weak = Arc::downgrade(&store);
        // The callback CAPTURES the store Arc — the exact cycle.
        let captured = Arc::clone(&store);
        let sub = store.subscribe_floors_raised(move |_raised| {
            let _keep = &captured;
        });
        // Dropping the external guard removes the callback, releasing its
        // captured `Arc<store>`; then the last external handle drops.
        drop(sub);
        drop(store);
        assert!(
            weak.upgrade().is_none(),
            "dropping the external guard must break the callback→store cycle so the store frees",
        );
    }

    /// R3-4: a callback that drops its OWN guard from inside the callback
    /// must not deadlock. `kill_and_drain` excludes this thread's own
    /// in-flight frame (via the thread-local lease tracking), so it does
    /// not wait for the frame that is dropping it; that frame's
    /// `LeaveOnDrop` performs the final retirement.
    ///
    /// Red-witness: reverting `kill_and_drain` to wait for `in_flight == 0`
    /// unconditionally deadlocks the self-dropping callback, so the worker
    /// never signals and the bounded `recv_timeout` expires.
    #[test]
    fn a_callback_can_drop_its_own_guard_without_deadlock() {
        use std::sync::mpsc;
        use std::time::Duration;

        let scratch = Scratch::new();
        let store = Arc::new(OrgRevocationStore::init(scratch.state_path()).expect("init"));
        // The guard lives in a slot the callback takes + drops from inside.
        let slot: Arc<Mutex<Option<RaiseSubscription>>> = Arc::new(Mutex::new(None));
        let slot_cb = Arc::clone(&slot);
        let sub = store.subscribe_floors_raised(move |_raised| {
            // Self-unsubscribe: drop this subscription's own guard.
            let _dropped = slot_cb.lock().take();
        });
        *slot.lock() = Some(sub);

        let (done_tx, done_rx) = mpsc::channel::<()>();
        let store_t = Arc::clone(&store);
        let worker = std::thread::spawn(move || {
            store_t
                .apply_bundle(&bundle_with_floor(5))
                .expect("apply 5");
            done_tx.send(()).expect("signal done");
        });
        assert!(
            done_rx.recv_timeout(Duration::from_secs(5)).is_ok(),
            "a callback dropping its own guard must not deadlock",
        );
        worker.join().expect("worker joined");
        assert_eq!(
            store.subscriber_count(),
            0,
            "the self-drop removed the subscription",
        );
    }

    /// R3-4 (cross-thread): a callback that self-unsubscribes while ANOTHER
    /// thread is inside a callback of the SAME subscription must not wait for
    /// that foreign frame — even (especially) when the foreign frame is
    /// blocked on a user lock the self-unsubscribing callback holds.
    ///
    /// Timeline: A enters and takes a user lock; B enters and blocks needing
    /// that lock; A self-unsubscribes (dropping its own guard) WHILE holding
    /// the lock and while B is in-flight, then releases the lock so B can
    /// finish.
    ///
    /// Red-witness: the pre-fix `while in_flight > own_frames` wait blocks A
    /// (in_flight == 2, own_frames == 1) on B; B is blocked on the user lock A
    /// holds; A cannot release it until the wait returns → cross-thread
    /// deadlock, and the bounded `recv_timeout`s below expire. `leave`'s
    /// notify-only-at-zero made even relaxing the threshold insufficient; the
    /// fix is to not wait at all when `own_frames > 0`.
    #[test]
    fn self_unsubscribe_does_not_wait_for_a_concurrent_foreign_callback() {
        use std::sync::mpsc;
        use std::time::Duration;

        let scratch = Scratch::new();
        let store = Arc::new(OrgRevocationStore::init(scratch.state_path()).expect("init"));

        // A user lock A holds across its self-unsubscribe and B needs.
        let user_lock = Arc::new(Mutex::new(()));
        // First entrant is role A (self-unsubscriber), second is role B.
        let role = Arc::new(AtomicUsize::new(0));
        // A's own guard, taken + dropped from inside A's callback.
        let slot: Arc<Mutex<Option<RaiseSubscription>>> = Arc::new(Mutex::new(None));

        let (a_holds_tx, a_holds_rx) = mpsc::channel::<()>();
        let (b_entered_tx, b_entered_rx) = mpsc::channel::<()>();
        let (proceed_a_tx, proceed_a_rx) = mpsc::channel::<()>();
        // The callback is `Fn + Send + Sync`; the mpsc endpoints are `!Sync`.
        let a_holds_tx = Mutex::new(a_holds_tx);
        let b_entered_tx = Mutex::new(b_entered_tx);
        let proceed_a_rx = Mutex::new(proceed_a_rx);

        let user_lock_cb = Arc::clone(&user_lock);
        let role_cb = Arc::clone(&role);
        let slot_cb = Arc::clone(&slot);
        let sub = store.subscribe_floors_raised(move |_raised| {
            if role_cb.fetch_add(1, Ordering::SeqCst) == 0 {
                // Role A: hold the user lock, announce, await the go-ahead,
                // then self-unsubscribe WHILE holding the lock and while B is
                // in-flight, and only then release the lock.
                let held = user_lock_cb.lock();
                a_holds_tx
                    .lock()
                    .send(())
                    .expect("A announces it holds the lock");
                proceed_a_rx.lock().recv().expect("A awaits go-ahead");
                drop(slot_cb.lock().take()); // self-unsubscribe (must not block)
                drop(held); // release → B can proceed
            } else {
                // Role B: needs the user lock A holds.
                b_entered_tx.lock().send(()).expect("B announces entry");
                let _held = user_lock_cb.lock();
            }
        });
        *slot.lock() = Some(sub);

        let (done_tx, done_rx) = mpsc::channel::<()>();

        // Fire A on thread 1; wait until it holds the user lock (role 0 taken).
        let store1 = Arc::clone(&store);
        let done1 = done_tx.clone();
        let t1 = std::thread::spawn(move || {
            store1.apply_bundle(&bundle_with_floor(5)).expect("apply 5");
            done1.send(()).expect("t1 done");
        });
        a_holds_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("A entered and holds the user lock");

        // Fire B on thread 2; wait until it has entered (in_flight == 2).
        let store2 = Arc::clone(&store);
        let done2 = done_tx.clone();
        let t2 = std::thread::spawn(move || {
            store2.apply_bundle(&bundle_with_floor(6)).expect("apply 6");
            done2.send(()).expect("t2 done");
        });
        b_entered_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("B entered the callback");

        // Release A: self-unsubscribe must return without waiting for B, so A
        // releases the user lock and BOTH workers finish within the bound.
        proceed_a_tx.send(()).expect("release A");
        for _ in 0..2 {
            done_rx
                .recv_timeout(Duration::from_secs(5))
                .expect("a worker deadlocked in self-unsubscribe");
        }
        t1.join().expect("thread 1 joined");
        t2.join().expect("thread 2 joined");

        assert_eq!(
            role.load(Ordering::SeqCst),
            2,
            "exactly A and B ran (each once)",
        );
        assert_eq!(
            store.subscriber_count(),
            0,
            "the self-drop removed the subscription",
        );
        // No future callback enters: the subscriber is gone, so a later raise
        // fires nothing and the role counter stays at 2.
        store.apply_bundle(&bundle_with_floor(7)).expect("apply 7");
        assert_eq!(
            role.load(Ordering::SeqCst),
            2,
            "no callback runs after self-unsubscription removed the subscriber",
        );
    }

    /// Review-9 addendum: aliased pathnames — `..` hops, `./`
    /// prefixes, symlinked parents — normalize onto ONE core and
    /// ONE poison key; no verbatim fallback survives.
    #[test]
    fn aliased_paths_share_one_core() {
        let scratch = Scratch::new();
        let sub = scratch.0.join("sub");
        std::fs::create_dir_all(&sub).expect("mkdir sub");
        let direct = sub.join("revocation-state.json");
        let dotted = scratch.0.join("sub/../sub/revocation-state.json");

        let store_a = OrgRevocationStore::init(&direct).expect("init direct");
        let store_b = OrgRevocationStore::open_existing(&dotted).expect("open dotted alias");
        assert!(
            store_a.shares_core_with(&store_b),
            "`..` alias joins the core"
        );
        store_a.apply_bundle(&bundle_with_floor(5)).expect("apply");
        assert_eq!(store_b.floor_for(&org().org_id(), &member()), 5);

        #[cfg(unix)]
        {
            let link = scratch.0.join("linked-sub");
            std::os::unix::fs::symlink(&sub, &link).expect("symlink dir");
            let via_link = OrgRevocationStore::open_existing(link.join("revocation-state.json"))
                .expect("open through symlinked parent");
            assert!(
                store_a.shares_core_with(&via_link),
                "symlinked-parent alias joins the core"
            );
        }

        // Normalization invariants: bare and `./`-prefixed names
        // resolve absolute (no verbatim fallback)…
        let bare = normalize_backing_path(Path::new("bare-floors.json")).expect("bare");
        let dot = normalize_backing_path(Path::new("./bare-floors.json")).expect("dot");
        assert!(bare.is_absolute());
        assert_eq!(bare, dot);
        // …and a path that cannot normalize is refused, never keyed
        // verbatim.
        assert!(
            normalize_backing_path(&scratch.0.join("no-such-dir/state.json")).is_err(),
            "unresolvable parent must refuse"
        );
        assert!(
            normalize_backing_path(Path::new("..")).is_err(),
            "no final component must refuse"
        );
    }

    /// Review-11 P2: the final component is validated ATOMICALLY
    /// (no-follow open), so a symlink final is refused in one
    /// syscall — no `symlink_metadata`→`canonicalize` TOCTOU. The
    /// parent is still canonicalized, so parent-side aliasing
    /// collapses; final-component case aliasing is deliberately NOT
    /// folded (the filename is a fixed constant in every real call
    /// site — trading it away removes the race).
    #[cfg(unix)]
    #[test]
    fn final_component_symlink_is_refused_atomically() {
        let scratch = Scratch::new();
        let path = scratch.state_path();
        OrgRevocationStore::init(&path).expect("init");
        drop(OrgRevocationStore::open_existing(&path).expect("regular final opens"));

        // Swap the final component for a symlink to a real file: the
        // no-follow validation refuses it — canonicalize never gets
        // the chance to follow it and re-key the store.
        let real = scratch.0.join("real-state.json");
        std::fs::rename(&path, &real).expect("move real");
        std::os::unix::fs::symlink(&real, &path).expect("plant final symlink");
        assert!(
            normalize_backing_path(&path).is_err(),
            "a symlink final component must refuse atomically"
        );
        assert!(
            OrgRevocationStore::open_existing(&path).is_err(),
            "open must refuse a symlink final"
        );
    }

    /// Review-9: lock inodes are held to the full regular-file
    /// policy — a planted FIFO is refused (and, thanks to
    /// `O_NONBLOCK`, cannot park the open forever waiting for a
    /// reader), it does not carry the lock.
    #[cfg(unix)]
    #[test]
    fn non_regular_lock_sidecar_is_refused() {
        let scratch = Scratch::new();
        let path = scratch.state_path();
        let mut lock_path = path.as_os_str().to_os_string();
        lock_path.push(".lock");
        let status = std::process::Command::new("mkfifo")
            .arg(&lock_path)
            .status()
            .expect("run mkfifo");
        assert!(status.success(), "mkfifo failed");

        let err = OrgRevocationStore::init(&path).expect_err("FIFO lock must refuse");
        assert!(matches!(err, OrgRevocationError::Io { .. }), "got: {err}");
    }

    /// R2-4 (#3): a `.lock` sidecar with more than one hard link is
    /// refused fail-closed. Hard-linking a sidecar's inode to a second
    /// name is the attack that would otherwise collapse two DISTINCT
    /// backing paths onto one [`BackingId`] (one core + one poison
    /// entry).
    ///
    /// Red-witness: dropping the link-count check in `lock_state_file` lets
    /// the reopen succeed. Runs on both Unix (`nlink`) and Windows
    /// (`GetFileInformationByHandle`'s `nNumberOfLinks`) — a hard-linked
    /// sidecar must be refused identically on each.
    #[cfg(any(unix, windows))]
    #[test]
    fn hard_linked_lock_sidecar_is_refused() {
        let scratch = Scratch::new();
        let path = scratch.state_path();
        // First open creates the sidecar (nlink == 1), then drops so its
        // advisory lock and core are released.
        drop(OrgRevocationStore::init(&path).expect("init"));
        let mut lock_path = path.as_os_str().to_os_string();
        lock_path.push(".lock");
        let lock_path = PathBuf::from(lock_path);
        let alias = scratch.0.join("alias.lock");
        std::fs::hard_link(&lock_path, &alias).expect("hard-link the sidecar");
        let err = OrgRevocationStore::open_existing(&path)
            .expect_err("a hard-linked sidecar must be refused");
        assert!(
            matches!(&err, OrgRevocationError::Io { reason, .. } if reason.contains("hard links")),
            "got: {err}",
        );
    }

    /// R2-4 (#2): the file-identity fallback key carries the COMPLETE
    /// normalized path, never a 64-bit hash — so two distinct paths can
    /// never collide onto one core/poison entry (the pre-R2-4
    /// `DefaultHasher` fallback could, at the ~2^32 birthday bound).
    ///
    /// This pins the fallback's TYPE (a `PathBuf`, not a hashed `u64`);
    /// the fstat-failure branch that selects it cannot be provoked from a
    /// unit test.
    #[test]
    fn path_fallback_backing_id_retains_the_full_path() {
        let a = BackingId::Path(PathBuf::from("/x/alpha/revocation-state.json"));
        let a2 = BackingId::Path(PathBuf::from("/x/alpha/revocation-state.json"));
        let b = BackingId::Path(PathBuf::from("/x/beta/revocation-state.json"));
        assert_eq!(a, a2, "the same normalized path is the same identity");
        assert_ne!(a, b, "distinct paths never share a fallback identity");
        assert_ne!(
            BackingId::FileId {
                device: 0,
                inode: 0
            },
            BackingId::Path(PathBuf::new()),
            "file-identity and path-fallback are distinct identity spaces",
        );
    }

    /// R2-4 (#4): a backing path whose `.lock` sidecar is REPLACED under
    /// a still-live core is refused loudly, rather than silently forking
    /// the path into a second independent core (two security views of one
    /// path).
    ///
    /// Red-witness: removing the binding check in `join_or_create_core`
    /// lets the second open create a fresh core for the new sidecar
    /// identity, so it succeeds instead of failing.
    #[cfg(unix)]
    #[test]
    fn recreated_sidecar_under_a_live_core_is_refused() {
        let scratch = Scratch::new();
        let path = scratch.state_path();
        // Keep the first store ALIVE: its core (and the path→identity
        // binding) survive the whole test.
        let live = OrgRevocationStore::init(&path).expect("init");
        let mut lock_path = path.as_os_str().to_os_string();
        lock_path.push(".lock");
        let lock_path = PathBuf::from(lock_path);
        // Replace the sidecar: unlink its directory entry (the original
        // inode persists under `live`'s open fd + advisory lock) and let
        // the next open create a FRESH inode under the same name.
        std::fs::remove_file(&lock_path).expect("unlink the old sidecar");
        let err = OrgRevocationStore::open_existing(&path)
            .expect_err("a recreated sidecar under a live core must be refused");
        assert!(
            matches!(err, OrgRevocationError::BackingIdentityConflict { .. }),
            "got: {err}",
        );
        drop(live);
    }

    /// Review-8 §9 plumbing: the raise callback fires with exactly
    /// the raised floors, and never for a no-op (lower) bundle.
    #[test]
    fn raise_callback_fires_only_on_raises() {
        let scratch = Scratch::new();
        let store = OrgRevocationStore::init(scratch.state_path()).expect("init");

        let seen: Arc<Mutex<Vec<RaisedFloor>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = seen.clone();
        let _sub = store.subscribe_floors_raised(move |raised| {
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

    /// R3-2: durability poison SURVIVES dropping every handle and
    /// recreating the `.lock` sidecar. Poison keyed only on the live
    /// sidecar [`BackingId`] would be laundered — a recreated `.lock` is a
    /// fresh, unpoisoned inode — so the canonical PATH tombstone keeps the
    /// path poisoned until explicit recovery, exactly once.
    ///
    /// Red-witness: dropping the `by_path` arm of `is_poisoned` lets the
    /// recreated-sidecar reopen skip recovery, so the "recovery still
    /// mandatory" `is_err()` assertion fails.
    #[cfg(unix)]
    #[test]
    fn poison_survives_dead_core_sidecar_recreation() {
        use std::os::unix::fs::PermissionsExt;
        let scratch = Scratch::new();
        let path = scratch.state_path();
        let mut lock_path = path.as_os_str().to_os_string();
        lock_path.push(".lock");
        let lock_path = PathBuf::from(lock_path);

        // 1. Create + poison via a real post-rename parent-fsync failure.
        let store = OrgRevocationStore::init(&path).expect("init");
        store.apply_bundle(&bundle_with_floor(5)).expect("apply 5");
        std::fs::set_permissions(&scratch.0, std::fs::Permissions::from_mode(0o300))
            .expect("chmod 0300");
        let err = store
            .apply_bundle(&bundle_with_floor(9))
            .expect_err("dir fsync must fail");
        assert!(
            matches!(err, OrgRevocationError::DurabilityUncertain { .. }),
            "got: {err}"
        );
        assert!(store.is_poisoned());

        // 2. Drop every handle: the core dies and its path binding is
        //    GC'd, so ONLY the poison registry remembers the uncertainty.
        drop(store);

        // 3. Replace the `.lock` sidecar with a fresh inode (new BackingId).
        std::fs::set_permissions(&scratch.0, std::fs::Permissions::from_mode(0o700))
            .expect("chmod 0700");
        std::fs::remove_file(&lock_path).expect("unlink old sidecar");

        // 4. Reopen with recovery still BLOCKED — the path poison survived
        //    the sidecar swap, so the fsync-refused reopen is refused.
        std::fs::set_permissions(&scratch.0, std::fs::Permissions::from_mode(0o300))
            .expect("chmod 0300 again");
        assert!(
            OrgRevocationStore::open_existing(&path).is_err(),
            "recovery must still be mandatory after sidecar recreation — poison survived",
        );

        // 5. Repair: the reopen recovers and clears the poison exactly once.
        std::fs::set_permissions(&scratch.0, std::fs::Permissions::from_mode(0o700))
            .expect("chmod back");
        let recovered = OrgRevocationStore::open_existing(&path).expect("recovered reopen");
        assert!(
            !recovered.is_poisoned(),
            "successful recovery clears poison"
        );
        assert_eq!(recovered.floor_for(&org().org_id(), &member()), 9);
        let reopened = OrgRevocationStore::open_existing(&path).expect("clean reopen");
        assert!(
            !reopened.is_poisoned(),
            "poison stays cleared (cleared exactly once)"
        );
    }

    /// R3-2: the same survival holds when the recreated path is reopened
    /// through a DIFFERENTLY-CASED alias on a case-insensitive filesystem —
    /// the canonical tombstone collapses the alias through the actual
    /// filesystem identity, so it is caught even after the sidecar swap.
    #[cfg(unix)]
    #[test]
    fn poison_survives_sidecar_recreation_across_a_cased_alias() {
        use std::os::unix::fs::PermissionsExt;
        let scratch = Scratch::new();
        let lower = scratch.0.join("revocation-state.json");
        let upper = scratch.0.join("REVOCATION-STATE.JSON");

        let store = OrgRevocationStore::init(&lower).expect("init lower");
        // Case-insensitivity probe — a no-op on a case-sensitive FS.
        if !upper.exists() {
            return;
        }
        store.apply_bundle(&bundle_with_floor(5)).expect("apply 5");
        std::fs::set_permissions(&scratch.0, std::fs::Permissions::from_mode(0o300))
            .expect("chmod 0300");
        let err = store
            .apply_bundle(&bundle_with_floor(9))
            .expect_err("dir fsync must fail");
        assert!(matches!(
            err,
            OrgRevocationError::DurabilityUncertain { .. }
        ));
        drop(store);

        // Recreate the `.lock` sidecar (new inode).
        std::fs::set_permissions(&scratch.0, std::fs::Permissions::from_mode(0o700))
            .expect("chmod 0700");
        let mut lock_path = lower.as_os_str().to_os_string();
        lock_path.push(".lock");
        std::fs::remove_file(PathBuf::from(lock_path)).expect("unlink sidecar");

        // Reopen through the UPPER-cased alias with recovery still blocked:
        // the tombstone must survive BOTH the sidecar swap and the alias.
        std::fs::set_permissions(&scratch.0, std::fs::Permissions::from_mode(0o300))
            .expect("chmod 0300 again");
        assert!(
            OrgRevocationStore::open_existing(&upper).is_err(),
            "poison must survive a sidecar swap AND a cased-alias reopen",
        );
        std::fs::set_permissions(&scratch.0, std::fs::Permissions::from_mode(0o700))
            .expect("chmod back");
        let recovered = OrgRevocationStore::open_existing(&upper).expect("recovered via alias");
        assert!(!recovered.is_poisoned());
    }

    /// P2 hygiene: recovering a canonical path retires EVERY sidecar identity
    /// ever poisoned under it — not just the recovering one — so a stale old
    /// `BackingId` (left behind when a sidecar was unlinked and recreated)
    /// does not linger in `by_id` to trip redundant recovery after inode
    /// reuse. Directly exercises the poison registry (no filesystem poison
    /// needed), so it runs on every platform.
    ///
    /// Red-witness: reverting `clear_poison` to remove only the passed `id`
    /// leaves `old_id` poisoned, so the final `is_poisoned(&old_id, ..)` holds.
    #[test]
    fn clear_poison_retires_all_stale_ids_for_the_path() {
        let scratch = Scratch::new();
        let path = scratch.state_path();
        let old_id = BackingId::FileId {
            device: 0x5005,
            inode: 0xF00D_0001,
        };
        let new_id = BackingId::FileId {
            device: 0x5005,
            inode: 0xF00D_0002,
        };
        // Two sidecar identities poisoned under the SAME path — the
        // unlink+recreate that strands the old id in `by_id`.
        mark_poisoned(&old_id, &path);
        mark_poisoned(&new_id, &path);
        assert!(is_poisoned(&old_id, &path));
        assert!(is_poisoned(&new_id, &path));

        // Recovery through the CURRENT (new) id clears the path tombstone AND
        // retires the stale old id in lockstep — no dead residue survives.
        clear_poison(&new_id, &path);
        assert!(!is_poisoned(&new_id, &path), "recovered id cleared");
        assert!(
            !is_poisoned(&old_id, &path),
            "stale old id retired with the path recovery — no dead residue",
        );
    }

    /// R3-3: an existing live handle's `apply_bundle` verifies the opened
    /// `.lock` sidecar identity against its core's BEFORE reread/merge/
    /// write. If the sidecar was replaced under the live handle (fresh
    /// inode — which the `nlink` refusal does not catch), the transaction
    /// is refused loudly with `BackingIdentityConflict` and neither the
    /// live view nor the disk floors advance.
    ///
    /// Red-witness: dropping the `opened_id != core.backing_id` check lets
    /// the existing handle lock and publish through the replaced sidecar,
    /// so `apply_bundle` returns `Ok` and the floor advances to 9.
    #[cfg(unix)]
    #[test]
    fn existing_handle_refuses_a_replaced_sidecar() {
        let scratch = Scratch::new();
        let path = scratch.state_path();
        let store = OrgRevocationStore::init(&path).expect("init");
        store.apply_bundle(&bundle_with_floor(5)).expect("apply 5");

        // Replace the `.lock` sidecar under the LIVE store: unlink it (the
        // old inode persists via the store's open fd + advisory lock); the
        // next lock open recreates it with a fresh inode.
        let mut lock_path = path.as_os_str().to_os_string();
        lock_path.push(".lock");
        std::fs::remove_file(PathBuf::from(lock_path)).expect("unlink sidecar");

        let err = store
            .apply_bundle(&bundle_with_floor(9))
            .expect_err("existing handle must refuse a replaced sidecar");
        assert!(
            matches!(err, OrgRevocationError::BackingIdentityConflict { .. }),
            "got: {err}"
        );
        // The live view never advanced past 5.
        assert_eq!(store.floor_for(&org().org_id(), &member()), 5);

        // Nor did the disk: a fresh handle (after the live core drops so
        // its stale path binding is released) reads 5, never 9.
        drop(store);
        let reopened = OrgRevocationStore::open_existing(&path).expect("reopen after drop");
        assert_eq!(
            reopened.floor_for(&org().org_id(), &member()),
            5,
            "the refused transaction must not have written floor 9 to disk",
        );
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
        let _sub = store.subscribe_floors_raised(move |raised| {
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
        // following the link to a foreign inode — at open time
        // (every open serializes behind the lock, review-9
        // addendum) and on a reload through a previously-opened
        // handle alike.
        let store = OrgRevocationStore::open_existing(&path).expect("open before planting");
        let mut lock_path = path.as_os_str().to_os_string();
        lock_path.push(".lock");
        let lock_path = PathBuf::from(lock_path);
        let _ = std::fs::remove_file(&lock_path);
        let foreign = scratch.0.join("foreign.lock");
        std::fs::write(&foreign, b"").expect("foreign lock");
        std::os::unix::fs::symlink(&foreign, &lock_path).expect("plant lock symlink");
        assert!(
            OrgRevocationStore::open_existing(&path).is_err(),
            "symlinked lock sidecar must refuse the open"
        );
        assert!(
            store.apply_bundle(&bundle_with_floor(9)).is_err(),
            "symlinked lock sidecar must refuse a reload"
        );
    }

    /// OA2-E1 (Kyra review) — the publication barrier. A barriered
    /// generation read issued while a floor publish is paused between
    /// the live-view swap and the generation bump must NOT observe the
    /// stale (pre-bump) generation the bare `publish_generation()`
    /// still returns; it blocks on `live.read()` and, once released,
    /// returns the NEW generation. Deterministic: the publisher is
    /// pinned in the exact "new view installed, old generation
    /// present" window by the one-shot pause hook.
    #[test]
    fn barriered_generation_never_observes_an_in_progress_publish() {
        use std::sync::mpsc;

        let scratch = Scratch::new();
        let store = Arc::new(OrgRevocationStore::init(scratch.state_path()).expect("init"));
        let g0 = store.barriered_generation();

        // Arm the one-shot pause, then raise a floor on another thread:
        // it swaps the live view and blocks BEFORE bumping the
        // generation, holding `live.write()` throughout.
        let (swapped_rx, resume_tx) = store.arm_publish_pause_for_test();
        let publisher = {
            let store = store.clone();
            std::thread::spawn(move || {
                store.apply_bundle(&bundle_with_floor(9)).expect("apply");
            })
        };
        swapped_rx.recv().expect("publisher reached the pause");

        // Window open: new view installed, generation NOT yet bumped,
        // write lock held. A BARE read sees the stale generation — the
        // hazard the barrier closes.
        assert_eq!(
            store.publish_generation(),
            g0,
            "bare read observes the pre-bump generation while the new floor is already swapped in",
        );

        // A BARRIERED read issued now must block on `live.read()` — no
        // result until the publisher releases the write lock.
        let (reader_tx, reader_rx) = mpsc::channel();
        let reader = {
            let store = store.clone();
            std::thread::spawn(move || {
                let g = store.barriered_generation();
                let _ = reader_tx.send(g);
            })
        };
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(
            reader_rx.try_recv().is_err(),
            "barriered read must block while the publish holds live.write() mid-swap",
        );

        // Release: the publisher bumps the generation and drops the
        // write lock; the barriered reader unblocks.
        resume_tx.send(()).expect("resume");
        publisher.join().expect("publisher join");
        reader.join().expect("reader join");

        let observed = reader_rx.recv().expect("barriered read result");
        assert_eq!(
            observed,
            g0 + 1,
            "the barriered read returned the NEW generation, never the stale one",
        );
        assert_eq!(store.barriered_generation(), g0 + 1);
        assert!(store.floor_for(&org().org_id(), &member()) >= 9);
    }
}
