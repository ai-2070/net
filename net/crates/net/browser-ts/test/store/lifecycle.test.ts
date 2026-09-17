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
  emitNext,
  QUEUE_MAX,
  emitting,
  unsent,
  newOwner,
  newReplica,
  ownerStep,
  replicaStep,
  type Assembly,
  type Message,
  type Owner,
  type OwnerHandle,
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
  /** A second replica on its own handle, for the isolation witnesses. */
  readonly peer: Replica | null;
  readonly peerPending: readonly Request[];
  /** Owner→replica messages in flight, in emission order. */
  readonly wire: readonly Message[];
  /** Messages a broken session took with it. */
  readonly lost: readonly Message[];
}

function sim(chunks = 2, aud: readonly string[] = ['crew']): Sim {
  return {
    owner: newOwner(chunks),
    replica: newReplica(aud),
    peer: null,
    peerPending: [],
    pending: [],
    wire: [],
    lost: [],
  };
}

/** The handle the primary replica is using. */
function handleOf(s: Sim): string {
  return s.replica.handle ?? `h${s.owner.issued}`;
}

/** This handle's owner-side installation record. */
function rec(s: Sim, h: string = handleOf(s)): OwnerHandle {
  const found = s.owner.handles[h];
  if (found === undefined) throw new Error(`owner has no handle ${h}`);
  return found;
}

const audOf = (s: Sim, h?: string): readonly string[] => rec(s, h).aud;
const genOf = (s: Sim, h?: string): number => rec(s, h).g;
const unsentOf = (s: Sim, h: string = handleOf(s)): number => unsent(s.owner, h);
const allocOf = (s: Sim, h?: string): number => rec(s, h).allocated;
const deferredOf = (s: Sim, h?: string): OwnerHandle['deferred'] => rec(s, h).deferred;

/** Attach a second replica, which will join on its own handle. */
function withPeer(s: Sim, aud: readonly string[]): Sim {
  return { ...s, peer: newReplica(aud) };
}

function peer(s: Sim): Replica {
  if (s.peer === null) throw new Error('no peer attached');
  return s.peer;
}

/** Apply a local event on the second replica. */
function localPeer(s: Sim, e: ReplicaEvent): Sim {
  const next = replicaStep(peer(s), e);
  return { ...s, peer: { ...next, out: [] }, peerPending: [...s.peerPending, ...next.out] };
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
function emit(s: Sim, h?: string): Sim {
  const { owner: next, message } = emitNext(s.owner, h);
  if (message === null) return { ...s, owner: next, wire: [...s.wire, ...next.out] };
  return { ...s, owner: next, wire: [...s.wire, message, ...next.out] };
}

/** Does this message belong to the second replica? */
function belongsToPeer(s: Sim, m: Message): boolean {
  const p = s.peer;
  if (p === null) return false;
  if (p.handle !== null) return m.h === p.handle;
  // Still joining: correlate by the request it answers.
  return (m.k === 'man' || m.k === 'no') && m.q !== null && m.q === p.slot;
}

/** Deliver the oldest in-flight message to whichever replica owns it. */
function pump(s: Sim): Sim {
  const [head, ...rest] = s.wire;
  if (head === undefined) return s;
  if (belongsToPeer(s, head)) {
    const next = replicaStep(peer(s), { t: 'recv', m: head });
    return {
      ...s,
      wire: rest,
      peer: { ...next, out: [] },
      peerPending: [...s.peerPending, ...next.out],
    };
  }
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
    if (s.peerPending.length > 0) {
      const [req, ...peerPending] = s.peerPending;
      if (req === undefined) throw new Error('unreachable');
      s = serve({ ...s, peerPending }, req);
      continue;
    }
    if (emitting(s.owner).length > 0) {
      s = emit(s);
      continue;
    }
    return s;
  }
  throw new Error('lifecycle did not settle');
}

/** Owner and replica agree on generation and revision, and it is live. */
function agrees(r: Replica, s: Sim): boolean {
  const h = r.handle;
  if (h === null) return false;
  const record = s.owner.handles[h];
  return (
    record !== undefined &&
    record.live &&
    r.state === 'ready' &&
    r.published &&
    !r.stale &&
    r.installed === record.g &&
    r.revision === s.owner.r
  );
}

function converged(s: Sim): boolean {
  return agrees(s.replica, s);
}

function peerConverged(s: Sim): boolean {
  return agrees(peer(s), s);
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

    s = settle(owner(s, { t: 'refresh', h: handleOf(s) }));

    expect(s.replica.installed).toBeGreaterThan(before);
    expect(converged(s)).toBe(true);
    expect(stillLive(s)).toBe(true);
  });

  it('a requested audience change installs the new audience', () => {
    let s = joined(3);

    s = settle(local(s, { t: 'setAudience', q: 'a-b', aud: ['deck'] }));

    expect(s.replica.desired).toEqual(['deck']);
    expect(audOf(s)).toEqual(['deck']);
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
    expect(s.wire[0]).toEqual({ k: 'no', h: 'h1', q: 'rc1', code: 'closed' });

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
    expect(audOf(s)).toEqual(['deck']);

    // A gap forces a resync, which carries no audience of its own.
    const g = s.replica.installed ?? 0;
    const base = s.replica.revision ?? 0;
    s = pump({ ...s, wire: [{ k: 'delta', h: 'h1', g, base: base + 1, r: base + 2 }] });
    const req = s.pending[0];
    expect(req?.k).toBe('resync');
    expect(req).not.toHaveProperty('aud');

    s = settle(s);
    // The owner recovers the handle's audience, not the join's.
    expect(audOf(s)).toEqual(['deck']);
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

    expect(audOf(s)).toEqual(['bridge']);
    expect(s.replica.desired).toEqual(['bridge']);
    expect(converged(s)).toBe(true);
    expect(stillLive(s)).toBe(true);
  });

  it('late chunks of a superseded assembly publish nothing', () => {
    let s = joined(2);
    const installed = s.replica.installed;
    const revision = s.replica.revision;

    // An owner refresh opens an assembly, one chunk arrives …
    s = pump(owner(s, { t: 'refresh', h: handleOf(s) }));
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
    s = pump(owner(s, { t: 'refresh', h: handleOf(s) }));

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

    expect(audOf(s)).toEqual(['bridge']);
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
    expect(audOf(s)).toEqual(['deck']);
    expect(converged(s)).toBe(true);
    expect(stillLive(s)).toBe(true);
  });

  it('reconnects mid-assembly, fencing the lost session’s work', () => {
    let s = joined(3);

    s = pump(owner(s, { t: 'refresh', h: handleOf(s) }));
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
    s = pump(owner(s, { t: 'refresh', h: handleOf(s) }));
    expect(s.replica.assembling?.g).toBe(2);
    s = owner(s, { t: 'stall', h: handleOf(s) });

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
    expect(deferredOf(s)).not.toBeNull();

    s = settle(owner(s, { t: 'projectable', can: true }));
    expect(audOf(s)).toEqual(['deck']);
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
    expect(unsentOf(s)).toBe(2);

    // The owner wants to replace it. Doing so would retire the chunks
    // the replica is still waiting for while the replica — correctly —
    // refuses the unsolicited replacement: both generations stall.
    const before = allocOf(s);
    s = owner(s, { t: 'refresh', h: handleOf(s) });
    expect(allocOf(s)).toBe(before);
    expect(s.wire).toHaveLength(0);

    s = settle(s);
    expect(converged(s)).toBe(true);

    // Once idle, the same refresh installs.
    s = settle(owner(s, { t: 'refresh', h: handleOf(s) }));
    expect(allocOf(s)).toBe(before + 1);
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
    s = pump(owner(s, { t: 'refresh', h: handleOf(s) }));
    expect(s.replica.assembling?.g).toBe(2);
    while (unsentOf(s) > 0) s = emit(s);
    s = owner(s, { t: 'refresh', h: handleOf(s) });
    expect(genOf(s)).toBe(3);

    // The refresh overtakes them.
    const manIndex = s.wire.findIndex((m) => m.k === 'man');
    s = pumpOutOfOrder(s, manIndex);
    expect(s.replica.dropped['man-unsolicited-in-installing']).toBe(1);
    expect(s.replica.skipped).toBe(3);

    s = settle(s);
    // Installed the older generation, then recovered to the owner's.
    expect(s.replica.skipped).toBeNull();
    expect(s.replica.installed).toBe(genOf(s));
    expect(converged(s)).toBe(true);
    expect(stillLive(s)).toBe(true);
  });

  it('a delta that overtakes its own assembly is recovered', () => {
    let s = joined(2);

    s = pump(owner(s, { t: 'refresh', h: handleOf(s) }));
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

    s = pump(owner(s, { t: 'refresh', h: handleOf(s) }));
    while (unsentOf(s) > 0) s = emit(s);
    s = owner(s, { t: 'advance' });
    s = owner(s, { t: 'refresh', h: handleOf(s) });

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
    s = owner(s, { t: 'refresh', h: handleOf(s) });
    while (unsentOf(s) > 0) s = emit(s);
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
    s = pump(owner(s, { t: 'refresh', h: handleOf(s) }));
    expect(s.replica.assembling?.g).toBe(2);
    expect(s.replica.retired).toBe(2);
    s = owner(s, { t: 'stall', h: handleOf(s) });
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

    s = pump(owner(s, { t: 'refresh', h: handleOf(s) }));
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
    expect(audOf(s)).toEqual(['deck']);
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
    expect(audOf(s)).toEqual(['bridge']);
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
    expect(unsentOf(s)).toBe(3);

    // A newer control is admissible mid-emission and retires the rest.
    s = settle(local(s, { t: 'setAudience', q: 'a-c', aud: ['bridge'] }));

    expect(s.owner.retiredUnsent).toBe(3);
    expect(audOf(s)).toEqual(['bridge']);
    expect(converged(s)).toBe(true);
  });
});

describe('handles have independent lifecycles', () => {
  it('a second join does not disturb the first handle’s installation', () => {
    // First replica joins and its manifest is delivered; its chunks are
    // still pending.
    let s = local(sim(2), { t: 'join', q: 'j1' });
    const first = take(s);
    s = pump(serve(first.rest, first.req));
    expect(s.replica.assembling?.g).toBe(1);
    expect(unsentOf(s, 'h1')).toBe(2);

    // A second replica joins before those chunks are handed over.
    s = withPeer(s, ['deck']);
    s = localPeer(s, { t: 'join', q: 'j2' });
    const second = s.peerPending[0];
    if (second === undefined) throw new Error('peer sent nothing');
    s = serve({ ...s, peerPending: [] }, second);

    // Nothing of the first handle's work was retired or redirected.
    expect(s.owner.retiredUnsent).toBe(0);
    expect(unsentOf(s, 'h1')).toBe(2);
    expect(unsentOf(s, 'h2')).toBe(2);
    // Each handle allocated its own generation 1.
    expect(genOf(s, 'h1')).toBe(1);
    expect(genOf(s, 'h2')).toBe(1);

    s = settle(s);

    expect(converged(s)).toBe(true);
    expect(peerConverged(s)).toBe(true);
    expect(s.replica.handle).toBe('h1');
    expect(peer(s).handle).toBe('h2');
    // Different audiences, independently projected.
    expect(audOf(s, 'h1')).toEqual(['crew']);
    expect(audOf(s, 'h2')).toEqual(['deck']);
  });

  it('both handles receive subsequent updates', () => {
    let s = withPeer(local(sim(2), { t: 'join', q: 'j1' }), ['deck']);
    s = settle(localPeer(s, { t: 'join', q: 'j2' }));
    expect(converged(s)).toBe(true);
    expect(peerConverged(s)).toBe(true);

    s = settle(owner(s, { t: 'advance' }));

    expect(s.replica.revision).toBe(s.owner.r);
    expect(peer(s).revision).toBe(s.owner.r);
    expect(converged(s)).toBe(true);
    expect(peerConverged(s)).toBe(true);
  });

  it('superseding one handle leaves the other operational', () => {
    let s = withPeer(local(sim(3), { t: 'join', q: 'j1' }), ['deck']);
    s = settle(localPeer(s, { t: 'join', q: 'j2' }));
    const mine = s.replica.installed;

    // The peer changes audience mid-flight and then again.
    s = localPeer(s, { t: 'setAudience', q: 'p-a', aud: ['bridge'] });
    s = settle(localPeer(s, { t: 'setAudience', q: 'p-b', aud: ['engine'] }));

    expect(audOf(s, 'h2')).toEqual(['engine']);
    expect(peerConverged(s)).toBe(true);
    // The first handle never moved.
    expect(s.replica.installed).toBe(mine);
    expect(genOf(s, 'h1')).toBe(mine);
    expect(audOf(s, 'h1')).toEqual(['crew']);
    expect(converged(s)).toBe(true);
    expect(stillLive(s)).toBe(true);
  });

  it('expiring one handle leaves the other operational', () => {
    let s = withPeer(local(sim(2), { t: 'join', q: 'j1' }), ['deck']);
    s = settle(localPeer(s, { t: 'join', q: 'j2' }));

    const peerRevision = peer(s).revision;
    s = owner(s, { t: 'expire', h: 'h2' });
    s = owner(s, { t: 'advance' });

    // Nothing is addressed to the dead handle …
    expect(s.wire.map((m) => m.h)).toEqual(['h1']);

    s = settle(s);

    // … the survivor keeps receiving live updates …
    expect(s.replica.revision).toBe(s.owner.r);
    expect(converged(s)).toBe(true);
    // … and the expired replica is left exactly where it was.
    expect(peer(s).revision).toBe(peerRevision);
    expect(rec(s, 'h2').live).toBe(false);
  });

  it('expiring a handle mid-emission retires only its own chunks', () => {
    let s = local(sim(4), { t: 'join', q: 'j1' });
    const first = take(s);
    s = pump(serve(first.rest, first.req));
    s = withPeer(s, ['deck']);
    s = localPeer(s, { t: 'join', q: 'j2' });
    const second = s.peerPending[0];
    if (second === undefined) throw new Error('peer sent nothing');
    s = pump(serve({ ...s, peerPending: [] }, second));

    s = emit(s, 'h1');
    s = owner(s, { t: 'expire', h: 'h1' });

    // Three of h1's four chunks were retired; h2 keeps all of its own.
    expect(s.owner.retiredUnsent).toBe(3);
    expect(unsentOf(s, 'h2')).toBe(4);

    s = settle(s);
    expect(peerConverged(s)).toBe(true);
  });
});

describe('a deferred projection is admitted again when it completes', () => {
  it('expiry retires the pending projection instead of completing it', () => {
    let s = joined(2);
    const h = handleOf(s);

    s = owner(s, { t: 'projectable', can: false });
    s = settle(local(s, { t: 'setAudience', q: 'a-b', aud: ['deck'] }));
    expect(deferredOf(s, h)).not.toBeNull();
    const generation = genOf(s, h);

    // The lease expires while the projection is still waiting.
    s = owner(s, { t: 'expire', h });
    expect(rec(s, h).live).toBe(false);
    expect(deferredOf(s, h)).toBeNull();
    expect(s.owner.deferredRetired).toBe(1);

    // Projection becomes available again.
    s = owner(s, { t: 'projectable', can: true });

    // No manifest, no new generation, and the handle stays dead: the
    // completion path revalidates rather than continuing.
    expect(s.wire).toHaveLength(0);
    expect(rec(s, h).live).toBe(false);
    expect(genOf(s, h)).toBe(generation);
    expect(allocOf(s, h)).toBe(generation);
    expect(s.replica.published).toBe(false);
  });

  it('a control on an expired handle is refused, not deferred', () => {
    let s = joined(2);
    const h = handleOf(s);

    // Projection unavailable AND the handle dead: the deferral branch
    // must not be reached, or a dead handle acquires pending work.
    s = owner(s, { t: 'projectable', can: false });
    s = owner(s, { t: 'expire', h });
    s = local(s, { t: 'setAudience', q: 'a-b', aud: ['deck'] });
    const taken = take(s);
    s = serve(taken.rest, taken.req);

    expect(s.wire).toEqual([{ k: 'no', h, q: 'a-b', code: 'closed' }]);
    expect(deferredOf(s, h)).toBeNull();

    // Actionable: the replica rejoins once a projection can be taken.
    s = pump(s);
    expect(s.replica.handle).toBeNull();
    s = settle(owner(s, { t: 'projectable', can: true }));
    expect(s.replica.handle).toBe('h2');
    expect(converged(s)).toBe(true);
  });

  it('a live deferred projection still completes (control)', () => {
    let s = joined(2);
    const h = handleOf(s);

    s = owner(s, { t: 'projectable', can: false });
    s = settle(local(s, { t: 'setAudience', q: 'a-b', aud: ['deck'] }));
    expect(deferredOf(s, h)).not.toBeNull();

    s = settle(owner(s, { t: 'projectable', can: true }));

    expect(audOf(s, h)).toEqual(['deck']);
    expect(converged(s)).toBe(true);
  });

  it('completion refuses a handle that died without retiring its work', () => {
    // Unreachable: `expire` retires the pending projection. Asserted
    // directly, against the state a forgetful expiry would leave, so
    // that revalidation at the installation point is a witnessed rule
    // and not an assumption.
    let s = joined(2);
    const h = handleOf(s);
    s = owner(s, { t: 'projectable', can: false });
    s = settle(local(s, { t: 'setAudience', q: 'a-b', aud: ['deck'] }));
    const stranded = { ...rec(s, h), live: false };
    s = { ...s, owner: { ...s.owner, handles: { ...s.owner.handles, [h]: stranded } } };
    expect(deferredOf(s, h)).not.toBeNull();

    s = owner(s, { t: 'projectable', can: true });

    // Refused, not installed — and the refusal is delivered, so the
    // caller can act on it instead of waiting out its deadline.
    expect(s.wire).toEqual([{ k: 'no', h, q: 'a-b', code: 'closed' }]);
    expect(s.owner.deferredRetired).toBe(1);
    expect(genOf(s, h)).toBe(1);
    expect(deferredOf(s, h)).toBeNull();

    // And that refusal is actionable: the replica rejoins.
    s = settle(s);
    expect(s.replica.handle).toBe('h2');
    expect(converged(s)).toBe(true);
  });

  it('a request on an expired handle is refused, and recovery rejoins at generation 1', () => {
    let s = joined(2);
    expect(s.replica.retired).toBe(1);

    s = owner(s, { t: 'expire', h: 'h1' });
    s = local(s, { t: 'reconnect', q: 'rc1' });
    s = settle(s);

    // Generations are per handle, so the new handle's first
    // installation is generation 1 again. Carrying the old watermark
    // across would fail `g > retired` and strand the rejoin — which the
    // round-4 global allocator hid by never issuing 1 twice.
    expect(s.replica.handle).toBe('h2');
    expect(s.replica.installed).toBe(1);
    expect(s.replica.retired).toBe(1);
    expect(allocOf(s, 'h2')).toBe(1);
    expect(converged(s)).toBe(true);
    expect(stillLive(s)).toBe(true);
  });
});

describe('the snapshot/live boundary is a property of the emitter', () => {
  it('an advance during emission is queued behind the chunks it depends on', () => {
    let s = joined(2);

    // A replacement is being emitted when the document advances.
    s = owner(s, { t: 'refresh', h: handleOf(s) });
    s = owner(s, { t: 'advance' });
    while (emitting(s.owner).length > 0) s = emit(s);

    // Ordinary FIFO order, produced by the owner: snapshot, then live.
    expect(s.wire.map((m) => m.k)).toEqual(['man', 'snap', 'snap', 'delta']);

    s = settle(s);

    // Reached the current revision without touching the out-of-order
    // recovery fallback at all.
    expect(s.replica.dropped['delta-ahead-of-assembly']).toBeUndefined();
    expect(s.replica.behind).toBe(false);
    expect(s.replica.revision).toBe(s.owner.r);
    expect(converged(s)).toBe(true);
  });

  it('several advances during one emission all land, in order', () => {
    let s = joined(3);

    s = owner(s, { t: 'refresh', h: handleOf(s) });
    s = owner(s, { t: 'advance' });
    s = owner(s, { t: 'advance' });
    s = settle(s);

    expect(s.replica.dropped['delta-ahead-of-assembly']).toBeUndefined();
    expect(s.replica.revision).toBe(s.owner.r);
    expect(converged(s)).toBe(true);
  });

  it('pending work is bounded: catch-up gives way to a replacement', () => {
    let s = joined(4);

    s = owner(s, { t: 'refresh', h: handleOf(s) });
    for (let i = 0; i < 8; i += 1) s = owner(s, { t: 'advance' });

    // The queue never grows without bound …
    expect(s.owner.handles[handleOf(s)]?.queue.length).toBeLessThanOrEqual(QUEUE_MAX);
    expect(s.owner.retiredDeltas).toBeGreaterThan(0);

    s = settle(s);

    // … and the replacement is what carries the replica to the current
    // revision, rather than a resync the replica had to ask for.
    expect(s.replica.dropped['delta-ahead-of-assembly']).toBeUndefined();
    expect(s.replica.revision).toBe(s.owner.r);
    expect(converged(s)).toBe(true);
  });
});

describe('an owner refresh never takes the caller’s turn', () => {
  it('does not replace a deferred caller transition', () => {
    let s = joined(2);
    const h = handleOf(s);

    // The caller asks for ['deck']; the owner cannot project yet.
    s = owner(s, { t: 'projectable', can: false });
    s = settle(local(s, { t: 'setAudience', q: 'deck-request', aud: ['deck'] }));
    expect(deferredOf(s, h)?.q).toBe('deck-request');

    // An owner refresh here would clear that pending projection.
    s = owner(s, { t: 'refresh', h });
    expect(s.wire).toHaveLength(0);
    expect(deferredOf(s, h)?.q).toBe('deck-request');

    // Availability returns and the caller's own request completes.
    s = settle(owner(s, { t: 'projectable', can: true }));

    expect(audOf(s, h)).toEqual(['deck']);
    expect(s.replica.desired).toEqual(['deck']);
    expect(converged(s)).toBe(true);
    expect(stillLive(s)).toBe(true);
  });

  it('does not replace a pending transition even when it could project', () => {
    // Unreachable through the reducers: `deferred` is only set while a
    // projection is unavailable, and `projectable` completes it the
    // moment availability returns. Asserted against the constructed
    // state so that the rule is a witnessed one and not a consequence
    // of that coupling — any future reason to defer (a rate limit, a
    // busy projector) would otherwise silently reopen the hole.
    let s = joined(2);
    const h = handleOf(s);
    const pending = { ...rec(s, h), deferred: { q: 'deck-request', aud: ['deck'] as string[] } };
    s = { ...s, owner: { ...s.owner, handles: { ...s.owner.handles, [h]: pending } } };
    const generation = genOf(s, h);

    s = owner(s, { t: 'refresh', h });

    expect(s.wire).toHaveLength(0);
    expect(genOf(s, h)).toBe(generation);
    expect(deferredOf(s, h)?.q).toBe('deck-request');

    // And the pending request — not a refresh — is what completes.
    s = owner(s, { t: 'projectable', can: true });
    expect(s.wire).toEqual([
      { k: 'man', h, g: generation + 1, r: s.owner.r, n: 2, q: 'deck-request' },
    ]);
    expect(audOf(s, h)).toEqual(['deck']);
  });

  it('does not emit a manifest it cannot follow with chunks', () => {
    let s = joined(2);
    const h = handleOf(s);
    const generation = genOf(s, h);

    s = owner(s, { t: 'projectable', can: false });
    s = owner(s, { t: 'refresh', h });

    // No manifest, no generation burned.
    expect(s.wire).toHaveLength(0);
    expect(genOf(s, h)).toBe(generation);
    expect(allocOf(s, h)).toBe(generation);

    // And once a projection can be taken, a refresh works (control).
    s = settle(owner(s, { t: 'projectable', can: true }));
    s = settle(owner(s, { t: 'refresh', h }));
    expect(genOf(s, h)).toBe(generation + 1);
    expect(converged(s)).toBe(true);
  });
});

describe('the refusal taxonomy', () => {
  it('`closed` discards the handle and rejoins, replaying nothing', () => {
    let s = joined(2);
    s = owner(s, { t: 'expire', h: 'h1' });

    s = local(s, { t: 'reconnect', q: 'rc1' });
    const taken = take(s);
    s = serve(taken.rest, taken.req);
    // One code for every unusable handle: unknown, expired, fenced or
    // bound to another peer are indistinguishable by design (§2).
    expect(s.wire).toEqual([{ k: 'no', h: 'h1', q: 'rc1', code: 'closed' }]);

    s = pump(s);

    // Exactly one request, and it is a join: nothing is resumed,
    // nothing is resynced, no action or input is replayed.
    expect(s.pending).toHaveLength(1);
    expect(s.pending[0]?.k).toBe('join');
    expect(s.replica.handle).toBeNull();
    expect(s.replica.retired).toBe(0);
    expect(s.replica.published).toBe(false);

    s = settle(s);
    expect(s.replica.handle).toBe('h2');
    expect(s.replica.installed).toBe(1);
    expect(converged(s)).toBe(true);
  });

  it('`owner-lost` is terminal — no rejoin, no inferred outcome', () => {
    let s = joined(2);

    s = local(s, { t: 'setAudience', q: 'a-b', aud: ['deck'] });
    s = { ...s, pending: [] };
    s = pump({ ...s, wire: [{ k: 'no', h: 'h1', q: 'a-b', code: 'owner-lost' }] });

    expect(s.replica.state).toBe('closed');
    expect(s.replica.published).toBe(false);
    // The distinction that earns a second code: nothing to rejoin.
    expect(s.pending).toHaveLength(0);
  });

  it('an unsolicited `closed` notice recovers the subscription', () => {
    let s = joined(2);

    // Lease expiry arrives with no `q` to correlate (§2).
    s = pump({ ...s, wire: [{ k: 'no', h: 'h1', q: null, code: 'closed' }] });

    expect(s.replica.handle).toBeNull();
    expect(s.pending[0]?.k).toBe('join');

    s = settle(s);
    expect(converged(s)).toBe(true);
  });

  it('a local leave is terminal and sends nothing', () => {
    let s = joined(2);

    s = local(s, { t: 'leave' });

    expect(s.replica.state).toBe('closed');
    expect(s.replica.published).toBe(false);
    expect(s.pending).toHaveLength(0);
  });

  it('any other refusal fences without rejoining', () => {
    let s = joined(2);

    s = local(s, { t: 'setAudience', q: 'a-b', aud: ['deck'] });
    s = { ...s, pending: [] };
    s = pump({ ...s, wire: [{ k: 'no', h: 'h1', q: 'a-b', code: 'forbidden' }] });

    expect(s.replica.state).toBe('fenced');
    expect(s.replica.published).toBe(false);
    expect(s.pending).toHaveLength(0);
    // The handle survives a refused request: it was the request that
    // was refused, not the subscription.
    expect(s.replica.handle).toBe('h1');
  });
});

describe('overflow recovery is scheduled, not hoped for', () => {
  it('a delta-only queue that overflows still converges', () => {
    let s = joined(2);
    const h = handleOf(s);

    // A replacement's chunks are queued, and updates pile up behind
    // them …
    s = owner(s, { t: 'refresh', h });
    for (let i = 0; i < 3; i += 1) s = owner(s, { t: 'advance' });
    expect(s.owner.handles[h]?.queue.map((m) => m.k)).toEqual([
      'snap', 'snap', 'delta', 'delta', 'delta',
    ]);

    // … then only the chunks are handed over, leaving a delta-only
    // queue.
    s = emit(s, h);
    s = emit(s, h);
    while (s.wire.length > 0) s = pump(s);
    expect(s.owner.handles[h]?.queue.every((m) => m.k === 'delta')).toBe(true);

    // Overflow it. The advance that overflows retires every queued
    // delta, so the queue is empty and nothing further will be
    // dequeued — there is no later message to piggyback recovery on.
    for (let i = 0; i < 4; i += 1) s = owner(s, { t: 'advance' });
    expect(s.owner.retiredDeltas).toBeGreaterThan(0);
    expect(s.owner.handles[h]?.replaceWhenDrained).toBe(false);

    s = settle(s);

    // Convergence must not depend on another message arriving, and the
    // replica must not have had to ask.
    expect(s.replica.revision).toBe(s.owner.r);
    expect(s.pending.filter((q) => q.k === 'resync')).toHaveLength(0);
    expect(converged(s)).toBe(true);
    expect(stillLive(s)).toBe(true);
  });

  it('overflow with chunks still outstanding waits for the drain (control)', () => {
    let s = joined(4);

    s = owner(s, { t: 'refresh', h: handleOf(s) });
    for (let i = 0; i < 8; i += 1) s = owner(s, { t: 'advance' });
    // The replacement may not pre-empt its own outstanding emission.
    expect(unsentOf(s)).toBeGreaterThan(0);
    expect(s.owner.handles[handleOf(s)]?.replaceWhenDrained).toBe(true);

    s = settle(s);

    expect(s.replica.revision).toBe(s.owner.r);
    expect(converged(s)).toBe(true);
  });

  it('overflow while a projection is unavailable recovers on restoration', () => {
    let s = joined(2);
    const h = handleOf(s);

    s = owner(s, { t: 'refresh', h });
    for (let i = 0; i < 3; i += 1) s = owner(s, { t: 'advance' });
    s = emit(s, h);
    s = emit(s, h);
    while (s.wire.length > 0) s = pump(s);

    s = owner(s, { t: 'projectable', can: false });
    for (let i = 0; i < 4; i += 1) s = owner(s, { t: 'advance' });

    // It cannot emit a manifest it could not follow with chunks, so the
    // recovery becomes this handle's pending projection …
    expect(s.wire).toHaveLength(0);
    expect(deferredOf(s, h)).not.toBeNull();
    expect(deferredOf(s, h)?.q).toBeNull();

    // … and restoration completes it through the same path a caller's
    // own request would take.
    s = settle(owner(s, { t: 'projectable', can: true }));
    expect(s.replica.revision).toBe(s.owner.r);
    expect(converged(s)).toBe(true);
  });

  it('a pending caller request subsumes the overflow recovery', () => {
    let s = joined(4);

    // Overflow while chunks are still outstanding.
    s = owner(s, { t: 'refresh', h: handleOf(s) });
    for (let i = 0; i < 8; i += 1) s = owner(s, { t: 'advance' });
    expect(s.owner.handles[handleOf(s)]?.replaceWhenDrained).toBe(true);

    // The caller asks for a different audience; the owner defers it.
    s = owner(s, { t: 'projectable', can: false });
    s = local(s, { t: 'setAudience', q: 'desired-deck', aud: ['deck'] });
    const taken = take(s);
    s = serve(taken.rest, taken.req);
    expect(deferredOf(s)?.q).toBe('desired-deck');

    // Draining the old chunks must not overwrite it with an
    // unsolicited replacement for the OLD audience.
    while (emitting(s.owner).length > 0) s = pump(emit(s));
    expect(deferredOf(s)?.q).toBe('desired-deck');
    expect(deferredOf(s)?.aud).toEqual(['deck']);

    s = settle(owner(s, { t: 'projectable', can: true }));

    expect(audOf(s)).toEqual(['deck']);
    expect(s.replica.desired).toEqual(['deck']);
    expect(converged(s)).toBe(true);
    expect(stillLive(s)).toBe(true);
  });
});

describe('a refused refresh does not cancel work someone else owed', () => {
  it('an armed overflow survives a refused refresh and still converges', () => {
    let s = joined(4);
    const h = handleOf(s);

    // Overflow while chunks remain: a replacement is owed.
    s = owner(s, { t: 'refresh', h });
    for (let i = 0; i < 8; i += 1) s = owner(s, { t: 'advance' });
    expect(s.owner.handles[h]?.replaceWhenDrained).toBe(true);
    expect(unsentOf(s, h)).toBeGreaterThan(0);

    // An optional refresh arrives and is refused, because this handle's
    // own emission is outstanding. It must not arm recovery — and must
    // not disarm the recovery the overflow already required.
    s = owner(s, { t: 'refresh', h });
    expect(s.wire.filter((m) => m.k === 'man')).toHaveLength(1);
    expect(s.owner.handles[h]?.replaceWhenDrained).toBe(true);

    // Drain everything, with no further updates at all.
    s = settle(s);

    expect(s.replica.revision).toBe(s.owner.r);
    expect(s.pending.filter((q) => q.k === 'resync')).toHaveLength(0);
    expect(converged(s)).toBe(true);
  });

  it('an armed overflow survives a refresh refused for availability', () => {
    let s = joined(2);
    const h = handleOf(s);

    // Arm the obligation, then let the chunks drain while a projection
    // is unavailable, so the obligation is owed with an empty queue.
    s = owner(s, { t: 'refresh', h });
    for (let i = 0; i < 3; i += 1) s = owner(s, { t: 'advance' });
    s = emit(s, h);
    s = emit(s, h);
    while (s.wire.length > 0) s = pump(s);
    s = owner(s, { t: 'projectable', can: false });
    for (let i = 0; i < 4; i += 1) s = owner(s, { t: 'advance' });
    expect(deferredOf(s, h)?.q).toBeNull();

    // A refused optional refresh must leave that pending recovery.
    s = owner(s, { t: 'refresh', h });
    expect(deferredOf(s, h)?.q).toBeNull();

    s = settle(owner(s, { t: 'projectable', can: true }));
    expect(s.replica.revision).toBe(s.owner.r);
    expect(converged(s)).toBe(true);
  });

  it('a refusal for availability preserves an owed replacement', () => {
    // Unreachable through the reducers: an overflow with an empty queue
    // is converted to a pending projection or installed at once, so an
    // obligation never coexists with an empty queue. Asserted against
    // the constructed state, because the rule is "a refusal never
    // disarms what someone else owed" on **every** branch — an
    // exception justified only by reachability is the kind that stops
    // being true when a later path reaches it.
    let s = joined(2);
    const h = handleOf(s);
    s = owner(s, { t: 'projectable', can: false });
    const armed = { ...rec(s, h), queue: [], deferred: null, replaceWhenDrained: true };
    s = { ...s, owner: { ...s.owner, handles: { ...s.owner.handles, [h]: armed } } };

    s = owner(s, { t: 'refresh', h });

    // The obligation became this handle's pending projection rather
    // than being dropped.
    expect(s.wire).toHaveLength(0);
    expect(deferredOf(s, h)).not.toBeNull();
    expect(deferredOf(s, h)?.q).toBeNull();

    s = settle(owner(s, { t: 'projectable', can: true }));
    expect(s.replica.revision).toBe(s.owner.r);
    expect(converged(s)).toBe(true);
  });

  it('a refused refresh with nothing owed arms nothing (control)', () => {
    let s = joined(4);
    const h = handleOf(s);

    // An installation in flight, but no overflow: nothing is owed.
    s = owner(s, { t: 'refresh', h });
    expect(s.owner.handles[h]?.replaceWhenDrained).toBe(false);
    const allocated = allocOf(s, h);

    s = owner(s, { t: 'refresh', h });
    expect(s.owner.handles[h]?.replaceWhenDrained).toBe(false);

    s = settle(s);

    // No second installation appeared behind the caller's back.
    expect(allocOf(s, h)).toBe(allocated);
    expect(converged(s)).toBe(true);
  });

  it('an installation satisfies the obligation and clears it', () => {
    let s = joined(2);
    const h = handleOf(s);

    s = owner(s, { t: 'refresh', h });
    for (let i = 0; i < 3; i += 1) s = owner(s, { t: 'advance' });
    s = emit(s, h);
    s = emit(s, h);
    while (s.wire.length > 0) s = pump(s);
    for (let i = 0; i < 4; i += 1) s = owner(s, { t: 'advance' });

    // The replacement ran, so nothing is owed any more …
    expect(s.owner.handles[h]?.replaceWhenDrained).toBe(false);
    s = settle(s);
    const allocated = allocOf(s, h);

    // … and draining does not install a second time.
    expect(allocOf(s, h)).toBe(allocated);
    expect(converged(s)).toBe(true);
  });
});

describe('handle loss preserves local intent', () => {
  it('an expiry notice does not reopen a cancelled subscription', () => {
    let s = joined(2);

    s = local(s, { t: 'setAudience', q: 'a-b', aud: ['deck'] });
    s = { ...s, pending: [] };
    s = local(s, { t: 'cancel' });
    expect(s.replica.state).toBe('fenced');

    // The handle expires while the caller is fenced.
    s = pump({ ...s, wire: [{ k: 'no', h: 'h1', q: null, code: 'closed' }] });

    // The handle and its generation state are gone …
    expect(s.replica.handle).toBeNull();
    expect(s.replica.retired).toBe(0);
    expect(s.replica.installed).toBeNull();
    // … but the fence the caller set is still theirs to lift.
    expect(s.replica.state).toBe('fenced');
    expect(s.pending).toHaveLength(0);
  });

  it('the caller’s own next request rejoins after a fenced expiry', () => {
    let s = joined(2);
    s = local(s, { t: 'setAudience', q: 'a-b', aud: ['deck'] });
    s = { ...s, pending: [] };
    s = local(s, { t: 'cancel' });
    s = pump({ ...s, wire: [{ k: 'no', h: 'h1', q: null, code: 'closed' }] });

    // With no handle learned, the only request it can issue is a join.
    s = local(s, { t: 'setAudience', q: 'a-c', aud: ['bridge'] });
    expect(s.pending[0]?.k).toBe('join');

    s = settle(s);
    expect(s.replica.handle).toBe('h2');
    expect(audOf(s, 'h2')).toEqual(['bridge']);
    expect(converged(s)).toBe(true);
  });

  it('an active subscription still rejoins on expiry (control)', () => {
    let s = joined(2);

    s = pump({ ...s, wire: [{ k: 'no', h: 'h1', q: null, code: 'closed' }] });

    expect(s.replica.state).toBe('joining');
    expect(s.pending[0]?.k).toBe('join');

    s = settle(s);
    expect(converged(s)).toBe(true);
  });

  it('a local leave stays terminal through an expiry notice (control)', () => {
    let s = joined(2);
    s = local(s, { t: 'leave' });

    s = pump({ ...s, wire: [{ k: 'no', h: 'h1', q: null, code: 'closed' }] });

    expect(s.replica.state).toBe('closed');
    expect(s.pending).toHaveLength(0);
  });
});
