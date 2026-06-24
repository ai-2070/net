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

use std::collections::HashMap;

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

    /// The index bucket this trigger waits on, so a fired event touches
    /// only the triggers keyed to it (perf note).
    fn key(&self) -> TriggerKey {
        match self {
            Trigger::AfterTask(task)
            | Trigger::IfResult { task, .. }
            | Trigger::AfterTerminal(task) => TriggerKey::Task(*task),
            Trigger::AtTick(_) => TriggerKey::Tick,
        }
    }
}

/// Index bucket — what a trigger waits on.
enum TriggerKey {
    Task(TaskId),
    Tick,
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
#[derive(Default)]
pub struct TriggerEngine {
    by_task: HashMap<TaskId, Vec<(Trigger, Action)>>,
    by_tick: Vec<(Trigger, Action)>,
}

impl TriggerEngine {
    /// A fresh engine with nothing armed.
    pub fn new() -> Self {
        Self::default()
    }

    /// Arm `trigger` to fire `action` once satisfied.
    pub fn arm(&mut self, trigger: Trigger, action: Action) {
        match trigger.key() {
            TriggerKey::Task(task) => self.by_task.entry(task).or_default().push((trigger, action)),
            TriggerKey::Tick => self.by_tick.push((trigger, action)),
        }
    }

    /// `task` changed status: evaluate only the triggers waiting on it,
    /// returning + disarming the satisfied ones. Triggers that aren't
    /// yet satisfied stay armed.
    pub fn on_task_change(&mut self, task: TaskId, world: &TriggerWorld<'_>) -> Vec<Action> {
        let Some(armed) = self.by_task.remove(&task) else {
            return Vec::new();
        };
        let (fired, still_armed): (Vec<_>, Vec<_>) =
            armed.into_iter().partition(|(t, _)| t.is_satisfied(world));
        if !still_armed.is_empty() {
            self.by_task.insert(task, still_armed);
        }
        fired.into_iter().map(|(_, action)| action).collect()
    }

    /// The logical clock advanced: evaluate tick-waiting triggers,
    /// returning + disarming the satisfied ones.
    pub fn on_tick(&mut self, world: &TriggerWorld<'_>) -> Vec<Action> {
        let armed = std::mem::take(&mut self.by_tick);
        let (fired, still_armed): (Vec<_>, Vec<_>) =
            armed.into_iter().partition(|(t, _)| t.is_satisfied(world));
        self.by_tick = still_armed;
        fired.into_iter().map(|(_, action)| action).collect()
    }

    /// Total triggers still armed (not yet fired).
    pub fn armed_count(&self) -> usize {
        self.by_task.values().map(Vec::len).sum::<usize>() + self.by_tick.len()
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
        // The non-matching branch stays armed (its condition may yet
        // hold under a different recorded result — though for a Done
        // task it won't; disarming is the caller's policy).
        assert_eq!(eng.armed_count(), 1);
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
