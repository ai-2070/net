/**
 * `hostPlayer` — the hosting node's own player, as a replica-shaped
 * handle.
 *
 * A node cannot join its own store: `openStream({peer})` needs a
 * session with that peer and a node has none with itself, so
 * `joinStore({ host: self })` is refused `invalid-data`. Every game
 * whose host is also a player therefore wrote the same wrapper — the
 * fleet demo's was forty lines — and each one had to remember to hold
 * the host's player to the same `authorize` a replica gets.
 *
 * This is that wrapper, made part of the store, with parity as its
 * contract rather than a habit:
 *
 * - **The same policy.** Reads, actions and inputs go to the host's
 *   `authorize` with the host's own node id as `peer`, in the same
 *   16-hex spelling a replica's frames are handed.
 * - **The same transaction.** Input parse, handler, output validation
 *   and the result's message budget run in one transaction on the
 *   owner, exactly as for a replica's `act` after the wire
 *   (`StoreOwner.localAct`), and the change reaches every replica.
 * - **The same values.** Inputs are refused and normalized by the
 *   wire's own rules (`encodeMessage`, then `parseStoreJson`), so a
 *   value that works for the host works for its players.
 * - **The same view.** `getState()` is the projection for this
 *   handle's audience, not the authoritative document: game code that
 *   renders from a replica renders the same thing on the host.
 *
 * What is NOT the same, deliberately: there is no wire, so no handle,
 * lease, sequence or replay ledger, and no `reconnect` to perform.
 * Calls still settle asynchronously, as a replica's do, so a handler
 * never runs nested inside the caller's own code.
 *
 * **The trust boundary is unchanged.** The hosting player's page holds
 * the authoritative document; the projection here keeps the host's
 * own UI honest, it does not hide anything from a player who opens the
 * developer tools. Games with real stakes need a dedicated host.
 */

import { StoreCore } from './core.js';
import { StoreError } from './errors.js';
import { hostInternals, type HostedStoreHandle } from './host.js';
import type { JoinedStoreHandle } from './join.js';
import { parseStoreJson, type JsonValue } from './json.js';
import type {
  ActionSpec,
  InputDisposition,
  InputSpec,
  ReadonlyState,
  SelectorOptions,
  StoreDefinition,
} from './types.js';
import { encodeMessage, type Hex } from './wire.js';

/** What the host's own player reads as. */
export interface HostPlayerOptions {
  /**
   * The audience this player reads, as a replica's `joinStore({
   * audience })`. Authorized by the host's policy like any other read.
   */
  readonly audience: readonly string[];
}

const PLACEHOLDER_Q = '0'.repeat(16) as Hex;
const PLACEHOLDER_H = '0'.repeat(32) as Hex;

/**
 * The wire's admission for one caller value: refused exactly as a
 * replica's `encodeMessage` refuses it, and parsed back exactly as the
 * owner's parser hands it to a handler.
 */
function admit(name: string, value: unknown, kind: 'act' | 'in'): JsonValue {
  let frame: string;
  try {
    frame =
      kind === 'act'
        ? encodeMessage({ k: 'act', q: PLACEHOLDER_Q, h: PLACEHOLDER_H, s: '1', name, in: value as never })
        : encodeMessage({ k: 'in', h: PLACEHOLDER_H, s: '1', name, in: value as never });
  } catch (error) {
    throw new StoreError('invalid-data', `the input for '${name}' cannot be encoded`, { cause: error });
  }
  const parsed = parseStoreJson(frame);
  if (!parsed.ok || typeof parsed.value !== 'object' || parsed.value === null || Array.isArray(parsed.value)) {
    throw new StoreError('invalid-data', `the input for '${name}' cannot be encoded`);
  }
  return (parsed.value as { readonly in: JsonValue }).in;
}

/** A value as a replica would receive it: JSON, detached from the host's objects. */
function detach(value: unknown): unknown {
  const parsed = parseStoreJson(JSON.stringify(value));
  return parsed.ok ? parsed.value : value;
}

/**
 * The hosting node's own player on `host`.
 *
 * ```ts
 * const host = hostStore({ definition: world, transport: node, … });
 * const me = hostPlayer(host, { audience: ['crew'] });
 * await me.ready();
 * await me.act('enlist', { colour: 2 });   // same authorize, same handler
 * ```
 *
 * Game code can then treat `me` and a `joinStore` replica the same way.
 * Throws `invalid-data` for a handle `hostStore` did not return.
 */
export function hostPlayer<S extends object, A extends ActionSpec, I extends InputSpec>(
  host: HostedStoreHandle<S, A, I>,
  options: HostPlayerOptions,
): JoinedStoreHandle<S, A, I> {
  const inner = hostInternals<S, A, I>(host);
  if (inner === undefined) {
    throw new StoreError('invalid-data', 'hostPlayer needs a handle returned by hostStore');
  }
  const { owner, peer } = inner;
  const definition: StoreDefinition<S, A, I> = host.definition;
  const view = new StoreCore<S, A, I>({ definition, initialState: definition.empty(), phase: 'syncing' });

  let audience: readonly string[] = [...options.audience];
  let closed = false;
  /** Terminal: the read was refused, the projection failed, or the host closed. */
  let ended = false;
  const pendingInputs = new Map<string, JsonValue>();

  function end(phase: 'failed' | 'closed', error: StoreError | null): void {
    ended = true;
    // As a replica does on a terminal refusal: the view is cleared, so
    // a revoked read stops showing what it no longer may.
    view.applySnapshot(definition.empty());
    view.setStatus({ phase, stale: false, error });
  }

  /** Re-read the view — authorized afresh, as a replica's feed is. */
  function refresh(): void {
    if (closed || ended) return;
    if (inner!.isClosed()) {
      end('closed', null);
      return;
    }
    if (!owner.localRead(peer, audience)) {
      end('failed', new StoreError('forbidden', 'the store refused: forbidden'));
      return;
    }
    const projected = owner.localView(peer, audience);
    if (projected === null) {
      end('failed', new StoreError('capacity', 'the store refused: capacity'));
      return;
    }
    view.applySnapshot(projected);
    view.setStatus({ phase: 'ready', stale: false, error: null });
  }

  const stopWatching = owner.subscribe(() => {
    refresh();
  });
  inner.onClose(() => {
    stopWatching();
    if (!closed && !ended) end('closed', null);
  });
  refresh();

  function refusal(): StoreError | null {
    if (closed) return new StoreError('closed', 'this store handle is closed');
    if (inner!.isClosed()) return new StoreError('owner-lost', 'the store owner closed this handle');
    if (ended) return view.getStatus().error ?? new StoreError('owner-lost', 'the store owner closed this handle');
    return null;
  }

  function flushInputs(): void {
    const batch = [...pendingInputs];
    pendingInputs.clear();
    if (refusal() !== null) return;
    for (const [name, value] of batch) inner!.dispatched(owner.localInput(name, value, peer));
  }

  function subscribe(
    first: (state: ReadonlyState<S>, previous: ReadonlyState<S>) => void,
  ): () => void;
  function subscribe<T>(
    selector: (state: ReadonlyState<S>) => T,
    listener: (selected: T, previous: T) => void,
    options?: SelectorOptions<T>,
  ): () => void;
  function subscribe<T>(
    first: ((state: ReadonlyState<S>, previous: ReadonlyState<S>) => void) | ((state: ReadonlyState<S>) => T),
    second?: (selected: T, previous: T) => void,
    selectorOptions?: SelectorOptions<T>,
  ): () => void {
    return second === undefined
      ? view.subscribe(first as (state: ReadonlyState<S>, previous: ReadonlyState<S>) => void)
      : view.subscribe(first as (state: ReadonlyState<S>) => T, second, selectorOptions);
  }

  return {
    getState: () => view.getState(),
    subscribe,
    getStatus: () => view.getStatus(),
    subscribeStatus: listener => view.subscribeStatus(listener),
    ready: () => {
      const status = view.getStatus();
      if (status.phase === 'ready') return Promise.resolve();
      return Promise.reject(
        status.error ?? new StoreError(closed ? 'closed' : 'owner-lost', 'this store handle is not ready'),
      );
    },
    act: async (name, input) => {
      const early = refusal();
      if (early !== null) throw early;
      const value = admit(name, input, 'act');
      // A turn later, as a replica's answer is: a handler never runs
      // inside the caller's own frame — including inside another
      // handler, where it would nest a transaction.
      await Promise.resolve();
      const late = refusal();
      if (late !== null) throw late;
      const { outcome, dispatched } = owner.localAct(name, value, peer);
      inner.dispatched(dispatched);
      if (outcome.kind === 'refusal') {
        throw new StoreError(outcome.code, `the store refused: ${outcome.code}`);
      }
      return detach(outcome.out) as never;
    },
    input: (name, value) => {
      if (refusal() !== null || view.getStatus().phase !== 'ready') {
        return { type: 'dropped', reason: 'not-ready' } satisfies InputDisposition;
      }
      const admitted = admit(name, value, 'in');
      // Newest wins, as a replica's inputs do: one pending value per
      // name, applied a turn later.
      const idle = pendingInputs.size === 0;
      const replaced = pendingInputs.has(name);
      pendingInputs.set(name, admitted);
      if (idle) queueMicrotask(flushInputs);
      return { type: replaced ? 'replaced' : 'queued' };
    },
    setAudience: async names => {
      const early = refusal();
      if (early !== null) throw early;
      try {
        encodeMessage({ k: 'aud', q: PLACEHOLDER_Q, h: PLACEHOLDER_H, aud: [...names] });
      } catch (error) {
        throw new StoreError('invalid-data', 'the audience cannot be encoded', { cause: error });
      }
      await Promise.resolve();
      const late = refusal();
      if (late !== null) throw late;
      if (!owner.localRead(peer, names)) {
        throw new StoreError('forbidden', 'the store refused: forbidden');
      }
      audience = [...names];
      refresh();
    },
    // Nothing to resume: there is no session under this handle.
    reconnect: () => Promise.resolve(),
    close: () => {
      if (!closed) {
        closed = true;
        pendingInputs.clear();
        stopWatching();
        view.close();
      }
      return Promise.resolve();
    },
  };
}
