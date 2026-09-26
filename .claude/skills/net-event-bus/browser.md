# The browser: a tab as a Net node

Read this page when the integration target is a **page** — a browser tab that
must publish/subscribe, call nRPC services, announce capabilities, open streams,
or host/join a networked store. The tab is a full mesh node: its own Ed25519
identity, its own Noise session, its own capability announcements.

Load `concepts.md` first like any other chapter — the mental model is the same.
What changes here is the transport underneath (a WebRTC DataChannel to a native
**anchor**, not UDP) and the package you import.

## It is a sibling package, not `@net-mesh/sdk`

| | `@net-mesh/sdk` | `@net-mesh/browser` |
|---|---|---|
| Runtime | Node (napi `@net-mesh/core`) | a page (wasm `net-mesh-leaf`) |
| Transport | UDP mesh | WebRTC DataChannel to an anchor |
| Entry | `new NetNode(…)` / SDK wrappers | `connect(…)` / `openSession(…)` |

`@net-mesh/sdk` is built on `@net-mesh/core`, the napi native binding, and every
one of its entry points resolves it — a browser bundle that walked that graph
would either fail on an unresolvable `.node` file or quietly ship a shim for
one. That is why the browser surface is its own package with its own dependency
graph: **nothing in a page's build resolves `@net-mesh/core` at all.** The two
packages version independently — the leaf's wasm ABI moves with the Rust crate,
the SDK's with napi.

The Rust half is `net-mesh-leaf` (`net/crates/net/leaf`), compiled to
`wasm32-unknown-unknown`; `@net-mesh/browser` is the typed surface over it. A
page imports the package, never `#[wasm_bindgen]` methods directly.

**Not on a registry yet.** `@net-mesh/browser` is built from the repository
(`net/crates/net/browser-ts`), which is why `examples/` has no browser route and
why the build recipe below starts with the wasm.

## Two entry points — and the one to pick

There is **one node per origin**. Tabs contend for a Web Lock; the holder runs
the node on the main thread (`RTCPeerConnection` does not exist in a worker) and
every other tab attaches as a **follower** and drives the same node through it.

| | `connect()` | `openSession()` |
|---|---|---|
| Gives you | **this tab's** node | **the origin's** node, whichever tab runs it |
| Two calls in one origin | two nodes contending for one identity | the same node |
| Tab closes | the node is gone | a follower is promoted and re-bootstraps under the same identity |
| Reach for it when | a harness, a demo, a page deliberately the one node | any real page |

**Use `openSession` unless you know you want otherwise.** Same capability
surface, with three methods promoted to promises because on a follower the work
happens in another tab — `counters()`, `isEnrolled()`, `openStream()`. A stale
tab's operation fails as `NotLeaderError` rather than silently doing nothing.
`session.role()` reads `'leader' | 'follower'`; `session.generation()` and
`session.onLifecycle(…)` (`leader_changed`, `leader_lost`, …) are how a page
learns the node moved.

```typescript
import { openSession } from '@net-mesh/browser';

const session = await openSession({
  credentialB64, capabilities: ['transcribe'], subscriptions: ['jobs'],
});
await session.subscribe('jobs');            // re-installed by a promoted follower
await session.announce(['transcribe']);
const reply = await session.call('summarise', bytes, 5_000);
```

## Connecting: the anchor and the bootstrap credential

```typescript
import { connect } from '@net-mesh/browser';

const node = await connect({
  credentialB64,                                    // minted by the anchor
  bootstrapUrl: 'https://anchor.example/rtc/bootstrap',
});
```

The credential is issued by the anchor (`net-mesh anchor credential mint` /
`inspect`; the CLI's live `anchor` verbs need the `rtc-bootstrap` feature — see
`cli.md`). It is **signed and secret-bearing**, and the leaf's job is to present
it unmodified: the anchor verifies the issuer signature.

- **`iceServers` is optional with a working default.** Omitted, the leaf gathers
  against the `stun_addr` the anchor announces on `GET /rtc/anchor`. An entry
  naming *this connection's own peer* is refused with `IceServerConflictError`
  before any ICE work — `rtc_addr` is the ICE peer, and a peer cannot be its own
  STUN server.
- **The control plane does not carry signalling.** Against an anchor, `signal()`
  is refused unconditionally (`LeafError::ControlPlane`); the carrier that does
  carry signalling envelopes is the anchorless one.
- **Admission is not carriage.** An enrollment refusal is `identity` (the anchor
  answered and said no: replay, expired invite, over-bound). `control-plane` is
  carriage. An anchor that never answered is neither — it is `rpc-timeout`.
  `node.isEnrolled()` distinguishes the provisional state.

## The node surface

| Call | Notes |
|---|---|
| `subscribe(channel)` / `announce(capabilities)` | the same channel/capability surfaces as every other binding |
| `query(capability)` | discovery — `NodeDescriptor[]` |
| `call(service, bytes, timeoutMs?)` | typed nRPC over the session |
| `openStream(options)` | see Streams below |
| `on(tag, fn)` / `onEvent(fn)` / `events()` | the event union, its parser and the fan-out hub |
| `connectPeer(hex)` / `acceptPeer(hex)` | establish a **leaf ↔ leaf** session |
| `counters()` / `isEnrolled()` / `enroll()` | leaf counters (u64s as exact strings), admission state, explicit enroll |
| `anchorIdHex()` / `signal(…)` | the anchor this leaf bootstrapped to; §9's signalling envelope (refused against an anchor) |
| `close()` | see Streams — it ends the iterators it handed out |

**Ids have two spellings, and mixing them is the classic bug.**
`NodeDescriptor.nodeId` is an **exact decimal string** (`JSON.parse` rounds
integers above 2⁵³, so the wrapper never hands a page a number), while the peer
verbs take **16 hex digits** — convert with `peerIdHex`. Stream events carry
`stream_id` in decimal and the handle exposes it in hex; they are the same u64.

## Streams

```typescript
const stream = node.openStream({ reliability: 'fireAndForget', peer: peerHex, streamId });
await stream.send(frame);
stream.onMessage((bytes) => render(bytes));
for await (const bytes of stream) consume(bytes);   // ends on node.close()
```

- **`reliability` is required, not defaulted.** The wasm boundary cannot tell an
  absent key from a *misspelled* one, so `reliabilty: 'fireAndForget'` would
  silently yield a reliable stream. As a required field of `OpenStreamOptions`, a
  typo is a compile error on an object literal.
- **A stream is identified by `(peer, id)`, not by its id.** The id is an
  application label scoped to a session, so two peer-addressed streams opened
  under one label on one leaf share an id and each receives the other's bytes
  with no error — vary the `label` or `streamId` per peer.
- **A peer-addressed stream works from a follower too.** A follower's stream is
  opened by the leader tab's node, and the request **carries the peer**, so a
  follower addresses a direct leaf ↔ leaf session exactly as the leader tab does
  (this was refused by name before the proxy request gained the field). What a
  page must handle instead: a stream handle is fenced to the session incarnation
  it was opened on, so when a routed pair upgrades to direct, `send` rejects with
  `session` ("stale stream handle … reopen the stream") — reopen with the same
  `peer` and `streamId` rather than assuming continuity.
- **`close()` ends the iterators it handed out** (direct and proxied alike): a
  `for await` loop leaves, `iterator.next()` resolves `done: true`, listeners
  drop. Opening a stream on a closed node is a typed `SessionError`. Teardown
  order is part of the contract — streams retire before the node — and if
  anything throws, `close()` throws an `AggregateError` of the typed errors in
  teardown order.

## Events

`node.on(tag, fn)` for one tag, `node.onEvent(fn)` for all, `node.events()` for
an async iterable. Tag values stay verbatim (`channel_message`, not
`channelMessage`) because they are the leaf's protocol vocabulary. An unknown
tag arrives as `{ type: 'unknown', tag, raw }` rather than being dropped, so a
newer leaf never goes silent against an older page. Byte payloads are decoded
for you (`Uint8Array`); 64-bit ids are exact decimal strings. A throwing
listener is reported to the console and skipped — it neither takes down its
siblings nor unwinds into the wasm frame that called it.

## Errors: what a page branches on

Everything rejects with a `LeafError` subclass whose `.kind` is a flat, stable
discriminant; `.message` is verbatim the Rust `Display` text. An unrecognised
message becomes `UnknownLeafError` rather than being folded into a near
neighbour — mis-typing a failure is the mistake this design exists to prevent.

| `.kind` | class | Rust variant |
|---|---|---|
| `wire` / `session` / `control-plane` / `identity` | `WireError` / `SessionError` / `ControlPlaneError` / `IdentityError` | `LeafError::{Wire,Session,ControlPlane,Identity}` |
| `not-leader` | `NotLeaderError` | `LeafError::NotLeader` |
| `ice-timeout` / `udp-blocked` / `channel-closed` / `rtc-unsupported` | `RtcError` | `RtcError::{IceTimeout,UdpBlocked,ChannelClosed,Unsupported}` |
| `rpc-refused` / `rpc-timeout` / `session-lost` / `leader-lost` / `rpc-indeterminate` / `rpc-malformed` | `RpcError` | `RpcError::{Refused,Timeout,SessionLost,LeaderLost,Indeterminate,Malformed}` |
| `ice-server-conflict` | `IceServerConflictError` | `LeafError::IceServerConflictsWithPeer` |

**An ICE timeout is not evidence that UDP is blocked.** An anchor that is down,
misconfigured or saturated produces the same symptom. Only two observations
together narrow it: the HTTPS bootstrap to *that anchor* succeeded **and** a
STUN binding to the `rtc_addr` *that same anchor published* went unanswered.
`classifyRtcFailure(observations)` is a pure function of those two facts and the
only path to `udp-blocked`; `probeStunBinding(addr)` produces the second
observation, `probeBootstrapReachable()` the first (before any `connected`
event), and `udpBlockedEvidence()` returns `null` unless both hold and the
address is named. Host candidates are ignored (they are gathered whatever the
network does), and a STUN **error** response still proves reachability — that is
why the rule is a code threshold, not a list. The probe needs a subject: the
`connected` event's `rtc_addr`, or `connect({ anchorRtcAddr })`.

**`rpc-indeterminate` is not a timeout, and must not be retried.** On a session,
a call can fail because the tab running the node was frozen by the browser: a
frozen document's task queues do not run, so nothing is answered — not the
calls, not the control chatter. The follower arms its own deadline over the
proxy round trip and reports `rpc-indeterminate`, which means "the remote may
have executed this". A frozen tab **keeps** its Web Lock, so no successor is
elected and the backlog flushes on resume — those calls may execute *late*.
Retrying can cause the effect twice. Surface it, or wait. (The signal:
`rpc-indeterminate` **and** `role() === 'follower'` **and** an unchanged
`generation()` **and** no reply to anything.)

## The networked store (and `./three`)

For a page with a world rather than a request: one node **hosts** an
authoritative document, others **join** replicas of it.

```typescript
import { defineStore, hostStore, joinStore } from '@net-mesh/browser';
import { bindEntities } from '@net-mesh/browser/three';

const host = hostStore({
  definition, store: 'world', transport: node, initialState,
  authorize: (request) => request.audience.every((a) => a !== 'command'),
  project: (state, audience) => (audience.includes('command') ? state : publicPart(state)),
  actions, inputs,
});
const replica = joinStore({ definition, store: 'world', transport: node, host: hostNodeIdHex, audience: ['crew'], key: 'player' });
await replica.ready();
```

- **The host is the authority.** An `act` executes there and its result comes
  back correlated to the request; an `input` is coalesced and unacknowledged;
  `project` decides what each audience may see, and a replica cannot read what it
  was not given. `authorize` is re-consulted on the ongoing delta feed, not just
  at join.
- **`store` names which store of a definition this is**, and a joiner asks for it
  by that name. Name them when one node hosts several, or the joins are
  ambiguous and the wrong store answers.
- **Branch on `StoreError.code`** — the wire carries exactly this set:
  `invalid-data`, `version-mismatch`, `forbidden`, `not-ready`, `capacity`,
  `timeout`, `aborted`, `indeterminate`, `owner-lost`, `closed`,
  `action-rejected`, `result-expired`. `result-expired` asserts nothing about
  whether the original attempt committed: a retired sequence may have been
  rejected, aborted before commit, or fenced without ever executing — never read
  it as a success receipt. `owner-lost` is terminal (the owner incarnation ended
  and a closing host says so unsolicited) and is deliberately **not** `closed`
  (the expiry notice a replica rejoins on) — rejoining an owner that is gone
  either hangs or attaches you to a successor's different document.
- **A joiner needs a session with the host**, which is what
  `StoreTransport.connectPeer` is for: `connect()`'s node has it, `openSession()`'s
  `MeshSession` does not. A host and a joiner in **one tab** are exercised by
  this package's tests; two tabs sharing a leader are not — the store's own use
  of a proxied handle on a follower (last-consumer cleanup, the peer/stream
  lifecycle across a leader change) is recorded as not established in
  `net/crates/net/browser-ts/src/store/index.ts`, so do not present it as proven.
- **`@net-mesh/browser/three`** turns a store's entity map into a scene graph via
  `bindEntities({ store, scene, select, binding })`. It imports nothing from
  `three` — the scene graph is anything with `add` and `remove`, the types are
  structural — so the package gains no renderer dependency. An entity whose
  reference did not change is not touched.

## Identity and the trust boundary

By default the identity is generated inside the wasm leaf from the platform
CSPRNG: an Ed25519 `EntityKeypair` plus a Noise X25519 static key. **Custodial
injection** hands both in instead (`connect({ entitySecretHex, noiseSecretHex })`)
— two pages given the same pair are the same node id. `noiseSecretHex` is read
only when `entitySecretHex` is present, and an unusable option (a secret that is
not 64 hex digits) rejects with `IdentityError` **before** the leaf is called
rather than falling back to a generated identity.

**The origin is the trust boundary.** The leaf persists the identity in
IndexedDB under a non-extractable WebCrypto AES-GCM key. Non-extractable means
script cannot read the *wrapping* key — but any script running on this origin
can ask the browser to use it. Storage protects the key from exfiltration, not
from a page that is already compromised; a deployment that needs the key held
elsewhere must inject it custodially.

## Build and run it

```sh
cd net/crates/net/leaf
cargo build --release --target wasm32-unknown-unknown
wasm-bindgen --target web --out-dir pkg target/wasm32-unknown-unknown/release/net_leaf.wasm

cd ../browser-ts
npm install && npm run build     # tsc + single-file bundle + copies the leaf pkg/
```

`wasm-bindgen-cli` must be **0.2.129** — the version the leaf pins; a mismatch is
a hard error at bindgen time. The wasm is fetched relative to the entry point, so
the wasm-bindgen output directory has to sit beside it (override with
`connect({ wasmUrl })`, `connect({ wasm })`, or `connect({ wasmModule })`).

The one-command path does all of the above and opens a running demo:
`net/crates/net/examples/browser-demo/run.sh` (`run.ps1` on Windows) — two tabs,
one anchor, a direct 60 Hz leaf ↔ leaf stream, and a third tab that keeps the
anchor's signalling counter moving. `--check` runs it headless and asserts the
five claims on the anchor rather than on the HUD.

## What this page is not evidence for

- **Native ↔ native WebRTC is not the path.** For two native nodes UDP plus the
  existing punch remains the transport; WebRTC's value is browser reach.
- **The package is unpublished** and the two-tab leader-proxy store case is not
  established (see above). Do not present either as shipped-and-proven.
- **A browser node cannot do everything a native node can** — the leaf is a
  deliberate subset of the Rust surface, and `udp-blocked` networks are an
  explicit, classified limitation rather than something the transport routes
  around.

## Where to look when this page is not enough

- **Authoritative source:** `net/crates/net/browser-ts/src/` — the page surface,
  the leader session, the stream wrapper, the event union, the typed errors, the
  UDP probe, the store and the Three.js binding — over
  `net/crates/net/leaf/src/`, the wasm half.
- **The package's design notes:** `net/crates/net/browser-ts/DESIGN.md` — the
  measured details behind every rule above (the ICE classification table, the
  freeze measurement, the stream-identity reasoning, the size table).
  `README.md` beside it is the user-facing quick start.
- **Worked page:** `net/crates/net/examples/browser-demo/page/demo.js`.
- **Docs:** [WebRTC transport](https://ai2070.net/docs/concepts/webrtc-transport)
  and [Browser SDK](https://ai2070.net/docs/sdk/browser).
