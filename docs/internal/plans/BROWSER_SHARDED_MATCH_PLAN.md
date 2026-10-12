# Sharded matches across player hosts — plan

**Status:** planned, 2026-10-12. No release targeted yet; slice S0 (the
measurement) decides whether the rest goes ahead.
**Scope:** `@net-mesh/browser` — the netcode module (`src/netcode/`), reuse of
the world module's forwarded actions (`src/world/`), and the lobby store
(`src/lobby.ts`). No wire change to the leaf, the anchor or the core.
**Companion to:** [browser lobbies and large worlds](BROWSER_LOBBIES_AND_LARGE_WORLDS_PLAN.md)
(§6 netcode model 2, §9 region hosts).
**Source baseline:** `fccd54453` (master), branch `LZL0/threejs-scale`.
**Audience:** the team.

---

## 1. The gap

The target is one match of **64 v 64 players** (128 in total), hosted by
**4–8 of the players' own machines** with no dedicated server. Every player
sees one shared arena with responsive movement and fair hit decisions.

What exists today, checked in the code:

- **Netcode has one authority per match.** `hostNetcode`
  (`browser-ts/src/netcode/host.ts`) runs the tick, applies inputs, sends
  snapshots to every player and keeps the rewind history. `joinNetcode`
  (`netcode/client.ts`) takes exactly one `host`. A player cannot draw a
  world that several hosts serve.
- **One host's upload grows with players × entities each can see × tick
  rate.** Snapshots are full states, not deltas (`snapshotFrames`, `host.ts`),
  sent separately to each player. The estimate below assumes ~80 B of JSON per
  player entity:

  | | 1 host | 4 hosts (each) | 8 hosts (each) |
  |---|---|---|---|
  | everyone sees everyone, 30 Hz | ~300 Mbit/s | ~75 Mbit/s | ~38 Mbit/s |
  | interest/fog (~30 visible), 30 Hz | ~75 Mbit/s | ~19 Mbit/s | ~9.5 Mbit/s |

  These are **estimates, not measurements**. S0 replaces them. Only the
  bottom-right cell fits a home connection.
- **Nothing has been measured at this size.** `scripts/bench-world.mjs`
  benchmarks the store, not netcode, and stops at 16 players (lobbies plan
  §5, §8).
- **The existing multi-host layer (`world/`) splits the map by area**
  (`regionOf`, `r:<rx>:<rz>`) and is built on the reliable store
  (`joinWorld` → `joinStore`). §2 explains why area is the wrong split for a
  dense arena. What carries over are its pieces that don't depend on area:
  `regionHandoffs(...).forward` (each cross-host action applied at most once,
  `world/link.ts`, `world/border.ts`), `handoff` (at-most-once ownership
  transfer, `world/handoff.ts`) and `handoffLink`'s per-sender
  authentication (`peerOf`).
- **Lobby capacity is not the limit.** `MAX_LOBBY_CAPACITY` is 256
  (`src/lobby.ts`), and the host is one of the `capacity` slots.
- **Nothing decides who runs an NPC.** `hostNetcode`'s `snapshot()` takes
  any entity map (`host.ts`), so an NPC can be sent like a player. But
  nothing assigns an NPC's AI to a host, nothing carries NPC↔player hits
  between hosts, and every NPC adds to the upload estimate above. Sending a
  distant NPC less often is not built either (lobbies plan §5, "update rate
  by distance").

## 2. The design

### Shard by player ownership

Each **shard host** is a player's node that owns a fixed set of player
entities for the whole match: 16 per host with 8 hosts, 32 with 4. Ownership
doesn't depend on position, so moving never triggers a handoff, and the
upload load is the same on every host by construction.

```
            ┌──────── lobby node (coordinator; also a shard host) ────────┐
            │ store: roster, teams, score, phase, ASSIGNMENT (epoch,       │
            │        shard → host, player → shard, npc → shard),           │
            │        NPC paths (§2 "NPCs")                                 │
            └──────────────────────────────────────────────────────────────┘
 shard:0 host ── hostNetcode(label) ── snapshots of ITS players + NPCs ──► every player
 shard:1 host ── hostNetcode(label) ── …                              ► every player
   …                    ▲ inputs only from the players it owns
   └─ host ↔ host: spectate each other (joinNetcode, no local entity)
                   + forward('hit', …) over handoffLink, applied once per id
```

- **Simulation.** Each shard host runs one `hostNetcode`. `step` applies
  inputs only from the players it owns and runs the AI of the NPCs it owns.
  `snapshot` returns only those players and NPCs. `visible` and `interest`
  filter per receiver, as they do today.
- **Players** join every shard host and merge the snapshots into one view
  (`joinShards`, S1). Each player has `local` prediction only on the shard
  that owns it and sends inputs only to that shard.
- **Hosts see each other's players** by joining each other as spectators
  (`joinNetcode` with no `local`). That gives every host the other shards'
  positions for line-of-sight checks, `visible` decisions and hit
  plausibility, at a cost of 7 peers × ~1.3 KB per tick, which is negligible.
  `visible` must let other shard hosts receive everything.
- **Hits across hosts.** The shooter's owning host A forwards the hit to the
  target's owning host B. B rewinds **its own** history and decides (S3). The
  target's owner is the authority, which matches the `world/border.ts` rule
  that B's authority decides.
- **The directory is the lobby's assignment.** Shard hosts are found through
  the assignment record in the lobby store, not through `announceRegions`.
  `handoffLink({ peerOf })` reads the assignment, so a message claiming to
  come from `shard:3` is accepted only from the node assigned to shard 3.

### Alternatives considered

| Approach | Why rejected |
|---|---|
| **One dedicated host** | The right answer when the game has real stakes, and it stays the recommendation for those. Rejected only because this plan's premise is no server. |
| **Split by area (`world/` regions, `joinWorld`)** | Load follows the fighting: 60 players at one objective puts nearly the whole match on one host. Crossing a border mid-fight freezes the entity for at least one round trip (250 ms per lost offer). In a small arena most fights are near a border anyway. And it runs on the reliable store, the wrong channel for movement. |
| **One simulating host plus a relay tree** (host → 4–8 relays → players) | Spreads the upload, but every snapshot takes an extra hop, a relay can delay or drop data and nobody would notice, and the simulating host's CPU and trust problems don't move. |
| **Split by team** (2 hosts) | Two hosts aren't enough for the bandwidth (~38 Mbit/s each with interest filtering), and each host would be the authority for its own team. |

### Decisions

| # | Decision | Recommendation |
|---|---|---|
| D1 | Which players a host owns | **One squad from each team, never the host player's own squad.** That makes the plain "host gives its own character god mode" exploit impossible. It is still **not** a security boundary (§5). |
| D2 | How hosts are found | **The lobby store's assignment, fenced by an `epoch`.** Not `announceRegions`: `announce` replaces a node's tags, and the lobby already owns its node's announcement (`src/lobby.ts`). An assignment can't be ambiguous, while announcements can be. |
| D3 | Where the code lives | **`joinShards` in `@net-mesh/browser/netcode`.** The host-side wiring starts as a demo under `browser-ts/demo/` plus a skill section, and becomes an API once a second game needs it. |
| D4 | Number of hosts, tick rate | **8 hosts at 30 Hz, or 4 at 20 Hz**, both with interest filtering. Confirmed or replaced by S0 and S7. |
| D5 | Rewind cap for cross-host hits | **300 ms** (default is 200). A forwarded hit is older by the time it reaches B: render delay + player→A + A→B. Each extra 100 ms is also more advantage a player can get by faking lag, so S3 measures the real age before this is fixed. |
| D6 | Durability | **None.** A match lives only in memory. `regionHandoffs` runs without `persist`. A crash loses that host's part of the match (S6), and at-most-once still holds because B's action records die together with B's entities. |
| D7 | Who owns an NPC | **Fixed at spawn: `hash(npcId) mod hosts`, recorded in the assignment. Fixed objectives belong to the lobby node.** Alternatives and reasons in §2 "NPCs". |
| D8 | Where NPC paths live | **In the lobby store, written through a host-only action that `authorize` admits only from the NPC's current owner (checked against the assignment).** Path changes are rare, so the lobby node's upload barely notices them. The alternative, a small path store on every shard host, would make every player join 4–8 more stores. |

### Clocks: every shard has its own

Each `joinNetcode` client keeps a separate `ClockEstimator` for its host.
A hit claim must therefore state the time in the **target's** clock:
`seen = clients[targetShard].hostNow() - interpolationDelayMs`. The player
puts that value in the input data it sends to A. A forwards it unchanged, and
B clamps it with its own `maxRewindMs`. `TickInput.seen` stays in A's clock,
and A uses it to rewind the shooter's own position.

### NPCs

An NPC is an entity like a player, except that a host's AI drives it instead
of inputs. Once its owner is decided, everything else carries over:

- it runs in its owning host's `step` and is sent in that host's snapshots,
  under the same `visible`/`interest` filtering;
- `joinShards` merges and interpolates it like any remote player;
- it is in its owner's rewind history, so lag-compensated hits on it work
  the same way as hits on players.

**Ownership (D7).** At spawn, an NPC is assigned to
`hash(npcId) mod hosts` and recorded in the assignment under the same
`epoch` as players. It keeps that owner for its whole life, except when a
host leaves cleanly (S5) or crashes (S6), where NPCs are handled exactly like
players. **Fixed objectives** (turrets, bosses, capture points) belong to the
lobby node, which is a shard host too. A fixed objective is a match's center
of gravity, and the lobby node already holds score and phase.

| Alternative | Why rejected |
|---|---|
| **Follow the target**: hand the NPC to the host that owns the player it is attacking | Fights become local to one host, with no forwarding. But every target switch freezes the NPC for a handoff (at least one round trip, 250 ms per lost offer), and NPCs that switch targets often keep moving between hosts. Measure later as an optimization (§6). |
| **By area**: NPCs belong to zones, like the `world/` regions | Natural for camps and turrets, but the busiest fight lands on one host again: the same reason area-based sharding was rejected for players. |

**Interactions.**

- **Player hits an NPC on another host:** the S3 path unchanged, with the
  NPC's owner as B.
- **NPC attacks a player on another host:** the NPC's host forwards `hit` to
  the player's owner. The NPC's position at the time of the attack comes
  from its host's own rewind history. The player's owner checks it against
  its spectator view of that host, with the same tolerance check as S3.
- **NPC perception:** the AI sees players on other hosts through its host's
  spectator view, which is about one interpolation delay plus one hop old
  (~100–150 ms). That is within normal AI reaction times. An NPC that needs
  frame-exact aim is out of scope.

**Bandwidth.** Every visible NPC costs as much upload as a visible player,
so 200 NPCs can more than double each host's upload. In order of payoff:

1. **Interest filtering**, as for players.
2. **NPCs that follow a path don't need snapshots (S8).** A patrolling,
   scripted or idle NPC is published once as a path record, and clients
   compute its position from it. The NPC switches to netcode snapshots only
   while it is in combat or off its path. Each NPC's mode is explicit in
   its record (`'path' | 'live'`). Being absent from a snapshot is never the
   signal, because interest filtering also removes entities from snapshots.
3. **Sending distant NPCs less often.** Not built; out of scope here (§6).

## 3. The slices

Each slice leaves the tree working. Timings are ms per tick and sizes are
bytes, as the benchmarks print them.

### S0 — Measure first: `scripts/bench-netcode.mjs`

Run `hostNetcode`/`joinNetcode` over `createLocalMesh` with N players and H
hosts (ownership split), configurable entity bytes, tick rate and
interest/visibility. Report per host: ms/tick, bytes/s sent, number of
streams. Report per player: bytes/s received.

- Points: H = 1, 4, 8 × N = 16, 64, 128 × {everyone sees everyone, interest}
  × {20, 30} Hz × NPCs = 0, 100, 300 (simulated moving entities owned by
  the hosts, spread with D7's hash).
- **Proves:** replaces the §1 estimates with recorded numbers (command and
  machine named, as in lobbies plan §5). **Stop rule:** if a single host at
  128 players with interest filtering already fits within 10 Mbit/s and
  10 ms/tick, sharding isn't needed and the plan ends here, recorded as
  such.

### S1 — `joinShards`: one merged view over several hosts

`src/netcode/shards.ts`, exported from `./netcode`:

```ts
const match = joinShards<E, I>({
  transport: node, label: 'arena.move',
  shards: { 'shard:0': hostHex0, /* … */ },  // from the lobby assignment
  epoch: 3, own: 'shard:2',
  local: { id: myId, predict },               // prediction on `own` only
  interest: cellsAround(x, z, { size: 32 }),
});
match.view();              // merged: remote interpolated, own predicted
match.input(data);         // to `own` only
match.hostNow('shard:5');  // that shard's clock, for hit claims
match.setInterest(keys);   // fanned out to every shard
match.setAssignment({ epoch, shards, own });  // S5/S6 reassignment
```

- One `joinNetcode` per shard, with `local` set only on `own`.
- Merge by entity id. If two shards both show an id (only possible while a
  reassignment is in flight), the copy from the shard the current `epoch`
  assigns it to wins. Use the same linger as `joinWorld` (`lingerMs`) so an
  entity doesn't blink while it moves between shards.
- `stats()` per shard, so a silent host can be identified by name.
- **Proves** (vitest over the local mesh, `test/netcode/shards.test.ts`):
  three hosts with disjoint entities merge into one view. A duplicate id
  resolves to the assigned shard; the test breaks the rule and must fail.
  Inputs reach only `own`, checked with per-host input counters. A host that
  goes silent shows in `stats()` and its entities leave after the linger.

### S2 — The shard host recipe (example)

`browser-ts/demo/sharded-arena/` wires:

- `hostNetcode` with `authorize` limited to the roster;
- `visible`, which shows other shard hosts everything and applies fog to
  players;
- spectator `joinNetcode`s to the other shards;
- `regionHandoffs({ region: 'shard:k', link: handoffLink({ peerOf: fromAssignment }) })`
  with no `persist` and no `ghosting` (`ghosting` assumes the `r:x:z` grid).
  `load`/`commit` read and write the shard's owned-player map, which the
  `snapshot` callback also returns.

- **Proves:** the example runs 4 hosts × 32 simulated players on the local
  mesh in CI's browser-ts job, and a test checks that each player's merged
  view matches the union of the hosts' authoritative states within one tick
  plus the interpolation delay.

### S3 — Cross-host hits

- Player input: `{ fire: { target, shard, seen } }`, with `seen` in the
  target shard's clock (§2).
- A checks the shot (ammo, cooldown, the shooter alive), rewinds its own
  history at `TickInput.seen` to get the shooter's position, and calls
  `forward(targetShard, 'hit', { shooter, pose, target, seen })`. For a hit
  between two players on the same shard, A handles it locally as today.
- B's `actions.hit` calls `rewind(seen)` on its own `hostNetcode`; a clamped
  rewind is refused. It checks range and line of sight against `pose`, and
  checks `pose` against B's spectator view of A within a tolerance. It then
  applies the damage in the committed state and answers.
- Timing: `giveUpMs` ~500 (a hit confirmed later than that is worse than a
  miss), `retryMs` 50, `handledRetentionMs` 10 s.
- **Proves** (link simulator that drops, duplicates and reorders, as in
  `test/netcode/sim.ts`): a duplicated hit is applied exactly once. A hit
  older than the cap is refused. A pose outside the tolerance is refused.
  The age distribution of `seen` at B is recorded, and that measurement sets
  D5.

### S3b — NPCs: ownership and hits across hosts

- NPCs in the assignment (`npc → shard`, D7). A shard host runs the AI of
  the NPCs it owns in `step` and returns them from `snapshot`, so its
  `regionHandoffs` `load`/`commit` cover them as well.
- Player→NPC hits take S3's path. NPC→player hits:
  `forward(playerShard, 'hit', { shooter: npcId, pose, target, seen })`,
  where `pose` comes from the NPC host's own `rewind` and `seen` is in the
  player's owner's clock (the NPC host reads it from its spectator client
  for that shard).
- Fixed objectives are spawned by the lobby node and owned by it.
- **Proves** (the S3 link simulator): a player's hit on an NPC owned by
  another host is applied exactly once under drop, duplication and
  reordering. An NPC's hit on a player on another host is applied exactly
  once and refused when its `pose` is outside the tolerance. A test that
  gives one NPC to two shards in the same epoch must fail validation.

### S4 — Lobby integration: roster, teams, assignment

- Lobby store fields: `assignment: { epoch, hosts: { [shard]: hex }, owner: { [player]: shard }, npcs: { [npc]: shard } }`,
  validated, and editable only by the lobby host.
- Host selection: players volunteer, and each reports a measured uplink
  (S0's method in a 2 s probe). D1's ownership rule is applied when the
  match starts.
- Team audiences through `authorize` (`team:red` / `team:blue`, lobbies plan
  §5), for chat, pings and objectives in the store.
- `capacity` counts the lobby host. A lobby with 128 players needs
  `capacity: 128` when the lobby host is one of them.
- **Proves:** validator tests, including one where a non-host write to
  `assignment` is refused. A joiner reads its `own` shard and connects
  through `joinShards`. A test that assigns a host its own squad must fail
  validation.

### S5 — Planned moves: rebalancing and a host leaving cleanly

A host that is leaving, or a rebalance, moves players with
`regionHandoffs.handoff(player, toShard)`, the existing at-most-once
protocol. When a player is `moved`, the coordinator bumps `epoch` and the
player's client calls `setAssignment`, which moves `local` prediction to the
new shard. Unacknowledged inputs are sent again to the new owner (input
redundancy already covers 8).

- **Proves:** a test moves a player in the middle of a steady stream of
  inputs. The entity is never authoritative on two shards. No input is
  applied twice (each `seq` is checked across both shards). Prediction keeps
  running, and `stats().corrections` stays within a stated bound.

### S6 — A host crashes

Handoff can't help here, because it needs the source host to be alive. The
coordinator detects the failure (`playerTimeoutMs` on its spectator stream to
that shard), bumps `epoch`, and gives the orphaned players to the remaining
hosts. Each new owner respawns them from the last state it saw as a
spectator. **Fencing:** clients and hosts ignore snapshots and forwarded
actions about an entity from a shard that doesn't own it in the current
epoch. A host that was only cut off and comes back sees the new epoch and
drops those entities.

- **Proves:** a network-partition test in which the old host keeps
  simulating. Its snapshots for reassigned players are ignored by every
  client, checked with a counter, and a hit it forwards about them is
  refused.
- The coordinator itself crashing ends the match (lobbies plan G6). See §6.

### S7 — The 128-player gate, and a run in real browsers

- `bench-netcode.mjs` at H = 4 and 8, N = 128. **Gate:** per-host upload and
  ms/tick at or below what S0 named as the budget (initial target ≤ 10 Mbit/s
  and ≤ 10 ms/tick at 30 Hz).
- An acceptance run with real Chrome profiles against a local anchor (like
  `net/crates/net/examples/anchor-acceptance`): 4 browser hosts plus as many player tabs as
  the machine can hold, recording peer-connection counts and the host tab's
  frame time while it also renders.
- **Proves:** the numbers are recorded in this plan with the command that
  produced them. A gate miss sends the plan to §5's fallbacks.

### S8 — NPCs driven by paths in the store

- Path record in the lobby store (D8):
  `{ mode: 'path', path: [...waypoints], startedAt, speed }`, with
  `startedAt` in the owner's clock, or `{ mode: 'live' }`. It is written
  only by the NPC's owner. Records carry an interest key (store interest,
  `store/interest.ts`), so a player receives only paths near it.
- `joinShards` merges path-driven NPCs into `view()`. For `'path'`, the
  position is computed from the record and `hostNow(ownerShard)`. For
  `'live'`, the snapshot copy is used. Switching to `'live'` is written
  before the NPC first appears in a snapshot. Switching back is written with
  a `startedAt` that continues from the NPC's last live position.
- **Hidden information:** a path record shows where the NPC will be. NPCs
  whose position is secret (behind fog of war) stay `'live'`, so `visible`
  keeps filtering them. Store visibility rules can hide their records, but
  the safe default is that secret NPCs don't use paths.
- **Proves:** a test drives an NPC through path → live → path. The
  client's computed position stays within a stated tolerance of the host's
  authoritative position the whole way, with no jump at either switch. An
  NPC hidden by `visible` never appears in a player's view by way of a
  path record (`assertHidden`). The S0 benchmark with 300 NPCs, 80% on
  paths, records the upload saved.

## 4. Order

S0 → (stop rule) → S1 → S2 → S3 → S3b → S4 → S7 (first gate) → S5 → S6 → S8.
S5 and S6 come after the gate on purpose: they only matter once the
steady state is shown to work. S8 is an optimization; S0's NPC dimension
says how much it is worth before it is built.

## 5. Risks

| Risk | Fallback |
|---|---|
| **Peer connections per host tab.** A host keeps ~127 player connections + 7 host connections, each with its own DTLS/SCTP. A browser tab may run out of memory or CPU, or hit a per-page connection cap, well below the bandwidth limit. | S7 measures it. If it fails: fewer players per host (more hosts), or one native host per region of players (`meshStoreTransport` in Node, lobbies plan §7). |
| **ICE failures multiply.** Each player needs a working path to all 4–8 hosts. There's no TURN fallback yet (G7), and a pair that never goes direct stays relayed through the anchor, which moves the upload onto the anchor. | Treat a host with unreachable players as unfit at S4 selection (probe before the match starts). TURN is P5 in the lobbies plan. |
| **Trust.** Every shard host sees the whole match as a spectator and is the authority for the players it owns. D1 and S3's pose check only raise the effort needed to cheat. | Documented, not solved: "player hosts are for casual matches; ranked play needs a dedicated host." No anti-cheat claim in docs or the skill. |
| **Cross-host hit latency.** One A→B round trip more than a hit on the same shard. | S3 measures it. If it feels wrong: show the hit locally right away and correct from B's answer. |
| **Hosting while playing.** The host tab simulates, sends snapshots *and* renders Three.js. | S7 records frame time. Move the node to a worker if the leader-tab design allows it (out of scope here). |
| **Split ownership after a partition.** | Epoch fencing (S6). Its test runs the cut-off host live, not stopped. |
| **The estimates are wrong.** | S0 runs first and has a stop rule. |
| **NPC count outgrows the budget.** Every visible NPC costs as much as a visible player. | S0's NPC dimension sets a per-match NPC budget. Then S8's paths, then fewer NPCs alive at once (a game decision). |
| **NPC AI reacts to stale positions.** Players on other hosts are ~100–150 ms old to the AI. | Accepted for normal AI. Games that need tighter NPCs give them to the target's host (the rejected alternative, §6). |

## 6. Not in scope

- Real anti-cheat or any security claim for player hosts.
- Splitting by area (the `world/` regions) for arenas. That remains the
  answer for large open worlds (lobbies plan §9).
- Recovering when the coordinator itself fails, and moving the lobby to
  another player. A crashed lobby node ends the match (G6).
- TURN and other reachability work (P5 in the lobbies plan).
- Matchmaking, skill rating, host selection beyond the volunteer and probe
  rule in S4.
- Rollback, lockstep, and delta snapshots on the netcode wire.
- Sending distant NPCs (or players) less often.
- Handing an NPC to the host of the player it is attacking. Measure after
  S7 if NPC→player hits across hosts feel slow.
- NPC AI itself, pathfinding, and spawning rules: game code.
- Bindings other than TypeScript (lobbies plan §1: this audience writes
  TS/JS).
