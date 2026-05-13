//! Phase B — [`MeshOsControl`]. The side-channel event a
//! supervised daemon receives from MeshOS. The daemon's normal
//! `process()` path stays sync (WASM compatibility); the
//! control channel is a separate async receive so an SDK-side
//! `DaemonHandle::receive_control().await` can park without
//! blocking the daemon's event-driven core.
//!
//! Phase B defines the enum + its constructors. The
//! per-daemon mpsc fan-out (one `Sender<MeshOsControl>` per
//! supervised daemon, owned by the supervisor; one `Receiver`
//! handed to the SDK) lands once the supervisor integration
//! layer attaches to the existing `DaemonRegistry`.

use std::time::Instant;

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
}
