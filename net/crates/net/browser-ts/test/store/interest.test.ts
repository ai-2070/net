/**
 * Interest management: a player receives only the entities near them,
 * changes far away cost them nothing, and moving their interest adds
 * and removes entities without the view ever blanking.
 */

import { describe, expect, it } from 'vitest';

import { createLocalMesh, type LocalNode } from '../../src/local.js';
import { defineStore } from '../../src/store/definition.js';
import { hostStore } from '../../src/store/host.js';
import { cellKey, cellsAround, sameCells, stickyCells } from '../../src/store/interest.js';
import { joinStore } from '../../src/store/join.js';
import { hostPlayer } from '../../src/store/player.js';
import type { Cancel } from '../../src/store/types.js';
import { decodeMessage, encodeMessage, MAX_INTEREST_KEYS, type Hex } from '../../src/store/wire.js';

const MAX_EVENT_BYTES = 8104;
const HOST = '00000000000000aa';
const NEAR = '00000000000000b1';
const FAR = '00000000000000b2';
const ALL = '00000000000000b3';
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
type Actions = { move: { input: { readonly id: string; readonly x: number; readonly z: number }; output: { readonly ok: boolean } } };
type Inputs = Record<string, never>;

function record(value: unknown): Record<string, unknown> {
  if (typeof value !== 'object' || value === null) throw new Error('not a record');
  return value as Record<string, unknown>;
}

const world = defineStore<World, Actions, Inputs>({
  id: 'interest-test.world',
  version: 1,
  state: value => {
    const ships: Record<string, Ship> = {};
    for (const [id, ship] of Object.entries(record(record(value).ships))) {
      ships[id] = { x: Number(record(ship).x), z: Number(record(ship).z) };
    }
    return { ships, tick: Number(record(value).tick) };
  },
  empty: () => ({ ships: {}, tick: 0 }),
  visibility: 'open',
  interest: { ships: (ship: Ship) => cellKey(ship.x, ship.z, SIZE) },
  actions: {
    move: {
      input: value => ({ id: String(record(value).id), x: Number(record(value).x), z: Number(record(value).z) }),
      output: value => ({ ok: record(value).ok === true }),
    },
  },
  inputs: {},
});

/** 400 ships, one at the centre of each cell of a 20×20 grid. */
function grid(): World {
  const ships: Record<string, Ship> = {};
  for (let column = 0; column < 20; column += 1) {
    for (let row = 0; row < 20; row += 1) ships[`s${column}_${row}`] = { x: column * SIZE + 5, z: row * SIZE + 5 };
  }
  return { ships, tick: 0 };
}

function serve() {
  const mesh = createLocalMesh();
  const host = hostStore<World, Actions, Inputs>({
    definition: world,
    transport: mesh.node(HOST),
    initialState: grid(),
    maxEventBytes: MAX_EVENT_BYTES,
    schedule: never,
    authorize: () => true,
    actions: {
      move: (input, context) => {
        context.setState({ ships: { ...context.getState().ships, [input.id]: { x: input.x, z: input.z } } });
        return { ok: true };
      },
    },
    inputs: {},
  });
  return { mesh, host };
}

function join(node: LocalNode, interest?: readonly string[]) {
  return joinStore<World, Actions, Inputs>({
    definition: world,
    transport: node,
    host: HOST,
    audience: [],
    key: 'player',
    maxEventBytes: MAX_EVENT_BYTES,
    schedule: never,
    ...(interest === undefined ? {} : { interest }),
  });
}

/** Raw frames, for counting and sizing. */
function frames(node: LocalNode): string[] {
  const seen: string[] = [];
  node.onEvent(event => {
    if (event.payload !== undefined) seen.push(new TextDecoder().decode(event.payload));
  });
  return seen;
}

async function settle(): Promise<void> {
  for (let turn = 0; turn < 60; turn += 1) await Promise.resolve();
}

describe('interest', () => {
  it('delivers only the entities near the player, and far fewer bytes', async () => {
    const { mesh } = serve();
    const nearNode = mesh.node(NEAR);
    const allNode = mesh.node(ALL);
    const nearBytes = frames(nearNode);
    const allBytes = frames(allNode);
    const near = join(nearNode, cellsAround(5, 5, { size: SIZE }));
    const all = join(allNode);
    await Promise.all([near.ready(), all.ready()]);

    expect(Object.keys(near.getState().ships).sort()).toEqual(['s0_0', 's0_1', 's1_0', 's1_1']);
    expect(Object.keys(all.getState().ships)).toHaveLength(400);
    const size = (list: string[]) => list.reduce((total, frame) => total + frame.length, 0);
    expect(size(nearBytes) * 20).toBeLessThan(size(allBytes));
  });

  it('sends nothing for a far change, one entity for a near one, and never forces a resync', async () => {
    const { mesh, host } = serve();
    const nearNode = mesh.node(NEAR);
    const farNode = mesh.node(FAR);
    const near = join(nearNode, cellsAround(5, 5, { size: SIZE }));
    const far = join(farNode, cellsAround(185, 185, { size: SIZE }));
    await Promise.all([near.ready(), far.ready()]);
    const toNear = frames(nearNode);
    const toFar = frames(farNode);

    // Ten changes near the NEAR player; the FAR player hears nothing.
    for (let step = 1; step <= 10; step += 1) {
      host.setState({ ...host.getState(), ships: { ...host.getState().ships, s0_0: { x: 5 + step / 10, z: 5 } } });
    }
    await settle();
    expect(toFar).toEqual([]);
    expect(toNear).toHaveLength(10);
    const ops = toNear.map(frame => JSON.parse(frame).ops);
    expect(ops.every(list => list.length === 1 && list[0].p.join('/') === 'ships/s0_0')).toBe(true);

    // Now a change near FAR: its delta applies on its old base — no gap,
    // no reinstall — even though ten revisions went by in between.
    host.setState({ ...host.getState(), ships: { ...host.getState().ships, s19_19: { x: 196, z: 196 } } });
    await settle();
    expect(toFar.map(frame => JSON.parse(frame).k)).toEqual(['delta']);
    expect(far.getState().ships.s19_19).toEqual({ x: 196, z: 196 });
    expect(far.getStatus().phase).toBe('ready');
  });

  it('adds an entity that moves into interest, and removes one that moves out', async () => {
    const { mesh, host } = serve();
    const near = join(mesh.node(NEAR), cellsAround(5, 5, { size: SIZE }));
    await near.ready();
    await near.act('move', { id: 's9_9', x: 6, z: 6 }); // into 0:0
    await settle();
    expect(near.getState().ships.s9_9).toEqual({ x: 6, z: 6 });
    await near.act('move', { id: 's0_0', x: 150, z: 150 }); // out of range
    await settle();
    expect(near.getState().ships.s0_0).toBeUndefined();
    expect(host.getState().ships.s0_0).toEqual({ x: 150, z: 150 });
  });

  it('moves interest additively: the view never blanks, and only the difference is sent', async () => {
    const { mesh } = serve();
    const node = mesh.node(NEAR);
    const replica = join(node, cellsAround(5, 5, { size: SIZE }));
    await replica.ready();
    const counts: number[] = [];
    replica.subscribe(state => counts.push(Object.keys(state.ships).length));
    const wire = frames(node);

    // One cell east: column 1..2 instead of 0..1.
    await replica.setInterest(cellsAround(15, 5, { size: SIZE }));
    await settle();
    expect(Object.keys(replica.getState().ships).sort()).toEqual(['s0_0', 's0_1', 's1_0', 's1_1', 's2_0', 's2_1']);
    expect(counts.every(count => count > 0)).toBe(true);
    const delta = wire.map(frame => JSON.parse(frame)).find(message => message.k === 'delta');
    expect(delta.ops.map((op: { o: string; p: string[] }) => `${op.o}:${op.p[1]}`).sort()).toEqual(['r:s2_0', 'r:s2_1']);
    expect(wire.map(frame => JSON.parse(frame).k)).not.toContain('man');

    // And back out of column 0: removals only.
    await replica.setInterest(cellsAround(25, 5, { size: SIZE }));
    await settle();
    expect(Object.keys(replica.getState().ships)).not.toContain('s0_0');
    expect(counts.every(count => count > 0)).toBe(true);
  });

  it('carries the interest set on the join frame, and bounds it on the wire', () => {
    const cells = cellsAround(5, 5, { size: SIZE });
    const frame = encodeMessage({
      k: 'join',
      q: '1'.repeat(16) as Hex,
      def: 'd',
      ver: 1,
      store: 'd',
      key: 'k',
      aud: [],
      int: cells,
    });
    const decoded = decodeMessage(frame, { maxBytes: 1 << 20, as: 'owner' });
    expect(decoded.ok && decoded.message.k === 'join' && decoded.message.int).toEqual(cells);
    const tooMany = Array.from({ length: MAX_INTEREST_KEYS + 1 }, (_, i) => `c${i}`);
    expect(() =>
      encodeMessage({ k: 'int', q: '1'.repeat(16) as Hex, h: '2'.repeat(32) as Hex, int: tooMany }),
    ).toThrow();
    expect(() =>
      encodeMessage({ k: 'int', q: '1'.repeat(16) as Hex, h: '2'.repeat(32) as Hex, int: ['x'.repeat(65)] }),
    ).toThrow();
  });

  it("filters the host's own player too", async () => {
    const { host } = serve();
    const me = hostPlayer(host, { audience: [], interest: cellsAround(5, 5, { size: SIZE }) });
    await me.ready();
    expect(Object.keys(me.getState().ships)).toHaveLength(4);
    await me.setInterest(cellsAround(95, 95, { size: SIZE }));
    expect(Object.keys(me.getState().ships)).toHaveLength(9);
  });
});

describe('grid helpers', () => {
  it('names cells and the block around a point', () => {
    expect(cellKey(-0.5, 25, 10)).toBe('-1:2');
    expect(cellsAround(5, 5, { size: 10 })).toHaveLength(9);
    expect(cellsAround(5, 5, { size: 10, radius: 0 })).toEqual(['0:0']);
  });

  it('holds a cell just left until the player is a margin past it', () => {
    const start = cellsAround(5, 5, { size: 10, radius: 0 }); // ['0:0']
    const stepped = stickyCells(start, 12, 5, { size: 10, radius: 0, margin: 1 });
    expect(sameCells(stepped, ['1:0', '0:0'])).toBe(true);
    const gone = stickyCells(stepped, 25, 5, { size: 10, radius: 0, margin: 1 });
    expect(sameCells(gone, ['2:0', '1:0'])).toBe(true);
  });
});
