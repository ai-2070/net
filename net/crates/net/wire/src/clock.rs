//! The `Clock` seam (Stage 2, proven in S0a).
//!
//! Every monotonic-time read in the extracted wire code goes through
//! this trait instead of calling `std::time::Instant::now()` directly.
//! Native builds keep `std::time::Instant`; `wasm32` builds use
//! `web_time::Instant`, which is `performance.now()` under the hood.
//!
//! `std::time::Instant::now()` does not merely mis-measure on
//! `wasm32-unknown-unknown` — it panics ("time not implemented on this
//! platform"), and so does `SystemTime::now()`. Both compile, which is
//! why `cargo check --target wasm32-unknown-unknown` alone would not
//! have caught the problem; the seam has to be explicit.

/// Monotonic instant type for the current target.
#[cfg(not(target_arch = "wasm32"))]
pub type Instant = std::time::Instant;

/// Monotonic instant type for the current target.
#[cfg(target_arch = "wasm32")]
pub type Instant = web_time::Instant;

/// Wall-clock type for the current target.
#[cfg(not(target_arch = "wasm32"))]
pub type SystemTime = std::time::SystemTime;

/// Wall-clock type for the current target.
#[cfg(target_arch = "wasm32")]
pub type SystemTime = web_time::SystemTime;

/// Unix epoch constant for [`SystemTime`].
#[cfg(not(target_arch = "wasm32"))]
pub const UNIX_EPOCH: SystemTime = std::time::UNIX_EPOCH;

/// Unix epoch constant for [`SystemTime`].
#[cfg(target_arch = "wasm32")]
pub const UNIX_EPOCH: SystemTime = web_time::UNIX_EPOCH;

/// The time source the wire modules read.
///
/// Kept object-unsafe-free deliberately: all call sites are static
/// (`SystemClock::now()`), so the seam costs nothing at runtime and
/// the wasm substitution is a type-level swap, not a vtable.
pub trait Clock {
    /// Monotonic reading, used for RTO/retransmit timing and the
    /// stream-close quarantine window.
    fn now() -> Instant;

    /// Wall-clock reading in nanoseconds since the Unix epoch, used
    /// for session/stream liveness stamps on the wire.
    fn now_unix_nanos() -> u64;
}

/// The platform clock: `std::time` natively, `web_time` on wasm32.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    #[inline]
    fn now() -> Instant {
        Instant::now()
    }

    #[inline]
    fn now_unix_nanos() -> u64 {
        let elapsed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX)
    }
}
