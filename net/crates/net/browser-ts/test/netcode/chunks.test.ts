/**
 * Snapshots larger than one frame travel as independent chunks: each fits
 * the frame budget, a lost chunk costs only its entities for one tick
 * (carried over from the previous snapshot), and the local entity is never
 * reconciled against a carried-over copy.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { hostNetcode, joinNetcode } from '../../src/netcode/index.js';
import { snapshotFrames } from '../../src/netcode/host.js';
import { chunkOf, encodeFrame } from '../../src/netcode/wire.js';
import type { NetcodeTransport } from '../../src/netcode/wire.js';
import { simNetwork } from './sim.js';

const HOST = '00000000000000aa';
const ALICE = '00000000000000a1';

interface Ship {
  readonly x: number;
  readonly y: number;
  readonly name: string;
}
interface Move {
  readonly dx: number;
}

/** 300 ships with names long enough that the world is several frames. */
function fleet(): Record<string, Ship> {
  const ships: Record<string, Ship> = {};
  for (let i = 0; i < 300; i += 1) ships[`ship-${i}`] = { x: i, y: 0, name: `vessel number ${i} of the fleet` };
  return ships;
}

describe('snapshotFrames', () => {
  it('sends a small snapshot whole, and a large one as chunks that fit and cover every entity once', () => {
    const small = snapshotFrames('l', 1, 10, 0, { a: { x: 1 } }, 8000);
    expect(small).toHaveLength(1);
    expect(small[0]!.k === 's' && small[0]!.of).toBeUndefined();

    const world = fleet();
    const frames = snapshotFrames('l', 1, 10, 0, world, 2000);
    expect(frames.length).toBeGreaterThan(1);
    const seen = new Map<string, number>();
    for (const frame of frames) {
      if (frame.k !== 's') throw new Error('not a snapshot');
      expect(encodeFrame(frame).length).toBeLessThanOrEqual(2000);
      for (const id of Object.keys(frame.e)) {
        seen.set(id, (seen.get(id) ?? 0) + 1);
        expect(chunkOf(id, frame.of!)).toBe(frame.c);
      }
    }
    expect(seen.size).toBe(300);
    expect([...seen.values()].every(count => count === 1)).toBe(true);
  });
});

describe('chunked snapshots over a lossy network', () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  /** The host's transport with every sent frame's size recorded. */
  function sizing(inner: NetcodeTransport, sizes: number[]): NetcodeTransport {
    return {
      nodeIdHex: () => inner.nodeIdHex(),
      onEvent: handler => inner.onEvent(handler),
      openStream: async options => {
        const stream = await inner.openStream(options);
        return {
          send: (payload: Uint8Array) => {
            sizes.push(payload.length);
            return stream.send(payload);
          },
          close: () => stream.close(),
        };
      },
    };
  }

  it('keeps every entity in view through lost chunks, and each frame fits the budget', async () => {
    const net = simNetwork({ latencyMs: 30, lossRate: 0.2, jitterMs: 10 });
    const sizes: number[] = [];
    const ships = fleet();
    const host = hostNetcode<Ship, Move>({
      transport: sizing(net.node(HOST), sizes),
      label: 'test.chunks',
      tickRate: 20,
      maxFrameBytes: 2000,
      now: () => Date.now(),
      step: ({ inputs }) => {
        for (const [peer, list] of inputs) {
          for (const input of list) {
            const was = ships[peer] ?? { x: 0, y: 0, name: 'alice' };
            ships[peer] = { ...was, x: was.x + input.data.dx };
          }
        }
        // Everything drifts, so every chunk carries news every tick.
        for (let i = 0; i < 300; i += 1) {
          const ship = ships[`ship-${i}`]!;
          ships[`ship-${i}`] = { ...ship, y: ship.y + 1 };
        }
      },
      snapshot: () => ({ ...ships }),
    });
    const alice = joinNetcode<Ship, Move>({
      transport: net.node(ALICE),
      host: HOST,
      label: 'test.chunks',
      now: () => Date.now(),
      local: { id: ALICE, predict: (ship, input) => ({ ...ship, x: ship.x + input.dx }) },
    });
    await vi.advanceTimersByTimeAsync(1_000);
    alice.input({ dx: 0 });
    await vi.advanceTimersByTimeAsync(500);

    for (let i = 0; i < 30; i += 1) {
      alice.input({ dx: 1 });
      await vi.advanceTimersByTimeAsync(50);
      // No ship ever drops out of view because its chunk was lost.
      const view = alice.view();
      expect(Object.keys(view).filter(id => id.startsWith('ship-'))).toHaveLength(300);
    }
    await vi.advanceTimersByTimeAsync(2_000);

    expect(alice.stats().partialSnapshots).toBeGreaterThan(0);
    expect(Math.max(...sizes)).toBeLessThanOrEqual(2000);
    // The prediction was reconciled only against what the host sent: it
    // ends where the host has the ship, with no spurious correction.
    expect(alice.view()[ALICE]?.x).toBe(30);
    expect(ships[ALICE]?.x).toBe(30);
    expect(alice.stats().corrections).toBe(0);
    alice.close();
    host.close();
  });
});
