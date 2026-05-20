//! PyO3 wrapper for the consumer-side Redis Streams dedup helper.
//!
//! Thin wrapper around `net_sdk::RedisStreamDedup`. See that
//! module's docs for the dedup contract — the Redis adapter writes
//! a `dedup_id` field on every XADD entry; this helper filters
//! duplicates by maintaining an LRU-bounded set of seen ids.
//!
//! Wire shape from Python:
//!
//! ```python
//! from net import RedisStreamDedup
//!
//! # Default capacity (4096).
//! dedup = RedisStreamDedup()
//!
//! # Explicit capacity for higher-throughput consumers.
//! dedup = RedisStreamDedup(capacity=65536)
//!
//! # Read entries from your Redis client of choice; pull the
//! # `dedup_id` field from each entry.
//! for entry in stream:
//!     if not dedup.is_duplicate(entry["dedup_id"]):
//!         await process(entry)
//! ```
//!
//! `RedisStreamDedup` is NOT thread-safe across Python threads.
//! Python users typically read a stream from a single async
//! context; if you need cross-thread dedup, instantiate one
//! helper per thread.

use parking_lot::Mutex;
use pyo3::prelude::*;

/// Consumer-side dedup helper for the Redis Streams adapter.
///
/// See `net::adapter::redis` module docs for the producer-side
/// contract that produces the `dedup_id` field this helper
/// filters on.
#[pyclass(name = "RedisStreamDedup")]
pub struct PyRedisStreamDedup {
    // The inner LRU is `!Sync` (it owns mutable state behind a
    // `&mut self` receiver). PyO3 exposes `&self` methods for
    // the Python-visible API, so we wrap in a `Mutex` to
    // serialize — which also matches the documented one-helper-
    // per-thread shape (no contention in the common case).
    inner: Mutex<net_sdk::RedisStreamDedup>,
}

#[pymethods]
impl PyRedisStreamDedup {
    /// Create a helper. `capacity` defaults to 4096 if omitted;
    /// `0` is clamped to `1`.
    ///
    /// Sizing: a consumer at ~10k events/sec with a 1 min
    /// dedup window should pick ~600,000.
    #[new]
    #[pyo3(signature = (capacity=None))]
    fn new(capacity: Option<usize>) -> Self {
        let inner = match capacity {
            Some(c) => net_sdk::RedisStreamDedup::with_capacity(c),
            None => net_sdk::RedisStreamDedup::new(),
        };
        Self {
            inner: Mutex::new(inner),
        }
    }

    /// Test-and-insert: returns `True` if the caller should treat
    /// the entry as a DUPLICATE (skip it), `False` if it's the
    /// first time we've seen this `dedup_id`.
    ///
    /// Maps to the Rust `is_duplicate(&mut self, &str) -> bool`.
    ///
    /// Releases the GIL via `Python::detach` while the inner
    /// mutex is held — under multi-thread contention this lets
    /// other Python threads keep running while the lookup/insert
    /// happens on the Rust side. The closure body is pure Rust
    /// + a borrowed `&str`, so no Python state is touched while
    /// the GIL is released.
    fn is_duplicate(&self, py: Python<'_>, dedup_id: &str) -> bool {
        py.detach(|| {
            let mut guard = self.inner.lock();
            guard.is_duplicate(dedup_id)
        })
    }

    /// Number of distinct ids currently tracked.
    #[getter]
    fn len(&self) -> usize {
        let guard = self.inner.lock();
        guard.len()
    }

    /// Configured maximum capacity.
    #[getter]
    fn capacity(&self) -> usize {
        let guard = self.inner.lock();
        guard.capacity()
    }

    /// True if no ids are tracked yet.
    #[getter]
    fn is_empty(&self) -> bool {
        let guard = self.inner.lock();
        guard.is_empty()
    }

    /// Clear all tracked ids. Use after a consumer-group
    /// rebalance to reset the dedup window without losing the
    /// helper instance.
    ///
    /// Releases the GIL — clear() can drop up to `capacity` heap
    /// allocations (the `Arc<str>` ids), which for a 600K-capacity
    /// helper means ~600K `Drop`s on the caller's thread. No
    /// reason to hold the GIL during that work.
    fn clear(&self, py: Python<'_>) {
        py.detach(|| {
            let mut guard = self.inner.lock();
            guard.clear();
        })
    }

    fn __repr__(&self) -> String {
        let guard = self.inner.lock();
        format!(
            "RedisStreamDedup(len={}, capacity={})",
            guard.len(),
            guard.capacity(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Smoke test: PyO3 surface answers the same way as the Rust
    /// helper for the canonical producer-retry scenario.
    ///
    /// CR-35 wraps the calls in `Python::attach` because
    /// `is_duplicate` and `clear` now take a `Python<'_>` token
    /// (used to release the GIL via `py.detach(|| ...)` while the
    /// inner mutex is held).
    #[test]
    fn pyo3_helper_filters_duplicates() {
        Python::attach(|py| {
            let dedup = PyRedisStreamDedup::new(Some(64));

            // First pass: every id is new.
            for i in 0..3 {
                let id = format!("deadbeef:0:0:{i}");
                assert!(!dedup.is_duplicate(py, &id));
            }
            assert_eq!(dedup.len(), 3);
            assert!(!dedup.is_empty());

            // Retry path: every id reappears with the same dedup_id.
            for i in 0..3 {
                let id = format!("deadbeef:0:0:{i}");
                assert!(dedup.is_duplicate(py, &id));
            }
            assert_eq!(dedup.len(), 3); // length unchanged on duplicate hits

            dedup.clear(py);
            assert_eq!(dedup.len(), 0);
            assert!(dedup.is_empty());
        })
    }

    #[test]
    fn pyo3_helper_default_capacity() {
        let dedup = PyRedisStreamDedup::new(None);
        assert_eq!(dedup.capacity(), 4096);
    }

    #[test]
    fn pyo3_helper_explicit_capacity() {
        let dedup = PyRedisStreamDedup::new(Some(8192));
        assert_eq!(dedup.capacity(), 8192);
    }

    #[test]
    fn pyo3_helper_capacity_zero_is_clamped() {
        let dedup = PyRedisStreamDedup::new(Some(0));
        assert_eq!(dedup.capacity(), 1);
    }

    /// CR-35: pin that the hot paths (`is_duplicate`, `clear`)
    /// release the GIL via `py.detach(|| ...)`. Pre-CR-35 these
    /// methods held the GIL across the inner mutex acquire +
    /// hash work, serializing the asyncio event loop on every
    /// dedup query under multi-thread contention. The fix wraps
    /// the mutex work in `py.detach` so other Python threads can
    /// run concurrently.
    ///
    /// We can't directly assert "the GIL is released" without
    /// instrumenting Python's threading state, so we use a
    /// source-level tripwire — same pattern as CR-12, CR-21,
    /// CR-32. The fixed forbidden shape is "fn is_duplicate / fn
    /// clear without `py.detach`."
    #[test]
    fn cr35_hot_paths_release_gil_via_py_detach() {
        let src = include_str!("redis_dedup.rs");

        // Find each hot-path method definition and assert the
        // body contains `py.detach(`. Built at runtime so the
        // test's source doesn't trigger the scan.
        let detach_token = format!("py{}{}(", ".", "detach");

        for method in &["is_duplicate", "clear"] {
            let needle = format!("fn {}(&self, py: Python", method);
            let idx = src.find(&needle).unwrap_or_else(|| {
                panic!(
                    "CR-35 regression: `{}` no longer takes `py: Python<'_>` — \
                     the GIL-release pattern requires this signature so the \
                     #[pymethods] macro injects the GIL token at the call site",
                    method
                )
            });
            // Look ahead in the body for the detach invocation.
            let body: String = src[idx..].lines().take(15).collect::<Vec<_>>().join("\n");
            assert!(
                body.contains(&detach_token),
                "CR-35 regression: `{}` does not call `py.detach(...)`. \
                 The hot path must release the GIL while holding the inner \
                 mutex. Method body:\n{}",
                method,
                body
            );
        }
    }
}
