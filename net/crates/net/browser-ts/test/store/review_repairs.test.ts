/**
 * The review round's reproductions, kept.
 *
 * Each row was written before its repair and failed against the head
 * it was written for, so each is a regression witness for a defect
 * that actually shipped: the assembly ceiling measured against
 * arrivals instead of reservations, application callbacks escaping
 * frame dispatch, a `resync` re-projecting without re-authorizing, a
 * stale refusal tearing down a healthy view, and a recovery in flight
 * reporting `ready`.
 *
 * They live together because they are one review round, and because
 * each crosses two modules — which is how they were missed by suites
 * organized one module at a time.
 */

import { describe, expect, it } from 'vitest';

import { AssemblyTable, MAX_ASSEMBLY_BYTES_TOTAL } from '../../src/store/assembly.js';
import { MAX_SNAPSHOT_BYTES } from '../../src/store/wire.js';
import { chunkSnapshot } from '../../src/store/chunker.js';
import { StoreCore } from '../../src/store/core.js';
import { defineStore } from '../../src/store/definition.js';
import { StoreOwner } from '../../src/store/owner.js';
import { StoreReplica } from '../../src/store/replica.js';
import { decodeMessage, encodeMessage, type Hex } from '../../src/store/wire.js';
import type { AccessRequest, StoreDefinition } from '../../src/store/types.js';

const MAX_EVENT_BYTES = 8104;
const PEER = '00000000000000aa';

interface Doc {
  readonly tick: number;
}

function definition(over: Partial<{ state: (v: unknown) => Doc; empty: () => Doc }> = {}) {
  return defineStore<Doc, Record<string, never>, Record<string, never>>({
    id: 'probe',
    version: 1,
    state: over.state ?? (value => ({ tick: Number((value as { tick?: unknown }).tick ?? 0) })),
    empty: over.empty ?? (() => ({ tick: 0 })),
    actions: {},
    inputs: {},
  });
}

function owner(options: {
  definition?: ReturnType<typeof definition>;
  authorize?: (r: AccessRequest<Record<string, never>, Record<string, never>>) => boolean;
  project?: (state: Doc, audience: readonly string[]) => Doc;
  newHandle?: () => Hex;
  newIncarnation?: () => Hex;
} = {}) {
  let handles = 0;
  const store = new StoreOwner<Doc, Record<string, never>, Record<string, never>>({
    store: 'test-store',
    definition: options.definition ?? definition(),
    authorize: options.authorize ?? (() => true),
    project: options.project ?? (state => state),
    maxEventBytes: MAX_EVENT_BYTES,
    now: () => 0,
    newHandle:
      options.newHandle ??
      (() => {
        handles += 1;
        return handles.toString(16).padStart(32, '0') as Hex;
      }),
    newIncarnation: options.newIncarnation ?? (() => 'abcdef0123456789' as Hex),
    canProject: () => true,
    actions: {},
    inputs: {},
  });
  store.commit({ tick: 1 });
  return store;
}

/**
 * An `empty()` that answers the construction-time calls and throws
 * thereafter.
 *
 * Two constructions call it up front — `defineStore` validates
 * `empty()` through `state()`, and `StoreOwner` takes its own copy —
 * and both are the caller's own synchronous calls, so throwing there is
 * their problem. The call this is aimed at is the one inside frame
 * dispatch, where an exception would escape into the transport loop.
 */
function failingEmpty(quiet = 2): () => Doc {
  let calls = 0;
  return () => {
    calls += 1;
    if (calls > quiet) throw new Error('empty explodes');
    return { tick: 0 };
  };
}

function joinFrame(): string {
  return encodeMessage({ k: 'join', q: '1'.repeat(16), def: 'probe', ver: 1, store: 'test-store', key: 'k', aud: ['crew'] });
}

function handleOf(frames: readonly { readonly frame: string }[]): Hex {
  const decoded = decodeMessage(frames[0]!.frame, { maxBytes: MAX_EVENT_BYTES, as: 'replica' });
  if (!decoded.ok || decoded.message.k !== 'man') throw new Error('no manifest');
  return decoded.message.h;
}

describe('B1 — the byte ceiling', () => {
  it('counts declared reservations, not only admitted bytes', () => {
    // Nine concurrent manifests, each declaring the per-snapshot
    // maximum. Each `open` sees almost no bytes ADMITTED yet, so a cap
    // tested against arrivals lets all of them in and the table is
    // committed to more than its bound.
    const table = new AssemblyTable(MAX_ASSEMBLY_BYTES_TOTAL);
    const declared = MAX_SNAPSHOT_BYTES;
    const fit = MAX_ASSEMBLY_BYTES_TOTAL / declared;

    for (let i = 0; i < fit; i += 1) {
      const opened = table.open(
        { h: i.toString(16).padStart(32, '0') as Hex, g: '1', r: '1', n: 2, bytes: String(declared) },
        0,
      );
      expect('accept' in opened).toBe(true);
    }

    const over = table.open({ h: 'f'.repeat(32) as Hex, g: '1', r: '1', n: 2, bytes: String(declared) }, 0);

    expect('accept' in over).toBe(false);
    // And not one byte has arrived: the bound is about commitment.
    expect(table.bytesHeld).toBe(0);
  });

  it('releases the reservation of an assembly the chunks killed', () => {
    // A fatal chunk refusal reclaims the assembly in place — it is
    // still in the table until something sweeps it. Its reservation
    // must go with it, or one conflicting snapshot wedges the ceiling
    // until the deadline.
    const table = new AssemblyTable(MAX_SNAPSHOT_BYTES);
    const manifest = { g: '1', r: '1', n: 2, bytes: String(MAX_SNAPSHOT_BYTES) } as const;
    const opened = table.open({ h: 'a'.repeat(32) as Hex, ...manifest }, 0);
    if (!('accept' in opened)) throw new Error('the first manifest was refused');

    // Two different payloads for position 0: unresolvable, fatal.
    opened.accept({ h: 'a'.repeat(32) as Hex, ...manifest, i: 0, d: 'AAAA' }, raw => raw, 0);
    const conflict = opened.accept({ h: 'a'.repeat(32) as Hex, ...manifest, i: 0, d: 'BBBB' }, raw => raw, 0);
    expect('fatal' in conflict && conflict.fatal).toBe(true);
    expect(table.size).toBe(1);

    expect('accept' in table.open({ h: 'b'.repeat(32) as Hex, ...manifest }, 0)).toBe(true);
  });

  it('releases a reservation when its assembly is reclaimed', () => {
    const table = new AssemblyTable(MAX_SNAPSHOT_BYTES);
    const manifest = { g: '1', r: '1', n: 2, bytes: String(MAX_SNAPSHOT_BYTES) } as const;
    table.open({ h: 'a'.repeat(32) as Hex, ...manifest }, 0);
    expect('accept' in table.open({ h: 'b'.repeat(32) as Hex, ...manifest }, 0)).toBe(false);

    table.reclaim('a'.repeat(32) as Hex, '1');

    // A reservation that is given up is given back, or the first
    // abandoned snapshot would wedge the table until its deadline.
    expect('accept' in table.open({ h: 'b'.repeat(32) as Hex, ...manifest }, 0)).toBe(true);
  });
});

describe('C — an application callback that throws', () => {
  it('survives an `empty()` that throws while a projection is invalid', () => {
    // `project` failing falls back to `empty()`, and `empty()` is
    // application code too. `defineStore` calls it once at
    // construction, so the escape needs one that succeeds there and
    // fails later — which is what an application whose empty value
    // depends on mutable state actually does.
    const store = owner({
      // The state the owner holds is valid; the PROJECTION of it is
      // not, which is the shape §1 already handles by shipping
      // `empty()` instead. Here that fallback fails too.
      definition: definition({
        state: value => {
          const tick = Number((value as { tick?: unknown }).tick ?? 0);
          if (tick === 999) throw new Error('invalid projection');
          return { tick };
        },
        empty: failingEmpty(),
      }),
      project: () => ({ tick: 999 }),
    });

    expect(() => store.receive(joinFrame(), PEER)).not.toThrow();

    // And the caller is TOLD. Emitting nothing would leave a replica
    // waiting on a manifest that is never coming, until its own
    // deadline.
    const refused = store.receive(joinFrame(), PEER);
    expect(refused.refused).not.toBe(null);
    expect(refused.out).toHaveLength(1);
    const decoded = decodeMessage(refused.out[0]!.frame, { maxBytes: MAX_EVENT_BYTES, as: 'replica' });
    expect(decoded.ok && decoded.message.k).toBe('no');
    expect(store.handleCount).toBe(0);
  });

  it('survives a `newHandle()` that throws', () => {
    const store = owner({
      newHandle: () => {
        throw new Error('no handles');
      },
    });

    expect(() => store.receive(joinFrame(), PEER)).not.toThrow();
  });

  it('survives a `project` that throws AND an `empty()` that throws', () => {
    const store = owner({
      project: () => {
        throw new Error('projection explodes');
      },
      definition: definition({ empty: failingEmpty() }),
    });

    expect(() => store.receive(joinFrame(), PEER)).not.toThrow();
  });

  it('refuses a handle typed when a commit can no longer project for it', () => {
    // The `propagate` side of the same application failure: the
    // projection worked at join and dies at a later commit, with
    // `empty()` broken too. Silence left the replica at its old
    // revision reporting `ready, stale: false`.
    let broken = false;
    const store = owner({
      definition: definition({ empty: failingEmpty() }),
      project: state => {
        if (broken) throw new Error('projection explodes');
        return state;
      },
    });

    // A caller holding a live view of that handle, through the real
    // replica.
    const replicaDefinition = definition();
    const replicaCore = new StoreCore<Doc, Record<string, never>, Record<string, never>>({
      definition: replicaDefinition,
      initialState: { tick: 0 },
    });
    let replicaQs = 0;
    const replica = new StoreReplica<Doc, Record<string, never>, Record<string, never>>({
      definition: replicaDefinition,
      core: replicaCore,
      maxEventBytes: MAX_EVENT_BYTES,
      now: () => 0,
      newQ: () => (++replicaQs).toString(16).padStart(16, '0') as Hex,
      audience: ['crew'],
      store: 'test-store',
      key: 'k',
    });
    const toReplica = (frames: readonly { readonly frame: string }[]) =>
      frames.flatMap(frame => replica.receive(frame.frame).out);
    const joined = store.receive(replica.join().frame, PEER);
    const h = handleOf(joined.out);
    toReplica(joined.out);
    expect(replica.state).toBe('ready');
    expect(store.handleCount).toBe(1);

    broken = true;
    const committed = store.commit({ tick: 2 });

    // The caller is TOLD — `closed`, the only unsolicited refusal the
    // wire admits — and the handle goes with it.
    expect(committed.out).toHaveLength(1);
    const decoded = decodeMessage(committed.out[0]!.frame, { maxBytes: MAX_EVENT_BYTES, as: 'replica' });
    expect(decoded.ok && decoded.message.k).toBe('no');
    expect(decoded.ok && decoded.message.k === 'no' && decoded.message.code).toBe('closed');
    expect(decoded.ok && decoded.message.k === 'no' && decoded.message.h).toBe(h);
    expect(store.handleCount).toBe(0);

    // And the ladder lands the TYPED refusal at the caller: the notice
    // provokes a rejoin, and the join — a request path that CAN carry
    // a refusal — answers `capacity` correlated.
    const rejoined = toReplica(committed.out);
    expect(rejoined.map(request => request.kind)).toEqual(['join']);
    toReplica(store.receive(rejoined[0]!.frame, PEER).out);
    expect(replica.state).toBe('fenced');
    expect(replicaCore.getStatus().error?.code).toBe('capacity');
  });
});

describe('C — a solicited `resync` is a read', () => {
  it('re-consults `authorize` before re-projecting', () => {
    const seen: AccessRequest<Record<string, never>, Record<string, never>>[] = [];
    const store = owner({
      authorize: request => {
        seen.push(request);
        return true;
      },
    });
    const h = handleOf(store.receive(joinFrame(), PEER).out);
    expect(seen).toHaveLength(1);

    store.receive(encodeMessage({ k: 'resync', q: '2'.repeat(16), h, g: '1', have: '1' }), PEER);

    // A resync takes a FRESH projection at the current revision: it is
    // a read, and a read the policy has since revoked must not be
    // served.
    expect(seen).toHaveLength(2);
    expect(seen[1]).toEqual({ type: 'read', peer: PEER, audience: ['crew'] });
  });

  it('refuses a resync the policy now forbids, and keeps shipping nothing', () => {
    let permit = true;
    const store = owner({ authorize: () => permit });
    const h = handleOf(store.receive(joinFrame(), PEER).out);

    permit = false;
    const revoked = store.receive(encodeMessage({ k: 'resync', q: '2'.repeat(16), h, g: '1', have: '1' }), PEER);

    expect(revoked.refused).not.toBe(null);
    for (const frame of revoked.out) {
      const decoded = decodeMessage(frame.frame, { maxBytes: MAX_EVENT_BYTES, as: 'replica' });
      expect(decoded.ok && decoded.message.k).toBe('no');
    }

    // The REQUEST was refused, not the subscription: the handle
    // survives, so a policy that permits again is reachable without a
    // rejoin.
    expect(store.handleCount).toBe(1);
    permit = true;
    const allowed = store.receive(
      encodeMessage({ k: 'resync', q: '3'.repeat(16), h, g: '1', have: '1' }),
      PEER,
    );
    expect(allowed.refused).toBe(null);
  });
});

describe('D — the replica', () => {
  function pair() {
    const definitionValue = definition();
    let qs = 0;
    const store = owner({ definition: definitionValue });
    const clock = { value: 0 };
    const core = new StoreCore<Doc, Record<string, never>, Record<string, never>>({
      definition: definitionValue,
      initialState: definitionValue.empty(),
    });
    const replica = new StoreReplica({
      store: 'test-store',
      definition: definitionValue,
      core,
      maxEventBytes: MAX_EVENT_BYTES,
      now: () => clock.value,
      newQ: () => {
        qs += 1;
        return qs.toString(16).padStart(16, '0') as Hex;
      },
      audience: ['crew'],
      key: 'k',
    });
    const deliver = (out: readonly { readonly frame: string }[]) => {
      const requests = [];
      for (const frame of out) requests.push(...replica.receive(frame.frame).out);
      return requests;
    };
    const joined = () => deliver(store.receive(replica.join().frame, PEER).out);
    return { store, core, replica, clock, deliver, joined };
  }

  it('does not let a stale refusal tear down a healthy view', () => {
    const { store, core, replica, joined, deliver } = pair();
    joined();
    const h = replica.handle as Hex;

    // A gap provokes a `resync`, the owner answers it, and the install
    // completes — so that request's `q` is finished and stale.
    const gap = replica.receive(encodeMessage({ k: 'delta', h, g: '1', base: '5', r: '6', ops: [] }));
    const staleQ = gap.out[0]!.q;
    deliver(store.receive(gap.out[0]!.frame, PEER).out);
    expect(replica.state).toBe('ready');

    // A REQUEST refusal answers a question, so one arriving for a request
    // already finished is news about nothing: tearing down a healthy view
    // on it would discard an installed document, the watermark and the
    // handle. (`closed` is deliberately not this case — see below.)
    const late = replica.receive(encodeMessage({ k: 'no', q: staleQ, h, code: 'forbidden' }));

    expect(late.dropped).toBe('stale-correlation');
    expect(replica.handle).toBe(h);
    expect(replica.retired).not.toBe('0');
    expect(core.getState()).toEqual({ tick: 1 });
  });

  it('treats a correlated `no {closed}` as handle death', () => {
    // `closed` is terminal for the HANDLE and for any action in flight on
    // it (errors.ts), and `receive`'s foreign-handle gate has already
    // discarded any `closed` naming a different handle — so one reaching
    // the refusal dispatch is news about THIS handle whatever `q` it
    // carries. Every owner-side handle-death answer is correlated to the
    // request that provoked it (`no {q: <an alive's q>, h, closed}`), so
    // reading that as "answered a dead question" held a dead handle for
    // ever: every later `alive` was answered the same way and counted the
    // same way, the view stayed published as `ready`, and no rejoin ran.
    const { store, core, replica, joined, deliver } = pair();
    joined();
    const h = replica.handle as Hex;

    const gap = replica.receive(encodeMessage({ k: 'delta', h, g: '1', base: '5', r: '6', ops: [] }));
    const staleQ = gap.out[0]!.q;
    deliver(store.receive(gap.out[0]!.frame, PEER).out);
    expect(replica.state).toBe('ready');

    const death = replica.receive(encodeMessage({ k: 'no', q: staleQ, h, code: 'closed' }));

    expect(death.dropped).toBe('closed');
    expect(death.out).toHaveLength(1);
    expect(replica.handle).toBe(null);
    expect(replica.retired).toBe('0');
    expect(replica.state).toBe('joining');
    // The view is cleared to the definition's EMPTY document — the world
    // this replica had is gone with the handle that carried it.
    expect(core.getState()).toEqual({ tick: 0 });
  });

  it('keeps a cancellation cancelled when the owner answers anyway', () => {
    const { store, replica, core, deliver } = pair();
    joinedThenCancel();

    function joinedThenCancel() {
      const join = replica.join();
      const served = store.receive(join.frame, PEER).out;
      deliver(served);
      // A gap provokes a request, then the caller gives up.
      const h = replica.handle as Hex;
      const gap = replica.receive(encodeMessage({ k: 'delta', h, g: '1', base: '5', r: '6', ops: [] }));
      replica.cancel();
      expect(replica.state).toBe('fenced');
      // The owner's answer to the abandoned request arrives.
      const answered = store.receive(gap.out[0]!.frame, PEER);
      deliver(answered.out);
    }

    expect(replica.state).toBe('fenced');
    expect(core.getStatus().phase).not.toBe('ready');
    expect(core.getState()).toEqual({ tick: 0 });
  });

  it('rejoins cleanly after a cancellation, asking for nothing extra', () => {
    const { store, core, replica, deliver } = pair();

    // Install far enough to have a watermark, then drop an owner
    // refresh mid-transition so a skip is recorded.
    const first = store.receive(replica.join().frame, PEER).out;
    deliver([first[0]!]);
    const man = decodeMessage(first[0]!.frame, { maxBytes: MAX_EVENT_BYTES, as: 'replica' });
    if (!man.ok || man.message.k !== 'man') throw new Error('no manifest');
    replica.receive(
      encodeMessage({ k: 'man', h: man.message.h, inc: man.message.inc, g: '2', r: '9', n: 1, bytes: '64' }),
    );
    expect(replica.skipped).toBe('2');

    replica.cancel();
    expect(replica.state).toBe('fenced');

    // A fresh join: a NEW handle, whose generations restart at 1. The
    // old watermark must not make that manifest inadmissible, and the
    // cancelled epoch's skip must not ask for anything.
    const rejoined = store.receive(replica.join().frame, PEER);
    const requests = deliver(rejoined.out);

    expect(replica.state).toBe('ready');
    expect(replica.installed).toBe('1');
    expect(requests).toEqual([]);
    expect(core.getState()).toEqual({ tick: 1 });
  });

  it('does not report `ready` while a resynchronization is in flight', () => {
    const { core, replica, joined } = pair();
    joined();
    expect(core.getStatus()).toEqual({ phase: 'ready', stale: false, error: null });
    const h = replica.handle as Hex;

    replica.receive(encodeMessage({ k: 'delta', h, g: '1', base: '9', r: '10', ops: [] }));

    // The view is known to be behind: the application must not be told
    // it is current.
    expect(replica.state).toBe('installing');
    expect(core.getStatus().phase).not.toBe('ready');
    expect(core.getStatus().stale).toBe(true);
  });
});

describe('the chunker and the table agree', () => {
  it('a real snapshot’s declared bytes are what the table reserves', () => {
    const chunked = chunkSnapshot({ tick: 7 }, 5934);
    const table = new AssemblyTable(chunked.bytes);

    const opened = table.open(
      { h: 'a'.repeat(32) as Hex, g: '1', r: '1', n: chunked.n, bytes: String(chunked.bytes) },
      0,
    );

    expect('accept' in opened).toBe(true);
    const second = table.open(
      { h: 'b'.repeat(32) as Hex, g: '1', r: '1', n: chunked.n, bytes: String(chunked.bytes) },
      0,
    );
    expect('accept' in second).toBe(false);
  });
});

/**
 * A host and a replica sharing one definition, for the cancellation
 * rows: the replica installs a one-chunk snapshot the owner emits, and
 * the cancellation happens inside application code the replica calls.
 */
function cancelRig(definition: StoreDefinition<Doc, Record<string, never>, Record<string, never>>) {
  let handles = 0;
  let qs = 0;
  const owner = new StoreOwner({
    store: 'test-store',
    definition,
    authorize: () => true,
    project: (state: Doc) => state,
    maxEventBytes: MAX_EVENT_BYTES,
    now: () => 0,
    newHandle: () => {
      handles += 1;
      return handles.toString(16).padStart(32, '0') as Hex;
    },
    newIncarnation: () => 'abcdef0123456789' as Hex,
    canProject: () => true,
    actions: {},
    inputs: {},
  });
  owner.commit({ tick: 5 });
  const core = new StoreCore({ definition, initialState: definition.empty() });
  const replica = new StoreReplica({
    store: 'test-store',
    definition,
    core,
    maxEventBytes: MAX_EVENT_BYTES,
    now: () => 0,
    newQ: () => {
      qs += 1;
      return qs.toString(16).padStart(16, '0') as Hex;
    },
    audience: [],
    key: 'k',
  });
  const deliver = () => {
    for (const frame of owner.receive(replica.join().frame, PEER).out) replica.receive(frame.frame);
  };
  return { owner, core, replica, deliver };
}

/**
 * The reviewer's second round: four executed counterexamples against
 * unchanged production modules, and the adapter boundary that stopped
 * a real browser join before a byte was sent.
 */
describe('an action is refused before it commits, never after', () => {
  interface Doc2 {
    readonly tick: number;
  }
  type Acts = {
    stage: { input: Record<string, never>; output: { readonly tick: number } };
    big: { input: Record<string, never>; output: { readonly pad: string } };
  };

  function rig2(options: { output?: (value: unknown) => { readonly tick: number } } = {}) {
    let handles = 0;
    const owner = new StoreOwner<Doc2, Acts, Record<string, never>>({
      store: 'test-store',
      definition: defineStore<Doc2, Acts, Record<string, never>>({
        id: 'commit',
        version: 1,
        state: value => ({ tick: Number((value as { tick?: unknown }).tick ?? 0) }),
        empty: () => ({ tick: 0 }),
        actions: {
          stage: {
            input: () => ({}),
            output:
              options.output ??
              (value => ({ tick: Number((value as { tick?: unknown }).tick) })),
          },
          big: { input: () => ({}), output: value => ({ pad: String((value as { pad?: unknown }).pad) }) },
        },
        inputs: {},
      }),
      authorize: () => true,
      project: state => state,
      maxEventBytes: MAX_EVENT_BYTES,
      now: () => 0,
      newHandle: () => {
        handles += 1;
        return handles.toString(16).padStart(32, '0') as Hex;
      },
      newIncarnation: () => 'abcdef0123456789' as Hex,
      canProject: () => true,
      actions: {
        stage: (_input, context) => {
          context.setState({ tick: 99 });
          return { tick: 99 };
        },
        // A result far past the 8104-byte budget.
        big: (_input, context) => {
          context.setState({ tick: 7 });
          return { pad: 'x'.repeat(9000) } as never;
        },
      },
      inputs: {},
    });
    owner.commit({ tick: 0 });
    return owner;
  }

  function joinedHandle(owner: StoreOwner<Doc2, Acts, Record<string, never>>): Hex {
    const out = owner.receive(
      encodeMessage({ k: 'join', q: '1'.repeat(16) as Hex, def: 'commit', ver: 1, store: 'test-store', key: 'k', aud: [] }),
      PEER,
    ).out;
    const decoded = decodeMessage(out[0]!.frame, { maxBytes: MAX_EVENT_BYTES, as: 'replica' });
    if (!decoded.ok || decoded.message.k !== 'man') throw new Error('no manifest');
    return decoded.message.h;
  }

  it('discards the writes of an action whose RESULT the definition refuses', () => {
    // The validator ran after the transaction had already committed,
    // so a handler whose output the definition rejects moved the
    // document, bumped the revision and shipped a delta — and then
    // answered `action-rejected` for a change that had happened.
    const owner = rig2({
      output: () => {
        throw new Error('the definition refuses this result');
      },
    });
    const h = joinedHandle(owner);
    const revisionBefore = owner.currentRevision;

    const rejected = owner.receive(
      encodeMessage({ k: 'act', q: '2'.repeat(16) as Hex, h, s: '1', name: 'stage', in: {} }),
      PEER,
    );

    const kinds = rejected.out.map(frame => {
      const decoded = decodeMessage(frame.frame, { maxBytes: MAX_EVENT_BYTES, as: 'replica' });
      return decoded.ok ? decoded.message.k : 'undecodable';
    });
    expect(kinds).toEqual(['no']);
    expect(owner.getState().tick).toBe(0);
    expect(owner.currentRevision).toBe(revisionBefore);
  });

  it('discards the writes of an action whose result does not fit the budget', () => {
    const owner = rig2();
    const h = joinedHandle(owner);
    const revisionBefore = owner.currentRevision;

    const refused = owner.receive(
      encodeMessage({ k: 'act', q: '3'.repeat(16) as Hex, h, s: '1', name: 'big', in: {} }),
      PEER,
    );

    const reply = decodeMessage(refused.out[0]!.frame, { maxBytes: MAX_EVENT_BYTES, as: 'replica' });
    // `capacity`, not `action-rejected`: the request was well-formed
    // and the ANSWER is what does not fit.
    expect(reply.ok && reply.message.k === 'no' && reply.message.code).toBe('capacity');
    expect(owner.getState().tick).toBe(0);
    expect(owner.currentRevision).toBe(revisionBefore);
    expect(refused.out).toHaveLength(1);
  });
});

describe('a validator runs once per document', () => {
  it('publishes exactly what `applyDelta` returned', () => {
    // The commit path validated again, and a validator is not
    // required to be idempotent: a parser that derives a field
    // applied its transformation twice, so the outcome the caller
    // read and the document its subscribers saw were different.
    const derived = defineStore<{ readonly tick: number }, Record<string, never>, Record<string, never>>({
      id: 'derive',
      version: 1,
      state: value => ({ tick: Number((value as { tick?: unknown }).tick ?? 0) + 1 }),
      empty: () => ({ tick: 0 }),
      actions: {},
      inputs: {},
    });
    const core = new StoreCore({ definition: derived, initialState: { tick: 0 } });

    const outcome = core.applyDelta([{ o: 'r', p: ['tick'], val: 10 }]);

    expect(outcome.ok).toBe(true);
    if (!outcome.ok) return;
    expect(outcome.next.tick).toBe(11);
    expect(core.getState().tick).toBe(11);
  });
});

describe('a cancellation during an installation is not overwritten', () => {
  it('stays fenced when the definition cancels while validating', () => {
    const latched = { done: false };
    const cancelled = { replica: null as StoreReplica<Doc, Record<string, never>, Record<string, never>> | null };
    const definition = defineStore<Doc, Record<string, never>, Record<string, never>>({
      id: 'cancel.validator',
      version: 1,
      state: value => {
        // Application code the replica calls INSIDE the assembly —
        // ONCE, latched: `cancel` publishes `empty()` through the
        // core, which validates, which would re-enter this validator
        // for ever.
        // Latched, and only once the replica exists: the core
        // validates its own initial state at construction, which
        // would otherwise consume the latch before there was
        // anything to cancel.
        if (!latched.done && cancelled.replica !== null) {
          latched.done = true;
          cancelled.replica.cancel();
        }
        return { tick: Number((value as { tick?: unknown }).tick ?? 0) };
      },
      empty: () => ({ tick: 0 }),
      actions: {},
      inputs: {},
    });
    const pair = cancelRig(definition);
    cancelled.replica = pair.replica;
    const seen: number[] = [];
    pair.core.subscribe(state => seen.push(state.tick));

    pair.deliver();

    // NOT ONE notification: the installation is abandoned where the
    // cancellation is noticed — before the document is published.
    // Publishing it and clearing it afterwards would hand every
    // subscriber a world the caller had already cancelled, and the
    // state assertions below cannot see that.
    expect(seen).toEqual([]);
    expect(pair.replica.state).toBe('fenced');
    expect(pair.replica.installed).toBe(null);
    expect(pair.core.getStatus().phase).not.toBe('ready');
    expect(pair.core.getState()).toEqual({ tick: 0 });
  });

  it('does not report `ready` over the view a subscriber’s cancellation cleared', () => {
    const definition = defineStore<Doc, Record<string, never>, Record<string, never>>({
      id: 'cancel.subscriber',
      version: 1,
      state: value => ({ tick: Number((value as { tick?: unknown }).tick ?? 0) }),
      empty: () => ({ tick: 0 }),
      actions: {},
      inputs: {},
    });
    const pair = cancelRig(definition);
    // Application code the replica calls DURING the publication.
    pair.core.subscribe(() => {
      pair.replica.cancel();
    });

    pair.deliver();

    expect(pair.replica.state).toBe('fenced');
    expect(pair.core.getStatus().phase).not.toBe('ready');
    expect(pair.core.getStatus().stale).toBe(false);
    expect(pair.core.getState()).toEqual({ tick: 0 });
  });
});
