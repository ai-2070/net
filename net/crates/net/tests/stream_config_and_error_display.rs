//! Coverage for the small, exported surface in
//! `src/adapter/net/stream.rs` flagged by the Codecov report
//! (the file sat at 55.56% — half the file is the builder +
//! `Display` impl, which never had a direct test). All checks
//! here are pure data — no async runtime, no network.
//!
//! Targets:
//!   * `StreamConfig::with_fairness_weight` clamping behavior
//!     (`weight.max(1)` at stream.rs:124) — 0 must clamp to 1.
//!   * `StreamConfig::with_close_behavior` — round-trips the
//!     `CloseBehavior` value (stream.rs:129).
//!   * `Display for StreamError` — exact message strings for all
//!     three variants (stream.rs:151-156). Operator log scrapers
//!     and downstream error renderers depend on these literals.

#![cfg(feature = "net")]

use net::adapter::net::{CloseBehavior, StreamConfig, StreamError};

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
fn fairness_weight_one_passes_through() {
    let cfg = StreamConfig::new().with_fairness_weight(1);
    assert_eq!(cfg.fairness_weight, 1);
}

#[test]
fn fairness_weight_above_one_passes_through() {
    let cfg = StreamConfig::new().with_fairness_weight(7);
    assert_eq!(cfg.fairness_weight, 7);
    let max = StreamConfig::new().with_fairness_weight(u8::MAX);
    assert_eq!(max.fairness_weight, u8::MAX);
}

#[test]
fn with_close_behavior_round_trips_drain_then_close() {
    let cfg = StreamConfig::new().with_close_behavior(CloseBehavior::DrainThenClose);
    assert_eq!(cfg.close_behavior, CloseBehavior::DrainThenClose);
}

#[test]
fn with_close_behavior_round_trips_drop_and_close() {
    let cfg = StreamConfig::new().with_close_behavior(CloseBehavior::DropAndClose);
    assert_eq!(cfg.close_behavior, CloseBehavior::DropAndClose);
}

#[test]
fn display_backpressure_message_is_stable() {
    // Operator log scrapers grep for these strings. The
    // assertion pins the exact message so a "polish" PR that
    // rewords them surfaces here, not as a silent grep miss in
    // an operator's alerting rules.
    assert_eq!(
        format!("{}", StreamError::Backpressure),
        "stream would block (queue full)"
    );
}

#[test]
fn display_not_connected_message_is_stable() {
    assert_eq!(
        format!("{}", StreamError::NotConnected),
        "stream not connected"
    );
}

#[test]
fn display_transport_message_includes_wrapped_string() {
    let inner = "ChaCha20-Poly1305 decryption failed";
    assert_eq!(
        format!("{}", StreamError::Transport(inner.into())),
        format!("stream transport error: {}", inner)
    );
}

#[test]
fn stream_error_implements_std_error() {
    // `StreamError: std::error::Error` so callers can `?` it
    // through `Box<dyn Error>` and the `source()` chain stays
    // walkable. The `impl std::error::Error for StreamError {}`
    // at stream.rs:159 has no body — this assertion at least
    // proves the trait bound holds.
    fn assert_error<E: std::error::Error>(_: &E) {}
    assert_error(&StreamError::Backpressure);
}
