/**
 * The sparse per-view path: when a commit changes only collections
 * declared in both `interest` and `entities`, and the host projects only
 * by declared rules, each view projects just the changed entities.
 *
 * The property: every replica ends up with EXACTLY what the full path
 * would give it — the host's document, projected for that player by the
 * declared rules, narrowed to its interest — through a long random run of
 * moves, arrivals, departures and secret changes. And the sparse path
 * must actually have run, or the property says nothing about it.
 */

import { describe, expect, it } from 'vitest';

import { createLocalMesh, type LocalNode } from '../../src/local.js';
import { defineStore } from '../../src/store/definition.js';
import { hostStore } from '../../src/store/host.js';
import { cellKey, cellsAround } from '../../src/store/interest.js';
import { joinStore, type JoinedStoreHandle } from '../../src/store/join.js';
import type { Cancel } from '../../src/store/types.js';
import { hiddenOr, projectVisible, type Hidden } from '../../src/store/visibility.js';

const MAX_EVENT_BYTES = 8104;
const HOST = '00000000000000aa';
const PLAYERS = ['00000000000000b1', '00000000000000b2', '00000000000000b3', '00000000000000b4'];
const SIZE = 10;
const never = (): Cancel => () => {};

interface Ship {
  readonly x: number;
  readonly z: number;
  readonly cargo: number | Hidden;
}
interface Mine {
  readonly x: number;
  readonly z: number;
}
interface World {
  readonly ships: Record<string, Ship>;
  readonly mines: Record<string, Mine>;
  readonly tick: number;
}
type Actions = Record<string, never>;
type Inputs = Record<string, never>;

function record(value: unknown): Record<string, unknown> {
  if (typeof value !== 'object' || value === null) throw new Error('not a record');
  return value as Record<string, unknown>;
}
const parseShip = (value: unknown): Ship => ({
  x: Number(record(value).x),
  z: Number(record(value).z),
  cargo: hiddenOr(v => Number(v))(record(value).cargo),
});
const parseMine = (value: unknown): Mine => ({ x: Number(record(value).x), z: Number(record(value).z) });

function mapOf<T>(value: unknown, parse: (entity: unknown) => T): Record<string, T> {
  const out: Record<string, T> = {};
  for (const [id, entity] of Object.entries(record(value))) out[id] = parse(entity);
  return out;
}

const world = defineStore<World, Actions, Inputs>({
  id: 'sparse-test.world',
  version: 1,
  state: value => ({
    ships: mapOf(record(value).ships, parseShip),
    // Hidden whole from everyone but the `gm` audience: the rule replaces
    // the collection with the marker. Hidden on both sides of a commit, so
    // the full path sends those viewers nothing, and so must this one.
    mines: record(value).mines === undefined ? {} : isHiddenMap(record(value).mines),
    tick: Number(record(value).tick),
  }),
  empty: () => ({ ships: {}, mines: {}, tick: 0 }),
  visibility: {
    'ships.*.cargo': 'owner',
    'ships.*': ['crew', 'gm'],
    mines: ['gm'],
  },
  interest: {
    ships: (ship: Ship) => cellKey(ship.x, ship.z, SIZE),
    mines: (mine: Mine) => cellKey(mine.x, mine.z, SIZE),
  },
  entities: { ships: parseShip, mines: parseMine },
  actions: {},
  inputs: {},
});

function isHiddenMap(value: unknown): Record<string, Mine> {
  // The hidden marker in place of the whole collection reads as no mines.
  if (typeof value === 'object' && value !== null && '$hidden' in value) return {};
  return mapOf(value, parseMine);
}

function mulberry(seed: number): () => number {
  let a = seed;
  return () => {
    a |= 0;
    a = (a + 0x6d2b79f5) | 0;
    let t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

async function settle(): Promise<void> {
  for (let turn = 0; turn < 80; turn += 1) await Promise.resolve();
}

/** What the full path gives `peer`: the declared projection, narrowed to `interest`. */
function expected(state: World, peer: string, audience: readonly string[], interest: readonly string[]): unknown {
  const view = projectVisible(world, state, { peer, audience }) as unknown as Record<string, unknown>;
  const wanted = new Set(interest);
  const narrow = <T extends { x: number; z: number }>(entities: unknown): Record<string, T> => {
    if (typeof entities !== 'object' || entities === null || '$hidden' in entities) return {};
    const out: Record<string, T> = {};
    for (const [id, entity] of Object.entries(entities as Record<string, T>)) {
      if (wanted.has(cellKey(entity.x, entity.z, SIZE))) out[id] = entity;
    }
    return out;
  };
  return JSON.parse(
    JSON.stringify({ ships: narrow(view.ships), mines: narrow(view.mines), tick: view.tick }),
  ) as unknown;
}

describe('sparse per-view projection', () => {
  it('gives every replica exactly what the full path would, through a random run', async () => {
    const random = mulberry(7);
    const ships: Record<string, Ship> = {};
    // Every player owns one ship (its id is the player's peer, which is
    // what the `owner` rule captures); forty more belong to nobody.
    for (const peer of PLAYERS) ships[peer] = { x: 25, z: 25, cargo: 1 };
    for (let i = 0; i < 40; i += 1) ships[`n${i}`] = { x: random() * 50, z: random() * 50, cargo: i };
    const mines: Record<string, Mine> = { m0: { x: 22, z: 22 }, m1: { x: 45, z: 5 } };

    const mesh = createLocalMesh();
    const host = hostStore<World, Actions, Inputs>({
      definition: world,
      transport: mesh.node(HOST),
      initialState: { ships, mines, tick: 0 },
      maxEventBytes: MAX_EVENT_BYTES,
      schedule: never,
      authorize: () => true,
      actions: {},
      inputs: {},
    });
    const audiences: readonly (readonly string[])[] = [['crew'], ['crew'], ['gm'], []];
    const interest = cellsAround(25, 25, { size: SIZE, radius: 1 });
    const replicas: JoinedStoreHandle<World, Actions, Inputs>[] = PLAYERS.map((peer, index) =>
      joinStore<World, Actions, Inputs>({
        definition: world,
        transport: mesh.node(peer) as LocalNode,
        host: HOST,
        audience: [...audiences[index]!],
        key: 'player',
        maxEventBytes: MAX_EVENT_BYTES,
        schedule: never,
        interest,
      }),
    );
    await Promise.all(replicas.map(replica => replica.ready()));

    for (let step = 0; step < 150; step += 1) {
      const state = host.getState();
      const changes: Record<string, Ship | undefined> = {};
      const ids = Object.keys(state.ships);
      for (let k = 0; k < 6; k += 1) {
        const id = ids[Math.floor(random() * ids.length)]!;
        const was = changes[id] ?? state.ships[id];
        const roll = random();
        if (roll < 0.08 && !PLAYERS.includes(id)) changes[id] = undefined;
        else if (roll < 0.2) changes[id] = { ...was!, cargo: Math.floor(random() * 100) };
        else if (was !== undefined) {
          changes[id] = { ...was, x: Math.max(0, Math.min(50, was.x + (random() - 0.5) * 8)), z: was.z };
        }
      }
      if (random() < 0.15) changes[`a${step}`] = { x: random() * 50, z: random() * 50, cargo: step };
      host.setEntities('ships', changes);
      if (random() < 0.1) host.setEntity('mines', `m${step}`, { x: random() * 50, z: random() * 50 });
      await settle();

      const now = host.getState();
      PLAYERS.forEach((peer, index) => {
        const got = JSON.parse(JSON.stringify(replicas[index]!.getState())) as unknown;
        expect(got, `step ${step}, player ${index}`).toEqual(expected(now, peer, audiences[index]!, interest));
      });
    }

    expect(host.counts().sparseViews).toBeGreaterThan(100);
    expect(host.counters()).toEqual({});
    expect(replicas.every(replica => replica.getStatus().phase === 'ready')).toBe(true);
  });
});
