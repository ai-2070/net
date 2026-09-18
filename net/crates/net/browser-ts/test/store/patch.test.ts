/**
 * Patch semantics (brief §1.13, §5.1).
 *
 * The three properties this file exists for are atomicity, prototype
 * safety and identity, and each is asserted as an **observable**: the
 * state a caller can read after a refusal, a global a caller can read
 * after an attack, and object references a render loop can compare.
 * "It threw the right error" is not one of them.
 */

import { describe, expect, it } from 'vitest';

import { applyPatch, FORBIDDEN_SEGMENTS } from '../../src/store/patch.js';
import type { WireOp } from '../../src/store/wire.js';

interface World {
  readonly ship: { readonly heading: number; readonly sails: { readonly main: boolean } };
  readonly crew: Record<string, string>;
  readonly log: readonly number[];
}

/** A validator with the shape a definition's `state()` has. */
function validate(raw: unknown): World {
  const value = raw as World;
  if (typeof value !== 'object' || value === null) throw new Error('not an object');
  if (typeof value.ship !== 'object' || value.ship === null) throw new Error('ship');
  if (typeof value.ship.heading !== 'number') throw new Error('heading');
  if (typeof value.crew !== 'object' || value.crew === null) throw new Error('crew');
  if (!Array.isArray(value.log)) throw new Error('log');
  // A validator necessarily returns fresh objects; identity comes back
  // through `reconcile` (store/state.ts).
  return {
    ship: { heading: value.ship.heading, sails: { main: Boolean(value.ship.sails?.main) } },
    crew: { ...value.crew },
    log: [...value.log],
  };
}

function world(): World {
  return validate({
    ship: { heading: 90, sails: { main: true } },
    crew: { bosun: 'kess', cook: 'ivo' },
    log: [1, 2, 3],
  });
}

function apply(current: World, ops: readonly WireOp[]) {
  return applyPatch(current, ops, validate);
}

/** Apply and require success. */
function applied(current: World, ops: readonly WireOp[]) {
  const outcome = apply(current, ops);
  if (!outcome.ok) throw new Error(`expected success, refused ${outcome.reason}`);
  return outcome;
}

/** Apply and require refusal. */
function rejected(current: World, ops: readonly WireOp[]) {
  const outcome = apply(current, ops);
  if (outcome.ok) throw new Error('expected a refusal');
  return outcome;
}

describe('controls — patches that must apply', () => {
  it('replaces a nested value and preserves untouched identity', () => {
    const before = world();
    const { next, changed } = applied(before, [{ o: 'r', p: ['ship', 'heading'], val: 180 }]);

    expect(changed).toBe(true);
    expect(next.ship.heading).toBe(180);
    // Untouched subtrees keep their references, which is what lets a
    // render loop diff by reference.
    expect(next.crew).toBe(before.crew);
    expect(next.log).toBe(before.log);
    expect(next.ship.sails).toBe(before.ship.sails);
    expect(next.ship).not.toBe(before.ship);
  });

  it('removes a key', () => {
    const before = world();
    const { next } = applied(before, [{ o: 'x', p: ['crew', 'bosun'] }]);

    expect(next.crew).toEqual({ cook: 'ivo' });
    expect(next.ship).toBe(before.ship);
  });

  it('replaces the whole state at the root', () => {
    const before = world();
    const { next, changed } = applied(before, [
      { o: 'r', p: [], val: { ship: { heading: 0, sails: { main: false } }, crew: {}, log: [] } },
    ]);

    expect(changed).toBe(true);
    expect(next.ship.heading).toBe(0);
    expect(next.crew).toEqual({});
  });

  it('replaces an array whole', () => {
    const before = world();
    const { next } = applied(before, [{ o: 'r', p: ['log'], val: [9] }]);
    expect(next.log).toEqual([9]);
  });

  it('reports an equivalent patch as unchanged', () => {
    const before = world();
    const outcome = applied(before, [{ o: 'r', p: ['ship', 'heading'], val: 90 }]);

    // Identical value ⇒ no revision, no notification, same root.
    expect(outcome.changed).toBe(false);
    expect(outcome.next).toBe(before);
  });

  it('applies overlapping operations in array order, last writer winning', () => {
    const before = world();
    const { next } = applied(before, [
      { o: 'r', p: ['ship'], val: { heading: 1, sails: { main: false } } },
      { o: 'r', p: ['ship', 'heading'], val: 2 },
      { o: 'r', p: ['ship', 'heading'], val: 3 },
    ]);

    expect(next.ship.heading).toBe(3);
    expect(next.ship.sails.main).toBe(false);
  });

  it('applies a remove and a replace through the same parent', () => {
    const before = world();
    const { next } = applied(before, [
      { o: 'x', p: ['crew', 'bosun'] },
      { o: 'r', p: ['crew', 'cook'], val: 'rea' },
    ]);
    expect(next.crew).toEqual({ cook: 'rea' });
  });
});

describe('atomicity — a refused patch leaves the revision intact', () => {
  it('refuses the whole patch when a later operation has a missing parent', () => {
    const before = world();
    const snapshot = JSON.stringify(before);

    const outcome = rejected(before, [
      { o: 'r', p: ['ship', 'heading'], val: 180 },
      { o: 'r', p: ['rigging', 'jib'], val: true },
    ]);

    expect(outcome.reason).toBe('missing-parent');
    // The earlier operation must not survive: no half-applied patch.
    expect(before.ship.heading).toBe(90);
    expect(JSON.stringify(before)).toBe(snapshot);
    // And the caller resynchronizes rather than guessing.
    expect(outcome.resync).toBe(true);
  });

  it('refuses the whole patch when a removal target is absent', () => {
    const before = world();
    const outcome = rejected(before, [
      { o: 'r', p: ['ship', 'heading'], val: 180 },
      { o: 'x', p: ['crew', 'quartermaster'] },
    ]);

    expect(outcome.reason).toBe('missing-key');
    expect(outcome.resync).toBe(true);
    expect(before.ship.heading).toBe(90);
    expect(before.crew).toEqual({ bosun: 'kess', cook: 'ivo' });
  });

  it('refuses when the final state fails validation, after every op applied cleanly', () => {
    const before = world();
    // Each operation is individually legal; the *result* is not a
    // world. Validation is of the final state, not of each step.
    const outcome = rejected(before, [{ o: 'x', p: ['ship'] }]);

    expect(outcome.reason).toBe('invalid-state');
    expect(outcome.resync).toBe(true);
    expect(before.ship.heading).toBe(90);
  });

  it('allows an intermediate state the validator would reject', () => {
    const before = world();
    // The control for the case above: `ship` is removed and then
    // restored, so only the final state is validated and the patch is
    // admissible.
    const { next } = applied(before, [
      { o: 'x', p: ['ship'] },
      { o: 'r', p: ['ship'], val: { heading: 7, sails: { main: true } } },
    ]);
    expect(next.ship.heading).toBe(7);
  });

  it('refuses a root removal: a state must exist', () => {
    const before = world();
    const outcome = rejected(before, [{ o: 'x', p: [] }]);

    expect(outcome.reason).toBe('root-remove');
    expect(outcome.resync).toBe(false);
    expect(before.ship.heading).toBe(90);
  });

  it('refuses a path that descends into an array', () => {
    const before = world();
    // Arrays are replaced whole in v1, so a segment is never an index.
    const outcome = rejected(before, [{ o: 'r', p: ['log', '0'], val: 9 }]);
    expect(outcome.reason).toBe('parent-not-object');
    expect(before.log).toEqual([1, 2, 3]);
  });

  it('bounds a path segment by bytes, not characters', () => {
    const before = world();
    // 33 three-byte characters: 33 characters, 99 bytes. A character
    // count admits it; a byte count does not.
    const wide = '\u4e2d'.repeat(33);
    expect(wide.length).toBe(33);
    expect(wide.length).toBeLessThanOrEqual(64);
    const outcome = rejected(before, [{ o: 'r', p: ['crew', wide], val: 'x' }]);
    expect(outcome.reason).toBe('segment-too-long');
    expect(outcome.code).toBe('capacity');

    // And a 64-byte ASCII segment is admissible (control).
    const { next } = applied(before, [{ o: 'r', p: ['crew', 'x'.repeat(64)], val: 'ok' }]);
    expect(next.crew['x'.repeat(64)]).toBe('ok');
  });

  it('refuses a bound violation before applying anything', () => {
    const before = world();
    const ops: WireOp[] = [
      { o: 'r', p: ['ship', 'heading'], val: 180 },
      { o: 'r', p: ['a', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'i'], val: 1 },
    ];
    const outcome = rejected(before, ops);

    expect(outcome.reason).toBe('path-too-deep');
    expect(outcome.code).toBe('capacity');
    // A bound violation is the sender's fault, not a divergence.
    expect(outcome.resync).toBe(false);
    expect(before.ship.heading).toBe(90);
  });
});

describe('prototype safety', () => {
  it('refuses every prototype-named segment', () => {
    const before = world();
    for (const segment of FORBIDDEN_SEGMENTS) {
      const outcome = rejected(before, [{ o: 'r', p: [segment], val: { polluted: true } }]);
      expect(outcome.reason, segment).toBe('segment-forbidden');
      const nested = rejected(before, [{ o: 'r', p: ['ship', segment], val: 1 }]);
      expect(nested.reason, segment).toBe('segment-forbidden');
    }
  });

  it('mutates no global when a pollution attempt is refused', () => {
    const before = world();
    rejected(before, [{ o: 'r', p: ['__proto__', 'polluted'], val: true }]);
    rejected(before, [{ o: 'r', p: ['constructor', 'prototype', 'polluted'], val: true }]);

    // The observable a caller can check: nothing anywhere acquired the
    // property.
    expect(({} as Record<string, unknown>)['polluted']).toBeUndefined();
    expect((Object.prototype as unknown as Record<string, unknown>)['polluted']).toBeUndefined();
    expect(([] as unknown as Record<string, unknown>)['polluted']).toBeUndefined();
    expect((before as unknown as Record<string, unknown>)['polluted']).toBeUndefined();
  });

  it('never resolves a path segment through the prototype chain', () => {
    // `toString` exists on every ordinary object by inheritance. A
    // traversal that used `in` or plain property access would find it
    // and treat the parent as present; own-property traversal refuses.
    const before = world();
    const outcome = rejected(before, [{ o: 'r', p: ['ship', 'toString', 'x'], val: 1 }]);
    // Not "not an object" — the segment does not resolve at all.
    expect(outcome.reason).toBe('missing-parent');

    const removal = rejected(before, [{ o: 'x', p: ['ship', 'toString'] }]);
    expect(removal.reason).toBe('missing-key');
  });

  it('exposes no inherited member of a patched node to traversal', () => {
    const before = world();
    const { next } = applied(before, [{ o: 'r', p: ['crew', 'cook'], val: 'rea' }]);
    expect(next.crew['cook']).toBe('rea');

    // `crew` was rebuilt by that patch. A later patch still cannot
    // reach an inherited member through it: traversal is own-property
    // only, whatever the node's prototype happens to be.
    expect(rejected(next, [{ o: 'r', p: ['crew', 'toString', 'x'], val: 1 }]).reason).toBe('missing-parent');
    expect(rejected(next, [{ o: 'x', p: ['crew', 'hasOwnProperty'] }]).reason).toBe('missing-key');
    // And what is published is frozen, so nothing can be written into
    // the committed graph afterwards either.
    expect(Object.isFrozen(next)).toBe(true);
    expect(Object.isFrozen(next.crew)).toBe(true);
  });

  it('accepts an ordinary key whose name merely resembles one (control)', () => {
    const before = world();
    const { next } = applied(before, [{ o: 'r', p: ['crew', 'proto'], val: 'ok' }]);
    expect(next.crew['proto']).toBe('ok');
  });
});

describe('A2 — a replacement value is data, through reconciliation', () => {
  /**
   * A validator that accepts only plain records and returns a fresh
   * JSON clone, as a definition's `state()` does. It is the oracle:
   * the corrupted result used to fail the very schema that accepted
   * the draft.
   */
  function plainRecords(raw: unknown): World {
    const value = raw as Record<string, unknown>;
    if (typeof value !== 'object' || value === null || Array.isArray(value)) {
      throw new Error('not a record');
    }
    for (const [key, nested] of Object.entries(value)) {
      if (typeof nested !== 'object' || nested === null) continue;
      if (Array.isArray(nested)) continue;
      if (Object.getPrototypeOf(nested) !== Object.prototype && Object.getPrototypeOf(nested) !== null) {
        throw new Error(`${key} is not a plain record`);
      }
    }
    return JSON.parse(JSON.stringify(value)) as World;
  }

  it('keeps `__proto__` as an own key of the replacement value', () => {
    const before = plainRecords({
      ship: { heading: 90, sails: { main: true } },
      crew: { bosun: 'kess' },
      log: [1],
    });
    // Built through JSON, like the decoder does: an object LITERAL
    // `{__proto__: …}` invokes the prototype setter and has no own
    // key at all, so it would not reproduce anything.
    const injected = JSON.parse('{"__proto__":{"reviewMarker":true}}') as Record<string, unknown>;
    expect(Object.prototype.hasOwnProperty.call(injected, '__proto__')).toBe(true);

    // No forbidden path SEGMENT anywhere: the special key is inside an
    // ordinary replacement value, and the path is just ["crew"].
    const outcome = applyPatch(before, [{ o: 'r', p: ['crew'], val: injected as never }], plainRecords);
    if (!outcome.ok) throw new Error(`expected success, refused ${outcome.reason}`);
    const next = outcome.next as unknown as Record<string, Record<string, unknown>>;

    // The data survives as data …
    expect(JSON.stringify(next['crew'])).toBe('{"__proto__":{"reviewMarker":true}}');
    expect(Object.prototype.hasOwnProperty.call(next['crew'] as object, '__proto__')).toBe(true);
    // … and is NOT the object's prototype.
    expect(Object.getPrototypeOf(next['crew'] as object)).toBe(Object.prototype);
    expect((next['crew'] as Record<string, unknown>)['reviewMarker']).toBeUndefined();

    // The successful result satisfies the schema that accepted it.
    expect(() => plainRecords(next)).not.toThrow();
    // And nothing global moved.
    expect(({} as Record<string, unknown>)['reviewMarker']).toBeUndefined();
  });

  it('keeps it at the root too', () => {
    const before = plainRecords({ ship: { heading: 1, sails: { main: true } }, crew: {}, log: [] });
    const root = JSON.parse(
      '{"ship":{"heading":2,"sails":{"main":false}},"crew":{},"log":[],' +
        '"__proto__":{"reviewMarker":true}}',
    ) as Record<string, unknown>;

    const outcome = applyPatch(before, [{ o: 'r', p: [], val: root as never }], plainRecords);
    if (!outcome.ok) throw new Error(`expected success, refused ${outcome.reason}`);
    const next = outcome.next as unknown as Record<string, unknown>;

    expect(Object.prototype.hasOwnProperty.call(next, '__proto__')).toBe(true);
    expect(next['reviewMarker']).toBeUndefined();
    expect(() => plainRecords(next)).not.toThrow();
  });

  it('leaves the previous state untouched either way', () => {
    const before = plainRecords({ ship: { heading: 90, sails: { main: true } }, crew: {}, log: [] });
    const snapshot = JSON.stringify(before);

    applyPatch(
      before,
      [{ o: 'r', p: ['crew'], val: JSON.parse('{"__proto__":{"reviewMarker":true}}') as never }],
      plainRecords,
    );

    expect(JSON.stringify(before)).toBe(snapshot);
    expect((before as unknown as Record<string, unknown>)['reviewMarker']).toBeUndefined();
  });

  it('still refuses `__proto__` as a path SEGMENT (control)', () => {
    const before = world();
    expect(rejected(before, [{ o: 'r', p: ['__proto__', 'x'], val: 1 }]).reason).toBe('segment-forbidden');
  });

  it('still shares identity for untouched subtrees (control)', () => {
    const before = world();
    const { next } = applied(before, [{ o: 'r', p: ['ship', 'heading'], val: 7 }]);
    expect(next.crew).toBe(before.crew);
    expect(next.log).toBe(before.log);
  });
});
