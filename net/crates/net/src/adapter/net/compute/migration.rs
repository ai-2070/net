//! Stateful daemon migration.
//!
//! Migration uses L4 `StateSnapshot` to move a daemon between nodes while
//! preserving causal chain continuity. The process is a 6-phase state machine.

use crate::adapter::net::state::causal::CausalEvent;
use crate::adapter::net::state::snapshot::StateSnapshot;

/// Subprotocol ID for migration control messages.
pub const SUBPROTOCOL_MIGRATION: u16 = 0x0500;

/// Phases of daemon migration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MigrationPhase {
    /// Take snapshot on source node.
    Snapshot,
    /// Transfer snapshot to target node.
    Transfer,
    /// Restore daemon on target, start buffering events.
    Restore,
    /// Replay buffered events on target.
    Replay,
    /// Atomic routing cutover: new events go to target.
    Cutover,
    /// Cleanup source.
    Complete,
}

/// Structured reason the migration target rejected (or the
/// orchestrator aborted) a migration. Replaces the free-form
/// `reason: String` on `MigrationMessage::MigrationFailed` so the
/// source can dispatch programmatically on the cause — specifically,
/// distinguish "retry this, the target is still booting" (`NotReady`)
/// from "give up, the target doesn't know this daemon kind"
/// (`FactoryNotFound`).
///
/// See [`DAEMON_RUNTIME_READINESS_PLAN.md`](../../../../docs/DAEMON_RUNTIME_READINESS_PLAN.md)
/// for the retry-classification table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MigrationFailureReason {
    /// Target runtime exists but hasn't called `start()` yet — the
    /// dispatcher received the migration before the runtime was
    /// ready to accept one. **Retriable**: source should back off
    /// + resend.
    NotReady,
    /// Target has no factory registered for the daemon's
    /// `origin_hash` (supplied in the outer `MigrationFailed`
    /// envelope). **Terminal** — retrying won't help; the target
    /// is mis-configured (wrong node), the kind is wrong, or the
    /// daemon never registered.
    FactoryNotFound,
    /// Target doesn't run a compute runtime at all (a bare `Mesh`
    /// with no `DaemonRuntime` attached). **Terminal** — source
    /// should pick a different target.
    ComputeNotSupported,
    /// Generic snapshot / restore / state-machine failure. Carries
    /// a human-readable detail. **Terminal.**
    StateFailed(String),
    /// A migration is already in flight for the same origin.
    /// **Terminal** on the duplicate attempt — caller should not
    /// retry, and the currently-active migration should be allowed
    /// to run to completion.
    AlreadyMigrating,
    /// Identity envelope failure: signature didn't verify, seal
    /// open failed, etc. **Terminal** — tampering or misconfigured
    /// target X25519 key; retry won't fix it.
    IdentityTransportFailed(String),
    /// Source gave up after exhausting its `NotReady` retry budget.
    /// **Terminal** on both sides; carries the retry attempt count
    /// for operator diagnosis.
    NotReadyTimeout {
        /// Number of `NotReady` retries the source attempted before
        /// giving up. ≥ 1 because the first attempt always counts.
        attempts: u8,
    },
}

impl MigrationFailureReason {
    /// `true` iff the source should retry after a short backoff
    /// when it sees this reason. Today only `NotReady` qualifies —
    /// the others are terminal.
    pub fn is_retriable(&self) -> bool {
        matches!(self, MigrationFailureReason::NotReady)
    }

    /// 16-bit wire code. Separating the code from the payload lets
    /// the dispatcher's decoder match on the tag cheaply and the
    /// payload length on-line with the variant.
    pub fn code(&self) -> u16 {
        match self {
            MigrationFailureReason::NotReady => 0,
            MigrationFailureReason::FactoryNotFound => 1,
            MigrationFailureReason::ComputeNotSupported => 2,
            MigrationFailureReason::StateFailed(_) => 3,
            MigrationFailureReason::AlreadyMigrating => 4,
            MigrationFailureReason::IdentityTransportFailed(_) => 5,
            MigrationFailureReason::NotReadyTimeout { .. } => 6,
        }
    }
}

impl std::fmt::Display for MigrationFailureReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotReady => write!(f, "target runtime not ready yet"),
            Self::FactoryNotFound => {
                write!(f, "no factory registered on target for this daemon")
            }
            Self::ComputeNotSupported => {
                write!(f, "target does not run a compute runtime")
            }
            Self::StateFailed(msg) => write!(f, "state failed: {msg}"),
            Self::AlreadyMigrating => write!(f, "daemon is already migrating"),
            Self::IdentityTransportFailed(msg) => {
                write!(f, "identity envelope transport failed: {msg}")
            }
            Self::NotReadyTimeout { attempts } => {
                write!(f, "source gave up after {attempts} NotReady retries")
            }
        }
    }
}

/// State of an in-progress migration.
pub struct MigrationState {
    /// Origin hash of the daemon being migrated.
    daemon_origin: u32,
    /// Source node ID.
    source_node: u64,
    /// Target node ID.
    target_node: u64,
    /// Current phase (only mutable through transition methods).
    phase: MigrationPhase,
    /// Snapshot taken from source (set in Snapshot phase).
    snapshot: Option<StateSnapshot>,
    /// Events buffered between snapshot and cutover.
    buffered_events: Vec<CausalEvent>,
    /// Monotonic instant when migration started. Pre-fix this
    /// was a `u64` of wall-clock nanoseconds, and `elapsed_ms`
    /// did `current_timestamp().saturating_sub(self.started_at)`
    /// — a wall-clock jump backward (NTP step, manual `date`,
    /// VM resume to an earlier moment) would saturate to `0`
    /// and report the migration as instantaneous, masking
    /// long-running stalls in operator dashboards. `Instant` is
    /// monotonic by contract and is unaffected by clock jumps.
    started_at: std::time::Instant,
}

impl MigrationState {
    /// Create a new migration.
    pub fn new(daemon_origin: u32, source_node: u64, target_node: u64) -> Self {
        Self {
            daemon_origin,
            source_node,
            target_node,
            phase: MigrationPhase::Snapshot,
            snapshot: None,
            buffered_events: Vec::new(),
            started_at: std::time::Instant::now(),
        }
    }

    /// Buffer an event that arrived during migration.
    pub fn buffer_event(&mut self, event: CausalEvent) {
        self.buffered_events.push(event);
    }

    /// Set the snapshot and advance to Transfer phase.
    pub fn set_snapshot(&mut self, snapshot: StateSnapshot) -> Result<(), MigrationError> {
        if self.phase != MigrationPhase::Snapshot {
            return Err(MigrationError::WrongPhase {
                expected: MigrationPhase::Snapshot,
                got: self.phase,
            });
        }
        // Validate snapshot belongs to the daemon being migrated
        if snapshot.entity_id.origin_hash() != self.daemon_origin {
            return Err(MigrationError::StateFailed(format!(
                "snapshot origin {:#x} does not match daemon {:#x}",
                snapshot.entity_id.origin_hash(),
                self.daemon_origin,
            )));
        }
        self.snapshot = Some(snapshot);
        self.phase = MigrationPhase::Transfer;
        Ok(())
    }

    /// Mark transfer complete, advance to Restore.
    pub fn transfer_complete(&mut self) -> Result<(), MigrationError> {
        if self.phase != MigrationPhase::Transfer {
            return Err(MigrationError::WrongPhase {
                expected: MigrationPhase::Transfer,
                got: self.phase,
            });
        }
        self.phase = MigrationPhase::Restore;
        Ok(())
    }

    /// Mark restore complete, advance to Replay.
    pub fn restore_complete(&mut self) -> Result<(), MigrationError> {
        if self.phase != MigrationPhase::Restore {
            return Err(MigrationError::WrongPhase {
                expected: MigrationPhase::Restore,
                got: self.phase,
            });
        }
        self.phase = MigrationPhase::Replay;
        Ok(())
    }

    /// Take buffered events for replay (drains the buffer).
    pub fn take_buffered_events(&mut self) -> Vec<CausalEvent> {
        std::mem::take(&mut self.buffered_events)
    }

    /// Mark replay complete, advance to Cutover.
    pub fn replay_complete(&mut self) -> Result<(), MigrationError> {
        if self.phase != MigrationPhase::Replay {
            return Err(MigrationError::WrongPhase {
                expected: MigrationPhase::Replay,
                got: self.phase,
            });
        }
        self.phase = MigrationPhase::Cutover;
        Ok(())
    }

    /// Mark cutover complete, advance to Complete.
    pub fn cutover_complete(&mut self) -> Result<(), MigrationError> {
        if self.phase != MigrationPhase::Cutover {
            return Err(MigrationError::WrongPhase {
                expected: MigrationPhase::Cutover,
                got: self.phase,
            });
        }
        self.phase = MigrationPhase::Complete;
        Ok(())
    }

    /// Force the phase to a specific value without validation.
    ///
    /// Used for multi-chunk snapshots where the orchestrator needs to advance
    /// past Snapshot without having the full snapshot for `set_snapshot()`.
    /// The target will validate the reassembled snapshot.
    pub(crate) fn force_phase(&mut self, phase: MigrationPhase) {
        self.phase = phase;
    }

    /// Check if migration is finished.
    pub fn is_complete(&self) -> bool {
        self.phase == MigrationPhase::Complete
    }

    /// Elapsed time in milliseconds since the migration was
    /// constructed. Backed by a monotonic `Instant`, so a system
    /// clock that jumps backward (NTP step, VM resume) does not
    /// reset this to `0`; long-running migrations stay observable
    /// in operator dashboards.
    pub fn elapsed_ms(&self) -> u64 {
        u64::try_from(self.started_at.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    /// Get the daemon origin hash.
    #[inline]
    pub fn daemon_origin(&self) -> u32 {
        self.daemon_origin
    }

    /// Get the source node ID.
    #[inline]
    pub fn source_node(&self) -> u64 {
        self.source_node
    }

    /// Get the target node ID.
    #[inline]
    pub fn target_node(&self) -> u64 {
        self.target_node
    }

    /// Get the current phase.
    #[inline]
    pub fn phase(&self) -> MigrationPhase {
        self.phase
    }

    /// Get the snapshot (if taken).
    #[inline]
    pub fn snapshot(&self) -> Option<&StateSnapshot> {
        self.snapshot.as_ref()
    }
}

impl std::fmt::Debug for MigrationState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MigrationState")
            .field("daemon", &format!("{:#x}", self.daemon_origin))
            .field("source", &format!("{:#x}", self.source_node))
            .field("target", &format!("{:#x}", self.target_node))
            .field("phase", &self.phase)
            .field("buffered", &self.buffered_events.len())
            .field("has_snapshot", &self.snapshot.is_some())
            .finish()
    }
}

/// Errors from migration operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MigrationError {
    /// Daemon not registered locally.
    DaemonNotFound(u32),
    /// Target node unreachable or refused.
    TargetUnavailable(u64),
    /// Auto-placement found no candidate node satisfying the
    /// daemon's capability requirements. Distinct from
    /// `TargetUnavailable(_)` which carries a specific failed
    /// target — auto-placement never has one to report. Pre-fix
    /// the auto path constructed `TargetUnavailable(0)`,
    /// surfacing "target node 0x0 unavailable" to operators when
    /// no specific node had ever been tried.
    NoTargetAvailable,
    /// Snapshot/restore failure.
    StateFailed(String),
    /// Migration already in progress for this daemon.
    AlreadyMigrating(u32),
    /// Attempted to advance from wrong phase.
    WrongPhase {
        /// Expected phase.
        expected: MigrationPhase,
        /// Actual phase.
        got: MigrationPhase,
    },
    /// Snapshot exceeds the maximum transferable size.
    SnapshotTooLarge {
        /// Actual size in bytes.
        size: usize,
        /// Maximum allowed size in bytes.
        max: usize,
    },
}

impl std::fmt::Display for MigrationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DaemonNotFound(id) => write!(f, "daemon {:#x} not found", id),
            Self::TargetUnavailable(id) => write!(f, "target node {:#x} unavailable", id),
            Self::NoTargetAvailable => {
                write!(
                    f,
                    "no candidate node satisfies the daemon's capability requirements"
                )
            }
            Self::StateFailed(msg) => write!(f, "state operation failed: {}", msg),
            Self::AlreadyMigrating(id) => write!(f, "daemon {:#x} already migrating", id),
            Self::WrongPhase { expected, got } => {
                write!(
                    f,
                    "wrong migration phase: expected {:?}, got {:?}",
                    expected, got
                )
            }
            Self::SnapshotTooLarge { size, max } => {
                write!(
                    f,
                    "snapshot too large: {} bytes exceeds max {} bytes",
                    size, max
                )
            }
        }
    }
}

impl std::error::Error for MigrationError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::net::state::causal::CausalLink;
    use bytes::Bytes;

    fn make_event(seq: u64) -> CausalEvent {
        CausalEvent {
            link: CausalLink {
                origin_hash: 0xAAAA,
                horizon_encoded: 0,
                sequence: seq,
                parent_hash: 0,
            },
            payload: Bytes::from_static(b"data"),
            received_at: 0,
        }
    }

    #[test]
    fn test_migration_phase_progression() {
        let kp = crate::adapter::net::identity::EntityKeypair::generate();
        let origin = kp.origin_hash();
        let mut state = MigrationState::new(origin, 0x1111, 0x2222);
        assert_eq!(state.phase(), MigrationPhase::Snapshot);

        // Can't skip phases
        assert!(state.transfer_complete().is_err());

        // Normal progression
        let snapshot = StateSnapshot::new(
            kp.entity_id().clone(),
            CausalLink::genesis(origin, 0),
            Bytes::from_static(b"state"),
            crate::adapter::net::state::horizon::ObservedHorizon::new(),
        );

        state.set_snapshot(snapshot).unwrap();
        assert_eq!(state.phase(), MigrationPhase::Transfer);

        state.transfer_complete().unwrap();
        assert_eq!(state.phase(), MigrationPhase::Restore);

        state.restore_complete().unwrap();
        assert_eq!(state.phase(), MigrationPhase::Replay);

        state.replay_complete().unwrap();
        assert_eq!(state.phase(), MigrationPhase::Cutover);

        state.cutover_complete().unwrap();
        assert_eq!(state.phase(), MigrationPhase::Complete);
        assert!(state.is_complete());
    }

    #[test]
    fn test_event_buffering() {
        let mut state = MigrationState::new(0xAAAA, 0x1111, 0x2222);

        state.buffer_event(make_event(1));
        state.buffer_event(make_event(2));
        state.buffer_event(make_event(3));

        let events = state.take_buffered_events();
        assert_eq!(events.len(), 3);
        assert!(state.buffered_events.is_empty());
    }

    #[test]
    fn test_wrong_phase_error() {
        let mut state = MigrationState::new(0xAAAA, 0x1111, 0x2222);

        let err = state.restore_complete().unwrap_err();
        assert_eq!(
            err,
            MigrationError::WrongPhase {
                expected: MigrationPhase::Restore,
                got: MigrationPhase::Snapshot,
            }
        );
    }

    // ---- Regression tests for Cubic AI findings ----

    #[test]
    fn test_regression_set_snapshot_rejects_wrong_origin() {
        // Regression: set_snapshot accepted snapshots from any entity,
        // allowing migration to bind state from the wrong daemon.
        let kp = crate::adapter::net::identity::EntityKeypair::generate();
        let wrong_origin = kp.origin_hash();

        // Migration is for daemon 0xBBBB, but snapshot is for kp's origin
        let mut state = MigrationState::new(0xBBBB, 0x1111, 0x2222);

        let snapshot = StateSnapshot::new(
            kp.entity_id().clone(),
            CausalLink::genesis(wrong_origin, 0),
            Bytes::from_static(b"state"),
            crate::adapter::net::state::horizon::ObservedHorizon::new(),
        );

        assert!(
            state.set_snapshot(snapshot).is_err(),
            "set_snapshot must reject snapshot from a different daemon"
        );
    }

    /// Source pin: `started_at` must be a monotonic `Instant`,
    /// not a wall-clock `u64` of nanoseconds. The pre-fix shape
    /// stored `current_timestamp()` (UNIX-epoch nanos) and
    /// computed `elapsed_ms` as `current_timestamp().saturating_sub(started_at)
    /// / 1_000_000`. A wall-clock jump backward (NTP step,
    /// manual `date` set, VM resume to an earlier moment)
    /// would saturate the subtraction to `0` and report a long
    /// migration as instantaneous, masking stalls in operator
    /// dashboards.
    ///
    /// We can't simulate a clock jump in a unit test, so this
    /// test pins the shape: the field must be an `Instant`, and
    /// `elapsed_ms` must derive from `started_at.elapsed()` —
    /// which is monotonic by contract. A revert to `u64` plus
    /// `current_timestamp().saturating_sub(...)` re-introduces
    /// the hazard and is rejected here.
    #[test]
    fn started_at_must_be_monotonic_instant_not_wall_clock_u64() {
        let src = include_str!("migration.rs");

        // Locate the `started_at` field declaration inside
        // `pub struct MigrationState { ... }`.
        let struct_marker = "pub struct MigrationState";
        let struct_start = src
            .find(struct_marker)
            .expect("MigrationState struct must exist");
        // The struct body ends at the next `}` at column 0 (or
        // before the next top-level `impl`/`pub`).
        let struct_end_offset = src[struct_start..]
            .find("\n}\n")
            .expect("struct body must terminate with `}`")
            + struct_start;
        let struct_body = &src[struct_start..struct_end_offset];

        let body_no_comments: String = struct_body
            .lines()
            .map(|l| match l.find("//") {
                Some(idx) => &l[..idx],
                None => l,
            })
            .collect::<Vec<_>>()
            .join("\n");

        assert!(
            body_no_comments.contains("started_at: std::time::Instant"),
            "regression: MigrationState.started_at must be a \
             monotonic `std::time::Instant`. A `u64` of wall-clock \
             nanoseconds is unsafe — a system clock that steps \
             backward (NTP / VM resume / manual `date`) saturates \
             elapsed_ms to 0 and masks long-running stalls."
        );
        assert!(
            !body_no_comments.contains("started_at: u64"),
            "regression: MigrationState.started_at must not be a \
             `u64` wall-clock timestamp."
        );

        // `elapsed_ms` must derive from `started_at.elapsed()`,
        // not from `current_timestamp().saturating_sub(...)`.
        let elapsed_marker = "pub fn elapsed_ms(";
        let elapsed_start = src.find(elapsed_marker).expect("elapsed_ms must exist");
        let elapsed_end_offset = src[elapsed_start..]
            .find("\n    }")
            .expect("elapsed_ms body must terminate")
            + elapsed_start;
        let elapsed_body = &src[elapsed_start..elapsed_end_offset];

        let elapsed_no_comments: String = elapsed_body
            .lines()
            .map(|l| match l.find("//") {
                Some(idx) => &l[..idx],
                None => l,
            })
            .collect::<Vec<_>>()
            .join("\n");

        assert!(
            elapsed_no_comments.contains("self.started_at.elapsed()"),
            "regression: elapsed_ms must derive from \
             `self.started_at.elapsed()` to stay monotonic. \
             Using `current_timestamp().saturating_sub(self.started_at)` \
             reintroduces the wall-clock-jump-saturates-to-zero hazard."
        );
        assert!(
            !elapsed_no_comments.contains("current_timestamp()"),
            "regression: elapsed_ms must not call \
             `current_timestamp()` — that's the wall-clock path \
             with the saturating-on-jump-backward bug."
        );
    }
}
