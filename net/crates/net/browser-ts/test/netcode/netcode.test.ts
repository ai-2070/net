// Netcode model 2 over a simulated lossy, laggy network: clock sync,
// snapshot interpolation, prediction + reconciliation, exactly-once input
// application, and capped lag compensation.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { ClockEstimator, SnapshotBuffer, hostNetcode, joinNetcode, lerpNumbers } from '../../src/netcode/index.js';
import type { TimedSnapshot } from '../../src/netcode/index.js';
import { simNetwork } from './sim.js';

const HOST = '00000000000000aa';
const ALICE = '00000000000000a1';
const BOB = '00000000000000b1';

interface Ship {
  readonly x: number;
  readonly y: number;
}
interface Move {
  readonly dx: number;
}
const move = (ship: Ship, input: Move): Ship => ({ ...ship, x: ship.x + input.dx });

describe('ClockEstimator', () => {
  it('recovers the offset exactly from symmetric paths', () => {
    const clock = new ClockEstimator();
    // Host is 5000 ms ahead; 40 ms each way; host holds the ping 2 ms.
    for (let i = 0; i < 5; i += 1) {
      const t0 = 1000 + i * 250;
      clock.add(t0, t0 + 40 + 5000, t0 + 42 + 5000, t0 + 82);
    }
    const estimate = clock.estimate()!;
    expect(estimate.offsetMs).toBeCloseTo(5000, 6);
    expect(estimate.rttMs).toBeCloseTo(80, 6);
    expect(estimate.jitterMs).toBeCloseTo(0, 6);
  });

  it('trusts the lowest-RTT samples, so queueing on one path does not skew the offset', () => {
    const clock = new ClockEstimator();
    // Four clean samples (40 ms each way) and eight where the return path
    // queued 100 ms extra — those alone would put the offset 50 ms off.
    for (let i = 0; i < 12; i += 1) {
      const t0 = i * 250;
      const back = i < 4 ? 40 : 140;
      clock.add(t0, t0 + 40 + 5000, t0 + 40 + 5000, t0 + 40 + back);
    }
    const estimate = clock.estimate()!;
    expect(estimate.offsetMs).toBeCloseTo(5000, 6);
    expect(estimate.jitterMs).toBeGreaterThan(40);
  });

  it('ignores a sample with a negative round trip', () => {
    const clock = new ClockEstimator();
    expect(clock.add(100, 0, 500, 101)).toBe(false);
    expect(clock.estimate()).toBeNull();
  });
});

describe('SnapshotBuffer', () => {
  const snap = (tick: number, time: number, x: number | null): TimedSnapshot<Ship> => ({
    tick,
    time,
    entities: x === null ? {} : { s: { x, y: 0 } },
  });

  it('interpolates between the snapshots around the render time, in order even when they arrive out of order', () => {
    const buffer = new SnapshotBuffer<Ship>();
    buffer.add(snap(2, 200, 20));
    buffer.add(snap(0, 0, 0));
    buffer.add(snap(1, 100, 10));
    expect(buffer.late).toBe(2);
    expect(buffer.at(150, lerpNumbers).s).toEqual({ x: 15, y: 0 });
    // Before the oldest: the oldest.
    expect(buffer.at(-50, lerpNumbers).s).toEqual({ x: 0, y: 0 });
    // Past the newest: held, not extrapolated.
    expect(buffer.at(999, lerpNumbers).s).toEqual({ x: 20, y: 0 });
  });

  it('shows an entity appearing as it is, and drops one that is gone from the later snapshot', () => {
    const buffer = new SnapshotBuffer<Ship>();
    buffer.add(snap(0, 0, null));
    buffer.add(snap(1, 100, 10));
    expect(buffer.at(50, lerpNumbers).s).toEqual({ x: 10, y: 0 });
    buffer.add(snap(2, 200, null));
    expect(buffer.at(150, lerpNumbers).s).toBeUndefined();
  });
});

describe('netcode over a lossy, laggy network', () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  /** A world of ships moved by inputs, hosted at 30 Hz. */
  function game(net: ReturnType<typeof simNetwork>, hostClockAhead = 0) {
    const ships = new Map<string, Ship>();
    const applied = new Map<string, number[]>();
    const host = hostNetcode<Ship, Move>({
      transport: net.node(HOST),
      label: 'test.movement',
      tickRate: 30,
      now: () => Date.now() + hostClockAhead,
      step: ({ inputs }) => {
        for (const [peer, list] of inputs) {
          for (const input of list) {
            ships.set(peer, move(ships.get(peer) ?? { x: 0, y: 0 }, input.data));
            applied.set(peer, [...(applied.get(peer) ?? []), input.seq]);
          }
        }
      },
      snapshot: () => Object.fromEntries(ships),
    });
    return { host, ships, applied };
  }

  function player(net: ReturnType<typeof simNetwork>, id: string) {
    return joinNetcode<Ship, Move>({
      transport: net.node(id),
      host: HOST,
      label: 'test.movement',
      now: () => Date.now(),
      local: { id, predict: move },
    });
  }

  it("syncs the player's clock to the host's, through latency and loss", async () => {
    const net = simNetwork({ latencyMs: 60, lossRate: 0.1, jitterMs: 10 });
    const { host } = game(net, 5_000);
    const alice = player(net, ALICE);
    await vi.advanceTimersByTimeAsync(4_000);
    const clock = alice.stats().clock!;
    expect(clock.samples).toBeGreaterThan(5);
    expect(Math.abs(clock.offsetMs - 5_000)).toBeLessThan(10);
    expect(clock.rttMs).toBeGreaterThanOrEqual(120);
    expect(clock.rttMs).toBeLessThan(145);
    alice.close();
    host.close();
  });

  it('applies every input exactly once despite 20% loss, and the prediction converges on the host', async () => {
    const net = simNetwork({ latencyMs: 50, lossRate: 0.2, jitterMs: 15 });
    const { host, ships, applied } = game(net);
    const alice = player(net, ALICE);
    await vi.advanceTimersByTimeAsync(1_000);
    expect(alice.view()[ALICE]).toBeUndefined();

    // First input creates the ship on the host; after that, 60 moves.
    alice.input({ dx: 0 });
    await vi.advanceTimersByTimeAsync(1_000);
    for (let i = 0; i < 60; i += 1) {
      const before = alice.view()[ALICE]?.x ?? 0;
      alice.input({ dx: 1 });
      // Prediction: the local ship moves NOW, not a round trip later.
      expect(alice.view()[ALICE]?.x).toBe(before + 1);
      await vi.advanceTimersByTimeAsync(33);
    }
    await vi.advanceTimersByTimeAsync(3_000);

    const seqs = applied.get(ALICE)!;
    expect(new Set(seqs).size).toBe(seqs.length);
    expect(seqs).toEqual([...seqs].sort((a, b) => a - b));
    expect(seqs.length).toBe(61);
    expect(ships.get(ALICE)).toEqual({ x: 60, y: 0 });
    expect(alice.view()[ALICE]).toEqual({ x: 60, y: 0 });
    expect(alice.stats().pendingInputs).toBe(0);
    // Prediction uses the host's own rule, so reconciliation — replaying
    // the unacknowledged inputs over each authoritative state — never has
    // to move the ship: no snap-back while inputs are in flight.
    expect(alice.stats().corrections).toBe(0);
    expect(net.stats.dropped).toBeGreaterThan(0);
    alice.close();
    host.close();
  });

  it('survives a replaced session mid-game: streams are reopened and every input still lands once', async () => {
    const net = simNetwork({ latencyMs: 40, lossRate: 0.1 });
    const { host, ships, applied } = game(net);
    const alice = player(net, ALICE);
    await vi.advanceTimersByTimeAsync(1_000);
    alice.input({ dx: 0 });
    for (let i = 0; i < 20; i += 1) {
      if (i === 10) {
        // A relayed → direct upgrade, say: both ends' handles go stale.
        net.replaceSession(HOST);
        net.replaceSession(ALICE);
      }
      alice.input({ dx: 1 });
      await vi.advanceTimersByTimeAsync(33);
    }
    await vi.advanceTimersByTimeAsync(3_000);
    expect(ships.get(ALICE)).toEqual({ x: 20, y: 0 });
    expect(new Set(applied.get(ALICE)).size).toBe(21);
    expect(alice.view()[ALICE]).toEqual({ x: 20, y: 0 });
    expect(alice.stats().pendingInputs).toBe(0);
    alice.close();
    host.close();
  });

  it("interpolates another player's ship smoothly, a delay behind the host", async () => {
    const net = simNetwork({ latencyMs: 40, lossRate: 0.15, jitterMs: 20 });
    const { host } = game(net);
    const alice = player(net, ALICE);
    const bob = player(net, BOB);
    await vi.advanceTimersByTimeAsync(1_000);
    alice.input({ dx: 0 });
    await vi.advanceTimersByTimeAsync(500);

    const seen: number[] = [];
    for (let i = 0; i < 90; i += 1) {
      alice.input({ dx: 1 });
      await vi.advanceTimersByTimeAsync(16);
      const x = bob.view()[ALICE]?.x;
      if (x !== undefined) seen.push(x);
    }
    expect(seen.length).toBeGreaterThan(60);
    for (let i = 1; i < seen.length; i += 1) {
      expect(seen[i]!).toBeGreaterThanOrEqual(seen[i - 1]!);
    }
    // Interpolated, not stepped: most frames show a position between ticks.
    const fractional = seen.filter(x => !Number.isInteger(x)).length;
    expect(fractional).toBeGreaterThan(seen.length / 3);
    alice.close();
    bob.close();
    host.close();
  });

  it('rewinds to what the player saw, never further back than the cap', async () => {
    const net = simNetwork({ latencyMs: 30 });
    const { host, ships } = game(net);
    const alice = player(net, ALICE);
    await vi.advanceTimersByTimeAsync(500);
    alice.input({ dx: 0 });
    await vi.advanceTimersByTimeAsync(300);
    for (let i = 0; i < 30; i += 1) {
      alice.input({ dx: 1 });
      await vi.advanceTimersByTimeAsync(33);
    }
    await vi.advanceTimersByTimeAsync(300);
    const now = Date.now();
    const recent = host.rewind(now - 100);
    expect(recent.clamped).toBe(false);
    expect(recent.time).toBe(now - 100);
    const far = host.rewind(now - 5_000);
    expect(far.clamped).toBe(true);
    expect(far.time).toBe(now - 200);
    expect(ships.get(ALICE)?.x).toBe(30);
    alice.close();
    host.close();
  });

  it('turns away a player authorize refuses', async () => {
    const net = simNetwork({ latencyMs: 10 });
    const host = hostNetcode<Ship, Move>({
      transport: net.node(HOST),
      label: 'test.movement',
      now: () => Date.now(),
      authorize: peer => peer !== BOB,
      step: () => {},
      snapshot: () => ({}),
    });
    const alice = player(net, ALICE);
    const bob = player(net, BOB);
    await vi.advanceTimersByTimeAsync(1_000);
    expect(host.players()).toEqual([ALICE]);
    expect(host.dropped.unauthorized).toBeGreaterThan(0);
    expect(bob.stats().snapshots).toBe(0);
    alice.close();
    bob.close();
    host.close();
  });
});

describe('netcode over the in-page local mesh', () => {
  it('a host and a player on createLocalMesh: the player joins, moves and sees its ship', async () => {
    const { createLocalMesh } = await import('../../src/local.js');
    const mesh = createLocalMesh();
    const hostNode = mesh.node();
    const playerNode = mesh.node();
    const ships = new Map<string, Ship>();
    const host = hostNetcode<Ship, Move>({
      transport: hostNode,
      label: 'local.movement',
      tickRate: 60,
      step: ({ inputs }) => {
        for (const [peer, list] of inputs) {
          for (const { data } of list) ships.set(peer, move(ships.get(peer) ?? { x: 0, y: 0 }, data));
        }
      },
      snapshot: () => Object.fromEntries(ships),
    });
    const self = playerNode.nodeIdHex()!;
    const player = joinNetcode<Ship, Move>({
      transport: playerNode,
      host: hostNode.nodeIdHex()!,
      label: 'local.movement',
      local: { id: self, predict: move },
    });
    const until = async (check: () => boolean) => {
      for (let i = 0; i < 200 && !check(); i += 1) await new Promise(resolve => setTimeout(resolve, 10));
      return check();
    };
    expect(await until(() => player.stats().snapshots > 0)).toBe(true);
    player.input({ dx: 0 });
    for (let i = 0; i < 5; i += 1) player.input({ dx: 2 });
    expect(await until(() => ships.get(self)?.x === 10)).toBe(true);
    expect(await until(() => player.stats().pendingInputs === 0)).toBe(true);
    expect(player.view()[self]).toEqual({ x: 10, y: 0 });
    expect(host.players()).toEqual([self]);
    player.close();
    host.close();
  });
});
