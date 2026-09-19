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

import { StoreCore } from '../../src/store/core.js';
import { defineStore } from '../../src/store/definition.js';
import { applyPatch } from '../../src/store/patch.js';
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

/**
 * The reviewer's second A2 round: `Object.defineProperty` repaired the
 * WRITES, and the reads still answered from the prototype. An own
 * `__proto__` key whose value is an *empty* object is the discriminating
 * case, because `Object.prototype` has no own enumerable keys either —
 * so the old comparison called them equal and the update disappeared.
 *
 * Every row here asserts what a caller can observe: the resulting
 * document, the revision, the notifications, and that the stored value
 * is a fresh frozen record rather than the global prototype.
 */
describe('an own key is never read through the prototype', () => {
  /** A definition that keeps whatever own keys it is given. */
  const bag = defineStore<Record<string, unknown>, Record<string, never>, Record<string, never>>({
    id: 'bag',
    version: 1,
    state(value) {
      const source = value as Record<string, unknown>;
      const out: Record<string, unknown> = {};
      for (const key of Object.keys(source)) {
        Object.defineProperty(out, key, {
          value: source[key],
          enumerable: true,
          writable: true,
          configurable: true,
        });
      }
      return out;
    },
    empty: () => ({}),
    actions: {},
    inputs: {},
  });

  function bagCore(initialState: Record<string, unknown>): StoreCore<
    Record<string, unknown>,
    Record<string, never>,
    Record<string, never>
  > {
    return new StoreCore({ definition: bag, initialState });
  }

  /** `{"__proto__":{}}` — an own data key with an empty object value. */
  function protoKeyed(json: string): Record<string, unknown> {
    const parsed = JSON.parse(json) as Record<string, unknown>;
    expect(Object.prototype.hasOwnProperty.call(parsed, '__proto__')).toBe(true);
    return parsed;
  }

  it('lands an owner update that adds an empty-valued `__proto__`', () => {
    const store = bagCore({ a: 1 });
    const seen: string[] = [];
    store.subscribe(state => seen.push(JSON.stringify(state)));

    store.applyOwnerUpdate(protoKeyed('{"a":1,"__proto__":{}}'));

    expect(JSON.stringify(store.getState())).toBe('{"a":1,"__proto__":{}}');
    expect(store.revision).toBe(1);
    expect(seen).toEqual(['{"a":1,"__proto__":{}}']);
  });

  it('replaces the root when the new snapshot has different keys of equal count', () => {
    // One own key each: `a` out, `__proto__` in. Equal cardinality was
    // the other half of the disguise.
    const store = bagCore({ a: 1 });
    const seen: string[] = [];
    store.subscribe(state => seen.push(JSON.stringify(state)));

    store.applySnapshot(protoKeyed('{"__proto__":{}}'));

    expect(JSON.stringify(store.getState())).toBe('{"__proto__":{}}');
    expect(store.revision).toBe(1);
    expect(seen).toEqual(['{"__proto__":{}}']);
  });

  it('stores a fresh frozen record, not the global prototype', () => {
    // Starting from `{}` produced the other outcome: the own key
    // survived, and its value was literally `Object.prototype`.
    const store = bagCore({});
    store.applySnapshot(protoKeyed('{"__proto__":{}}'));

    const stored = Object.getOwnPropertyDescriptor(store.getState(), '__proto__')?.value as object;
    expect(stored).not.toBe(Object.prototype);
    expect(Object.isFrozen(stored)).toBe(true);
    expect(Object.keys(stored)).toEqual([]);
    // A fresh ordinary record: the repair is that it is not the global
    // prototype, not that it is null-prototyped.
    expect(Object.getPrototypeOf(stored)).toBe(Object.prototype);
  });

  it('lands a nested empty-valued `__proto__`', () => {
    const store = bagCore({ crew: { rank: 1 } });
    const update = JSON.parse('{"crew":{"rank":1,"__proto__":{}}}') as Record<string, unknown>;
    // The own key is one level down here, so assert it there.
    expect(Object.prototype.hasOwnProperty.call(update['crew'] as object, '__proto__')).toBe(true);
    store.applyOwnerUpdate(update);

    expect(JSON.stringify(store.getState())).toBe('{"crew":{"rank":1,"__proto__":{}}}');
    const crew = store.getState()['crew'] as object;
    const nested = Object.getOwnPropertyDescriptor(crew, '__proto__')?.value as object;
    expect(nested).not.toBe(Object.prototype);
    expect(Object.isFrozen(nested)).toBe(true);
  });

  it('applyPatch replaces the root and reports the change', () => {
    // The nonempty version of this row already exists in
    // patch.test.ts; the empty value is the one that read as equal.
    const before = Object.freeze({ a: 1 }) as Record<string, unknown>;
    const outcome = applyPatch(
      before,
      [{ o: 'r', p: [], val: protoKeyed('{"__proto__":{}}') as never }],
      value => bag.state(value),
    );

    expect(outcome.ok).toBe(true);
    if (!outcome.ok) return;
    expect(outcome.changed).toBe(true);
    expect(outcome.next).not.toBe(before);
    expect(JSON.stringify(outcome.next)).toBe('{"__proto__":{}}');
  });

  it('reconcile does not accept an inherited value as the previous one', () => {
    // With the prior-subtree read repaired, `__proto__` can no longer
    // reach the equality test: the merged value is a fresh record and
    // the inherited one is `Object.prototype`, so they differ. What
    // still reaches it is a value IDENTICAL to an inherited member.
    // The wire cannot carry one — JSON has no function values — but a
    // local `applyOwnerUpdate` can, so the equality test asks about
    // own keys rather than trusting the count.
    const previous = Object.freeze({ rank: 1 });

    for (const inherited of ['toString', 'constructor', 'hasOwnProperty'] as const) {
      const next = { [inherited]: (previous as Record<string, unknown>)[inherited] };
      expect(Object.keys(next)).toHaveLength(Object.keys(previous).length);

      const out = reconcile(previous, next);

      expect(out).not.toBe(previous);
      expect(Object.keys(out)).toEqual([inherited]);
    }
  });

  it('drops no nested owner update whose value is an inherited member', () => {
    const store = bagCore({ crew: { rank: 1 } });
    store.applyOwnerUpdate({ crew: { toString: Object.prototype.toString } });

    expect(Object.keys(store.getState()['crew'] as object)).toEqual(['toString']);
    expect(store.revision).toBe(1);
  });

  it('reconcile keeps the previous root only when the own keys match', () => {
    const previous = Object.freeze({ a: 1 });

    // Equal count, different own key ⇒ a new root.
    expect(reconcile(previous, protoKeyed('{"__proto__":{}}'))).not.toBe(previous);
    // Equal count, same own key, same value ⇒ the old root.
    expect(reconcile(previous, { a: 1 })).toBe(previous);
  });

  it('mergeShallow reports the change for an empty-valued `__proto__`', () => {
    const current = Object.freeze({ a: 1 }) as Record<string, unknown>;
    const merged = mergeShallow(current, protoKeyed('{"__proto__":{}}'));

    expect(merged).not.toBe(current);
    expect(JSON.stringify(merged)).toBe('{"a":1,"__proto__":{}}');
    expect(Object.getPrototypeOf(merged)).toBe(Object.prototype);
  });
});
