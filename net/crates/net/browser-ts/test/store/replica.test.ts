/**
 * Slice D — the replica, driven by the REAL owner wherever a real
 * owner can produce the arrival.
 *
 * The two halves check each other: slice C's dispatcher emits `man` and
 * `snap` and this installs them, so a disagreement about the contract
 * shows up as a failing witness rather than as two modules that each
 * pass their own tests. Where the arrival is one the owner does not yet
 * emit — an unsolicited refresh, a delayed manifest for an abandoned
 * generation — the frame is built with the real encoder, because a
 * hand-built object would not have gone through the ladder.
 */

import { describe, expect, it, vi } from 'vitest';

import { StoreCore } from '../../src/store/core.js';
import { defineStore } from '../../src/store/definition.js';
import { StoreOwner, type Outbound } from '../../src/store/owner.js';
import { StoreReplica, type Request } from '../../src/store/replica.js';
import { decimalValue, decodeMessage, encodeMessage, type Hex, type WireOp } from '../../src/store/wire.js';

interface World {
  readonly tick: number;
  readonly crew: Record<string, { readonly hp: number }>;
}

const MAX_EVENT_BYTES = 8104;

function asRecord(value: unknown): Record<string, unknown> {
  if (typeof value !== 'object' || value === null) throw new Error('not a record');
  return value as Record<string, unknown>;
}

const world = defineStore<World, Record<string, never>, Record<string, never>>({
  id: 'world',
  version: 3,
  state(value) {
    const raw = asRecord(value);
    const crew: Record<string, { hp: number }> = {};
    for (const [name, entry] of Object.entries(asRecord(raw['crew'] ?? {}))) {
      crew[name] = { hp: Number(asRecord(entry)['hp']) };
    }
    return { tick: Number(raw['tick'] ?? 0), crew };
  },
  empty: () => ({ tick: 0, crew: {} }),
  actions: {},
  inputs: {},
});

const PEER = '00000000000000aa';

/** An owner and a replica of it, wired only through frames. */
function pair(initial: World = { tick: 1, crew: { ada: { hp: 10 } } }) {
  let handles = 0;
  let qs = 0;
  const owner = new StoreOwner<World, Record<string, never>, Record<string, never>>({
    definition: world,
    authorize: () => true,
    project: state => state,
    maxEventBytes: MAX_EVENT_BYTES,
    now: () => clock.value,
    newHandle: () => {
      handles += 1;
      return handles.toString(16).padStart(32, '0') as Hex;
    },
    newIncarnation: () => 'abcdef0123456789' as Hex,
    canProject: () => true,
    actions: {},
    inputs: {},
  });
  owner.commit(initial);

  const clock = { value: 0 };
  const core = new StoreCore<World, Record<string, never>, Record<string, never>>({
    definition: world,
    initialState: world.empty(),
  });
  const replica = new StoreReplica({
    definition: world,
    core,
    maxEventBytes: MAX_EVENT_BYTES,
    now: () => clock.value,
    newQ: () => {
      qs += 1;
      return qs.toString(16).padStart(16, '0') as Hex;
    },
    audience: ['crew'],
    key: 'ada',
  });

  /** Hand every frame the owner produced to the replica, in order. */
  function deliver(out: readonly Outbound[]): readonly Request[] {
    const requests: Request[] = [];
    for (const frame of out) requests.push(...replica.receive(frame.frame).out);
    return requests;
  }

  /** Drive a join to completion and return the replica's requests. */
  function joined(): readonly Request[] {
    const join = replica.join();
    return deliver(owner.receive(join.frame, PEER).out);
  }

  return { owner, core, replica, clock, deliver, joined };
}

/** The `man` frame an owner emitted, decoded. */
function manifestOf(out: readonly Outbound[]) {
  const decoded = decodeMessage(out[0]!.frame, { maxBytes: MAX_EVENT_BYTES, as: 'replica' });
  if (!decoded.ok || decoded.message.k !== 'man') throw new Error('not a manifest');
  return decoded.message;
}

describe('a join is installed', () => {
  it('learns the handle from the manifest and publishes the assembled document', () => {
    const { owner, core, replica, deliver } = pair();

    expect(replica.state).toBe('joining');
    expect(replica.handle).toBe(null);

    const join = replica.join();
    const out = owner.receive(join.frame, PEER).out;
    const requests = deliver(out);

    expect(replica.state).toBe('ready');
    // The handle is LEARNED from the owner's own manifest, never
    // chosen locally.
    expect(replica.handle).toBe(manifestOf(out).h);
    expect(core.getState()).toEqual({ tick: 1, crew: { ada: { hp: 10 } } });
    expect(core.getStatus()).toEqual({ phase: 'ready', stale: false, error: null });
    // Nothing to ask for: the install was clean.
    expect(requests).toEqual([]);
  });

  it('publishes nothing until the last chunk', () => {
    // A partial world is never shown. The owner's projection here is
    // large enough to need several chunks.
    const crew: Record<string, { hp: number }> = {};
    for (let i = 0; i < 700; i += 1) crew[`crew${String(i)}`] = { hp: i };
    const { owner, core, replica, deliver } = pair({ tick: 4, crew });

    const join = replica.join();
    const out = owner.receive(join.frame, PEER).out;
    const manifest = manifestOf(out);
    expect(manifest.n).toBeGreaterThan(1);

    // Every frame but the last leaves the view empty and not ready.
    for (const frame of out.slice(0, -1)) {
      replica.receive(frame.frame);
      expect(core.getState()).toEqual(world.empty());
      expect(core.getStatus().phase).not.toBe('ready');
    }
    deliver(out.slice(-1));

    expect(replica.state).toBe('ready');
    expect(Object.keys(core.getState().crew)).toHaveLength(700);
    expect(core.revision).toBe(1);
  });

  it('sets the watermark to the admitted generation', () => {
    const { replica, joined } = pair();
    joined();

    expect(replica.installed).toBe('1');
    expect(replica.retired).toBe('1');
    expect(replica.revision).toBe('1');
  });
});

describe('the watermark fences a generation for ever', () => {
  /** A manifest frame for an arbitrary generation, through the encoder. */
  function manifest(h: Hex, g: string, r: string, options: { q?: Hex; inc?: Hex } = {}): string {
    return encodeMessage({
      k: 'man',
      ...(options.q === undefined ? {} : { q: options.q }),
      h,
      inc: options.inc ?? ('abcdef0123456789' as Hex),
      g,
      r,
      n: 1,
      bytes: '64',
    });
  }

  it('drops a duplicate manifest for the generation being assembled', () => {
    const { owner, replica, deliver } = pair();
    const join = replica.join();
    const out = owner.receive(join.frame, PEER).out;
    deliver([out[0]!]);
    const first = manifestOf(out);

    // The same manifest again, unsolicited: admitting the first set
    // `retired := g`, so this fails `g > retired`.
    const again = replica.receive(manifest(first.h, first.g, first.r));

    expect(again.dropped).toBe('stale-generation');
    // And the open assembly is untouched: the rest of the chunks still
    // install.
    deliver(out.slice(1));
    expect(replica.state).toBe('ready');
    expect(replica.installed).toBe('1');
  });

  it('refuses a delayed manifest for an abandoned generation', () => {
    const { owner, replica, clock, deliver } = pair();
    const join = replica.join();
    const out = owner.receive(join.frame, PEER).out;
    deliver([out[0]!]);
    const abandoned = manifestOf(out);

    // The assembly times out: `assembling` is cleared, `retired` keeps
    // the generation.
    clock.value = 10_000;
    const recovery = replica.tick();

    expect(recovery).toHaveLength(1);
    expect(recovery[0]!.kind).toBe('resync');
    expect(replica.retired).toBe('1');

    // The delayed manifest for that same generation — `g > installed`
    // holds (nothing is installed) and must not be enough.
    const late = replica.receive(manifest(abandoned.h, abandoned.g, abandoned.r, { q: recovery[0]!.q }));

    expect(late.dropped).toBe('stale-generation');
    expect(replica.installed).toBe(null);
  });

  it('refuses a chunk belonging to another generation', () => {
    // Chunk identity is the assembler's property (slice B1): a chunk
    // whose `(h, g, r, n)` is not this manifest's cannot join, so a
    // retransmission from a superseded attempt at the same revision
    // cannot be mixed into the live one.
    const { owner, core, replica, deliver } = pair();
    const join = replica.join();
    const out = owner.receive(join.frame, PEER).out;
    deliver([out[0]!]);
    const manifest = manifestOf(out);
    const chunk = decodeMessage(out[1]!.frame, { maxBytes: MAX_EVENT_BYTES, as: 'replica' });
    if (!chunk.ok || chunk.message.k !== 'snap') throw new Error('not a chunk');

    const foreign = replica.receive(
      encodeMessage({ ...chunk.message, g: String(decimalValue(manifest.g) + 7n) }),
    );

    expect(foreign.dropped).toBe('chunk-wrong-assembly');
    expect(replica.state).toBe('installing');
    expect(core.getState()).toEqual(world.empty());
    // And the live assembly is untouched: the real chunk still lands.
    deliver(out.slice(1));
    expect(replica.state).toBe('ready');
  });

  it('publishes nothing after cancellation mid-assembly', () => {
    // The fence against publishing a superseded installation IS the
    // retirement: every supersession path retires the open assembly,
    // so the chunk that would have completed it has nothing to join.
    const { owner, core, replica, deliver } = pair();
    const join = replica.join();
    const out = owner.receive(join.frame, PEER).out;
    deliver([out[0]!]);

    replica.cancel();
    const late = deliver(out.slice(1));

    expect(late).toEqual([]);
    expect(replica.state).toBe('fenced');
    expect(replica.installed).toBe(null);
    expect(core.getState()).toEqual(world.empty());
    expect(replica.dropped['no-open-assembly']).toBe(1);
  });

  it('refuses a manifest from another incarnation', () => {
    const { replica, joined } = pair();
    joined();
    const h = replica.handle as Hex;

    const other = replica.receive(manifest(h, '2', '9', { inc: '1111111111111111' as Hex }));

    expect(other.dropped).toBe('foreign-incarnation');
    expect(replica.installed).toBe('1');
  });

  it('drops any message naming a handle it did not learn', () => {
    const { replica, joined } = pair();
    joined();

    const foreign = replica.receive(manifest('f'.repeat(32) as Hex, '9', '9'));

    expect(foreign.dropped).toBe('foreign-handle');
    expect(replica.dropped['foreign-handle']).toBe(1);
    expect(replica.installed).toBe('1');
  });
});

describe('deltas apply on their base, or provoke recovery', () => {
  function delta(h: Hex, g: string, base: string, r: string, ops: readonly WireOp[]): string {
    return encodeMessage({ k: 'delta', h, g, base, r, ops });
  }

  it('applies a delta whose base is the installed revision', () => {
    const { core, replica, joined } = pair();
    joined();
    const h = replica.handle as Hex;
    const listener = vi.fn();
    core.subscribe(listener);

    const applied = replica.receive(
      delta(h, '1', '1', '2', [{ o: 'r', p: ['crew', 'ada', 'hp'], val: 7 }]),
    );

    expect(applied.dropped).toBe(null);
    expect(core.getState().crew['ada']).toEqual({ hp: 7 });
    expect(replica.revision).toBe('2');
    expect(listener).toHaveBeenCalledTimes(1);
  });

  it('shares identity for the subtrees a delta did not touch', () => {
    const { core, replica, joined } = pair({ tick: 1, crew: { ada: { hp: 10 }, bob: { hp: 3 } } });
    joined();
    const h = replica.handle as Hex;
    const before = core.getState();

    replica.receive(delta(h, '1', '1', '2', [{ o: 'r', p: ['crew', 'ada', 'hp'], val: 7 }]));

    expect(core.getState().crew['bob']).toBe(before.crew['bob']);
    expect(core.getState()).not.toBe(before);
  });

  it('asks to resynchronize on a gap, and does not apply the delta', () => {
    const { core, replica, joined } = pair();
    joined();
    const h = replica.handle as Hex;

    // base 5 against an installed revision of 1.
    const gap = replica.receive(delta(h, '1', '5', '6', [{ o: 'r', p: ['tick'], val: 99 }]));

    expect(gap.dropped).toBe('gap');
    expect(core.getState().tick).toBe(1);
    expect(replica.state).toBe('installing');
    expect(gap.out).toHaveLength(1);
    const asked = decodeMessage(gap.out[0]!.frame, { maxBytes: MAX_EVENT_BYTES, as: 'owner' });
    expect(asked.ok && asked.message.k === 'resync' && asked.message).toMatchObject({
      h,
      // ADVISORY: what this replica actually has.
      g: '1',
      have: '1',
    });
  });

  it('asks to resynchronize when a patch does not apply', () => {
    const { core, replica, joined } = pair();
    joined();
    const h = replica.handle as Hex;

    // `crew.ada` is a record, so descending through `hp` into a
    // number cannot apply.
    const broken = replica.receive(
      delta(h, '1', '1', '2', [{ o: 'r', p: ['crew', 'ada', 'hp', 'deeper'], val: 1 }]),
    );

    expect(broken.dropped).not.toBe(null);
    expect(broken.out).toHaveLength(1);
    expect(broken.out[0]!.kind).toBe('resync');
    // The old view is still the published one: a failed patch never
    // publishes half of itself.
    expect(core.getState()).toEqual({ tick: 1, crew: { ada: { hp: 10 } } });
    expect(replica.revision).toBe('1');
  });

  it('asks to resynchronize when the patched document does not validate', () => {
    const { core, replica, joined } = pair();
    joined();
    const h = replica.handle as Hex;

    // `state()` reads `crew` as a record of records; a string is not
    // one, and validation is what must catch it.
    const invalid = replica.receive(delta(h, '1', '1', '2', [{ o: 'r', p: ['crew'], val: 'nobody' }]));

    expect(invalid.out[0]!.kind).toBe('resync');
    expect(core.getState().crew).toEqual({ ada: { hp: 10 } });
  });

  it('drops a delta for a generation other than the installed one', () => {
    const { core, replica, joined } = pair();
    joined();
    const h = replica.handle as Hex;

    const stale = replica.receive(delta(h, '2', '1', '2', [{ o: 'r', p: ['tick'], val: 99 }]));

    expect(stale.dropped).toBe('stale-generation');
    expect(stale.out).toEqual([]);
    expect(core.getState().tick).toBe(1);
  });

  it('applies an unchanged delta without notifying', () => {
    const { core, replica, joined } = pair();
    joined();
    const h = replica.handle as Hex;
    const listener = vi.fn();
    core.subscribe(listener);

    const same = replica.receive(delta(h, '1', '1', '2', [{ o: 'r', p: ['crew', 'ada', 'hp'], val: 10 }]));

    expect(same.dropped).toBe(null);
    expect(listener).not.toHaveBeenCalled();
    // The revision still moves: the owner said so, and the next
    // delta's base will be this one.
    expect(replica.revision).toBe('2');
  });
});

describe('a dropped refresh is recovered, not stranded', () => {
  it('records the skip mid-transition and asks once on reaching ready', () => {
    const { owner, replica, deliver } = pair();
    const join = replica.join();
    const out = owner.receive(join.frame, PEER).out;
    deliver([out[0]!]);
    const first = manifestOf(out);

    // Two owner refreshes arrive while the replica is installing. Both
    // are dropped; the skip is coalesced into one number.
    for (const g of ['2', '3']) {
      const refresh = replica.receive(
        encodeMessage({ k: 'man', h: first.h, inc: first.inc, g, r: '9', n: 1, bytes: '64' }),
      );
      expect(refresh.dropped).toBe('mid-transition');
    }
    expect(replica.skipped).toBe('3');

    // Completing the install then asks ONCE.
    const requests = deliver(out.slice(1));

    expect(requests).toHaveLength(1);
    expect(requests[0]!.kind).toBe('resync');
    expect(replica.state).toBe('installing');
    expect(replica.skipped).toBe(null);
  });

  it('records `behind` for a delta that arrives mid-transition', () => {
    const { owner, replica, deliver } = pair();
    const join = replica.join();
    const out = owner.receive(join.frame, PEER).out;
    deliver([out[0]!]);
    const first = manifestOf(out);

    const early = replica.receive(
      encodeMessage({ k: 'delta', h: first.h, g: '1', base: '1', r: '2', ops: [] }),
    );
    expect(early.dropped).toBe('not-ready');

    const requests = deliver(out.slice(1));

    expect(requests.map(r => r.kind)).toEqual(['resync']);
  });

  it('does not ask when the completed install subsumes the skip', () => {
    const { owner, replica, deliver } = pair();
    const join = replica.join();
    const out = owner.receive(join.frame, PEER).out;
    deliver([out[0]!]);
    const first = manifestOf(out);

    // A refresh for the generation being installed is not a skip
    // ahead of it.
    replica.receive(
      encodeMessage({ k: 'man', h: first.h, inc: first.inc, g: '1', r: '1', n: 1, bytes: '64' }),
    );
    const requests = deliver(out.slice(1));

    expect(requests).toEqual([]);
    expect(replica.state).toBe('ready');
  });

  it('names its OWN position when recovering, not the owner’s', () => {
    // §1.8's repair, and the sequence that forced it: installed at
    // generation 1, the owner's replacement 2 is admitted (so the
    // watermark is 2) and then times out. The only generation this
    // replica can honestly name is 1 — the one it actually has — and a
    // `resync` naming 2 would be claiming a view it never installed.
    const { owner, replica, clock, joined } = pair();
    joined();
    const h = replica.handle as Hex;

    const refresh = replica.receive(
      encodeMessage({ k: 'man', h, inc: 'abcdef0123456789', g: '2', r: '9', n: 1, bytes: '64' }),
    );
    expect(refresh.dropped).toBe(null);
    expect(replica.retired).toBe('2');
    expect(replica.installed).toBe('1');

    clock.value += 10_000;
    const recovery = replica.tick();

    expect(recovery).toHaveLength(1);
    const asked = decodeMessage(recovery[0]!.frame, { maxBytes: MAX_EVENT_BYTES, as: 'owner' });
    expect(asked.ok && asked.message.k === 'resync' && asked.message).toMatchObject({
      g: '1',
      have: '1',
    });
    // And the owner answers it rather than refusing the stale position.
    expect(owner.receive(recovery[0]!.frame, PEER).refused).toBeNull();
  });

  it('stops asking once an installation subsumes the skip', () => {
    // Otherwise recovery never terminates: each install asks again for
    // a generation it has already passed.
    const { owner, replica, joined, deliver } = pair();
    joined();
    const h = replica.handle as Hex;

    // A gap provokes a `resync`, and a refresh arrives while it is in
    // flight, so a skip is recorded.
    const gap = replica.receive(
      encodeMessage({ k: 'delta', h, g: '1', base: '5', r: '6', ops: [] }),
    );
    replica.receive(
      encodeMessage({ k: 'man', h, inc: 'abcdef0123456789', g: '2', r: '9', n: 1, bytes: '64' }),
    );
    expect(replica.skipped).toBe('2');

    // The owner's answer installs generation 2, which subsumes it.
    const answered = owner.receive(gap.out[0]!.frame, PEER);
    const requests = deliver(answered.out);

    expect(replica.installed).toBe('2');
    expect(replica.state).toBe('ready');
    expect(requests).toEqual([]);
  });

  it('accepts an unsolicited refresh only when ready', () => {
    const { owner, replica, joined } = pair();
    joined();
    const h = replica.handle as Hex;

    // Ready: the owner may replace the view on its own initiative.
    const refresh = replica.receive(
      encodeMessage({ k: 'man', h, inc: 'abcdef0123456789', g: '2', r: '9', n: 1, bytes: '64' }),
    );

    expect(refresh.dropped).toBe(null);
    expect(replica.state).toBe('installing');
    expect(replica.retired).toBe('2');
    // Still published while the replacement assembles: this is a
    // refresh, not an audience change.
    expect(owner.handleCount).toBe(1);
  });
});

describe('a fence is the caller’s to lift', () => {
  it('fences on a refusal for the pending slot, keeping the handle', () => {
    const { replica, joined, core } = pair();
    joined();
    const h = replica.handle as Hex;
    // Provoke a request so there is a slot to refuse.
    const gap = replica.receive(
      encodeMessage({ k: 'delta', h, g: '1', base: '5', r: '6', ops: [] }),
    );
    const slot = gap.out[0]!.q;

    const refused = replica.receive(encodeMessage({ k: 'no', q: slot, h, code: 'forbidden' }));

    expect(refused.dropped).toBe('refused-forbidden');
    expect(replica.state).toBe('fenced');
    // The REQUEST was refused, not the subscription.
    expect(replica.handle).toBe(h);
    expect(core.getStatus().phase).toBe('failed');
    expect(core.getState()).toEqual(world.empty());
  });

  it('records no skip while fenced, and no owner emission lifts the fence', () => {
    const { replica, joined } = pair();
    joined();
    const h = replica.handle as Hex;
    const gap = replica.receive(
      encodeMessage({ k: 'delta', h, g: '1', base: '5', r: '6', ops: [] }),
    );
    replica.receive(encodeMessage({ k: 'no', q: gap.out[0]!.q, h, code: 'forbidden' }));

    const refresh = replica.receive(
      encodeMessage({ k: 'man', h, inc: 'abcdef0123456789', g: '5', r: '9', n: 1, bytes: '64' }),
    );

    expect(refresh.dropped).toBe('fenced');
    expect(replica.state).toBe('fenced');
    expect(replica.skipped).toBe(null);
  });

  it('keeps the fence when an expiry notice arrives while fenced', () => {
    const { replica, joined } = pair();
    joined();
    const h = replica.handle as Hex;
    const gap = replica.receive(
      encodeMessage({ k: 'delta', h, g: '1', base: '5', r: '6', ops: [] }),
    );
    replica.receive(encodeMessage({ k: 'no', q: gap.out[0]!.q, h, code: 'forbidden' }));

    const expiry = replica.receive(encodeMessage({ k: 'no', h, code: 'closed' }));

    expect(expiry.dropped).toBe('closed-while-fenced');
    expect(expiry.out).toEqual([]);
    expect(replica.state).toBe('fenced');
    // The handle is gone, so the caller's next request can only be a
    // join — which is the only thing that lifts this.
    expect(replica.handle).toBe(null);
  });

  it('lifts the fence with a resynchronization that the owner answers', () => {
    const { owner, replica, joined, deliver } = pair();
    joined();
    const h = replica.handle as Hex;
    const gap = replica.receive(
      encodeMessage({ k: 'delta', h, g: '1', base: '5', r: '6', ops: [] }),
    );
    // The owner answers the `resync` the gap produced with a new
    // generation, which installs.
    const answered = owner.receive(gap.out[0]!.frame, PEER);

    expect(answered.refused).toBe(null);
    deliver(answered.out);

    expect(replica.state).toBe('ready');
    expect(replica.installed).toBe('2');
    expect(replica.retired).toBe('2');
  });
});

describe('the handle’s end', () => {
  it('rejoins after `no {closed}`, replaying nothing', () => {
    const { replica, joined, core } = pair();
    joined();
    const h = replica.handle as Hex;

    const closed = replica.receive(encodeMessage({ k: 'no', h, code: 'closed' }));

    expect(closed.out).toHaveLength(1);
    expect(closed.out[0]!.kind).toBe('join');
    expect(replica.state).toBe('joining');
    // All of the generation state is discarded: a new handle starts
    // over, including the watermark.
    expect(replica.handle).toBe(null);
    expect(replica.retired).toBe('0');
    expect(replica.installed).toBe(null);
    expect(core.getState()).toEqual(world.empty());
  });

  it('closes terminally on `no {owner-lost}`, even for a stale correlation', () => {
    // §1.12's ladder refuses a q-less `no` that is not `closed`, so
    // `owner-lost` always arrives correlated. It is terminal anyway:
    // the incarnation that held the document is gone, whichever
    // request the refusal happens to answer.
    const { replica, joined, core } = pair();
    joined();

    const h = replica.handle as Hex;
    const lost = replica.receive(
      encodeMessage({ k: 'no', q: 'dead'.repeat(4) as Hex, h, code: 'owner-lost' }),
    );

    expect(lost.out).toEqual([]);
    expect(replica.state).toBe('closed');
    expect(core.getStatus().phase).toBe('closed');
    // Terminal: nothing to rejoin, so a further arrival is only
    // counted.
    expect(replica.receive(encodeMessage({ k: 'no', h, code: 'closed' })).dropped).toBe('closed');
    expect(() => replica.join()).toThrow(/closed/);
  });

  it('fences on cancellation and keeps no view', () => {
    const { replica, joined, core } = pair();
    joined();

    replica.cancel();

    expect(replica.state).toBe('fenced');
    expect(core.getState()).toEqual(world.empty());
    expect(core.getStatus().phase).toBe('failed');
  });
});

describe('kinds this slice does not implement', () => {
  it('counts a correlated reply rather than half-applying it', () => {
    const { replica, joined } = pair();
    joined();
    const h = replica.handle as Hex;

    const result = replica.receive(encodeMessage({ k: 'res', q: '1'.repeat(16), h, s: '1', out: {} }));
    const ok = replica.receive(encodeMessage({ k: 'ok', q: '2'.repeat(16), h }));

    expect([result.dropped, ok.dropped]).toEqual(['unimplemented-kind', 'unimplemented-kind']);
    expect(replica.dropped['unimplemented-kind']).toBe(2);
    expect(replica.state).toBe('ready');
  });

  it('counts an undecodable frame without changing state', () => {
    const { replica, joined } = pair();
    joined();

    const bad = replica.receive('{"v":1,"k":"man"');

    expect(bad.dropped).not.toBe(null);
    expect(replica.state).toBe('ready');
    expect(replica.installed).toBe('1');
  });
});
