//! Time-ordered expiry index for a [`FoldState`](super::FoldState):
//! CAPABILITY_FOLD_SCALE_PLAN.md Track A2 / Slice 8.
//!
//! Before this, every sweep walked the whole primary map to find the
//! few entries (usually none) whose deadline had passed: 5.7 ms at 1M
//! entries, twice a second, to find nothing. The wheel keeps every
//! entry's deadline in a structure ordered by time, so a sweep visits
//! only the slots whose time has come.
//!
//! # Shape: a hashed timing wheel
//!
//! Time is cut into [`SLOT`]-long slots, numbered from the wheel's
//! epoch. A slot lives in ring position `slot % RING`, so the ring
//! covers one lap of [`RING`] × [`SLOT`] (512 s) and a longer deadline
//! shares its position with nearer ones until its own lap comes round.
//! Each ring position heads an intrusive doubly linked list of
//! [`Node`]s. The nodes live in one slab, recycled through a free list.
//!
//! The plan's draft shape was `BTreeMap<u64, HashSet<Key>>`. It is not
//! used because a refresh moves its key to a later bucket, and a bucket
//! that is new, or has to grow, allocates. That breaks Slice 3's gate,
//! zero allocations on a warm index-equivalent refresh, for whichever
//! refresh lands first in a bucket. Here a refresh only relinks its
//! node: [`ExpiryWheel::reschedule`] never allocates. The slab grows
//! only on a cold insert, which allocates anyway.
//!
//! # Contracts
//!
//! - **One active placement per key.** Each entry owns exactly one node,
//!   named by its handle (`FoldEntry::expiry_node`). A refresh moves that
//!   node; nothing is left behind. So `len() == entries.len()` holds
//!   after every mutation and at every lock release, including between
//!   the chunks of one sweep.
//! - **Partial-bucket draining, not ceiling placement.** A node sits in
//!   the slot holding its exact deadline (floor placement). A sweep visits
//!   every slot from the cursor up to and including the current one, and
//!   evicts exactly the nodes whose deadline has passed. Nodes in the
//!   current slot that are not yet due, and nodes a lap or more ahead,
//!   stay where they are. So expiry is exact, as it was with the full
//!   walk. Ceiling placement would let an entry outlive its deadline by up
//!   to one slot plus a sweep interval, which a reservation lease should
//!   not.
//! - **What a sweep visits.** It visits the nodes in the slots between the
//!   previous sweep and now (about 4 slots at the default 500 ms
//!   interval), never the rest of the fold. Not-yet-due nodes in those
//!   slots are visited without being evicted: the current slot's
//!   undue tail, and longer-lived entries a lap or more ahead. Both are
//!   a small, bounded share of the fold, not a walk of it.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Length of one wheel slot.
pub(super) const SLOT: Duration = Duration::from_millis(125);

/// Number of ring positions. One lap is `RING × SLOT` = 512 s, longer
/// than the default TTLs, so a lap-ahead node is the exception.
pub(super) const RING: usize = 4096;

/// Marks the end of a list, an unlinked node, and an entry with no node.
pub(super) const NIL: u32 = u32::MAX;

#[derive(Debug)]
struct Node<Q> {
    /// The scheduled key, `None` while the node is on the free list.
    key: Option<Q>,
    deadline: Instant,
    /// Ring position this node is linked into.
    position: u32,
    prev: u32,
    /// The next node in its ring list, or in the free list.
    next: u32,
}

/// Deadline-ordered index of a fold's keys. See the module doc.
#[derive(Debug)]
pub(super) struct ExpiryWheel<Q> {
    epoch: Instant,
    /// First slot not yet fully drained. Every scheduled node sits at a
    /// slot at or after it, so a sweep starting here misses nothing.
    ///
    /// Atomic so that a read-locked probe that finds nothing due can
    /// advance it (`fetch_max`, monotonic under concurrent probes); every
    /// other write happens under the state write lock. Without that, an
    /// idle fold's probe range grew from a few slots toward the whole
    /// ring on every sweep (cubic, PR #1210).
    cursor: AtomicU64,
    heads: Box<[u32]>,
    nodes: Vec<Node<Q>>,
    free: u32,
    len: usize,
    /// Most nodes the slab may hold. `NIL` is a node index's sentinel, so
    /// at most `NIL` nodes (indices `0..NIL`) are addressable. Callers
    /// check [`Self::has_room`] before an insert and refuse the entry.
    limit: usize,
}

/// What [`ExpiryWheel::take_due`] did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct Drain {
    /// Nodes visited, due or not: the sweep's entry-visit cost.
    pub(super) visited: usize,
    /// Every due node up to `now` has been taken.
    pub(super) exhausted: bool,
}

impl<Q> ExpiryWheel<Q> {
    fn cursor(&self) -> u64 {
        self.cursor.load(Ordering::Relaxed)
    }

    pub(super) fn new(epoch: Instant) -> Self {
        Self {
            epoch,
            cursor: AtomicU64::new(0),
            heads: vec![NIL; RING].into_boxed_slice(),
            nodes: Vec::new(),
            free: NIL,
            len: 0,
            limit: NIL as usize,
        }
    }

    /// Scheduled keys: always equal to the fold's entry count.
    pub(super) fn len(&self) -> usize {
        self.len
    }

    /// Most keys the wheel can schedule at once.
    pub(super) fn limit(&self) -> usize {
        self.limit
    }

    /// Whether one more key can be scheduled: a freed node to reuse, or
    /// room to grow the slab. Never wraps into `NIL`.
    pub(super) fn has_room(&self) -> bool {
        self.free != NIL || self.nodes.len() < self.limit
    }

    /// Lower the node limit, so a test can reach it.
    #[cfg(test)]
    pub(super) fn set_limit(&mut self, limit: usize) {
        self.limit = limit.min(NIL as usize);
    }

    fn slot_of(&self, at: Instant) -> u64 {
        let since = at.saturating_duration_since(self.epoch);
        (since.as_nanos() / SLOT.as_nanos()) as u64
    }

    /// Ring position for `deadline`. A deadline before the cursor (a
    /// restored entry anchored in the past, say) is placed at the cursor,
    /// which the next sweep visits.
    fn position_of(&self, deadline: Instant) -> u32 {
        let slot = self.slot_of(deadline).max(self.cursor());
        (slot % RING as u64) as u32
    }

    fn link(&mut self, at: u32, position: u32) {
        let head = self.heads[position as usize];
        {
            let node = &mut self.nodes[at as usize];
            node.position = position;
            node.prev = NIL;
            node.next = head;
        }
        if head != NIL {
            self.nodes[head as usize].prev = at;
        }
        self.heads[position as usize] = at;
    }

    fn unlink(&mut self, at: u32) {
        let (prev, next, position) = {
            let node = &self.nodes[at as usize];
            (node.prev, node.next, node.position)
        };
        if prev == NIL {
            self.heads[position as usize] = next;
        } else {
            self.nodes[prev as usize].next = next;
        }
        if next != NIL {
            self.nodes[next as usize].prev = prev;
        }
        let node = &mut self.nodes[at as usize];
        node.prev = NIL;
        node.next = NIL;
    }

    /// Schedule `key` at `deadline`, returning the node handle the entry
    /// keeps. Reuses a freed node when there is one.
    pub(super) fn insert(&mut self, key: Q, deadline: Instant) -> u32 {
        let position = self.position_of(deadline);
        let at = if self.free != NIL {
            let at = self.free;
            let node = &mut self.nodes[at as usize];
            self.free = node.next;
            node.key = Some(key);
            node.deadline = deadline;
            at
        } else {
            let at = u32::try_from(self.nodes.len()).unwrap_or(NIL);
            debug_assert!(
                self.nodes.len() < self.limit && at != NIL,
                "expiry wheel insert past its limit; callers check has_room"
            );
            self.nodes.push(Node {
                key: Some(key),
                deadline,
                position,
                prev: NIL,
                next: NIL,
            });
            at
        };
        self.link(at, position);
        self.len += 1;
        at
    }

    /// Move node `at` to `deadline`. Never allocates.
    pub(super) fn reschedule(&mut self, at: u32, deadline: Instant) {
        let Some(node) = self.nodes.get_mut(at as usize) else {
            return;
        };
        if node.key.is_none() {
            return;
        }
        node.deadline = deadline;
        let position = self.position_of(deadline);
        if self.nodes[at as usize].position != position {
            self.unlink(at);
            self.link(at, position);
        }
    }

    /// Unschedule node `at`, returning its key.
    pub(super) fn remove(&mut self, at: u32) -> Option<Q> {
        let key = self.nodes.get_mut(at as usize)?.key.take()?;
        self.unlink(at);
        self.nodes[at as usize].next = self.free;
        self.free = at;
        self.len -= 1;
        Some(key)
    }

    /// Unschedule everything and restart the cursor at `now`, ahead of a
    /// restore. Keeps the slab's capacity.
    pub(super) fn clear(&mut self, now: Instant) {
        self.heads.fill(NIL);
        self.nodes.clear();
        self.free = NIL;
        self.len = 0;
        self.cursor.store(self.slot_of(now), Ordering::Relaxed);
    }

    /// The slots a sweep at `now` visits, as `(first, last)`: from the
    /// cursor up to and including `now`'s slot, at most one lap, since a
    /// lap visits every ring position once.
    fn due_slots(&self, now: Instant) -> (u64, u64) {
        let cursor = self.cursor();
        let last = self.slot_of(now).max(cursor);
        let first = cursor.max((last + 1).saturating_sub(RING as u64));
        (first, last)
    }

    /// Whether any node is due at `now`, and how many nodes the check
    /// visited. Read-only, so a sweep can decide under the read lock that
    /// there is nothing to do.
    pub(super) fn probe_due(&self, now: Instant) -> (bool, usize) {
        let (first, last) = self.due_slots(now);
        let mut visited = 0usize;
        for slot in first..=last {
            let mut at = self.heads[(slot % RING as u64) as usize];
            while at != NIL {
                let node = &self.nodes[at as usize];
                visited += 1;
                if node.deadline <= now {
                    return (true, visited);
                }
                at = node.next;
            }
        }
        // Nothing is due anywhere up to `now`. A node in a slot before
        // `last` would have a deadline before `now` and so be due; none is,
        // so no current-lap node sits before `last` and the next probe can
        // start there. `last`'s own slot can still fill: the cursor stops
        // at it, never past.
        self.cursor.fetch_max(last, Ordering::Relaxed);
        (false, visited)
    }

    /// Unschedule up to `max` nodes due at `now` (deadline `<= now`),
    /// appending their keys to `out` (whatever it already holds). The cursor advances past every slot
    /// before `now`'s that this call fully drained, so a later call
    /// resumes where this one stopped. Node handles of the taken keys are
    /// freed; the caller removes the matching entries in the same
    /// critical section.
    pub(super) fn take_due(&mut self, now: Instant, max: usize, out: &mut Vec<Q>) -> Drain {
        let (first, last) = self.due_slots(now);
        self.cursor.store(first, Ordering::Relaxed);
        let mut drain = Drain::default();
        // `max` bounds what THIS call takes, whatever `out` already holds.
        let limit = out.len().saturating_add(max);
        for slot in first..=last {
            let position = (slot % RING as u64) as usize;
            let mut at = self.heads[position];
            while at != NIL {
                let (next, due) = {
                    let node = &self.nodes[at as usize];
                    (node.next, node.deadline <= now)
                };
                if due && out.len() == limit {
                    // Left for the next call, which visits it then.
                    return drain;
                }
                drain.visited += 1;
                if due {
                    if let Some(key) = self.remove(at) {
                        out.push(key);
                    }
                }
                at = next;
            }
            // Every node left at this position is due in a later slot.
            // The current slot may still fill, so the cursor stops there.
            if slot < last {
                self.cursor.store(slot + 1, Ordering::Relaxed);
            }
        }
        drain.exhausted = true;
        drain
    }

    /// The keys scheduled at each ring position, with their deadlines:
    /// for invariant checks in tests.
    #[cfg(test)]
    pub(super) fn scheduled(&self) -> impl Iterator<Item = (u32, &Q, Instant)> + '_ {
        self.nodes
            .iter()
            .enumerate()
            .filter_map(|(at, node)| node.key.as_ref().map(|k| (at as u32, k, node.deadline)))
    }

    /// The ring position node `at` is linked into.
    #[cfg(test)]
    pub(super) fn position_of_node(&self, at: u32) -> Option<u32> {
        let node = self.nodes.get(at as usize)?;
        node.key.as_ref().map(|_| node.position)
    }

    /// The ring position a deadline maps to now.
    #[cfg(test)]
    pub(super) fn position_for(&self, deadline: Instant) -> u32 {
        self.position_of(deadline)
    }

    /// Check the lists against the slab: every live node is linked into
    /// exactly the position it records, with consistent back links, and
    /// the lists hold exactly `len` nodes.
    #[cfg(test)]
    pub(super) fn assert_consistent(&self) {
        let mut linked = 0usize;
        for (position, &head) in self.heads.iter().enumerate() {
            let mut prev = NIL;
            let mut at = head;
            while at != NIL {
                let node = &self.nodes[at as usize];
                assert!(node.key.is_some(), "free node {at} is linked");
                assert_eq!(node.position as usize, position, "node {at} position");
                assert_eq!(node.prev, prev, "node {at} back link");
                linked += 1;
                prev = at;
                at = node.next;
            }
        }
        assert_eq!(linked, self.len, "linked nodes != len");
        let live = self.nodes.iter().filter(|n| n.key.is_some()).count();
        assert_eq!(live, self.len, "live nodes != len");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(epoch: Instant, ms: u64) -> Instant {
        epoch + Duration::from_millis(ms)
    }

    /// A deadline 0.75 of the way into a slot, swept at its half: the
    /// node stays (not due), and the next sweep after the deadline takes
    /// it. Floor placement plus an exact re-check never loses it.
    #[test]
    fn partial_slot_deadline_is_never_lost() {
        let epoch = Instant::now();
        let mut wheel = ExpiryWheel::new(epoch);
        // Slot 8 spans 1000..1125 ms.
        wheel.insert("k", at(epoch, 1094));
        let mut out = Vec::new();
        assert!(wheel.take_due(at(epoch, 1062), 16, &mut out).exhausted);
        assert!(out.is_empty(), "not due mid-slot");
        assert_eq!(wheel.len(), 1);
        wheel.take_due(at(epoch, 1094), 16, &mut out);
        assert_eq!(out, vec!["k"], "due exactly at its deadline");
        assert_eq!(wheel.len(), 0);
        wheel.assert_consistent();
    }

    /// A node a whole lap ahead shares a ring position with a due one:
    /// the sweep takes the due node and leaves the lap-ahead one.
    #[test]
    fn lap_ahead_node_survives_its_shared_position() {
        let epoch = Instant::now();
        let mut wheel = ExpiryWheel::new(epoch);
        let lap = SLOT * RING as u32;
        wheel.insert("now", at(epoch, 10));
        wheel.insert("next lap", at(epoch, 10) + lap);
        let mut out = Vec::new();
        wheel.take_due(at(epoch, 20), 16, &mut out);
        assert_eq!(out, vec!["now"]);
        assert_eq!(wheel.len(), 1);
        out.clear();
        wheel.take_due(at(epoch, 20) + lap, 16, &mut out);
        assert_eq!(out, vec!["next lap"]);
        wheel.assert_consistent();
    }

    /// `max` bounds one call; the next call resumes and the lists stay
    /// consistent in between.
    #[test]
    fn take_due_resumes_across_calls() {
        let epoch = Instant::now();
        let mut wheel = ExpiryWheel::new(epoch);
        for i in 0..100u64 {
            wheel.insert(i, at(epoch, i * 7));
        }
        let mut out = Vec::new();
        let mut calls = 0;
        loop {
            calls += 1;
            let drain = wheel.take_due(at(epoch, 1000), 30, &mut out);
            wheel.assert_consistent();
            assert_eq!(wheel.len() + out.len(), 100);
            if drain.exhausted {
                break;
            }
            assert_eq!(out.len() % 30, 0);
        }
        assert_eq!(calls, 4);
        out.sort_unstable();
        assert_eq!(out, (0..100).collect::<Vec<_>>());
    }

    /// Rescheduling relinks the node in place: the same handle, a new
    /// position, nothing left in the old one, and no new slab node.
    #[test]
    fn reschedule_moves_the_one_placement() {
        let epoch = Instant::now();
        let mut wheel = ExpiryWheel::new(epoch);
        let handle = wheel.insert("k", at(epoch, 300_000));
        let before = wheel.position_of_node(handle);
        wheel.reschedule(handle, at(epoch, 450_000));
        let after = wheel.position_of_node(handle);
        assert_ne!(before, after);
        assert_eq!(after, Some(wheel.position_for(at(epoch, 450_000))));
        assert_eq!(wheel.len(), 1);
        assert_eq!(wheel.nodes.len(), 1, "no second node");
        wheel.assert_consistent();
    }

    /// A sweep that fell more than a lap behind still visits every
    /// position once and takes everything due.
    #[test]
    fn a_sweep_a_lap_late_takes_everything_due() {
        let epoch = Instant::now();
        let mut wheel = ExpiryWheel::new(epoch);
        let lap = SLOT * RING as u32;
        for i in 0..50u64 {
            wheel.insert(i, at(epoch, i * 1000));
        }
        let mut out = Vec::new();
        let drain = wheel.take_due(at(epoch, 0) + lap * 3, 1000, &mut out);
        assert!(drain.exhausted);
        assert_eq!(out.len(), 50);
        assert_eq!(wheel.len(), 0);
    }

    /// An idle probe advances the cursor, so the next idle probe visits
    /// only the slots since, not every slot since the wheel's epoch. A node
    /// one lap ahead sits at an early ring position: the first probe
    /// passes it (not due); a second probe a second later must not visit
    /// it again (cubic, PR #1210).
    #[test]
    fn an_idle_probe_advances_the_cursor() {
        let epoch = Instant::now();
        let mut wheel = ExpiryWheel::new(epoch);
        let lap = SLOT * RING as u32;
        wheel.insert("next lap", at(epoch, 5_000) + lap);
        let (due, visited) = wheel.probe_due(at(epoch, 10_000));
        assert!(!due);
        assert_eq!(visited, 1, "the lap-ahead node is passed once");
        assert_eq!(wheel.cursor(), wheel.slot_of(at(epoch, 10_000)));
        let (due, visited) = wheel.probe_due(at(epoch, 11_000));
        assert!(!due);
        assert_eq!(visited, 0, "the next probe starts where the last stopped");
        // A deadline inside the current slot is still found.
        wheel.insert("soon", at(epoch, 11_050));
        assert!(wheel.probe_due(at(epoch, 11_060)).0);
        wheel.assert_consistent();
    }

    #[test]
    fn freed_nodes_are_reused() {
        let epoch = Instant::now();
        let mut wheel = ExpiryWheel::new(epoch);
        let a = wheel.insert("a", at(epoch, 10));
        assert_eq!(wheel.remove(a), Some("a"));
        assert_eq!(wheel.remove(a), None, "double remove is a no-op");
        let b = wheel.insert("b", at(epoch, 10));
        assert_eq!(a, b, "slot reused");
        assert_eq!(wheel.nodes.len(), 1);
        wheel.assert_consistent();
    }
}
