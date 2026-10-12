# Sharded matches across player hosts — plan

**Status:** in progress, as an **experiment**. Revised after review on
2026-10-12 (§8). No release targeted. **S0 and S0c done; S0b harness
built and passing single-machine runs (2026-10-12)**, with transport
overhead (~160–170 B per frame) now the limiting term. The multi-machine
S0b run is next.
With the netcode codec, snapshots are ~7–8× smaller than JSON. One host's
**payload** for 128 players at 20 Hz is 5.7 Mbit/s, but measured
transport overhead projects its **wire** upload to ~9.9 Mbit/s, at the
budget. 4 hosts fit 300 NPCs (5.9 Mbit/s payload). A single host with
300 NPCs is CPU-bound (~17 ms/tick) (§3 S0, S0c and S0b results).
Whether sharding is needed at all is decided after the multi-machine
S0b run.
**Scope:** `@net-mesh/browser`: the netcode module (`src/netcode/`), reuse of
the world module's forwarded actions (`src/world/`), and the lobby store
(`src/lobby.ts`). There is **no change to the leaf, the anchor or the core
wire**. The netcode frames (TypeScript, `netcode/wire.ts`) gain one optional
field (§2 "Assignment, epochs and leases").
**Companion to:** [browser lobbies and large worlds](BROWSER_LOBBIES_AND_LARGE_WORLDS_PLAN.md)
(§6 netcode model 2, §9 region hosts).
**Source baseline:** `fccd54453` (master), branch `LZL0/threejs-scale`.
**Audience:** the team.

---

## 1. The gap

The target is one match of **64 v 64 players** (128 in total), hosted by
**4–8 of the players' own machines** with no dedicated server. Every player
sees one shared arena with responsive movement and fair hit decisions,
within the combat scope in §2.

What exists today, checked in the code:

- **Netcode has one authority per match.** `hostNetcode`
  (`browser-ts/src/netcode/host.ts`) runs the tick, applies inputs, sends
  snapshots to every player and keeps the rewind history. `joinNetcode`
  (`netcode/client.ts`) takes exactly one `host`.
- **A host's upload grows with players × entities each can see × tick
  rate.** Snapshots are full states, not deltas (`snapshotFrames`), sent
  separately to each player. The estimate below covers payload only,
  assuming ~80 B of JSON per entity and 127 receivers:

  | | 1 host | 4 hosts (each) | 8 hosts (each) |
  |---|---|---|---|
  | everyone sees everyone, 30 Hz | ~312 Mbit/s | ~78 Mbit/s | ~39 Mbit/s |
  | interest/fog (~30 visible), 30 Hz | ~73 Mbit/s | ~18 Mbit/s | ~9.1 Mbit/s |
  | interest/fog, 20 Hz | ~49 Mbit/s | ~12 Mbit/s | ~6.1 Mbit/s |

  These are **estimates**. The payload alone already nearly fills a
  10 Mbit/s budget at 8 hosts and 30 Hz. On top of it come frame envelopes,
  transport overhead, the hosts' spectator streams to each other (~2.2 Mbit/s
  per host at 30 Hz, §2), and uneven visibility. S0 and S0b measure all of
  them.
- **Every host serves every player.** In this design, adding hosts reduces
  the entities each host owns. It does **not** reduce the 127 player
  connections each host keeps (plus one to each other host). Whether a
  browser tab can hold that many is the first question (S0b), and adding
  hosts can't fix it if it can't (D9).
- **Nothing has been measured at this size.** `scripts/bench-world.mjs`
  benchmarks the store, not netcode, and stops at 16 players. It runs over
  `createLocalMesh`, which delivers by in-process call (`src/local.ts`), so
  it measures serialization and application work, but not WebRTC connection
  cost, real upload, NAT behavior, or hosting while rendering.
- **The input watermark lives only inside netcode.** The host's
  `Player.lastApplied` and the client's `seq`/`pending` (`host.ts`,
  `client.ts`) are internal. `world/handoff.ts` moves an entity's *state*
  and nothing else. Moving a player between hosts without applying an input
  twice needs new API (S5).
- **What `world/` provides for this:** it splits the map by area (`r:x:z`)
  on the reliable store. That's the wrong split for a dense arena (§2), but
  three of its pieces don't depend on area and carry over:
  `regionHandoffs(...).forward` (each cross-host action applied at most
  once), `handoff`, and `handoffLink`'s per-sender authentication (`peerOf`).
- **Nothing decides who runs an NPC.** `snapshot()` takes any entity map, so
  NPCs can be sent. Nothing assigns their AI to a host, and each visible NPC
  costs as much upload as a visible player.
- **Lobby capacity is not the limit.** `MAX_LOBBY_CAPACITY` is 256
  (`src/lobby.ts`).

## 2. The design

### Shard by ownership

Each **shard host** is a player's node that owns a fixed set of entities
(players first, NPCs later) for the whole match: 16 players per host with
8 hosts, 32 with 4. Ownership doesn't depend on position, so ordinary
movement never moves an entity between hosts. A fight around an objective
can't turn into a burst of handoffs.

```
            ┌──────── lobby node (coordinator; also a shard host) ─────────┐
            │ store: roster, teams, score, phase, ASSIGNMENT               │
            │        (epoch, lease, shard → host, entity → shard)          │
            └───────────────────────────────────────────────────────────────┘
 shard:0 host ── hostNetcode ── snapshots of the entities it owns ──► every player (D9)
 shard:1 host ── hostNetcode ── …                                  ► every player
   …                 ▲ inputs only from the players it owns
   └─ host ↔ host: spectate each other (joinNetcode, no local entity)
                   + forward('hit', …) over handoffLink, applied once per id
```

- **Simulation.** Each shard host runs one `hostNetcode`. `step` applies the
  inputs of the players it owns, and `snapshot` returns the entities it
  owns. `visible`/`interest` filter per receiver.
- **Players** join every shard host and merge the snapshots into one view
  (`joinShards`, S1). They predict and send inputs only on their owning
  shard. D9 keeps a fallback topology in reserve.
- **Hosts spectate each other** (`joinNetcode` with no `local`) for
  line-of-sight checks, `visible` decisions and hit checks. `visible` lets
  other hosts receive everything. This costs real upload, about 2.2 Mbit/s
  per host at 30 Hz with 8 hosts, and it counts toward the budget.
- **One authority per entity.** Each entity has exactly one owner per
  epoch. There is no consensus between hosts in combat: the target's owner
  decides (S3), matching `world/border.ts`'s rule that the owner's authority
  decides.

### Alternatives considered

| Approach | Why rejected |
|---|---|
| **One dedicated host** | The right answer when the game has real stakes, and it stays the recommendation for those. Rejected only because this plan's premise is no server. |
| **Split by area** (`world/` regions) | Load follows the fighting, crossing a border mid-fight freezes the entity for at least one round trip, most fights in a small arena are near a border, and it runs on the reliable store. |
| **One simulating host plus a relay tree** | Spreads the upload, but adds a hop to every snapshot, and the simulating host's CPU and trust problems don't move. |
| **Split by team** (2 hosts) | Not enough hosts for the bandwidth, and each host is the authority for its own team. |

### Topology: who connects to whom (D9, decided by S0b)

| | **A. Every player joins every host** (default) | **B. Home host** (fallback) |
|---|---|---|
| Connections per host | 127 players + 7 hosts | its 16–32 own players + 7 hosts |
| Upload per host | its own entities × 127 receivers | the full visible view × its own players. Same total egress, about the same per host. |
| Remote entity latency | direct from the owner | one more hop (owner → home host → player) |
| Client | `joinShards` merges N hosts | plain `joinNetcode` to one host. The merge moves to the host. |
| Trust | a host only controls what it owns | a home host also controls what its players see of everyone else |

A is the default because it has fewer hops and a home host controls less.
If S0b shows that a host tab can't hold ~134 connections, B is the answer.
Adding hosts doesn't help, because under A every host still serves every
player.

### Assignment, epochs and leases

Ownership is an explicit contract and is kept above the leaf and core wire.

- **The assignment** lives in the lobby store, written only by the lobby
  node:
  `{ epoch, hosts: { [shard]: nodeHex }, owner: { [entityId]: shard } }`.
  Everyone, hosts and players alike, holds a replica of it.
- **Every message that carries authority carries the sender's epoch.** The
  netcode snapshot (`s`) and input (`i`) frames get an optional `ep` field.
  `decodeFrame` ignores fields it doesn't check, so this is additive.
  Forwarded actions and their results carry `ep` in their body.
- **Rules, for a participant at installed epoch N:**
  - A **host** simulates and sends an entity only if `owner[entity]` is
    itself at epoch N **and its lease is live** (below). It applies an input
    only for an entity it owns at N. It refuses a forwarded action whose `ep`
    isn't N, or whose target it doesn't own, with the reason `stale-epoch`.
  - A **client** draws an entity only from the shard that owns it at N.
    Snapshot entries with `ep < N` are dropped. A snapshot with `ep > N`
    means the client is behind: it holds that shard's entities at their last
    state until its replica installs the newer assignment.
  - Inputs and actions queued under an older epoch are refused or dropped on
    arrival. Nothing is moved over to the new epoch automatically.
- **Leases bound how stale a participant can be.** A lease is the only way
  to fence a participant that hasn't heard about a newer epoch:
  - The lobby node renews every host's lease through the store (a counter
    bumped every `leaseMs / 3`). A host whose lease has gone `leaseMs`
    without renewal, measured on its own clock, **stops simulating and
    sending**.
  - A client that hasn't heard from the coordinator for `leaseMs` freezes
    its view and shows "reconnecting".
  - The lobby node gives a lost host's entities to another host **only after
    `leaseMs` plus a clock-drift margin** has passed since it last renewed
    that lease.
- **The guarantee, stated narrowly:** no entity is simulated by two hosts at
  the same time, provided clock drift between hosts stays inside the margin.
  The cost: if the lobby node is unreachable for longer than `leaseMs`, the
  whole match pauses. The lobby node is already a single point of failure
  (lobbies plan G6), and this makes it one explicitly.

### Clocks and hit claims

Each `joinNetcode` client keeps one `ClockEstimator` per host. A fire input
carries two times, read from the same render frame:

- `seenA`: `TickInput.seen`, in the clock of A (the shooter's owner).
- `seenB`: `clients[B].hostNow() - interpolationDelayMs`, in the clock of B
  (the target's owner).

A rewinds the shooter at `seenA` and forwards `{ shooter, target, poseA,
seenA, seenB, ep }`. B then:

1. rewinds its own target at `seenB`, refusing if that is beyond its
   `maxRewindMs`;
2. checks `poseA` against **its spectator history of A at A-time `seenA`**,
   not against its current view. Snapshots carry the host time `t`, so B's
   history of A is indexed in A's clock. That needs a small new client API,
   `NetcodeClient.sampleAt(hostTime)`, over the existing `SnapshotBuffer`.
   The tolerance is `maxSpeed × (snapshot interval + B's measured jitter to
   A)` plus a fixed epsilon;
3. refuses with `no-history` when its history of A doesn't cover `seenA`
   (lost snapshots, or too old). It counts these refusals, never accepts
   without a check, and S3 measures the rate.

`seenA` and `seenB` are claims from the client. As in single-host lag
compensation, they are bounded by the rewind caps and nothing more. Raising
the cap (D5) gives cross-host hits room for the extra hop. It does not
resolve disagreement between the two clocks; the history check above is
what handles that.

### Combat scope for the first game

**In scope:** hitscan weapons, collision against static world geometry,
and players that don't block each other (they pass through). Each death is
decided by the target's owner, so **simultaneous kills across hosts are
trades: both stand**, and that is defined behavior.

**Out of scope, each needing game-specific rules between hosts:**
player–player collision, moving cover, projectiles, explosions and area
damage, knockback. They aren't ruled out forever; they're named here so
"one shared arena" isn't read as a promise to support them.

### NPCs (after the player-only proof)

NPCs are entities whose owner runs their AI instead of applying inputs. They
use the same netcode path, merge, rewind history and epoch rules.
**Ownership (D7):** fixed at spawn as `hash(npcId) mod hosts`, recorded in
`owner`. Fixed objectives belong to the lobby node. Hits in either direction
use S3's path, and NPC AI sees players on other hosts ~100–150 ms late.
Two alternatives were rejected: handing an NPC to its target's host (churn
on every target switch) and splitting NPCs by area (hot spots). NPC counts
are part of S0's load from the start, but NPC features (N1, N2) come after
S7.

### Decisions

| # | Decision | Recommendation |
|---|---|---|
| D1 | Which players a host owns | **One squad from each team, never the host player's own squad.** That makes the plain "god mode for my own character" exploit impossible. **Not** a security boundary (§5). |
| D2 | How hosts are found | **The lobby store's assignment** (above). Not `announceRegions`: the lobby already owns its node's announcement, and an assignment can't be ambiguous. |
| D3 | Where the code lives | **`joinShards` in `@net-mesh/browser/netcode`.** Host-side wiring is a demo under `browser-ts/demo/` plus a skill section, until a second game needs it as an API. |
| D4 | Number of hosts, tick rate | **Changed 2026-10-12 (S0 encoding experiment): with compact snapshots (S0c), 1 host for player-only matches at 20 Hz, and 2–4 hosts with NPCs or at 30 Hz.** The earlier "8 hosts at 20 Hz" was sized for JSON, at ~8.9 Mbit/s with no margin. S0b sets the final number from measured overhead. |
| D5 | Rewind cap for cross-host hits | **300 ms** (default 200), to be fixed by S3's measured age of `seenB` at B. |
| D6 | Durability | **None, and crash recovery is a rollback** (S6). The match lives only in memory. Spectators' copies of a crashed host's entities survive it, so recovery from them is a policy, not a continuation. |
| D7 | Who owns an NPC | **Fixed at spawn, `hash(npcId) mod hosts`.** Objectives belong to the lobby node. |
| D8 | Where NPC paths live (N2) | **The lobby store, written only by the NPC's owner** (checked against the assignment). |
| D9 | Topology | **A (every player joins every host)**, falling back to **B (home host)** if S0b fails the connection count. |
| D10 | Lease length | **3 s.** Shorter makes a match pause on brief coordinator hiccups. Longer delays crash recovery (S6). S0b records how long coordinator renewals actually take. |

## 3. The slices

Each slice leaves the tree working. Sizes are bytes, CPU is ms per tick, and
every number is recorded with the command that produced it.

### Phase 1 — feasibility gates

#### S0 — CPU and bytes: `scripts/bench-netcode.mjs` (local mesh)

`hostNetcode`/`joinNetcode` over `createLocalMesh` with N players, H hosts
(split by ownership, plus spectating between hosts), configurable entity
bytes, tick rate, interest/visibility, and NPC count as extra moving
entities.

- Points: H = 1, 4, 8 × N = 16, 64, 128 × {everyone sees everyone, interest}
  × {20, 30} Hz × NPCs = 0, 100, 300.
- Reports per host: ms/tick, payload and frame bytes/s, split into
  player-facing and host-facing. Per player: bytes/s received.
- **What it can't show:** connection cost, real upload, NAT, rendering
  (S0b).
- **Stop rule:** if a single host at 128 players with interest filtering
  fits the budget (payload + measured overhead ≤ 10 Mbit/s, ≤ 10 ms/tick),
  sharding isn't needed and the plan ends here.

**Result — S0 built and run (2026-10-12).** `npm run bench:netcode`
(`browser-ts/scripts/bench-netcode.mjs`, default sweep, 40 measured ticks
per point), Node 24.19, i9-14900K, Windows. All 54 points ran with every
client joined (`notReady` 0). Upload is netcode frame payload (JSON, frame
envelope included) for the busiest host. Transport overhead (Noise,
SCTP/DTLS, UDP/IP) is not included. "ctl" is clock pongs at the real 4/s
per client. Selected rows, interest mode:

| H | N | NPCs | host ms/tick med / p95 | receivers/host | B/host/tick → players / → hosts | Mbit/s/host @20 / @30 (incl. ctl) | entities/player/tick | B/entity |
|---|---|---|---|---|---|---|---|---|
| 1 | 128 | 0 | 6.13 / 6.31 | 128 | 258,728 / 0 | 41.7 / 62.4 | 24.2 | 84 |
| 4 | 128 | 0 | 1.85 / 1.95 | 131 | 74,511 / 7,931 | 14.4 / 21.4 | 25.3 | 92 |
| 8 | 128 | 0 | 1.24 / 1.34 | 135 | 41,301 / 9,480 | **8.9** / 13.2 | 24.8 | 104 |
| 8 | 128 | 100 | 1.76 / 1.91 | 135 | 57,607 / 14,684 | 12.4 / 18.5 | 42.2 | 85 |
| 8 | 128 | 300 | 2.86 / 3.59 | 135 | 92,342 / 25,294 | 20.5 / 30.5 | 78.0 | 74 |
| 1 | 128 | 0 (full) | 28.09 / 32.59 | 128 | 1,338,102 / 0 | 214 / 321 | 128 | 82 |
| 8 | 128 | 0 (full) | 2.13 / 2.25 | 135 | 173,289 / 9,476 | 29.7 / 44.4 | 128 | 85 |

What it says:

1. **The stop rule does not fire.** A single host at 128 players with
   interest filtering needs ~42 Mbit/s at 20 Hz. Sharding is needed.
2. **Upload, not CPU, is the limit.** Every sharded point is under 4 ms/tick
   at p95. Every budget miss is bandwidth.
3. **Only one configuration fits, and barely: 8 hosts, 20 Hz, interest, no
   NPCs, at ~8.9 Mbit/s.** That leaves ~1 Mbit/s for transport overhead,
   which S0b measures. 4 hosts (14.4) and 30 Hz (13.2) are over budget
   before overhead. D4 (8 hosts at 20 Hz) stands, with no margin.
4. **Spectating between hosts is ~19% of a host's upload** at 8 hosts
   (9.5 KB of 50.8 KB per tick), as the review predicted. Clock pongs add
   ~0.3 Mbit/s.
5. **NPCs don't fit as they are.** 100 visible-on-average NPCs take 8 hosts
   to 12.4 Mbit/s, and 300 to 20.5.
6. **The encoding is the cheapest lever.** An entity costs 74–104 B as
   JSON, and its 16-hex-digit id key alone is ~19 B of that. A compact
   binary snapshot encoding (short ids, quantized floats) is likely to cut
   the dominant term by half or more without changing the topology. That
   is a netcode-module change, not a leaf, anchor or core change. It is
   recommended **before S1** if S0b confirms the overhead leaves no room.

**Compact encoding experiment (2026-10-12).** The same harness now also
encodes every snapshot frame it captures with a prototype binary codec,
`compactCodec` in `bench-netcode.mjs`. It is measurement only; the wire is
unchanged. The layout:

- a 19 B frame header;
- per entity, a 2 B match-local index instead of the 16-hex-digit key;
- `x`/`z` quantized to 16 bits over the arena (~3 mm at its ~200-unit
  side);
- yaw in 8 bits (~1.4°), then hp, ammo and team (or an NPC's hp and kind)
  at 1 B each;
- 10 B per player, 9 per NPC.

The first 200 frames of every point are decoded again and checked against
the original. That check caught a codec bug on its first run: a position
rounded after being clamped can sit a hair past the edge, and it wrapped
to 0 in a u16. Fixed by clamping in the quantizer. Same command, flags
`--players 128` and `--hosts 1|2|4|8`. Compact output is **0.13–0.15 of
JSON at every point (~7× smaller)**. Busiest host, interest, pongs
included:

| H | NPCs | JSON Mbit/s @20 / @30 | compact Mbit/s @20 / @30 |
|---|---|---|---|
| 1 | 0 | 42.0 / 62.8 | **5.7** / 8.3 |
| 1 | 100 | 61.9 / 92.8 | 8.8 / 13.0 |
| 2 | 0 | 23.3 / 34.8 | 3.4 / 4.9 |
| 2 | 100 | 36.1 / 54.0 | **5.3** / 7.8 |
| 2 | 300 | 58.7 / 87.9 | 8.7 / 12.8 |
| 4 | 300 | 35.4 / 52.9 | **5.4** / 8.0 |
| 8 | 0 | 8.9 / 13.1 | 1.6 / 2.2 |

**What changes:**

1. **With compact snapshots, one host fits 128 players (no NPCs) at
   20 Hz**, at ~5.7 Mbit/s, leaving ~4 Mbit/s for transport overhead. Once
   the codec exists, the stop rule may fire for player-only matches. That
   depends on S0b's overhead and connection numbers.
2. **Sharding is then for NPCs, 30 Hz and headroom, and needs 2–4 hosts,
   not 8.** Fewer hosts also means less spectating between hosts, fewer
   connections per player (topology A), and fewer machines that can fail.
   **D4 changed** (below).
3. **The codec benefits every netcode game, sharded or not.** It is the
   most valuable single change found so far, so it becomes the next slice
   (**S0c**), and S0b measures it rather than JSON.

Host CPU at a given point varies between runs by up to ~40%. For example,
H=1 interest no-NPC measured 6.13 and 3.74 ms median. Treat ms/tick as an
order of magnitude. CPU stays far from the 10 ms budget in every sharded
point either way.

The harness simplifies in three ways: hosts are separate nodes, not
players (128 receivers + H − 1 spectators per host). With N = 16, squads of
8 leave some hosts owning nothing. And the clients' ping timer runs in real
time while ticks run as fast as possible, which is why pongs are computed
rather than counted.

#### S0c — Compact snapshot codec in the netcode module

Added 2026-10-12 after the S0 encoding experiment.

- **API:** an optional, game-supplied `codec` on `hostNetcode`/`joinNetcode`
  (`encodeEntity`/`decodeEntity` with a fixed byte size, or a schema of
  quantized fields). JSON stays the default, so every existing game keeps
  working unchanged.
- **Ids:** entities are named on the wire by a 16-bit index from a table
  the host publishes reliably (in the store, or a reliable netcode frame).
  An index is never reused within a match, so a late or reordered snapshot
  can't refer to the wrong entity.
- **Frames:** a binary `s` frame next to the JSON ones, with the frame kind
  in the first byte. `decodeFrame` keeps refusing what it doesn't
  recognize.
- **Chunking:** sized from the binary length, so far fewer frames are
  chunked.
- **Proves:**
  - a property test that encode → decode is exact within each field's
    quantum, the arena edges included (the S0 bug, kept as a named case);
  - a reused or unknown index is refused, not drawn;
  - existing JSON netcode tests pass unchanged;
  - `bench-netcode.mjs` measures the real codec, replacing the prototype,
    within 10% of the prototype's bytes;
  - host ms/tick no worse than JSON.

**Result — S0c built (2026-10-12).** `browser-ts/src/netcode/codec.ts`:
`entityCodec` over `quantized`/`angle`/`uint`/`int`/`float32`/`bool`
fields, a binary snapshot frame (magic `0xb1`, label hash, varint header,
names, entities), and negotiation by codec fingerprint on `h`/`p` frames
with a `u` frame to request lost names. The design differs from the sketch
above in three places:

- **Names travel with snapshots, not in a separate table:** an entity's
  name rides the first snapshot that shows it to a player, and the player
  asks again for any index it can't name. No acknowledgements are needed.
- **Indexes are varints, not u16**, so there's no cap of 65,536 entities
  per match, and they're 1 B up to 127.
- **Reconciliation compares within the codec's quanta** (`codec.near`).
  Otherwise every quantized snapshot would read as a correction.

Evidence:

- **Tests:** `test/netcode/codec.test.ts`, 19 tests, covering:
  - field property tests, with the S0 edge-wrap bug as a named case;
  - frame round-trip, every truncation refused, trailing byte and duplicate
    index refused;
  - JSON fallback for a mismatched codec and for no codec on either side;
  - lost-name recovery;
  - JSON fallback when the codec refuses an entity;
  - index never reused;
  - chunking by binary size.
- **Mutations:** five mutations each make a test fail: removing the clamp,
  exact correction compare, ignoring `u` requests, reusing an index, and no
  fallback.
- **Full suite:** `npm test`, 1,030 tests passed.
- **Benchmark:** `npm run bench:netcode` now takes `--codec json|compact`.
  Full sweep, 108 points, all joined, every compact snapshot binary. Players
  and NPCs share a 9 B schema (x, z, yaw, hp, ammo, team, kind). Interest,
  128 players, busiest host, pongs included:

| H | NPCs | JSON Mbit/s @20 / @30 | compact Mbit/s @20 / @30 | prototype @20 | host ms/tick med, JSON → compact |
|---|---|---|---|---|---|
| 1 | 0 | 46.2 / 69.1 | **5.7** / 8.5 | 5.65 | 6.50 → 4.97 |
| 1 | 100 | 73.0 / 109 | 9.5 / 14.1 | 8.75 | 10.80 → 8.80 |
| 1 | 300 | 137 / 206 | 18.6 / 27.7 | 16.1 | 24.93 → **16.73** |
| 4 | 0 | 15.8 / 23.6 | 2.3 / 3.3 | 2.24 | 1.10 → 1.51 |
| 4 | 300 | 43.3 / 64.8 | **5.9** / 8.6 | 5.41 | 3.23 → 3.93 |
| 8 | 0 | 9.7 / 14.4 | 1.7 / 2.3 | 1.58 | 1.26 → 0.98 |
| 8 | 300 | 24.8 / 37.0 | 3.6 / 5.2 | — | 3.01 → 2.26 |

The real codec is within 2–8% of the prototype's bytes; its frames carry
an exact f64 time and one more field. Host CPU is in the same band as JSON:
lower at most points, higher at a few within the run-to-run noise noted in
S0. **New finding:** a single host with 300 NPCs costs ~17 ms/tick, over
the 10 ms budget. With many NPCs, CPU becomes the reason to shard before
bandwidth does.

#### S0b — Topology, in real browsers (the gate for D9)

- **Hosts:** 4 Chrome instances on separate machines with rendering on
  (a Three.js scene of the target complexity). **Players:** headless
  Chromium instances spread over enough machines to reach **127 player
  connections per host**, against a real anchor (as in
  `net/crates/net/examples/anchor-acceptance`). That's a fixed target, not
  "as many tabs as fit".
- Records per host: peer-connection setup time and success rate, memory,
  CPU, frame time while hosting, real upload, and **direct versus
  anchor-relayed pairs, measured separately**.
- **Passes** when every host holds 127 + 3 connections at D4's rate within
  budget, with rendering at its target frame rate. **Fails over to D9-B**
  when the connection count is what breaks, not the bandwidth. B is then
  measured with the same harness.
- Also records how long the coordinator's lease renewals take (for D10).

**Progress — harness built, single-machine runs (2026-10-12).** The
harness is `net/crates/net/examples/shard-topology/` (`node run.mjs
coordinator …` plus `node run.mjs browsers …` on other machines; the
coordinator prints the commands with its port and certificate pin).

**How it measures:**

- Upload is **wire** bytes, from Chrome's own `RTCPeerConnection.getStats()`
  transport stats. That includes DTLS, SCTP and Noise. The page wraps the
  constructor before the package loads, and the first connection (the
  anchor's) is counted separately as "via anchor".
- Payload is what `hostNetcode` hands the transport.
- Main-thread busy time comes from CDP `Performance.getMetrics`.
- Each host renders the merged world every frame, burning `--burn-ms`
  (default 4) as a stand-in for a heavier scene.
- The run passes only when every host holds `N + H − 1` netcode peers, all
  of them direct, with upload ≤ `--budget-mbit` and a median frame rate
  ≥ 90% of `--target-fps`.

Release `net-mesh` built `--features rtc-bootstrap`, Chrome 155, compact
codec, 20 Hz, one i9-14900K Windows machine running everything:

| H × N | result | direct pairs per host | wire up Mbit/s per host | payload | overhead | first snapshot p50 / max | host fps | main thread busy |
|---|---|---|---|---|---|---|---|---|
| 2 × 2 (JSON) | pass | 3/3 | 0.22 / 0.15 | 0.12 / 0.05 | 1.9× / 3.0× | 632 / 635 ms | 60 | 0.26 |
| 2 × 16 | pass | 17/17 | 0.77 / 0.76 | 0.25 / 0.24 | 3.1× / 3.2× | 635 / 747 ms | 60 | 0.34 |
| 1 × 32 | pass | 32/32 | 2.19 | 1.15 | **1.9×** | 590 / 726 ms | 60 | 0.47 |
| 1 × 64 | **inconclusive** | — | — | — | — | 7,969 / 28,600 ms | — | — |

Findings:

1. **Transport overhead is the next limit, not payload.** At these sizes it
   is a fixed ~160–170 B per frame the host sends, including what it spends
   acknowledging inbound inputs and pings. Example: at 1 × 32, wire minus
   payload is ~130 KB/s over ~770 frames/s (640 snapshots + 128 pongs).
   Projected to **1 host × 128 players: ~5.7 Mbit/s payload + ~4.2 Mbit/s
   overhead ≈ 9.9 Mbit/s, at the budget with no margin.** The
   multi-machine run must measure it. Levers, if it is confirmed: a lower
   tick rate (both terms scale with it), fewer pongs once the clock has
   settled, or 2 hosts.
2. **1 × 64 on one machine is inconclusive, not a failure of the
   protocol.** 65 Chrome renderers and the anchor shared one CPU. Joins
   were slow; connect outcomes were 33 direct, 13 `iceTimeout` and
   18 `error:session`. The host's reports never reached the measuring
   window. Rerun it on several machines before drawing any conclusion.
3. **Every page must announce to be reachable** (found building the
   harness). A browser node accepts a handshake only from a peer whose
   signed announcement it holds. The lobby announces for its users, but a
   raw `joinNetcode` against a host that never received the player's
   announcement waits forever: its internal `connectPeer` *resolves*
   `noAnnouncement`, which nothing reports. **Defect, deferred:**
   `joinNetcode` should surface the connect outcome (a stat, or a typed
   error after a bounded wait).
4. **The anchor's per-address limits** (10 bootstrap offers and 30
   credentials per IP per minute) refuse a lab machine running many player
   tabs (`429 rate_limited`). The harness raises them with
   `--offers-per-minute` / `--credentials-per-minute` and records the value
   in its results. Real players have their own addresses.

**Follow-up fixes (2026-10-12), each with tests:**

- **Finding 3 fixed:** `joinNetcode().ready()` rejects with a typed
  `NetcodeError` (`no-announcement`, `unreachable`, `timeout`, `closed`).
  `stats()` gains `connectOutcome` and `lastError`
  (`test/netcode/join.test.ts`).
- **Fewer clock pings once the clock has settled** (4/s, then 1/s after
  8 samples). Measured at 1 × 32: host wire upload 2.19 → 1.82 Mbit/s.
- **Finding 4 fixed in the anchor:** offers are charged per (address,
  credential), 10 a minute. The per-address ceiling is a backstop, 10 →
  120, and credentials per address go 30 → 120. Witnesses are in
  `sdk/tests/rtc_bootstrap_listener.rs`, three of them pinned in ci.yml.
- **New defect, found and fixed: a re-offer storm.** At 1 × 64, one
  player's direct attempt ended `iceTimeout`. `joinNetcode` then
  re-offered on every stream reopen; each fresh ICE attempt that ended
  late replaced the session, so the stream went stale. That was 44
  attempts in five minutes, and no snapshot ever arrived. A relayed pair
  now keeps its stream, and direct is retried at most every
  `directRetryMs` (30 s). After the fix, all 64 players join.
- **What `error:session` was:** "did not complete the relayed Noise
  handshake inside 5000 ms". The host tab's main thread was 93% busy
  during the 64-player join burst.

**1 × 64 on one machine, after the fixes:** every player joined (first
snapshot p50 6.4 s, max 20 s). But the host held only 46 of 64 peers in
the measuring window, the host tab was 93% busy (tick p95 17.5 ms), and
wire upload was 9.4 Mbit/s against 0.67 Mbit/s of payload (14×).
**Announcement gossip was ruled out** as the cause: at 1 × 32, announcing
every 30 s instead of every 2 s left the overhead unchanged (1.87× vs
2.00×). The rest can't be separated from CPU saturation while one machine
runs the host and all 64 players. Assume nothing about a browser host's
capacity above 32 players until the multi-machine run measures it.

**Credit leak on lossy streams, found and fixed (2026-10-12).** The
staggered 1 × 64 run (players joining 500 ms apart) left its last two
relayed players with `backpressure: … needs 192 bytes of send credit and
has 34`. Cause: a stream's credit is
`window − (sent − receiver's consumed)`, and on a FIRE-AND-FORGET stream a
lost packet is never consumed. Its bytes stayed outstanding forever, the
64 KiB window drained (~402 frames), and the stream could never send
again; a reopen keeps the label's stream id and the drained state.

**Fix**, in `net-mesh-wire` `StreamState`: a fire-and-forget stream whose
credit made no progress for `FIRE_AND_FORGET_RECLAIM_AFTER` (2 s) with
bytes outstanding writes them off as lost. Later grants are offset by the
written-off total, since the receiver never counts those bytes. Reliable
streams, and promoted ones, are never written off. A late arrival is
clamped to the sent watermark.

**Evidence:**
- five witnesses in `wire/src/session.rs`, each checked by a mutation;
- wire 288 / native credit units 138 / stream integration 6 /
  `rtc_backpressure` 7 / leaf native 485, all green;
- **rerun: all 64 players joined**, including 14 whose direct attempt
  failed and who rode the relay (first snapshot p50 598 ms, max 27 s).

The host page's reports stopped 65 s before that run's verdict. It's a
harness gap: one `getStats()` that never resolves stalls every later
report. Fixed with a per-connection timeout.

**Split view of a pair, found and fixed (2026-10-13).** The per-pair
dump (`node.peerAttempt` from both ends of each failed pair) showed 3 of
10 failures with the SAME dialog seen two ways:

- the player (offerer) had `failed`, `direct:false`;
- the host (answerer) had `open`, `direct:true`, and was holding the
  player in its netcode.

Cause, in `leaf/src/wasm.rs`:

1. The offerer's Noise wait shares its 10 s ICE deadline, so on a slow
   host message 2 arrived late; the offerer settled `failed` and kept its
   routed session.
2. The answerer had cleared the relay at message 1, and `installed()` was
   `has_session && no relay`. The routed session was still in the table,
   so the answerer reported direct before anything was proven.
3. Nothing rolled it back: the proof sweep destroyed only the keys, not
   the attempt or the relay entry.

That broke the transport plan's §9 steps 4 and 6 and §5.

**Fix:**

- `installed()` now requires no pending establishment proof.
- An admission that expires unproven restores the relay its message 1
  displaced, keeps the routed session, and ends the attempt `Relayed`.
- The answerer's own deadline waits while a proof is pending, which the
  5 s proof deadline bounds.
- Supersession and close keep their designed destructive teardown; their
  witnesses are unchanged.

**Evidence:**

- the witness
  `an_establishment_abandoned_by_its_offerer_returns_the_pair_to_its_relay`,
  run in headless Chrome with chromedriver 155, failing under both
  mutations;
- leaf wasm suites all green (lib 24, `wasm_leader` 38 at CI's 120 s
  runner timeout, `wasm_leaf` 16);
- leaf native 485.

Rerun (staggered 1 × 64): 57/64 joined and **no split-view pair
remains**. The 7 left:

- 4 relayed pairs the host never hears (bug 2, both ends `iceTimeout`,
  host not holding the player);
- 3 of the last joiners never saw the host's announcement within 300 s.

Both sit on a machine running 65 tabs; the multi-machine run separates
them from load.

**Bug 2, relayed pairs the host never heard: announcement flooding
(2026-10-13).** Leaf drop counters and the host's console showed the
host's ANCHOR link pinned at its 96 KiB SCTP advisory, with 84,667 refused
sends starting in the first seconds of the join. Everything the host sent
through the anchor was refused, and every relayed player starved.

Isolating it:

| Run | Players joined | Advisory refusals |
|---|---|---|
| 1 × 16, all relayed | all joined | 0 |
| 1 × 64, announcements every 2 s | 60/64 | 84,667 |
| 1 × 64, announcements every 30 s | **64/64, every pair direct**, first snapshot ≤ 5 s | **0** |

Cause: every node re-announced every 2 s (`LOBBY_ANNOUNCE_MS`,
`REGION_ANNOUNCE_MS`, the harness). Each refresh is flooded through the
anchor to every peer, and each peer answers every packet with a credit
grant, so the load grows with the square of the players.

The lobbies plan had left the 2 s cadence as a guess "correct under
either behaviour", pending a measurement, and an announcement carries a
300 s TTL anyway. **Fix:** both constants are now 30 s. A change of
players is still announced at once. `joinLobby` gains `announceEveryMs`.

**Measured, and not separable from this machine.** With announcements
every 30 s, 1 × 64 is clean on joins: all direct, all held, 7.3 Mbit/s
wire upload. But the host tab's main thread is 99% busy (tick p95
30.6 ms, 10 fps). The wire/payload ratio (7.4×) disagrees with what the
players report receiving, so treat it as unreliable under saturation.
Both need the multi-machine run.

**Still open for S0b:** the multi-machine run at 4 hosts × 127 players
(and 1 × 128 for the single-host case the codec made plausible). It
needs ≥ 3 machines.

### Phase 2 — player-only proof

#### S1 — `joinShards`: one merged view over several hosts

`src/netcode/shards.ts`, exported from `./netcode`:

```ts
const match = joinShards<E, I>({
  transport: node, label: 'arena.move',
  assignment,                  // { epoch, hosts, owner } — the lobby record, or:
  // ownerOf: (id) => shard,   // an explicit resolver over the same epoch
  self: myId,
  local: { id: myId, predict },
  interest: cellsAround(x, z, { size: 32 }),
});
match.view();                  // merged: remote interpolated, own predicted
match.input(data);             // to owner(self) only, with `ep`
match.hostNow('shard:5');      // that shard's clock, for hit claims
match.setInterest(keys);       // fanned out to every shard
match.setAssignment(next);     // install a newer epoch (§2 rules)
```

- One `joinNetcode` per shard, with `local` only on `owner[self]`.
- An entity is drawn only from its owner at the installed epoch. Entries
  from other shards are dropped and counted. A snapshot carrying a newer
  `ep` holds that shard's entities at their last state until
  `setAssignment` catches up.
- `setAssignment` with an older epoch is ignored. When an entity's owner
  changes, the old shard's copy lingers (as `joinWorld` does with
  `lingerMs`) until the new owner's first snapshot.
- `stats()` per shard, plus counts of what epoch fencing dropped.
- **Proves** (`test/netcode/shards.test.ts`, local mesh):
  - three hosts with disjoint entities merge into one view;
  - an entity sent by a non-owner is not drawn (break the check and the
    test must fail);
  - inputs reach only the owner;
  - older-epoch snapshots are dropped after `setAssignment`, and a
    newer-epoch snapshot is held, not drawn, until the assignment arrives;
  - a silent host shows in `stats()`.

#### S2 — The shard host recipe (demo)

`browser-ts/demo/sharded-arena/` wires:

- `hostNetcode` with `authorize` limited to the roster;
- `visible` (other hosts see everything, players get fog);
- spectator `joinNetcode`s to the other hosts;
- `regionHandoffs({ region: 'shard:k', link: handoffLink({ peerOf: fromAssignment }) })`
  with no `persist` and no `ghosting`;
- a static assignment (no lobby yet, no leases).

**Proves:** 4 hosts × 32 simulated players on the local mesh, and a test
that each player's merged view matches the union of the hosts'
authoritative states within one tick plus the interpolation delay.

#### S3 — Cross-host hits (combat scope of §2)

- Fire input: `{ fire: { target, seenB } }`, with `seenA` from
  `TickInput.seen`.
- A checks the shot (ammo, cooldown, shooter alive), rewinds the shooter
  at `seenA`, and forwards `{ shooter, target, poseA, seenA, seenB, ep }`.
  For a hit between two players on the same shard, A handles it locally.
- B follows §2 "Clocks and hit claims" steps 1–3, applies damage in its
  committed state, and answers.
- New API: `NetcodeClient.sampleAt(hostTime)`.
- Timing: `giveUpMs` ~500 (a hit confirmed later than that is worse than a
  miss), `retryMs` 50, `handledRetentionMs` 10 s. An `unresolved` or
  `stale-epoch` result counts as a **miss**, never re-sent under a new id.
- **Proves** (link simulator that drops, duplicates and reorders,
  `test/netcode/sim.ts`):
  - a duplicated hit is applied exactly once;
  - a `seenB` beyond the cap is refused;
  - a `poseA` outside the tolerance against B's history is refused;
  - a legitimate fast-moving shooter is **accepted** (the review's
    false-rejection case: the check must use history, not the current
    view);
  - a gap in history yields `no-history`;
  - a mutual kill across hosts is a trade.
- **Records:** the distribution of `seenB` age at B (sets D5) and the
  `no-history` rate.

#### S7 — The 128-player gate (players only)

The S0b harness with the real game loop: S1 + S2 + S3, 128 players,
D9's topology, D4's rate, combat running. **Gate:** each host within
budget, frame time at target, hit confirmation latency across hosts
recorded, `no-history` rate below 1%. A miss sends the plan to §5's
fallbacks.

### Phase 3 — new contracts (only after S7 passes)

S5 and S6 are **new contracts, not reuse**. Existing handoff moves entity
state. These slices define what moves with it.

#### S4 — Lobby integration: assignment, epochs and leases

- The assignment record and its validator: written only by the lobby node,
  each player owned by exactly one shard, and D1's rule.
- `ep` on netcode frames and forwarded actions, the §2 rules in hosts and
  `joinShards`.
- Leases: renewals, a host stopping when its lease expires, clients
  freezing, and the coordinator waiting before reassigning.
- Host selection: volunteers, measured by a **real network probe** (a timed
  burst over the actual transport to another volunteer, checked against
  S0b's measured upload). Not S0's local method.
- Team audiences through `authorize` (`team:red` / `team:blue`).
- **Proves:**
  - a non-lobby-node write to the assignment is refused;
  - an assignment giving a host its own squad, or one entity to two shards,
    fails validation;
  - a host cut off from the coordinator stops within `leaseMs` (test with a
    simulated partition, the host left running);
  - a client cut off from the coordinator freezes within `leaseMs`;
  - forwarded actions carrying a stale `ep` are refused.

#### S5 — Planned moves: rebalancing and a host leaving cleanly

**The contract:** an entity's state and its processed-input boundary move
together, and the client's input sequence continues across the move.

- New netcode API:
  - `host.releasePlayer(peer)` → `{ lastApplied }`, taken at a tick
    boundary in the same step that freezes the entity;
  - `host.adoptPlayer(peer, { lastApplied })`;
  - in `joinShards`, one input sequencer shared by all shards, so `seq` and
    `pending` survive a change of owner.
- The handoff offer carries `{ state, lastApplied }`. The new owner applies
  only `seq > lastApplied`.
- The lobby node bumps `epoch` when the handoff reports `moved`.
- **Proves:** the review's counterexample as a named test. The old host
  applies input 41, its acknowledgement is lost, the frozen state contains
  41's effect, and the client re-sends 41 to the new host: **41 is not
  applied again**. Also: the entity is never authoritative on two shards,
  and `stats().corrections` stays within a stated bound during the move.

#### S6 — A host crashes: recovery is a rollback

Handoff can't help when the source host is dead. After the lease wait
(§2), the lobby node bumps `epoch` and gives the orphaned entities to the
remaining hosts. Each new owner **restores them from the last state it
observed as a spectator**. That is a rollback to an observed state, and
players see it. The policy:

| What | After recovery |
|---|---|
| Position, health, ammo | As the new owner last observed them. Later changes made by the dead host are lost. |
| A death the dead host decided after that observation | Undone. The player is alive at the observed state. |
| A death already observed | Stands. |
| Score | Held by the lobby store. A kill the dead host decided but never reported is lost. |
| The player's inputs | Resume at the client's current `seq` (`adoptPlayer` with `lastApplied = seq`). Pending inputs are dropped, and prediction snaps back to the restored state. |
| Actions forwarded **to** the dead host | `unresolved`, so a miss. Never re-sent to the new owner, because the dead host may have applied them and the spectator copy may already show the effect. |
| Actions the dead host forwarded that others applied | Stand. Recorded by their targets. |

- **Proves:** a partition test in which the old host keeps running stops at
  lease expiry, and **no entity is simulated by two hosts at once** (the
  counter must stay at zero). A recovered player's state equals the new
  owner's last observation. A hit that was in flight to the dead host is
  counted as a miss and never applied twice.

### Phase 4 — NPCs

#### N1 — NPC ownership and hits across hosts

NPCs in `owner` (D7), with AI run in the owner's `step` and hits in both
directions on S3's path (an NPC's `pose` comes from its owner's rewind).
Fixed objectives belong to the lobby node.
**Proves:** hits in both directions applied exactly once under drop,
duplication and reordering; an NPC→player hit with an out-of-tolerance
`pose` is refused; one NPC on two shards fails validation.

#### N2 — NPCs driven by paths in the store (a feature, not free savings)

- Path records in the lobby store (D8), carrying an interest key.
- **The switching rule:** each NPC has a mode version `mv` that its owner
  bumps on every switch between `'path'` and `'live'`. Both the store
  record and the snapshot entry carry `(mv, mode)`. The client uses
  **whichever source shows the highest `mv`**. The store is reliable and
  snapshots are not, so the two can arrive in either order; the version
  makes the order irrelevant. If the highest `mv` says `'path'` but the
  record hasn't arrived, the client carries the last live state forward
  for at most 500 ms, then holds it. A switch blends over
  `correctionSmoothingMs`.
- NPCs whose position is secret (fog of war) never use paths.
- **Proves:** a path → live → path test under random reordering between
  the store and snapshots stays within the position tolerance with no jump.
  An NPC hidden by `visible` never appears through a path record
  (`assertHidden`). The S0 benchmark records the upload saved.

## 4. Order

S0 (done) → **S0c** (compact codec) → S0b (gates; D4, D9 and whether to
shard at all decided) → S1 → S2 → S3 → **S7** (player-only gate) → S4 → S5
→ S6 → N1 → N2.

The point of this order: prove players → merged view → combat across hosts
→ measured 128-player topology **before** building ownership moves or NPC
features.

## 5. Risks

| Risk | Fallback |
|---|---|
| **Connections per host.** Under topology A, each host keeps ~127 player connections + 7 host connections whatever the number of hosts. **More hosts don't reduce it.** | S0b measures it first. Fallbacks: topology B (16–32 players per host), or a native host (`meshStoreTransport` in Node, lobbies plan §7). |
| **Bandwidth margin is thin.** Payload alone is ~9.1 Mbit/s at 8 hosts and 30 Hz. Overhead, spectating between hosts and uneven visibility come on top. | D4 defaults to 20 Hz. Further levers: a more compact entity encoding, and lower rates for distant entities (§6, not built). |
| **ICE failures multiply.** Each player needs a path to every host (under A). There's no TURN yet (G7), and relayed pairs move the upload onto the anchor. | S0b records direct and relayed pairs separately. S4's host selection rejects a host that players can't reach directly. TURN is lobbies plan P5. |
| **Hosting while rendering.** | S0b measures it with rendering on. Move the node to a worker if the leader-tab design allows it (out of scope). |
| **False rejections in the hit check.** Clock jitter or lost snapshots make legitimate hits fail. | S3 measures `no-history` and rejection rates. Widen the tolerance with measured jitter before raising the cap. |
| **Lobby node outage pauses the match** (leases). | Accepted and stated. Recovering when the lobby node itself fails is out of scope (G6). |
| **Clock drift beyond the lease margin** breaks the no-double-simulation guarantee. | Size the margin from S0b's measured drift. Hosts also refuse to simulate an entity a newer-epoch snapshot shows elsewhere (best effort). |
| **Trust.** Each host sees the whole match and decides for what it owns. Under B it also controls what its players see. | Documented: player hosts are for casual matches; ranked play needs a dedicated host. No anti-cheat claim. |
| **The estimates are wrong.** | S0 and S0b come first, with a stop rule. |

## 6. Not in scope

- Real anti-cheat, or any security claim for player hosts.
- Combat beyond §2's scope: player–player collision, moving cover,
  projectiles, explosions, knockback.
- Splitting by area for arenas (that remains the answer for large open
  worlds, lobbies plan §9).
- Recovering when the lobby node itself fails, and moving the lobby to
  another player.
- TURN and other reachability work.
- Sending distant entities less often, and delta snapshots on the netcode
  wire.
- Handing an NPC to its target's host. NPC AI, pathfinding and spawn
  rules (game code).
- Matchmaking and skill rating. Bindings other than TypeScript.

## 7. What a reviewer should check first

S0b's numbers. The review's point, which this plan adopts: the practical
limit is more likely to be browser connection fan-out or combat timing than
the choice of what to shard by.

## 8. Revisions

**2026-10-12, after review (Kyra).** Kept: ownership sharding, one authority
per entity, `joinShards` as API with the host side as a recipe, the stop
rule, and the casual-only trust model. Changed:

1. **Topology is gated before anything is built** (new S0b). The earlier
   fallback "fewer players per host, more hosts" was wrong: under this
   design every host serves every player whatever the number of hosts.
   Topology B is added as the fallback that does reduce it (D9). S0 now
   says what the local mesh can't measure. The volunteer uplink probe in S4
   is a real network probe.
2. **The bandwidth budget counts spectating between hosts** (~2.2 Mbit/s
   per host at 30 Hz, previously called negligible) and overhead. D4 now
   defaults to 20 Hz.
3. **Ownership is an explicit contract:** `joinShards` takes the assignment
   or an `ownerOf` resolver. Epoch fencing is specified (`ep` on frames and
   actions, the rules, leases that bound staleness), and its guarantee is
   stated narrowly.
4. **S5 no longer claims that input redundancy prevents double
   application.** It defines moving the input boundary together with the
   state, with new API and the review's counterexample as a test. **S6
   defines recovery as a rollback**, with a policy per kind of state. D6 is
   corrected: spectator copies survive a crash.
5. **The hit check compares against history in the shooter's clock, not
   B's current view**, with `no-history` defined. The first game's combat
   is scoped to hitscan, static collision and players that don't block
   each other.
6. **NPC features move behind the player-only gate** (S3b → N1, S8 → N2).
   NPC load stays in S0. N2 gets a versioned path/live switching rule,
   because a reliable write followed by a lossy snapshot doesn't arrive in
   a guaranteed order.

**2026-10-12, after S0.** S0's encoding experiment showed snapshots ~7×
smaller with a game-supplied binary codec. New slice **S0c** puts that
codec in the netcode module before S0b. **D4** moved from "8 hosts" to
"1 host for players only, 2–4 with NPCs or at 30 Hz". Whether sharding
is needed at all is re-decided after S0b.
