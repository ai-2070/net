---
title: "v0.41.0 — Thunderstruck"
description: "Release notes for Net v0.41.0 — Thunderstruck — what shipped, what changed, and what it means for compatibility."
---
# Net v0.41 — "Thunderstruck"

*AC/DC, 1990: a whole stadium at once. v0.41 makes the capability fold hold a million-node fleet without stalling, and makes a public anchor say why it turned a player away.*

## What's in it

- **The capability fold at fleet scale.** With a million nodes resident and the fleet announcing, the capability cache now hits 99.8% of the time (it was 33.8%). An expiry sweep walks the fold once, and broad queries are about three times faster.
- **An anchor names why it refused a page.** A full anchor answers `at_capacity` with its session count, every other refusal carries the real reason, and the anchor's stats line shows how full it is.
- **`@net-mesh/browser` exports its whole module surface**, and a lobby or region host whose node id has no hex letters can be joined.
- **Breaking, Rust only:** the fold's public types changed shape. See "Breaking changes".

---

## The capability fold at fleet scale

Every node keeps a fold of what its peers announce, and every capability query, route choice and placement filter reads it. At a million resident nodes, with the fleet announcing, the fold spent most of its time invalidating its own cache and re-walking itself. Measured on an i9-14900K at 1M resident:

| measure | v0.40 | v0.41 |
|---|---|---|
| cache hit rate, 200-node hot set, fleet announcing | 33.8% | 99.8% |
| expiry sweep, 10% expired | 1.43 s, 99 walks | 0.56 s, 1 walk |
| expiry sweep, whole fleet | 14.1 s | 7.0 s |
| insert / refresh / changed replace | 5.99 / 1.40 / 16.4 µs | 2.86 / 1.21 / 10.4 µs |
| translate an announcement | 9.79 µs | 3.7 µs |
| `query_complex` / `query_tag`, 50k | 1.52 ms / 737 µs | 613 / 353 µs |
| mixed workload: refresh p50 / p99 / max | 21.7 / 52.6 µs / 37.1 ms | 15.8 / 34.5 µs / 8.5 ms |
| mixed workload: broad query p50 | 40.5 ms | 13.5 ms |

- **The cache is invalidated per publisher.** One node's announcement used to bump a fold-wide generation and stale every cached entry. Each publisher now has its own revision, so an announcement invalidates only that publisher's entries.
- **The expiry sweep walks once**, then evicts in chunks.
- **A warm refresh allocates nothing.** Tags are read by borrowing, index keys are built in a reused buffer, and a same-owner replace keeps its reverse-index place.
- **Queries stream their candidates.** Constraints resolve to borrowed index buckets, smallest first, and the result is collected once.
- **The fold's maps use a keyed hasher.** A class hash is declared by the publisher, so the fold now uses `foldhash`'s randomly seeded `RandomState`, not an unkeyed mixer a peer could aim collisions at.
- A cache hit now takes the fold's read lock, so it can wait behind a writer. Under announcement load, p50 and p99 still improved by orders of magnitude.

Still open: memory at 1M is 4.2 GB, and the interning, bitmap and time-indexed expiry work that would cut it is on hold. The full record, with every witness and review finding, is [`CAPABILITY_FOLD_SCALE_PLAN.md`](https://github.com/ai-2070/net/blob/master/docs/internal/plans/CAPABILITY_FOLD_SCALE_PLAN.md).

---

## An anchor says why it refused a page

A page could get `offer_refused` with the message `the offer was refused: Busy`, and nothing on the anchor recorded why. That `Busy` stood for four different failures: a full session table, an offer SDP that did not parse, a session that could not be created, and an offer the WebRTC stack rejected.

- **A full anchor answers `at_capacity` (503)**, with its count: "this anchor holds 256 of its 256 RTC sessions". The listener checks before handing the offer to the driver, as it already did at the provisional-session bound, and an offer that races past the check gets the same answer.
- **Every other refusal names its reason** in the message, for example `Busy (rtc: bad offer: …)`, and the anchor logs it at warn. A full anchor is a normal state under load, so that one logs at debug.
- **The `--game-stats-secs` line shows how full the anchor is:** `rtc: {sessions, max_sessions, provisional, max_provisional}`. It now prints on an anchor without `--game` or `--open-games` too.
- New: `MeshNode::rtc_session_load()`, the live session count beside `max_peers`.

---

## `@net-mesh/browser`

- **The whole module surface is exported.** The netcode wire helpers (`eventPeer`, `peerHex`, `encodeFrame`, `decodeFrame`, `chunkOf`, `snapshotFrames`) from `@net-mesh/browser/netcode`, the store's internals from `@net-mesh/browser/store`, and the peer driver, org trampolines and refusal constants, `StreamIdentityError` and the `LeafWasmOrg*` types from the root. Code that builds its own transport on the same primitives no longer has to copy them. Nothing that was exported before changed.
- **A host whose hex node id has no letters is joinable.** `joinStore` read its `host` the way it reads an event's peer, where a string of digits is decimal, so for about one host id in 1,800 the joiner addressed a different node and could never join. An id the caller holds is now read as hex, for a store's host, the region directory and the handoff link. 0.39 fixed the joiner's own id; this is the host's.

---

## Smaller changes

- **CI stability.** The RTC dispatch-pause test no longer parks a runtime worker (it timed out silently after 180 s). The C-consumer job no longer rebuilds its scenario generators on every compiler lane. A CortEX checkpoint test's injected failure is armed for its own file, so a parallel test can no longer consume it.
- **The README** explains what a browser tab is on the mesh: a full node, with the same identity, channels and RPC, connected through a native anchor.
- Dependency updates, including hyper, napi, zeroize and Next.js.

---

## Version bump

Everything published moves to **0.41.0**:

- every manifest: crate, wire, leaf, CLI, deck, SDK, payments, and the Go, Node and Python bindings;
- the `@net-mesh/*` pins and the `net-mesh` Python bound (now `>=0.41.0,<0.42.0`);
- the skills' `net-version`;
- the Hermes integration pin;
- the lockfiles.

---

## Breaking changes

Rust source only, for code that uses the fold types directly. Nothing on the wire changed, and the Go, Node, Python and C surfaces are unaffected.

- `FoldKind` has a new required associated type, `KeyHasher`.
- `FoldState::entries` and `FoldState::by_node` take a hasher type parameter.
- `FoldState::keys_for` returns `Option<&[K::Key]>`, not `Option<&HashSet<K::Key>>`.
- `FoldState::by_node`'s values are `NodeRecord`s with private fields.
- `FoldState` has a private field, so it is built with `new()` or `Default`.
- `FoldStats` has two new public fields, `sweep_walks` and `sweep_yielded`.
- `SignalOutcome::Reject` has a new field, `detail`.

---

## How to upgrade

Bump to 0.41.0 and rebuild. If you implement `FoldKind` or read `FoldState` directly:

- Add `type KeyHasher = std::collections::hash_map::RandomState;` to a `FoldKind` implementation.
- Add `K::KeyHasher` wherever you name the full type of `entries` or `by_node`.
- Read a `NodeRecord` with `record.keys()` and `record.rev()`.
- Treat `keys_for`'s result as a slice.
- Set `sweep_walks` and `sweep_yielded` in a `FoldStats` struct literal. Deserializing older JSON is unaffected.
- Match `SignalOutcome::Reject { dialog, reason, .. }`.

Operators: an anchor's `--game-stats-secs` line gains an `rtc` object, so a parser that rejects unknown keys needs updating.

---

Released 2026-10-07.

## License

See [LICENSE](https://github.com/ai-2070/net/blob/master/net/crates/net/LICENSE-APACHE).
