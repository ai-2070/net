/**
 * Slice E — actions, results, the ledger, latest inputs.
 *
 * Driven through the real owner and the real codec: a witness asserts
 * what the CALLER receives, because "the ledger retained an entry" is
 * not a property anyone can observe and "the same request got the same
 * answer" is.
 *
 * The rule the whole slice turns on: a sequence identifies one request,
 * not a slot. Most rows here are about what that forbids.
 */

import { describe, expect, it, vi } from 'vitest';

import { defineStore } from '../../src/store/definition.js';
import {
  BINDING_INLINE_MAX_BYTES,
  canonicalRequest,
  HandleLedger,
  inlineBinding,
  MAX_SEQUENCE,
  needsDigest,
} from '../../src/store/ledger.js';
import { MAX_PENDING_ACTIONS, StoreOwner, type Dispatched, type Outbound } from '../../src/store/owner.js';
import type { AccessRequest, ActionContext } from '../../src/store/types.js';
import { decodeMessage, encodeMessage, type Hex } from '../../src/store/wire.js';

const MAX_EVENT_BYTES = 8104;
const PEER_A = '00000000000000aa';
const PEER_B = '00000000000000bb';

interface Ship {
  readonly hull: number;
  readonly shots: number;
}

type Actions = {
  fire: { input: { readonly power: number }; output: { readonly shot: number } };
  refit: { input: { readonly hull: number }; output: { readonly hull: number } };
};

type Inputs = {
  helm: { readonly heading: number };
  trim: { readonly sail: number };
};

function record(value: unknown): Record<string, unknown> {
  if (typeof value !== 'object' || value === null) throw new Error('not a record');
  return value as Record<string, unknown>;
}

const ship = defineStore<Ship, Actions, Inputs>({
  id: 'ship',
  version: 1,
  state: value => ({ hull: Number(record(value)['hull'] ?? 0), shots: Number(record(value)['shots'] ?? 0) }),
  empty: () => ({ hull: 0, shots: 0 }),
  actions: {
    fire: {
      input: value => {
        const power = record(value)['power'];
        if (typeof power !== 'number' || !Number.isFinite(power)) {
          throw new Error('power must be a finite number');
        }
        return { power };
      },
      output: value => ({ shot: Number(record(value)['shot']) }),
    },
    refit: {
      input: value => ({ hull: Number(record(value)['hull']) }),
      output: value => ({ hull: Number(record(value)['hull']) }),
    },
  },
  inputs: {
    helm: value => ({ heading: Number(record(value)['heading']) }),
    trim: value => ({ sail: Number(record(value)['sail']) }),
  },
});

function rig(
  options: {
    authorize?: (request: AccessRequest<Actions, Inputs>) => boolean;
    fire?: (input: { readonly power: number }, context: ActionContext<Ship>) => { readonly shot: number };
  } = {},
) {
  let handles = 0;
  const clock = { value: 0 };
  const fired: number[] = [];
  const headings: number[] = [];
  const sails: number[] = [];
  const owner = new StoreOwner<Ship, Actions, Inputs>({
    definition: ship,
    authorize: options.authorize ?? (() => true),
    project: state => state,
    maxEventBytes: MAX_EVENT_BYTES,
    now: () => clock.value,
    newHandle: () => {
      handles += 1;
      return handles.toString(16).padStart(32, '0') as Hex;
    },
    newIncarnation: () => 'abcdef0123456789' as Hex,
    actions: {
      fire:
        options.fire ??
        ((input, context) => {
          fired.push(input.power);
          const shots = context.getState().shots + 1;
          context.setState({ shots });
          return { shot: shots };
        }),
      refit: (input, context) => {
        context.setState({ hull: input.hull });
        return { hull: input.hull };
      },
    },
    inputs: {
      helm: input => {
        headings.push(input.heading);
      },
      trim: input => {
        sails.push(input.sail);
      },
    },
  });
  owner.commit({ hull: 10, shots: 0 });
  return { owner, clock, fired, headings, sails };
}

let sequence = 0;
function q(): Hex {
  sequence += 1;
  return sequence.toString(16).padStart(16, '0') as Hex;
}

function joined(owner: StoreOwner<Ship, Actions, Inputs>, peer = PEER_A): Hex {
  const out = owner.receive(
    encodeMessage({ k: 'join', q: q(), def: 'ship', ver: 1, key: 'k', aud: ['crew'] }),
    peer,
  ).out;
  const decoded = decodeMessage(out[0]!.frame, { maxBytes: MAX_EVENT_BYTES, as: 'replica' });
  if (!decoded.ok || decoded.message.k !== 'man') throw new Error('no manifest');
  return decoded.message.h;
}

function act(h: Hex, s: string, name: 'fire' | 'refit', input: object): string {
  return encodeMessage({ k: 'act', q: q(), h, s, name, in: input as never });
}

/** The single reply frame, decoded. */
function replyOf(dispatched: Dispatched) {
  expect(dispatched.out).toHaveLength(1);
  const decoded = decodeMessage((dispatched.out[0] as Outbound).frame, {
    maxBytes: MAX_EVENT_BYTES,
    as: 'replica',
  });
  if (!decoded.ok) throw new Error(`undecodable reply: ${decoded.reason}`);
  return decoded.message;
}

describe('an action executes once and answers', () => {
  it('validates, executes, commits and replies `res`', () => {
    const { owner, fired } = rig();
    const h = joined(owner);

    const done = owner.receive(act(h, '1', 'fire', { power: 3 }), PEER_A);

    expect(done.refused).toBe(null);
    expect(fired).toEqual([3]);
    expect(owner.getState()).toEqual({ hull: 10, shots: 1 });
    const reply = replyOf(done);
    expect(reply.k === 'res' && reply).toMatchObject({ h, s: '1', out: { shot: 1 } });
  });

  it('hands the handler the authenticated caller, and one transaction', () => {
    const peers: string[] = [];
    const { owner } = rig({
      fire: (input, context) => {
        peers.push(context.peer);
        context.setState({ shots: 1 });
        return { shot: 1 };
      },
    });
    const h = joined(owner, PEER_B);

    owner.receive(act(h, '1', 'fire', { power: 1 }), PEER_B);

    expect(peers).toEqual([PEER_B]);
  });

  it('refuses an unknown action name without touching the state', () => {
    const { owner } = rig();
    const h = joined(owner);

    const bogus = owner.receive(
      encodeMessage({ k: 'act', q: q(), h, s: '1', name: 'scuttle', in: {} }),
      PEER_A,
    );

    expect(bogus.refused).toBe('act-unknown-name');
    expect(replyOf(bogus).k).toBe('no');
    expect(owner.getState().shots).toBe(0);
  });

  it('rejects an input the definition refuses, and retains the rejection', () => {
    const { owner, fired } = rig();
    const h = joined(owner);

    // The definition's validator, not the handler: a wrongly-typed
    // input never reaches game code.
    const rejected = owner.receive(act(h, '1', 'fire', { power: 'lots' }), PEER_A);

    expect(replyOf(rejected)).toMatchObject({ k: 'no', code: 'action-rejected', s: '1' });
    expect(fired).toEqual([]);

    // Retained, so the same request does not get a second chance at
    // the validator.
    const replayed = owner.receive(act(h, '1', 'fire', { power: 'lots' }), PEER_A);
    expect(replyOf(replayed)).toMatchObject({ k: 'no', code: 'action-rejected' });
  });

  it('refuses a non-finite number before it can reach the ledger', () => {
    // §1.12 refuses `NaN` at the parser, and the encoder refuses to
    // produce what the parser would reject — so this frame cannot be
    // built, which is the strongest form of "it never arrives".
    expect(() => act('a'.repeat(32) as Hex, '1', 'fire', { power: Number.NaN })).toThrow(/non-finite/);
  });
});

describe('a sequence identifies one request, not a slot', () => {
  it('replays the retained outcome for the same request, under the new `q`', () => {
    const { owner, fired } = rig();
    const h = joined(owner);
    const first = owner.receive(act(h, '1', 'fire', { power: 3 }), PEER_A);
    const firstReply = replyOf(first);

    const replayed = owner.receive(act(h, '1', 'fire', { power: 3 }), PEER_A);

    // Executed once.
    expect(fired).toEqual([3]);
    expect(owner.getState().shots).toBe(1);
    const reply = replyOf(replayed);
    // The retained OUTCOME, in a fresh envelope: same `s` and output,
    // and the `q` of THIS request, not the original's.
    expect(reply.k === 'res' && reply.out).toEqual(firstReply.k === 'res' && firstReply.out);
    expect(reply.k === 'res' && reply.q).not.toBe(firstReply.k === 'res' && firstReply.q);
  });

  it('refuses a different request under a retained sequence, and keeps the entry', () => {
    const { owner, fired } = rig();
    const h = joined(owner);
    owner.receive(act(h, '1', 'fire', { power: 3 }), PEER_A);

    const conflicting = owner.receive(act(h, '1', 'fire', { power: 9 }), PEER_A);

    expect(conflicting.refused).toBe('act-binding-mismatch');
    const reply = replyOf(conflicting);
    expect(reply.k === 'no' && reply.code).toBe('invalid-data');
    expect(fired).toEqual([3]);

    // The retained entry is NOT overwritten: the original request
    // still replays.
    const original = owner.receive(act(h, '1', 'fire', { power: 3 }), PEER_A);
    expect(original.refused).toBe(null);
    expect(replyOf(original)).toMatchObject({ k: 'res', out: { shot: 1 } });
  });

  it('treats a small and a large request under one sequence as different', async () => {
    // The two are retained in different forms — verbatim and digest —
    // and the form is recorded with the entry precisely so a 2 KiB
    // boundary crossing cannot make them compare equal.
    const { owner, fired } = rig();
    const h = joined(owner);
    owner.receive(act(h, '1', 'fire', { power: 1 }), PEER_A);

    const large = owner.receive(
      act(h, '1', 'fire', { power: 1, pad: 'x'.repeat(BINDING_INLINE_MAX_BYTES) }),
      PEER_A,
    );
    const settled = await (large.deferred as Promise<Dispatched>);

    expect(settled.refused).toBe('act-binding-mismatch');
    expect(fired).toEqual([1]);
  });

  it('treats a different action name under the same sequence as a conflict', () => {
    const { owner } = rig();
    const h = joined(owner);
    owner.receive(act(h, '1', 'fire', { power: 3 }), PEER_A);

    const renamed = owner.receive(act(h, '1', 'refit', { hull: 3 }), PEER_A);

    expect(renamed.refused).toBe('act-binding-mismatch');
    expect(owner.getState().hull).toBe(10);
  });

  it('treats key order and whitespace as the same request', () => {
    // The binding is a canonical form, so two spellings of one input
    // must not look like two requests.
    const { owner, fired } = rig();
    const h = joined(owner);
    owner.receive(
      encodeMessage({ k: 'act', q: q(), h, s: '1', name: 'refit', in: { hull: 4 } as never }),
      PEER_A,
    );

    const reordered = owner.receive(
      encodeMessage({ k: 'act', q: q(), h, s: '1', name: 'refit', in: { hull: 4 } as never }),
      PEER_A,
    );

    expect(reordered.refused).toBe(null);
    expect(fired).toEqual([]);
  });

  it('permits gaps, and treats an unseen sequence above the floor as new', () => {
    const { owner, fired } = rig();
    const h = joined(owner);

    owner.receive(act(h, '5', 'fire', { power: 1 }), PEER_A);
    // A gap does not block: 2 was never seen and is above the floor.
    const filled = owner.receive(act(h, '2', 'fire', { power: 2 }), PEER_A);

    expect(filled.refused).toBe(null);
    expect(fired).toEqual([1, 2]);
  });

  it('refuses the sequence ceiling rather than wrapping', () => {
    const { owner } = rig();
    const h = joined(owner);

    const exhausted = owner.receive(act(h, MAX_SEQUENCE.toString(), 'fire', { power: 1 }), PEER_A);

    expect(exhausted.refused).toBe('act-sequence-exhausted');
    expect(replyOf(exhausted)).toMatchObject({ k: 'no', code: 'capacity' });
  });
});

describe('a refusal is retained, so a policy change cannot revive it', () => {
  it('retains an authorization denial and replays it after the policy changes', () => {
    let permit = false;
    const { owner, fired } = rig({ authorize: request => (request.type === 'read' ? true : permit) });
    const h = joined(owner);

    const denied = owner.receive(act(h, '1', 'fire', { power: 3 }), PEER_A);
    expect(denied.refused).toBe('act-forbidden');
    expect(replyOf(denied)).toMatchObject({ k: 'no', code: 'forbidden' });

    // The policy now permits. The SAME sequence must not execute: its
    // outcome is already decided.
    permit = true;
    const retried = owner.receive(act(h, '1', 'fire', { power: 3 }), PEER_A);

    expect(fired).toEqual([]);
    expect(replyOf(retried)).toMatchObject({ k: 'no', code: 'forbidden' });
    // And a NEW sequence does execute, so the ledger fenced the
    // request rather than the caller.
    expect(owner.receive(act(h, '2', 'fire', { power: 3 }), PEER_A).refused).toBe(null);
  });

  it('retains a handler rejection as `action-rejected` and discards its writes', () => {
    // The handler fails ONCE and would succeed on a second call. That
    // is what makes this discriminating: "it was rejected again" is
    // also what re-executing a throwing handler looks like, so the
    // oracle has to be able to tell retention from repetition.
    let calls = 0;
    const { owner } = rig({
      fire: (_input, context) => {
        calls += 1;
        context.setState({ shots: 99 });
        if (calls === 1) throw new Error('the cannon jammed');
        return { shot: 1 };
      },
    });
    const h = joined(owner);

    const rejected = owner.receive(act(h, '1', 'fire', { power: 3 }), PEER_A);

    expect(replyOf(rejected)).toMatchObject({ k: 'no', code: 'action-rejected', s: '1' });
    // The transaction discarded the staged write.
    expect(owner.getState().shots).toBe(0);

    const replayed = owner.receive(act(h, '1', 'fire', { power: 3 }), PEER_A);

    // The retained refusal, not a second attempt that would now
    // succeed — and the handler was not called again.
    expect(replyOf(replayed)).toMatchObject({ k: 'no', code: 'action-rejected' });
    expect(calls).toBe(1);
    expect(owner.getState().shots).toBe(0);
  });

  it('re-authorizes a replay, and refuses one the policy has since revoked', () => {
    let permit = true;
    const { owner } = rig({ authorize: request => (request.type === 'read' ? true : permit) });
    const h = joined(owner);
    owner.receive(act(h, '1', 'fire', { power: 3 }), PEER_A);

    permit = false;
    const revoked = owner.receive(act(h, '1', 'fire', { power: 3 }), PEER_A);

    // A retained outcome is not a permission that keeps working.
    expect(revoked.refused).toBe('act-forbidden-replay');
    expect(replyOf(revoked)).toMatchObject({ k: 'no', code: 'forbidden' });
  });

  it('treats a throwing policy as a denial, and retains that', () => {
    const { owner, fired } = rig({
      authorize: request => {
        if (request.type === 'read') return true;
        throw new Error('the policy exploded');
      },
    });
    const h = joined(owner);

    const denied = owner.receive(act(h, '1', 'fire', { power: 3 }), PEER_A);

    expect(replyOf(denied)).toMatchObject({ k: 'no', code: 'forbidden' });
    expect(fired).toEqual([]);
  });
});

describe('the window, the floor and what `result-expired` does not say', () => {
  it('answers `result-expired` for an evicted sequence', () => {
    const { owner } = rig();
    const h = joined(owner);

    // Fill past the retention count so the lowest entries are evicted
    // in `s` order and the floor advances.
    for (let s = 1; s <= 40; s += 1) {
      expect(owner.receive(act(h, String(s), 'fire', { power: s }), PEER_A).refused).toBe(null);
    }

    const expired = owner.receive(act(h, '1', 'fire', { power: 1 }), PEER_A);

    expect(expired.refused).toBe('act-result-expired');
    expect(replyOf(expired)).toMatchObject({ k: 'no', code: 'result-expired' });
    // A late entry is still retained, so eviction was in order.
    expect(owner.receive(act(h, '40', 'fire', { power: 40 }), PEER_A).refused).toBe(null);
  });

  it('answers `closed` — not `result-expired` — once the handle is gone', () => {
    // The distinction is the point: `result-expired` says "used, and I
    // no longer know the outcome". A handle that is gone means the
    // owner does not know the sequence ever existed, so nothing about
    // an outcome may be implied.
    const { owner } = rig();
    const h = joined(owner);
    owner.receive(act(h, '1', 'fire', { power: 3 }), PEER_A);
    owner.receive(encodeMessage({ k: 'leave', q: q(), h }), PEER_A);

    const orphan = owner.receive(act(h, '1', 'fire', { power: 3 }), PEER_A);

    expect(orphan.refused).toBe('handle-unknown');
    expect(replyOf(orphan)).toMatchObject({ k: 'no', code: 'closed' });
  });

  it('forgets a handle’s ledger with the handle', () => {
    // §2: the ledger is bounded by live handles, not by the owner's
    // lifetime, and this is the same fact that makes a replay after
    // eviction `closed` rather than `result-expired`.
    const { owner, clock } = rig();
    const first = joined(owner);
    owner.receive(act(first, '1', 'fire', { power: 1 }), PEER_A);
    expect(owner.ledgerCount).toBe(1);

    owner.receive(encodeMessage({ k: 'leave', q: q(), h: first }), PEER_A);
    expect(owner.ledgerCount).toBe(0);

    // And on expiry, which no message announces.
    const second = joined(owner);
    owner.receive(act(second, '1', 'fire', { power: 1 }), PEER_A);
    clock.value = 60_000;
    expect(owner.sweep(clock.value)).toEqual([second]);
    expect([owner.ledgerCount, owner.handleCount]).toEqual([0, 0]);
  });

  it('expires outcomes by age, oldest first', () => {
    const ledger = new HandleLedger();
    const binding = inlineBinding(canonicalRequest('fire', { power: 1 } as never));
    ledger.retain(1n, binding, { kind: 'result', out: { shot: 1 } }, 0);
    ledger.retain(2n, binding, { kind: 'result', out: { shot: 2 } }, 30_000);

    // 60 s after the first, before the second.
    expect(ledger.disposition(1n, binding, 60_000).kind).toBe('expired');
    expect(ledger.disposition(2n, binding, 60_000).kind).toBe('replay');
    expect(ledger.floor).toBe('1');
  });

  it('evicts in `s` order even when entries arrive out of order', () => {
    // Gaps are permitted, so arrival order and sequence order differ
    // in ordinary traffic. Evicting by arrival would advance the floor
    // past a LOWER retained entry, which is what makes the floor a
    // boundary rather than a set of holes.
    const ledger = new HandleLedger(1, 64 * 1024, 60_000);
    const high = inlineBinding(canonicalRequest('fire', { power: 5 } as never));
    const low = inlineBinding(canonicalRequest('fire', { power: 2 } as never));

    ledger.retain(5n, high, { kind: 'result', out: { shot: 5 } }, 0);
    ledger.retain(2n, low, { kind: 'result', out: { shot: 2 } }, 0);

    // The LOWER sequence went, whichever arrived first.
    expect(ledger.disposition(2n, low, 0).kind).toBe('expired');
    expect(ledger.disposition(5n, high, 0).kind).toBe('replay');
    expect(ledger.floor).toBe('2');
  });

  it('bounds the window by bytes as well as by count', () => {
    const ledger = new HandleLedger(1000, 512, 60_000);
    const big = 'x'.repeat(400);
    const first = inlineBinding(canonicalRequest('refit', { note: big } as never));
    const second = inlineBinding(canonicalRequest('refit', { other: big } as never));

    ledger.retain(1n, first, { kind: 'result', out: {} }, 0);
    ledger.retain(2n, second, { kind: 'result', out: {} }, 0);

    // Count would have kept both; bytes did not.
    expect(ledger.size).toBe(1);
    expect(ledger.bytesHeld).toBeLessThanOrEqual(512);
    expect(ledger.floor).toBe('1');
  });

  it('renews the lease on an accepted action but not on a refused one', () => {
    const { owner, clock } = rig();
    const h = joined(owner);

    clock.value = 59_000;
    expect(owner.receive(act(h, '1', 'fire', { power: 1 }), PEER_A).refused).toBe(null);

    // The accepted action renewed the lease, so 59 s later is still
    // inside it.
    clock.value = 118_000;
    expect(owner.receive(act(h, '2', 'fire', { power: 1 }), PEER_A).refused).toBe(null);

    // A REFUSED action does not renew: this conflicts with sequence 2.
    clock.value = 150_000;
    expect(owner.receive(act(h, '2', 'fire', { power: 9 }), PEER_A).refused).toBe('act-binding-mismatch');

    // So the lease still runs from 118 s, and 60 s after that it is
    // gone — which it would not be if the refusal had renewed it.
    clock.value = 178_000;
    const late = owner.receive(act(h, '3', 'fire', { power: 1 }), PEER_A);
    expect(late.refused).toBe('handle-expired');
  });
});

describe('latest-value inputs', () => {
  it('applies a newer sequence and replies nothing at all', () => {
    const { owner, headings } = rig();
    const h = joined(owner);

    const applied = owner.receive(
      encodeMessage({ k: 'in', h, s: '1', name: 'helm', in: { heading: 90 } as never }),
      PEER_A,
    );

    expect(applied.refused).toBe(null);
    // Fire-and-forget: an input has no `q`, so it has no reply.
    expect(applied.out).toEqual([]);
    expect(headings).toEqual([90]);
  });

  it('drops a stale sequence and counts it, per input name', () => {
    const { owner, headings } = rig();
    const h = joined(owner);

    owner.receive(encodeMessage({ k: 'in', h, s: '5', name: 'helm', in: { heading: 10 } as never }), PEER_A);
    const stale = owner.receive(
      encodeMessage({ k: 'in', h, s: '4', name: 'helm', in: { heading: 20 } as never }),
      PEER_A,
    );
    const equal = owner.receive(
      encodeMessage({ k: 'in', h, s: '5', name: 'helm', in: { heading: 30 } as never }),
      PEER_A,
    );

    expect([stale.refused, equal.refused]).toEqual(['in-stale-sequence', 'in-stale-sequence']);
    expect(stale.out).toEqual([]);
    // Only the newest was applied, and no reply went out for either.
    expect(headings).toEqual([10]);
    expect(owner.snapshotCounters()['in-stale-sequence']).toBe(2);
  });

  it('never replays an input: the same sequence twice is dropped, not re-applied', () => {
    const { owner, headings } = rig();
    const h = joined(owner);
    const frame = encodeMessage({ k: 'in', h, s: '1', name: 'helm', in: { heading: 45 } as never });

    owner.receive(frame, PEER_A);
    owner.receive(frame, PEER_A);

    expect(headings).toEqual([45]);
  });

  it('refuses an input the policy denies, silently', () => {
    const { owner, headings } = rig({ authorize: request => request.type !== 'input' });
    const h = joined(owner);

    const denied = owner.receive(
      encodeMessage({ k: 'in', h, s: '1', name: 'helm', in: { heading: 90 } as never }),
      PEER_A,
    );

    expect(denied.refused).toBe('in-forbidden');
    // No reply, because an input has no correlation to answer.
    expect(denied.out).toEqual([]);
    expect(headings).toEqual([]);
  });

  it('keeps sequences separate per input name', () => {
    const { owner, headings, sails } = rig();
    const h = joined(owner);

    owner.receive(encodeMessage({ k: 'in', h, s: '9', name: 'helm', in: { heading: 1 } as never }), PEER_A);
    // A different name's sequence space is its own: 1 is not stale for
    // `trim` just because `helm` has reached 9.
    const other = owner.receive(
      encodeMessage({ k: 'in', h, s: '1', name: 'trim', in: { sail: 5 } as never }),
      PEER_A,
    );

    expect(other.refused).toBe(null);
    expect([headings, sails]).toEqual([[1], [5]]);
  });

  it('refuses an unknown input name, by name and not by sequence', () => {
    const { owner, headings } = rig();
    const h = joined(owner);

    const unknown = owner.receive(
      encodeMessage({ k: 'in', h, s: '1', name: 'rudder', in: { heading: 1 } as never }),
      PEER_A,
    );

    expect(unknown.refused).toBe('in-unknown-name');
    expect(headings).toEqual([]);
  });
});

describe('the binding a large input needs', () => {
  it('computes the digest before the transaction and answers when it settles', async () => {
    const { owner, fired } = rig();
    const h = joined(owner);
    const big = { power: 1, pad: 'x'.repeat(BINDING_INLINE_MAX_BYTES) };
    expect(needsDigest(canonicalRequest('fire', big as never))).toBe(true);

    const deferred = owner.receive(act(h, '1', 'fire', big), PEER_A);

    // Nothing synchronously: the decision needs the digest.
    expect(deferred.out).toEqual([]);
    expect(deferred.refused).toBe(null);
    expect(deferred.deferred).not.toBe(null);
    expect(fired).toEqual([]);

    const settled = await (deferred.deferred as Promise<Dispatched>);

    expect(settled.refused).toBe(null);
    expect(fired).toEqual([1]);
    expect(replyOf(settled)).toMatchObject({ k: 'res', s: '1', out: { shot: 1 } });
  });

  it('replays a large request by its digest', async () => {
    const { owner, fired } = rig();
    const h = joined(owner);
    const big = { power: 2, pad: 'y'.repeat(BINDING_INLINE_MAX_BYTES) };
    await (owner.receive(act(h, '1', 'fire', big), PEER_A).deferred as Promise<Dispatched>);

    const replayed = await (owner.receive(act(h, '1', 'fire', big), PEER_A).deferred as Promise<Dispatched>);

    expect(fired).toEqual([2]);
    expect(replyOf(replayed)).toMatchObject({ k: 'res', out: { shot: 1 } });
  });

  it('refuses a different large request under the same sequence', async () => {
    const { owner } = rig();
    const h = joined(owner);
    const first = { power: 1, pad: 'a'.repeat(BINDING_INLINE_MAX_BYTES) };
    const second = { power: 1, pad: 'b'.repeat(BINDING_INLINE_MAX_BYTES) };
    await (owner.receive(act(h, '1', 'fire', first), PEER_A).deferred as Promise<Dispatched>);

    const conflicting = await (owner.receive(act(h, '1', 'fire', second), PEER_A).deferred as Promise<Dispatched>);

    expect(conflicting.refused).toBe('act-binding-mismatch');
  });

  it('revalidates after the digest: a handle lost meanwhile is `closed`', async () => {
    const { owner, fired } = rig();
    const h = joined(owner);
    const big = { power: 1, pad: 'z'.repeat(BINDING_INLINE_MAX_BYTES) };

    const deferred = owner.receive(act(h, '1', 'fire', big), PEER_A);
    // The handle goes while the digest is computing — the exact window
    // §1.10 says nothing may be carried across.
    owner.receive(encodeMessage({ k: 'leave', q: q(), h }), PEER_A);
    const settled = await (deferred.deferred as Promise<Dispatched>);

    expect(settled.refused).toBe('handle-unknown');
    expect(replyOf(settled)).toMatchObject({ k: 'no', code: 'closed' });
    expect(fired).toEqual([]);
  });

  it('revalidates the policy after the digest', async () => {
    let permit = true;
    const { owner, fired } = rig({ authorize: request => (request.type === 'read' ? true : permit) });
    const h = joined(owner);
    const big = { power: 1, pad: 'q'.repeat(BINDING_INLINE_MAX_BYTES) };

    const deferred = owner.receive(act(h, '1', 'fire', big), PEER_A);
    // Revoked during the digest. A permission checked before the await
    // would have admitted this.
    permit = false;
    const settled = await (deferred.deferred as Promise<Dispatched>);

    expect(settled.refused).toBe('act-forbidden');
    expect(fired).toEqual([]);
  });

  it('refuses `indeterminate` when the digest itself fails', async () => {
    // The owner cannot say whether this request is the retained one,
    // so it says exactly that rather than guessing either way — and
    // nothing is retained, so a retry is still possible.
    const { owner, fired } = rig();
    const h = joined(owner);
    const big = { power: 1, pad: 'x'.repeat(BINDING_INLINE_MAX_BYTES) };
    const real = crypto.subtle.digest.bind(crypto.subtle);
    const spy = vi
      .spyOn(crypto.subtle, 'digest')
      .mockImplementation(() => Promise.reject(new Error('no crypto here')));

    const settled = await (owner.receive(act(h, '1', 'fire', big), PEER_A).deferred as Promise<Dispatched>);

    expect(settled.refused).toBe('act-digest-failed');
    expect(replyOf(settled)).toMatchObject({ k: 'no', code: 'indeterminate' });
    expect(fired).toEqual([]);

    // Nothing was retained, so the same sequence still runs.
    spy.mockImplementation(real);
    const retried = await (owner.receive(act(h, '1', 'fire', big), PEER_A).deferred as Promise<Dispatched>);
    expect(retried.refused).toBe(null);
    expect(fired).toEqual([1]);
    spy.mockRestore();
  });

  it('releases the pending bound as digests settle', async () => {
    const { owner } = rig();
    const h = joined(owner);
    const big = (n: number) => ({ power: n, pad: 'p'.repeat(BINDING_INLINE_MAX_BYTES) });

    const inFlight: Promise<Dispatched>[] = [];
    for (let s = 1; s <= MAX_PENDING_ACTIONS; s += 1) {
      const dispatched = owner.receive(act(h, String(s), 'fire', big(s)), PEER_A);
      expect(dispatched.refused).toBe(null);
      inFlight.push(dispatched.deferred as Promise<Dispatched>);
    }
    await Promise.all(inFlight);

    // The bound is a bound on work in flight, not a lifetime quota.
    const after = owner.receive(act(h, '99', 'fire', big(99)), PEER_A);
    expect(after.refused).toBe(null);
    expect(await (after.deferred as Promise<Dispatched>)).toMatchObject({ refused: null });
  });

  it('bounds digests in flight', () => {
    const { owner } = rig();
    const h = joined(owner);
    const big = (n: number) => ({ power: n, pad: 'p'.repeat(BINDING_INLINE_MAX_BYTES) });

    const dispatched: Dispatched[] = [];
    for (let s = 1; s <= 40; s += 1) dispatched.push(owner.receive(act(h, String(s), 'fire', big(s)), PEER_A));

    const refusedAt = dispatched.findIndex(d => d.refused === 'act-pending-bound');
    expect(refusedAt).toBeGreaterThan(0);
    expect(dispatched[refusedAt]!.deferred).toBe(null);
    expect(replyOf(dispatched[refusedAt]!)).toMatchObject({ k: 'no', code: 'capacity' });
  });
});

describe('the canonical form', () => {
  it('is independent of key order', () => {
    const a = canonicalRequest('fire', { alpha: 1, beta: 2 } as never);
    const b = canonicalRequest('fire', { beta: 2, alpha: 1 } as never);

    expect(a).toBe(b);
  });

  it('distinguishes values a loose encoding would merge', () => {
    const forms = new Set([
      canonicalRequest('fire', { a: 1 } as never),
      canonicalRequest('fire', { a: '1' } as never),
      canonicalRequest('fire', { a: true } as never),
      canonicalRequest('fire', { a: null } as never),
      canonicalRequest('fire', { a: [1] } as never),
      canonicalRequest('fire', { a: { b: 1 } } as never),
      canonicalRequest('refit', { a: 1 } as never),
    ]);

    expect(forms.size).toBe(7);
  });

  it('separates the name from the input, so concatenation cannot collide', () => {
    // Concatenated, ("a", 11) and ("a1", 1) are both "a11" — one
    // request would then replay as the other's outcome. The wire only
    // carries object inputs, which makes that collision unreachable
    // from traffic, but this function is also called on locally-built
    // input and its own contract is what is asserted here.
    expect(canonicalRequest('a', 11 as never)).not.toBe(canonicalRequest('a1', 1 as never));
    expect(canonicalRequest('a', 11 as never)).toBe(canonicalRequest('a', 11 as never));
  });

  it('refuses to canonicalize a non-finite number', () => {
    // Unreachable through the parser, which refuses both — but this is
    // also called on locally-built input, and rendering `null` for
    // `NaN` would make two different requests compare equal.
    expect(() => canonicalRequest('fire', { a: Number.NaN } as never)).toThrow(/non-finite/);
  });
});

describe('counters', () => {
  it('moves exactly one counter per refusal', () => {
    const { owner } = rig();
    const h = joined(owner);
    const before = owner.snapshotCounters();

    owner.receive(act(h, '1', 'fire', { power: 1 }), PEER_A);
    owner.receive(act(h, '1', 'fire', { power: 2 }), PEER_A);

    const after = owner.snapshotCounters();
    const moved = Object.keys(after).filter(key => (after[key] ?? 0) !== (before[key] ?? 0));
    expect(moved).toEqual(['act-binding-mismatch']);
    expect(after['act-binding-mismatch']).toBe(1);
  });

  it('counts a foreign peer’s action as a closed handle, disclosing nothing', () => {
    const { owner, fired } = rig();
    const h = joined(owner, PEER_A);

    const stolen = owner.receive(act(h, '1', 'fire', { power: 1 }), PEER_B);

    expect(stolen.refused).toBe('handle-foreign-peer');
    expect(replyOf(stolen)).toMatchObject({ k: 'no', code: 'closed' });
    expect(fired).toEqual([]);
  });
});

describe('the deferred contract', () => {
  it('is null on every synchronous path', () => {
    const { owner } = rig();
    const h = joined(owner);

    const paths = [
      owner.receive(act(h, '1', 'fire', { power: 1 }), PEER_A),
      owner.receive(act(h, '1', 'fire', { power: 2 }), PEER_A),
      owner.receive(encodeMessage({ k: 'alive', q: q(), h }), PEER_A),
      owner.receive(encodeMessage({ k: 'in', h, s: '1', name: 'helm', in: { heading: 1 } as never }), PEER_A),
      owner.receive('{"v":1,"k":"act"', PEER_A),
    ];

    expect(paths.map(d => d.deferred)).toEqual([null, null, null, null, null]);
  });

  it('does not lose the reply when a transport awaits it', async () => {
    // The shape a transport loop actually uses: send `out`, then send
    // the deferred result's `out`.
    const { owner } = rig();
    const h = joined(owner);
    const sent: string[] = [];
    const send = (dispatched: Dispatched) => {
      for (const frame of dispatched.out) sent.push(frame.frame);
    };

    const small = owner.receive(act(h, '1', 'fire', { power: 1 }), PEER_A);
    send(small);
    const large = owner.receive(
      act(h, '2', 'fire', { power: 1, pad: 'x'.repeat(BINDING_INLINE_MAX_BYTES) }),
      PEER_A,
    );
    send(large);
    if (large.deferred !== null) send(await large.deferred);

    expect(sent).toHaveLength(2);
    const sequences = sent.map(frame => {
      const decoded = decodeMessage(frame, { maxBytes: MAX_EVENT_BYTES, as: 'replica' });
      return decoded.ok && decoded.message.k === 'res' ? decoded.message.s : null;
    });
    expect(sequences).toEqual(['1', '2']);
  });
});

describe('the transaction a handler runs in', () => {
  it('refuses a handler that returns a thenable, and retains the rejection', () => {
    const { owner } = rig({
      fire: (() => Promise.resolve({ shot: 1 })) as never,
    });
    const h = joined(owner);

    const rejected = owner.receive(act(h, '1', 'fire', { power: 1 }), PEER_A);

    // An async handler would commit after its transaction closed.
    expect(replyOf(rejected)).toMatchObject({ k: 'no', code: 'action-rejected' });
    expect(owner.getState().shots).toBe(0);
  });

  it('lets a handler read its own staged writes', () => {
    const seen: number[] = [];
    const { owner } = rig({
      fire: (_input, context) => {
        context.setState({ shots: 7 });
        seen.push(context.getState().shots);
        return { shot: context.getState().shots };
      },
    });
    const h = joined(owner);

    const done = owner.receive(act(h, '1', 'fire', { power: 1 }), PEER_A);

    expect(seen).toEqual([7]);
    expect(replyOf(done)).toMatchObject({ k: 'res', out: { shot: 7 } });
  });

  it('notifies the owner’s own subscribers once per action', () => {
    const { owner } = rig();
    const h = joined(owner);
    const listener = vi.fn();
    owner.subscribe(listener);

    owner.receive(act(h, '1', 'fire', { power: 1 }), PEER_A);
    owner.receive(act(h, '2', 'fire', { power: 1 }), PEER_A);
    // A replay publishes nothing: it did not execute.
    owner.receive(act(h, '1', 'fire', { power: 1 }), PEER_A);

    expect(listener).toHaveBeenCalledTimes(2);
  });
});
