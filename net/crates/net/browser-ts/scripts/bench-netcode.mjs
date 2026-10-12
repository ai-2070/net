// A sharded-match measurement harness for netcode (plan
// BROWSER_SHARDED_MATCH_PLAN.md, slice S0): host CPU per tick and upload per
// host, for one match split by OWNERSHIP across H hosts.
//
//   npm run build && npm run bench:netcode                     # the default sweep
//   node scripts/bench-netcode.mjs --hosts 8 --players 128 --npcs 100 --mode interest
//   node scripts/bench-netcode.mjs --json > result.json
//
// Topology A of the plan: every player joins every host, predicts on the
// host that owns it and sends inputs only there; hosts spectate each other
// (a `joinNetcode` with no local entity) and receive everything. Player i
// belongs to squad floor(i / 8), owned by host squad mod H; NPC k by a hash
// of its id mod H.
//
// It runs the BUILT package (`dist/`) over `@net-mesh/browser/local`, so
// every frame is really built, encoded, sent, parsed and applied. What it
// leaves out is the network: delivery is an in-process call, so it says
// nothing about WebRTC connection cost, real upload, NAT, or hosting while
// rendering — that is slice S0b, in real browsers.
//
// What is counted:
//   - host ms/tick: one `tick()` (step + snapshot + per-player filtering +
//     encoding + handing frames to the transport). Not the work of
//     receiving inputs, which happens between ticks.
//   - snapshot bytes: netcode frame payloads (the JSON frame, its envelope
//     included), split into player-facing and host-facing. Not the
//     transport's own overhead (Noise, SCTP/DTLS, IP/UDP).
//   - `--codec compact`: hosts and players use the netcode entity codec
//     (`entityCodec`, binary snapshots) instead of JSON frames.
//   - control: clock pongs are timer-driven in real time (4/s per client),
//     and this harness ticks faster than real time, so their rate is
//     computed — measured pong size × 4/s × receivers — not counted.
// Bandwidth is bytes/tick × tick rate: one run gives every rate.
//
// Modes:
//   full      — no interest: every player receives every entity.
//   interest  — each player asks for the 3×3 cells around it; the arena is
//               sized so that a 3×3 block holds ~`--visible` players.

import { performance } from 'node:perf_hooks';

import * as pkg from '../dist/index.js';
import {
  angle,
  decodeBinarySnapshot,
  entityCodec,
  hostNetcode,
  isBinaryFrame,
  joinNetcode,
  quantized,
  uint,
} from '../dist/netcode/index.js';
import { createLocalMesh } from '../dist/local.js';

const args = process.argv.slice(2);
const has = name => args.includes(`--${name}`);
const flag = (name, fallback) => {
  const at = args.indexOf(`--${name}`);
  return at === -1 ? fallback : Number(args[at + 1]);
};
const text = (name, fallback) => {
  const at = args.indexOf(`--${name}`);
  return at === -1 ? fallback : args[at + 1];
};
const json = has('json');

const LABEL = 'arena.move';
const CELL = 32;
const SQUAD = 8;
const PING_HZ = 4;
const RATES = [20, 30];
const decoder = new TextDecoder();

/** A deterministic PRNG, so two runs measure the same match. */
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

/** FNV-1a: the NPC → host hash (plan D7). */
function hash(text) {
  let h = 0x811c9dc5;
  for (let i = 0; i < text.length; i += 1) h = Math.imul(h ^ text.charCodeAt(i), 0x01000193) >>> 0;
  return h;
}

const round = n => Math.round(n * 100) / 100;
const clamp = (n, side) => Math.min(side, Math.max(0, n));

/** One movement input applied to a player — the host's rule and the prediction. */
function move(state, input) {
  return {
    ...state,
    x: round(clamp(state.x + input.dx, input.side)),
    z: round(clamp(state.z + input.dz, input.side)),
    yaw: round(input.yaw),
  };
}

function median(values) {
  const sorted = [...values].sort((a, b) => a - b);
  return sorted[Math.floor(sorted.length / 2)] ?? 0;
}
function percentile(values, p) {
  const sorted = [...values].sort((a, b) => a - b);
  return sorted[Math.min(sorted.length - 1, Math.floor(sorted.length * p))] ?? 0;
}
const mean = values => (values.length === 0 ? 0 : values.reduce((a, b) => a + b, 0) / values.length);

const turn = () => new Promise(resolve => setImmediate(resolve));
const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));

/**
 * The entity schema for `--codec compact`. Players and NPCs share one
 * shape (an NPC has no ammo or team, a player no kind), since one codec
 * serves one netcode instance.
 */
function codecFor(side) {
  return entityCodec({
    x: quantized(0, side),
    z: quantized(0, side),
    yaw: angle(),
    hp: uint(8),
    ammo: uint(8),
    team: uint(8),
    kind: uint(8),
  });
}

/** Wrap a node so every frame it sends is recorded, to be tallied OUTSIDE the timed tick. */
function recording(node, hostIds, outbox) {
  return {
    nodeIdHex: () => node.nodeIdHex(),
    onEvent: handler => node.onEvent(handler),
    openStream: options => {
      const stream = node.openStream(options);
      const toHost = options.peer !== undefined && hostIds.has(options.peer);
      return {
        send: payload => {
          outbox.push({ toHost, payload });
          return stream.send(payload);
        },
        close: () => stream.close(),
      };
    },
  };
}

async function run({ hosts: H, players: N, npcs: M, mode, codec: codecName, ticks, visible, warmupMs }) {
  const random = rng(H * 1_000_003 + N * 1009 + M * 7 + (mode === 'full' ? 1 : 2));
  // The arena: a 3×3 block of cells holds ~`visible` of the N players.
  const side = Math.sqrt(N / Math.min(visible, N)) * 3 * CELL;
  const codec = codecName === 'compact' ? codecFor(side) : undefined;
  const withCodec = codec === undefined ? {} : { codec };
  const mesh = createLocalMesh();
  const hostNodes = Array.from({ length: H }, () => mesh.node());
  const hostIds = new Set(hostNodes.map(node => node.nodeIdHex()));
  const playerNodes = Array.from({ length: N }, () => mesh.node());

  // Ownership.
  const ownerOfPlayer = i => Math.floor(i / SQUAD) % H;
  const ownerOfNpc = id => hash(id) % H;

  // Authoritative state, per host: the entities it owns.
  const worlds = hostNodes.map(() => new Map());
  const positions = new Map(); // player id → bench-side position, for interest
  playerNodes.forEach((node, i) => {
    const id = node.nodeIdHex();
    const state = { x: round(random() * side), z: round(random() * side), yaw: 0, hp: 100, ammo: 30, team: i % 2, kind: 0 };
    worlds[ownerOfPlayer(i)].set(id, state);
    positions.set(id, state);
  });
  for (let k = 0; k < M; k += 1) {
    const id = `npc${k}`;
    worlds[ownerOfNpc(id)].set(id, { x: round(random() * side), z: round(random() * side), yaw: 0, hp: 200, ammo: 0, team: 0, kind: 1 + (k % 4) });
  }

  // Hosts.
  const outboxes = hostNodes.map(() => []);
  const hostRandom = hostNodes.map((_, h) => rng(h + 17));
  const hosts = hostNodes.map((node, h) =>
    hostNetcode({
      transport: recording(node, hostIds, outboxes[h]),
      label: LABEL,
      tickRate: 30,
      autoTick: false,
      step: ({ inputs }) => {
        const world = worlds[h];
        for (const [peer, list] of inputs) {
          const state = world.get(peer);
          if (state === undefined) continue; // not ours: inputs go to the owner only
          let next = state;
          for (const { data } of list) next = move(next, data);
          world.set(peer, next);
        }
        // NPCs: a random walk, every tick.
        const r = hostRandom[h];
        for (const [id, state] of world) {
          if (!id.startsWith('npc')) continue;
          world.set(id, {
            ...state,
            x: round(clamp(state.x + (r() - 0.5) * 2, side)),
            z: round(clamp(state.z + (r() - 0.5) * 2, side)),
            yaw: round(r() * 6.28),
          });
        }
      },
      snapshot: () => Object.fromEntries(worlds[h]),
      ...withCodec,
      ...(mode === 'interest' ? { interest: (_id, e) => pkg.cellKey(e.x, e.z, CELL) } : {}),
    }),
  );

  // Hosts spectate each other: everything, no local entity.
  const spectators = [];
  for (let a = 0; a < H; a += 1) {
    for (let b = 0; b < H; b += 1) {
      if (a === b) continue;
      spectators.push(joinNetcode({ transport: hostNodes[a], host: hostNodes[b].nodeIdHex(), label: LABEL, ...withCodec }));
    }
  }

  // Players: one client per host; predicted and fed inputs on the owner only.
  const received = new Map(); // player node → { bytes, entities }
  const clients = playerNodes.map((node, i) => {
    const id = node.nodeIdHex();
    const tally = { bytes: 0, entities: 0 };
    received.set(id, tally);
    node.onEvent(event => {
      if (event.payload === undefined) return;
      if (isBinaryFrame(event.payload)) {
        const decoded = codec === undefined ? null : decodeBinarySnapshot(event.payload, LABEL, codec);
        if (decoded === null) return;
        tally.bytes += event.payload.length;
        tally.entities += decoded.entities.size;
        return;
      }
      let frame;
      try {
        frame = JSON.parse(decoder.decode(event.payload));
      } catch {
        return;
      }
      if (frame.k !== 's') return;
      tally.bytes += event.payload.length;
      tally.entities += Object.keys(frame.e).length;
    });
    const at = positions.get(id);
    return hostNodes.map((hostNode, h) =>
      joinNetcode({
        transport: node,
        host: hostNode.nodeIdHex(),
        label: LABEL,
        ...(h === ownerOfPlayer(i) ? { local: { id, predict: move } } : {}),
        ...withCodec,
        ...(mode === 'interest' ? { interest: pkg.cellsAround(at.x, at.z, { size: CELL }) } : {}),
      }),
    );
  });
  const interestOf = new Map(playerNodes.map(node => [node.nodeIdHex(), '']));

  const inputFor = id => {
    const at = positions.get(id);
    const input = { dx: (random() - 0.5) * 3, dz: (random() - 0.5) * 3, yaw: random() * 6.28, side };
    positions.set(id, move(at, input));
    return input;
  };
  const sendInputs = () => {
    playerNodes.forEach((node, i) => {
      const id = node.nodeIdHex();
      clients[i][ownerOfPlayer(i)].input(inputFor(id));
      if (mode === 'interest') {
        const at = positions.get(id);
        const cells = pkg.cellsAround(at.x, at.z, { size: CELL });
        const key = cells.join('|');
        if (key !== interestOf.get(id)) {
          interestOf.set(id, key);
          for (const client of clients[i]) client.setInterest(cells);
        }
      }
    });
  };
  const tickAll = timed => {
    for (let h = 0; h < H; h += 1) {
      const started = performance.now();
      hosts[h].tick();
      const ms = performance.now() - started;
      if (timed) timed[h].push(ms);
    }
  };
  const settle = async () => {
    for (let k = 0; k < 4; k += 1) await turn();
  };

  // Warm up in real time: joins are repeated on a timer until the first
  // snapshot, and interest must be acknowledged before counting.
  const warmStarted = performance.now();
  const allJoined = () =>
    clients.every(row => row.every(client => client.stats().snapshots > 0)) &&
    spectators.every(client => client.stats().snapshots > 0);
  // Pongs are sent on the clients' real-time ping timer, so their size is
  // measured here, during the real-time warm-up.
  const pong = { bytes: 0, count: 0 };
  const collectPongs = () => {
    for (const outbox of outboxes) {
      for (const { payload } of outbox) {
        if (isBinaryFrame(payload) || JSON.parse(decoder.decode(payload)).k !== 'q') continue;
        pong.bytes += payload.length;
        pong.count += 1;
      }
      outbox.length = 0;
    }
  };
  while (performance.now() - warmStarted < warmupMs) {
    sendInputs();
    tickAll(null);
    await settle();
    await sleep(5);
    collectPongs();
    if (allJoined() && pong.count > 0 && performance.now() - warmStarted > 600) break;
  }
  const joinedAll = allJoined();

  // Measure.
  for (const outbox of outboxes) outbox.length = 0;
  for (const tally of received.values()) {
    tally.bytes = 0;
    tally.entities = 0;
  }
  const hostMs = hostNodes.map(() => []);
  const sent = hostNodes.map(() => ({ toPlayers: 0, toHosts: 0, frames: 0, binaryFrames: 0 }));
  const tally = () => {
    outboxes.forEach((outbox, h) => {
      for (const { toHost, payload } of outbox) {
        const binary = isBinaryFrame(payload);
        if (!binary && JSON.parse(decoder.decode(payload)).k !== 's') continue;
        sent[h].frames += 1;
        if (binary) sent[h].binaryFrames += 1;
        if (toHost) sent[h].toHosts += payload.length;
        else sent[h].toPlayers += payload.length;
      }
      outbox.length = 0;
    });
  };
  const wallStarted = performance.now();
  for (let tick = 0; tick < ticks; tick += 1) {
    sendInputs();
    await settle();
    tickAll(hostMs);
    tally();
    await settle();
  }
  const wallMs = performance.now() - wallStarted;

  const receivers = hosts.map(host => host.players().length);
  const perHost = sent.map(s => ({
    toPlayers: s.toPlayers / ticks,
    toHosts: s.toHosts / ticks,
    frames: s.frames / ticks,
    binaryShare: s.frames === 0 ? 0 : s.binaryFrames / s.frames,
  }));
  const pongSize = pong.count === 0 ? 0 : pong.bytes / pong.count;
  const controlMbit = pongSize * PING_HZ * mean(receivers) * 8 / 1e6;
  const totals = perHost.map(p => p.toPlayers + p.toHosts);
  const mbit = rate => (bytesPerTick => (bytesPerTick * rate * 8) / 1e6);
  const recv = [...received.values()];
  const dropped = {};
  for (const host of hosts) for (const [why, n] of Object.entries(host.dropped)) dropped[why] = (dropped[why] ?? 0) + n;
  const notReady = clients.flat().filter(client => client.stats().snapshots === 0).length;

  for (const row of clients) for (const client of row) client.close();
  for (const client of spectators) client.close();
  for (const host of hosts) host.close();

  const allMs = hostMs.flat();
  const result = {
    hosts: H,
    players: N,
    npcs: M,
    mode,
    codec: codecName,
    ticks,
    arenaSide: Math.round(side),
    joinedAll,
    hostMsMedian: median(allMs),
    hostMsP95: percentile(allMs, 0.95),
    hostMsWorstMedian: Math.max(...hostMs.map(median)),
    receiversPerHost: mean(receivers),
    snapshotBytesPerHostTick: { toPlayers: mean(perHost.map(p => p.toPlayers)), toHosts: mean(perHost.map(p => p.toHosts)), max: Math.max(...totals) },
    framesPerHostTick: mean(perHost.map(p => p.frames)),
    entitiesPerPlayerTick: mean(recv.map(r => r.entities / ticks)),
    bytesPerPlayerTick: mean(recv.map(r => r.bytes / ticks)),
    bytesPerEntity: (() => {
      const e = recv.reduce((a, r) => a + r.entities, 0);
      return e === 0 ? 0 : recv.reduce((a, r) => a + r.bytes, 0) / e;
    })(),
    pongBytes: pongSize,
    controlMbitPerHost: controlMbit,
    // The busiest host's upload: snapshots at each rate, plus control.
    mbitPerHostMax: Object.fromEntries(RATES.map(rate => [`${rate}Hz`, mbit(rate)(Math.max(...totals)) + controlMbit])),
    mbitPerHostMean: Object.fromEntries(RATES.map(rate => [`${rate}Hz`, mbit(rate)(mean(totals)) + controlMbit])),
    // With --codec compact: the share of snapshot frames that went binary
    // (1.0 once every client negotiated; JSON before that, and on fallback).
    binaryShare: mean(perHost.map(p => p.binaryShare)),
    wallMsPerTick: wallMs / ticks,
    notReady,
    dropped,
  };
  return result;
}

// The sweep.
const pick = (name, fallback) => (has(name) ? [flag(name, 0)] : fallback);
const hostsList = pick('hosts', [1, 4, 8]);
const playersList = pick('players', [16, 64, 128]);
const npcsList = pick('npcs', [0, 100, 300]);
const modes = has('mode') ? [text('mode', 'interest')] : ['full', 'interest'];
const codecs = has('codec') ? [text('codec', 'json')] : ['json', 'compact'];
for (const codec of codecs) if (codec !== 'json' && codec !== 'compact') throw new Error(`--codec is 'json' or 'compact', got '${codec}'`);
for (const mode of modes) if (mode !== 'full' && mode !== 'interest') throw new Error(`--mode is 'full' or 'interest', got '${mode}'`);
const base = { ticks: flag('ticks', 40), visible: flag('visible', 30), warmupMs: flag('warmup-ms', 10_000) };

const fmt = (n, digits = 1) => (n >= 100 ? Math.round(n).toString() : n.toFixed(digits));
if (!json) {
  console.log(
    `ticks=${base.ticks} visible≈${base.visible} cell=${CELL} squad=${SQUAD} (local mesh, dist build; payload only, no transport overhead)\n`,
  );
  console.log(
    'codec   mode      H    N  npc | host ms/tick med  p95 worst | recv/host | B/host/tick →players →hosts | Mbit/s/host max @20  @30 | binary | ctl Mbit | ent/player/tick B/ent | notReady',
  );
}

// A point that never finishes is a finding, not a hang: bounded and named.
const LIMIT_MS = flag('limit-ms', 300_000);
const results = [];
for (const codec of codecs) for (const mode of modes) {
  for (const hosts of hostsList) {
    for (const players of playersList) {
      for (const npcs of npcsList) {
        const label = `${codec} ${mode} H=${hosts} N=${players} npc=${npcs}`;
        let timer;
        const r = await Promise.race([
          run({ ...base, hosts, players, npcs, mode, codec }),
          new Promise((_, reject) => {
            timer = setTimeout(() => reject(new Error(`${label} did not finish within ${String(LIMIT_MS)} ms`)), LIMIT_MS);
          }),
        ]).finally(() => clearTimeout(timer));
        results.push(r);
        if (json) continue;
        console.log(
          [
            r.codec.padEnd(7),
            r.mode.padEnd(9),
            String(r.hosts).padStart(1),
            String(r.players).padStart(4),
            String(r.npcs).padStart(4),
            '|',
            fmt(r.hostMsMedian, 2).padStart(16),
            fmt(r.hostMsP95, 2).padStart(5),
            fmt(r.hostMsWorstMedian, 2).padStart(5),
            '|',
            fmt(r.receiversPerHost).padStart(9),
            '|',
            fmt(r.snapshotBytesPerHostTick.toPlayers).padStart(20),
            fmt(r.snapshotBytesPerHostTick.toHosts).padStart(7),
            '|',
            fmt(r.mbitPerHostMax['20Hz'], 2).padStart(21),
            fmt(r.mbitPerHostMax['30Hz'], 2).padStart(5),
            '|',
            fmt(r.binaryShare, 2).padStart(6),
            '|',
            fmt(r.controlMbitPerHost, 2).padStart(8),
            '|',
            fmt(r.entitiesPerPlayerTick).padStart(15),
            fmt(r.bytesPerEntity).padStart(5),
            '|',
            `${String(r.notReady).padStart(8)}${r.joinedAll ? '' : ' (warm-up incomplete)'}`,
          ].join(' '),
        );
      }
    }
  }
}

if (json) console.log(JSON.stringify({ base, results }, null, 2));
process.exit(0);
