/**
 * joinWorld over the local mesh: two host nodes serve four regions (two
 * region stores per node), a player's view merges the regions around it,
 * follows it as it moves, routes actions to its region, and refuses to
 * guess when a region's directory entry is contested.
 */

import { afterEach, describe, expect, it } from 'vitest';

import { createLocalMesh } from '../../src/local.js';
import { defineStore } from '../../src/store/definition.js';
import { hostStore, type HostedStoreHandle } from '../../src/store/host.js';
import type { Cancel } from '../../src/store/types.js';
import { announceRegions, joinWorld, regionOf, regionsAround, type WorldView } from '../../src/world/index.js';

const H1 = '00000000000000d1';
const H2 = '00000000000000d2';
const ROGUE = '00000000000000ee';
const PLAYER = '00000000000000c1';
const SIZE = 100;
const WORLD = 'test-world';

interface Ship {
  readonly x: number;
  readonly z: number;
}
interface Region {
  readonly ships: Record<string, Ship>;
}
type Actions = { poke: { input: Record<string, never>; output: { readonly region: string } } };

function record(value: unknown): Record<string, unknown> {
  if (typeof value !== 'object' || value === null) throw new Error('not a record');
  return value as Record<string, unknown>;
}
const definition = defineStore<Region, Actions, Record<string, never>>({
  id: 'test-world.region',
  version: 1,
  state: value => {
    const ships: Record<string, Ship> = {};
    for (const [id, ship] of Object.entries(record(record(value).ships))) {
      ships[id] = { x: Number(record(ship).x), z: Number(record(ship).z) };
    }
    return { ships };
  },
  empty: () => ({ ships: {} }),
  visibility: 'open',
  actions: { poke: { input: () => ({}), output: value => ({ region: String(record(value).region) }) } },
  inputs: {},
});

const stops: Cancel[] = [];
/** One node per id per mesh: `mesh.node(id)` creates, and refuses a second time. */
const nodes = new WeakMap<ReturnType<typeof createLocalMesh>, Map<string, ReturnType<ReturnType<typeof createLocalMesh>['node']>>>();
function nodeOf(mesh: ReturnType<typeof createLocalMesh>, id: string) {
  let byId = nodes.get(mesh);
  if (byId === undefined) nodes.set(mesh, (byId = new Map()));
  let node = byId.get(id);
  if (node === undefined) byId.set(id, (node = mesh.node(id)));
  return node;
}
const hosts: HostedStoreHandle<Region, Actions, Record<string, never>>[] = [];
const views: WorldView<Ship, Actions>[] = [];
afterEach(async () => {
  for (const stop of stops.splice(0)) stop();
  for (const view of views.splice(0)) await view.close();
  for (const host of hosts.splice(0)) await host.close();
});

async function until(check: () => boolean, what: string, ms = 5000): Promise<void> {
  const deadline = Date.now() + ms;
  while (!check()) {
    if (Date.now() > deadline) throw new Error(`timed out: ${what}`);
    await new Promise(resolve => setTimeout(resolve, 10));
  }
}

/** A region store on `node`, with two ships inside the region. */
function region(mesh: ReturnType<typeof createLocalMesh>, node: string, name: string, extra: Record<string, Ship> = {}) {
  const [, rx, rz] = name.split(':').map(Number) as [number, number, number];
  const ships: Record<string, Ship> = {
    [`${name}/a`]: { x: rx * SIZE + 10, z: rz * SIZE + 10 },
    [`${name}/b`]: { x: rx * SIZE + 60, z: rz * SIZE + 60 },
    ...extra,
  };
  const host = hostStore<Region, Actions, Record<string, never>>({
    definition,
    store: name,
    transport: nodeOf(mesh, node),
    initialState: { ships },
    maxEventBytes: 8104,
    authorize: () => true,
    actions: { poke: () => ({ region: name }) },
    inputs: {},
  });
  hosts.push(host);
  return host;
}

function world(mesh: ReturnType<typeof createLocalMesh>) {
  region(mesh, H1, 'r:0:0');
  // A ship that is in r:1:0 by position, still held by r:0:0 too — what a
  // handoff in flight can briefly look like to a player.
  region(mesh, H1, 'r:1:0', { straddler: { x: 150, z: 50 } });
  hosts[0]!.setState({ ships: { ...hosts[0]!.getState().ships, straddler: { x: 99, z: 50 } } });
  region(mesh, H2, 'r:0:1');
  region(mesh, H2, 'r:1:1');
  stops.push(announceRegions(nodeOf(mesh, H1), WORLD, ['r:0:0', 'r:1:0']));
  stops.push(announceRegions(nodeOf(mesh, H2), WORLD, ['r:0:1', 'r:1:1']));
}

describe('regions', () => {
  it('names regions by position and lists the ones around it', () => {
    expect(regionOf(150, -1, SIZE)).toBe('r:1:-1');
    expect(regionsAround(50, 50, { size: SIZE, radius: 1 })).toHaveLength(9);
    expect(regionsAround(50, 50, { size: SIZE, radius: 0 })).toEqual(['r:0:0']);
  });
});

describe('joinWorld', () => {
  it('merges the regions around the player, routes actions to its region, and lets far ones go', async () => {
    const mesh = createLocalMesh();
    world(mesh);
    const view = joinWorld<Region, Actions, Record<string, never>, Ship>({
      node: nodeOf(mesh, PLAYER),
      world: WORLD,
      definition,
      collection: 'ships',
      position: { x: 50, z: 50 },
      size: SIZE,
      key: 'player',
      maxEventBytes: 8104,
      positionOf: entity => entity as Ship,
    });
    views.push(view);
    await until(() => view.regions().filter(r => r.phase === 'ready').length === 4, 'four regions ready');
    const ids = Object.keys(view.getState()).sort();
    expect(ids).toEqual([
      'r:0:0/a', 'r:0:0/b', 'r:0:1/a', 'r:0:1/b', 'r:1:0/a', 'r:1:0/b', 'r:1:1/a', 'r:1:1/b', 'straddler',
    ]);
    // Held by two regions, shown once: the copy from the region containing it.
    expect(view.getState().straddler).toEqual({ x: 150, z: 50 });
    expect(view.regions().filter(r => r.phase === 'unhosted')).toHaveLength(5);

    expect(await view.act('poke', {})).toEqual({ region: 'r:0:0' });
    view.setPosition(150, 50);
    expect(view.currentRegion()).toBe('r:1:0');
    expect(await view.act('poke', {})).toEqual({ region: 'r:1:0' });

    // Far away: every held region is released and the view empties.
    view.setPosition(1_050, 1_050);
    await until(() => view.regions().every(r => !r.region.startsWith('r:0:') && !r.region.startsWith('r:1:')), 'released');
    expect(view.getState()).toEqual({});
  });

  it('does not join a region two nodes claim, unless the world names its hosts', async () => {
    const mesh = createLocalMesh();
    world(mesh);
    stops.push(announceRegions(nodeOf(mesh, ROGUE), WORLD, ['r:0:0']));
    const open = joinWorld<Region, Actions, Record<string, never>, Ship>({
      node: nodeOf(mesh, PLAYER),
      world: WORLD,
      definition,
      collection: 'ships',
      position: { x: 50, z: 50 },
      size: SIZE,
      radius: 0,
      key: 'player',
      maxEventBytes: 8104,
    });
    views.push(open);
    await until(() => open.regions()[0]?.phase === 'ambiguous', 'ambiguous');
    expect(open.getState()).toEqual({});

    const trusting = joinWorld<Region, Actions, Record<string, never>, Ship>({
      node: nodeOf(mesh, '00000000000000c2'),
      world: WORLD,
      definition,
      collection: 'ships',
      position: { x: 50, z: 50 },
      size: SIZE,
      radius: 0,
      key: 'player',
      maxEventBytes: 8104,
      trustedHosts: [H1, H2],
    });
    views.push(trusting);
    await until(() => trusting.regions()[0]?.phase === 'ready', 'trusted region ready');
    expect(trusting.regions()[0]?.host).toBe(H1);
  });

  // One hex id in ~1,800 has no letter in it. The directory read such a
  // host as a DECIMAL, so the region resolved to 00110c53bcd7ffa3: a node
  // that is not on the mesh, and not the one `trustedHosts` names either.
  it('finds a region whose host hex id has no letters in it', async () => {
    const DIGITS = '4798628394172323';
    const mesh = createLocalMesh();
    region(mesh, DIGITS, 'r:0:0');
    stops.push(announceRegions(nodeOf(mesh, DIGITS), WORLD, ['r:0:0']));
    const view = joinWorld<Region, Actions, Record<string, never>, Ship>({
      node: nodeOf(mesh, PLAYER),
      world: WORLD,
      definition,
      collection: 'ships',
      position: { x: 50, z: 50 },
      size: SIZE,
      radius: 0,
      key: 'player',
      maxEventBytes: 8104,
      trustedHosts: [DIGITS],
    });
    views.push(view);
    await until(() => view.regions()[0]?.phase === 'ready', 'all-digit region ready');
    expect(view.regions()[0]?.host).toBe(DIGITS);
    expect(await view.act('poke', {})).toEqual({ region: 'r:0:0' });
  });
});
