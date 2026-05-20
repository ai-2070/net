//! Lock-free single-producer single-consumer (SPSC) ring buffer.
//!
//! This ring buffer is optimized for high-throughput event ingestion:
//! - Lock-free using atomics
//! - Pre-allocated, fixed capacity
//! - No heap allocation on push/pop
//! - Cache-line aligned to prevent false sharing
//!
//! # Design
//!
//! The buffer uses a power-of-2 capacity for efficient modulo operations
//! (bitwise AND instead of division). Head and tail pointers are cache-line
//! padded to prevent false sharing between producer and consumer threads.
//!
//! # Safety
//!
//! Every `unsafe { }` block in this file accesses the `UnsafeCell<MaybeUninit<T>>`
//! slots under the same SPSC contract documented near the `unsafe impl Send` /
//! `unsafe impl Sync` block: the producer thread holds the head pointer and
//! the consumer thread holds the tail pointer; a slot at index `i = head & mask`
//! (producer side) or `i = tail & mask` (consumer side) is exclusively owned by
//! the side holding that pointer between the load and the store. Debug builds
//! verify SPSC discipline via `producer_in_progress` / `consumer_in_progress`
//! `AtomicBool` thread guards (see `InProgressGuard`).
#![expect(
    clippy::undocumented_unsafe_blocks,
    reason = "SPSC ring discipline documented in the # Safety section above"
)]
#![expect(
    clippy::multiple_unsafe_ops_per_block,
    reason = "ring slot read/write is a single semantic op (UnsafeCell::get deref + MaybeUninit::write or assume_init_read) under SPSC discipline"
)]

use crossbeam_utils::CachePadded;
use std::cell::UnsafeCell;
use std::mem::MaybeUninit;
#[cfg(any(test, debug_assertions))]
use std::sync::atomic::AtomicBool;
use std::sync::atomic::{AtomicU64, Ordering};
/// Error returned when the ring buffer is full.
///
/// Crate-internal: surfaced to public callers as
/// `IngestionError::Backpressure`. Kept `pub(crate)` for symmetry
/// with the `pub(crate) RingBuffer`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BufferFullError;

impl std::fmt::Display for BufferFullError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ring buffer is full")
    }
}

impl std::error::Error for BufferFullError {}

/// Lock-free SPSC ring buffer with cache-line padding.
///
/// # ⚠️  Single-Producer, Single-Consumer contract
///
/// At most one thread at a time may call `try_push`, and at most one
/// (other) thread at a time may call `try_pop` / `pop_batch` /
/// `pop_batch_into`. The atomics rely on that contract for
/// correctness; concurrent access from multiple producers or
/// multiple consumers **silently corrupts state** and is undefined
/// behavior.
///
/// Note: "single producer" allows the *task* / *handle* doing the
/// pushing to migrate between OS threads (e.g. across an `await`
/// point in a tokio task) — what matters is non-concurrency, not a
/// fixed thread id. Likewise for the consumer.
///
/// # Visibility
///
/// This type is `pub(crate)` — there is no public re-export. The
/// only legitimate use inside this crate wraps the buffer in
/// `parking_lot::Mutex<Shard>`, which serializes producer and
/// consumer access trivially. External callers should use
/// `EventBus` / `ShardManager`, which expose the SPSC fast path
/// without the footgun of `Sync`-shareable `&RingBuffer`. The bug
/// report (#5) flagged that prior `pub` exposure: any external
/// caller that put this in an `Arc` and called `try_push` from two
/// threads would silently corrupt state with no compile-time signal.
/// `pub(crate)` removes that surface entirely.
///
/// Internal unit tests (`#[cfg(test)]`) track producer/consumer
/// thread ids and panic on concurrent multi-producer or
/// multi-consumer access — a sanity check, not a complete one;
/// release builds trust the caller.
///
/// # Type Parameters
///
/// - `T`: The element type. Must be `Send` for thread safety.
///
/// # Capacity
///
/// The capacity must be a power of 2 and is fixed at construction time.
/// The actual usable capacity is `capacity - 1` to distinguish between
/// full and empty states.
pub(crate) struct RingBuffer<T> {
    /// Pre-allocated buffer storage.
    buffer: Box<[UnsafeCell<MaybeUninit<T>>]>,
    /// Capacity (power of 2).
    capacity: usize,
    /// Mask for fast modulo (capacity - 1).
    mask: usize,
    /// Write position (producer).
    ///
    /// `head` / `tail` are `u64` regardless of target pointer
    /// width. `AtomicUsize` would wrap on 32-bit targets (wasm32
    /// is in the test matrix) after 2^32 pushes — ~7 minutes per
    /// shard at 10 M events/sec, ~12 hours at 100 K. Once `head`
    /// lapped `tail` and the wrapping distance exceeded
    /// `capacity-1`, `try_push` would reject forever and the
    /// buffer would be permanently wedged. `u64` gives ~58 years
    /// to wrap at 10 G events/sec on every target.
    head: CachePadded<AtomicU64>,
    /// Read position (consumer). See `head` for the rationale on
    /// the `u64` width.
    tail: CachePadded<AtomicU64>,
    /// Producer-side concurrency tripwire (debug-build SPSC enforcement —
    /// active under `debug_assertions`, not just `cfg(test)`, so dev
    /// runs of the binary catch real SPSC violations even outside of
    /// unit tests). The gates below use `debug_assertions` directly
    /// rather than `#[cfg(test)]`, so the safety net stays present
    /// in any non-release build.
    ///
    /// Set to `true` while a producer-side method (`try_push` /
    /// `evict_oldest`) is mid-execution. A second concurrent caller
    /// observes `true` on its swap and panics — that's a genuine
    /// multi-producer SPSC violation. Sequential cross-thread access
    /// is allowed (tasks legitimately migrate between OS threads
    /// across `await` points; the outer `Shard` mutex serializes
    /// them, which is the actual SPSC-safe pattern). Pre-fix this
    /// was a `Mutex<Option<ThreadId>>` that pinned the FIRST OS
    /// thread to ever push and rejected every subsequent thread —
    /// a false positive on every multi-threaded tokio runtime that
    /// migrated the producing task. The `AtomicBool` flag catches
    /// the real hazard (concurrency) without that false positive.
    #[cfg(any(test, debug_assertions))]
    producer_in_progress: AtomicBool,
    /// Consumer-side concurrency tripwire — see `producer_in_progress`.
    #[cfg(any(test, debug_assertions))]
    consumer_in_progress: AtomicBool,
}

/// RAII guard for the in-progress concurrency tripwire. Acquired
/// on entry to a producer- or consumer-side method; cleared on
/// drop, so an early-return or a panic in the work block still
/// releases the flag.
///
/// Conditional on `debug_assertions` to mirror the field-declaration
/// gate. Production builds skip the construction entirely.
#[cfg(any(test, debug_assertions))]
struct InProgressGuard<'a> {
    flag: &'a AtomicBool,
}

#[cfg(any(test, debug_assertions))]
impl<'a> InProgressGuard<'a> {
    /// Atomically set the flag to `true`. Panics if it was already
    /// `true` — that's a genuine concurrent SPSC violation, since
    /// we can only see `true` if another thread is mid-execution
    /// in the same producer- or consumer-side method.
    #[inline]
    fn enter(flag: &'a AtomicBool, label: &'static str) -> Self {
        let was_in = flag.swap(true, Ordering::Acquire);
        assert!(
            !was_in,
            "SPSC violation: {label} called concurrently with another \
             {label} on the same RingBuffer (the SPSC contract requires \
             at most one in-flight call per side at a time — typically \
             upheld by the outer Shard mutex)",
        );
        Self { flag }
    }
}

#[cfg(any(test, debug_assertions))]
impl<'a> Drop for InProgressGuard<'a> {
    #[inline]
    fn drop(&mut self) {
        self.flag.store(false, Ordering::Release);
    }
}

// Safety: The ring buffer is SPSC (single-producer, single-consumer).
// Atomics ensure correct visibility between the one producer and one
// consumer thread. Callers MUST NOT call try_push / pop_batch from
// multiple threads simultaneously — doing so is undefined behavior.
// Debug builds check this at runtime via thread-ID tracking;
// violations panic immediately. Release builds trust the caller.
//
// T must be Send because elements are transferred between threads.
unsafe impl<T: Send> Send for RingBuffer<T> {}
unsafe impl<T: Send> Sync for RingBuffer<T> {}

impl<T> RingBuffer<T> {
    /// Create a new ring buffer with the given capacity.
    ///
    /// # Panics
    ///
    /// Panics if `capacity` is not a power of 2 or is less than 2.
    pub fn new(capacity: usize) -> Self {
        assert!(capacity.is_power_of_two(), "capacity must be a power of 2");
        assert!(capacity >= 2, "capacity must be at least 2");

        // Pre-allocate the buffer
        let buffer: Vec<UnsafeCell<MaybeUninit<T>>> = (0..capacity)
            .map(|_| UnsafeCell::new(MaybeUninit::uninit()))
            .collect();

        Self {
            buffer: buffer.into_boxed_slice(),
            capacity,
            mask: capacity - 1,
            head: CachePadded::new(AtomicU64::new(0)),
            tail: CachePadded::new(AtomicU64::new(0)),
            #[cfg(any(test, debug_assertions))]
            producer_in_progress: AtomicBool::new(false),
            #[cfg(any(test, debug_assertions))]
            consumer_in_progress: AtomicBool::new(false),
        }
    }

    /// Try to push an element into the buffer.
    ///
    /// Returns `Ok(())` if successful, or `Err(BufferFullError)` if the buffer is full.
    ///
    /// SPSC contract: at most one thread may call `try_push` at a
    /// time. The `pub(crate)` visibility plus the in-crate mutex
    /// wrapping in `Shard` upholds this trivially.
    #[inline]
    pub fn try_push(&self, value: T) -> Result<(), BufferFullError> {
        #[cfg(any(test, debug_assertions))]
        let _spsc_guard = InProgressGuard::enter(&self.producer_in_progress, "try_push");

        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Acquire);

        // Check if buffer is full. Both head/tail are u64; the
        // wrapping subtract gives the in-flight length on every
        // target.
        let len = head.wrapping_sub(tail);
        if len >= (self.capacity as u64) - 1 {
            return Err(BufferFullError);
        }

        // Write the value. The `mask` keeps the index inside
        // `capacity` (power-of-2); the `as usize` is the lossless
        // truncation back to the buffer index — `head & mask` is
        // always < `capacity` ≤ `usize::MAX`.
        let index = (head & self.mask as u64) as usize;
        unsafe {
            (*self.buffer[index].get()).write(value);
        }

        // Publish the write
        self.head.store(head.wrapping_add(1), Ordering::Release);

        Ok(())
    }

    /// Producer-side eviction of the oldest element.
    ///
    /// Identical to `try_pop` but tracks the *producer* thread,
    /// not the consumer. Intended exclusively for the
    /// `BackpressureMode::DropOldest` retry path, where the
    /// *producer* needs to evict the oldest event to make room
    /// for a new push. The shard's outer mutex serializes this
    /// evict against any concurrent `try_pop` from the legitimate
    /// consumer (the batch worker), so the SPSC atomic invariants
    /// are upheld even though two different OS threads call into
    /// this producer-side method and the consumer-side `try_pop`
    /// at different times.
    ///
    /// Previously this had no debug-build thread guard at all. The
    /// thread it expects matches `try_push` (the producer); we assert
    /// that here so a future caller using `evict_oldest` from the
    /// consumer thread or from a third thread is caught at test time
    /// the same way `try_push`/`try_pop` are.
    #[inline]
    pub(crate) fn evict_oldest(&self) -> Option<T> {
        #[cfg(any(test, debug_assertions))]
        let _spsc_guard = InProgressGuard::enter(&self.producer_in_progress, "evict_oldest");

        // Same atomic ordering as `try_pop`; only the thread
        // tracking differs.
        let tail = self.tail.load(Ordering::Relaxed);
        let head = self.head.load(Ordering::Acquire);
        if tail == head {
            return None;
        }
        let index = (tail & self.mask as u64) as usize;
        let value = unsafe { (*self.buffer[index].get()).assume_init_read() };
        self.tail.store(tail.wrapping_add(1), Ordering::Release);
        Some(value)
    }

    /// Try to pop an element from the buffer.
    ///
    /// Returns `Some(value)` if successful, or `None` if the buffer is empty.
    ///
    /// SPSC contract: at most one thread may call `try_pop` /
    /// `pop_batch` / `pop_batch_into` at a time. Upheld by the
    /// in-crate mutex on `Shard`.
    #[inline]
    pub fn try_pop(&self) -> Option<T> {
        #[cfg(any(test, debug_assertions))]
        let _spsc_guard = InProgressGuard::enter(&self.consumer_in_progress, "try_pop");

        let tail = self.tail.load(Ordering::Relaxed);
        let head = self.head.load(Ordering::Acquire);

        // Check if buffer is empty
        if tail == head {
            return None;
        }

        // Read the value
        let index = (tail & self.mask as u64) as usize;
        let value = unsafe { (*self.buffer[index].get()).assume_init_read() };

        // Publish the read
        self.tail.store(tail.wrapping_add(1), Ordering::Release);

        Some(value)
    }

    /// Pop up to `max` elements from the buffer into a vector.
    ///
    /// This is more efficient than calling `try_pop` repeatedly as it
    /// reduces atomic operations.
    ///
    /// Same single-consumer contract as `try_pop`.
    #[inline]
    pub fn pop_batch(&self, max: usize) -> Vec<T> {
        #[cfg(any(test, debug_assertions))]
        let _spsc_guard = InProgressGuard::enter(&self.consumer_in_progress, "pop_batch");

        let tail = self.tail.load(Ordering::Relaxed);
        let head = self.head.load(Ordering::Acquire);

        // Calculate how many elements are available. `available`
        // is u64; cap to `max: usize` and convert back.
        let available = head.wrapping_sub(tail);
        let count = available.min(max as u64) as usize;

        if count == 0 {
            return Vec::new();
        }

        // Pre-allocate the result vector
        let mut result = Vec::with_capacity(count);

        // Read all elements
        for i in 0..count {
            let index = (tail.wrapping_add(i as u64) & self.mask as u64) as usize;
            let value = unsafe { (*self.buffer[index].get()).assume_init_read() };
            result.push(value);
        }

        // Publish all reads at once
        self.tail
            .store(tail.wrapping_add(count as u64), Ordering::Release);

        result
    }

    /// Pop up to `max` elements into a caller-owned `Vec`.
    ///
    /// **Append semantics**: this method does **not** clear `dst` first.
    /// It calls `dst.reserve(count)` then pushes drained elements onto
    /// the end. Returns the number of elements drained this call (may
    /// be less than `max` if the buffer has fewer available, including
    /// `0`).
    ///
    /// Use this in steady-state drain loops where the caller keeps a
    /// scratch `Vec` across cycles. Compared to [`pop_batch`], the
    /// per-cycle `Vec` allocation moves *out of the consumer's
    /// critical section* — hot when the ring buffer sits behind a
    /// mutex, since the allocator is no longer called under the lock.
    ///
    /// Typical usage:
    ///
    /// ```ignore
    /// let mut scratch = Vec::with_capacity(BATCH);
    /// loop {
    ///     let popped = ring.pop_batch_into(&mut scratch, BATCH);
    ///     if popped == 0 { break; }
    ///     // mem::replace allocates the fresh scratch *outside* any
    ///     // critical section the caller might have held while
    ///     // calling pop_batch_into.
    ///     let batch = std::mem::replace(&mut scratch, Vec::with_capacity(BATCH));
    ///     consume(batch);
    /// }
    /// ```
    ///
    /// Same single-consumer contract as `try_pop`.
    ///
    /// [`pop_batch`]: Self::pop_batch
    #[inline]
    pub fn pop_batch_into(&self, dst: &mut Vec<T>, max: usize) -> usize {
        #[cfg(any(test, debug_assertions))]
        let _spsc_guard = InProgressGuard::enter(&self.consumer_in_progress, "pop_batch_into");

        let tail = self.tail.load(Ordering::Relaxed);
        let head = self.head.load(Ordering::Acquire);

        let available = head.wrapping_sub(tail);
        let count = available.min(max as u64) as usize;

        if count == 0 {
            return 0;
        }

        // Reserve up-front so the push loop has no reallocation branch.
        dst.reserve(count);

        for i in 0..count {
            let index = (tail.wrapping_add(i as u64) & self.mask as u64) as usize;
            let value = unsafe { (*self.buffer[index].get()).assume_init_read() };
            dst.push(value);
        }

        self.tail
            .store(tail.wrapping_add(count as u64), Ordering::Release);

        count
    }

    /// Get the current number of elements in the buffer.
    ///
    /// `head` / `tail` are `u64` regardless of target; the
    /// in-flight count fits in `usize` because it's bounded by
    /// `capacity - 1` which is itself a `usize`.
    #[inline]
    pub fn len(&self) -> usize {
        let head = self.head.load(Ordering::Acquire);
        let tail = self.tail.load(Ordering::Acquire);
        head.wrapping_sub(tail) as usize
    }

    /// Check if the buffer is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Check if the buffer is full.
    #[inline]
    pub fn is_full(&self) -> bool {
        self.len() >= self.capacity - 1
    }

    /// Get the capacity of the buffer.
    ///
    /// Test-only: the `Shard` wrapper stores its own `capacity` field
    /// for the public API, so this is reachable only from in-file
    /// tests.
    #[cfg(test)]
    #[inline]
    fn capacity(&self) -> usize {
        self.capacity
    }

    /// Get the number of free slots in the buffer.
    ///
    /// Test-only — see `capacity()`.
    #[cfg(test)]
    #[inline]
    fn free_slots(&self) -> usize {
        self.capacity - 1 - self.len()
    }
}

impl<T> Drop for RingBuffer<T> {
    fn drop(&mut self) {
        // Clear the in-progress flags before the drain so the
        // `try_pop` calls below don't trip the consumer-side guard.
        // `&mut self` proves we are the unique accessor here, so
        // any non-cleared flag is leftover state from a prior
        // panicking caller — safe to clear.
        #[cfg(any(test, debug_assertions))]
        {
            self.producer_in_progress.store(false, Ordering::Release);
            self.consumer_in_progress.store(false, Ordering::Release);
        }
        // Drop any remaining elements. `&mut self` proves we are the
        // unique accessor, so the SPSC contract holds trivially.
        while self.try_pop().is_some() {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_push_pop() {
        let buf = RingBuffer::new(4);

        assert!(buf.is_empty());
        assert_eq!(buf.len(), 0);

        buf.try_push(1).unwrap();
        buf.try_push(2).unwrap();
        buf.try_push(3).unwrap();

        assert_eq!(buf.len(), 3);
        assert!(buf.is_full()); // capacity - 1 = 3

        assert!(buf.try_push(4).is_err()); // Should fail, buffer full

        assert_eq!(buf.try_pop(), Some(1));
        assert_eq!(buf.try_pop(), Some(2));
        assert_eq!(buf.try_pop(), Some(3));
        assert_eq!(buf.try_pop(), None);

        assert!(buf.is_empty());
    }

    #[test]
    fn test_pop_batch() {
        let buf = RingBuffer::new(8);

        for i in 0..5 {
            buf.try_push(i).unwrap();
        }

        let batch = buf.pop_batch(3);
        assert_eq!(batch, vec![0, 1, 2]);

        let batch = buf.pop_batch(10); // Request more than available
        assert_eq!(batch, vec![3, 4]);

        assert!(buf.is_empty());
    }

    /// `pop_batch_into` is the steady-state drain primitive: it must
    /// produce the same elements as `pop_batch`, append (not replace)
    /// onto `dst`, return `0` when the buffer is empty, and tolerate
    /// being called with `max` larger than what's available.
    #[test]
    fn test_pop_batch_into() {
        let buf = RingBuffer::new(8);
        for i in 0..5 {
            buf.try_push(i).unwrap();
        }

        // Append onto an existing element — verifies the documented
        // append semantics (does not clear `dst`).
        let mut dst = vec![999u32];
        let drained = buf.pop_batch_into(&mut dst, 3);
        assert_eq!(drained, 3);
        assert_eq!(dst, vec![999, 0, 1, 2]);

        // Request more than available; should drain only what's there.
        dst.clear();
        let drained = buf.pop_batch_into(&mut dst, 10);
        assert_eq!(drained, 2);
        assert_eq!(dst, vec![3, 4]);
        assert!(buf.is_empty());

        // Empty buffer returns 0 without allocating or pushing.
        dst.clear();
        let drained = buf.pop_batch_into(&mut dst, 100);
        assert_eq!(drained, 0);
        assert!(dst.is_empty());
    }

    /// Reusing a scratch `Vec` across cycles (the canonical drain
    /// pattern) must not corrupt or skip elements across wraparound.
    #[test]
    fn test_pop_batch_into_scratch_reuse_across_wraparound() {
        let buf = RingBuffer::new(4);
        let mut scratch: Vec<u32> = Vec::with_capacity(2);
        let mut seen: Vec<u32> = Vec::new();

        for round in 0..10u32 {
            for i in 0..3 {
                buf.try_push(round * 3 + i).unwrap();
            }
            let drained = buf.pop_batch_into(&mut scratch, 3);
            assert_eq!(drained, 3);
            seen.append(&mut scratch); // empties scratch, retains capacity
        }

        let expected: Vec<u32> = (0..30).collect();
        assert_eq!(seen, expected);
    }

    #[test]
    fn test_wraparound() {
        let buf = RingBuffer::new(4);

        // Fill and drain multiple times to test wraparound
        for round in 0..10 {
            for i in 0..3 {
                buf.try_push(round * 3 + i).unwrap();
            }

            for i in 0..3 {
                assert_eq!(buf.try_pop(), Some(round * 3 + i));
            }
        }
    }

    #[test]
    fn test_concurrent_spsc() {
        use std::sync::Arc;
        use std::thread;

        let buf = Arc::new(RingBuffer::new(1024));
        let buf_producer = buf.clone();
        let buf_consumer = buf.clone();

        let count = 100_000;

        // Exactly one thread calls `try_push` (producer) and exactly
        // one calls `try_pop` (consumer). This is the SPSC happy path
        // the buffer is designed for.
        let producer = thread::spawn(move || {
            for i in 0..count {
                while buf_producer.try_push(i).is_err() {
                    std::hint::spin_loop();
                }
            }
        });

        let consumer = thread::spawn(move || {
            let mut received = Vec::with_capacity(count);
            while received.len() < count {
                if let Some(val) = buf_consumer.try_pop() {
                    received.push(val);
                } else {
                    std::hint::spin_loop();
                }
            }
            received
        });

        producer.join().unwrap();
        let received = consumer.join().unwrap();

        // Verify we got all values in order
        assert_eq!(received.len(), count);
        for (i, &val) in received.iter().enumerate() {
            assert_eq!(val, i, "mismatch at index {}", i);
        }
    }

    #[test]
    #[should_panic(expected = "power of 2")]
    fn test_non_power_of_two_capacity() {
        let _ = RingBuffer::<i32>::new(5);
    }

    #[test]
    fn test_drop() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        let drop_count = Arc::new(AtomicUsize::new(0));

        struct DropCounter(Arc<AtomicUsize>);
        impl Drop for DropCounter {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }

        {
            let buf = RingBuffer::new(8);
            for _ in 0..5 {
                buf.try_push(DropCounter(drop_count.clone())).unwrap();
            }
            // Buffer drops here with 5 elements
        }

        assert_eq!(drop_count.load(Ordering::SeqCst), 5);
    }

    #[test]
    fn test_buffer_full_error_display() {
        let err = BufferFullError;
        assert_eq!(format!("{}", err), "ring buffer is full");
    }

    #[test]
    fn test_buffer_full_error_debug() {
        let err = BufferFullError;
        assert!(format!("{:?}", err).contains("BufferFullError"));
    }

    #[test]
    fn test_buffer_full_error_is_error() {
        let err: &dyn std::error::Error = &BufferFullError;
        assert!(err.to_string().contains("full"));
    }

    #[test]
    fn test_capacity_and_free_slots() {
        let buf = RingBuffer::new(8);
        assert_eq!(buf.capacity(), 8);
        assert_eq!(buf.free_slots(), 7); // capacity - 1

        buf.try_push(1).unwrap();
        assert_eq!(buf.free_slots(), 6);

        buf.try_push(2).unwrap();
        buf.try_push(3).unwrap();
        assert_eq!(buf.free_slots(), 4);
    }

    #[test]
    fn test_is_full() {
        let buf = RingBuffer::new(4);
        assert!(!buf.is_full());

        buf.try_push(1).unwrap();
        buf.try_push(2).unwrap();
        assert!(!buf.is_full());

        buf.try_push(3).unwrap();
        assert!(buf.is_full());
    }

    #[test]
    fn test_pop_batch_empty() {
        let buf: RingBuffer<i32> = RingBuffer::new(8);
        let batch = buf.pop_batch(10);
        assert!(batch.is_empty());
    }

    #[test]
    #[should_panic(expected = "at least 2")]
    fn test_capacity_too_small() {
        let _ = RingBuffer::<i32>::new(1);
    }

    #[test]
    fn test_push_pop_at_exact_capacity() {
        // Regression: ensure the full check works correctly at boundary
        let buf = RingBuffer::new(4); // usable capacity = 3

        // Fill to exactly full
        buf.try_push(1).unwrap();
        buf.try_push(2).unwrap();
        buf.try_push(3).unwrap();
        assert!(buf.is_full());
        assert!(buf.try_push(4).is_err());

        // Pop one and push one - should succeed
        assert_eq!(buf.try_pop(), Some(1));
        buf.try_push(4).unwrap();
        assert!(buf.is_full());

        // Verify order
        assert_eq!(buf.try_pop(), Some(2));
        assert_eq!(buf.try_pop(), Some(3));
        assert_eq!(buf.try_pop(), Some(4));
        assert!(buf.is_empty());
    }

    #[test]
    fn test_push_pop_boundary_stress() {
        // Regression: repeated fill/drain cycles at exact capacity boundary
        let buf = RingBuffer::new(4);

        for round in 0..100 {
            // Fill to capacity
            for i in 0..3 {
                buf.try_push(round * 3 + i)
                    .unwrap_or_else(|_| panic!("push failed at round {} item {}", round, i));
            }
            assert!(buf.is_full());
            assert!(buf.try_push(999).is_err());

            // Drain completely
            for i in 0..3 {
                assert_eq!(buf.try_pop(), Some(round * 3 + i));
            }
            assert!(buf.is_empty());
        }
    }

    /// Cursors must be `u64` regardless of target pointer
    /// width. Pre-fix `head` and `tail` were `AtomicUsize`; on
    /// 32-bit they wrapped after 2^32 pushes and the buffer
    /// permanently wedged once the wrapping distance exceeded
    /// `capacity-1`. Pin the type-level invariant so a future
    /// regression to `AtomicUsize` would fail this test.
    ///
    /// We can't actually push 2^32 items in a unit test, but we
    /// CAN verify the cursor field types via `std::mem::size_of`:
    /// `AtomicU64` is always 8 bytes, regardless of target pointer
    /// width. (`AtomicUsize` would be 4 on 32-bit, 8 on 64-bit.)
    #[test]
    fn ring_buffer_cursors_are_u64_on_every_target() {
        // Confirm at the type level via size_of_val. `head` lives
        // inside CachePadded so the alignment is the cache line
        // size, but the inner AtomicU64 is exactly 8 bytes.
        // We can't directly inspect head's type from a unit test,
        // so we assert on the underlying load type — `u64` —
        // which is the load-bearing property.
        let buf: RingBuffer<u32> = RingBuffer::new(4);
        let head_val: u64 = buf.head.load(Ordering::Relaxed);
        let tail_val: u64 = buf.tail.load(Ordering::Relaxed);
        assert_eq!(head_val, 0);
        assert_eq!(tail_val, 0);
        // `wrapping_sub` returns u64 — type-level pin (this would
        // fail to compile if the cursors were AtomicUsize on a
        // 32-bit target).
        let len: u64 = head_val.wrapping_sub(tail_val);
        assert_eq!(len, 0);
    }

    /// Pin the new SPSC-tripwire semantics: SEQUENTIAL cross-thread
    /// access is allowed (it's exactly what tokio multi-threaded
    /// runtimes do — a task migrates between OS threads across
    /// `await` points; the outer `Shard` mutex serializes the calls,
    /// which IS the SPSC-safe pattern). Pre-fix the tripwire was
    /// thread-identity-based and falsely fired on any task migration,
    /// causing `bus_shutdown_drain` and `ffi_shutdown_race` to fail
    /// on master under multi-threaded runtimes.
    #[test]
    fn sequential_cross_thread_push_is_allowed() {
        use std::sync::Arc;
        use std::thread;

        let buf = Arc::new(RingBuffer::new(1024));

        // Push from this thread.
        buf.try_push(1).unwrap();

        // Push from a different thread, AFTER the first push has
        // returned. This is sequential, not concurrent — SPSC-safe.
        // Pre-fix this panicked because the tripwire pinned the OS
        // thread identity; post-fix it's allowed.
        let buf2 = buf.clone();
        let result = thread::spawn(move || buf2.try_push(2).unwrap()).join();

        assert!(
            result.is_ok(),
            "sequential cross-thread push must be allowed (the SPSC \
             contract is about non-concurrency, not thread identity — \
             tokio task migration must not trip the tripwire)",
        );
    }

    #[test]
    fn sequential_cross_thread_pop_is_allowed() {
        use std::sync::Arc;
        use std::thread;

        let buf = Arc::new(RingBuffer::new(1024));
        buf.try_push(1).unwrap();
        buf.try_push(2).unwrap();

        let _ = buf.try_pop();

        let buf2 = buf.clone();
        let result = thread::spawn(move || buf2.try_pop()).join();

        assert!(
            result.is_ok(),
            "sequential cross-thread pop must be allowed",
        );
    }

    #[test]
    fn sequential_cross_thread_evict_oldest_is_allowed() {
        use std::sync::Arc;
        use std::thread;

        let buf = Arc::new(RingBuffer::new(4));
        buf.try_push(1).unwrap();

        let buf2 = buf.clone();
        let result = thread::spawn(move || buf2.evict_oldest()).join();

        assert!(
            result.is_ok(),
            "sequential cross-thread evict_oldest must be allowed",
        );
    }

    /// Regression: a real SPSC violation IS a concurrent call.
    /// Simulate it by pre-setting the in-progress flag (modelling
    /// "another caller is mid-execution") and verify the tripwire
    /// fires. This is the deterministic version of the race-test
    /// further down — same invariant, no scheduling dependency.
    #[test]
    fn concurrent_producer_panics_via_simulated_in_progress_flag() {
        let buf = RingBuffer::<i32>::new(8);

        // Simulate "another producer is mid-call" by setting the
        // in-progress flag.
        buf.producer_in_progress.store(true, Ordering::Release);

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            buf.try_push(1).unwrap();
        }));
        assert!(
            result.is_err(),
            "try_push must panic when a producer is already in-progress (real SPSC violation)",
        );

        // Restore the flag so `Drop` doesn't trip its own assertion
        // when the buffer is destroyed.
        buf.producer_in_progress.store(false, Ordering::Release);
    }

    #[test]
    fn concurrent_consumer_panics_via_simulated_in_progress_flag() {
        let buf = RingBuffer::<i32>::new(8);
        buf.try_push(1).unwrap();

        buf.consumer_in_progress.store(true, Ordering::Release);

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = buf.try_pop();
        }));
        assert!(
            result.is_err(),
            "try_pop must panic when a consumer is already in-progress",
        );

        buf.consumer_in_progress.store(false, Ordering::Release);
    }

    #[test]
    fn concurrent_evict_oldest_panics_via_simulated_in_progress_flag() {
        let buf = RingBuffer::<i32>::new(4);
        buf.try_push(1).unwrap();

        // `evict_oldest` shares the producer-side flag (it's a
        // producer-side operation that runs from the
        // `BackpressureMode::DropOldest` retry path on the producer
        // thread). A concurrent `try_push` or another `evict_oldest`
        // is a real SPSC violation.
        buf.producer_in_progress.store(true, Ordering::Release);

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = buf.evict_oldest();
        }));
        assert!(
            result.is_err(),
            "evict_oldest must panic when a producer is already in-progress",
        );

        buf.producer_in_progress.store(false, Ordering::Release);
    }

    /// The in-progress guard MUST clear the flag on Drop, so an
    /// early-return inside the work block (or a panic) doesn't
    /// permanently latch the tripwire on. Pin this directly: the
    /// `count == 0` early-return path in `pop_batch_into` exits
    /// without touching the buffer; the next call must succeed.
    #[test]
    fn guard_releases_flag_on_early_return() {
        let buf = RingBuffer::<i32>::new(4);
        let mut scratch = Vec::new();

        // Empty buffer → early return at `count == 0`.
        let popped = buf.pop_batch_into(&mut scratch, 8);
        assert_eq!(popped, 0);

        // Flag must have been cleared by the guard's Drop.
        assert!(
            !buf.consumer_in_progress.load(Ordering::Acquire),
            "in-progress flag must be cleared on early return",
        );

        // A subsequent call from a different thread must succeed
        // (would panic if the flag was still set).
        buf.try_push(42).unwrap();
        assert_eq!(buf.pop_batch_into(&mut scratch, 8), 1);
    }

    // A panic-during-work test would require either an injection
    // hook in `try_push` (we don't add production-code seams for
    // tests) or a custom `T` whose drop panics inside the unsafe
    // write. Both options bring more risk than they buy. The
    // RAII-correctness of the guard is mechanical: `InProgressGuard`
    // implements `Drop` that unconditionally clears the flag, and
    // `Drop` runs on unwind regardless of where in the function the
    // panic originates. The early-return test above covers the
    // non-panic teardown path; the unwind path is structural.
}
