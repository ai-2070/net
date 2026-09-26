---
title: Browser
description: "Run a Net node in a browser tab over a WebRTC DataChannel — connect or openSession, a networked store, and a Three.js scene binding."
---

# Browser SDK

`@net-mesh/browser` is a Net mesh node **in a browser tab**: a WebRTC
DataChannel to a native anchor, a real Noise session over it, and the same nRPC,
channels, streams, fold announcements and capability queries the native SDKs use.

```typescript
import { connect } from '@net-mesh/browser';

const node = await connect({
  credentialB64,                          // the anchor's bootstrap credential
  bootstrapUrl: 'https://anchor.example', // overrides the credential's URL
});

await node.subscribe('jobs');
await node.announce(['transcribe']);

const reply = await node.call('summarise', new TextEncoder().encode('…'), 5_000);
```

The transport underneath — the anchor, the credential, direct versus routed — is
[WebRTC transport](/docs/concepts/webrtc-transport). The Rust half of the package
is `net-mesh-leaf`, compiled to WebAssembly.

## A sibling package, not a sub-path

`@net-mesh/sdk` is built on `@net-mesh/core`, the napi native binding. Every one
of the SDK's entry points resolves it, and a browser bundle that walked that
dependency graph would either fail on an unresolvable `.node` file or quietly
ship a shim for one.

So the browser surface is its own package with its own dependency graph. Nothing
in a page's build resolves `@net-mesh/core` at all, and the two packages version
their surfaces independently: the leaf's wasm ABI moves with the Rust crate, the
SDK's with napi.

## Build it

Install it from npm (published from 0.37.0):

```sh
npm install @net-mesh/browser
```

To build it from the repository instead — for development, or to try an
unreleased change — run two builds, in order: the leaf's WebAssembly, then the
package.

```sh
cd net/crates/net/leaf
cargo build --release --target wasm32-unknown-unknown
wasm-bindgen --target web --out-dir pkg \
  target/wasm32-unknown-unknown/release/net_leaf.wasm

cd ../browser-ts
npm install
npm run build
```

`wasm-bindgen-cli` must be **0.2.129** — the version the leaf pins. A mismatch is
a hard error at bindgen time, not a subtle one at run time.

`npm run build` runs `tsc` into `dist/` and then bundles a single file, copying
the leaf's `pkg/` beside the entry point. The copy looks at `$NET_LEAF_PKG` and
falls back to `../leaf/pkg`; if neither exists it says so rather than producing a
package that cannot load.

The one-command path does all of the above and opens a running demo:
`net/crates/net/examples/browser-demo/run.sh` (`run.ps1` on Windows).

## Load it in a page

Two shapes come out of that one build, and either is fine:

- serve `dist/` and `import { connect } from '/browser/index.js'` — the entry
  imports its siblings with explicit `.js` specifiers, so no bundler or import
  map is involved;
- map the single file `dist/index.bundle.js`.

Either way the wasm is fetched **relative to the entry point**, so the `pkg/`
output has to sit beside it. Override the lookup with `connect({ wasmUrl })`, a
pre-imported module with `connect({ wasm })`, or a factory with
`connect({ wasmModule })`.

## Two entry points

There is one node per origin. Tabs contend for a Web Lock, the holder runs the
node on the main thread — `RTCPeerConnection` does not exist in a worker — and
every other tab attaches as a follower and drives the same node through it.

| | `connect()` | `openSession()` |
| --- | --- | --- |
| Gives you | **this tab's** node | **the origin's** node, whichever tab runs it |
| Call it twice in one origin | two nodes contending for one identity | the same node |
| Survives the tab closing | no | yes — a follower is promoted |
| Reach for it when | a harness, or a page that is deliberately the one node | any real page |

**Use `openSession` unless you know you want otherwise.** It is the same
capability surface, with three methods promoted to promises because on a
follower the work happens in another tab: `counters()`, `isEnrolled()` and
`openStream()`. Everything else — nRPC, channels, streams, announcements,
queries, enrollment — has the same shape and the same typed errors on both.

## Follow the path

1. [Quickstart](/docs/sdk/browser/quickstart) — build it, connect, call, close
2. [Session](/docs/sdk/browser/session) — one node per origin, and its lifecycle
3. [Store](/docs/sdk/browser/store) — an authoritative document served to replicas
4. [Three](/docs/sdk/browser/three) — bind a store's entities to a scene graph
5. [Errors](/docs/sdk/browser/errors) — the typed taxonomy, and `udp-blocked`

## The store and the scene binding

Two exports beyond the node itself, for a page that has a world rather than a
request: one node hosts an authoritative document, others join replicas of it,
and a subpath binds the entities to a scene graph.

```typescript
import { defineStore, hostStore, joinStore, hostPlayer } from '@net-mesh/browser';
import { bindEntities } from '@net-mesh/browser/three';
import { createLocalMesh } from '@net-mesh/browser/local';   // offline development
```

`hostPlayer` is the hosting node's own player, with the same handle shape as a
joined replica. `@net-mesh/browser/local` runs several nodes in one page with no
anchor, for building game logic before there is a network.

`@net-mesh/browser/three` imports nothing from `three` — the scene graph is
anything with `add` and `remove`, and the types are structural — so the package
gains no renderer dependency. See [Store](/docs/sdk/browser/store) and
[Three](/docs/sdk/browser/three).
