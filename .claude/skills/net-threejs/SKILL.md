---
name: net-threejs
description: "Use this skill when building a multiplayer Three.js scene or browser game on Net — `@net-mesh/browser` (the browser leaf), `@net-mesh/browser/three` (`bindEntities`), and the networked game store (`defineStore` / `hostStore` / `joinStore`). Covers: a WebRTC DataChannel from a browser tab to a native anchor, the anchor/bootstrap-credential model ('anchors are how browsers FIND each other, not how they TALK'), one-node-per-origin and `openSession` vs `connect`, the authoritative owner + read-only replica store, `inputs` (latest-value intent) vs `actions` (correlated, policy-refused) vs audience-scoped `project`, binding store entities to a scene graph by reference identity, the leader/follower session, the typed error taxonomy, and the v1 limitations (UDP-blocked browsers unsupported, no media, native↔native stays UDP, an always-on anchor is required). Triggers on `@net-mesh/browser`, `@net-mesh/browser/three`, `bindEntities`, `hostStore`, `joinStore`, `defineStore`, `openSession`, `MeshSession`, `net-bootstrap:`, 'three.js multiplayer', 'browser game on Net', 'browser mesh node', 'WebRTC DataChannel mesh', 'direct path'. Browser-side TypeScript only — not the Node `@net-mesh/sdk` or the Rust/Python/Go/C bindings (that is the `net-event-bus` skill)."
allowed-tools: ["Read", "Grep", "Glob", "Bash", "Edit", "Write"]
metadata:
  skill-version: 1.0.0
  last-updated: 2026-09-18
  net-version: 0.36.0
---

# Net browser store → Three.js scenes

A **browser tab is a first-class Net node**: its own identity, its own Noise
session over a WebRTC DataChannel, and the same store/channel/stream machinery
the native mesh runs. `@net-mesh/browser` is the package; the networked game
store is re-exported from its root, and `@net-mesh/browser/three` binds that
store to a scene graph.

**Before you write a line, read `concepts.md`.** Almost every mistake here is a
mental-model mistake: treating an anchor as the data path, keeping game state in
the renderer, rebuilding meshes every frame, or thinking an ICE timeout proves
UDP is blocked. Those all compile, run, and are rejected.

## How to use this skill

Load reference files on demand — do not read them all up front.

| File | Read when |
|---|---|
| `concepts.md` | **Always first.** The leaf/anchor model, one-node-per-origin, the owner/replica store, inputs vs actions vs audiences, why reference identity is the point. |
| `store.md` | Writing the networked state: `defineStore` / `hostStore` / `joinStore`, the handles, the protocol facts, the error codes, and the enforced bounds. |
| `three.md` | Rendering it: `bindEntities`, the create/update/remove contract, the reference-identity skip rule, and what must stay out of the store. |
| `bootstrap.md` | Getting two browsers connected: the anchor (`net-mesh anchor serve`), `net-mesh anchor credential mint`, the credential, TLS/origin rules, and the supported harnesses. |
| `gotchas.md` | Before merging, or when the user's framing carries a wrong model — the review invariant and the footguns that cost the most. |
| `testing.md` | Writing witnesses — what the real-browser harnesses actually establish, and the two demo modes' very different evidentiary weight. |
| `source-access.md` | You need the exact signature or mechanism and are not inside the Net repository. |

## The five things that are load-bearing

1. **An anchor is how browsers *find* each other, not how they *talk* to each
   other.** It bootstraps signalling, answers STUN, and relays only for the
   minority of pairs ICE cannot connect. Once a pair is direct, the anchor is
   off the application data path. The direct path is a **product requirement**
   here — a server hop on every position update is the thing this design exists
   to remove — not a latency optimization.
2. **One node per origin.** Tabs contend for a Web Lock; the holder runs the
   node on the main thread (`RTCPeerConnection` does not exist in a worker) and
   the rest attach as followers. A page that might be open twice uses
   `openSession()`, never `connect()` — two `connect()` nodes on one origin are
   two nodes contending for one identity.
3. **One authoritative owner per store incarnation; replicas read.** Only the
   owner gets `setState`. Everyone else writes through `inputs` (latest-value
   intent) and `actions` (acknowledged operations). There is no election, no
   host migration, no CRDT, and no multi-writer merge in v1.
4. **Visibility lives in the schema, not the renderer.** The host's `project`
   returns a validated `S` with invisible entities *omitted* and hidden fields
   explicitly `null` — a `crew` caller never receives the host's `command`
   field. A zero must never mean "you cannot see this".
5. **The store shares references; `bindEntities` spends that.** An entity whose
   reference did not change is skipped, so a 60 Hz loop over a world where one
   ship moved does one update. Never rebuild a mesh whose entity is unchanged,
   and always dispose what leaves the world.

## Workflow

1. **Get the vocabulary right.** Read `concepts.md`. Confirm the user is
   building a browser client (not a native node); if not, this is the wrong
   skill.
2. **Define the store.** `defineStore({ id, version, state, empty, actions,
   inputs })` — validators only, so a joining bundle never carries the owner's
   gameplay code. `empty()` is *absence*, and it must itself pass `state()`.
3. **Host it.** `hostStore({ definition, transport, initialState, maxEventBytes,
   authorize, project, actions, inputs })` on the origin's session.
4. **Join it.** `joinStore({ definition, transport, host, audience, key,
   maxEventBytes })`, then `await handle.ready()` before rendering a consistent
   view. `joinStore` resolves as soon as the join is *sent*.
5. **Bind the scene.** `bindEntities({ store, scene, select, binding })` from
   `@net-mesh/browser/three`. `create` builds once per id, `update` runs only on
   a changed reference, `remove` disposes geometry and materials.
6. **Drive local state locally.** Camera, menu, interpolation, prediction stay
   in an ordinary local store. Do not replicate Three.js objects or React state
   through the mesh.
7. **Verify against a real browser.** `testing.md` — the in-page `?mode=local`
   demo is not evidence that two browsers can play.
