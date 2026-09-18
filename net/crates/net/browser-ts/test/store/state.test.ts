/**
 * Reference stability, which is a promise `getState()` makes and a
 * naive apply breaks silently.
 *
 * The four witnesses the reviewer required (design §7 slice 1):
 * updating one entity preserves unrelated entity references; an
 * equivalent resynchronization snapshot preserves the root reference;
 * changing part of a snapshot preserves unchanged subtrees; and an
 * unchanged selector result does not notify.
 *
 * The first three are this file. The selector one needs subscriptions,
 * so it lives in `core.test.ts` beside the other notification counts.
 */

import { describe, expect, it } from 'vitest';

import { mergeShallow, reconcile } from '../../src/store/state.js';

interface Player {
  readonly pos: { readonly x: number; readonly y: number };
  readonly name: string;
}
interface World {
  readonly players: Record<string, Player>;
  readonly tick: number;
}

const WORLD: World = {
  players: {
    a: { pos: { x: 1, y: 2 }, name: 'ann' },
    b: { pos: { x: 3, y: 4 }, name: 'bob' },
  },
  tick: 7,
};

describe('reconcile', () => {
  it('preserves unrelated entity references when one entity changes', () => {
    // `initial` is built from a CLONE, so its nodes are not the same
    // objects `moved` carries. Sharing is then the only way
    // `initial.players.b === next.players.b` can hold — without it the
    // assertion passes trivially, because `reconcile` would hand back
    // the very object it was given. Found by running the inverse.
    const initial = reconcile(undefined, JSON.parse(JSON.stringify(WORLD)) as World);
    const moved: World = {
      ...WORLD,
      players: {
        ...WORLD.players,
        a: { pos: { x: 9, y: 2 }, name: 'ann' },
      },
    };
    const next = reconcile(initial, moved);

    expect(next).not.toBe(initial);
    // The entity that moved is a new object...
    expect(next.players.a).not.toBe(initial.players.a);
    // ...and the one that did not is the SAME object, which is what
    // lets a render loop skip it by reference.
    expect(next.players.b).toBe(initial.players.b);
    // Its unchanged nested subtree is shared too, not just the record.
    expect(next.players.a?.pos).not.toBe(initial.players.a?.pos);
    expect(next.players.b?.pos).toBe(initial.players.b?.pos);
  });

  it('preserves the root reference for an equivalent resynchronization snapshot', () => {
    const initial = reconcile(undefined, WORLD);
    // A resync delivers a structurally identical, freshly parsed value —
    // exactly what a validator returns. Nothing changed, so nothing
    // may look changed.
    const resynced = reconcile(initial, JSON.parse(JSON.stringify(WORLD)) as World);

    expect(resynced).toBe(initial);
  });

  it('preserves unchanged subtrees when part of a snapshot changes', () => {
    const initial = reconcile(undefined, WORLD);
    const changed = reconcile(initial, { ...WORLD, tick: 8 });

    expect(changed).not.toBe(initial);
    expect(changed.tick).toBe(8);
    // `players` was untouched by a scalar change at the root.
    expect(changed.players).toBe(initial.players);
  });

  it('shares unchanged array elements and keeps an equal array identical', () => {
    const initial = reconcile(undefined, { items: [{ v: 1 }, { v: 2 }] });
    const equal = reconcile(initial, { items: [{ v: 1 }, { v: 2 }] });
    expect(equal).toBe(initial);

    const grown = reconcile(initial, { items: [{ v: 1 }, { v: 2 }, { v: 3 }] });
    expect(grown.items).toHaveLength(3);
    // Length changed, so the array is new — but its surviving elements
    // are not rebuilt.
    expect(grown.items[0]).toBe(initial.items[0]);
    expect(grown.items[1]).toBe(initial.items[1]);
  });

  it('distinguishes absence from a zeroed value', () => {
    // The projection disposition in type form: `null` and a zeroed
    // record must not reconcile to the same thing, or "you cannot see
    // this ship" becomes "this ship points north".
    const hidden = reconcile(undefined, { ship: null });
    const zeroed = reconcile(hidden, { ship: { heading: 0, sail: 0, shots: 0 } });

    expect(hidden.ship).toBeNull();
    expect(zeroed.ship).not.toBeNull();
    expect(zeroed).not.toBe(hidden);
  });

  it('freezes what it emits, so a caller cannot mutate a snapshot', () => {
    const initial = reconcile(undefined, WORLD);
    expect(Object.isFrozen(initial)).toBe(true);
    expect(Object.isFrozen(initial.players)).toBe(true);
    expect(Object.isFrozen(initial.players.a)).toBe(true);
    expect(Object.isFrozen(initial.players.a?.pos)).toBe(true);
  });
});

describe('mergeShallow', () => {
  it('returns the same object when the patch moves nothing', () => {
    const initial = reconcile(undefined, WORLD);
    expect(mergeShallow(initial, {})).toBe(initial);
    expect(mergeShallow(initial, { tick: 7 })).toBe(initial);
    // Deep-equal but freshly built: still not a change.
    expect(mergeShallow(initial, { players: { ...WORLD.players } })).toBe(initial);
  });

  it('replaces only the patched keys', () => {
    const initial = reconcile(undefined, WORLD);
    const next = mergeShallow(initial, { tick: 8 });

    expect(next).not.toBe(initial);
    expect(next.players).toBe(initial.players);
    expect(next.tick).toBe(8);
  });
});

describe('a record key is data, never a prototype', () => {
  it('reconcile keeps an own `__proto__` key as data', () => {
    // Built through JSON so the key is an OWN property; an object
    // literal would invoke the prototype setter instead.
    const next = JSON.parse('{"crew":{"__proto__":{"marker":true}}}') as Record<string, unknown>;

    const out = reconcile({}, next) as Record<string, Record<string, unknown>>;

    expect(JSON.stringify(out)).toBe('{"crew":{"__proto__":{"marker":true}}}');
    expect(Object.getPrototypeOf(out['crew'] as object)).toBe(Object.prototype);
    expect((out['crew'] as Record<string, unknown>)['marker']).toBeUndefined();
    expect(({} as Record<string, unknown>)['marker']).toBeUndefined();
  });

  it('mergeShallow keeps one as data too', () => {
    const patch = JSON.parse('{"__proto__":{"marker":true}}') as Record<string, unknown>;

    const out = mergeShallow({ a: 1 } as Record<string, unknown>, patch);

    expect(Object.prototype.hasOwnProperty.call(out, '__proto__')).toBe(true);
    expect((out as Record<string, unknown>)['marker']).toBeUndefined();
    expect(Object.getPrototypeOf(out)).toBe(Object.prototype);
  });

  it('an ordinary key is unaffected (control)', () => {
    const out = reconcile({}, { crew: { bosun: 'kess' } }) as Record<string, Record<string, string>>;
    expect(out['crew']?.['bosun']).toBe('kess');
    expect(Object.isFrozen(out)).toBe(true);
  });
});
