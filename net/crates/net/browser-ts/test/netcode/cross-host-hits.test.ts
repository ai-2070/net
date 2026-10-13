// Cross-host hits (sharded-match plan, S3): a shot whose target another
// host owns is checked by the shooter's host (A), forwarded, and decided by
// the target's host (B) against B's own history — over a link that drops,
// duplicates and reorders.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { shardHost } from '../../demo/sharded-arena/shard.js';
import type { ShardCombat, ShardHost } from '../../demo/sharded-arena/shard.js';
import { hostNetcode, joinNetcode, joinShards } from '../../src/netcode/index.js';
import type { ShardAssignment, ShardedMatch } from '../../src/netcode/index.js';
import { BorderActionError, handoffLink, regionHandoffs, regionState } from '../../src/world/index.js';
import { simNetwork, type SimOptions } from './sim.js';

const lib = { hostNetcode, joinNetcode, handoffLink, regionHandoffs, regionState };
const LABEL = 'arena.move';
const DELAY_MS = 100;
const A = '00000000000000a0';
const B = '00000000000000b0';
const SHOOTER = '00000000000000e1';
const TARGET = '00000000000000e2';

interface Body {
  readonly x: number;
  readonly z: number;
  readonly hp: number;
  readonly ammo: number;
}
interface Act {
  readonly dx?: number;
  readonly fire?: { readonly target: string; readonly seenB: number };
}
const apply = (body: Body, input: Act): Body => ({ ...body, x: body.x + (input.dx ?? 0) });

const combat: ShardCombat<Body> = {
  fire: shooter => (shooter.hp <= 0 ? 'dead' : shooter.ammo <= 0 ? 'empty' : { ...shooter, ammo: shooter.ammo - 1 }),
  hits: (shooter, target) => Math.hypot(shooter.x - target.x, shooter.z - target.z) <= 30,
  damage: target => ({ ...target, hp: target.hp - 50 }),
  maxSpeed: 40,
};

/** `clock: 'default'` leaves every `now` unset: the recipe's and the netcode's own clock, as a page runs. */
function arena(sim: SimOptions, damage = combat.damage, clock: 'date' | 'default' = 'date') {
  const now = clock === 'date' ? { now: () => Date.now() } : {};
  const net = simNetwork(sim);
  const assignment: ShardAssignment = { epoch: 1, hosts: { s0: A, s1: B }, owner: { [SHOOTER]: 's0', [TARGET]: 's1' } };
  const host = (id: string, shard: string, initial: Record<string, Body>) =>
    shardHost<Body, Act>(lib, {
      transport: net.node(id),
      label: LABEL,
      assignment,
      shard,
      roster: [SHOOTER, TARGET],
      initial,
      apply,
      combat: { ...combat, damage },
      ...now,
    });
  const hosts: [ShardHost<Body>, ShardHost<Body>] = [
    host(A, 's0', { [SHOOTER]: { x: 0, z: 0, hp: 100, ammo: 10 } }),
    host(B, 's1', { [TARGET]: { x: 20, z: 0, hp: 100, ammo: 10 } }),
  ];
  const player = (id: string): ShardedMatch<Body, Act> =>
    joinShards<Body, Act>({
      transport: net.node(id),
      label: LABEL,
      assignment,
      local: { id, predict: apply },
      interpolationDelayMs: DELAY_MS,
      ...now,
    });
  const players = { shooter: player(SHOOTER), target: player(TARGET) };
  /** `who` fires at `at`, with `seenB` read the way a client reads it: the target's host clock, minus the delay. */
  const fire = (who: ShardedMatch<Body, Act>, at: string, shard: string, seenOffsetMs = 0) =>
    who.input({ fire: { target: at, seenB: who.hostNow(shard)! - DELAY_MS - seenOffsetMs } });
  const close = () => {
    players.shooter.close();
    players.target.close();
    for (const h of hosts) h.close();
  };
  return { net, hosts, players, fire, close };
}

describe('cross-host hits', () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it('applies a duplicated, retried hit exactly once', async () => {
    // 40 ms each way: every forward is retried (50 ms) before its answer
    // can be back, and the link itself duplicates half of everything.
    const m = arena({ latencyMs: 40, jitterMs: 10, duplicateRate: 0.5 });
    await vi.advanceTimersByTimeAsync(2_000);
    m.fire(m.players.shooter, TARGET, 's1');
    await vi.advanceTimersByTimeAsync(1_500);
    const [a, b] = m.hosts;
    expect(b.world()[TARGET]!.hp).toBe(50);
    expect(b.combat().served.hit).toBe(1);
    expect(a.combat().forwarded.hit).toBe(1);
    expect(a.world()[SHOOTER]!.ammo).toBe(9);
    // S3's record: how old `seenB` was when B judged it.
    expect(b.combat().seenBAgeMs).toHaveLength(1);
    expect(b.combat().seenBAgeMs[0]).toBeGreaterThanOrEqual(DELAY_MS);
    m.close();
  });

  it('lands a cross-host hit on the default clock (no `now` passed anywhere)', async () => {
    // Every other test here passes `now: () => Date.now()`. A page passes
    // none: the recipe must then date hits on the same clock as the
    // netcode, which `seenB` (from `hostNow()`) is on.
    const m = arena({ latencyMs: 30, jitterMs: 10 }, combat.damage, 'default');
    await vi.advanceTimersByTimeAsync(2_000);
    m.fire(m.players.shooter, TARGET, 's1');
    await vi.advanceTimersByTimeAsync(1_000);
    const [a, b] = m.hosts;
    expect(b.combat().served).toEqual({ hit: 1 });
    expect(a.combat().forwarded).toEqual({ hit: 1 });
    expect(b.world()[TARGET]!.hp).toBe(50);
    // `seenB` was judged on B's own clock: its age is the view delay plus
    // the trip, not a difference between two clocks.
    expect(b.combat().seenBAgeMs[0]).toBeGreaterThanOrEqual(DELAY_MS);
    expect(b.combat().seenBAgeMs[0]).toBeLessThan(500);
    m.close();
  });

  it('refuses a seenB beyond the rewind cap, and that shot is a miss', async () => {
    const m = arena({ latencyMs: 20 });
    await vi.advanceTimersByTimeAsync(2_000);
    m.fire(m.players.shooter, TARGET, 's1', 1_000);
    await vi.advanceTimersByTimeAsync(1_000);
    const [a, b] = m.hosts;
    expect(b.combat().served['beyond-cap']).toBe(1);
    expect(a.combat().forwarded['refused:beyond-cap']).toBe(1);
    expect(b.world()[TARGET]!.hp).toBe(100);
    m.close();
  });

  it('refuses a poseA that does not match B’s history of A', async () => {
    const m = arena({ latencyMs: 20 });
    await vi.advanceTimersByTimeAsync(2_000);
    const [a, b] = m.hosts;
    const seenA = Date.now() - DELAY_MS;
    const claim = { shooter: SHOOTER, target: TARGET, poseA: { x: 15, z: 0, hp: 100, ammo: 9 }, seenA, seenB: Date.now() - DELAY_MS, ep: 1 };
    // The shooter was at x 0; a claim that it stood at 15 (in range of
    // nothing it could have reached) is not what B saw.
    const verdict = a.handoffs.forward('shard:s1', 'hit', claim).catch((error: unknown) => error);
    await vi.advanceTimersByTimeAsync(500);
    expect(await verdict).toBeInstanceOf(BorderActionError);
    expect(((await verdict) as BorderActionError).message).toBe('pose-mismatch');
    expect(b.world()[TARGET]!.hp).toBe(100);

    // Control: the same claim, made now, with the true pose is a hit.
    const seenNow = Date.now() - DELAY_MS;
    const honest = a.handoffs.forward('shard:s1', 'hit', {
      ...claim,
      seenA: seenNow,
      seenB: seenNow,
      poseA: a.netcode.rewind(seenNow).entities[SHOOTER],
    });
    await vi.advanceTimersByTimeAsync(500);
    expect(await honest).toEqual({ hit: true });
    m.close();
  });

  it('accepts a legitimate fast-moving shooter: the check uses history, not the current view', async () => {
    const m = arena({ latencyMs: 30, jitterMs: 10 });
    await vi.advanceTimersByTimeAsync(2_000);
    // 2 units an input at 20 Hz: 40 units/s, the cap. Run for a second,
    // firing mid-stride from inside range.
    for (let i = 0; i < 10; i += 1) {
      m.players.shooter.input({ dx: 2 });
      await vi.advanceTimersByTimeAsync(50);
    }
    m.fire(m.players.shooter, TARGET, 's1');
    for (let i = 0; i < 10; i += 1) {
      m.players.shooter.input({ dx: 2 });
      await vi.advanceTimersByTimeAsync(50);
    }
    await vi.advanceTimersByTimeAsync(1_000);
    const [a, b] = m.hosts;
    expect(b.combat().served.hit).toBe(1);
    expect(b.combat().served['pose-mismatch']).toBeUndefined();
    // By the time B judged it, the shooter had moved far past any
    // tolerance from where it fired: a check against the current view
    // would have refused this.
    expect(a.world()[SHOOTER]!.x).toBeGreaterThanOrEqual(38);
    m.close();
  });

  it('a gap in B’s history of A is no-history, refused and counted', async () => {
    const m = arena({ latencyMs: 20 });
    await vi.advanceTimersByTimeAsync(2_000);
    const [a, b] = m.hosts;
    // A's snapshots to B are lost for 400 ms: B's history has a hole.
    m.net.block(A, B, 400);
    await vi.advanceTimersByTimeAsync(200);
    const seenA = Date.now() - 50;
    await vi.advanceTimersByTimeAsync(400);
    const verdict = a.handoffs
      .forward('shard:s1', 'hit', {
        shooter: SHOOTER,
        target: TARGET,
        poseA: a.netcode.rewind(seenA).entities[SHOOTER],
        seenA,
        seenB: Date.now() - DELAY_MS,
        ep: 1,
      })
      .catch((error: unknown) => error);
    await vi.advanceTimersByTimeAsync(500);
    expect(((await verdict) as BorderActionError).message).toBe('no-history');
    expect(b.combat().served['no-history']).toBe(1);
    expect(b.world()[TARGET]!.hp).toBe(100);
    m.close();
  });

  it('a mutual kill across hosts is a trade: both stand', async () => {
    // One shot kills.
    const m = arena({ latencyMs: 30, jitterMs: 5 }, target => ({ ...target, hp: target.hp - 100 }));
    await vi.advanceTimersByTimeAsync(2_000);
    m.fire(m.players.shooter, TARGET, 's1');
    m.fire(m.players.target, SHOOTER, 's0');
    await vi.advanceTimersByTimeAsync(1_000);
    const [a, b] = m.hosts;
    expect(a.world()[SHOOTER]!.hp).toBeLessThanOrEqual(0);
    expect(b.world()[TARGET]!.hp).toBeLessThanOrEqual(0);
    expect(a.combat().served.hit).toBe(1);
    expect(b.combat().served.hit).toBe(1);
    m.close();
  });

  it('refuses a claim stamped with another epoch', async () => {
    const m = arena({ latencyMs: 20 });
    await vi.advanceTimersByTimeAsync(2_000);
    const [a, b] = m.hosts;
    const seenA = Date.now() - DELAY_MS;
    const verdict = a.handoffs
      .forward('shard:s1', 'hit', {
        shooter: SHOOTER,
        target: TARGET,
        poseA: a.netcode.rewind(seenA).entities[SHOOTER],
        seenA,
        seenB: seenA,
        ep: 0,
      })
      .catch((error: unknown) => error);
    await vi.advanceTimersByTimeAsync(500);
    expect(((await verdict) as BorderActionError).message).toBe('stale-epoch');
    expect(b.world()[TARGET]!.hp).toBe(100);
    m.close();
  });
});
