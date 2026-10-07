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
  rest from the newer state; supply your own for angles that wrap.
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
  `ClockEstimator` per peer (median RTT, jitter); `peerAttempt(peer).direct`
  says whether each pair is direct. Show both in a debug overlay.

## Rules that bite

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
- **Not built yet:** binary encoding (frames are JSON).
- **Building blocks are exported too**, for a custom loop: `ClockEstimator`
  (NTP-style offset from the lowest-RTT samples), `SnapshotBuffer`
  (time-ordered snapshots with interpolated reads) and `lerpNumbers` (the
  default interpolator).
- The **anchor must be the same release** as the package: an older anchor
  treats the lossy channel as its only one.
- Source: `net/crates/net/browser-ts/src/netcode/`, tests
  `net/crates/net/browser-ts/test/netcode/netcode.test.ts` (a simulated
  lossy, laggy network) and `net/crates/net/sdk-ts/test/store_transport.test.ts`
  (a native dedicated host).
