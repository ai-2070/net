# Changelog

Notable changes to `@net-mesh/browser`.

This file records what a page has to do differently. The full
per-release story for the whole system lives in the crate's release
notes (`net/crates/net/docs/releases/`); this is the subset that
reaches this package's public surface.

`@net-mesh/browser` and `net-mesh-leaf` (the wasm) are released
together and must be upgraded together: the package declares the
wasm-bindgen boundary in `src/wasm.ts`, so a version skew shows up as a
missing method at the call site rather than at install time. Unlike
`@net-mesh/sdk`, this package never depends on `@net-mesh/core` — see
the README on why it is a sibling package rather than a sub-path.

## Unreleased

### Added

- **`hostPlayer(host, { audience })`** — the hosting node's own player,
  with the handle shape `joinStore` returns. A node cannot join its own
  store, so every game whose host also plays wrote a wrapper around the
  handlers; this replaces it and holds the host's player to parity with
  a replica: the host's `authorize` with its own node id as `peer`,
  input and output validation and the result budget inside one
  transaction, the wire's value rules, and `getState()` as the
  projection for its audience rather than the raw document.
  `HostedStoreHandle` now carries its `definition` and its action and
  input types.
- **`projectFor(state, { peer, audience })`** — a per-player
  projection, given to `hostStore` instead of `project`, for views that
  depend on who is looking (your own hand). Computed once per distinct
  player and audience; a player whose view did not change is sent
  nothing. Exactly one of `project` and `projectFor` is accepted, in the
  types (`HostProjection`) and at construction (`invalid-data`).
- **`@net-mesh/browser/local`** — `createLocalMesh()`, several nodes in
  one page with no anchor and no network, each a `StoreTransport`. The
  store on top is the real one; delivery is a function call and the
  peer is assigned rather than proved. Local nodes also `announce` and
  `query` in the real descriptor shape, with the leaf's lease (another
  node's announcement only, expiring after 300 s by default), so
  discovery code runs offline. For building game logic first;
  the demo's `demo/local-mesh.js` is replaced by it.

## Unreleased — targets 0.36.0

### Added

- **The game store.** `defineStore()`, `hostStore()` and
  `joinStore()`: one authoritative document served to replicas over a
  mesh stream. A node may host several stores of one definition, so
  each names itself — `hostStore({store})` declares an address and
  `joinStore({store})` asks for one; both default to the definition
  id. The address rides on the `join` frame, because every host store
  on a node sees every frame and a join names no handle. Two stores
  answering to one name on one transport is refused at construction.
  Each store serves with audience projections, correlated actions,
  coalesced inputs, chunked snapshots and a replica that recovers a
  lost manifest or a stalled assembly by asking again. `StoreError`
  carries the code a caller branches on. See `demo/README.md` for the
  worked example and the vocabulary.
- **`@net-mesh/browser/three`.** `bindEntities()` — a subpath export
  that reconciles a store's entity map into any scene graph with
  `add`/`remove`, skipping entities whose reference did not change and
  removing what left the world. It imports nothing from `three`: the
  types are structural, so the package gains no renderer dependency
  and the binding drives a `THREE.Scene` or a test double equally.

- **The package.** `connect()`, `BrowserNode` (`call`, `subscribe`,
  `publish`, `openStream`, `announce`, `query`, `signal`, `enroll`,
  `isEnrolled`, `anchorIdHex`, `nodeIdHex`, `counters`, `close`),
  `LeafStream`, and a
  typed event surface (`on`, `onEvent`, `events()`) over the
  `net-mesh-leaf` wasm node. `query` resolves parsed descriptors
  (`{ nodeId, entityId, capabilities, rtcAddr, noisePubkey, version }`)
  and `counters()` parsed counters, both with `u64`s as exact strings.

- **`openSession()` and `MeshSession`** — §8's leader/follower surface:
  one node per origin, elected by Web Lock, with every tab driving it
  through the same API. `role()`, `generation()`, `fingerprint()`,
  `scope()`, `interruptionMs()`, `onLifecycle()` and the five lifecycle
  events (`leader_changed`, `subscription_restored`, `leader_lost`,
  `generation_fenced`, `not_leader`) on top of the direct surface's
  operations. `counters()`, `isEnrolled()` and `openStream()` are
  promises here because on a follower the work happens in another tab.
  Prefer it over `connect()`: two tabs calling `connect()` on one origin
  are two nodes contending for one identity.

- **Typed errors mirroring `net_leaf::error`.** Every rejection is a
  `LeafError` subclass whose `.message` is verbatim the Rust `Display`
  text and whose `.kind` is one flat, stable string per Rust variant
  (`wire`, `session`, `control-plane`, `identity`, `not-leader`,
  `ice-timeout`, `udp-blocked`, `channel-closed`, `rtc-unsupported`,
  `rpc-refused`, `rpc-timeout`, `session-lost`, `leader-lost`,
  `rpc-indeterminate`, `rpc-malformed`). An unrecognised message
  becomes `UnknownLeafError`
  rather than being folded into a near neighbour.

- **The `udp-blocked` classification, with its evidence.** An ICE
  failure surfaces as `ice-timeout` unless two observations hold
  together: the anchor answered its HTTPS bootstrap, and a STUN binding
  to the `rtc_addr` that anchor published went unanswered.
  `classifyRtcFailure()` is the pure rule; `probeStunBinding()` and
  `probeBootstrapReachable()` produce the observations. Supply
  `connect({ anchorRtcAddr })` when the page knows the anchor's address
  before its first `connected` event; `connect({ failureTyping: {
  probeOnIceTimeout: false } })` switches the probe off, which means
  `ice-timeout` always.

- **Custodial identity.** `connect({ entitySecretHex, noiseSecretHex })`
  (32 bytes of hex each) makes the leaf build its identity from those
  secrets instead of generating one, so two pages given the same pair
  are the same node id. `noiseSecretHex` is read only alongside
  `entitySecretHex`. An unusable option — a secret that is not 64 hex
  digits, or
  `noiseSecretHex` without its entity half — rejects with
  `IdentityError` before the leaf is called, never falling through to a
  generated identity.

- **`npm run size`** — raw and gzipped bytes for the wasm, the
  wasm-bindgen glue, the ESM directory and the single-file bundle, with
  `SIZE <artifact> raw=<n> gz=<n>` lines for CI, a comparison against
  the S0a baseline, and `--assert` / `--require-leaf` exit codes.

### Changed

- **A stream terminal's final body is no longer dropped.** A completion
  frame carrying a non-empty final body now surfaces as
  `{ done: true, value }` from `next()` — the done item's `value` is
  `Uint8Array | undefined` — and `for await` yields those bytes before
  the loop ends. Previously the value was silently discarded at the
  seam, so a provider's final bytes never reached the page.

- **A peer session is not usable the instant a handshake completes.**
  A node that receives an inbound peer establishment now attributes it
  only after the initiator's signed establishment proof verifies
  against the identity in its announcement, so there is a brief window
  in which the peer is connecting but not yet connected. A page sees
  this only as `connectPeer`/`acceptPeer` resolving a few milliseconds
  later; what changed is that a peer claiming an identity it does not
  own never reaches your application at all. Nothing in the public API
  moved, and no page supplies keys — the implementation resolves them.

- **`NodeDescriptor.nodeId` is decimal; `connectPeer()` takes
  16-digit hex.** These were always different spellings and the
  documentation wrongly equated them, which is a data-loss trap: a
  decimal string can be a valid-looking hex length and address a
  different node. The types and docs now say so and a conversion is
  provided. Deliberately NOT normalized silently — a peer id you did
  not mean is worse than an error you can see.

- **`candidateError` survives to the status surface.** It was emitted
  by the leaf and discarded by the TypeScript parser, leaving a
  console warning as the only trace of an ICE candidate the browser
  refused. It is machine-readable again.

- **Trickle candidates are retained across the phases that used to
  drop them** — a candidate arriving before the page accepts an offer,
  or before an answer makes a remote description usable, is held under
  the dialog that authorized it and applied when it can be, bounded at
  32 per queue. Previously such candidates were spent on a call that
  could only fail, which cost connectivity on paths where the winning
  pair arrived early.

- **`retryReport()` reports what the policy will actually do**, and
  gains `discardedUnarmed`. The ICE-failure watcher previously acted
  while the report said retry was unarmed, and a node that had been
  both offerer and answerer for one peer stayed eligible to initiate
  repair from both ends.

- **Iterators returned by a direct `BrowserNode` now complete when
  `close()` is called.** A consumer sitting in
  `for await (const bytes of stream)` leaves the loop and one awaiting
  `iterator.next()` resolves `{ value: undefined, done: true }`;
  `onMessage` listeners are dropped. Previously `node.close()` closed
  the wasm node and left every iterator it had handed out parked
  forever, because the wasm side stops calling `on_message` and
  nothing else could end the queue. Opening a stream on a node that
  is already closed is now visibly a typed `SessionError`
  (`kind: 'session'`,
  `session: the node is closed: it no longer holds this origin's identity`)
  rather than a handle that silently sends nothing.

  This is a **behaviour change** for code that relied on an iterator
  outliving its node, or on a `for await` loop parking until some
  other mechanism tore it down: that loop now finishes. Nothing new
  is thrown at the consumer — the terminal is the normal end of
  iteration — so a loop that already handles "the stream ended" needs
  no change, and one whose only exit was an `AbortController` can drop
  it.

  **`MeshSession` already behaved this way and is unchanged; it is the
  direct path that moved.** The leader-proxied surface has always
  ended its streams on a generation change or a leadership loss,
  because Rust fences a stale handle by the generation it was opened
  under and a consumer parked on a handle that will never emit again
  has to be settled by something. `BrowserNode` was the outlier, and
  the two surfaces now dispose of a stream identically.

- **`BrowserNode.close()` finishes what it latched, and reports what
  failed.** It closes every stream it handed out, then the event
  surface, then the leaf — and each of those is now attempted
  independently. Previously the first stream whose `close` threw
  aborted the loop, and because `close` latches and empties its set
  before starting, nothing could retry: the remaining streams stayed
  open with their iterators parked forever, the event surface kept
  dispatching, the wasm node was never closed, and a second
  `close()` returned immediately.

  If anything did throw, `close()` now throws an **`AggregateError`**
  whose `.errors` are the typed `LeafError`s in teardown order —
  always an aggregate, however many failed, so a caller does not have
  to branch on a shape to discover that one part of its teardown did
  not happen. The ordinary path still throws nothing. This is
  reachable because `LeafWasmStreamLike` is a public interface a host
  may implement; the leaf's own `close` does not throw.

- **A stream delivers its own peer's payloads, not its id's.**
  `stream_data` now carries `peerNode` (and `incarnation`), and a
  stream matches an inbound frame on `(peerNode, streamId)` rather
  than on the id alone.

  A stream id is an application label scoped to a session, so
  `openStream({ peer, streamId })` against two peers under one id is
  an ordinary composition — and the wasm callback is handed the
  node-wide event vector. Keyed on the id alone, both wrappers
  admitted whichever peer's frame arrived first, so two streams under
  one label received each other's bytes and a page that echoed what
  it received amplified one peer's payload onto the other's stream.
  The pair-specific-label workaround pages were told to use is not
  needed for this and was never the fix.

  A host-supplied `LeafWasmStreamLike` that spells no peer
  (`peer_node_hex` is optional) keeps the id-only behaviour: a filter
  that cannot be evaluated must not become one that drops
  everything. If you implement that interface, add `peer_node_hex()`
  — 16 lowercase hex digits, the spelling `nodeIdHex()` hands out —
  to get the peer-keyed delivery.

### Notes for consumers

- **64-bit ids are exact decimal strings**, never JS numbers.
  `channel_hash`, `origin_hash`, `stream_id`, `node_id`, `peer_node`,
  `dialog`, `not_after`, `call_id`, `seq`, `incarnation` and
  `generation` all arrive as
  strings, because `JSON.parse` rounds integer literals above 2^53 and
  a rounded channel hash would match the wrong channel.

- **Event tags are the leaf's vocabulary, verbatim** —
  `channel_message`, not `channelMessage`. Field names are camelCased.
  An unknown tag arrives as `{ type: 'unknown', tag, raw }`.

- **`session-lost` and `leader-lost` are never retried for you.**
  They are surfaced typed, and the caller decides.

- **Stream payloads are decoded here, not encoded twice in Rust.** The
  wasm boundary delivers the node's `stream_data` event JSON to a
  stream's callback —
  `{"type":"stream_data","peer_node":"200","incarnation":"1",`
  `"stream_id":"9","seq":"1","payload":"AQI="}` — and `LeafStream`
  parses and
  base64-decodes it, so `onMessage` and `for await` yield
  `Uint8Array` on the direct and the leader-proxied surface alike.
  The declaration in `src/wasm.ts` used to say `Uint8Array` and the
  argument was forwarded verbatim, so a page received the JSON string.
  If you wired `LeafWasmStream.on_message` yourself, parse it — and
  filter on `peer_node` as well as `stream_id`.

- **`OpenStreamOptions.channelHash` is a `number`**, not a string, and
  must be a whole number in `0..=65535`. Rust reads it as a number;
  the string the old type asked for was read as absent, and the stream
  rode channel hash 0. A string, a fraction or an out-of-range value
  is now a typed error instead of a silently different channel.
  `streamId` stays a decimal or `0x`-hex **string** — and a number
  there is refused rather than ignored.

- **`ConnectOptions.iceServers` is honoured.** The leaf now parses
  real `RTCIceServer` objects (`urls` as a string or an array, plus
  `username`/`credential` for TURN) and configures the
  `RTCPeerConnection` with them. It previously read each entry with
  `as_string`, so every object a page passed was dropped and its
  STUN/TURN configuration never reached the offer. A bare URL string
  is refused rather than accepted as a second spelling.

- **`LeafNode.effective_ice_servers(opts)` and
  `LeafNode.effective_stream_options(opts)`** are static, pure readers
  on the wasm surface that report what `connect()` / `open_stream()`
  would actually use, so the ABI can be asserted against the built
  package without an anchor. `tests/abi_real_package.mjs` does exactly
  that against `dist/` and the `pkg/` beside it.

- **`ConnectOptions.iceServers` now has a working default, and one
  refusal.** Omitted, the leaf gathers against the STUN endpoint the
  anchor announces separately — `stun_addr` on `GET /rtc/anchor`, a
  second UDP endpoint distinct from `rtc_addr` — so the advertised
  configuration works without a page choosing a STUN service. An
  anchor that announces none leaves the connection with no ICE
  servers, which is what it had before. An explicit `iceServers: []`
  is still a caller choosing none, and is honoured: the default turns
  on the key's absence, not on emptiness.

  Supplied entries are honoured verbatim except one, which is now
  `IceServerConflictError` (`kind: 'ice-server-conflict'`) raised
  **before any ICE work**: an entry whose STUN endpoint is this
  connection's own peer RTC endpoint. An anchor's `rtc_addr` is that
  connection's ICE peer, not a STUN server for it, so the connection
  gathers no server-reflexive candidate and times out. It is refused
  rather than silently stripped — stripping turns an explicit
  NAT-traversal configuration into a host-candidate-only attempt
  while appearing to have accepted it. The error carries `entry` and
  `peerRtcAddr`. Detection is endpoint equality only (after
  default-port normalisation); a DNS alias that resolves to the peer
  is not detected, and the announced endpoint is what makes detection
  unnecessary for the configuration Net supplies.

- **`stunUrl` is now `diagnosticStunUrl`.** Same behaviour, honest
  name: it builds the throwaway UDP probe's target out of `rtc_addr`
  and is for `probeStunBinding` only. The old name implied that
  turning `rtc_addr` into a `stun:` URL made it suitable for that
  anchor's own connection — the configuration the leaf now refuses,
  and the one both of this repo's harnesses had adopted. There is no
  alias: update the call site.