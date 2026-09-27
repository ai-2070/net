---
title: "v0.37.1 — Turbo Lover"
description: "Release notes for Net v0.37.1 — Turbo Lover — what shipped, what changed, and what it means for compatibility."
---
# Net v0.37.1 — "Turbo Lover"

*Same Judas Priest chrome as [v0.37](/docs/releases/release-v0.37-turbo-lover), with the engine no longer stalling on the anchor. One mesh fix: a browser tab stays connected for as long as it keeps talking.*

## What's in it

v0.37.1 is a **patch release with one behavioral fix in the mesh core** and nothing else that changes how a node runs. The fix matters to anyone running the anchor that shipped in v0.37 against `@net-mesh/browser`. Without it, every browser peer was dropped about 90 seconds after it connected.

---

## Fix: a peer that never heartbeats is no longer dropped while it talks

**The symptom.** An anchor (`net-mesh anchor serve`) marked every connected browser peer failed, and let its direct route to that peer age out, `3 × session_timeout` after the session came up. With the default timeout that is about 90 seconds. It happened whether or not the page was active. After that, the anchor dropped any traffic it relayed toward the page, so leaf-to-leaf pairs that ICE could not connect directly lost each other.

**The cause.** Two things kept a peer alive, and both were fed only by heartbeats and pingwaves:

- the failure detector's liveness record;
- the `updated_at` stamp on the peer's direct route.

A browser leaf sends neither. It speaks only authenticated session traffic. So neither was ever refreshed after the handshake, and both expired on schedule however much data the leaf sent.

**The fix.** An inbound packet that decrypts under the peer's session key and clears the replay window now counts as proof of life:

- It refreshes the peer's failure-detector record.
- It refreshes the `updated_at` stamp of the peer's own direct route. That is only the protected candidate whose next hop is the peer itself, and only while it is active.

Both refreshes are rate-limited to once a second per peer, and the common case takes a read lock and returns. Nothing else changes:

- An unauthenticated or replayed packet refreshes nothing.
- The refresh never reactivates a withdrawn route.
- It never touches a route learned through another node.
- A peer that goes quiet still expires on the same schedule as before.

Native nodes, which heartbeat anyway, behave exactly as they did. Two new tests run over real UDP sessions and cover both directions:

- a heartbeatless peer that keeps sending stays `Healthy` and routable past both expiries;
- the same peer, silent, still reaches both.

The full diagnosis, with the anchor-side measurements, is in [`NET_ISSUE_PEERS_EXPIRE.md`](https://github.com/ai-2070/net/blob/master/docs/internal/misc/NET_ISSUE_PEERS_EXPIRE.md).

---

## Version bump

`0.37.0 → 0.37.1`, applied to:

- every manifest: crate, wire, leaf, CLI, deck, SDK, payments, and the Go, Node and Python bindings;
- the `@net-mesh/*` pins and the `net-mesh` Python bound;
- the skills' `net-version`;
- the Hermes integration pin;
- the lockfiles.

---

## Dependency updates

- The standalone test and example lockfiles (`examples/browser-demo/host`, `tests/natsim/browser`, `tests/rtc_browser/runner`) move to `str0m` `0.24.0`, which the workspace already required, and `is` `0.11.1`.
- `sharp` → `0.35.5` (web).

---

## Release tooling

- The npm release's zig linker wrapper now strips rustc's `-Wl,--fix-cortex-a53-843419` argument, which zig's linker rejects. This is what failed the v0.37.0 `aarch64-unknown-linux-musl` build of `@net-mesh/core`; that build was re-run from the fix, so v0.37.0 shipped complete. No package contents change.

---

## Breaking changes

None.

---

## How to upgrade

Bump to 0.37.1 and rebuild. No API, configuration or wire-format change. If you run an anchor for browser pages, upgrade the anchor: the fix is on the side that receives the traffic, so pages on `@net-mesh/browser` 0.37.0 benefit as soon as their anchor runs 0.37.1.

---

Released 2026-09-27.

## License

See [LICENSE](https://github.com/ai-2070/net/blob/master/net/crates/net/LICENSE-APACHE).
