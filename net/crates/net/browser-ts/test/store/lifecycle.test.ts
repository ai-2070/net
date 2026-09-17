/**
 * The installation lifecycle, executed.
 *
 * Four blockers in the brief's normative tables were found by reading
 * them; these witnesses are what stops the fifth. Every assertion is
 * about **publication, the active assembly, the desired audience and
 * convergence** — never about a refusal code alone, because a model
 * that drops everything refuses correctly and converges never.
 *
 * The positive controls are therefore load-bearing: a happy join that
 * installs and applies deltas, an owner refresh that installs while
 * `ready`, a superseded assembly whose successor still publishes, and
 * every `converged()` assertion all fail under a drop-everything
 * implementation. Strictness cannot satisfy this file.
 *
 * Generations are never synthesised: every `g` comes from the owner
 * model's own allocator, so the retirement watermark is tested against
 * numbers the owner would really have issued.
 */

import { describe, expect, it } from 'vitest';

import {
  emitChunk,
  newOwner,
  newReplica,
  ownerStep,
  replicaStep,
  type Assembly,
  type Message,
  type Owner,
  type Replica,
  type ReplicaEvent,
  type Request,
} from './lifecycle-model.js';

interface Sim {
  readonly owner: Owner;
  readonly replica: Replica;
  /**
   * Wire requests the replica has emitted and the owner has not yet
   * seen. Queued at the instant of emission: a later local step must
   * never be able to swallow a request that was already sent, which is
   * a bug this harness had until it bit a witness.
   */
  readonly pending: readonly Request[];
  /** Messages the harness chose to lose in flight. */
  readonly lost: readonly Message[];
}

function sim(chunks = 2, aud: readonly string[] = ['crew']): Sim {
  return { owner: newOwner(chunks), replica: newReplica(aud), pending: [], lost: [] };
}

/** Apply a local (caller-side) event, banking anything it put on the wire. */
function local(s: Sim, e: ReplicaEvent): Sim {
  const replica = replicaStep(s.replica, e);
  return { ...s, replica: { ...replica, out: [] }, pending: [...s.pending, ...replica.out] };
}

/** Deliver one owner message to the replica. */
function deliver(s: Sim, m: Message): Sim {
  const replica = replicaStep(s.replica, { t: 'recv', m });
  return { ...s, replica: { ...replica, out: [] }, pending: [...s.pending, ...replica.out] };
}

function lose(s: Sim, m: Message): Sim {
  return { ...s, lost: [...s.lost, m] };
}

/** Hand the owner one request and deliver whatever it emits. */
function serve(s: Sim, q: Request): Sim {
  const owner = ownerStep(s.owner, { t: 'recv', q });
  return drain({ ...s, owner });
}

/** Deliver the owner's queued output. */
function drain(s: Sim): Sim {
  const out = s.owner.out;
  s = { ...s, owner: { ...s.owner, out: [] } };
  for (const m of out) s = deliver(s, m);
  return s;
}

/** Emit the owner's next snapshot chunk; `keep: false` loses it. */
function chunk(s: Sim, keep = true): Sim {
  const { owner, message } = emitChunk(s.owner);
  s = { ...s, owner };
  if (message === null) return s;
  return keep ? deliver(s, message) : lose(s, message);
}

/** Take the oldest pending wire request. */
function take(s: Sim): { rest: Sim; req: Request } {
  const [req, ...pending] = s.pending;
  if (req === undefined) throw new Error('expected a pending request');
  return { rest: { ...s, pending }, req };
}

/** Run requests and chunks to quiescence. */
function settle(s: Sim): Sim {
  for (let guard = 0; guard < 200; guard += 1) {
    if (s.pending.length > 0) {
      const { rest, req } = take(s);
      s = serve(rest, req);
      continue;
    }
    if (s.owner.unsent > 0) {
      s = chunk(s);
      continue;
    }
    return s;
  }
  throw new Error('lifecycle did not settle');
}

/** Owner and replica agree on generation and revision, and it is live. */
function converged(s: Sim): boolean {
  return (
    s.replica.state === 'ready' &&
    s.replica.published &&
    !s.replica.stale &&
    s.replica.installed === s.owner.g &&
    s.replica.revision === s.owner.r
  );
}

/** A joined, installed, live replica. */
function joined(chunks = 2, aud: readonly string[] = ['crew']): Sim {
  const s = settle(local(sim(chunks, aud), { t: 'join', q: 'j1' }));
  expect(converged(s)).toBe(true);
  return s;
}

/** Live updates still reach the application. */
function stillLive(s: Sim): boolean {
  const advanced = drain({ ...s, owner: ownerStep(s.owner, { t: 'advance' }) });
  return converged(advanced) && advanced.replica.revision === advanced.owner.r;
}

describe('positive controls — a drop-everything model must fail these', () => {
  it('a happy join installs and deltas apply', () => {
    const s = joined(3);

    expect(s.replica.state).toBe('ready');
    expect(s.replica.published).toBe(true);
    expect(s.replica.installed).toBe(1);
    expect(stillLive(s)).toBe(true);
  });

  it('an owner refresh installs while ready', () => {
    let s = joined();
    const before = s.replica.installed ?? 0;

    s = settle(drain({ ...s, owner: ownerStep(s.owner, { t: 'refresh' }) }));

    expect(s.replica.installed).toBeGreaterThan(before);
    expect(converged(s)).toBe(true);
    expect(stillLive(s)).toBe(true);
  });

  it('a requested audience change installs the new audience', () => {
    let s = joined(3);

    s = settle(local(s, { t: 'setAudience', q: 'a-b', aud: ['deck'] }));

    expect(s.replica.desired).toEqual(['deck']);
    expect(s.owner.aud).toEqual(['deck']);
    expect(converged(s)).toBe(true);
    expect(stillLive(s)).toBe(true);
  });
});

describe('blocker 1 — the initial join has a legal manifest transition', () => {
  it('admits the first manifest against the pending join and publishes', () => {
    let s = local(sim(3), { t: 'join', q: 'j1' });

    expect(s.replica.state).toBe('joining');
    expect(s.replica.slot).toBe('j1');
    expect(s.pending).toEqual([{ k: 'join', q: 'j1', aud: ['crew'] }]);

    s = settle(s);

    // The literal reading of the old table dropped exactly this message.
    expect(s.replica.dropped['man-wrong-slot']).toBeUndefined();
    expect(s.replica.installed).toBe(1);
    expect(s.replica.retired).toBe(1);
    expect(converged(s)).toBe(true);
  });

  it('refuses a manifest correlated to a join it did not issue', () => {
    let s = local(sim(), { t: 'join', q: 'j1' });
    s = deliver(s, { k: 'man', h: 'h0', g: 1, r: 100, n: 2, q: 'someone-else' });

    expect(s.replica.assembling).toBeNull();
    expect(s.replica.published).toBe(false);
    expect(s.replica.dropped['man-wrong-slot']).toBe(1);
  });

  it('refuses an unsolicited manifest before any join is answered', () => {
    let s = local(sim(), { t: 'join', q: 'j1' });
    s = deliver(s, { k: 'man', h: 'h0', g: 1, r: 100, n: 2, q: null });

    expect(s.replica.assembling).toBeNull();
    expect(s.replica.dropped['man-unsolicited-in-joining']).toBe(1);
  });
});

describe('blocker 2 — supersession retires the assembly and fences publication', () => {
  it('a superseded transition publishes nothing and its successor converges', () => {
    let s = joined(3);

    // Open a transition to ['deck'] and let one chunk arrive.
    s = local(s, { t: 'setAudience', q: 'a-b', aud: ['deck'] });
    const taken = take(s);
    s = serve(taken.rest, taken.req);
    s = chunk(s);
    expect(s.replica.assembling?.have.size).toBe(1);

    // Supersede it before completion.
    s = local(s, { t: 'setAudience', q: 'a-c', aud: ['bridge'] });
    expect(s.replica.assembling).toBeNull();
    expect(s.replica.published).toBe(false);
    expect(s.replica.desired).toEqual(['bridge']);

    s = settle(s);

    expect(s.owner.aud).toEqual(['bridge']);
    expect(s.replica.desired).toEqual(['bridge']);
    expect(converged(s)).toBe(true);
    expect(stillLive(s)).toBe(true);
  });

  it('late chunks of a superseded assembly publish nothing', () => {
    let s = joined(2);
    const installed = s.replica.installed;
    const revision = s.replica.revision;

    // An owner refresh opens an assembly, one chunk arrives …
    s = drain({ ...s, owner: ownerStep(s.owner, { t: 'refresh' }) });
    const g = s.replica.assembling?.g;
    expect(g).toBe(2);
    s = chunk(s);

    // … then the caller supersedes it, and the rest arrive late.
    s = local(s, { t: 'setAudience', q: 'a-z', aud: ['deck'] });
    s = chunk(s);

    expect(s.replica.dropped['snap-no-assembly']).toBe(1);
    expect(s.replica.installed).toBe(installed);
    expect(s.replica.revision).toBe(revision);
    expect(s.replica.published).toBe(false);
  });

  it('an old-generation delta cannot repopulate a cleared view', () => {
    let s = joined();
    const g = s.replica.installed ?? 0;
    const base = s.replica.revision ?? 0;

    s = local(s, { t: 'setAudience', q: 'a-b', aud: ['deck'] });
    expect(s.replica.published).toBe(false);

    s = deliver(s, { k: 'delta', h: 'h0', g, base, r: base + 1 });

    // State-gated, not merely generation-gated: this delta names the
    // generation the replica last installed and is still inadmissible.
    expect(s.replica.dropped['delta-in-installing']).toBe(1);
    expect(s.replica.published).toBe(false);
    expect(s.replica.revision).toBe(base);
  });

  it('a delta cannot repopulate a fenced view', () => {
    let s = joined();
    const g = s.replica.installed ?? 0;
    const base = s.replica.revision ?? 0;

    s = local(s, { t: 'setAudience', q: 'a-b', aud: ['deck'] });
    s = local(s, { t: 'cancel' });
    expect(s.replica.state).toBe('fenced');

    s = deliver(s, { k: 'delta', h: 'h0', g, base, r: base + 1 });
    expect(s.replica.dropped['delta-in-fenced']).toBe(1);
    expect(s.replica.published).toBe(false);
  });
});

describe('blocker 3 — a skipped owner refresh still converges', () => {
  it('recovers to the owner generation it had to skip', () => {
    let s = joined(2);

    // The replica opens its own transition; the owner answers and emits
    // every chunk, but the last one is still in flight.
    s = local(s, { t: 'setAudience', q: 'a-b', aud: ['deck'] });
    const taken = take(s);
    s = serve(taken.rest, taken.req);
    const mine = s.replica.assembling?.g;
    expect(mine).toBe(2);
    s = chunk(s);
    const held = emitChunk(s.owner);
    s = { ...s, owner: held.owner };
    expect(s.owner.unsent).toBe(0);

    // Meanwhile the owner advances to a newer installation on its own.
    s = drain({ ...s, owner: ownerStep(s.owner, { t: 'refresh' }) });
    expect(s.owner.g).toBe(3);
    expect(s.replica.skipped).toBe(3);
    expect(s.replica.dropped['man-unsolicited-in-installing']).toBe(1);

    // The held chunk lands: the replica installs the OLDER generation.
    s = deliver(s, held.message as Message);
    expect(s.replica.installed).toBe(2);
    expect(s.pending.some((q) => q.k === 'resync')).toBe(true);

    // Recovery, not stranding: it converges on the owner's generation
    // and live updates resume.
    s = settle(s);
    expect(s.replica.skipped).toBeNull();
    expect(s.replica.installed).toBe(s.owner.g);
    expect(converged(s)).toBe(true);
    expect(stillLive(s)).toBe(true);
  });

  it('coalesces several skipped refreshes into one recovery', () => {
    let s = joined(2);

    s = local(s, { t: 'setAudience', q: 'a-b', aud: ['deck'] });
    const taken = take(s);
    s = serve(taken.rest, taken.req);
    s = chunk(s);
    const held = emitChunk(s.owner);
    s = { ...s, owner: held.owner };

    s = drain({ ...s, owner: ownerStep(s.owner, { t: 'refresh' }) });
    s = drain({ ...s, owner: ownerStep(s.owner, { t: 'refresh' }) });
    // Bounded knowledge: one number, the newest.
    expect(s.replica.skipped).toBe(s.owner.g);

    s = deliver(s, held.message as Message);
    const resyncs = s.pending.filter((q) => q.k === 'resync');
    expect(resyncs).toHaveLength(1);

    s = settle(s);
    expect(converged(s)).toBe(true);
  });

  it('does not reopen a fenced replica with an owner refresh', () => {
    let s = joined();

    s = local(s, { t: 'setAudience', q: 'a-b', aud: ['deck'] });
    s = local(s, { t: 'cancel' });
    expect(s.replica.state).toBe('fenced');

    // The fence is the caller's; an owner emission must not lift it,
    // and must not be banked as a skip either — a fenced replica is
    // waiting for its own caller, not for the owner.
    s = drain({ ...s, owner: ownerStep(s.owner, { t: 'refresh' }) });

    expect(s.replica.state).toBe('fenced');
    expect(s.replica.published).toBe(false);
    expect(s.replica.skipped).toBeNull();
    expect(s.replica.dropped['man-unsolicited-in-fenced']).toBe(1);
  });

  it('a fenced replica recovers on its caller’s own next request', () => {
    let s = joined();
    s = local(s, { t: 'setAudience', q: 'a-b', aud: ['deck'] });
    s = local(s, { t: 'cancel' });

    s = settle(local(s, { t: 'setAudience', q: 'a-c', aud: ['bridge'] }));

    expect(s.owner.aud).toEqual(['bridge']);
    expect(converged(s)).toBe(true);
  });
});

describe('blocker 4 — recovery from every live state', () => {
  it('reconnects during installation, keeping the latest desired audience', () => {
    let s = joined(3);

    // A transition to ['deck'] is in flight when the session drops.
    s = local(s, { t: 'setAudience', q: 'a-b', aud: ['deck'] });
    s = { ...s, pending: [] }; // the old session took it with it
    s = local(s, { t: 'reconnect', q: 'rc1' });

    const resume = s.pending[0];
    expect(resume?.k).toBe('resume');
    // Not the older owner-side audience.
    expect(resume?.aud).toEqual(['deck']);
    expect(s.replica.assembling).toBeNull();

    s = settle(s);
    expect(s.owner.aud).toEqual(['deck']);
    expect(converged(s)).toBe(true);
    expect(stillLive(s)).toBe(true);
  });

  it('reconnects during the initial join', () => {
    let s = local(sim(3), { t: 'join', q: 'j1' });
    s = { ...s, pending: [] };
    s = local(s, { t: 'reconnect', q: 'rc1' });

    expect(s.replica.state).toBe('installing');
    expect(s.pending[0]?.aud).toEqual(['crew']);

    s = settle(s);
    expect(converged(s)).toBe(true);
  });

  it('reconnect from ready retains the stale view while recovering', () => {
    let s = joined(3);

    s = local(s, { t: 'reconnect', q: 'rc1' });
    // Retained, not cleared: same audience, so the game keeps drawing.
    expect(s.replica.stale).toBe(true);
    expect(s.replica.published).toBe(true);

    s = settle(s);
    expect(s.replica.stale).toBe(false);
    expect(converged(s)).toBe(true);
  });

  it('an assembly deadline recovers by naming the position it actually has', () => {
    let s = joined(3);
    const installedAt = s.replica.installed;
    const revisionAt = s.replica.revision;

    // An owner refresh opens an assembly whose chunks are all lost.
    s = drain({ ...s, owner: ownerStep(s.owner, { t: 'refresh' }) });
    expect(s.replica.assembling?.g).toBe(2);
    while (s.owner.unsent > 0) s = chunk(s, false);

    s = local(s, { t: 'assemblyDeadline' });
    const resync = s.pending[0];
    expect(resync?.k).toBe('resync');
    // Advisory position: what it HAS, older than the owner's current
    // generation — and the owner must not refuse it for that.
    if (resync?.k === 'resync') {
      expect(resync.g).toBe(installedAt);
      expect(resync.have).toBe(revisionAt);
    }

    s = settle(s);
    expect(converged(s)).toBe(true);
  });

  it('a revision gap recovers to the owner’s current revision', () => {
    let s = joined();
    const g = s.replica.installed ?? 0;
    const base = s.replica.revision ?? 0;

    // A delta that skips one: the replica cannot apply it.
    s = deliver(s, { k: 'delta', h: 'h0', g, base: base + 1, r: base + 2 });
    expect(s.replica.revision).toBe(base);
    expect(s.pending[0]?.k).toBe('resync');

    s = settle(s);
    expect(converged(s)).toBe(true);
    expect(stillLive(s)).toBe(true);
  });

  it('an unavailable projection defers and still converges', () => {
    let s = joined();

    s = { ...s, owner: ownerStep(s.owner, { t: 'projectable', can: false }) };
    s = settle(local(s, { t: 'setAudience', q: 'a-b', aud: ['deck'] }));

    // Deferred, not refused: nothing told the replica `not-ready`, and
    // it is still installing rather than fenced.
    expect(s.replica.state).toBe('installing');
    expect(s.replica.dropped['no-wrong-slot']).toBeUndefined();
    expect(s.owner.deferred).not.toBeNull();

    s = settle(drain({ ...s, owner: ownerStep(s.owner, { t: 'projectable', can: true }) }));
    expect(s.owner.aud).toEqual(['deck']);
    expect(converged(s)).toBe(true);
  });
});

describe('the retirement watermark', () => {
  it('an abandoned generation cannot be reopened by a late manifest', () => {
    let s = joined(3);
    const installed = s.replica.installed;

    // The owner refreshes; the replica admits the manifest but the
    // assembly is abandoned, so this generation never installs.
    s = drain({ ...s, owner: ownerStep(s.owner, { t: 'refresh' }) });
    const abandoned = s.replica.assembling;
    expect(abandoned?.g).toBe(2);
    expect(s.replica.retired).toBe(2);
    while (s.owner.unsent > 0) s = chunk(s, false);
    s = { ...s, pending: [] };
    s = local(s, { t: 'assemblyDeadline' });
    s = { ...s, pending: [] };

    // A delayed duplicate of that manifest. Under `g > installed` this
    // is admissible, because generation 2 never installed.
    s = deliver(s, { k: 'man', h: 'h0', g: 2, r: 700, n: 3, q: null });

    expect(s.replica.dropped['man-retired']).toBe(1);
    expect(s.replica.assembling).toBeNull();
    expect(s.replica.installed).toBe(installed);
  });

  it('a duplicate manifest leaves an open assembly untouched', () => {
    let s = joined(3);

    s = drain({ ...s, owner: ownerStep(s.owner, { t: 'refresh' }) });
    s = chunk(s);
    const have = s.replica.assembling?.have.size;
    expect(have).toBe(1);

    s = deliver(s, { k: 'man', h: 'h0', g: 2, r: s.owner.r, n: 3, q: null });

    expect(s.replica.dropped['man-retired']).toBe(1);
    expect(s.replica.assembling?.have.size).toBe(have);

    // And the original assembly still completes and publishes.
    s = settle(s);
    expect(s.replica.installed).toBe(2);
    expect(converged(s)).toBe(true);
  });
});

describe('publication is fenced against the exact active installation', () => {
  // These two states are unreachable while every supersession path also
  // retires the assembly — which is exactly why they are asserted
  // directly. The fence is the invariant; clearing the assembly is only
  // the mechanism, and an invariant with no witness is an assumption.
  // Running the inverses is what exposed this branch as untested.
  function withAssembly(s: Sim, state: Replica['state'], slot: string | null, a: Assembly): Sim {
    return { ...s, replica: { ...s.replica, state, slot, assembling: a } };
  }

  it('refuses a completing assembly whose transition is no longer desired', () => {
    let s = joined(2);
    const installed = s.replica.installed;

    // What a supersession path that forgot to retire would leave behind.
    s = withAssembly(s, 'installing', 'a-new', {
      g: 9, r: 900, n: 1, q: 'a-old', have: new Set<number>(),
    });
    s = deliver(s, { k: 'snap', h: 'h0', g: 9, r: 900, n: 1, i: 0 });

    expect(s.replica.dropped['publish-superseded']).toBe(1);
    expect(s.replica.installed).toBe(installed);
    expect(s.replica.revision).toBe(s.owner.r);
  });

  it('refuses a completing owner refresh once the replica left installing', () => {
    let s = joined(2);
    const installed = s.replica.installed;

    s = withAssembly(s, 'ready', null, {
      g: 9, r: 900, n: 1, q: null, have: new Set<number>(),
    });
    s = deliver(s, { k: 'snap', h: 'h0', g: 9, r: 900, n: 1, i: 0 });

    expect(s.replica.dropped['publish-not-installing']).toBe(1);
    expect(s.replica.installed).toBe(installed);
  });
});

describe('waiters are independently cancellable on one wire request', () => {
  it('cancelling one of two waiters keeps the transition alive', () => {
    let s = joined(3);

    s = local(s, { t: 'setAudience', q: 'a-b', aud: ['deck'] });
    // One wire request for the shared transition …
    expect(s.pending).toHaveLength(1);
    // … and a second local waiter joins it.
    s = { ...s, replica: { ...s.replica, waiters: s.replica.waiters + 1 } };

    s = local(s, { t: 'cancel' });
    expect(s.replica.state).toBe('installing');
    expect(s.replica.slot).toBe('a-b');
    expect(s.pending).toHaveLength(1);

    s = settle(s);
    expect(s.owner.aud).toEqual(['deck']);
    expect(converged(s)).toBe(true);
  });

  it('cancelling the last waiter fences and does not restore the old view', () => {
    let s = joined(3);

    s = local(s, { t: 'setAudience', q: 'a-b', aud: ['deck'] });
    s = local(s, { t: 'cancel' });

    expect(s.replica.state).toBe('fenced');
    expect(s.replica.published).toBe(false);
    expect(s.replica.assembling).toBeNull();
    expect(s.replica.slot).toBeNull();
  });

  it('a cancelled transition’s manifest publishes nothing', () => {
    let s = joined(3);

    s = local(s, { t: 'setAudience', q: 'a-b', aud: ['deck'] });
    const { rest, req } = take(s);
    s = local(rest, { t: 'cancel' });

    // The owner's answer arrives after the cancellation.
    s = settle(serve(s, req));

    expect(s.replica.state).toBe('fenced');
    expect(s.replica.published).toBe(false);
    expect(s.replica.dropped['man-wrong-slot']).toBe(1);
  });
});

describe('the owner retires unsent work it has superseded', () => {
  it('does not emit chunks of a superseded installation', () => {
    let s = joined(4);

    s = local(s, { t: 'setAudience', q: 'a-b', aud: ['deck'] });
    const taken = take(s);
    s = serve(taken.rest, taken.req);
    s = chunk(s);
    expect(s.owner.unsent).toBe(3);

    // A newer control is admissible mid-emission and retires the rest.
    s = settle(local(s, { t: 'setAudience', q: 'a-c', aud: ['bridge'] }));

    expect(s.owner.retiredUnsent).toBe(3);
    expect(s.owner.aud).toEqual(['bridge']);
    expect(converged(s)).toBe(true);
  });
});
