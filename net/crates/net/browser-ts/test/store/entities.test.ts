/**
 * Entity writes (`setEntities` / `setEntity`): only the written entities
 * are validated, the whole-document validator does not run, untouched
 * entities keep their identity, and a replica receives one small delta.
 */

import { describe, expect, it } from 'vitest';

import { createLocalMesh } from '../../src/local.js';
import { defineStore } from '../../src/store/definition.js';
import { StoreError } from '../../src/store/errors.js';
import { hostStore, type HostedStoreHandle } from '../../src/store/host.js';
import { cellKey, cellsAround } from '../../src/store/interest.js';
import { joinStore } from '../../src/store/join.js';
import type { Cancel } from '../../src/store/types.js';

const MAX_EVENT_BYTES = 8104;
const HOST = '00000000000000aa';
const NEAR = '00000000000000b1';
const SIZE = 10;
const never = (): Cancel => () => {};

interface Ship {
  readonly x: number;
  readonly z: number;
}
interface World {
  readonly ships: Record<string, Ship>;
  readonly tick: number;
}
type Actions = { move: { input: { readonly id: string; readonly x: number }; output: { readonly ok: boolean } } };
type Inputs = Record<string, never>;

function record(value: unknown): Record<string, unknown> {
  if (typeof value !== 'object' || value === null) throw new Error('not a record');
  return value as Record<string, unknown>;
}

const calls = { state: 0, ship: 0 };

function parseShip(value: unknown): Ship {
  calls.ship += 1;
  const x = Number(record(value).x);
  const z = Number(record(value).z);
  if (!Number.isFinite(x) || !Number.isFinite(z)) throw new Error('a ship needs finite x and z');
  return { x, z };
}

const world = defineStore<World, Actions, Inputs>({
  id: 'entities-test.world',
  version: 1,
  state: value => {
    calls.state += 1;
    const ships: Record<string, Ship> = {};
    for (const [id, ship] of Object.entries(record(record(value).ships))) ships[id] = parseShip(ship);
    return { ships, tick: Number(record(value).tick) };
  },
  empty: () => ({ ships: {}, tick: 0 }),
  visibility: 'open',
  interest: { ships: (ship: Ship) => cellKey(ship.x, ship.z, SIZE) },
  entities: { ships: parseShip },
  actions: {
    move: {
      input: value => ({ id: String(record(value).id), x: Number(record(value).x) }),
      output: value => ({ ok: record(value).ok === true }),
    },
  },
  inputs: {},
});

/** 400 ships, one per cell of a 20×20 grid. */
function grid(): World {
  const ships: Record<string, Ship> = {};
  for (let column = 0; column < 20; column += 1) {
    for (let row = 0; row < 20; row += 1) ships[`s${column}_${row}`] = { x: column * SIZE + 5, z: row * SIZE + 5 };
  }
  return { ships, tick: 0 };
}

function serve() {
  const mesh = createLocalMesh();
  // The action writes through the HOST handle from inside its handler, so
  // the entity write has to join the handler's transaction.
  const box: { host?: HostedStoreHandle<World, Actions, Inputs> } = {};
  const host = hostStore<World, Actions, Inputs>({
    definition: world,
    transport: mesh.node(HOST),
    initialState: grid(),
    maxEventBytes: MAX_EVENT_BYTES,
    schedule: never,
    authorize: () => true,
    actions: {
      move: input => {
        box.host!.setEntity('ships', input.id, { x: input.x, z: 5 });
        if (input.x < 0) throw new Error('no negative x');
        return { ok: true };
      },
    },
    inputs: {},
  });
  box.host = host;
  return { mesh, host };
}

async function settle(): Promise<void> {
  for (let turn = 0; turn < 60; turn += 1) await Promise.resolve();
}

describe('entity writes', () => {
  it('validate only what they write and never run the whole-document validator', () => {
    const { host } = serve();
    calls.state = 0;
    calls.ship = 0;
    host.setEntities('ships', { s0_0: { x: 6, z: 5 }, s1_1: { x: 16, z: 15 } });
    expect(calls).toEqual({ state: 0, ship: 2 });
    expect(host.getState().ships.s0_0).toEqual({ x: 6, z: 5 });
    expect(host.getState().ships.s1_1).toEqual({ x: 16, z: 15 });
  });

  it('keep every untouched entity, and the rest of the document, by identity', () => {
    const { host } = serve();
    const before = host.getState();
    host.setEntity('ships', 's0_0', { x: 7, z: 5 });
    const after = host.getState();
    expect(after).not.toBe(before);
    expect(after.ships).not.toBe(before.ships);
    expect(after.ships.s5_5).toBe(before.ships.s5_5);
    expect(Object.isFrozen(after.ships)).toBe(true);
    expect(Object.isFrozen(after.ships.s0_0)).toBe(true);
  });

  it('remove an entity with undefined, and an unchanged write is no change at all', () => {
    const { host } = serve();
    host.setEntity('ships', 's0_0', undefined);
    expect('s0_0' in host.getState().ships).toBe(false);
    expect(Object.keys(host.getState().ships)).toHaveLength(399);
    const same = host.getState();
    host.setEntity('ships', 's1_1', { x: 15, z: 15 });
    expect(host.getState()).toBe(same);
  });

  it('notify a subscriber once per write, with the document it replaced; removing an absent id is nothing', async () => {
    const { host } = serve();
    const seen: [World, World][] = [];
    host.subscribe((state, previous) => seen.push([state, previous]));
    const before = host.getState();
    host.setEntities('ships', { s0_0: { x: 6, z: 5 }, s0_1: undefined, missing: undefined });
    host.setEntity('ships', 'missing', undefined);
    await settle();
    expect(seen).toHaveLength(1);
    expect(seen[0]![1]).toBe(before);
    expect(seen[0]![0]).toBe(host.getState());
    expect(Object.keys(host.getState().ships)).toHaveLength(399);
  });

  it('refuse an invalid entity with nothing published', () => {
    const { host } = serve();
    const before = host.getState();
    let thrown: unknown;
    try {
      host.setEntities('ships', { s0_0: { x: 1, z: 1 }, s0_1: { x: 'north', z: 1 } as unknown as Ship });
    } catch (error) {
      thrown = error;
    }
    expect(thrown).toBeInstanceOf(StoreError);
    expect((thrown as StoreError).code).toBe('invalid-data');
    expect(String((thrown as StoreError).message)).toContain('ships.s0_1');
    expect(host.getState()).toBe(before);
  });

  it('refuse a collection the definition gave no per-entity parser', () => {
    const { host } = serve();
    expect(() => host.setEntity('tick' as never, 'x', 1 as never)).toThrow(/no per-entity parser for 'tick'/);
    expect(() => host.setEntity('toString' as never, 'x', 1 as never)).toThrow(/no per-entity parser/);
  });

  it('store an id of __proto__ as data, not as a prototype', () => {
    const { host } = serve();
    host.setEntity('ships', '__proto__', { x: 1, z: 1 });
    const ships = host.getState().ships;
    expect(Object.prototype.hasOwnProperty.call(ships, '__proto__')).toBe(true);
    expect(Object.getPrototypeOf(ships)).toBe(Object.prototype);
  });

  it('reach an interested replica as one delta carrying only the written entity', async () => {
    const { mesh, host } = serve();
    const node = mesh.node(NEAR);
    const near = joinStore<World, Actions, Inputs>({
      definition: world,
      transport: node,
      host: HOST,
      audience: [],
      key: 'player',
      maxEventBytes: MAX_EVENT_BYTES,
      schedule: never,
      interest: cellsAround(5, 5, { size: SIZE }),
    });
    await near.ready();
    const seen: string[] = [];
    node.onEvent(event => {
      if (event.payload !== undefined) seen.push(new TextDecoder().decode(event.payload));
    });

    host.setEntities('ships', { s0_0: { x: 8, z: 5 }, s19_19: { x: 196, z: 196 } });
    await settle();
    expect(seen).toHaveLength(1);
    const ops = JSON.parse(seen[0]!).ops as { p: string[] }[];
    expect(ops.map(op => op.p.join('/'))).toEqual(['ships/s0_0']);
    expect(near.getState().ships.s0_0).toEqual({ x: 8, z: 5 });
  });

  it('join an action handler`s transaction, and roll back with it', async () => {
    const { mesh, host } = serve();
    const player = joinStore<World, Actions, Inputs>({
      definition: world,
      transport: mesh.node(NEAR),
      host: HOST,
      audience: [],
      key: 'player',
      maxEventBytes: MAX_EVENT_BYTES,
      schedule: never,
    });
    await player.ready();
    await expect(player.act('move', { id: 's0_0', x: 9 })).resolves.toEqual({ ok: true });
    expect(host.getState().ships.s0_0).toEqual({ x: 9, z: 5 });

    const before = host.getState();
    await expect(player.act('move', { id: 's0_0', x: -1 })).rejects.toBeInstanceOf(StoreError);
    expect(host.getState()).toBe(before);
  });
});

describe('defineStore entities', () => {
  const base = {
    id: 'entities-test.bad',
    version: 1,
    state: (value: unknown) => value as World,
    empty: () => ({ ships: {}, tick: 0 }),
    actions: {},
    inputs: {},
  };

  it('refuses a dotted or empty collection name, and a non-function parser', () => {
    expect(() => defineStore({ ...base, entities: { 'a.b': parseShip } })).toThrow(/entities maps/);
    expect(() => defineStore({ ...base, entities: { '': parseShip } })).toThrow(/entities maps/);
    expect(() => defineStore({ ...base, entities: { ships: 1 as never } })).toThrow(/entities maps/);
  });

  it('refuses a collection that is not an entity map in empty()', () => {
    expect(() => defineStore({ ...base, entities: { tick: parseShip } })).toThrow(/not a record of entities/);
  });
});
