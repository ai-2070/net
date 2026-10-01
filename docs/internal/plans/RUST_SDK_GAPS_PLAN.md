# Rust SDK gaps — per-stream inbound receive, and what isn't a gap

## Status

Done, 2026-10-01. Targets the release after 0.38. Branch `LZL0/python-sdk`.
S1 and S2 landed 2026-10-01 (see each slice). Nothing else is planned here; R2's
migration and R3 are deferred decisions, not open slices.

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
| `poll_shard` / `num_shards` | `Mesh::recv_shard` / `Mesh::num_shards` (`sdk/src/mesh.rs:980`, `:986` as of this PR's head) |
| `list_tools` / `watch_tools`, blob transfer, capability aggregation, A2A, enrollment, NAT traversal | All present (`tool.rs`, `transport.rs`, `sdk/src/mesh.rs:1572` / `:1636`, `mesh_a2a.rs`, `mesh_enroll.rs`, `sdk/src/mesh.rs:1806–1985`) |

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
(`sdk/src/mesh.rs:774`) to learn who sent a stream event. The Rust SDK sits
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
  - **No node keep-alive.** On a never-started node, opening an inbox and a
    subscription must leave `Arc::strong_count(mesh.node())` unchanged. That
    is the discriminator; it fails if either handle holds an
    `Arc<MeshNode>`. Shutting down with both handles held, then dropping
    them, is kept as a teardown safety check only. (The first draft used
    shutdown itself as the discriminator; see the note below for why it
    can't be.)

    *Changed during implementation (2026-10-01):* this witness, as written,
    can't tell `Weak` from `Arc`. Core `MeshNode::shutdown(&self)` succeeds
    with outstanding `Arc`s; it's the **Node binding's** `shutdown` that fails
    on them (`Arc::try_unwrap`, `bindings/node/src/lib.rs` ~line 2631). That's
    where the "a strong reference makes shutdown fail" comment comes from. A
    background task may also keep the node alive after shutdown, so "close()
    returns without effect" isn't deterministic either. The witness now
    reads `Arc::strong_count(mesh.node())` on a never-started node before and
    after opening each handle; the count must not change. Shutdown with both
    handles still held, then dropping them, stays in the test as a safety
    check (no panic), not as the discriminator.
- Run it with `cargo nextest run -p net-mesh-sdk --features net --test
  stream_inbound --no-tests=fail --retries 0`. (The first draft said
  `cargo tf`, which wasn't used: `tf`'s feature list belongs to the root
  crate, and this binary only needs the SDK's `net` feature.)
- No `ci.yml` pin is needed: CI's nextest step auto-discovers every
  `sdk/tests/*.rs` (`ci.yml` ~line 2448), and the pin-guard treats `sdk` as
  auto-discovered (`ci.yml` ~line 1212). Gate the test on the `net` feature so
  it doesn't compile to an empty binary under a narrower feature set.
- Pre-push: `cargo doc -p net-mesh-sdk --no-deps --features "full rtc-bootstrap"`
  with `-D warnings`, because the new doc comments link core types.

- **Done 2026-10-01.**
  - `Mesh::open_stream_inbox` and `Mesh::on_stream_data` are in
    `sdk/src/mesh.rs`. `StreamDataSubscription` is defined there too: `Weak`
    node, stream id + registration id, idempotent `close()`, `Drop`, not
    `Clone`.
  - `StreamInbox` / `StreamInboundEvent` are re-exported from
    `net_sdk::mesh`, and all three from the crate root (`sdk/src/lib.rs`).
  - **GREEN:** `sdk/tests/stream_inbound.rs` passes, 6/6, with the nextest
    command above. The two ownership tests are
    `a_stale_handle_cannot_evict_its_successor` and
    `handles_hold_the_node_weakly`.
  - **RED, by mutation** (each applied to `sdk/src/mesh.rs`, run, then
    reverted):
    1. The subscription also holds an `Arc<MeshNode>`.
       `handles_hold_the_node_weakly` fails with `subscription is weak`,
       `left: 2, right: 1`.
    2. `close()` drops the closed guard and removes **any** registration on
       the stream (it tries `registration_id..registration_id + 8`), i.e.
       teardown that isn't owned. `a_stale_handle_cannot_evict_its_successor`
       fails at its repeat-close assertion (`stream_inbound.rs:224`), where the
       stale handle's second `close()` evicted B and returned `true`.
  - **Gates:** `fmt.py --check` clean after formatting. `cargo clippy -p
    net-mesh-sdk --features net --lib -- -D warnings` and the `--tests`
    variant are clean. `RUSTDOCFLAGS="-D warnings" cargo doc -p net-mesh-sdk
    --no-deps --features "full rtc-bootstrap"` is clean; the only output is the
    existing `panic`-in-bench-profile manifest warning.
  - **Review follow-up (2026-10-01), three findings, all fixed:**
    - **A panicking `on_stream_data` handler killed the node's receive
      path.** The core calls the sink inline in `dispatch_local_packet`
      with no unwind guard. The SDK now wraps the handler in
      `catch_unwind`: the one event is dropped, the panic is counted
      (`StreamDataSubscription::panics()`), and receiving continues. New
      test `a_panicking_handler_is_contained_and_the_node_keeps_receiving`.
      **RED** with the guard removed: `timed out waiting for: the event
      after the panic`. The docs state the limit: under `panic = "abort"`
      (the workspace `release` profile) a panic still aborts the process.
    - **`close()` doesn't wait for a callback in flight:** the receive path
      clones the sink before calling it, so a concurrent close can return
      first. Documented on the type and on `close()`.
    - **`open_stream_inbox(capacity = 0)` holds one event,** since the core
      clamps it to one. Documented.
    - With the new test, `stream_inbound.rs` passes 7/7.
  - **Not run:** the full `net-mesh-sdk` suite and the workspace-wide pre-push
    checklist. The change is additive (two methods, one type, re-exports), so
    those are left to CI.

### S2 — record the R2 rule

- Add the "new binding surface lands in `net_sdk` first" rule to `AGENTS.md`
  (Architecture notes) and to `CONTRIBUTING.md`, citing R1 as the incident.
- **Proves it:** nothing mechanical. It's a reviewing rule; the next binding
  PR either follows it or says why not.
- **Done 2026-10-01.** A new `AGENTS.md` subsection under Architecture
  notes ("New binding surface lands in `net-mesh-sdk` first"), and a new
  `CONTRIBUTING.md` subsection under Pull requests ("New binding surface goes
  into the Rust SDK first"). Both cite R1 as the incident and point back at
  this plan.

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
