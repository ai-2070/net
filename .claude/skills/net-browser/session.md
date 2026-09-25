# Session — connecting, staying connected, and moving bytes

## The two entry points

```typescript
import { connect, openSession } from '@net-mesh/browser';

// A real page: the ORIGIN's node, whoever is running it.
const session = await openSession({
  credentialB64,
  capabilities: ['transcribe'],   // re-announced if this tab becomes leader
  subscriptions: ['jobs'],        // re-subscribed if this tab becomes leader
});

// A harness, or a page that is deliberately the one node: THIS TAB's node.
const node = await connect({ credentialB64, bootstrapUrl: 'https://anchor.example/rtc/bootstrap' });
```

| | `connect()` | `openSession()` |
|---|---|---|
| Gives you | this tab's node | the origin's node, whichever tab runs it |
| Called twice in one origin | two nodes contending for one identity | the same node |
| Tab closes | the node is gone | a follower is promoted and re-bootstraps |
| Three promise-valued methods | — | `counters()`, `isEnrolled()`, `openStream()` |
| Reach for it when | a harness, a demo, one-node page | any real page |

`session.role()` reads `'leader' | 'follower'`; `session.generation()` is the
fence value that moves on promotion; `session.onLifecycle(handler)` delivers
`leader_changed`, `leader_lost`, `generation_fenced`, `subscription_restored`
and `not_leader` events. `session.nodeIdHex()`, `session.fingerprint()` and
`session.scope()` identify the node; `session.interruptionMs()` reports the
observed gap across a handoff.

A stale tab's operation fails as `not-leader` **by name** rather than silently
doing nothing — that is the point of the typed surface.

## The anchor and the bootstrap credential

```typescript
const node = await connect({
  credentialB64,                                    // minted by the anchor
  bootstrapUrl: 'https://anchor.example/rtc/bootstrap',
});
```

`credentialB64` is issued by the anchor (`net-mesh anchor credential mint`,
`net-mesh anchor credential inspect`) and is **signed and secret-bearing**; the
leaf's job is to present it unmodified, and the anchor verifies the issuer
signature. The anchor's live verbs (`ls`, `stats`, `serve`) need the CLI's
`rtc-bootstrap` feature, and so does the bootstrap listener a page connects to —
standard CLI packaging does not include them.

**ICE configuration.** `iceServers` is optional with a working default: omitted,
the leaf gathers against the `stun_addr` the anchor announces on `GET
/rtc/anchor`. An entry naming *this connection's own peer* — i.e. the anchor's
`rtc_addr` — is refused with `IceServerConflictError` **before** any ICE work,
because a peer cannot be its own STUN server.

**Admission is not carriage.** Three distinct outcomes a page must not blur:

| Situation | What you get |
|---|---|
| The anchor answered and refused enrollment (replay, expired invite, over §12's bound) | `identity` |
| The offer/trickle/announcement-publish/signal envelope did not get there | `control-plane` |
| The anchor never answered at all | `rpc-timeout` |

`node.isEnrolled()` is the cheap state check between them, and `enroll()` is
explicit for harnesses (`connect()` already enrolls).

**Signalling against an anchor is refused, always.** `signal()` addresses §9's
session-independent signalling envelope; an anchor's control plane does not carry
signalling envelopes, so it rejects with `control-plane`. The carrier that does
carry them is the anchorless one, and `signal()` is how a page reaches it once
attached.

## Leaf ↔ leaf: peer sessions

Two leaves find each other through a capability query and then establish a
**direct** session:

```typescript
await node.announce([PEER_TAG]);
const peers = await node.query(PEER_TAG);          // descriptors, not addresses
const outcome = await node.connectPeer(peerIdHex(peers[0].nodeId));  // or acceptPeer on the other side
```

Rules that bite:

- **A `NodeDescriptor.nodeId` is an exact decimal string**; `connectPeer` /
  `acceptPeer` / `openStream({ peer })` want **16 hex digits**. Use `peerIdHex`
  to convert. Handing the decimal straight back names a *different* peer for a
  16-digit decimal — a cross-peer defect wearing the shape of a formatting bug.
- **`connectPeer` is the drive loop plus one offer**; `acceptPeer` is its
  counterpart. Both run the same loop whether the tab is the leader or a follower,
  so a proxied attempt and a direct one cannot disagree about what supersession
  or a terminal reading looks like. `peerAttempt(hex)` reads the attempt's status
  without driving it, and `handshakePeer(hex, dialog)` completes a dialog you
  already hold.
- **ICE may fail.** `outcome` is a reading, not a promise of connectivity; see
  `errors.md` for what an ICE failure does and does not prove, and `refineIceFailure`
  for turning a raw failure into a classified one.
- **A relayed session is a session.** `openStream({ peer })` refuses a peer the
  node has **no session** with (`session: no session with 0x…`), and sessions are
  installed by an attempt — not by discovery. Discovery alone is not enough to
  send bytes.

## Streams

```typescript
const stream = node.openStream({ reliability: 'fireAndForget', peer: peerHex, streamId });
await stream.send(frame);
stream.onMessage((bytes) => render(bytes));
for await (const bytes of stream) consume(bytes);   // ends when the node or stream closes
```

- **`reliability` is required, not defaulted.** At the wasm boundary an absent
  key and a *misspelled* one are indistinguishable, so `reliabilty:
  'fireAndForget'` would silently yield a reliable stream. As a required field it
  is a compile error on an object literal.
- **A stream is identified by `(peer, id)`, not by its id.** The id is an
  application label scoped to a session: two peer-addressed streams opened under
  one label on one leaf share an id, and each receives the other's bytes with no
  error. Vary the `label` (or the `streamId`) per peer.
- **`label`, not `streamId`, when you want the id derived.** The leaf derives a
  `u64` id from the label and sets a discriminator bit so an unsolicited arrival
  classifies as stream data at the far end. Both ends derive the same id without
  exchanging one — which is exactly what the store relies on.
- **A stream handle is fenced to the session incarnation it was opened on.** When
  a routed pair upgrades to direct, `send` on the old handle rejects with
  `session` ("stale stream handle … reopen the stream"). Reopen with the same
  `peer` and `streamId` rather than assuming continuity.
- **Size limits.** A leaf fragments up to 8 pieces / 64 832 B and refuses above
  that with a typed `wire` error naming streams. A peer that does not advertise
  fragment reassembly is refused at the ordinary event bound.
- **`close()` ends the iterators it handed out** on both surfaces: a `for await`
  loop leaves, `next()` resolves `done: true`, `onMessage` listeners drop.
  Opening a stream on a closed node is a typed `session` error. Teardown order is
  part of the contract (streams retire before the node), and if anything throws
  during teardown, `close()` throws an `AggregateError` whose `.errors` are the
  typed errors in teardown order.

## Events

```typescript
node.on('channel_message', (event) => render(event.payload));
node.onEvent((event) => log(event.type));            // every event
for await (const event of node.events()) { /* … */ }
```

- Tag values stay **verbatim** (`channel_message`, not `channelMessage`) because
  they are the leaf's protocol vocabulary and appear in Rust and browser logs
  alike; field names are camelCased because they are property accesses.
- `connected`, `disconnected`, `channel_message`, `stream_data`, `announcement`,
  `signal`, `rpc_response`, `dropped`, `rtc_failure` are typed. An unknown tag
  arrives as `{ type: 'unknown', tag, raw }` rather than being dropped, so a
  newer leaf never goes silent against an older page.
- **64-bit ids are exact decimal strings**, never numbers: `JSON.parse` rounds
  integers above 2⁵³, which would silently mis-route a page filtering a
  `channel_message` by hash. Byte payloads arrive decoded as `Uint8Array`.
- A throwing listener is reported to the console and skipped — it neither takes
  down its siblings nor unwinds into the wasm frame that called it.

## Identity and the origin trust boundary

By default the leaf generates the identity from the platform CSPRNG. **Custodial
injection** hands both halves in as 32 bytes of hex each:

```typescript
const node = await connect({ credentialB64, entitySecretHex, noiseSecretHex });
```

Two pages given the same pair are the same node id — how a deployment holds the
key elsewhere, and how a test forces two tabs onto one identity.
`noiseSecretHex` is read only when `entitySecretHex` is present; supplying it
alone is **rejected** rather than half-honoured (which would give two tabs one
Noise key and two identities). A secret that is not 64 hex digits, or a Noise
half without its entity half, rejects with `identity` before the leaf is called —
it never falls back to a generated identity.

The **origin is the trust boundary** (see `concepts.md` § 8). Persisting the
identity belongs to the leaf; non-extractable protects the key from
exfiltration, not from a compromised page on the same origin.

## Build it

Two builds, in order — the package consumes the leaf's wasm-bindgen output:

```sh
cd net/crates/net/leaf
cargo build --release --target wasm32-unknown-unknown
wasm-bindgen --target web --out-dir pkg target/wasm32-unknown-unknown/release/net_leaf.wasm

cd ../browser-ts
npm install && npm run build      # tsc + the single-file bundle, copying the leaf pkg/ beside it
```

`wasm-bindgen-cli` must be **0.2.129** — the version the leaf pins; a mismatch is
a hard error at bindgen time rather than a subtle one at run time.

**Loading.** Either serve the build output directory and `import { connect } from
'/browser/index.js'` (no bundler involved — the entry imports its siblings with
explicit `.js` specifiers), or map the single file `dist/index.bundle.js`. Either
way the wasm is fetched **relative to the entry point**, so the wasm-bindgen
output must sit beside it; override the lookup with `connect({ wasmUrl })`,
`connect({ wasm })` (an already-imported module) or `connect({ wasmModule })`.

**One command does all of it and opens a running demo:**
`net/crates/net/examples/browser-demo/run.sh` (`run.ps1` on Windows) — two tabs,
one anchor, a direct 60 Hz leaf ↔ leaf stream, and a third tab that keeps the
anchor's signalling counter moving so the pair's flat counter means something.
`--check` runs it headless and asserts five claims on the anchor itself.

**Dev loop.** `npm test` runs vitest against a fake wasm module satisfying the
declared boundary — that catches Rust/TypeScript drift at compile time, not
behaviour. The real artifact is exercised by the Playwright matrix in
`net/crates/net/tests/rtc_browser/`, which loads the built bundle and the
wasm-bindgen output beside it.
