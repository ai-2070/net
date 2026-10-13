// Fixes from the sharded-match review (NET-1 … NET-12,
// docs/internal/misc/CODE_REVIEW_2026_10_13_SHARDED_MATCH.md): acks trim
// every client's inputs, a re-created host's indexes, a rejected offer's
// throttle, angles the short way, late adoptions, name spam, forgotten
// names, hostile seqs and floats, and sampling at a snapshot's exact time.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import {
  SnapshotBuffer,
  angle,
  decodeBinarySnapshot,
  decodeFrame,
  encodeBinarySnapshot,
  entityCodec,
  float32,
  hostNetcode,
  joinNetcode,
  lerpAngle,
  lerpNumbers,
  quantized,
  uint,
} from '../../src/netcode/index.js';
import type { NetcodeTransport, TickInput } from '../../src/netcode/index.js';
import { simNetwork } from './sim.js';

const HOST = '00000000000000aa';
const ALICE = '00000000000000a1';
const LABEL = 'test.review';
const TURN = Math.PI * 2;

interface Ship {
  readonly x: number;
  readonly yaw: number;
  readonly hp: number;
}
interface Move {
  readonly dx: number;
}
const shipCodec = entityCodec<Ship>({ x: quantized(0, 1000), yaw: angle(16), hp: uint(8) });
const ship = (x: number, hp = 1, yaw = 0): Ship => ({ x, yaw, hp });
const utf8 = (text: string) => new TextEncoder().encode(text);
const json = (value: unknown) => utf8(JSON.stringify(value));

/**
 * A transport the test drives by hand: `deliver` hands the node a frame as
 * if `from` sent it; everything the node sends lands in `sent`.
 */
function handDriven(self: string) {
  let handler: Parameters<NetcodeTransport['onEvent']>[0] | undefined;
  const sent: { peer: string | undefined; payload: Uint8Array }[] = [];
  const transport: NetcodeTransport = {
    nodeIdHex: () => self,
    openStream: ({ peer }) => ({
      send: (payload: Uint8Array) => {
        sent.push({ peer, payload });
      },
      close: () => undefined,
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
  return { transport, sent, deliver };
}

/** The JSON frames in `sent`, decoded. */
const jsonFrames = (sent: readonly { payload: Uint8Array }[]) =>
  sent.map(({ payload }) => decodeFrame<Ship, Move>(payload, LABEL)).filter(frame => frame !== null);

/** A binary snapshot for the client to read. */
function binary(
  tick: number,
  t: number,
  names: Record<number, string>,
  entities: Record<number, Ship>,
  instance?: number,
): Uint8Array {
  return encodeBinarySnapshot(
    LABEL,
    { tick, t, ack: 0, ...(instance === undefined ? {} : { instance }) },
    new Map(Object.entries(names).map(([index, id]) => [Number(index), utf8(id)])),
    new Map(Object.entries(entities).map(([index, entity]) => [Number(index), entity])),
    shipCodec,
  );
}

beforeEach(() => {
  vi.useFakeTimers();
});
afterEach(() => {
  vi.useRealTimers();
});

describe('NET-1: acknowledged inputs are trimmed whether or not the client predicts', () => {
  it('a client without `local` drops acked inputs and goes back to small frames after a backlog', async () => {
    const net = simNetwork({ latencyMs: 30 });
    const host = hostNetcode<Ship, Move>({
      transport: net.node(HOST),
      label: LABEL,
      now: () => Date.now(),
      step: () => undefined,
      snapshot: () => ({}),
    });
    const inner = net.node(ALICE);
    const sent: Uint8Array[] = [];
    const spy: NetcodeTransport = {
      ...inner,
      openStream: options => {
        const stream = inner.openStream(options) as { send(p: Uint8Array): unknown; close(): unknown };
        return {
          send: (payload: Uint8Array) => {
            sent.push(payload);
            return stream.send(payload);
          },
          close: () => stream.close(),
        };
      },
    };
    const player = joinNetcode<Ship, Move>({ transport: spy, host: HOST, label: LABEL, now: () => Date.now() });
    await vi.advanceTimersByTimeAsync(500);
    // A backlog: more pending than the redundancy (8), so one frame carries all.
    for (let i = 0; i < 20; i += 1) player.input({ dx: 1 });
    await vi.advanceTimersByTimeAsync(500);
    expect(player.stats().pendingInputs).toBe(0);
    sent.length = 0;
    player.input({ dx: 1 });
    const inputs = jsonFrames(sent.map(payload => ({ payload }))).filter(frame => frame.k === 'i');
    expect(inputs).toHaveLength(1);
    expect((inputs[0] as { readonly i: readonly unknown[] }).i).toHaveLength(1);
    player.close();
    host.close();
  });
});

describe('NET-2: a re-created host is never read through the old one\'s names', () => {
  it('the binary frame round-trips the instance, with and without an epoch, and refuses every truncation', () => {
    for (const ep of [undefined, 3]) {
      const frame = encodeBinarySnapshot(
        LABEL,
        { tick: 9, t: 1.5, ack: 2, instance: 0xdeadbeef, ...(ep === undefined ? {} : { ep }) },
        new Map([[0, utf8('a')]]),
        new Map([[0, ship(1)]]),
        shipCodec,
      );
      const decoded = decodeBinarySnapshot(frame, LABEL, shipCodec)!;
      expect([decoded.instance, decoded.ep, decoded.tick, decoded.ack]).toEqual([0xdeadbeef, ep, 9, 2]);
      for (let length = 0; length < frame.length; length += 1) expect(decodeBinarySnapshot(frame.slice(0, length), LABEL, shipCodec)).toBeNull();
    }
    expect(decodeBinarySnapshot(binary(1, 0, {}, {}), LABEL, shipCodec)?.instance).toBeUndefined();
    expect(() => binary(1, 0, {}, {}, 2 ** 32)).toThrow(RangeError);
  });

  it('a new host instance drops the old names: an unnamed index is asked for, not misnamed; the old instance is ignored after', async () => {
    const { transport, sent, deliver } = handDriven(ALICE);
    const player = joinNetcode<Ship, Move>({ transport, host: HOST, label: LABEL, codec: shipCodec, now: () => Date.now() });
    await vi.advanceTimersByTimeAsync(0);
    deliver(HOST, binary(1, 100, { 0: 'a' }, { 0: ship(10, 1) }, 7));
    expect(Object.keys(player.latest()!.entities)).toEqual(['a']);
    // Re-created: index 0 is now another entity, and the frame naming it was lost.
    sent.length = 0;
    deliver(HOST, binary(0, 200, {}, { 0: ship(20, 3) }, 8));
    expect(player.latest()!.entities).toEqual({});
    const asked = jsonFrames(sent).filter(frame => frame.k === 'u');
    expect(asked.map(frame => (frame as { readonly u: readonly number[] }).u)).toEqual([[0]]);
    deliver(HOST, binary(1, 233, { 0: 'c' }, { 0: ship(20, 3) }, 8));
    expect(Object.keys(player.latest()!.entities)).toEqual(['c']);
    // A late frame from the old instance: not a third host.
    deliver(HOST, binary(2, 150, {}, { 0: ship(10, 1) }, 7));
    expect(Object.keys(player.latest()!.entities)).toEqual(['c']);
    expect(player.stats().binaryFrames).toBe(3);
    player.close();
  });

  it('without an instance, a conflicting name starts the names over rather than dropping every frame', async () => {
    const { transport, sent, deliver } = handDriven(ALICE);
    const player = joinNetcode<Ship, Move>({ transport, host: HOST, label: LABEL, codec: shipCodec, now: () => Date.now() });
    await vi.advanceTimersByTimeAsync(0);
    deliver(HOST, binary(1, 100, { 0: 'a', 1: 'b' }, { 0: ship(10, 1), 1: ship(20, 2) }));
    deliver(HOST, binary(2, 200, { 0: 'c' }, { 0: ship(30, 3) }));
    expect(player.latest()!.entities).toEqual({ c: expect.objectContaining({ hp: 3 }) });
    sent.length = 0;
    // Index 1 was the old host's 'b': no longer trusted, so asked for.
    deliver(HOST, binary(3, 300, {}, { 0: ship(30, 3), 1: ship(40, 4) }));
    expect(Object.keys(player.latest()!.entities)).toEqual(['c']);
    expect(jsonFrames(sent).some(frame => frame.k === 'u' && (frame as { readonly u: readonly number[] }).u.includes(1))).toBe(true);
    player.close();
  });

  it('end to end: a host re-created under the same label, its first frame lost, is drawn under the right ids', async () => {
    const net = simNetwork({ latencyMs: 30 });
    const node = net.node(HOST);
    let world: Record<string, Ship> = { a: ship(10, 1), b: ship(20, 2) };
    const start = () =>
      hostNetcode<Ship, Move>({ transport: node, label: LABEL, codec: shipCodec, now: () => Date.now(), step: () => undefined, snapshot: () => world });
    let host = start();
    const player = joinNetcode<Ship, Move>({ transport: net.node(ALICE), host: HOST, label: LABEL, codec: shipCodec, now: () => Date.now() });
    await vi.advanceTimersByTimeAsync(1_000);
    expect(player.latest()!.entities.b?.hp).toBe(2);
    host.close();
    // The new host's index 0 is 'c', 1 is 'a'; its first binary frame (the
    // one naming them) is lost.
    world = { c: ship(30, 3), a: ship(10, 1) };
    let binarySeen = 0;
    const lossy: NetcodeTransport = {
      ...node,
      openStream: options => {
        const stream = node.openStream(options) as { send(p: Uint8Array): unknown; close(): unknown };
        return {
          send: (payload: Uint8Array) => (payload[0] === 0xb1 && ++binarySeen === 1 ? undefined : stream.send(payload)),
          close: () => stream.close(),
        };
      },
    };
    host = hostNetcode<Ship, Move>({ transport: lossy, label: LABEL, codec: shipCodec, now: () => Date.now(), step: () => undefined, snapshot: () => world });
    await vi.advanceTimersByTimeAsync(1_000);
    const latest = player.latest()!.entities;
    expect(Object.keys(latest).sort()).toEqual(['a', 'c']);
    expect(latest.a!.hp).toBe(1);
    expect(latest.c!.hp).toBe(3);
    player.close();
    host.close();
  });
});

describe('NET-3 is covered in lobby.test.ts and world/regions.test.ts', () => {
  it('(see there)', () => undefined);
});

describe('NET-4: an adoption outlives a slow or not-yet-admitted player', () => {
  function host(options: { authorize?: (peer: string) => boolean } = {}) {
    let clock = 0;
    const applied: number[] = [];
    const { transport, deliver } = handDriven(HOST);
    const net = hostNetcode<Ship, Move>({
      transport,
      label: LABEL,
      autoTick: false,
      playerTimeoutMs: 100,
      now: () => clock,
      step: ({ inputs }) => {
        for (const list of inputs.values()) for (const input of list) applied.push(input.seq);
      },
      snapshot: () => ({}),
      ...(options.authorize ? { authorize: options.authorize } : {}),
    });
    const inputs = (from: number, to: number) =>
      deliver(ALICE, json({ n: LABEL, k: 'i', i: Array.from({ length: to - from + 1 }, (_, k) => ({ seq: from + k, seen: 0, data: { dx: 1 } })) }));
    return { net, applied, inputs, advance: (ms: number) => (clock += ms) };
  }

  it('a player that arrives after the timeout keeps the boundary it was adopted with', () => {
    const { net, applied, inputs, advance } = host();
    net.adoptPlayer(ALICE, { lastApplied: 5 });
    advance(500);
    net.tick(); // the record times out before the player's first frame
    expect(net.players()).toEqual([]);
    inputs(3, 7);
    net.tick();
    expect(applied).toEqual([6, 7]);
  });

  it('a player authorize refuses at adoption keeps the boundary for when it is admitted', () => {
    let admitted = false;
    const { net, applied, inputs } = host({ authorize: () => admitted });
    net.adoptPlayer(ALICE, { lastApplied: 5 });
    expect(net.dropped['adopt-unauthorized']).toBe(1);
    admitted = true;
    inputs(3, 7);
    net.tick();
    expect(applied).toEqual([6, 7]);
  });

  it('a timed-out player released later reports how far it was applied, and stays released', () => {
    const { net, applied, inputs, advance } = host();
    inputs(1, 4);
    net.tick();
    advance(500);
    net.tick();
    expect(net.players()).toEqual([]);
    expect(net.releasePlayer(ALICE)).toEqual({ lastApplied: 4 });
    inputs(5, 6);
    net.tick();
    expect(applied).toEqual([1, 2, 3, 4]);
    expect(net.dropped['released']).toBe(1);
  });
});

describe('NET-5: a rejected offer is throttled like a relayed one', () => {
  it('connectPeer rejecting does not make every reopen offer again', async () => {
    let offers = 0;
    let opens = 0;
    const transport: NetcodeTransport = {
      nodeIdHex: () => ALICE,
      connectPeer: async () => {
        offers += 1;
        throw new Error('session: the peer answered Reject');
      },
      openStream: () => {
        opens += 1;
        return {
          send: () => {
            throw new Error('session: stale stream handle; reopen the stream');
          },
          close: () => undefined,
        };
      },
      onEvent: () => () => undefined,
    };
    const player = joinNetcode<Ship, Move>({ transport, host: HOST, label: LABEL, now: () => Date.now() });
    await vi.advanceTimersByTimeAsync(10_000);
    expect(opens).toBeGreaterThan(5);
    expect(offers).toBe(1);
    await vi.advanceTimersByTimeAsync(25_000);
    expect(offers).toBe(2);
    player.close();
  });
});

describe('NET-6: angles interpolate the short way', () => {
  it('lerpAngle crosses 0 / 2π instead of going round, and lands on `to` exactly', () => {
    const from = TURN - 0.1;
    const to = 0.1;
    const mid = lerpAngle(from, to, 0.5);
    expect(((mid % TURN) + TURN) % TURN < 1e-9 || Math.abs((((mid % TURN) + TURN) % TURN) - TURN) < 1e-9).toBe(true);
    expect(lerpAngle(from, to, 0)).toBeCloseTo(-0.1, 9);
    expect(lerpAngle(from, to, 1)).toBe(to);
    expect(lerpAngle(0.1, TURN - 0.1, 0.5)).toBeCloseTo(TURN, 9);
    expect(lerpAngle(1, 2, 0.25)).toBeCloseTo(1.25, 9);
  });

  it("a codec's interpolate turns its angle() fields the short way, and lerps the rest", () => {
    const blended = shipCodec.interpolate!(ship(0, 0, TURN - 0.2), ship(10, 10, 0.2), 0.5);
    expect(blended.x).toBe(5);
    expect(blended.hp).toBe(5);
    expect(Math.min(Math.abs(blended.yaw), Math.abs(blended.yaw - TURN))).toBeLessThan(1e-9);
  });

  it('a client with a codec samples an angle the short way; one without keeps the plain lerp', async () => {
    for (const codec of [shipCodec, undefined]) {
      const { transport, deliver } = handDriven(ALICE);
      const player = joinNetcode<Ship, Move>({ transport, host: HOST, label: LABEL, now: () => Date.now(), ...(codec ? { codec } : {}) });
      await vi.advanceTimersByTimeAsync(0);
      const snapshot = (tick: number, t: number, yaw: number) =>
        json({ n: LABEL, k: 's', tick, t, ack: 0, e: { s: ship(0, 1, yaw) } });
      deliver(HOST, snapshot(1, 0, TURN - 0.2));
      deliver(HOST, snapshot(2, 100, 0.2));
      const yaw = player.sampleAt(50)!.entities.s!.yaw;
      if (codec) expect(Math.min(Math.abs(yaw), Math.abs(yaw - TURN))).toBeLessThan(1e-9);
      else expect(yaw).toBeCloseTo(Math.PI, 9);
      player.close();
    }
  });
});

describe('NET-7: name requests cannot make every tick carry every name', () => {
  it('an index is named again at most once per 100 ms however often it is asked for', async () => {
    let clock = 0;
    const { transport, sent, deliver } = handDriven(HOST);
    const net = hostNetcode<Ship, Move>({
      transport,
      label: LABEL,
      autoTick: false,
      codec: shipCodec,
      now: () => clock,
      step: () => undefined,
      snapshot: () => ({ rock: ship(1, 9) }),
    });
    deliver(ALICE, json({ n: LABEL, k: 'h', f: shipCodec.fingerprint }));
    net.tick();
    await vi.advanceTimersByTimeAsync(0);
    sent.length = 0;
    for (let k = 1; k <= 20; k += 1) {
      clock = k * 10;
      deliver(ALICE, json({ n: LABEL, k: 'u', u: [0] }));
      net.tick();
    }
    const named = sent.filter(({ payload }) => (decodeBinarySnapshot(payload, LABEL, shipCodec)?.names.size ?? 0) > 0);
    expect(sent.length).toBe(20);
    expect(named.length).toBeLessThanOrEqual(2);
    expect(named.length).toBeGreaterThanOrEqual(1);
    expect(net.dropped['name-request-throttled']).toBeGreaterThan(0);
    net.close();
  });
});

describe('NET-8: name tables forget ids gone for a while', () => {
  it('the host gives an id that comes back after the forget time a fresh index, named again', async () => {
    let clock = 0;
    const { transport, sent, deliver } = handDriven(HOST);
    let world: Record<string, Ship> = { rock: ship(1, 9), shot: ship(2, 7) };
    const net = hostNetcode<Ship, Move>({
      transport,
      label: LABEL,
      autoTick: false,
      codec: shipCodec,
      playerTimeoutMs: 60_000,
      now: () => clock,
      step: () => undefined,
      snapshot: () => world,
    });
    // A ping first: its pong opens the stream, so the first snapshot (the
    // one that names everything) is sent, not dropped.
    deliver(ALICE, json({ n: LABEL, k: 'p', t0: 0, f: shipCodec.fingerprint }));
    await vi.advanceTimersByTimeAsync(0);
    const indexOf = (id: string) => {
      for (const { payload } of sent) {
        for (const [index, name] of decodeBinarySnapshot(payload, LABEL, shipCodec)?.names ?? []) if (name === id) return index;
      }
      return undefined;
    };
    net.tick();
    const before = indexOf('shot');
    expect(before).toBeDefined();
    world = { rock: ship(1, 9) };
    for (let t = 500; t <= 12_000; t += 500) {
      clock = t;
      net.tick();
    }
    sent.length = 0;
    world = { rock: ship(1, 9), shot: ship(2, 7) };
    clock = 12_100;
    net.tick();
    const after = indexOf('shot');
    expect(after).toBeDefined();
    expect(after).not.toBe(before);
    // 'rock' never left: still known, not named again.
    expect(indexOf('rock')).toBeUndefined();
    net.close();
  });

  it('a player forgets a name no frame has carried for three times the forget time', async () => {
    let clock = 0;
    const { transport, deliver } = handDriven(ALICE);
    const player = joinNetcode<Ship, Move>({ transport, host: HOST, label: LABEL, codec: shipCodec, now: () => clock });
    await vi.advanceTimersByTimeAsync(0);
    deliver(HOST, binary(1, 100, { 0: 'shot', 1: 'rock' }, { 0: ship(1), 1: ship(2) }, 5));
    // Only 'rock' for 31 s.
    for (let t = 1_000; t <= 31_000; t += 1_000) {
      clock = t;
      deliver(HOST, binary(t / 1000 + 1, t, {}, { 1: ship(2) }, 5));
    }
    expect(player.stats().unnamedEntities).toBe(0);
    clock = 32_000;
    deliver(HOST, binary(40, 32_000, {}, { 0: ship(1), 1: ship(2) }, 5));
    expect(player.stats().unnamedEntities).toBe(1);
    expect(Object.keys(player.latest()!.entities)).toEqual(['rock']);
    player.close();
  });
});

describe('NET-9 / NET-10: hostile input seqs', () => {
  it('a seq that is not a positive safe integer is never applied, and does not wedge later inputs', () => {
    const { transport, deliver } = handDriven(HOST);
    const seqs: number[] = [];
    const net = hostNetcode<Ship, Move>({
      transport,
      label: LABEL,
      autoTick: false,
      now: () => 0,
      step: ({ inputs }) => {
        for (const list of inputs.values()) for (const input of list) seqs.push(input.seq);
      },
      snapshot: () => ({}),
    });
    deliver(ALICE, json({ n: LABEL, k: 'i', i: [{ seq: 1e300, seen: 0, data: {} }, { seq: 2 ** 53, seen: 0, data: {} }, { seq: 0, seen: 0, data: {} }] }));
    net.tick();
    deliver(ALICE, json({ n: LABEL, k: 'i', i: [{ seq: 1, seen: 0, data: {} }] }));
    net.tick();
    expect(seqs).toEqual([1]);
    net.close();
  });

  it('a lease-lost tick with a huge pending set does not throw', () => {
    const { transport, deliver } = handDriven(HOST);
    let live = true;
    const stepped: ReadonlyMap<string, readonly TickInput<Move>[]>[] = [];
    const net = hostNetcode<Ship, Move>({
      transport,
      label: LABEL,
      autoTick: false,
      now: () => 0,
      live: () => live,
      step: ({ inputs }) => {
        stepped.push(inputs);
      },
      snapshot: () => ({}),
    });
    const count = 300_000;
    deliver(ALICE, json({ n: LABEL, k: 'i', i: Array.from({ length: count }, (_, k) => ({ seq: k + 1, seen: 0, data: {} })) }));
    live = false;
    expect(() => net.tick()).not.toThrow();
    expect(net.dropped['lease-lost']).toBe(count);
    live = true;
    deliver(ALICE, json({ n: LABEL, k: 'i', i: [{ seq: count, seen: 0, data: {} }, { seq: count + 1, seen: 0, data: {} }] }));
    net.tick();
    expect(stepped.at(-1)!.get(ALICE)!.map(input => input.seq)).toEqual([count + 1]);
    net.close();
  });
});

describe('NET-11: the decoder refuses a non-finite float32', () => {
  it('NaN or an infinity in a float32 field refuses the frame', () => {
    const codec = entityCodec<{ v: number }>({ v: float32() });
    const frame = encodeBinarySnapshot(LABEL, { tick: 1, t: 0, ack: 0 }, new Map(), new Map([[0, { v: 1.5 }]]), codec);
    expect(decodeBinarySnapshot(frame, LABEL, codec)?.entities.get(0)).toEqual({ v: 1.5 });
    for (const bad of [Number.NaN, Number.POSITIVE_INFINITY, Number.NEGATIVE_INFINITY]) {
      const patched = frame.slice();
      new DataView(patched.buffer).setFloat32(patched.length - 4, bad);
      expect(decodeBinarySnapshot(patched, LABEL, codec)).toBeNull();
    }
  });
});

describe('NET-12: sampling at a snapshot\'s exact time', () => {
  it('includes an entity that first appears in that snapshot', () => {
    const buffer = new SnapshotBuffer<{ x: number }>();
    buffer.add({ tick: 0, time: 0, entities: { s: { x: 0 } } });
    buffer.add({ tick: 1, time: 100, entities: { s: { x: 10 }, n: { x: 5 } } });
    buffer.add({ tick: 2, time: 200, entities: { s: { x: 20 }, n: { x: 6 } } });
    expect(buffer.sample(100, lerpNumbers)).toEqual({ entities: { s: { x: 10 }, n: { x: 5 } }, gapMs: 100 });
    expect(buffer.sample(0, lerpNumbers)?.entities).toEqual({ s: { x: 0 } });
    // Between snapshots, still only what both ends know.
    expect(buffer.sample(50, lerpNumbers)?.entities).toEqual({ s: { x: 5 } });
  });
});
