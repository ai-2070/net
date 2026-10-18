// A player cut off from its host (net-fps report against 0.45.0). The leaf
// half of that report (a closed stream dropped its peer's later frames, so a
// host that closed a timed-out player's stream never heard it again) is
// witnessed in the leaf's own tests; these pin the netcode halves: the host
// re-admits a timed-out player from its parked boundary, and the player's
// backlog, frame size and send rate stay bounded while the host is silent.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { decodeFrame, hostNetcode, joinNetcode } from '../../src/netcode/index.js';
import type { NetcodeTransport } from '../../src/netcode/index.js';

const HOST = '00000000000000aa';
const ALICE = '00000000000000a1';
const LABEL = 'test.cut-off';
/** One event: the leaf's MAX_EVENT_SIZE. An input frame must fit in it. */
const ONE_EVENT = 8000;

interface Pad {
  readonly pad: string;
}
type None = Record<string, never>;

const json = (value: unknown) => new TextEncoder().encode(JSON.stringify(value));

/**
 * A transport the test drives by hand: `deliver` hands the node a frame as
 * if `from` sent it; everything the node sends lands in `sent`, and every
 * stream close is counted.
 */
function handDriven(self: string) {
  let handler: Parameters<NetcodeTransport['onEvent']>[0] | undefined;
  const sent: { peer: string | undefined; payload: Uint8Array }[] = [];
  let closes = 0;
  const transport: NetcodeTransport = {
    nodeIdHex: () => self,
    openStream: ({ peer }) => ({
      send: (payload: Uint8Array) => {
        sent.push({ peer, payload });
      },
      close: () => {
        closes += 1;
      },
    }),
    onEvent: h => {
      handler = h;
      return () => {
        handler = undefined;
      };
    },
  };
  const deliver = (from: string, payload: Uint8Array) =>
    handler?.({ type: 'stream_data', peerNode: BigInt(`0x${from}`).toString(10), payload });
  return { transport, sent, deliver, closes: () => closes };
}

/** The input frames in `sent`, decoded, with their size on the wire. */
const inputFrames = (sent: readonly { payload: Uint8Array }[]) =>
  sent.flatMap(({ payload }) => {
    const frame = decodeFrame<None, Pad>(payload, LABEL);
    return frame !== null && frame.k === 'i'
      ? [{ seqs: (frame as { readonly i: readonly { readonly seq: number }[] }).i.map(input => input.seq), bytes: payload.byteLength }]
      : [];
  });

beforeEach(() => {
  vi.useFakeTimers();
});
afterEach(() => {
  vi.useRealTimers();
});

describe('a timed-out player comes back', () => {
  it('is re-admitted after the host closed its stream, and carries on from its parked boundary', async () => {
    let clock = 0;
    const applied: number[] = [];
    const { transport, deliver, closes } = handDriven(HOST);
    const net = hostNetcode<None, Pad>({
      transport,
      label: LABEL,
      autoTick: false,
      playerTimeoutMs: 100,
      now: () => clock,
      step: ({ inputs }) => {
        for (const list of inputs.values()) for (const input of list) applied.push(input.seq);
      },
      snapshot: () => ({}),
    });
    const inputs = (from: number, to: number) =>
      deliver(
        ALICE,
        json({ n: LABEL, k: 'i', i: Array.from({ length: to - from + 1 }, (_, k) => ({ seq: from + k, seen: 0, data: { pad: '' } })) }),
      );

    inputs(1, 4);
    net.tick();
    await vi.advanceTimersByTimeAsync(0); // the stream the first snapshot asked for opens
    net.tick();
    clock += 500;
    net.tick(); // timed out: the stream is closed and the boundary parked
    expect(net.players()).toEqual([]);
    expect(closes()).toBe(1);

    inputs(3, 6); // the player was never told: it repeats what was not acked
    net.tick();
    expect(net.players()).toEqual([ALICE]);
    expect(applied).toEqual([1, 2, 3, 4, 5, 6]);
    net.close();
  });
});

describe('a player whose host is silent', () => {
  it('keeps at most 64 inputs, sends frames under one event, and repeats them only on the timer', async () => {
    const { transport, sent, deliver } = handDriven(ALICE);
    const player = joinNetcode<None, Pad>({ transport, host: HOST, label: LABEL, now: () => Date.now() });
    await vi.advanceTimersByTimeAsync(0); // the stream opens
    const pad = 'x'.repeat(400);

    // The host may still answer: one frame per input, each under one event
    // even once the backlog would not fit, carrying the newest inputs.
    sent.length = 0;
    for (let i = 0; i < 30; i += 1) player.input({ pad });
    const early = inputFrames(sent);
    expect(early).toHaveLength(30);
    for (const frame of early) expect(frame.bytes).toBeLessThanOrEqual(ONE_EVENT);
    expect(early.at(-1)!.seqs.at(-1)).toBe(30);
    expect(early.at(-1)!.seqs.length).toBeLessThan(30); // trimmed to fit, oldest first

    // Silent for longer than a second: inputs no longer go out per input.
    await vi.advanceTimersByTimeAsync(1500);
    for (let i = 0; i < 500; i += 1) player.input({ pad });
    expect(player.stats().pendingInputs).toBe(64);
    sent.length = 0;
    for (let i = 0; i < 100; i += 1) player.input({ pad });
    expect(inputFrames(sent)).toHaveLength(0);

    // The ping timer (4/s by default) still repeats the newest of the backlog.
    await vi.advanceTimersByTimeAsync(1000);
    const repeated = inputFrames(sent);
    expect(repeated.length).toBeGreaterThanOrEqual(3);
    expect(repeated.length).toBeLessThanOrEqual(5);
    for (const frame of repeated) {
      expect(frame.bytes).toBeLessThanOrEqual(ONE_EVENT);
      expect(frame.seqs.at(-1)).toBe(630);
    }

    // The host is heard again: per-input sending resumes at once.
    deliver(HOST, json({ n: LABEL, k: 'q', t0: 0, t1: 0, t2: 0 }));
    sent.length = 0;
    player.input({ pad });
    expect(inputFrames(sent)).toHaveLength(1);
    player.close();
  });
});
