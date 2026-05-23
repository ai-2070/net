//! Bounded FIFO ring with drop-oldest-on-overflow semantics.
//!
//! Used by buffering audit appenders (meshos action chain, meshos
//! admin audit, meshos log chain) and the fold-runtime ring audit
//! sink — every one of those wants the same shape: a thread-safe
//! `VecDeque<T>` capped at `capacity`, that drops the oldest entry
//! and increments a counter when full, with snapshot via `Clone`.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};

/// Thread-safe bounded FIFO. On push past capacity, drops the
/// oldest entry and increments [`Self::dropped`].
///
/// `capacity == 0` is accepted and treated as "store nothing" —
/// every `push` increments the dropped counter and returns
/// without growing the deque.
#[derive(Debug)]
pub struct BoundedRing<T> {
    capacity: usize,
    items: parking_lot::Mutex<VecDeque<T>>,
    dropped: AtomicU64,
}

impl<T> BoundedRing<T> {
    /// Build an empty ring capped at `capacity` items.
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            items: parking_lot::Mutex::new(VecDeque::new()),
            dropped: AtomicU64::new(0),
        }
    }

    /// Capacity the ring was constructed with.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Current stored count (always `<= capacity`).
    pub fn len(&self) -> usize {
        self.items.lock().len()
    }

    /// Whether the ring currently holds zero items.
    pub fn is_empty(&self) -> bool {
        self.items.lock().is_empty()
    }

    /// Total number of items dropped because the ring was at
    /// capacity (or because `capacity == 0`). Strictly
    /// non-decreasing.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// Push an item. If the ring is full, drops the oldest and
    /// bumps the dropped counter. Returns `true` if a drop
    /// occurred (either the displaced oldest, or — when
    /// `capacity == 0` — the just-pushed item itself).
    pub fn push(&self, item: T) -> bool {
        if self.capacity == 0 {
            self.dropped.fetch_add(1, Ordering::Relaxed);
            return true;
        }
        let mut items = self.items.lock();
        if items.len() >= self.capacity {
            items.pop_front();
            items.push_back(item);
            self.dropped.fetch_add(1, Ordering::Relaxed);
            true
        } else {
            items.push_back(item);
            false
        }
    }
}

impl<T: Clone> BoundedRing<T> {
    /// Snapshot the stored items in insertion order (oldest →
    /// newest). Returns a clone so the caller can render
    /// without holding the ring's lock.
    pub fn snapshot(&self) -> Vec<T> {
        self.items.lock().iter().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_under_capacity_does_not_drop() {
        let r = BoundedRing::new(4);
        assert!(!r.push(1));
        assert!(!r.push(2));
        assert_eq!(r.len(), 2);
        assert_eq!(r.dropped(), 0);
    }

    #[test]
    fn push_at_capacity_drops_oldest() {
        let r = BoundedRing::new(2);
        assert!(!r.push(1));
        assert!(!r.push(2));
        assert!(r.push(3));
        assert!(r.push(4));
        assert_eq!(r.snapshot(), vec![3, 4]);
        assert_eq!(r.dropped(), 2);
    }

    #[test]
    fn zero_capacity_stores_nothing() {
        let r = BoundedRing::new(0);
        assert!(r.push(1));
        assert!(r.push(2));
        assert!(r.is_empty());
        assert_eq!(r.len(), 0);
        assert!(r.snapshot().is_empty());
        assert_eq!(r.dropped(), 2);
    }
}
