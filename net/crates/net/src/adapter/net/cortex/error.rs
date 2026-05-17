//! Error type for CortEX adapter operations.

use thiserror::Error;

use super::super::redex::RedexError;

/// Errors produced by [`super::CortexAdapter`] operations.
#[derive(Debug, Error)]
pub enum CortexAdapterError {
    /// Underlying RedEX storage error.
    #[error("redex: {0}")]
    Redex(#[from] RedexError),

    /// The adapter has been closed.
    #[error("adapter closed")]
    Closed,

    /// The fold task has stopped (first fold error under
    /// [`super::FoldErrorPolicy::Stop`]). Holds the RedEX sequence
    /// at which the fold stopped.
    #[error("fold stopped at seq {seq}")]
    FoldStopped {
        /// The RedEX sequence of the event whose fold returned an error.
        seq: u64,
    },

    /// `wait_for_seq` was asked to wait past a seq the fold task
    /// will never reach: the task halted (close, Stop-policy halt,
    /// retention-evicted tail lag) with the folded watermark in
    /// `folded_through`. Pre-fix this manifested as
    /// `wait_for_seq` silently returning `()` and the caller
    /// reading state that did not reflect the requested seq.
    #[error("fold stopped before seq {wanted}; folded through {folded_through:?}")]
    FoldStoppedBeforeSeq {
        /// The seq the caller was waiting for.
        wanted: u64,
        /// The highest seq the fold task processed before
        /// stopping; `None` if it stopped without processing
        /// anything.
        folded_through: Option<u64>,
    },

    /// `open` was called with a `StartPosition` that requires
    /// externally-rehydrated state — `FromSeq(n)` for `n > 0` or
    /// `LiveOnly`. Callers using these positions must construct
    /// the adapter via [`super::CortexAdapter::open_from_snapshot`]
    /// instead so the watermark and `state` are properly anchored
    /// to the prior events the adapter is going to skip.
    ///
    /// Accepting these positions in `open` would set
    /// `initial_watermark = start_seq - 1`, making
    /// `wait_for_seq(k)` for `k <= start_seq-1` return immediately
    /// — the adapter would claim those seqs were "applied" while
    /// `state` had never seen them.
    #[error("StartPosition::{0} requires open_from_snapshot, not open")]
    InvalidStartPosition(&'static str),
}
