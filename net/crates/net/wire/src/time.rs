//! The coarse packet clock: `current_timestamp` and its pure core.
//!
//! Moved out of `net::adapter::net` (`mod.rs`) in Stage 2 with both
//! time reads routed through [`crate::clock::Clock`]: `session.rs`
//! and `pool.rs` call it on every packet, so it is wire surface. The
//! core re-exports all three items, and `current_timestamp_micros` —
//! a diagnostics helper no wire code calls — stayed behind.
//!
//! On `wasm32-unknown-unknown` `std::time::Instant::now()` and
//! `SystemTime::now()` compile and then panic at runtime, so both
//! reads here go through [`crate::clock::SystemClock`].

use std::cell::Cell;

use crate::clock::{Clock, Instant, SystemClock};

/// Threshold below which the cached coarse-clock reading is reused
/// instead of re-reading the OS wall clock. 1 ms is well below the
/// session-timeout / heartbeat / NACK cadence the consumers care
/// about (those tick on the seconds scale), and well above the
/// `Instant::now` cost (~10 ns) we still pay per call to gate the
/// cache. Per PERF_AUDIT §2.7.
pub const COARSE_CLOCK_REFRESH_NS: u64 = 1_000_000; // 1 ms

/// Current timestamp in nanoseconds since the Unix epoch.
///
/// Shared utility — avoids duplicating this across `causal.rs`, `snapshot.rs`,
/// `observation.rs`, `migration.rs`, `session.rs`, and `token.rs`.
///
/// Saturates via `try_from` so future-dated clocks land at
/// `u64::MAX` instead of wrapping near 0. A bare `as u64` would
/// silently truncate the `u128` returned by
/// `Duration::as_nanos()`. Practical wraparound from monotonic
/// flow doesn't happen until ~year 2554, but a system whose clock
/// was misconfigured to a far-future date would produce a tiny
/// truncated timestamp — immediately tripping `is_timed_out`
/// everywhere. `unwrap_or_default()` returning `Duration::ZERO`
/// for a pre-epoch clock would also produce identical timestamps
/// that break ordering.
///
/// Coarse-clock cached per thread at [`COARSE_CLOCK_REFRESH_NS`]
/// granularity (PERF_AUDIT §2.7) — readings may be up to 1 ms
/// stale, and two threads may disagree by up to that much.
/// Consumers doing timeout arithmetic MUST use `saturating_sub`
/// (they all do today) so a reader with a staler cache than the
/// toucher can't wrap and false-expire.
#[inline]
pub fn current_timestamp() -> u64 {
    // **PERF_AUDIT §2.7.** Per-packet RX/TX paths each call
    // `current_timestamp()` twice (one stream `touch` + one session
    // `touch`). On Windows `SystemTime::now()` is
    // `GetSystemTimePreciseAsFileTime` (~600 ns); on Linux it's
    // `clock_gettime(CLOCK_REALTIME)` (~120 ns). At sustained packet
    // rates the four wall-clock reads per ping eat measurable CPU.
    //
    // Coarse-clock cache: a `thread_local!` Cell holds the last
    // `(Instant, u64-ns)` pair. Each call asks `Instant::now()`
    // (~10 ns — TSC-backed on both Linux and Windows) whether 1 ms
    // has elapsed; if not, the cached `u64` is reused. Repeated
    // calls within the same millisecond from the same thread pay
    // one Instant comparison instead of one OS wall-clock syscall.
    //
    // Consumers (`session.is_timed_out` against multi-second
    // timeouts, `last_activity_ns` for diagnostics) are insensitive
    // to ≤ 1 ms drift; the wire envelopes that need absolute epoch
    // ns (capability announcements, snapshots) call
    // `current_timestamp_micros` or stamp `SystemTime::now()`
    // directly — neither hits this path.
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

/// Pure core of the §2.7 coarse clock: given the cached
/// `(instant, ns)` pair and the current `Instant`, decide whether
/// to reuse the cached reading (younger than
/// [`COARSE_CLOCK_REFRESH_NS`]) or call `read_wall` for a fresh
/// wall-clock value. Returns `(cache update, value)` — `None`
/// means a cache hit (nothing to store, keeping the hit path
/// store-free); `Some(pair)` rebases the refresh window on the
/// read instant.
///
/// Extracted from [`current_timestamp`] so the reuse/refresh
/// decision is testable with synthetic instants. The previous
/// test drove the real thread-local with a 100-read burst and
/// asserted every value matched — correct on an idle machine, but
/// a > 1 ms OS preemption mid-burst legitimately rolls the window,
/// so the assertion was probabilistic under CI load even with
/// retries.
#[inline]
pub fn coarse_clock_advance(
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
