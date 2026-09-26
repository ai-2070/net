// A large-world measurement harness for the store: host cost per tick and
// bytes per player, across world sizes, player counts and projection modes.
//
//   npm run build && npm run bench:world                 # the default sweep
//   node scripts/bench-world.mjs --entities 2000 --players 16 --ticks 60
//   node scripts/bench-world.mjs --json > result.json
//   node scripts/bench-world.mjs --players 0           # the commit alone, no fan-out
//
// It runs the BUILT package (`dist/`) over `@net-mesh/browser/local`, so
// every frame is really encoded, chunked, sent, parsed and applied; what
// it leaves out is the network. The numbers are about the store's own
// work and its bytes on the wire, not about latency.
//
// Modes:
//   full      — no interest: every player receives every entity.
//   interest  — each player asks for the cells around it (3×3).
//   owner     — interest, plus a per-player secret (`ships.*.cargo` owner),
//               which makes every projection per player.
//
// Each tick moves a fraction of the entities (default 5%) by a small step
// and commits once, as a game loop's authoritative update would.

import { performance } from 'node:perf_hooks';

import * as pkg from '../dist/index.js';
import { createLocalMesh } from '../dist/local.js';

const args = process.argv.slice(2);
const flag = (name, fallback) => {
  const at = args.indexOf(`--${name}`);
  return at === -1 ? fallback : Number(args[at + 1]);
};
const json = args.includes('--json');
const only = (() => {
  const at = args.indexOf('--mode');
  return at === -1 ? null : args[at + 1];
})();

const CELL = 32;
const MAX_EVENT_BYTES = 8104;

/** A deterministic PRNG, so two runs measure the same world. */
function rng(seed) {
  let state = seed >>> 0;
  return () => {
    state = (state + 0x6d2b79f5) >>> 0;
    let t = state;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

function definitionFor(mode) {
  const ship = raw => {
    const out = { x: Number(raw.x), z: Number(raw.z), hull: Number(raw.hull), owner: String(raw.owner) };
    if (raw.cargo !== undefined) out.cargo = pkg.hiddenOr(v => Number(v))(raw.cargo);
    return out;
  };
  return pkg.defineStore({
    id: `bench.world.${mode}`,
    version: 1,
    state: value => {
      const ships = {};
      for (const [id, entity] of Object.entries(value.ships)) ships[id] = ship(entity);
      return { ships, tick: Number(value.tick) };
    },
    empty: () => ({ ships: {}, tick: 0 }),
    visibility: mode === 'owner' ? { 'ships.*.cargo': 'owner' } : 'open',
    ...(mode === 'full' ? {} : { interest: { ships: s => pkg.cellKey(s.x, s.z, CELL) } }),
    actions: {},
    inputs: {},
  });
}

function median(values) {
  const sorted = [...values].sort((a, b) => a - b);
  return sorted[Math.floor(sorted.length / 2)] ?? 0;
}
function percentile(values, p) {
  const sorted = [...values].sort((a, b) => a - b);
  return sorted[Math.min(sorted.length - 1, Math.floor(sorted.length * p))] ?? 0;
}

const turn = () => new Promise(resolve => setImmediate(resolve));

async function run({ mode, entities, players, ticks, moving, density }) {
  const random = rng(entities * 31 + players);
  // Side of the square world, so the mean is `density` entities per cell.
  const side = Math.sqrt(entities / density) * CELL;
  const mesh = createLocalMesh();
  const hostNode = mesh.node();
  const playerIds = Array.from({ length: players }, () => mesh.node());
  const ships = {};
  for (let i = 0; i < entities; i += 1) {
    const owner = i < players ? playerIds[i].nodeIdHex() : hostNode.nodeIdHex();
    ships[`e${i}`] = { x: random() * side, z: random() * side, hull: 100, owner, ...(mode === 'owner' ? { cargo: i } : {}) };
  }
  const definition = definitionFor(mode);
  const host = pkg.hostStore({
    definition,
    transport: hostNode,
    initialState: { ships, tick: 0 },
    maxEventBytes: MAX_EVENT_BYTES,
    schedule: () => () => {},
    authorize: () => true,
    actions: {},
    inputs: {},
  });

  const bytes = new Map();
  const frames = new Map();
  const replicas = [];
  for (const node of playerIds) {
    bytes.set(node, 0);
    frames.set(node, 0);
    node.onEvent(event => {
      if (event.payload === undefined) return;
      bytes.set(node, bytes.get(node) + event.payload.length);
      frames.set(node, frames.get(node) + 1);
    });
    const at = ships[`e${playerIds.indexOf(node)}`];
    replicas.push(
      pkg.joinStore({
        definition,
        transport: node,
        host: hostNode.nodeIdHex(),
        audience: [],
        key: 'player',
        maxEventBytes: MAX_EVENT_BYTES,
        schedule: () => () => {},
        ...(mode === 'full' ? {} : { interest: pkg.cellsAround(at.x, at.z, { size: CELL }) }),
      }),
    );
  }
  const joinStarted = performance.now();
  await Promise.all(replicas.map(replica => replica.ready()));
  const joinMs = performance.now() - joinStarted;
  const initialBytes = [...bytes.values()];
  for (const node of playerIds) {
    bytes.set(node, 0);
    frames.set(node, 0);
  }

  const hostMs = [];
  const ids = Object.keys(ships);
  const perTick = Math.max(1, Math.round(entities * moving));
  const settleStarted = performance.now();
  for (let tick = 1; tick <= ticks; tick += 1) {
    const state = host.getState();
    const next = { ...state.ships };
    for (let k = 0; k < perTick; k += 1) {
      const id = ids[Math.floor(random() * ids.length)];
      const s = next[id];
      next[id] = {
        ...s,
        x: Math.min(side, Math.max(0, s.x + (random() - 0.5) * 4)),
        z: Math.min(side, Math.max(0, s.z + (random() - 0.5) * 4)),
      };
    }
    const started = performance.now();
    host.setState({ ships: next, tick });
    hostMs.push(performance.now() - started);
    await turn();
  }
  for (let i = 0; i < 20; i += 1) await turn();
  const totalMs = performance.now() - settleStarted;

  const counters = host.counters();
  const resyncs = counters['resync'] ?? 0;
  const perPlayerBytes = [...bytes.values()].map(value => value / ticks);
  const perPlayerFrames = [...frames.values()].map(value => value / ticks);
  const stale = replicas.filter(replica => replica.getStatus().phase !== 'ready').length;
  await Promise.all(replicas.map(replica => replica.close()));
  await host.close();
  return {
    mode,
    entities,
    players,
    ticks,
    movedPerTick: perTick,
    hostMsMedian: median(hostMs),
    hostMsP95: percentile(hostMs, 0.95),
    // `--players 0` measures the commit alone: no fan-out, nothing to average.
    bytesPerPlayerTickMean: players === 0 ? 0 : perPlayerBytes.reduce((a, b) => a + b, 0) / players,
    bytesPerPlayerTickMax: players === 0 ? 0 : Math.max(...perPlayerBytes),
    framesPerPlayerTick: players === 0 ? 0 : perPlayerFrames.reduce((a, b) => a + b, 0) / players,
    initialBytesMean: players === 0 ? 0 : initialBytes.reduce((a, b) => a + b, 0) / players,
    joinMs,
    wallMsPerTick: totalMs / ticks,
    resyncs,
    notReady: stale,
  };
}

const sweep = [];
if (args.includes('--entities') || args.includes('--players')) {
  sweep.push({ entities: flag('entities', 2000), players: flag('players', 8) });
} else {
  for (const entities of [500, 2000, 8000]) for (const players of [4, 16]) sweep.push({ entities, players });
}
const modes = only === null ? ['full', 'interest', 'owner'] : [only];
const base = { ticks: flag('ticks', 40), moving: flag('moving', 0.05), density: flag('density', 2) };

const fmt = (n, digits = 1) => (n >= 100 ? Math.round(n).toString() : n.toFixed(digits));
if (!json) {
  console.log(
    `ticks=${base.ticks} moving=${base.moving * 100}%/tick density=${base.density}/cell cell=${CELL} (local mesh, dist build)\n`,
  );
  console.log('mode      entities players | host ms/tick med  p95 | B/player/tick mean   max | frames/tick | initial B/player | resyncs notReady');
}

// A point that never finishes is a finding, not a hang: bounded, and
// named, so the sweep says which world stuck.
const LIMIT_MS = flag('limit-ms', 120_000);
const results = [];
for (const point of sweep) {
  for (const mode of modes) {
    const label = `${mode} ${String(point.entities)}×${String(point.players)}`;
    let timer;
    const r = await Promise.race([
      run({ ...base, ...point, mode }),
      new Promise((_, reject) => {
        timer = setTimeout(() => reject(new Error(`${label} did not finish within ${String(LIMIT_MS)} ms`)), LIMIT_MS);
      }),
    ]).finally(() => clearTimeout(timer));
    results.push(r);
    if (json) continue;
    // One row as each point finishes, so a slow or stuck point shows where.
    console.log(
      [
        r.mode.padEnd(9),
        String(r.entities).padStart(8),
        String(r.players).padStart(7),
        '|',
        fmt(r.hostMsMedian, 2).padStart(16),
        fmt(r.hostMsP95, 2).padStart(5),
        '|',
        fmt(r.bytesPerPlayerTickMean).padStart(18),
        fmt(r.bytesPerPlayerTickMax).padStart(5),
        '|',
        fmt(r.framesPerPlayerTick, 2).padStart(11),
        '|',
        fmt(r.initialBytesMean).padStart(16),
        '|',
        String(r.resyncs).padStart(7),
        String(r.notReady).padStart(8),
      ].join(' '),
    );
  }
}

if (json) console.log(JSON.stringify({ base, results }, null, 2));
process.exit(0);
