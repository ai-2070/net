/**
 * Patch semantics, frozen (brief §1.13).
 *
 * A delta is a list of `{"o":"r","p":[…],"val":…}` (replace) and
 * `{"o":"x","p":[…]}` (remove) operations over **own property-name
 * segments**. There is no JSON Pointer escaping, no arbitrary
 * operation, and nothing executable: the op set is two entries because
 * a patch language is an interpreter, and an interpreter on untrusted
 * input is a much larger promise than this protocol needs to make.
 *
 * Three properties carry the weight, and each is a witness rather than
 * a comment:
 *
 * - **Atomicity.** The whole patch is validated, applied to a draft,
 *   and the *final* state validated by the definition's `state()`
 *   before anything is committed. A half-applied patch is never
 *   published, and a refused patch leaves the previous revision
 *   byte-for-byte intact — the caller then resynchronizes (§1.8),
 *   because a replica whose view disagrees with the owner's must not
 *   guess.
 * - **Prototype safety.** `__proto__`, `constructor` and `prototype`
 *   are refused as segments, traversal is own-property only, and every
 *   node the patch builds has a **null prototype**. Refusing the names
 *   alone would not be enough — a draft built from `{}` inherits a
 *   `constructor` that `in` would find — so the two rules are both
 *   present and both witnessed.
 * - **Identity.** The committed state goes through
 *   {@link reconcile}, so untouched subtrees keep their references and
 *   a Three.js loop can diff by reference. An equivalent patch is a
 *   no-op, not a whole-world change.
 */

import type { StoreErrorCode } from './errors.js';
import type { JsonValue } from './json.js';
import { reconcile } from './state.js';
import type { Parse } from './types.js';
import { MAX_PATCH_OPS, MAX_PATH_SEGMENTS, MAX_PATH_SEGMENT_BYTES, utf8Length, type WireOp } from './wire.js';

/** Segments that would reach the prototype chain. */
export const FORBIDDEN_SEGMENTS: readonly string[] = ['__proto__', 'constructor', 'prototype'];

const FORBIDDEN: ReadonlySet<string> = new Set(FORBIDDEN_SEGMENTS);

/** Why a patch was refused. Stable and countable. */
export type PatchRefusal =
  | 'ops-too-many'
  | 'path-too-deep'
  | 'segment-too-long'
  | 'segment-forbidden'
  | 'root-remove'
  | 'missing-parent'
  | 'parent-not-object'
  | 'missing-key'
  | 'invalid-state';

export interface PatchRejected {
  readonly ok: false;
  readonly code: StoreErrorCode;
  readonly reason: PatchRefusal;
  /**
   * Whether the replica should resynchronize. True for the divergence
   * refusals (`missing-parent`, `missing-key`, `parent-not-object`,
   * `invalid-state`): the views disagree and only the owner can settle
   * it. False for the bound refusals, which are the sender's fault.
   */
  readonly resync: boolean;
}

export interface PatchApplied<S> {
  readonly ok: true;
  readonly next: S;
  /**
   * False when the patch moved nothing — `next === current`. An
   * unchanged update must not create a revision or a notification
   * (design §4), and that decision is made here, once.
   */
  readonly changed: boolean;
}

export type PatchOutcome<S> = PatchApplied<S> | PatchRejected;

function reject(reason: PatchRefusal, resync: boolean, code: StoreErrorCode = 'invalid-data'): PatchRejected {
  return { ok: false, code, reason, resync };
}

/**
 * Apply a delta's operations atomically.
 *
 * `current` is never mutated, whatever the outcome: on refusal the
 * caller still holds exactly the state it had, which is what makes
 * "a failed patch leaves the previous revision intact" a property of
 * this function rather than a rule its callers have to remember.
 */
export function applyPatch<S extends object>(
  current: S,
  ops: readonly WireOp[],
  validate: Parse<S>,
): PatchOutcome<S> {
  // Bounds first, and over the whole list: nothing is applied until
  // every operation is admissible, so a patch cannot be half-refused.
  if (ops.length > MAX_PATCH_OPS) return reject('ops-too-many', false, 'capacity');
  for (const op of ops) {
    if (op.p.length > MAX_PATH_SEGMENTS) return reject('path-too-deep', false, 'capacity');
    for (const segment of op.p) {
      if (utf8Length(segment) > MAX_PATH_SEGMENT_BYTES) {
        return reject('segment-too-long', false, 'capacity');
      }
      // Prototype-named segments are refused outright. Traversal below
      // is own-property only as well; either alone would be a hole.
      if (FORBIDDEN.has(segment)) return reject('segment-forbidden', false);
    }
    // A state must exist: replacing the root is permitted, removing it
    // is not.
    if (op.o === 'x' && op.p.length === 0) return reject('root-remove', false);
  }

  // Apply to a draft. Nodes are copied along the touched path only, so
  // an untouched subtree is the *same object* in the draft and is then
  // recognised by `reconcile` as unchanged.
  let draft: unknown = current;
  const copied = new WeakSet<object>();

  for (const op of ops) {
    if (op.p.length === 0) {
      // Root replace; root remove was refused above.
      if (op.o === 'r') draft = op.val;
      continue;
    }

    if (!isTraversable(draft)) return reject('parent-not-object', true);
    const root = copyOnce(draft as object, copied);
    draft = root;

    const parent = descend(root, op.p.slice(0, -1), copied);
    if (parent === MISSING) return reject('missing-parent', true);
    if (parent === NOT_OBJECT) return reject('parent-not-object', true);

    const container = parent as Record<string, JsonValue>;
    const leaf = op.p[op.p.length - 1] as string;
    if (op.o === 'x') {
      // A remove whose target is absent means the views have already
      // diverged: refuse the whole patch and resynchronize.
      if (!Object.prototype.hasOwnProperty.call(container, leaf)) {
        return reject('missing-key', true);
      }
      delete container[leaf];
      continue;
    }
    container[leaf] = op.val;
  }

  // The definition validates the **final** state, not each step: an
  // intermediate shape may legitimately be invalid, and a patch is one
  // transition.
  let validated: S;
  try {
    validated = validate(draft);
  } catch {
    return reject('invalid-state', true);
  }

  const next = reconcile(current, validated);
  return { ok: true, next, changed: !Object.is(next, current) };
}

const MISSING = Symbol('missing-parent');
const NOT_OBJECT = Symbol('parent-not-object');

/**
 * Walk to the parent of the target, copying each node on the way.
 *
 * `root` is already a draft copy. Each child is copied once and written
 * back into its (copied) holder, so a second operation through the same
 * parent mutates the draft's node instead of copying it again — which
 * is what makes overlapping operations apply in array order with the
 * later one winning.
 */
function descend(
  root: object,
  path: readonly string[],
  copied: WeakSet<object>,
): object | typeof MISSING | typeof NOT_OBJECT {
  let node: object = root;
  for (const segment of path) {
    const holder = node as Record<string, unknown>;
    // Own-property only: the prototype chain is never followed, so a
    // path segment can never resolve to an inherited member.
    if (!Object.prototype.hasOwnProperty.call(holder, segment)) return MISSING;
    const child = holder[segment];
    if (!isTraversable(child)) return NOT_OBJECT;
    const fresh = copyOnce(child as object, copied);
    holder[segment] = fresh;
    node = fresh;
  }
  return node;
}

/**
 * Arrays are replaced whole in v1, so they are not traversable: a path
 * that descends into one is a refusal, not an index.
 */
function isTraversable(value: unknown): boolean {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

/**
 * Copy a node into the draft once.
 *
 * The clone has a **null prototype**: refusing `__proto__`,
 * `constructor` and `prototype` as segments stops the names, and this
 * stops the inheritance, so no patched node can reach `Object.prototype`
 * at all.
 */
function copyOnce<T extends object>(node: T, copied: WeakSet<object>): T {
  if (copied.has(node)) return node;
  const clone = Object.assign(Object.create(null) as Record<string, unknown>, node) as T;
  copied.add(clone);
  return clone;
}
