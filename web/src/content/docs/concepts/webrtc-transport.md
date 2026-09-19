---
title: WebRTC transport
description: "How a browser tab becomes a Net node — a WebRTC DataChannel to a native anchor, a Noise session over it, and what changes when ICE fails."
---

# WebRTC transport

A browser cannot open a UDP socket, so it cannot send a Net packet the way every
other node does. The WebRTC transport is how a tab joins anyway: the node runs as
a WebAssembly leaf, reaches a native **anchor** over an `RTCPeerConnection`, and
speaks the same Noise session, streams, channels and nRPC over the resulting
DataChannel.

Nothing above the transport changes. A browser leaf has its own entity identity,
its own session keys, and its own capability announcements, and an anchor cannot
tell a browser session from a UDP one once the transport is up.

```text title="A browser leaf, its anchor, and the mesh"
  page (tab)                 native anchor (webrtc)         mesh
  ┌───────────────┐  HTTPS   ┌───────────────────────┐
  │ net-mesh-leaf │ ───────▶ │ POST /rtc/offer       │
  │    (wasm)     │ ◀─────── │ GET  /rtc/trickle     │
  │               │          │ GET  /rtc/anchor      │
  │ RTCPeerConn   │ ═══════▶ │ DataChannel endpoint  │ ──▶ UDP peers
  │ + Noise NKpsk0│  DTLS    │ + Noise NKpsk0        │
  └───────────────┘          └───────────────────────┘
```

## Two security layers, and why both

DTLS already secures a DataChannel, and Net runs its own Noise NKpsk0 session on
top of it rather than trusting the DTLS session to be the mesh session. That is
what makes a browser node a *node*: its identity is the mesh's Ed25519 entity,
its session is authenticated by the mesh's admission rules, and a relay above it
cannot see or forge application traffic.

Dropping Noise in favour of the DTLS exporter was measured and left deferred. On
a headless Chromium 149 run with the scalar-wasm build, Net's AEAD on top of
DTLS costs **+3.5 µs per 1 KiB packet hot** — **+0.21 ms of main-thread time per
second** at 60 Hz. The measurement and its method are in
`docs/internal/performance/WEBRTC_DOUBLE_AEAD.md` in the repository; the
decision it supports is that the second AEAD stays.

## The anchor

An anchor is an ordinary native node built with the `webrtc` feature. It does
three things a browser cannot do for itself:

- **Bootstrap over HTTPS.** A page's first contact is `POST /rtc/offer`, not a
  Net packet.
- **Answer STUN.** The anchor announces a STUN endpoint separately from its own
  RTC endpoint, and a leaf's connections gather against it by default.
- **Forward.** Until a pair's ICE connects, the anchor relays their traffic — and
  it keeps relaying for pairs ICE never connects.

The feature is **off by default**, and that is a build-cost decision rather than
a maturity one: `str0m`'s pinned crypto stack resolves `aws-lc-rs`, whose build
script compiles C, so `--features webrtc` needs cmake and a C toolchain. The
default build's exported C-ABI symbol set and observable behaviour are unchanged
by it. See `CONTRIBUTING.md` in the repository for the build detail.

The two endpoints an anchor publishes are distinct and easy to confuse:

| Field | What it is |
| --- | --- |
| `rtc_addr` | The anchor's own RTC endpoint — the **ICE peer** of a connection to it |
| `stun_addr` | The STUN endpoint a connection gathers against |

Configuring a connection's ICE servers with its own peer's `rtc_addr` is refused
before any ICE work, because a peer cannot be its own STUN server.

## Bootstrap: the credential

A page has no provisioning step, so the secret its Noise handshake needs arrives
in a **browser bootstrap credential** — a `net-bootstrap:` string minted by an
operator and handed to the page out of band.

```sh
net-mesh anchor credential mint --root … --psk-hex … \
  --anchor-noise-pubkey … --url https://anchor.example
net-mesh anchor credential inspect --credential @credential.txt
```

The credential carries an invite token, the anchor's Noise static public key, the
mesh PSK, a trust-domain id derived from that PSK, and the bootstrap URL. Two
lifetimes ride in the format and are enforced separately: the invite nonce is
single-use and short-lived, and the PSK is standing with its own longer deadline.
An anchor refuses a credential minted for another trust domain *before* any
handshake — the domain is a one-way function of the PSK, so the check is
mechanical and publishing the id reveals nothing.

The encoded string contains the PSK, which is why `mint` stages it with the same
file-permission discipline the CLI uses for key material.

### The bootstrap listener

| Request | Purpose |
| --- | --- |
| `POST /rtc/offer` | Credential + claimed node id + SDP offer → SDP answer |
| `GET /rtc/trickle` | Upgraded to a WebSocket; ICE candidates both ways |
| `GET /rtc/anchor` | The announcement fields, so a page can compare the credential's pinned key against the live anchor |

An HTTP-originated offer is an **offer**: it enters the same path a `0x0D02`
signalling offer takes, and the session that results is provisional under the
same admission contract as any other browser-facing session, until enrollment
promotes it. The claimed node id is a claim — it rides into the Noise prologue,
so a page that claims an id it cannot handshake as fails the handshake.

Two deployment constraints are deliberate and have no override:

- **TLS is browser-trusted or nothing** — an operator PEM chain and key, or ACME.
  There is no self-signed mode, because a page cannot fetch an endpoint whose
  certificate it does not accept, and a harness that ignored that would prove
  something no deployment can rely on.
- **CORS is an explicit allow-list with no wildcard**, because the endpoint takes
  a credential. Origin is validated on the trickle WebSocket too.

## Direct and routed

Every browser ↔ browser and browser ↔ native pair starts **routed**: the two
leaves have a session through the anchor, and the anchor forwards their packets.
A peer attempt then tries to install a direct session over ICE, and once it lands
the pair's traffic moves to the DataChannel with no anchor in the path.

A routed pair is not a degraded state to be fixed, and an ICE failure is not a
connection failure: if ICE does not connect inside the attempt's deadline, the
routed session simply stays, and the pair keeps working. What a page can observe
is a typed outcome — `direct`, `iceTimeout`, or `udpBlocked` — and the anchor's
own per-pair `forwarded_app_packets` counter, which goes flat when a pair stops
needing it.

That counter **excludes `0x0D02` signalling**, so "flat once direct" is a claim
about application traffic rather than an artefact of signalling having stopped.
Native ↔ native pairs keep using UDP and the existing hole punch; WebRTC's value
here is browser reach, not a better path between two native nodes.

## When ICE fails

An ICE timeout is **not** evidence that UDP is blocked. A down, misconfigured or
saturated anchor produces the same symptom, so a failure is typed `ice-timeout`
unless two observations hold together:

1. the HTTPS bootstrap to **that anchor** succeeded — it is up and addressable;
2. a STUN binding to the `rtc_addr` **that same anchor published** went
   unanswered.

`udp-blocked` is the name for exactly that pair of facts, and it is the only path
to it. Networks that block UDP outright still work: the pair stays routed through
the anchor, and the page sees `udpBlocked` as an explanation rather than as a
dead end. The classification, its probes and the measured browser behaviour
behind the rules are documented with the [browser SDK errors](/docs/sdk/browser/errors).

## What this is not

- **Not a replacement for NAT traversal between native nodes.** UDP, reflex
  discovery, rendezvous and hole punching remain the native path; see
  [NAT and traversal](/docs/guides/nat-and-traversal).
- **Not a browser transport without an anchor.** Signalling, the initial
  bootstrap and the routed fallback all live on a native node; a page needs one
  to reach.
- **Not a second identity system.** The origin is the browser trust boundary, and
  a deployment that needs the key held elsewhere injects it custodially rather
  than relying on browser storage.

## Where to read next

- [Browser SDK](/docs/sdk/browser) — the TypeScript surface over this transport
- [Browser quickstart](/docs/sdk/browser/quickstart) — build the leaf and connect
- [NAT and traversal](/docs/guides/nat-and-traversal) — the native path
- [Production deployment](/docs/guides/production-deployment) — running an anchor
