/**
 * §8's session surface: one node per origin, leader-elected.
 *
 * `connect()` gives a page this tab's node; `openSession()` gives it
 * the origin's node, whichever tab is running it. See
 * {@link MeshSession} for the three methods that are promises here and
 * synchronous on the direct surface, and why.
 */

export { MeshSession, openSession } from './session.js';
export type { SessionOptions, SessionRole } from './session.js';

export { isLifecycleEvent, parseSessionEvent } from './events.js';
export type {
  GenerationFencedEvent,
  LeaderChangedEvent,
  LeaderLostEvent,
  NotLeaderEvent,
  SessionEvent,
  SessionEventOf,
  SessionLifecycleEvent,
  SubscriptionRestoredEvent,
} from './events.js';

export { asSessionModule } from './wasm.js';
export type {
  LeafWasmSession,
  LeafWasmSessionModule,
  LeafWasmSessionOptions,
} from './wasm.js';
