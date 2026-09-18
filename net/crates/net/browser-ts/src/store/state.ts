/**
 * Identity-preserving state application.
 *
 * `getState()` promises that "unchanged subtrees retain identity"
 * (design §4). That is not documentation — it is what lets a Three.js
 * loop diff by reference instead of walking the scene every frame, and
 * a naive apply (`JSON.parse` → replace the root) breaks it silently:
 * every render sees a brand-new root, every selector fires, and the
 * only symptom is that the demo feels heavy.
 *
 * So both write paths funnel through {@link reconcile}:
 *
 * - the owner's shallow merge ({@link mergeShallow}), and
 * - a replica applying a full validated snapshot.
 *
 * A validator necessarily returns fresh objects, so validation always
 * happens *first* and reconciliation restores identity afterwards.
 * That composition is also what makes an equivalent resynchronization
 * snapshot a no-op rather than a whole-world change.
 *
 * Everything reconcile emits is frozen. New nodes are frozen as they
 * are built and unchanged nodes were frozen when they were first
 * built, so the reachable graph is deeply immutable without a separate
 * deep-freeze pass over unchanged data.
 */

/**
 * The reconciled form of `next`, reusing every part of `previous` that
 * is structurally equal.
 *
 * Returns `previous` itself when the two are equivalent, so
 * `reconcile(state, validate(raw)) === state` is the test for "this
 * snapshot told us nothing new".
 */
export function reconcile<T>(previous: unknown, next: T): T {
  if (Object.is(previous, next)) return next;

  if (Array.isArray(next)) {
    const source = next as readonly unknown[];
    if (!Array.isArray(previous)) return freezeArray(source, undefined) as unknown as T;
    const merged = freezeArray(source, previous);
    // Same length and every element reused ⇒ the old array still
    // describes the world, so keep its identity.
    if (previous.length === source.length && sameElements(previous, merged)) {
      return previous as unknown as T;
    }
    return merged as unknown as T;
  }

  if (isPlainObject(next)) {
    if (!isPlainObject(previous)) return freezeRecord(next, undefined) as T;
    const merged = freezeRecord(next, previous);
    const nextKeys = Object.keys(next);
    if (
      nextKeys.length === Object.keys(previous).length &&
      nextKeys.every(key => Object.is(previous[key], merged[key]))
    ) {
      return previous as unknown as T;
    }
    return merged as T;
  }

  // Primitive, function, class instance or null: nothing to share.
  return next;
}

/**
 * The owner's transaction: a shallow merge at the root, reconciled.
 *
 * Returns `current` unchanged when the patch moves nothing, which is
 * what makes "empty/unchanged updates do not create notifications or
 * revisions" true at the source instead of at every notify site.
 */
export function mergeShallow<S extends object>(current: S, patch: Partial<S>): S {
  const keys = Object.keys(patch) as (keyof S & string)[];
  if (keys.length === 0) return current;

  let changed = false;
  const draft: Record<string, unknown> = { ...(current as Record<string, unknown>) };
  for (const key of keys) {
    const incoming = reconcile(
      (current as Record<string, unknown>)[key],
      (patch as Record<string, unknown>)[key],
    );
    if (!Object.is((current as Record<string, unknown>)[key], incoming)) {
      // `defineProperty` for the reason `freezeRecord` uses it: a
      // plain assignment of `__proto__` replaces this object's
      // prototype rather than storing the caller's data. The spread
      // above is already safe (it defines rather than assigns); this
      // was the one that was not.
      Object.defineProperty(draft, key, {
        value: incoming,
        enumerable: true,
        writable: true,
        configurable: true,
      });
      changed = true;
    }
  }
  if (!changed) return current;
  return Object.freeze(draft) as S;
}

function isPlainObject(value: unknown): value is Record<string, unknown> {
  if (typeof value !== 'object' || value === null || Array.isArray(value)) return false;
  const proto = Object.getPrototypeOf(value) as unknown;
  return proto === Object.prototype || proto === null;
}

function sameElements(previous: readonly unknown[], merged: readonly unknown[]): boolean {
  for (let i = 0; i < merged.length; i += 1) {
    if (!Object.is(previous[i], merged[i])) return false;
  }
  return true;
}

function freezeArray(
  next: readonly unknown[],
  previous: readonly unknown[] | undefined,
): readonly unknown[] {
  const out = new Array<unknown>(next.length);
  for (let i = 0; i < next.length; i += 1) {
    out[i] = reconcile(previous?.[i], next[i]);
  }
  return Object.freeze(out);
}

function freezeRecord(
  next: Record<string, unknown>,
  previous: Record<string, unknown> | undefined,
): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  for (const key of Object.keys(next)) {
    // `defineProperty`, never `out[key] = …`.
    //
    // A plain assignment of the key `__proto__` goes through
    // `Object.prototype`'s legacy setter: it REPLACES this object's
    // prototype instead of storing a property. A patch whose
    // replacement *value* is the ordinary JSON data
    // `{"__proto__":{…}}` — no forbidden path segment anywhere — then
    // came out of a successful, validated reconciliation as `{}` whose
    // prototype carried the data, so the published state both lost the
    // key and answered `true` for a field it does not own. The
    // null-prototype parser and the null-prototype patch draft do not
    // help here: this is the last step, after validation, and it built
    // an ordinary object.
    Object.defineProperty(out, key, {
      value: reconcile(previous?.[key], next[key]),
      enumerable: true,
      writable: false,
      configurable: false,
    });
  }
  return Object.freeze(out);
}
