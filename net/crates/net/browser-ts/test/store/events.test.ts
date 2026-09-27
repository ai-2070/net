/**
 * The `onEvent` hook (join / leave / area) and the inventory bones, in
 * the shape a game uses them together: a starter kit on join, areas by
 * position, potions spent by an action, each player seeing only their
 * own bag.
 */

import { describe, expect, it } from 'vitest';

import { createLocalMesh, type LocalNode } from '../../src/local.js';
import { defineStore } from '../../src/store/definition.js';
import { hostStore } from '../../src/store/host.js';
import {
  addItems,
  countItems,
  emptyInventory,
  hasItems,
  InventoryError,
  inventoryOf,
  onlyOwn,
  parseInventory,
  removeItems,
  type Inventory,
} from '../../src/store/inventory.js';
import { joinStore } from '../../src/store/join.js';
import type { StoreEvent } from '../../src/store/owner.js';
import { hostPlayer } from '../../src/store/player.js';
import type { ActionContext, Cancel } from '../../src/store/types.js';
import { sniff } from './sniff.js';

const MAX_EVENT_BYTES = 8104;
const HOST = '00000000000000aa';
const GUEST = '00000000000000bb';
const rules = { maxKinds: 4, maxCount: { potion: 5 } };

interface World {
  readonly x: Record<string, number>;
  readonly inventories: Record<string, Inventory>;
}
type Actions = {
  walk: { input: { readonly dx: number }; output: { readonly x: number } };
  drink: { input: Record<string, never>; output: { readonly left: number } };
};
type Inputs = Record<string, never>;

function record(value: unknown): Record<string, unknown> {
  if (typeof value !== 'object' || value === null) throw new Error('not a record');
  return value as Record<string, unknown>;
}

const world = defineStore<World, Actions, Inputs>({
  id: 'events-test.world',
  version: 1,
  state: value => {
    const raw = record(value);
    const x: Record<string, number> = {};
    for (const [peer, at] of Object.entries(record(raw.x))) x[peer] = Number(at);
    const inventories: Record<string, Inventory> = {};
    for (const [peer, bag] of Object.entries(record(raw.inventories))) inventories[peer] = parseInventory(bag, rules);
    return { x, inventories };
  },
  empty: () => ({ x: {}, inventories: {} }),
  actions: {
    walk: { input: value => ({ dx: Number(record(value).dx) }), output: value => ({ x: Number(record(value).x) }) },
    drink: { input: () => ({}), output: value => ({ left: Number(record(value).left) }) },
  },
  inputs: {},
});

const never = (): Cancel => () => {};

interface Options {
  readonly hook?: (event: StoreEvent, context: ActionContext<World>) => void;
  readonly areaOf?: (state: World, peer: string) => string | null;
  readonly allow?: { read: boolean };
}

function setup(options: Options = {}) {
  const mesh = createLocalMesh();
  const events: StoreEvent[] = [];
  const host = hostStore<World, Actions, Inputs>({
    definition: world,
    transport: mesh.node(HOST),
    initialState: world.empty(),
    maxEventBytes: MAX_EVENT_BYTES,
    schedule: never,
    authorize: request => request.type !== 'read' || (options.allow?.read ?? true),
    projectFor: (state, { peer }) => ({ ...state, inventories: onlyOwn(state.inventories, peer) }),
    areaOf: options.areaOf ?? ((state, peer) => (state.x[peer] === undefined ? null : state.x[peer]! < 10 ? 'town' : 'forest')),
    onEvent:
      options.hook ??
      ((event, context) => {
        events.push(event);
        if (event.type !== 'join') return;
        const state = context.getState();
        context.setState({
          x: { ...state.x, [event.peer]: 0 },
          inventories: { ...state.inventories, [event.peer]: addItems(emptyInventory(), 'potion', 3, rules) },
        });
      }),
    actions: {
      walk: (input, context) => {
        const x = (context.getState().x[context.peer] ?? 0) + input.dx;
        context.setState({ x: { ...context.getState().x, [context.peer]: x } });
        return { x };
      },
      drink: (_input, context) => {
        const inventories = { ...context.getState().inventories };
        inventories[context.peer] = removeItems(inventoryOf(inventories, context.peer), 'potion', 1);
        context.setState({ inventories });
        return { left: countItems(inventories[context.peer], 'potion') };
      },
    },
    inputs: {},
  });
  return { mesh, host, events };
}

function join(node: LocalNode) {
  return joinStore<World, Actions, Inputs>({
    definition: world,
    transport: node,
    host: HOST,
    audience: [],
    key: 'player',
    maxEventBytes: MAX_EVENT_BYTES,
    schedule: never,
  });
}

async function settle(): Promise<void> {
  for (let turn = 0; turn < 40; turn += 1) await Promise.resolve();
}

describe('onEvent', () => {
  it('runs on join as a transaction: the starter kit reaches the player, and only them', async () => {
    const { mesh, host, events } = setup();
    const guestNode = mesh.node(GUEST);
    const wire = sniff(guestNode);
    const me = hostPlayer(host, { audience: [] });
    const guest = join(guestNode);
    await Promise.all([me.ready(), guest.ready()]);
    await settle();

    expect(events.map(event => [event.type, event.peer])).toEqual([
      ['join', HOST],
      ['area', HOST],
      ['join', GUEST],
      ['area', GUEST],
    ]);
    expect(events[1]).toEqual({ type: 'area', peer: HOST, from: null, to: 'town' });
    expect(guest.getState().inventories).toEqual({ [GUEST]: { potion: 3 } });
    expect(me.getState().inventories).toEqual({ [HOST]: { potion: 3 } });
    expect(Object.keys(host.getState().inventories).sort()).toEqual([HOST, GUEST]);
    // The sniff can see an inventory — the guest's own — and never the host's.
    expect(wire.text()).toContain(`"${GUEST}":{"potion":3}`);
    expect(wire.text()).not.toContain(`"${HOST}":{"potion`);
  });

  it('fires area only when the area changes', async () => {
    const { mesh, events } = setup();
    const guest = join(mesh.node(GUEST));
    await guest.ready();
    await settle();
    events.length = 0;
    await guest.act('walk', { dx: 4 }); // 4: still town
    await guest.act('walk', { dx: 8 }); // 12: forest
    await guest.act('walk', { dx: 1 }); // 13: forest
    await settle();
    expect(events).toEqual([{ type: 'area', peer: GUEST, from: 'town', to: 'forest' }]);
  });

  it('fires leave with the reason: left, or refused when the policy turns them away', async () => {
    const allow = { read: true };
    const { mesh, host, events } = setup({ allow });
    const guest = join(mesh.node(GUEST));
    await guest.ready();
    await guest.close();
    await settle();
    expect(events.at(-1)).toEqual({ type: 'leave', peer: GUEST, reason: 'left' });

    const again = join(mesh.node('00000000000000cc'));
    await again.ready();
    allow.read = false;
    host.setState({ ...host.getState() }); // an equivalent commit changes nothing…
    host.setState({ ...host.getState(), x: { ...host.getState().x, nobody: 1 } }); // …a real one re-checks
    await settle();
    expect(events.at(-1)).toEqual({ type: 'leave', peer: '00000000000000cc', reason: 'refused' });
  });

  it("fires join and leave for the host's own player", async () => {
    const { host, events } = setup();
    const me = hostPlayer(host, { audience: [] });
    await me.close();
    await settle();
    expect(events.filter(event => event.type !== 'area').map(event => event.type)).toEqual(['join', 'leave']);
    expect(events.at(-1)).toEqual({ type: 'leave', peer: HOST, reason: 'left' });
  });

  it('discards a throwing hook’s writes and counts it', async () => {
    const { mesh, host } = setup({
      hook: (_event, context) => {
        context.setState({ x: { poisoned: 1 } });
        throw new Error('boom');
      },
    });
    const guest = join(mesh.node(GUEST));
    await guest.ready();
    await settle();
    expect(host.getState().x).toEqual({});
    expect(host.counters()['event-rejected']).toBeGreaterThan(0);
  });

  it('stops two hooks ping-ponging instead of looping forever', async () => {
    const { mesh, host } = setup({
      hook: (event, context) => {
        // Every area change teleports the player to the other area.
        if (event.type === 'area' || event.type === 'join') {
          const x = context.getState().x[event.peer] ?? 0;
          context.setState({ x: { ...context.getState().x, [event.peer]: x < 10 ? 20 : 0 } });
        }
      },
    });
    const guest = join(mesh.node(GUEST));
    await guest.ready();
    await settle();
    expect(host.counters()['event-rounds-bound']).toBe(1);
  });
});

describe('inventory', () => {
  it('adds, counts and removes whole stacks', () => {
    let bag = emptyInventory();
    bag = addItems(bag, 'potion', 2, rules);
    bag = addItems(bag, 'potion', 3, rules);
    expect(countItems(bag, 'potion')).toBe(5);
    expect(hasItems(bag, 'potion', 5)).toBe(true);
    bag = removeItems(bag, 'potion', 5);
    expect(bag).toEqual({});
    expect(countItems(bag, 'potion')).toBe(0);
    expect(Object.isFrozen(bag)).toBe(true);
  });

  it('refuses what breaks a rule, changing nothing', () => {
    const bag = addItems(emptyInventory(), 'potion', 5, rules);
    expect(() => addItems(bag, 'potion', 1, rules)).toThrow(expect.objectContaining({ code: 'full' }));
    expect(() => removeItems(bag, 'potion', 6)).toThrow(expect.objectContaining({ code: 'insufficient' }));
    let full = emptyInventory();
    for (const item of ['a', 'b', 'c', 'd']) full = addItems(full, item, 1, rules);
    expect(() => addItems(full, 'e', 1, rules)).toThrow(expect.objectContaining({ code: 'full' }));
    expect(addItems(full, 'a', 1, rules).a).toBe(2); // a kind it already has is fine
    for (const bad of [0, -1, 1.5, Number.NaN]) expect(() => addItems(bag, 'x', bad)).toThrow(InventoryError);
    expect(() => addItems(bag, '', 1)).toThrow(InventoryError);
    expect(() => addItems(bag, '__proto__', 1)).toThrow(InventoryError);
    expect(bag).toEqual({ potion: 5 });
  });

  it('validates an inventory in a store document', () => {
    expect(parseInventory({ potion: 2 }, rules)).toEqual({ potion: 2 });
    expect(() => parseInventory({ potion: 9 }, rules)).toThrow(InventoryError);
    expect(() => parseInventory({ potion: '2' })).toThrow(InventoryError);
    expect(() => parseInventory([])).toThrow(InventoryError);
  });

  it('refuses an action that spends what the player does not have, with no partial write', async () => {
    const { mesh, host } = setup();
    const guest = join(mesh.node(GUEST));
    await guest.ready();
    await settle();
    for (const left of [2, 1, 0]) await expect(guest.act('drink', {})).resolves.toEqual({ left });
    await expect(guest.act('drink', {})).rejects.toMatchObject({ code: 'action-rejected' });
    expect(host.getState().inventories[GUEST]).toEqual({});
  });
});
