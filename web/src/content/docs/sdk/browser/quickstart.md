---
title: Browser quickstart
description: "Connect a page to the mesh: build the wasm leaf, connect through an anchor, then announce, call, query and stream."
---

# Browser quickstart

Build the package ([Browser SDK](/docs/sdk/browser#build-it) has the two
commands), serve `dist/` over HTTP, and open a page.

## What the first run needs

Two things, and neither comes from the page:

- **An anchor**, reachable over HTTPS.
- **A bootstrap credential** per player, which that anchor issues.

Run the anchor for your game with the CLI. It needs a certificate browsers
trust, your page's origin, a pre-shared key file and an issuer key file
(`net-mesh identity generate --out issuer.toml`):

```sh
net-mesh anchor serve --psk-file psk.hex \
  --url https://anchor.example.com --tls-cert cert.pem --tls-key key.pem \
  --allow-origin https://game.example.com \
  --issuer-identity issuer.toml --game my-game
```

Each player then asks it for an anonymous credential — see *Connect* below.
Without `--game` the anchor serves no enrollment, and a page's `connect()` fails
with `session: rpc: the call's deadline elapsed` after a perfectly good TLS
handshake. One anchor can serve several games but does not yet isolate them
from each other; run one per game for now.

`tests/rtc_browser/run.sh` (`run.ps1` on Windows) is the other one: the CI
harness, on Chromium and Firefox, which issues its own CA and trusts it per
engine rather than disabling certificate checking.

## Connect

```typescript
import { connect, rememberedIdentity, requestCredential } from '@net-mesh/browser';

// An anonymous credential for this player, from your game's anchor.
const { credentialB64, bootstrapUrl } = await requestCredential({
  anchorUrl: 'https://anchor.example.com',
  game: 'my-game',
});
```

`rememberedIdentity()` keeps the player the same across visits (the secrets live
in `localStorage`; one player per browser profile). A credential binds to the
first player that uses it.

```typescript
const node = await connect({
  ...rememberedIdentity(),
  credentialB64,                          // the whole `net-bootstrap:…` string
  bootstrapUrl: 'https://anchor.example', // optional; the credential carries one
});

console.log(node.nodeIdHex(), node.anchorIdHex());
```

`connect()` resolves once the session is established **and enrollment has run** —
it awaits that exchange for you. `bootstrapUrl` is the anchor's base URL: the
leaf appends `/rtc/anchor`, `/rtc/offer` and `/rtc/trickle` to it.

`node.isEnrolled()` is the honest read on whether the anchor admitted this leaf.
`false` means the session is still provisional, and a call will die on its
deadline — check it before blaming a `rpc-timeout` on a slow anchor.

## Subscribe, announce, call

```typescript
node.on('channel_message', (event) => render(event.payload));

await node.subscribe('jobs');
await node.announce(['transcribe']);

const reply = await node.call('summarise', new TextEncoder().encode('…'), 5_000);
const workers = await node.query('transcribe');
// [{ nodeId, entityId, capabilities, rtcAddr, noisePubkey, version }]
```

- `subscribe` makes the leaf deliver a channel's messages as
  `channel_message` events. The leaf only delivers channels this node subscribed
  to.
- `announce` publishes the capability tags other nodes discover you by.
  **An announcement is a lease, not a registration** — a peer that looks a few
  seconds later finds nothing unless you re-announce. On a session, declaring
  `capabilities` in `openSession` makes a new leader re-announce them for you.
- `call` is nRPC and resolves the reply. It rejects with a typed `RpcError` —
  `rpc-refused`, `rpc-timeout`, `session-lost`, `leader-lost`,
  `rpc-malformed` — and **never retries silently**. A call whose session or
  leader went away is surfaced so the caller decides.
- `query` resolves parsed descriptors. `nodeId` is **decimal**;
  `peerIdHex(descriptor.nodeId)` is the 16-hex spelling `connectPeer` takes.

## Streams

```typescript
const stream = node.openStream({ reliability: 'fireAndForget' });
await stream.send(frame);

for await (const payload of stream) consume(payload);   // Uint8Array
```

`reliability` is **required**, not defaulted. At the wasm boundary an absent key
and a misspelled one are the same thing, so `{ reliabilty: 'fireAndForget' }`
would silently produce a reliable stream; making the field required turns that
typo into a compile error.

`reliable` retransmits and reorders by `seq`. `fireAndForget` does neither, which
is the point of it: a dropped frame stays dropped and the consumer sees the gap.

A stream is identified by **`(peer, streamId)`**, not by its id alone — a stream
id is an application label scoped to a session, so the same label against two
peers is two streams. `openStream({ peer })` addresses a peer in 16 hex digits;
absent, the stream addresses the anchor.

## Events

```typescript
node.on('stream_data', (event) => { /* event.payload is Uint8Array */ });
node.onEvent((event) => log(event.type));             // every event
for await (const event of node.events()) { /* … */ }  // async iterable
```

Tags are the leaf's vocabulary verbatim — `channel_message`, not
`channelMessage`. `connected`, `disconnected`, `channel_message`, `stream_data`,
`announcement`, `signal`, `rpc_response`, `dropped`, `rtc_failure` and
`leader_changed` are typed; an unknown tag arrives as
`{ type: 'unknown', tag, raw }` rather than being dropped, so a newer leaf never
goes silent against an older page.

Two invariants worth knowing:

- **64-bit ids are exact decimal strings**, never JS numbers. `JSON.parse` rounds
  integer literals above 2^53, which would silently mis-route a page filtering a
  channel by hash.
- **Byte payloads are already decoded** for you: standard base64 on the wire,
  `Uint8Array` in the event.

A listener that throws is reported to the console and skipped. It neither takes
down its siblings nor unwinds into the wasm frame that called it.

## Close

```typescript
node.close();
```

`close()` ends the iterators it handed out — a `for await` loop leaves the loop,
and an awaiting `iterator.next()` resolves `{ done: true }`. That is the normal
end of iteration, so a loop written to handle "the stream ended" needs nothing.
Opening a stream on a closed node is a typed `SessionError`, not a dead handle.

## Loading the wasm differently

The default lookup is relative to the entry point. Override it when your
deployment puts the wasm somewhere else:

```typescript
await connect({ credentialB64, wasmUrl: '/assets/net_leaf_bg.wasm' });
await connect({ credentialB64, wasm: await import('/assets/net_leaf.js') });
```

## Sizes

The package ships a size report, because a page pays for all of it:

```sh
npm run size                     # table + machine-readable SIZE lines
node scripts/size.mjs --assert   # non-zero exit if the wasm exceeds 1.5 MB gz
```

Measured on the current build: `net_leaf_bg.wasm` is **225 369 B gzipped** and
the wasm-bindgen glue 8 684 B, so a page that takes the single-file bundle
downloads about **240 kB** in total.

## Next

- [Session](/docs/sdk/browser/session) — one node per origin, and what happens
  when the tab running it closes
- [Store](/docs/sdk/browser/store) — a networked document rather than a request
- [Errors](/docs/sdk/browser/errors) — the taxonomy, and `udp-blocked`
