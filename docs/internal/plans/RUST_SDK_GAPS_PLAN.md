# Rust SDK gaps — per-stream inbound receive, and what isn't a gap

## Status

Planned, 2026-10-01. Targets the release after 0.38. Branch `LZL0/python-sdk`.
No slice has landed yet.

Amended 2026-10-01 after review: the R1 design now requires `StreamDataSubscription` to keep core's registration ownership (`Weak<MeshNode>`, unregister by stream id + registration id). S1 gained a stale-handle teardown witness and a no-keep-alive-past-shutdown witness.

This plan was split out of
[`PYTHON_SDK_WRAPPER_PARITY_PLAN.md`](PYTHON_SDK_WRAPPER_PARITY_PLAN.md), where
the survey started as its Part B. The two plans touch at one point: R1 here
and the Python receive design (Decision 3, slice S1) there add the same
per-stream inbox, so all three SDKs line up.

## The gap

The Rust SDK (`net-mesh-sdk`, `sdk/src/`) is far more complete than the Python
SDK. `net_sdk::Mesh` has 90+ methods (`sdk/src/mesh.rs`, plus `impl Mesh`
blocks in `mesh_rpc.rs`, `mesh_rpc_resilience.rs`, `mesh_a2a.rs`,
`mesh_enroll.rs`, `sensing.rs`, `org/`). Most "gaps" a first pass turns up are
not real, so this plan records what was ruled out as carefully as what was
found.

### How this was checked

1. Every function the Python and Node bindings expose was grepped for a
   `pub fn` in `sdk/src`.
2. Each miss was traced to where the binding actually gets it.
3. Every public core module (`src/adapter/net/mod.rs`,
   `src/adapter/net/behavior/mod.rs`) was grepped for references from
   `sdk/src` and from both bindings, and its main types were grepped for in
   `src/adapter/net/mesh.rs` to see whether the running node uses them.

One structural fact explains most of the noise. **Neither binding is built on
`net_sdk::Mesh`.** Python's `NetMesh` holds `Option<Arc<MeshNode>>`
(`bindings/python/src/lib.rs:1351`) and Node's holds
`Arc<ArcSwapOption<MeshNode>>` (`bindings/node/src/lib.rs:1655`). Both import
core paths directly, so "the binding imports core for X" does not mean "the SDK
lacks X".

### Ruled out (the SDK has it)

| Looked like a gap | Where it actually is |
|---|---|
| `store_dir` (no `pub fn` in `sdk/src`) | Re-exported from core in `sdk/src/transport.rs:104`, beside `fetch_dir` |
| Placement-filter registry | Re-exported in `sdk/src/capabilities.rs:249` |
| Load-balancer `Strategy` / `RequestContext` | Re-exported in `sdk/src/groups/common.rs:8` |
| `verify_signature`, `delegate_token` free functions | Methods on re-exported types: `EntityId::verify` (`identity/entity.rs:79`), `PermissionToken::delegate` (`identity/token.rs:519`); re-exported by `sdk/src/identity.rs:56` |
| `stream_id_from_label` | Core re-exports it at `net::adapter::net` (`mod.rs:250`). The SDK doesn't re-export it, but Rust users reach it through the `net` dependency they already have. Cosmetic. |
| `publish_tools` | Lives in the MCP adapter (`net_mcp::wrap`), not `net_sdk`. Deliberate per the MCP adapter-boundary rule. |
| `poll_shard` / `num_shards` | `Mesh::recv_shard` / `Mesh::num_shards` (`sdk/src/mesh.rs:909`, `:915`) |
| `list_tools` / `watch_tools`, blob transfer, capability aggregation, A2A, enrollment, NAT traversal | All present (`tool.rs`, `transport.rs`, `mesh.rs:1433` / `:1497`, `mesh_a2a.rs`, `mesh_enroll.rs`, `mesh.rs:1667–1846`) |

### R1 — per-stream receive with the authenticated sender (real gap)

Core has two ways to receive one stream's events tagged with the
**authenticated** sender (`StreamInboundEvent.from_node`, `mesh.rs:2064`):

- `MeshNode::register_stream_inbound(stream_id, sink)` (`mesh.rs:34173`), a
  callback.
- `MeshNode::open_stream_inbox(stream_id, capacity)` (`mesh.rs:34198`), a
  bounded pull queue that counts drops.

Both bindings expose these: Node as `onStreamData`
(`bindings/node/src/lib.rs:2202`), Python as `NetMesh.open_stream_inbox`
(`bindings/python/src/lib.rs:2328`). **`net_sdk::Mesh` exposes neither.**
`Mesh::recv` / `recv_shard` return `StoredEvent`s, which don't carry the sender
(TS documents exactly this: "What `MeshNode.recv` cannot tell you"). So a Rust
SDK user is the only one who has to drop to `mesh.node()`
(`sdk/src/mesh.rs:703`) to learn who sent a stream event. The Rust SDK sits
*behind* its own bindings here.

### R2 — the bindings don't sit on the Rust SDK (structural)

Because both bindings wrap core `MeshNode` directly, every SDK-level behaviour
is implemented two or three times, and parity drifts. R1 is the proof: the
bindings got the inbox, and the SDK didn't.

### R3 — behaviour-plane libraries with no SDK surface

These public core modules are referenced by **neither** `sdk/src` nor either
binding, and the running `MeshNode` doesn't use their main types (0 hits in
`mesh.rs`):

- `behavior::rules` (`RuleEngine`)
- `behavior::safety` (`SafetyEnforcer`)
- `behavior::api` (`ApiRegistry`)
- `behavior::metadata` (`MetadataStore`)
- `behavior::loadbalance` (`LoadBalancer`; only its `Strategy` /
  `RequestContext` types are used, by groups)
- `contested` (`CorrelatedFailureDetector`, `PartitionDetector`)
- `continuity` (`assess_continuity`)

They are standalone libraries ("Phase 4C–4I" in their module docs), not mesh
features.

Related, and minor: core exposes `MeshNode::proximity_graph()`
(`mesh.rs:24157`), which the SDK only reaches through `mesh.node()`. The
bindings use it only for a node count (`bindings/python/src/lib.rs:2244`),
which the SDK already covers with `discovered_nodes`. No slice.

## The design

### R1 — `Mesh::open_stream_inbox` and `Mesh::on_stream_data`

- Add `Mesh::open_stream_inbox(stream_id, capacity) -> Option<StreamInbox>` and
  re-export `StreamInbox` / `StreamInboundEvent` from `net_sdk`.
- Add `Mesh::on_stream_data(stream_id, f) -> Option<StreamDataSubscription>`
  over `register_stream_inbound`. The subscription unregisters on drop,
  mirroring TS `StreamDataSubscription`.
- **Registration ownership is preserved, not re-invented** (added 2026-10-01
  after review). Core's ownership model is already right, and the SDK handle
  must keep it exactly:
  - `register_stream_inbound` returns a registration id, and
    `unregister_stream_inbound(stream_id, registration_id)` removes the sink
    only if that id still owns the slot. Its doc comment states the purpose:
    "a stale teardown cannot evict a newer registration" (`mesh.rs` ~line
    34170).
  - `StreamInbox` holds `Weak<MeshNode>` plus its registration id
    (`mesh.rs:2085`), and closes on drop.
  - The Node binding's `StreamDataSubscription` (`bindings/node/src/lib.rs:1133`)
    is the reference implementation. It holds `Weak<MeshNode>`, `stream_id`,
    `registration_id` and an idempotent `closed` flag, and unregisters by
    **both** ids. Its comment gives the reason for `Weak`: a live subscription
    must not keep the node alive past `shutdown()`, because an outstanding
    strong reference makes shutdown fail.

  So the SDK's `StreamDataSubscription` stores `Weak<MeshNode>` (never an
  `Arc`), `stream_id`, `registration_id` and a `closed` flag. Its
  `close()` / `Drop` call `unregister_stream_inbound(stream_id,
  registration_id)` once, and are a no-op when the node is gone. It never
  calls a stream-id-only removal. `Mesh::open_stream_inbox` returns the core
  `StreamInbox` unwrapped, so its `Weak` ownership carries through unchanged.
- The doc comment carries core's contract: one sink per stream (`None` if one
  is already registered); events past `capacity` are dropped and counted, not
  queued.
- Rejected: an async `Stream` adapter as the only API. The core inbox is a sync
  `mpsc` built for bindings that must not run on the receive path. An async
  adapter can come later, layered on the callback, once a caller needs one.

### R2 — decision: defer the binding migration

Moving the bindings onto `net_sdk::Mesh` would make the Rust SDK the single
source of the surface. But it touches every `NetMesh` method in two bindings,
and the `Arc<MeshNode>` sharing that `DaemonRuntime` relies on
(`bindings/python/src/lib.rs:1345`).

**Recommendation: defer.** Adopt a rule instead: new binding surface lands in
`net_sdk` first, and the binding forwards to it. Re-plan the migration on its
own if drift keeps showing up.

### R3 — decision: no SDK surface for unwired libraries

**Recommendation: no SDK surface.** An SDK wrapper would promise a mesh
integration that doesn't exist, and Rust users who want these libraries
already reach them through the `net` crate. Revisit if one gets wired into
`MeshNode`; at that point it gets an SDK surface in the same change.

## The slices

### S1 — R1, per-stream inbound receive on `net_sdk::Mesh`

- `Mesh::open_stream_inbox`, `Mesh::on_stream_data`,
  `StreamDataSubscription`, and the re-exports.
- **Proves it:** a new integration test, `sdk/tests/stream_inbound.rs`, with
  two `Mesh` nodes:
  - Events sent on a stream arrive in the inbox with `from_node` equal to the
    sender's `node_id()`.
  - A second `open_stream_inbox` on the same stream returns `None`.
  - Dropping the subscription returns events to `recv`.
  - Overflowing `capacity` increments `dropped()`.
  - **Stale-handle teardown (the ownership witness).**
    1. Subscribe A on stream S, then `close()` A.
    2. Subscribe B on S. It succeeds, and the core issues a new registration id.
    3. Drop A's (already closed) handle, and call `close()` on it again first,
       to check repeat closes too. The handle is deliberately not `Clone`, so
       there's no second copy to test.
    4. B must still receive events on S.

    Repeat with A as an inbox and B as a callback. This fails if the handle
    ever unregisters by stream id alone, or by a registration id it doesn't
    own.
  - **No node keep-alive.** With a live subscription and a live inbox still
    held, `mesh.shutdown()` succeeds. Afterwards, dropping both handles doesn't
    panic, and `close()` returns without effect. This fails if either handle
    holds an `Arc<MeshNode>`.
- Run it with `cargo tf -p net-mesh-sdk --test stream_inbound` (TESTS.md
  idiom).
- No `ci.yml` pin is needed: CI's nextest step auto-discovers every
  `sdk/tests/*.rs` (`ci.yml` ~line 2448), and the pin-guard treats `sdk` as
  auto-discovered (`ci.yml` ~line 1212). Gate the test on the `net` feature so
  it doesn't compile to an empty binary under a narrower feature set.
- Pre-push: `cargo doc -p net-mesh-sdk --no-deps --features "full rtc-bootstrap"`
  with `-D warnings`, because the new doc comments link core types.

### S2 — record the R2 rule

- Add the "new binding surface lands in `net_sdk` first" rule to `AGENTS.md`
  (Architecture notes) and to `CONTRIBUTING.md`, citing R1 as the incident.
- **Proves it:** nothing mechanical. It's a reviewing rule; the next binding
  PR either follows it or says why not.

## Risks

- **The receive loop is hot.** A callback registered through `on_stream_data`
  runs on the mesh's receive path, so a slow Rust callback stalls it.
  *Mitigation:* the doc comment says so and points at `open_stream_inbox` for
  any work that can block. This is the same split core already documents for
  the bindings.
- **A handle that "simplifies" ownership.** Holding an `Arc<MeshNode>`, or
  unregistering by stream id alone, both look harmless and both are wrong:
  the first blocks `shutdown()`, the second lets a stale handle evict a
  successor. *Mitigation:* the two S1 ownership witnesses fail on either
  change.
- **Sink conflicts with the bindings.** Core allows one sink per stream. A Rust
  SDK user who also hands the node to a binding could collide. *Mitigation:*
  `None` on conflict is already core's behaviour; the SDK passes it through and
  doesn't paper over it.
- **The R2 rule gets ignored.** It's convention, not a check. *Fallback:* if
  drift recurs, re-plan the migration (see Not in scope).

## Not in scope

- Moving the Python / Node bindings onto `net_sdk::Mesh` (R2 migration).
- SDK surfaces for the unwired behaviour-plane libraries (R3).
- An async `Stream` adapter over the stream inbox.
- A `proximity_graph()` accessor on `net_sdk::Mesh`.
- Re-exporting `stream_id_from_label` from `net_sdk` (cosmetic).
