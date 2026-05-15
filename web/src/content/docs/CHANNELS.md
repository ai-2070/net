# Channels & Authorization

Named, hierarchical communication endpoints with wire-speed authorization. Channels carry policy -- access control is enforced via capability filters and permission tokens, cached in a bloom filter for <10ns per-packet checks.

## Channel Names

Channels use hierarchical names with `/` separators (e.g., `sensors/lidar/front`). The `channel_hash: u16` in the Net header is derived via xxh3 truncation for wire-speed filtering.

```rust
pub struct ChannelName(String);

impl ChannelName {
    fn new(name: &str) -> Result<Self, ChannelError>  // Validates format
    fn hash(&self) -> u16                              // xxh3 -> u16 for header
    fn depth(&self) -> usize                           // Number of segments
    fn is_prefix_of(&self, other: &ChannelName) -> bool // Wildcard matching
}
```

**Validation rules:**
- Max 255 bytes
- Valid characters: `a-z`, `A-Z`, `0-9`, `-`, `_`, `.`, `/`
- Must not start or end with `/`
- Must not contain `//`

`ChannelId` pairs a `ChannelName` with its precomputed `u16` hash. `ChannelRegistry` tracks live channels via `DashMap`.

## Channel Configuration

Each channel carries policy via `ChannelConfig`:

```rust
pub struct ChannelConfig {
    pub channel_id: ChannelId,
    pub visibility: Visibility,          // SubnetLocal | ParentVisible | Exported | Global
    pub publish_caps: Option<CapabilityFilter>,   // Who can publish
    pub subscribe_caps: Option<CapabilityFilter>, // Who can subscribe
    pub require_token: bool,             // Require PermissionToken in addition to caps
    pub priority: u8,                    // Default packet priority
    pub reliable: bool,                  // Default reliability mode
    pub max_rate_pps: Option<u32>,       // Rate limit (packets/sec)
}
```

### Visibility

Controls how far packets on this channel propagate through the subnet hierarchy:

| Visibility | Behavior |
|------------|----------|
| `SubnetLocal` | Never crosses subnet boundaries |
| `ParentVisible` | Visible to parent subnet, not siblings |
| `Exported` | Exported to specific target subnets (via gateway export table) |
| `Global` | No subnet restriction (default) |

### Authorization Flow

1. Node announces capabilities via `CapabilityAd`
2. If `publish_caps` / `subscribe_caps` is set, the node's `CapabilitySet` must match the filter
3. If `require_token` is true, the node must also hold a valid `PermissionToken` with the appropriate scope
4. On success, `(origin_hash, channel_hash)` is inserted into the `AuthGuard`

## AuthGuard -- Wire-Speed Authorization

The `AuthGuard` combines a bloom filter with a verified-positive cache for O(1) per-packet authorization.

```rust
pub struct AuthGuard {
    bloom: Vec<AtomicU8>,                    // 4 KB, fits in L1 cache
    verified: DashMap<(u32, u16), bool>,     // Verified-positive cache
}
```

### Fast Path (`check_fast`)

Called on every packet by forwarding nodes:

1. Compute bloom key from `(origin_hash, channel_hash)` via xxh3
2. Probe 2 bloom filter positions (atomic reads, no locks)
3. If either bit is 0 -> `Denied` (definite, no false negatives)
4. If both bits set, probe verified cache -> `Allowed` or `NeedsFullCheck`

```rust
pub enum AuthVerdict {
    Allowed,         // Bloom hit + verified cache hit
    Denied,          // Bloom miss (definite)
    NeedsFullCheck,  // Bloom hit but not in verified cache
}
```

**Performance:** <10ns for the Allowed/Denied paths. The bloom filter is 2^15 bits (4 KB), fitting entirely in L1 cache.

### Slow Path

`authorize()` inserts into both the bloom filter and verified cache at subscription time. `revoke()` removes from the verified cache only -- bloom filters don't support deletion, but verified cache misses cause `NeedsFullCheck` which then fails full verification.

## Fan-out publishers

Channels carry policy and names; they do not carry membership. To publish the same payload to every subscriber of a channel, wrap a [`ChannelPublisher`](../../src/adapter/net/channel/publisher.rs) around the channel name and call `MeshNode::publish` — one per-peer unicast per subscriber, no multicast primitive, no group cryptography.

### Subscriber roster

`SubscriberRoster` in `channel/roster.rs` tracks, for every `ChannelId`, the set of peer `node_id`s that have subscribed. The roster is populated by the `SUBPROTOCOL_CHANNEL_MEMBERSHIP` subprotocol (`0x0A00`, carrying `Subscribe` / `Unsubscribe` / `Ack` messages) and reaped automatically when the failure detector marks a peer `Failed`.

- `MeshNode::subscribe_channel(publisher_node_id, channel)` and `unsubscribe_channel(...)` send one `Subscribe` / `Unsubscribe` and block until the `Ack` (or the configured `membership_ack_timeout`).
- Incoming `Subscribe` packets are authorized by `max_channels_per_peer` cap, then by the `ChannelConfigRegistry` (if one is set via `MeshNode::set_channel_configs`): unknown channel names are rejected with `AckReason::UnknownChannel`. Full capability / token checks are a follow-up.
- `MeshNode::roster()` exposes the live roster for diagnostics.

### `ChannelPublisher` API

```rust
let publisher = mesh.channel_publisher(
    ChannelName::new("sensors/lidar").unwrap(),
    PublishConfig::new()
        .with_reliability(Reliability::FireAndForget)
        .with_on_failure(OnFailure::Collect)
        .with_max_inflight(32),
);
let report = mesh.publish(&publisher, payload).await?;
```

`PublishConfig.on_failure` controls what per-peer errors mean:

| Policy | Behavior |
|--------|----------|
| `BestEffort` (default) | Log per-peer errors, return `Ok` if any subscriber received the payload. Returns `Err` only if every attempted peer failed. |
| `FailFast` | Stop at the first per-peer error and return the partial `PublishReport`. |
| `Collect` | Never short-circuit; always return a full `PublishReport` with every per-peer outcome. |

Per-peer concurrency is bounded by `PublishConfig.max_inflight` (default 32) via a `Semaphore`. The default is fine for rosters up to a few hundred subscribers; larger fan-outs need tuning or a different primitive — this helper is explicitly **not** the right tool for millions-of-subscribers pub/sub.

### Non-goals (non-negotiable)

- **No multicast packet primitive.** One `publish` call = N independent unicasts, one per subscriber. There is no packet that "delivers to many peers in one socket op," no tree dissemination, no gossip.
- **No group cryptography in transport.** Each unicast uses the existing per-peer Noise session. There is no group key, no shared session, no fan-out-specific AEAD.
- **No new header bits.** Routing header and AEAD AAD are unchanged. Membership rides on `SUBPROTOCOL_CHANNEL_MEMBERSHIP`; the fan-out payload rides on normal per-peer streams.
- **No implicit "everyone" broadcast.** Fan-out targets only explicit subscribers; "all connected peers" is not a supported publish mode.
- **No history / catch-up.** A node that subscribes at time T never receives earlier publishes. Durable streams are a separate concern (EventBus / causal subprotocol).
- **No atomic fan-out.** Partial delivery is by design; the `PublishReport` is the caller's ground truth.

## Source Files

| File | Purpose |
|------|---------|
| `channel/name.rs` | `ChannelName`, `ChannelId`, `ChannelRegistry`, validation |
| `channel/config.rs` | `ChannelConfig`, `Visibility`, `ChannelConfigRegistry` |
| `channel/guard.rs` | `AuthGuard`, bloom filter, `AuthVerdict` |
| `channel/roster.rs` | `SubscriberRoster` — per-channel subscriber index |
| `channel/membership.rs` | `MembershipMsg` + `SUBPROTOCOL_CHANNEL_MEMBERSHIP` codec |
| `channel/publisher.rs` | `ChannelPublisher`, `PublishConfig`, `OnFailure`, `PublishReport` |
