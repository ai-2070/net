---
title: Browser
description: "Run a Net node in a browser tab with @net-mesh/browser — a WebRTC DataChannel leaf, the anchor bootstrap model, and the networked store bound to a Three.js scene."
---

# Browser SDK

`@net-mesh/browser` makes a browser tab a first-class Net node: its own entity
identity, its own Noise session over a WebRTC DataChannel, and the same channels,
nRPC calls and streams the native mesh runs. Native peers cannot tell a browser
session from a UDP one above the transport layer.

```bash
npm install @net-mesh/browser
```

It is a **separate package** from `@net-mesh/sdk`, deliberately. The Node SDK is
built on `@net-mesh/core`, the napi native binding, and its dependency graph
contains a `.node` addon. Browser code publishes out of a package whose graph
never contains that addon, so a page's bundle cannot resolve it by accident.

The browser surface is TypeScript-only; the Rust, Python, Go and C bindings have
no equivalent.

## The anchor

A browser reaches the mesh through a native **anchor**: a node that serves the
HTTPS bootstrap endpoint, answers STUN, and relays for the minority of pairs ICE
cannot connect.

**An anchor is how browsers find each other, not how they talk to each other.**
Browser ↔ browser and browser ↔ native sessions ride a DataChannel directly
whenever ICE can connect the pair; the anchor is control plane and fallback. Once
a pair is direct, the anchor is off the application data path.

Signalling, the bootstrap credential, TLS requirements and the supported harnesses
are covered in the [quickstart](/docs/sdk/browser/quickstart).

## One node per origin

There is **one node per origin**. Tabs contend for a Web Lock; the holder runs
the node on the main thread (`RTCPeerConnection` does not exist in a worker) and
every other tab attaches as a follower and drives the same node.

Use `openSession()` for a page that might be open twice — which is every real
page. `connect()` gives *this tab* its own node, and two tabs calling it on one
origin are two nodes contending for one identity.

## Choose the surface

| I want to | Start here |
|---|---|
| Connect a page and use channels, nRPC, streams | [Quickstart](/docs/sdk/browser/quickstart) |
| Share authoritative game/world state the safe way | [Networked store](/docs/sdk/browser/store) |
| Render that state in a Three.js scene | [Three.js binding](/docs/sdk/browser/three) |

## What is not in v1

- **UDP-blocked networks are unsupported.** A browser whose network blocks UDP
  cannot reach its anchor either, and there is no TURN server or rescue path. An
  ICE failure is reported as a typed `ice-timeout`; a narrower `udp-blocked`
  classification requires positive evidence — see the
  [quickstart](/docs/sdk/browser/quickstart).
- **No media.** DataChannels only — no SRTP, no audio or video tracks.
- **No serverless-only hosting.** A tab has no raw UDP, so it cannot cover for a
  runtime that cannot hold an ICE agent or relay session alive. v1 requires at
  least one always-on anchor.
- **A leaf does not forward.** It never re-floods announcements and drops routing
  packets not addressed to it.
- **The public capability path only.** Organization-protected calls are not part
  of browser v1.
