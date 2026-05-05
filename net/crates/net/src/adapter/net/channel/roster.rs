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
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use super::name::ChannelId;

/// Named queue group identifier. Wraps `String` so it's a distinct
/// type from `ChannelId` at the API boundary.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct QueueGroupName(String);

impl QueueGroupName {
    /// Construct from any `Into<String>`. No syntactic restrictions
    /// today — the name is opaque routing metadata.
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }
    /// Borrow the underlying string. Useful for logs / metrics
    /// that want to tag dispatches with the group name.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for QueueGroupName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// How a subscriber wants to receive events from a channel.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SubscriptionMode {
    /// Existing behavior: every published event is delivered to
    /// this subscriber. Multiple `Broadcast` subscribers each
    /// receive an independent copy of every event. Right for
    /// pub/sub event-bus semantics.
    Broadcast,
    /// Work-distribution: every published event is delivered to
    /// exactly ONE subscriber across all peers in the named
    /// queue group. Multiple subscribers in the same
    /// `QueueGroup(name)` divide the stream amongst themselves.
    /// Right for request/response (nRPC) and any one-of-N
    /// job-distribution pattern.
    QueueGroup(QueueGroupName),
}

/// One queue group's state on a single channel.
struct QueueGroup {
    members: DashSet<u64>,
    /// Round-robin cursor. `select()` snapshots `members` into a
    /// Vec and returns `members[cursor.fetch_add(1) % vec.len()]`.
    /// `Relaxed` is sufficient — there's no happens-before edge
    /// the cursor needs to enforce; uneven distribution under
    /// reordering is a metric, not a correctness concern.
    cursor: AtomicUsize,
}

impl QueueGroup {
    fn new() -> Self {
        Self {
            members: DashSet::new(),
            cursor: AtomicUsize::new(0),
        }
    }

    /// Pick one member for this dispatch. Returns `None` if the
    /// group is empty. The selection is round-robin against a
    /// snapshot — concurrent membership changes don't poison the
    /// dispatch (they take effect on the next call).
    fn select(&self) -> Option<u64> {
        let snapshot: Vec<u64> = self.members.iter().map(|e| *e).collect();
        if snapshot.is_empty() {
            return None;
        }
        let idx = self.cursor.fetch_add(1, Ordering::Relaxed) % snapshot.len();
        Some(snapshot[idx])
    }
}

/// Per-channel subscriber set: a flat broadcast roster plus zero
/// or more named queue groups. Both flavors coexist on one channel
/// so a service with both an audit logger (`Broadcast`) and
/// load-balanced workers (`QueueGroup`) is naturally expressible.
struct ChannelSubscribers {
    broadcasters: DashSet<u64>,
    queue_groups: DashMap<QueueGroupName, QueueGroup>,
}

impl ChannelSubscribers {
    fn new() -> Self {
        Self {
            broadcasters: DashSet::new(),
            queue_groups: DashMap::new(),
        }
    }

    /// True if no broadcasters and every queue group is empty.
    /// The outer `subs` map evicts on this predicate to avoid
    /// leaking per-channel entries for ephemeral channels.
    fn is_empty(&self) -> bool {
        self.broadcasters.is_empty()
            && self
                .queue_groups
                .iter()
                .all(|e| e.value().members.is_empty())
    }

    /// All subscribers regardless of mode. The set-membership view
    /// (`SubscriberRoster::members`) uses this. Each peer appears
    /// once even if a future relaxation lets a peer be in
    /// multiple groups.
    fn all_subscribers(&self) -> Vec<u64> {
        let mut out: Vec<u64> = self.broadcasters.iter().map(|e| *e).collect();
        for grp in self.queue_groups.iter() {
            for m in grp.value().members.iter() {
                if !out.contains(&m) {
                    out.push(*m);
                }
            }
        }
        out
    }

    /// Per-publish dispatch view: every broadcaster, plus one
    /// selected member of each non-empty queue group. Per-publish
    /// queue-group selection is round-robin (see `QueueGroup::select`).
    fn dispatch_recipients(&self) -> Vec<u64> {
        let mut out: Vec<u64> = self.broadcasters.iter().map(|e| *e).collect();
        for grp in self.queue_groups.iter() {
            if let Some(picked) = grp.value().select() {
                if !out.contains(&picked) {
                    out.push(picked);
                }
            }
        }
        out
    }

    /// Mode under which `node_id` is subscribed to this channel,
    /// if any. Used by `remove` to know which inner container to
    /// touch and by diagnostics.
    fn mode_of(&self, node_id: u64) -> Option<SubscriptionMode> {
        if self.broadcasters.contains(&node_id) {
            return Some(SubscriptionMode::Broadcast);
        }
        for grp in self.queue_groups.iter() {
            if grp.value().members.contains(&node_id) {
                return Some(SubscriptionMode::QueueGroup(grp.key().clone()));
            }
        }
        None
    }

    /// Add `node_id` under `mode`. If the peer was previously
    /// subscribed under a different mode on this channel, that
    /// prior subscription is removed first (mode-change
    /// semantics — re-subscribing in the same mode is a no-op,
    /// re-subscribing under a different mode moves the peer).
    /// Returns `true` if the (peer, mode) pair is newly inserted,
    /// `false` if the peer was already subscribed under the same
    /// mode.
    fn add(&self, node_id: u64, mode: SubscriptionMode) -> bool {
        // Mode-change: clear any prior subscription on this channel
        // before inserting. The current-mode check is cheap because
        // most peers don't change modes.
        if let Some(prev) = self.mode_of(node_id) {
            if prev == mode {
                return false; // idempotent same-mode re-add
            }
            self.remove(node_id);
        }
        match mode {
            SubscriptionMode::Broadcast => self.broadcasters.insert(node_id),
            SubscriptionMode::QueueGroup(name) => {
                let grp = self
                    .queue_groups
                    .entry(name)
                    .or_insert_with(QueueGroup::new);
                grp.members.insert(node_id)
            }
        }
    }

    /// Remove `node_id` from whichever container it sits in.
    /// Returns `true` if the peer was present.
    ///
    /// Evicts the queue-group entry when its last member leaves.
    /// Without eviction, a peer that subscribes/unsubscribes under
    /// N distinct group names leaves N empty `QueueGroup` shells
    /// per channel — bounded only by attacker effort. The cost of
    /// evict-then-readd for a churning legit group is one cursor
    /// reset (round-robin restarts at the "first" member), which
    /// is acceptable because round-robin distribution is already
    /// best-effort.
    fn remove(&self, node_id: u64) -> bool {
        if self.broadcasters.remove(&node_id).is_some() {
            return true;
        }
        // Find the group that contains the peer, remove the peer,
        // remember the group name if it just became empty.
        let mut now_empty: Option<QueueGroupName> = None;
        let mut found = false;
        for grp in self.queue_groups.iter() {
            if grp.value().members.remove(&node_id).is_some() {
                found = true;
                if grp.value().members.is_empty() {
                    now_empty = Some(grp.key().clone());
                }
                break;
            }
        }
        if let Some(name) = now_empty {
            // Re-check inside the conditional remove in case a
            // concurrent `add_with_mode` raced our removal and
            // re-populated the group between our `is_empty()` and
            // the eviction below. Only evict if STILL empty.
            self.queue_groups
                .remove_if(&name, |_, g| g.members.is_empty());
        }
        found
    }
}

/// Subscriber roster keyed by `ChannelId`.
///
/// Bidirectional index:
/// * `subs[channel] -> ChannelSubscribers` for `members(channel)` /
///   `dispatch_recipients(channel)` lookups.
/// * `by_peer[node_id] -> {channels}` for cheap `remove_peer` on failure.
///
/// The two indices can briefly disagree during concurrent updates; readers
/// that need a consistent snapshot should call `members()` which resolves
/// the forward index only.
#[derive(Default)]
pub struct SubscriberRoster {
    subs: DashMap<ChannelId, Arc<ChannelSubscribers>>,
    by_peer: DashMap<u64, Arc<DashSet<ChannelId>>>,
}

impl SubscriberRoster {
    /// Create an empty roster.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add `node_id` as a `Broadcast` subscriber of `channel`.
    /// Back-compat shim around [`Self::add_with_mode`]; existing
    /// callers that don't yet care about queue groups continue to
    /// get the current behavior. Returns `true` if newly inserted,
    /// `false` if the peer was already subscribed in the same mode.
    pub fn add(&self, channel: ChannelId, node_id: u64) -> bool {
        self.add_with_mode(channel, node_id, SubscriptionMode::Broadcast)
    }

    /// Add `node_id` as a subscriber of `channel` under the given
    /// `mode`. Returns `true` if the (peer, mode) pair is newly
    /// inserted, `false` if the peer was already subscribed in the
    /// same mode (idempotent re-add).
    ///
    /// **Mode-change semantics.** If the peer was previously
    /// subscribed to this channel under a different mode (e.g.
    /// `Broadcast` and now `QueueGroup("workers")`, or moving
    /// between groups), the prior subscription is removed first
    /// and the new one inserted. The peer is in exactly one mode
    /// per channel at any time.
    ///
    /// **Orphan-prevention.** The mutation of the inner
    /// `ChannelSubscribers` (insert into broadcasters or into a
    /// queue group's member set) happens **inside** the outer-map
    /// entry guard. A previous implementation cloned the inner
    /// `Arc` out of the guard before mutating; between those two
    /// steps a concurrent `remove()` on the same channel could
    /// observe an empty set and evict the outer entry via
    /// `remove_if`, leaving our cloned `Arc` orphaned — the
    /// subscription would appear in `by_peer` but never in
    /// `members(channel)`, silently breaking fan-out. Keeping the
    /// inner mutation under the entry guard closes that race.
    pub fn add_with_mode(&self, channel: ChannelId, node_id: u64, mode: SubscriptionMode) -> bool {
        let inserted = {
            let entry = self
                .subs
                .entry(channel.clone())
                .or_insert_with(|| Arc::new(ChannelSubscribers::new()));
            entry.add(node_id, mode)
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

    /// Remove `node_id` from `channel`, regardless of mode. Returns
    /// `true` if the pair was present. Caller doesn't have to know
    /// whether the peer was a `Broadcast` subscriber or a member of
    /// some queue group — `remove` finds whichever it was.
    pub fn remove(&self, channel: &ChannelId, node_id: u64) -> bool {
        let removed = match self.subs.get(channel) {
            Some(subs) => subs.remove(node_id),
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
        //
        // `ChannelSubscribers::is_empty` is "no broadcasters AND
        // every queue group is empty" — the channel-level eviction
        // semantic is unchanged from the pre-queue-group shape.
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
            if let Some(subs) = self.subs.get(ch) {
                subs.remove(node_id);
            }
            self.subs.remove_if(ch, |_, v| v.is_empty());
        }
        channels
    }

    /// Snapshot of current subscribers for `channel`, regardless of
    /// mode. Each peer appears at most once. This is the **set
    /// membership** view — used by anything that asks "is this peer
    /// subscribed?" or counts subscribers. The per-publish dispatch
    /// view (broadcasters + one-of-N per queue group) is
    /// [`Self::dispatch_recipients`].
    pub fn members(&self, channel: &ChannelId) -> Vec<u64> {
        match self.subs.get(channel) {
            Some(subs) => subs.all_subscribers(),
            None => Vec::new(),
        }
    }

    /// Per-publish dispatch list for `channel`: every `Broadcast`
    /// subscriber, plus one selected member of each non-empty queue
    /// group. Each peer appears at most once. The publisher iterates
    /// this list and unicasts to each recipient.
    ///
    /// Selection inside each queue group is round-robin against a
    /// snapshot of the group's members — concurrent membership
    /// changes don't poison this dispatch (they take effect on the
    /// next call).
    pub fn dispatch_recipients(&self, channel: &ChannelId) -> Vec<u64> {
        match self.subs.get(channel) {
            Some(subs) => subs.dispatch_recipients(),
            None => Vec::new(),
        }
    }

    /// Mode under which `node_id` is subscribed to `channel`, if
    /// any. Used for diagnostics and by code that needs to
    /// distinguish broadcast subscribers from queue-group members
    /// (e.g., the membership-change handler that surfaces a
    /// `QueueGroup` event back to the originating peer).
    pub fn subscriber_mode(&self, node_id: u64, channel: &ChannelId) -> Option<SubscriptionMode> {
        self.subs.get(channel).and_then(|s| s.mode_of(node_id))
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

    /// True if `node_id` is currently subscribed to `channel`. Used by
    /// `MeshNode::authorize_subscribe` to gate the per-peer channel
    /// cap check: an idempotent re-subscribe (already in the roster)
    /// suppresses the `TooManyChannels` rejection only — pre-fix
    /// a peer at the cap that retransmitted a Subscribe for a channel
    /// it already held was rejected with `TooManyChannels`, even though
    /// the underlying `add` is set-typed and the operation would have
    /// been a no-op. The visibility / registry / token gates further
    /// down the auth chain still run on every Subscribe, so a re-emitted
    /// Subscribe with a now-revoked token or under tightened visibility
    /// rejects the same way a fresh Subscribe would.
    pub fn is_subscribed(&self, node_id: u64, channel: &ChannelId) -> bool {
        match self.by_peer.get(&node_id) {
            Some(set) => set.contains(channel),
            None => false,
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

    /// Regression for `BUG_AUDIT_2026_05_03_MESH.md` #5: the
    /// idempotent re-subscribe path in `MeshNode::authorize_subscribe`
    /// calls `SubscriberRoster::is_subscribed` to suppress the
    /// per-peer channel cap rejection. Pin the predicate here so a
    /// future reshuffling of the bidirectional index can't silently
    /// break the suppression (which would re-introduce the
    /// `TooManyChannels` rejection of a no-op re-subscribe at the
    /// cap).
    #[test]
    fn is_subscribed_returns_true_for_existing_pair_and_false_otherwise() {
        let r = SubscriberRoster::new();
        let a = ch("sensors/lidar");
        let b = ch("sensors/camera");

        // Empty roster.
        assert!(!r.is_subscribed(1, &a));

        // Add (1, a) — only that pair returns true.
        r.add(a.clone(), 1);
        assert!(r.is_subscribed(1, &a));
        assert!(!r.is_subscribed(1, &b));
        assert!(!r.is_subscribed(2, &a));

        // remove evicts the pair.
        r.remove(&a, 1);
        assert!(!r.is_subscribed(1, &a));
    }

    /// Mirrors the authorize_subscribe production logic for the
    /// cap-check arm. The cap rejection fires only when the peer is
    /// NOT already subscribed AND is at the cap; an already-subscribed
    /// peer at the cap is admitted past the cap check (and then
    /// continues into the visibility / registry / token gates further
    /// down the chain — those are not modeled here, but their
    /// independent enforcement is the reason this test deliberately
    /// does NOT assert `(true, None)` for the at-cap re-subscribe).
    /// Pre-fix, the cap check ran unconditionally and the at-cap
    /// re-subscribe was rejected with `TooManyChannels` even though
    /// `add` would have been a no-op. The combined predicate pinned
    /// here matches the new dispatch order:
    ///
    /// ```text
    /// !is_subscribed(node, ch) && channels_for_peer_count(node) >= cap
    /// ```
    #[test]
    fn cap_rejection_is_suppressed_only_for_already_subscribed_peers() {
        let r = SubscriberRoster::new();
        let cap = 3usize;

        for i in 0..cap {
            r.add(ch(&format!("ch/{}", i)), 42);
        }
        assert_eq!(r.channels_for_peer_count(42), cap);

        // Helper mirroring the production combined predicate.
        let cap_would_reject = |node: u64, channel: &ChannelId| -> bool {
            !r.is_subscribed(node, channel) && r.channels_for_peer_count(node) >= cap
        };

        // At-cap re-subscribe of an already-held channel: cap
        // rejection is suppressed. (The production path then runs
        // visibility / registry / token validation; if those pass,
        // the peer is admitted. If they don't, the peer is rejected
        // for the right reason — not for `TooManyChannels`.)
        let already = ch("ch/0");
        assert!(
            !cap_would_reject(42, &already),
            "regression: an at-cap re-subscribe to a channel the peer \
             already holds must NOT be cap-rejected — `add` is set-typed \
             and the operation is a no-op."
        );

        // Genuinely new channel at cap: cap rejection still fires.
        let fresh = ch("ch/new");
        assert!(
            cap_would_reject(42, &fresh),
            "regression: a new channel at cap must still hit the \
             TooManyChannels rejection — the suppression is keyed on \
             already-subscribed pairs only, otherwise the cap is moot."
        );

        // Under-cap peer (not at cap, not already subscribed): cap
        // rejection does not fire — the under-cap fall-through still
        // exercises the rest of the auth chain.
        let under_cap_peer = 99u64;
        assert_eq!(r.channels_for_peer_count(under_cap_peer), 0);
        assert!(!cap_would_reject(under_cap_peer, &fresh));
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

    // ====================================================================
    // SubscriptionMode::QueueGroup — work-distribution semantics.
    //
    // These pin the new primitive that nRPC needs: queue-group
    // subscribers divide a stream of events among themselves
    // (one-of-N), broadcast subscribers each receive every event
    // (all-of-N), and the two coexist on one channel without
    // interfering.
    // ====================================================================

    fn qg(name: &str) -> SubscriptionMode {
        SubscriptionMode::QueueGroup(QueueGroupName::new(name))
    }

    /// Backward compat: existing callers using plain `add()` get
    /// `Broadcast` semantics, identical to pre-change behavior.
    /// `members()` still returns "all subscribers."
    #[test]
    fn add_without_mode_is_broadcast() {
        let r = SubscriberRoster::new();
        let c = ch("svc/req");
        r.add(c.clone(), 1);
        r.add(c.clone(), 2);
        assert_eq!(
            r.subscriber_mode(1, &c),
            Some(SubscriptionMode::Broadcast),
            "default add must be Broadcast",
        );
        assert_eq!(r.subscriber_mode(2, &c), Some(SubscriptionMode::Broadcast));
        let mut members = r.members(&c);
        members.sort();
        assert_eq!(members, vec![1, 2]);
        // dispatch_recipients matches members for broadcast-only channels.
        let mut dispatched = r.dispatch_recipients(&c);
        dispatched.sort();
        assert_eq!(dispatched, vec![1, 2]);
    }

    /// Two subscribers in the same queue group: each call to
    /// `dispatch_recipients` returns exactly ONE of them. Across
    /// many calls the round-robin selector visits both.
    #[test]
    fn queue_group_dispatch_picks_one_member_per_call() {
        let r = SubscriberRoster::new();
        let c = ch("svc/req");
        r.add_with_mode(c.clone(), 1, qg("workers"));
        r.add_with_mode(c.clone(), 2, qg("workers"));

        // Each individual dispatch picks exactly one of the two.
        let mut counts = [0usize; 3];
        for _ in 0..20 {
            let picks = r.dispatch_recipients(&c);
            assert_eq!(
                picks.len(),
                1,
                "queue group must produce exactly one recipient per dispatch",
            );
            counts[picks[0] as usize] += 1;
        }
        assert!(
            counts[1] > 0 && counts[2] > 0,
            "round-robin must visit both members across 20 dispatches; got {counts:?}",
        );
    }

    /// Broadcast and queue-group subscribers coexist on one
    /// channel: every dispatch reaches every broadcaster AND
    /// exactly one member of each queue group. This is the shape
    /// nRPC needs when an audit logger broadcasts alongside
    /// load-balanced workers.
    #[test]
    fn broadcast_and_queue_group_coexist_on_one_channel() {
        let r = SubscriberRoster::new();
        let c = ch("svc/req");
        // 1 = audit logger (Broadcast)
        r.add_with_mode(c.clone(), 1, SubscriptionMode::Broadcast);
        // 10, 11, 12 = worker pool (QueueGroup)
        r.add_with_mode(c.clone(), 10, qg("workers"));
        r.add_with_mode(c.clone(), 11, qg("workers"));
        r.add_with_mode(c.clone(), 12, qg("workers"));

        for _ in 0..30 {
            let picks = r.dispatch_recipients(&c);
            // The broadcaster (1) must always be in the dispatch list.
            assert!(picks.contains(&1), "broadcaster must always receive");
            // Exactly one worker (10/11/12) must also be in the list.
            let workers_in_dispatch: Vec<u64> =
                picks.iter().copied().filter(|n| *n >= 10).collect();
            assert_eq!(
                workers_in_dispatch.len(),
                1,
                "exactly one queue-group member per dispatch; got {workers_in_dispatch:?}",
            );
        }

        // Set membership view: members() returns ALL subscribers,
        // not just dispatch picks.
        let mut all = r.members(&c);
        all.sort();
        assert_eq!(all, vec![1, 10, 11, 12]);
    }

    /// Two distinct queue groups on the same channel each
    /// independently pick one member per dispatch. Useful for
    /// patterns like "request workers" + "audit shippers" where
    /// both want one-of-N but they're disjoint pools.
    #[test]
    fn distinct_queue_groups_dispatch_independently() {
        let r = SubscriberRoster::new();
        let c = ch("svc/req");
        r.add_with_mode(c.clone(), 1, qg("group_a"));
        r.add_with_mode(c.clone(), 2, qg("group_a"));
        r.add_with_mode(c.clone(), 100, qg("group_b"));
        r.add_with_mode(c.clone(), 101, qg("group_b"));

        for _ in 0..20 {
            let picks = r.dispatch_recipients(&c);
            // Exactly one from each group.
            let from_a = picks.iter().filter(|&&n| n < 10).count();
            let from_b = picks.iter().filter(|&&n| n >= 100).count();
            assert_eq!(from_a, 1, "exactly one from group_a per dispatch");
            assert_eq!(from_b, 1, "exactly one from group_b per dispatch");
            assert_eq!(picks.len(), 2, "no other recipients");
        }
    }

    /// Mode-change: re-subscribing the same peer under a different
    /// mode moves them. The peer must end up in the new mode and
    /// not appear in the old one. Returns `true` (newly inserted in
    /// the new mode); same-mode re-add returns `false`.
    #[test]
    fn re_add_with_different_mode_moves_subscription() {
        let r = SubscriberRoster::new();
        let c = ch("svc/req");
        // Start as broadcaster.
        assert!(r.add_with_mode(c.clone(), 7, SubscriptionMode::Broadcast));
        assert_eq!(r.subscriber_mode(7, &c), Some(SubscriptionMode::Broadcast));

        // Move to a queue group: returns true (mode-change is a real insert).
        assert!(r.add_with_mode(c.clone(), 7, qg("workers")));
        assert_eq!(r.subscriber_mode(7, &c), Some(qg("workers")));

        // Re-add to the same group: idempotent, returns false.
        assert!(!r.add_with_mode(c.clone(), 7, qg("workers")));

        // The peer appears exactly once in `members` (set membership).
        assert_eq!(r.members(&c), vec![7]);
        // And exactly once in `dispatch_recipients` (one-of-N from
        // the workers group, which has only 7 in it).
        assert_eq!(r.dispatch_recipients(&c), vec![7]);
    }

    /// `remove` finds the peer regardless of which mode they're in.
    /// Channel eviction still fires when the channel goes fully
    /// empty (broadcasters AND every queue group empty).
    #[test]
    fn remove_finds_peer_in_either_mode_and_evicts_empty_channel() {
        let r = SubscriberRoster::new();
        let c = ch("svc/req");
        r.add_with_mode(c.clone(), 1, SubscriptionMode::Broadcast);
        r.add_with_mode(c.clone(), 2, qg("workers"));

        // Remove the broadcaster.
        assert!(r.remove(&c, 1));
        assert_eq!(r.members(&c), vec![2]);
        // Channel still present (queue-group member remains).
        assert_eq!(r.channel_count(), 1);

        // Remove the last queue-group member.
        assert!(r.remove(&c, 2));
        // Channel evicted.
        assert_eq!(r.channel_count(), 0);

        // Removing an absent peer is a no-op.
        assert!(!r.remove(&c, 1));
    }

    /// Regression: when the last member of a queue group leaves,
    /// the QueueGroup entry itself is evicted from the
    /// `queue_groups` map. Without this, a peer that subscribes /
    /// unsubscribes under N distinct group names leaves N empty
    /// shells per channel — bounded only by attacker effort. The
    /// `is_empty` channel-eviction predicate also depends on
    /// post-removal cleanup so a churning channel doesn't leak
    /// unbounded empty groups.
    #[test]
    fn empty_queue_groups_are_evicted_on_last_member_leaving() {
        let r = SubscriberRoster::new();
        let c = ch("svc/req");
        // Three distinct group names, one subscriber each.
        r.add_with_mode(c.clone(), 1, qg("group-a"));
        r.add_with_mode(c.clone(), 2, qg("group-b"));
        r.add_with_mode(c.clone(), 3, qg("group-c"));

        // Probe the internal map size via a channel-level helper —
        // we can't borrow the internal DashMap directly, so check
        // `dispatch_recipients` which iterates `queue_groups`. With
        // 3 groups, dispatch returns 3 recipients (one per group).
        assert_eq!(r.dispatch_recipients(&c).len(), 3);

        // Remove all three. After the last removal the channel
        // entry itself should be evicted.
        assert!(r.remove(&c, 1));
        assert!(r.remove(&c, 2));
        assert!(r.remove(&c, 3));
        // No queue-group shells left → channel is fully empty →
        // outer `subs` map evicts the channel entry.
        assert_eq!(r.channel_count(), 0);

        // Re-add a fresh subscriber with one of the previously-used
        // group names: succeeds (the prior shell was evicted, so we
        // start clean) and the channel reappears.
        assert!(r.add_with_mode(c.clone(), 1, qg("group-a")));
        assert_eq!(r.channel_count(), 1);
        assert_eq!(r.dispatch_recipients(&c), vec![1]);
    }

    /// `remove_peer` (failure-driven cleanup) clears the peer from
    /// every channel they were subscribed to, regardless of mode on
    /// each. Pin so a future change to `ChannelSubscribers::remove`
    /// can't quietly miss queue-group entries.
    #[test]
    fn remove_peer_clears_across_modes() {
        let r = SubscriberRoster::new();
        let a = ch("a/svc");
        let b = ch("b/svc");
        r.add_with_mode(a.clone(), 42, SubscriptionMode::Broadcast);
        r.add_with_mode(b.clone(), 42, qg("workers"));
        r.add_with_mode(a.clone(), 7, SubscriptionMode::Broadcast);

        let cleared = r.remove_peer(42);
        assert_eq!(cleared.len(), 2);
        assert_eq!(r.members(&a), vec![7]);
        assert!(r.members(&b).is_empty());
        assert_eq!(r.channels_for_peer_count(42), 0);
    }

    /// The set-membership cap (`channels_for_peer_count`) counts a
    /// peer's subscriptions across modes. A peer subscribed to 3
    /// channels — one Broadcast, two QueueGroup — counts as 3
    /// against the per-peer channel cap. Pin so the cap stays
    /// mode-agnostic (it's a resource cap on subscription state,
    /// not a "broadcast budget").
    #[test]
    fn channels_for_peer_count_aggregates_across_modes() {
        let r = SubscriberRoster::new();
        r.add_with_mode(ch("a/x"), 1, SubscriptionMode::Broadcast);
        r.add_with_mode(ch("b/x"), 1, qg("g1"));
        r.add_with_mode(ch("c/x"), 1, qg("g2"));
        assert_eq!(r.channels_for_peer_count(1), 3);
    }

    /// The idempotent-resubscribe cap-suppression that mesh.rs
    /// `authorize_subscribe` relies on (`is_subscribed` returning
    /// true for any mode the peer is in) must still work for
    /// queue-group peers. A peer at the per-peer channel cap that
    /// resubscribes to a channel they already hold (in any mode)
    /// must NOT trip `TooManyChannels`.
    #[test]
    fn is_subscribed_returns_true_regardless_of_mode() {
        let r = SubscriberRoster::new();
        let c = ch("svc/req");
        r.add_with_mode(c.clone(), 1, SubscriptionMode::Broadcast);
        r.add_with_mode(c.clone(), 2, qg("workers"));
        assert!(r.is_subscribed(1, &c));
        assert!(r.is_subscribed(2, &c));
        assert!(!r.is_subscribed(3, &c));
    }
}
