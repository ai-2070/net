//! Subnet gateway — the causal membrane at subnet boundaries.
//!
//! A gateway node sits at the boundary between subnets and enforces
//! visibility policy. It reads header fields (no decryption) to make
//! forward/drop decisions. Encrypted payloads pass through untouched.

use std::sync::Arc;

use dashmap::DashMap;

use super::id::SubnetId;
use crate::adapter::net::channel::{ChannelConfigRegistry, Visibility};

/// Reason a packet was dropped at a gateway.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropReason {
    /// Channel is SubnetLocal — never crosses boundaries.
    SubnetLocal,
    /// Channel is ParentVisible but destination is not an ancestor.
    NotAncestor,
    /// Channel is Exported but destination is not in the export table.
    NotExported,
    /// Packet's subnet_id doesn't match any known subnet.
    UnknownSubnet,
    /// TTL expired.
    TtlExpired,
}

impl std::fmt::Display for DropReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SubnetLocal => write!(f, "channel is subnet-local"),
            Self::NotAncestor => write!(f, "destination is not ancestor of source"),
            Self::NotExported => write!(f, "channel not exported to destination subnet"),
            Self::UnknownSubnet => write!(f, "unknown subnet"),
            Self::TtlExpired => write!(f, "TTL expired"),
        }
    }
}

/// Gateway forwarding decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForwardDecision {
    /// Packet should be forwarded.
    Forward,
    /// Packet should be dropped.
    Drop(DropReason),
}

/// Subnet gateway that enforces visibility policy at subnet boundaries.
///
/// The gateway reads only header fields — it does not decrypt or modify
/// packet payloads. This is the "causal membrane" that filters traffic
/// between subnets.
pub struct SubnetGateway {
    /// This gateway's subnet.
    local_subnet: SubnetId,
    /// Known peer subnets this gateway bridges to. Stored in a
    /// `parking_lot::RwLock` so `add_peer` can mutate it through
    /// an `&self` handle the same way `export_channel` already
    /// mutates the export table — lets `MeshNode` keep its
    /// gateway behind an `Arc` without an outer `Mutex`.
    peer_subnets: parking_lot::RwLock<Vec<SubnetId>>,
    /// Export table: channel_hash -> allowed destination subnets.
    /// Only consulted for `Visibility::Exported` channels.
    export_table: DashMap<u16, Vec<SubnetId>>,
    /// Channel config registry for looking up visibility. Shared
    /// `Arc` so the gateway sees the same registry the host
    /// `MeshNode` mutates through `set_channel_configs` /
    /// `insert` — without this, the gateway's view would drift
    /// from the substrate's actual config.
    channel_configs: Arc<ChannelConfigRegistry>,
    /// Gateway stats.
    forwarded: std::sync::atomic::AtomicU64,
    dropped: std::sync::atomic::AtomicU64,
}

impl SubnetGateway {
    /// Create a new gateway for a subnet, sharing the supplied
    /// `ChannelConfigRegistry` with the host. The registry is held
    /// behind an `Arc` so subsequent inserts on the substrate side
    /// flow through to gateway visibility lookups.
    pub fn new(local_subnet: SubnetId, channel_configs: Arc<ChannelConfigRegistry>) -> Self {
        Self {
            local_subnet,
            peer_subnets: parking_lot::RwLock::new(Vec::new()),
            export_table: DashMap::new(),
            channel_configs,
            forwarded: std::sync::atomic::AtomicU64::new(0),
            dropped: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Add a peer subnet this gateway bridges to. Idempotent —
    /// re-registering the same subnet is a no-op. The Vec is
    /// kept sorted by raw bits on insert so [`Self::peer_subnets`]
    /// can return a plain clone without re-sorting.
    pub fn add_peer(&self, subnet: SubnetId) {
        let mut peers = self.peer_subnets.write();
        if let Err(pos) = peers.binary_search_by_key(&subnet.raw(), |s| s.raw()) {
            peers.insert(pos, subnet);
        }
    }

    /// Snapshot of every peer subnet currently bridged to,
    /// sorted by raw bits for stable operator-tool output.
    pub fn peer_subnets(&self) -> Vec<SubnetId> {
        self.peer_subnets.read().clone()
    }

    /// Export a channel to specific subnets.
    pub fn export_channel(&self, channel_hash: u16, targets: Vec<SubnetId>) {
        self.export_table.insert(channel_hash, targets);
    }

    /// Snapshot of the export table as `(channel_hash, targets)`
    /// pairs, sorted by `channel_hash` for stable output. Used by
    /// operator tooling (`net gateway exports`) to render the
    /// current set of explicit cross-subnet allow-rules.
    pub fn exports(&self) -> Vec<(u16, Vec<SubnetId>)> {
        let mut out: Vec<(u16, Vec<SubnetId>)> = self
            .export_table
            .iter()
            .map(|e| (*e.key(), e.value().clone()))
            .collect();
        out.sort_by_key(|(hash, _)| *hash);
        out
    }

    /// Look up the export targets for a single `channel_hash`, or
    /// `None` if the channel is not in the export table. Used by
    /// `net gateway export <channel>` to render the current
    /// allow-list before an operator mutates it.
    pub fn exports_for_channel(&self, channel_hash: u16) -> Option<Vec<SubnetId>> {
        self.export_table.get(&channel_hash).map(|e| e.value().clone())
    }

    /// Record a forward decision that bypassed the gateway's
    /// own `should_forward` entrypoint (e.g. an inline
    /// publish-fanout visibility check on `MeshNode`). Lets
    /// gateway counters reflect every visibility decision the
    /// host node makes, not just the ones routed through
    /// `should_forward`.
    pub fn record_forward(&self) {
        self.forwarded
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    /// Companion to [`Self::record_forward`] — drops a packet
    /// the host visibility check rejected.
    pub fn record_drop(&self, _reason: DropReason) {
        self.dropped
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    /// Get this gateway's local subnet.
    #[inline]
    pub fn local_subnet(&self) -> SubnetId {
        self.local_subnet
    }

    /// Make a forwarding decision for a packet crossing this gateway.
    ///
    /// Reads only header fields: `subnet_id`, `channel_hash`, `hop_ttl`, `hop_count`.
    /// No decryption, no payload inspection.
    pub fn should_forward(
        &self,
        source_subnet: SubnetId,
        dest_subnet: SubnetId,
        channel_hash: u16,
        hop_ttl: u8,
        hop_count: u8,
    ) -> ForwardDecision {
        // TTL check.
        //
        // Treating `hop_ttl == 0` as "expired" is critical:
        // `NetHeader::new` defaults `hop_ttl` to 0 and `hop_count`
        // is excluded from AAD (mutable in transit), so a malicious
        // or buggy peer could craft `hop_ttl=0` packets that loop
        // through gateways with no Net-layer bound. Routing-layer
        // TTL still bounds end-to-end loops for routed packets, but
        // pure subnet-gateway forwarding paths (no routing header)
        // would have no cap. Any header that hasn't explicitly set
        // `hop_ttl` via `NetHeader::with_hops(ttl)` is dropped at
        // the gateway.
        if hop_ttl == 0 || hop_count >= hop_ttl {
            self.dropped
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            return ForwardDecision::Drop(DropReason::TtlExpired);
        }

        // Look up channel visibility by the wire `u16` hash — that's
        // what the inbound packet header carries here. `get_by_wire_hash`
        // returns `None` both for unknown channels and on wire-bucket
        // collisions. In either case the gateway cannot prove the
        // channel is allowed to cross a subnet boundary, so we must
        // drop rather than forward. Defaulting to `Global` would
        // silently leak traffic when a `SubnetLocal` channel collides
        // with any other config.
        let visibility = self
            .channel_configs
            .get_by_wire_hash(channel_hash)
            .map(|c| c.visibility)
            .unwrap_or(Visibility::SubnetLocal);

        let decision = match visibility {
            Visibility::SubnetLocal => ForwardDecision::Drop(DropReason::SubnetLocal),

            Visibility::ParentVisible => {
                // Per the channel-config doc: "Visible to the parent
                // subnet but not siblings." Traffic flows from a
                // child up to its ancestor — i.e., dest must be a
                // (strict or non-strict) ancestor of source.
                // Forwarding the other direction (parent broadcasts
                // *down* to descendants) violates the
                // principle-of-least-privilege framing and silently
                // leaks region-scoped traffic into every fleet /
                // vehicle below it.
                if dest_subnet.is_ancestor_of(source_subnet) {
                    ForwardDecision::Forward
                } else {
                    ForwardDecision::Drop(DropReason::NotAncestor)
                }
            }

            Visibility::Exported => {
                if let Some(targets) = self.export_table.get(&channel_hash) {
                    if targets
                        .iter()
                        .any(|t| t.is_same_subnet(dest_subnet) || t.is_ancestor_of(dest_subnet))
                    {
                        ForwardDecision::Forward
                    } else {
                        ForwardDecision::Drop(DropReason::NotExported)
                    }
                } else {
                    ForwardDecision::Drop(DropReason::NotExported)
                }
            }

            Visibility::Global => ForwardDecision::Forward,
        };

        match decision {
            ForwardDecision::Forward => {
                self.forwarded
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
            ForwardDecision::Drop(_) => {
                self.dropped
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        }

        decision
    }

    /// Get the number of forwarded packets.
    pub fn forwarded_count(&self) -> u64 {
        self.forwarded.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Get the number of dropped packets.
    pub fn dropped_count(&self) -> u64 {
        self.dropped.load(std::sync::atomic::Ordering::Relaxed)
    }
}

impl std::fmt::Debug for SubnetGateway {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SubnetGateway")
            .field("local_subnet", &self.local_subnet)
            .field("peer_subnets", &self.peer_subnets)
            .field("exports", &self.export_table.len())
            .field("forwarded", &self.forwarded_count())
            .field("dropped", &self.dropped_count())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::net::channel::{ChannelConfig, ChannelId};

    use crate::adapter::net::channel::ChannelName;

    fn make_channel(name: &str, vis: Visibility, reg: &ChannelConfigRegistry) -> u16 {
        let id = ChannelId::new(ChannelName::new(name).unwrap());
        // `should_forward` operates on the wire `u16` hash (that's
        // what the packet header carries); return the wire hash for
        // the gateway-side test exercises.
        let wire = id.wire_hash();
        reg.insert(ChannelConfig::new(id).with_visibility(vis));
        wire
    }

    /// Default `hop_ttl` for tests that aren't testing TTL itself.
    /// Post-#88, `hop_ttl == 0` is treated as expired by the
    /// gateway, so non-TTL tests must pass a non-zero value to
    /// avoid short-circuiting on the TTL check.
    const TEST_TTL: u8 = 8;

    #[test]
    fn test_global_always_forwards() {
        let reg = Arc::new(ChannelConfigRegistry::new());
        let ch = make_channel("test/global", Visibility::Global, &reg);
        let gw = SubnetGateway::new(SubnetId::new(&[1]), reg);

        let decision = gw.should_forward(
            SubnetId::new(&[1, 1]),
            SubnetId::new(&[2, 1]),
            ch,
            TEST_TTL,
            0,
        );
        assert_eq!(decision, ForwardDecision::Forward);
    }

    #[test]
    fn test_subnet_local_always_drops() {
        let reg = Arc::new(ChannelConfigRegistry::new());
        let ch = make_channel("test/local", Visibility::SubnetLocal, &reg);
        let gw = SubnetGateway::new(SubnetId::new(&[1]), reg);

        let decision = gw.should_forward(
            SubnetId::new(&[1, 1]),
            SubnetId::new(&[1, 2]),
            ch,
            TEST_TTL,
            0,
        );
        assert_eq!(decision, ForwardDecision::Drop(DropReason::SubnetLocal));
    }

    #[test]
    fn test_parent_visible_allows_ancestor() {
        let reg = Arc::new(ChannelConfigRegistry::new());
        let ch = make_channel("test/parent-vis", Visibility::ParentVisible, &reg);
        let gw = SubnetGateway::new(SubnetId::new(&[1]), reg);

        // Child to parent — allowed
        let decision =
            gw.should_forward(SubnetId::new(&[1, 2]), SubnetId::new(&[1]), ch, TEST_TTL, 0);
        assert_eq!(decision, ForwardDecision::Forward);

        // Sibling to sibling — not allowed
        let decision = gw.should_forward(
            SubnetId::new(&[1, 2]),
            SubnetId::new(&[1, 3]),
            ch,
            TEST_TTL,
            0,
        );
        assert_eq!(decision, ForwardDecision::Drop(DropReason::NotAncestor));
    }

    /// Pin: `ParentVisible` is "visible to the parent subnet but not
    /// siblings" — strictly upward. A parent broadcast must NOT be
    /// forwarded *down* into descendants (that would leak parent-
    /// scoped traffic into every child fleet / vehicle, breaking the
    /// principle-of-least-privilege framing). Pre-fix the predicate
    /// accepted both `dest.is_ancestor_of(source)` (correct) and
    /// `source.is_ancestor_of(dest)` (incorrect downward leak).
    #[test]
    fn parent_visible_drops_parent_to_descendant() {
        let reg = Arc::new(ChannelConfigRegistry::new());
        let ch = make_channel("test/parent-down", Visibility::ParentVisible, &reg);
        let gw = SubnetGateway::new(SubnetId::new(&[1]), reg);

        // Parent → child must drop.
        let decision =
            gw.should_forward(SubnetId::new(&[1]), SubnetId::new(&[1, 2]), ch, TEST_TTL, 0);
        assert_eq!(
            decision,
            ForwardDecision::Drop(DropReason::NotAncestor),
            "parent → descendant must NOT be forwarded under ParentVisible \
             — `ParentVisible` is unidirectional (child → ancestor only)"
        );

        // Grandparent → grandchild also blocked.
        let decision = gw.should_forward(
            SubnetId::new(&[1]),
            SubnetId::new(&[1, 2, 3]),
            ch,
            TEST_TTL,
            0,
        );
        assert_eq!(
            decision,
            ForwardDecision::Drop(DropReason::NotAncestor),
            "ancestor → distant-descendant must drop too"
        );
    }

    #[test]
    fn test_exported_channel() {
        let reg = Arc::new(ChannelConfigRegistry::new());
        let ch = make_channel("test/exported", Visibility::Exported, &reg);
        let gw = SubnetGateway::new(SubnetId::new(&[1]), reg);

        gw.export_channel(ch, vec![SubnetId::new(&[2])]);

        // Forward to exported target
        let decision = gw.should_forward(SubnetId::new(&[1]), SubnetId::new(&[2]), ch, TEST_TTL, 0);
        assert_eq!(decision, ForwardDecision::Forward);

        // Drop to non-exported target
        let decision = gw.should_forward(SubnetId::new(&[1]), SubnetId::new(&[3]), ch, TEST_TTL, 0);
        assert_eq!(decision, ForwardDecision::Drop(DropReason::NotExported));
    }

    #[test]
    fn test_ttl_expired() {
        let reg = Arc::new(ChannelConfigRegistry::new());
        let ch = make_channel("test/ttl", Visibility::Global, &reg);
        let gw = SubnetGateway::new(SubnetId::new(&[1]), reg);

        let decision = gw.should_forward(
            SubnetId::new(&[1]),
            SubnetId::new(&[2]),
            ch,
            4, // ttl = 4
            4, // hop_count = 4 (expired)
        );
        assert_eq!(decision, ForwardDecision::Drop(DropReason::TtlExpired));
    }

    /// Regression for BUG_AUDIT_2026_04_30_CORE.md #88: previously
    /// the TTL gate was `hop_ttl > 0 && hop_count >= hop_ttl`,
    /// short-circuiting to "always forward" when `hop_ttl == 0`.
    /// `NetHeader::new` defaults `hop_ttl` to 0 and the field is
    /// excluded from AAD-protection (`hop_count` is mutable in
    /// transit per `protocol.rs:319`), so an attacker could craft
    /// `hop_ttl=0` packets that loop through gateways forever.
    /// Post-fix, `hop_ttl == 0` is treated as expired.
    #[test]
    fn ttl_zero_is_treated_as_expired() {
        let reg = Arc::new(ChannelConfigRegistry::new());
        let ch = make_channel("test/ttl-zero", Visibility::Global, &reg);
        let gw = SubnetGateway::new(SubnetId::new(&[1]), reg);

        let decision = gw.should_forward(
            SubnetId::new(&[1]),
            SubnetId::new(&[2]),
            ch,
            0, // ttl = 0 — pre-fix this short-circuited to forward
            0, // hop_count = 0
        );
        assert_eq!(
            decision,
            ForwardDecision::Drop(DropReason::TtlExpired),
            "pre-fix: this returned Forward because the guard was \
             `hop_ttl > 0 && hop_count >= hop_ttl`, which short-\
             circuits when hop_ttl == 0"
        );
    }

    #[test]
    fn test_unknown_channel_defaults_subnet_local() {
        // Unknown channels cannot be proven safe to cross subnet boundaries,
        // so the gateway drops them (SubnetLocal semantics). Previously this
        // defaulted to Global, silently forwarding traffic for any hash the
        // local node hadn't seen.
        let reg = Arc::new(ChannelConfigRegistry::new());
        let gw = SubnetGateway::new(SubnetId::new(&[1]), reg);

        let decision = gw.should_forward(
            SubnetId::new(&[1]),
            SubnetId::new(&[2]),
            0x9999,
            TEST_TTL,
            0,
        );
        assert_eq!(decision, ForwardDecision::Drop(DropReason::SubnetLocal));
    }

    #[test]
    fn test_regression_collision_between_subnet_local_and_global_drops() {
        // Regression: gateway used `unwrap_or(Visibility::Global)` when the
        // registry returned `None`. After the wire-keyed lookup was
        // fixed to return `None` on `u16` wire-bucket collisions,
        // that fallback recreated the exact leak the registry fix was
        // meant to prevent — a `SubnetLocal` channel colliding with a
        // `Global` channel would still be forwarded across subnet
        // boundaries.
        //
        // Fix: default to `SubnetLocal` on `None`, so a collision
        // forces a drop rather than a permissive forward. The
        // collision space exercised here is the wire `u16` bucket
        // (what `should_forward` keys on), not the canonical `u32`.
        let mut seen = std::collections::HashMap::<u16, String>::new();
        let (name1, name2) = loop {
            let name = format!("gw-ch-{}", seen.len());
            let wire = ChannelId::parse(&name).unwrap().wire_hash();
            if let Some(existing) = seen.get(&wire) {
                break (existing.clone(), name);
            }
            seen.insert(wire, name);
        };

        let reg = Arc::new(ChannelConfigRegistry::new());
        let id1 = ChannelId::parse(&name1).unwrap();
        let id2 = ChannelId::parse(&name2).unwrap();
        let colliding_wire = id1.wire_hash();
        assert_eq!(
            id1.wire_hash(),
            id2.wire_hash(),
            "precondition: wire hashes must collide"
        );

        reg.insert(ChannelConfig::new(id1).with_visibility(Visibility::SubnetLocal));
        reg.insert(ChannelConfig::new(id2).with_visibility(Visibility::Global));

        let gw = SubnetGateway::new(SubnetId::new(&[1]), reg);

        // A colliding wire hash must not produce a permissive forward.
        let decision = gw.should_forward(
            SubnetId::new(&[1]),
            SubnetId::new(&[2]),
            colliding_wire,
            TEST_TTL,
            0,
        );
        assert_eq!(decision, ForwardDecision::Drop(DropReason::SubnetLocal));
    }

    #[test]
    fn test_stats() {
        let reg = Arc::new(ChannelConfigRegistry::new());
        let ch_global = make_channel("test/stats-global", Visibility::Global, &reg);
        let ch_local = make_channel("test/stats-local", Visibility::SubnetLocal, &reg);
        let gw = SubnetGateway::new(SubnetId::new(&[1]), reg);

        gw.should_forward(
            SubnetId::new(&[1]),
            SubnetId::new(&[2]),
            ch_global,
            TEST_TTL,
            0,
        );
        gw.should_forward(
            SubnetId::new(&[1]),
            SubnetId::new(&[2]),
            ch_local,
            TEST_TTL,
            0,
        );
        gw.should_forward(
            SubnetId::new(&[1]),
            SubnetId::new(&[2]),
            ch_global,
            TEST_TTL,
            0,
        );

        assert_eq!(gw.forwarded_count(), 2);
        assert_eq!(gw.dropped_count(), 1);
    }

    #[test]
    fn exports_snapshot_round_trips_export_table() {
        // Pin the new operator-tool accessor: every `export_channel`
        // insert shows up in `exports()` keyed by channel_hash and
        // sorted ascending. `exports_for_channel` is a per-channel
        // point lookup.
        let reg = Arc::new(ChannelConfigRegistry::new());
        let gw = SubnetGateway::new(SubnetId::new(&[1]), reg);

        gw.export_channel(0x42, vec![SubnetId::new(&[2]), SubnetId::new(&[3])]);
        gw.export_channel(0x10, vec![SubnetId::new(&[5])]);
        gw.export_channel(0x20, vec![]);

        let snap = gw.exports();
        let keys: Vec<u16> = snap.iter().map(|(k, _)| *k).collect();
        assert_eq!(keys, vec![0x10, 0x20, 0x42]);
        assert_eq!(snap[0].1, vec![SubnetId::new(&[5])]);
        assert_eq!(
            snap[2].1,
            vec![SubnetId::new(&[2]), SubnetId::new(&[3])],
        );

        assert_eq!(
            gw.exports_for_channel(0x42),
            Some(vec![SubnetId::new(&[2]), SubnetId::new(&[3])]),
        );
        assert_eq!(gw.exports_for_channel(0xDEAD), None);
    }

    #[test]
    fn peer_subnets_snapshot_is_idempotent_and_sorted() {
        // Pin `add_peer` (now `&self`) + `peer_subnets()` snapshot.
        // Re-adding the same subnet is a no-op; output is sorted by
        // raw bits for stable operator output.
        let reg = Arc::new(ChannelConfigRegistry::new());
        let gw = SubnetGateway::new(SubnetId::new(&[1]), reg);

        gw.add_peer(SubnetId::new(&[3, 7]));
        gw.add_peer(SubnetId::new(&[2]));
        gw.add_peer(SubnetId::new(&[3, 7])); // duplicate
        gw.add_peer(SubnetId::new(&[3]));

        let peers = gw.peer_subnets();
        assert_eq!(peers.len(), 3, "duplicate add must be a no-op");
        // Sorted by raw bits: SubnetId::new(&[2]).raw() < SubnetId::new(&[3]).raw() < SubnetId::new(&[3,7]).raw()
        assert_eq!(peers[0], SubnetId::new(&[2]));
        assert_eq!(peers[1], SubnetId::new(&[3]));
        assert_eq!(peers[2], SubnetId::new(&[3, 7]));
    }

    #[test]
    fn record_forward_and_record_drop_tick_independent_counters() {
        // `record_forward` / `record_drop` are the entry points for
        // host visibility checks that bypass `should_forward` (e.g.
        // MeshNode's inline publish-fanout). Pin that they each bump
        // their dedicated counter and don't cross-contaminate.
        let reg = Arc::new(ChannelConfigRegistry::new());
        let gw = SubnetGateway::new(SubnetId::new(&[1]), reg);

        gw.record_forward();
        gw.record_forward();
        gw.record_drop(DropReason::SubnetLocal);

        assert_eq!(gw.forwarded_count(), 2);
        assert_eq!(gw.dropped_count(), 1);
    }
}
