/**
 * Cross-border actions (browser plan §9 item 4): region A asks region B to
 * act on an entity B owns — a hit across the border, a door on the other
 * side — and **B's authority decides**. At-most-once, like handoff:
 *
 * - A sends `{ id, name, input }` and repeats it until B answers.
 * - B runs the action ONCE per id, records the outcome in `acts` in the same
 *   commit as its effect, makes both durable, and answers. Every repeat of
 *   that id gets the recorded answer and changes nothing.
 * - A that hears nothing within `giveUpMs` reports `unresolved`: the action
 *   may or may not have happened, but it never happened twice. The caller
 *   decides whether to ask again under a NEW id.
 *
 * A's pending actions are not persisted: an action A loses in a crash either
 * reached B (and happened once) or did not (and did not happen).
 */

import type { RegionState } from './handoff.js';

/** A to B: act on something B owns. */
export interface ForwardedAction {
  readonly k: 'act';
  readonly id: string;
  /** The asking region. */
  readonly from: string;
  readonly name: string;
  readonly input: unknown;
}

/** B to A: the recorded outcome of one action id, the same every time. */
export type ActionResult =
  | { readonly k: 'result'; readonly id: string; readonly from: string; readonly ok: true; readonly output: unknown }
  | { readonly k: 'result'; readonly id: string; readonly from: string; readonly ok: false; readonly reason: string };

/** An outcome the target recorded for an action id. */
export interface ActRecord {
  readonly at: number;
  readonly ok: boolean;
  readonly output?: unknown;
  readonly reason?: string;
}

/**
 * How a region applies one kind of forwarded action: the entities after it
 * and the answer, or a refusal reason (string). Pure: the driver commits the
 * returned entities, the record and nothing else.
 */
export type BorderAction<E> = (
  entities: Readonly<Record<string, E>>,
  input: unknown,
  from: string,
) => { readonly entities: Readonly<Record<string, E>>; readonly output: unknown } | string;

const has = (record: object, key: string): boolean => Object.prototype.hasOwnProperty.call(record, key);

function resultOf(region: string, id: string, record: ActRecord): ActionResult {
  return record.ok
    ? { k: 'result', id, from: region, ok: true, output: record.output ?? null }
    : { k: 'result', id, from: region, ok: false, reason: record.reason ?? 'refused' };
}

/**
 * At the target: apply a forwarded action once per id and answer every copy
 * of it the same way. An unknown action name is refused (and recorded, so
 * the answer stays the same).
 */
export function onForwardedAction<E>(
  state: RegionState<E>,
  act: ForwardedAction,
  now: number,
  actions: Readonly<Record<string, BorderAction<E>>>,
): { readonly state: RegionState<E>; readonly reply: ActionResult } {
  const acts = state.acts ?? {};
  if (has(acts, act.id)) return { state, reply: resultOf(state.region, act.id, acts[act.id]!) };
  let record: ActRecord;
  let entities = state.entities;
  const handler = has(actions, act.name) ? actions[act.name] : undefined;
  if (handler === undefined) {
    record = { at: now, ok: false, reason: `no action '${act.name}' here` };
  } else {
    let outcome: ReturnType<BorderAction<E>>;
    try {
      outcome = handler(state.entities, act.input, act.from);
    } catch (error) {
      outcome = `action threw: ${String(error)}`;
    }
    if (typeof outcome === 'string') {
      record = { at: now, ok: false, reason: outcome };
    } else {
      entities = outcome.entities;
      record = { at: now, ok: true, output: outcome.output };
    }
  }
  const nextActs: Record<string, ActRecord> = { ...acts };
  Object.defineProperty(nextActs, act.id, { value: record, enumerable: true, writable: true, configurable: true });
  return { state: { ...state, entities, acts: nextActs }, reply: resultOf(state.region, act.id, record) };
}

/**
 * Region A to neighbour B: A's entities near their shared border, read-only
 * (plan §9 item 5, ghosting). Newer `seq` replaces older; nothing here is
 * authoritative in B.
 */
export interface GhostFrame<E> {
  readonly k: 'ghost';
  readonly from: string;
  readonly seq: number;
  readonly entities: Readonly<Record<string, E>>;
}

/**
 * Which neighbours of region `r:<rx>:<rz>` (squares of `size`) should see an
 * entity at (x, z) within `margin` of their edge — up to three (two sides
 * and the corner between them).
 */
export function ghostTargets(region: string, x: number, z: number, size: number, margin: number): string[] {
  const match = /^r:(-?\d+):(-?\d+)$/.exec(region);
  if (match === null) return [];
  const rx = Number(match[1]);
  const rz = Number(match[2]);
  const near = (low: number, high: number, at: number): number[] => {
    const out: number[] = [0];
    if (at - low < margin) out.push(-1);
    if (high - at <= margin) out.push(1);
    return out;
  };
  const out: string[] = [];
  for (const dx of near(rx * size, (rx + 1) * size, x)) {
    for (const dz of near(rz * size, (rz + 1) * size, z)) {
      if (dx !== 0 || dz !== 0) out.push(`r:${rx + dx}:${rz + dz}`);
    }
  }
  return out;
}

/** Forget action records older than `retentionMs` (which must outlast any source's retries). */
export function pruneActs<E>(state: RegionState<E>, now: number, retentionMs: number): RegionState<E> {
  const acts = state.acts;
  if (acts === undefined) return state;
  let next: Record<string, ActRecord> | null = null;
  for (const [id, record] of Object.entries(acts)) {
    if (now - record.at > retentionMs) {
      next ??= { ...acts };
      delete next[id];
    }
  }
  return next === null ? state : { ...state, acts: next };
}
