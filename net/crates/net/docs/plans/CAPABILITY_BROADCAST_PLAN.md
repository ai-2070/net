# Capability broadcast plan — Stage C of the SDK security surface

## Context

[`SDK_SECURITY_SURFACE_PLAN.md`](SDK_SECURITY_SURFACE_PLAN.md) Stage C
proposes adding `Mesh::announce_capabilities(caps)` and
`Mesh::find_nodes(filter)` to the SDK. The stated exit criterion is a
two-node test where A announces capabilities and B's `find_nodes`
returns A.

A survey of `src/adapter/net/` turned up three gaps in the core crate
that the Stage C estimate (~3–4 days) did not account for:

1. **No subprotocol carries capability announcements.** `CapabilityAd`
   (`swarm.rs:288`) and `CapabilityAnnouncement`
   (`behavior/capability.rs:690`) both exist as standalone data types
   with `to_bytes` / `from_bytes` codecs, but nothing in `MeshNode`
   emits or receives them.
2. **No broadcast primitive on `MeshNode`.** The heartbeat loop
   (`mesh.rs:1458`) sends keepalives + pingwaves to each peer; there
   is no `broadcast(subprotocol, payload)` helper or capability slot
   in the heartbeat packet.
3. **`CapabilityIndex` is unowned at the mesh level.** It is a free-
   standing `DashMap`-backed structure (`behavior/capability.rs:1083`)
   with no `MeshNode` field and no GC lifecycle.

This plan closes those gaps so the SDK Stage C surface can ship for
real (cross-node, not just in-process). It is a **core + SDK + NAPI
+ TS** effort; the SDK-only method signatures Stage C sketches are
the final layer, not the starting point.

Stage C's own dependencies remain unchanged: it requires Stages A
(Rust SDK `Identity`) and B (NAPI + TS token surface), both shipped.

## Scope

**In scope:**

- New subprotocol `SUBPROTOCOL_CAPABILITY_ANN` for carrying
  `CapabilityAnnouncement` payloads between directly-connected peers.
- `MeshNode` owns a `CapabilityIndex`, receives inbound
  announcements into it, and exposes a query API.
- `MeshNode::announce_capabilities(caps)` signs and broadcasts the
  caller's capabilities to every active peer session; also
  self-indexes (so single-node queries return the local node).
- Background GC: periodic `CapabilityIndex::gc()` tick from the
  heartbeat loop.
- SDK (`Mesh`), NAPI (`NetMesh`), and TS SDK (`MeshNode`) wrappers.
- Two-node integration test + TTL expiry test, in Rust and TS.

**Out of scope:**

- Gossip across multi-hop — originally deferred, now **shipped**
  via hop-count-bounded forwarding + `(origin, version)` dedup.
  See [`MULTIHOP_CAPABILITY_PLAN.md`](MULTIHOP_CAPABILITY_PLAN.md)
  for the design.
- Bandwidth optimizations (delta encoding, hash-summary + fetch).
  Full-announcement push is the v1 shape; optimization lands if
  profiling shows it.
- Cross-subnet visibility. Subnet filtering of announcement fan-out
  ties into Stage D's `SubnetGateway` wiring and is treated as an
  extension of that work, not this one. Tag-based discovery scope
  (per-tenant / per-region / subnet-local query filtering) shipped
  via reserved `scope:*` tags + `ScopeFilter` —
  see [`SCOPED_CAPABILITIES_PLAN.md`](SCOPED_CAPABILITIES_PLAN.md).
- `CapabilityAd` / `LocalGraph` (the swarm.rs topology system). It is
  a parallel subsystem with its own lifecycle; this plan does **not**
  integrate them.

## Design

### On-wire form

Use [`CapabilityAnnouncement`](../src/adapter/net/behavior/capability.rs)
(behavior/capability.rs:690). It already has:

- Full `CapabilitySet` (hardware, software, models, tools, tags, limits)
- Version counter (`CapabilityIndex::index` skips stale versions)
- `ttl_secs` (per-announcement TTL, honored by `CapabilityIndex::gc()`)
- Optional ed25519 `Signature64` field

Wire layout is stable (`to_bytes` / `from_bytes` are implemented).

### Size budget

CapabilitySet can be several KB for rich announcements. Cap inbound
payloads at **16 KB** and drop oversize packets with a
`"capability: oversize"` trace — protects the index from adversarial
bloat without foreclosing realistic payloads.

### Subprotocol ID

Allocate:

```rust
// src/adapter/net/behavior/broadcast.rs (new)
pub const SUBPROTOCOL_CAPABILITY_ANN: u16 = 0x0C00;
```

Adjacent to existing IDs (0x0400 CAUSAL … 0x0B00 STREAM_WINDOW). Add
to the subprotocol negotiation table so peers advertise support.

### Broadcast strategy

**Direct-peer push, on announce + on session-open.**

- `announce_capabilities(caps)` → build `CapabilityAnnouncement`
  (stamped with `node_id`, an incremented version, `timestamp_ns`,
  `ttl_secs`, and — if an `Identity` is bound — a signature) →
  store in `MeshNode::local_announcement: Arc<ArcSwapOption<CapabilityAnnouncement>>`
  → self-index → send to every peer in `peers` via
  `send_subprotocol(peer_addr, SUBPROTOCOL_CAPABILITY_ANN, bytes)`.
- On new session established (`accept` / `connect` completion path):
  if `local_announcement.load()` is `Some`, push it to the new peer
  right after the handshake completes. Covers late joiners without a
  periodic re-send.

Bandwidth profile: each `announce_capabilities` call is O(N_peers)
packets; re-announcement frequency is caller-controlled. No periodic
heartbeat piggyback — keeps the heartbeat path unchanged.

Multi-hop gossip originally deferred to v1; **now shipped** under
[`MULTIHOP_CAPABILITY_PLAN.md`](MULTIHOP_CAPABILITY_PLAN.md).
Announcements fan out up to `MAX_CAPABILITY_HOPS = 16` hops with
`(origin, version)` dedup, origin-side rate limiting, and route
install on receipt. Signature verification holds across hops —
`hop_count` sits outside the signed envelope so forwarders never
touch the origin's signature.

### Receiver path

Extend the inbound dispatch branch in `src/adapter/net/mesh.rs`
(around line 1293 where `SUBPROTOCOL_CHANNEL_MEMBERSHIP` is already
handled):

```rust
if parsed.header.subprotocol_id == SUBPROTOCOL_CAPABILITY_ANN {
    Self::handle_capability_announcement(&payload, from_node, ctx);
    return;
}
```

With:

```rust
fn handle_capability_announcement(
    payload: &[u8],
    from_node: u64,
    ctx: &DispatchCtx,
) {
    if payload.len() > MAX_CAPABILITY_ANNOUNCEMENT_BYTES {
        tracing::trace!(from_node, len = payload.len(), "capability: oversize drop");
        return;
    }
    let Some(ann) = CapabilityAnnouncement::from_bytes(payload) else {
        tracing::trace!(from_node, "capability: decode failed");
        return;
    };
    if ann.node_id != from_node {
        // Senders can only announce *their own* capabilities.
        tracing::trace!(from_node, ann_id = ann.node_id, "capability: node_id mismatch");
        return;
    }
    if ctx.capability_config.require_signature && ann.signature.is_none() {
        tracing::trace!(from_node, "capability: unsigned announcement rejected");
        return;
    }
    // Optional signature verification (see below).
    ctx.capability_index.index(ann);
}
```

Signature verification: if `ann.signature` is present, we need the
sender's `EntityId` (public key) to verify. Today peers are addressed
by `node_id` (u64), not `EntityId` (32 bytes). First implementation
accepts the signature as advisory (stored on the announcement but not
verified); the `require_signature` config flag gates *presence*, not
*validity*. Full verification requires a node_id → entity_id binding,
which lands with Stage E (channel auth). Document this limitation in
the receiver's trace logs.

### Index lifecycle on MeshNode

Add to `MeshNode` (in `src/adapter/net/mesh.rs:319` struct):

```rust
/// Per-mesh capability index, populated by inbound
/// `SUBPROTOCOL_CAPABILITY_ANN` packets and queried by
/// `find_nodes_by_filter`.
capability_index: Arc<CapabilityIndex>,
/// Most recent announcement this node published. Pushed to new
/// peers on session-open; `None` until the first `announce_*` call.
local_announcement: Arc<ArcSwapOption<CapabilityAnnouncement>>,
/// Monotonic version counter for local announcements.
capability_version: Arc<AtomicU64>,
```

And to `MeshNodeConfig`:

```rust
/// Require inbound `CapabilityAnnouncement`s to carry a signature.
/// Unsigned announcements are dropped. Default: false (signatures
/// are advisory until Stage E binds node_id → entity_id).
pub require_signed_capabilities: bool,
/// GC interval for expired capability index entries. Default: 60s.
pub capability_gc_interval: Duration,
```

### GC task

Spawn a long-lived task from `MeshNode::start()`:

```rust
let index = self.capability_index.clone();
let shutdown = self.shutdown_notify.clone();
let interval = self.config.capability_gc_interval;
tokio::spawn(async move {
    let mut tick = tokio::time::interval(interval);
    loop {
        tokio::select! {
            _ = tick.tick() => { index.gc(); }
            _ = shutdown.notified() => return,
        }
    }
});
```

Stop handle: reuses the existing `shutdown_notify` already wired for
the heartbeat task, so `MeshNode::shutdown` drains the GC task with
no additional plumbing.

### MeshNode public API

```rust
impl MeshNode {
    /// Announce this node's capabilities to every connected peer.
    /// Caller's `Identity` (if set) signs the announcement.
    pub async fn announce_capabilities(
        &self,
        caps: CapabilitySet,
    ) -> Result<(), AdapterError>;

    /// Extended form: override TTL and sign-or-not.
    pub async fn announce_capabilities_with(
        &self,
        caps: CapabilitySet,
        ttl: Duration,
        sign: bool,
    ) -> Result<(), AdapterError>;

    /// Query the local capability index. Returns (node_id, score)
    /// pairs ordered by score descending.
    pub fn find_nodes_by_filter(
        &self,
        filter: &CapabilityFilter,
    ) -> Vec<u64>;

    /// Scored query — for callers that want ranked placement rather
    /// than a set membership check.
    pub fn find_best_node(
        &self,
        req: &CapabilityRequirement,
    ) -> Option<u64>;

    /// Shared reference to the capability index, for callers that
    /// want finer-grained queries than the two helpers above.
    pub fn capability_index(&self) -> &Arc<CapabilityIndex>;
}
```

### Rust SDK (`net_sdk::mesh::Mesh`)

Thin wrappers:

```rust
impl Mesh {
    pub async fn announce_capabilities(
        &self,
        caps: CapabilitySet,
    ) -> Result<()>;

    pub fn find_nodes(
        &self,
        filter: &CapabilityFilter,
    ) -> Vec<u64>;
}
```

Both live behind the `capabilities` feature flag (already declared
in Stage A).

### NAPI surface

New file `bindings/node/src/capabilities.rs`, gated on a new
`capabilities = ["net"]` feature. POJOs mirror the Rust structs but
collapse Option<T> fields that are awkward in JS:

```rust
#[napi(object)]
pub struct CapabilitySetJs {
    pub hardware: Option<HardwareJs>,
    pub software: Option<SoftwareJs>,
    pub models: Vec<ModelJs>,
    pub tools: Vec<ToolJs>,
    pub tags: Vec<String>,
    // ResourceLimits mapped to a flat POJO.
    pub limits: Option<CapabilityLimitsJs>,
}

#[napi(object)]
pub struct CapabilityFilterJs {
    pub require_tags: Vec<String>,
    pub require_models: Vec<String>,
    pub require_tools: Vec<String>,
    pub min_memory_mb: Option<u32>,
    pub require_gpu: bool,
    pub gpu_vendor: Option<String>,    // "nvidia" | "amd" | "intel" | "apple" | "qualcomm"
    pub min_vram_mb: Option<u32>,
    pub min_context_length: Option<u32>,
}

#[napi(object)]
pub struct PeerMatchJs {
    pub node_id: BigInt,
    pub score: f64,
}
```

Methods on `NetMesh`:

```rust
#[napi]
pub async fn announce_capabilities(&self, caps: CapabilitySetJs) -> Result<()>;

#[napi]
pub fn find_nodes(&self, filter: CapabilityFilterJs) -> Vec<PeerMatchJs>;
```

POJO ↔ core conversions sit in `capabilities.rs` as pure functions.

### TS SDK surface

New file `sdk-ts/src/capabilities.ts`:

```ts
export interface CapabilitySet {
  hardware?: Hardware;
  software?: Software;
  models?: ModelCapability[];
  tools?: ToolCapability[];
  tags?: string[];
  limits?: CapabilityLimits;
}

export interface CapabilityFilter {
  requireTags?: string[];
  requireModels?: string[];
  requireTools?: string[];
  minMemoryMb?: number;
  requireGpu?: boolean;
  gpuVendor?: 'nvidia' | 'amd' | 'intel' | 'apple' | 'qualcomm';
  minVramMb?: number;
  minContextLength?: number;
}

export interface PeerMatch {
  nodeId: bigint;
  score: number;
}
```

Extend `MeshNode`:

```ts
async announceCapabilities(caps: CapabilitySet): Promise<void>;
findNodes(filter: CapabilityFilter): PeerMatch[];
```

Camel-case conversion is explicit in the wrapper (no reflection), so
a refactor in the NAPI POJOs doesn't silently change the TS shape.

## Staged rollout

Five PRs, in order. Each is independently testable but the first two
are a single logical unit (don't merge C-1 without C-2).

| Stage | What | Days |
|---|---|---|
| **C-1** | Core: subprotocol, receiver dispatch, index on MeshNode, GC task, `announce_capabilities_with` + `find_nodes_by_filter` on `MeshNode`. Rust integration test (two in-process mesh nodes). | 2 |
| **C-2** | Rust SDK: thin `Mesh::announce_capabilities` / `find_nodes` wrappers + doctest. | 0.5 |
| **C-3** | NAPI: POJOs, conversions, `NetMesh.announceCapabilities` / `findNodes`, `capabilities` feature flag, smoke test. | 1 |
| **C-4** | TS SDK: interfaces, `MeshNode.announceCapabilities` / `findNodes`, `sdk-ts/src/capabilities.ts`, two-node TS test. | 1 |
| **C-5** | TTL expiry test (Rust + TS), signature-present path regression, README Security section expansion, docs cross-link from `SDK_SECURITY_SURFACE_PLAN.md`. | 0.5 |

**Total: ~5 days** — ~1.5× the original Stage C estimate, which is
the real cost of the work Stage C implicitly required.

## Test plan

### Rust integration (`src/adapter/net/mesh/tests.rs` or a new `tests/capability_broadcast.rs`)

1. **Two-node announce → find**: spin up A and B, handshake,
   `A.announce_capabilities(CapabilitySet::new().add_tag("gpu"))`,
   poll `B.find_nodes_by_filter(...)` until it contains `A.node_id`
   or a 2 s deadline expires.
2. **TTL expiry**: A announces with `ttl = 1s`, wait 2 s (advance
   beyond TTL + GC tick), assert `B.find_nodes_by_filter` no longer
   returns A.
3. **Late joiner**: A announces, *then* C connects, assert C's index
   contains A's announcement after session-open.
4. **Oversize drop**: send a 32 KB packet on
   `SUBPROTOCOL_CAPABILITY_ANN` directly, confirm it is dropped and
   the index is unchanged.
5. **Node-id mismatch**: A sends an announcement claiming
   `node_id = B.node_id`, confirm receiver drops it.
6. **Version skip**: A announces v2 before v1 arrives, confirm v1 is
   ignored once indexed.

### TS (`sdk-ts/test/capabilities.test.ts`)

Mirror of the Rust two-node test using the existing handshake helper
from `channels.test.ts`, plus TTL expiry.

### Regression coverage

- Bench smoke: `announce_capabilities` called in a 10-peer mesh does
  not regress publish latency (measure before/after on the existing
  `publish` benchmark).
- No increase in heartbeat packet size (assert with a byte-size
  snapshot test), since heartbeat is untouched.

## Risks

- **Signature verification is advisory in v1.** Without a node_id →
  entity_id binding, a malicious peer could announce inflated
  capabilities under any node_id it can route to. Full verification
  ties in with Stage E's channel-auth work. Document in the receiver
  trace and in the SDK README.
- **Unbounded index growth across a large mesh.** TTL-based GC caps
  this, but adversarial high-version churn could still bloat the
  index (each version bump keeps one entry alive). Hard cap at e.g.
  10_000 indexed peers; evict by oldest `timestamp_ns` on overflow.
  Decide at C-1 whether to ship the cap or defer to a follow-up.
- **Session-open push timing.** Pushing an announcement right after
  handshake completion races the session's first inbound packet.
  Accept the race: the inbound branch is idempotent (DashMap insert
  with version skip), so a duplicate or out-of-order delivery is
  harmless. Document the behavior rather than adding ordering.
- **NAPI POJO churn.** The full `CapabilitySet` is ~12 nested fields;
  every addition to the core type requires a mirror update in the
  POJO + TS interface. Mitigation: the conversion helper lives in
  one file and has a round-trip test that catches omissions.

## Files touched (estimate)

| File | Why |
|---|---|
| `src/adapter/net/behavior/broadcast.rs` (new) | `SUBPROTOCOL_CAPABILITY_ANN` + receiver helper |
| `src/adapter/net/behavior/mod.rs` | re-export the const |
| `src/adapter/net/mesh.rs` | `MeshNode` fields, dispatch branch, `announce_capabilities*`, `find_nodes_by_filter`, GC task, session-open push |
| `src/adapter/net/mesh/config.rs` *or inline* | `require_signed_capabilities`, `capability_gc_interval` |
| `sdk/src/mesh.rs` | `Mesh::announce_capabilities`, `Mesh::find_nodes` |
| `sdk/src/capabilities.rs` | (exists; no change — re-exports already cover the new types) |
| `sdk/README.md` | extend Security section |
| `bindings/node/Cargo.toml` | add `capabilities` feature |
| `bindings/node/src/capabilities.rs` (new) | POJOs + conversions |
| `bindings/node/src/lib.rs` | declare the module, add methods on `NetMesh` |
| `sdk-ts/src/capabilities.ts` (new) | interfaces + conversion helpers |
| `sdk-ts/src/mesh.ts` | `MeshNode.announceCapabilities` / `findNodes` |
| `sdk-ts/src/index.ts` | export new types |
| `sdk-ts/test/capabilities.test.ts` (new) | two-node + TTL tests |

## Exit criteria

- Two-node Rust test passes: A announces `{tags: ["gpu", "inference"]}`,
  B's `find_nodes_by_filter({require_tags: ["gpu"]})` returns A's
  `node_id` within 2 s.
- TTL test passes: re-query after `ttl + gc_interval` returns empty.
- Two-node TS test passes (mirrors Rust).
- `cargo clippy --all-features --all-targets -- -D warnings` clean on
  both `net` and `net-sdk`.
- `cargo doc --all-features --no-deps` clean.
- No regression in existing `mesh_test.go`, `channels.test.ts`, or
  the heartbeat / channel-membership paths.

## Explicit follow-ups (not in this plan)

- ~~Multi-hop capability gossip~~ — shipped in
  [`MULTIHOP_CAPABILITY_PLAN.md`](MULTIHOP_CAPABILITY_PLAN.md).
- Full signature verification (requires node_id → entity_id binding
  from Stage E).
- Delta encoding / hash-summary + fetch for bandwidth reduction.
- `SubnetGateway`-aware announcement fan-out (only within
  accessible subnets) — ties into Stage D.
- Bench target for announcement throughput at N peers.
