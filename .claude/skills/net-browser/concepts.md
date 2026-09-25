# Concepts — what a browser node is, and what it is not

Read this before writing code. Every trap in this skill traces back to one of
these facts.

## 1. A tab is a node

`@net-mesh/browser` runs `net-mesh-leaf` (Rust, compiled to WebAssembly) inside
the page. The leaf holds:

- an **Ed25519** entity keypair (`node_id` and `origin_hash` are derived from
  it) and a **Noise X25519** static key;
- a Noise **NKpsk0** session to the anchor or to a peer;
- streams, channel subscriptions, capability announcements and nRPC calls.

An anchor cannot tell a browser session from a UDP one once the transport is up:
the identity is the mesh's, the session is authenticated by the mesh's admission
rules, and a relay above it cannot read or forge application traffic.

**Two security layers, deliberately.** DTLS already protects a DataChannel; Net
runs its own Noise session on top. That is what makes the tab a *node* rather
than a browser client. Dropping Noise in favour of the DTLS exporter was
measured as cheaper and left deferred on purpose.

## 2. The anchor finds peers; it does not carry them

A native node compiled with the `webrtc` feature can act as an **anchor**: a
bootstrap and signalling listener, a STUN answerer, and a relay for the minority
of pairs ICE cannot connect. The intended steady state for a game is a
**direct leaf ↔ leaf DataChannel**, with the anchor's per-pair forwarding counter
going flat the moment the pair stops needing it.

That framing is load-bearing when you read the failure modes: `ice-timeout` is a
statement about *this pair*, not about the anchor's health, and a relayed session
is the correctness fallback rather than the design.

## 3. One node per origin, elected by Web Lock

`RTCPeerConnection` does not exist in a worker, so exactly one document runs the
node — on its main thread. Tabs contend for a **Web Lock**; the holder is the
**leader**, every other tab attaches as a **follower** and drives the shared node
through it (`BroadcastChannel` + a leadership protocol).

Consequences that show up in ordinary code:

- **`connect()` per tab is two nodes contending for one identity.** Use
  `openSession()` for anything real: it returns the origin's node, whichever tab
  runs it, and survives that tab closing.
- **A follower's methods are proxy round trips.** Three are promises for that
  reason — `counters()`, `isEnrolled()`, `openStream()`. Everything else has the
  same shape and the same typed errors on both surfaces; a stale tab's operation
  fails as `not-leader` rather than silently doing nothing.
- **Promotion restores declared state.** A promoted follower re-bootstraps under
  the same identity with a **new generation**, and restores the `capabilities`
  and `subscriptions` the session was opened with. That is why they are options
  on `openSession` and not only methods you call later.
- **A frozen tab keeps its lock.** A browser-frozen leader is not replaced — its
  task queues simply do not run, so nothing is answered. `errors.md` covers the
  `rpc-indeterminate` this produces.

## 4. The store: one authority, many replicas

For a world rather than a request, `@net-mesh/browser` exports a **networked
store**. It is not a CRDT and not a lock-step replication protocol; the shape is:

```
        host  (the authority)
          │  holds the document, executes acts, projects per audience
          │
   ┌──────┴──────┐
 replica A     replica B        each holds a projection it was GIVEN
```

- **A `definition`** declares the document type (id + version), its state shape,
  the audience names, the actions, the inputs and the projection function. Both
  ends of a store instance share it; a version disagreement is a typed refusal.
- **An `audience`** is a name describing *who is reading* (`'crew'`,
  `'command'`). `project(state, audience)` decides what that audience may see,
  and `authorize(request)` decides whether a request is allowed at all.
- **`act` is a correlated transaction** — it executes on the host and its result
  comes back to the caller. **`input` is coalesced and unacknowledged** — the
  right shape for movement and other high-frequency intent.
- **A replica cannot read what it was not given.** Projection is applied by the
  host before anything leaves; the replica's state is its bound audience's view.

## 5. Choose the plane by the question, not by the API

| The task | The plane | Where |
|---|---|---|
| Call a service and get a value | nRPC (`call`) | `session.md` |
| Tell many peers something / hear from them | channels (`subscribe` / `publish` / `announce`) | `session.md` |
| Move bytes to one peer, ordered or not | streams (`openStream`) | `session.md` |
| Keep many peers on one authoritative state | the store (`defineStore` / `hostStore` / `joinStore`) | `store.md` |
| Draw that state in a scene graph | `bindEntities` | `store.md` |

They compose rather than compete: both store roles take the node from
`connect()` (or a session) as their `transport`.

## 6. What the page downloads, and what it costs

The wasm, its glue and the package bundle are shipped together and the wasm is
fetched **relative to the entry point**. Measured against a release leaf build:
about 612 KB raw / 240 KB gzipped for a page's whole download (wasm 553 KB raw /
225 KB gzip, glue 43 KB, bundle 16 KB). A size gate (`node scripts/size.mjs
--assert`) fails the package above 1.5 MB gzipped.

Budget for that before promising a lightweight embed — and note the wasm is
scalar (no SIMD), so the AEAD cost measured for the double-encryption decision
was measured on the same artifact a page gets.

## 7. What a browser cannot do

- **No UDP socket.** The transport is WebRTC DataChannel over DTLS. A network
  that blocks UDP cannot be routed around by this package; it is *classified*
  (`udp-blocked`) so the page can say why. See `errors.md`.
- **No `RTCPeerConnection` in a worker.** The node is on the main thread of the
  lock holder. Heavy synchronous work in that tab slows the node down with it.
- **No registry install yet.** `@net-mesh/browser` is built from the repository —
  the leaf's wasm first, then the package — and it is not on a registry. See
  `session.md` § Build it.
- **No native node's full surface.** The leaf is a deliberate subset; if a
  mechanism is not in `source-access.md`'s file list, treat it as absent rather
  than assuming the native spelling works here.

## 8. The trust boundary is the origin

The leaf persists the identity in IndexedDB wrapped by a **non-extractable**
WebCrypto AES-GCM key. Non-extractable means script cannot read the *wrapping*
key — it does not mean a page on this origin cannot ask the browser to use it.
Storage protects against exfiltration, not against a compromised page. A
deployment that needs the key held elsewhere injects it custodially; see
`session.md` § Identity.
