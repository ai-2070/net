# Netcode — responsive movement (`@net-mesh/browser/netcode`)

Use this for **high-rate, superseded state**: positions, aim, velocities —
anything sent many times a second where only the newest value matters. Keep
durable, validated state (inventory, score, doors, who owns what) in the
store (`store.md`). Most action games use both.

It is netcode "model 2": an authoritative host runs a fixed-rate tick loop;
each player **predicts** its own entity instantly from its inputs and
**reconciles** to the host; everyone else's entities are **interpolated** a
little in the past so loss and jitter do not make them stutter. Every frame
rides a fire-and-forget **lossy** stream (`openStream({ lossy: true })`):
unordered, no retransmits, never delaying anything else.

## Host

```ts
import { hostNetcode } from '@net-mesh/browser/netcode';

const net = hostNetcode<Ship, Move>({
  transport: node,                 // BrowserNode, createLocalMesh().node(), or meshStoreTransport(mesh)
  label: 'my-game.movement',       // players join the same label
  tickRate: 30,
  step: ({ inputs, dtMs }) => {    // inputs: Map<peer, [{ seq, seen, data }]>, each applied once
    for (const [peer, list] of inputs) for (const { data } of list) ships[peer] = move(ships[peer], data);
  },
  snapshot: () => ships,           // entity id → state, what players see
  visible: (peer, id, ship) => !ship.cloaked,             // optional: the PERMISSION
  interest: (id, ship) => cellKey(ship.x, ship.z, 32),    // optional: the key players ask by
  authorize: peer => players.has(peer),                   // optional
});
```

- `step` runs every tick with each player's inputs since the last one,
  oldest first, **exactly once** however many times the lossy carrier
  repeated them. A lost input the redundancy could not repair is skipped,
  never waited on.
- **Lag compensation:** `net.rewind(input.seen)` returns the entities as that
  player saw them when they acted (`seen` is the host time they were
  rendering). It never goes further back than `maxRewindMs` (default 200 ms),
  so faked lag buys nothing; `clamped` says when the cap applied.
- **Host options beyond the example:** `maxFrameBytes` (default 8000, see
  below), `playerTimeoutMs` (default 5000: a player silent that long is
  dropped), `autoTick: false` to drive the loop yourself with `net.tick()`
  (default `true`, at `tickRate`, default 30), and `now` for a test clock.
- **Host handle:** `players()` (16-hex ids now connected), `currentTick`,
  `tick()`, `rewind(seen)`, `dropped` (frames refused, by reason:
  `unauthorized`, `no-authenticated-peer`, `unexpected-frame`,
  `stream-not-open`, `stream-open-failed`, `send-failed`), `lastSendError` (the
  last send/open error text, or `null`), and `close()`.
- **`visible` is the permission, `interest` is only a filter.** `visible(peer,
  id, entity)` decides what a player may see at all. `interest(id, entity)`
  keys each entity (return `null` for "always delivered"), and a player that
  stated interest keys then receives only the visible entities under them. A
  player that stated none receives everything `visible` allows.
- A dedicated host (Node) uses `meshStoreTransport(mesh, { listen: [label] })`
  from `@net-mesh/sdk`. Its store document survives a restart with
  `persistStore(host, { file })` and `restoreStore(file, definition)` (a RedEX
  file opened `persistent: true` with a small `retentionMaxEvents`).

## Player

```ts
import { joinNetcode } from '@net-mesh/browser/netcode';

const net = joinNetcode<Ship, Move>({
  transport: node,
  host: hostNodeId,                // 16 hex
  label: 'my-game.movement',
  local: { id: node.nodeIdHex()!, predict: move },   // the SAME rule the host applies
  interpolationDelayMs: 100,       // ≥ two snapshot intervals
  interest: cellsAround(x, z, { size: 32 }),   // optional; net.setInterest(keys) to move
});
onInput(input => net.input(input));   // your ship moves NOW; the host gets it too
function frame() { draw(net.view()); requestAnimationFrame(frame); }
```

- `view()` = everyone else interpolated at `hostNow − interpolationDelayMs`,
  your entity predicted. `predict` must be the host's own rule, or every
  snapshot corrects you (`stats().corrections` counts it).
- `stats()`: `clock` (`offsetMs`, `rttMs`, `jitterMs`, `samples`, or `null`
  before the first pong), `snapshots`, `lateSnapshots` (reordered),
  `pendingInputs`, `corrections`, `partialSnapshots`.
- `hostNow()`: the host clock as estimated (`null` before the first pong);
  `close()` stops the pings and the stream.
- **Player options beyond the example:** `inputRedundancy` (default 8:
  unacknowledged inputs repeated in each input frame), `pingRate` (clock
  pings per second, default 4), `correctionSmoothingMs`, `extrapolateMs`,
  `interpolate`, `now`.
- **Interest:** `interest` (at join) and `net.setInterest(keys)` (later) name
  the keys to receive. Both throw `RangeError` over 256 keys, or for a key over
  64 characters. The set crosses the lossy carrier as a versioned frame
  repeated until a snapshot acknowledges it, so it survives loss.
- The default `interpolate` lerps numeric fields (nested too) and takes the
  rest from the newer state. With a `codec`, the default is
  `codec.interpolate`, which turns `angle()` fields the short way (in the
  correction blend too); without one, supply your own for angles that wrap
  (`lerpAngle(from, to, alpha)` is exported for it).
- **Acks trim every client's pending inputs**, predicting (`local`) or not,
  so a backlog frame (up to 64 inputs, after a change of owner) goes back to
  `inputRedundancy` once acknowledged. Under `joinShards` only the client
  that owns the player trims the shared sequencer.
- **`interpolationDelayMs` is fixed.** Over bursty links (a relayed pair, a
  busy Wi-Fi) a fixed delay either stutters or adds lag everyone feels. If it
  stutters, raise it; a custom loop can adapt it (see below).

## No authority: a full mesh

`hostNetcode` / `joinNetcode` assume one authoritative host. A game where
**each page simulates what it owns** (its own player, plus shared objects for
whoever leads) can skip them and build on the same parts. Rose & Blade, a three.js melee game
with physics ragdolls and 2–16 players, works this way:

- **Transport:** one lossy `openStream({ reliability: 'fireAndForget', peer,
  label, lossy: true })` to every other page, each reopened on its next send
  after it goes stale (see `session.md` § *Streams*). Direct links per pair,
  with the glare and answering rules in `session.md`.
- **Ownership:** every page broadcasts its own entities. One page **leads**
  (shared AI, props, the match clock); when it goes, the next takes over and
  re-opens the lobby (`store.md` § lobby gotchas).
- **Rate:** 120 broadcasts a second, one per rendered frame at most; 60 above
  8 players, since each page uploads to every other (8v8 at 120 Hz is about
  9 Mbit/s up). Things that change rarely ride a 60 Hz pulse instead, so a
  higher send rate doesn't multiply their cost.
- **Adaptive interpolation delay.** Show each page's frames
  `delay` in the past, per sender: the 95th percentile of how late its frames
  arrived over the last 3 s, plus 1.2 frame gaps, plus a few ms, clamped to
  12–400 ms. Raise it at once when frames come later; ease it back down over
  about a second. A fixed 40 ms stuttered on real links.
- **Events (a hit, a cut) ride several broadcasts:** the 1st, 2nd, 3rd, 5th,
  9th, 17th… after they happen, for a bounded window, de-duplicated by id at
  the receiver. That survives a lost broadcast without every frame carrying
  every event.
- **Packing (still JSON):** fields that change every frame go every time; the
  rest of each entity is a versioned "cold" part sent when it changes (again
  1, 2, 4, 8… broadcasts later), in full twice a second, and to a page that
  just joined. A receiver missing a cold version treats that entity as lost for
  the broadcast, never mixing old and new.
- **Size:** frames above 7000 B are split into parts, below the leaf's
  8104 B event bound (`maxEventBytes`).
- **Liveness:** a page sends a heartbeat a second when backgrounded (it sends
  no frames then); one silent for 4 s is taken for gone, and its entities stay
  drawn 2.5 s longer for whoever takes them over.
- **Measuring links:** a small ping/pong on the same stream feeds a
  `ClockEstimator` per peer (median RTT, jitter); `(await
  node.peerAttempt(peer)).direct` says whether each pair is direct. Show both in a debug overlay.

## Compact snapshots

Give host and players the same entity codec, and snapshots go binary: about
7x smaller than JSON for typical transforms (measured:
`browser-ts/scripts/bench-netcode.mjs --codec compact`).

```ts
import { angle, entityCodec, quantized, uint } from '@net-mesh/browser/netcode';

const shipCodec = entityCodec<Ship>({
  x: quantized(0, 512),   // 16 bits over the range: ~8 mm
  z: quantized(0, 512),
  yaw: angle(),           // 8 bits: ~1.4 degrees
  hp: uint(8),
});
hostNetcode({ /* ... */, codec: shipCodec });
joinNetcode({ /* ... */, codec: shipCodec });
```

- **Fields:** `quantized(min, max, bits = 16)` (clamped to the range),
  `angle(bits = 8)`, `uint(bits)`, `int(bits)`, `float32()`, `bool()`. An
  entity must be a flat object of exactly the schema's fields.
- **JSON is the fallback, never an error.** A player sends its codec's
  fingerprint; the host sends binary only on a match, JSON otherwise. An
  entity the codec refuses (an extra field, a value out of range) sends that
  player JSON for that tick and counts `dropped['codec-write-failed']`.
- **Values arrive quantized.** Reconciliation compares within the codec's
  quanta (`codec.near`), so quantization is not a correction. A NaN or
  infinite `float32()` off the wire refuses the frame.
- **Ids travel as indexes** the host never reuses; a lost name is asked for
  again, and the entity is left out until it arrives
  (`stats().unnamedEntities`). `stats().binaryFrames` counts binary snapshots.
  The host honours a `u` request for an index at most once per 100 ms
  (`dropped['name-request-throttled']`). Ids unseen for `NAME_FORGET_MS`
  (10 s) are forgotten and named afresh on return (the player keeps names
  30 s), so projectiles do not grow the tables all match.
- **Binary snapshots carry the host's instance** (wire version 2, part of
  the fingerprint). A host re-created under the same label resets the
  player's names and snapshots instead of being read through the old
  names; a pre-instance package has another fingerprint, so a mixed pair
  runs on JSON.

## Sharded matches: several hosts, one arena

When one host's upload can't carry the match, split the entities across
hosts. Each player joins **every** host and merges them with `joinShards`.
Each host owns some entities and is authoritative for those only.
Ownership is an explicit record, the `ShardAssignment`:
`{ epoch, hosts: { [shard]: nodeHex }, owner: { [entityId]: shard } }`.

```ts
import { joinShards } from '@net-mesh/browser/netcode';

const match = joinShards<Ship, Move>({
  transport: node,
  label: 'arena.move',
  assignment,                         // or `ownerOf: (id, assignment) => shard`
  local: { id: myId, predict: move }, // predicted on the shard that owns you
  interest: cellsAround(x, z),        // sent to every shard
});
draw(match.view());                   // merged: remote interpolated, yours predicted
match.input(data);                    // to the owning shard only, stamped with the epoch
match.hostNow('s1');                  // that shard's clock (for hit claims)
match.setAssignment(next);            // a higher epoch only; returns false otherwise
match.stats();                        // per shard (silentMs), and what the fencing dropped
```

- **An entity is drawn only from its owner at the installed epoch.**
  Another shard's copy is dropped (`droppedNotOwner`).
- **A shard sending snapshots from another epoch is frozen** at its last
  drawn state, minus what it no longer owns, until it catches up
  (`droppedStaleEpoch`) or you install its newer epoch (`heldNewerEpoch`).
  So a player that installs an assignment before its hosts doesn't see the
  world blink.
- **An owner change lingers** at the old owner's last state for `lingerMs`
  (default 1000), or until the new owner first shows it.
- **The epoch rides the wire:** `hostNetcode({ epoch })` stamps snapshots
  and drops input frames from another epoch (`dropped['stale-epoch']`).
  `joinNetcode({ epoch })` stamps inputs, and `stats().epoch` is the newest
  snapshot's.

**The host side is a recipe, not an API:**
`net/crates/net/browser-ts/demo/sharded-arena/shard.js` (`shardHost(lib,
options)`, typed in `shard.d.ts`). Copy it. It wires:
- `hostNetcode` sending and applying only what the shard owns;
- `authorize` limited to the roster plus the other hosts;
- fog by the player's position on whichever host owns it;
- a spectator `joinNetcode` to every other host;
- `regionHandoffs` over a `handoffLink` whose peers come from the
  assignment.

The page beside it runs 4 hosts × 32 bots on the local mesh.

**Cross-host hits** (`options.combat`, hitscan). A fire input
`{ fire: { target, seenB } }` (with `seenB = match.hostNow(targetShard) -
interpolationDelayMs`) is checked at the shooter's host. A shot at another
shard's target is forwarded as a `hit` action, which the target's host
applies at most once. That host refuses for:
- another epoch (`stale-epoch`);
- a `seenB` older than its rewind cap (`beyond-cap`);
- no history of the shooter at `seenA` (`no-history`);
- a claimed shooter pose that doesn't match that history (`pose-mismatch`).

It checks the pose against **`NetcodeClient.sampleAt(hostTime)`**, its own
past snapshots of the shooter's host, never the current view, so a fast
shooter isn't falsely refused. An unanswered hit is a miss after 500 ms.
Mutual kills across hosts trade.

- **The rewind cap is the latency budget.** A claim reaches the target's
  host aged interpolation delay + player→host + up to a tick + host→host.
  The recipe's default `combat.maxRewindMs` is 500 ms. Measured:
  - 300 ms lost most cross-host hits beyond ~40 ms one-way, with only 11%
    landing at 80–140 ms;
  - 500 ms lands 97% at 80–140 ms.

  A longer rewind lets laggy players hit around corners; size it for your
  players.
- **The assignment lives in the lobby's store** as a `MatchAssignment`
  (`{ epoch, shards: { [name]: { host, squads } }, squads, leases, renewals }`):
  - `assignmentProblems` validates it: each player owned once, and no host
    owning its own squad.
  - `assignAction({ lobby })` is the store action only the lobby node may
    call.
  - `shardAssignment(record)` is what `joinShards` takes.
  - `authorizeTeams` admits `team:<name>` audiences.
- **Leases fence what can't hear the lobby.** The lobby bumps counters with
  `renewLeases` every `leaseMs / 3`. `leaseWatch({ read, leaseMs })` judges
  one on this node's own clock, from when a renewal arrives:
  - a host passes it as `hostNetcode({ live })` and stops simulating and
    sending without it;
  - a player passes one over `renewals` as `joinShards({ live })` and
    freezes (`stats().frozen`: show "reconnecting").
  - The first value it reads is a baseline, not a lease (a replica can
    hold a counter written long ago). It is live once a renewal is seen
    to arrive, about `leaseMs / 3` after it starts. Poll it every tick or
    frame: it notices a move only when asked.

  Reassign a lost host's entities only once `mayReassign(lastRenewedAt,
  now, leaseMs, driftMarginMs, maxDeliveryMs)` says so. `maxDeliveryMs`
  bounds how late a renewal can reach a host: one delivered later keeps
  that host live past the reassignment. Size it from the store's measured
  replication delay; the default 0 assumes instant delivery.
- **Planned moves carry the input boundary.** The recipe's
  `movePlayer(peer, toShard)` freezes the entity together with
  `hostNetcode.releasePlayer(peer)`'s `{ lastApplied }` in one
  synchronous turn. The new owner adopts it (`adoptPlayer`), so an input
  the old host applied is never applied again, even when its
  acknowledgement was lost. The boundary waits for the player however late
  it arrives: a record that times out first, or a peer `authorize` refuses
  for now (`dropped['adopt-unauthorized']`), keeps it for when the player is
  admitted. `joinShards` shares one input sequence across
  shards (`createInputSequencer`). Raise the epoch when the handoff
  reports `moved`.
- **A lost host is a rollback.** After the lease wait, reassign its
  entities. The new owner restores each one from the last snapshot it
  observed (`NetcodeClient.latest()`), and the player resumes from its
  next input (`adoptPlayer(peer, { resume: true })` drops what was
  pending). A hit in flight to the lost host is a miss, and is never
  re-sent.
- **NPCs** belong to a shard per `npcShard(id, shards)`, deterministic on
  every node (`withNpcs` spawns a batch). The owner runs their AI in the
  recipe's `advance(entities, ai)` and fires with `ai.fire(npc, target)`.
- **NPCs on paths:** publish a route once (`NpcPath` in the lobby store,
  written through `pathAction`, owner-only) instead of every tick. Draw
  with `npcModes().resolve(view, records, ownerNow)`, which picks the
  source with the higher mode version `mv`. Never put a fogged NPC on a
  path: the record goes to every reader. Measured with 50 NPCs: 47 KB/s
  live, 1.6 KB/s on paths, per viewer.
  - The saving is for players only. The recipe keeps sending a path NPC
    to the other hosts and into its own rewind history, so it can still
    be hit, shoot, and be restored when its host is lost (it comes back
    live under a new `mv`).
  - A refused path write switches the NPC back to live under a further
    `mv`. A path NPC that dies, or can't be restored, gets its record
    retired (`mode: 'live'`, no points). `npcModes` holds a live NPC no
    snapshot shows for `carryMs`, then stops drawing it.
- **One clock per host.** The recipe's `now` defaults to the netcode's
  (`performance.now()`), the clock `hostNow()` estimates. Hits, NPC shots
  and path points are all on it; `host.now()` reads it for path points.
  Don't pass `Date.now` to one piece and leave the rest on the default:
  every forwarded hit is then refused `beyond-cap`.
- Tests:
  `net/crates/net/browser-ts/test/netcode/shards.test.ts`,
  `net/crates/net/browser-ts/test/netcode/sharded-arena.test.ts`,
  `net/crates/net/browser-ts/test/netcode/cross-host-hits.test.ts`,
  `net/crates/net/browser-ts/test/netcode/assignment.test.ts`,
  `net/crates/net/browser-ts/test/netcode/planned-move.test.ts`,
  `net/crates/net/browser-ts/test/netcode/host-crash.test.ts`,
  `net/crates/net/browser-ts/test/netcode/npc.test.ts`,
  `net/crates/net/browser-ts/test/netcode/npc-paths.test.ts`.

## Rules that bite

- **A join that never connects says why.** `await net.ready()` rejects
  with a `NetcodeError` after `joinTimeoutMs` (default 15 s):
  `no-announcement` means this node never heard the host announce, so it
  cannot reach it. A lobby announces for you; a page that joins a host
  directly must make sure the host announces and query its tag first.
  `stats().connectOutcome` shows the last `connectPeer` outcome. After a
  direct attempt ends relayed, or `connectPeer` rejects, the next offer
  waits `directRetryMs` (default 30 s); reopened streams ride the session
  there is meanwhile.

- **Lossy streams are `connect()`-only.** `openSession()` refuses
  `lossy: true`; games run on `connect()` anyway (stores need it too).
- **Keep snapshots small anyway.** A snapshot over `maxFrameBytes` (default
  8000, under one event) is sent as independent chunks, each entity always in
  the same chunk. A lost chunk's entities are carried over from the previous
  snapshot for that tick (`stats().partialSnapshots` counts it), never
  dropped from view. Every chunk still costs bandwidth each tick, so narrow
  what each player gets (`interest` keys, or `visible` where it is a real
  permission) rather than sending the world.
- **Corrections blend in** over `correctionSmoothingMs` (default 100, `0`
  snaps); inputs keep moving the drawn entity during the blend.
- **Past the newest snapshot remote entities hold**, unless you set
  `extrapolateMs` (then they carry on along their last motion for at most
  that long; a custom `interpolate` then sees `alpha > 1`).
- **Snapshots are JSON unless both ends have a codec** (Compact snapshots, above).
- **Building blocks are exported too**, for a custom loop: `ClockEstimator`
  (NTP-style offset from the lowest-RTT samples), `SnapshotBuffer`
  (time-ordered snapshots with interpolated reads; `sample` at a snapshot's
  exact time returns that snapshot whole), `lerpNumbers` (the default
  interpolator) and `lerpAngle`.
- The **anchor must be the same release** as the package: an older anchor
  treats the lossy channel as its only one.
- Source: `net/crates/net/browser-ts/src/netcode/`, tests
  `net/crates/net/browser-ts/test/netcode/netcode.test.ts` (a simulated
  lossy, laggy network), `net/crates/net/browser-ts/test/netcode/codec.test.ts` (compact
  snapshots) and `net/crates/net/sdk-ts/test/store_transport.test.ts`
  (a native dedicated host).
