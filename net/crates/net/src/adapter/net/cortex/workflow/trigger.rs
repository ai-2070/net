//! Triggers (plan piece 3 / Phase B) — the substrate of dependencies,
//! branches, and DAGs.
//!
//! A trigger is a condition that, once satisfied, fires an [`Action`]
//! (submit / start a task). **DAGs emerge** from tasks arming triggers
//! on each other — `after_task` is a dependency edge, `if_result` is a
//! branch — with no DAG DSL, no controller loop, no engine. The
//! [`TriggerEngine`] is a pure, *indexed* predicate evaluator: it is
//! driven by events (a task changed status, the logical clock ticked),
//! and a fired event touches only the triggers keyed to it — the
//! plan's O(relevant)-not-O(all) index (perf note).
//!
//! Determinism is preserved exactly as in the fold: there is no
//! `now()`. The clock is an explicit `tick` input and results are an
//! explicit map, so the same event sequence fires the same triggers on
//! replay.

use std::collections::{BTreeMap, HashMap};

use super::state::WorkflowState;
use super::types::{TaskId, TaskStatus};

/// A condition that, once satisfied, fires an [`Action`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Trigger {
    /// Dependency: fires once `task` reaches `Done`. The edge
    /// `after_task:<id>` of the plan.
    AfterTask(TaskId),
    /// Branch: fires once `task` is `Done` **and** its recorded result
    /// at `key` equals `value` (the `if_result:<path matches>` shape).
    IfResult {
        /// The task whose result gates the branch.
        task: TaskId,
        /// Result key (a `results/*` path, modeled as a key).
        key: String,
        /// The value that fires this branch.
        value: String,
    },
    /// Failure-aware dependency: fires once `task` reaches **either**
    /// terminal state (`Done` *or* `Failed`). This is the primitive
    /// failure propagation needs — `AfterTask` only observes `Done`, so
    /// a failed predecessor would leave its dependents armed forever.
    /// A handler inspects the predecessor's status to branch on the
    /// outcome (e.g. submit a compensating task on `Failed`).
    AfterTerminal(TaskId),
    /// Timestamp: fires once the logical clock reaches `tick`. Time is
    /// an explicit input (never `now()`), preserving determinism.
    AtTick(u64),
}

/// What a fired trigger does. A DAG edge is, e.g., `AfterTask(A) →
/// Submit(B)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Submit a new task (the dependent / branch target).
    Submit(TaskId),
    /// Start (→ `Running`) an already-submitted task.
    Start(TaskId),
}

/// The deterministic world a trigger evaluates against. No `now()`:
/// the clock is the explicit `tick`, results are an explicit map.
pub struct TriggerWorld<'a> {
    /// Task statuses (for the `Done` checks of `after_task` /
    /// `if_result`).
    pub tasks: &'a WorkflowState,
    /// Logical clock — advanced by explicit `Tick` events.
    pub tick: u64,
    /// Recorded task results: `task → (key → value)`. Content-
    /// addressed in production (`results/stepN.out.ref`); a value map
    /// here for branch evaluation.
    pub results: &'a HashMap<TaskId, HashMap<String, String>>,
}

impl<'a> TriggerWorld<'a> {
    /// Build a world with no results (sufficient for `after_task` /
    /// `at_tick`).
    pub fn new(tasks: &'a WorkflowState, tick: u64) -> Self {
        Self {
            tasks,
            tick,
            results: EMPTY_RESULTS.get_or_init(HashMap::new),
        }
    }

    fn task_done(&self, task: TaskId) -> bool {
        self.tasks
            .get(task)
            .map(|s| s.status == TaskStatus::Done)
            .unwrap_or(false)
    }

    fn task_terminal(&self, task: TaskId) -> bool {
        self.tasks
            .get(task)
            .map(|s| s.status.is_terminal())
            .unwrap_or(false)
    }
}

static EMPTY_RESULTS: std::sync::OnceLock<HashMap<TaskId, HashMap<String, String>>> =
    std::sync::OnceLock::new();

impl Trigger {
    /// Is this trigger's condition satisfied in `world`? Pure and
    /// deterministic.
    pub fn is_satisfied(&self, world: &TriggerWorld<'_>) -> bool {
        match self {
            Trigger::AfterTask(task) => world.task_done(*task),
            Trigger::IfResult { task, key, value } => {
                world.task_done(*task)
                    && world
                        .results
                        .get(task)
                        .and_then(|m| m.get(key))
                        .map(|v| v == value)
                        .unwrap_or(false)
            }
            Trigger::AfterTerminal(task) => world.task_terminal(*task),
            Trigger::AtTick(tick) => world.tick >= *tick,
        }
    }

    /// Can this trigger *never* become satisfied in `world`, so it
    /// should be disarmed rather than re-armed? A task's result and
    /// done-ness are immutable once it is terminal, so:
    /// - `AfterTask` waits for `Done`; if the task reached a terminal
    ///   state that is *not* `Done` (i.e. `Failed`), it can never fire.
    /// - `IfResult` needs `Done` + a value match; on a terminal task
    ///   whose value doesn't match (or that `Failed`), it can never fire.
    ///
    /// `AfterTerminal` fires *on* terminal so it is never dead, and
    /// `AtTick` is drained by [`Self::on_tick`], never re-armed here.
    /// Without this, a branch's non-taken `IfResult` arms accumulate
    /// permanently in `by_task` once the task completes (review #7).
    fn is_dead(&self, world: &TriggerWorld<'_>) -> bool {
        match self {
            Trigger::AfterTask(task) | Trigger::IfResult { task, .. } => {
                world.task_terminal(*task) && !self.is_satisfied(world)
            }
            Trigger::AfterTerminal(_) | Trigger::AtTick(_) => false,
        }
    }

    /// The index bucket this trigger waits on, so a fired event touches
    /// only the triggers keyed to it (perf note).
    fn key(&self) -> TriggerKey {
        match self {
            Trigger::AfterTask(task)
            | Trigger::IfResult { task, .. }
            | Trigger::AfterTerminal(task) => TriggerKey::Task(*task),
            Trigger::AtTick(tick) => TriggerKey::Tick(*tick),
        }
    }
}

/// Index bucket — what a trigger waits on. `Tick` carries the deadline
/// so the engine can key the tick index by value (corrections #5).
enum TriggerKey {
    Task(TaskId),
    Tick(u64),
}

/// An indexed set of armed triggers. Driven by events: when a task
/// changes status call [`Self::on_task_change`]; when the clock advances
/// call [`Self::on_tick`]. Each evaluates **only** the triggers keyed to
/// that event and returns the [`Action`]s of the satisfied ones,
/// disarming them (fire-once).
///
/// The engine is pure — it performs no I/O and starts no tasks itself;
/// the caller applies the returned actions (e.g. via
/// [`WorkflowAdapter`](super::WorkflowAdapter)). That is what keeps this
/// a *substrate* rather than a controller loop.
///
/// **Complexity** (T = armed triggers; the index is the plan's
/// O(relevant)-not-O(all) promise on *both* axes — corrections #5):
/// - [`arm`](Self::arm): O(1) amortized for task-keyed triggers, O(log T)
///   for `AtTick` (BTreeMap insert).
/// - [`on_task_change`](Self::on_task_change): O(triggers waiting on that
///   task) — task-keyed, not a scan of all T.
/// - [`on_tick`](Self::on_tick): O(triggers due at `now` + log T) —
///   the BTreeMap drains the `tick <= now` prefix, never the whole set
///   (the naive `Vec` scan this replaced was O(all tick triggers)).
/// - [`on_delete`](Self::on_delete): O(triggers waiting on that task).
/// - [`armed_count`](Self::armed_count): O(distinct task ids + distinct
///   tick values).
#[derive(Default)]
pub struct TriggerEngine {
    by_task: HashMap<TaskId, Vec<(Trigger, Action)>>,
    /// `AtTick` triggers keyed by their tick value, so `on_tick(now)`
    /// drains only the due `tick <= now` prefix instead of scanning
    /// every armed tick trigger (corrections #5).
    by_tick: BTreeMap<u64, Vec<(Trigger, Action)>>,
}

impl TriggerEngine {
    /// A fresh engine with nothing armed.
    pub fn new() -> Self {
        Self::default()
    }

    /// Arm `trigger` to fire `action` once satisfied. O(1) amortized for
    /// task-keyed triggers; O(log T) for `AtTick` (BTreeMap insert).
    pub fn arm(&mut self, trigger: Trigger, action: Action) {
        match trigger.key() {
            TriggerKey::Task(task) => self.by_task.entry(task).or_default().push((trigger, action)),
            TriggerKey::Tick(tick) => self.by_tick.entry(tick).or_default().push((trigger, action)),
        }
    }

    /// `task` changed status: evaluate only the triggers waiting on it,
    /// returning + disarming the satisfied ones. Triggers that aren't
    /// yet satisfied stay armed — unless they can *never* be satisfied
    /// ([`Trigger::is_dead`], e.g. a branch's non-matching `IfResult`
    /// arm once the task is terminal), which are dropped so they don't
    /// accumulate forever (review #7).
    pub fn on_task_change(&mut self, task: TaskId, world: &TriggerWorld<'_>) -> Vec<Action> {
        let Some(armed) = self.by_task.remove(&task) else {
            return Vec::new();
        };
        let (fired, rest): (Vec<_>, Vec<_>) =
            armed.into_iter().partition(|(t, _)| t.is_satisfied(world));
        // Keep only arms that may still fire; drop the dead ones.
        let still_armed: Vec<_> = rest.into_iter().filter(|(t, _)| !t.is_dead(world)).collect();
        if !still_armed.is_empty() {
            self.by_task.insert(task, still_armed);
        }
        fired.into_iter().map(|(_, action)| action).collect()
    }

    /// The logical clock advanced to `world.tick`: fire + disarm every
    /// `AtTick` trigger whose deadline has passed. O(due + log T): the
    /// BTreeMap splits off the `tick <= now` prefix (all of which are
    /// satisfied — `AtTick` fires at `now >= tick`), leaving the future
    /// ones armed, rather than scanning every armed tick trigger.
    pub fn on_tick(&mut self, world: &TriggerWorld<'_>) -> Vec<Action> {
        // Keep the strictly-future ones (tick > now); drain the rest.
        let future = self.by_tick.split_off(&world.tick.saturating_add(1));
        let due = std::mem::replace(&mut self.by_tick, future);
        due.into_values()
            .flatten()
            .map(|(_, action)| action)
            .collect()
    }

    /// `task` was deleted: drop every trigger waiting on it — they can
    /// never fire (the task is gone), so leaving them armed would leak
    /// (corrections #4). Returns the number dropped. When deleting a
    /// subtree, call this for each id in
    /// [`WorkflowAdapter::subtree`](super::WorkflowAdapter::subtree).
    pub fn on_delete(&mut self, task: TaskId) -> usize {
        self.by_task.remove(&task).map(|v| v.len()).unwrap_or(0)
    }

    /// Total triggers still armed (not yet fired).
    pub fn armed_count(&self) -> usize {
        self.by_task.values().map(Vec::len).sum::<usize>()
            + self.by_tick.values().map(Vec::len).sum::<usize>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::types::TaskState;

    fn results(pairs: &[(TaskId, &str, &str)]) -> HashMap<TaskId, HashMap<String, String>> {
        let mut m: HashMap<TaskId, HashMap<String, String>> = HashMap::new();
        for (task, k, v) in pairs {
            m.entry(*task)
                .or_default()
                .insert((*k).to_string(), (*v).to_string());
        }
        m
    }

    fn state_with(pairs: &[(TaskId, TaskStatus)]) -> WorkflowState {
        let mut s = WorkflowState::new();
        for (id, status) in pairs {
            s.tasks.insert(
                *id,
                TaskState {
                    step: 0,
                    status: *status,
                    attempts: 0,
                },
            );
        }
        s
    }

    #[test]
    fn after_task_satisfied_only_when_done() {
        let pending = state_with(&[(1, TaskStatus::Running)]);
        let done = state_with(&[(1, TaskStatus::Done)]);
        let t = Trigger::AfterTask(1);
        assert!(!t.is_satisfied(&TriggerWorld::new(&pending, 0)));
        assert!(t.is_satisfied(&TriggerWorld::new(&done, 0)));
        // Unknown task → not satisfied.
        assert!(!t.is_satisfied(&TriggerWorld::new(&WorkflowState::new(), 0)));
    }

    #[test]
    fn if_result_needs_done_and_matching_value() {
        let done = state_with(&[(1, TaskStatus::Done)]);
        let res = results(&[(1, "branch", "left")]);
        let world = TriggerWorld {
            tasks: &done,
            tick: 0,
            results: &res,
        };
        let left = Trigger::IfResult {
            task: 1,
            key: "branch".into(),
            value: "left".into(),
        };
        let right = Trigger::IfResult {
            task: 1,
            key: "branch".into(),
            value: "right".into(),
        };
        assert!(left.is_satisfied(&world));
        assert!(!right.is_satisfied(&world)); // value mismatch
        // Done but no result recorded → not satisfied.
        let empty = HashMap::new();
        let world2 = TriggerWorld {
            tasks: &done,
            tick: 0,
            results: &empty,
        };
        assert!(!left.is_satisfied(&world2));
    }

    #[test]
    fn after_terminal_fires_on_done_or_failed_but_not_running() {
        let t = Trigger::AfterTerminal(1);
        let running = state_with(&[(1, TaskStatus::Running)]);
        let waiting = state_with(&[(1, TaskStatus::Waiting)]);
        let done = state_with(&[(1, TaskStatus::Done)]);
        let failed = state_with(&[(1, TaskStatus::Failed)]);
        // Non-terminal → not satisfied (where AfterTask would also wait).
        assert!(!t.is_satisfied(&TriggerWorld::new(&running, 0)));
        assert!(!t.is_satisfied(&TriggerWorld::new(&waiting, 0)));
        // BOTH terminal states fire — this is the failure-propagation
        // primitive `AfterTask` (Done-only) lacks.
        assert!(t.is_satisfied(&TriggerWorld::new(&done, 0)));
        assert!(t.is_satisfied(&TriggerWorld::new(&failed, 0)));
        // A `Failed` predecessor leaves an `AfterTask` armed forever but
        // fires `AfterTerminal`.
        assert!(!Trigger::AfterTask(1).is_satisfied(&TriggerWorld::new(&failed, 0)));
        // Unknown task → not satisfied.
        assert!(!t.is_satisfied(&TriggerWorld::new(&WorkflowState::new(), 0)));
    }

    #[test]
    fn at_tick_fires_once_clock_reaches_it() {
        let s = WorkflowState::new();
        let t = Trigger::AtTick(5);
        assert!(!t.is_satisfied(&TriggerWorld::new(&s, 4)));
        assert!(t.is_satisfied(&TriggerWorld::new(&s, 5)));
        assert!(t.is_satisfied(&TriggerWorld::new(&s, 9)));
    }

    #[test]
    fn engine_fires_only_triggers_keyed_to_the_event() {
        // Arm one trigger per task; a change to task 1 must evaluate
        // ONLY task 1's trigger — the perf-note index.
        let mut eng = TriggerEngine::new();
        eng.arm(Trigger::AfterTask(1), Action::Submit(10));
        eng.arm(Trigger::AfterTask(2), Action::Submit(20));
        assert_eq!(eng.armed_count(), 2);

        let done1 = state_with(&[(1, TaskStatus::Done), (2, TaskStatus::Running)]);
        let fired = eng.on_task_change(1, &TriggerWorld::new(&done1, 0));
        assert_eq!(fired, vec![Action::Submit(10)]);
        // Task 2's trigger is untouched + still armed.
        assert_eq!(eng.armed_count(), 1);
    }

    #[test]
    fn unsatisfied_trigger_stays_armed_until_condition_holds() {
        let mut eng = TriggerEngine::new();
        eng.arm(Trigger::AfterTask(1), Action::Start(10));

        // Task 1 transitions to Running (not Done yet) → nothing fires,
        // trigger stays armed.
        let running = state_with(&[(1, TaskStatus::Running)]);
        assert!(eng
            .on_task_change(1, &TriggerWorld::new(&running, 0))
            .is_empty());
        assert_eq!(eng.armed_count(), 1);

        // Later it reaches Done → fires + disarms.
        let done = state_with(&[(1, TaskStatus::Done)]);
        assert_eq!(
            eng.on_task_change(1, &TriggerWorld::new(&done, 0)),
            vec![Action::Start(10)]
        );
        assert_eq!(eng.armed_count(), 0);
    }

    #[test]
    fn branch_fires_exactly_the_matching_arm() {
        // if_result fan-out: two branches on the same task, only the
        // one whose value matches the recorded result fires.
        let mut eng = TriggerEngine::new();
        eng.arm(
            Trigger::IfResult {
                task: 1,
                key: "branch".into(),
                value: "left".into(),
            },
            Action::Submit(10),
        );
        eng.arm(
            Trigger::IfResult {
                task: 1,
                key: "branch".into(),
                value: "right".into(),
            },
            Action::Submit(20),
        );

        let done = state_with(&[(1, TaskStatus::Done)]);
        let res = results(&[(1, "branch", "left")]);
        let world = TriggerWorld {
            tasks: &done,
            tick: 0,
            results: &res,
        };
        assert_eq!(eng.on_task_change(1, &world), vec![Action::Submit(10)]);
        // Task 1 is terminal (Done), so the non-matching "right" branch
        // can never fire — it is disarmed rather than re-armed, so it
        // doesn't accumulate forever (review #7).
        assert_eq!(eng.armed_count(), 0);
    }

    /// An `AfterTask` waiting on a task that reaches a terminal state
    /// *other than* `Done` (i.e. `Failed`) can never fire, so it is
    /// disarmed instead of re-armed (review #7).
    #[test]
    fn after_task_on_a_failed_task_is_disarmed() {
        let mut eng = TriggerEngine::new();
        eng.arm(Trigger::AfterTask(1), Action::Submit(10));
        assert_eq!(eng.armed_count(), 1);

        let failed = state_with(&[(1, TaskStatus::Failed)]);
        let world = TriggerWorld::new(&failed, 0);
        // Task failed → AfterTask never fires, and is dropped.
        assert!(eng.on_task_change(1, &world).is_empty());
        assert_eq!(eng.armed_count(), 0);
    }

    #[test]
    fn on_delete_prunes_triggers_waiting_on_the_task() {
        // Two tasks (A, B) each gate a dependent; deleting A drops only
        // A's armed trigger (corrections #4 — they can never fire).
        let mut eng = TriggerEngine::new();
        eng.arm(Trigger::AfterTask(1), Action::Submit(10));
        eng.arm(Trigger::AfterTerminal(1), Action::Submit(11));
        eng.arm(Trigger::AfterTask(2), Action::Submit(20));
        assert_eq!(eng.armed_count(), 3);

        // Delete task 1 → both triggers waiting on it are dropped.
        assert_eq!(eng.on_delete(1), 2);
        assert_eq!(eng.armed_count(), 1);
        // Deleting an id with no armed triggers is a no-op.
        assert_eq!(eng.on_delete(99), 0);
        // Task 2's trigger survives and still fires.
        let done2 = state_with(&[(2, TaskStatus::Done)]);
        assert_eq!(
            eng.on_task_change(2, &TriggerWorld::new(&done2, 0)),
            vec![Action::Submit(20)]
        );
    }

    #[test]
    fn tick_triggers_fire_on_clock_advance() {
        let mut eng = TriggerEngine::new();
        eng.arm(Trigger::AtTick(3), Action::Submit(1));
        let s = WorkflowState::new();
        assert!(eng.on_tick(&TriggerWorld::new(&s, 2)).is_empty());
        assert_eq!(eng.armed_count(), 1);
        assert_eq!(
            eng.on_tick(&TriggerWorld::new(&s, 3)),
            vec![Action::Submit(1)]
        );
        assert_eq!(eng.armed_count(), 0);
    }

    #[test]
    fn on_tick_drains_only_the_due_prefix_and_keeps_future_triggers() {
        // The BTreeMap index (corrections #5): on_tick(now) fires the
        // tick<=now prefix in deterministic (tick, insertion) order and
        // leaves strictly-future deadlines armed — not an O(all) scan.
        let mut eng = TriggerEngine::new();
        eng.arm(Trigger::AtTick(3), Action::Submit(1));
        eng.arm(Trigger::AtTick(5), Action::Submit(2));
        eng.arm(Trigger::AtTick(5), Action::Submit(3)); // same deadline
        eng.arm(Trigger::AtTick(9), Action::Submit(4));
        assert_eq!(eng.armed_count(), 4);
        let s = WorkflowState::new();

        // now=5 fires deadlines 3, 5, 5 (ordered), leaving 9 armed.
        assert_eq!(
            eng.on_tick(&TriggerWorld::new(&s, 5)),
            vec![Action::Submit(1), Action::Submit(2), Action::Submit(3)],
        );
        assert_eq!(eng.armed_count(), 1);
        // A tick below the next deadline fires nothing.
        assert!(eng.on_tick(&TriggerWorld::new(&s, 8)).is_empty());
        assert_eq!(eng.armed_count(), 1);
        // now=9 fires the last.
        assert_eq!(
            eng.on_tick(&TriggerWorld::new(&s, 9)),
            vec![Action::Submit(4)]
        );
        assert_eq!(eng.armed_count(), 0);
    }

    /// Phase B "Done when": a dependent task B auto-starts on A's
    /// `Done`, end-to-end over a real workflow chain. The driver is a
    /// few lines (no engine, no loop): on a task change, evaluate the
    /// triggers keyed to it and apply the returned actions.
    #[tokio::test]
    async fn dependent_task_auto_submits_on_predecessor_done() {
        use super::super::WorkflowAdapter;
        use crate::adapter::net::redex::Redex;

        const A: TaskId = 1;
        const B: TaskId = 2;

        let redex = Redex::new();
        let wf = WorkflowAdapter::open(&redex, 0x0F10_00B1).await.unwrap();
        let mut eng = TriggerEngine::new();
        // DAG edge: B depends on A.
        eng.arm(Trigger::AfterTask(A), Action::Submit(B));

        // Run A to completion.
        wf.submit(A).unwrap();
        wf.start(A).unwrap();
        let seq = wf.complete(A).unwrap();
        wf.wait_for_seq(seq).await.unwrap();
        assert!(wf.get(B).is_none(), "B not submitted until A is Done");

        // Driver: A changed → fire its triggers → apply the actions.
        let actions = {
            let state = wf.state();
            let guard = state.read();
            eng.on_task_change(A, &TriggerWorld::new(&guard, 0))
        };
        assert_eq!(actions, vec![Action::Submit(B)]);
        let mut last = 0;
        for action in actions {
            last = match action {
                Action::Submit(id) => wf.submit(id).unwrap(),
                Action::Start(id) => wf.start(id).unwrap(),
            };
        }
        wf.wait_for_seq(last).await.unwrap();

        // B auto-started, and A's trigger is spent (fire-once).
        assert!(wf.get(B).is_some(), "B auto-submitted on A's Done");
        assert_eq!(eng.armed_count(), 0);
    }
}
