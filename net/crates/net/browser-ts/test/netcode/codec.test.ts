// Compact snapshots (sharded-match plan S0c): the field codecs, the binary
// snapshot frame, and the negotiated binary path between a host and its
// players — with JSON as the fallback for every mismatch.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import {
  angle,
  bool,
  decodeBinarySnapshot,
  decodeFrame,
  encodeBinarySnapshot,
  entityCodec,
  float32,
  hostNetcode,
  int,
  joinNetcode,
  quantized,
  uint,
} from '../../src/netcode/index.js';
import type { NetcodeTransport } from '../../src/netcode/index.js';
import { simNetwork } from './sim.js';

const HOST = '00000000000000aa';
const ALICE = '00000000000000a1';
const LABEL = 'test.codec';

interface Ship {
  readonly x: number;
  readonly z: number;
  readonly yaw: number;
  readonly hp: number;
}
interface Move {
  readonly dx: number;
}
const SIDE = 1000;
const shipCodec = entityCodec<Ship>({ x: quantized(0, SIDE), z: quantized(0, SIDE), yaw: angle(), hp: uint(8) });
const move = (ship: Ship, input: Move): Ship => ({ ...ship, x: Math.min(SIDE, Math.max(0, ship.x + input.dx)) });

/** A deterministic PRNG. */
function rng(seed: number) {
  let state = seed >>> 0;
  return () => {
    state = (state + 0x6d2b79f5) >>> 0;
    let t = state;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

const roundTrip = <T>(field: ReturnType<typeof quantized>, value: T) => {
  const view = new DataView(new ArrayBuffer(field.size));
  field.write(view, 0, value);
  return field.read(view, 0);
};

describe('field codecs', () => {
  it('quantized: every value in range comes back within its quantum', () => {
    const random = rng(1);
    for (const bits of [8, 16] as const) {
      const field = quantized(-50, 250, bits);
      for (let i = 0; i < 2_000; i += 1) {
        const value = -50 + random() * 300;
        expect(Math.abs((roundTrip(field, value) as number) - value)).toBeLessThanOrEqual(field.quantum + 1e-9);
      }
      expect(roundTrip(field, -50)).toBe(-50);
      expect(roundTrip(field, 250)).toBe(250);
    }
  });

  // The S0 prototype bug: a position rounded AFTER being clamped can sit a
  // hair past the edge; quantized naively, 65536 wraps to 0 in a u16.
  it('quantized: a value a hair past either edge clamps to that edge, never wraps', () => {
    const field = quantized(0, 197.9);
    expect(roundTrip(field, 197.9 + 0.004)).toBeCloseTo(197.9, 6);
    expect(roundTrip(field, 1e9)).toBeCloseTo(197.9, 6);
    expect(roundTrip(field, -0.004)).toBe(0);
    expect(roundTrip(field, -1e9)).toBe(0);
  });

  it('angle: any angle comes back in [0, 2π) within its quantum, wrapping both ways', () => {
    const field = angle(8);
    const turn = Math.PI * 2;
    const random = rng(2);
    for (let i = 0; i < 1_000; i += 1) {
      const value = (random() - 0.5) * 40;
      const read = roundTrip(field, value) as number;
      expect(read).toBeGreaterThanOrEqual(0);
      expect(read).toBeLessThan(turn);
      const d = Math.abs((((read - value) % turn) + turn) % turn);
      expect(Math.min(d, turn - d)).toBeLessThanOrEqual(field.quantum + 1e-9);
    }
    expect(roundTrip(field, turn)).toBe(0);
    expect(roundTrip(field, turn - 1e-9)).toBe(0);
  });

  it('uint, int, bool and float32 are exact, and refuse what they cannot carry', () => {
    expect(roundTrip(uint(8), 255)).toBe(255);
    expect(() => roundTrip(uint(8), 256)).toThrow(RangeError);
    expect(() => roundTrip(uint(8), 1.5)).toThrow(RangeError);
    expect(() => roundTrip(uint(16), -1)).toThrow(RangeError);
    expect(roundTrip(int(16), -32768)).toBe(-32768);
    expect(() => roundTrip(int(16), 32768)).toThrow(RangeError);
    expect(roundTrip(bool(), true)).toBe(true);
    expect(() => roundTrip(bool(), 1)).toThrow(TypeError);
    expect(roundTrip(float32(), 1.5)).toBe(1.5);
    expect(() => roundTrip(quantized(0, 1), Number.NaN)).toThrow(TypeError);
    expect(() => roundTrip(quantized(0, 1), '0.5')).toThrow(TypeError);
  });
});

describe('entityCodec', () => {
  it('round-trips an entity within each field, in the schema order', () => {
    const view = new DataView(new ArrayBuffer(shipCodec.size));
    expect(shipCodec.size).toBe(2 + 2 + 1 + 1);
    shipCodec.write({ x: 123.456, z: 999.99, yaw: -1, hp: 77 }, view, 0);
    const read = shipCodec.read(view, 0);
    expect(read.x).toBeCloseTo(123.456, 1);
    expect(read.z).toBeCloseTo(999.99, 1);
    expect(read.hp).toBe(77);
    expect(shipCodec.near(read, { x: 123.456, z: 999.99, yaw: -1, hp: 77 })).toBe(true);
    expect(shipCodec.near(read, { x: 124, z: 999.99, yaw: -1, hp: 77 })).toBe(false);
    expect(shipCodec.near(read, { x: 123.456, z: 999.99, yaw: -1, hp: 76 })).toBe(false);
  });

  it('refuses an entity with a field the schema does not have, or one missing', () => {
    const view = new DataView(new ArrayBuffer(shipCodec.size));
    expect(() => shipCodec.write({ x: 1, z: 1, yaw: 0, hp: 1, name: 'x' } as unknown as Ship, view, 0)).toThrow(/name/);
    expect(() => shipCodec.write({ x: 1, z: 1, yaw: 0 } as unknown as Ship, view, 0)).toThrow(/hp/);
  });

  it('fingerprints names, order and specs', () => {
    const same = entityCodec<Ship>({ x: quantized(0, SIDE), z: quantized(0, SIDE), yaw: angle(), hp: uint(8) });
    expect(same.fingerprint).toBe(shipCodec.fingerprint);
    const reordered = entityCodec<Ship>({ z: quantized(0, SIDE), x: quantized(0, SIDE), yaw: angle(), hp: uint(8) });
    const resized = entityCodec<Ship>({ x: quantized(0, SIDE), z: quantized(0, SIDE), yaw: angle(16), hp: uint(8) });
    const rescaled = entityCodec<Ship>({ x: quantized(0, 2 * SIDE), z: quantized(0, SIDE), yaw: angle(), hp: uint(8) });
    for (const other of [reordered, resized, rescaled]) expect(other.fingerprint).not.toBe(shipCodec.fingerprint);
  });
});

describe('the binary snapshot frame', () => {
  const ship = (x: number): Ship => ({ x, z: 5, yaw: 1, hp: 100 });
  const names = new Map([
    [0, new TextEncoder().encode('alice')],
    [300, new TextEncoder().encode('npc-é')],
  ]);
  const entities = new Map([
    [0, ship(10)],
    [300, ship(20)],
    [7, ship(30)],
  ]);
  const frame = encodeBinarySnapshot(LABEL, { tick: 70_000, t: 12_345.678, ack: 42, iv: 3, c: 2, of: 4 }, names, entities, shipCodec);

  it('round-trips header, names and entities', () => {
    const decoded = decodeBinarySnapshot(frame, LABEL, shipCodec)!;
    expect(decoded).not.toBeNull();
    expect([decoded.tick, decoded.t, decoded.ack, decoded.iv, decoded.c, decoded.of]).toEqual([70_000, 12_345.678, 42, 3, 2, 4]);
    expect([...decoded.names]).toEqual([
      [0, 'alice'],
      [300, 'npc-é'],
    ]);
    expect([...decoded.entities.keys()]).toEqual([0, 300, 7]);
    expect(decoded.entities.get(300)!.x).toBeCloseTo(20, 1);
  });

  it('is much smaller than the same snapshot as JSON', () => {
    const json = new TextEncoder().encode(
      JSON.stringify({ n: LABEL, k: 's', tick: 70_000, t: 12_345.678, ack: 42, iv: 3, c: 2, of: 4, e: { alice: ship(10), 'npc-é': ship(20), b: ship(30) } }),
    );
    expect(frame.length).toBeLessThan(json.length / 2);
  });

  it('is not read for another label, and is never mistaken for JSON', () => {
    expect(decodeBinarySnapshot(frame, 'other.label', shipCodec)).toBeNull();
    expect(decodeFrame(frame, LABEL)).toBeNull();
  });

  it('refuses every truncation, a trailing byte, and a duplicate index', () => {
    for (let length = 0; length < frame.length; length += 1) {
      expect(decodeBinarySnapshot(frame.slice(0, length), LABEL, shipCodec)).toBeNull();
    }
    const longer = new Uint8Array(frame.length + 1);
    longer.set(frame);
    expect(decodeBinarySnapshot(longer, LABEL, shipCodec)).toBeNull();
    // The last entity's one-byte index (7) rewritten to the first's (0).
    const duplicate = frame.slice();
    const lastIndexAt = frame.length - shipCodec.size - 1;
    expect(duplicate[lastIndexAt]).toBe(7);
    duplicate[lastIndexAt] = 0;
    expect(decodeBinarySnapshot(duplicate, LABEL, shipCodec)).toBeNull();
  });

  it('refuses a chunk index outside its chunk count', () => {
    const bad = encodeBinarySnapshot(LABEL, { tick: 1, t: 0, ack: 0, c: 4, of: 4 }, new Map(), new Map(), shipCodec);
    expect(decodeBinarySnapshot(bad, LABEL, shipCodec)).toBeNull();
  });
});

describe('the JSON frames that negotiate it', () => {
  const enc = (value: unknown) => new TextEncoder().encode(JSON.stringify(value));
  it('reads a fingerprint on join and ping, and a bounded name request', () => {
    expect(decodeFrame(enc({ n: LABEL, k: 'h', f: 'abcd1234' }), LABEL)).not.toBeNull();
    expect(decodeFrame(enc({ n: LABEL, k: 'p', t0: 1, f: 'abcd1234' }), LABEL)).not.toBeNull();
    expect(decodeFrame(enc({ n: LABEL, k: 'h', f: 7 }), LABEL)).toBeNull();
    expect(decodeFrame(enc({ n: LABEL, k: 'u', u: [0, 5] }), LABEL)).not.toBeNull();
    expect(decodeFrame(enc({ n: LABEL, k: 'u', u: [-1] }), LABEL)).toBeNull();
    expect(decodeFrame(enc({ n: LABEL, k: 'u', u: Array.from({ length: 257 }, (_, i) => i) }), LABEL)).toBeNull();
  });
});

describe('compact snapshots between a host and a player', () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  /** Wrap a node: `drop` decides, per outgoing payload, whether it is lost. */
  function filtered(node: NetcodeTransport, drop: (payload: Uint8Array) => boolean, sent?: Uint8Array[]): NetcodeTransport {
    return {
      ...node,
      nodeIdHex: () => node.nodeIdHex(),
      onEvent: handler => node.onEvent(handler),
      openStream: options => {
        const inner = node.openStream(options) as { send(p: Uint8Array): unknown; close(): unknown };
        return {
          send: (payload: Uint8Array) => {
            sent?.push(payload);
            return drop(payload) ? undefined : inner.send(payload);
          },
          close: () => inner.close(),
        };
      },
    };
  }

  function game(
    transport: NetcodeTransport,
    options: { codec?: typeof shipCodec; maxFrameBytes?: number; world?: Map<string, Ship | Record<string, unknown>> } = {},
  ) {
    const world = options.world ?? new Map<string, Ship | Record<string, unknown>>([['rock', { x: 500.123, z: 250.5, yaw: 0.5, hp: 9 }]]);
    const host = hostNetcode<Ship, Move>({
      transport,
      label: LABEL,
      tickRate: 30,
      now: () => Date.now(),
      step: ({ inputs }) => {
        for (const [peer, list] of inputs) {
          for (const input of list) world.set(peer, move((world.get(peer) as Ship) ?? { x: 0, z: 0, yaw: 0, hp: 100 }, input.data));
        }
      },
      snapshot: () => Object.fromEntries(world) as Record<string, Ship>,
      ...(options.codec ? { codec: options.codec } : {}),
      ...(options.maxFrameBytes ? { maxFrameBytes: options.maxFrameBytes } : {}),
    });
    return { host, world };
  }

  function alice(transport: NetcodeTransport, codec?: typeof shipCodec) {
    return joinNetcode<Ship, Move>({
      transport,
      host: HOST,
      label: LABEL,
      now: () => Date.now(),
      local: { id: ALICE, predict: move },
      ...(codec ? { codec } : {}),
    });
  }

  it('a matching codec gets binary snapshots: entities arrive quantized, and quantization is not a correction', async () => {
    const net = simNetwork({ latencyMs: 30 });
    const { host, world } = game(net.node(HOST), { codec: shipCodec });
    const player = alice(net.node(ALICE), shipCodec);
    await vi.advanceTimersByTimeAsync(500);
    player.input({ dx: 0 });
    await vi.advanceTimersByTimeAsync(300);
    // Steps that are not on the quantization grid, for many snapshots.
    for (let i = 0; i < 60; i += 1) {
      player.input({ dx: 0.337 });
      await vi.advanceTimersByTimeAsync(33);
    }
    await vi.advanceTimersByTimeAsync(500);
    const stats = player.stats();
    expect(stats.binaryFrames).toBeGreaterThan(20);
    expect(stats.corrections).toBe(0);
    const rock = player.view().rock!;
    expect(rock.x).not.toBe(500.123); // quantized…
    expect(rock.x).toBeCloseTo(500.123, 1); // …within its quantum
    expect((world.get(ALICE) as Ship).x).toBeCloseTo(60 * 0.337, 6);
    expect(player.view()[ALICE]!.x).toBeCloseTo(60 * 0.337, 1);
    player.close();
    host.close();
  });

  it('a mismatched codec, or none on either side, falls back to JSON and still works', async () => {
    const other = entityCodec<Ship>({ x: quantized(0, 2000), z: quantized(0, SIDE), yaw: angle(), hp: uint(8) });
    for (const [hostCodec, playerCodec] of [
      [shipCodec, other],
      [shipCodec, undefined],
      [undefined, shipCodec],
    ] as const) {
      const net = simNetwork({ latencyMs: 30 });
      const { host } = game(net.node(HOST), hostCodec ? { codec: hostCodec } : {});
      const player = alice(net.node(ALICE), playerCodec);
      await vi.advanceTimersByTimeAsync(800);
      expect(player.stats().snapshots).toBeGreaterThan(5);
      expect(player.stats().binaryFrames).toBe(0);
      expect(player.view().rock).toEqual({ x: 500.123, z: 250.5, yaw: 0.5, hp: 9 }); // exact: JSON
      player.close();
      host.close();
    }
  });

  it('a lost name is asked for again: the entity is left out, then drawn', async () => {
    const net = simNetwork({ latencyMs: 30 });
    // Lose the host's first binary snapshot — the one that names 'rock'.
    let binarySeen = 0;
    const hostNode = filtered(net.node(HOST), payload => payload[0] === 0xb1 && ++binarySeen === 1);
    const { host } = game(hostNode, { codec: shipCodec });
    const player = alice(net.node(ALICE), shipCodec);
    await vi.advanceTimersByTimeAsync(1_000);
    expect(player.stats().unnamedEntities).toBeGreaterThan(0);
    expect(player.view().rock?.hp).toBe(9);
    player.close();
    host.close();
  });

  it('an entity the codec refuses goes to that player as JSON, counted, and nothing is lost', async () => {
    const net = simNetwork({ latencyMs: 30 });
    const world = new Map<string, Ship | Record<string, unknown>>([['sign', { x: 1, z: 1, yaw: 0, hp: 1, text: 'hello' }]]);
    const { host } = game(net.node(HOST), { codec: shipCodec, world });
    const player = alice(net.node(ALICE), shipCodec);
    await vi.advanceTimersByTimeAsync(800);
    expect(host.dropped['codec-write-failed']).toBeGreaterThan(0);
    expect(host.lastSendError).toMatch(/text/);
    expect(player.view().sign).toEqual({ x: 1, z: 1, yaw: 0, hp: 1, text: 'hello' });
    player.close();
    host.close();
  });

  it('never reuses an index: a removed entity keeps its index, a new one gets the next', async () => {
    const net = simNetwork({ latencyMs: 30 });
    const sent: Uint8Array[] = [];
    const world = new Map<string, Ship | Record<string, unknown>>([['first', { x: 1, z: 1, yaw: 0, hp: 1 }]]);
    const { host } = game(
      filtered(net.node(HOST), () => false, sent),
      { codec: shipCodec, world },
    );
    const player = alice(net.node(ALICE), shipCodec);
    await vi.advanceTimersByTimeAsync(500);
    world.delete('first');
    world.set('second', { x: 2, z: 2, yaw: 0, hp: 2 });
    await vi.advanceTimersByTimeAsync(500);
    const named = new Map<number, string>();
    for (const payload of sent) {
      const decoded = payload[0] === 0xb1 ? decodeBinarySnapshot(payload, LABEL, shipCodec) : null;
      for (const [index, id] of decoded?.names ?? []) {
        if (named.has(index)) expect(named.get(index)).toBe(id);
        named.set(index, id);
      }
    }
    const indexOf = (id: string) => [...named].find(([, name]) => name === id)?.[0];
    expect(indexOf('first')).toBeDefined();
    expect(indexOf('second')).toBeDefined();
    expect(indexOf('second')).not.toBe(indexOf('first'));
    expect(player.view().first).toBeUndefined();
    expect(player.view().second?.hp).toBe(2);
    player.close();
    host.close();
  });

  it('chunks a large binary snapshot by its binary size, and the player assembles it', async () => {
    const net = simNetwork({ latencyMs: 30 });
    const sent: Uint8Array[] = [];
    const world = new Map<string, Ship | Record<string, unknown>>();
    for (let i = 0; i < 200; i += 1) world.set(`e${i}`, { x: i, z: i, yaw: 0, hp: i % 256 });
    const { host } = game(
      filtered(net.node(HOST), () => false, sent),
      { codec: shipCodec, world, maxFrameBytes: 400 },
    );
    const player = alice(net.node(ALICE), shipCodec);
    await vi.advanceTimersByTimeAsync(1_000);
    const binary = sent.filter(payload => payload[0] === 0xb1);
    expect(binary.length).toBeGreaterThan(0);
    expect(binary.every(payload => payload.length <= 400)).toBe(true);
    expect(binary.some(payload => decodeBinarySnapshot(payload, LABEL, shipCodec)?.of !== undefined)).toBe(true);
    expect(Object.keys(player.view()).filter(id => id.startsWith('e'))).toHaveLength(200);
    expect(player.view().e150?.hp).toBe(150);
    player.close();
    host.close();
  });
});
