/**
 * The installation lifecycle, executed.
 *
 * Every assertion is about **publication, the active assembly, the
 * desired audience and convergence** — never about a refusal code
 * alone, because a model that drops everything refuses correctly and
 * converges never. The positive controls are therefore load-bearing: a
 * happy join that installs and applies deltas, an owner refresh that
 * installs while `ready`, a superseded assembly whose successor still
 * publishes, and every `converged()` assertion fail under a
 * drop-everything implementation.
 *
 * **Message delivery is per-direction FIFO.** One reliable ordered
 * stream per handle (§1.9a) is a stated transport assumption, so the
 * harness cannot reorder owner→replica traffic; "loss" is an owner
 * stall or a broken session, never selective per-message loss. The
 * violated-assumption case is covered separately and explicitly.
 *
 * Generations come from the owner's own allocator in every transition
 * trace. The two whitebox fence witnesses construct a generation
 * directly — they assert an invariant against a state no owner-issued
 * trace can reach, which is the point of them, and they are not
 * transition traces.
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
   * never be able to swallow a request that was already sent.
   */
  readonly pending: readonly Request[];
  /** Owner→replica messages in flight, in emission order. */
  readonly wire: readonly Message[];
  /** Messages a broken session took with it. */
  readonly lost: readonly Message[];
}

function sim(chunks = 2, aud: readonly string[] = ['crew']): Sim {
  return { owner: newOwner(chunks), replica: newReplica(aud), pending: [], wire: [], lost: [] };
}

/** Apply a local (caller-side) event, banking anything it put on the wire. */
function local(s: Sim, e: ReplicaEvent): Sim {
  const replica = replicaStep(s.replica, e);
  return { ...s, replica: { ...replica, out: [] }, pending: [...s.pending, ...replica.out] };
}

/** Hand the owner one request; its output joins the ordered wire. */
function serve(s: Sim, q: Request): Sim {
  const owner = ownerStep(s.owner, { t: 'recv', q });
  return { ...s, owner: { ...owner, out: [] }, wire: [...s.wire, ...owner.out] };
}

/** Apply an owner-side event; its output joins the ordered wire. */
function owner(s: Sim, e: Parameters<typeof ownerStep>[1]): Sim {
  const next = ownerStep(s.owner, e);
  return { ...s, owner: { ...next, out: [] }, wire: [...s.wire, ...next.out] };
}

/** Hand the next unsent chunk to the transport, in order. */
function emit(s: Sim): Sim {
  const { owner: next, message } = emitChunk(s.owner);
  if (message === null) return { ...s, owner: next };
  return { ...s, owner: next, wire: [...s.wire, message] };
}

/** Deliver the oldest in-flight message. */
function pump(s: Sim): Sim {
  const [head, ...rest] = s.wire;
  if (head === undefined) return s;
  const replica = replicaStep(s.replica, { t: 'recv', m: head });
  return {
    ...s,
    wire: rest,
    replica: { ...replica, out: [] },
    pending: [...s.pending, ...replica.out],
  };
}

/**
 * Deliver an in-flight message out of order.
 *
 * Reserved for the violated-assumption witnesses: §1.9a's single
 * ordered stream makes this unreachable in production, and the rules it
 * exercises exist so that a violated assumption cannot strand the
 * replica silently.
 */
function pumpOutOfOrder(s: Sim, index: number): Sim {
  const m = s.wire[index];
  if (m === undefined) throw new Error('no message at that wire index');
  const replica = replicaStep(s.replica, { t: 'recv', m });
  return {
    ...s,
    wire: s.wire.filter((_, i) => i !== index),
    replica: { ...replica, out: [] },
    pending: [...s.pending, ...replica.out],
  };
}

/** The session dies: everything in flight goes with it. */
function breakSession(s: Sim): Sim {
  return { ...s, wire: [], lost: [...s.lost, ...s.wire] };
}

/** Take the oldest pending wire request. */
function take(s: Sim): { rest: Sim; req: Request } {
  const [req, ...pending] = s.pending;
  if (req === undefined) throw new Error('expected a pending request');
  return { rest: { ...s, pending }, req };
}

/** Run requests, emissions and deliveries to quiescence. */
function settle(s: Sim): Sim {
  for (let guard = 0; guard < 300; guard += 1) {
    if (s.wire.length > 0) {
      s = pump(s);
      continue;
    }
    if (s.pending.length > 0) {
      const { rest, req } = take(s);
      s = serve(rest, req);
      continue;
    }
    if (s.owner.unsent > 0) {
      s = emit(s);
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
    s.replica.handle === s.owner.h &&
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
  const advanced = settle(owner(s, { t: 'advance' }));
  return converged(advanced) && advanced.replica.revision === advanced.owner.r;
}

describe('positive controls — a drop-everything model must fail these', () => {
  it('a happy join installs, learns its handle, and applies deltas', () => {
    const s = joined(3);

    expect(s.replica.state).toBe('ready');
    expect(s.replica.published).toBe(true);
    expect(s.replica.installed).toBe(1);
    // The handle was issued by the owner and learned from the manifest.
    expect(s.replica.handle).toBe('h1');
    expect(stillLive(s)).toBe(true);
  });

  it('an owner refresh installs while ready', () => {
    let s = joined();
    const before = s.replica.installed ?? 0;

    s = settle(owner(s, { t: 'refresh' }));

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

describe('handle admission — only join creates a handle', () => {
  it('reconnecting before a handle is learned rejoins instead of resuming', () => {
    // The initial join never reaches the owner.
    let s = local(sim(2), { t: 'join', q: 'j1' });
    s = { ...s, pending: [] };
    expect(s.replica.handle).toBeNull();

    s = local(s, { t: 'reconnect', q: 'rc1' });

    // `resume` would name a handle the replica has not been issued, and
    // the owner would have to invent one to answer it.
    const req = s.pending[0];
    expect(req?.k).toBe('join');
    if (req?.k === 'join') expect(req.aud).toEqual(['crew']);

    s = settle(s);
    expect(s.replica.handle).toBe('h1');
    expect(converged(s)).toBe(true);
  });

  it('an expired handle is refused, and recovery rejoins', () => {
    let s = joined(2);
    expect(s.replica.handle).toBe('h1');

    // The owner forgets the handle; the replica does not know yet.
    s = owner(s, { t: 'expire', h: 'h1' });
    s = local(s, { t: 'reconnect', q: 'rc1' });
    const { rest, req } = take(s);
    expect(req.k).toBe('resume');

    s = serve(rest, req);
    expect(s.wire[0]).toEqual({ k: 'no', h: 'h1', q: 'rc1', code: 'unknown-handle' });

    s = pump(s);
    // Refused, not silently honoured — and recovery is a fresh join.
    expect(s.replica.handle).toBeNull();
    expect(s.replica.published).toBe(false);
    expect(s.pending[0]?.k).toBe('join');

    s = settle(s);
    expect(s.replica.handle).toBe('h2');
    expect(converged(s)).toBe(true);
    expect(stillLive(s)).toBe(true);
  });

  it('an unsolicited manifest for another handle installs nothing', () => {
    let s = joined(2);
    const installed = s.replica.installed;

    s = pump({ ...s, wire: [{ k: 'man', h: 'h9', g: 99, r: 999, n: 2, q: null }] });

    expect(s.replica.dropped['foreign-handle']).toBe(1);
    expect(s.replica.assembling).toBeNull();
    expect(s.replica.installed).toBe(installed);
    expect(s.replica.retired).toBe(1);
  });

  it('a resync recovers within the audience bound to the handle', () => {
    let s = joined(2, ['crew']);
    s = settle(local(s, { t: 'setAudience', q: 'a-b', aud: ['deck'] }));
    expect(s.owner.aud).toEqual(['deck']);

    // A gap forces a resync, which carries no audience of its own.
    const g = s.replica.installed ?? 0;
    const base = s.replica.revision ?? 0;
    s = pump({ ...s, wire: [{ k: 'delta', h: 'h1', g, base: base + 1, r: base + 2 }] });
    const req = s.pending[0];
    expect(req?.k).toBe('resync');
    expect(req).not.toHaveProperty('aud');

    s = settle(s);
    // The owner recovers the handle's audience, not the join's.
    expect(s.owner.aud).toEqual(['deck']);
    expect(converged(s)).toBe(true);
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
    s = pump({ ...s, wire: [{ k: 'man', h: 'h1', g: 1, r: 100, n: 2, q: 'someone-else' }] });

    expect(s.replica.assembling).toBeNull();
    expect(s.replica.handle).toBeNull();
    expect(s.replica.published).toBe(false);
    expect(s.replica.dropped['man-wrong-slot']).toBe(1);
  });

  it('refuses an unsolicited manifest before any join is answered', () => {
    let s = local(sim(), { t: 'join', q: 'j1' });
    s = pump({ ...s, wire: [{ k: 'man', h: 'h1', g: 1, r: 100, n: 2, q: null }] });

    expect(s.replica.assembling).toBeNull();
    expect(s.replica.handle).toBeNull();
    expect(s.replica.dropped['man-unsolicited-in-joining']).toBe(1);
  });
});

describe('blocker 2 — supersession retires the assembly and fences publication', () => {
  it('a superseded transition publishes nothing and its successor converges', () => {
    let s = joined(3);

    // Open a transition to ['deck'] and let one chunk arrive.
    s = local(s, { t: 'setAudience', q: 'a-b', aud: ['deck'] });
    const taken = take(s);
    s = pump(serve(taken.rest, taken.req));
    s = pump(emit(s));
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
    s = pump(owner(s, { t: 'refresh' }));
    expect(s.replica.assembling?.g).toBe(2);
    s = pump(emit(s));

    // … then the caller supersedes it, and the rest arrive late.
    s = local(s, { t: 'setAudience', q: 'a-z', aud: ['deck'] });
    s = pump(emit(s));

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

    s = pump({ ...s, wire: [{ k: 'delta', h: 'h1', g, base, r: base + 1 }] });

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

    s = pump({ ...s, wire: [{ k: 'delta', h: 'h1', g, base, r: base + 1 }] });
    expect(s.replica.dropped['delta-in-fenced']).toBe(1);
    expect(s.replica.published).toBe(false);
  });
});

describe('blocker 3 — a skipped owner refresh still converges', () => {
  it('does not reopen a fenced replica with an owner refresh', () => {
    let s = joined();

    s = local(s, { t: 'setAudience', q: 'a-b', aud: ['deck'] });
    s = local(s, { t: 'cancel' });
    expect(s.replica.state).toBe('fenced');

    // The fence is the caller's; an owner emission must not lift it,
    // and must not be banked as a skip either.
    s = pump(owner(s, { t: 'refresh' }));

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
    s = breakSession({ ...s, pending: [] }); // the old session took it
    s = local(s, { t: 'reconnect', q: 'rc1' });

    const resume = s.pending[0];
    expect(resume?.k).toBe('resume');
    // Not the older owner-side audience.
    if (resume?.k === 'resume') expect(resume.aud).toEqual(['deck']);
    expect(s.replica.assembling).toBeNull();

    s = settle(s);
    expect(s.owner.aud).toEqual(['deck']);
    expect(converged(s)).toBe(true);
    expect(stillLive(s)).toBe(true);
  });

  it('reconnects mid-assembly, fencing the lost session’s work', () => {
    let s = joined(3);

    s = pump(owner(s, { t: 'refresh' }));
    s = pump(emit(s));
    expect(s.replica.assembling?.have.size).toBe(1);

    s = breakSession(s);
    s = local(s, { t: 'reconnect', q: 'rc1' });
    // The lost session's assembly cannot be completed by the new one.
    expect(s.replica.assembling).toBeNull();

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

    // The owner opens a replacement and then stalls: the remaining
    // chunks are never handed over.
    s = pump(owner(s, { t: 'refresh' }));
    expect(s.replica.assembling?.g).toBe(2);
    s = owner(s, { t: 'stall' });

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
    s = pump({ ...s, wire: [{ k: 'delta', h: 'h1', g, base: base + 1, r: base + 2 }] });
    expect(s.replica.revision).toBe(base);
    expect(s.pending[0]?.k).toBe('resync');

    s = settle(s);
    expect(converged(s)).toBe(true);
    expect(stillLive(s)).toBe(true);
  });

  it('an unavailable projection defers and still converges', () => {
    let s = joined();

    s = owner(s, { t: 'projectable', can: false });
    s = settle(local(s, { t: 'setAudience', q: 'a-b', aud: ['deck'] }));

    // Deferred, not refused: nothing told the replica `not-ready`, and
    // it is still installing rather than fenced.
    expect(s.replica.state).toBe('installing');
    expect(s.replica.published).toBe(false);
    expect(s.owner.deferred).not.toBeNull();

    s = settle(owner(s, { t: 'projectable', can: true }));
    expect(s.owner.aud).toEqual(['deck']);
    expect(converged(s)).toBe(true);
  });

  it('a deferred initial join still converges', () => {
    let s = sim(2);
    s = owner(s, { t: 'projectable', can: false });
    s = settle(local(s, { t: 'join', q: 'j1' }));

    expect(s.replica.state).toBe('joining');
    expect(s.replica.handle).toBeNull();

    s = settle(owner(s, { t: 'projectable', can: true }));
    expect(s.replica.handle).toBe('h1');
    expect(converged(s)).toBe(true);
  });
});

describe('the owner never supersedes its own in-flight emission', () => {
  it('refuses a refresh while emitting, and the installation completes', () => {
    let s = joined(3);

    // A caller transition is being emitted.
    s = local(s, { t: 'setAudience', q: 'a-b', aud: ['deck'] });
    const taken = take(s);
    s = pump(serve(taken.rest, taken.req));
    s = pump(emit(s));
    expect(s.owner.unsent).toBe(2);

    // The owner wants to replace it. Doing so would retire the chunks
    // the replica is still waiting for while the replica — correctly —
    // refuses the unsolicited replacement: both generations stall.
    const before = s.owner.allocated;
    s = owner(s, { t: 'refresh' });
    expect(s.owner.allocated).toBe(before);
    expect(s.wire).toHaveLength(0);

    s = settle(s);
    expect(converged(s)).toBe(true);

    // Once idle, the same refresh installs.
    s = settle(owner(s, { t: 'refresh' }));
    expect(s.owner.allocated).toBe(before + 1);
    expect(converged(s)).toBe(true);
  });
});

describe('a violated ordering assumption cannot strand the replica', () => {
  // §1.9a's single ordered stream makes every case here unreachable:
  // each needs a message to overtake the chunks of the installation it
  // belongs to. They are asserted because a transport assumption that
  // is load-bearing for *liveness* must not be load-bearing for
  // *silence*: if it breaks, the replica must recover, not sit behind
  // the owner for ever.
  it('a refresh that overtakes an in-flight assembly is recovered', () => {
    let s = joined(2);

    // All of generation 2's chunks are handed over but still in flight,
    // and only then does the owner refresh to generation 3.
    s = pump(owner(s, { t: 'refresh' }));
    expect(s.replica.assembling?.g).toBe(2);
    while (s.owner.unsent > 0) s = emit(s);
    s = owner(s, { t: 'refresh' });
    expect(s.owner.g).toBe(3);

    // The refresh overtakes them.
    const manIndex = s.wire.findIndex((m) => m.k === 'man');
    s = pumpOutOfOrder(s, manIndex);
    expect(s.replica.dropped['man-unsolicited-in-installing']).toBe(1);
    expect(s.replica.skipped).toBe(3);

    s = settle(s);
    // Installed the older generation, then recovered to the owner's.
    expect(s.replica.skipped).toBeNull();
    expect(s.replica.installed).toBe(s.owner.g);
    expect(converged(s)).toBe(true);
    expect(stillLive(s)).toBe(true);
  });

  it('a delta that overtakes its own assembly is recovered', () => {
    let s = joined(2);

    s = pump(owner(s, { t: 'refresh' }));
    s = pump(emit(s));
    s = emit(s); // the last chunk is in flight
    s = owner(s, { t: 'advance' });

    // The delta overtakes the chunk it depends on.
    const deltaIndex = s.wire.findIndex((m) => m.k === 'delta');
    s = pumpOutOfOrder(s, deltaIndex);
    expect(s.replica.dropped['delta-ahead-of-assembly']).toBe(1);
    expect(s.replica.behind).toBe(true);

    // Installing must not look "current" afterwards.
    s = pump(s);
    expect(s.replica.revision).toBeLessThan(s.owner.r);
    expect(s.pending.some((q) => q.k === 'resync')).toBe(true);

    s = settle(s);
    expect(s.replica.revision).toBe(s.owner.r);
    expect(converged(s)).toBe(true);
  });

  it('a skipped refresh and an overtaking delta coalesce into one resync', () => {
    let s = joined(2);

    s = pump(owner(s, { t: 'refresh' }));
    while (s.owner.unsent > 0) s = emit(s);
    s = owner(s, { t: 'advance' });
    s = owner(s, { t: 'refresh' });

    // Both the delta and the newer manifest overtake the chunks.
    s = pumpOutOfOrder(s, s.wire.findIndex((m) => m.k === 'delta'));
    s = pumpOutOfOrder(s, s.wire.findIndex((m) => m.k === 'man'));
    expect(s.replica.behind).toBe(true);
    expect(s.replica.skipped).not.toBeNull();

    // Completing the assembly issues ONE recovery request, not two.
    while (s.wire.some((m) => m.k === 'snap')) s = pump(s);
    expect(s.pending.filter((q) => q.k === 'resync')).toHaveLength(1);

    s = settle(s);
    expect(converged(s)).toBe(true);
  });
});

describe('the snapshot/live boundary', () => {
  it('a delta cannot overtake the chunks of its own installation', () => {
    let s = joined(3);

    // The owner opens a replacement, hands over every chunk, and only
    // then advances the revision.
    s = owner(s, { t: 'refresh' });
    while (s.owner.unsent > 0) s = emit(s);
    s = owner(s, { t: 'advance' });

    // Ordered stream: manifest, chunks, then the delta.
    expect(s.wire.map((m) => m.k)).toEqual(['man', 'snap', 'snap', 'snap', 'delta']);

    s = settle(s);
    // The delta applies to the snapshot it follows.
    expect(s.replica.revision).toBe(s.owner.r);
    expect(converged(s)).toBe(true);
  });

});

describe('the retirement watermark', () => {
  it('an abandoned generation cannot be reopened by a late manifest', () => {
    let s = joined(3);
    const installed = s.replica.installed;

    // The owner refreshes, the replica admits the manifest, and the
    // owner then stalls, so this generation never installs.
    s = pump(owner(s, { t: 'refresh' }));
    expect(s.replica.assembling?.g).toBe(2);
    expect(s.replica.retired).toBe(2);
    s = owner(s, { t: 'stall' });
    s = local(s, { t: 'assemblyDeadline' });
    s = { ...s, pending: [] };

    // A delayed duplicate of that manifest. Under `g > installed` this
    // is admissible, because generation 2 never installed.
    s = pump({ ...s, wire: [{ k: 'man', h: 'h1', g: 2, r: 700, n: 3, q: null }] });

    expect(s.replica.dropped['man-retired']).toBe(1);
    expect(s.replica.assembling).toBeNull();
    expect(s.replica.installed).toBe(installed);
  });

  it('a duplicate manifest leaves an open assembly untouched', () => {
    let s = joined(3);

    s = pump(owner(s, { t: 'refresh' }));
    s = pump(emit(s));
    const have = s.replica.assembling?.have.size;
    expect(have).toBe(1);

    s = pump({ ...s, wire: [{ k: 'man', h: 'h1', g: 2, r: s.owner.r, n: 3, q: null }] });

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
  // directly, with a constructed generation rather than an owner-issued
  // trace. The fence is the invariant; clearing the assembly is only
  // the mechanism, and an invariant with no witness is an assumption.
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
    s = pump({ ...s, wire: [{ k: 'snap', h: 'h1', g: 9, r: 900, n: 1, i: 0 }] });

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
    s = pump({ ...s, wire: [{ k: 'snap', h: 'h1', g: 9, r: 900, n: 1, i: 0 }] });

    expect(s.replica.dropped['publish-not-installing']).toBe(1);
    expect(s.replica.installed).toBe(installed);
  });
});

describe('one wire request, many independently cancellable waiters', () => {
  it('two equal audience requests share one wire request', () => {
    let s = joined(3);

    // Both requests go through the reducer; nothing is supplied by hand.
    s = local(s, { t: 'setAudience', q: 'a-b', aud: ['deck'] });
    s = local(s, { t: 'setAudience', q: 'a-c', aud: ['deck'] });

    expect(s.pending).toHaveLength(1);
    expect(s.replica.waiters).toBe(2);
    // The slot is the first request's `q`: the second joined it.
    expect(s.replica.slot).toBe('a-b');

    // Cancelling one waiter does not supersede the other.
    s = local(s, { t: 'cancel' });
    expect(s.replica.state).toBe('installing');
    expect(s.replica.slot).toBe('a-b');
    expect(s.replica.waiters).toBe(1);
    expect(s.pending).toHaveLength(1);

    s = settle(s);
    expect(s.owner.aud).toEqual(['deck']);
    expect(converged(s)).toBe(true);
  });

  it('a different audience supersedes instead of joining', () => {
    let s = joined(3);

    s = local(s, { t: 'setAudience', q: 'a-b', aud: ['deck'] });
    s = local(s, { t: 'setAudience', q: 'a-c', aud: ['bridge'] });

    // The control for the case above: two wire requests, new slot, one
    // waiter — the first transition was replaced, not joined.
    expect(s.pending).toHaveLength(2);
    expect(s.replica.slot).toBe('a-c');
    expect(s.replica.waiters).toBe(1);
    expect(s.replica.desired).toEqual(['bridge']);

    s = settle(s);
    expect(s.owner.aud).toEqual(['bridge']);
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
    s = pump(serve(taken.rest, taken.req));
    s = pump(emit(s));
    expect(s.owner.unsent).toBe(3);

    // A newer control is admissible mid-emission and retires the rest.
    s = settle(local(s, { t: 'setAudience', q: 'a-c', aud: ['bridge'] }));

    expect(s.owner.retiredUnsent).toBe(3);
    expect(s.owner.aud).toEqual(['bridge']);
    expect(converged(s)).toBe(true);
  });
});
