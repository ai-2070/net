# Subnets & Hierarchy

Hierarchical 4-level subnet encoding in 4 bytes. Nodes are assigned to
subnets by label-based rules; channel `Visibility` limits where channel
traffic propagates across that hierarchy.

**Doctrine — topology is not authority.** Everything on this page is
topology: where a node sits and where traffic should propagate. None of
it authorizes anything. A peer's subnet is derived from its own
self-declared capability tags, so visibility is a *propagation filter*,
not an access boundary — a channel that must exclude anyone pairs its
visibility with token enforcement (`ChannelConfig::with_token_roots`).
Protected transport rights (`ATTACH`/`ROUTE`/`EXPORT`) require an
authority-qualified `SubnetGrant`; fleet/enterprise membership lives in
org identity, not in these four levels. See
`docs/internal/plans/SUBNET_AUTH_PLAN.md` for the authority model and
`SubnetRef` (authority + path), of which the type below is the path
half (alias: `TopologySubnetId`).

## Subnet ID

`SubnetId` packs a 4-level hierarchy into a `u32`. Each level gets
8 bits (256 values). Level meanings are operator-chosen within one
installation's hierarchy — do not encode fleet, customer, or geography
populations in them.

```
subnet_id (u32):
  [level_0: 8 bits] [level_1: 8 bits] [level_2: 8 bits] [level_3: 8 bits]
```

Actual surface (`subnet/id.rs`):

```rust
pub struct SubnetId(u32);
pub type TopologySubnetId = SubnetId;   // security-facing alias

impl SubnetId {
    pub const GLOBAL: Self;                 // 0 — global / authority-local root
    pub const MAX_DEPTH: u8;                // 4

    pub fn new(levels: &[u8]) -> Self;      // panics on >4 levels
    pub fn try_new(levels: &[u8]) -> Result<Self, SubnetError>;
    pub const fn from_raw(raw: u32) -> Self;
    pub const fn raw(self) -> u32;
    pub const fn level(self, n: u8) -> u8;  // 0 for unset levels
    pub fn depth(self) -> u8;               // count of non-zero levels
    pub const fn is_global(self) -> bool;
    pub fn parent(self) -> Self;            // strip deepest level
    pub fn is_ancestor_of(self, other: Self) -> bool;
    pub fn is_ancestor_or_self_of(self, target: Self) -> bool; // canonical
    pub const fn is_same_subnet(self, other: Self) -> bool;
    pub fn is_sibling(self, other: Self) -> bool;
    pub const fn mask_for_depth(depth: u8) -> u32;
}
```

(There is no `contains` or `distance` method; an earlier revision of
this page documented both. Scope containment is
`is_ancestor_or_self_of`, whose truth table — including the `0` rows —
is pinned by `ancestor_or_self_truth_table` in `subnet/id.rs`.)

`Display`/`FromStr` round-trip `"global"` and dotted forms like
`"3.7.2"`. `Ord` is derived on the raw `u32` purely as a deterministic
tiebreaker; it does not follow the hierarchy.

**Examples:**
- `SubnetId::new(&[3])` — level-0 value 3, deeper levels unrestricted
- `SubnetId::new(&[3, 7])` — first two levels set
- `SubnetId::new(&[3, 7, 1, 4])` — fully specified
- `SubnetId::GLOBAL` — no restriction; as a *grant scope* under an
  authority it is the authority-local root covering every path

## Subnet Assignment

`SubnetPolicy` derives a node's `SubnetId` from its capability tags.
Assignment **classifies topology, not authority**: the tags are the
announcing peer's own claim.

Actual surface (`subnet/assignment.rs`):

```rust
pub struct SubnetPolicy { /* rules: Vec<SubnetRule> (private) */ }

pub struct SubnetRule {
    pub tag_prefix: String,          // e.g. "region:"
    pub level: u8,                   // which hierarchy level (0-3)
    pub values: HashMap<String, u8>, // exact suffix -> level value
}

impl SubnetPolicy {
    pub fn new() -> Self;                          // empty: everyone GLOBAL
    pub fn add_rule(self, rule: SubnetRule) -> Self;      // panics on level >= 4
    pub fn try_add_rule(self, rule: SubnetRule) -> Result<Self, SubnetError>;
    pub fn assign(&self, caps: &CapabilitySet) -> SubnetId;
    pub fn assign_from_rendered_tags(&self, tags: &[String]) -> SubnetId;
    pub fn can_assign_non_global(&self) -> bool;
}
```

Semantics (pinned by unit tests in the module; an earlier revision of
this page said "first matching rule determines the subnet" — wrong):

1. **Rule order is declaration order; later-rule-wins per level.** Two
   rules targeting the same `level` resolve to the later match.
2. **Smallest matching tag wins per rule.** Inside one rule the
   lexicographically smallest tag whose stripped suffix is in `values`
   wins — deterministic across receivers regardless of tag order.
3. **Exact value lookup.** The suffix after `tag_prefix` is matched by
   exact string equality; no partial-prefix matching.
4. **Unmatched levels stay zero** ("no restriction at this level").
   Zero-valued mappings are skipped.

There is no `default_subnet` field; a node with no matching rules gets
`SubnetId::GLOBAL`.

## Channel Visibility

Per-channel `Visibility` (`channel/config.rs`) limits propagation:

| Visibility | Propagation |
|------------|-------------|
| `SubnetLocal` | Same subnet only |
| `ParentVisible` | Strictly upward — own subnet and ancestors, never siblings or descendants |
| `Exported` | Only to subnets in a gateway export table |
| `Global` (default) | Everywhere |

Live enforcement points (both inline in `mesh.rs`, via
`MeshNode::subnet_visible`):

- **Subscribe gate** — a subscribe that fails the visibility check is
  rejected `Unauthorized`.
- **Publish fan-out filter** — subscribers outside the visible set are
  skipped.

A peer whose subnet has not been derived (`peer_subnets` miss) is
handled per visibility mode: on a `GLOBAL`-subnet node the unknown peer
passes (a node with no subnet identity cannot express "same subnet as
me" as a constraint); on a subnet-scoped node it fails closed.

Because the peer subnet comes from self-declared tags, these checks are
routing decisions. Registering a `SubnetLocal`/`ParentVisible` channel
with no token gate logs one info-level note recording that the channel
is soft.

## Subnet Gateway

`SubnetGateway` (`subnet/gateway.rs`) evaluates visibility for traffic
crossing subnet boundaries, reading only header fields.

```rust
pub struct SubnetGateway {
    local_subnet: SubnetId,
    peer_subnets: parking_lot::RwLock<Vec<SubnetId>>,
    export_table: DashMap<u16, Vec<SubnetId>>,   // wire hash -> allowed subnets
    channel_configs: Arc<ChannelConfigRegistry>, // shared with the MeshNode
    forwarded: AtomicU64,
    dropped: AtomicU64,
}
```

### Forwarding decisions

`should_forward(source, dest, channel_hash, hop_ttl, hop_count)`
resolves the channel's `Visibility` by wire hash — an unknown or
collided hash resolves to `SubnetLocal` (fail closed) — and returns:

```rust
pub enum ForwardDecision { Forward, Drop(DropReason) }

pub enum DropReason {
    SubnetLocal,    // channel never crosses boundaries
    NotAncestor,    // ParentVisible, destination not an ancestor
    NotExported,    // Exported, destination not in export table
    UnknownSubnet,  // unresolvable subnet
    TtlExpired,     // hop TTL reached zero
}
```

Stats (`forwarded_count()` / `dropped_count()`) are atomics; the
`net gateway stats|exports` CLI reads them, and the inline
`subnet_visible` call sites increment them when a gateway is installed.

### Current enforcement status (honest)

- `should_forward`, `add_peer`, and `export_channel` have **no
  production call sites** — packet-level gateway forwarding is not yet
  wired, and the export table is empty in a running node.
- `NetHeader.subnet_id` exists on the wire (AAD-covered) but is always
  0 in production.
- `Visibility::Exported` is therefore unsatisfiable today: the node
  path hard-denies it and nothing populates export tables.

Wiring these live — gated by `SubnetGrant::ROUTE`/`EXPORT` — is slice
S4 of `SUBNET_AUTH_PLAN.md`; no ungated forwarding path will ship
first.

## Source Files

| File | Purpose |
|------|---------|
| `subnet/id.rs` | `SubnetId`/`TopologySubnetId`, hierarchy operations, pinned containment truth table |
| `subnet/assignment.rs` | `SubnetPolicy`, `SubnetRule`, tag-prefix matching |
| `subnet/gateway.rs` | `SubnetGateway`, visibility evaluation, export table |
| `subnet/error.rs` | `SubnetError` — every failure the subnet surface returns |
