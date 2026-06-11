//! Reactive watcher over `TasksState`.
//!
//! Fluent builder mirroring [`super::query::TasksQuery`] that produces
//! a `Stream<Item = Vec<Task>>`. The stream yields the current filter
//! result on open, then yields again whenever a fold tick produces a
//! different filter result (deduplicated by `Vec<Task>` equality).
//!
//! ```ignore
//! let mut pending_stream = Box::pin(
//!     tasks.watch()
//!         .where_status(TaskStatus::Pending)
//!         .order_by(OrderBy::CreatedDesc)
//!         .stream()
//! );
//!
//! while let Some(current) = pending_stream.next().await {
//!     // `current` is the freshly-evaluated pending list.
//! }
//! ```

use std::sync::Arc;

use futures::stream::BoxStream;
use futures::{Stream, StreamExt};
use parking_lot::RwLock;
use tokio::sync::watch;
use tokio_stream::wrappers::WatchStream;

use super::query::{OrderBy, TasksFilterSpec, TitleNeedle};
use super::state::TasksState;
use super::types::{Task, TaskId, TaskStatus};

/// Reactive filter over `TasksState`. Created via
/// [`super::TasksAdapter::watch`].
pub struct TasksWatcher {
    state: Arc<RwLock<TasksState>>,
    changes: BoxStream<'static, u64>,
    spec: TasksFilterSpec,
}

impl TasksWatcher {
    /// Build a watcher from the adapter's state handle + change stream.
    /// Intended to be called only by [`super::TasksAdapter::watch`].
    pub(super) fn new(state: Arc<RwLock<TasksState>>, changes: BoxStream<'static, u64>) -> Self {
        Self {
            state,
            changes,
            spec: TasksFilterSpec::default(),
        }
    }

    /// Restrict to tasks with the given status.
    pub fn where_status(mut self, status: TaskStatus) -> Self {
        self.spec.status = Some(status);
        self
    }

    /// Restrict to tasks whose id is in the provided collection.
    pub fn where_id_in(mut self, ids: impl IntoIterator<Item = TaskId>) -> Self {
        self.spec.id_in = Some(ids.into_iter().collect());
        self
    }

    /// Restrict to `created_ns >= ns` (inclusive).
    pub fn created_after(mut self, ns: u64) -> Self {
        self.spec.created_after_ns = Some(ns);
        self
    }

    /// Restrict to `created_ns <= ns` (inclusive).
    pub fn created_before(mut self, ns: u64) -> Self {
        self.spec.created_before_ns = Some(ns);
        self
    }

    /// Restrict to `updated_ns >= ns` (inclusive).
    pub fn updated_after(mut self, ns: u64) -> Self {
        self.spec.updated_after_ns = Some(ns);
        self
    }

    /// Restrict to `updated_ns <= ns` (inclusive).
    pub fn updated_before(mut self, ns: u64) -> Self {
        self.spec.updated_before_ns = Some(ns);
        self
    }

    /// Restrict to tasks whose title contains `needle` (case-insensitive).
    pub fn title_contains(mut self, needle: impl Into<String>) -> Self {
        self.spec.title_contains = Some(TitleNeedle::new(needle));
        self
    }

    /// Order each emitted result set.
    pub fn order_by(mut self, order: OrderBy) -> Self {
        self.spec.order_by = Some(order);
        self
    }

    /// Truncate each emitted result set to `n` after ordering.
    pub fn limit(mut self, n: usize) -> Self {
        self.spec.limit = Some(n);
        self
    }

    /// Expose the filter spec for one-shot callers like
    /// [`super::TasksAdapter::snapshot_and_watch`] that need to
    /// execute the filter **once** against the current state before
    /// handing the watcher off to stream subsequent changes.
    pub(super) fn spec_for_snapshot(&self) -> TasksFilterSpec {
        let mut spec = self.spec.clone();
        if spec.order_by.is_none() {
            // Mirror the default that `stream()` applies so the
            // snapshot's ordering matches the stream's emissions.
            spec.order_by = Some(OrderBy::IdAsc);
        }
        spec
    }

    /// Start emitting. The stream yields:
    ///
    /// - The current filter result immediately (first element).
    /// - A new result vector on each subsequent fold tick where the
    ///   filter's result differs from the previously emitted one.
    ///
    /// Backing channel is single-slot: if the consumer falls behind
    /// a fast fold task, intermediate filter results are dropped and
    /// the consumer sees the latest state on the next poll. Same
    /// "drop intermediate, final state is correct" semantic as
    /// [`crate::adapter::net::cortex::CortexAdapter::changes`].
    ///
    /// If `order_by` was not set, the watcher defaults to `IdAsc`
    /// so Vec-equality dedup is deterministic — otherwise HashMap
    /// iteration order could produce spurious re-emissions.
    ///
    /// The stream ends when the adapter's change stream ends (e.g.
    /// when all adapter handles drop and the fold task exits).
    pub fn stream(self) -> impl Stream<Item = Vec<Task>> + Send + 'static {
        let TasksWatcher {
            state,
            mut changes,
            mut spec,
        } = self;
        if spec.order_by.is_none() {
            spec.order_by = Some(OrderBy::IdAsc);
        }

        let initial = {
            let guard = state.read();
            spec.execute(&guard)
        };
        let (tx, rx) = watch::channel(initial.clone());

        tokio::spawn(async move {
            let mut last = initial;
            loop {
                tokio::select! {
                    // Consumer dropped the stream: stop folding
                    // immediately, don't wait for the next change
                    // tick (which may never arrive on an idle log).
                    _ = tx.closed() => return,
                    maybe_seq = changes.next() => {
                        let Some(_seq) = maybe_seq else { return };
                        // PERF_AUDIT §5.6 — opportunistic batching;
                        // see the matching comment in
                        // `memories/watch.rs` for the full
                        // rationale. End-of-stream observed during
                        // the drain must still deliver the final
                        // state below (we already received a tick
                        // this iteration) — only THEN exit.
                        use futures::future::FutureExt;
                        let mut stream_ended = false;
                        while let Some(maybe_more) = changes.next().now_or_never() {
                            if maybe_more.is_none() {
                                stream_ended = true;
                                break;
                            }
                        }
                        let current = {
                            let guard = state.read();
                            spec.execute(&guard)
                        };
                        if current != last {
                            if tx.send(current.clone()).is_err() {
                                return;
                            }
                            last = current;
                        }
                        if stream_ended {
                            return;
                        }
                    }
                }
            }
        });

        WatchStream::new(rx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// PERF_AUDIT §5.6 regression — mirror of
    /// `memories::watch::tests::burst_drain_hitting_end_of_stream_still_delivers_final_state`;
    /// see that test for the full rationale. The tasks watcher's
    /// burst drain must deliver the final state before exiting on
    /// end-of-stream.
    #[tokio::test]
    async fn burst_drain_hitting_end_of_stream_still_delivers_final_state() {
        let state = Arc::new(RwLock::new(TasksState::new()));
        let changes = StreamExt::boxed(futures::stream::iter(vec![0u64, 1, 2]));
        let watcher = TasksWatcher::new(state.clone(), changes);
        let mut out = Box::pin(watcher.stream());

        // Mutate state AFTER `stream()` computed the initial
        // (empty) snapshot but BEFORE the watcher task first polls.
        state.write().tasks.insert(
            7,
            Task {
                id: 7,
                title: "final value".into(),
                status: TaskStatus::Pending,
                created_ns: 1,
                updated_ns: 1,
            },
        );

        let initial = out.next().await.unwrap();
        assert!(initial.is_empty(), "initial snapshot precedes the insert");

        let last = out
            .next()
            .await
            .expect("final state must be delivered before the stream ends");
        assert_eq!(last.len(), 1);
        assert_eq!(last[0].id, 7);

        assert!(
            out.next().await.is_none(),
            "stream ends after the final delivery"
        );
    }
}
