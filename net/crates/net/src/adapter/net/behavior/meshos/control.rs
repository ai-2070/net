//! Phase B — [`MeshOsControl`]. The side-channel event a
//! supervised daemon receives from MeshOS, in the SDK's
//! richer form (Instant-based deadlines). The supervisor
//! integration layer converts to the WASM-friendly
//! [`crate::adapter::net::compute::DaemonControl`]
//! before invoking the daemon's `on_control` method.
//!
//! Two parallel forms intentionally:
//!
//! - `MeshOsControl` (this module): SDK-internal scheduling.
//!   Carries `Instant` deadlines so the loop's admit /
//!   stabilization layers can compare against `now` without
//!   re-computing the wall clock.
//! - `DaemonControl` (in `compute::daemon`, always available):
//!   what a `MeshDaemon::on_control` receiver sees. Carries
//!   relative-millisecond fields so a daemon running in any
//!   clock domain (including WASM) can react without an
//!   `Instant` reference.

use std::time::Instant;

use crate::adapter::net::compute::DaemonControl;

/// Supervisor → daemon control event. Delivered via the
/// per-daemon control channel (separate from the `process()`
/// event stream). The daemon implements `on_control(&mut self,
/// event: MeshOsControl)` (default impl ignores everything).
///
/// `#[non_exhaustive]` so later phases add control variants
/// without breaking implementors.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum MeshOsControl {
    /// Graceful shutdown. The daemon should finish in-flight
    /// work and exit before `deadline`. Past the deadline the
    /// supervisor force-terminates.
    Shutdown {
        /// Wall-clock deadline by which the daemon should be
        /// gone. Past this, the supervisor force-terminates.
        deadline: Instant,
    },

    /// Drain start. Stop accepting new work; in-flight work
    /// continues until [`MeshOsControl::DrainFinish`] arrives or
    /// the deadline elapses.
    DrainStart {
        /// Deadline by which drain should be complete.
        deadline: Instant,
    },

    /// Drain done. All in-flight work that's still running may
    /// be abandoned; the daemon should exit.
    DrainFinish,

    /// Cluster-wide backpressure is active. The daemon should
    /// reduce optional work (cache warmup, background indexing,
    /// etc.) by roughly `level` ∈ `[0.0, 1.0]` — 1.0 means
    /// "pause optional work entirely".
    BackpressureOn {
        /// Severity in `[0.0, 1.0]`. 0 means just-barely
        /// triggered, 1 means catastrophic queue depth.
        level: f32,
    },

    /// Cluster-wide backpressure cleared. Resume normal work.
    BackpressureOff,
}

impl MeshOsControl {
    /// Convert this SDK-internal event to the WASM-friendly
    /// [`DaemonControl`] form the daemon's `on_control` method
    /// receives. `now` anchors the relative-ms conversion of
    /// the `Instant` deadlines; deadlines in the past clamp to
    /// `0`.
    pub fn to_daemon_control(&self, now: Instant) -> DaemonControl {
        match self {
            MeshOsControl::Shutdown { deadline } => DaemonControl::Shutdown {
                grace_period_ms: deadline.saturating_duration_since(now).as_millis() as u64,
            },
            MeshOsControl::DrainStart { deadline } => DaemonControl::DrainStart {
                grace_period_ms: deadline.saturating_duration_since(now).as_millis() as u64,
            },
            MeshOsControl::DrainFinish => DaemonControl::DrainFinish,
            MeshOsControl::BackpressureOn { level } => DaemonControl::BackpressureOn {
                level: *level,
            },
            MeshOsControl::BackpressureOff => DaemonControl::BackpressureOff,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn enum_variants_are_constructible_with_runtime_values() {
        let now = Instant::now();
        let _ = MeshOsControl::Shutdown {
            deadline: now + Duration::from_secs(5),
        };
        let _ = MeshOsControl::DrainStart {
            deadline: now + Duration::from_secs(30),
        };
        let _ = MeshOsControl::DrainFinish;
        let _ = MeshOsControl::BackpressureOn { level: 0.5 };
        let _ = MeshOsControl::BackpressureOff;
    }

    #[test]
    fn to_daemon_control_converts_instant_deadlines_to_relative_ms() {
        let now = Instant::now();
        let ev = MeshOsControl::Shutdown {
            deadline: now + Duration::from_millis(2500),
        };
        match ev.to_daemon_control(now) {
            DaemonControl::Shutdown { grace_period_ms } => {
                // Allow small slop for the Instant arithmetic.
                assert!((2400..=2500).contains(&grace_period_ms));
            }
            other => panic!("expected Shutdown, got {other:?}"),
        }
    }

    #[test]
    fn to_daemon_control_clamps_past_deadlines_to_zero() {
        let now = Instant::now();
        let ev = MeshOsControl::DrainStart {
            deadline: now - Duration::from_secs(1),
        };
        match ev.to_daemon_control(now) {
            DaemonControl::DrainStart { grace_period_ms } => {
                assert_eq!(grace_period_ms, 0);
            }
            other => panic!("expected DrainStart, got {other:?}"),
        }
    }

    #[test]
    fn to_daemon_control_passes_backpressure_level_through_unchanged() {
        let now = Instant::now();
        let ev = MeshOsControl::BackpressureOn { level: 0.75 };
        match ev.to_daemon_control(now) {
            DaemonControl::BackpressureOn { level } => {
                assert!((level - 0.75).abs() < 1e-6);
            }
            other => panic!("expected BackpressureOn, got {other:?}"),
        }
    }

    #[test]
    fn to_daemon_control_passes_drain_finish_and_backpressure_off_through() {
        let now = Instant::now();
        assert!(matches!(
            MeshOsControl::DrainFinish.to_daemon_control(now),
            DaemonControl::DrainFinish
        ));
        assert!(matches!(
            MeshOsControl::BackpressureOff.to_daemon_control(now),
            DaemonControl::BackpressureOff
        ));
    }
}
