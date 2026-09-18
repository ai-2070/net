---
title: Browser quickstart
description: "Connect a page to a Net mesh anchor, open a session, and use channels, nRPC calls, streams and typed errors from @net-mesh/browser."
---

# Browser quickstart

A browser connects to the mesh through a native **anchor** and a **bootstrap
credential**. The credential carries the anchor's URL, the transport PSK and the
anchor's Noise public key; the page fetches its first SDP exchange over HTTPS and
then speaks the Net wire over the DataChannel.

## 1. Start an anchor

The anchor is a native node. `anchor serve` is behind the `rtc-bootstrap` feature
(which implies `webrtc`); `anchor credential mint` is in every build.

```bash
cd net/crates/net

# One issuer identity and a trust-domain PSK, once.
cargo run -p net-cli -- identity generate --out issuer.toml
openssl rand -hex 32 > psk.hex

# serve first: it prints the anchor's noise_pubkey, which mint pins.
cargo run -p net-cli --features rtc-bootstrap -- \
  anchor serve \
  --psk-file psk.hex \
  --url https://anchor.example:8443 \
  --credential-issuer <issuer public_key_hex> \
  --allow-origin http://localhost:5173 \
  --tls-cert cert.pem --tls-key key.pem
```

`serve` prints one JSON report and then runs until interrupted. Keep its
`noise_pubkey`.

## 2. Mint a credential

```bash
cargo run -p net-cli -- \
  anchor credential mint \
  --root <mesh-root-entity-hex> \
  --issuer-identity issuer.toml \
  --anchor-noise-pubkey <noise_pubkey> \
  --psk-file psk.hex \
  --url https://anchor.example:8443
```

The output is a `net-bootstrap:…` string. Three rules a browser forces:

- **`--url` must be browser-fetchable:** `https://`, or `http://` only on
  `localhost`, `127.0.0.1` or `[::1]`. It is validated at mint time.
- **`--allow-origin` is repeatable and has no wildcard.** Name the origin the
  page is actually served from.
- **Self-signed TLS is refused.** Pass an operator chain, or use ACME.

The credential contains the PSK. Do not ship a private deployment's PSK to public
visitors; a public browser deployment is a separate transport trust domain.

## 3. Open a session

```ts
import { openSession } from '@net-mesh/browser';

const session = await openSession({
  credentialB64: bootstrapCredential,     // the whole net-bootstrap:… string
  capabilities: ['render'],               // re-announced when this tab leads
  subscriptions: ['scene'],               // restored by a new leader
});

session.role();   // 'leader' | 'follower' — same surface either way
```

`openSession()` returns the **origin's** node, whichever tab is running it, and
survives that tab closing: a follower is promoted, re-bootstraps under the same
identity, and restores its declared subscriptions.

`connect()` returns *this tab's* own node. Use it for a one-tab page or a
harness, not for a page that might be open twice.

## 4. Use the mesh

```ts
await session.subscribe('scene');
await session.announce(['render']);

session.on('channel_message', event => render(event.payload));
session.onEvent(event => log(event.type));            // every event

const reply = await session.call('summarise', bytes, 5_000);
const nodes = await session.query('render');          // NodeDescriptor[]
```

- An event's tag is the leaf's own vocabulary (`channel_message`), fields are
  camelCased, and **every 64-bit id is an exact decimal string** — never a
  JavaScript number, which would round above 2^53.
- Byte payloads arrive as `Uint8Array`.
- Before any `connected` event, `session.nodeIdHex()` is `null`.

### Streams

```ts
const stream = await session.openStream({ reliability: 'reliable' });

await stream.send(frame);
for await (const bytes of stream) consume(bytes);
```

Three things to hold on to:

- **`reliability` is required**, deliberately: the wasm boundary cannot tell an
  absent key from a misspelled one, so a typo would silently yield a reliable
  stream.
- A stream is identified by **(peer, id)**, not by its id alone.
- A stream handle is fenced to its session incarnation. On the routed → direct
  upgrade the session is replaced, and `send` rejects; reopen the stream with the
  same peer and id.

## 5. Handle typed errors

```ts
import { LeafError, isUdpBlocked } from '@net-mesh/browser';

try {
  await session.call('summarise', bytes, 5_000);
} catch (error) {
  if (error instanceof LeafError) route(error.kind);   // a stable discriminant
}
```

`.message` is the verbatim native display text; `.kind` is the discriminant a
caller branches on. The full taxonomy is in
[Error codes](/docs/reference/error-codes). The two that matter most here:

- **`ice-timeout`** — ICE did not connect inside the deadline. This does *not*
  establish that UDP is blocked.
- **`udp-blocked`** — asserted only with two observations together: the HTTPS
  bootstrap to that anchor succeeded, and a STUN binding to the `rtc_addr` that
  same anchor published went unanswered. Such networks are unsupported in v1.

A follower's operation that cannot reach the leader is `not-leader`; a call whose
outcome is unknown is `rpc-indeterminate`, and it must **not** be retried — a
resumed leader may still execute it late.

## Next

- [Networked store](/docs/sdk/browser/store) — share authoritative state.
- [Three.js binding](/docs/sdk/browser/three) — render it.
- [NAT and traversal](/docs/guides/nat-and-traversal) — the native side of the
  same substrate.
