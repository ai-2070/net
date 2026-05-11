# Net Rust SDK

Ergonomic Rust SDK for the Net mesh network.

The core `net` crate is the engine. This SDK is what Rust developers import.

## Install

```bash
cargo add ai2070-net-sdk
```

Or in `Cargo.toml`:

```toml
[dependencies]
ai2070-net-sdk = "0.13.0"
```

The crate publishes as `ai2070-net-sdk` on crates.io but imports as `use net_sdk::...` (the in-source crate name is preserved via package aliasing).

Features: `redis`, `jetstream`, `net` (mesh transport), `nat-traversal` (classifier + `connect_direct`, opt-in), `port-mapping` (NAT-PMP + UPnP, opt-in), `cortex` (event-sourced tasks/memories + NetDb), `compute` (daemons + migration), `groups` (replica / fork / standby), `local` (bundles `net` + `cortex` + `compute` + `groups`), `full` (bundles `local` + `redis` + `jetstream`). NAT features stay opt-in — they are *not* pulled in by `full`.

```bash
cargo add ai2070-net-sdk --features full        # everything bundled
cargo add ai2070-net-sdk --features local       # mesh + storage, no external transports
cargo add ai2070-net-sdk --features net,redis   # mesh + Redis Streams adapter
```

## Quick Start

```rust
use net_sdk::{Net, Backpressure};
use futures::StreamExt;

#[tokio::main]
async fn main() -> net_sdk::error::Result<()> {
    let node = Net::builder()
        .shards(4)
        .backpressure(Backpressure::DropOldest)
        .memory()
        .build()
        .await?;

    // Emit events
    node.emit(&serde_json::json!({"token": "hello", "index": 0}))?;
    node.emit_raw(b"{\"token\": \"world\"}" as &[u8])?;
    node.emit_str("{\"token\": \"foo\"}")?;

    // Batch
    let count = node.emit_batch(&[
        serde_json::json!({"a": 1}),
        serde_json::json!({"a": 2}),
    ])?;

    node.flush().await?;

    // Poll
    let response = node.poll(net_sdk::PollRequest {
        limit: 100,
        ..Default::default()
    }).await?;

    for event in &response.events {
        println!("{}", event.raw_str());
    }

    // Stream
    let mut stream = node.subscribe(Default::default());
    while let Some(event) = stream.next().await {
        println!("{}", event?.raw_str());
    }

    node.shutdown().await
}
```

## Typed Streams

```rust
use serde::Deserialize;
use futures::StreamExt;

#[derive(Deserialize)]
struct TokenEvent {
    token: String,
    index: u32,
}

let mut stream = node.subscribe_typed::<TokenEvent>(Default::default());
while let Some(token) = stream.next().await {
    let token = token?;
    println!("{}: {}", token.index, token.token);
}
```

## Ingestion Methods

| Method | Input | Speed | Returns |
|--------|-------|-------|---------|
| `emit(&T)` | Any `Serialize` | Fast | `Receipt` |
| `emit_raw(bytes)` | `impl Into<Bytes>` | Fastest | `Receipt` |
| `emit_str(json)` | `&str` | Fast | `Receipt` |
| `emit_batch(&[T])` | Slice of `Serialize` | Bulk | `usize` |
| `emit_raw_batch(Vec<Bytes>)` | Raw byte vecs | Bulk fastest | `usize` |

## Transports

```rust
// In-memory (default, single process)
Net::builder().memory()

// Redis Streams
Net::builder().redis(RedisAdapterConfig::new("redis://localhost:6379"))

// NATS JetStream
Net::builder().jetstream(JetStreamAdapterConfig::new("nats://localhost:4222"))

// Encrypted UDP mesh
Net::builder().mesh(NetAdapterConfig::initiator(bind, peer, psk, peer_pubkey))
```

### Persistent producer nonce (cross-restart dedup)

The JetStream and Redis adapters key dedup on a `(producer_nonce,
shard, sequence_start, i)` tuple. Without persistence, the nonce
is fresh per process — a producer that crashes mid-batch and
restarts gets a new nonce, retransmits look fresh to the
backend, and the partial-batch's accepted half is persisted
twice.

Configure `EventBusConfig::producer_nonce_path` to make the
nonce survive restart:

```rust
let cfg = EventBusConfig::builder()
    .num_shards(4)
    .redis(RedisAdapterConfig::new("redis://localhost:6379"))
    .producer_nonce_path("/var/lib/myapp/producer.nonce")
    .build()?;
```

The bus loads (or creates on first run) a u64 nonce at this
path. JetStream gets server-side dedup automatically (the
existing `Nats-Msg-Id` format absorbs the persistent nonce);
Redis Streams ships the same id as a `dedup_id` field on every
XADD, filterable consumer-side via the helper below.

## Redis Streams consumer-side dedup helper

```rust
use net_sdk::RedisStreamDedup;

// Sizing: ~10k events/sec * 1 min dedup window → ~600,000.
let mut dedup = RedisStreamDedup::with_capacity(600_000);

// Read entries from your Redis client of choice; pull the
// `dedup_id` field from each XADD entry's field map.
for entry in stream {
    let id = entry.fields["dedup_id"].as_str();
    if !dedup.is_duplicate(id) {
        process(entry);
    }
}
```

The helper is transport-agnostic — it answers a test-and-insert
question against an in-memory LRU. The producer-side
`MULTI/EXEC`-timeout race can otherwise produce duplicate stream
entries with distinct server-generated `*` ids that consumers
can't dedupe; the `dedup_id` field is stable across retries
(and across process restart when `producer_nonce_path` is
configured) so this filter cleanly removes them.

The helper is also re-exported as `net_sdk::RedisStreamDedup`;
the canonical impl lives in `net::adapter::RedisStreamDedup`.
Cross-language wrappers (NAPI, PyO3, cgo, C) ship in the
respective bindings.

## NAT Traversal (optimization, not correctness)

Two NATed peers already reach each other through the mesh's routed-handshake path. NAT traversal opens a shorter direct path when the NAT shape allows it, cutting the per-packet relay tax. Everything in this section is disabled unless the core is built with `--features nat-traversal`; without it the routed path keeps working unchanged and the five reader methods below return `Unsupported`.

```rust
// Run a reflex probe + peer-probed classification.
mesh.reclassify_nat().await;

// Read the current classification + public reflex the mesh
// advertises to peers. NatClass is Open | Cone | Symmetric | Unknown.
let class = mesh.nat_type();
let reflex = mesh.reflex_addr();                   // Option<SocketAddr>

// Directly query any connected peer's reflex.
let observed = mesh.probe_reflex(peer_node_id).await?; // -> SocketAddr

// Attempt a direct connection via the pair-type matrix.
// `coordinator` mediates the punch when the matrix picks one.
// Returns Ok regardless of path — inspect stats to learn which.
mesh.connect_direct(peer_node_id, &peer_pubkey, coordinator_node_id).await?;

// Cumulative counters partition real activity.
let stats = mesh.traversal_stats();
stats.punches_attempted;   // coordinator mediated a PunchRequest + Introduce
stats.punches_succeeded;   // ack arrived AND direct handshake landed
stats.relay_fallbacks;     // landed on the routed path after skip/fail
```

Operators with a known-public address — port-forwarded servers, successful UPnP / NAT-PMP installs — can skip the classifier sweep entirely. A runtime override forces `"open"` and the supplied `SocketAddr` on every capability announcement from this node; call `announce_capabilities` after to propagate to peers (the setter resets the rate-limit floor so the next announce is guaranteed to broadcast).

```rust
mesh.set_reflex_override("203.0.113.5:9001".parse().unwrap());
mesh.announce_capabilities(CapabilitySet::new()).await?;
// ... later, if the mapping drops:
mesh.clear_reflex_override();
mesh.announce_capabilities(CapabilitySet::new()).await?;
```

Opt into automatic UPnP-IGD / NAT-PMP port mapping via `MeshBuilder::try_port_mapping(true)` (requires `--features port-mapping`). The mesh spawns a task that probes NAT-PMP first, falls back to UPnP, installs a mapping on success, and renews every 30 minutes; on install it calls `set_reflex_override(external)` for you. A router that doesn't speak either protocol leaves the node on the classifier path — that's fine.

`SdkError::Traversal` wraps every `TraversalError` variant with a stable `kind` discriminator (`reflex-timeout` | `peer-not-reachable` | `transport` | `rendezvous-no-relay` | `rendezvous-rejected` | `punch-failed` | `port-map-unavailable` | `unsupported`). None of these is a connectivity failure — the routed path is always available regardless.

## Mesh Streams (multi-peer + back-pressure)

For direct peer-to-peer messaging outside the event bus — open a typed
stream to a specific peer, send batches, and react to back-pressure:

```rust
use net_sdk::{Mesh, MeshBuilder, StreamConfig, Reliability};
use net_sdk::error::SdkError;
use bytes::Bytes;

let node = MeshBuilder::new("127.0.0.1:9000", &[0x42u8; 32])?
    .build()
    .await?;

// ... handshake with a peer via node.inner().connect(...) ...

// Open a per-peer stream with explicit reliability + back-pressure window.
let stream = node.open_stream(
    peer_node_id,
    0x42,
    StreamConfig::new()
        .with_reliability(Reliability::Reliable)
        .with_window_bytes(256),   // max in-flight packets before Backpressure
)?;

// Three canonical daemon patterns:

// 1. Drop on pressure — best for telemetry / sampled streams.
//
// `SdkError` is `#[non_exhaustive]`. Always include a wildcard arm
// when matching: future variant additions (e.g. `Sampled`, `Unrouted`)
// will be a minor-version change, but a closed match would stop
// compiling. Match by variant where the remediation differs;
// fall through with `Err(e)` for the rest.
match node.send_on_stream(&stream, &[Bytes::from_static(b"{}")]).await {
    Ok(()) => {}
    Err(SdkError::Backpressure) => metrics::inc("stream.backpressure_drops"),
    Err(SdkError::NotConnected) => {/* peer gone or stream closed */}
    Err(e) => tracing::warn!(error = %e, "transport error"),
}

// 2. Retry with exponential backoff — best for important events.
node.send_with_retry(&stream, &[Bytes::from_static(b"{}")], 8).await?;

// 3. Block until the network lets up (bounded retry, ~13 min worst case).
node.send_blocking(&stream, &[Bytes::from_static(b"{}")]).await?;

// Live stats — per-stream tx/rx seq, in-flight, window, backpressure count.
// Returns `None` if the stream was closed or never opened.
let stats = node.stream_stats(peer_node_id, 0x42);
```

`SdkError::Backpressure` is a signal, not a policy — the transport never
retries or buffers on its own behalf. `StreamStats.backpressure_events`
counts cumulative rejections for observability. See
[`docs/TRANSPORT.md`](../docs/TRANSPORT.md) for the full contract and
[`docs/STREAM_BACKPRESSURE_PLAN.md`](../docs/STREAM_BACKPRESSURE_PLAN.md)
for the design.

## Security (identity, tokens, capabilities, subnets)

Identity, capabilities, and subnets ride the `net` feature as a
single security unit — they share the mesh's subprotocol dispatch
and operate together at runtime (subnet enforcement reuses the
capability broadcast; channel auth threads identity + capabilities
+ subnets together), so `--features net` gives you the whole
surface:

```rust
use std::time::Duration;
use net_sdk::{Identity, TokenScope};
use net_sdk::mesh::{Mesh, MeshBuilder};
use net_sdk::ChannelName;

# async fn example() -> net_sdk::error::Result<()> {
// Load once from caller-owned storage (vault / k8s secret / enclave).
let seed: [u8; 32] = [/* 32 bytes */ 0x42u8; 32];
let id = Identity::from_seed(seed);

// Stable node id across restarts — derived from the ed25519 seed.
println!("node_id = {:#x}", id.node_id());

// Issue a scoped subscribe grant to a peer and hand it over.
let subscriber_entity = Identity::generate(); // pretend we received this
let channel = ChannelName::new("sensors/temp").unwrap();
let token = id.issue_token(
    subscriber_entity.entity_id().clone(),
    TokenScope::SUBSCRIBE,
    &channel,
    Duration::from_secs(300),
    0, // delegation depth — 0 forbids re-delegation
);
// `issue_token` soft-clamps `Duration::ZERO` to 1 second (and
// `debug_assert!`s in dev so the misuse is loud in tests). Callers
// that need to *reject* zero-TTL inputs at the boundary should use
// `id.try_issue_token(...)`, which returns `TokenError::ZeroTtl`.
// Token is a signed, transport-ready blob.
assert_eq!(token.to_bytes().len(), net_sdk::PermissionToken::WIRE_SIZE);

// Pin this identity on the mesh builder — without this call the
// builder generates an ephemeral keypair and the node_id changes on
// every restart.
let _mesh = MeshBuilder::new("127.0.0.1:9001", &[0x42u8; 32])?
    .identity(id)
    .build()
    .await?;
# Ok(())
# }
```

**What's wired in this release:**

- `Identity` generation / seed round-trip / signing / token issuance
  + verification + install + lookup.
- `MeshBuilder::identity(...)` pins the keypair used by the mesh's
  Noise handshake so `node_id()` is stable.
- **Capability announcements — cross-node (direct-peer).** See the
  subsection below.
- Re-exports of `SubnetId` / `SubnetPolicy` / `SubnetRule` (builder
  hook + gateway wiring land next).

**Treat `Identity::to_bytes()` as secret material** — it's the
32-byte ed25519 seed. The SDK never touches a hardcoded path; where
you put the bytes (disk, vault, enclave, k8s secret) is your call.

### Capability announcements

`Mesh::announce_capabilities(caps)` pushes a `CapabilityAnnouncement`
to every directly-connected peer and self-indexes locally.
`Mesh::find_nodes(filter)` queries the local index — results include
this node's own id when self matches.

```rust
use net_sdk::capabilities::{CapabilityFilter, CapabilitySet, GpuInfo, GpuVendor, HardwareCapabilities};
use net_sdk::mesh::MeshBuilder;

# async fn example() -> net_sdk::error::Result<()> {
let mesh = MeshBuilder::new("127.0.0.1:0", &[0x42u8; 32])?
    .build()
    .await?;

let hw = HardwareCapabilities::new()
    .with_cpu(16, 32)
    .with_memory(65_536)
    .with_gpu(GpuInfo::new(GpuVendor::Nvidia, "RTX 4090", 24_576));
mesh.announce_capabilities(
    CapabilitySet::new().with_hardware(hw).add_tag("gpu"),
)
.await?;

// Self-match: returns our own node_id.
let hits = mesh.find_nodes(
    &CapabilityFilter::new().require_gpu().with_min_vram(16_384),
);
assert!(hits.contains(&mesh.node_id()));
mesh.shutdown().await?;
# Ok(())
# }
```

#### Scoped discovery (reserved `scope:*` tags)

A provider can narrow *who their query result reaches* by tagging
its `CapabilitySet` with reserved `scope:*` tags. Queries call
`find_nodes_scoped(filter, scope)` (or `find_best_node_scoped`)
to filter candidates. The wire format and forwarders are
untouched — enforcement is purely query-side.

```rust
use net_sdk::capabilities::{CapabilityFilter, CapabilitySet, ScopeFilter};
# async fn example(mesh: &net_sdk::mesh::Mesh) -> net_sdk::error::Result<()> {
// GPU pool advertised to one tenant only.
mesh.announce_capabilities(
    CapabilitySet::new()
        .add_tag("model:llama3-70b")
        .with_tenant_scope("oem-123"),
)
.await?;

// Tenant-scoped query — returns this node + any `Global` (untagged) peers.
let oem = mesh.find_nodes_scoped(
    &CapabilityFilter::new().require_tag("model:llama3-70b"),
    &ScopeFilter::Tenant("oem-123"),
);
# let _ = oem;
# Ok(())
# }
```

Reserved tag forms: `scope:subnet-local` (visible only under
`ScopeFilter::SameSubnet`), `scope:tenant:<id>`,
`scope:region:<name>`. Strictest scope wins —
`subnet-local` dominates tenant/region tags on the same set.
Untagged peers resolve to `Global` and stay visible under
permissive queries (matches the v1 default; you opt *in* to
narrowing, never out by accident). Full design:
[`docs/SCOPED_CAPABILITIES_PLAN.md`](../docs/SCOPED_CAPABILITIES_PLAN.md).

**Scope today:**

- Multi-hop fan-out bounded by `MAX_CAPABILITY_HOPS = 16`.
  Forwarders re-broadcast every received announcement to their
  other peers (minus the sender and any split-horizon peer),
  bumping `hop_count` outside the signed envelope so the origin's
  signature keeps verifying end-to-end. Dedup on
  `(origin, version)` drops duplicates at diamond-topology
  converge points. See
  [`docs/MULTIHOP_CAPABILITY_PLAN.md`](../docs/MULTIHOP_CAPABILITY_PLAN.md).
- Origin-side rate limiting: `min_announce_interval` (default 10s)
  coalesces rapid `announce_capabilities` calls into a single
  broadcast, preventing a busy-loop announcer from flooding the
  mesh. Self-index + late-joiner session-open push still reflect
  the latest caps inside the window.
- TTL + GC eviction: per-announcement `ttl_secs` drives
  `CapabilityIndex::gc()` on a configurable tick
  (`capability_gc_interval`, default 60 s).
- Signatures are advisory. The `require_signed_capabilities` config
  knob rejects unsigned announcements at the receiver, but
  *signature validity* is not enforced end-to-end yet — it requires
  a `node_id → entity_id` binding that lands with channel auth.

Wire-level details and the subprotocol layout live in
[`docs/CAPABILITY_BROADCAST_PLAN.md`](../docs/CAPABILITY_BROADCAST_PLAN.md).

#### Capability enhancements (typed taxonomy + predicates + validation)

The substrate's `CapabilitySet` is a `{ tags, metadata }` wire shape
post-Phase A.5.N. Beyond `announce_capabilities` / `find_nodes`, the
SDK exposes the caller-local enhancement layer mirroring
[`CAPABILITY_ENHANCEMENTS_PLAN.md`](../docs/plans/CAPABILITY_ENHANCEMENTS_PLAN.md):

```rust
use net_sdk::capabilities::{
    // Typed taxonomy
    Tag, TagKey, TaxonomyAxis, RESERVED_PREFIXES,
    // Lazy view projections
    CapabilitySet, CapabilityViews,
    // Diff
    CapabilitySetDiff, MetadataChange,
    // Predicates (substrate AST + nRPC envelope + trace)
    predicate::{
        Predicate, EvalContext, PredicateDebugReport,
        predicate_to_rpc_header, predicate_from_rpc_headers,
        RPC_WHERE_HEADER,
    },
    pred,
    // Validation
    schema::{validate_capabilities, ValidationReport, SchemaError},
};

# async fn example() -> Result<(), Box<dyn std::error::Error>> {
# let caps = CapabilitySet::default();
# let prev = CapabilitySet::default();
# let tags: Vec<Tag> = Vec::new();
# let metadata = std::collections::BTreeMap::<String, String>::new();
// Lazy view projections: per-axis OnceCell-cached decode.
let views = caps.views();
let _hw = views.hardware();          // Decodes hardware.* tags on first call.
let _sw = views.software();          // Independent of hardware decode.

// Predicate AST — language-idiomatic builder + macro form.
let p = pred!(and [
    pred!(exists "hardware.gpu"),
    pred!(num_at_least "hardware.memory_mb", 65536.0),
    pred!(metadata_equals "intent", "ml-training"),
]);

// Local evaluation against any (tags, metadata) context.
let ctx = EvalContext::new(&tags, &metadata);
let _matched = p.evaluate(&ctx);

// Single-evaluation trace — every clause's verdict + skipped
// children for short-circuit AND/OR.
let (_result, _trace) = p.evaluate_with_trace(&ctx);

// Wire form for nRPC `net-where:` headers — pair with the
// substrate's `*_with_headers` calls so server-side filtering
// shortcircuits without re-running the predicate per hop.
let (_name, _value): (String, Vec<u8>) = predicate_to_rpc_header(&p)?;
let _ = RPC_WHERE_HEADER;
// Reverse direction: parse a peer-supplied header back into the AST.
// `predicate_from_rpc_headers` returns `Option<Result<Predicate, _>>`
// — `None` when the `net-where` header is absent, `Some(Err(_))`
// on malformed payload. Use `.transpose()?` to flip into
// `Result<Option<Predicate>, _>` and propagate decode errors.
# let header_pairs: Vec<(String, Vec<u8>)> = Vec::new();
let _decoded: Option<_> = predicate_from_rpc_headers(&header_pairs).transpose()?;

// Validate against the canonical schema.
let report = validate_capabilities(&caps);
if !report.is_valid() {
    eprintln!("schema errors: {:?}", report.errors);
}

// Detect what changed between two snapshots — drives placement
// re-evaluation when a daemon's CapabilitySet updates.
let _diff = caps.diff(&prev);

// Profile a predicate across a corpus — per-clause hit/miss
// stats with short-circuit accounting. Bindings (TS / Python /
// Go) wrap this with a `redact_metadata_keys` helper for safe
// persistence; Rust callers compose redaction at the application
// layer.
# let corpus = std::iter::empty::<EvalContext<'_>>();
let _report = PredicateDebugReport::from_evaluations(&p, corpus);
# Ok(())
# }
```

For host-side placement-filter callbacks, implement
[`PlacementFilter`](https://docs.rs/ai2070-net-sdk/latest/net_sdk/capabilities/trait.PlacementFilter.html)
directly and register the impl with
[`global_placement_filter_registry()`](https://docs.rs/ai2070-net-sdk/latest/net_sdk/capabilities/fn.global_placement_filter_registry.html);
the TS / Python / Go bindings auto-wrap closures via
`placement_filter_from_fn` for the same registry.

The wire format is byte-identical across all five bindings (Rust /
TS / Python / Go / C) — pinned by the JSON fixtures under
`tests/cross_lang_capability/`. A worked-examples guide for each
enhancement API:
[`CAPABILITY_ENHANCEMENTS_USAGE.md`](../docs/CAPABILITY_ENHANCEMENTS_USAGE.md).

### Subnets (visibility partitioning)

`MeshBuilder::subnet(id)` pins a node to one of 2³² possible 4-level
subnet ids; `subnet_policy(policy)` derives each *peer's* subnet by
applying a shared tag-matching policy to their inbound
`CapabilityAnnouncement`. Channel visibility then gates publish
fan-out and subscribe authorization against that geometry.

```rust
use std::sync::Arc;
use net_sdk::capabilities::CapabilitySet;
use net_sdk::mesh::MeshBuilder;
use net_sdk::subnets::{SubnetId, SubnetPolicy, SubnetRule};

# async fn example() -> net_sdk::error::Result<()> {
// Mesh-wide policy: `region:<x>` maps to the level 0 byte,
// `fleet:<x>` maps to level 1. Every node in the mesh must
// construct the same policy — mismatched policies yield
// asymmetric views of peer subnets.
let policy = Arc::new(
    SubnetPolicy::new()
        .add_rule(SubnetRule::new("region:", 0).map("us", 3).map("eu", 4))
        .add_rule(SubnetRule::new("fleet:", 1).map("blue", 7).map("green", 8)),
);

let node = MeshBuilder::new("127.0.0.1:0", &[0x42u8; 32])?
    .subnet(SubnetId::new(&[3, 7]))           // us/blue
    .subnet_policy(policy)
    .build()
    .await?;

// Announce with tags matching the policy so peers derive the same
// subnet (`[3, 7]`) when they apply their own policy to our caps.
node.announce_capabilities(
    CapabilitySet::new()
        .add_tag("region:us")
        .add_tag("fleet:blue"),
)
.await?;

// Register a SubnetLocal channel — only peers with the exact same
// SubnetId will be accepted as subscribers and included in publish
// fan-out. Any cross-subnet subscribe rejects with `Unauthorized`.
// (Channel registration uses the channel config types from the
// `Channels` section below.)
node.shutdown().await?;
# Ok(())
# }
```

**Visibility semantics** (from `Visibility` enum):

| Variant | Delivery |
|---|---|
| `Global` | every peer |
| `SubnetLocal` | peers with an identical `SubnetId` |
| `ParentVisible` | same subnet OR either side is an ancestor of the other |
| `Exported` | per-channel export table — **deferred**, drops in v1 |

**Scope today**:

- Enforcement is end-to-end through the publish + subscribe gates.
  Filtered subscribers do not appear in `PublishReport.attempted`.
- Peer subnets are derived locally from each peer's capability
  announcement via `SubnetPolicy::assign`. No dedicated subnet
  subprotocol; announcements piggyback on the capability broadcast
  from Stage C.
- Multi-hop subnet-aware routing (forwarding filters at the packet
  header) is a follow-up.

Wire-level details and the enforcement matrix live in
[`docs/SUBNET_ENFORCEMENT_PLAN.md`](../docs/SUBNET_ENFORCEMENT_PLAN.md).

### Channel authentication

`ChannelConfig` carries three auth knobs that are now enforced
end-to-end at both the subscribe gate and the publish path:

- `publish_caps: CapabilityFilter` — publisher must satisfy before
  fan-out. Failing publishes return an `AdapterError`; no peers are
  attempted.
- `subscribe_caps: CapabilityFilter` — subscribers must satisfy
  before being added to the roster. Failures surface as
  `SdkError::ChannelRejected(Some(Unauthorized))`.
- `require_token: bool` — subscribers must present a valid
  `PermissionToken` whose subject matches their entity id. The
  token rides on the subscribe message; the publisher verifies the
  ed25519 signature on arrival, installs it in its local
  `TokenCache`, then runs `can_subscribe`.

```rust
use std::sync::Arc;
use std::time::Duration;
use net_sdk::capabilities::{CapabilityFilter, CapabilitySet};
use net_sdk::mesh::MeshBuilder;
use net_sdk::{
    ChannelConfig, ChannelId, ChannelName, Identity, PublishConfig, Reliability,
    SubscribeOptions, TokenScope,
};
# async fn example() -> net_sdk::error::Result<()> {
// Both sides bind caller-owned identities so tokens + entity_ids
// are load-bearing.
let publisher_identity = Identity::generate();
let subscriber_identity = Identity::generate();

let publisher = MeshBuilder::new("127.0.0.1:9001", &[0x42u8; 32])?
    .identity(publisher_identity.clone())
    .build()
    .await?;

// Register a channel that requires `gpu` AND a token.
let name = ChannelName::new("events/inference").unwrap();
let filter = CapabilityFilter::new().require_tag("gpu");
publisher.register_channel(
    ChannelConfig::new(ChannelId::new(name.clone()))
        .with_subscribe_caps(filter)
        .with_require_token(true),
);

// Issue a SUBSCRIBE-scope token for the subscriber. This also
// pre-caches it in the publisher's identity (unused for this
// flow since the subscriber will present the same token on the
// wire, but useful for the "pre-seed" pattern).
let token = publisher_identity.issue_token(
    subscriber_identity.entity_id().clone(),
    TokenScope::SUBSCRIBE,
    &name,
    Duration::from_secs(300), // zero is soft-clamped to 1s; use try_issue_token to reject
    0,
);

// Subscriber attaches the token.
let subscriber: &net_sdk::Mesh = unimplemented!();
subscriber
    .subscribe_channel_with(
        publisher.node_id(),
        &name,
        SubscribeOptions { token: Some(token) },
    )
    .await?;
# Ok(())
# }
```

**Scope today**:

- Full enforcement at subscribe + publish; empty-caps / missing-
  entity defaults fail closed when `require_token` is set.
- Every publish fan-out consults the `AuthGuard` fast path (4 KB
  bloom filter + verified-subscribe cache) so revocations apply on
  the next publish without a roster refresh. Single-threaded
  microbenchmark: ~20 ns per `check_fast` call.
- Periodic token-expiry sweep (default 30 s,
  `MeshNodeConfig::with_token_sweep_interval`) evicts subscribers
  whose tokens age out of their TTL — they stop receiving events
  within one sweep tick instead of staying on the roster forever.
- Per-peer auth-failure rate limiter (`with_auth_failure_limit`,
  default 16 failures per 60 s window → 30 s throttle) short-
  circuits bad-token subscribe storms with `AckReason::RateLimited`
  before ed25519 verification runs. Successful subscribes clear
  the counter.
- `CapabilityAnnouncement` now carries the sender's `entity_id` and
  is signed — verified end-to-end (closes the "signature advisory"
  caveat from the capability section above).
- `node_id → entity_id` is pinned on first sight (TOFU); rebind
  attempts in later announcements are silently rejected.
- Any auth-rule denial surfaces as `AckReason::Unauthorized`;
  throttled bursts surface as `AckReason::RateLimited`. Sub-reasons
  within the auth rejection (cap-failed vs token-failed vs
  subnet-failed) are not split yet.

Wire-format details and the token presentation flow live in
[`docs/CHANNEL_AUTH_PLAN.md`](../docs/CHANNEL_AUTH_PLAN.md); the
fast-path / sweep / rate-limit design lives in
[`docs/CHANNEL_AUTH_GUARD_PLAN.md`](../docs/CHANNEL_AUTH_GUARD_PLAN.md).

## Channels (distributed pub/sub)

Named pub/sub over the encrypted mesh. Publishers register channels
with access policy; subscribers ask to join via a membership
subprotocol with an Ack round-trip. `publish` / `publish_many` fan
payloads out to every current subscriber.

```rust
use bytes::Bytes;
use net_sdk::mesh::{Mesh, MeshBuilder};
use net_sdk::{ChannelConfig, ChannelId, ChannelName, PublishConfig, Reliability, Visibility};

# async fn example() -> net_sdk::error::Result<()> {
let publisher = MeshBuilder::new("127.0.0.1:9001", &[0x42u8; 32])?
    .build().await?;
let subscriber = MeshBuilder::new("127.0.0.1:9000", &[0x42u8; 32])?
    .build().await?;
// (handshake omitted — see Mesh Streams example)

// Publisher registers a channel.
let channel = ChannelName::new("sensors/temp").unwrap();
publisher.register_channel(
    ChannelConfig::new(ChannelId::new(channel.clone()))
        .with_visibility(Visibility::Global)
        .with_reliable(true)
        .with_priority(2),
);

// Subscriber joins. Network-rejected acks surface as
// `SdkError::ChannelRejected(reason)`.
subscriber.subscribe_channel(publisher.inner().node_id(), &channel).await?;

// Fan out.
let report = publisher.publish(
    &channel,
    Bytes::from_static(b"22.5"),
    PublishConfig {
        reliability: Reliability::Reliable,
        ..Default::default()
    },
).await?;
println!("{}/{} delivered", report.delivered, report.attempted);
# Ok(())
# }
```

`register_channel` stores into a shared `ChannelConfigRegistry`
installed on the underlying `MeshNode` at build time — so multiple
`register_channel` calls are just inserts and require only `&Mesh`,
not `&mut`.

Subscribers today receive payloads via the existing `recv` /
`recv_shard` surface. A dedicated `on_channel(&ChannelName)` stream
is a follow-up.

## CortEX & NetDb (event-sourced state)

For typed, event-sourced state — tasks and memories with filterable
queries and reactive watches — enable the `cortex` feature and import
from `net_sdk::cortex`:

```rust
use net_sdk::cortex::{NetDb, Redex, TaskStatus};
use futures::StreamExt;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let redex = Redex::new();                       // or `.with_persistent_dir("/var/lib/net")`
    let db = NetDb::builder(redex)
        .origin(0xABCD_EF01)                        // producer identity on every event
        .with_tasks()
        .with_memories()
        .build()?;

    // Ingest through the domain API; no EventMeta plumbing.
    let seq = db.tasks().create(1, "write docs", 0)?;
    db.tasks().wait_for_seq(seq).await;             // wait for the fold to apply

    // Query the materialized state.
    assert_eq!(db.tasks().count(), 1);

    // Snapshot + watch: "paint what's there now, then react to changes."
    // The stream drops only leading emissions that equal the snapshot,
    // so a mutation racing during construction is still delivered.
    let watcher = db.tasks().watch().where_status(TaskStatus::Pending);
    let (snapshot, mut deltas) = db.tasks().snapshot_and_watch(watcher);
    println!("initial: {} pending", snapshot.len());
    while let Some(batch) = deltas.next().await {
        println!("delta: {} pending", batch.len());
    }
    Ok(())
}
```

### Persistence

With `redex-disk` (pulled in by `cortex`), point `Redex` at a directory
and flip `persistent(true)` on the builder:

```rust
let redex = Redex::new().with_persistent_dir("/var/lib/net/redex");
let db = NetDb::builder(redex)
    .origin(origin_hash)
    .persistent(true)
    .with_tasks()
    .build()?;
```

Use `RedexFileConfig` + `FsyncPolicy` (both re-exported from
`net_sdk::cortex`) to tune per-file fsync semantics.

### Raw RedEX file

For domain-agnostic persistent logs (no CortEX, no fold, no typed
state), use the `Redex` manager directly via `Redex::open_file`. This
unlocks `RedexFile::append` / `tail` for custom event pipelines.

See [`docs/STORAGE_AND_CORTEX.md`](../docs/STORAGE_AND_CORTEX.md) for
the full narrative and
[`docs/REDEX_PLAN.md`](../docs/REDEX_PLAN.md) /
[`docs/CORTEX_ADAPTER_PLAN.md`](../docs/CORTEX_ADAPTER_PLAN.md) for the
design.

## nRPC (request / response over the mesh)

nRPC is the request/response convention layer riding on top of the
pub/sub mesh + CortEX folds. It turns a directed channel pair
(`<service>.requests` / `<service>.replies.<caller_origin>`) into a
typed RPC surface with deadlines, queue-group fan-out, response
streaming, and end-to-end cancellation. Enable the `cortex` feature
(nRPC depends on the CortEX rpc.rs fold).

### Typed serve + call

```rust
use net_sdk::mesh::{Mesh, MeshBuilder};
use net_sdk::mesh_rpc::CallOptions;
use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Serialize, Deserialize)]
struct EchoSumRequest { text: String, numbers: Vec<i64> }
#[derive(Serialize, Deserialize)]
struct EchoSumResponse { echo: String, sum: i64 }

# async fn example() -> net_sdk::error::Result<()> {
let server = MeshBuilder::new("127.0.0.1:9001", &[0x42u8; 32])?.build().await?;
let client = MeshBuilder::new("127.0.0.1:9000", &[0x42u8; 32])?.build().await?;
// (handshake omitted — see Mesh Streams example)

// Server side: register a typed handler. Returns a `ServeHandle`
// that unregisters on Drop AND lets in-flight handlers complete
// (no abort).
let _handle = server.serve_rpc_typed(
    "echo_sum",
    |req: EchoSumRequest| async move {
        Ok::<_, String>(EchoSumResponse {
            echo: req.text,
            sum: req.numbers.iter().sum(),
        })
    },
)?;

// Client side: typed call with a 200ms deadline.
let opts = CallOptions::default().with_deadline(Duration::from_millis(200));
let resp: EchoSumResponse = client.call_typed(
    server.inner().node_id(),
    "echo_sum",
    &EchoSumRequest { text: "hi".into(), numbers: vec![1, 2, 3] },
    opts,
).await?;
assert_eq!(resp.sum, 6);
# Ok(())
# }
```

`call_typed` and `call_service_typed` (service-discovery variant)
default to JSON. Use the raw-bytes path (`call` / `call_service`)
when you own the encoding.

### Streaming responses

```rust
use futures::StreamExt;
use net_sdk::mesh_rpc::CallOptions;

# async fn example(client: net_sdk::mesh::Mesh, target: u64) -> net_sdk::error::Result<()> {
// Optional flow control: install an initial credit window.
let opts = CallOptions::default().with_stream_window_initial(8);
let mut stream = client.call_streaming_typed::<MyReq, MyChunk>(
    target, "tail", &MyReq { tail: "events" }, opts,
).await?;
while let Some(chunk) = stream.next().await {
    let chunk = chunk?;          // Result<MyChunk, RpcError>
    process(chunk);
}
// Dropping the stream emits CANCEL to the server (best-effort);
// in-flight chunks are silently discarded by the client fold.
# Ok(())
# }
# fn process<T>(_: T) {}
# #[derive(serde::Serialize, serde::Deserialize)] struct MyReq { tail: &'static str }
# #[derive(serde::Serialize, serde::Deserialize)] struct MyChunk;
```

`RpcStream::grant(amount)` issues an explicit credit publish
when batched cadence is preferable to the per-chunk auto-grant
default (no-op on streams that didn't opt into flow control).

### Resilience helpers

`Mesh::call_with_retry` wraps a unary call in exponential backoff
with jitter; the default `RetryPolicy::default()` retries
`no_route` + `transport` and skips terminal errors:

```rust
use net_sdk::mesh_rpc_resilience::{RetryPolicy, HedgePolicy};
use net_sdk::mesh_rpc::CallOptions;
use bytes::Bytes;
use std::time::Duration;

# async fn example(client: net_sdk::mesh::Mesh, target: u64) -> net_sdk::error::Result<()> {
let policy = RetryPolicy::default()
    .with_max_attempts(4)
    .with_initial_backoff(Duration::from_millis(50))
    .with_max_backoff(Duration::from_secs(1));
let resp = client.call_with_retry(
    target, "echo", Bytes::from_static(b"hi"),
    CallOptions::default(), policy,
).await?;

// Hedging fans out parallel attempts on a delay; first success wins.
let _hedge = HedgePolicy::default().with_max_parallel(3);
# let _ = resp;
# Ok(())
# }
```

`CircuitBreaker` (in `mesh_rpc_resilience`) tracks consecutive
failures and trips open after a threshold; open breakers reject
calls outright until the cooldown allows a half-open probe.

### Errors

`RpcError` is the unified failure surface. Variants: `NoRoute`,
`Timeout`, `ServerError { status, message }`, `Transport`,
`Codec { direction, message }`. Status codes use `u16`; the
application-defined band is `0x8000..=0xFFFF`. Two stable
constants ship in `net_sdk::mesh_rpc`:

| Status hex | Constant                       | Trigger                                          |
| ---------- | ------------------------------ | ------------------------------------------------ |
| `0x0000`   | `RpcStatus::Ok`                | Normal response.                                 |
| `0x8000`   | `NRPC_TYPED_BAD_REQUEST`       | Typed handler couldn't decode the request body.  |
| `0x8001`   | `NRPC_TYPED_HANDLER_ERROR`     | Typed handler ran but returned an exception.     |

Cross-binding contract spec — including the canonical
`cross_lang_echo_sum` service used by every binding's wire-format
compat test — lives in [`../README.md#nrpc`](../README.md#nrpc).

## Compute (daemons + migration)

Enable the `compute` feature to run `MeshDaemon`s from your SDK
code. A daemon is a stateful event processor with a deterministic
causal chain; `DaemonRuntime` owns the factory table, the per-
daemon hosts, the lifecycle gate (`Registering → Ready →
ShuttingDown`), and the migration subprotocol plumbing. The full
staging and design notes live in
[`docs/SDK_COMPUTE_SURFACE_PLAN.md`](../docs/SDK_COMPUTE_SURFACE_PLAN.md);
the runtime readiness fence in
[`docs/DAEMON_RUNTIME_READINESS_PLAN.md`](../docs/DAEMON_RUNTIME_READINESS_PLAN.md).

```rust
use std::sync::Arc;
use bytes::Bytes;
use net_sdk::{Identity, MeshBuilder};
use net_sdk::capabilities::CapabilityFilter;
use net_sdk::compute::{
    CausalEvent, ComputeDaemonError as DaemonError, DaemonHostConfig,
    DaemonRuntime, MeshDaemon,
};

struct EchoDaemon;
impl MeshDaemon for EchoDaemon {
    fn name(&self) -> &str { "echo" }
    fn requirements(&self) -> CapabilityFilter { CapabilityFilter::default() }
    fn process(&mut self, event: &CausalEvent) -> Result<Vec<Bytes>, DaemonError> {
        Ok(vec![event.payload.clone()])
    }
}

# async fn example() -> Result<(), Box<dyn std::error::Error>> {
let mesh = MeshBuilder::new("127.0.0.1:0", &[0x42u8; 32])?
    .build()
    .await?;
let rt = DaemonRuntime::new(Arc::new(mesh));

// 1. Register factories BEFORE flipping the runtime to Ready.
rt.register_factory("echo", || Box::new(EchoDaemon))?;

// 2. Ready the runtime. After this point spawn / migration accept.
rt.start().await?;

// 3. Spawn a local daemon. `Identity` pins the daemon's ed25519
//    keypair → `origin_hash` / `entity_id` are stable across migrations.
let handle = rt
    .spawn("echo", Identity::generate(), DaemonHostConfig::default())
    .await?;
println!("origin = {:#x}", handle.origin_hash);

// 4. Hand events to the daemon. The SDK links each event into the
//    causal chain and forwards produced payloads to subscribers.
let event = CausalEvent::new(handle.origin_hash, 1, Bytes::from_static(b"hi"));
rt.deliver(handle.origin_hash, &event)?;

// 5. Clean shutdown — stops every daemon and tears down the gate.
rt.shutdown().await?;
# Ok(())
# }
```

The `MeshDaemon` trait is intentionally minimal:

```rust
pub trait MeshDaemon: Send + Sync {
    fn name(&self) -> &str;
    fn requirements(&self) -> CapabilityFilter;
    fn process(&mut self, event: &CausalEvent) -> Result<Vec<Bytes>, DaemonError>;
    fn snapshot(&self) -> Option<Bytes> { None }        // opt into migration
    fn restore(&mut self, state: Bytes) -> Result<(), DaemonError> { Ok(()) }
}
```

`requirements()` feeds the `PlacementScheduler` — a GPU daemon
advertises `require_gpu()` and only lands on nodes whose
`CapabilityAnnouncement` matches. `snapshot` / `restore` are opt-in:
leave the defaults for stateless daemons; implement them to enable
live migration of stateful ones.

### Migration

Once a daemon is up, `start_migration` orchestrates the six-phase
cutover to another node: `Snapshot → Transfer → Restore → Replay →
Cutover → Complete`. The source seals the daemon's seed into the
outbound snapshot (sealed with the target's X25519 pubkey); the
target rebuilds the daemon via the factory registered under the same
`kind`, replays any events that arrived during transfer, then
activates.

```rust
use net_sdk::compute::{MigrationHandle, MigrationOpts};

// Caller side: start a migration to `target_node`. Returns as soon
// as the SNAPSHOT phase has begun; `wait()` drives to completion.
let mig: MigrationHandle = rt
    .start_migration(handle.origin_hash, /* source */ src_id, /* target */ dst_id)
    .await?;
assert_eq!(mig.origin_hash, handle.origin_hash);
println!("phase = {:?}", mig.phase());   // Some(MigrationPhase::Snapshot)
mig.wait().await?;                       // blocks to Complete
```

- `start_migration_with(origin, src, dst, MigrationOpts { seal_seed, .. })`
  toggles seed-sealing and other advanced knobs.
- On the *target* side, `DaemonRuntime::register_migration_target_identity(...)`
  pins the X25519 keypair used to unseal inbound seeds. If unset,
  the runtime rejects inbound migrations with
  `MigrationFailureReason::SealedSeedMissing`.
- Failures from any of the six phases surface as a
  `MigrationFailureReason` variant on `MigrationHandle::wait()` (or
  on the receiving `expect_migration` hook), mirroring the wire-
  level `MigrationFailureMessage`.

### Stop / snapshot / inspect

| Method | Description |
|---|---|
| `rt.spawn(kind, identity, cfg)` | Launch a daemon from a registered kind |
| `rt.spawn_from_snapshot(...)` | Bootstrap from a previously captured `StateSnapshot` |
| `rt.stop(origin)` | Gracefully stop a local daemon |
| `rt.snapshot(origin)` | Capture a `StateSnapshot` for persistence / migration |
| `rt.deliver(origin, &event)` | Feed the daemon an event (returns produced payloads) |
| `rt.daemon_count()` / `rt.is_ready()` | Runtime introspection |
| `rt.start_migration(origin, src, dst)` | Orchestrate a live migration |
| `rt.subscribe_channel(origin, &name, ...)` | Attach a daemon to a mesh channel |
| `handle.stats()` / `handle.snapshot()` | Per-daemon observability |

Errors surface as `ComputeDaemonError` (`NotReady` before `start`,
`FactoryNotFound(kind)`, `FactoryAlreadyRegistered(kind)`,
`ShuttingDown` after `shutdown`, plus `Core(_)` for the underlying
scheduler / registry failures).

## Groups (replica / fork / standby)

Enable the `groups` feature (implies `compute`) to spawn logical
clusters of daemons from a single `DaemonRuntime`. Three flavours
share one coordination layer:

- `ReplicaGroup` — N interchangeable copies. Each replica gets a
  deterministic identity from `group_seed + index`, so a replacement
  respawned on another node has a stable `origin_hash`. Load-balances
  inbound events across healthy members; auto-replaces on node failure.
- `ForkGroup` — N independent daemons forked from a common parent at
  `fork_seq`. Unique keypairs, shared ancestry via a verifiable
  `ForkRecord` (sentinel hash linking each fork to the parent chain).
- `StandbyGroup` — active-passive replication. One member processes
  events; standbys hold snapshots and catch up via `sync_standbys()`.
  On active failure, the most-synced standby promotes and replays the
  events buffered since the last sync.

```rust
use net_sdk::compute::{DaemonHostConfig, DaemonRuntime};
use net_sdk::groups::{
    ForkGroup, ForkGroupConfig, GroupError, ReplicaGroup, ReplicaGroupConfig,
    RequestContext, StandbyGroup, StandbyGroupConfig,
};
use net_sdk::groups::common::Strategy;

# async fn example(rt: DaemonRuntime) -> Result<(), GroupError> {
// Register the factory the group will call for each member.
rt.register_factory("counter", || Box::new(CounterDaemon::new()))?;

// --- ReplicaGroup ----------------------------------------------------
let replicas = ReplicaGroup::spawn(&rt, "counter", ReplicaGroupConfig {
    replica_count: 3,
    group_seed: [0x11; 32],
    lb_strategy: Strategy::ConsistentHash,
    host_config: DaemonHostConfig::default(),
})?;

let ctx = RequestContext::new().with_routing_key("user:42");
let origin = replicas.route_event(&ctx)?;
// rt.deliver(origin, &event)?;   // hand the event to the chosen replica

replicas.scale_to(5)?;                    // grow
replicas.on_node_failure(failed_id)?;     // respawn elsewhere

// --- ForkGroup -------------------------------------------------------
let forks = ForkGroup::fork(
    &rt,
    "counter",
    /* parent_origin */ 0xabcd_ef01,
    /* fork_seq */ 42,
    ForkGroupConfig {
        fork_count: 3,
        lb_strategy: Strategy::RoundRobin,
        host_config: DaemonHostConfig::default(),
    },
)?;
assert!(forks.verify_lineage());           // sentinel + signature ok
let records = forks.fork_records();        // one ForkRecord per member

// --- StandbyGroup ----------------------------------------------------
let hot = StandbyGroup::spawn(&rt, "counter", StandbyGroupConfig {
    member_count: 3,                       // 1 active + 2 standbys
    group_seed: [0x77; 32],
    host_config: DaemonHostConfig::default(),
})?;
// rt.deliver(hot.active_origin(), &event)?;
hot.on_event_delivered(event.clone());     // buffer for replay
hot.sync_standbys()?;                      // periodic catchup
// On active-node failure: hot.on_node_failure(failed_id)?;
# Ok(())
# }
```

Errors surface as `GroupError`: `NotReady` (runtime not started),
`FactoryNotFound(kind)` (`kind` was never registered), `Core(_)`
wrapping `InvalidConfig` / `PlacementFailed` / `RegistryFailed`, and
`Daemon(_)` for runtime-level failures. Match on the variant to
dispatch — the wire form through the FFI is
`daemon: group: <kind>[: detail]` and stays consistent across all
language bindings.

Full staging, wire formats, and rationale:
[`docs/SDK_GROUPS_SURFACE_PLAN.md`](../docs/SDK_GROUPS_SURFACE_PLAN.md).
Core semantics (placement spread, health aggregation, failure
domains) live in [`../README.md#daemons`](../README.md#daemons).

## API

| Method | Description |
|--------|-------------|
| `Net::builder()` | Create a configuration builder |
| `emit(&T)` | Emit a serializable event |
| `emit_raw(bytes)` | Emit raw bytes (fastest) |
| `emit_str(json)` | Emit a JSON string |
| `emit_batch(&[T])` | Batch emit |
| `emit_raw_batch(vecs)` | Batch emit raw bytes |
| `poll(request)` | One-shot poll |
| `subscribe(opts)` | Async event stream |
| `subscribe_typed::<T>(opts)` | Typed async stream |
| `stats()` | Ingestion statistics |
| `shards()` | Number of active shards |
| `health()` | Check node health |
| `flush()` | Flush pending batches |
| `shutdown()` | Graceful shutdown |
| `bus()` | Access underlying `EventBus` |

`SdkError` is `#[non_exhaustive]`; structured ingestion failures
(`Sampled`, `Unrouted`, `Backpressure`) and stream-side rejections
(`ChannelRejected`) surface as their own variants rather than being
funnelled through `Ingestion(String)`. Always include a wildcard
arm when matching so a future variant addition is a minor-version
change, not a breaking one.

### Channel surface (feature `net`)

| Method | Description |
|---|---|
| `mesh.register_channel(config)` | Install / replace a channel's access config |
| `mesh.subscribe_channel(peer_id, &name)` | Ask `peer_id` to add us as a subscriber |
| `mesh.unsubscribe_channel(peer_id, &name)` | Leave a channel (idempotent) |
| `mesh.publish(&name, bytes, cfg)` | Fan one payload to all subscribers |
| `mesh.publish_many(&name, &[bytes], cfg)` | Fan a batch to all subscribers |
| `SdkError::ChannelRejected(reason)` | Typed subscribe/unsubscribe rejection |

### CortEX surface (feature `cortex`)

| Entry point | Description |
|---|---|
| `cortex::Redex::new()` | In-memory event-log manager |
| `cortex::Redex::with_persistent_dir(path)` | Disk-backed manager |
| `cortex::NetDb::builder(redex)` | Fluent `NetDb` construction |
| `cortex::TasksAdapter::open(redex, origin)` | Open tasks model standalone |
| `cortex::MemoriesAdapter::open(redex, origin)` | Open memories model standalone |
| `db.tasks() / db.memories()` | Typed adapter handles on `NetDb` |
| `adapter.snapshot_and_watch(watcher)` | Atomic initial-result + delta stream |
| `db.snapshot()` | `NetDbSnapshot` bundle for persistence |
| `NetDb::builder(...).build_from_snapshot(&bundle)` | Restore from bundle |

## License

Apache-2.0
