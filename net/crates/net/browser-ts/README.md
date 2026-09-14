# `@net-mesh/browser`

A Net mesh node **in the browser**: a WebRTC DataChannel to a native
anchor, a real Noise session over it, nRPC, channels, streams, fold
announcements and capability queries — the same mesh, from a page.

```typescript
import { connect, isUdpBlocked } from '@net-mesh/browser';

const node = await connect({
  credentialB64,                                    // issued by the anchor
  bootstrapUrl: 'https://anchor.example/rtc/bootstrap',
});

node.on('channel_message', (event) => render(event.payload));
await node.subscribe('jobs');
await node.announce(['transcribe']);

const reply = await node.call('summarise', new TextEncoder().encode('…'), 5_000);
const workers = await node.query('transcribe');
// [{ nodeId, entityId, capabilities, rtcAddr, noisePubkey, version }]

const stream = node.openStream({ reliability: 'fireAndForget' });
await stream.send(frame);
for await (const payload of stream) consume(payload);

node.anchorIdHex();     // the anchor this leaf is bootstrapped to
node.counters();        // every leaf counter, u64s as exact strings
await node.signal(peerHex, dialog, 'offer', sdpBytes);  // 0x0D02, no session needed

node.close();
```

The Rust half is `net-mesh-leaf` (`net/crates/net/leaf`), compiled to
wasm; this package is the typed surface over it.

## Why a sibling package and not `@net-mesh/sdk/browser`

`@net-mesh/sdk` is built on `@net-mesh/core` — the **napi native
binding** in `net/crates/net/bindings/node`. It is not an optional
corner of the SDK: `src/types.ts`, `src/mesh.ts`, `src/node.ts` and
`src/stream.ts` all `import` from `@net-mesh/core`, and the SDK's
`prebuild` script compiles that binding before `tsc` runs.

A sub-path export (`@net-mesh/sdk/browser`) would publish browser code
out of a package whose dependency graph contains a `.node` addon.
Every bundler resolves the package, not the sub-path, when deciding
what a dependency needs: `@net-mesh/core` would be walked, and a
browser build would either fail on an unresolvable `.node` file or
quietly ship a shim for one. Keeping the two apart means a page's
dependency graph never contains the native binding at all, and the two
packages can version their surfaces independently — the leaf's wasm
ABI moves with the Rust crate, the SDK's with napi.

Same repo, same licences, same conventions (TypeScript, `tsc`,
vitest); different runtime, so a different package. `@net-mesh/sdk`
stays the Node surface, `@net-mesh/browser` is the browser surface,
and nothing needs a "browser" field to disambiguate them.

## Layout

```
browser-ts/
  src/
    index.ts        the public surface (re-exports only)
    node.ts         BrowserNode + connect(): the API a page uses
    stream.ts       LeafStream: send + async-iterate a stream
    events.ts       the event union, its parser, and the fan-out hub
    errors.ts       LeafError/RtcError/RpcError mirrored from Rust
    udp-probe.ts    the UDP-blocked classification and its two probes
    wasm.ts         the wasm-bindgen boundary, declared in one place
    async-queue.ts  the one-consumer queue behind both iterables
  test/             vitest: classification, errors, events, node surface
    fake-wasm.ts    a fake leaf module satisfying src/wasm.ts
  scripts/
    bundle.mjs      esbuild single-file bundle + copies the leaf pkg/
    size.mjs        raw + gzipped bytes, with the exit-criterion verdict
```

`npm run build` runs `tsc` (ESM + `.d.ts` into `dist/`) and then
`scripts/bundle.mjs`, which writes `dist/index.bundle.js` and copies
the leaf's wasm-bindgen output (`$NET_LEAF_PKG`, else `../leaf/pkg`)
next to it.

Two ways to load it in a page, both produced by that one build:

- serve `dist/` and `import { connect } from '/browser/index.js'` —
  the entry imports its siblings with explicit `.js` specifiers, so no
  bundler or import map is involved;
- map the single file `dist/index.bundle.js`.

Either way the wasm is fetched **relative to the entry**: the loader
does `import(new URL('./net_leaf.js', import.meta.url))`, and the
generated glue fetches `net_leaf_bg.wasm` beside itself. Override with
`connect({ wasmModule })`, `connect({ wasmUrl })`, or hand over an
already-imported module with `connect({ wasm })`.

## Typed errors

Everything rejects with a `LeafError` subclass mirroring
`net_leaf::error`. `.message` is verbatim the Rust `Display` text that
crossed the boundary; `.kind` is a flat, stable discriminant:

| `.kind` | class | Rust variant |
|---|---|---|
| `wire` | `WireError` | `LeafError::Wire` |
| `session` | `SessionError` | `LeafError::Session` |
| `control-plane` | `ControlPlaneError` | `LeafError::ControlPlane` |
| `identity` | `IdentityError` | `LeafError::Identity` |
| `not-leader` | `NotLeaderError` | `LeafError::NotLeader` |
| `ice-timeout` | `RtcError` | `RtcError::IceTimeout` |
| `udp-blocked` | `RtcError` | `RtcError::UdpBlocked` |
| `channel-closed` | `RtcError` | `RtcError::ChannelClosed` |
| `rtc-unsupported` | `RtcError` | `RtcError::Unsupported` |
| `rpc-refused` | `RpcError` | `RpcError::Refused` |
| `rpc-timeout` | `RpcError` | `RpcError::Timeout` |
| `session-lost` | `RpcError` | `RpcError::SessionLost` |
| `leader-lost` | `RpcError` | `RpcError::LeaderLost` |
| `rpc-malformed` | `RpcError` | `RpcError::Malformed` |
| `unknown` | `UnknownLeafError` | *nothing* — see below |

An unrecognised message becomes `UnknownLeafError` rather than being
folded into a near neighbour. Mis-typing a failure is the exact
mistake the next section exists to prevent, so the package does not
guess.

`RpcError::SessionLost` and `RpcError::LeaderLost` are surfaced, never
retried silently — that is §8's rule for a call whose leader or
session went away, and the caller is the one who gets to decide.

## `udp-blocked`: the correction, and what actually establishes it

An ICE timeout is **not** evidence that UDP is blocked. An anchor that
is down, misconfigured or saturated produces exactly the same symptom.
So an ICE failure surfaces as `ice-timeout`, and only two observations
together may narrow it:

1. the HTTPS bootstrap to **that anchor** succeeded — it is up and
   addressable; and
2. a STUN binding to the `rtc_addr` **that same anchor published** went
   unanswered.

`classifyRtcFailure(observations)` is a pure function of those two
facts and is the only path to a `udp-blocked` error. `UdpBlockedEvidence`
types both observations as the literal `true`, and
`udpBlockedEvidence()` returns `null` unless both hold and the address
is named — the mirror of Rust's `UdpBlockedEvidence::new`.

`probeStunBinding(addr)` produces the second observation with an
`RTCPeerConnection` whose only ICE server is that address. What it keys
on was **measured in headless Chromium**, not assumed:

| what the engine did | outcome | classification |
|---|---|---|
| server-reflexive candidate arrived | `reflexive` | stays `ice-timeout` |
| `icecandidateerror` with a code < 700 (a STUN **error response**) | `stunError` | stays `ice-timeout` |
| `icecandidateerror` 701, or gathering completed with no reflexive candidate | `unanswered` | `udp-blocked` **if** the bootstrap succeeded |
| nothing at all before the deadline | `unanswered` | `udp-blocked` **if** the bootstrap succeeded |

Three measured details that shape the implementation:

- **Host candidates prove nothing** and are ignored. They are gathered
  whatever the network does to UDP — Chromium even hides them behind an
  mDNS `.local` name.
- **A STUN error response still proves reachability.** An anchor's ICE
  agent refusing an unauthenticated binding means the packet arrived
  and the reply got home. Chromium reports the STUN error *number*
  (`1` for a 401), which is why the rule is a `< 700` threshold rather
  than a list of codes.
- **Against a black-holed address Chromium emits no error event and
  never completes gathering.** The probe's deadline is therefore
  load-bearing, not a safety net, and the evidence it records says so
  ("no answer inside the probe deadline").

The first observation comes from the leaf: a `connected` event proves
the bootstrap worked. Before any `connected` — a first attempt that
never got a session — `probeBootstrapReachable()` asks the question
directly, with a `no-cors`, `no-store` request so that a cross-origin
anchor without CORS headers cannot be mistaken for an unreachable one;
any response, including a 4xx, counts as reachable.

**The probe needs a subject.** The address comes from the `connected`
event's `rtc_addr`, or from `connect({ anchorRtcAddr })` for a page
that already knows it. With neither, there is no evidence and an ICE
timeout correctly stays `ice-timeout`; `connect({ failureTyping: {
probeOnIceTimeout: false } })` switches probing off with the same
consequence.

**The one assumption this rests on**, named rather than buried: the
anchor's published `rtc_addr` must answer an unauthenticated STUN
binding request (with a success *or* an error response). If a future
anchor silently drops them, a healthy anchor would look "unanswered"
and an unrelated ICE failure could be mis-typed. That is a property of
the anchor, so the browser matrix asserts it directly: a healthy-anchor
control run must show `reflexive` or `stunError`.

## Events

The wasm node delivers one JSON string per event; this package parses
it into a discriminated union. Tag values stay verbatim
(`channel_message`, not `channelMessage`) because they are the leaf's
own protocol vocabulary and appear in Rust logs, browser logs and test
assertions; field names are camelCased because they are TypeScript
property accesses.

```typescript
node.on('stream_data', (event) => { /* event.payload is Uint8Array */ });
node.onEvent((event) => log(event.type));            // every event
for await (const event of node.events()) { /* … */ }  // async iterable
```

`connected`, `disconnected`, `channel_message`, `stream_data`,
`announcement`, `signal`, `rpc_response`, `dropped`, `rtc_failure` and
`leader_changed` are typed; an unknown tag arrives as
`{ type: 'unknown', tag, raw }` rather than being dropped, so a newer
leaf never goes silent against an older page.

Two invariants worth knowing:

- **64-bit ids are exact decimal strings**, not numbers.
  `JSON.parse` rounds integer literals above 2^53 —
  `18446744073709551615` becomes `18446744073709552000` — which would
  silently mis-route a page filtering `channel_message` by hash.
  `parseEvent` re-quotes the known `u64` keys before parsing, so
  precision survives whether the leaf sends numbers or strings.
- **Byte payloads are decoded for you**: standard base64 on the wire,
  `Uint8Array` in the event.

A throwing listener is reported to the console and skipped; it neither
takes down its siblings nor unwinds into the wasm frame that called it.

## Identity and the trust boundary

The node's identity is generated inside the wasm leaf from the
platform CSPRNG: an Ed25519 `EntityKeypair` (which `node_id` and
`origin_hash` are derived from) plus a Noise X25519 static key. A
custodial identity can be supplied on the Rust surface
(`LeafIdentity::from_secrets`, the shape of
`MeshNodeConfig::entity_keypair`); that path is **not** yet reachable
through `connect()`'s options, so today a page always gets a
leaf-generated identity.

**The origin is the trust boundary.** Persisting the identity —
IndexedDB under a non-extractable WebCrypto AES-GCM key — belongs to
the leaf's §8 storage slice, and the honest statement holds whatever
the wrapping: non-extractable means script cannot read the wrapping
key, but any script running *on this origin* can ask the browser to
use it. The storage protects the key from exfiltration, not from a
page that has already been compromised. A deployment that needs the
key held elsewhere must inject it custodially rather than rely on
browser storage.

## Sizes

`npm run size` reports raw and gzipped bytes for the wasm, the glue,
the ESM directory and the single-file bundle, prints
`SIZE <artifact> raw=<n> gz=<n>` lines for CI to grep, and compares the
wasm against the S0a baseline (576 461 B raw / 160 883 B gzipped — the
wire crate alone, before a wasm-bindgen pass).

```
npm run size                    # table + machine-readable lines
node scripts/size.mjs --json    # the same numbers as JSON
node scripts/size.mjs --assert  # non-zero exit if the wasm exceeds 1.5 MB gzipped
node scripts/size.mjs --require-leaf   # also fail if the real leaf wasm is absent
```

Measured against `net-mesh-leaf` as built by
`cargo build --release --target wasm32-unknown-unknown` +
`wasm-bindgen --target web`:

| artifact | raw | gzip -9 |
|---|---|---|
| `net_leaf_bg.wasm` | 553 668 B | **225 369 B** — 6.7× under the limit |
| `net_leaf.js` (glue) | 43 026 B | 8 684 B |
| this package, ESM directory | 50 329 B | 17 430 B |
| this package, single-file bundle | 16 064 B | 5 541 B |
| what a page downloads (wasm + glue + bundle) | 612 758 B | 239 594 B |

Against the S0a baseline the leaf wasm is 22 793 B *smaller* raw and
64 486 B larger gzipped: S0a's raw figure was inflated by the
180 737 B `__wasm_bindgen_unstable` section that the CLI pass consumes,
while the gzipped growth is the leaf's own weight — Ed25519, X25519,
`serde_json`, base64 and the `web-sys` glue on top of the wire code.

## Development

```
npm install
npm run build     # tsc + bundle + copy the leaf pkg/
npm test          # vitest
npm run size
```

The unit tests run against `test/fake-wasm.ts`, a fake module
satisfying the interfaces in `src/wasm.ts` — the real wasm is exercised
by the Playwright matrix in `net/crates/net/tests/rtc_browser/`. If the
Rust surface changes, `src/` stops compiling against the declared
boundary and the fake stops satisfying it: both are compile-time
failures, not silent drift.
