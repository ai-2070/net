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
// `signal` addresses §9's session-independent 0x0D02 envelope. Against an
// ANCHOR it is REFUSED, deliberately and always: `AnchorControlPlane::signal`
// rejects unconditionally with `LeafError::ControlPlane`, because the anchor
// control plane does not carry signalling envelopes. The carrier that does is
// the anchorless one (§9 / `wasm_anchorless`), and this call is how a page
// reaches it once one is attached.
await node.signal(peerHex, dialog, 'offer', sdpBytes).catch((e) => {
  e.kind; // 'control-plane' — the anchor refused; attach an anchorless carrier
});
await node.enroll();    // connect() already did this; explicit for harnesses
node.isEnrolled();      // false => the anchor still has this peer provisional

node.close();
```

The Rust half is `net-mesh-leaf` (`net/crates/net/leaf`), compiled to
wasm; this package is the typed surface over it.

## Two entry points: `openSession` for real pages, `connect` for one node

There is **one node per origin** (§8). Tabs contend for a Web Lock, the
holder runs the node on the main thread — `RTCPeerConnection` does not
exist in a worker — and every other tab attaches as a follower and
drives the same node through it.

```typescript
import { openSession } from '@net-mesh/browser';

const session = await openSession({ credentialB64, capabilities: ['transcribe'], subscriptions: ['jobs'] });
session.role();            // 'leader' | 'follower' — same surface either way
session.onLifecycle((event) => log(event.type));  // leader_changed, leader_lost, …
const reply = await session.call('summarise', bytes, 5_000);
```

**Use `openSession` unless you know you want otherwise.** `connect`
gives *this tab* its own node; two tabs calling `connect` on one origin
are two nodes contending for one identity. `openSession` gives the
origin's node, whichever tab is running it, and survives that tab
closing: a follower is promoted, re-bootstraps under the same identity
with a fenced new generation, and restores the `subscriptions` it was
opened with.

Three methods are promises on the session and synchronous on the direct
surface — `counters()`, `isEnrolled()` and `openStream()` — because on
a follower the work happens in another tab. Everything else has the
same shape and the same typed errors; a stale tab's operation fails as
`NotLeaderError` rather than silently doing nothing.

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
| `rpc-indeterminate` | `RpcError` | `RpcError::Indeterminate` |
| `rpc-malformed` | `RpcError` | `RpcError::Malformed` |
| `ice-server-conflict` | `IceServerConflictError` | `LeafError::IceServerConflictsWithPeer` |
| `unknown` | `UnknownLeafError` | *nothing* — see below |

An unrecognised message becomes `UnknownLeafError` rather than being
folded into a near neighbour. Mis-typing a failure is the exact
mistake the next section exists to prevent, so the package does not
guess.

`RpcError::SessionLost` and `RpcError::LeaderLost` are surfaced, never
retried silently — that is §8's rule for a call whose leader or
session went away, and the caller is the one who gets to decide.

**Admission is not carriage.** An enrollment refusal is `identity` —
`identity: the anchor rejected enrollment: replay (5): …`, or an
expired invite, or a request over §12's 16 384-byte bound — because the
anchor answered and said no. `control-plane` is for carriage:
offer/trickle/announcement-publish/signal. An anchor that never
answered at all is neither: it is `rpc-timeout`. That three-way
distinction plus `node.isEnrolled()` is how a page tells "my invite was
already redeemed" from "the anchor is slow" from "the anchor is
broken", all of which otherwise look like a call that never returned.

**A frozen leader, and why it is `rpc-indeterminate`.** On a session a
call can also fail because the tab running the node was frozen by the
browser. Two facts, and only one of them is a measurement:

- **Measured** (Chromium 152, two real tabs driven over CDP): a frozen
  tab **keeps** its Web Lock. So no successor is elected while it
  sleeps, and nothing in this tab's view changes — `role()` still says
  `'follower'`, `generation()` does not move, and no `leader_lost` or
  `leader_changed` arrives. Whether the `RTCPeerConnection` itself
  survives a freeze was *not* measured and is not claimed.
- **Decisive regardless of the transport**: a frozen document's task
  queues do not run. The leaf's pump, the DataChannel `onmessage`
  handler and the `BroadcastChannel` delivery carrying a follower's
  request are all queued tasks in that document, so even if every
  packet still arrives, nothing is processed and **nothing is
  answered** — not the calls, and not the cheap control messages
  either. A live-but-provisional leader still answers an attach with a
  `leadership` message; a frozen one answers nothing.

The follower arms its **own** deadline over the proxy round trip — the
caller's `timeoutMs` when one was given, the leaf's 30 s default when
it was not, plus a 250 ms grace for a reply already on the channel —
and what that deadline produces is `rpc-indeterminate`, not
`rpc-timeout`. The distinction is the whole disposition: a deadline on
*this* tab cannot cancel work that may already have been admitted on
another, so the honest answer is "the remote may have executed this",
which is what `rpc-indeterminate` means. A `rpc-timeout` from a
session is the node's own deadline, reported by a leader that was
running.

So the signal is: `rpc-indeterminate` **and** `role() === 'follower'`
**and** an unchanged `generation()` **and** no reply to anything,
including the control chatter. Fencing is unaffected — the generation,
not lock loss, is what fences a resumed tab — but liveness is: the
window is bounded by this deadline, and the *work* is bounded only by
the mesh's own failure detection.

**Do not retry on it.** On resume the backlog flushes and the frozen
tab is still the legitimate leader holding a valid generation — no
successor was elected, which is exactly the liveness gap — so those
calls may be executed *late*, after the caller already saw
`rpc-indeterminate`. A page that retries can therefore cause the
effect twice. Neither the follower's timer nor this package re-issues
anything: surface it, or wait.

This package deliberately does **not** paper over it with a
wrapper-level failure detector. Timing a leader out and forcing an
election would put a second detector beside the mesh's and race a tab
that is about to resume holding a valid generation — the stale-holder
hazard the leaf's three fence enforcers exist to prevent.

**Why `reliability` is required, not defaulted.** The wasm surface
treats an absent reliability as `reliable`, which is the right default
there — a stream with nothing said about it genuinely wants
retransmission, unlike an identity, which has no meaningful default and
so hard-fails above. But at the wasm boundary an absent key and a
*misspelled* one are the same thing, so `open_stream({ reliabilty:
'fireAndForget' })` silently yields a reliable stream. Making
`reliability` a required field of `OpenStreamOptions` is how this
package removes that footgun for its callers: a typo is a compile error
on an object literal. It is not removed for code that builds the
options object dynamically and widens the type, nor for pages that call
the wasm surface directly — which is why the leaf also asserts at the
wire level that a fire-and-forget stream emits packets with the
`RELIABLE` flag clear.

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

**The probe's subject is `rtc_addr`, and only `rtc_addr`.**
`diagnosticStunUrl(rtcAddr)` builds that target; the name says what it
is for. It is **not** a source of `ConnectOptions.iceServers`: for a
connection with that anchor, `rtc_addr` is the ICE peer, and a peer
cannot be its own STUN server. The endpoint an anchor connection
gathers against is the separate one the anchor announces as
`stun_addr` on `GET /rtc/anchor`, which `connect()` uses by default.
Configuring the peer anyway rejects with `IceServerConflictError`
before any ICE work, naming both endpoints.

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

## The stream boundary: one contract, tested through the built package

`LeafStream.onMessage` and `for await (const bytes of stream)` yield
`Uint8Array`. That promise is kept **here**, in TypeScript: the wasm
boundary hands this package the node's own `stream_data` event JSON —
the same string `on_event` delivers, filtered to the stream's id —
and `LeafStream` parses it and base64-decodes `payload`.

```text
Rust  LeafStream::on_message(cb)  ->  cb('{"type":"stream_data","stream_id":"9","seq":"1","payload":"AQI="}')
TS    new LeafStream(inner)       ->  onMessage(Uint8Array([1, 2]))
```

Emitting bytes from Rust instead would mean a second encoding of an
event that already exists, and would throw away `seq` and the rest of
the event's provenance at the boundary. So the direction is fixed:
**Rust emits its canonical event JSON, TypeScript decodes it**, on the
direct stream and the leader-proxied one alike — `MeshSession`
streams are the same `LeafStream` over the same event shape.

Two consequences a page can see:

- The event's `stream_id` is **decimal** and `LeafStream.streamId` is
  **hex**; they are the same `u64`. The wrapper matches inbound events
  numerically, so an id past 2^53 routes correctly.
- Options are typed at the shape Rust actually reads.
  `OpenStreamOptions.channelHash` is a **number** in `0..=65535` (it
  was a string that Rust read with `as_f64` and therefore ignored, so
  the stream rode hash 0), `streamId` is a decimal or `0x`-hex
  **string** because a `u64` does not survive a JS number, and
  `ConnectOptions.iceServers` is a real `RTCIceServer[]` — `urls` a
  string or an array, `username`/`credential` carried through for
  TURN. Each is refused with a typed error rather than silently
  dropped. `ConnectOptions.iceServers` is also **optional with a
  working default**: omitted, the leaf gathers against the `stun_addr`
  the anchor announces, and an entry naming this connection's peer
  rejects with `IceServerConflictError` before ICE rather than being
  silently stripped.

None of that is taken on trust. `tests/abi_real_package.mjs` loads the
**built** `dist/` and the wasm-bindgen `pkg/` beside it and asserts the
decode, the ids, the ICE parse and the effective stream options against
the real artifacts; `LeafNode.effective_ice_servers(opts)` and
`LeafNode.effective_stream_options(opts)` are the static, pure readers
it uses, and they are the same ones `connect` and `open_stream` call.
The package's test doubles no longer invent a shape either: they emit
the vectors in `test/fixtures/leaf-abi.json`, which
`leaf/tests/ts_abi_fixture.rs` regenerates and pins against production
`LeafEvent::to_json`.

## Closing a node ends the iterators it handed out

**Iterators returned by a direct `BrowserNode` now complete when
`close()` is called.** A consumer sitting in
`for await (const bytes of stream)` leaves the loop; one awaiting
`iterator.next()` resolves `{ value: undefined, done: true }`;
`onMessage` listeners are dropped. Opening a stream on a node that is
already closed is a typed `SessionError`
(`kind: 'session'`, message
`session: the node is closed: it no longer holds this origin's identity`)
— the leaf's own fence, not a dead handle and not a generic throw.

```ts
const stream = node.openStream({ reliability: 'reliable' });
(async () => {
  for await (const bytes of stream) render(bytes);
  // Reached on node.close(). Before this change, never.
})();
node.close();
```

**This is a behaviour change.** Code written against the old
behaviour — an iterator that outlived its node, or a `for await` loop
expected to park indefinitely and be torn down some other way — now
sees the loop finish. Nothing new is thrown at the consumer: the
terminal is the normal end of iteration, so a loop that already
handles "the stream ended" needs no change, while a loop whose only
exit was an `AbortController` can drop it.

**The symmetry, and which side moved.** `MeshSession` — the
leader-proxied surface — has always ended its streams when the
generation moved or leadership was lost: a stream is session-scoped,
Rust fences a stale handle by the generation it was opened under, and
a consumer parked on a handle that will never emit again has to be
settled by *something*. The direct surface was the outlier. It closed
the wasm node and left its iterators parked forever, because the wasm
side simply stops calling `on_message` and nothing else can end the
queue. **The direct path moved to match the proxied one**; the
proxied path is unchanged, and both now dispose of a stream the same
way.

Order matters inside `close()` and is part of the contract: the
streams are retired **before** the node, because the leaf retires a
stream handle *through* the node — a handle closed after the node is
never retired at all. `tests/abi_real_package.mjs` asserts the
terminal, the retirement order and the typed refusal against the
built `dist/`, with the refusal text read out of
`leaf/src/wasm.rs` rather than restated in the test.

## Identity and the trust boundary

By default the identity is generated inside the wasm leaf from the
platform CSPRNG: an Ed25519 `EntityKeypair` (which `node_id` and
`origin_hash` are derived from) plus a Noise X25519 static key.

**Custodial injection** hands both halves in instead, as 32 bytes of
hex each:

```typescript
const node = await connect({ credentialB64, entitySecretHex, noiseSecretHex });
```

Two pages given the same pair are the same node id — which is how a
deployment holds the key elsewhere, and how a test forces two tabs
onto one identity before §8's Web Lock election exists.
`noiseSecretHex` is read only when `entitySecretHex` is also present,
so supplying it alone is rejected rather than half-honoured: letting
the entity half be generated would give two tabs one Noise key and two
identities.

**An unusable identity option fails loudly.** A secret that is not 64
hex digits, or a `noiseSecretHex` without its entity half, rejects with
`IdentityError` (kind `identity`) *before* the leaf is called. It never
falls through to a generated identity — a silent fallback there reads
as "the leaf ignores the option", which is two tabs disagreeing about
who they are with nothing in either log saying why.

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
