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

## Unreleased — targets 0.36.0

### Added

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

### Notes for consumers

- **64-bit ids are exact decimal strings**, never JS numbers.
  `channel_hash`, `origin_hash`, `stream_id`, `node_id`, `peer_node`,
  `dialog`, `not_after`, `call_id`, `seq` and `generation` all arrive as
  strings, because `JSON.parse` rounds integer literals above 2^53 and
  a rounded channel hash would match the wrong channel.

- **Event tags are the leaf's vocabulary, verbatim** —
  `channel_message`, not `channelMessage`. Field names are camelCased.
  An unknown tag arrives as `{ type: 'unknown', tag, raw }`.

- **`session-lost` and `leader-lost` are never retried for you.**
  They are surfaced typed, and the caller decides.

- **Stream payloads are decoded here, not encoded twice in Rust.** The
  wasm boundary delivers the node's `stream_data` event JSON to a
  stream's callback — `{"type":"stream_data","stream_id":"9",
  "seq":"1","payload":"AQI="}` — and `LeafStream` parses and
  base64-decodes it, so `onMessage` and `for await` yield
  `Uint8Array` on the direct and the leader-proxied surface alike.
  The declaration in `src/wasm.ts` used to say `Uint8Array` and the
  argument was forwarded verbatim, so a page received the JSON string.
  If you wired `LeafWasmStream.on_message` yourself, parse it.

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
