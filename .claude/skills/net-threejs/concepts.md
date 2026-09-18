# The mental model

Three models stack here, and confusing them is the failure this file exists to
prevent: the **transport** (a browser leaf on the mesh), the **session** (one
node per origin), and the **store** (an authoritative owner and read-only
replicas on top of it).

---

## 1. A browser is a leaf; an anchor is control plane

`@net-mesh/browser` is a TypeScript surface over the `net-mesh-leaf` wasm node.
The leaf holds a real Net identity (an Ed25519 entity key and a Noise X25519
static key), a real Noise session over a WebRTC DataChannel, and speaks the
exact Net wire — 68-byte header, Noise NKpsk0, ChaCha20-Poly1305, the same
subprotocol ids. A native peer cannot tell a browser session from a UDP one
above the transport layer.

The **anchor** is a native node that:

- serves the HTTPS bootstrap endpoint that hands a browser its first SDP
  exchange,
- answers STUN on its RTC socket,
- forwards for the minority of pairs ICE cannot connect.

It is **how browsers find each other, not how they talk to each other**. Browser
↔ browser and browser ↔ native sessions ride a DataChannel **directly** whenever
ICE can connect the pair; the relay is the correctness fallback. Quote that
framing in docstrings and READMEs — the design inverts the usual NAT-traversal
framing on purpose. There, a direct path is an optimization; here it is a
**product requirement**, because a server hop on every position update is the
thing this design refuses to ship.

**A leaf does not forward.** Non-forwarding is a *role*, not a TTL: a leaf never
originates pingwaves, never re-floods announcements, and drops any routing
envelope whose destination is not itself. It tags its announcement `leaf` /
`transport:rtc` and omits a reflex address.

### What is deliberately unsupported in v1

- **UDP-blocked browsers.** A browser whose network blocks UDP cannot reach its
  anchor either — the bootstrap DataChannel is ICE-over-UDP too. This is a
  *declared limitation* with a prompt typed failure, not a gap to discover in
  production. There is no TURN protocol server and no rescue path.
- **ICE timeout ≠ UDP blocked.** An anchor that is down, misconfigured or
  saturated produces an identical timeout. An ICE failure surfaces as
  `ice-timeout`, and only two observations together narrow it to `udp-blocked`:
  the HTTPS bootstrap to *that anchor* succeeded, **and** a STUN binding to the
  `rtc_addr` *that same anchor published* went unanswered.
- **Media.** DataChannels only — no SRTP, no audio/video tracks.
- **Native ↔ native WebRTC.** UDP plus the existing punch remains the native
  path.
- **Anchorless hosting.** Serverless runtimes expose no listening UDP socket and
  cannot hold an ICE agent alive between invocations; a tab has no raw UDP to
  cover for it. v1 requires at least one always-on native anchor.
- **Organization-protected calls from a leaf.** v1 browser scope is the public
  capability path; a leaf drops the org membership subprotocol as unknown.

---

## 2. One node per origin — use `openSession`

There is **one node per origin**. Tabs contend for a Web Lock; the holder runs
the node on the main thread (`RTCPeerConnection` does not exist in a worker) and
every other tab attaches as a **follower** and drives the same node through a
`BroadcastChannel`.

```ts
import { openSession } from '@net-mesh/browser';

const session = await openSession({
  credentialB64,
  capabilities: ['render'],
  subscriptions: ['scene'],
});
session.role();   // 'leader' | 'follower' — the same surface either way
```

- **`openSession()` gives the origin's node**, whichever tab runs it, and
  survives that tab closing: a follower is promoted, re-bootstraps under the
  same identity with a fenced new generation, and restores the `subscriptions`
  it was opened with.
- **`connect()` gives *this tab* its own node.** Two tabs calling `connect()` on
  one origin are two nodes contending for one identity. Use it only when you
  know you want that (a one-tab page, or a harness).

A follower's work happens in another tab, so three methods are **promises** on
the session and synchronous on the direct node: `openStream`, `counters`,
`isEnrolled`. Everything else has the same shape and the same typed errors; a
stale tab's operation fails as `not-leader` rather than silently doing nothing.

**A leaf's identity is scoped to its origin.** Both tabs of a two-tab game must
be served from the same origin, which is why the demo's static server sets
`Cross-Origin-Opener-Policy: same-origin`.

**A frozen leader is `rpc-indeterminate`, not `rpc-timeout`.** A frozen tab
keeps its Web Lock, so no successor is elected and nothing changes in the
follower's view — but its task queues do not run, so nothing is answered,
including the cheap control chatter. The follower's own deadline then produces
`rpc-indeterminate`, which means "the remote may have executed this". **Do not
retry on it** — a resumed tab is still the legitimate leader and those calls may
execute late, so a retry can cause the effect twice.

---

## 3. The store: one owner, read-only replicas

The networked game store (`defineStore` / `hostStore` / `joinStore`, re-exported
from the package root) layers on the session. A store incarnation is identified
by **(authority identity, definition id, version, application key)** — the key
can name a ship, a room or a region. There is no global world authority and no
mandatory anchor data hop: a browser can host one store and join another, and the
owner is the application endpoint, not a relay inserted between endpoints.

### Zustand-style, but not every replica may write

- The **owner** has `setState` and is the only writer.
- A **replica** has `getState`, selector `subscribe`, and `getStatus`, and writes
  only through `actions` and `inputs`. A replica having no `setState` is a
  **type-level fact**, not a runtime check.
- v1 has one fixed owner per incarnation. There is no election, no automatic
  host migration, no CRDT, no arbitrary multi-writer merge. An owner closing
  ends that incarnation; reconnecting to a new one produces a fresh snapshot and
  does not rerun unresolved actions.

### `inputs` vs `actions` — the single most important distinction

| | `input` | `action` |
|---|---|---|
| Shape | latest-value intent | acknowledged request/response |
| Returns | `InputDisposition`, **synchronously** | `Promise<output>` |
| Correlation | none — no reply frame | correlated before send |
| Loss | a dropped one may be the last one | refused or answered, never silent |
| Use for | held direction, pointer, aim, throttle | fire, buy, enlist, place |
| Refusals | dropped: `not-ready`, `capacity` | `forbidden`, `action-rejected`, `capacity`, `indeterminate` |

A camera position, menu state, animation interpolation or prediction is neither
— it is local, and it never crosses the mesh.

### Audiences are an authorization boundary, not a UI hint

The host's `project(state, audience)` produces each caller's view, and every
delta is computed from **projected** before/after states. A change invisible to
an audience produces **no frame at all** for that caller. `authorize` runs per
read and a denied audience is **refused**, never narrowed. This is what makes an
audience-scoped field genuinely absent rather than hidden in the renderer.

---

## 4. Reference identity is the point

The store's reconciliation **shares the references of subtrees that did not
change** and freezes the graph. That promise is worthless if the render layer
rebuilds everything anyway — which is exactly what a naive
`for (const entity of Object.values(state.entities))` loop throws away.
`@net-mesh/browser/three` turns it into the thing you wanted: an entity whose
reference is unchanged is not touched, so a 60 Hz loop over a world where one
ship moved does one update. See `three.md`.

---

## 5. The trust boundary

**The origin is the trust boundary.** The leaf persists its identity in
IndexedDB under a non-extractable WebCrypto AES-GCM key; non-extractable means
script cannot read the wrapping key, but any script running *on this origin* can
ask the browser to use it. The storage protects the key from exfiltration, not
from a page that has already been compromised. A deployment that needs the key
held elsewhere injects it custodially instead.

**Identity on the wire is proved by the transport, never claimed by a frame.** A
store frame arrives as a `stream_data` event that carries `peerNode` — the peer
whose installed session opened the packet — and *that* is the identity
`authorize` and every handler are handed. No store frame has an originator field
to consult, and a frame whose event carries no authenticated peer is dropped and
counted rather than dispatched with a guess.
