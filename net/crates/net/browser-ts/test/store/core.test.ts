/**
 * The local store contract: notifications, selector equality, status,
 * and the synchronous transaction.
 *
 * The transaction cases are the ones worth having. Each corresponds to
 * a way a handler can break the "one synchronous transaction, all or
 * nothing" rule, and each is refused rather than half-applied:
 * throwing discards, a thenable is refused, a retained context is
 * dead, a read-only callback cannot write, and the public owner
 * setter joins an open transaction instead of committing beside it.
 */

import { describe, expect, it, vi } from 'vitest';

import { StoreCore } from '../../src/store/core.js';
import { defineStore } from '../../src/store/definition.js';
import { StoreError } from '../../src/store/errors.js';
import type {
  ActionContext,
  JoinedStore,
  ReadonlyState,
} from '../../src/store/types.js';

interface Ship {
  readonly heading: number;
  readonly sail: number;
  readonly shots: number;
}
interface ShipState {
  readonly ship: Ship | null;
}
type ShipActions = { fire: { input: { cannon: string }; output: { shot: number } } };
type ShipInputs = { helm: { heading: number } };

function asRecord(value: unknown): Record<string, unknown> {
  if (typeof value !== 'object' || value === null || Array.isArray(value)) {
    throw new Error('expected record');
  }
  return value as Record<string, unknown>;
}

const ship = defineStore<ShipState, ShipActions, ShipInputs>({
  id: 'pirate.ship',
  version: 1,
  empty: () => ({ ship: null }),
  state(value) {
    const v = asRecord(value);
    if (v.ship === null || v.ship === undefined) return { ship: null };
    const s = asRecord(v.ship);
    for (const key of ['heading', 'sail', 'shots']) {
      if (typeof s[key] !== 'number' || !Number.isFinite(s[key])) {
        throw new Error(`ship.${key} must be a finite number`);
      }
    }
    return {
      ship: {
        heading: s.heading as number,
        sail: s.sail as number,
        shots: s.shots as number,
      },
    };
  },
  actions: {
    fire: {
      input: value => ({ cannon: String(asRecord(value).cannon) }),
      output: value => ({ shot: asRecord(value).shot as number }),
    },
  },
  inputs: {
    helm: value => ({ heading: asRecord(value).heading as number }),
  },
});

function core(): StoreCore<ShipState, ShipActions, ShipInputs> {
  return new StoreCore({
    definition: ship,
    initialState: { ship: { heading: 0, sail: 0, shots: 0 } },
  });
}

describe('notifications', () => {
  it('notifies root listeners once per applied transaction, with no initial call', () => {
    const store = core();
    const seen: number[] = [];
    store.subscribe(state => seen.push(state.ship?.shots ?? -1));

    expect(seen).toEqual([]);
    store.applyOwnerUpdate({ ship: { heading: 0, sail: 0, shots: 1 } });
    store.applyOwnerUpdate({ ship: { heading: 0, sail: 0, shots: 2 } });

    expect(seen).toEqual([1, 2]);
    expect(store.revision).toBe(2);
  });

  it('does not notify or bump the revision for an unchanged update', () => {
    const store = core();
    const listener = vi.fn();
    store.subscribe(listener);

    store.applyOwnerUpdate({});
    store.applyOwnerUpdate({ ship: { heading: 0, sail: 0, shots: 0 } });

    expect(listener).not.toHaveBeenCalled();
    expect(store.revision).toBe(0);
  });

  it('does not notify a selector whose result is unchanged', () => {
    const store = core();
    const heading = vi.fn();
    store.subscribe(state => state.ship?.heading ?? null, heading);

    // `shots` moves; the heading selector must stay quiet. This is the
    // fourth reference-stability witness.
    store.applyOwnerUpdate({ ship: { heading: 0, sail: 0, shots: 1 } });
    expect(heading).not.toHaveBeenCalled();

    store.applyOwnerUpdate({ ship: { heading: 90, sail: 0, shots: 1 } });
    expect(heading).toHaveBeenCalledTimes(1);
    expect(heading).toHaveBeenCalledWith(90, 0);
  });

  it('fires a selector immediately with the current value as both arguments', () => {
    const store = core();
    const listener = vi.fn();
    store.subscribe(state => state.ship?.sail ?? null, listener, {
      fireImmediately: true,
    });
    expect(listener).toHaveBeenCalledWith(0, 0);
  });

  it('honours an explicit comparator instead of deep equality', () => {
    const store = core();
    const listener = vi.fn();
    // Selector returns a fresh object every time, so `Object.is` would
    // fire on every transaction. The comparator is what makes it quiet.
    store.subscribe(
      state => ({ heading: state.ship?.heading ?? 0 }),
      listener,
      { equality: (a, b) => a.heading === b.heading },
    );

    store.applyOwnerUpdate({ ship: { heading: 0, sail: 1, shots: 0 } });
    expect(listener).not.toHaveBeenCalled();

    store.applyOwnerUpdate({ ship: { heading: 5, sail: 1, shots: 0 } });
    expect(listener).toHaveBeenCalledTimes(1);
  });

  it('keeps other listeners and later cleanup alive when one throws', () => {
    const store = core();
    const errors = vi.spyOn(console, 'error').mockImplementation(() => {});
    const after = vi.fn();
    store.subscribe(() => {
      throw new Error('listener bug');
    });
    store.subscribe(after);

    store.applyOwnerUpdate({ ship: { heading: 1, sail: 0, shots: 0 } });

    expect(after).toHaveBeenCalledTimes(1);
    expect(errors).toHaveBeenCalled();
    errors.mockRestore();
  });

  it('has an idempotent disposer', () => {
    const store = core();
    const listener = vi.fn();
    const stop = store.subscribe(listener);

    stop();
    stop();
    store.applyOwnerUpdate({ ship: { heading: 3, sail: 0, shots: 0 } });
    expect(listener).not.toHaveBeenCalled();
  });
});

describe('status', () => {
  it('keeps one object identity until a real transition, and never fires on subscribe', () => {
    const store = core();
    const listener = vi.fn();
    const first = store.getStatus();
    store.subscribeStatus(listener);

    expect(listener).not.toHaveBeenCalled();
    store.setStatus({ phase: 'ready' });
    expect(listener).not.toHaveBeenCalled();
    expect(store.getStatus()).toBe(first);

    store.setStatus({ phase: 'reconnecting', stale: true });
    expect(listener).toHaveBeenCalledTimes(1);
    expect(store.getStatus().phase).toBe('reconnecting');
    expect(store.getStatus().stale).toBe(true);
  });
});

describe('the synchronous transaction', () => {
  it('lets a handler read its own staged writes and commits one revision', () => {
    const store = core();
    const listener = vi.fn();
    store.subscribe(listener);

    const result = store.transact(ctx => {
      ctx.setState({ ship: { heading: 10, sail: 0, shots: 0 } });
      const staged = ctx.getState().ship;
      ctx.setState({ ship: { heading: staged?.heading ?? 0, sail: 1, shots: 0 } });
      return staged?.heading ?? -1;
    }, 'a1b2c3d4e5f60718');

    expect(result).toBe(10);
    expect(store.getState().ship).toEqual({ heading: 10, sail: 1, shots: 0 });
    // Two setState calls, ONE transaction, one notification.
    expect(listener).toHaveBeenCalledTimes(1);
    expect(store.revision).toBe(1);
  });

  it('discards every staged change when a handler throws', () => {
    const store = core();
    const before = store.getState();
    const listener = vi.fn();
    store.subscribe(listener);

    expect(() =>
      store.transact(ctx => {
        ctx.setState({ ship: { heading: 99, sail: 1, shots: 5 } });
        throw new Error('no powder');
      }, 'a1b2c3d4e5f60718'),
    ).toThrow('no powder');

    expect(store.getState()).toBe(before);
    expect(listener).not.toHaveBeenCalled();
    expect(store.revision).toBe(0);
  });

  it('refuses a handler that returns a thenable, and publishes nothing', () => {
    const store = core();
    const before = store.getState();

    let thrown: unknown;
    try {
      store.transact(ctx => {
        ctx.setState({ ship: { heading: 42, sail: 0, shots: 0 } });
        return Promise.resolve('too late');
      }, 'a1b2c3d4e5f60718');
    } catch (error) {
      thrown = error;
    }

    expect(thrown).toBeInstanceOf(StoreError);
    expect((thrown as StoreError).code).toBe('action-rejected');
    expect((thrown as StoreError).message).toContain('thenable');
    expect(store.getState()).toBe(before);
  });

  it('kills a context that outlives its transaction', () => {
    const store = core();
    // A holder, not a `let`: TypeScript does not track the assignment
    // made inside the callback, and would narrow a plain `let` to
    // `never` after the call.
    const holder: { ctx: ActionContext<ShipState> | null } = { ctx: null };

    store.transact(ctx => {
      holder.ctx = ctx;
      return undefined;
    }, 'a1b2c3d4e5f60718');

    expect(holder.ctx).not.toBeNull();
    let thrown: unknown;
    try {
      holder.ctx?.setState({ ship: { heading: 7, sail: 0, shots: 0 } });
    } catch (error) {
      thrown = error;
    }
    expect(thrown).toBeInstanceOf(StoreError);
    expect((thrown as StoreError).code).toBe('action-rejected');
    expect(store.getState().ship?.heading).toBe(0);
  });

  it('refuses a write from a read-only callback', () => {
    const store = core();
    let thrown: unknown;

    store.transact(ctx => {
      try {
        // The update-function form is read-only; reentering the setter
        // from inside it is the mutation this fence exists for.
        ctx.setState(() => {
          ctx.setState({ ship: { heading: 1, sail: 0, shots: 0 } });
          return {};
        });
      } catch (error) {
        thrown = error;
      }
      return undefined;
    }, 'a1b2c3d4e5f60718');

    expect(thrown).toBeInstanceOf(StoreError);
    expect((thrown as StoreError).code).toBe('action-rejected');
  });

  it('joins an open transaction when the public owner setter is used inside a handler', () => {
    const store = core();
    const listener = vi.fn();
    store.subscribe(listener);

    expect(() =>
      store.transact(ctx => {
        ctx.setState({ ship: { heading: 4, sail: 0, shots: 0 } });
        // Going through the public handle must NOT commit beside the
        // transaction, or a handler's rollback could be defeated by
        // routing the write through `host.setState`.
        store.applyOwnerUpdate({ ship: { heading: 5, sail: 0, shots: 0 } });
        throw new Error('abort');
      }, 'a1b2c3d4e5f60718'),
    ).toThrow('abort');

    expect(listener).not.toHaveBeenCalled();
    expect(store.getState().ship?.heading).toBe(0);
  });

  it('discards a nested full snapshot when the handler throws', () => {
    // `host.setState` reaches the core as a FULL SNAPSHOT through
    // `commit`, so `applySnapshot` is the public-handle path a
    // rollback must not be defeated through — committing beside the
    // open transaction published the handler's world AND then
    // reported `action-rejected` for it.
    const store = core();
    const listener = vi.fn();
    store.subscribe(listener);

    expect(() =>
      store.transact(ctx => {
        ctx.setState({ ship: { heading: 4, sail: 0, shots: 0 } });
        store.applySnapshot({ ship: { heading: 5, sail: 0, shots: 0 } });
        throw new Error('abort');
      }, 'a1b2c3d4e5f60718'),
    ).toThrow('abort');

    expect(listener).not.toHaveBeenCalled();
    expect(store.getState().ship?.heading).toBe(0);
    expect(store.revision).toBe(0);
  });

  it('commits a nested full snapshot with the transaction instead of reverting it', () => {
    const store = core();
    const listener = vi.fn();
    store.subscribe(listener);

    store.transact(ctx => {
      ctx.setState({ ship: { heading: 4, sail: 1, shots: 0 } });
      // Routed beside the transaction, this publishes at once and the
      // transaction's own commit then silently reverts it — after
      // subscribers were told.
      store.applySnapshot({ ship: { heading: 5, sail: 1, shots: 0 } });
      return undefined;
    }, 'a1b2c3d4e5f60718');

    expect(store.getState().ship).toEqual({ heading: 5, sail: 1, shots: 0 });
    expect(listener).toHaveBeenCalledTimes(1);
    expect(store.revision).toBe(1);
  });

  it('refuses a nested transaction', () => {
    const store = core();
    let thrown: unknown;

    store.transact(() => {
      try {
        store.transact(() => undefined, 'a1b2c3d4e5f60718');
      } catch (error) {
        thrown = error;
      }
      return undefined;
    }, 'a1b2c3d4e5f60718');

    expect(thrown).toBeInstanceOf(StoreError);
    expect((thrown as StoreError).code).toBe('action-rejected');
  });

  it('rejects a commit its own schema refuses, and publishes nothing', () => {
    const store = core();
    const before = store.getState();

    let thrown: unknown;
    try {
      // `heading: NaN` is not finite, so the validator refuses it.
      store.applyOwnerUpdate({ ship: { heading: Number.NaN, sail: 0, shots: 0 } });
    } catch (error) {
      thrown = error;
    }

    expect(thrown).toBeInstanceOf(StoreError);
    expect((thrown as StoreError).code).toBe('invalid-data');
    expect(store.getState()).toBe(before);
  });
});

describe('snapshots and close', () => {
  it('treats an equivalent snapshot as no news at all', () => {
    const store = core();
    const listener = vi.fn();
    store.subscribe(listener);
    const before = store.getState();

    store.applySnapshot(JSON.parse(JSON.stringify(before)));

    expect(store.getState()).toBe(before);
    expect(listener).not.toHaveBeenCalled();
    expect(store.revision).toBe(0);
  });

  it('applies a snapshot that differs, preserving unchanged subtrees', () => {
    const store = core();
    store.applySnapshot({ ship: { heading: 33, sail: 0, shots: 0 } });
    expect(store.getState().ship?.heading).toBe(33);
    expect(store.revision).toBe(1);
  });

  it('clears visibility to absence, which IS an observable change', () => {
    const store = core();
    const listener = vi.fn();
    store.subscribe(listener);

    store.applySnapshot(ship.empty());

    // Audience clearing is exempt from the "quiet when unchanged"
    // rule: losing visibility must notify.
    expect(store.getState().ship).toBeNull();
    expect(listener).toHaveBeenCalledTimes(1);
  });

  it('reports closed, stale and frozen state after close, and refuses writes', () => {
    const store = core();
    const statuses: string[] = [];
    store.subscribeStatus(status => statuses.push(status.phase));

    store.close();
    store.close();

    expect(statuses).toEqual(['closed']);
    expect(store.getStatus().stale).toBe(true);
    expect(Object.isFrozen(store.getState())).toBe(true);

    let thrown: unknown;
    try {
      store.applyOwnerUpdate({ ship: null });
    } catch (error) {
      thrown = error;
    }
    expect((thrown as StoreError).code).toBe('closed');
  });
});

describe('defineStore', () => {
  it('refuses a definition whose empty() its own schema rejects', () => {
    let thrown: unknown;
    try {
      defineStore<ShipState, ShipActions, ShipInputs>({
        ...ship,
        // A replica installs this when it loses visibility, so an
        // invalid empty() would fail mid audience-transition.
        empty: () => ({ ship: { heading: Number.NaN, sail: 0, shots: 0 } }),
      });
    } catch (error) {
      thrown = error;
    }
    expect(thrown).toBeInstanceOf(StoreError);
    expect((thrown as StoreError).message).toContain('empty()');
  });

  it('refuses a blank id or a non-integer version', () => {
    expect(() => defineStore({ ...ship, id: '' })).toThrow(StoreError);
    expect(() => defineStore({ ...ship, version: 1.5 })).toThrow(StoreError);
  });
});

/**
 * Compile-time assertions. Never called: each `@ts-expect-error` is
 * the test, and `tsc -p tsconfig.test.json` fails if the error it
 * claims does not occur. Running these lines would only mutate a
 * local — the type system is the subject here, not the runtime.
 */
function typeContract(
  replica: JoinedStore<ShipState, ShipActions, ShipInputs>,
  state: ReadonlyState<ShipState>,
): void {
  // @ts-expect-error a snapshot is deeply readonly
  state.ship = null;

  // @ts-expect-error a replica has no setState; writes go through actions/inputs
  replica.setState({ ship: null });

  // @ts-expect-error 'broadside' is not a declared action
  void replica.actions.broadside({ cannon: 'port' });

  // @ts-expect-error 'fire' takes { cannon: string }, not a number
  void replica.actions.fire(7);

  // @ts-expect-error 'rudder' is not a declared input
  void replica.inputs.rudder({ heading: 0 });

  void state;
}
void typeContract;

describe('the type contract', () => {
  it('hands out state a caller cannot mutate at runtime either', () => {
    const store = core();
    const state = store.getState();

    // Frozen, and this module is ESM, so the assignment throws rather
    // than failing silently the way sloppy mode would.
    expect(Object.isFrozen(state)).toBe(true);
    expect(() => {
      (state as { ship: unknown }).ship = null;
    }).toThrow(TypeError);
  });
});

/** A permissive store for the cancellation rows below. */
const bagDefinition = defineStore<{ readonly tick: number }, Record<string, never>, Record<string, never>>({
  id: 'bag.cancel',
  version: 1,
  state: value => ({ tick: Number((value as { tick?: unknown }).tick ?? 0) }),
  empty: () => ({ tick: 0 }),
  actions: {},
  inputs: {},
});

function bag(): StoreCore<{ readonly tick: number }, Record<string, never>, Record<string, never>> {
  return new StoreCore({ definition: bagDefinition, initialState: { tick: 0 } });
}

describe('a cancellation that has returned is honoured', () => {
  it('a listener cancelled during a notification is not called again', () => {
    const store = bag();
    const seen: number[] = [];
    const cancel = store.subscribe(state => {
      seen.push(state.tick);
      cancel();
    });

    store.applyOwnerUpdate({ tick: 1 });
    store.applyOwnerUpdate({ tick: 2 });

    expect(seen).toEqual([1]);
  });

  it('a listener cancelled by ANOTHER listener mid-notification is not called', () => {
    const store = bag();
    const second = vi.fn();
    const cancelSecond = { current: () => {} };
    store.subscribe(() => cancelSecond.current());
    cancelSecond.current = store.subscribe(second);

    store.applyOwnerUpdate({ tick: 1 });

    expect(second).not.toHaveBeenCalled();
  });

  it('a STATUS listener cancelled by another mid-notification is not called', () => {
    const store = bag();
    const second = vi.fn();
    const cancelSecond = { current: () => {} };
    store.subscribeStatus(() => cancelSecond.current());
    cancelSecond.current = store.subscribeStatus(second);

    store.setStatus({ phase: 'syncing' });

    expect(second).not.toHaveBeenCalled();
  });

  it('cancelling twice does not remove a later identical listener', () => {
    const store = bag();
    const listener = vi.fn();
    const cancel = store.subscribe(listener);
    cancel();
    cancel();

    const again = vi.fn();
    store.subscribe(again);
    store.applyOwnerUpdate({ tick: 1 });

    expect(listener).not.toHaveBeenCalled();
    expect(again).toHaveBeenCalledTimes(1);
  });

  it('a selector subscription cancelled during its own call stays cancelled', () => {
    const store = bag();
    const seen: number[] = [];
    const cancel = store.subscribe(
      state => state.tick,
      value => {
        seen.push(value);
        cancel();
      },
    );

    store.applyOwnerUpdate({ tick: 1 });
    store.applyOwnerUpdate({ tick: 2 });

    expect(seen).toEqual([1]);
  });

  it('a status listener cancelled during its own call stays cancelled', () => {
    const store = bag();
    const seen: string[] = [];
    const cancel = store.subscribeStatus(status => {
      seen.push(status.phase);
      cancel();
    });

    store.setStatus({ phase: 'syncing' });
    store.setStatus({ phase: 'ready' });

    expect(seen).toEqual(['syncing']);
  });

  it('close() cancels every listener', () => {
    const store = bag();
    const listener = vi.fn();
    const status = vi.fn();
    store.subscribe(listener);
    store.subscribeStatus(status);

    store.close();
    status.mockClear();
    expect(() => store.applyOwnerUpdate({ tick: 1 })).toThrow();

    expect(listener).not.toHaveBeenCalled();
    expect(status).not.toHaveBeenCalled();
  });
});
