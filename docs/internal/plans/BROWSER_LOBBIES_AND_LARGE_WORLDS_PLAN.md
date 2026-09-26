# Browser lobbies and large worlds — plan

**Status:** proposed. Nothing here is implemented unless §2 says so.
**Scope:** `@net-mesh/browser` (the store and its Three.js binding), the browser
anchor, and a new world layer on the mesh. Companion to
[the browser game store design](BROWSER_GAME_STORE_API_DESIGN.md) and
[the browser WebRTC transport plan](BROWSER_NATIVE_WEBRTC_TRANSPORT_PLAN.md).
**Source baseline:** `93b224799` (master after the org-nrpc merge).
**Audience:** the team, before any of this is built.

**Supersedes one decision.** The store design (§5, "Snapshot, deltas and
audiences") says: *"A minimal spatial selector is application code, not a new
interest-management service."* That holds for a room of a few players. It does
not hold for worlds larger than what one player should receive, which is the
goal of this plan. §5 below proposes interest management as a store feature.

---

## 1. Goal

A Three.js developer can build a multiplayer game that a player joins **by
opening a link**, in a world that can be **larger than any one player loads**:

1. **Drop-in lobbies.** No per-player setup, no hand-minted credentials, no
   repository checkout. A lobby list or a room code, and you're in.
2. **Large worlds.** A player receives only the part of the world near them.
   Beyond what one host can hold, the world is split across several hosts, and
   crossing between them is seamless.
3. **Built with an AI agent.** Every step is expressible in the `net-browser`
   skill, so a developer who vibe-codes gets a correct result.

### Non-goals (for this plan)

- A general physics or simulation engine. The store replicates validated game
  state; simulation stays game code.
- Deterministic lockstep or CRDT multi-writer state. One authority per piece of
  the world, as today.
- Matchmaking by skill or rating. A lobby list and room codes only; ranking is
  application code on top.

---

## 2. Where we are (verified at the baseline)

What works, with the code that backs it:

| Capability | Where |
|---|---|
| Authoritative host + joiners, late joiners get a chunked snapshot | `browser-ts/src/store/{host,join,owner,chunker,assembly}.ts` |
| Actions (correlated, answered) vs inputs (coalesced, newest wins) | `store/join.ts` `act` / `input`; dispositions in `store/types.ts` |
| Caller identity proven by the transport; `authorize` sees `request.peer` | `store/types.ts` `AccessRequest`, `store/owner.ts` |
| Per-audience projection; hidden data never sent | `store/owner.ts` `project`, one diff per distinct audience |
| Several named stores per node (`store: 'lobby'` / `'match'`) | `store/host.ts` (duplicate name on one transport → `invalid-data`) |
| Three.js binding: create / update-only-on-change / remove / dispose | `browser-ts/src/three/index.ts` |
| Discovery: announce a tag, `query(tag)` | `browser-ts/src/node.ts` |
| Direct WebRTC between players; the anchor only introduces | leaf + `node.ts` `connectPeer` |
| An in-page transport for offline development (not exported) | `browser-ts/demo/local-mesh.js` |
| Daemon placement and migration on native nodes | [Daemons and placement](https://ai2070.net/docs/guides/daemons-and-placement), [Continuity and migration](https://ai2070.net/docs/guides/continuity-and-migration) |

What stands in the way:

| # | Gap | Evidence |
|---|---|---|
| G1 | **No installable anchor admits browsers.** `net-mesh anchor serve` registers no enrollment service, so a page's `connect()` times out. Only the `examples/browser-demo/host` example serves enrollment. | `cli/src/commands/anchor.rs` `run_serve`; `browser-ts/demo/README.md` |
| G2 | **Credentials are minted by hand** (`net-mesh anchor credential mint`) or by the demo host's `/config?tab=N`. | `cli/src/commands/anchor.rs` |
| G3 | **Closed:** `createLobby` / `listLobbies` / `joinLobby` (`src/lobby.ts`). Was: **No lobby API.** Every game re-implements re-announce loops, query-until-present polling, ready, enlist, and a ~40-line wrapper so the host can play in its own world. | `browser-ts/demo/main.js` |
| G4 | ~~**The host player can't join its own store** (`invalid-data`); the workaround is application code.~~ **Closed:** `hostPlayer(host, { audience })`. | `store/player.ts` |
| G5 | ~~**The offline transport isn't in the package**, so prototyping needs a clone of the repo.~~ **Closed:** `@net-mesh/browser/local` (`createLocalMesh`). | `src/local.ts` |
| G6 | **The host leaving ends the world.** Replicas get `owner-lost`, which is terminal; there is no migration. By design for v1. | `store/errors.ts`; store design §1 |
| G7 | **No fallback when UDP is blocked.** `udp-blocked` is classified, but nothing relays. `iceServers` accepts TURN credentials; provisioning and fallback are not built. | `browser-ts/src/node.ts`, `udp-probe.ts` |
| G8 | **Every state change re-projects the whole world, once per distinct audience set.** Cost ∝ distinct views × world size. | `store/owner.ts` (one projection pair + root diff per distinct audience) |
| G9 | **Changing audience blanks the view.** The old subscription is fenced, the view shows `empty()`, then a full snapshot arrives. Using cells as audiences, a player crossing a border sees the world vanish. | `store/owner.ts` `aud`; store design §5 |
| G10 | ~~**`project` doesn't know the player.**~~ **Closed:** `projectFor(state, { peer, audience })`, an alternative to `project` (exactly one), keyed per player so `project`'s per-audience sharing is unchanged. | `store/owner.ts`, `store/host.ts` `HostProjection` |
| G11 | **Hard bounds sized for rooms:** 32 audience labels per handle, 1 MiB snapshot, 64 KiB store message. | store design §5 "Bounded defaults" |
| G12 | **The store only runs in a browser today**, and a player-host sees the whole world (it can read hidden state and modify its own client). Whether the store can run on a Node transport is **unverified**: `sdk-ts` has a `MeshNode`, but nobody has checked it against `StoreTransport` (`nodeIdHex`, `openStream({reliability, peer, label})`, `onEvent`). | `store/host.ts` `StoreTransport`; `sdk-ts/src/mesh.ts` |

---

## 3. Phases at a glance

| Phase | Delivers | Closes | Size | Depends on |
|---|---|---|---|---|
| **P0** Foundation | A browser-capable anchor you can install; automatic per-visitor credentials | G1, G2 | M | — |
| **P1** Drop-in lobbies | Lobby API in the package, host-player helper, presence, exported offline transport | G3, G4, G5 | M | P0 (P1's offline parts don't) |
| **P2** Interest management + hidden information | Only nearby state reaches each player, on one host; secrets declared, not hand-projected | G8, G9, G10, G11 | M–L | — |
| **P3** Dedicated hosts | The store runs on a native node; lobbies and regions survive players leaving | G6, G12 | M | Node transport (open question Q1) |
| **P4** Large worlds | The world is split across region hosts with seamless crossing | — | L | P2, P3 |
| **P5** Reach | TURN fallback, reconnection hardening, region-aware anchors | G7 | M | P0 |

P2 needs nothing from P0 or P1 and can start first, to de-risk the store changes.
P1's offline work (exporting the local transport, the host-player helper) also
needs nothing and is the fastest visible win.

---

## 4. P0 + P1 — drop-in lobbies

### P0. A browser anchor and credentials

- **CLI:** `net-mesh anchor serve --browsers` (name to decide) that serves the
  enrollment service the demo host serves today, shipped in the normal CLI
  release. Acceptance: a page built from the npm package connects to it and
  `isEnrolled()` is true, with no repo checkout.
- **Credential endpoint:** an HTTP route on the anchor (or a documented
  companion) that issues one credential per visitor. Policy is a decision
  (Q3): anonymous with rate limiting, or behind the game's own login via a
  signed request. The demo's `/config?tab=N` is the prototype.
- **Docs + skill:** the README's "Before players can connect" and SKILL.md's
  fast path switch from the demo host to this.

### P1. The lobby API (proposed)

A thin layer over what exists; it adds no protocol.

```ts
// Host side: one call instead of hostStore + announce loop + host-player wrapper.
const lobby = await createLobby({
  node, definition, initialState, actions, inputs, authorize, project,
  game: 'my-game',                 // namespace for discovery
  name: 'Friday arena',
  capacity: 8,
  visibility: 'public',            // or 'unlisted' (room code / link only)
});
lobby.code;                        // short room code
lobby.link;                        // shareable URL
lobby.self;                        // the host's own player handle — same shape as a replica
lobby.players.subscribe(list => …); // presence, from the host's authority

// Anyone: browse or join by code/link.
const lobbies = await listLobbies({ node, game: 'my-game' });  // name, players, capacity, code
const game = await joinLobby({ node, definition, code });      // or { lobby: lobbies[0] }
await game.ready();
```

- **Discovery:** lobby metadata rides the announcement (tag plus a small
  metadata record), re-announced on a timer the helper owns. Stale lobbies
  vanish when announcements lapse. Concretely (Q4), with no protocol change —
  capability tags are free-form strings, only `causal:`/`fork-of:`/`heat:`/
  `scope:` are reserved, and the core imposes no tag length:
  - `net-lobby:<game>` — the queryable listing tag, public lobbies only;
  - `net-lobby:<game>:rec:<base64url JSON>` — the record: version, code, name,
    players, capacity, store `id@version`, and app `info` (≤ 256 bytes of
    JSON); the whole tag ≤ 512 bytes. Public lobbies only;
  - `net-lobby:<game>:code:<sha-256(code), 32 hex>` — how a code is looked up;
    both kinds announce it. Unlisted means *not listed*, not *secret*: a
    6-character code can be brute-forced against the hash, and the anchor sees
    every announcement. Access control is the host's `authorize`.
- **Capacity and kick** are enforced in the `authorize` the helper wraps around
  the game's, counted from the owner's live handles (not from guesses about
  who joined), and a kick re-authorizes installed handles at once rather than
  at the next change.
- **The host's own player (G4):** `lobby.self` is the demo's wrapper made
  official: the same `authorize`, the same handlers, the same handle shape as a
  replica, so game code doesn't branch on "am I the host". **Done** as the
  standalone `hostPlayer(host, { audience })` (it reads `authorize`, the
  handlers and `project` from the host, so nothing is passed twice);
  `lobby.self` will wrap it.
- **Capacity and kick:** enforced in the host's `authorize` generated by the
  helper (full → `forbidden` with a reason; kick = peer deny-list).
- **Presence:** join/leave derived from the host's handles, exposed as a store
  the UI can bind to.
- **Offline (G5):** **done** — `@net-mesh/browser/local` exports
  `createLocalMesh()`; `createLobby`/`joinLobby` must run on it in one page
  with no network. Local nodes `announce`/`query` with the leaf's lease
  semantics, so lobby discovery can run offline too.

**Acceptance:** the demo becomes a lobby-based game in fewer lines than today;
two browser profiles on one machine can list, join by code and play against a
P0 anchor; the whole flow is in the skill and a fresh agent reproduces it.

---

## 5. P2 — interest management on one host

**Goal:** a player receives only the entities near them, and the host's cost
per change is proportional to what changed, not to the world × the number of
views.

### What changes in the store

1. **Interest keys instead of audience-as-cells.** A replica declares an
   *interest set* (e.g. the cells around it) separately from its audience
   (what it may see). Audience stays the permission; interest becomes the
   spatial filter. This keeps the 32-label bound meaningful for permissions and
   gives interest its own, larger bound.
2. **A spatial index on the host.** The definition says how to key an entity
   (e.g. `cellOf(entity) → string`). The host maintains entity → cell and
   cell → entities incrementally from each commit's diff.
3. **Dirty tracking.** A commit marks the cells its changes touch. Only
   replicas interested in a dirty cell get a delta, and only for those cells.
   The per-commit cost becomes ∝ changed cells × interested replicas, not
   world × views (closes G8).
4. **Additive interest changes (closes G9).** Changing interest sends only the
   difference: entities of newly entered cells as adds, entities of left cells
   as removes. The view never blanks. Permission (audience) changes keep today's
   fence-and-resnapshot semantics, because narrowing permission must never
   leave stale private data visible.
5. **The player in `project` (closes G10).** **Done** as `projectFor(state,
   { peer, audience })` beside `project` rather than a changed signature:
   the per-audience cache stays for `project`, and the per-player cost is
   opted into by name.
6. **Hysteresis.** Leaving a cell drops it only after the player is a margin
   away, so standing on a border doesn't churn.
7. **Update rate by distance (later).** Near cells every commit; far cells
   coalesced to a lower rate. Needs measurement first; not v1 of P2.
8. **Bounds (G11).** Re-derive the limits for interest sets and snapshot sizes
   from measurements on a large demo scene, not by guessing.

### Proposed API

```ts
const world = defineStore({
  id: 'my-game.world', version: 1, state, empty, actions, inputs,
  interest: { key: entity => cellOf(entity.x, entity.z) },   // opt-in
});

const replica = joinStore({ …, interest: cellsAround(myX, myZ, 1) });
replica.setInterest(cellsAround(x, z, 1));   // additive, no blank frame
```

`bindEntities` needs no change: it already diffs by reference, and additive
interest produces ordinary adds and removes.

**Acceptance:**
- A demo world with thousands of entities: each player's received bytes are
  proportional to entities near them.
- The host's per-commit work is proportional to dirty cells (measured with the
  existing counters).
- Crossing a cell border shows no empty frame, verified in the browser matrix.
- Hidden-information guarantees are unchanged: a witness that a permission
  narrowing still removes private data immediately.

### Hidden information, modeled (part of P2)

**Why.** Enforcement is already right: `project` runs on the host and what it
omits is never sent, and a throwing handler answers the caller with the code
alone — the handler's message never leaves the host (`store/owner.ts`, the
`action-rejected` refusal carries no text). The weakness is authoring. The
developer writes `project` by hand, and every mistake is a **silent** leak: the
game works and the secret is readable in the browser. The common ones:

- returning `state` unchanged — which the README and skill examples do today,
  for brevity;
- forgetting a nested field when copying;
- a plausible value (`0`, `""`) standing for "you cannot see this";
- per-player secrets (your own hand), awkward because `project` does not know
  the viewer (G10).

For AI-written games this is the worst class of bug: nothing fails.

**What to add** — one enforcement point (`project`), generated rather than
hand-written:

1. **Declarative visibility on the definition.** Paths with a visibility rule;
   the SDK builds the projection from them, and it composes with interest keys
   (visibility = who *may* see, interest = what is *near*).

   ```ts
   defineStore({
     …,
     visibility: {
       'players.*.hand': 'owner',      // only the player whose key it is
       'deck':           'nobody',     // host only
       'deck.length':    'everyone',   // reveal a count, not contents
       'units.*':        'team',       // viewer's team, from an app-provided resolver
       'waypoint':       ['command'],  // an audience
     },
   });
   ```

   Needs the viewer in `project` (G10). A hand-written `project` stays
   supported; when both exist, the declared rules apply **after** it, so a
   hand-written projection can narrow but never widen a declared secret.
2. **Private stays private by construction.** Unlisted paths keep today's
   behavior, so nothing existing changes. A path marked private is removed
   (collections) or set to the typed hidden marker (fields) — never a
   plausible default — and `empty()` is checked to satisfy the same rules.
3. **Proof tools.**
   - `assertHidden(definition, state, { viewer }, paths)` for tests, and an
     executable example in the skill.
   - A dev-mode check: when the host projects for viewer A, flag any
     `owner`-scoped data belonging to B. Off in production.
4. **Standard patterns as helpers:** reveal-count-not-contents; reveal-on-event
   (a card becomes `everyone` when played — a state change, not a rule
   change); a typed `hidden` marker instead of magic zeros.
5. **The trust boundary, stated in the API.** `host.trust` is `'player'` or
   `'dedicated'` (P3). Docs say plainly: a player-host sees everything, so games
   with real stakes need a dedicated host. No SDK feature can change this, and
   the SDK should not imply otherwise.

**Defaults.** Principle: *compatible by default, loud when a secret could
leak.* A strict default that hides data makes a game look broken and pushes the
developer to disable it; a leak looks fine. So the defaults force an explicit
choice without breaking anything:

1. **No `visibility` → today's behavior.** Existing stores are untouched; this
   is not a breaking change.
2. **`owner` needs no configuration.** The standard pattern keys each player's
   entity by the proven caller (`ships[context.peer]`), so by default `owner`
   means *the map key equals the viewer's peer id*. `ownerOf(entity)` overrides
   it for other layouts.
3. **Rules inherit and cannot be widened.** A rule on `players.*.hand` covers
   everything beneath it; a hand-written `project` may narrow it further, never
   widen it.
4. **One hidden form.** Collections drop hidden entries; hidden fields become
   the typed `hidden` marker — never `0`, `""` or `null`.
5. **Unknowable inputs fail at definition time.** `team` without a team
   resolver (and any other rule needing app data) throws in `defineStore`, not
   at first projection.
6. **Presets for common game types** — a small closed set an AI agent can pick
   correctly, each shipped only with an executed example:

   ```ts
   visibility: 'open'                                         // everyone sees everything — explicit
   visibility: 'card-game'                                    // hands owner, deck nobody + count everyone, table everyone
   visibility: { preset: 'card-game', 'players.*.score': 'everyone' }   // preset + overrides
   ```

   Later presets (team game; fog of war composed with interest keys) land only
   with their examples.
7. **Development-mode warnings, on by default:**
   - A store that sends the entire state to every player *without saying so*
     warns once: "every player receives the full state; declare
     `visibility: 'open'` if intended". An explicit `'open'` silences it —
     turning an accident into a decision.
   - The cross-player leak check (item 3 above) runs in development, off in
     production.
   - Secrets declared on a player-hosted store remind once that the hosting
     player can see them (`host.trust === 'player'`).

**Defaults deliberately avoided:**
- **Guessing from names** (hiding fields called `hand`, `secret`, `deck`): too
  magical, and wrong often enough to create confusing bugs.
- **Deny-everything once any rule is declared:** safe in theory, but games lose
  their world state and developers reach for `'open'` just to make it work,
  defeating the point.

**Deliberately not in scope:**
- **Line of sight / fog of war as computation.** Whether a unit sees another
  is game logic; the SDK makes "visible to this viewer" easy to express, it
  does not compute it.
- **Traffic analysis.** A player can infer that something visible to them
  changed from updates arriving. Hidden-only changes already produce no
  per-audience diff; padding or constant-rate updates are not worth their
  cost for games in v1. Revisit only for a concrete threat.
- **Action results.** An output goes to its caller only — already the right
  scope.

**Acceptance:**
- A card-game example (hands `owner`, deck `nobody` + count `everyone`,
  table `everyone`) written with no hand-written `project`; `assertHidden`
  proves each player sees only their own hand, over the real wire.
- Removing a visibility rule reddens that test; the dev-mode check reports a
  deliberate cross-player leak in a fixture.
- The README and skill examples move from `project: state => state` to a
  declared rule, with an executed example.
- Decision Q7 (§10) resolved before the API freezes.

---

## 6. P3 — dedicated hosts

**Why:** a player's tab is a fragile, all-seeing host. Lobbies that outlive
their creator, and every region in P4, need a host that is not a player.

- **Q1 first:** can the store (plain TypeScript) run in Node over `sdk-ts`'s
  `MeshNode`, or is a small `StoreTransport` adapter needed? If neither, a
  native Rust store host is a much larger project, which changes P4's cost.
- **A dedicated host** runs the same `hostStore` with game rules loaded from the
  developer's code, on a native node, discoverable by the same lobby tags.
- **Browser players become pure replicas** of it, so the "host sees
  everything" problem is gone and hidden information holds against everyone.
- **Survival:** a dedicated host is a mesh daemon, so placement and migration
  apply (docs linked in §2). A player-hosted lobby still ends with its host
  (G6); **host migration between players is deferred** (§9).

---

## 7. P4 — large worlds across region hosts

**Model:** the world map is divided into regions. Each region is its own store
(`store: 'region:4:7'`), hosted by a dedicated host (P3). A player's client
holds replicas of its current region and its neighbours.

### Pieces

1. **Region directory = discovery.** A region host announces
   `my-world:region:4:7`; clients and other hosts find it with `query`. No
   central directory service. This is the property no game SDK has, and why it
   belongs on the mesh.
2. **Client world view.** A `joinWorld({ node, world, position })` helper keeps
   replicas of the current region plus neighbours (with P2 interest inside
   each), joins and leaves as the player moves, and exposes **one merged view**
   the Three.js binding renders (`bindEntities` over the merged state).
3. **Entity handoff.** Crossing a border moves an entity's authority from
   region A to region B:
   - A freezes the entity at a fenced epoch and sends its state to B.
   - B admits it at the next epoch and becomes authoritative; A keeps a
     read-only ghost until B confirms.
   - Late inputs to A for that entity are forwarded to B or refused typed, never
     applied twice.
   - The store's existing incarnation and ledger fencing is the base. The
     protocol (and its failure cases: B down, A dies mid-handoff) needs its own
     design and model-checking, like the org-streaming lifecycle got.
4. **Cross-border actions.** Firing from A into B is forwarded host to host over
   the mesh; the target's authority (B) decides. Result goes back through A.
5. **Ghosting.** Entities near a border are mirrored read-only into the
   neighbour so interactions and rendering across the line work.
6. **Load balancing.** Split a busy region, merge quiet ones, move a region host
   to another machine using daemon placement and migration. Splitting is a
   handoff of many entities at once, so it reuses item 3.

**Acceptance:**
- A walkable world of several regions on several native hosts; a player
  crosses borders with no visible pop or duplicate entity.
- An action across a border resolves exactly once.
- Killing a region host mid-handoff yields a typed, recoverable outcome (no
  duplicated or lost entity), proven by a deterministic simulation test before
  any browser test.
- Region hosts move between machines while players stay connected.

---

## 8. P5 — reach and resilience

- **TURN (G7):** provision TURN servers with the anchor (or document bringing
  your own), pass them as `iceServers`, and fall back automatically after a
  classified `udp-blocked`. Needs an end-to-end test on a UDP-blocked network
  (the natsim harness is the candidate).
- **Reconnection:** the lobby and world helpers use `replica.reconnect()` so a
  mobile player's brief drop resumes rather than rejoins.
- **Region-aware anchors:** once several anchors exist, the lobby layer picks
  the nearest (the mesh's proximity data applies).

---

## 9. Deliberately deferred

- **Host migration between players (G6).** Electing a new player-host and
  moving authority without a dedicated host is the hardest version of the P4
  handoff with the least reliable participants. Dedicated hosts (P3) cover the
  need first; revisit only with demand.
- **Unreliable replica updates.** The store design already defers a separate
  unreliable replica protocol until measurements demand it; P2's distance-based
  rates come first.
- **Anti-cheat beyond authority.** Server authority plus hidden information is
  the baseline; client-side cheat detection is out of scope.

---

## 10. Decisions needed from the owner

| # | Question | Why it matters |
|---|---|---|
| Q1 | Run the store in Node over `sdk-ts`, or build a native store host? (Spike first.) | Sets the cost of P3 and P4 |
| Q2 | CLI anchor mode vs a hosted anchor service (or both)? | P0 shape; business model |
| Q3 | Credential policy: anonymous + rate limit, or game-login-signed requests? | Abuse surface |
| Q4 | ~~Lobby metadata on announcements: size limit and what's public~~ **Decided (2026-09-26):** a fixed record (name, players, capacity, code, store version) plus small developer-defined fields, under a byte cap; unlisted lobbies publish no record. | Privacy of unlisted lobbies |
| Q5 | ~~Interest keys: cells only, or arbitrary keys?~~ **Decided (2026-09-26):** arbitrary string keys; grid cells are one helper (`cellsAround`). | P2 API generality |
| Q6 | Handoff consistency target: at-most-once with typed failure, or exactly-once? | P4 protocol complexity |
| Q7 | **Decided (2026-09-26):** path rules plus presets (`open`, `card-game` first), development warnings behind an explicit `dev` flag rather than `NODE_ENV`; `team` resolution still to specify. Original question — Visibility API and defaults: path strings (as sketched) or schema annotations (e.g. a validator wrapper `secret(owner, schema)`)? How is `team` resolved — an app callback? Which presets ship first (proposed: `open`, `card-game`)? How is "development mode" detected — an explicit `dev` flag, or the bundler's `NODE_ENV`? | P2 hidden-information API shape and defaults |

---

## 11. Competitive context (brief)

Hosted room servers (Colyseus, Photon, Nakama, PartyKit) ship lobbies,
matchmaking and reconnection today but are services with their own identity.
Web drop-in kits (Playroom, Rune) nail "open a link and play" on a proprietary
cloud. P2P libraries (Trystero, PeerJS) are drop-in but have no authority or
hidden information. Replicated computation (Croquet) shows everything to
everyone. Protocol peers (libp2p, Iroh) stop at transport. SpatialOS-style
spatial sharding existed only as an expensive centralized cloud.

This plan's differentiator is the combination: open protocol, proven identity
feeding authority, hidden information enforced before sending, **and** a world
whose regions are discovered, placed and migrated on the same mesh that lets a
game call AI characters or native services. P1 makes the entry point
competitive; P4 is the part that is hard to copy.

(Landscape as of mid-2026, from memory — refresh before external use.)

---

## 12. Risks

- **Handoff correctness (P4)** is the dominant risk. Mitigation: a
  deterministic model and simulation tests before browser tests, as the
  org-streaming lifecycle did.
- **Store performance under P2** could hit JavaScript limits on the host at
  large entity counts. Mitigation: measure on the demo early; P3's native host
  is the escape hatch if Node suffices, a Rust store if not (Q1).
- **Scope creep toward a game engine.** The non-goals in §1 are the guard.
- **Skill drift.** Each phase lands with its `net-browser` skill update and an
  executed example, as the current skill fixes did; otherwise vibe-coded games
  break first.
