# Browser lobbies and large worlds — plan

**Status:** in progress. P1, P2 and the P3 transport are built on the
`browser-host-player` branch (§2). **Next: P0, a shared-ready CLI anchor**, then
**netcode model 2** (§6). Direction re-set on 2026-09-26 — see §3.
**Scope:** `@net-mesh/browser` (the store, its Three.js binding, and optional
netcode modules), the browser anchor, the leaf's transport primitives, and a world
layer on the mesh. Companion to
[the browser game store design](BROWSER_GAME_STORE_API_DESIGN.md) and
[the browser WebRTC transport plan](BROWSER_NATIVE_WEBRTC_TRANSPORT_PLAN.md).
**Source baseline:** `93b224799` (master after the org-nrpc merge).
**Audience:** the team.

**Supersedes two earlier decisions.**
- The store design (§5, "Snapshot, deltas and audiences") said *"A minimal
  spatial selector is application code, not a new interest-management
  service."* That holds for a room of a few players, not for worlds larger than
  one player should receive. Interest management is now a store feature (§5).
- This plan's own first non-goal, "no deterministic lockstep", is now **parked
  and revisited on demand** (§1, §9) rather than ruled out.

---

## 1. Goal

A Three.js developer can build a multiplayer game that a player joins **by
opening a link**, in a world that can be **larger than any one player loads**,
with movement that **feels responsive at real latency**:

1. **Drop-in lobbies.** No per-player setup, no hand-minted credentials, no
   repository checkout. A lobby list or a room code, and you're in.
2. **Large worlds.** A player receives only the part of the world near them.
   Beyond what one host can hold, the world is split across several hosts, and
   crossing between them is seamless.
3. **Responsive movement.** Interpolated remote entities, predicted local ones,
   reconciliation to the host, and fair hit judgement — netcode model 2 (§6).
4. **Built with an AI agent.** Every step is expressible in the `net-browser`
   skill, so a developer who vibe-codes gets a correct result.

**Audience and language.** This audience writes TypeScript/JavaScript, so this
plan invests in **`@net-mesh/browser` and the Node SDK (`sdk-ts`) only**. The
Python, Go and C surfaces already committed on the branch (the stream inbox, §7)
stay as they are, with no further investment here.

### Non-goals (for this plan)

- A general physics or simulation engine. The store replicates validated game
  state; simulation stays game code.
- CRDT multi-writer state. One authority per piece of the world, as today.
- Matchmaking by skill or rating. A lobby list and room codes only; ranking is
  application code on top.
- **Parked, not ruled out:** rollback netcode, deterministic lockstep and
  client-authoritative dead reckoning (netcode models 3–5, §6). They are
  revisited only when a game needs them.

---

## 2. Where we are (updated 2026-09-26)

Built on the `browser-host-player` branch, each with tests (most checked by
deliberately breaking the code), docs, skill and changelog:

| Capability | Where |
|---|---|
| Authoritative host + joiners, chunked snapshots, actions vs inputs | `browser-ts/src/store/*` (pre-existing) |
| The host's own player, replica-shaped and held to the same policy | `store/player.ts` `hostPlayer` |
| Offline development: a mesh in one page, with discovery | `@net-mesh/browser/local` `createLocalMesh` |
| Per-player projection | `projectFor(state, { peer, audience })` |
| Lobbies: list, room code, link, capacity, kick, presence | `src/lobby.ts` |
| One host hook for players: `join` / `leave` / `area` | `onEvent`, `areaOf` |
| Inventory bones (trading deferred) | `store/inventory.ts` |
| Declared visibility: path rules, presets, `HIDDEN`, `assertHidden`, dev warnings | `store/visibility.ts` |
| Interest management: keyed entities, per-entity deltas, additive `setInterest` | `store/interest.ts`, `int` wire message |
| A world-scale benchmark harness | `browser-ts/scripts/bench-world.mjs` |
| A native host for the browser store (dedicated hosts) | core `register_stream_inbound`; `sdk-ts` `meshStoreTransport` |
| Browser ↔ native witness: a page's labeled stream attributed to the page | runner `stage5.rs` |

Gaps, with their state:

| # | Gap | State |
|---|---|---|
| G1 | No installable anchor admits browsers (`net-mesh anchor serve` serves no enrollment) | **Open — P0** |
| G2 | Credentials are minted by hand | **Open — P0** |
| G3 | No lobby API | Closed (`src/lobby.ts`) |
| G4 | The host player can't join its own store | Closed (`hostPlayer`) |
| G5 | The offline transport isn't in the package | Closed (`@net-mesh/browser/local`) |
| G6 | The host leaving ends the world | Dedicated hosts address it (P3); host migration between players deferred |
| G7 | No fallback when UDP is blocked | Open — P5 (deferred) |
| G8 | Every change re-projects the whole world per view | **Partly closed.** Per-player bytes are O(nearby); host cost is still O(world) per commit — §8 |
| G9 | Changing audience blanks the view | Closed for interest (additive `setInterest`); audience changes still resnapshot by design |
| G10 | `project` doesn't know the player | Closed (`projectFor`) |
| G11 | Bounds sized for rooms | Partly: interest has its own bounds; one-frame deltas remain (§8) |
| G12 | The store only runs in a browser | Closed: runs in Node over `meshStoreTransport` |
| G13 | **The leaf has only a reliable, ordered DataChannel** | **Open — netcode model 2 (§6)** |
| G14 | **The host's commit validates the whole world** (15.6 ms at 8,000 entities, no players) | **Mitigated — §8: `setEntities` 2.2 ms; per-player views O(changed) (owner, 16 players 459 → 25 ms); O(nearby) open** |

---

## 3. Direction (decided 2026-09-26)

- **TypeScript only** for this audience (§1). Other bindings on the branch are
  kept, not extended.
- **Simple identity:** anonymous per-visitor credentials, and the player's key
  kept in `localStorage` (`rememberedIdentity()`). Games run on `connect()`,
  which persists nothing by itself (§4).
- **Anchors:** a **CLI anchor first, built shared-ready from day one** — game-id
  namespacing, per-game credential issuance and rate limits, per-game counters,
  stateless credential checks — so a **shared anchor** follows without rework
  (§4).
- **Next netcode milestone: model 2** (snapshot interpolation, prediction,
  reconciliation, lag compensation) on two new Rust primitives: an **unreliable
  latest-value channel** and **clock sync with round-trip and jitter estimates**
  (§6).
- **Rollback, lockstep and client-authoritative dead reckoning are parked** until
  a game needs them (§6, §9).

### Phases

| Phase | Delivers | Closes | Size | State |
|---|---|---|---|---|
| **P0** Shared-ready CLI anchor | Browser-capable anchor, anonymous per-visitor credentials, game-id namespacing, per-game limits and counters | G1, G2 | M | **Next** |
| **P1** Drop-in lobbies | Lobby API, host-player helper, presence, offline transport | G3–G5 | M | Built |
| **P2** Interest + hidden information | Nearby-only delivery; declared secrets | G8–G11 (part) | M–L | Built (host cost open, §8) |
| **P3** Dedicated hosts | The store on a native node; persistence (§7) | G6, G12 | M | Transport built; persistence open |
| **Netcode model 2** | Unreliable channel, clock sync, host tick loop, interpolation/prediction module | G13 | L | After P0 |
| **Host commit cost** | Incremental validation; maintained spatial index | G14, G8 | M | Parallel to model 2 |
| **P4** Large worlds | Region hosts, handoff (at-most-once) | — | L | After model 2 |
| **P5** Reach | TURN fallback, region-aware anchor endpoints | G7 | M | Deferred |

### Release sequence

1. **The CLI anchor** (P0), in the normal CLI release.
2. **The two-browser acceptance run** against it: two browser profiles on one
   machine list, join by code and play through the published-shape package.
3. **Publish `@net-mesh/browser`** (first release 0.37.0; `release-npm-browser.yml`).
   The leaf's wasm and the package are released together; the unreliable
   channel (§6) changes the leaf's wasm boundary, so from then on a package
   release pins the leaf it was built with.
4. **The shared anchor**, the same code run for many games.

---

## 4. P0 — a shared-ready CLI anchor, and identity

**Goal:** `net-mesh anchor serve --browsers` (name to decide) admits browsers and
issues their credentials, built so that the same code runs as a **shared anchor**
for many games without rework.

### Identity: anonymous, per browser profile

- The page asks the anchor's credential endpoint for a credential; the anchor
  issues one per visitor, with **no login**. The player's key is kept in
  `localStorage` by `rememberedIdentity()` and passed to `connect()`. Games must
  use `connect()`: a store needs a session with its host, installed by
  `connectPeer`, which exists only on the `connect()` node, not on
  `openSession()`'s (which does persist its own key, in IndexedDB). Measured
  2026-09-26 in the acceptance run.
- **What that identity is**, stated in the docs: one player per **browser
  profile**, not per person. Clearing site data makes a new player; the same
  person on two devices is two players. `authorize`, `context.peer` and
  inventories all key on it. That is the right trade for drop-in games.
- **Room to grow:** a game that later needs accounts adds game-login-signed
  credential requests (Q3's other option) without a protocol change — the
  credential format already carries what the anchor checks.

### Shared-ready from day one

1. **Game-id namespacing, enforced, not declared.** Each credential **carries
   its game id**. The anchor refuses announcements, lobby tags
   (`net-lobby:<game>…`) and routing outside the credential's game. Tags alone
   would be honour-system: any node can announce any tag (§5, lobbies).
2. **Per-game credential issuance and rate limits** — a pool per game, not one
   global pool, keyed on the game id in the credential, which a client cannot
   forge.
3. **Per-game counters now, metering later.** Cheap counters per game (sessions,
   routed bytes, credential issues, refusals) are what make per-game limits
   enforceable. Billing and quotas wait; the counters do not.
4. **Stateless credential checks.** Credentials are self-contained and signed,
   so any anchor instance can verify any credential. Discovery state
   (announcements) already federates over the mesh, so one anchor can become
   several behind a region-aware endpoint later (P5) without redesign.

### Announcement lifetime

The leaf keeps an announcement for its TTL (300 s default), while earlier docs
said announcements vanish "in seconds" (likely the anchor's forwarding). The
lobby helper re-announces every ~2 s, which is correct under either behaviour.
**Measure once against the P0 anchor; investigate only if the anchor really
drops announcements in seconds.**

**Status: P0 done** — slice 1, slice 2 and the acceptance run below.

**Acceptance:** a page built from the npm package connects to a freshly
installed CLI anchor and `isEnrolled()` is true, with no repo checkout; a
credential for game A cannot announce a lobby for game B; per-game counters and
limits are visible; README "Before players can connect" and the skill's fast
path switch to it.

### Slice 1 — built (2026-09-26)

- **SDK `game_anchor`:** `GameRegistry` — games by id, each with an enrollment
  root derived from the anchor secret (`from_identity`: from the issuer key),
  so instances with the same key agree with no shared state. Invites are
  **self-verifying** (nonce = random ‖ deadline ‖ keyed MAC), rebuilt from the
  join request alone. Per-game issuance ceiling per minute; counters
  (credentials issued/refused, enrollments admitted/refused).
- **An invite binds to its first device.** The same device may re-enroll with
  it (a promoted leader tab reuses the credential `openSession()` was opened
  with — a one-shot invite would leave it provisional); another device is a
  replay. One credential = one identity, which is what makes issuance limits
  meaningful. Invites therefore live 12 h. Bindings are per instance (pruned at
  the deadline); strict cross-instance binding needs a shared map (P5).
- **`POST /credential {game}`** on the bootstrap listener
  (`BootstrapConfig.credential_issuance`), per-IP ceiling, typed refusals
  (`unknown_game`, `rate_limited`, `malformed_request`); the listener refuses to
  start if the issuing key is not the issuer it verifies.
- **CLI:** `anchor serve --issuer-identity … --game ID[:N]`,
  `--credentials-per-minute`, `--game-stats-secs`.
- **`@net-mesh/browser`:** `requestCredential({ anchorUrl, game })`.
- **Witnessed:** registry unit tests (roots, MAC, expiry, binding, per-game
  limits), listener route tests, CLI flag tests, package tests — each security
  check shown to fail when removed. **Not yet:** a real browser enrolling
  through it (the two-browser acceptance run), so README/skill still point at
  the demo host.
- **Follow-up:** `openSession({ credential: () => Promise<string> })` — fetch a
  fresh credential per connect, so a promotion never depends on an old invite.
  A leaf change; not needed while invites bind to their device.

### Acceptance run — passed (2026-09-26), gated in CI (2026-09-27)

CI job `browser-acceptance` builds `net-mesh` with `rtc-bootstrap`, the leaf
bundle and the package, and runs it headless in Playwright's Chromium. The job
pins all ten check names (each must be in `run.mjs` and print `PASS`) and the
`10/10` summary.

`net/crates/net/examples/anchor-acceptance` (`node run.mjs --net-mesh <bin>`):
the real `net-mesh anchor serve --game`, two isolated Chrome contexts, the built
package. **7/7:** the anchor reports its game and endpoint; a host fetches a
credential, enrolls and opens a lobby; a second player lists it, joins and is
seated; an unknown game is refused typed; a third identity presenting another
player's credential is refused at `connect()` (`replay`); a reloaded player
(same storage) comes back as the **same node** and re-enrolls with its original
credential; the anchor's counters agree (2 issued, 3 admitted, 1 refused).

It found a **P1 lobby defect**: a browser node accepts a relayed handshake only
from a peer whose signed announcement it holds, and a joiner announced nothing,
so no lobby could be joined over a real anchor (the local mesh does not model
discovery-before-handshake). Fixed in `joinLobby` (announce a `seek` tag, retry
reaching the host); shown causal by mutation both in the real run and in unit
tests that model the rule. Not yet in CI (needs a release CLI build and Chrome);
the README, skill and quickstart now point at the CLI anchor.

### Slice 2 — built (2026-09-26): the core records the game

- **Recording.** `PeerAdmission::Admitted` carries `tenant: Option<TenantId>`
  (an opaque `u64`; the core only compares). At promotion the core reads the
  Admitted outcome's chain and asks the application's resolver
  (`MeshNode::set_enrollment_tenant_resolver`); `serve_game_enrollment`
  installs one that maps the chain's root to its game
  (`GameRegistry::tenant_of_chain`, `tenant_id(game)` = a hash of the id).
  Bound to the promoted incarnation; `peer_tenant(node)` reads it.
- **Rule** (`tenants_may_meet`): only two DIFFERENT tenants are kept apart; a
  session with none (native peer, other anchor, dedicated host) meets all.
- **Enforced at the three places a leaf can touch another:** the announcement
  flood (per recipient), the replay of held announcements on attach, and the
  relay-transit arm (source = the authenticated adjacent session, never the
  header's `src_id`). New counters `tenant_withheld_announcement`,
  `tenant_refused_transit`. Leaves never query the anchor — they answer
  `query()` from pushed announcements — so filtering the two push paths IS
  discovery isolation (mapped 2026-09-26).
- **Witnessed:** core tests for each of the three (each fails with its filter
  removed); SDK test that a grant names its game and nothing else does; and
  the acceptance run's rival game — its player finds its own lobby and never
  game A's over 25 s, and with the resolver removed from the SDK the same run
  fails (`sawForeign: 22`).
- **Not covered, by design for now:** an announcement forwarded from ANOTHER
  anchor carries no tenant here (multi-anchor is P5); a departed player's
  held announcement loses its tenant with its session until it expires;
  native-only paths (protected hops, punch, scoped announcements, the fold
  subprotocol) are not browser paths. Tags a player announces are not
  checked against its game — isolation makes a false tag visible only to its
  own game.

---

## 5. P1 + P2 — lobbies, interest and hidden information (built)

### Lobbies

`createLobby` / `listLobbies` / `joinLobby` (`src/lobby.ts`) — a thin layer
over the store and discovery, with no protocol of its own:

- **Discovery** rides capability tags in the host's signed announcement (Q4):
  - `net-lobby:<game>` — the queryable listing tag, public lobbies only;
  - `net-lobby:<game>:rec:<base64url JSON>` — code, name, players, capacity,
    store `id@version`, app `info` (≤ 256 bytes; tag ≤ 512 bytes);
  - `net-lobby:<game>:code:<sha-256(code), 32 hex>` — how a code is looked up.

  Unlisted means *not listed*, not *secret*. A record is the host's claim,
  validated on read; the host id comes from the signed announcement; a code
  claimed by two nodes is refused as `ambiguous`. P0's game-id credentials turn
  the `<game>` namespace from a convention into an enforced boundary.
- **Capacity and kick** are enforced in front of the game's `authorize`, counted
  from the owner's live handles; a kick re-authorizes installed handles at once.
- **The host's own player** is `lobby.self` (`hostPlayer`); **presence** is
  `players()` / `subscribePlayers`.
- **Players' events:** one host hook, `onEvent(event, context)` — `join`,
  `leave` (with a reason), `area` (from `areaOf`) — running as a transaction.
  **Inventory bones** (item → count, `onlyOwn`) ship; trading is deferred.

### Interest management (built)

A definition names its top-level entity maps and how an entity is keyed:
`interest: { ships: ship => cellKey(ship.x, ship.z, 32) }` (any string; `null`
= always delivered, Q5). A replica joins with `interest: [...]` and moves with
`setInterest(keys)`:

- only entities whose key is in the set are delivered, as **per-entity** ops;
  far changes send nothing;
- `setInterest` is one additive delta (the `int` wire message) — the view never
  blanks; `stickyCells` adds hysteresis at borders;
- interest is a filter, not a permission: it never calls `authorize`.

A pre-existing defect surfaced and was fixed: a delta's `base` was the owner's
previous revision, so a replica sent nothing on a commit resynced its whole view
on the next. `base` is now the revision each replica is at.

**Not built:** update rate by distance, and bounds re-derived from measurement.

### Hidden information (built)

`defineStore({ visibility })`: path rules (`'everyone'`, `'nobody'`, `'owner'`
by the first `*`, audience lists; `x.length` reveals a hidden array's count),
presets `'open'` and `'card-game'` with overrides. Enforced by the host after any
`project` / `projectFor`, so code narrows but never widens. Hidden entries are
removed, hidden fields become `HIDDEN` (`hiddenOr`); a validator that refuses the
marker is refused at `hostStore`. `assertHidden` for tests; `hostStore({ dev })`
warns about an undeclared everything-to-everyone store.

- **`team` visibility: not built, and not planned** (Q7). Games put players in a
  `team:red` audience through `authorize` and write `['team:red']` rules — zero
  new API.
- Not built: an `ownerOf` override, `host.trust`.
- **The trust boundary stands:** a player-host sees everything; games with real
  stakes need a dedicated host (P3).

### Measurements (`browser-ts/scripts/bench-world.mjs`)

`npm run bench:world` runs the built package over the local mesh. World: 2
entities per 32-unit cell on average, 5% moving per tick, one `setState` per tick,
40 ticks. Modes: `full`, `interest` (3×3 cells), `owner` (interest plus a
per-player secret). Windows dev box, Node 24; medians.

| mode | entities × players | host ms/tick | bytes/player/tick | initial bytes/player |
|---|---|---|---|---|
| full | 500 × 16 | 1.8 | 3,082 | 62,050 |
| full | 8,000 × 16 | 767 | 1,002,012 (whole world, 127 frames) | 1,001,791 |
| interest | 500 × 16 | 1.1 | 219 | 2,171 |
| interest | 8,000 × 16 | 18.1 | 235 | 2,452 |
| owner | 500 × 16 | 22 | 238 | 2,691 |
| owner | 8,000 × 16 | 531 | 257 | 3,043 |
| interest, 0 players | 8,000 | 15.6 | — | — |

Bytes per player are flat in world size under interest; host work is flat in
player count. Fixed on the way: maps resent whole (62 KB → 3.1 KB), quadratic
visibility (6,649 → 30 ms), identity projections re-validated (30 → 17 ms), and a
latent replica defect (a cancelling validator published the cancelled document).
Open findings are §8.

---

## 6. Netcode — models 1 and 2 now, the rest on demand

### The five models

| # | Model | Genres | What it takes | State |
|---|---|---|---|---|
| 1 | **Authoritative state sync** (today's store) | MMO, RPG, card, strategy, party | Done, plus interest management and visibility | **Built** |
| 2 | **Snapshot interpolation plus prediction and reconciliation** | Shooters, action, MMO movement, casual racing | Interpolation buffers, local prediction, reconciliation to the host's state, lag compensation (the host rewinds to judge hits) | **Next** |
| 3 | Rollback netcode (the model behind GGPO) | Fighting games, precision 1v1 or 2v2 | Deterministic fixed-step simulation, exchanged inputs, prediction, rollback and re-simulation, adjustable input delay | Deferred |
| 4 | Deterministic lockstep | RTS, large unit counts | Input-only sync on a synchronized frame clock, desync detection | Deferred |
| 5 | Client-authoritative with host validation, plus dead reckoning | Racing, casual physics | Each player owns their vehicle; others extrapolate; the host validates and settles collisions | Deferred |

**Only models 1 and 2 are in scope.** Casual racing is served by model 2, so no
genre is left without a model; 3–5 wait for a game that needs them.

### Rust primitives (next to the WebRTC leaf) — only what model 2 needs

1. **An unreliable, latest-value channel** for high-rate inputs and positions.
   - **Verified fact (2026-09-26):** the leaf opens **one** DataChannel,
     `ordered: true`, with no retransmit limit (`leaf/src/rtc.rs:495`). Every Net
     stream — "fire-and-forget" included — rides reliable, ordered SCTP, so a
     lost packet is retransmitted underneath and **blocks everything behind it**
     (head-of-line blocking). Fire-and-forget at the Net layer does not make
     60 Hz positions unreliable on the wire (G13).
   - **The primitive:** a second DataChannel, **unordered with zero
     retransmits**, negotiated alongside the first on the same session and
     carrying the same Noise-protected frames; plus a receiver that keeps **only
     the newest value per key** (sequence-numbered, stale ones dropped).
   - Native side: the same semantics on the core's UDP path (fire-and-forget
     without per-stream ordering holds).
   - **Built (2026-09-26), step 1:** `PacketFlags::LOSSY` (0x80, the one free
     header bit — authenticated, ignored by receivers that don't know it) and
     `net_wire::carrier` (`rides_lossy_carrier`: LOSSY and nothing needing
     delivery; reads past a routing header; labels `net` / `net-u`). The leaf
     offers `net-u` (unordered, `maxRetransmits: 0`) beside `net`, the answerer
     tells them apart by label, and a lossy packet is sent now or dropped
     (counted), never queued. The native driver tracks the two by label,
     writes lossy packets (including relayed ones) on `net-u`, and survives
     `net-u` closing; native offers open both. Page API: `openStream({
     reliability: 'fireAndForget', lossy: true })` (`connect()` nodes; the
     leader proxy refuses it typed). Counters `lossy_written` /
     `lossy_dropped` / `lossy_ingress` on both sides (the leaf's native-stats
     inventory now also names slice 2's two tenant counters). Witnessed: wire
     classifier tests, a leaf send test, a native loopback test (fails when
     the anchor ignores the label), all 157 native RTC tests and 468 leaf
     tests, and the browser witness
     `stage5_a_lossy_page_stream_rides_the_lossy_channel_to_the_anchor`.
     **Not yet:** native senders cannot stamp LOSSY (a dedicated host's
     snapshots to browsers need it — model 2's next step, with the tick
     loop); the leader proxy does not carry `lossy`; the replay window (1024)
     bounds how far the reliable channel may lag the lossy one.
   - **Built (2026-09-26), step 2: native senders.** `StreamConfig::lossy`
     (`with_lossy`) stamps a fire-and-forget stream's packets LOSSY, fragments
     included; napi `lossy` (refused with reliable), `sdk-ts`
     `StreamConfig.lossy`, and `meshStoreTransport` passes it through.
     Witnessed natively (a loopback test: a native lossy stream arrives on
     `net-u` and is delivered; fails when the stamp is removed).
2. **Clock sync and round-trip/jitter estimates per peer**, so host and players
   agree on one **tick timeline**. Interpolation delay, prediction horizon and
   reconciliation all depend on it. (The reliable stream's SRTT is transport
   state for loss recovery and stays there; this is an application-facing
   estimate.)

**Built (2026-09-26), step 3 — clock sync and the netcode module.** Clock sync
is built in **TypeScript**, not Rust, as part of the netcode module
(`ClockEstimator`, NTP-style, offset from the lowest-RTT samples of a window
of 16, jitter = mean absolute deviation). Why not Rust: the game's timeline
is the JS event loop on both ends, the same module runs unchanged on a page
and on a Node dedicated host, and a Rust-side estimate would add precision no
game code could observe. Revisit only if a measurement disagrees.

`@net-mesh/browser/netcode` (`browser-ts/src/netcode/`): `hostNetcode` —
fixed-rate tick, inputs applied exactly once per player (dedup by seq, gaps
skipped), per-player `visible`, `authorize`, history ring, `rewind(seen)`
capped at 200 ms; `joinNetcode` — clock pings, `SnapshotBuffer`
interpolation a delay behind the host (reorder-tolerant, held past the
newest), prediction + reconciliation (replay of unacked inputs over each
newer authoritative state), inputs sent with redundancy and re-sent on a
timer. JSON frames tagged with the instance label, all on lossy streams.
Witnessed: 11 tests over a simulated lossy (≤20%), laggy, jittery network and
the local mesh (clock within 10 ms of a 5 s offset; 61 inputs applied exactly
once at 20% loss; zero corrections; monotonic, interpolated remote
positions; the rewind cap), each property shown to fail when removed (7
mutations); and an `sdk-ts` test with a **Node dedicated host** on native
nodes via `meshStoreTransport`. **In real browsers (2026-09-26):** the
acceptance run (`examples/anchor-acceptance`, 10/10) adds a netcode phase —
host page `hostNetcode`, joiner `joinNetcode`, through the real CLI anchor: 31
inputs applied exactly once, prediction immediate and converged with zero
corrections, clock RTT 31.7 ms / jitter 6.3 ms, and the traffic on the lossy
channel both ways (`lossy_written` / `lossy_ingress` on both pages). It found
a real defect the simulated network could not: the browser node reports an
event's sender as a DECIMAL u64 and netcode read it as hex, refusing every
frame; netcode now reads event peers exactly as the store does
(`peerHexOf`), and the simulator reports decimal like the leaf. The runner witness
`stage7_netcode_predicts_a_ship_and_the_host_applies_every_input_over_the_lossy_channel`
runs on the DIRECT pair (after §8). **Not yet:** correction smoothing,
extrapolation, binary frames, fragmentation-aware snapshot sizing,
interest-key integration (today `visible`).

**Fixed (2026-09-27) — netcode on a direct pair.** Run right after the pair
went direct, the host's netcode sends to the player all failed. Cause:
`joinNetcode` (like the lobby) calls `connectPeer` "to be sure" before
opening its stream, and on a pair that was ALREADY direct that made a fresh
offer. The new connection replaced the player's transport link and so closed
the working one under the host. The host kept reading the pair as direct
(session installed, no relay) while its channel was `Closed`: every send was
refused (`channel … is Closed`, counted as `admission_refused_unknown_peer`)
and the stream's credit ran out. Only the player → host lossy channel of the
new, never-answered connection still carried anything. Fix: `connectPeer` is
idempotent on a healthy pair. The leaf's `peer_direct_dialog` reports the
live dialog when the session is installed, unrelayed and the transport open,
and `BrowserNode.connectPeer` then resolves `direct` without offering.
Witnessed by `stage6_connect_peer_on_a_healthy_direct_pair_is_idempotent`.
Stage 6's unanswered-offer witness now makes its offer through the raw
`peer_offer` primitive, which is how the re-attempt owner makes one. The
leader-proxied `openSession().connectPeer` does the same through a new proxy
op, `peer_direct_dialog`. A leader too old for it refuses the op, and the
follower offers as before. Separately, credit is
spent before the transport refuses a packet; a refused send should arguably
not consume credit.

### What model 2 adds on top

- **A host tick loop.** A fixed-rate authoritative step on the host
  (`tickRate`), stamping snapshots with a tick number. Store revisions count
  changes, not time; model 2 needs time.
- **A separate optional module**, e.g. `@net-mesh/browser/netcode`, **alongside
  the store rather than inside it**:
  - the store keeps durable, validated state (inventory, score, doors);
  - the netcode module carries high-rate transforms over the unreliable channel,
    **filtered by the same interest keys**, so large-world movement stays cheap;
  - remote entities: interpolation buffers rendered a small delay behind the
    newest snapshot; local entity: predicted from its own inputs, reconciled to
    the host's tick-stamped state with input replay.
- **Lag compensation with a bounded rewind.** The host keeps a bounded history
  of positions (a ring of N ticks) and judges a hit where the target was at the
  shooter's tick — **capped (≈ 200 ms)**, so faked latency cannot buy an
  advantage.
- **Optional modules, not core weight.** Each netcode model ships as its own
  subpath, so "any game" does not bloat the core package.

**Acceptance:** a demo with interpolated remote players and a predicted local
player at 100–150 ms simulated latency with loss, no visible snapping; loss on
the unreliable channel stalls nothing on the reliable one (a witness measuring
it); a hit judged by rewind within the cap, refused beyond it.

### Deferred primitives (models 3–5)

- Input ring buffers and a rollback/lockstep engine (frame scheduling,
  confirmation, resimulation bookkeeping in Rust; the game's `step(state,
  inputs)` in TypeScript).
- **Host-less** peer-to-peer sessions for 1v1. (Direct browser-to-browser pairs
  already exist and the store rides them when a pair is promoted; what is
  deferred is a match with no host at all.)

---

## 7. P3 — dedicated hosts, and persistence

**Why:** a player's tab is a fragile, all-seeing host. Lobbies that outlive their
creator, and every region in P4, need a host that is not a player.

### The transport (built)

- **Q1 spike:** the store runs in Node unchanged; the gap was that no Node API
  delivered stream bytes with the authenticated sender. The core had it and
  dropped it where it queued `StoredEvent`; nRPC's `callerOrigin` is
  caller-supplied, not an identity.
- **Built:** core `MeshNode::register_stream_inbound` (vacant-only, registration
  ids; after the admission gate and the blob-transfer divert, before the shard
  queue); `net_wire::channel::name::stream_id_from_label` as the one label → id
  derivation (the leaf delegates to it); napi `NetMesh.onStreamData` and
  `streamIdFromLabel`; `sdk-ts` `MeshNode.onStreamData` and
  `meshStoreTransport(mesh, { listen })`.
- **Witnessed:** core tests over real UDP; `sdk-ts` test serving the browser
  store to two native players with `authorize` refusing one by the reported
  peer; browser runner
  `stage5_a_labeled_page_stream_reaches_a_native_sink_attributed_to_the_page`.
- **Other bindings (kept, not extended):** a pull-based inbox,
  `MeshNode::open_stream_inbox`, with Python `open_stream_inbox`, C
  `net_mesh_open_stream_inbox` & co. (`NET_ERR_MESH_STREAM_OCCUPIED`, -109) and
  Go `OpenStreamInbox`, each tested.

### Persistence (in scope, minimal)

- A dedicated host **snapshots its store document to RedEX** (the repo's durable
  log) on an interval and on clean shutdown, and **restores it on start**.
- It is also Q6's recovery path: a failed region handoff re-joins the player to
  the destination region from persisted state (§9).
- Deferred: per-entity databases, migrations between store versions, replay.

### Survival

A dedicated host is a mesh daemon, so placement and migration apply. A
player-hosted lobby still ends with its host (G6); host migration between players
is deferred.

---

## 8. Host commit cost

**Measured:** 8,000 entities with **no players** cost **15.6 ms per `setState`**
(G14). The core validates the whole document on every commit, and every action
and input transaction commits the same way. Per-player projections under
`projectFor` / `owner` rules add O(world × players) (531 ms at 8,000 × 16).

**Follow-up, in order:**

1. **Incremental validation in TypeScript first — DONE (2026-09-27).** The
   definition's `entities: { [collection]: parseOne }` plus the host's
   `setEntities(collection, changes)` / `setEntity(...)`. Only the written
   entities are parsed. The whole-document validator is skipped by contract:
   declaring a collection says `state` imposes nothing on it beyond each entity.
   Outside a transaction, only the changed entities are reconciled, and the
   collection is rebuilt by one spread with no whole-tree reconcile. The
   profile showed that reconcile's `defineProperty` walk cost as much as the
   validator. Measured with `bench-world.mjs --write entities` (8,000 entities,
   5% moving, ms/tick median):

   | | `setState` | `setEntities` |
   |---|---|---|
   | no players | 15.9 | 2.2 |
   | interest, 4 / 16 players | 18.4 / 18.2 | 4.6 / 4.7 |
   | owner rule, 4 / 16 players | 135 / 486 | 130 / 459 |

   (The byte columns also fell, but mostly because the entity bench does not
   bump `tick`.) The `owner` rows are per-player projection over the whole
   world, and that is item 2. Not done: an entity write from an action
   handler's `context` (a handler can call the host handle, and the write
   joins its transaction, but that commit validates the document as before).
   Replicas still validate the whole document per delta.
2. **Per-view cost O(changed), not O(world) — DONE (2026-09-27).** This is
   the first half of the item. When a commit changes only collections
   declared in both `interest` and `entities`, and the host projects only by
   DECLARED rules (no `project` / `projectFor`), `propagate` computes the
   raw entity change once. Each view then projects just the changed entities
   on a sparse document: declared rules are path patterns, so an entity's
   visibility does not depend on the rest of the world. It validates them
   with the per-entity parser, where the full path ran `state` over the whole
   view. Anything it cannot answer exactly falls back to the full path.
   `host.counts().sparseViews` counts the uses. A 150-step random property
   test (`test/store/sparse-views.test.ts`) holds every replica equal to the
   declared projection narrowed to its interest. Measured (8,000 entities,
   5% moving, `owner` rule, ms/tick median): 4 players 130 → 10.3, 16
   players 459 → 25.0.
   **Still open: O(changed ∩ nearby).** Each view still projects every
   changed entity, not only the ones its interest covers. Narrowing by the
   RAW interest key is not sound as it stands, because the key is taken from
   the PROJECTED entity. When a rule hides the keyed field, the key function
   sees the marker and the entity is delivered to everyone. A maintained
   index (cell → ids) needs a rule that keyed fields are never hidden, or a
   per-rule check of that, before it can filter first.
3. **Moving the hot path to Rust** only if the harness still demands it after 1
   and 2 — a much larger project.

Also open: **a delta is one frame** (a tick changing more than ~8 KB without
interest reinstalls the view; chunked deltas would be a wire change), and **one
unreproduced harness stall** (each point is now bounded and named).

---

## 9. P4 — large worlds across region hosts

**Model:** the world map is divided into regions. Each region is its own store
(`store: 'region:4:7'`) hosted by a dedicated host (P3); a client holds replicas
of its current region and its neighbours.

1. **Region directory = discovery.** A region host announces
   `my-world:region:4:7`; clients and hosts find it with `query`. No central
   directory service — the property no game SDK has.
2. **Client world view.** `joinWorld({ node, world, position })` keeps replicas
   of the current region plus neighbours (with interest inside each) and exposes
   one merged view to `bindEntities`.
3. **Entity handoff — at-most-once with a typed failure (Q6).** A freezes the
   entity at a fenced epoch and sends its state to B; B admits it and becomes
   authoritative; late inputs to A are forwarded or refused typed, never applied
   twice. **A failed handoff re-joins the player to B from persisted state**
   (§7). Exactly-once is not attempted: it is a large protocol project, needed
   only for things like item transfers mid-crossing, which are designed around.
4. **Cross-border actions** forward host to host; the target's authority decides.
5. **Ghosting** mirrors border entities read-only into the neighbour.
6. **Load balancing** splits and merges regions with daemon placement and
   migration; a split reuses handoff.

**Acceptance:** a walkable multi-region world on several native hosts with no
visible pop or duplicate at borders; killing a region host mid-handoff yields a
typed, recoverable outcome (no duplicated or lost entity), proven by a
deterministic simulation before any browser test.

---

## 10. P5 — reach and resilience (deferred)

- **TURN (G7):** provision or bring-your-own TURN, fall back after a classified
  `udp-blocked`; test on the natsim harness.
- **Reconnection:** lobby and world helpers use `replica.reconnect()`.
- **Region-aware anchor endpoints:** several shared anchors behind one endpoint
  picking the nearest; P0's stateless credentials make this additive.

---

## 11. Deliberately deferred

- **Netcode models 3–5** — rollback, lockstep, client-authoritative dead
  reckoning — and their primitives (input ring buffers, a rollback/lockstep
  engine, host-less 1v1 sessions). Revisit on demand.
- **Metering and billing.** Per-game counters ship with P0; quotas and billing
  wait.
- **Host migration between players (G6).** Dedicated hosts cover the need first.
- **Trading** (both players' consent, one atomic transfer).
- **`team` visibility rules** — use audiences (§5).
- **Update rate by distance; chunked deltas; bounds from measurement.**
- **Further Python / Go / C investment** for this audience.
- **Anti-cheat beyond authority.** Server authority, hidden information and the
  capped lag-compensation rewind are the baseline.

---

## 12. Decisions

| # | Question | Decision |
|---|---|---|
| Q1 | Store in Node, or a native store host? | **Node.** Spiked and built: the store runs as-is; the core now delivers stream data with its authenticated sender (§7). |
| Q2 | CLI anchor, hosted anchor service, or both? | **CLI anchor first, shared anchor second**, the CLI anchor shared-ready from day one (§4). |
| Q3 | Credential policy? | **Anonymous per-visitor credentials with per-game rate limits**, visitor key in `localStorage`; login-signed requests can be added later without a protocol change (§4). |
| Q4 | Lobby metadata on announcements? | A fixed record plus ≤ 256 bytes of app fields; unlisted lobbies publish only a code hash (§5). |
| Q5 | Interest keys? | Arbitrary string keys; grid cells are a helper (§5). |
| Q6 | Handoff consistency? | **At-most-once with a typed failure**; a failed handoff re-joins from persisted state (§9). |
| Q7 | Visibility API and defaults? | Path rules plus presets, dev warnings behind an explicit `dev` flag; **`team` left out — use audiences** (§5). |
| Q8 | Bindings for this audience? | **TypeScript only**; the Python/Go/C surfaces on the branch are kept, not extended (§1). |
| Q9 | Netcode scope? | **Models 1 and 2**; the Rust primitives trimmed to the unreliable channel and clock sync; models 3–5 deferred (§6). |
| Q10 | Announcement lifetime? | Keep re-announcing every ~2 s; measure once against the P0 anchor, investigate only if it drops announcements in seconds (§4). |

---

## 13. Competitive context (brief)

Hosted room servers (Colyseus, Photon, Nakama, PartyKit) ship lobbies,
matchmaking and reconnection but are services with their own identity. Web
drop-in kits (Playroom, Rune) nail "open a link and play" on a proprietary cloud.
P2P libraries (Trystero, PeerJS) are drop-in but have no authority or hidden
information. Replicated computation (Croquet) shows everything to everyone.
Protocol peers (libp2p, Iroh) stop at transport. SpatialOS-style spatial sharding
existed only as an expensive centralized cloud.

The differentiator is the combination: an open protocol, proven identity feeding
authority, hidden information enforced before sending, responsive netcode, **and**
a world whose regions are discovered, placed and migrated on the same mesh that
lets a game call AI characters or native services. P0–P1 make the entry point
competitive; P4 is the part that is hard to copy.

(Landscape as of mid-2026, from memory — refresh before external use.)

---

## 14. Risks

- **Handoff correctness (P4)** remains the dominant risk. Mitigation: a
  deterministic model and simulation tests before browser tests.
- **The unreliable channel (G13)** is new transport work in the leaf: a second
  DataChannel's negotiation, and keeping the Noise session's guarantees on an
  unordered, lossy carrier. Mitigation: witnesses in the browser runner that
  loss on it stalls nothing on the reliable channel.
- **Clock and fairness.** A lag-compensated host trusts claimed latency within
  the cap; the cap is the guard, and it is a tuning parameter to measure.
- **Host commit cost (G14)** could cap world size before P4 splits it.
  Mitigation: §8's incremental validation, measured with the harness.
- **Multi-tenant leakage on the shared anchor.** Mitigation: game ids in
  credentials, enforced at the anchor, witnessed by a cross-game refusal test.
- **Scope creep toward a game engine.** The non-goals in §1 and the deferred
  list in §11 are the guard.
- **Skill drift.** Each phase lands with its `net-browser` skill update and an
  executed example; otherwise vibe-coded games break first.
