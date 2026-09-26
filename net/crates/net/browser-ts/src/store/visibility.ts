/**
 * Declarative visibility: say which parts of the state are secret, and
 * the host removes them from every player's view before anything is
 * sent.
 *
 * ```ts
 * defineStore({
 *   …,
 *   visibility: {
 *     'players.*.hand': 'owner',     // only the player whose id is that key
 *     'deck':           'nobody',    // the host only
 *     'deck.length':    'everyone',  // …but everyone may count it
 *     'waypoint':       ['command'], // players reading the `command` audience
 *   },
 * });
 * ```
 *
 * Why this exists: a hand-written `project` fails SILENTLY — the game
 * works and the secret is readable in the browser. Rules declared here
 * are enforced by the SDK, after any hand-written projection, so code
 * can narrow what a rule allows but never widen it.
 *
 * **The rules.**
 *
 * - A path is dot-separated keys; `*` matches any key of an object (or
 *   index of an array). `deck.length` — a path ending in `.length` —
 *   reveals only the length of an array its parent rule hides.
 * - `'everyone'`, `'nobody'` (the host only), `'owner'` (the player
 *   whose peer id is the key matched by the path's FIRST `*` — the
 *   `ships[context.peer]` pattern needs no setup), or a list of
 *   audiences (any one of them suffices).
 * - Unlisted paths are visible, as without rules. A rule covers
 *   everything beneath its path, and a deeper rule cannot re-open what
 *   a shallower one hid (except `.length`).
 *
 * **What hidden looks like.** An entry of a collection (a path ending
 * in `*`) is removed. A field becomes the {@link HIDDEN} marker — never
 * `0`, `""` or `null`, which a game would read as real values — so the
 * state validator must accept it there: wrap that field's parser with
 * {@link hiddenOr}. A counted array becomes an array of markers of the
 * same length.
 *
 * **Presets**, for the common shapes: `'open'` (everyone sees
 * everything — an explicit decision rather than an accident) and
 * `'card-game'` (`players.*.hand` owner, `deck` nobody, `deck.length`
 * everyone). A preset takes overrides: `{ preset: 'card-game',
 * 'players.*.score': 'nobody' }`.
 *
 * **What it cannot do.** The host itself holds everything: a player who
 * hosts can read every secret in their own tab. Games with real stakes
 * need a host that is not a player.
 */

import { StoreError } from './errors.js';
import type { Viewer } from './owner.js';

/** Who may see a path. */
export type VisibilityRule = 'everyone' | 'nobody' | 'owner' | readonly string[];

/** The presets. */
export type VisibilityPreset = 'card-game';

/** Path → rule. */
export type VisibilityRules = { readonly [path: string]: VisibilityRule };

/** What `defineStore({ visibility })` accepts. */
export type Visibility =
  | 'open'
  | VisibilityPreset
  | VisibilityRules
  | ({ readonly preset: VisibilityPreset } & { readonly [path: string]: VisibilityRule | VisibilityPreset });

/** The marker a hidden field carries in a player's view. */
export interface Hidden {
  readonly $hidden: true;
}

/** The one hidden value. Plain JSON, so it survives the wire. */
export const HIDDEN: Hidden = Object.freeze({ $hidden: true as const });

/** Whether a value is the hidden marker (the original, or one parsed off the wire). */
export function isHidden(value: unknown): value is Hidden {
  return (
    typeof value === 'object' &&
    value !== null &&
    !Array.isArray(value) &&
    Object.keys(value).length === 1 &&
    (value as { $hidden?: unknown }).$hidden === true
  );
}

/** Wrap a field's parser so the field may also be {@link HIDDEN}. */
export function hiddenOr<T>(parse: (value: unknown) => T): (value: unknown) => T | Hidden {
  return value => (isHidden(value) ? HIDDEN : parse(value));
}

const PRESETS: Readonly<Record<VisibilityPreset, VisibilityRules>> = Object.freeze({
  'card-game': Object.freeze({
    'players.*.hand': 'owner',
    deck: 'nobody',
    'deck.length': 'everyone',
  }),
});

interface CompiledRule {
  readonly path: string;
  readonly segments: readonly string[];
  readonly rule: VisibilityRule;
  /** A `.length` rule: reveals the count of what its parent hides. */
  readonly counts: boolean;
}

/** Rules ready to apply. `'open'` compiles to no rules. */
export interface CompiledVisibility {
  readonly rules: readonly CompiledRule[];
  /** Whether any rule depends on who is looking — projections are then per player. */
  readonly perPlayer: boolean;
}

const SEGMENT = /^(\*|[^.*]+)$/;

/**
 * Check and compile a visibility declaration. Throws `invalid-data` for
 * anything that could not be enforced as written — at definition time,
 * not at the first projection.
 */
export function compileVisibility(visibility: Visibility): CompiledVisibility {
  let declared: Record<string, VisibilityRule>;
  if (visibility === 'open') {
    declared = {};
  } else if (typeof visibility === 'string') {
    const preset = PRESETS[visibility as VisibilityPreset];
    if (preset === undefined) throw new StoreError('invalid-data', `unknown visibility preset '${visibility}'`);
    declared = { ...preset };
  } else if (typeof visibility === 'object' && visibility !== null) {
    const { preset, ...rest } = visibility as Record<string, unknown>;
    declared = {};
    if (preset !== undefined) {
      const base = PRESETS[preset as VisibilityPreset];
      if (base === undefined) throw new StoreError('invalid-data', `unknown visibility preset '${String(preset)}'`);
      Object.assign(declared, base);
    }
    Object.assign(declared, rest);
  } else {
    throw new StoreError('invalid-data', 'visibility is a preset name or an object of path → rule');
  }

  const rules: CompiledRule[] = [];
  let perPlayer = false;
  for (const [path, rule] of Object.entries(declared)) {
    const segments = path.split('.');
    if (path.length === 0 || !segments.every(segment => SEGMENT.test(segment))) {
      throw new StoreError('invalid-data', `visibility path '${path}' is not dot-separated keys and '*'`);
    }
    const counts = segments.length > 1 && segments[segments.length - 1] === 'length';
    const valid =
      rule === 'everyone' ||
      rule === 'nobody' ||
      rule === 'owner' ||
      (Array.isArray(rule) && rule.length > 0 && rule.every(label => typeof label === 'string' && label.length > 0));
    if (!valid) {
      throw new StoreError(
        'invalid-data',
        `visibility rule for '${path}' must be 'everyone', 'nobody', 'owner' or a non-empty list of audiences`,
      );
    }
    if (rule === 'owner') {
      if (!segments.includes('*')) {
        throw new StoreError('invalid-data', `'owner' needs a '*' in the path to name the owner: '${path}'`);
      }
      perPlayer = true;
    }
    rules.push(Object.freeze({ path, segments: Object.freeze(segments), rule, counts }));
  }
  // Shallow first: a hidden parent removes its subtree before a child
  // rule could be looked at.
  rules.sort((a, b) => a.segments.length - b.segments.length);
  return Object.freeze({ rules: Object.freeze(rules), perPlayer });
}

function allows(rule: VisibilityRule, captures: readonly string[], viewer: Viewer | null): boolean {
  if (rule === 'everyone') return true;
  if (rule === 'nobody' || viewer === null) return false;
  if (rule === 'owner') return captures[0] === viewer.peer;
  return rule.some(label => viewer.audience.includes(label));
}

interface Match {
  readonly keys: readonly (string | number)[];
  readonly captures: readonly string[];
}

/** Every concrete location a pattern names in `value`. */
function matches(value: unknown, segments: readonly string[]): Match[] {
  const out: Match[] = [];
  const walk = (node: unknown, depth: number, keys: (string | number)[], captures: string[]): void => {
    if (depth === segments.length) {
      out.push({ keys: [...keys], captures: [...captures] });
      return;
    }
    if (typeof node !== 'object' || node === null) return;
    const segment = segments[depth]!;
    if (segment === '*') {
      const entries: [string | number, unknown][] = Array.isArray(node)
        ? node.map((child, index) => [index, child])
        : Object.entries(node);
      for (const [key, child] of entries) {
        walk(child, depth + 1, [...keys, key], [...captures, String(key)]);
      }
      return;
    }
    if (Array.isArray(node) || !Object.prototype.hasOwnProperty.call(node, segment)) return;
    walk((node as Record<string, unknown>)[segment], depth + 1, [...keys, segment], captures);
  };
  walk(value, 0, [], []);
  return out;
}

const DROP = Symbol('drop');

/** Copy-on-write set along `keys`. */
function setIn(root: unknown, keys: readonly (string | number)[], value: unknown): unknown {
  if (keys.length === 0) return value;
  const [head, ...rest] = keys;
  if (Array.isArray(root)) {
    const copy = [...root];
    copy[head as number] = setIn(copy[head as number], rest, value);
    return copy;
  }
  const record = root as Record<string, unknown>;
  return { ...record, [head as string]: setIn(record[head as string], rest, value) };
}

/** Remove the DROP sentinels: object keys deleted, array entries filtered. */
function sweep(node: unknown): unknown {
  if (Array.isArray(node)) {
    let changed = false;
    const out: unknown[] = [];
    for (const child of node) {
      if (child === DROP) {
        changed = true;
        continue;
      }
      const next = sweep(child);
      if (next !== child) changed = true;
      out.push(next);
    }
    return changed ? out : node;
  }
  if (typeof node === 'object' && node !== null) {
    let changed = false;
    const out: Record<string, unknown> = {};
    for (const [key, child] of Object.entries(node)) {
      if (child === DROP) {
        changed = true;
        continue;
      }
      const next = sweep(child);
      if (next !== child) changed = true;
      out[key] = next;
    }
    return changed ? out : node;
  }
  return node;
}

function isUnder(keys: readonly (string | number)[], prefix: readonly (string | number)[]): boolean {
  return prefix.length <= keys.length && prefix.every((key, index) => keys[index] === key);
}

/**
 * Apply compiled rules for one viewer (`null`: nobody in particular —
 * sees only what `'everyone'` may).
 */
export function applyVisibility<S>(compiled: CompiledVisibility, state: S, viewer: Viewer | null): S {
  if (compiled.rules.length === 0) return state;
  let out: unknown = state;
  const hidden: (readonly (string | number)[])[] = [];
  let touched = false;

  for (const rule of compiled.rules) {
    if (rule.counts) continue;
    for (const match of matches(state, rule.segments)) {
      if (hidden.some(keys => isUnder(match.keys, keys))) continue;
      if (allows(rule.rule, match.captures, viewer)) continue;
      // Last pattern segment a wildcard: an entry of a collection, removed.
      const entry = rule.segments[rule.segments.length - 1] === '*';
      let replacement: unknown = entry ? DROP : HIDDEN;
      // A `.length` rule on this path may reveal the count.
      const countRule = compiled.rules.find(
        candidate =>
          candidate.counts &&
          candidate.segments.length === rule.segments.length + 1 &&
          candidate.segments.slice(0, -1).every((segment, index) => segment === rule.segments[index]),
      );
      const current = match.keys.reduce<unknown>((node, key) => (node as Record<string, unknown>)[key as string], state);
      if (countRule !== undefined && Array.isArray(current) && allows(countRule.rule, match.captures, viewer)) {
        replacement = current.map(() => HIDDEN);
      }
      out = setIn(out, match.keys, replacement);
      hidden.push(match.keys);
      touched = true;
    }
  }
  return (touched ? sweep(out) : out) as S;
}

/**
 * The view a viewer would receive under a definition's declared rules,
 * for tests and tools. (A host also applies its hand-written `project`
 * / `projectFor` first; this is the rules alone.)
 */
export function projectVisible<S>(
  definition: { readonly visibility?: Visibility },
  state: S,
  viewer: Viewer | null,
): S {
  if (definition.visibility === undefined) return state;
  return applyVisibility(compileVisibility(definition.visibility), state, viewer);
}

/**
 * Throw if `viewer` could read any of `paths` (concrete dot paths, e.g.
 * `players.bob.hand`) under the definition's rules. Absent, or the
 * hidden marker, is hidden; anything else is a leak.
 */
export function assertHidden<S>(
  definition: { readonly visibility?: Visibility },
  state: S,
  viewer: Viewer | null,
  paths: readonly string[],
): void {
  const view = projectVisible(definition, state, viewer);
  for (const path of paths) {
    let node: unknown = view;
    for (const key of path.split('.')) {
      if (typeof node !== 'object' || node === null || !Object.prototype.hasOwnProperty.call(node, key)) {
        node = undefined;
        break;
      }
      node = (node as Record<string, unknown>)[key];
    }
    const leaked =
      node !== undefined && !isHidden(node) && !(Array.isArray(node) && node.every(element => isHidden(element)));
    if (leaked) {
      throw new Error(`'${path}' is visible to ${viewer === null ? 'nobody in particular' : viewer.peer}`);
    }
  }
}
