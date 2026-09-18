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
import type { AccessRequest } from '../../src/store/types.js';

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
  return encodeMessage({ k: 'join', q: '1'.repeat(16), def: 'probe', ver: 1, key: 'k', aud: ['crew'] });
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

    const late = replica.receive(encodeMessage({ k: 'no', q: staleQ, h, code: 'closed' }));

    // A refusal of an abandoned request is not news about the handle.
    expect(late.dropped).toBe('stale-correlation');
    expect(replica.handle).toBe(h);
    expect(replica.retired).not.toBe('0');
    expect(core.getState()).toEqual({ tick: 1 });
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
