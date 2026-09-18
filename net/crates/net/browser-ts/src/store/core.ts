/**
 * The local half of a store: state, subscriptions, status and the
 * synchronous transaction.
 *
 * This is deliberately transport-free. Both the authoritative host and
 * a joined replica are this object plus a network layer, which is why
 * every rule that does *not* need a peer lives here and is testable
 * without a mesh: immutability, identity preservation, selector
 * equality, notification counting, and the transaction contract.
 *
 * **The transaction contract is the sharp part.** A handler runs
 * synchronously, stages changes, and either commits one revision or
 * discards everything:
 *
 * - `getState()` inside a handler reads the *staged* value, so a
 *   handler sees its own writes.
 * - A throw discards the staged changes. Nothing is published.
 * - Returning a thenable is refused, because an `async` handler would
 *   commit after its transaction closed — the TypeScript signature
 *   already says `undefined`, and JavaScript callers bypass types.
 * - The context is valid only for that transaction. A retained context
 *   cannot write from a later microtask.
 * - Read-only callbacks (validators, `authorize`, `project`, update
 *   functions) may not reenter a setter.
 * - A public owner `setState` called synchronously *inside* a handler
 *   joins the same staged transaction rather than committing beside it,
 *   so a handler's rollback cannot be defeated by going through the
 *   public handle.
 */

import { StoreError } from './errors.js';
import { mergeShallow, reconcile } from './state.js';
import { applyPatch, type PatchOutcome } from './patch.js';
import type { WireOp } from './wire.js';
import type {
  Cancel,
  ReadonlyState,
  SelectorOptions,
  StateUpdate,
  StoreDefinition,
  StorePhase,
  StoreStatus,
  ActionContext,
  ActionSpec,
  InputSpec,
} from './types.js';

/** One fan-out target: a root listener or a selector subscription. */
interface Subscription<S extends object> {
  notify(next: S, previous: S): void;
}

interface Transaction<S extends object> {
  staged: S;
  active: boolean;
}

/**
 * The transport-free store.
 *
 * `S` is the validated state shape. The definition is held for its
 * `state` validator, which every commit passes through — an owner
 * cannot publish state its own schema rejects.
 */
export class StoreCore<S extends object, A extends ActionSpec, I extends InputSpec> {
  readonly #definition: StoreDefinition<S, A, I>;
  readonly #subscriptions = new Set<Subscription<S>>();
  readonly #statusListeners = new Set<
    (status: StoreStatus, previous: StoreStatus) => void
  >();

  #state: S;
  #status: StoreStatus;
  #revision = 0;
  #transaction: Transaction<S> | null = null;
  #readOnlyDepth = 0;
  #closed = false;

  constructor(options: {
    definition: StoreDefinition<S, A, I>;
    initialState: S;
    phase?: StorePhase;
  }) {
    this.#definition = options.definition;
    // Validate and freeze once at construction: from here on the only
    // way in is a commit, and every commit reconciles against this.
    this.#state = reconcile(undefined, this.#validate(options.initialState));
    this.#status = Object.freeze({
      phase: options.phase ?? 'ready',
      stale: false,
      error: null,
    });
  }

  /** The current immutable snapshot. Never initiates I/O. */
  getState(): ReadonlyState<S> {
    return this.#state as ReadonlyState<S>;
  }

  /** Monotone count of applied transactions. Unchanged updates do not move it. */
  get revision(): number {
    return this.#revision;
  }

  subscribe(
    listener: (state: ReadonlyState<S>, previous: ReadonlyState<S>) => void,
  ): Cancel;
  subscribe<T>(
    selector: (state: ReadonlyState<S>) => T,
    listener: (selected: T, previous: T) => void,
    options?: SelectorOptions<T>,
  ): Cancel;
  subscribe<T>(
    first:
      | ((state: ReadonlyState<S>, previous: ReadonlyState<S>) => void)
      | ((state: ReadonlyState<S>) => T),
    second?: (selected: T, previous: T) => void,
    options?: SelectorOptions<T>,
  ): Cancel {
    const subscription =
      second === undefined
        ? this.#rootSubscription(
            first as (state: ReadonlyState<S>, previous: ReadonlyState<S>) => void,
          )
        : this.#selectorSubscription(
            first as (state: ReadonlyState<S>) => T,
            second,
            options,
          );

    this.#subscriptions.add(subscription);
    let live = true;
    return () => {
      if (!live) return;
      live = false;
      this.#subscriptions.delete(subscription);
    };
  }

  getStatus(): StoreStatus {
    return this.#status;
  }

  subscribeStatus(
    listener: (status: StoreStatus, previous: StoreStatus) => void,
  ): Cancel {
    this.#statusListeners.add(listener);
    let live = true;
    return () => {
      if (!live) return;
      live = false;
      this.#statusListeners.delete(listener);
    };
  }

  /**
   * Move readiness. A transition to an identical status is not a
   * transition: the object keeps its identity and nobody is notified.
   */
  setStatus(patch: Partial<StoreStatus>): void {
    const previous = this.#status;
    const candidate: StoreStatus = {
      phase: patch.phase ?? previous.phase,
      stale: patch.stale ?? previous.stale,
      error: patch.error === undefined ? previous.error : patch.error,
    };
    if (
      candidate.phase === previous.phase &&
      candidate.stale === previous.stale &&
      Object.is(candidate.error, previous.error)
    ) {
      return;
    }
    this.#status = Object.freeze(candidate);
    for (const listener of [...this.#statusListeners]) {
      dispatch(() => listener(this.#status, previous));
    }
  }

  /**
   * Run one handler as a transaction and commit its staged changes.
   *
   * Returns whatever the handler returned. A throw propagates with
   * nothing published.
   */
  transact<R>(run: (context: ActionContext<S>) => R, peer: string): R {
    this.#refuseWhenClosed();
    if (this.#transaction !== null) {
      throw new StoreError(
        'action-rejected',
        'a store transaction is already open on this store; handlers are synchronous and must not nest',
      );
    }

    const transaction: Transaction<S> = { staged: this.#state, active: true };
    this.#transaction = transaction;
    try {
      const result = run(this.#context(transaction, peer));
      if (isThenable(result)) {
        throw new StoreError(
          'action-rejected',
          'a store handler returned a thenable; handlers are synchronous so that a throw can discard the whole transaction',
        );
      }
      this.#commit(transaction.staged);
      return result;
    } finally {
      // Invalidate before clearing, so a context captured by the
      // handler is dead even if the handler stashed it somewhere.
      transaction.active = false;
      this.#transaction = null;
    }
  }

  /**
   * The owner's public write. Inside a handler it joins that handler's
   * transaction; outside one it is its own single-revision transaction.
   */
  applyOwnerUpdate(update: StateUpdate<S>): void {
    this.#refuseWhenClosed();
    const open = this.#transaction;
    if (open !== null && open.active) {
      open.staged = this.#stage(open.staged, update);
      return;
    }
    const staged = this.#stage(this.#state, update);
    this.#commit(staged);
  }

  /**
   * Apply a full snapshot: validate, then reconcile.
   *
   * An equivalent snapshot is a no-op — same root reference, no
   * notifications — which is what stops a periodic resynchronization
   * from looking like a world change to every selector.
   */
  applySnapshot(raw: unknown): void {
    this.#refuseWhenClosed();
    this.#commit(this.#validate(raw));
  }

  /**
   * Apply a replica delta: the owner's patch operations, at this
   * document, validated by this definition.
   *
   * This lives here rather than in the replica because the store owns
   * the document, its validator and its revision. A replica that read
   * the state out, patched it outside and pushed the result back would
   * validate twice per delta and would be free to publish something
   * the definition never saw.
   *
   * Refusals are returned, not thrown: a delta that does not apply is
   * a recoverable divergence (§1.8 resynchronization), not a caller
   * error.
   */
  applyDelta(ops: readonly WireOp[]): PatchOutcome<S> {
    this.#refuseWhenClosed();
    const outcome = applyPatch(this.#state, ops, raw => this.#validate(raw));
    if (outcome.ok && outcome.changed) this.#commit(outcome.next);
    return outcome;
  }

  /**
   * Run a read-only callback with setters fenced off.
   *
   * The host layer routes `authorize` and `project` through this, so a
   * callback that is documented as read-only cannot quietly mutate.
   */
  runReadOnly<R>(run: () => R): R {
    this.#readOnlyDepth += 1;
    try {
      return run();
    } finally {
      this.#readOnlyDepth -= 1;
    }
  }

  /** Fence the handle: closed, stale, no listeners retained. */
  close(): void {
    if (this.#closed) return;
    this.#closed = true;
    this.setStatus({ phase: 'closed', stale: true });
    this.#subscriptions.clear();
    this.#statusListeners.clear();
  }

  #context(transaction: Transaction<S>, peer: string): ActionContext<S> {
    return {
      peer,
      getState: () => {
        this.#refuseEscapedContext(transaction);
        return transaction.staged as ReadonlyState<S>;
      },
      setState: (update: StateUpdate<S>) => {
        this.#refuseEscapedContext(transaction);
        if (this.#readOnlyDepth > 0) {
          throw new StoreError(
            'action-rejected',
            'a read-only store callback may not write state; validators, authorize, project and update functions run with setters fenced',
          );
        }
        transaction.staged = this.#stage(transaction.staged, update);
      },
    };
  }

  #stage(from: S, update: StateUpdate<S>): S {
    const patch =
      typeof update === 'function'
        ? this.runReadOnly(() => update(from as ReadonlyState<S>))
        : update;
    return mergeShallow(from, patch);
  }

  #commit(candidate: S): void {
    const validated = this.#validate(candidate);
    const next = reconcile(this.#state, validated);
    if (Object.is(next, this.#state)) return;

    const previous = this.#state;
    this.#state = next;
    this.#revision += 1;
    for (const subscription of [...this.#subscriptions]) {
      subscription.notify(next, previous);
    }
  }

  #validate(raw: unknown): S {
    try {
      return this.runReadOnly(() => this.#definition.state(raw));
    } catch (error) {
      if (error instanceof StoreError) throw error;
      throw new StoreError(
        'invalid-data',
        `state failed the '${this.#definition.id}' validator: ${String(error)}`,
        { cause: error },
      );
    }
  }

  #rootSubscription(
    listener: (state: ReadonlyState<S>, previous: ReadonlyState<S>) => void,
  ): Subscription<S> {
    return {
      notify: (next, previous) => {
        dispatch(() =>
          listener(next as ReadonlyState<S>, previous as ReadonlyState<S>),
        );
      },
    };
  }

  #selectorSubscription<T>(
    selector: (state: ReadonlyState<S>) => T,
    listener: (selected: T, previous: T) => void,
    options: SelectorOptions<T> | undefined,
  ): Subscription<S> {
    const equality = options?.equality ?? Object.is;
    let last = this.runReadOnly(() => selector(this.#state as ReadonlyState<S>));
    if (options?.fireImmediately === true) {
      dispatch(() => listener(last, last));
    }
    return {
      notify: next => {
        const selected = this.runReadOnly(() => selector(next as ReadonlyState<S>));
        const previous = last;
        // An unchanged selection is not an event. This is what makes a
        // 60 Hz render loop cheap, and it is asserted, not assumed.
        if (equality(selected, previous)) return;
        last = selected;
        dispatch(() => listener(selected, previous));
      },
    };
  }

  #refuseEscapedContext(transaction: Transaction<S>): void {
    if (transaction.active) return;
    throw new StoreError(
      'action-rejected',
      'this store transaction has already ended; its context cannot read or write from a later turn',
    );
  }

  #refuseWhenClosed(): void {
    if (!this.#closed) return;
    throw new StoreError('closed', 'this store handle is closed');
  }
}

function isThenable(value: unknown): boolean {
  if (value === null || (typeof value !== 'object' && typeof value !== 'function')) {
    return false;
  }
  return typeof (value as { then?: unknown }).then === 'function';
}

/**
 * Run one listener in isolation.
 *
 * A listener's bug is not the store's: report it where a page can see
 * it and keep the remaining listeners — and the cleanup that follows
 * them — running. Same convention as the event hub in `events.ts`.
 */
function dispatch(deliver: () => void): void {
  try {
    deliver();
  } catch (error) {
    console.error('[@net-mesh/browser] store listener threw', error);
  }
}
