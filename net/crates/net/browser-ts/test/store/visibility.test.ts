/**
 * Declarative visibility.
 *
 * The acceptance row from the lobbies plan: a card game — hands
 * `owner`, deck `nobody` with its count `everyone`, table `everyone` —
 * written with NO hand-written projection, where each player receives
 * only their own hand, proved on the wire.
 */

import { describe, expect, it } from 'vitest';

import { createLocalMesh, type LocalNode } from '../../src/local.js';
import { defineStore } from '../../src/store/definition.js';
import { StoreError } from '../../src/store/errors.js';
import { hostStore } from '../../src/store/host.js';
import { joinStore } from '../../src/store/join.js';
import { hostPlayer } from '../../src/store/player.js';
import type { ActionContext, Cancel } from '../../src/store/types.js';
import { sniff } from './sniff.js';
import {
  assertHidden,
  HIDDEN,
  hiddenOr,
  isHidden,
  projectVisible,
  type Hidden,
  type Visibility,
} from '../../src/store/visibility.js';

const MAX_EVENT_BYTES = 8104;
const HOST = '00000000000000aa';
const ALICE = '00000000000000b1';
const BOB = '00000000000000b2';
const never = (): Cancel => () => {};

interface Table {
  readonly players: Record<string, { readonly hand: readonly string[] | Hidden; readonly score: number }>;
  readonly deck: readonly (string | Hidden)[] | Hidden;
  readonly table: readonly string[];
}
type Actions = { draw: { input: Record<string, never>; output: { readonly drew: string } } };
type Inputs = Record<string, never>;

function record(value: unknown): Record<string, unknown> {
  if (typeof value !== 'object' || value === null) throw new Error('not a record');
  return value as Record<string, unknown>;
}
const cards = (value: unknown): string[] => {
  if (!Array.isArray(value)) throw new Error('not cards');
  return value.map(String);
};

function tableDefinition(visibility: Visibility | undefined, strict = false) {
  return defineStore<Table, Actions, Inputs>({
    id: 'visibility-test.table',
    version: 1,
    state: value => {
      const raw = record(value);
      const players: Record<string, { hand: readonly string[] | Hidden; score: number }> = {};
      for (const [peer, player] of Object.entries(record(raw.players))) {
        const p = record(player);
        players[peer] = { hand: strict ? cards(p.hand) : hiddenOr(cards)(p.hand), score: Number(p.score) };
      }
      const deck = hiddenOr(value => (value as unknown[]).map(card => hiddenOr(String)(card)))(raw.deck);
      return { players, deck, table: cards(raw.table) };
    },
    empty: () => ({ players: {}, deck: [], table: [] }),
    actions: { draw: { input: () => ({}), output: value => ({ drew: String(record(value).drew) }) } },
    inputs: {},
    ...(visibility === undefined ? {} : { visibility }),
  });
}

const dealt: Table = {
  players: {
    [HOST]: { hand: ['host-7'], score: 0 },
    [ALICE]: { hand: ['alice-ace', 'alice-2'], score: 3 },
    [BOB]: { hand: ['bob-king'], score: 1 },
  },
  deck: ['deck-q', 'deck-j', 'deck-10'],
  table: ['table-5'],
};

function serve(definition: ReturnType<typeof tableDefinition>, extra: Record<string, unknown> = {}) {
  const mesh = createLocalMesh();
  const host = hostStore<Table, Actions, Inputs>({
    definition,
    transport: mesh.node(HOST),
    initialState: dealt,
    maxEventBytes: MAX_EVENT_BYTES,
    schedule: never,
    authorize: () => true,
    actions: {
      draw: (_input: unknown, context: ActionContext<Table>) => {
        const state = context.getState();
        const deck = state.deck as string[];
        const drew = deck[0]!;
        const me = state.players[context.peer]!;
        context.setState({
          deck: deck.slice(1),
          players: { ...state.players, [context.peer]: { ...me, hand: [...(me.hand as string[]), drew] } },
        });
        return { drew };
      },
    },
    inputs: {},
    ...extra,
  } as never);
  return { mesh, host };
}

function sniffingJoin(node: LocalNode, definition: ReturnType<typeof tableDefinition>) {
  const wire = sniff(node);
  const replica = joinStore<Table, Actions, Inputs>({
    definition,
    transport: node,
    host: HOST,
    audience: [],
    key: 'player',
    maxEventBytes: MAX_EVENT_BYTES,
    schedule: never,
  });
  return { replica, wire };
}

async function settle(): Promise<void> {
  for (let turn = 0; turn < 40; turn += 1) await Promise.resolve();
}

describe('visibility: the card game, with no hand-written projection', () => {
  it('gives each player their own hand, the deck as a count, and the table — on the wire', async () => {
    const definition = tableDefinition('card-game');
    const { mesh, host } = serve(definition);
    const alice = sniffingJoin(mesh.node(ALICE), definition);
    const bob = sniffingJoin(mesh.node(BOB), definition);
    await Promise.all([alice.replica.ready(), bob.replica.ready()]);

    const view = alice.replica.getState();
    expect(view.players[ALICE]?.hand).toEqual(['alice-ace', 'alice-2']);
    expect(isHidden(view.players[BOB]?.hand)).toBe(true);
    expect(view.players[BOB]?.score).toBe(1); // unlisted: visible
    expect(Array.isArray(view.deck) && view.deck.length === 3 && view.deck.every(isHidden)).toBe(true);
    expect(view.table).toEqual(['table-5']);

    await alice.replica.act('draw', {});
    await settle();
    expect(alice.replica.getState().players[ALICE]?.hand).toEqual(['alice-ace', 'alice-2', 'deck-q']);
    expect((bob.replica.getState().deck as unknown[]).length).toBe(2);

    const toBob = bob.wire.text();
    const toAlice = alice.wire.text();
    // Positive control: the sniff sees Bob's own hand.
    expect(toBob).toContain('bob-king');
    for (const secret of ['alice-ace', 'alice-2', 'host-7', 'deck-q', 'deck-j', 'deck-10']) {
      expect(toBob).not.toContain(secret);
    }
    expect(toAlice).not.toContain('bob-king');
    expect(toAlice).not.toContain('deck-j');
    // The host holds everything.
    expect(host.getState().deck).toEqual(['deck-j', 'deck-10']);
  });

  it("shows the host's own player its hand and nobody else's", async () => {
    const definition = tableDefinition('card-game');
    const { host } = serve(definition);
    const me = hostPlayer(host, { audience: [] });
    await me.ready();
    expect(me.getState().players[HOST]?.hand).toEqual(['host-7']);
    expect(isHidden(me.getState().players[ALICE]?.hand)).toBe(true);
  });

  it('proves it with assertHidden, and assertHidden catches a leak', () => {
    const definition = tableDefinition('card-game');
    const alice = { peer: ALICE, audience: [] };
    assertHidden(definition, dealt, alice, [`players.${BOB}.hand`, `players.${HOST}.hand`, 'deck']);
    expect(() => assertHidden(definition, dealt, alice, [`players.${ALICE}.hand`])).toThrow(/visible/);
    // Without the rules, the same assertion fails: the rules are what hide it.
    expect(() => assertHidden(tableDefinition('open'), dealt, alice, [`players.${BOB}.hand`])).toThrow();
  });
});

describe('visibility rules', () => {
  const state = {
    ships: { a: { x: 1, secret: 's-a' }, b: { x: 2, secret: 's-b' } },
    waypoint: { x: 9 },
    log: ['one', 'two'],
  };
  const view = (visibility: Visibility, peer = 'a', audience: string[] = []) =>
    projectVisible({ visibility }, state, { peer, audience });

  it('removes hidden collection entries and marks hidden fields', () => {
    expect(view({ 'ships.*': 'owner' }).ships).toEqual({ a: state.ships.a });
    expect(view({ 'ships.*.secret': 'owner' }).ships).toEqual({
      a: { x: 1, secret: 's-a' },
      b: { x: 2, secret: HIDDEN },
    });
    expect(view({ 'log.*': 'nobody' }).log).toEqual([]);
    expect(view({ waypoint: 'nobody' }).waypoint).toBe(HIDDEN);
  });

  it('grants by audience', () => {
    expect(view({ waypoint: ['command'] }, 'a', ['crew']).waypoint).toBe(HIDDEN);
    expect(view({ waypoint: ['command'] }, 'a', ['crew', 'command']).waypoint).toEqual({ x: 9 });
  });

  it('never lets a deeper rule re-open what a shallower one hid', () => {
    expect(view({ ships: 'nobody', 'ships.*.x': 'everyone' }).ships).toBe(HIDDEN);
  });

  it('leaves a hidden parent alone when deeper rules also hide', () => {
    // A second rule inside a hidden field must not write into the marker…
    expect(view({ ships: 'nobody', 'ships.*.secret': 'nobody' }).ships).toBe(HIDDEN);
    // …nor resurrect a removed entry as a stub.
    expect(view({ 'ships.*': 'owner', 'ships.*.secret': 'nobody' }).ships).toEqual({
      a: { x: 1, secret: HIDDEN },
    });
  });

  it('takes a preset with overrides', () => {
    const definition = { visibility: { preset: 'card-game', 'players.*.score': 'nobody' } as Visibility };
    const seen = projectVisible(definition, dealt, { peer: ALICE, audience: [] });
    expect(seen.players[ALICE]?.score).toBe(HIDDEN);
    expect(isHidden(seen.players[BOB]?.hand)).toBe(true);
  });

  it('refuses at definition time what it could not enforce', () => {
    for (const visibility of [
      { 'ships..x': 'everyone' },
      { hand: 'owner' },
      { hand: 'friends' },
      { hand: [] },
      'poker',
      { preset: 'poker' },
    ]) {
      expect(() => tableDefinition(visibility as Visibility)).toThrow(StoreError);
    }
  });
});

describe('visibility on the host', () => {
  it('applies after a hand-written projection, which cannot widen it', async () => {
    const definition = tableDefinition('card-game');
    const { mesh } = serve(definition, { projectFor: (state: Table) => state });
    const bob = sniffingJoin(mesh.node(BOB), definition);
    await bob.replica.ready();
    expect(bob.wire.text()).toContain('bob-king');
    expect(bob.wire.text()).not.toContain('alice-ace');
  });

  it('refuses a host whose validator rejects the hidden marker', () => {
    expect(() => serve(tableDefinition('card-game', true))).toThrow(/hiddenOr/);
  });

  it('refuses a host with no projection and no declared visibility', () => {
    expect(() => serve(tableDefinition(undefined))).toThrow(StoreError);
  });

  it('warns in development when everyone gets everything unannounced — and not once it is declared open', async () => {
    const run = async (definition: ReturnType<typeof tableDefinition>, extra: Record<string, unknown>) => {
      const warnings: string[] = [];
      const { mesh } = serve(definition, { dev: (message: string) => warnings.push(message), ...extra });
      await sniffingJoin(mesh.node(BOB), definition).replica.ready();
      return warnings;
    };
    const loud = await run(tableDefinition(undefined), { project: (state: Table) => state });
    expect(loud).toHaveLength(1);
    expect(loud[0]).toMatch(/visibility: 'open'/);
    expect(await run(tableDefinition('open'), {})).toEqual([]);
  });
});
