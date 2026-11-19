---
title: "v0.46.0 — Here I Go Again"
description: "Release notes for Net v0.46.0 — Here I Go Again — what shipped, what changed, and what it means for compatibility."
---
# Net v0.46 — "Here I Go Again"

*Whitesnake, 1987. Until now two paired machines could not reach each other again after either one restarted. v0.46 brings a restarted machine back on the address its peers already have, and lets an aggregator daemon answer the queries it was built for.*

## What's in it

- **Paired machines reconnect after a restart.** A node built from an identity seed now keeps its Noise key across restarts, and a peer that comes back is re-attached instead of timing out.
- **Large files move several times faster and fail fast.** A file's chunks fetch in parallel; a 190 MiB file went from about 3.9 to 31 MiB/s on loopback. A stalled transfer fails on silence instead of hanging.
- **Streamed calls stop losing chunks.** A streamed nRPC reply is retransmitted like any reliable stream, and a sink can wait for room instead of dropping.
- **Services stay up until you stop them.** In Node and Python, a serve handle the app does not keep referenced no longer stops its service at garbage collection.
- **The aggregator daemon answers queries.** `net-aggregator-daemon` serves `fold.query` and a new group-aware `fold.query.v2`, serves one subnet, and enforces who can see its summaries.
- **Breaking, in a few places:** the daemon's config, serve handles' lifetime, an exclusive mesh port, and how a dropped stream chunk is reported. See "Breaking changes".

---

## Reconnecting after a restart

An app built on the Node SDK paired two machines and stored each one's rendezvous string (`addr|noise_pubkey|node_id`). After either machine restarted, the pair could not talk again. Two bugs, one on each side:

- **The restarted node had a new Noise key.** The Noise static key (`publicKey()`) was generated afresh on every start, so every saved rendezvous named a key nobody held any more, and no binding offered a way to keep one.
- **The survivor kept a stale reply route.** A peer that handshook again before the failure detector fired came back with an empty reply roster, while the survivor still believed it was subscribed. Calls timed out.

Now:

- **A seeded node derives its Noise key from its identity** (domain-separated BLAKE3, context `net-noise-static-key-v1`, pinned by a golden vector), so its rendezvous survives a restart. This happens for every way of building a node. An explicit key still wins:

  | Surface | Option |
  |---|---|
  | Rust SDK | `MeshBuilder::noise_static_key(...)` |
  | Node / TypeScript | `noiseStaticKey` (a `Buffer`) |
  | Python | `noise_static_key` (bytes) |
  | C | `noise_static_key_hex` in the mesh config JSON |
  | Go | `NoiseStaticKeyHex` |

  A node with no seed still gets a fresh key on each start. Rotating a seeded node's Noise key now means rotating its identity, or setting an explicit key.
- **A new session with a peer refreshes its reply routes.** Whenever a session with a peer is replaced (a re-handshake or a routed rotation) or the peer fails, the survivor drops that peer's cached reply subscriptions, so the next call subscribes again.
- **Re-attaching is one call.** Rust `Mesh::ensure_attached_via`, Node and TypeScript `ensureAttached`, and Python `ensure_attached` dial the peer while watching the existing session. A peer that restarted is re-attached at once. A peer still on its old session keeps it, which can take up to one heartbeat interval to tell. The dial rides out a session rotation already in progress, which could previously expire a single `connect_via` just short of acceptance.
- **`renew` re-attaches first,** and names a failed handshake ("unreachable, or its Noise key no longer matches … re-pair") instead of hiding it.
- **A rotation in the middle of a call's first reply-subscribe** no longer fails the call with `NoRoute`. The subscribe retries while the peer still has a session.

`sdk/tests/restart_reconnect.rs` restarts each side in turn under the default 5 s heartbeat and under a 1.5 s session timeout, and also covers an explicit key and a node with no seed.

---

## Large files

A single large file used to move at about 4 MB/s, and a 190 MB file could fail outright: a holder that gave up mid-chunk stopped without a word, and the requester waited out a fixed 30 seconds.

- **A file's chunks fetch in parallel,** up to 8 at a time (`MAX_CHUNKS_IN_FLIGHT_PER_FILE`), and are written in manifest order. The shared in-flight budget rises from 8 MiB to 32 MiB (`DEFAULT_INFLIGHT_BUDGET_BYTES`). Each chunk stream still keeps its ~256 KB window, so about 2 MB is on the wire at once. The storing side keeps its own 8 MiB budget, so it cannot buffer four times as many whole files.
- **A chunk fails on silence, not on a clock.** A fetch fails after 15 seconds with no progress (`TRANSFER_CHUNK_IDLE_TIMEOUT`), however long the whole chunk takes.
- **A holder that gives up says so.** It sends `StreamReset`, and the requester's fetch fails at once instead of waiting out the idle limit. Only then is the chunk retried, up to 3 attempts with a short backoff; a missing chunk or a silent holder fails at once.
- **Tuning:** `FetchDirOptions` and `fetch_dir_with` set the budget and the limits.

Measured on Windows 11 loopback with default socket buffers, one 190 MiB file took 48.9 s (3.9 MiB/s) before and 6.1 s (31.0 MiB/s) after; a 1 GiB file took 32.8 s (31.2 MiB/s). Linux numbers are not measured yet.

---

## Streamed calls

A server-streaming nRPC call could lose chunks while `send()` returned `true`.

- **Replies are retransmitted.** Streamed reply packets are registered for retransmission with their routing header, so a rebuilt packet still reaches the caller. A reply sent through the roster fallback is now `Reliable`, like the direct route, and a reply's first copy takes the same route as its retransmits.
- **A dropped chunk is reported.** Node and Python `sink.send` return `false` when the chunk was not queued, and the Rust SDK's `ResponseSinkTyped` gains `try_send`.
- **A sink can wait for room:** Node `sendWait`, Python `send_wait` and `asend_wait`, Rust `ResponseSinkTyped::send_wait`. Awaiting it in a loop paces the handler to the caller.
- **No silent gaps.** A chunk that cannot be sent within 30 seconds ends the call with an `Internal` "response pump failed" terminal (`PumpFailed`) instead of leaving a hole in the stream. A caller that stalls only on full windows gets retries rather than an immediate `PumpFailed`.

---

## Services stay up

In Node and Python, a serve handle (nRPC, enrollment, org, A2A) that the app did not keep referenced stopped serving at the next garbage collection: promptly in Python, at an arbitrary later moment under V8.

The mesh now owns each registration until `close()` / `stop()`, or until the mesh's `shutdown()`, which releases them first. Rust keeps drop-to-stop, and its `ServeHandle` and `ToolServeHandle` are now `#[must_use]`.

---

## The mesh port is exclusive

A second process could bind the same mesh UDP port and split the node's traffic. The mesh socket no longer sets `SO_REUSEADDR`, and on Windows it sets `SO_EXCLUSIVEADDRUSE`. Binding a port that a mesh dropped without `shutdown()` still holds fails with `AddrInUse`, and the error names `shutdown()`.

---

## The aggregator daemon

`net-aggregator-daemon` never answered `fold.query`, so the CLI's `query --remote` could not work against the real binary. It could not tell which subnet a peer belonged to, and nothing enforced who could see its summaries.

- **It serves queries.** The daemon answers `fold.query` (v1) and the new `fold.query.v2` from its whole registry.
- **`fold.query.v2` names a group.** It adds a group selector, returns one record per summary, rejects trailing bytes, and admits a caller by the same visibility rule as channel fan-out. Operators are always admitted. v1 is frozen.
  - Clients: Rust SDK `query_group_latest` / `query_group_summarize_now`, Node `queryGroupLatest` / `queryGroupSummarizeNow`, Python `query_group_latest`, Go `QueryGroupLatest` / `QueryGroupSummarizeNow`, and C `net_fold_query_client_query_group_latest` / `net_fold_query_client_query_group_summarize_now`.
  - A target that does not serve v2 is reported as `NET_REGISTRY_ERR_SERVER_TOO_OLD` (12) in C and `server-too-old` in Go.
- **One subnet per daemon.** A daemon-level `subnet` (default `global`) and `summary_visibility` (`subnet-local`, `parent-visible` or `global`) set its publication boundary, and the daemon installs a channel registry so that boundary is enforced. `[[subnet_rule]]` blocks classify peers into subnets from their signed tags; rule order is part of the policy's fingerprint.
- **`SummarizeNow` publishes** the summary it produces (it used to only buffer it), and on-demand ticks are coalesced to one per 100 ms.
- **Errors:** a v1 authorization refusal now reads `unauthorized` instead of `transport`, and Go maps error kinds 8–11, which used to read "unknown-11".

The daemon guide shows the full config. A new test relays capability announcements through a real daemon hub (replay, live flood, supersede).

---

## Smaller changes

- `new Redex(dir)` throws at once and names the `{ persistentDir }` form, instead of quietly building an in-memory store.
- The website has a new homepage, and its share images load on Twitter and Discord. The README has a hero image and key links.
- A WebRTC test (`a_busy_peer_cannot_starve_a_sibling_or_the_socket`) stays saturated under `sctp-proto` 0.10.7, which drains its queue about three times faster.
- `wasm-bindgen` is 0.2.130 in both leaf crates and every CI and release install. CI runs on Node 26.
- Dependency updates, including `roaring` 0.12, `async-nats` 0.51, `lru` 0.19, `redis` 1.8, `napi` 3.16.1 and the web site's packages.

---

## Version bump

Everything published moves to **0.46.0**: every manifest and pin, the `net-mesh` Python bound (now `>=0.46.0,<0.47.0`), the skills' `net-version`, the Hermes integration pin and the lockfiles.

---

## Breaking changes

Nothing was removed from the mesh wire.

**The aggregator daemon's config**

- `source_subnet` on a group or template no longer sets the subnet the daemon serves; the daemon-level `subnet` does. A config that labels a group with a non-global subnet and sets no daemon `subnet` is refused at boot, and the error names the fix.
- A non-global `subnet` requires `[[subnet_rule]]` blocks that can classify peers into it, and at most one group runs per fold kind. A config that aggregated several subnets in one daemon must be split, one daemon per subnet.
- A global daemon with subnet rules must set `summary_visibility = "global"`, or it is refused at boot, because the rules' peers would silently lose its summaries.

**Serve handles (Node and Python)**

- Dropping a serve handle no longer stops its service. Code that relied on that must call `close()` / `stop()`; serving the same name again without it fails. A mesh dropped without `shutdown()` keeps its services, its node and its UDP port until the process exits.

**The mesh port**

- In one process, binding a port that a mesh dropped without `shutdown()` still holds now fails with `AddrInUse`. A second process can no longer share a mesh port.

**Streamed replies**

- Node and Python `sink.send` return `false` on a real drop, where they used to return `true`.
- A chunk that cannot be sent within 30 seconds ends the call with `Internal` "response pump failed".
- Core `RpcAsyncResponseEmitter` futures resolve to `bool` (whether the frame was sent). This breaks only code that builds that type directly.

**Smaller**

- A seeded node's `publicKey()` is now stable across restarts instead of random. This only removes breakage, but tooling that expected a new key per start will see the same one.
- `new Redex(dir)` throws where it used to return an in-memory store.
- A v1 `fold.query` authorization refusal reads `unauthorized`, not `transport`.

---

## How to upgrade

Bump to 0.46.0 and rebuild.

- **To reconnect after a restart,** upgrade both machines: the restarted one keeps its key, and the survivor refreshes its reply routes. Pairings made before 0.46 named a random key, so re-pair once after upgrading; pairings made on 0.46 survive restarts. To keep the Noise key separate from the identity, pass an explicit key.
- **Node and Python services:** call `close()` / `stop()` on any handle you used to let go out of scope, or `shutdown()` the mesh.
- **Tests that rebind a port** in one process: `shutdown()` the first mesh before building the next on the same port.
- **Streaming handlers:** use `sendWait` / `send_wait` / `asend_wait` to pace to the caller, or check `send()`'s result.
- **Aggregator daemons:** add `subnet` and `summary_visibility` to each config and move any `source_subnet` there; run one daemon per subnet. The boot error names what to change.
- **To use `fold.query.v2`,** upgrade the daemon first. A 0.45 daemon answers v2 with `server-too-old`.
- **Redex in Node:** pass `new Redex({ persistentDir })`.

---

Released 2026-10-31.

## License

See [LICENSE](https://github.com/ai-2070/net/blob/master/net/crates/net/LICENSE-APACHE).
