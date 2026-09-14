//! The leaf's one time source.
//!
//! **Why a module and not a bare `Instant::now()`.** On
//! `wasm32-unknown-unknown` `std::time::Instant::now()` compiles and
//! then panics at runtime ("time not implemented on this platform"),
//! and so does `SystemTime::now()` (S0a §Instant/Clock). A `cargo
//! check --target wasm32-unknown-unknown` cannot see it. The wire
//! crate answered that with [`net_wire::clock`]; the leaf reads the
//! clock in three more places — call deadlines, reassembly expiry and
//! the signalling envelope's `not_after` — so it routes all of them
//! through here, and the executed wasm test asserts a real reading.
//!
//! Nothing in this crate may call `std::time::Instant::now()`. The
//! only `Instant` in scope is the wire crate's target-selected alias.

use net_wire::clock::{Clock, Instant, SystemClock};

/// Monotonic reading: `std::time::Instant` natively,
/// `web_time::Instant` (i.e. `performance.now()`) on wasm32.
#[inline]
pub fn now() -> Instant {
    SystemClock::now()
}

/// Wall-clock reading in **seconds** since the Unix epoch.
///
/// The signalling envelope's `not_after` and the capability
/// announcement's TTL are both wall-clock; everything else in the
/// leaf is monotonic.
#[inline]
pub fn now_unix_secs() -> u64 {
    SystemClock::now_unix_nanos() / 1_000_000_000
}

/// Wall-clock reading in nanoseconds since the Unix epoch — the unit
/// `CapabilityAnnouncement::timestamp_ns` is stamped in.
#[inline]
pub fn now_unix_nanos() -> u64 {
    SystemClock::now_unix_nanos()
}

/// A monotonic deadline.
///
/// Copy, cheap, and comparable against a single [`now`] reading so a
/// sweep over many deadlines takes one clock read rather than one per
/// entry — which matters because the sweep runs on every inbound
/// packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Deadline(Instant);

impl Deadline {
    /// A deadline `after_ms` milliseconds from now.
    #[inline]
    pub fn in_ms(after_ms: u64) -> Self {
        Self(now() + core::time::Duration::from_millis(after_ms))
    }

    /// Has this deadline passed as of `at`?
    #[inline]
    pub fn expired_at(self, at: Instant) -> bool {
        at >= self.0
    }

    /// Milliseconds remaining as of `at`; `0` once expired.
    #[inline]
    pub fn remaining_ms_at(self, at: Instant) -> u64 {
        if at >= self.0 {
            0
        } else {
            u64::try_from(self.0.duration_since(at).as_millis()).unwrap_or(u64::MAX)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_deadline_is_not_expired_before_its_instant_and_is_after() {
        let d = Deadline::in_ms(50);
        let t0 = now();
        assert!(!d.expired_at(t0));
        assert!(d.remaining_ms_at(t0) > 0);

        let later = t0 + core::time::Duration::from_millis(100);
        assert!(d.expired_at(later));
        assert_eq!(d.remaining_ms_at(later), 0);
    }

    #[test]
    fn a_zero_deadline_is_expired_immediately() {
        let d = Deadline::in_ms(0);
        assert!(d.expired_at(now()));
    }

    #[test]
    fn the_wall_clock_returns_a_real_epoch_reading() {
        // Guards against a seam that silently returns 0 — which is
        // what a naive wasm shim would do, and what would make every
        // `not_after` window either always-expired or never-expired.
        assert!(now_unix_secs() > 1_600_000_000);
    }
}
