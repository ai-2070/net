//! `current_timestamp()` — copied from
//! `net/crates/net/src/adapter/net/mod.rs:281` (the `#[inline]`
//! coarse-clock helper plus its pure core `coarse_clock_advance`,
//! `mod.rs:338`) with both time reads routed through
//! [`crate::clock::Clock`].
//!
//! The original is `pub(crate)` in `adapter::net`; `session.rs` and
//! `pool.rs` call it on every packet, so it is part of the wire
//! surface whether or not §7 named it.

use std::cell::Cell;

use crate::clock::{Clock, Instant, SystemClock};

/// Refresh interval for the coarse clock (1 ms), copied from
/// `adapter/net/mod.rs`.
pub const COARSE_CLOCK_REFRESH_NS: u64 = 1_000_000;

/// Current timestamp in nanoseconds since the Unix epoch, cached per
/// thread for up to [`COARSE_CLOCK_REFRESH_NS`].
///
/// The `thread_local!` survives `wasm32-unknown-unknown` unchanged
/// (single-threaded there, but the TLS lowering is supported).
#[inline]
pub fn current_timestamp() -> u64 {
    thread_local! {
        static COARSE_CLOCK: Cell<Option<(Instant, u64)>> = const { Cell::new(None) };
    }
    COARSE_CLOCK.with(|cell| {
        let now_inst = SystemClock::now();
        let (store, ns) = coarse_clock_advance(cell.get(), now_inst, SystemClock::now_unix_nanos);
        if let Some(pair) = store {
            cell.set(Some(pair));
        }
        ns
    })
}

/// Pure core of the coarse clock: reuse the cached reading when it is
/// younger than [`COARSE_CLOCK_REFRESH_NS`], otherwise take a fresh
/// wall-clock reading and rebase the window.
#[inline]
fn coarse_clock_advance(
    cached: Option<(Instant, u64)>,
    now_inst: Instant,
    read_wall: impl FnOnce() -> u64,
) -> (Option<(Instant, u64)>, u64) {
    if let Some((last_inst, last_ns)) = cached {
        if now_inst.duration_since(last_inst).as_nanos() < COARSE_CLOCK_REFRESH_NS as u128 {
            return (None, last_ns);
        }
    }
    let ns = read_wall();
    (Some((now_inst, ns)), ns)
}
