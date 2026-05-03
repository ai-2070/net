//! Per-channel subscriber roster for daemon-layer fan-out.
//!
//! The roster tracks which peer `node_id`s have subscribed to which
//! channels, populated by `SUBPROTOCOL_CHANNEL_MEMBERSHIP` messages
//! and reaped by the failure detector. It's the thing a
//! [`ChannelPublisher`](crate::adapter::net::ChannelPublisher) iterates
//! over when fanning out a publish.
//!
//! This is not a transport primitive. One publish call still becomes
//! N per-peer unicasts — the roster just tells the publisher who
//! those N peers are.

use dashmap::DashMap;
use dashmap::DashSet;
use std::sync::Arc;

use super::name::ChannelId;

/// Subscriber roster keyed by `ChannelId`.
///
/// Bidirectional index:
/// * `subs[channel] -> {node_ids}` for `members(channel)` lookups.
/// * `by_peer[node_id] -> {channels}` for cheap `remove_peer` on failure.
///
/// The two indices can briefly disagree during concurrent updates; readers
/// that need a consistent snapshot should call `members()` which resolves
/// the forward index only.
#[derive(Default)]
pub struct SubscriberRoster {
    subs: DashMap<ChannelId, Arc<DashSet<u64>>>,
    by_peer: DashMap<u64, Arc<DashSet<ChannelId>>>,
}

impl SubscriberRoster {
    /// Create an empty roster.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add `node_id` as a subscriber of `channel`. Returns true if the
    /// pair was newly inserted, false if it was already present.
    ///
    /// Insertion into each inner `DashSet` happens **inside** the
    /// outer-map entry guard. A previous implementation cloned the
    /// inner `Arc` out of the guard first and then inserted; between
    /// those two steps a concurrent `remove()` on the same channel
    /// could observe an empty set and evict the outer entry via
    /// `remove_if`, leaving our cloned `Arc` orphaned — the
    /// subscription would appear in `by_peer` but never in
    /// `members(channel)`, silently breaking fan-out.
    pub fn add(&self, channel: ChannelId, node_id: u64) -> bool {
        let inserted = {
            let entry = self
                .subs
                .entry(channel.clone())
                .or_insert_with(|| Arc::new(DashSet::new()));
            entry.insert(node_id)
        };
        {
            let entry = self
                .by_peer
                .entry(node_id)
                .or_insert_with(|| Arc::new(DashSet::new()));
            entry.insert(channel);
        }
        inserted
    }

    /// Remove `node_id` from `channel`. Returns true if the pair was present.
    pub fn remove(&self, channel: &ChannelId, node_id: u64) -> bool {
        let removed = match self.subs.get(channel) {
            Some(set) => set.remove(&node_id).is_some(),
            None => false,
        };
        if let Some(peer_set) = self.by_peer.get(&node_id) {
            peer_set.remove(channel);
        }
        // Clean up empty shells so the roster doesn't leak per-channel entries
        // for ephemeral channels that churn through many subscribers.
        // The pre-check `if let Some + is_empty` was a TOCTOU window
        // closed only by `remove_if`'s atomic re-check of the
        // predicate — but the pre-check itself was load-bearing only
        // for skipping the call. `remove_if` already returns `None`
        // (no-op) when the predicate is false, so the unconditional
        // call is equivalent in correctness and harder to misread.
        // Pre-fix the pattern was idempotent but a future reader
        // could remove the `remove_if` predicate, thinking the outer
        // `is_empty` already covered the race.
        self.subs.remove_if(channel, |_, v| v.is_empty());
        self.by_peer.remove_if(&node_id, |_, v| v.is_empty());
        removed
    }

    /// Remove `node_id` from every channel it was subscribed to. Called by
    /// the failure-detector hook when a peer transitions to `Failed`. Returns
    /// the list of channels the peer was removed from, for diagnostics.
    pub fn remove_peer(&self, node_id: u64) -> Vec<ChannelId> {
        let arc_set = match self.by_peer.remove(&node_id) {
            Some((_, set)) => set,
            None => return Vec::new(),
        };
        // We just removed the only `by_peer` reference to this `Arc`.
        // Within this module the `by_peer` map is the sole owner —
        // `add()` and `remove()` are the only sites that touch the
        // `Arc<DashSet<ChannelId>>` and neither hands out clones — so
        // `try_unwrap` succeeds in the steady state and we can drain
        // the set into owned `ChannelId`s without per-element
        // `String` allocations. Fall back to the elementwise clone
        // path defensively in case a future caller hands out an
        // `Arc::clone` we didn't anticipate.
        let channels: Vec<ChannelId> = match Arc::try_unwrap(arc_set) {
            Ok(dashset) => dashset.into_iter().collect(),
            Err(arc) => arc.iter().map(|c| c.clone()).collect(),
        };
        for ch in &channels {
            if let Some(set) = self.subs.get(ch) {
                set.remove(&node_id);
            }
            self.subs.remove_if(ch, |_, v| v.is_empty());
        }
        channels
    }

    /// Snapshot of current subscribers for `channel`.
    pub fn members(&self, channel: &ChannelId) -> Vec<u64> {
        match self.subs.get(channel) {
            Some(set) => set.iter().map(|entry| *entry).collect(),
            None => Vec::new(),
        }
    }

    /// Snapshot of channels `node_id` subscribes to.
    pub fn channels_for(&self, node_id: u64) -> Vec<ChannelId> {
        match self.by_peer.get(&node_id) {
            Some(set) => set.iter().map(|entry| entry.clone()).collect(),
            None => Vec::new(),
        }
    }

    /// Number of distinct channels with at least one subscriber.
    pub fn channel_count(&self) -> usize {
        self.subs.len()
    }

    /// Number of distinct peers subscribed to at least one channel.
    pub fn peer_count(&self) -> usize {
        self.by_peer.len()
    }

    /// How many channels `node_id` is subscribed to. Used by per-peer
    /// channel cap enforcement on incoming `Subscribe`.
    pub fn channels_for_peer_count(&self, node_id: u64) -> usize {
        match self.by_peer.get(&node_id) {
            Some(set) => set.len(),
            None => 0,
        }
    }
}

impl std::fmt::Debug for SubscriberRoster {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SubscriberRoster")
            .field("channels", &self.subs.len())
            .field("peers", &self.by_peer.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ch(name: &str) -> ChannelId {
        ChannelId::parse(name).unwrap()
    }

    #[test]
    fn test_add_and_members() {
        let r = SubscriberRoster::new();
        let c = ch("sensors/lidar");

        assert!(r.add(c.clone(), 1));
        assert!(r.add(c.clone(), 2));
        // Re-adding the same pair is idempotent.
        assert!(!r.add(c.clone(), 1));

        let mut members = r.members(&c);
        members.sort();
        assert_eq!(members, vec![1, 2]);
    }

    #[test]
    fn test_remove() {
        let r = SubscriberRoster::new();
        let c = ch("sensors/lidar");
        r.add(c.clone(), 1);
        r.add(c.clone(), 2);

        assert!(r.remove(&c, 1));
        assert_eq!(r.members(&c), vec![2]);

        // Removing again is a no-op.
        assert!(!r.remove(&c, 1));

        // Removing the last subscriber cleans up the channel bucket.
        assert!(r.remove(&c, 2));
        assert_eq!(r.channel_count(), 0);
    }

    #[test]
    fn test_remove_peer_evicts_everywhere() {
        let r = SubscriberRoster::new();
        let a = ch("sensors/lidar");
        let b = ch("sensors/camera");
        r.add(a.clone(), 42);
        r.add(b.clone(), 42);
        r.add(a.clone(), 7);

        let channels = r.remove_peer(42);
        assert_eq!(channels.len(), 2);

        assert_eq!(r.members(&a), vec![7]);
        assert!(r.members(&b).is_empty());
        assert_eq!(r.channels_for_peer_count(42), 0);
    }

    #[test]
    fn test_channels_for() {
        let r = SubscriberRoster::new();
        let a = ch("a/b");
        let b = ch("c/d");
        r.add(a.clone(), 1);
        r.add(b.clone(), 1);

        let mut got: Vec<String> = r
            .channels_for(1)
            .into_iter()
            .map(|c| c.name().to_string())
            .collect();
        got.sort();
        assert_eq!(got, vec!["a/b", "c/d"]);
    }

    #[test]
    fn test_peer_count_and_channel_count() {
        let r = SubscriberRoster::new();
        assert_eq!(r.peer_count(), 0);
        assert_eq!(r.channel_count(), 0);

        let a = ch("a");
        let b = ch("b");
        r.add(a.clone(), 1);
        r.add(a.clone(), 2);
        r.add(b.clone(), 2);

        assert_eq!(r.peer_count(), 2);
        assert_eq!(r.channel_count(), 2);
        assert_eq!(r.channels_for_peer_count(2), 2);
    }

    #[test]
    fn test_remove_peer_unknown_is_noop() {
        let r = SubscriberRoster::new();
        let channels = r.remove_peer(99);
        assert!(channels.is_empty());
    }

    #[test]
    fn test_regression_concurrent_add_remove_same_channel_no_orphan() {
        // Regression (MEDIUM, BUGS.md): `add` used to clone the inner
        // `Arc<DashSet>` out of the entry guard before inserting the
        // member. A concurrent `remove(channel, other_node)` in the
        // narrow window between the two could observe the still-empty
        // inner set and evict the outer entry via `remove_if`,
        // orphaning our cloned Arc — the subscription showed up in
        // `by_peer` but was missing from `members(channel)`, silently
        // breaking fan-out.
        //
        // This test hammers `add(channel, N)` from many threads while
        // another thread tries to `remove(channel, 9999)` (a peer
        // that's never added) — which under the old code drove the
        // `remove_if` path that triggered the bug. After all adds
        // return, every inserted member must be visible in `members`.
        use std::sync::Arc as StdArc;
        use std::thread;

        let r = StdArc::new(SubscriberRoster::new());
        let ch = ch("race/target");
        const N: u64 = 200;

        let mut handles = Vec::new();

        // Adders: each inserts one distinct node_id.
        for i in 0..N {
            let r = StdArc::clone(&r);
            let ch = ch.clone();
            handles.push(thread::spawn(move || {
                r.add(ch, i);
            }));
        }

        // Remover: repeatedly tries to remove a peer that was never
        // added, which drives the `remove_if(is_empty)` path for any
        // momentarily-empty outer entry.
        for _ in 0..50 {
            let r = StdArc::clone(&r);
            let ch = ch.clone();
            handles.push(thread::spawn(move || {
                for _ in 0..100 {
                    let _ = r.remove(&ch, 9999);
                }
            }));
        }

        for h in handles {
            h.join().unwrap();
        }

        let members = r.members(&ch);
        for i in 0..N {
            assert!(
                members.contains(&i),
                "subscriber {i} must appear in members after concurrent add/remove; \
                 got {} members",
                members.len(),
            );
        }
    }
}
