// joinShards (sharded-match plan, S1): one merged view over several hosts,
// drawn only from each entity's owner at the installed epoch.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import {
  decodeBinarySnapshot,
  decodeFrame,
  encodeBinarySnapshot,
  entityCodec,
  hostNetcode,
  joinShards,
  uint,
} from '../../src/netcode/index.js';
import type { HostNetcode, ShardAssignment } from '../../src/netcode/index.js';
import { simNetwork } from './sim.js';

const LABEL = 'arena.move';
const HOSTS = { a: '00000000000000a0', b: '00000000000000b0', c: '00000000000000c0' } as const;
const ME = '00000000000000e1';

interface Ship {
  readonly x: number;
}
interface Move {
  readonly dx: number;
}
const move = (ship: Ship, input: Move): Ship => ({ x: ship.x + input.dx });

type Net = ReturnType<typeof simNetwork>;

/** A shard host serving `world()` at the epoch `epoch()`, counting the inputs it applies. */
function shardHost(net: Net, id: string, world: () => Record<string, Ship>, epoch: () => number) {
  const applied: Move[] = [];
  const host: HostNetcode<Ship> = hostNetcode<Ship, Move>({
    transport: net.node(id),
    label: LABEL,
    tickRate: 20,
    now: () => Date.now(),
    epoch,
    step: ({ inputs }) => {
      for (const list of inputs.values()) for (const { data } of list) applied.push(data);
    },
    snapshot: world,
  });
  return { host, applied };
}

const assignment = (epoch: number, owner: Record<string, string>): ShardAssignment => ({ epoch, hosts: HOSTS, owner });

describe('joinShards', () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it('merges three hosts with disjoint entities into one view', async () => {
    const net = simNetwork({ latencyMs: 20, lossRate: 0.2, jitterMs: 5 });
    const hosts = [
      shardHost(net, HOSTS.a, () => ({ a1: { x: 1 }, a2: { x: 2 } }), () => 1),
      shardHost(net, HOSTS.b, () => ({ b1: { x: 10 } }), () => 1),
      shardHost(net, HOSTS.c, () => ({ c1: { x: 100 } }), () => 1),
    ];
    const match = joinShards<Ship, Move>({
      transport: net.node(ME),
      label: LABEL,
      now: () => Date.now(),
      assignment: assignment(1, { a1: 'a', a2: 'a', b1: 'b', c1: 'c' }),
    });
    const ready = match.ready();
    await vi.advanceTimersByTimeAsync(2_000);
    await ready;
    expect(match.view()).toEqual({ a1: { x: 1 }, a2: { x: 2 }, b1: { x: 10 }, c1: { x: 100 } });
    expect(match.stats().droppedNotOwner).toBe(0);
    match.close();
    for (const { host } of hosts) host.close();
  });

  it('never draws an entity from a shard that does not own it', async () => {
    const net = simNetwork({ latencyMs: 20 });
    // b claims a1 too, with a different state; a owns it.
    const hosts = [
      shardHost(net, HOSTS.a, () => ({ a1: { x: 1 } }), () => 1),
      shardHost(net, HOSTS.b, () => ({ a1: { x: 666 }, b1: { x: 10 } }), () => 1),
    ];
    const match = joinShards<Ship, Move>({
      transport: net.node(ME),
      label: LABEL,
      now: () => Date.now(),
      assignment: { epoch: 1, hosts: { a: HOSTS.a, b: HOSTS.b }, owner: { a1: 'a', b1: 'b' } },
    });
    await vi.advanceTimersByTimeAsync(2_000);
    for (let i = 0; i < 10; i += 1) {
      expect(match.view().a1).toEqual({ x: 1 });
      await vi.advanceTimersByTimeAsync(50);
    }
    expect(match.view().b1).toEqual({ x: 10 });
    expect(match.stats().droppedNotOwner).toBeGreaterThan(0);

    // And an entity nobody owns is drawn from nobody.
    const orphan = joinShards<Ship, Move>({
      transport: net.node('00000000000000e2'),
      label: LABEL,
      now: () => Date.now(),
      assignment: { epoch: 1, hosts: { a: HOSTS.a, b: HOSTS.b }, owner: { b1: 'b' } },
    });
    await vi.advanceTimersByTimeAsync(2_000);
    expect(orphan.view()).toEqual({ b1: { x: 10 } });
    orphan.close();
    match.close();
    for (const { host } of hosts) host.close();
  });

  it('sends inputs to the owner only, stamped with the epoch, and predicts locally', async () => {
    const net = simNetwork({ latencyMs: 20 });
    let mine: Ship = { x: 0 };
    const a = shardHost(net, HOSTS.a, () => ({ [ME]: mine }), () => 1);
    const b = shardHost(net, HOSTS.b, () => ({ b1: { x: 10 } }), () => 1);
    // a applies my inputs to my ship; b must never see them.
    const step = a.applied;
    const match = joinShards<Ship, Move>({
      transport: net.node(ME),
      label: LABEL,
      now: () => Date.now(),
      assignment: { epoch: 1, hosts: { a: HOSTS.a, b: HOSTS.b }, owner: { [ME]: 'a', b1: 'b' } },
      local: { id: ME, predict: move },
    });
    await vi.advanceTimersByTimeAsync(1_000);
    for (let i = 0; i < 5; i += 1) {
      match.input({ dx: 1 });
      await vi.advanceTimersByTimeAsync(50);
    }
    mine = { x: 5 };
    await vi.advanceTimersByTimeAsync(1_000);
    expect(step).toHaveLength(5);
    expect(b.applied).toHaveLength(0);
    expect(match.view()[ME]).toEqual({ x: 5 });
    expect(match.stats().inputsUnowned).toBe(0);
    match.close();
    a.host.close();
    b.host.close();
  });

  it('a host drops an input frame stamped with another epoch', async () => {
    const net = simNetwork({ latencyMs: 20 });
    let hostEpoch = 1;
    const a = shardHost(net, HOSTS.a, () => ({ [ME]: { x: 0 } }), () => hostEpoch);
    const match = joinShards<Ship, Move>({
      transport: net.node(ME),
      label: LABEL,
      now: () => Date.now(),
      assignment: { epoch: 1, hosts: { a: HOSTS.a }, owner: { [ME]: 'a' } },
      local: { id: ME, predict: move },
    });
    await vi.advanceTimersByTimeAsync(1_000);
    hostEpoch = 2;
    match.input({ dx: 1 });
    await vi.advanceTimersByTimeAsync(500);
    expect(a.applied).toHaveLength(0);
    expect(a.host.dropped['stale-epoch']).toBeGreaterThan(0);
    match.close();
    a.host.close();
  });

  it('freezes a shard at another epoch, and follows setAssignment', async () => {
    const net = simNetwork({ latencyMs: 20 });
    let epochA = 1;
    let xA = 1;
    const a = shardHost(net, HOSTS.a, () => ({ a1: { x: xA }, moving: { x: 7 } }), () => epochA);
    const b = shardHost(net, HOSTS.b, () => ({ b1: { x: 10 }, moving: { x: 70 } }), () => 1);
    const match = joinShards<Ship, Move>({
      transport: net.node(ME),
      label: LABEL,
      now: () => Date.now(),
      lingerMs: 1_000,
      assignment: { epoch: 1, hosts: { a: HOSTS.a, b: HOSTS.b }, owner: { a1: 'a', b1: 'b', moving: 'a' } },
    });
    await vi.advanceTimersByTimeAsync(1_000);
    expect(match.view()).toEqual({ a1: { x: 1 }, b1: { x: 10 }, moving: { x: 7 } });

    // a moves to epoch 2 ahead of this player: held at its last state.
    epochA = 2;
    xA = 2;
    await vi.advanceTimersByTimeAsync(1_000);
    expect(match.view().a1).toEqual({ x: 1 });
    expect(match.stats().heldNewerEpoch).toBeGreaterThan(0);

    // The player installs epoch 2, which also hands `moving` to b.
    expect(match.setAssignment({ epoch: 2, hosts: { a: HOSTS.a, b: HOSTS.b }, owner: { a1: 'a', b1: 'b', moving: 'b' } })).toBe(true);
    // An older or equal epoch never replaces it.
    expect(match.setAssignment({ epoch: 2, hosts: { a: HOSTS.a, b: HOSTS.b }, owner: {} })).toBe(false);
    // b is still at epoch 1: frozen, and showing nothing it does not own.
    await vi.advanceTimersByTimeAsync(300);
    expect(match.view().a1).toEqual({ x: 2 });
    expect(match.stats().droppedStaleEpoch).toBeGreaterThan(0);
    // `moving` lingers at a's last state (b's newer copy is from epoch 1).
    expect(match.view().moving).toEqual({ x: 7 });
    await vi.advanceTimersByTimeAsync(1_000);
    // The linger expired and b has not reached epoch 2: not drawn.
    expect(match.view().moving).toBeUndefined();
    expect(match.view().b1).toEqual({ x: 10 });
    match.close();
    a.host.close();
    b.host.close();
  });

  it('a rejoined shard keeps showing everything else it owns until the new connection delivers (no blink on a move)', async () => {
    const net = simNetwork({ latencyMs: 40 });
    let epoch = 1;
    let mine: Ship = { x: 0 };
    const owner1 = { [ME]: 'a', a1: 'a', a2: 'a', b1: 'b', b2: 'b' };
    const owner2 = { ...owner1, [ME]: 'b' };
    const a = shardHost(net, HOSTS.a, (): Record<string, Ship> => (epoch === 1 ? { [ME]: mine, a1: { x: 1 }, a2: { x: 2 } } : { a1: { x: 1 }, a2: { x: 2 } }), () => epoch);
    const b = shardHost(net, HOSTS.b, (): Record<string, Ship> => (epoch === 1 ? { b1: { x: 10 }, b2: { x: 20 } } : { [ME]: mine, b1: { x: 10 }, b2: { x: 20 } }), () => epoch);
    const match = joinShards<Ship, Move>({
      transport: net.node(ME),
      label: LABEL,
      now: () => Date.now(),
      local: { id: ME, predict: move },
      assignment: { epoch: 1, hosts: { a: HOSTS.a, b: HOSTS.b }, owner: owner1 },
    });
    await vi.advanceTimersByTimeAsync(1_500);
    expect(Object.keys(match.view()).sort()).toEqual([ME, 'a1', 'a2', 'b1', 'b2'].sort());
    // A planned move: this player's owner goes a → b, so BOTH shards are
    // rejoined. Every other entity on them must stay on screen meanwhile.
    mine = { x: 5 };
    epoch = 2;
    expect(match.setAssignment({ epoch: 2, hosts: { a: HOSTS.a, b: HOSTS.b }, owner: owner2 })).toBe(true);
    let missing = 0;
    for (let t = 0; t < 1_000; t += 10) {
      const view = match.view();
      for (const id of [ME, 'a1', 'a2', 'b1', 'b2']) if (view[id] === undefined) missing += 1;
      await vi.advanceTimersByTimeAsync(10);
    }
    expect(missing).toBe(0);
    expect(match.view()).toMatchObject({ [ME]: { x: 5 }, a1: { x: 1 }, b2: { x: 20 } });
    match.close();
    a.host.close();
    b.host.close();
  });

  it('ready() waits on the connections that are live, across a rejoin', async () => {
    const net = simNetwork({ latencyMs: 20 });
    const a = shardHost(net, HOSTS.a, () => ({ a1: { x: 1 } }), () => 1);
    const b = shardHost(net, HOSTS.b, () => ({ b1: { x: 10 } }), () => 1);
    const c = shardHost(net, HOSTS.c, () => ({ b1: { x: 10 } }), () => 1);
    const match = joinShards<Ship, Move>({
      transport: net.node(ME),
      label: LABEL,
      now: () => Date.now(),
      assignment: { epoch: 1, hosts: { a: HOSTS.a, b: HOSTS.b }, owner: { a1: 'a', b1: 'b' } },
    });
    let outcome: string | null = null;
    match.ready().then(
      () => (outcome = 'ready'),
      (error: unknown) => (outcome = `failed: ${String(error)}`),
    );
    // Shard b moves to host c before anything arrived: its first connection
    // is closed. That is not a join failure, and the wait follows c.
    match.setAssignment({ epoch: 2, hosts: { a: HOSTS.a, b: HOSTS.c }, owner: { a1: 'a', b1: 'b' } });
    await vi.advanceTimersByTimeAsync(1_000);
    expect(outcome).toBe('ready');
    expect(match.stats().shards.b!.host).toBe(HOSTS.c);
    expect(match.stats().shards.b!.snapshots).toBeGreaterThan(0);
    match.close();
    for (const { host } of [a, b, c]) host.close();
  });

  it('a silent host shows in stats', async () => {
    const net = simNetwork({ latencyMs: 20 });
    const a = shardHost(net, HOSTS.a, () => ({ a1: { x: 1 } }), () => 1);
    const b = shardHost(net, HOSTS.b, () => ({ b1: { x: 10 } }), () => 1);
    const match = joinShards<Ship, Move>({
      transport: net.node(ME),
      label: LABEL,
      now: () => Date.now(),
      assignment: { epoch: 1, hosts: { a: HOSTS.a, b: HOSTS.b }, owner: { a1: 'a', b1: 'b' } },
    });
    await vi.advanceTimersByTimeAsync(1_000);
    b.host.close();
    await vi.advanceTimersByTimeAsync(3_000);
    const { shards } = match.stats();
    expect(shards.a!.silentMs).toBeLessThan(200);
    expect(shards.b!.silentMs).toBeGreaterThan(2_500);
    match.close();
    a.host.close();
  });
});

describe('the epoch on the wire', () => {
  const codec = entityCodec<Ship>({ x: uint(8) });

  it('round-trips in a binary snapshot, under a kind of its own', () => {
    const frame = { tick: 7, t: 1.5, ack: 3, ep: 300 };
    const bytes = encodeBinarySnapshot(LABEL, frame, new Map(), new Map([[0, { x: 9 }]]), codec);
    expect(bytes[5]).toBe(2);
    expect(decodeBinarySnapshot(bytes, LABEL, codec)).toMatchObject({ tick: 7, ack: 3, ep: 300 });
    const plain = encodeBinarySnapshot(LABEL, { tick: 7, t: 1.5, ack: 3 }, new Map(), new Map([[0, { x: 9 }]]), codec);
    expect(plain[5]).toBe(1);
    expect(decodeBinarySnapshot(plain, LABEL, codec)?.ep).toBeUndefined();
  });

  it('refuses a JSON frame whose epoch is not a non-negative integer', () => {
    const encode = (value: unknown) => new TextEncoder().encode(JSON.stringify(value));
    expect(decodeFrame(encode({ n: LABEL, k: 'i', i: [], ep: 2 }), LABEL)).not.toBeNull();
    expect(decodeFrame(encode({ n: LABEL, k: 'i', i: [], ep: -1 }), LABEL)).toBeNull();
    expect(decodeFrame(encode({ n: LABEL, k: 's', tick: 1, t: 1, ack: 0, e: {}, ep: 1.5 }), LABEL)).toBeNull();
    expect(decodeFrame(encode({ n: LABEL, k: 's', tick: 1, t: 1, ack: 0, e: {}, ep: 4 }), LABEL)).not.toBeNull();
  });
});
