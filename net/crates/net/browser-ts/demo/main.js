/**
 * The demo's wiring: a store, a scene, a keyboard.
 *
 * Two modes, and the difference is stated plainly because it is the
 * difference between a demo and evidence:
 *
 * - `?mode=local` (default) — host and player in this one page over
 *   the development bus in `local-mesh.js`. The store is real; the
 *   mesh is not.
 * - `?mode=mesh` — this page's own node from the built package, over a
 *   real anchor. `&host=<16 hex>` joins that node's store; without it
 *   the page hosts and prints its own id to join from another browser.
 *
 * Nothing in here reaches into the package's internals: it imports the
 * built `dist/` entry point, exactly as an application would.
 */

import { defineGame, project, handlers, authorize } from './game.js';
import { localNode } from './local-mesh.js';
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

function hex(bytes) {
  const buffer = new Uint8Array(bytes);
  crypto.getRandomValues(buffer);
  return [...buffer].map(byte => byte.toString(16).padStart(2, '0')).join('');
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
  const hostId = hex(8);
  const playerId = hex(8);
  const spectatorId = hex(8);

  const host = pkg.hostStore({
    definition: game,
    transport: localNode(hostId),
    initialState: { ships: {}, waypoint: { x: 6, z: -6 }, tick: 0 },
    maxEventBytes: 8104,
    authorize: request => authorize({ ...request, host: hostId }),
    project,
    ...handlers(),
  });

  const player = pkg.joinStore({
    definition: game,
    transport: localNode(playerId),
    host: hostId,
    audience: ['crew'],
    key: 'player',
    maxEventBytes: 8104,
  });

  // A second caller reading a narrower audience, so the demo shows a
  // projection withholding something rather than claiming it would.
  const spectator = pkg.joinStore({
    definition: game,
    transport: localNode(spectatorId),
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

async function startMesh(pkg) {
  const credential = params.get('credential');
  if (credential === null) {
    throw new Error('mesh mode needs ?credential=<base64 bootstrap credential> — see demo/README.md');
  }
  const session = await pkg.openSession({
    credential,
    bootstrapUrl: params.get('bootstrap') ?? undefined,
  });
  const game = defineGame(pkg.defineStore);
  const self = session.nodeIdHex();
  const hostNode = params.get('host');

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
    // The host plays too, through its own loopback join.
    const player = pkg.joinStore({
      definition: game,
      transport: session,
      host: self,
      audience: ['crew', 'command'],
      key: 'host',
      maxEventBytes: 8104,
    });
    await player.ready();
    await player.act('enlist', { colour: 2 });
    return { host, player, self, others: [] };
  }

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
  return { host: null, player, self, others: [] };
}

async function main() {
  const pkg = await loadPackage();
  say(`starting (${mode})…`);
  const world = mode === 'mesh' ? await startMesh(pkg) : await startLocal(pkg);
  const scene = createScene(canvas);
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

    scene.apply(world.player.getState(), world.self);
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
