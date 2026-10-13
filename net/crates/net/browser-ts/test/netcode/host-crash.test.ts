// A host lost (sharded-match plan, S6): recovery is a rollback to what the
// surviving hosts observed, and never two hosts simulating one entity.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { shardHost } from '../../demo/sharded-arena/shard.js';
import type { ShardCombat, ShardHost } from '../../demo/sharded-arena/shard.js';
import { hostNetcode, joinNetcode, joinShards, leaseWatch, mayReassign } from '../../src/netcode/index.js';
import type { ShardAssignment, ShardedMatch } from '../../src/netcode/index.js';
import { handoffLink, regionHandoffs, regionState } from '../../src/world/index.js';
import { simNetwork } from './sim.js';

const lib = { hostNetcode, joinNetcode, handoffLink, regionHandoffs, regionState };
const LABEL = 'arena.move';
const A = '00000000000000a0';
const B = '00000000000000b0';
const PA = '00000000000000e1'; // owned by s0, on host A — the host that is lost
const PB = '00000000000000e2'; // owned by s1, on host B
const LEASE_MS = 300;
const DRIFT_MS = 100;
/** How late a renewal can reach a host here (20 ms latency + 5 ms jitter, rounded up): part of the reassignment wait. */
const MAX_DELIVERY_MS = 50;

interface Body {
  readonly x: number;
  readonly z: number;
  readonly hp: number;
}
interface Act {
  readonly dx?: number;
  readonly fire?: { readonly target: string; readonly seenB: number };
}
const apply = (body: Body, input: Act): Body => ({ ...body, x: body.x + (input.dx ?? 0) });
const combat: ShardCombat<Body> = {
  fire: shooter => (shooter.hp > 0 ? shooter : 'dead'),
  hits: () => true,
  damage: target => ({ ...target, hp: target.hp - 50 }),
  maxSpeed: 40,
};

function arena() {
  const net = simNetwork({ latencyMs: 20, jitterMs: 5 });
  let assignment: ShardAssignment = { epoch: 1, hosts: { s0: A, s1: B }, owner: { [PA]: 's0', [PB]: 's1' } };

  // The lobby, standing in: lease counters, renewed every LEASE_MS / 3 for
  // each host it can reach. A partitioned host's copy stops moving.
  const leases: Record<string, number> = { s0: 0, s1: 0 };
  const seenBy: Record<string, number> = { s0: 0, s1: 0 };
  const lastRenewedAt: Record<string, number> = { s0: Date.now(), s1: Date.now() };
  const cut = new Set<string>();
  const renewer = setInterval(() => {
    for (const shard of Object.keys(leases)) {
      if (cut.has(shard)) continue;
      leases[shard]! += 1;
      seenBy[shard] = leases[shard]!;
      lastRenewedAt[shard] = Date.now();
    }
  }, LEASE_MS / 3);

  // Which host last ADVANCED each entity, and when: simulation as it
  // happened, not as a host reports it.
  const advanced: Record<string, Record<string, number>> = { s0: {}, s1: {} };
  const host = (id: string, shard: string, initial: Record<string, Body>) =>
    shardHost<Body, Act>(lib, {
      transport: net.node(id),
      label: LABEL,
      assignment,
      shard,
      roster: [PA, PB],
      initial,
      apply,
      combat,
      advance: entities => {
        for (const entity of Object.keys(entities)) advanced[shard]![entity] = Date.now();
        return entities;
      },
      live: leaseWatch({ read: () => seenBy[shard], leaseMs: LEASE_MS }).live,
      now: () => Date.now(),
    });
  const hosts = { s0: host(A, 's0', { [PA]: { x: 0, z: 0, hp: 100 } }), s1: host(B, 's1', { [PB]: { x: 5, z: 0, hp: 100 } }) };
  const player = (id: string): ShardedMatch<Body, Act> =>
    joinShards<Body, Act>({ transport: net.node(id), label: LABEL, assignment, local: { id, predict: apply }, now: () => Date.now() });
  const players = { [PA]: player(PA), [PB]: player(PB) };

  /** Cut host A off from everything — the lobby too. It keeps running. */
  const lose = (shard: 's0') => {
    cut.add(shard);
    for (const other of [B, PA, PB]) {
      net.block(A, other, 60_000);
      net.block(other, A, 60_000);
    }
  };
  /** The lobby's reassignment of a lost shard's entities, once the lease wait allows. */
  const reassign = (from: 's0', to: 's1') => {
    if (!mayReassign(lastRenewedAt[from]!, Date.now(), LEASE_MS, DRIFT_MS, MAX_DELIVERY_MS)) return false;
    const owner = { ...assignment.owner };
    for (const [id, shard] of Object.entries(owner)) if (shard === from) owner[id] = to;
    assignment = { ...assignment, epoch: assignment.epoch + 1, owner };
    for (const h of Object.values(hosts)) h.setAssignment(assignment);
    for (const p of Object.values(players)) p.setAssignment(assignment);
    return true;
  };
  /** Host A is back: the network heals and the lobby renews it again. */
  const heal = (shard: 's0') => {
    cut.delete(shard);
    for (const other of [B, PA, PB]) {
      net.block(A, other, 0);
      net.block(other, A, 0);
    }
  };
  const close = () => {
    clearInterval(renewer);
    for (const p of Object.values(players)) p.close();
    for (const h of Object.values(hosts)) h.close();
  };
  return { net, hosts, players, advanced, lose, reassign, heal, close };
}

describe('a host lost', () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it('stops at lease expiry though it keeps running, and no entity is ever simulated by two hosts', async () => {
    const m = arena();
    await vi.advanceTimersByTimeAsync(1_500);
    const steer = setInterval(() => {
      m.players[PA]!.input({ dx: 1 });
      m.players[PB]!.input({ dx: 1 });
    }, 50);
    let overlaps = 0;
    const watch = setInterval(() => {
      const now = Date.now();
      for (const id of [PA, PB]) {
        const onA = now - (m.advanced.s0![id] ?? -Infinity) < 60;
        const onB = now - (m.advanced.s1![id] ?? -Infinity) < 60;
        if (onA && onB) overlaps += 1;
      }
    }, 5);
    await vi.advanceTimersByTimeAsync(500);

    m.lose('s0');
    const cutAt = Date.now();
    // Too early: the lease wait has not passed.
    await vi.advanceTimersByTimeAsync(LEASE_MS / 2);
    expect(m.reassign('s0', 's1')).toBe(false);
    // Host A, still running, stops on its own clock within leaseMs.
    await vi.advanceTimersByTimeAsync(LEASE_MS);
    expect(Date.now() - (m.advanced.s0![PA] ?? 0)).toBeGreaterThan(50);
    expect((m.advanced.s0![PA] ?? 0) - cutAt).toBeLessThanOrEqual(LEASE_MS);
    await vi.advanceTimersByTimeAsync(DRIFT_MS);

    // The lobby reassigns; B restores PA from what it last observed of A.
    const observed = m.hosts.s1.observed('s0')![PA]!;
    expect(m.reassign('s0', 's1')).toBe(true);
    expect(m.hosts.s1.recovered()).toEqual([PA]);
    expect(m.hosts.s1.world()[PA]).toEqual(observed);

    // The player resumes on its new owner from its next input; what it had
    // pending was made against the rolled-back state, and is dropped.
    await vi.advanceTimersByTimeAsync(1_000);
    expect(m.hosts.s1.world()[PA]!.x).toBeGreaterThan(observed.x);
    expect(m.hosts.s1.netcode.dropped.resumed).toBeGreaterThan(0);

    // Host A comes back, lease and all. It holds the new assignment, so it
    // simulates nothing it no longer owns.
    m.heal('s0');
    await vi.advanceTimersByTimeAsync(1_000);
    clearInterval(steer);
    clearInterval(watch);
    expect(m.hosts.s0.world()[PA]).toBeUndefined();
    expect(overlaps).toBe(0);
    m.close();
  });

  it('a hit in flight to the lost host is a miss, never re-sent to the new owner, never applied twice', async () => {
    const m = arena();
    await vi.advanceTimersByTimeAsync(1_500);
    m.lose('s0');
    // PB (on B) fires at PA (owned by the host just lost).
    const shooter = m.players[PB]!;
    shooter.input({ fire: { target: PA, seenB: shooter.hostNow('s0')! - 100 } });
    await vi.advanceTimersByTimeAsync(LEASE_MS + DRIFT_MS + MAX_DELIVERY_MS);
    const observed = m.hosts.s1.observed('s0')![PA]!;
    expect(m.reassign('s0', 's1')).toBe(true);
    await vi.advanceTimersByTimeAsync(1_000);
    // Unanswered: a miss.
    expect(m.hosts.s1.combat().forwarded.unresolved).toBe(1);
    // Not re-sent to PA's new owner (B itself), not applied there.
    expect(m.hosts.s1.combat().served.hit).toBeUndefined();
    expect(m.hosts.s1.world()[PA]!.hp).toBe(observed.hp);
    expect(observed.hp).toBe(100);
    m.close();
  });
});

// SHARD-7: installing an assignment is defence in depth — a bad epoch is
// refused, a host that no longer serves its shard stops, and the hosts it
// trusts and spectates follow the assignment.
describe('shardHost.setAssignment', () => {
  const C = '00000000000000c0';
  beforeEach(() => {
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  function trio() {
    const net = simNetwork({ latencyMs: 20 });
    const first: ShardAssignment = { epoch: 1, hosts: { s0: A, s1: B }, owner: { [PA]: 's0', [PB]: 's1' } };
    const host = (id: string, shard: string, assignment: ShardAssignment, initial: Record<string, Body>) =>
      shardHost<Body, Act>(lib, { transport: net.node(id), label: LABEL, assignment, shard, roster: [PA, PB], initial, apply });
    const a = host(A, 's0', first, { [PA]: { x: 0, z: 0, hp: 100 } });
    const b = host(B, 's1', first, { [PB]: { x: 5, z: 0, hp: 100 } });
    return { net, first, host, a, b };
  }

  it('refuses an epoch that is not a safe integer above the installed one — and a rollback after it', async () => {
    const m = trio();
    await vi.advanceTimersByTimeAsync(500);
    for (const epoch of [Number.NaN, undefined, 1.5, 1, 0, Number.MAX_SAFE_INTEGER + 2]) {
      expect(m.b.setAssignment({ ...m.first, epoch } as unknown as ShardAssignment)).toBe(false);
    }
    // Had NaN been installed, every later comparison would be false and
    // this rollback to an older epoch would be taken.
    expect(m.b.setAssignment({ ...m.first, epoch: 0, owner: {} })).toBe(false);
    expect(m.b.world()[PB]).toBeDefined();
    expect(m.b.setAssignment({ ...m.first, epoch: 2 })).toBe(true);
    m.a.close();
    m.b.close();
  });

  it('stops serving a shard the assignment gives another host; the others trust and spectate the new host', async () => {
    const m = trio();
    await vi.advanceTimersByTimeAsync(1_000);
    // s0 moves to host C (A leaves the match cleanly; C restores nothing,
    // it starts from PA's state as the lobby hands it over).
    const second: ShardAssignment = { ...m.first, epoch: 2, hosts: { s0: C, s1: B } };
    const c = m.host(C, 's0', second, { [PA]: m.a.world()[PA]! });
    expect(m.a.setAssignment(second)).toBe(true);
    expect(m.b.setAssignment(second)).toBe(true);
    // A holds and sends nothing for s0 any more, and serves no hits.
    expect(m.a.world()).toEqual({});
    await vi.advanceTimersByTimeAsync(1_000);
    expect(m.a.world()).toEqual({});
    // B admits C (its `authorize` follows the hosts) and spectates C, not A.
    expect(c.observed('s1')?.[PB]).toBeDefined();
    expect(m.b.observed('s0')?.[PA]).toEqual(c.world()[PA]);
    // (C is on no roster: only a hosts set that followed the assignment admits it.)
    expect(m.b.netcode.players()).toContain(C);
    expect(m.b.netcode.dropped.unauthorized ?? 0).toBe(0);
    // And A, serving nothing, spectates nothing.
    expect(m.a.observed('s1')).toBeNull();
    m.a.close();
    m.b.close();
    c.close();
  });
});
