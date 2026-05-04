//! `TasksFold` — decodes `EventMeta` + payload, routes on dispatch,
//! mutates [`super::state::TasksState`].

use super::super::super::redex::{RedexError, RedexEvent, RedexFold};
use super::super::meta::{
    compute_checksum, compute_checksum_with_meta, EventMeta, EVENT_META_SIZE,
};
use super::dispatch::{
    DISPATCH_TASK_COMPLETED, DISPATCH_TASK_CREATED, DISPATCH_TASK_DELETED, DISPATCH_TASK_RENAMED,
};
use super::state::TasksState;
use super::types::{
    Task, TaskCompletedPayload, TaskCreatedPayload, TaskDeletedPayload, TaskRenamedPayload,
    TaskStatus,
};

/// Fold implementation for the tasks model.
pub struct TasksFold;

impl RedexFold<TasksState> for TasksFold {
    fn apply(&mut self, ev: &RedexEvent, state: &mut TasksState) -> Result<(), RedexError> {
        // Per-event decode failures use `RedexError::Decode` (a
        // recoverable variant the fold-error-policy interpreter
        // treats as skip-and-continue even under `Stop`). Returning
        // `Encode` would halt the fold task under `Stop` — a single
        // corrupt event could wedge the fold task forever, DoSing a
        // multi-tenant cortex via one bad event past the 32-bit
        // checksum. User-level fold errors and storage-side encode
        // failures still use `Encode` and properly halt under
        // `Stop`.
        if ev.payload.len() < EVENT_META_SIZE {
            return Err(RedexError::Decode(format!(
                "tasks payload too short: {} bytes (need >= {})",
                ev.payload.len(),
                EVENT_META_SIZE
            )));
        }
        let meta = EventMeta::from_bytes(&ev.payload[..EVENT_META_SIZE])
            .ok_or_else(|| RedexError::Decode("bad EventMeta prefix".into()))?;
        let tail = &ev.payload[EVENT_META_SIZE..];

        // Verify the corruption-detection checksum stamped at
        // ingest against the bytes we received from RedEX.
        //
        // v2 covers (header-with-zeroed-checksum-slot || tail),
        // so a bit-flip in the dispatch byte (or any other
        // header field) is caught — the legacy tail-only hash
        // left those bytes unprotected and a `STORED → DELETED`
        // flip silently re-routed the event to the wrong fold
        // arm. Fall back to v1 (tail-only) for records
        // written by pre-fix adapters; legacy records keep their
        // original limitation, new writes get full coverage.
        let v2_expected = compute_checksum_with_meta(&meta, tail);
        let valid = if meta.checksum == v2_expected {
            true
        } else {
            meta.checksum == compute_checksum(tail)
        };
        if !valid {
            return Err(RedexError::Decode(format!(
                "tasks fold: EventMeta checksum mismatch at seq {} (got {:#010x}, v2 expected {:#010x})",
                ev.entry.seq, meta.checksum, v2_expected
            )));
        }

        match meta.dispatch {
            DISPATCH_TASK_CREATED => {
                let p: TaskCreatedPayload =
                    postcard::from_bytes(tail).map_err(|e| RedexError::Decode(e.to_string()))?;
                state.tasks.insert(
                    p.id,
                    Task {
                        id: p.id,
                        title: p.title,
                        status: TaskStatus::Pending,
                        created_ns: p.now_ns,
                        updated_ns: p.now_ns,
                    },
                );
            }
            DISPATCH_TASK_RENAMED => {
                let p: TaskRenamedPayload =
                    postcard::from_bytes(tail).map_err(|e| RedexError::Decode(e.to_string()))?;
                if let Some(t) = state.tasks.get_mut(&p.id) {
                    t.title = p.new_title;
                    t.updated_ns = p.now_ns;
                }
                // Rename on an unknown id is a no-op; the log is the
                // source of truth and a missing create simply means
                // the rename refers to state we never observed.
            }
            DISPATCH_TASK_COMPLETED => {
                let p: TaskCompletedPayload =
                    postcard::from_bytes(tail).map_err(|e| RedexError::Decode(e.to_string()))?;
                if let Some(t) = state.tasks.get_mut(&p.id) {
                    t.status = TaskStatus::Completed;
                    t.updated_ns = p.now_ns;
                }
            }
            DISPATCH_TASK_DELETED => {
                let p: TaskDeletedPayload =
                    postcard::from_bytes(tail).map_err(|e| RedexError::Decode(e.to_string()))?;
                state.tasks.remove(&p.id);
            }
            other => {
                // Unknown dispatches in the CortEX-internal range are
                // treated as forward-compatibility — log and skip.
                tracing::debug!(
                    dispatch = other,
                    seq = ev.entry.seq,
                    "tasks fold: ignoring unknown dispatch"
                );
            }
        }
        Ok(())
    }
}
