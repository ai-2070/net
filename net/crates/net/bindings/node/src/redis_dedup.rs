//! NAPI wrapper for the consumer-side Redis Streams dedup helper.
//!
//! Thin wrapper around `net_sdk::RedisStreamDedup`. See that
//! module's docs for the dedup contract — briefly: the Redis
//! adapter writes a `dedup_id` field on every XADD entry; this
//! helper filters duplicates by maintaining an LRU-bounded set of
//! seen ids.
//!
//! Wire shape from the JS side. Users can import either from
//! `@net-mesh/core` (the NAPI module directly) or from
//! `@net-mesh/sdk` (which re-exports the same class via
//! `sdk-ts/src/redis-dedup.ts`):
//!
//! ```js
//! // From the NAPI module directly:
//! const { RedisStreamDedup } = require('@net-mesh/core');
//! // Or via the TS SDK package:
//! const { RedisStreamDedup } = require('@net-mesh/sdk');
//!
//! // Default capacity (4096).
//! const dedup = new RedisStreamDedup();
//!
//! // Explicit capacity for higher-throughput consumers.
//! const dedup = new RedisStreamDedup(65_536);
//!
//! // Read entries from your Redis client of choice; pull the
//! // dedup_id field from each entry.
//! for (const entry of stream) {
//!   if (!dedup.isDuplicate(entry.fields.dedup_id)) {
//!     await process(entry);
//!   }
//! }
//! ```
//!
//! `RedisStreamDedup` is `Send + Sync`; concurrent `isDuplicate`
//! calls from multiple NAPI worker threads serialize safely on
//! the underlying mutex. Production-shape is still one helper per
//! consumer thread (each consuming a disjoint partition) to avoid
//! the contention; sharing one handle is supported but costs lock
//! time on the hot path.
//!
//! NAPI doesn't have a GIL — Node's JS thread is single-threaded
//! and NAPI worker threads (libuv) don't share an interpreter
//! lock with it. The PyO3 binding
//! (`bindings/python/src/redis_dedup.rs`) releases the GIL via
//! `Python::detach` because the GIL serializes all Python threads
//! through the interpreter; that's the right move there. Here,
//! the only contention concern is the inner Rust mutex, which a
//! `Python::detach`-equivalent wouldn't help with. For the
//! cross-worker shared-handle case the answer is "use one handle
//! per worker" — the documented production shape.

#![allow(dead_code)]

use napi_derive::napi;
use parking_lot::Mutex;

/// Consumer-side dedup helper for the Redis Streams adapter.
///
/// See `net::adapter::redis` module docs for the producer-side
/// contract that produces the `dedup_id` field this helper
/// filters on.
#[napi]
pub struct RedisStreamDedup {
    // The inner LRU is `!Sync` (it owns mutable state behind a
    // `&mut self` receiver). NAPI exposes `&self` methods for
    // the JS-visible API, so we wrap in a `Mutex` to serialize
    // — which also matches the documented one-helper-per-worker
    // shape (no contention in the common case).
    inner: Mutex<net_sdk::RedisStreamDedup>,
}

#[napi]
impl RedisStreamDedup {
    /// Create a helper with the given LRU capacity. Defaults to
    /// 4096 if omitted. `0` is clamped to 1.
    ///
    /// Sizing: a consumer at ~10k events/sec with a 1 min
    /// dedup window should pick ~600k.
    #[napi(constructor)]
    pub fn new(capacity: Option<u32>) -> Self {
        let inner = match capacity {
            Some(c) => net_sdk::RedisStreamDedup::with_capacity(c as usize),
            None => net_sdk::RedisStreamDedup::new(),
        };
        Self {
            inner: Mutex::new(inner),
        }
    }

    /// Test-and-insert: returns `true` if the caller should treat
    /// the entry as a DUPLICATE (skip it), `false` if it's the
    /// first time we've seen this `dedupId`.
    ///
    /// Matches the Rust `is_duplicate(&mut self, &str) -> bool`.
    #[napi]
    pub fn is_duplicate(&self, dedup_id: String) -> bool {
        // Mutex poisoning would only happen if a previous thread
        // panicked while holding the lock; in that case the
        // helper's state is unknown but the LRU semantics are
        // still safe to continue from. Recover the inner.
        let mut guard = self.inner.lock();
        guard.is_duplicate(&dedup_id)
    }

    /// Number of distinct ids currently tracked.
    #[napi(getter)]
    pub fn len(&self) -> u32 {
        let guard = self.inner.lock();
        // `as u32` is fine: capacity is bounded by the constructor
        // argument which we accept as `u32`, so `len()` can never
        // exceed it.
        guard.len() as u32
    }

    /// Configured maximum capacity.
    #[napi(getter)]
    pub fn capacity(&self) -> u32 {
        let guard = self.inner.lock();
        guard.capacity() as u32
    }

    /// True if no ids are tracked yet.
    #[napi(getter)]
    pub fn is_empty(&self) -> bool {
        let guard = self.inner.lock();
        guard.is_empty()
    }

    /// Clear all tracked ids. Use after a consumer-group
    /// rebalance to reset the dedup window without losing the
    /// helper instance.
    #[napi]
    pub fn clear(&self) {
        let mut guard = self.inner.lock();
        guard.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Smoke test: NAPI surface answers the same way as the Rust
    /// helper for the canonical producer-retry scenario.
    /// Pre-fix consumers had no way to filter — the duplicate
    /// XADDs reached the application; the helper makes the filter
    /// trivial.
    #[test]
    fn napi_helper_filters_duplicates() {
        let dedup = RedisStreamDedup::new(Some(64));

        // First pass: every id is new.
        for i in 0..3 {
            let id = format!("deadbeef:0:0:{i}");
            assert!(!dedup.is_duplicate(id));
        }
        assert_eq!(dedup.len(), 3);
        assert!(!dedup.is_empty());

        // Retry path: every id reappears with the same dedup_id.
        for i in 0..3 {
            let id = format!("deadbeef:0:0:{i}");
            assert!(dedup.is_duplicate(id));
        }
        assert_eq!(dedup.len(), 3); // length unchanged on duplicate hits

        dedup.clear();
        assert_eq!(dedup.len(), 0);
        assert!(dedup.is_empty());
    }

    #[test]
    fn napi_helper_default_capacity() {
        let dedup = RedisStreamDedup::new(None);
        assert_eq!(dedup.capacity(), 4096);
    }

    #[test]
    fn napi_helper_explicit_capacity() {
        let dedup = RedisStreamDedup::new(Some(8192));
        assert_eq!(dedup.capacity(), 8192);
    }

    #[test]
    fn napi_helper_capacity_zero_is_clamped() {
        let dedup = RedisStreamDedup::new(Some(0));
        assert_eq!(dedup.capacity(), 1);
    }
}
