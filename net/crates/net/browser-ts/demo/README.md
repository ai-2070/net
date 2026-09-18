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
demo) and a bootstrap credential minted against the same PSK. Both
are `net-mesh` commands; the flags below are the real ones — run
`net-mesh anchor serve --help` and `net-mesh anchor credential mint
--help` for the rest, and read
`docs/internal/spikes/S4B_REPORT.md` for why the RTC and STUN
endpoints must be distinct.

Two things about the build and the order, because both are easy to
get wrong:

- The CLI has exactly three features — `webrtc`, `rtc-bootstrap`
  (which implies `webrtc`) and `keychain`. There is no `net` feature.
  `anchor serve` is behind `rtc-bootstrap`, the only build that
  carries an HTTP server at all; `anchor credential mint` is in every
  build and needs no feature.
- **`serve` runs before `mint`.** `mint` has to pin the anchor's
  Noise static public key, and the only place that key is printed is
  `serve`'s own JSON report, as `noise_pubkey`.

```bash
cd net/crates/net

# 0. An issuer identity and a trust-domain PSK, once. `identity
#    generate` prints the identity's `public_key_hex`; that hex is
#    what `--credential-issuer` wants in step 1 (`net-mesh identity
#    show issuer.toml` prints it again later).
cargo run -p net-cli -- identity generate --out issuer.toml
openssl rand -hex 32 > psk.hex

# 1. The anchor. `rtc-bootstrap` is NOT a default feature. It prints
#    one JSON report — keep its `noise_pubkey` — and then serves
#    until ctrl-c:
cargo run -p net-cli --features rtc-bootstrap -- \
  anchor serve \
  --psk-file psk.hex \
  --listen 0.0.0.0:8443 \
  --url https://<name-on-your-certificate>:8443 \
  --credential-issuer <issuer public_key_hex> \
  --allow-origin http://localhost:8173 \
  --tls-cert cert.pem --tls-key key.pem \
  --rtc-bind 0.0.0.0:4433 \
  --rtc-public-addr <public-ip>:4433 \
  --rtc-stun-bind 0.0.0.0:0

# 2. A bootstrap credential for the browser, `net-bootstrap:…` on
#    stdout (`--out <path>` writes it 0600 instead). `--url` is the
#    same URL the anchor published:
cargo run -p net-cli -- \
  anchor credential mint \
  --root <mesh-root-entity-hex> \
  --issuer-identity issuer.toml \
  --anchor-noise-pubkey <noise_pubkey from step 1> \
  --psk-file psk.hex \
  --url https://<name-on-your-certificate>:8443

# 3. Serve the demo over the SAME origin for both tabs:
cd browser-ts/demo && npm install && node serve.mjs
```

`--bind` (the node's own mesh address) defaults to `0.0.0.0:0` and is
left out above; `--listen` defaults to `0.0.0.0:8443`. Everything
else on `serve` in that block is required: without `--psk-file`,
`--url`, `--credential-issuer` or at least one `--allow-origin` the
command refuses to start. On Windows `openssl` is usually not on
PATH — any 64 hex chars in a file will do, e.g. `-join ((1..32) |
ForEach-Object { '{0:x2}' -f (Get-Random -Max 256) }) | Set-Content
psk.hex`.

Steps 0 and 2 need no feature, but cargo fingerprints per feature
set: passing `--features rtc-bootstrap` to all three reuses one
build instead of paying for a second one.

**The page's origin must be in the allow-list.** `--allow-origin` is
repeatable and has **no wildcard** — an endpoint that takes a
credential does not get one — so the origin the tabs are actually on
has to be named. `node serve.mjs` serves `http://localhost:8173`, so
that is the origin to allow; `PORT=…` changes it and the flag with
it. `--url` is checked as a browser-fetchable URL at mint time: it
must be `https://`, or `http://` on `localhost`, `127.0.0.1` or
`[::1]`.

**A self-signed certificate is refused, by name.** The bootstrap
listener takes exactly two TLS sources: an operator PEM chain and key
via `--tls-cert`/`--tls-key`, or ACME via `--acme-directory` with
`--acme-email` (HTTP-01 on this same listener, cached under
`--acme-cache`). Anything else exits with *"browser-trusted TLS is
required … there is no self-signed mode, because a browser refuses
one"*. A page cannot fetch an endpoint whose certificate it does not
accept, and the anchor does not pretend otherwise.

**The supported way to get a browser talking to an anchor today is
one of the two harnesses**, which already do this bootstrap for real
— certificate and per-engine trust included — and need none of the
flags above:

- `tests/rtc_browser/run.ps1` (`run.sh` on Linux/macOS) — the CI
  gate, on **Chromium and Firefox** (`-Engine chromium|firefox`). It
  issues its own CA and a `localhost` leaf and makes only the
  launched engine trust that one key — an SPKI pin for Chromium, the
  profile's own `cert9.db` for Firefox, never the platform store —
  then starts the anchor and the bootstrap listener, serves the page
  on `http://localhost`, and prints one `RTCB PASS`/`RTCB FAIL` line
  per witness. There is no `--ignore-certificate-errors` anywhere in
  it.
- `examples/browser-demo/run.ps1` (`run.sh`) — the same bootstrap end
  to end for the three-tab direct-path demo, with `-Check` for a
  headless asserting run.

`tests/rtc_browser/runner/src/main.rs` is where the trust step is
written down per engine (`issue_localhost_certificate`,
`establish_trust`). Read it before attempting the recipe above by
hand: on a workstation the hard part is not the flags but the
certificate — ACME cannot issue for `localhost`, so a by-hand
`mode=mesh` needs either a real DNS name pointed at the machine or
exactly the private-CA-plus-per-engine-trust those harnesses
automate.

Then:

1. **Host tab** — open
   `http://localhost:8173/demo/index.html?mode=mesh&credential=<credential>`,
   where `<credential>` is the **whole** `net-bootstrap:…` string from
   step 2, prefix included (the body is URL-safe unpadded base64, so
   it survives a query string as-is). It prints `hosting as <16 hex>`
   in the HUD.
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

## The adapter is a package, not demo code

The scene binding this demo grew is published as the
`@net-mesh/browser/three` subpath:

```js
import { bindEntities } from '@net-mesh/browser/three';

const binding = bindEntities({
  store, scene, select: state => state.ships,
  binding: {
    create: (ship, id) => buildShip(ship, id),
    update: (object, ship) => object.position.set(ship.x, 0, ship.z),
    remove: object => object.traverse(disposeOf),
  },
});
```

It imports nothing from `three` — the scene graph is anything with
`add` and `remove`, and the objects are whatever `create` returns — so
the browser package gains no renderer dependency and the binding works
with a test double as readily as with a `THREE.Scene`.

What it is for: the store already shares the references of subtrees
that did not change, and a render loop that iterates
`Object.values(state.entities)` throws that away. `bindEntities` turns
it into the thing you wanted from it — an entity whose reference is
unchanged is not touched — and removes what left the world, which is
the other half nobody writes.

## The files

| File | What it is |
|---|---|
| `game.js` | the store definition, projection, handlers and policy — the only file that says what the game *is* |
| `scene.js` | Three.js: what a ship LOOKS like. The add/update/remove reconciliation is `@net-mesh/browser/three`, not this file |
| `main.js` | wiring: mode selection, the frame loop, keyboard → input/action |
| `local-mesh.js` | the development bus for `mode=local`, and what it is not |
| `serve.mjs` | a dependency-free static server; ES modules and importmaps need an origin |
| `package.json` | Three.js, scoped to the demo so the published package does not carry it |
