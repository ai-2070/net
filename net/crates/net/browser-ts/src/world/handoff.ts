/**
 * Entity handoff between region hosts — at-most-once, with a typed failure
 * (browser plan §9 item 3, decision Q6).
 *
 * An entity crossing from region A to region B must never be live in both,
 * and must never be applied twice. Exactly-once is not attempted; the
 * failure a crash can cause is an entity that is **nowhere live for a
 * while**, reported typed, never a duplicate.
 *
 * The protocol, over any lossy, duplicating, reordering carrier:
 *
 * 1. **A freezes.** The entity leaves A's live entities and waits in A's
 *    `outgoing` under a fresh handoff id, with its state. A answers inputs for
 *    it with `in-transit` (see {@link locate}).
 * 2. **A offers** `{ id, entity, state }` to B, and repeats the offer until it
 *    hears back. The id is the idempotence key.
 * 3. **B decides once per id.** On the first offer it either admits the
 *    entity (it is now live in B) or refuses it, and records the decision in
 *    `handled`. Every later copy of that offer gets the SAME answer and
 *    changes nothing.
 * 4. **A settles.** `accept`: the frozen copy is dropped, and the entity is
 *    B's. `refuse`: the entity returns to A's live entities. Only an explicit
 *    refusal can bring it back to A, because silence proves nothing: B may
 *    have admitted it and the answer been lost.
 * 5. **No answer.** A keeps it frozen and keeps offering. After `giveUpMs`,
 *    {@link retryHandoffs} reports it `unresolved` and stops offering. The
 *    entity stays frozen, still nowhere live twice. Recovery (the destination
 *    returning, an operator) re-offers with the same id through
 *    {@link reofferHandoff}, which B answers from its record.
 *
 * **Durability is the caller's, and it is the whole correctness argument.**
 * Every step here is a pure function from one {@link RegionState} to the
 * next. The host MUST make the returned state durable (e.g.
 * `persistStore(…).flush()`) BEFORE it sends the messages the step returns. A
 * crash then either loses the step with its messages, which is the same as a
 * lost message, or keeps both. B must also remember a decision for longer
 * than any A keeps offering: `handledRetentionMs` > `giveUpMs`.
 */

import type { ActRecord, ActionResult, ForwardedAction, GhostFrame } from './border.js';

/** A handoff's idempotence key: unique per crossing. */
export type HandoffId = string;

/** A to B: take this entity. */
export interface HandoffOffer<E> {
  readonly k: 'offer';
  readonly id: HandoffId;
  /** The source region. */
  readonly from: string;
  readonly entity: string;
  readonly state: E;
}

/** B to A: the decision for one handoff id, the same every time it is asked. */
export type HandoffReply =
  | { readonly k: 'accept'; readonly id: HandoffId; readonly from: string }
  | { readonly k: 'refuse'; readonly id: HandoffId; readonly from: string; readonly reason: string };

/** A message between region hosts, addressed to region `to`. */
export type HandoffMessage<E> = {
  readonly to: string;
  readonly body: HandoffOffer<E> | HandoffReply | ForwardedAction | ActionResult | GhostFrame<E>;
};

/** An entity frozen at the source, on its way out. */
export interface Outgoing<E> {
  readonly entity: string;
  readonly state: E;
  readonly to: string;
  /** When the handoff began, ms. */
  readonly since: number;
  /** When the offer was last sent, ms. */
  readonly sentAt: number;
  /** Stopped offering after `giveUpMs` (typed `unresolved`); still frozen. */
  readonly unresolved: boolean;
}

/** A decision the target recorded for an id. */
export interface Handled {
  readonly decision: 'accept' | 'refuse';
  readonly reason?: string;
  readonly at: number;
}

/**
 * A region host's handoff-relevant state. Keep it IN the region's persisted
 * document, and make it durable as one unit with the entities: a frozen
 * entity must not come back live after a restart, and an admitted one must
 * not be admitted again.
 */
export interface RegionState<E> {
  readonly region: string;
  readonly entities: Readonly<Record<string, E>>;
  readonly outgoing: Readonly<Record<HandoffId, Outgoing<E>>>;
  readonly handled: Readonly<Record<HandoffId, Handled>>;
  /** Cross-border actions applied here, by id (`border.ts`). */
  readonly acts?: Readonly<Record<string, ActRecord>>;
}

/** Timing for {@link retryHandoffs} and {@link pruneHandled}. */
export interface HandoffTiming {
  /** Re-offer an unanswered handoff this often, ms. Default 250. */
  readonly retryMs?: number;
  /** Stop offering after this long and report `unresolved`, ms. Default 60 000. */
  readonly giveUpMs?: number;
  /** How long a target remembers a decision, ms. Default 600 000; must exceed `giveUpMs`. */
  readonly handledRetentionMs?: number;
}

/** What one step did, for the host to act on AFTER making the state durable. */
export interface HandoffStep<E> {
  readonly state: RegionState<E>;
  /** Send these, after the state is durable. */
  readonly send: readonly HandoffMessage<E>[];
  /** Settled handoffs, for the game (e.g. tell the player where to go). */
  readonly events: readonly HandoffEvent[];
}

/** A handoff outcome at the source, or an admission at the target. */
export type HandoffEvent =
  | { readonly type: 'moved'; readonly id: HandoffId; readonly entity: string; readonly to: string }
  | { readonly type: 'refused'; readonly id: HandoffId; readonly entity: string; readonly to: string; readonly reason: string }
  | { readonly type: 'unresolved'; readonly id: HandoffId; readonly entity: string; readonly to: string }
  | { readonly type: 'admitted'; readonly id: HandoffId; readonly entity: string; readonly from: string };

/** An empty region. */
export function regionState<E>(region: string, entities: Readonly<Record<string, E>> = {}): RegionState<E> {
  return { region, entities, outgoing: {}, handled: {} };
}

function timing(options: HandoffTiming = {}): Required<HandoffTiming> {
  const retryMs = options.retryMs ?? 250;
  const giveUpMs = options.giveUpMs ?? 60_000;
  const handledRetentionMs = options.handledRetentionMs ?? 600_000;
  if (!(handledRetentionMs > giveUpMs)) {
    // A target that forgot a decision would admit a late re-offer a second
    // time: the one way this protocol could duplicate.
    throw new RangeError('handledRetentionMs must exceed giveUpMs, or a late offer could be admitted twice');
  }
  return { retryMs, giveUpMs, handledRetentionMs };
}

const has = (record: object, key: string): boolean => Object.prototype.hasOwnProperty.call(record, key);

function without<T>(record: Readonly<Record<string, T>>, key: string): Record<string, T> {
  const out: Record<string, T> = {};
  for (const k of Object.keys(record)) if (k !== key) Object.defineProperty(out, k, { value: record[k], enumerable: true, writable: true, configurable: true });
  return out;
}

function withEntry<T>(record: Readonly<Record<string, T>>, key: string, value: T): Record<string, T> {
  const out: Record<string, T> = { ...record };
  Object.defineProperty(out, key, { value, enumerable: true, writable: true, configurable: true });
  return out;
}

/**
 * Step 1–2 at the source: freeze `entity` and offer it to region `to`.
 * Throws if the entity is not live here (frozen already, or unknown).
 */
export function beginHandoff<E>(
  state: RegionState<E>,
  entity: string,
  to: string,
  id: HandoffId,
  now: number,
): HandoffStep<E> {
  if (!has(state.entities, entity)) throw new Error(`entity '${entity}' is not live in region '${state.region}'`);
  if (has(state.outgoing, id) || has(state.handled, id)) throw new Error(`handoff id '${id}' is already in use`);
  if (to === state.region) throw new Error('a handoff needs another region');
  const frozen = state.entities[entity]!;
  const outgoing: Outgoing<E> = { entity, state: frozen, to, since: now, sentAt: now, unresolved: false };
  return {
    state: { ...state, entities: without(state.entities, entity), outgoing: withEntry(state.outgoing, id, outgoing) },
    send: [{ to, body: { k: 'offer', id, from: state.region, entity, state: frozen } }],
    events: [],
  };
}

/**
 * Step 3 at the target: decide an offer once, and answer every copy of it
 * the same way. `admit` is asked only the first time; it returns `true` or a
 * refusal reason. An admitted entity whose id is already live here is
 * refused (`occupied`), never overwritten.
 */
export function onHandoffOffer<E>(
  state: RegionState<E>,
  offer: HandoffOffer<E>,
  now: number,
  admit: (entity: string, value: E) => true | string = () => true,
): HandoffStep<E> {
  const previous = has(state.handled, offer.id) ? state.handled[offer.id]! : undefined;
  if (previous !== undefined) {
    return { state, send: [{ to: offer.from, body: replyOf(state.region, offer.id, previous) }], events: [] };
  }
  let decision: Handled;
  if (has(state.entities, offer.entity)) {
    decision = { decision: 'refuse', reason: 'occupied', at: now };
  } else {
    let verdict: true | string;
    try {
      verdict = admit(offer.entity, offer.state);
    } catch (error) {
      verdict = `admit threw: ${String(error)}`;
    }
    decision = verdict === true ? { decision: 'accept', at: now } : { decision: 'refuse', reason: verdict, at: now };
  }
  const entities = decision.decision === 'accept' ? withEntry(state.entities, offer.entity, offer.state) : state.entities;
  return {
    state: { ...state, entities, handled: withEntry(state.handled, offer.id, decision) },
    send: [{ to: offer.from, body: replyOf(state.region, offer.id, decision) }],
    events:
      decision.decision === 'accept' ? [{ type: 'admitted', id: offer.id, entity: offer.entity, from: offer.from }] : [],
  };
}

function replyOf(region: string, id: HandoffId, handled: Handled): HandoffReply {
  return handled.decision === 'accept'
    ? { k: 'accept', id, from: region }
    : { k: 'refuse', id, from: region, reason: handled.reason ?? 'refused' };
}

/**
 * Step 4 at the source: settle on the target's answer. A reply for a handoff
 * this region is not holding (already settled, or never begun) changes
 * nothing: replies are duplicated and reordered like everything else.
 */
export function onHandoffReply<E>(state: RegionState<E>, reply: HandoffReply): HandoffStep<E> {
  if (!has(state.outgoing, reply.id)) return { state, send: [], events: [] };
  const outgoing = state.outgoing[reply.id]!;
  if (reply.from !== outgoing.to) return { state, send: [], events: [] };
  const rest = without(state.outgoing, reply.id);
  if (reply.k === 'accept') {
    return {
      state: { ...state, outgoing: rest },
      send: [],
      events: [{ type: 'moved', id: reply.id, entity: outgoing.entity, to: outgoing.to }],
    };
  }
  // Only an explicit refusal returns it. An entity that went live here again
  // in the meantime (a restore, a new spawn) is kept, and the frozen copy is
  // discarded rather than overwriting it.
  const entities = has(state.entities, outgoing.entity)
    ? state.entities
    : withEntry(state.entities, outgoing.entity, outgoing.state);
  return {
    state: { ...state, entities, outgoing: rest },
    send: [],
    events: [{ type: 'refused', id: reply.id, entity: outgoing.entity, to: outgoing.to, reason: reply.reason }],
  };
}

/**
 * Step 2 and 5 at the source, on a timer: re-offer what is unanswered, and
 * mark what has waited past `giveUpMs` as `unresolved` (reported once, still
 * frozen, no longer offered).
 */
export function retryHandoffs<E>(state: RegionState<E>, now: number, options?: HandoffTiming): HandoffStep<E> {
  const { retryMs, giveUpMs } = timing(options);
  let outgoing: Record<HandoffId, Outgoing<E>> | null = null;
  const send: HandoffMessage<E>[] = [];
  const events: HandoffEvent[] = [];
  for (const [id, pending] of Object.entries(state.outgoing)) {
    if (pending.unresolved) continue;
    if (now - pending.since >= giveUpMs) {
      outgoing ??= { ...state.outgoing };
      outgoing[id] = { ...pending, unresolved: true };
      events.push({ type: 'unresolved', id, entity: pending.entity, to: pending.to });
      continue;
    }
    if (now - pending.sentAt >= retryMs) {
      outgoing ??= { ...state.outgoing };
      outgoing[id] = { ...pending, sentAt: now };
      send.push({ to: pending.to, body: { k: 'offer', id, from: state.region, entity: pending.entity, state: pending.state } });
    }
  }
  return { state: outgoing === null ? state : { ...state, outgoing }, send, events };
}

/**
 * Recovery: offer an `unresolved` handoff again, under the SAME id (the
 * destination answers it from its record, so this cannot admit twice), and
 * restart its give-up clock. Use it when the destination is known to be
 * back. A no-op for a handoff that is not unresolved.
 */
export function reofferHandoff<E>(state: RegionState<E>, id: HandoffId, now: number): HandoffStep<E> {
  if (!has(state.outgoing, id) || !state.outgoing[id]!.unresolved) return { state, send: [], events: [] };
  const pending = state.outgoing[id]!;
  return {
    state: { ...state, outgoing: withEntry(state.outgoing, id, { ...pending, unresolved: false, since: now, sentAt: now }) },
    send: [{ to: pending.to, body: { k: 'offer', id, from: state.region, entity: pending.entity, state: pending.state } }],
    events: [],
  };
}

/** Forget target decisions older than `handledRetentionMs` (which must outlast any source's offers). */
export function pruneHandled<E>(state: RegionState<E>, now: number, options?: HandoffTiming): RegionState<E> {
  const { handledRetentionMs } = timing(options);
  let handled: Record<HandoffId, Handled> | null = null;
  for (const [id, decision] of Object.entries(state.handled)) {
    if (now - decision.at > handledRetentionMs) {
      handled ??= { ...state.handled };
      delete handled[id];
    }
  }
  return handled === null ? state : { ...state, handled };
}

/**
 * Where an entity is, from this region's point of view — what to answer an
 * input for it: `here` (apply it), `in-transit` (frozen, on its way to
 * `to`; refuse, never apply), or `unknown`.
 */
export function locate<E>(
  state: RegionState<E>,
  entity: string,
): { readonly at: 'here' } | { readonly at: 'in-transit'; readonly to: string; readonly id: HandoffId } | { readonly at: 'unknown' } {
  if (has(state.entities, entity)) return { at: 'here' };
  for (const [id, pending] of Object.entries(state.outgoing)) {
    if (pending.entity === entity) return { at: 'in-transit', to: pending.to, id };
  }
  return { at: 'unknown' };
}
