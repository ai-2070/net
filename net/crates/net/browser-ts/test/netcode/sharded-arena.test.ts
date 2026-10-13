// The shard host recipe (sharded-match plan, S2): 4 hosts × 32 players on
// the local mesh, and each player's merged view against the union of the
// hosts' authoritative states.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { shardHost } from '../../demo/sharded-arena/shard.js';
import type { ShardHost } from '../../demo/sharded-arena/shard.js';
import { createLocalMesh } from '../../src/local.js';
import { hostNetcode, joinNetcode, joinShards } from '../../src/netcode/index.js';
import type { ShardAssignment, ShardedMatch } from '../../src/netcode/index.js';
import { handoffLink, regionHandoffs, regionState } from '../../src/world/index.js';

const lib = { hostNetcode, joinNetcode, handoffLink, regionHandoffs, regionState };
const LABEL = 'arena.move';
const TICK_RATE = 20;
const TICK_MS = 1000 / TICK_RATE;
const DELAY_MS = 100;

interface Body {
  readonly x: number;
  readonly z: number;
}
interface Move {
  readonly dx: number;
}
const apply = (body: Body, input: Move): Body => ({ ...body, x: body.x + input.dx });

const hex = (prefix: number, i: number) => (prefix * 0x100 + i).toString(16).padStart(16, '0');

/** A match: `hostCount` shard hosts, `playerCount` players owned round-robin, `npcs` per shard. */
function match(hostCount: number, playerCount: number, npcs: number, fogRadius?: number) {
  const mesh = createLocalMesh();
  const shardNames = Array.from({ length: hostCount }, (_, k) => `s${k}`);
  const hostIds = shardNames.map((_, k) => hex(0xa, k));
  const playerIds = Array.from({ length: playerCount }, (_, i) => hex(0xb, i));
  const owner: Record<string, string> = {};
  const initial: Record<string, Record<string, Body>> = Object.fromEntries(shardNames.map(name => [name, {}]));
  playerIds.forEach((id, i) => {
    const name = shardNames[i % hostCount]!;
    owner[id] = name;
    initial[name]![id] = { x: 0, z: i * 10 };
  });
  shardNames.forEach((name, k) => {
    for (let n = 0; n < npcs; n += 1) {
      const id = `npc-${name}-${n}`;
      owner[id] = name;
      initial[name]![id] = { x: 0, z: 1000 + k * 100 + n };
    }
  });
  const assignment: ShardAssignment = {
    epoch: 1,
    hosts: Object.fromEntries(shardNames.map((name, k) => [name, hostIds[k]!])),
    owner,
  };
  const hosts: ShardHost<Body>[] = shardNames.map((shard, k) =>
    shardHost<Body, Move>(lib, {
      transport: mesh.node(hostIds[k]),
      label: LABEL,
      assignment,
      shard,
      roster: playerIds,
      initial: initial[shard]!,
      apply,
      // NPCs drift half a unit a tick: the view has to follow motion, not
      // just a world that stopped.
      advance: entities => {
        const next: Record<string, Body> = {};
        for (const [id, body] of Object.entries(entities)) next[id] = id.startsWith('npc-') ? { ...body, x: body.x + 0.5 } : body;
        return next;
      },
      tickRate: TICK_RATE,
      ...(fogRadius === undefined ? {} : { fogRadius }),
      now: () => Date.now(),
    }),
  );
  const playerNodes = playerIds.map(id => mesh.node(id));
  const players: ShardedMatch<Body, Move>[] = playerIds.map((id, i) =>
    joinShards<Body, Move>({
      transport: playerNodes[i]!,
      label: LABEL,
      assignment,
      local: { id, predict: apply },
      interpolationDelayMs: DELAY_MS,
      now: () => Date.now(),
    }),
  );
  const union = () => Object.assign({}, ...hosts.map(host => host.world())) as Record<string, Body>;
  const close = () => {
    for (const player of players) player.close();
    for (const host of hosts) host.close();
    mesh.close();
  };
  return { mesh, hosts, players, playerIds, playerNodes, union, close };
}

describe('the shard host recipe', () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it('4 hosts × 32 players: every merged view is the union of the hosts, within one tick plus the interpolation delay', async () => {
    const m = match(4, 32, 4);
    await vi.advanceTimersByTimeAsync(1_000);
    // Every player moves one unit per input, 20 inputs a second.
    const steering = setInterval(() => {
      for (const player of m.players) player.input({ dx: 1 });
    }, TICK_MS);
    await vi.advanceTimersByTimeAsync(2_000);

    const union = m.union();
    expect(Object.keys(union)).toHaveLength(32 + 16);
    // The fastest thing in the world moves 20 units a second: how far a
    // state may trail the host's after one tick plus the interpolation delay.
    const lag = (20 * (TICK_MS + DELAY_MS)) / 1000;
    m.players.forEach((player, i) => {
      const view = player.view();
      expect(Object.keys(view).sort()).toEqual(Object.keys(union).sort());
      for (const [id, body] of Object.entries(view)) {
        const truth = union[id]!;
        expect(body.z).toBe(truth.z);
        if (id === m.playerIds[i]) {
          // Your own entity is predicted: ahead of the host by the inputs
          // still in flight, never behind it.
          expect(body.x - truth.x).toBeGreaterThanOrEqual(0);
          expect(body.x - truth.x).toBeLessThanOrEqual(player.stats().shards[ownerOf(i)]!.pendingInputs);
        } else {
          expect(truth.x - body.x).toBeGreaterThanOrEqual(0);
          expect(truth.x - body.x).toBeLessThanOrEqual(lag);
        }
      }
      expect(player.stats().droppedNotOwner).toBe(0);
      expect(player.stats().inputsUnowned).toBe(0);
    });
    // Each host moved only its own players: 8 each, 40 inputs each.
    for (const host of m.hosts) {
      const own = Object.entries(host.world()).filter(([id]) => !id.startsWith('npc-'));
      expect(own).toHaveLength(8);
      for (const [, body] of own) expect(body.x).toBeGreaterThanOrEqual(38);
    }
    clearInterval(steering);
    m.close();
  });

  it('fogs players by where their entity is, on whichever host owns it; hosts see everything', async () => {
    // Two hosts; player 0 (on s0) at z 0, player 1 (on s1) at z 10. A fog
    // of 5 shows each player only itself and its own host's nearby NPCs.
    const m = match(2, 2, 1, 5);
    await vi.advanceTimersByTimeAsync(2_000);
    const [p0, p1] = m.players;
    expect(Object.keys(p0!.view()).sort()).toEqual([m.playerIds[0]]);
    expect(Object.keys(p1!.view()).sort()).toEqual([m.playerIds[1]]);
    // A host spectates the other in full: fog is for players only.
    for (const host of m.hosts) expect(Object.keys(host.known()).sort()).toEqual(Object.keys(m.union()).sort());
    m.close();
  });

  it('a host applies no input to an entity it does not own, and admits no one off the roster', async () => {
    const m = match(2, 2, 0);
    await vi.advanceTimersByTimeAsync(1_000);
    const before = m.hosts[0]!.world()[m.playerIds[0]!];
    // Player 0 belongs to s0, and sends inputs straight to s1 anyway.
    const sideways = joinNetcode<Body, Move>({
      transport: m.playerNodes[0]!,
      host: hex(0xa, 1),
      label: LABEL,
      now: () => Date.now(),
    });
    // And a node that is not on the roster at all joins s1.
    const outsider = joinNetcode<Body, Move>({ transport: m.mesh.node(hex(0xc, 0)), host: hex(0xa, 1), label: LABEL, now: () => Date.now() });
    await vi.advanceTimersByTimeAsync(500);
    for (let i = 0; i < 10; i += 1) {
      sideways.input({ dx: 1 });
      await vi.advanceTimersByTimeAsync(TICK_MS);
    }
    await vi.advanceTimersByTimeAsync(500);
    expect(m.hosts[1]!.world()[m.playerIds[0]!]).toBeUndefined();
    expect(m.hosts[0]!.world()[m.playerIds[0]!]).toEqual(before);
    expect(m.hosts[1]!.netcode.dropped.unauthorized).toBeGreaterThan(0);
    sideways.close();
    outsider.close();
    m.close();
  });
});

/** Shard of player `i` in a 4-host match (round-robin). */
const ownerOf = (i: number) => `s${i % 4}`;

