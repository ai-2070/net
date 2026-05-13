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
use parking_lot::Mutex;
use tokio::sync::mpsc;
use tokio::time::sleep_until;

use super::action::{MeshOsAction, PendingAction};
use super::backpressure::{AdmissionResult, BackpressureState};
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
        }
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
                    self.handle_one(due.action).await;
                }
            }
        }
        Arc::clone(&self.stats)
    }

    async fn handle_one(&mut self, action: PendingAction) {
        let now = Instant::now();
        self.backpressure.tick(now);
        match self
            .backpressure
            .admit(&action.action, now, &self.config.backpressure)
        {
            AdmissionResult::Admit => self.dispatch_now(action).await,
            AdmissionResult::Defer { retry_after } => {
                ExecutorStats::inc(&self.stats.deferred);
                self.deferred.push(DeferredEntry {
                    retry_at: now + retry_after,
                    action,
                });
            }
            AdmissionResult::Gate {
                cooldown_until,
                reason,
            } => {
                ExecutorStats::inc(&self.stats.gated);
                let age = cooldown_until.saturating_duration_since(now);
                self.record_failure(
                    format!("action-id:{}", action.id.0),
                    format!("gated ({reason}) for {} ms", age.as_millis()),
                );
            }
        }
    }

    async fn dispatch_now(&mut self, action: PendingAction) {
        let result = self
            .dispatcher
            .dispatch(action.action.clone())
            .await;
        match result {
            Ok(()) => {
                ExecutorStats::inc(&self.stats.dispatched);
            }
            Err(err) => {
                if let Some(after) = err.retry_after {
                    ExecutorStats::inc(&self.stats.dispatch_retries);
                    let now = Instant::now();
                    self.deferred.push(DeferredEntry {
                        retry_at: now + after,
                        action,
                    });
                } else {
                    ExecutorStats::inc(&self.stats.failed);
                    self.record_failure(
                        format!("action-id:{}", action.id.0),
                        err.reason,
                    );
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

    /// Test-only view into the recent-failures ring buffer.
    #[cfg(test)]
    pub(crate) fn recent_failures(&self) -> &VecDeque<FailureRecord> {
        &self.recent_failures
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
