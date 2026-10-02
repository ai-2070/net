---
title: "v0.39.0 — Kickstart My Heart"
description: "Release notes for Net v0.39.0 — Kickstart My Heart — what shipped, what changed, and what it means for compatibility."
---
# Net v0.39 — "Kickstart My Heart"

*Mötley Crüe, 1989: from a dead stop to full speed. v0.39 measures how long a node takes to come alive, makes the first call to a missing service fail at once instead of hanging, and puts the whole native surface behind the Node and Python SDKs.*

## What's in it

- **The SDKs reach everything.** `@net-mesh/sdk` and `net_sdk` (Python) now expose the full native surface, so an application no longer imports `@net-mesh/core` or the `net` wheel to get at it. Fixing the gaps turned up replication, shutdown and crash bugs, which are fixed here too.
- **The Rust SDK gets per-stream receive with the sender** (`Mesh::open_stream_inbox`, `Mesh::on_stream_data`), which both bindings already had.
- **A call to a service nobody serves fails at once.** The target answers `NotFound` instead of saying nothing, so the caller no longer waits out its whole deadline.
- **`connectPeer` no longer cancels an attempt still under way** (`@net-mesh/browser`).
- **Cold start is measured.** A new benchmark times a node from nothing to its first peer and its first nRPC reply, and a smoke test in CI holds the result to a ceiling.
- **Rust 1.99.**

---

## The SDKs reach the whole native surface

### Node: `@net-mesh/sdk`

- **New exports from the root:** consent, delegation, enrollment, the blob types, the aggregator clients, `WriteToken`, and the RedEX replication and greedy methods. `@net-mesh/sdk/deck` and `@net-mesh/sdk/tool` are now published entry points.
- **`MeshNode` forwards 25 more methods** (NAT, A2A, enrollment, tool publishing) and every native option, including `reflexOverride`, `tryPortMapping`, `autoDirectUpgrade` and `permissiveChannels`.
- **Fixes:**
  - A replicated or interval-fsync `openFile` no longer crashes the process. The spawn-capable `Redex` calls now run on a tokio runtime with a reactor, not on the caller's thread.
  - A served blob transfer no longer pins the holder node across the stream's graceful close, so `shutdown()` succeeds on the serving side.
  - Native aggregator clients gain `close()`, so they no longer hold up `shutdown()` until garbage collection. A closed client rejects further calls.
  - The registry classifier returns typed errors for `unknown-group`, `scale-rejected`, `scale-not-supported` and `unauthorized` instead of raw `Error`s. Enrollment argument failures carry `enrollment:`.
- Tests that import only from the SDK root, a package-boundary smoke test, and a drift check that fails when Rust emits a new migration error kind.

### Replication (every binding)

- **A replicated channel leaves Idle.** Pinned members bootstrap to Replica, an armed leader wait elects a leader (`leader_pinned` decides it), and a failed chain announce is retried every tick.
- **`Standard` and `ColocationStrict` placement now replicate.** Every node advertises a replica-candidate tag and computes the same set from viewer-independent inputs, then joins or leaves through `PlacementDeselected`. A colocation hint must be a chain's canonical 16-hex hash.
- **Tags are counted per holder on the mesh**, not per RedEX, so two managers, or a quick disable and re-enable, can't withdraw each other's live tags.
- **`ReplicationConfig` carries `placementMetadata`**, across Node, Python and the C ABI / Go.
- **`disableReplication` undoes `enableReplication`**, and now resolves only once every runtime has withdrawn and released the mesh.

### Python: `net_sdk`

- **`net_sdk.MeshNode` gains the rest of the SDK surface:** mesh channels, `rpc()`, capability aggregation, tools, blob and directory transfer, `discovered_nodes`, connectivity, A2A and enrollment. The six dropped native constructor options are back, `require_signed_capabilities` among them.
- **`net_sdk.compute` and `net_sdk.groups`** build straight from a `net_sdk.MeshNode`, with the typed vocabulary the TypeScript SDK ships. `net_sdk.identity` and `net_sdk.subnets` re-export the wheel's identity surface and build the native subnet shapes.
- **`net_sdk.AsyncMeshNode`**, an async twin over the same node, with an `async for` event loop.
- **`_net.pyi`** gains method-level stubs, and `test_stub_drift.py` checks them against the binding in both directions.
- **Fixes:**
  - `AsyncNetMesh.poll` read shard 0 only, so the async node missed most streams. Both polls now sweep every shard from a shared rotating start.
  - A mistyped group strategy or seed raises `GroupError`.
  - `net_sdk.blob` and `net_sdk.transport` surface a real load failure as itself, not as "missing feature".
- The README and web docs drop the `_native` workarounds, and six Python capability cells move from `core-only` to `supported`.

### Rust: `net_sdk::Mesh`

`Mesh::open_stream_inbox` and `Mesh::on_stream_data` receive one stream's events with the authenticated sender, the shape Node (`onStreamData`) and Python (`open_stream_inbox`) already had. `StreamDataSubscription` keeps core's registration ownership and returns the stream's events to the shard queue when dropped.

---

## A missing service says so

A call naming a service the target does not serve used to get no answer. The REQUEST fell into the target's application queue as an ordinary event, and the caller waited out its whole deadline. A node with a channel registry already failed fast, because the reply-channel subscribe was refused. A node without one admitted it: the Python binding with `permissive_channels=True`, and the Rust integration default. That is how `describe_a2a` against a free-path `serve_a2a` node took 30 s to raise.

The target now answers `NotFound`, a status the wire has always defined ("no service registered with the requested name") and nothing sent. It answers:

- only a well-formed REQUEST whose route matches the service it names, and only when no dispatcher is registered for that route. A frame whose route and payload disagree gets no answer;
- only the authenticated session peer, on the reply channel for its pinned origin, by the same path as a capability denial;
- not a provisional peer, and not an auth-throttled one. At most 64 answers are in flight at a time.

A served service is untouched: its dispatcher takes the frame before this point. The subnet floor readback already read `NotFound` as "a verifier that predates readback", and now gets that answer straight away.

---

## `connectPeer` waits for an attempt under way

In `@net-mesh/browser`, every offer replaces the pair's link. So a second `connectPeer` while the first was still connecting cancelled it, and both callers ended on the relay or in `iceTimeout`. No page had to do anything unusual for this to happen: `joinLobby` reaches the host, then the store it joins reaches it again before its first stream.

Now, per node (`BrowserNode` and `MeshSession` alike):

- a call while another `connectPeer` for the same peer is in flight resolves with that attempt's outcome;
- a call while an `acceptPeer` is in flight waits for it, and takes its outcome when that settles the pair (`direct`, `iceTimeout`, `udpBlocked`, or `superseded` by a newer attempt). It offers only after an inconclusive answer;
- concurrent `acceptPeer` calls share one answer;
- the healthy-pair check runs inside that gate, just before any offer.

This completes the 0.37.0 rule that a pair already direct is never offered again: a page may call `connectPeer` "to be sure" at any point. Another tab's node is outside the rule.

---

## Cold start, measured

Every other benchmark measures a node that is already warm. `benches/cold_start.rs` times getting there: keypair, socket bind, start, handshake, the first capability visible to the peer, the first nRPC reply, and a peer restart.

It runs under both the wire-floor policy and the shipped announce policy. On an Apple M1 Max, nothing to first nRPC reply is about 1.0 ms p50. The full rows are in [`BENCHMARKS.md`](https://github.com/ai-2070/net/blob/master/net/crates/net/BENCHMARKS.md), which also gains a provenance table (date, commit, machine, command per section).

`tests/cold_start_smoke.rs` replays the cold-start and restart paths in CI. It holds the fastest of three attempts under 500 ms, which catches an order-of-magnitude regression without flaking on a busy runner.

---

## Smaller changes

- **Rust 1.99.0.** `Atomic*::fetch_update` is renamed `try_update` across the tree, and the infallible sites use the new `update`.
- **The CLI's npm and PyPI READMEs** carry the current subcommand table, and CI now holds every README outside the website to its checks (`check-readmes.py`, with a self-test).
- **The browser demo's** signalling prober runs its attempts back to back, and the check window is 14 s.

---

## Version bump

Everything published moves to **0.39.0**:

- every manifest: crate, wire, leaf, CLI, deck, SDK, payments, and the Go, Node and Python bindings;
- the `@net-mesh/*` pins and the `net-mesh` Python bound (now `>=0.39.0,<0.40.0`);
- the skills' `net-version`;
- the Hermes integration pin;
- the lockfiles.

---

## Breaking changes

None to wire formats. Behavior that code may have relied on:

- **A call to an unserved service** on a 0.39 target returns `RpcError::ServerError` with status `NotFound` at once, where it used to time out. Code that treated the timeout as "not served" should read the status instead.
- **Python:** a mistyped group strategy or seed raises `GroupError`, not `DaemonError`, and `GroupError` does not derive from `DaemonError`.
- **Node:** `disableReplication()` now resolves only once every runtime has withdrawn. A colocation hint must be a chain's canonical 16-hex hash.
- **Building from source** needs Rust 1.99.

---

## How to upgrade

Bump to 0.39.0 and rebuild. Code importing `@net-mesh/core` or the `net` wheel only to reach a method the SDK lacked can now import the SDK alone. Pages and anchors ship together: upgrade `@net-mesh/browser` with the leaf.

---

Released 2026-10-02.

## License

See [LICENSE](https://github.com/ai-2070/net/blob/master/net/crates/net/LICENSE-APACHE).
