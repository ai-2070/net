/**
 * The store's public type contract.
 *
 * These are the declarations from
 * `docs/internal/plans/BROWSER_GAME_STORE_API_DESIGN.md` §2, moved into
 * the package so they are compiled rather than merely written down — but
 * compiled is all that binds them: the shipped handles are
 * `HostedStoreHandle` (`host.ts`) and `JoinedStoreHandle` (`join.ts`), and
 * `OperationOptions`, `StoreLimits`, `HostedStore` and `JoinedStore` below
 * have no implementor (`HostedStoreHandle.setState(next)` full-replaces;
 * `joinStore` returns `JoinedStoreHandle` synchronously with
 * `act(name, input)` / `input(name, value)`, no options).
 *
 * Two of them encode a review disposition and must not be "simplified":
 *
 * - {@link HostedStore} has `setState`; {@link JoinedStore} does **not**.
 *   A replica cannot write, and that is a type-level fact, not a runtime
 *   check a caller can be talked out of.
 * - The projection callback on the host returns a validated `S`, never a
 *   `Partial<S>`. Visibility is expressed **in the schema** — collections
 *   omit invisible entities, individually hidden fields are explicit
 *   `null` or a tagged value, and `empty()` means absence. A zero must
 *   never be readable as "you cannot see this".
 */

import type { StoreError } from './errors.js';
import type { Visibility } from './visibility.js';

/** Deeply-readonly view of validated plain state. */
export type ReadonlyState<T> = T extends readonly (infer V)[]
  ? readonly ReadonlyState<V>[]
  : T extends object
    ? { readonly [K in keyof T]: ReadonlyState<T[K]> }
    : T;

/**
 * A validator: total on accepted input, throwing otherwise.
 *
 * Deliberately the shape of `schema.parse` so Zod/Valibot/hand-written
 * checks are all usable without the store depending on any of them.
 */
export type Parse<T> = (value: unknown) => T;

/** A subscription disposer. Calling it more than once is harmless. */
export type Cancel = () => void;

/** Declared actions: acknowledged, validated request/response pairs. */
export type ActionSpec = Record<string, { input: unknown; output: unknown }>;

/** Declared latest-value inputs: coalesced intent, no per-call promise. */
export type InputSpec = Record<string, unknown>;

/** A reusable typed description of a store. Carries no state or connection. */
export interface StoreDefinition<
  S extends object,
  A extends ActionSpec,
  I extends InputSpec,
> {
  readonly id: string;
  readonly version: number;
  readonly state: Parse<S>;
  /**
   * The value that represents **absence** — no visible entities, no
   * visible fields. Not a plausible set of game values: a reader that
   * cannot see the ship must not receive a ship pointing north.
   */
  readonly empty: () => S;
  readonly actions: {
    readonly [K in keyof A]: {
      readonly input: Parse<A[K]['input']>;
      readonly output: Parse<A[K]['output']>;
    };
  };
  readonly inputs: { readonly [K in keyof I]: Parse<I[K]> };
  /**
   * What each player may see, declared: `'open'`, a preset, or path →
   * rule. Enforced by the host after any hand-written projection. See
   * `visibility.ts`.
   */
  readonly visibility?: Visibility;
}

/** Where a handle is in its lifecycle. Kept out of game state. */
export type StorePhase =
  | 'connecting'
  | 'syncing'
  | 'ready'
  | 'reconnecting'
  | 'failed'
  | 'closed';

/** Readiness, separate from the state itself. */
export interface StoreStatus {
  readonly phase: StorePhase;
  /** Retained state is no longer known-current. Not "empty world". */
  readonly stale: boolean;
  readonly error: StoreError | null;
}

/** Equality and delivery options for a selector subscription. */
export interface SelectorOptions<T> {
  readonly equality?: (a: T, b: T) => boolean;
  readonly fireImmediately?: boolean;
}

/** The read surface both the owner and a replica expose. */
export interface StoreReader<S extends object> {
  getState(): ReadonlyState<S>;
  subscribe(
    listener: (state: ReadonlyState<S>, previous: ReadonlyState<S>) => void,
  ): Cancel;
  subscribe<T>(
    selector: (state: ReadonlyState<S>) => T,
    listener: (selected: T, previous: T) => void,
    options?: SelectorOptions<T>,
  ): Cancel;
  getStatus(): StoreStatus;
  subscribeStatus(
    listener: (status: StoreStatus, previous: StoreStatus) => void,
  ): Cancel;
}

/** Per-call deadline and cancellation. */
export interface OperationOptions {
  readonly timeoutMs?: number;
  readonly signal?: AbortSignal;
}

/** What happened to a latest-value input locally. Not remote acceptance. */
export type InputDisposition =
  | { readonly type: 'queued' | 'replaced' }
  | { readonly type: 'dropped'; readonly reason: 'not-ready' | 'capacity' };

/** An owner state update: a shallow patch, or a function producing one. */
export type StateUpdate<S extends object> =
  | Partial<S>
  | ((state: ReadonlyState<S>) => Partial<S>);

/**
 * The handle a handler gets for the duration of **one synchronous
 * transaction**. Valid only during that transaction: returning,
 * throwing or handing back a thenable invalidates it, so a retained
 * context cannot write from a later microtask.
 */
export interface ActionContext<S extends object> {
  /** The authenticated caller's 16-lowercase-hex mesh node id. */
  readonly peer: string;
  getState(): ReadonlyState<S>;
  setState(update: StateUpdate<S>): void;
}

/** What the owner's `authorize` is asked to rule on. */
export type AccessRequest<A extends ActionSpec, I extends InputSpec> =
  | { readonly type: 'read'; readonly peer: string; readonly audience: readonly string[] }
  | {
      [K in keyof A]: {
        readonly type: 'action';
        readonly peer: string;
        readonly name: K;
        readonly input: A[K]['input'];
      };
    }[keyof A]
  | {
      [K in keyof I]: {
        readonly type: 'input';
        readonly peer: string;
        readonly name: K;
        readonly input: I[K];
      };
    }[keyof I];

/** Declared bounds. The host enforces its own; callers cannot raise them. */
export interface StoreLimits {
  readonly maxSnapshotBytes?: number;
  readonly maxMessageBytes?: number;
  readonly maxPendingActions?: number;
  readonly maxAudiences?: number;
}

/** The authoritative instance. The only handle with `setState`. */
export interface HostedStore<S extends object> extends StoreReader<S> {
  readonly authority: string;
  setState(update: StateUpdate<S>): void;
  close(): Promise<void>;
}

/** A subscribed replica. Writes go through `actions` / `inputs`, never `setState`. */
export interface JoinedStore<
  S extends object,
  A extends ActionSpec,
  I extends InputSpec,
> extends StoreReader<S> {
  readonly actions: {
    readonly [K in keyof A]: (
      input: A[K]['input'],
      options?: OperationOptions,
    ) => Promise<A[K]['output']>;
  };
  readonly inputs: {
    readonly [K in keyof I]: (input: I[K]) => InputDisposition;
  };
  setAudience(names: readonly string[], options?: OperationOptions): Promise<void>;
  close(): Promise<void>;
}
