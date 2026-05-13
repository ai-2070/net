//! The action executor — drains the
//! [`super::action::PendingAction`] queue the
//! [`super::event_loop::MeshOsLoop`] fills, runs each action
//! through the Phase G [`super::backpressure::BackpressureState::admit`]
//! gate, and dispatches to a pluggable
//! [`ActionDispatcher`].
//!
//! Locked decision #4 (action emission ≠ action execution): the
//! executor is a separate task, not inlined in reconcile.
//! Locked decision #10 (single backpressure layer): every
//! action passes through one admit; deferrals re-enter via a
//! per-executor `BinaryHeap` keyed by retry deadline; gates
//! drop with a structured failure record.
//!
//! Phase-A through G shipped the upstream side (event loop +
//! state + reconcile + gate); this module is the downstream
//! consumer. The dispatcher itself is pluggable — a
//! [`LoggingDispatcher`] ships for bootstrap / tests, and the
//! production path will wrap [`super::action::MeshOsAction`]
//! variants over the existing `DaemonRegistry`, migration
//! orchestrator, and `MeshNode::send_subprotocol` paths.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::future::BoxFuture;
use futures::FutureExt;
use parking_lot::Mutex;
use tokio::sync::mpsc;
use tokio::time::sleep_until;

use super::action::{MeshOsAction, PendingAction};
use super::backpressure::{
    AdmissionResult, BackpressureState, ClusterBackpressureChange,
};
use super::chain::{
    append_dispatched, append_failed, append_gated, ActionChainAppender,
    NoOpActionChainAppender,
};
use super::config::MeshOsConfig;
use super::snapshot::{FailureRecord, RECENT_FAILURES_CAPACITY};

/// Pluggable action sink. The executor calls `dispatch` once
/// per admitted action; the impl owns the substrate-side
/// wiring (daemon registry, migration orchestrator, MeshDB
/// admin commits, etc.).
///
/// Returns a [`BoxFuture`] so the trait stays dyn-compatible;
/// production dispatchers spawn substrate-side futures
/// themselves rather than blocking the executor task.
pub trait ActionDispatcher: Send + Sync + 'static {
    /// Dispatch an admitted action. Errors record on the
    /// recent-failures ring buffer; the action is not retried
    /// (admit / defer is the retry surface).
    fn dispatch<'a>(
        &'a self,
        action: MeshOsAction,
    ) -> BoxFuture<'a, Result<(), DispatchError>>;

    /// Cluster-wide backpressure flag transitioned. The executor
    /// invokes this once per edge crossing — `Asserted` when the
    /// action-queue depth crosses the high-water mark, `Released`
    /// when it drops below the low-water mark. Production
    /// dispatchers fan `DaemonControl::BackpressureOn { level }` /
    /// `DaemonControl::BackpressureOff` out to supervised daemons
    /// so they can shed optional work. Default impl is a no-op —
    /// dispatchers that don't supervise daemons (e.g. the test
    /// logger) can ignore the hook.
    fn on_cluster_backpressure(&self, _change: ClusterBackpressureChange) {}
}

/// Dispatch error surface. Carries the operator-readable reason
/// and an optional retry hint — the executor honors the hint
/// by re-queuing the action through `admit()` after the hint
/// elapses (if any).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DispatchError {
    /// Operator-readable reason.
    pub reason: String,
    /// Optional retry hint — if `Some`, the executor re-enters
    /// the action through `admit()` after this duration. `None`
    /// drops the action (the typical case — admit is the retry
    /// surface).
    pub retry_after: Option<Duration>,
}

impl DispatchError {
    /// Construct a non-retried error.
    pub fn drop(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            retry_after: None,
        }
    }

    /// Construct a retried error.
    pub fn retry(reason: impl Into<String>, after: Duration) -> Self {
        Self {
            reason: reason.into(),
            retry_after: Some(after),
        }
    }
}

/// Logging-only dispatcher. Records every admitted action in an
/// internal `Mutex<Vec<MeshOsAction>>` and returns `Ok(())`.
/// Useful for bootstrap (before real subsystem wiring lands) +
/// the executor's unit tests.
#[derive(Debug, Default)]
pub struct LoggingDispatcher {
    log: Mutex<Vec<MeshOsAction>>,
    fail_next: Mutex<Option<DispatchError>>,
    backpressure_log: Mutex<Vec<ClusterBackpressureChange>>,
}

impl LoggingDispatcher {
    /// Construct an empty logger.
    pub fn new() -> Self {
        Self::default()
    }

    /// Snapshot of the actions dispatched so far.
    pub fn log(&self) -> Vec<MeshOsAction> {
        self.log.lock().clone()
    }

    /// Inject an error to surface on the next `dispatch` call.
    /// Used by tests to exercise the retry / drop paths.
    pub fn fail_next(&self, err: DispatchError) {
        *self.fail_next.lock() = Some(err);
    }

    /// Snapshot of the cluster-backpressure transitions the
    /// executor has surfaced through `on_cluster_backpressure`.
    pub fn backpressure_log(&self) -> Vec<ClusterBackpressureChange> {
        self.backpressure_log.lock().clone()
    }
}

impl ActionDispatcher for LoggingDispatcher {
    fn dispatch<'a>(
        &'a self,
        action: MeshOsAction,
    ) -> BoxFuture<'a, Result<(), DispatchError>> {
        Box::pin(async move {
            if let Some(err) = self.fail_next.lock().take() {
                return Err(err);
            }
            self.log.lock().push(action);
            Ok(())
        })
    }

    fn on_cluster_backpressure(&self, change: ClusterBackpressureChange) {
        self.backpressure_log.lock().push(change);
    }
}

/// Counters the executor maintains for diagnostics / Deck
/// rendering. Returned by [`ActionExecutor::run`] when the
/// task exits; sampled live via [`ExecutorHandle::stats`].
#[derive(Debug, Default)]
pub struct ExecutorStats {
    /// Total actions admitted + successfully dispatched.
    pub dispatched: AtomicU64,
    /// Total actions admitted but failed in dispatch (no retry).
    pub failed: AtomicU64,
    /// Total actions deferred via `AdmissionResult::Defer`.
    /// Re-admits count here each time, so a flapping action
    /// inflates the metric — the queue-depth gauge is the
    /// healthy signal, not this counter.
    pub deferred: AtomicU64,
    /// Total actions hard-gated via `AdmissionResult::Gate`.
    pub gated: AtomicU64,
    /// Total actions retried via a dispatch error's
    /// `retry_after` hint.
    pub dispatch_retries: AtomicU64,
    /// Number of cluster-backpressure assert transitions
    /// surfaced to the dispatcher.
    pub cluster_backpressure_asserts: AtomicU64,
    /// Number of cluster-backpressure release transitions
    /// surfaced to the dispatcher.
    pub cluster_backpressure_releases: AtomicU64,
}

impl ExecutorStats {
    fn inc(counter: &AtomicU64) {
        counter.fetch_add(1, Ordering::Relaxed);
    }
}

/// Internal heap entry — `Reverse<Instant>` so the smallest
/// retry deadline is popped first.
struct DeferredEntry {
    retry_at: Instant,
    action: PendingAction,
    /// Number of times this action has been deferred. Capped by
    /// `BackpressureConfig::max_defer_count`; past the cap the
    /// executor drops the action with a structured failure
    /// record rather than keep it on the heap forever.
    defer_count: u32,
}

impl PartialEq for DeferredEntry {
    fn eq(&self, other: &Self) -> bool {
        self.retry_at == other.retry_at
    }
}
impl Eq for DeferredEntry {}
impl PartialOrd for DeferredEntry {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for DeferredEntry {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Min-heap by retry_at: smallest first.
        Reverse(self.retry_at).cmp(&Reverse(other.retry_at))
    }
}

/// The executor task body. Owns:
///
/// - the receiver side of the loop's action queue,
/// - a [`BackpressureState`] (Phase G),
/// - the dispatcher,
/// - the deferred-retry heap.
///
/// Construct via [`ActionExecutor::new`]; drive via
/// [`ActionExecutor::run`].
pub struct ActionExecutor<D: ActionDispatcher> {
    actions_rx: mpsc::Receiver<PendingAction>,
    config: Arc<MeshOsConfig>,
    backpressure: BackpressureState,
    dispatcher: Arc<D>,
    deferred: BinaryHeap<DeferredEntry>,
    recent_failures: VecDeque<FailureRecord>,
    stats: Arc<ExecutorStats>,
    /// Optional action-chain appender. Each admit/dispatch
    /// outcome appends an [`super::chain::ActionChainRecord`].
    /// Defaults to [`NoOpActionChainAppender`] — a real
    /// appender wires only when a chain consumer is set up.
    chain_appender: Arc<dyn ActionChainAppender>,
}

impl<D: ActionDispatcher> ActionExecutor<D> {
    /// Build an executor. `actions_rx` is the loop's queue
    /// (returned by [`super::event_loop::MeshOsLoop::new`]).
    pub fn new(
        actions_rx: mpsc::Receiver<PendingAction>,
        config: Arc<MeshOsConfig>,
        dispatcher: Arc<D>,
    ) -> Self {
        Self {
            actions_rx,
            config,
            backpressure: BackpressureState::new(),
            dispatcher,
            deferred: BinaryHeap::new(),
            recent_failures: VecDeque::with_capacity(RECENT_FAILURES_CAPACITY),
            stats: Arc::new(ExecutorStats::default()),
            chain_appender: Arc::new(NoOpActionChainAppender),
        }
    }

    /// Builder: install an action-chain appender. The default
    /// `NoOpActionChainAppender` swallows every record; a real
    /// appender (e.g. one writing to a RedEX chain consumed by
    /// `MeshOsSnapshotFold`) takes over per-action recording.
    pub fn with_chain_appender(mut self, appender: Arc<dyn ActionChainAppender>) -> Self {
        self.chain_appender = appender;
        self
    }

    /// Handle on the executor's live state — `stats` + the
    /// recent-failures snapshot. Cheap to clone (Arc /
    /// fixed-size copies). Useful for Phase F snapshot
    /// building from outside the task.
    pub fn handle(&self) -> ExecutorHandle {
        ExecutorHandle {
            stats: Arc::clone(&self.stats),
        }
    }

    /// Clone the stats `Arc`. Useful for the [`super::runtime::MeshOsRuntime`]
    /// stitching layer, which holds the Arc across `run()`'s
    /// consumption of `self`.
    pub fn stats_arc(&self) -> Arc<ExecutorStats> {
        Arc::clone(&self.stats)
    }

    /// Drive the executor until either the action receiver
    /// closes (the loop dropped its sender) or the inner
    /// dispatcher panics. Returns the accumulated stats.
    pub async fn run(mut self) -> Arc<ExecutorStats> {
        loop {
            let next_deadline = self.deferred.peek().map(|e| e.retry_at);
            tokio::select! {
                action = self.actions_rx.recv() => {
                    let Some(action) = action else { break };
                    self.handle_one(action).await;
                }
                _ = sleep_until_opt(next_deadline), if next_deadline.is_some() => {
                    // SAFETY: peek above returned Some.
                    let due = self.deferred.pop().expect("deferred heap non-empty");
                    self.handle_one_retry(due.action, due.defer_count).await;
                }
            }
        }
        Arc::clone(&self.stats)
    }

    async fn handle_one(&mut self, action: PendingAction) {
        self.handle_one_retry(action, 0).await
    }

    async fn handle_one_retry(&mut self, action: PendingAction, prior_defers: u32) {
        let now = Instant::now();
        self.backpressure.tick(now);
        // Compute live queue depth (channel + deferred heap) and
        // run hysteresis; surface edge crossings to the
        // dispatcher so it can broadcast
        // `DaemonControl::BackpressureOn`/`Off` to supervised
        // daemons.
        let depth = self.actions_rx.len() + self.deferred.len() + 1;
        let change = self
            .backpressure
            .update_cluster_backpressure(depth, &self.config.backpressure);
        match change {
            ClusterBackpressureChange::Asserted => {
                ExecutorStats::inc(&self.stats.cluster_backpressure_asserts);
                self.dispatcher.on_cluster_backpressure(change);
            }
            ClusterBackpressureChange::Released => {
                ExecutorStats::inc(&self.stats.cluster_backpressure_releases);
                self.dispatcher.on_cluster_backpressure(change);
            }
            ClusterBackpressureChange::Steady => {}
        }
        match self
            .backpressure
            .admit(&action.action, now, &self.config.backpressure)
        {
            AdmissionResult::Admit => {
                self.dispatch_now_with_defer_count(action, now, prior_defers).await
            }
            AdmissionResult::Defer { retry_after } => {
                let next_count = prior_defers.saturating_add(1);
                if next_count > self.config.backpressure.max_defer_count {
                    ExecutorStats::inc(&self.stats.failed);
                    let reason = format!(
                        "deferred {next_count} times — exceeds max_defer_count {}",
                        self.config.backpressure.max_defer_count,
                    );
                    self.record_failure(
                        format!("action-id:{}", action.id.0),
                        reason.clone(),
                    );
                    let _ = append_failed(&self.chain_appender, &action, reason, None);
                    return;
                }
                ExecutorStats::inc(&self.stats.deferred);
                self.deferred.push(DeferredEntry {
                    retry_at: now + retry_after,
                    action,
                    defer_count: next_count,
                });
            }
            AdmissionResult::Gate {
                cooldown_until,
                reason,
            } => {
                ExecutorStats::inc(&self.stats.gated);
                let age = cooldown_until.saturating_duration_since(now);
                let cooldown_ms = age.as_millis() as u64;
                self.record_failure(
                    format!("action-id:{}", action.id.0),
                    format!("gated ({reason}) for {cooldown_ms} ms"),
                );
                let _ = append_gated(
                    &self.chain_appender,
                    &action,
                    reason.to_string(),
                    Some(cooldown_ms),
                );
            }
        }
    }

    async fn dispatch_now_with_defer_count(
        &mut self,
        action: PendingAction,
        admit_anchor: Instant,
        prior_defers: u32,
    ) {
        // Wrap the dispatcher in `catch_unwind` so a panicking
        // future doesn't unwind the executor task. The trait is
        // pluggable + third-party-installed; trust-but-isolate.
        let dispatch_future = self.dispatcher.dispatch(action.action.clone());
        let result = match std::panic::AssertUnwindSafe(dispatch_future)
            .catch_unwind()
            .await
        {
            Ok(result) => result,
            Err(_) => {
                tracing::error!(
                    target: "meshos",
                    action_id = action.id.0,
                    "dispatcher panicked — recording as drop",
                );
                Err(DispatchError::drop("dispatcher panicked"))
            }
        };
        match result {
            Ok(()) => {
                ExecutorStats::inc(&self.stats.dispatched);
                let _ = append_dispatched(&self.chain_appender, &action);
            }
            Err(err) => {
                // Dispatch did not happen — roll back the
                // reservations admit installed against
                // `admit_anchor` so unrelated future actions
                // aren't gated by a side effect that never
                // occurred.
                self.backpressure
                    .release_failed_admit(&action.action, admit_anchor);
                if let Some(after) = err.retry_after {
                    // Dispatch-error retries share the
                    // max_defer_count budget with admit-side
                    // defers — both occupy the same heap, both
                    // are "this action couldn't run, try later".
                    let next_count = prior_defers.saturating_add(1);
                    if next_count > self.config.backpressure.max_defer_count {
                        ExecutorStats::inc(&self.stats.failed);
                        let reason = format!(
                            "dispatch retry budget exhausted after {next_count} attempts",
                        );
                        self.record_failure(
                            format!("action-id:{}", action.id.0),
                            reason.clone(),
                        );
                        let _ = append_failed(
                            &self.chain_appender,
                            &action,
                            reason,
                            None,
                        );
                        return;
                    }
                    ExecutorStats::inc(&self.stats.dispatch_retries);
                    let retry_ms = after.as_millis() as u64;
                    let _ = append_failed(
                        &self.chain_appender,
                        &action,
                        err.reason.clone(),
                        Some(retry_ms),
                    );
                    let now = Instant::now();
                    self.deferred.push(DeferredEntry {
                        retry_at: now + after,
                        action,
                        defer_count: next_count,
                    });
                } else {
                    ExecutorStats::inc(&self.stats.failed);
                    let reason = err.reason.clone();
                    self.record_failure(
                        format!("action-id:{}", action.id.0),
                        err.reason,
                    );
                    let _ = append_failed(&self.chain_appender, &action, reason, None);
                }
            }
        }
    }

    fn record_failure(&mut self, source: String, reason: String) {
        if self.recent_failures.len() >= RECENT_FAILURES_CAPACITY {
            self.recent_failures.pop_front();
        }
        self.recent_failures.push_back(FailureRecord {
            source,
            reason,
            age_ms: 0,
        });
    }

}

/// External handle for sampling executor live state.
#[derive(Clone)]
pub struct ExecutorHandle {
    stats: Arc<ExecutorStats>,
}

impl ExecutorHandle {
    /// Sample the current stats. Atomic loads; consistent
    /// per-counter but not as a single snapshot.
    pub fn stats(&self) -> ExecutorStatsSnapshot {
        ExecutorStatsSnapshot {
            dispatched: self.stats.dispatched.load(Ordering::Relaxed),
            failed: self.stats.failed.load(Ordering::Relaxed),
            deferred: self.stats.deferred.load(Ordering::Relaxed),
            gated: self.stats.gated.load(Ordering::Relaxed),
            dispatch_retries: self.stats.dispatch_retries.load(Ordering::Relaxed),
            cluster_backpressure_asserts: self
                .stats
                .cluster_backpressure_asserts
                .load(Ordering::Relaxed),
            cluster_backpressure_releases: self
                .stats
                .cluster_backpressure_releases
                .load(Ordering::Relaxed),
        }
    }
}

/// Plain-value stats snapshot (no atomics; safe to copy +
/// serialize).
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct ExecutorStatsSnapshot {
    /// Total actions admitted + successfully dispatched.
    pub dispatched: u64,
    /// Total actions admitted but failed in dispatch.
    pub failed: u64,
    /// Total `AdmissionResult::Defer` re-queues.
    pub deferred: u64,
    /// Total `AdmissionResult::Gate` drops.
    pub gated: u64,
    /// Total dispatch errors retried via `retry_after`.
    pub dispatch_retries: u64,
    /// Number of cluster-backpressure assert edges surfaced.
    pub cluster_backpressure_asserts: u64,
    /// Number of cluster-backpressure release edges surfaced.
    pub cluster_backpressure_releases: u64,
}

async fn sleep_until_opt(deadline: Option<Instant>) {
    if let Some(deadline) = deadline {
        sleep_until(tokio::time::Instant::from_std(deadline)).await;
    } else {
        // No deferred work — park forever. The select! arm
        // gating on `if next_deadline.is_some()` keeps this
        // branch from ever being polled when no deadline is
        // pending.
        std::future::pending::<()>().await;
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::sync::mpsc;

    use super::*;
    use super::super::action::{ActionId, MaintenanceTransition};
    use super::super::config::MeshOsConfig;
    use super::super::event::{ChainId, DaemonRef};

    fn pending(id: u64, action: MeshOsAction) -> PendingAction {
        PendingAction {
            id: ActionId(id),
            action,
            emitted_at: Instant::now(),
        }
    }

    fn dref(name: &str, id: u64) -> DaemonRef {
        DaemonRef {
            id,
            name: name.into(),
        }
    }

    fn fast_cfg() -> Arc<MeshOsConfig> {
        Arc::new(MeshOsConfig::default())
    }

    #[tokio::test]
    async fn admitted_actions_reach_the_dispatcher() {
        let (tx, rx) = mpsc::channel(8);
        let cfg = fast_cfg();
        let dispatcher = Arc::new(LoggingDispatcher::new());
        let exec = ActionExecutor::new(rx, cfg, Arc::clone(&dispatcher));
        let task = tokio::spawn(exec.run());

        tx.send(pending(
            1,
            MeshOsAction::CommitMaintenanceTransition {
                node: 1,
                target: MaintenanceTransition::Maintenance,
            },
        ))
        .await
        .unwrap();
        tx.send(pending(
            2,
            MeshOsAction::CommitMaintenanceTransition {
                node: 1,
                target: MaintenanceTransition::Active,
            },
        ))
        .await
        .unwrap();
        drop(tx);

        let stats = task.await.expect("join");
        assert_eq!(stats.dispatched.load(Ordering::Relaxed), 2);
        assert_eq!(dispatcher.log().len(), 2);
    }

    #[tokio::test]
    async fn gated_actions_do_not_reach_the_dispatcher() {
        let (tx, rx) = mpsc::channel(8);
        let cfg = fast_cfg();
        let dispatcher = Arc::new(LoggingDispatcher::new());
        let mut exec = ActionExecutor::new(rx, cfg, Arc::clone(&dispatcher));
        // Pre-load the daemon gate so StartDaemon is gated.
        let d = dref("telemetry", 1);
        exec.backpressure
            .record_daemon_gate(d.clone(), Instant::now() + Duration::from_secs(60));
        let task = tokio::spawn(exec.run());

        tx.send(pending(1, MeshOsAction::StartDaemon { daemon: d }))
            .await
            .unwrap();
        drop(tx);

        let stats = task.await.expect("join");
        assert_eq!(stats.dispatched.load(Ordering::Relaxed), 0);
        assert_eq!(stats.gated.load(Ordering::Relaxed), 1);
        assert_eq!(dispatcher.log().len(), 0);
    }

    #[tokio::test]
    async fn deferred_actions_eventually_reach_the_dispatcher() {
        // Two PullReplica in quick succession; default
        // pull_cooldown is 250 ms so the second defers.
        // tokio::time::pause() doesn't compose with our
        // Instant::now() reads (we use std time, not tokio
        // time), so we rely on real-time delays — pull_cooldown
        // is 250 ms, drift is small enough.
        let (tx, rx) = mpsc::channel(8);
        let cfg = fast_cfg();
        let dispatcher = Arc::new(LoggingDispatcher::new());
        let exec = ActionExecutor::new(rx, cfg, Arc::clone(&dispatcher));
        let task = tokio::spawn(exec.run());

        let chain_a: ChainId = 1;
        let chain_b: ChainId = 2;
        tx.send(pending(
            1,
            MeshOsAction::PullReplica {
                chain: chain_a,
                source: 5,
            },
        ))
        .await
        .unwrap();
        tx.send(pending(
            2,
            MeshOsAction::PullReplica {
                chain: chain_b,
                source: 5,
            },
        ))
        .await
        .unwrap();

        // Give the executor enough wall time to: dispatch the
        // first, defer the second, wake up after the cooldown,
        // and dispatch the second.
        tokio::time::sleep(Duration::from_millis(500)).await;
        drop(tx);

        let stats = task.await.expect("join");
        assert_eq!(
            stats.dispatched.load(Ordering::Relaxed),
            2,
            "both pulls should eventually reach the dispatcher",
        );
        assert!(
            stats.deferred.load(Ordering::Relaxed) >= 1,
            "second pull should have been deferred at least once",
        );
        assert_eq!(dispatcher.log().len(), 2);
    }

    #[tokio::test]
    async fn dispatch_errors_without_retry_record_failures() {
        let (tx, rx) = mpsc::channel(8);
        let cfg = fast_cfg();
        let dispatcher = Arc::new(LoggingDispatcher::new());
        dispatcher.fail_next(DispatchError::drop("boom"));
        let exec = ActionExecutor::new(rx, cfg, Arc::clone(&dispatcher));
        let task = tokio::spawn(exec.run());

        tx.send(pending(
            1,
            MeshOsAction::CommitMaintenanceTransition {
                node: 1,
                target: MaintenanceTransition::Active,
            },
        ))
        .await
        .unwrap();
        drop(tx);

        let stats = task.await.expect("join");
        assert_eq!(stats.dispatched.load(Ordering::Relaxed), 0);
        assert_eq!(stats.failed.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn dispatch_errors_with_retry_re_enqueue() {
        let (tx, rx) = mpsc::channel(8);
        let cfg = fast_cfg();
        let dispatcher = Arc::new(LoggingDispatcher::new());
        // First call errors with a 50 ms retry; the second
        // call (after re-queue) succeeds.
        dispatcher.fail_next(DispatchError::retry("transient", Duration::from_millis(50)));
        let exec = ActionExecutor::new(rx, cfg, Arc::clone(&dispatcher));
        let task = tokio::spawn(exec.run());

        tx.send(pending(
            1,
            MeshOsAction::CommitMaintenanceTransition {
                node: 1,
                target: MaintenanceTransition::Active,
            },
        ))
        .await
        .unwrap();
        // Wait long enough for the retry to fire + drain.
        tokio::time::sleep(Duration::from_millis(200)).await;
        drop(tx);

        let stats = task.await.expect("join");
        assert_eq!(stats.dispatched.load(Ordering::Relaxed), 1);
        assert_eq!(stats.dispatch_retries.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn executor_exits_when_sender_drops() {
        let (tx, rx) = mpsc::channel(8);
        let cfg = fast_cfg();
        let dispatcher = Arc::new(LoggingDispatcher::new());
        let exec = ActionExecutor::new(rx, cfg, Arc::clone(&dispatcher));
        let task = tokio::spawn(exec.run());
        drop(tx);
        let stats = tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .expect("executor did not exit after sender dropped")
            .expect("join");
        assert_eq!(stats.dispatched.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn dispatch_retry_drops_after_exceeding_max_defer_count() {
        // Regression for I7: a dispatcher that returns
        // `retry_after` forever (a poison pill) used to occupy
        // the deferred-action heap indefinitely. The defer
        // budget caps total attempts; past the cap the
        // executor drops the action with a failure record.
        struct AlwaysRetry {
            attempts: parking_lot::Mutex<u32>,
        }
        impl ActionDispatcher for AlwaysRetry {
            fn dispatch<'a>(
                &'a self,
                _action: MeshOsAction,
            ) -> BoxFuture<'a, Result<(), DispatchError>> {
                Box::pin(async move {
                    *self.attempts.lock() += 1;
                    Err(DispatchError::retry(
                        "transient",
                        Duration::from_millis(5),
                    ))
                })
            }
        }

        let mut cfg = MeshOsConfig::default();
        cfg.backpressure.max_defer_count = 3;
        let cfg = Arc::new(cfg);
        let (tx, rx) = mpsc::channel(8);
        let dispatcher = Arc::new(AlwaysRetry {
            attempts: parking_lot::Mutex::new(0),
        });
        let exec = ActionExecutor::new(rx, cfg, Arc::clone(&dispatcher));
        let task = tokio::spawn(exec.run());

        tx.send(pending(
            1,
            MeshOsAction::CommitMaintenanceTransition {
                node: 1,
                target: MaintenanceTransition::Active,
            },
        ))
        .await
        .unwrap();
        // Give the executor enough wall time for max_defer_count
        // attempts + a few ms each.
        tokio::time::sleep(Duration::from_millis(200)).await;
        drop(tx);
        let stats = task.await.expect("join");
        let attempts = *dispatcher.attempts.lock();
        assert_eq!(
            stats.failed.load(Ordering::Relaxed),
            1,
            "action must drop with a failure after exceeding max_defer_count",
        );
        assert!(
            attempts >= 3 && attempts <= 5,
            "expected ~max_defer_count dispatch attempts, got {attempts}",
        );
    }

    #[tokio::test]
    async fn dispatcher_panic_does_not_kill_executor() {
        // Regression for I6: a panicking dispatcher future used
        // to unwind the executor task. The catch_unwind wrapper
        // converts the panic into a `DispatchError::drop`, so
        // the executor continues servicing subsequent actions.
        struct PanicOnce {
            armed: parking_lot::Mutex<bool>,
            log: Mutex<Vec<MeshOsAction>>,
        }
        impl ActionDispatcher for PanicOnce {
            fn dispatch<'a>(
                &'a self,
                action: MeshOsAction,
            ) -> BoxFuture<'a, Result<(), DispatchError>> {
                Box::pin(async move {
                    let armed = {
                        let mut g = self.armed.lock();
                        let was = *g;
                        *g = false;
                        was
                    };
                    if armed {
                        panic!("boom");
                    }
                    self.log.lock().push(action);
                    Ok(())
                })
            }
        }

        let (tx, rx) = mpsc::channel(8);
        let cfg = fast_cfg();
        let dispatcher = Arc::new(PanicOnce {
            armed: parking_lot::Mutex::new(true),
            log: Mutex::new(Vec::new()),
        });
        let exec = ActionExecutor::new(rx, cfg, Arc::clone(&dispatcher));
        let task = tokio::spawn(exec.run());

        tx.send(pending(
            1,
            MeshOsAction::CommitMaintenanceTransition {
                node: 1,
                target: MaintenanceTransition::Maintenance,
            },
        ))
        .await
        .unwrap();
        tx.send(pending(
            2,
            MeshOsAction::CommitMaintenanceTransition {
                node: 1,
                target: MaintenanceTransition::Active,
            },
        ))
        .await
        .unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        drop(tx);

        let stats = task.await.expect(
            "executor task should NOT have panicked despite dispatcher panic",
        );
        assert_eq!(
            stats.dispatched.load(Ordering::Relaxed),
            1,
            "second action should have dispatched after the first panicked",
        );
        assert_eq!(stats.failed.load(Ordering::Relaxed), 1);
        assert_eq!(dispatcher.log.lock().len(), 1);
    }

    #[tokio::test]
    async fn cluster_backpressure_edges_surface_through_dispatcher_hook() {
        // Set high-water = 3, low-water = 1 so the channel-only
        // depth crosses the threshold quickly. The executor pushes
        // four actions into a buffered channel before draining;
        // depth at first admit reaches 4 (rx.len() == 3 + 1 in
        // flight) crossing the high mark, then drops as actions
        // drain.
        let mut cfg = MeshOsConfig::default();
        cfg.backpressure.cluster_backpressure_threshold = 3;
        cfg.backpressure.cluster_backpressure_release = 1;
        let cfg = Arc::new(cfg);
        let (tx, rx) = mpsc::channel(8);
        let dispatcher = Arc::new(LoggingDispatcher::new());
        let exec = ActionExecutor::new(rx, cfg, Arc::clone(&dispatcher));
        // Buffer four actions before letting the executor start
        // (cargo holds the spawn until we `.await`).
        for i in 1..=4u64 {
            tx.send(pending(
                i,
                MeshOsAction::CommitMaintenanceTransition {
                    node: 1,
                    target: MaintenanceTransition::Active,
                },
            ))
            .await
            .unwrap();
        }
        let task = tokio::spawn(exec.run());
        // Let everything drain.
        tokio::time::sleep(Duration::from_millis(50)).await;
        drop(tx);
        let stats = task.await.expect("join");
        assert!(
            stats.cluster_backpressure_asserts.load(Ordering::Relaxed) >= 1,
            "depth crossed the high-water mark at least once",
        );
        assert!(
            stats.cluster_backpressure_releases.load(Ordering::Relaxed) >= 1,
            "depth dropped below the low-water mark at least once",
        );
        let log = dispatcher.backpressure_log();
        assert!(matches!(log.first(), Some(ClusterBackpressureChange::Asserted)));
        assert!(matches!(log.last(), Some(ClusterBackpressureChange::Released)));
    }

    #[tokio::test]
    async fn dispatch_failure_with_retry_releases_pull_cooldown() {
        // Regression: a PullReplica admit sets the global pull
        // cooldown; if dispatch fails the cooldown must be
        // rolled back so unrelated pulls aren't gated by a side
        // effect that never happened.
        let (tx, rx) = mpsc::channel(8);
        let cfg = fast_cfg();
        let dispatcher = Arc::new(LoggingDispatcher::new());
        // First dispatch fails with a long retry hint; the
        // second admit (on a different chain) must succeed
        // without waiting on the rolled-back cooldown.
        dispatcher.fail_next(DispatchError::retry(
            "transient",
            Duration::from_secs(60),
        ));
        let exec = ActionExecutor::new(rx, cfg, Arc::clone(&dispatcher));
        let task = tokio::spawn(exec.run());

        tx.send(pending(
            1,
            MeshOsAction::PullReplica { chain: 1, source: 5 },
        ))
        .await
        .unwrap();
        // Brief settle: first action processed (admit + fail +
        // release + heap push) before the second arrives.
        tokio::time::sleep(Duration::from_millis(50)).await;
        tx.send(pending(
            2,
            MeshOsAction::PullReplica { chain: 2, source: 5 },
        ))
        .await
        .unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        drop(tx);

        let stats = task.await.expect("join");
        assert_eq!(
            stats.dispatched.load(Ordering::Relaxed),
            1,
            "second pull should dispatch immediately after the first \
             released its leaked cooldown",
        );
        assert_eq!(stats.dispatch_retries.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn handle_exposes_atomic_stats_to_outside_observers() {
        let (tx, rx) = mpsc::channel(8);
        let cfg = fast_cfg();
        let dispatcher = Arc::new(LoggingDispatcher::new());
        let exec = ActionExecutor::new(rx, cfg, Arc::clone(&dispatcher));
        let handle = exec.handle();
        let task = tokio::spawn(exec.run());

        tx.send(pending(
            1,
            MeshOsAction::CommitMaintenanceTransition {
                node: 1,
                target: MaintenanceTransition::Active,
            },
        ))
        .await
        .unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;

        let snap = handle.stats();
        assert!(snap.dispatched >= 1);
        drop(tx);
        let _ = task.await;
    }
}
