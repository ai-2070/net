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
  visible: (peer, id, ship) => near(ships[peer], ship),   // optional per-player filter
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
- A dedicated host (Node) uses `meshStoreTransport(mesh, { listen: [label] })`
  from `@net-mesh/sdk`.

## Player

```ts
import { joinNetcode } from '@net-mesh/browser/netcode';

const net = joinNetcode<Ship, Move>({
  transport: node,
  host: hostNodeId,                // 16 hex
  label: 'my-game.movement',
  local: { id: node.nodeIdHex()!, predict: move },   // the SAME rule the host applies
  interpolationDelayMs: 100,       // ≥ two snapshot intervals
});
onInput(input => net.input(input));   // your ship moves NOW; the host gets it too
function frame() { draw(net.view()); requestAnimationFrame(frame); }
```

- `view()` = everyone else interpolated at `hostNow − interpolationDelayMs`,
  your entity predicted. `predict` must be the host's own rule, or every
  snapshot corrects you (`stats().corrections` counts it).
- `stats()`: `clock` (`offsetMs`, `rttMs`, `jitterMs`), snapshots, late
  (reordered) snapshots, pending inputs, corrections.
- The default `interpolate` lerps numeric fields (nested too) and takes the
  rest from the newer state; supply your own for angles that wrap.

## Rules that bite

- **Lossy streams are `connect()`-only.** `openSession()` refuses
  `lossy: true`; games run on `connect()` anyway (stores need it too).
- **Keep snapshots small.** A snapshot over one event (~8 KiB) is sent as
  fragments, and on the lossy carrier one lost fragment loses the whole
  snapshot. Filter with `visible` (interest) rather than sending the world.
- **Not built yet:** smoothing of corrections (they snap), extrapolation
  past the newest snapshot (it holds), binary encoding (frames are JSON).
- The **anchor must be the same release** as the package: an older anchor
  treats the lossy channel as its only one.
- Source: `net/crates/net/browser-ts/src/netcode/`, tests
  `net/crates/net/browser-ts/test/netcode/netcode.test.ts` (a simulated
  lossy, laggy network) and `net/crates/net/sdk-ts/test/store_transport.test.ts`
  (a native dedicated host).
