// One browser's side of the S0b topology run (sharded-match plan): a shard
// HOST or a PLAYER, against a real `net-mesh anchor serve --game`, with the
// published package's public API only.
//
// Topology A: every player joins every host's netcode, predicts on the host
// that owns it (player index i → host floor(i / 8) mod H) and sends inputs
// only there. Hosts spectate each other. A host also renders the merged
// world every frame, so its numbers are "hosting while rendering".
//
// The page reports to the coordinator (`/api/report`, same origin) once a
// second: wire bytes from Chrome's own RTCPeerConnection stats (DTLS, SCTP
// and Noise included), the payload netcode handed the transport, peer
// connection outcomes, frame times, tick times and heap. run.mjs adds the
// main thread's busy time from CDP (`globalThis.__cdp`).

// Wrap RTCPeerConnection BEFORE the package loads: every connection the leaf
// opens is recorded, so its bytes can be read with getStats(). The first
// one is the anchor's (connect() dials the anchor first); the rest are
// direct peer pairs.
const connections = [];
{
  const Native = globalThis.RTCPeerConnection;
  globalThis.RTCPeerConnection = function (...args) {
    const pc = new Native(...args);
    connections.push({ pc, createdAt: performance.now() });
    return pc;
  };
  globalThis.RTCPeerConnection.prototype = Native.prototype;
  Object.setPrototypeOf(globalThis.RTCPeerConnection, Native);
}

const pkg = await import('/pkg/index.bundle.js');
const netcode = await import('/pkg/netcode/index.js');
const { cellKey, cellsAround, connect, rememberedIdentity, requestCredential } = pkg;
const { angle, entityCodec, hostNetcode, joinNetcode, quantized, uint } = netcode;

const params = new URLSearchParams(location.search);
const role = params.get('role');
const status = { role, phase: 'starting', error: null, joins: {} };
globalThis.__match = status;
const show = () => {
  document.getElementById('log').textContent = JSON.stringify(status, null, 2);
};

const api = async (path, body) => {
  const response = await fetch(`/api/${path}`, body === undefined ? {} : { method: 'POST', body: JSON.stringify(body) });
  if (!response.ok) throw new Error(`${path}: HTTP ${response.status}`);
  return response.json();
};
const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
const until = async (what, check, ms) => {
  const deadline = Date.now() + ms;
  for (;;) {
    const value = await check();
    if (value) return value;
    if (Date.now() > deadline) throw new Error(`timed out waiting for ${what}`);
    await sleep(250);
  }
};

const config = await api('config');
const LABEL = 'shard-topology.move';
const CELL = 32;
const SQUAD = 8;
const side = Math.sqrt(config.players / Math.min(config.visible, config.players)) * 3 * CELL;
const codec =
  config.codec === 'compact'
    ? entityCodec({ x: quantized(0, side), z: quantized(0, side), yaw: angle(), hp: uint(8), ammo: uint(8), team: uint(8), kind: uint(8) })
    : undefined;
const withCodec = codec === undefined ? {} : { codec };
const round = n => Math.round(n * 100) / 100;
const clamp = n => Math.min(side, Math.max(0, n));
const move = (state, input) => ({ ...state, x: round(clamp(state.x + input.dx)), z: round(clamp(state.z + input.dz)), yaw: round(input.yaw) });

// ── measurement ─────────────────────────────────────────────────────────

const quantile = (values, q) => {
  if (values.length === 0) return null;
  const sorted = [...values].sort((a, b) => a - b);
  return sorted[Math.min(sorted.length - 1, Math.floor(sorted.length * q))];
};
const frames = [];
let lastFrame = null;
const tickMs = [];
let payloadBytes = 0;
const outcomes = { connect: {}, accept: {}, errors: [] };
const note = (kind, outcome) => {
  outcomes[kind][outcome] = (outcomes[kind][outcome] ?? 0) + 1;
};
let previousWire = new Map();

/** Wire bytes since the last reading, split anchor / direct, and how many direct pairs are up. */
async function wire() {
  let anchorSent = 0;
  let anchorReceived = 0;
  let directSent = 0;
  let directReceived = 0;
  let directUp = 0;
  // All reads at once, each bounded: sequential reads over 65 connections
  // under load took longer than the report interval, so reports piled up
  // and none landed (seen at 1 × 64). A connection that does not answer
  // keeps its last reading, so the next one is not a delta from zero.
  const readings = await Promise.all(
    connections.map(async ({ pc }, i) => {
      if (i > 0 && pc.connectionState === 'closed') return null;
      try {
        const report = await Promise.race([
          pc.getStats(),
          new Promise((_, reject) => setTimeout(() => reject(new Error('getStats timed out')), 500)),
        ]);
        let sent = 0;
        let received = 0;
        report.forEach(entry => {
          if (entry.type === 'transport') {
            sent += entry.bytesSent ?? 0;
            received += entry.bytesReceived ?? 0;
          }
        });
        return { pc, i, sent, received };
      } catch {
        return null;
      }
    }),
  );
  const now = new Map(previousWire);
  for (const reading of readings) {
    if (reading === null) continue;
    const { pc, i, sent, received } = reading;
    const before = previousWire.get(pc) ?? { sent: 0, received: 0 };
    now.set(pc, { sent, received });
    if (i === 0) {
      anchorSent += sent - before.sent;
      anchorReceived += received - before.received;
    } else {
      directSent += sent - before.sent;
      directReceived += received - before.received;
      if (pc.connectionState === 'connected') directUp += 1;
    }
  }
  previousWire = now;
  return { anchorSent, anchorReceived, directSent, directReceived, directUp, connections: connections.length };
}

function frameStats() {
  const window = frames.splice(0);
  return { frames: window.length, p50: quantile(window, 0.5), p95: quantile(window, 0.95), max: window.length ? Math.max(...window) : null };
}

// ── connect ─────────────────────────────────────────────────────────────

let node;
let index;
const extra = {};
async function start() {
  const connectStarted = performance.now();
  const credential = await requestCredential({ anchorUrl: config.anchorUrl, game: config.game });
  node = await connect({ credentialB64: credential.credentialB64, bootstrapUrl: credential.bootstrapUrl, ...rememberedIdentity() });
  await until('enrollment', () => node.isEnrolled(), 60_000);
  extra.connectMs = Math.round(performance.now() - connectStarted);
  status.node = node.nodeIdHex();
  ({ index } = await api('register', { role, node: status.node }));
  status.index = index;
  status.phase = 'registered';
  show();

  // Answer every peer's offer, so pairs go direct (unless the run measures
  // the relayed path on purpose).
  if (!config.relayed) {
    node.onEvent(event => {
      if (event.type !== 'signal' || event.kind !== '1' || !/^\d{1,20}$/.test(event.from ?? '')) return;
      const peer = BigInt(event.from).toString(16).padStart(16, '0');
      node.acceptPeer(peer).then(
        outcome => note('accept', outcome.type),
        error => note('accept', `error:${error?.kind ?? 'unknown'}`),
      );
    });
  }

  // A browser node accepts a handshake only from a peer whose signed
  // announcement it holds, so every page announces (a lease, renewed).
  const tag = `${config.game}:${role}`;
  const announce = () => node.announce([tag]).catch(() => {});
  await announce();
  setInterval(announce, config.announceMs);

  const hosts = await until('every host registered', async () => {
    const { hosts } = await api('hosts');
    return hosts.length >= config.hosts ? hosts : null;
  }, 300_000);
  // Dial nobody before their announcement is here: a host waits for every
  // player and host, a player for every host.
  status.phase = 'awaiting announcements';
  show();
  const seen = async which => new Set((await node.query(`${config.game}:${which}`)).map(d => d.peerIdHex));
  await until('every host announced', async () => {
    const ids = await seen('host');
    return hosts.every(h => h.node === status.node || ids.has(h.node));
  }, 300_000);

  if (role === 'host') await runHost(hosts);
  else await runPlayer(hosts);
}

/** Join every shard host in `hosts` except `self`: as the owner's player, or as a spectator. */
async function joinAll(hosts, self, local) {
  const clients = new Map();
  const joinedAt = {};
  await Promise.all(
    hosts.map(async ({ node: host }, h) => {
      if (host === self) return;
      const started = performance.now();
      status.joins[h] = 'connectPeer';
      if (!config.relayed) {
        try {
          const outcome = await node.connectPeer(host);
          note('connect', outcome.type);
          extra[`connect:${h}`] = { outcome: outcome.type, ms: Math.round(performance.now() - started) };
        } catch (error) {
          note('connect', `error:${error?.kind ?? 'unknown'}`);
          if (outcomes.errors.length < 10) outcomes.errors.push({ host: h, ms: Math.round(performance.now() - started), message: String(error?.message ?? error) });
        }
      }
      const client = joinNetcode({
        transport: node,
        host,
        label: LABEL,
        ...withCodec,
        ...(local && local.owner === h ? { local: local.entity } : {}),
        ...(local ? { interest: cellsAround(local.at.x, local.at.z, { size: CELL }) } : {}),
      });
      clients.set(h, client);
      (extra.joining ??= new Map()).set(h, client);
      status.joins[h] = 'awaiting first snapshot';
      await until(`host ${h}'s first snapshot`, () => client.stats().snapshots > 0, 300_000);
      joinedAt[h] = Math.round(performance.now() - started);
      status.joins[h] = 'joined';
    }),
  );
  return { clients, joinedAt };
}

// ── host ────────────────────────────────────────────────────────────────

async function runHost(hosts) {
  const self = status.node;
  const shard = hosts.findIndex(entry => entry.node === self);
  const world = new Map();
  let r = (shard + 1) * 7919;
  const random = () => {
    r = (r * 16807) % 2147483647;
    return r / 2147483647;
  };
  for (let k = 0; k < config.npcs; k += 1) {
    if (k % config.hosts !== shard) continue;
    world.set(`npc${k}`, { x: round(random() * side), z: round(random() * side), yaw: 0, hp: 200, ammo: 0, team: 0, kind: 1 + (k % 4) });
  }
  // The payload netcode hands the transport, to set against wire bytes.
  const counted = {
    nodeIdHex: () => node.nodeIdHex(),
    onEvent: handler => node.onEvent(handler),
    connectPeer: peer => node.connectPeer(peer),
    openStream: async options => {
      const stream = await node.openStream(options);
      return {
        send: payload => {
          payloadBytes += payload.length;
          return stream.send(payload);
        },
        close: () => stream.close(),
      };
    },
  };
  const host = hostNetcode({
    transport: counted,
    label: LABEL,
    tickRate: config.tickRate,
    autoTick: false,
    maxRewindMs: 300,
    step: ({ inputs }) => {
      for (const [peer, list] of inputs) {
        let state = world.get(peer) ?? { x: round(random() * side), z: round(random() * side), yaw: 0, hp: 100, ammo: 30, team: 0, kind: 0 };
        for (const { data } of list) state = move(state, data);
        world.set(peer, state);
      }
      for (const [id, state] of world) {
        if (!id.startsWith('npc')) continue;
        world.set(id, { ...state, x: round(clamp(state.x + (random() - 0.5) * 2)), z: round(clamp(state.z + (random() - 0.5) * 2)) });
      }
    },
    snapshot: () => Object.fromEntries(world),
    interest: (_id, e) => cellKey(e.x, e.z, CELL),
    ...withCodec,
  });
  setInterval(() => {
    const started = performance.now();
    host.tick();
    tickMs.push(performance.now() - started);
  }, 1000 / config.tickRate);
  status.phase = 'hosting';
  show();

  // Spectate the other shards; render the merged world every frame.
  const { clients } = await joinAll(hosts, self, null);
  const canvas = document.getElementById('scene');
  const draw = canvas.getContext('2d');
  const render = at => {
    if (lastFrame !== null) frames.push(at - lastFrame);
    lastFrame = at;
    draw.clearRect(0, 0, canvas.width, canvas.height);
    const scale = canvas.width / side;
    const paint = entities => {
      for (const e of Object.values(entities)) draw.fillRect(e.x * scale, e.z * scale, 3, 3);
    };
    paint(Object.fromEntries(world));
    for (const client of clients.values()) paint(client.view());
    // A stand-in for a heavier scene: burn this long per frame.
    const until = performance.now() + config.burnMs;
    while (performance.now() < until);
    requestAnimationFrame(render);
  };
  requestAnimationFrame(render);
  status.phase = 'ready';
  extra.players = () => host.players().length;
  extra.heldPeers = () => host.players();
  extra.lastSendError = () => host.lastSendError;
  extra.clients = () => Object.fromEntries([...clients].map(([h, c]) => [h, c.stats()]));
  extra.dropped = () => host.dropped;
}

// ── player ──────────────────────────────────────────────────────────────

async function runPlayer(hosts) {
  const self = status.node;
  const owner = Math.floor(index / SQUAD) % config.hosts;
  let at = { x: Math.random() * side, z: Math.random() * side };
  const { clients, joinedAt } = await joinAll(hosts, null, {
    owner,
    at,
    entity: { id: self, predict: move },
  });
  extra.joinedAt = joinedAt;
  status.phase = 'ready';
  show();
  let interestKey = '';
  setInterval(() => {
    const input = { dx: (Math.random() - 0.5) * 3, dz: (Math.random() - 0.5) * 3, yaw: Math.random() * 6.28 };
    clients.get(owner).input(input);
    const mine = clients.get(owner).view()[self];
    if (mine) at = mine;
    const cells = cellsAround(at.x, at.z, { size: CELL });
    const key = cells.join('|');
    if (key !== interestKey) {
      interestKey = key;
      for (const client of clients.values()) client.setInterest(cells);
    }
  }, 1000 / config.tickRate);
  const loop = time => {
    if (lastFrame !== null) frames.push(time - lastFrame);
    lastFrame = time;
    for (const client of clients.values()) client.view();
    requestAnimationFrame(loop);
  };
  requestAnimationFrame(loop);
  extra.netcode = () => {
    const out = {};
    for (const [h, client] of clients) {
      const s = client.stats();
      out[h] = { snapshots: s.snapshots, binary: s.binaryFrames, corrections: s.corrections, partial: s.partialSnapshots, rtt: s.clock?.rttMs ?? null };
    }
    return out;
  };
}

// This node's view of one peer pair: the live attempt (direct or not),
// and whether netcode holds the peer. For a pair that never delivered.
globalThis.__peer = async id => {
  let attempt;
  try {
    attempt = await node.peerAttempt(id);
  } catch (error) {
    attempt = { error: String(error?.message ?? error) };
  }
  return { attempt, heldByHost: typeof extra.heldPeers === 'function' ? extra.heldPeers().includes(id) : null };
};

// Every non-zero leaf counter, drop reasons included: where a peer's
// packets die (`no_session`, `routing_expired`, …) when a pair never
// delivers.
const nonZeroCounters = () => {
  try {
    return Object.fromEntries(Object.entries(node.counters()).filter(([, v]) => v !== '0'));
  } catch (error) {
    return { error: String(error?.message ?? error) };
  }
};
globalThis.__counters = nonZeroCounters;

globalThis.__diag = () => ({
  outcomes,
  connect: Object.fromEntries(Object.entries(extra).filter(([key]) => key.startsWith('connect:'))),
  players: typeof extra.players === 'function' ? extra.players() : null,
  dropped: typeof extra.dropped === 'function' ? extra.dropped() : null,
  joining: extra.joining ? Object.fromEntries([...extra.joining].map(([h, c]) => [h, c.stats()])) : null,
  lastSendError: typeof extra.lastSendError === 'function' ? extra.lastSendError() : null,
  clients: typeof extra.clients === 'function' ? extra.clients() : null,
  counters: node?.rtcStats().counters ?? null,
  connections: connections.map(({ pc }) => pc.connectionState),
  leafCounters: node ? nonZeroCounters() : null,
});

// ── report ──────────────────────────────────────────────────────────────

// One report at a time: a slow one is skipped over, never stacked.
let reporting = false;
setInterval(async () => {
  if (index === undefined || reporting) return;
  reporting = true;
  try {
    const counters = node.rtcStats().counters;
    const ticks = tickMs.splice(0);
    await api('report', {
      role,
      index,
      node: status.node,
      phase: status.phase,
      joins: status.joins,
      at: Date.now(),
      wire: await wire(),
      payloadBytes: (() => {
        const value = payloadBytes;
        payloadBytes = 0;
        return value;
      })(),
      frame: frameStats(),
      tick: { count: ticks.length, p50: quantile(ticks, 0.5), p95: quantile(ticks, 0.95) },
      heapMb: performance.memory ? performance.memory.usedJSHeapSize / 1e6 : null,
      cdp: globalThis.__cdp ?? null,
      ice: { attempted: counters.ice_attempted, direct: counters.ice_direct, relayed: counters.ice_relayed, failed: counters.ice_failed },
      outcomes,
      players: typeof extra.players === 'function' ? extra.players() : null,
      dropped: typeof extra.dropped === 'function' ? extra.dropped() : null,
      netcode: typeof extra.netcode === 'function' ? extra.netcode() : null,
      joinedAt: extra.joinedAt ?? null,
      connectMs: extra.connectMs ?? null,
      connect: Object.fromEntries(Object.entries(extra).filter(([key]) => key.startsWith('connect:'))),
    });
  } catch (error) {
    status.reportError = String(error?.message ?? error);
  } finally {
    reporting = false;
  }
}, 1000);

start().catch(error => {
  status.phase = 'failed';
  status.error = String(error?.message ?? error);
  show();
  api('report', { role, index, node: status.node, phase: 'failed', error: status.error, at: Date.now() }).catch(() => {});
});
