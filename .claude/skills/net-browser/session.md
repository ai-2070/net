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
const node = await connect({ credentialB64, bootstrapUrl: 'https://anchor.ai2070.net' });
```

| | `connect()` | `openSession()` |
|---|---|---|
| Gives you | this tab's node | the origin's node, whichever tab runs it |
| Called twice in one origin | two nodes contending for one identity | the same node |
| Tab closes | the node is gone | a follower is promoted and re-bootstraps |
| Three promise-valued methods | — | `counters()`, `isEnrolled()`, `openStream()` |
| Only on this surface | `peerAttempt`, `handshakePeer`, `refineIceFailure`, `rtcStats`, `retryReport`, `enableNetworkRetry`, `anchorIdHex`, `originHashHex` | `role`, `generation`, `fingerprint`, `scope`, `interruptionMs`, `onLifecycle`, `unsubscribe` |
| `nodeIdHex()` | `string` | `string \| null` |
| Reach for it when | a harness, a demo, one-node page, **the game store** | any real page (but a store over it is not established) |

`session.role()` reads `'leader' | 'follower'`; `session.generation()` is the
fence value that moves on promotion; `session.onLifecycle(handler)` delivers
`leader_changed`, `leader_lost`, `generation_fenced`, `subscription_restored`,
`not_leader` and `promotion_failed` events. `promotion_failed` means this tab
won the lock but failed to re-bootstrap the node: it is a follower again and
will ask for the lock again shortly. `session.nodeIdHex()`,
`session.fingerprint()` and `session.scope()` identify the node;
`session.interruptionMs()` reports the observed gap across a handoff.

A stale tab's operation fails as `not-leader` **by name** rather than silently
doing nothing — that is the point of the typed surface.

## The anchor and the bootstrap credential

```typescript
const node = await connect({
  credentialB64,                                    // minted by the anchor
  bootstrapUrl: 'https://anchor.ai2070.net',         // optional; the credential carries it
});
```

`bootstrapUrl` is the anchor's **base URL**, not an endpoint: the leaf appends
`/rtc/anchor`, `/rtc/offer` and `/rtc/trickle` itself. A URL that already ends
in a path (`…/rtc/bootstrap`) points every request at the wrong place.

`credentialB64` is issued by the anchor (`net-mesh anchor credential mint`,
`net-mesh anchor credential inspect`) and is **signed and secret-bearing**; the
leaf's job is to present it unmodified, and the anchor verifies the issuer
signature. `credential mint` / `inspect` need no extra feature; the anchor's
live verbs (`ls`, `stats`, `serve`) need the CLI's `rtc-bootstrap` feature —
standard CLI packaging does not include them.

**A game's anchor issues credentials itself.** With `--game` (and the issuer's
key file, `--issuer-identity`), `net-mesh anchor serve` serves enrollment and
`POST <url>/credential {"game"}`:

```sh
net-mesh anchor serve --psk-file psk.hex \
  --url https://anchor.example.com --tls-cert cert.pem --tls-key key.pem \
  --allow-origin https://game.example.com \
  --issuer-identity issuer.toml --game my-game
```

A page calls `requestCredential({ anchorUrl, game })` and passes the result to
`connect()` with `...rememberedIdentity()`. Each credential is anonymous and
binds to the first identity that enrolls with it (which may reconnect with it
for 12 h); another identity presenting it is refused as a replay, surfacing
from `connect()` as `identity: the anchor rejected enrollment: replay`.
Without `--game` the anchor registers no enrollment service and `connect()`
times out with `rpc-timeout`. Several games on one anchor are kept apart: the
anchor records which game admitted each session and neither floods, replays
nor relays between games, so a player never discovers another game's lobbies.
`examples/anchor-acceptance` runs the whole flow with real browsers, a rival
game included.

**`rememberedIdentity()` or a fresh identity per load.** Remembering keeps the
player the same across visits, but a reloaded page then comes back as the node
it was, while the host may still hold that node's old session. A game whose
players needn't persist can leave `rememberedIdentity()` out: each load is a new
player, and a reload never collides with itself (Rose & Blade does this). Two
tabs that each call `connect()` without it are two separate nodes.

**The public anchor, and overriding it.** Unless you need your own, use
`https://anchor.ai2070.net` (`--open-games`: any game id, keyed on the page's
origin). Read an override from the page URL so the same build can run against
a local anchor:

```typescript
const anchorUrl = new URLSearchParams(location.search).get('anchor') ?? 'https://anchor.ai2070.net';
```

**A local anchor for development.** The full set of flags a dev script needs,
from Rose & Blade's dev script:

```sh
net-mesh --output ndjson anchor serve \
  --bind 127.0.0.1:0 --psk-file psk.hex \
  --listen 127.0.0.1:8444 --url https://localhost:8444 \
  --rtc-bind 127.0.0.1:0 \
  --tls-cert cert.pem --tls-key key.pem \
  --issuer-identity issuer.toml --insecure-permissions \
  --game my-game --allow-origin https://localhost:8443 \
  --game-stats-secs 5     # optional: per-game counters on stdout
```

- `psk.hex` is 32 random bytes as hex; `issuer.toml` comes from `net-mesh
  identity generate --out issuer.toml`. Keep both private.
- The certificate comes from **mkcert** (`mkcert -install` once, then `mkcert
  -cert-file cert.pem -key-file key.pem localhost 127.0.0.1`), so the browser
  trusts it. `localhost` counts as a secure context, so the page itself may be
  served over plain HTTP on one machine; across a LAN both page and anchor
  need HTTPS, and every machine must trust your mkcert CA.
- `--insecure-permissions` accepts key files with loose permissions (Windows,
  a checked-out dev folder). Never on a real anchor.
- With `--output ndjson`, wait for the line carrying `credential_endpoint`
  before opening the page: the anchor is ready then.
- The binary needs the `rtc-bootstrap` feature: download the prebuilt
  `net-mesh-anchor-v<version>-<platform>` archive from the GitHub release, or
  `cargo install net-cli --features rtc-bootstrap`.

**ICE configuration.** `iceServers` is optional with a working default: omitted,
the leaf gathers against the `stun_addr` the anchor announces on `GET
/rtc/anchor`. An entry naming *this connection's own peer* — i.e. the anchor's
`rtc_addr` — is refused with `IceServerConflictError` **before** any ICE work,
because a peer cannot be its own STUN server.

**Admission is not carriage.** Distinct outcomes a page must not blur:

| Situation | What you get |
|---|---|
| The anchor answered and refused enrollment (replay, expired invite, over §12's bound) | `identity` |
| The offer/trickle/announcement-publish/signal envelope did not get there | `control-plane` |
| The anchor never answered at all | `rpc-timeout` |
| `rpc-refused` with status 1 (NotFound) or 2 (Unauthorized) and a message containing "refused this caller's reply subscription" | the provider refused the reply plane — the anchor answered; it is not a slow provider |

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
  or a terminal reading looks like. On `connect()`'s `BrowserNode` only,
  `peerAttempt(hex)` reads the attempt's status without driving it, and
  `handshakePeer(hex, dialog)` completes a dialog you already hold; a
  `MeshSession` has `connectPeer` / `acceptPeer` but neither of those.
- **`connectPeer` on an already-direct, open pair is a no-op** that resolves
  `direct`, on `connect()`'s node and on a `MeshSession` alike, so calling it
  "to be sure" is safe. (A follower whose leader is from an older release
  still re-offers.) Nor does it cancel an attempt still under way: a second
  `connectPeer` for a connecting peer resolves with that attempt's outcome, and
  one during an `acceptPeer` waits for it and takes its outcome when that
  settles the pair (`direct`, `iceTimeout`, `udpBlocked`, or `superseded` by a
  newer attempt), offering only after an inconclusive one. Every offer replaces
  the pair's link, so this is what lets a lobby, a store and netcode each reach
  the same host. This holds per node: another tab's node is outside the rule,
  as it is outside the surface — and so is **the other page**. See the next
  bullet.
- **Two pages offering to each other at once is glare.** The idempotency above
  stops a node cancelling *its own* attempt; nothing stops page A's offer
  cancelling page B's. It bites after an attempt **ended** (`iceTimeout`,
  `udpBlocked`, `failed`): if both pages offer again, each new offer cancels the
  other's attempt, both run out their ICE deadline, and the pair stays relayed
  for good. Give the pair one offerer: when the last attempt ended, only the page
  whose node id is the **lower** (`myId < peerId`, hex strings compare
  correctly) offers again, and the other answers. For a first contact, let the
  page new to the group reach out and have the others wait a few seconds
  before reaching it (Rose & Blade waits 3 s).
- **Answer by state, not by memory.** The answering page calls `acceptPeer(id)`
  whenever `peerAttempt(id)` shows the pair is neither direct nor connecting,
  whoever offered before. With no offer waiting, `acceptPeer` gives up after a
  few seconds and changes nothing, so polling it (e.g. once a second, per peer
  you have a stream with) is safe. Gating it on sticky flags ("we offered once",
  "it was direct once") leaves every later offer unanswered: each runs out its
  ICE deadline. Read `peerAttempt` as:

  | `peerAttempt(id)` | The pair is | Offer? Answer? |
  |---|---|---|
  | `direct: true` | direct | neither |
  | `state` `gathering` or `open`, not direct | connecting | neither: a new offer cancels it |
  | `state` `iceTimeout`, `udpBlocked` or `failed` | ended, relayed | lower id offers, higher id answers |
  | throws | never attempted | the page reaching out offers |

- **Don't wait for direct before playing.** The relayed session is up long
  before ICE finishes, and most direct links land after the first second or so.
  Race `connectPeer` against a short timer (Rose & Blade uses 1.5 s), open the
  stream on the relayed session, and record the real outcome when the full
  promise settles. The stream goes stale when the pair upgrades (see *Streams*),
  so reopen it on the next send that fails:

  ```typescript
  const full = node.connectPeer(peer);
  full.then((outcome) => { if (outcome.type === 'direct') markDirect(peer); }).catch(() => {});
  await Promise.race([full, new Promise((r) => setTimeout(r, 1500))]);
  const stream = node.openStream({ reliability: 'fireAndForget', peer, label, lossy: true });
  ```
- **ICE may fail.** `outcome` is a reading, not a promise of connectivity; see
  `errors.md` for what an ICE failure does and does not prove, and
  `refineIceFailure` (a `BrowserNode` method) for turning a raw failure into a
  classified one.
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
- **`lossy: true` rides a second, unordered DataChannel with no retransmits**
  (`openStream({ reliability: 'fireAndForget', peer, label, lossy: true })`).
  A packet arrives promptly or not at all, and none delays anything else: use it
  for positions and inputs, where only the newest value matters. The rules:
  - It is valid **with `'fireAndForget'` only**; with `'reliable'` it throws.
  - It is available on **`connect()` only**; a `MeshSession`'s `openStream`
    refuses it.
  - A packet that would queue behind a backed-up buffer is dropped (and
    counted), never queued.
  - A peer or anchor that opens no lossy channel still gets the packets, on
    the reliable one. The anchor must be the same release, since an older one
    does not know the second channel.

  Netcode (`netcode.md`) is built on it.
- **Size limits.** A leaf fragments up to 8 pieces / 64 832 B and refuses above
  that with a typed `wire` error naming streams. A peer that does not advertise
  fragment reassembly is refused at the ordinary event bound.
- **`close()` ends the iterators it handed out** on both surfaces: a `for await`
  loop leaves, `next()` resolves `done: true`, `onMessage` listeners drop.
  Opening a stream on a closed node is a typed `session` error. Teardown order is
  part of the contract (streams retire before the node). On `connect()`'s
  `BrowserNode`, if anything throws during teardown, `close()` throws an
  `AggregateError` whose `.errors` are the typed errors in teardown order; a
  `MeshSession`'s `close()` ends its streams and org handles but does not
  aggregate.

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

## Staying connected: background tabs

A page left in the background (another tab, another program in front) can lose
its session with the anchor: the browser throttles or freezes it, and the
anchor drops it. The node then neither sees other players' lobbies nor is seen
hosting one, and nothing in the page's own state says so. Check before relying
on the node, and when the page comes back to the front:

- **`disconnected` events** (`node.onEvent`) catch the loss the leaf noticed.
  Record it; don't rebuild from inside the listener.
- **A probe catches the rest.** Call a service nobody serves:
  `node.call('my-game.alive', new Uint8Array(0), 5000)`. The anchor itself
  refuses it, so a rejection with `kind === 'rpc-refused'` **proves the session
  is alive**; a timeout means it is gone. A `query` won't do: the node answers
  it from what it last heard.
- **Re-check on `visibilitychange`** to `visible`, and before showing a lobby
  list or hosting if the last check is older than ~20 s. One check at a time;
  everything that asks waits on it.
- **Lost: close the node and `connect()` a new one**, as a reload would. With a
  fresh identity per load (see *The anchor and the bootstrap credential*) the
  new node never collides with the old session.
- **Not mid-match.** A match keeps its node; a heartbeat (Rose & Blade sends
  one a second from a background tab) keeps the other players from taking the
  page for gone.

```typescript
async function alive(node: BrowserNode): Promise<boolean> {
  return Promise.race([
    node.call('my-game.alive', new Uint8Array(0), 5000).then(
      () => true,
      (error) => error?.kind === 'rpc-refused',
    ),
    new Promise<boolean>((resolve) => setTimeout(() => resolve(false), 5500)),
  ]);
}
document.addEventListener('visibilitychange', async () => {
  if (document.visibilityState === 'visible' && !inMatch && !(await alive(node))) {
    node.close();
    const { credentialB64, bootstrapUrl } = await requestCredential({ anchorUrl, game });
    node = await connect({ credentialB64, bootstrapUrl });
  }
});
```

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
