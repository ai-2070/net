/**
 * The demo's wiring: a store, a scene, a keyboard.
 *
 * Two modes, and the difference is stated plainly because it is the
 * difference between a demo and evidence:
 *
 * - `?mode=local` (default) — host and player in this one page over
 *   `@net-mesh/browser/local`. The store is real; the mesh is not.
 * - `?mode=mesh` — this page's own node from the built package, over a
 *   real anchor. `&host=<16 hex>` joins that node's store; without it
 *   the page hosts and prints its own id to join from another browser.
 *
 * Nothing in here reaches into the package's internals: it imports the
 * built `dist/` entry point, exactly as an application would.
 */

import { defineGame, project, handlers, authorize } from './game.js';
import { createScene } from './scene.js';

const params = new URLSearchParams(globalThis.location?.search ?? '');
const mode = params.get('mode') ?? 'local';
const status = document.querySelector('#status');
const canvas = document.querySelector('#stage');

function say(text) {
  if (status !== null) status.textContent = text;
}

/** The built package, as a consumer sees it. */
async function loadPackage() {
  return import('../dist/index.js');
}

/** The in-page mesh, as a consumer imports `@net-mesh/browser/local`. */
async function loadLocal() {
  return import('../dist/local.js');
}

/** Held keys → a steer input, sent at most once per frame. */
function keyboard() {
  const held = new Set();
  addEventListener('keydown', event => held.add(event.key.toLowerCase()));
  addEventListener('keyup', event => held.delete(event.key.toLowerCase()));
  return {
    direction: () => ({
      dx: (held.has('d') || held.has('arrowright') ? 1 : 0) - (held.has('a') || held.has('arrowleft') ? 1 : 0),
      dz: (held.has('s') || held.has('arrowdown') ? 1 : 0) - (held.has('w') || held.has('arrowup') ? 1 : 0),
    }),
    firing: () => held.has(' '),
    clear: key => held.delete(key),
  };
}

async function startLocal(pkg) {
  const game = defineGame(pkg.defineStore);
  const mesh = (await loadLocal()).createLocalMesh();
  const hostNode = mesh.node();
  const playerNode = mesh.node();
  const spectatorNode = mesh.node();
  const hostId = hostNode.nodeIdHex();
  const playerId = playerNode.nodeIdHex();
  const spectatorId = spectatorNode.nodeIdHex();

  const host = pkg.hostStore({
    definition: game,
    transport: hostNode,
    initialState: { ships: {}, waypoint: { x: 6, z: -6 }, tick: 0 },
    maxEventBytes: 8104,
    authorize: request => authorize({ ...request, host: hostId }),
    project,
    ...handlers(),
  });

  const player = pkg.joinStore({
    definition: game,
    transport: playerNode,
    host: hostId,
    audience: ['crew'],
    key: 'player',
    maxEventBytes: 8104,
  });

  // A second caller reading a narrower audience, so the demo shows a
  // projection withholding something rather than claiming it would.
  const spectator = pkg.joinStore({
    definition: game,
    transport: spectatorNode,
    host: hostId,
    audience: ['crew'],
    key: 'spectator',
    maxEventBytes: 8104,
  });

  await Promise.all([player.ready(), spectator.ready()]);
  await player.act('enlist', { colour: 0 });
  await spectator.act('enlist', { colour: 1 });

  return { host, player, self: playerId, spectator, others: [spectatorId] };
}

/** Node ids arrive decimal from a descriptor and hex from a URL. */
function samePeerId(a, b) {
  const value = text => {
    const raw = String(text).trim();
    return raw.startsWith('0x') ? BigInt(raw) : /^[0-9]+$/.test(raw) ? BigInt(raw) : BigInt(`0x${raw}`);
  };
  try {
    return value(a) === value(b);
  } catch {
    return false;
  }
}

async function startMesh(pkg) {
  const credential = params.get('credential');
  if (credential === null) {
    throw new Error('mesh mode needs ?credential=<base64 bootstrap credential> — see demo/README.md');
  }
  // `connect`, not `openSession`, and the reason is a finding rather
  // than a preference: joining a store needs a SESSION with the host
  // peer, which `connectPeer` installs — and `connectPeer` is on the
  // `connect()` node, not on the leader/follower `MeshSession`. A
  // store joined over a session therefore fails with `no session
  // with 0x…`, which is what a real browser reported here. One tab
  // per node is all this demo wants anyway; the leader surface earns
  // its keep when several tabs share one node.
  const session = await pkg.connect({
    credential,
    bootstrapUrl: params.get('bootstrap') ?? undefined,
  });
  const game = defineGame(pkg.defineStore);
  const self = session.nodeIdHex();
  const hostNode = params.get('host');

  // Both sides announce this, and the joiner waits to SEE the host
  // before it joins. Not decoration: `connectPeer` builds a session
  // from the peer's own signed announcement, so a joiner that has
  // never seen one cannot reach the host at all — the store's frames
  // then fail with `no session with 0x…`, which is what this demo
  // did before the wait existed. Discovery is the mesh's
  // introduction; the store is what happens afterwards.
  const FLEET_TAG = 'fleet.host';
  // Announced REPEATEDLY, not once. An announcement is a lease the
  // mesh lets expire — announce once and a peer that looks a few
  // seconds later finds nothing, which is exactly what this demo did
  // before: `the host … never announced fleet.host`, from a host that
  // had announced it and then gone quiet. The native browser-demo
  // re-announces every 500 ms; twice a second is generous here
  // because nothing about this demo is announcement-latency bound.
  await session.announce([FLEET_TAG]);
  setInterval(() => {
    void session.announce([FLEET_TAG]).catch(() => {
      // A refused refresh is not fatal: the next tick tries again,
      // and a page that cannot announce fails visibly at its join.
    });
  }, 2_000);

  if (hostNode === null) {
    const host = pkg.hostStore({
      definition: game,
      transport: session,
      initialState: { ships: {}, waypoint: { x: 6, z: -6 }, tick: 0 },
      maxEventBytes: 8104,
      authorize: request => authorize({ ...request, host: self }),
      project,
      ...handlers(),
    });
    say(`hosting as ${String(self)} — join with ?mode=mesh&host=${String(self)}`);
    // The hosting tab plays through its OWN authority: a node cannot
    // join its own store (there is no session with itself), so
    // `hostPlayer` gives it a replica-shaped handle held to the same
    // `authorize`, handlers and projection as every other player.
    const player = pkg.hostPlayer(host, { audience: ['crew', 'command'] });
    await player.act('enlist', { colour: 2 });
    return { host, player, self, others: [], node: session };
  }

  // Wait for the host's announcement, then let `joinStore` install
  // the session from it.
  const deadline = Date.now() + 20_000;
  let seen = false;
  while (!seen && Date.now() < deadline) {
    const peers = await session.query(FLEET_TAG);
    seen = peers.some(peer => samePeerId(peer.nodeId, hostNode));
    if (!seen) await new Promise(resolve => setTimeout(resolve, 250));
  }
  if (!seen) throw new Error(`the host ${hostNode} never announced ${FLEET_TAG}`);

  const player = pkg.joinStore({
    definition: game,
    transport: session,
    host: hostNode,
    audience: ['crew'],
    key: 'player',
    maxEventBytes: 8104,
  });
  await player.ready();
  await player.act('enlist', { colour: 3 });
  say(`joined ${hostNode} as ${String(self)}`);
  return { host: null, player, self, others: [], node: session };
}

async function main() {
  const pkg = await loadPackage();
  say(`starting (${mode})…`);
  const world = mode === 'mesh' ? await startMesh(pkg) : await startLocal(pkg);
  const scene = createScene(canvas);
  // The store drives the scene graph through
  // `@net-mesh/browser/three`; this page only says what a ship LOOKS
  // like, never when to add or remove one.
  scene.attach(world.player, world.self);
  const keys = keyboard();

  function fit() {
    scene.resize(canvas.clientWidth, canvas.clientHeight);
  }
  addEventListener('resize', fit);
  fit();

  let previous = performance.now();
  let lastSent = { dx: 0, dz: 0 };

  function frame(at) {
    const dt = Math.min((at - previous) / 1000, 0.25);
    previous = at;

    const direction = keys.direction();
    // A held direction is an INPUT: coalesced, unacknowledged, and the
    // newest one wins. Sending only on change keeps a still ship
    // silent.
    if (direction.dx !== lastSent.dx || direction.dz !== lastSent.dz || direction.dx !== 0 || direction.dz !== 0) {
      world.player.input('steer', { ...direction, dt });
      lastSent = direction;
    }

    if (keys.firing()) {
      keys.clear(' ');
      const target = Object.keys(world.player.getState().ships).find(id => id !== world.self);
      if (target !== undefined) {
        // An ACTION: correlated, answered, and refused if the policy
        // says so — `fire` at yourself is one the host rejects.
        world.player
          .act('fire', { at: target })
          .then(result => {
            say(`hit ${target.slice(0, 8)} → hull ${String(result.hull)}`);
          })
          .catch(error => {
            say(`refused: ${String(error.code ?? error.message)}`);
          });
      }
    }

    scene.apply(world.player.getState());
    scene.render();
    requestAnimationFrame(frame);
  }

  world.player.subscribeStatus(next => {
    say(`${mode}: ${next.phase}${next.stale ? ' (stale)' : ''}`);
  });
  say(`${mode}: ready — WASD to steer, space to fire`);
  requestAnimationFrame(frame);

  // For the harness: what the page believes, without scraping pixels.
  globalThis.__demo = {
    mode,
    /** The node this page is on, for a run that needs to ask the mesh. */
    node: world.node ?? null,
    self: world.self,
    state: () => world.player.getState(),
    status: () => world.player.getStatus(),
    scene: () => scene.readback(),
    steer: (dx, dz, dt = 0.1) => world.player.input('steer', { dx, dz, dt }),
    fire: at => world.player.act('fire', { at }),
    others: world.others,
    hostState: () => (world.host === null ? null : world.host.getState()),
  };
}

main().catch(error => {
  say(`failed: ${String(error?.message ?? error)}`);
  globalThis.__demo = { failed: String(error?.stack ?? error) };
});
