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

**This has been run, and getting it to run found four things.** All
four are recorded here because each one presents as a different
failure than its cause.

1. **`net-mesh anchor serve` cannot host a browser.** A browser's
   `connect()` makes an ENROLLMENT call while establishing its
   session, and `anchor serve` registers only the anchor directory
   and the ICE ledger (`cli/src/commands/anchor.rs`, `run_serve`).
   Nothing answers `ENROLL_SERVICE`, so the page fails with
   `session: rpc: the call's deadline elapsed` — after a perfectly
   good TLS listener, credential and handshake. The browser-demo
   example says it from the other side: its provider guard is "held
   for the process's lifetime … then `connect()` would hang on its
   enrollment call again".
2. **The store needs `connect()`, not `openSession()`.** Joining
   needs a SESSION with the host peer, which `connectPeer` installs —
   and `connectPeer` lives on the `connect()` node, not on the
   leader/follower `MeshSession`. Over a session the store's frames
   fail with `no session with 0x…`. One tab per node is what this
   demo wants anyway; the leader surface earns its keep when several
   tabs share one node.
3. **An announcement is a lease.** Announce once and a peer that
   looks a few seconds later finds nothing: the joiner then reports
   `the host … never announced fleet.host` about a host that did.
   Both sides re-announce on a timer (the native browser-demo does it
   every 500 ms).
4. **Two tabs of one origin in one browser profile are ONE node** —
   that is the identity-sharing feature, and it means the second tab
   tries to join itself. The store refuses that with `invalid-data`
   ("a replica cannot join the node it runs on"). Use two profiles
   (`--user-data-dir`), two browsers, or two machines.

#### The runnable path today

The browser-demo example is the only shipped thing that serves
enrollment, so it is also the only local anchor a browser can
actually use — and it now serves this demo's tree at `/fleet/`:

```bash
cd net/crates/net
cargo run --release --manifest-path examples/browser-demo/host/Cargo.toml -- \
  --headless --seconds 600

# It prints:  [demo] page http://localhost:<port>/
# A credential for any of its three contexts:
curl -s "http://localhost:<port>/config?tab=2" | jq -r .credentialB64
```

Then open the HOST with that credential:

```
http://localhost:<port>/fleet/demo/index.html?mode=mesh&credential=<credential>
```

It prints its own node id in the status line. Open the JOINER **in a
different browser profile**, with a different `tab=` credential:

```
http://localhost:<port>/fleet/demo/index.html?mode=mesh&host=<host node id>&credential=<other credential>
```

What that produced here, on two real nodes over one real anchor: two
ships in both views, the joiner's steering moving its ship on the
HOST's view, `fire` returning `{hull: 75}` and the damage appearing
on both, `fire` at yourself refused `forbidden`, and the host seeing
`waypoint: true` while the joiner sees `waypoint: false` — the
`command` audience withheld from a replica across a real transport,
which is the projection property the store exists for.

#### The anchor-only recipe (for a deployment that has enrollment)

Everything below brings up the anchor half. It is correct as far as it
goes, and by itself it is not enough — see (1).

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

What this DIRECTORY establishes and what it does not, now that
`?mode=mesh` has been run (see above):

- **Mesh mode works**, on two nodes over one real anchor, and the
  audience projection holds across it — that is the run described
  above, in two headless Chromium profiles.
- **Firefox** is NOT verified by this demo. The engine matrix lives in
  the browser harness (`tests/rtc_browser/run.sh --engine firefox`),
  which is where CI holds it.
- **Per-pair forwarding, flat for direct and increasing for routed**,
  is not measured here either: Stage 6's witnesses 9–11 hold it on the
  raw stream surface and Stage 7's fifth witness holds the routed half
  on STORE traffic. Nothing in this directory reads a counter.
- **Reliable transfer** (the B2′ gate) is held by the Stage 7 harness
  witness that installs a snapshot through injected loss and reorder,
  not by anything you can see by opening this page.

So: this is a demonstration of the store, and — in mesh mode — of the
store composed with the mesh. The transport's own properties are the
harness's to prove, and it does.

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
