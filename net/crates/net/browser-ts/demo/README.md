# Fleet — the store, a Three.js scene, and one recipe

A small playable scene that consumes the **built** `@net-mesh/browser`
package and the **real** store: `defineStore`, `hostStore`, `joinStore`,
the codec, the chunker, the assembler, the action ledger and both state
machines. No mock store, no private WASM hooks — `demo/main.js` imports
`../dist/index.js`, which is what an application imports.

Two ships, WASD to steer, space to fire.

- **Steering is an `input`**: coalesced, unacknowledged, newest wins.
  A dropped one may be the last one, and the scene tolerates that.
- **Firing is an `action`**: correlated, answered with its result, and
  refused by the host's policy when you fire at yourself — the HUD
  prints the refusal code. A page cannot grant itself that permission.
- **The waypoint is audience-scoped**: the host holds it, a `crew`
  caller never receives it. Not hidden in the renderer — absent from
  the frames.

## Run it

### Local mode — no anchor, no network (default)

```bash
cd net/crates/net/browser-ts
npm install            # the package
npm run build          # produces dist/, which the page imports
cd demo && npm install # Three.js, only the demo needs it
node serve.mjs         # http://localhost:8173/
```

Open <http://localhost:8173/demo/index.html>. The host and two joiners
run in that one page over `demo/local-mesh.js`.

**What local mode is, exactly.** The store is real and every frame is
really encoded, parsed, chunked, assembled and dispatched. The *mesh*
is not: delivery is a function call, and the authenticated peer is
assigned by `local-mesh.js` instead of proved by a handshake. It is for
developing the game without an anchor. A screenshot of it is **not**
evidence that two browsers can play.

### Mesh mode — two browsers, one anchor

You need a running anchor (the native host path, unchanged by this
demo) and a bootstrap credential minted against the same PSK. Both are
`net-mesh` commands; the flags below are the real ones — run
`net-mesh anchor serve --help` and `net-mesh anchor credential mint
--help` for the rest, and read
`docs/internal/spikes/S4B_REPORT.md` for why the RTC and STUN
endpoints must be distinct.

```bash
cd net/crates/net

# 0. An issuer identity and a trust-domain PSK, once:
cargo run -p net-cli -- identity generate --out issuer.toml
openssl rand -hex 32 > psk.hex

# 1. The anchor. `rtc-bootstrap` is NOT a default feature:
cargo run -p net-cli --features "net webrtc rtc-bootstrap" -- \
  anchor serve \
  --bind 0.0.0.0:4433 \
  --psk-file psk.hex \
  --listen 0.0.0.0:8443 \
  --url https://<name-on-your-certificate>:8443 \
  --rtc-public-addr <public-ip>:4433 \
  --rtc-stun-bind 0.0.0.0:0 \
  --tls-cert cert.pem --tls-key key.pem

# 2. A bootstrap credential for the browser, base64 on stdout:
cargo run -p net-cli --features "net rtc-bootstrap" -- \
  anchor credential mint \
  --root <mesh-root-entity-hex> \
  --issuer-identity issuer.toml \
  --anchor-noise-pubkey <anchor-noise-x25519-pubkey-hex> \
  --psk-file psk.hex

# 3. Serve the demo over the SAME origin for both tabs:
cd browser-ts/demo && npm install && node serve.mjs
```

The bootstrap listener needs a **browser-trusted** certificate: a page
cannot fetch an endpoint whose certificate it does not accept. Supply
one with `--tls-cert`/`--tls-key`, or let the anchor obtain one with
`--acme-directory`.

Then:

1. **Host tab** — open
   `http://localhost:8173/demo/index.html?mode=mesh&credential=<base64>`.
   It prints `hosting as <16 hex>` in the HUD.
2. **Player tab** (second browser, or another machine on the same
   anchor) — open the same URL plus `&host=<16 hex>`.

`&bootstrap=https://host:8443` overrides the endpoint carried by the
credential.

A leaf's identity is scoped to its **origin**, so both tabs must be on
the same origin to be the same node; the server sets
`Cross-Origin-Opener-Policy: same-origin` for that reason.

## What this demo does and does not establish

It establishes that the built package's store drives a real scene:
[`test/store/hosted.test.ts`](../test/store/hosted.test.ts) exercises
the same two halves over a transport double, and the page exposes
`globalThis.__demo` (`state()`, `scene()`, `steer()`, `fire()`,
`hostState()`) so a harness can drive it without scraping pixels.

It does **not** establish the stage's remaining acceptance criteria:

- **Chromium *and* Firefox** on the supported path. Verified here:
  headless Chromium only.
- **Direct and forced-fallback** delivery, receiver-observed, with
  per-pair application forwarding flat for direct and increasing for
  routed. Nothing in this directory measures that.
- **Reliable transfer** (the B2′ gate) and the **leader-proxy
  lifecycle** (the G gate): two tabs sharing an origin's leader, and
  last-consumer cleanup on a proxied handle.

Those are transport properties, and `mode=mesh` is written but unrun.
Until they are established, this is a working demonstration of the
store — not a demonstration of the mesh.

## The files

| File | What it is |
|---|---|
| `game.js` | the store definition, projection, handlers and policy — the only file that says what the game *is* |
| `scene.js` | Three.js: state in, meshes out; reconciles rather than rebuilds |
| `main.js` | wiring: mode selection, the frame loop, keyboard → input/action |
| `local-mesh.js` | the development bus for `mode=local`, and what it is not |
| `serve.mjs` | a dependency-free static server; ES modules and importmaps need an origin |
| `package.json` | Three.js, scoped to the demo so the published package does not carry it |
