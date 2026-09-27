// One player's side of the anchor acceptance run. Everything here is the
// published package's public API against a real `net-mesh anchor serve
// --game`: no hand-minted credential, no demo host.
//
// `?role=host` fetches a credential, connects, and opens a lobby.
// `?role=join` fetches its own credential, connects, finds the lobby in
// the list, joins it and acts.
// `?role=unknown-game` asks for a game the anchor does not admit.
// `?role=reuse&credential=…` connects with ANOTHER player's credential —
// the anchor must refuse to enroll a second identity with it.
// `?role=return&credential=…` is a reload: the same browser storage, so
// the same remembered player, reconnecting with the credential it was
// first given — it must come back as the same node, enrolled.
//
// The result lands on `globalThis.__acceptance` for run.mjs to read.

import {
  connect,
  createLobby,
  defineStore,
  joinLobby,
  listLobbies,
  rememberedIdentity,
  requestCredential,
} from '/pkg/index.bundle.js';
import { hostNetcode, joinNetcode } from '/pkg/netcode/index.js';

// Netcode (model 2) rides beside the lobby: ships moved by inputs,
// predicted locally, on the lossy carrier.
const NET_LABEL = 'acceptance.movement';
const move = (ship, input) => ({ x: ship.x + input.dx });
const lossy = node => {
  const counters = node.rtcStats().counters;
  return { written: Number(counters.lossy_written ?? 0), ingress: Number(counters.lossy_ingress ?? 0) };
};

const params = new URLSearchParams(location.search);
const role = params.get('role');
const anchorUrl = params.get('anchor');
const game = params.get('game') ?? 'acceptance';
const lobbyName = params.get('name') ?? 'Acceptance arena';
const result = { role, done: false, ok: false, steps: [] };
globalThis.__acceptance = result;

function step(name, detail = {}) {
  result.steps.push({ name, ...detail });
  document.getElementById('log').textContent = JSON.stringify(result, null, 2);
}

const record = value => (typeof value === 'object' && value !== null ? value : {});
const room = defineStore({
  id: 'acceptance.room',
  version: 1,
  state: value => {
    const seats = {};
    for (const [peer, seat] of Object.entries(record(record(value).seats))) seats[peer] = Number(seat);
    return { seats };
  },
  empty: () => ({ seats: {} }),
  visibility: 'open',
  actions: { sit: { input: () => ({}), output: value => ({ seat: Number(record(value).seat) }) } },
  inputs: {},
});

const until = async (what, check, ms = 60_000) => {
  const deadline = Date.now() + ms;
  for (;;) {
    const value = await check();
    if (value) return value;
    if (Date.now() > deadline) throw new Error(`timed out waiting for ${what}`);
    await new Promise(resolve => setTimeout(resolve, 250));
  }
};

async function connected(credentialB64, bootstrapUrl) {
  // The same player on every visit: the secrets live in this browser
  // context's localStorage.
  const node = await connect({ credentialB64, bootstrapUrl, ...rememberedIdentity() });
  step('connected', { node: node.nodeIdHex() });
  await until('enrollment', () => node.isEnrolled(), 30_000);
  step('enrolled');
  return node;
}

async function run() {
  if (role === 'unknown-game') {
    try {
      await requestCredential({ anchorUrl, game: 'no-such-game' });
      step('issued', { unexpected: true });
    } catch (error) {
      step('refused', { kind: error.kind, status: error.status });
      result.ok = error.kind === 'unknown-game';
    }
    return;
  }
  if (role === 'reuse') {
    // Someone else's credential: the handshake may complete, but the
    // anchor must not enroll this second identity with it.
    // connect() awaits enrollment, so the anchor's refusal surfaces
    // here, typed — or, if it ever resolves, the node must not be
    // enrolled.
    try {
      const node = await connect({ credentialB64: params.get('credential'), bootstrapUrl: params.get('bootstrap') });
      step('connected', { node: node.nodeIdHex() });
      await new Promise(resolve => setTimeout(resolve, 8_000));
      const enrolled = node.isEnrolled();
      step('after-wait', { enrolled });
      result.ok = !enrolled;
    } catch (error) {
      const message = String(error?.message ?? error);
      step('refused', { message });
      result.ok = /replay/.test(message);
    }
    return;
  }

  if (role === 'return') {
    const node = await connected(params.get('credential'), params.get('bootstrap'));
    result.node = node.nodeIdHex();
    result.ok = true;
    return;
  }

  const credential = await requestCredential({ anchorUrl, game });
  step('credential', { game: credential.game, bootstrapUrl: credential.bootstrapUrl });
  result.credentialB64 = credential.credentialB64;
  result.bootstrapUrl = credential.bootstrapUrl;
  const node = await connected(credential.credentialB64, credential.bootstrapUrl);
  result.node = node.nodeIdHex();

  if (role === 'host') {
    const lobby = await createLobby({
      node,
      game,
      name: lobbyName,
      capacity: 4,
      definition: room,
      initialState: { seats: {} },
      actions: {
        sit: (_input, context) => {
          const seat = Object.keys(context.getState().seats).length + 1;
          context.setState({ seats: { ...context.getState().seats, [context.peer]: seat } });
          return { seat };
        },
      },
      inputs: {},
    });
    step('lobby', { code: lobby.code });
    result.code = lobby.code;
    const ships = new Map();
    const applied = [];
    const net = params.get('wait') === '0' ? null : hostNetcode({
      transport: node,
      label: NET_LABEL,
      tickRate: 30,
      step: ({ inputs }) => {
        for (const [peer, list] of inputs) {
          for (const input of list) {
            applied.push(input.seq);
            ships.set(peer, move(ships.get(peer) ?? { x: 0 }, input.data));
          }
        }
      },
      snapshot: () => Object.fromEntries(ships),
    });
    if (params.get('wait') === '0') {
      result.ok = true;
      return;
    }
    await until('a second player seated', () => Object.keys(lobby.host.getState().seats).length >= 1, 120_000);
    step('seated', { seats: lobby.host.getState().seats });
    const joiner = Object.keys(lobby.host.getState().seats)[0];
    globalThis.__diag = () => ({ dropped: net.dropped, players: net.players(), lossy: lossy(node), ships: Object.fromEntries(ships) });
    await until("the joiner's netcode ship at x=30", () => ships.get(joiner)?.x === 30, 120_000);
    result.netcode = {
      ship: ships.get(joiner),
      applied: applied.length,
      unique: new Set(applied).size,
      players: net.players(),
      lossy: lossy(node),
    };
    step('netcode', result.netcode);
    result.ok = true;
    return;
  }

  if (role === 'isolated') {
    // A player of ANOTHER game on the same anchor: it must find its own
    // game's lobby (so its discovery demonstrably works) and never see
    // the foreign game's — by list or by raw tag — over a window long
    // enough for several of that host's re-announcements.
    const foreign = params.get('foreign');
    let foundOwn = false;
    let sawForeign = [];
    const until = Date.now() + 25_000;
    let ownAt = null;
    while (Date.now() < until && (ownAt === null || Date.now() - ownAt < 10_000)) {
      if (!foundOwn) {
        foundOwn = (await listLobbies({ node, game })).some(entry => entry.name === lobbyName);
        if (foundOwn) ownAt = Date.now();
      }
      const listed = await listLobbies({ node, game: foreign });
      const tagged = await node.query(`net-lobby:${foreign}`);
      if (listed.length > 0 || tagged.length > 0) sawForeign.push({ listed: listed.length, tagged: tagged.length });
      await new Promise(resolve => setTimeout(resolve, 500));
    }
    step('watched', { foundOwn, sawForeign: sawForeign.length });
    result.ok = foundOwn && sawForeign.length === 0;
    return;
  }

  // join
  const listing = await until('the lobby in the list', async () => {
    const lobbies = await listLobbies({ node, game });
    return lobbies.find(entry => entry.name === lobbyName);
  });
  step('listed', { code: listing.code, host: listing.host });
  const player = await joinLobby({ node, definition: room, game, lobby: listing });
  await player.ready();
  step('joined');
  const { seat } = await player.act('sit', {});
  step('sat', { seat });
  await until('my seat in my own view', () => player.getState().seats[node.nodeIdHex()] === seat, 30_000);

  // Netcode: join the host's movement, predict my ship, send 31 inputs.
  const self = node.nodeIdHex();
  const net = joinNetcode({ transport: node, host: listing.host, label: NET_LABEL, local: { id: self, predict: move } });
  globalThis.__diag = () => ({ stats: net.stats(), lossy: lossy(node), view: net.view() });
  await until('netcode snapshots', () => net.stats().snapshots > 0, 30_000);
  net.input({ dx: 0 });
  let immediate = true;
  for (let i = 0; i < 30; i += 1) {
    const before = net.view()[self]?.x;
    net.input({ dx: 1 });
    if (before !== undefined && net.view()[self]?.x !== before + 1) immediate = false;
    await new Promise(resolve => setTimeout(resolve, 33));
  }
  await until('every input acknowledged', () => net.stats().pendingInputs === 0 && net.view()[self]?.x === 30, 60_000);
  const stats = net.stats();
  result.netcode = {
    view: net.view()[self],
    immediate,
    corrections: stats.corrections,
    snapshots: stats.snapshots,
    clock: stats.clock,
    lossy: lossy(node),
  };
  step('netcode', result.netcode);
  result.ok = true;
}

run()
  .catch(error => {
    let diag = null;
    try {
      diag = globalThis.__diag?.() ?? null;
    } catch (e) {
      diag = String(e);
    }
    step('error', { message: String(error?.message ?? error), kind: error?.kind, diag });
  })
  .finally(() => {
    result.done = true;
    step('done', { ok: result.ok });
  });
