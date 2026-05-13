//! Action-chain integration — the missing piece between Phase F
//! (`MeshOsSnapshot`) and CortEX's `RedexFold<State>` surface.
//!
//! Today's executor dispatches actions but doesn't commit them
//! anywhere durable. Deck's snapshot view is built on demand via
//! [`super::event_loop::MeshOsSnapshotReader::read`] — sufficient
//! for in-process consumers but not for cross-node observation.
//! Phase F's design pointed at an "action chain" — a RedEX
//! chain whose entries are committed by the executor and whose
//! fold rebuilds a `MeshOsSnapshot` on each node.
//!
//! This module ships the integration scaffold:
//!
//! - [`ActionChainRecord`] — the serializable per-action wire
//!   form. Carries the action id + kind discriminator + wall-
//!   clock millis + disposition (Dispatched / Failed / Gated).
//! - [`ActionDisposition`] — the outcome the executor reports
//!   alongside each record.
//! - [`ActionChainAppender`] — trait the executor calls per
//!   action. A production impl writes records to a RedEX
//!   chain (the dispatcher knows which chain).
//! - [`MeshOsSnapshotFold`] — `impl RedexFold<MeshOsSnapshot>`
//!   that decodes records and updates the snapshot's
//!   `recent_failures` ring buffer.
//!
//! The integration is decoupled from `MeshOsAction` serialization
//! — the appender records only the kind discriminator + id +
//! disposition + reason. Full action serialization rides a
//! separate channel if a consumer asks for it.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::adapter::net::redex::{RedexError, RedexEvent, RedexFold};

use super::action::{MeshOsAction, PendingAction};
use super::snapshot::{action_kind_str, FailureRecord, MeshOsSnapshot, RECENT_FAILURES_CAPACITY};

/// Per-action chain record. Bounded shape — carries only what
/// observers need to reason about the action chain without
/// requiring `MeshOsAction` to be `Serialize`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActionChainRecord {
    /// Process-local action id from `MeshOsAction`'s execution
    /// path. Not stable across node restarts; observers correlate
    /// by `(node_id, action_id, emitted_at_ms)` if they need a
    /// globally-unique key.
    pub id: u64,
    /// Stable kind discriminator (`"start_daemon"`, `"pull_replica"`,
    /// …). Matches [`action_kind_str`]'s output so observers
    /// branch the same way.
    pub kind: String,
    /// Wall-clock milliseconds-since-Unix-epoch at emission.
    /// `u64` ms gives ~584 million years of headroom — fine.
    pub emitted_at_ms: u64,
    /// What happened to the action after admit.
    pub disposition: ActionDisposition,
}

/// Outcome the executor reports alongside each record.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[non_exhaustive]
pub enum ActionDisposition {
    /// Admitted and dispatched successfully.
    Dispatched,
    /// Admitted, dispatched, and the dispatcher returned an
    /// error. `reason` is the operator-readable explanation.
    /// `retry_after_ms` carries the dispatcher's retry hint if
    /// any.
    Failed {
        /// Operator-readable reason.
        reason: String,
        /// Retry hint in ms, or `None` for "no retry" /
        /// "drop after this failure."
        retry_after_ms: Option<u64>,
    },
    /// Hard-gated by admit (e.g. crash-loop). `reason` is the
    /// admit gate's static reason string.
    Gated {
        /// Static reason from admit (e.g. `"daemon-backoff"`).
        reason: String,
        /// When the gate releases (ms-from-emit). `None` when
        /// release is open-ended (Pose configurable cool-downs).
        cooldown_ms: Option<u64>,
    },
}

/// Build a record from a [`PendingAction`] + disposition.
/// Wall-clock time is taken at call-time from `SystemTime`
/// (the executor doesn't carry an explicit clock dep).
pub fn record_from(
    pending: &PendingAction,
    disposition: ActionDisposition,
) -> ActionChainRecord {
    let emitted_at_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    ActionChainRecord {
        id: pending.id.0,
        kind: action_kind_str(&pending.action).to_string(),
        emitted_at_ms,
        disposition,
    }
}

/// Trait the executor calls per admitted action. Production
/// impls write the record to a RedEX chain; tests + bootstrap
/// can use [`NoOpActionChainAppender`].
pub trait ActionChainAppender: Send + Sync + 'static {
    /// Append a record. Errors are non-fatal — the executor
    /// proceeds with the action regardless.
    fn append(&self, record: ActionChainRecord) -> Result<(), AppendError>;
}

/// Append failure surface — operator-readable reason; the
/// executor logs it and continues.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppendError {
    /// Reason the append failed.
    pub reason: String,
}

impl std::fmt::Display for AppendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "action-chain append failed: {}", self.reason)
    }
}
impl std::error::Error for AppendError {}

/// No-op appender. Useful for tests, bootstrap, and any
/// consumer that doesn't yet wire a RedEX chain. Returns
/// `Ok(())` for every record.
#[derive(Debug, Default)]
pub struct NoOpActionChainAppender;

impl ActionChainAppender for NoOpActionChainAppender {
    fn append(&self, _record: ActionChainRecord) -> Result<(), AppendError> {
        Ok(())
    }
}

/// Buffering appender — collects records in an internal `Vec`
/// for tests to inspect.
#[derive(Debug, Default)]
pub struct BufferingActionChainAppender {
    records: parking_lot::Mutex<Vec<ActionChainRecord>>,
}

impl BufferingActionChainAppender {
    /// Construct an empty buffer.
    pub fn new() -> Self {
        Self::default()
    }

    /// Snapshot the buffered records.
    pub fn records(&self) -> Vec<ActionChainRecord> {
        self.records.lock().clone()
    }

    /// Count of buffered records.
    pub fn len(&self) -> usize {
        self.records.lock().len()
    }

    /// `true` if no records have been appended.
    pub fn is_empty(&self) -> bool {
        self.records.lock().is_empty()
    }
}

impl ActionChainAppender for BufferingActionChainAppender {
    fn append(&self, record: ActionChainRecord) -> Result<(), AppendError> {
        self.records.lock().push(record);
        Ok(())
    }
}

/// `RedexFold<MeshOsSnapshot>` impl that maintains the snapshot
/// from a stream of [`ActionChainRecord`] events.
///
/// The fold's contract:
///
/// - `ActionDisposition::Dispatched` → no snapshot mutation.
///   The action succeeded; the per-tick `pending` rebuild is
///   the right surface for "what's in flight."
/// - `ActionDisposition::Failed { reason, .. }` → push a
///   `FailureRecord` onto `state.recent_failures`. Ring buffer
///   bounded by [`RECENT_FAILURES_CAPACITY`].
/// - `ActionDisposition::Gated { reason, .. }` → push a
///   `FailureRecord` (with a different source prefix to
///   distinguish from real failures).
///
/// Deck's view of "recent issues" thus reflects both true
/// failures (dispatcher returned an error) and gated actions
/// (admit said no).
#[derive(Debug, Default)]
pub struct MeshOsSnapshotFold;

impl RedexFold<MeshOsSnapshot> for MeshOsSnapshotFold {
    fn apply(
        &mut self,
        ev: &RedexEvent,
        state: &mut MeshOsSnapshot,
    ) -> Result<(), RedexError> {
        let record: ActionChainRecord = postcard::from_bytes(&ev.payload).map_err(|e| {
            RedexError::Decode(format!("ActionChainRecord postcard decode: {e}"))
        })?;
        match record.disposition {
            ActionDisposition::Dispatched => {
                // Successful dispatch isn't a failure — no
                // snapshot mutation.
            }
            ActionDisposition::Failed { reason, .. } => {
                push_failure(
                    state,
                    format!("action-id:{}:{}", record.id, record.kind),
                    reason,
                );
            }
            ActionDisposition::Gated { reason, cooldown_ms } => {
                let detail = match cooldown_ms {
                    Some(ms) => format!("gated ({reason}); cooldown {ms} ms"),
                    None => format!("gated ({reason})"),
                };
                push_failure(
                    state,
                    format!("action-id:{}:{}", record.id, record.kind),
                    detail,
                );
            }
        }
        Ok(())
    }
}

fn push_failure(state: &mut MeshOsSnapshot, source: String, reason: String) {
    if state.recent_failures.len() >= RECENT_FAILURES_CAPACITY {
        state.recent_failures.pop_front();
    }
    state.recent_failures.push_back(FailureRecord {
        source,
        reason,
        age_ms: 0,
    });
}

/// Convenience: build + append the record for a successful
/// dispatch. Production executors call this in their happy
/// path; the no-op appender makes it cheap when no chain is
/// wired.
pub fn append_dispatched(
    appender: &Arc<dyn ActionChainAppender>,
    pending: &PendingAction,
) -> Result<(), AppendError> {
    appender.append(record_from(pending, ActionDisposition::Dispatched))
}

/// Convenience for failure records.
pub fn append_failed(
    appender: &Arc<dyn ActionChainAppender>,
    pending: &PendingAction,
    reason: String,
    retry_after_ms: Option<u64>,
) -> Result<(), AppendError> {
    appender.append(record_from(
        pending,
        ActionDisposition::Failed {
            reason,
            retry_after_ms,
        },
    ))
}

/// Convenience for gated records.
pub fn append_gated(
    appender: &Arc<dyn ActionChainAppender>,
    pending: &PendingAction,
    reason: String,
    cooldown_ms: Option<u64>,
) -> Result<(), AppendError> {
    appender.append(record_from(
        pending,
        ActionDisposition::Gated { reason, cooldown_ms },
    ))
}

// Suppress unused `MeshOsAction` import warning when consumers
// only touch the appender side.
#[allow(dead_code)]
const _: Option<MeshOsAction> = None;

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;
    use super::super::action::{ActionId, MaintenanceTransition};

    fn record(id: u64, kind: &str, disposition: ActionDisposition) -> ActionChainRecord {
        ActionChainRecord {
            id,
            kind: kind.into(),
            emitted_at_ms: 1_000_000,
            disposition,
        }
    }

    fn redex_event(payload: Vec<u8>) -> RedexEvent {
        use crate::adapter::net::redex::RedexEntry;
        RedexEvent {
            entry: RedexEntry {
                seq: 1,
                payload_offset: 0,
                payload_len: payload.len() as u32,
                flags_and_checksum: 0,
            },
            payload: bytes::Bytes::from(payload),
        }
    }

    #[test]
    fn record_round_trips_through_postcard() {
        let r = record(
            42,
            "start_daemon",
            ActionDisposition::Failed {
                reason: "boom".into(),
                retry_after_ms: Some(500),
            },
        );
        let bytes = postcard::to_allocvec(&r).unwrap();
        let back: ActionChainRecord = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(back, r);
    }

    #[test]
    fn record_from_pending_action_uses_action_kind_str() {
        let pending = PendingAction {
            id: ActionId(7),
            action: MeshOsAction::CommitMaintenanceTransition {
                node: 0,
                target: MaintenanceTransition::Maintenance,
            },
            emitted_at: Instant::now(),
        };
        let rec = record_from(&pending, ActionDisposition::Dispatched);
        assert_eq!(rec.id, 7);
        assert_eq!(rec.kind, "commit_maintenance_transition");
        assert!(matches!(rec.disposition, ActionDisposition::Dispatched));
    }

    #[test]
    fn buffering_appender_collects_records() {
        let appender = BufferingActionChainAppender::new();
        appender
            .append(record(1, "start_daemon", ActionDisposition::Dispatched))
            .unwrap();
        appender
            .append(record(
                2,
                "stop_daemon",
                ActionDisposition::Failed {
                    reason: "boom".into(),
                    retry_after_ms: None,
                },
            ))
            .unwrap();
        assert_eq!(appender.len(), 2);
        assert_eq!(appender.records()[0].id, 1);
        assert_eq!(appender.records()[1].id, 2);
    }

    #[test]
    fn noop_appender_swallows_all_records() {
        let appender = NoOpActionChainAppender;
        let r = record(1, "start_daemon", ActionDisposition::Dispatched);
        appender.append(r).unwrap();
        // No state to assert; the contract is just "always Ok."
    }

    #[test]
    fn fold_dispatched_record_leaves_recent_failures_empty() {
        let mut fold = MeshOsSnapshotFold;
        let mut state = MeshOsSnapshot::default();
        let r = record(1, "start_daemon", ActionDisposition::Dispatched);
        let bytes = postcard::to_allocvec(&r).unwrap();
        fold.apply(&redex_event(bytes), &mut state).unwrap();
        assert!(state.recent_failures.is_empty());
    }

    #[test]
    fn fold_failed_record_pushes_failure_with_action_id_source() {
        let mut fold = MeshOsSnapshotFold;
        let mut state = MeshOsSnapshot::default();
        let r = record(
            42,
            "start_daemon",
            ActionDisposition::Failed {
                reason: "process died".into(),
                retry_after_ms: None,
            },
        );
        let bytes = postcard::to_allocvec(&r).unwrap();
        fold.apply(&redex_event(bytes), &mut state).unwrap();
        assert_eq!(state.recent_failures.len(), 1);
        assert_eq!(
            state.recent_failures[0].source,
            "action-id:42:start_daemon",
        );
        assert_eq!(state.recent_failures[0].reason, "process died");
    }

    #[test]
    fn fold_gated_record_pushes_failure_with_cooldown_detail() {
        let mut fold = MeshOsSnapshotFold;
        let mut state = MeshOsSnapshot::default();
        let r = record(
            7,
            "stop_daemon",
            ActionDisposition::Gated {
                reason: "daemon-backoff".into(),
                cooldown_ms: Some(5000),
            },
        );
        let bytes = postcard::to_allocvec(&r).unwrap();
        fold.apply(&redex_event(bytes), &mut state).unwrap();
        assert_eq!(state.recent_failures.len(), 1);
        assert!(
            state.recent_failures[0].reason.contains("daemon-backoff"),
            "got reason {}",
            state.recent_failures[0].reason
        );
        assert!(
            state.recent_failures[0].reason.contains("5000"),
            "cooldown ms not in reason: {}",
            state.recent_failures[0].reason
        );
    }

    #[test]
    fn fold_drops_oldest_failure_at_ring_capacity() {
        let mut fold = MeshOsSnapshotFold;
        let mut state = MeshOsSnapshot::default();
        for i in 0..(RECENT_FAILURES_CAPACITY + 5) {
            let r = record(
                i as u64,
                "start_daemon",
                ActionDisposition::Failed {
                    reason: format!("err {i}"),
                    retry_after_ms: None,
                },
            );
            let bytes = postcard::to_allocvec(&r).unwrap();
            fold.apply(&redex_event(bytes), &mut state).unwrap();
        }
        // The buffer holds exactly RECENT_FAILURES_CAPACITY most-
        // recent records.
        assert_eq!(state.recent_failures.len(), RECENT_FAILURES_CAPACITY);
        // Oldest five were dropped; first surviving entry's id
        // is 5 (action ids 5..N+5 made it in).
        assert!(
            state.recent_failures[0].source.contains(":5"),
            "expected oldest survivor id=5, got source {}",
            state.recent_failures[0].source
        );
    }

    #[tokio::test]
    async fn end_to_end_executor_buffer_fold_reproduces_failures_on_snapshot() {
        // Spin up a full executor with a BufferingActionChainAppender;
        // dispatch one Failed action; replay the buffered
        // records through MeshOsSnapshotFold and assert the
        // failure surfaces on the rebuilt MeshOsSnapshot.
        use std::sync::Arc;
        use tokio::sync::mpsc;

        use super::super::action::ActionId;
        use super::super::config::MeshOsConfig;
        use super::super::executor::{
            ActionExecutor, DispatchError, LoggingDispatcher,
        };

        let (tx, rx) = mpsc::channel(8);
        let dispatcher = Arc::new(LoggingDispatcher::new());
        dispatcher.fail_next(DispatchError::drop("test failure"));

        let appender = Arc::new(BufferingActionChainAppender::new());
        let exec = ActionExecutor::new(
            rx,
            Arc::new(MeshOsConfig::default()),
            Arc::clone(&dispatcher),
        )
        .with_chain_appender(Arc::clone(&appender) as Arc<dyn ActionChainAppender>);

        let pending = PendingAction {
            id: ActionId(99),
            action: MeshOsAction::CommitMaintenanceTransition {
                node: 0,
                target: MaintenanceTransition::Active,
            },
            emitted_at: Instant::now(),
        };
        tx.send(pending).await.unwrap();
        let task = tokio::spawn(exec.run());
        drop(tx);
        let _ = task.await.expect("executor join");

        // The buffer should now hold one Failed record.
        let records = appender.records();
        assert_eq!(records.len(), 1, "expected one chain record, got {records:?}");
        assert_eq!(records[0].id, 99);
        assert!(matches!(records[0].disposition, ActionDisposition::Failed { .. }));

        // Replay through the fold; the snapshot's
        // recent_failures should reflect the failure.
        let mut fold = MeshOsSnapshotFold;
        let mut state = MeshOsSnapshot::default();
        for record in records {
            let bytes = postcard::to_allocvec(&record).unwrap();
            fold.apply(&redex_event(bytes), &mut state).unwrap();
        }
        assert_eq!(state.recent_failures.len(), 1);
        assert_eq!(state.recent_failures[0].reason, "test failure");
    }

    #[test]
    fn fold_decode_error_surfaces_as_redex_error() {
        let mut fold = MeshOsSnapshotFold;
        let mut state = MeshOsSnapshot::default();
        // Garbage bytes — not a valid postcard ActionChainRecord.
        let ev = redex_event(vec![0xFF, 0xFF, 0xFF]);
        let err = fold.apply(&ev, &mut state).unwrap_err();
        match err {
            RedexError::Decode(msg) => {
                assert!(msg.contains("ActionChainRecord"));
            }
            other => panic!("expected Decode, got {other:?}"),
        }
    }
}
