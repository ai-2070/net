/**
 * Slice F — audience transitions and `resume`.
 *
 * Both sides, wired only through frames, because the properties here
 * are about what a caller can SEE at each of the three separate
 * moments: local intent (immediate), owner acceptance (allocates), and
 * installation (publishes).
 *
 * The two sharp rules:
 *
 * - `setAudience` clears the view **immediately**, before the owner has
 *   agreed to anything. Continuing to render the previous audience
 *   while a narrowing is in flight is the disclosure it exists to stop.
 * - `resume` carries the caller's **latest desired** audience, never
 *   the owner's older one — otherwise a reconnect re-delivers a wider
 *   view than the caller now wants.
 */

import { describe, expect, it } from 'vitest';

import { StoreCore } from '../../src/store/core.js';
import { defineStore } from '../../src/store/definition.js';
import { StoreOwner, type Outbound } from '../../src/store/owner.js';
import { StoreReplica, type Request } from '../../src/store/replica.js';
import type { AccessRequest } from '../../src/store/types.js';
import { decodeMessage, encodeMessage, type Hex } from '../../src/store/wire.js';

const MAX_EVENT_BYTES = 8104;
const PEER = '00000000000000aa';

interface World {
  readonly crew: Record<string, number>;
  readonly deck: Record<string, number>;
}

function asRecord(value: unknown): Record<string, unknown> {
  if (typeof value !== 'object' || value === null) throw new Error('not a record');
  return value as Record<string, unknown>;
}

function numbers(value: unknown): Record<string, number> {
  const out: Record<string, number> = {};
  for (const [key, entry] of Object.entries(asRecord(value ?? {}))) out[key] = Number(entry);
  return out;
}

const world = defineStore<World, Record<string, never>, Record<string, never>>({
  id: 'world',
  version: 1,
  state: value => ({ crew: numbers(asRecord(value)['crew']), deck: numbers(asRecord(value)['deck']) }),
  empty: () => ({ crew: {}, deck: {} }),
  actions: {},
  inputs: {},
});

const FULL: World = { crew: { ada: 1 }, deck: { hoist: 2 } };

/** The owner projects strictly by label: an audience sees its own key. */
function projection(state: World, audience: readonly string[]): World {
  return {
    crew: audience.includes('crew') ? state.crew : {},
    deck: audience.includes('deck') ? state.deck : {},
  };
}

function pair(
  options: {
    authorize?: (request: AccessRequest<Record<string, never>, Record<string, never>>) => boolean;
    audience?: readonly string[];
  } = {},
) {
  let handles = 0;
  let qs = 0;
  const clock = { value: 0 };
  const projectable = { value: true };
  const owner = new StoreOwner<World, Record<string, never>, Record<string, never>>({
    store: 'test-store',
    definition: world,
    authorize: options.authorize ?? (() => true),
    project: projection,
    maxEventBytes: MAX_EVENT_BYTES,
    now: () => clock.value,
    newHandle: () => {
      handles += 1;
      return handles.toString(16).padStart(32, '0') as Hex;
    },
    newIncarnation: () => 'abcdef0123456789' as Hex,
    canProject: () => projectable.value,
    actions: {},
    inputs: {},
  });
  owner.commit(FULL);

  const core = new StoreCore<World, Record<string, never>, Record<string, never>>({
    definition: world,
    initialState: world.empty(),
  });
  const replica = new StoreReplica({
    store: 'test-store',
    definition: world,
    core,
    maxEventBytes: MAX_EVENT_BYTES,
    now: () => clock.value,
    newQ: () => {
      qs += 1;
      return qs.toString(16).padStart(16, '0') as Hex;
    },
    audience: options.audience ?? ['crew'],
    key: 'ada',
  });

  const deliver = (out: readonly Outbound[]): readonly Request[] => {
    const requests: Request[] = [];
    for (const frame of out) requests.push(...replica.receive(frame.frame).out);
    return requests;
  };
  const send = (requests: readonly Request[]) => {
    let out: readonly Outbound[] = [];
    for (const request of requests) out = [...out, ...owner.receive(request.frame, PEER).out];
    return out;
  };
  const settle = (requests: readonly Request[]) => deliver(send(requests));
  const joined = () => settle([replica.join()]);

  return { owner, core, replica, clock, projectable, deliver, send, settle, joined };
}

function refusalOf(out: readonly Outbound[]): string | null {
  for (const frame of out) {
    const decoded = decodeMessage(frame.frame, { maxBytes: MAX_EVENT_BYTES, as: 'replica' });
    if (decoded.ok && decoded.message.k === 'no') return decoded.message.code;
  }
  return null;
}

describe('a narrowing is local before it is agreed', () => {
  it('clears the view immediately, before the owner has answered', () => {
    const { core, replica, joined } = pair();
    joined();
    expect(core.getState()).toEqual({ crew: { ada: 1 }, deck: {} });

    const requests = replica.setAudience(['deck']);

    // Nothing has been sent to the owner yet, and the crew view is
    // already gone: this is the disclosure boundary.
    expect(core.getState()).toEqual(world.empty());
    expect(core.getStatus()).toEqual({ phase: 'syncing', stale: false, error: null });
    expect(replica.state).toBe('installing');
    // Nothing is installed any more either: a retained generation
    // would make the next reconnect call this view retained-but-valid.
    expect(replica.installed).toBe(null);
    expect(replica.revision).toBe(null);
    expect(requests.map(r => r.kind)).toEqual(['aud']);
  });

  it('installs the new audience and nothing of the old one', () => {
    const { core, replica, joined, settle } = pair();
    joined();

    settle(replica.setAudience(['deck']));

    expect(replica.state).toBe('ready');
    expect(core.getState()).toEqual({ crew: {}, deck: { hoist: 2 } });
    expect(core.getStatus().phase).toBe('ready');
  });

  it('allocates a new generation and retires the superseded one', () => {
    const { replica, joined, settle } = pair();
    joined();
    expect(replica.installed).toBe('1');

    settle(replica.setAudience(['deck']));

    // The superseded generation stays consumed: the new one is 2, and
    // the watermark moved with it.
    expect(replica.installed).toBe('2');
    expect(replica.retired).toBe('2');
  });

  it('supersedes a transition still in flight, with a new `q`', () => {
    const { replica, joined, owner, deliver } = pair();
    joined();

    const first = replica.setAudience(['deck']);
    const second = replica.setAudience(['crew', 'deck']);
    expect(second.map(r => r.kind)).toEqual(['aud']);
    expect(second[0]!.q).not.toBe(first[0]!.q);

    // The owner answers the NEWER request; the older `q` is stale and
    // its manifest is dropped.
    const superseded = owner.receive(first[0]!.frame, PEER);
    expect(deliver(superseded.out)).toEqual([]);
    const accepted = owner.receive(second[0]!.frame, PEER);
    deliver(accepted.out);

    expect(replica.state).toBe('ready');
    expect(replica.desired).toEqual(['crew', 'deck']);
  });

  it('shares one wire request between equal pending transitions', () => {
    const { replica, joined } = pair();
    joined();

    const first = replica.setAudience(['deck']);
    const again = replica.setAudience(['deck']);

    // `q` is the transition's identity on the wire, not a waiter's.
    expect(first).toHaveLength(1);
    expect(again).toEqual([]);
    expect(replica.waiters).toBe(2);
  });

  it('fences when the last waiter cancels, and does not restore the old view', () => {
    const { core, replica, joined } = pair();
    joined();
    replica.setAudience(['deck']);
    replica.setAudience(['deck']);

    replica.cancelWaiter();
    expect(replica.state).toBe('installing');

    replica.cancelWaiter();

    expect(replica.state).toBe('fenced');
    // The previous view was invalidated at request time: the caller
    // asked for a different audience, so restoring it would show what
    // they asked to stop seeing.
    expect(core.getState()).toEqual(world.empty());
  });

  it('rides the join when no handle has been learned yet', () => {
    const { replica, owner, deliver } = pair();

    const requests = replica.setAudience(['deck']);

    expect(requests.map(r => r.kind)).toEqual(['join']);
    const decoded = decodeMessage(requests[0]!.frame, { maxBytes: MAX_EVENT_BYTES, as: 'owner' });
    expect(decoded.ok && decoded.message.k === 'join' && decoded.message.aud).toEqual(['deck']);
    deliver(owner.receive(requests[0]!.frame, PEER).out);
    expect(replica.installed).toBe('1');
  });
});

describe('an audience is authorized, never narrowed', () => {
  it('refuses an audience the policy denies, and does not serve a narrower one', () => {
    const { owner, core, replica, joined } = pair({
      authorize: request => request.type === 'read' && !request.audience.includes('deck'),
    });
    joined();

    const requested = replica.setAudience(['deck']);
    const answered = owner.receive(requested[0]!.frame, PEER);

    expect(answered.refused).toBe('aud-forbidden');
    expect(refusalOf(answered.out)).toBe('forbidden');
    // Not narrowed to the audience the caller may read: refused.
    expect(answered.out).toHaveLength(1);

    // And the caller is fenced with no view, not quietly left on the
    // old one.
    replica.receive(answered.out[0]!.frame);
    expect(replica.state).toBe('fenced');
    expect(core.getState()).toEqual(world.empty());
  });

  it('refuses a mixed request rather than serving the part it permits', () => {
    // Narrowing to "what you may read" would answer a different
    // question than the one asked, and the caller would have no way to
    // know its request was not honoured.
    const { owner, replica, joined } = pair({
      authorize: request => request.type === 'read' && !request.audience.includes('deck'),
    });
    joined();

    const requested = replica.setAudience(['crew', 'deck']);
    const answered = owner.receive(requested[0]!.frame, PEER);

    expect(answered.refused).toBe('aud-forbidden');
    expect(refusalOf(answered.out)).toBe('forbidden');
    // No manifest: not even the permitted half.
    expect(answered.out).toHaveLength(1);
  });

  it('authorizes the requested audience, not the bound one', () => {
    const seen: readonly string[][] = [];
    const audiences: string[][] = [];
    const { replica, joined, send } = pair({
      authorize: request => {
        if (request.type === 'read') audiences.push([...request.audience]);
        return true;
      },
    });
    joined();

    send(replica.setAudience(['deck']));

    expect(audiences).toEqual([['crew'], ['deck']]);
    expect(seen).toEqual([]);
  });

  it('cannot even form a request past the label bound', () => {
    // The bound is the parser's (rung 6), and the encoder refuses to
    // produce what the parser would reject — so an over-long audience
    // is unrepresentable rather than refused at dispatch. That is why
    // the owner carries no second check for it.
    const { replica, joined } = pair();
    joined();
    const many = Array.from({ length: 33 }, (_unused, i) => `label${String(i)}`);

    expect(() => replica.setAudience(many)).toThrow(/audience-too-many/);
  });

  it('never answers a control `not-ready`', () => {
    // §1.7b: `not-ready` refuses gameplay only. A control on a live
    // handle is admissible even mid-emission, and a refused one says
    // why in some other code.
    const { owner, replica, joined } = pair({
      authorize: request => request.type === 'read' && !request.audience.includes('deck'),
    });
    joined();
    const h = replica.handle as Hex;

    const controls = [
      encodeMessage({ k: 'aud', q: 'a'.repeat(16) as Hex, h, aud: ['deck'] }),
      encodeMessage({ k: 'resume', q: 'b'.repeat(16) as Hex, h, aud: ['deck'] }),
      encodeMessage({ k: 'resync', q: 'c'.repeat(16) as Hex, h, g: '1', have: '1' }),
      encodeMessage({ k: 'alive', q: 'd'.repeat(16) as Hex, h }),
    ];

    for (const frame of controls) {
      expect(refusalOf(owner.receive(frame, PEER).out)).not.toBe('not-ready');
    }
  });
});

describe('a reconnect resumes, and states what the caller wants now', () => {
  it('retains the view, marks it stale, and asks with the desired audience', () => {
    const { core, replica, joined } = pair();
    joined();
    const installed = core.getState();

    const requests = replica.reconnect();

    // Retained, not cleared: what the caller may see has not changed,
    // only whether it is current.
    expect(core.getState()).toBe(installed);
    expect(core.getStatus()).toMatchObject({ phase: 'reconnecting', stale: true });
    expect(requests.map(r => r.kind)).toEqual(['resume']);
    const decoded = decodeMessage(requests[0]!.frame, { maxBytes: MAX_EVENT_BYTES, as: 'owner' });
    expect(decoded.ok && decoded.message.k === 'resume' && decoded.message.aud).toEqual(['crew']);
  });

  it('resumes with the LATEST desired audience, not the owner’s older one', () => {
    // The sequence that forces it: the caller narrows, the session is
    // lost before the owner answers, and the owner still holds the
    // wider audience. A bare resume would re-deliver it.
    const { core, replica, joined, settle } = pair();
    joined();
    replica.setAudience(['deck']);

    const requests = replica.reconnect();
    const decoded = decodeMessage(requests[0]!.frame, { maxBytes: MAX_EVENT_BYTES, as: 'owner' });

    expect(decoded.ok && decoded.message.k === 'resume' && decoded.message.aud).toEqual(['deck']);
    settle(requests);
    expect(core.getState()).toEqual({ crew: {}, deck: { hoist: 2 } });
  });

  it('rejoins instead of resuming when no handle was learned', () => {
    // Only `join` creates a handle, and a forged `h` is exactly what a
    // `resume` must never be able to mint.
    const { replica } = pair();
    replica.join();

    const requests = replica.reconnect();

    expect(requests.map(r => r.kind)).toEqual(['join']);
    expect(replica.handle).toBe(null);
  });

  it('refuses a `resume` for an unknown handle rather than allocating one', () => {
    const { owner } = pair();

    const forged = owner.receive(
      encodeMessage({ k: 'resume', q: 'f'.repeat(16) as Hex, h: 'e'.repeat(32) as Hex, aud: ['crew'] }),
      PEER,
    );

    expect(forged.refused).toBe('handle-unknown');
    expect(refusalOf(forged.out)).toBe('closed');
    expect(owner.handleCount).toBe(0);
  });

  it('retires the lost session’s assembly', () => {
    const { owner, replica, deliver } = pair();
    const join = replica.join();
    const out = owner.receive(join.frame, PEER).out;
    // Only the manifest arrives; the session dies mid-assembly.
    deliver([out[0]!]);

    const requests = replica.reconnect();
    // The rest of the old snapshot cannot complete on the new session.
    expect(deliver(out.slice(1))).toEqual([]);
    expect(replica.installed).toBe(null);

    // And the resume installs a fresh generation.
    deliver(owner.receive(requests[0]!.frame, PEER).out);
    expect(replica.state).toBe('ready');
    expect(replica.installed).toBe('2');
  });
});

describe('a projection that cannot be taken is deferred, never refused', () => {
  it('holds the control and emits when a projection becomes available', () => {
    const { owner, core, replica, joined, projectable, send, deliver } = pair();
    joined();

    projectable.value = false;
    const requested = replica.setAudience(['deck']);
    const answered = owner.receive(requested[0]!.frame, PEER);

    // Deferred: accepted, nothing emitted, and no refusal — a control
    // never has to interpret `not-ready`.
    expect(answered.refused).toBe(null);
    expect(answered.out).toEqual([]);
    expect(owner.deferredCount).toBe(1);
    expect(core.getState()).toEqual(world.empty());

    projectable.value = true;
    deliver(owner.resumeDeferred().out);

    expect(replica.state).toBe('ready');
    expect(core.getState()).toEqual({ crew: {}, deck: { hoist: 2 } });
  });

  it('replaces a pending projection with a newer control', () => {
    const { owner, replica, joined, projectable, deliver } = pair();
    joined();
    projectable.value = false;

    owner.receive(replica.setAudience(['deck'])[0]!.frame, PEER);
    owner.receive(replica.setAudience(['crew', 'deck'])[0]!.frame, PEER);

    // One pending projection per handle, not a queue.
    expect(owner.deferredCount).toBe(1);

    projectable.value = true;
    deliver(owner.resumeDeferred().out);

    expect(replica.state).toBe('ready');
    expect(replica.desired).toEqual(['crew', 'deck']);
  });

  it('emits nothing while a projection still cannot be taken', () => {
    const { owner, replica, joined, projectable } = pair();
    joined();
    projectable.value = false;
    owner.receive(replica.setAudience(['deck'])[0]!.frame, PEER);

    const early = owner.resumeDeferred();

    expect(early.out).toEqual([]);
    // And the pending projection is KEPT, not consumed by the attempt.
    expect(owner.deferredCount).toBe(1);
    projectable.value = true;
    expect(owner.resumeDeferred().out.length).toBeGreaterThan(0);
  });

  it('never acquires a pending projection for a dead handle', () => {
    const { owner, replica, joined, projectable } = pair();
    joined();
    const h = replica.handle as Hex;
    owner.receive(encodeMessage({ k: 'leave', q: '9'.repeat(16) as Hex, h }), PEER);

    projectable.value = false;
    const orphan = owner.receive(
      encodeMessage({ k: 'aud', q: '8'.repeat(16) as Hex, h, aud: ['deck'] }),
      PEER,
    );

    expect(orphan.refused).toBe('handle-unknown');
    expect(owner.deferredCount).toBe(0);
  });

  it('revalidates on completion: a handle lost meanwhile emits nothing', () => {
    const { owner, replica, joined, projectable, clock } = pair();
    joined();
    projectable.value = false;
    owner.receive(replica.setAudience(['deck'])[0]!.frame, PEER);
    expect(owner.deferredCount).toBe(1);

    // The handle expires while the projection is unavailable.
    clock.value = 60_000;
    projectable.value = true;
    const emitted = owner.resumeDeferred();

    expect(emitted.out).toEqual([]);
    expect(owner.deferredCount).toBe(0);
    expect(owner.handleCount).toBe(0);
  });

  it('retires a pending projection with its handle', () => {
    const { owner, replica, joined, projectable } = pair();
    joined();
    const h = replica.handle as Hex;
    projectable.value = false;
    owner.receive(replica.setAudience(['deck'])[0]!.frame, PEER);
    expect(owner.deferredCount).toBe(1);

    owner.receive(encodeMessage({ k: 'leave', q: '7'.repeat(16) as Hex, h }), PEER);

    expect(owner.deferredCount).toBe(0);
  });

  it('keeps the audience it authorized when the projection is deferred', () => {
    // The audience is rebound before the deferral, so what is emitted
    // later can only be the audience that was authorized then.
    const audiences: string[][] = [];
    const { owner, replica, joined, projectable, deliver, core } = pair({
      authorize: request => {
        if (request.type === 'read') audiences.push([...request.audience]);
        return true;
      },
    });
    joined();

    projectable.value = false;
    owner.receive(replica.setAudience(['deck'])[0]!.frame, PEER);
    projectable.value = true;
    deliver(owner.resumeDeferred().out);

    expect(audiences).toEqual([['crew'], ['deck']]);
    expect(core.getState()).toEqual({ crew: {}, deck: { hoist: 2 } });
  });
});

describe('a control supersedes an emission; the owner does not supersede its own', () => {
  it('retires the unsent chunks of a superseded installation', () => {
    // A large projection so the emission is many chunks, then an `aud`
    // mid-emission. The superseded chunks must never be emitted.
    const crew: Record<string, number> = {};
    for (let i = 0; i < 700; i += 1) crew[`crew${String(i)}`] = i;
    const { owner, replica, deliver, core } = pair();
    owner.commit({ crew, deck: { hoist: 2 } });

    const join = replica.join();
    const out = owner.receive(join.frame, PEER).out;
    expect(out.length).toBeGreaterThan(2);
    deliver([out[0]!, out[1]!]);
    expect(replica.state).toBe('installing');

    const requested = replica.setAudience(['deck']);
    const answered = owner.receive(requested[0]!.frame, PEER);
    expect(answered.refused).toBe(null);

    // The rest of the old snapshot is refused by the replica, and the
    // new one installs.
    expect(deliver(out.slice(2))).toEqual([]);
    deliver(answered.out);

    expect(replica.state).toBe('ready');
    expect(core.getState()).toEqual({ crew: {}, deck: { hoist: 2 } });
  });

  it('admits a control while an emission is in flight', () => {
    const { owner, replica, deliver } = pair();
    const join = replica.join();
    const out = owner.receive(join.frame, PEER).out;
    deliver([out[0]!]);
    const h = replica.handle as Hex;

    // Mid-emission, every lifecycle control is admissible.
    for (const frame of [
      encodeMessage({ k: 'alive', q: '1'.repeat(16) as Hex, h }),
      encodeMessage({ k: 'resync', q: '2'.repeat(16) as Hex, h, g: '1', have: '0' }),
      encodeMessage({ k: 'aud', q: '3'.repeat(16) as Hex, h, aud: ['deck'] }),
    ]) {
      expect(owner.receive(frame, PEER).refused).toBe(null);
    }
  });
});
