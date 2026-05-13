//! Phase B — supervisor primitives. The `BackoffTracker` is the
//! per-daemon decision surface for restart gating: it records
//! crash timestamps inside a rolling window, doubles the backoff
//! window on each crash up to a cap, and flips to a
//! `CrashLooping { until }` state once the rolling-window crash
//! count crosses the threshold. Pure-sync; no I/O; no allocs
//! beyond the crash-history deque.
//!
//! Reconcile reads each daemon's `RestartState` to decide
//! whether `StartDaemon` is admissible (only when `Idle`); the
//! supervisor's event-fold side records the crash via
//! `BackoffTracker::observe_crash` when a `DaemonLifecycleSignal::Crashed`
//! arrives. Clean exits reset the tracker via
//! `observe_clean_exit`.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// Tunables for the backoff machine. `Default::default()`
/// reproduces the plan's numbers: 500 ms initial, doubling, 60 s
/// cap; 5 crashes per 60 s window flips to `CrashLooping`;
/// 5-minute cooldown before the gate releases.
#[derive(Clone, Debug)]
pub struct BackoffConfig {
    /// Backoff window after the first crash. Default 500 ms.
    pub initial: Duration,
    /// Multiplicative factor applied to the window on each
    /// subsequent crash. Default 2.0.
    pub factor: f32,
    /// Cap on the backoff window. Default 60 s.
    pub max: Duration,
    /// A crash-rate above N crashes per
    /// [`crash_loop_window`](Self::crash_loop_window) flips the
    /// daemon to `CrashLooping`. Default 5.
    pub crash_loop_threshold: u32,
    /// Rolling window used to count crashes for the crash-loop
    /// gate. Default 60 s.
    pub crash_loop_window: Duration,
    /// Cooldown applied once the crash-loop gate trips. Default
    /// 5 min.
    pub crash_loop_cooldown: Duration,
    /// How long after a successful run before the backoff
    /// window resets to [`initial`](Self::initial). Default
    /// 60 s — same as `crash_loop_window` so a daemon that
    /// stays alive for one window earns a fresh slate.
    pub stable_run_threshold: Duration,
}

impl Default for BackoffConfig {
    fn default() -> Self {
        Self {
            initial: Duration::from_millis(500),
            factor: 2.0,
            max: Duration::from_secs(60),
            crash_loop_threshold: 5,
            crash_loop_window: Duration::from_secs(60),
            crash_loop_cooldown: Duration::from_secs(5 * 60),
            stable_run_threshold: Duration::from_secs(60),
        }
    }
}

/// Per-daemon restart gate. Reconcile reads this to decide
/// whether a `StartDaemon` action is admissible.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum RestartState {
    /// No backoff in effect. `StartDaemon` is admissible if
    /// desired-state asks for it and the daemon is currently
    /// stopped.
    Idle,
    /// Backoff window active. `StartDaemon` is gated until
    /// `until`; reconcile emits `ApplyBackoff` to record the
    /// gate on the snapshot fold.
    BackingOff {
        /// Earliest instant a restart may be admitted.
        until: Instant,
    },
    /// Crash-loop gate tripped. `StartDaemon` is gated until
    /// `until`, after which the gate flips back to `Idle` and
    /// the next crash starts the rolling-window count fresh.
    CrashLooping {
        /// Earliest instant the crash-loop gate releases.
        until: Instant,
    },
}

impl RestartState {
    /// `true` when the gate is open (no backoff or crash-loop
    /// hold active relative to `now`).
    pub fn is_admissible(&self, now: Instant) -> bool {
        match self {
            RestartState::Idle => true,
            RestartState::BackingOff { until } | RestartState::CrashLooping { until } => {
                *until <= now
            }
        }
    }

    /// The instant the gate releases, or `None` for `Idle`.
    pub fn release_at(&self) -> Option<Instant> {
        match self {
            RestartState::Idle => None,
            RestartState::BackingOff { until } | RestartState::CrashLooping { until } => {
                Some(*until)
            }
        }
    }
}

/// Per-daemon backoff bookkeeping. Holds the rolling crash
/// window + the current backoff duration + the gate state.
///
/// All methods are sync; no allocation beyond the crash-history
/// deque, which is bounded by
/// [`BackoffConfig::crash_loop_threshold`].
#[derive(Clone, Debug)]
pub struct BackoffTracker {
    config: BackoffConfig,
    crash_history: VecDeque<Instant>,
    next_backoff: Duration,
    /// Last `observe_start` timestamp, if any. Used to detect
    /// "stable run" → reset the backoff window.
    last_started: Option<Instant>,
    state: RestartState,
}

impl Default for BackoffTracker {
    fn default() -> Self {
        Self::new(BackoffConfig::default())
    }
}

impl BackoffTracker {
    /// Build a tracker with the given config. Starts in `Idle`
    /// with the initial backoff window.
    pub fn new(config: BackoffConfig) -> Self {
        Self {
            next_backoff: config.initial,
            crash_history: VecDeque::with_capacity(config.crash_loop_threshold as usize),
            last_started: None,
            state: RestartState::Idle,
            config,
        }
    }

    /// Current gate state.
    pub fn state(&self) -> RestartState {
        self.state
    }

    /// Record that the daemon started successfully. The backoff
    /// window resets to `initial` if the previous start lasted
    /// at least `stable_run_threshold` (the "stable run earns a
    /// fresh slate" rule).
    pub fn observe_start(&mut self, now: Instant) {
        if let Some(last) = self.last_started {
            if now.saturating_duration_since(last) >= self.config.stable_run_threshold {
                self.next_backoff = self.config.initial;
                self.crash_history.clear();
            }
        }
        self.last_started = Some(now);
        self.state = RestartState::Idle;
    }

    /// Record a clean exit. Mirrors the "successful run resets
    /// the window" behavior of `observe_start` for daemons that
    /// exit on their own terms after a long run.
    pub fn observe_clean_exit(&mut self, now: Instant) {
        if let Some(last) = self.last_started {
            if now.saturating_duration_since(last) >= self.config.stable_run_threshold {
                self.next_backoff = self.config.initial;
                self.crash_history.clear();
            }
        }
        self.state = RestartState::Idle;
    }

    /// Record a crash. Advances the gate: BackingOff for the
    /// current window, or CrashLooping if the rolling-window
    /// count crosses the threshold.
    pub fn observe_crash(&mut self, now: Instant) {
        // Slide the crash window forward.
        let cutoff = now
            .checked_sub(self.config.crash_loop_window)
            .unwrap_or(now);
        while self
            .crash_history
            .front()
            .copied()
            .is_some_and(|t| t < cutoff)
        {
            self.crash_history.pop_front();
        }
        self.crash_history.push_back(now);

        // Crash-loop gate first — has priority over BackingOff
        // since a daemon flipping the threshold deserves the
        // longer cooldown.
        if self.crash_history.len() as u32 >= self.config.crash_loop_threshold {
            self.state = RestartState::CrashLooping {
                until: now
                    .checked_add(self.config.crash_loop_cooldown)
                    .unwrap_or(now),
            };
            // Reset the per-restart backoff window so when the
            // cooldown elapses the daemon gets the initial
            // window again, not the maxed-out one.
            self.next_backoff = self.config.initial;
            return;
        }

        // Otherwise, advance to BackingOff with the current
        // window, then double it (capped at `max`) for the next
        // crash.
        let until = now.checked_add(self.next_backoff).unwrap_or(now);
        self.state = RestartState::BackingOff { until };
        let doubled = self
            .next_backoff
            .as_secs_f64()
            .max(self.config.initial.as_secs_f64())
            * self.config.factor as f64;
        let doubled = Duration::from_secs_f64(doubled.min(self.config.max.as_secs_f64()));
        self.next_backoff = doubled;
    }

    /// Called periodically (every Tick) to release a gate whose
    /// `until` has elapsed. Returns `true` when the state
    /// transitioned to `Idle` on this call.
    pub fn maybe_release(&mut self, now: Instant) -> bool {
        if !self.state.is_admissible(now) {
            return false;
        }
        if matches!(self.state, RestartState::Idle) {
            return false;
        }
        self.state = RestartState::Idle;
        true
    }

    /// Test-only accessor for the current backoff window.
    #[cfg(test)]
    pub(crate) fn current_window(&self) -> Duration {
        self.next_backoff
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(secs: u64) -> Instant {
        // Anchored to a single base so test arithmetic is stable
        // (Instant::now() drifts between calls).
        static BASE: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
        let base = *BASE.get_or_init(Instant::now);
        base + Duration::from_secs(secs)
    }

    #[test]
    fn fresh_tracker_is_idle_and_admissible() {
        let bt = BackoffTracker::default();
        assert_eq!(bt.state(), RestartState::Idle);
        assert!(bt.state().is_admissible(t(0)));
    }

    #[test]
    fn first_crash_flips_to_backing_off_with_initial_window() {
        let mut bt = BackoffTracker::default();
        bt.observe_crash(t(0));
        match bt.state() {
            RestartState::BackingOff { until } => {
                assert_eq!(until, t(0) + Duration::from_millis(500));
            }
            other => panic!("expected BackingOff(500ms), got {other:?}"),
        }
    }

    #[test]
    fn consecutive_crashes_double_the_backoff_up_to_the_cap() {
        let mut bt = BackoffTracker::default();
        // Below the crash-loop threshold (5): 4 crashes only.
        bt.observe_crash(t(0));
        bt.observe_crash(t(1));
        bt.observe_crash(t(2));
        bt.observe_crash(t(3));
        // After 4 crashes the next backoff would be
        // 500 → 1s → 2s → 4s → 8s — but we look at `next_backoff`
        // which has already been doubled past the last observe.
        // The fourth observe set state to BackingOff(4s after t(3)).
        match bt.state() {
            RestartState::BackingOff { until } => {
                assert_eq!(until, t(3) + Duration::from_secs(4));
            }
            other => panic!("expected BackingOff after 4 crashes, got {other:?}"),
        }
    }

    #[test]
    fn fifth_crash_within_window_flips_to_crash_looping() {
        let mut bt = BackoffTracker::default();
        for i in 0..5 {
            bt.observe_crash(t(i));
        }
        match bt.state() {
            RestartState::CrashLooping { until } => {
                assert_eq!(until, t(4) + Duration::from_secs(5 * 60));
            }
            other => panic!("expected CrashLooping after 5 crashes, got {other:?}"),
        }
    }

    #[test]
    fn crash_loop_count_slides_with_the_window() {
        let mut bt = BackoffTracker::default();
        // 4 crashes very close together.
        for i in 0..4 {
            bt.observe_crash(t(i));
        }
        // Wait past the crash_loop_window (60 s); a fresh crash
        // should NOT flip to CrashLooping because the prior 4 are
        // out of the rolling window.
        bt.observe_crash(t(120));
        assert!(
            matches!(bt.state(), RestartState::BackingOff { .. }),
            "got {:?}",
            bt.state(),
        );
    }

    #[test]
    fn observe_start_after_stable_run_resets_the_window() {
        let mut bt = BackoffTracker::default();
        bt.observe_crash(t(0));
        bt.observe_start(t(1));
        // Daemon now ran for 120 s (well past the 60 s stable
        // threshold). The next observe_start should reset
        // next_backoff to initial.
        bt.observe_start(t(121));
        assert_eq!(bt.current_window(), Duration::from_millis(500));
    }

    #[test]
    fn observe_start_before_stable_run_keeps_the_doubled_window() {
        let mut bt = BackoffTracker::default();
        bt.observe_crash(t(0));
        // Initial window after first crash advanced
        // next_backoff to 1 s.
        assert_eq!(bt.current_window(), Duration::from_secs(1));
        bt.observe_start(t(1));
        bt.observe_start(t(5)); // 4 s, well under the 60 s threshold
        assert_eq!(bt.current_window(), Duration::from_secs(1));
    }

    #[test]
    fn maybe_release_flips_backing_off_to_idle_after_until_elapses() {
        let mut bt = BackoffTracker::default();
        bt.observe_crash(t(0));
        // Still gated immediately after the crash.
        assert!(!bt.maybe_release(t(0)));
        assert!(matches!(bt.state(), RestartState::BackingOff { .. }));
        // After the 500 ms window, release.
        let released = bt.maybe_release(t(0) + Duration::from_millis(600));
        assert!(released);
        assert_eq!(bt.state(), RestartState::Idle);
    }

    #[test]
    fn admissibility_predicate_honors_until_boundary() {
        let mut bt = BackoffTracker::default();
        bt.observe_crash(t(0));
        let until = match bt.state() {
            RestartState::BackingOff { until } => until,
            _ => unreachable!(),
        };
        assert!(!bt.state().is_admissible(until - Duration::from_millis(1)));
        assert!(bt.state().is_admissible(until));
    }

    #[test]
    fn backoff_caps_at_max() {
        let mut bt = BackoffTracker::default();
        // Many crashes far apart so the crash-loop gate doesn't
        // trip (each crash falls outside the 60 s rolling window).
        for i in 0..15 {
            bt.observe_crash(t(i * 200));
        }
        // After 15 doublings (500ms → 1s → 2s → ... ) the
        // window should be capped at 60 s.
        assert_eq!(bt.current_window(), Duration::from_secs(60));
    }
}
