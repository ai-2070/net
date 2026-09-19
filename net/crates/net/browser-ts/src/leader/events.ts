/**
 * §8's lifecycle, as events.
 *
 * A session emits every {@link LeafEvent} the direct surface does,
 * plus five tags of its own. They are here rather than in
 * `src/events.ts` because they describe the *lifecycle*, not the node:
 * a page using {@link BrowserNode} never sees them, and a page using a
 * session cannot explain a leader change without them.
 *
 * `parseEvent` carries an unknown tag through intact, so a bundle
 * whose leaf is newer than its wrapper stays legible; this module
 * refines exactly the five it knows and leaves the rest as they
 * arrived.
 */

import { parseEvent, type LeafEvent, type UnknownEvent } from '../events.js';

/** The Web Lock changed hands; this tab's generation moved. */
export interface LeaderChangedEvent {
  readonly type: 'leader_changed';
  /** The new generation, exact decimal. */
  readonly generation: string;
}

/**
 * A channel subscription was (re-)established by the leader.
 *
 * Carries the channel **name**, not its hash: the leader restores by
 * name, and a page that declared `subscriptions` recognises the name
 * it asked for.
 */
export interface SubscriptionRestoredEvent {
  readonly type: 'subscription_restored';
  readonly channel: string;
}

/**
 * The leader stood down, and the work of `generation` has been failed.
 *
 * `failed` is how many in-flight requests were rejected — a count, so
 * a `number` is exact; it is not a `u64`.
 */
export interface LeaderLostEvent {
  readonly type: 'leader_lost';
  /** The generation whose in-flight work just died, exact decimal. */
  readonly generation: string;
  readonly failed: number;
}

/**
 * This tab refused a message from a superseded generation.
 *
 * The fence firing, surfaced rather than swallowed: a page debugging a
 * resumed or restored tab needs to see the refusal, and a silent drop
 * looks exactly like a lost message.
 */
export interface GenerationFencedEvent {
  readonly type: 'generation_fenced';
  /** What the sender presented, exact decimal. */
  readonly presented: string;
  /** What is in force, exact decimal. */
  readonly current: string;
}

/**
 * This tab discovered it was superseded and stood down.
 *
 * The other side of {@link GenerationFencedEvent}: not "somebody sent
 * me a stale message" but "I am the stale one".
 */
export interface NotLeaderEvent {
  readonly type: 'not_leader';
  /** The generation this tab held, exact decimal. */
  readonly presented: string;
  /** The generation now in force, exact decimal. */
  readonly current: string;
}

/**
 * This tab was granted the lock and then failed to re-bootstrap its
 * node.
 *
 * It is a follower again, re-declared to whoever takes the lock next,
 * and it will ask for the lock again shortly — the origin has no node
 * after a failed promotion, so somebody has to keep trying. Surfaced
 * rather than logged because "the page briefly had no node and you
 * could not see why" is the symptom this used to produce.
 */
export interface PromotionFailedEvent {
  readonly type: 'promotion_failed';
  /** The generation the failed acquisition had been allocated. */
  readonly generation: string;
  /** The typed failure, as its message. */
  readonly detail: string;
}

/** The six lifecycle tags a session adds. */
export type SessionLifecycleEvent =
  | LeaderChangedEvent
  | SubscriptionRestoredEvent
  | LeaderLostEvent
  | GenerationFencedEvent
  | NotLeaderEvent
  | PromotionFailedEvent;

/** Everything a session can deliver. */
export type SessionEvent = LeafEvent | SessionLifecycleEvent;

/** `SessionEvent` narrowed by its tag. */
export type SessionEventOf<T extends SessionEvent['type']> = Extract<SessionEvent, { type: T }>;

/**
 * Parse one event JSON string from a session.
 *
 * Never throws: this runs inside a callback the wasm module invokes,
 * and an exception there would unwind through Rust. Anything that is
 * not one of the five lifecycle tags goes to `parseEvent`, so the node
 * events and the unknown-tag passthrough behave exactly as they do on
 * the direct surface.
 */
export function parseSessionEvent(json: string): SessionEvent {
  const base = parseEvent(json);
  if (base.type !== 'unknown') return base;
  const refined = refine(base);
  return refined ?? base;
}

/**
 * `true` for the six tags a session adds, so a page can filter the
 * lifecycle out of the event stream without listing them.
 */
export function isLifecycleEvent(event: SessionEvent): event is SessionLifecycleEvent {
  switch (event.type) {
    case 'leader_changed':
    case 'subscription_restored':
    case 'leader_lost':
    case 'generation_fenced':
    case 'not_leader':
    case 'promotion_failed':
      return true;
    default:
      return false;
  }
}

/**
 * Refine an event `parseEvent` did not recognise.
 *
 * `raw` is whatever `parseJsonPreservingU64` produced, so the exact
 * decimal strings for `generation`, `presented` and `current` are
 * already intact — they are in `U64_EVENT_KEYS`, which is what keeps
 * a large generation from being rounded into a fence that no longer
 * fences.
 */
function refine(event: UnknownEvent): SessionLifecycleEvent | null {
  const raw = event.raw;
  if (raw === null || typeof raw !== 'object') return null;
  const fields: Record<string, unknown> = { ...raw };

  switch (event.tag) {
    case 'leader_changed':
      return { type: 'leader_changed', generation: exact(fields.generation) };
    case 'subscription_restored':
      return {
        type: 'subscription_restored',
        channel: typeof fields.channel === 'string' ? fields.channel : '',
      };
    case 'leader_lost': {
      // `failed` is a `usize` count, not a `u64` id: a number is
      // exact for it, and it is deliberately not in `U64_EVENT_KEYS`.
      const failed = Number(fields.failed);
      return {
        type: 'leader_lost',
        generation: exact(fields.generation),
        failed: Number.isFinite(failed) ? failed : 0,
      };
    }
    case 'generation_fenced':
      return {
        type: 'generation_fenced',
        presented: exact(fields.presented),
        current: exact(fields.current),
      };
    case 'not_leader':
      return {
        type: 'not_leader',
        presented: exact(fields.presented),
        current: exact(fields.current),
      };
    case 'promotion_failed':
      return {
        type: 'promotion_failed',
        generation: exact(fields.generation),
        detail: typeof fields.detail === 'string' ? fields.detail : '',
      };
    default:
      return null;
  }
}

/**
 * A `u64` field, kept exactly as it arrived.
 *
 * Five call sites that must agree: a generation read as a number
 * anywhere is a generation that rounds above 2^53, and a rounded
 * generation is a fence that silently stops fencing.
 */
function exact(value: unknown): string {
  if (typeof value === 'string') return value;
  if (typeof value === 'number' && Number.isFinite(value)) return String(value);
  return '0';
}
