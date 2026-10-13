// NPCs across hosts (sharded-match plan, N1): owned per D7, AI run by the
// owner, hits in both directions on the cross-host path.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { shardHost } from '../../demo/sharded-arena/shard.js';
import type { ShardCombat, ShardHost } from '../../demo/sharded-arena/shard.js';
import {
  assignmentProblems,
  hostNetcode,
  joinNetcode,
  joinShards,
  npcShard,
  shardAssignment,
  withNpcs,
} from '../../src/netcode/index.js';
import type { MatchAssignment, ShardAssignment } from '../../src/netcode/index.js';
import { BorderActionError, handoffLink, regionHandoffs, regionState } from '../../src/world/index.js';
import { simNetwork } from './sim.js';

const lib = { hostNetcode, joinNetcode, handoffLink, regionHandoffs, regionState };
const LABEL = 'arena.move';
const A = '00000000000000a0';
const B = '00000000000000b0';
const P = '00000000000000e1';
const NPC = 'npc-1';

interface Body {
  readonly x: number;
  readonly z: number;
  readonly hp: number;
}
interface Act {
  readonly fire?: { readonly target: string; readonly seenB: number };
}
const combat: ShardCombat<Body> = {
  fire: shooter => (shooter.hp > 0 ? shooter : 'dead'),
  hits: (shooter, target) => Math.hypot(shooter.x - target.x, shooter.z - target.z) <= 30,
  damage: target => ({ ...target, hp: target.hp - 10 }),
  maxSpeed: 40,
};

/** NPC on host A, player on host B; the NPC takes one shot at P per `fireNpc()`. */
function arena(clock: 'date' | 'default' = 'date') {
  // 'default': no `now` anywhere, as a page runs (the netcode's clock).
  const now = clock === 'date' ? { now: () => Date.now() } : {};
  // Drops, duplicates and reorders (jitter wider than a tick's spacing of frames).
  const net = simNetwork({ latencyMs: 25, jitterMs: 20, lossRate: 0.1, duplicateRate: 0.3 });
  const assignment: ShardAssignment = { epoch: 1, hosts: { s0: A, s1: B }, owner: { [NPC]: 's0', [P]: 's1' } };
  let npcShots = 0;
  const host = (id: string, shard: string, initial: Record<string, Body>) =>
    shardHost<Body, Act>(lib, {
      transport: net.node(id),
      label: LABEL,
      assignment,
      shard,
      roster: [P],
      initial,
      apply: body => body,
      combat,
      // The NPC's AI: patrol, and take one shot at P when told to — at P
      // as this host sees it, through its spectator of B.
      advance: (entities, ai) => {
        const npc = entities[NPC];
        if (npc === undefined) return entities;
        if (npcShots > 0 && ai.known()[P] !== undefined) {
          npcShots -= 1;
          ai.fire(NPC, P);
        }
        return { ...entities, [NPC]: { ...npc, z: npc.z + 0.5 } };
      },
      ...now,
    });
  const hosts: [ShardHost<Body>, ShardHost<Body>] = [
    host(A, 's0', { [NPC]: { x: 10, z: 0, hp: 100 } }),
    host(B, 's1', { [P]: { x: 0, z: 0, hp: 100 } }),
  ];
  const player: ShardedMatch = joinShards<Body, Act>({
    transport: net.node(P),
    label: LABEL,
    assignment,
    local: { id: P, predict: body => body },
    ...now,
  });
  const close = () => {
    player.close();
    for (const h of hosts) h.close();
  };
  return { hosts, player, fireNpc: () => (npcShots += 1), close };
}
type ShardedMatch = ReturnType<typeof joinShards<Body, Act>>;

describe('NPCs across hosts', () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it.each(['date', 'default'] as const)('hits in both directions are applied exactly once under drop, duplication and reordering (%s clock)', async clock => {
    const m = arena(clock);
    const [a, b] = m.hosts;
    await vi.advanceTimersByTimeAsync(2_000);
    // Five shots each way, 200 ms apart: a race lost one time in two
    // (a claim landing before the history it is checked against) shows.
    for (let shot = 0; shot < 5; shot += 1) {
      // Player → NPC: forwarded from B to the NPC's owner, A.
      m.player.input({ fire: { target: NPC, seenB: m.player.hostNow('s0')! - 100 } });
      // NPC → player: the AI on A fires; B owns the target and decides.
      m.fireNpc();
      await vi.advanceTimersByTimeAsync(200);
    }
    await vi.advanceTimersByTimeAsync(2_000);
    expect(a.world()[NPC]!.hp).toBe(50);
    expect(b.world()[P]!.hp).toBe(50);
    expect(a.combat().served).toEqual({ hit: 5 });
    expect(b.combat().served).toEqual({ hit: 5 });
    m.close();
  });

  it('refuses an NPC→player hit whose pose is not what the target’s host saw of the NPC', async () => {
    const m = arena();
    const [a, b] = m.hosts;
    await vi.advanceTimersByTimeAsync(2_000);
    const seenA = Date.now() - 100;
    const truePose = a.netcode.rewind(seenA).entities[NPC]!;
    const claim = { shooter: NPC, target: P, seenA, seenB: Date.now() - 100, ep: 1 };
    const forged = a.handoffs.forward('shard:s1', 'hit', { ...claim, poseA: { ...truePose, x: truePose.x + 25 } });
    const verdict = forged.catch((error: unknown) => error);
    await vi.advanceTimersByTimeAsync(600);
    expect(((await verdict) as BorderActionError).message).toBe('pose-mismatch');
    expect(b.world()[P]!.hp).toBe(100);
    m.close();
  });
});

describe('NPC ownership (D7)', () => {
  const H0 = '00000000000000a0';
  const H1 = '00000000000000a1';
  const base: MatchAssignment = {
    epoch: 1,
    shards: { s0: { host: H0, squads: ['blue'] }, s1: { host: H1, squads: ['red'] } },
    squads: { red: { team: 'red', members: [H0] }, blue: { team: 'blue', members: [H1] } },
    leases: { s0: 0, s1: 0 },
    renewals: 0,
  };

  it('places each NPC on one shard by its id, the same everywhere', () => {
    const npcs = Array.from({ length: 40 }, (_, i) => `npc-${i}`);
    const placed = withNpcs(base, npcs);
    expect(assignmentProblems(placed)).toEqual([]);
    const owner = shardAssignment(placed).owner!;
    for (const npc of npcs) expect(owner[npc]).toBe(npcShard(npc, ['s1', 's0']));
    // Both shards get some.
    expect(placed.shards.s0!.npcs!.length).toBeGreaterThan(5);
    expect(placed.shards.s1!.npcs!.length).toBeGreaterThan(5);
  });

  it('refuses one NPC on two shards, and an NPC with a player’s id', () => {
    const twice = { ...base, shards: { s0: { ...base.shards.s0!, npcs: ['npc-1'] }, s1: { ...base.shards.s1!, npcs: ['npc-1'] } } };
    expect(assignmentProblems(twice)).toContain('npc npc-1 is owned by two shards: s0 and s1');
    const impostor = { ...base, shards: { ...base.shards, s0: { ...base.shards.s0!, npcs: [H1] } } };
    expect(assignmentProblems(impostor)).toContain(`npc ${H1} has a player's id`);
  });
});
