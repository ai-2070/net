/**
 * `projectFor` — a projection that knows which player it is for.
 *
 * The claim that matters is about the WIRE, not the replica's state: a
 * player's secret must never be in a frame sent to another player. So
 * each row that makes it sniffs every byte a node receives.
 */

import { describe, expect, it } from 'vitest';

import { createLocalMesh, type LocalNode } from '../../src/local.js';
import { defineStore } from '../../src/store/definition.js';
import { StoreError } from '../../src/store/errors.js';
import { hostStore, type HostStoreOptions } from '../../src/store/host.js';
import { joinStore } from '../../src/store/join.js';
import { hostPlayer } from '../../src/store/player.js';
import type { Cancel } from '../../src/store/types.js';

const MAX_EVENT_BYTES = 8104;
const HOST = '00000000000000aa';
const ALICE = '00000000000000b1';
const BOB = '00000000000000b2';

interface Table {
  /** Each player's hand, keyed by peer. A viewer sees only their own. */
  readonly hands: Record<string, readonly string[]>;
  readonly pot: number;
}

type Actions = { draw: { input: { readonly card: string }; output: { readonly size: number } } };
type Inputs = Record<string, never>;

function record(value: unknown): Record<string, unknown> {
  if (typeof value !== 'object' || value === null) throw new Error('not a record');
  return value as Record<string, unknown>;
}

const table = defineStore<Table, Actions, Inputs>({
  id: 'project-for.table',
  version: 1,
  state: value => {
    const raw = record(value);
    const hands: Record<string, string[]> = {};
    for (const [peer, cards] of Object.entries(record(raw.hands))) {
      hands[peer] = (cards as unknown[]).map(String);
    }
    return { hands, pot: Number(raw.pot) };
  },
  empty: () => ({ hands: {}, pot: 0 }),
  actions: {
    draw: {
      input: value => ({ card: String(record(value).card) }),
      output: value => ({ size: Number(record(value).size) }),
    },
  },
  inputs: {},
});

const never = (): Cancel => () => {};
const decoder = new TextDecoder();

/** Every frame a node receives, as text. */
function sniff(node: LocalNode): string[] {
  const seen: string[] = [];
  node.onEvent(event => {
    if (event.payload !== undefined) seen.push(decoder.decode(event.payload));
  });
  return seen;
}

function host(mesh: ReturnType<typeof createLocalMesh>, calls: { peer: string; audience: readonly string[] }[]) {
  return hostStore<Table, Actions, Inputs>({
    definition: table,
    transport: mesh.node(HOST),
    initialState: {
      hands: { [HOST]: ['host-card'], [ALICE]: ['alice-ace'], [BOB]: ['bob-king'] },
      pot: 10,
    },
    maxEventBytes: MAX_EVENT_BYTES,
    schedule: never,
    authorize: () => true,
    projectFor: (state, viewer) => {
      calls.push(viewer);
      const own = state.hands[viewer.peer];
      return { ...state, hands: own === undefined ? {} : { [viewer.peer]: own } };
    },
    actions: {
      draw: (input, context) => {
        const hands = { ...context.getState().hands };
        hands[context.peer] = [...(hands[context.peer] ?? []), input.card];
        context.setState({ hands });
        return { size: hands[context.peer]!.length };
      },
    },
    inputs: {},
  });
}

function join(node: LocalNode) {
  return joinStore<Table, Actions, Inputs>({
    definition: table,
    transport: node,
    host: HOST,
    audience: ['table'],
    key: 'player',
    maxEventBytes: MAX_EVENT_BYTES,
    schedule: never,
  });
}

async function settle(): Promise<void> {
  for (let turn = 0; turn < 30; turn += 1) await Promise.resolve();
}

describe('projectFor', () => {
  it('shows each player their own hand, and never sends one to another player', async () => {
    const mesh = createLocalMesh();
    const calls: { peer: string; audience: readonly string[] }[] = [];
    const store = host(mesh, calls);
    const aliceNode = mesh.node(ALICE);
    const bobNode = mesh.node(BOB);
    const toAlice = sniff(aliceNode);
    const toBob = sniff(bobNode);
    const alice = join(aliceNode);
    const bob = join(bobNode);
    await Promise.all([alice.ready(), bob.ready()]);

    expect(alice.getState().hands).toEqual({ [ALICE]: ['alice-ace'] });
    expect(bob.getState().hands).toEqual({ [BOB]: ['bob-king'] });
    expect(alice.getState().pot).toBe(10);

    // The viewer is the authenticated peer, in `context.peer`'s spelling.
    expect(calls.map(call => call.peer)).toEqual(expect.arrayContaining([ALICE, BOB]));
    expect(calls.every(call => Object.isFrozen(call) && Object.isFrozen(call.audience))).toBe(true);

    await expect(alice.act('draw', { card: 'alice-queen' })).resolves.toEqual({ size: 2 });
    await settle();
    expect(alice.getState().hands).toEqual({ [ALICE]: ['alice-ace', 'alice-queen'] });
    expect(store.getState().hands[ALICE]).toEqual(['alice-ace', 'alice-queen']);

    const bobWire = toBob.join('\n');
    const aliceWire = toAlice.join('\n');
    expect(bobWire).not.toContain('alice-');
    expect(bobWire).not.toContain('host-card');
    expect(aliceWire).not.toContain('bob-king');
    expect(aliceWire).toContain('alice-queen');
  });

  it("sends nothing to a player whose view did not change", async () => {
    const mesh = createLocalMesh();
    host(mesh, []);
    const aliceNode = mesh.node(ALICE);
    const bobNode = mesh.node(BOB);
    const alice = join(aliceNode);
    const bob = join(bobNode);
    await Promise.all([alice.ready(), bob.ready()]);
    await settle();

    const toBob = sniff(bobNode);
    await alice.act('draw', { card: 'alice-queen' });
    await settle();
    // Alice's hand is not in Bob's view, so Bob's projection did not
    // change — and a per-audience diff would have sent him a delta
    // computed from someone else's view.
    expect(toBob).toEqual([]);
  });

  it("gives the host's own player its own hand", async () => {
    const mesh = createLocalMesh();
    const store = host(mesh, []);
    const me = hostPlayer(store, { audience: ['table'] });
    await me.ready();
    expect(me.getState().hands).toEqual({ [HOST]: ['host-card'] });
  });

  it('refuses a host with both projections or neither', () => {
    const mesh = createLocalMesh();
    const base = {
      definition: table,
      initialState: table.empty(),
      maxEventBytes: MAX_EVENT_BYTES,
      schedule: never,
      authorize: () => true,
      actions: { draw: () => ({ size: 0 }) },
      inputs: {},
    };
    const both = {
      ...base,
      transport: mesh.node(),
      project: (state: Table) => state,
      projectFor: (state: Table) => state,
    } as unknown as HostStoreOptions<Table, Actions, Inputs>;
    const neither = { ...base, transport: mesh.node() } as unknown as HostStoreOptions<Table, Actions, Inputs>;
    expect(() => hostStore(both)).toThrow(StoreError);
    expect(() => hostStore(neither)).toThrow(StoreError);
  });

  it('keeps `project` shared per audience: one projection pair per change, not one per player', async () => {
    const mesh = createLocalMesh();
    let projections = 0;
    const store = hostStore<Table, Actions, Inputs>({
      definition: table,
      transport: mesh.node(HOST),
      initialState: { hands: {}, pot: 0 },
      maxEventBytes: MAX_EVENT_BYTES,
      schedule: never,
      authorize: () => true,
      project: state => {
        projections += 1;
        return state;
      },
      actions: { draw: () => ({ size: 0 }) },
      inputs: {},
    });
    const alice = join(mesh.node(ALICE));
    const bob = join(mesh.node(BOB));
    await Promise.all([alice.ready(), bob.ready()]);
    projections = 0;
    store.setState({ hands: {}, pot: 5 });
    // Before and after, once, for the one audience both players read.
    expect(projections).toBe(2);
  });
});
