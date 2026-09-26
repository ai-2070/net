/**
 * Netcode interest: a player receives only the entities under the keys it
 * named, over a lossy carrier (the `w` frame repeats until a snapshot
 * echoes its version), `visible` stays the permission, a `null` key is
 * always delivered, and a stale interest version never wins.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { hostNetcode, joinNetcode } from '../../src/netcode/index.js';
import { decodeFrame, encodeFrame, type NetcodeTransport } from '../../src/netcode/wire.js';
import { simNetwork } from './sim.js';

const HOST = '00000000000000aa';
const ALICE = '00000000000000a1';
const LABEL = 'test.interest';

interface Thing {
  readonly x: number;
  readonly secret?: boolean;
}
type Move = { readonly dx: number };

/** Cell of 10 units: `c0` holds x 0..9, `c1` x 10..19, and so on. */
const cell = (thing: Thing): string => `c${Math.floor(thing.x / 10)}`;

/** Things at x = 0..99, one secret, one keyed `null` (a beacon). */
function world(): Record<string, Thing> {
  const things: Record<string, Thing> = {};
  for (let x = 0; x < 100; x += 1) things[`t${x}`] = { x, ...(x === 3 ? { secret: true } : {}) };
  return things;
}

/** Wraps a transport and counts the `w` frames it sends. */
function counting(inner: NetcodeTransport, sent: { w: number }): NetcodeTransport {
  return {
    nodeIdHex: () => inner.nodeIdHex(),
    onEvent: handler => inner.onEvent(handler),
    openStream: async options => {
      const stream = await inner.openStream(options);
      return {
        send: (payload: Uint8Array) => {
          if (decodeFrame(payload, LABEL)?.k === 'w') sent.w += 1;
          return stream.send(payload);
        },
        close: () => stream.close(),
      };
    },
  };
}

describe('netcode interest', () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  function host(net: ReturnType<typeof simNetwork>) {
    const things = world();
    return hostNetcode<Thing, Move>({
      transport: net.node(HOST),
      label: LABEL,
      tickRate: 20,
      now: () => Date.now(),
      step: () => {},
      snapshot: () => ({ ...things, beacon: { x: 500 } }),
      visible: (_peer, _id, thing) => thing.secret !== true,
      interest: (id, thing) => (id === 'beacon' ? null : cell(thing)),
    });
  }

  const idsOf = (view: Readonly<Record<string, Thing>>) => Object.keys(view).sort();

  it('delivers only the named cells (plus null-keyed ones), through loss, and follows setInterest', async () => {
    const net = simNetwork({ latencyMs: 30, lossRate: 0.3, jitterMs: 10 });
    const served = host(net);
    const sent = { w: 0 };
    const alice = joinNetcode<Thing, Move>({
      transport: counting(net.node(ALICE), sent),
      host: HOST,
      label: LABEL,
      now: () => Date.now(),
      interest: ['c0'],
    });
    await vi.advanceTimersByTimeAsync(3_000);
    // c0 is x 0..9 minus the secret at 3 (`visible` is the permission,
    // interest only narrows), and the beacon keyed null.
    expect(idsOf(alice.view())).toEqual(['beacon', 't0', 't1', 't2', 't4', 't5', 't6', 't7', 't8', 't9']);

    // Acknowledged: the w frame stops repeating.
    const settled = sent.w;
    await vi.advanceTimersByTimeAsync(2_000);
    expect(sent.w).toBe(settled);

    alice.setInterest(['c9']);
    await vi.advanceTimersByTimeAsync(3_000);
    expect(idsOf(alice.view())).toEqual(['beacon', 't90', 't91', 't92', 't93', 't94', 't95', 't96', 't97', 't98', 't99']);
    alice.close();
    served.close();
  });

  it('sends everything visible to a player that names no interest', async () => {
    const net = simNetwork({ latencyMs: 20 });
    const served = host(net);
    const alice = joinNetcode<Thing, Move>({ transport: net.node(ALICE), host: HOST, label: LABEL, now: () => Date.now() });
    await vi.advanceTimersByTimeAsync(1_500);
    expect(Object.keys(alice.view())).toHaveLength(100); // 99 visible things + the beacon
    alice.close();
    served.close();
  });

  it('never lets a stale interest version replace a newer one', async () => {
    const net = simNetwork({ latencyMs: 20 });
    const served = host(net);
    const alice = joinNetcode<Thing, Move>({
      transport: net.node(ALICE),
      host: HOST,
      label: LABEL,
      now: () => Date.now(),
      interest: ['c0'],
    });
    await vi.advanceTimersByTimeAsync(1_000);
    alice.setInterest(['c5']); // version 2
    await vi.advanceTimersByTimeAsync(1_000);
    // A replayed version-1 frame, as a lossy carrier can deliver late.
    const raw = await net.node(ALICE).openStream({ reliability: 'fireAndForget', peer: HOST, label: LABEL, lossy: true });
    await raw.send(encodeFrame({ n: LABEL, k: 'w', v: 1, w: ['c0'] }));
    await vi.advanceTimersByTimeAsync(1_000);
    expect(idsOf(alice.view()).filter(id => id !== 'beacon').every(id => Number(id.slice(1)) >= 50 && Number(id.slice(1)) < 60)).toBe(true);
    alice.close();
    served.close();
  });

  it('refuses an interest set over the limits', () => {
    const net = simNetwork({ latencyMs: 20 });
    const alice = joinNetcode<Thing, Move>({ transport: net.node(ALICE), host: HOST, label: LABEL, now: () => Date.now() });
    expect(() => alice.setInterest(Array.from({ length: 257 }, (_, i) => `k${i}`))).toThrow(RangeError);
    expect(() => alice.setInterest(['x'.repeat(65)])).toThrow(RangeError);
    alice.close();
  });
});
