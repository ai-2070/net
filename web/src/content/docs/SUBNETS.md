# Subnets & Hierarchy

Hierarchical 4-level subnet encoding in 4 bytes. Nodes are assigned to subnets by label-based rules, and gateways enforce channel visibility at subnet boundaries without decrypting payloads.

## Subnet ID

`SubnetId` packs a 4-level hierarchy into a `u32`. Each level gets 8 bits (256 values).

```
subnet_id (u32):
  [level_0: 8 bits] [level_1: 8 bits] [level_2: 8 bits] [level_3: 8 bits]
   ^region (256)     ^fleet (256)       ^vehicle (256)     ^subsystem (256)
```

```rust
pub struct SubnetId(u32);

impl SubnetId {
    const GLOBAL: Self = Self(0);              // No subnet restriction

    fn new(levels: &[u8]) -> Self              // &[3, 7] -> 0x03_07_00_00
    fn level(self, n: u8) -> u8               // Extract level 0-3
    fn depth(self) -> u8                      // Non-zero levels count
    fn parent(self) -> Self                   // Strip deepest level
    fn contains(self, other: Self) -> bool    // Ancestor check
    fn is_sibling(self, other: Self) -> bool  // Same parent
    fn distance(self, other: Self) -> u8      // Levels of separation
}
```

Parent/child/sibling relationships resolve with bitwise operations at wire speed.

**Examples:**
- `SubnetId::new(&[3])` -- region 3
- `SubnetId::new(&[3, 7])` -- region 3, fleet 7
- `SubnetId::new(&[3, 7, 1, 4])` -- fully specified down to subsystem
- `SubnetId::GLOBAL` -- no subnet restriction

## Subnet Assignment

`SubnetPolicy` assigns nodes to subnets based on labels (capability tags).

```rust
pub struct SubnetPolicy {
    pub rules: Vec<SubnetRule>,
    pub default_subnet: SubnetId,
}

pub struct SubnetRule {
    pub match_tags: Vec<String>,         // Tags that must be present
    pub target_subnet: SubnetId,         // Subnet to assign
}
```

Rules are evaluated in order. The first matching rule determines the subnet. If no rule matches, the node gets `default_subnet`.

## Subnet Gateway

`SubnetGateway` sits at subnet boundaries and enforces channel visibility. It reads only header fields -- no decryption, no payload modification.

```rust
pub struct SubnetGateway {
    local_subnet: SubnetId,
    peer_subnets: Vec<SubnetId>,
    export_table: DashMap<u16, Vec<SubnetId>>,  // channel_hash -> allowed subnets
    channel_configs: ChannelConfigRegistry,
}
```

### Forwarding Decisions

The gateway reads `channel_hash` and `subnet_id` from the header and consults the channel's `Visibility`:

| Visibility | Decision |
|------------|----------|
| `SubnetLocal` | Always drop at boundary |
| `ParentVisible` | Forward only to ancestor subnets |
| `Exported` | Forward only to subnets in the export table |
| `Global` | Always forward |

```rust
pub enum ForwardDecision {
    Forward,
    Drop(DropReason),
}

pub enum DropReason {
    SubnetLocal,       // Channel never crosses boundaries
    NotAncestor,       // Destination is not an ancestor
    NotExported,       // Channel not in export table for destination
    UnknownSubnet,     // Unknown subnet_id
    TtlExpired,        // TTL reached zero
}
```

Gateway stats (forwarded/dropped counts) are tracked via atomics for zero-contention monitoring.

## Source Files

| File | Purpose |
|------|---------|
| `subnet/id.rs` | `SubnetId`, hierarchy operations, distance |
| `subnet/assignment.rs` | `SubnetPolicy`, `SubnetRule`, label matching |
| `subnet/gateway.rs` | `SubnetGateway`, visibility enforcement, export table |
