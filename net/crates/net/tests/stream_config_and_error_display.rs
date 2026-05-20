//! Invariant tests for `src/adapter/net/stream.rs`:
//!
//!   * `StreamConfig::with_fairness_weight` clamps `0` to `1` —
//!     pins the production starvation prevention so a future
//!     "let zero through" refactor surfaces here, not as a
//!     real starvation bug.
//!   * `StreamError` implements `std::error::Error` so callers can
//!     `?` it through `Box<dyn Error>` and the `source()` chain
//!     stays walkable.

#![cfg(feature = "net")]

use net::adapter::net::{StreamConfig, StreamError};

#[test]
fn fairness_weight_zero_clamps_to_one() {
    // 0 would starve the stream under the fair scheduler. The
    // builder clamps to 1 — pin the boundary explicitly so a
    // future "let zero through" refactor surfaces here, not as a
    // production starvation bug.
    let cfg = StreamConfig::new().with_fairness_weight(0);
    assert_eq!(cfg.fairness_weight, 1, "weight=0 must clamp to 1");
}

#[test]
fn stream_error_implements_std_error() {
    fn assert_error<E: std::error::Error>(_: &E) {}
    assert_error(&StreamError::Backpressure);
}
