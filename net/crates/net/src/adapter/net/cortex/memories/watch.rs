//! Reactive watcher over `MemoriesState`.
//!
//! Fluent builder mirroring [`super::query::MemoriesQuery`] that
//! produces a `Stream<Item = Vec<Memory>>`. Yields the current filter
//! result on open, then yields again whenever a fold tick produces a
//! different filter result (deduplicated by `Vec<Memory>` equality).
//!
//! ```ignore
//! let mut stream = Box::pin(
//!     memories.watch()
//!         .where_tag("urgent")
//!         .order_by(OrderBy::CreatedDesc)
//!         .stream()
//! );
//!
//! while let Some(current) = stream.next().await {
//!     // current: freshly-evaluated tagged-urgent list.
//! }
//! ```

use std::sync::Arc;

use futures::stream::BoxStream;
use futures::{Stream, StreamExt};
use parking_lot::RwLock;
use tokio::sync::watch;
use tokio_stream::wrappers::WatchStream;

use super::query::{ContentNeedle, MemoriesFilterSpec, OrderBy};
use super::state::MemoriesState;
use super::types::{Memory, MemoryId};

/// Reactive filter over `MemoriesState`. Created via
/// [`super::MemoriesAdapter::watch`].
pub struct MemoriesWatcher {
    state: Arc<RwLock<MemoriesState>>,
    changes: BoxStream<'static, u64>,
    spec: MemoriesFilterSpec,
}

impl MemoriesWatcher {
    /// Build a watcher from the adapter's state handle + change stream.
    /// Intended to be called only by [`super::MemoriesAdapter::watch`].
    pub(super) fn new(state: Arc<RwLock<MemoriesState>>, changes: BoxStream<'static, u64>) -> Self {
        Self {
            state,
            changes,
            spec: MemoriesFilterSpec::default(),
        }
    }

    /// Restrict to memories whose id is in the provided collection.
    pub fn where_id_in(mut self, ids: impl IntoIterator<Item = MemoryId>) -> Self {
        self.spec.id_in = Some(ids.into_iter().collect());
        self
    }

    /// Restrict to memories from this source.
    pub fn where_source(mut self, source: impl Into<String>) -> Self {
        self.spec.source = Some(source.into());
        self
    }

    /// Restrict to memories whose content contains `needle`
    /// (case-insensitive).
    pub fn content_contains(mut self, needle: impl Into<String>) -> Self {
        self.spec.content_contains = Some(ContentNeedle::new(needle));
        self
    }

    /// Restrict to memories tagged with `tag`.
    pub fn where_tag(mut self, tag: impl Into<String>) -> Self {
        self.spec.require_tag = Some(tag.into());
        self
    }

    /// Restrict to memories that have AT LEAST ONE of the given tags.
    pub fn where_any_tag(mut self, tags: impl IntoIterator<Item = String>) -> Self {
        self.spec.require_any_tag = Some(tags.into_iter().collect());
        self
    }

    /// Restrict to memories that have EVERY tag in the given set.
    pub fn where_all_tags(mut self, tags: impl IntoIterator<Item = String>) -> Self {
        self.spec.require_all_tags = Some(tags.into_iter().collect());
        self
    }

    /// Restrict to pinned (`true`) or unpinned (`false`) only.
    pub fn where_pinned(mut self, pinned: bool) -> Self {
        self.spec.only_pinned = Some(pinned);
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
    /// [`super::MemoriesAdapter::snapshot_and_watch`] that need to
    /// execute the filter **once** against the current state before
    /// handing the watcher off to stream subsequent changes.
    pub(super) fn spec_for_snapshot(&self) -> MemoriesFilterSpec {
        let mut spec = self.spec.clone();
        if spec.order_by.is_none() {
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
    pub fn stream(self) -> impl Stream<Item = Vec<std::sync::Arc<Memory>>> + Send + 'static {
        let MemoriesWatcher {
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
                        // PERF_AUDIT §5.6 — opportunistic batching.
                        // Pre-fix every fold-apply tick triggered a
                        // full `spec.execute` re-scan of the
                        // memories HashMap. During a catch-up
                        // replay or any bursty publisher, the same
                        // filtered result was recomputed once per
                        // event when downstream callers only ever
                        // see the final state (the watch channel
                        // conflates on the consumer side).
                        //
                        // Drain any change events that have ALREADY
                        // arrived in the stream's buffer before
                        // re-querying. `now_or_never` is a non-
                        // blocking probe — does not delay queries
                        // for spread-out changes; only coalesces
                        // bursts where multiple ticks are already
                        // queued.
                        //
                        // End-of-stream during the drain must NOT
                        // short-circuit past the query below: we
                        // already received at least one tick this
                        // iteration, so the final state still has
                        // to be computed and delivered (eventual
                        // consistency of the last value). Returning
                        // here would drop the burst's final
                        // emission whenever the producer's last
                        // append races the adapter teardown.
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

    /// PERF_AUDIT §5.6 regression — the opportunistic burst drain
    /// must NOT short-circuit past the re-query when it observes
    /// end-of-stream. A producer that appends a burst and is then
    /// torn down (changes stream ends) still owes the consumer the
    /// burst's final state: pre-fix the drain's `None => return`
    /// dropped the already-received tick(s) on the floor and the
    /// consumer never saw the last value.
    ///
    /// Deterministic under the current-thread test runtime: the
    /// spawned watcher task cannot run until our first `.await`, so
    /// the state mutation below lands before the watcher drains the
    /// (immediately-ready, immediately-ending) 3-tick stream.
    #[tokio::test]
    async fn burst_drain_hitting_end_of_stream_still_delivers_final_state() {
        let state = Arc::new(RwLock::new(MemoriesState::new()));
        // A 3-tick burst followed immediately by end-of-stream —
        // models "append burst, then adapter teardown".
        let changes = StreamExt::boxed(futures::stream::iter(vec![0u64, 1, 2]));
        let watcher = MemoriesWatcher::new(state.clone(), changes);
        let mut out = Box::pin(watcher.stream());

        // Mutate state AFTER `stream()` computed the initial
        // (empty) snapshot but BEFORE the watcher task first polls.
        state.write().memories.insert(
            7,
            Arc::new(Memory {
                id: 7,
                content: "final value".into(),
                tags: vec!["t".into()],
                source: "test".into(),
                created_ns: 1,
                updated_ns: 1,
                pinned: false,
            }),
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
