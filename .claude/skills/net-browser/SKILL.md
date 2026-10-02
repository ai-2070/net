---
name: net-browser
description: "Use this skill when the target is a **browser page** — Net in a tab, or a multiplayer Three.js game built on it. Covers: **`@net-mesh/browser`** (a sibling package to the Node SDK, never a sub-path of it) riding a **WebRTC DataChannel** to a native **anchor** ('run Net in a browser', 'WebRTC transport', 'connect a page to the mesh'). **One node per origin** — Web Lock election, leader vs follower tabs, `connect()` vs `openSession()`, a follower promoted when the holder closes, generation fencing. **The anchor + bootstrap credential** (`net-mesh anchor credential mint`, `credentialB64`, `bootstrapUrl`), the ICE/STUN split, and the ICE-failure classification (`udp-blocked` needs two observations; `ice-timeout` alone is not evidence). **Leaf-to-leaf sessions** (`connectPeer` / `acceptPeer`), streams with a required `reliability`, and the typed `LeafError` / `RtcError` / `RpcError` kinds — including `rpc-indeterminate` from a frozen leader, which must not be retried. **The networked store** for game state: `defineStore`, `hostStore`, `joinStore`, `hostPlayer` (the host's own player), `createLobby` / `listLobbies` / `joinLobby` (lobbies, room codes, capacity, kick), interest management for large worlds (`interest` keys, `cellsAround` / `stickyCells`, `setInterest`), declared `visibility` (rules, presets, `hiddenOr`, `assertHidden`), the `onEvent` hook (join / leave / area) and inventory helpers (`addItems` / `removeItems` / `onlyOwn`), `createLocalMesh` from `@net-mesh/browser/local` (offline prototyping in one page), one authoritative document with replicas, audiences with `project`/`authorize`, correlated `act` versus coalesced `input`, chunked snapshots, the twelve `StoreError` codes, and `bindEntities` from `@net-mesh/browser/three` binding entities to a scene graph ('multiplayer game state', 'authoritative game document', 'three.js networked store', 'multiplayer browser game', 'sync players', 'one player hosts'). **Game anchors**: `requestCredential` / `rememberedIdentity` against `net-mesh anchor serve --game`. **Lossy streams** (`openStream({ lossy: true })`) and **netcode** from `@net-mesh/browser/netcode` (`hostNetcode` / `joinNetcode`: prediction, reconciliation, interpolation, lag compensation, interest keys). **Large worlds** from `@net-mesh/browser/world`: regions (`announceRegions`, `regionDirectory`, `joinWorld`), at-most-once entity handoff between region hosts (`regionHandoffs`, `handoffLink`), cross-border actions and ghosting. **Dedicated Node hosts** through `@net-mesh/sdk`'s `meshStoreTransport` and `persistStore` / `restoreStore`. Skip for native/Node/Python/Go/C mesh work that has no browser in it, and for editing Net's own internals."
allowed-tools: ["Read", "Grep", "Glob", "Bash", "Edit", "Write"]
metadata:
  skill-version: 1.0.0
  last-updated: 2026-09-27
  net-version: 0.39.0
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
   shape (host player via `hostPlayer(host)`, joiners `joinStore`, `enlist` keyed
   by `context.peer`, and `createLobby` / `listLobbies` / `joinLobby` for
   finding each other).
2. **Use `connect()`, one tab per player** — a store over `openSession` is not
   established. Test two players with two browser profiles, not two tabs.
3. **The anchor is `net-mesh anchor serve --issuer-identity <key> --game <id>`**
   (plus `--psk-file`, `--url`, `--tls-cert`/`--tls-key`, `--allow-origin`).
   Each player asks it for an anonymous credential and keeps one identity:
   `const { credentialB64, bootstrapUrl } = await requestCredential({ anchorUrl,
   game })`, then `connect({ credentialB64, bootstrapUrl, ...rememberedIdentity()
   })`. One credential is one player (another identity presenting it is refused).
   A **public anchor** (`--open-games <state-file>`) admits any game id from any
   page with no `--game`/`--allow-origin` for it; each open game is keyed on the
   page's `Origin` plus the id, so two sites' `chess` are two separate games.
   Several games may share one anchor; it keeps their players apart.
4. **Prototype game logic offline** with `createLocalMesh()` from
   `@net-mesh/browser/local` — several nodes in one page, the real store, no
   anchor, no network.
5. **Render with `bindEntities`** from `@net-mesh/browser/three`.
6. **Walkthrough for game developers:** `net/crates/net/browser-ts/README.md`.

## How to use this skill

| File | Read when |
|---|---|
| `concepts.md` | **Always first** — the mental model. A tab is a node; the anchor finds peers rather than carrying traffic; one node per origin; a store has one authority; what a browser *cannot* do. ~6 min. |
| `session.md` | Connecting and staying connected — `connect()` vs `openSession()`, the bootstrap credential, ICE/STUN and the UDP probe, leader/follower lifecycle, leaf-to-leaf peer sessions, streams, the event union, identity and the origin trust boundary. |
| `netcode.md` | Responsive movement — `hostNetcode` / `joinNetcode` from `@net-mesh/browser/netcode`: host tick loop, snapshot interpolation, prediction and reconciliation, capped lag compensation, clock sync, all on the lossy carrier (`openStream({ lossy: true })`). |
| `world.md` | Large worlds — `@net-mesh/browser/world`: regions each hosted as a store, `announceRegions` / `regionDirectory` (with `trustedHosts`), at-most-once entity handoff between region hosts (`regionHandoffs`, `handoffLink`, `storeRegion`), cross-border actions (`forward`), ghosting, and a player's merged view across regions (`joinWorld`). |
| `store.md` | Multiplayer state — `defineStore` / `hostStore` / `joinStore`, audiences and projection, `act` vs `input`, snapshots and their bounds, expiry/resync/revocation, the `StoreError` codes, and `bindEntities` to a scene graph. |
| `errors.md` | A rejection you have to classify. The full `LeafError` kind table, the admission/carriage/no-answer split, `udp-blocked`'s two observations, `rpc-indeterminate`, and the `CredentialRequestError` / `LobbyError` / `BorderActionError` codes. |
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
   binding to **every** endpoint the anchor published (one per family on a
   dual-stack anchor) is, and even then the message states the observation, not
   a cause. (A dual-stack anchor also needs a STUN endpoint per family; Firefox
   on an IPv6-only network with no IPv4 route gathers nothing, which reads as
   `ice-timeout`. That is an unsupported network, not blocked UDP. On 464XLAT
   it connects.) And `rpc-indeterminate` from
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
   is signed and secret-bearing; the leaf presents it unmodified. A game's
   anchor issues one per visitor: `net-mesh anchor serve --issuer-identity
   <key> --game <id> …` serves `POST <url>/credential {"game"}`, which
   `requestCredential({ anchorUrl, game })` calls; refusals are a typed
   `CredentialRequestError` (`unknown-game`, `rate-limited`, …). The credential
   binds to the first identity that enrolls with it, which may reconnect with it
   for 12 h. Without `--game` the anchor serves no enrollment and `connect()`
   times out with `rpc-timeout`; `anchor credential mint` still mints one by
   hand. `anchor ls` / `stats` / `serve` need the CLI's `rtc-bootstrap` feature
   — see `session.md`.
4. **Wire identity deliberately.** `openSession()` persists its identity (in
   IndexedDB, under a non-extractable key); **`connect()` does not** — without
   `entitySecretHex` + `noiseSecretHex` every page load is a new node. Games run
   on `connect()`, so pass `...rememberedIdentity()`, which keeps both secrets in
   `localStorage`. Either way the origin is the trust boundary: script on it
   can use the key.
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
