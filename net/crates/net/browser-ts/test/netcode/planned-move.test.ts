// Planned moves (sharded-match plan, S5): a player's entity and its
// processed-input boundary move between hosts together, and the player's
// input sequence carries on across the move.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { shardHost } from '../../demo/sharded-arena/shard.js';
import type { ShardHost } from '../../demo/sharded-arena/shard.js';
import { hostNetcode, joinNetcode, joinShards } from '../../src/netcode/index.js';
import type { ShardAssignment, ShardedMatch } from '../../src/netcode/index.js';
import { handoffLink, regionHandoffs, regionState } from '../../src/world/index.js';
import type { HandoffEvent } from '../../src/world/index.js';
import { simNetwork, type SimOptions } from './sim.js';

const lib = { hostNetcode, joinNetcode, handoffLink, regionHandoffs, regionState };
const LABEL = 'arena.move';
const A = '00000000000000a0';
const B = '00000000000000b0';
const P = '00000000000000e1';
const WATCHER = '00000000000000e9';

interface Body {
  readonly x: number;
  readonly z: number;
}
interface Step {
  readonly dx: number;
}
const apply = (body: Body, input: Step): Body => ({ ...body, x: body.x + input.dx });

/** Two hosts and one player owned by s0; a stand-in lobby that raises the epoch when a move reports `moved`. */
function arena(sim: SimOptions, lobbyDelayMs = 0) {
  const net = simNetwork(sim);
  let assignment: ShardAssignment = { epoch: 1, hosts: { s0: A, s1: B }, owner: { [P]: 's0' } };
  const events: HandoffEvent[] = [];
  const hosts: ShardHost<Body>[] = [];
  let match: ShardedMatch<Body, Step> | null = null;
  const onHandoffEvent = (event: HandoffEvent) => {
    events.push(event);
    if (event.type !== 'moved') return;
    // The lobby node: the move is done, so the owner changes at a new epoch
    // — reaching everyone `lobbyDelayMs` later, as a store write would.
    setTimeout(() => {
      assignment = { ...assignment, epoch: assignment.epoch + 1, owner: { ...assignment.owner, [P]: 's1' } };
      for (const host of hosts) host.setAssignment(assignment);
      match?.setAssignment(assignment);
    }, lobbyDelayMs);
  };
  const host = (id: string, shard: string, initial: Record<string, Body>) =>
    shardHost<Body, Step>(lib, {
      transport: net.node(id),
      label: LABEL,
      assignment,
      shard,
      roster: [P, WATCHER],
      initial,
      apply,
      onHandoffEvent,
      now: () => Date.now(),
    });
  hosts.push(host(A, 's0', { [P]: { x: 0, z: 0 } }), host(B, 's1', {}));
  match = joinShards<Body, Step>({
    transport: net.node(P),
    label: LABEL,
    assignment,
    local: { id: P, predict: apply },
    now: () => Date.now(),
  });
  // Another player, watching P move.
  let watcher: ShardedMatch<Body, Step> | null = null;
  const watch = () => {
    watcher = joinShards<Body, Step>({ transport: net.node(WATCHER), label: LABEL, assignment, now: () => Date.now() });
    const install = match!.setAssignment.bind(match);
    match!.setAssignment = next => {
      watcher!.setAssignment(next);
      return install(next);
    };
    return watcher;
  };
  const close = () => {
    watcher?.close();
    match!.close();
    for (const h of hosts) h.close();
  };
  return { net, hosts, match, events, close, watch, epoch: () => assignment.epoch };
}

/** Where the player's entity is live, by shard: never more than one. */
const liveOn = (hosts: readonly ShardHost<Body>[]) => hosts.filter(host => host.world()[P] !== undefined).map(host => host.shard);

describe('planned moves', () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it('input 41: applied by the old host, its acknowledgement lost, re-sent to the new host — not applied again', async () => {
    const m = arena({ latencyMs: 20 });
    const [a, b] = m.hosts as [ShardHost<Body>, ShardHost<Body>];
    await vi.advanceTimersByTimeAsync(1_500);
    for (let i = 0; i < 40; i += 1) {
      m.match.input({ dx: 1 });
      await vi.advanceTimersByTimeAsync(50);
    }
    await vi.advanceTimersByTimeAsync(300);
    expect(a.world()[P]).toEqual({ x: 40, z: 0 });
    expect(m.match.stats().shards.s0!.pendingInputs).toBe(0);

    // Everything the old host sends the player is lost from here: the
    // snapshot that would have acknowledged 41 never arrives.
    m.net.block(A, P, 2_000);
    m.match.input({ dx: 1 });
    await vi.advanceTimersByTimeAsync(100);
    expect(a.world()[P]).toEqual({ x: 41, z: 0 }); // 41 applied
    expect(m.match.stats().shards.s0!.pendingInputs).toBe(1); // and never acknowledged

    await a.movePlayer(P, 's1');
    await vi.advanceTimersByTimeAsync(1_000);
    expect(m.events.map(event => event.type)).toContain('moved');
    expect(m.epoch()).toBe(2);
    // The player re-sent 41 to its new owner, which already holds 41's
    // effect in the state it was handed: x stays 41.
    expect(b.world()[P]).toEqual({ x: 41, z: 0 });
    expect(liveOn(m.hosts)).toEqual(['s1']);
    expect(m.match.stats().shards.s1!.pendingInputs).toBe(0);

    // And the sequence carries on: 42 is applied, once, by the new owner.
    m.match.input({ dx: 1 });
    await vi.advanceTimersByTimeAsync(300);
    expect(b.world()[P]).toEqual({ x: 42, z: 0 });
    m.close();
  });

  // The new epoch lands `lobbyDelayMs` after the move: the player keeps
  // sending to the old host meanwhile, which must neither apply nor
  // acknowledge. 800 ms holds 16 inputs — twice what one input frame repeats.
  it.each([200, 800])('moves a player that keeps moving (epoch %i ms behind): every input applied exactly once, never on two hosts, few corrections', async lobbyDelayMs => {
    const m = arena({ latencyMs: 25, jitterMs: 10, lossRate: 0.1 }, lobbyDelayMs);
    const [a, b] = m.hosts as [ShardHost<Body>, ShardHost<Body>];
    const watcher = m.watch();
    await vi.advanceTimersByTimeAsync(1_500);
    let sent = 0;
    let mostLive = 0;
    let unseen = 0;
    const steer = setInterval(() => {
      m.match.input({ dx: 1 });
      sent += 1;
    }, 50);
    const watch = setInterval(() => {
      mostLive = Math.max(mostLive, liveOn(m.hosts).length);
      if (watcher.view()[P] === undefined) unseen += 1;
    }, 5);
    await vi.advanceTimersByTimeAsync(1_000);
    void a.movePlayer(P, 's1');
    await vi.advanceTimersByTimeAsync(1_500);
    clearInterval(steer);
    await vi.advanceTimersByTimeAsync(1_000);
    clearInterval(watch);

    expect(m.epoch()).toBe(2);
    expect(liveOn(m.hosts)).toEqual(['s1']);
    // Never authoritative on two shards at once (it is on none while the
    // handoff is in flight).
    expect(mostLive).toBe(1);
    // And another player never loses sight of it while it moves.
    expect(unseen).toBe(0);
    // Exactly once: one unit of x per input sent, across both owners.
    expect(b.world()[P]!.x).toBe(sent);
    // The bound: the move costs the prediction at most one correction. The
    // new owner's client is the only one predicting after the move, and it
    // was joined for it, so its count is exactly the move's.
    expect(m.match.stats().shards.s1!.corrections).toBeLessThanOrEqual(1);
    m.close();
  });
});
