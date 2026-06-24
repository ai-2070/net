//! `WorkflowFold` — decodes `EventMeta` + payload, routes on dispatch,
//! and applies the deterministic task-lifecycle state transition.
//!
//! The chain is single-writer (the task-lease holder), so the fold
//! does not arbitrate concurrent transitions — it simply replays the
//! writer's cursor advances. Same chain → same state.

use super::super::super::redex::{RedexError, RedexEvent, RedexFold};
use super::super::meta::{
    compute_checksum, compute_checksum_with_meta, EventMeta, EVENT_META_SIZE,
};
use super::dispatch::{
    DISPATCH_TASK_ADVANCED, DISPATCH_TASK_DELETED, DISPATCH_TASK_RETRIED, DISPATCH_TASK_SUBMITTED,
    DISPATCH_TASK_TRANSITIONED,
};
use super::state::WorkflowState;
use super::types::{
    AdvancedPayload, DeletedPayload, RetriedPayload, SubmittedPayload, TaskState, TaskStatus,
    TransitionedPayload,
};

/// Fold implementation for the task-lifecycle model.
pub struct WorkflowFold;

impl RedexFold<WorkflowState> for WorkflowFold {
    fn apply(&mut self, ev: &RedexEvent, state: &mut WorkflowState) -> Result<(), RedexError> {
        // Decode failures use `RedexError::Decode` (recoverable —
        // skip-and-continue even under the `Stop` policy) so one
        // corrupt event can't wedge the fold task forever; same
        // rationale as `TasksFold`.
        if ev.payload.len() < EVENT_META_SIZE {
            return Err(RedexError::Decode(format!(
                "workflow payload too short: {} bytes (need >= {})",
                ev.payload.len(),
                EVENT_META_SIZE
            )));
        }
        let meta = EventMeta::from_bytes(&ev.payload[..EVENT_META_SIZE])
            .ok_or_else(|| RedexError::Decode("bad EventMeta prefix".into()))?;
        let tail = &ev.payload[EVENT_META_SIZE..];

        // Verify the ingest-time checksum over (header-with-zeroed-
        // checksum ++ tail); fall back to the legacy tail-only hash
        // for records written by pre-fix adapters.
        let v2_expected = compute_checksum_with_meta(&meta, tail);
        let valid = meta.checksum == v2_expected || meta.checksum == compute_checksum(tail);
        if !valid {
            return Err(RedexError::Decode(format!(
                "workflow fold: EventMeta checksum mismatch at seq {} (got {:#010x}, v2 expected {:#010x})",
                ev.entry.seq, meta.checksum, v2_expected
            )));
        }

        match meta.dispatch {
            DISPATCH_TASK_SUBMITTED => {
                let p: SubmittedPayload =
                    postcard::from_bytes(tail).map_err(|e| RedexError::Decode(e.to_string()))?;
                // Submit is the baseline; a re-submit of a live id
                // resets it to the fresh state (the log is the source
                // of truth).
                state.tasks.insert(p.id, TaskState::submitted());
            }
            DISPATCH_TASK_TRANSITIONED => {
                let p: TransitionedPayload =
                    postcard::from_bytes(tail).map_err(|e| RedexError::Decode(e.to_string()))?;
                if let Some(t) = state.tasks.get_mut(&p.id) {
                    t.status = p.status;
                }
                // A transition for an unknown id is a no-op: the submit
                // we never observed simply isn't in our view.
            }
            DISPATCH_TASK_ADVANCED => {
                let p: AdvancedPayload =
                    postcard::from_bytes(tail).map_err(|e| RedexError::Decode(e.to_string()))?;
                if let Some(t) = state.tasks.get_mut(&p.id) {
                    t.step = t.step.saturating_add(1);
                    // A new step starts with a clean attempt counter.
                    t.attempts = 0;
                }
            }
            DISPATCH_TASK_RETRIED => {
                let p: RetriedPayload =
                    postcard::from_bytes(tail).map_err(|e| RedexError::Decode(e.to_string()))?;
                if let Some(t) = state.tasks.get_mut(&p.id) {
                    t.attempts = t.attempts.saturating_add(1);
                    t.status = TaskStatus::Running;
                }
            }
            DISPATCH_TASK_DELETED => {
                let p: DeletedPayload =
                    postcard::from_bytes(tail).map_err(|e| RedexError::Decode(e.to_string()))?;
                state.tasks.remove(&p.id);
            }
            other => {
                // Unknown dispatches in the CortEX-internal range are
                // forward-compatibility — log and skip.
                tracing::debug!(
                    dispatch = other,
                    seq = ev.entry.seq,
                    "workflow fold: ignoring unknown dispatch"
                );
            }
        }
        Ok(())
    }
}
