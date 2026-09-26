---
name: net-browser
description: "Use this skill when the target is a **browser page** — Net in a tab, or a multiplayer Three.js game built on it. Covers: **`@net-mesh/browser`** (a sibling package to the Node SDK, never a sub-path of it) riding a **WebRTC DataChannel** to a native **anchor** ('run Net in a browser', 'WebRTC transport', 'connect a page to the mesh'). **One node per origin** — Web Lock election, leader vs follower tabs, `connect()` vs `openSession()`, a follower promoted when the holder closes, generation fencing. **The anchor + bootstrap credential** (`net-mesh anchor credential mint`, `credentialB64`, `bootstrapUrl`), the ICE/STUN split, and the ICE-failure classification (`udp-blocked` needs two observations; `ice-timeout` alone is not evidence). **Leaf-to-leaf sessions** (`connectPeer` / `acceptPeer`), streams with a required `reliability`, and the typed `LeafError` / `RtcError` / `RpcError` kinds — including `rpc-indeterminate` from a frozen leader, which must not be retried. **The networked store** for game state: `defineStore`, `hostStore`, `joinStore`, one authoritative document with replicas, audiences with `project`/`authorize`, correlated `act` versus coalesced `input`, chunked snapshots, the twelve `StoreError` codes, and `bindEntities` from `@net-mesh/browser/three` binding entities to a scene graph ('multiplayer game state', 'authoritative game document', 'three.js networked store', 'multiplayer browser game', 'sync players', 'one player hosts'). Skip for native/Node/Python/Go/C mesh work, and for editing Net's own internals."
allowed-tools: ["Read", "Grep", "Glob", "Bash", "Edit", "Write"]
metadata:
  skill-version: 1.0.0
  last-updated: 2026-09-26
  net-version: 0.37.0
---

# Net in the browser — a tab as a mesh node

`@net-mesh/browser` runs a Net node **inside a page**: a WebRTC DataChannel to a
native **anchor**, a real Noise session over it, and the same channels, nRPC,
streams, capability queries and announcements the native SDKs use. Its Rust half
is `net-mesh-leaf`, compiled to WebAssembly.

The thing that makes this skill worth its own directory is the second half: a
**networked store** for multiplayer state — one node hosts an authoritative
document, other nodes join replicas of it, and a sub-path binds those entities to
a Three.js scene graph. Game code does not manage signalling, connection
bookkeeping or packet dispatch; it reads state and dispatches intents.

**Read `concepts.md` before writing any code.** The browser surface looks like
the Node SDK with different imports and is not: the transport is different, the
lifecycle is per-origin rather than per-page, and the store has an authority
model that a client-prediction habit will get wrong.

## Building a game? The fast path

1. **Read `store.md`** — its opening example and § Game recipe are the runnable
   shape (host player renders from `host`, joiners `joinStore`, `enlist` keyed
   by `context.peer`, re-announce, poll `query` before joining).
2. **Use `connect()`, one tab per player** — a store over `openSession` is not
   established. Test two players with two browser profiles, not two tabs.
3. **The anchor is `examples/browser-demo/host`** — from the Rust workspace
   root (net/crates/net), `cargo run --release --manifest-path
   examples/browser-demo/host/Cargo.toml -- --headless --seconds 600`; each
   player's credential is the `credentialB64` from its `/config?tab=N`.
   `net-mesh anchor serve` cannot host a browser today.
4. **Prototype game logic offline** in `net/crates/net/browser-ts/demo/` — a host
   and two players in one page over a local bus, the real store, no network.
5. **Render with `bindEntities`** from `@net-mesh/browser/three`.
6. **Walkthrough for game developers:** `net/crates/net/browser-ts/README.md`.

## How to use this skill

| File | Read when |
|---|---|
| `concepts.md` | **Always first** — the mental model. A tab is a node; the anchor finds peers rather than carrying traffic; one node per origin; a store has one authority; what a browser *cannot* do. ~6 min. |
| `session.md` | Connecting and staying connected — `connect()` vs `openSession()`, the bootstrap credential, ICE/STUN and the UDP probe, leader/follower lifecycle, leaf-to-leaf peer sessions, streams, the event union, identity and the origin trust boundary. |
| `store.md` | Multiplayer state — `defineStore` / `hostStore` / `joinStore`, audiences and projection, `act` vs `input`, snapshots and their bounds, expiry/resync/revocation, the `StoreError` codes, and `bindEntities` to a scene graph. |
| `errors.md` | A rejection you have to classify. The full `LeafError` kind table, the admission/carriage/no-answer split, `udp-blocked`'s two observations, and `rpc-indeterminate`. |
| `source-access.md` | You are not inside the Net repository and need to open a file this skill cites, or a mechanism this skill only summarizes. One command fetches the whole tree; the page carries the root map. |

## TL;DR mental model

1. **A tab is a full node, not a client.** It has its own Ed25519 identity, its
   own Noise session, its own streams, channels and capability announcements. An
   anchor cannot tell a browser session from a UDP one once the transport is up.
2. **The anchor is how browsers *find* each other, not how they *talk*.** It
   bootstraps signalling, answers STUN and relays the minority of pairs ICE
   cannot connect. A direct leaf ↔ leaf session is the intended path; a server
   hop on every position update is the thing this transport exists to avoid.
3. **One node per origin, elected.** Tabs contend for a Web Lock; the holder
   runs the node on the main thread (`RTCPeerConnection` does not exist in a
   worker) and every other tab is a **follower** driving the same node. A
   follower is promoted when the holder closes, re-bootstraps under the same
   identity with a new fenced generation, and restores its declared
   `subscriptions` and `capabilities`.
4. **Two entry points with different lifetimes.** `connect()` gives *this tab* a
   node; `openSession()` gives the *origin* its node, whoever is running it. Real
   pages want `openSession` — except the game store: use `connect()`, one tab
   per node; a store over `openSession` is not established. Three methods are promises there because the work
   happens in another tab — `counters()`, `isEnrolled()`, `openStream()`.
5. **The store is one authoritative document with replicas, not a CRDT.** The
   host executes `act`s (correlated results), coalesces `input`s
   (unacknowledged), and decides per audience what each replica may see with
   `project`; `authorize` is consulted on join *and* on the ongoing delta feed.
   A replica cannot read what it was not given.
6. **Two failures a page must not misread.** An ICE timeout is **not** evidence
   that UDP is blocked — only a successful bootstrap *plus* an unanswered STUN
   binding to the anchor's published `rtc_addr` is. And `rpc-indeterminate` from
   a frozen leader means "the remote may have executed this"; retrying can cause
   the effect twice.
7. **The package is published as `@net-mesh/browser`** (install with `npm
   install @net-mesh/browser`); from the repository, build the leaf's wasm, then
   the package. Nothing here resolves the napi binding — that is the whole reason
   it is a sibling package.

## Workflow when integrating

1. **Decide which plane the task needs.** A *request* (call a service, publish,
   subscribe, move bytes to one peer) → the node surface in `session.md`. A
   *world* (authoritative state many peers must agree on) → the store in
   `store.md`. Most games need both, and they compose: the store rides the node
   as its `transport`.
2. **Pick the entry point before writing code.** `openSession()` for a real page
   (one node per origin, survives a tab closing); `connect()` only for a harness
   or a page that is deliberately the one node — **and for the game store**: use
   `connect()`, one tab per node; a store over `openSession` is not established.
   `openSession` takes `capabilities` and `subscriptions` **up front** so a new
   leader can restore them without being asked.
3. **Get the credential from the anchor, and know its scope.** `credentialB64`
   is signed and secret-bearing; the leaf presents it unmodified. **`net-mesh
   anchor serve` cannot host a browser today** — it registers no enrollment
   service, so `connect()` times out with `rpc-timeout`. The only anchor a page
   can use is the browser-demo host: from the workspace root (net/crates/net),
   `cargo run --release --manifest-path examples/browser-demo/host/Cargo.toml --
   --headless --seconds 600`, which serves `/config?tab=N` returning a `credentialB64` per tab. `anchor
   credential mint` needs no extra feature; `anchor ls` / `stats` / `serve` need
   the CLI's `rtc-bootstrap` feature — see `session.md`.
4. **Wire identity deliberately.** By default the leaf generates the identity
   from the platform CSPRNG and persists it under a non-extractable key. The
   origin is the trust boundary: any script on it can ask the browser to use that
   key, so a deployment that needs the key elsewhere must inject it custodially
   (`entitySecretHex` + `noiseSecretHex`).
5. **Keep the lifecycle honest.** Subscribe before you need deliveries, and on a
   session declare channels in `subscriptions` rather than subscribing late — a
   tab that only called `subscribe()` after a handoff leaves a window where
   nobody holds the subscription.
6. **Classify every rejection with `errors.md`** before deciding to retry.
   `rpc-refused`, `forbidden` and `identity` are verdicts; `rpc-timeout` and
   `ice-timeout` are deadlines; `rpc-indeterminate` is *unknown*, and neither the
   package nor your page may retry it blindly.
7. **For multiplayer state, define the document first** — `defineStore`'s
   definition id/version, the state shape, the audience names, `project`, and
   only then `hostStore`/`joinStore`. Version disagreement is a typed refusal
   (`version-mismatch`), not a silent merge.
8. **Bind to the renderer last, and structurally.** `bindEntities` needs only
   `add`/`remove` on the scene graph; it deliberately imports nothing from
   `three`. Do not rebuild an entity whose reference did not change — the store
   shares unchanged subtrees and a full-state walk throws that away.
9. **Test with the browser demo harness, not a mock leaf.** `npm test` runs
   against a fake wasm module (compile-time drift protection only); the real
   package is proven by a Playwright run against a built bundle with the
   wasm-bindgen output beside it, and the worked example in the repository runs a
   real anchor.

## What this skill does not cover

- **The native mesh surface.** Channels, nRPC, orgs, subnets, RedEX/CortEX,
  Dataforts, MCP and the CLI are a different skill's territory; this one mentions
  a mesh verb only where a page must call it.
- **Native ↔ native WebRTC.** For two native nodes, UDP plus hole punching
  remains the transport. WebRTC in this repository exists for browser reach.
- **The store's unproven corners.** Two tabs sharing a leader around a store is
  recorded as not established in the source; `store.md` says exactly what is and
  is not witnessed. Do not present the rest as proven.
- **Anchor deployment.** Minting a credential and running a listener is a CLI
  concern; `session.md` covers only what a page must know about it.

## Further reading

- [Browser SDK](https://ai2070.net/docs/sdk/browser) — quickstart, session, store, Three.js, errors.
- [WebRTC transport](https://ai2070.net/docs/concepts/webrtc-transport) — how the leaf, the anchor and the mesh fit together.
