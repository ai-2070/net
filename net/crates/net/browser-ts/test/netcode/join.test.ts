// A join that cannot connect must say why (found by the S0b topology run):
// `connectPeer` RESOLVES `noAnnouncement` when this node holds no
// announcement from the host, and `joinNetcode` used to discard it — the
// player waited forever with nothing in `stats()`.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { NetcodeError, hostNetcode, joinNetcode } from '../../src/netcode/index.js';
import type { NetcodeTransport } from '../../src/netcode/index.js';
import { simNetwork } from './sim.js';

const HOST = '00000000000000aa';
const ALICE = '00000000000000a1';
const LABEL = 'test.join';

interface Ship {
  readonly x: number;
}

/**
 * A player transport with a leaf-shaped `connectPeer`: `noAnnouncement`
 * until `announced()` is called, then `direct`. Until then `openStream`
 * refuses, as a leaf with no session to the host does.
 */
function leafLike(node: NetcodeTransport) {
  let announced = false;
  let attempts = 0;
  const transport: NetcodeTransport = {
    nodeIdHex: () => node.nodeIdHex(),
    onEvent: handler => node.onEvent(handler),
    connectPeer: async peer => {
      attempts += 1;
      return announced
        ? { type: 'direct', peer, dialog: '1' }
        : { type: 'noAnnouncement', peer, detail: 'no verified announcement for the peer' };
    },
    openStream: options => {
      if (!announced) throw new Error(`session: no session with 0x${options.peer ?? ''}`);
      return node.openStream(options);
    },
  };
  return {
    transport,
    announce: () => {
      announced = true;
    },
    attempts: () => attempts,
  };
}

describe('joinNetcode says why a join is not happening', () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  const host = (net: ReturnType<typeof simNetwork>) =>
    hostNetcode<Ship, never>({
      transport: net.node(HOST),
      label: LABEL,
      now: () => Date.now(),
      step: () => {},
      snapshot: () => ({ rock: { x: 1 } }),
    });

  it('a host this node never heard announce: ready() rejects `no-announcement`, and stats() shows the outcome', async () => {
    const net = simNetwork({ latencyMs: 20 });
    const server = host(net);
    const leaf = leafLike(net.node(ALICE));
    const player = joinNetcode<Ship, never>({ transport: leaf.transport, host: HOST, label: LABEL, now: () => Date.now(), joinTimeoutMs: 3_000 });
    const ready = player.ready();
    const settled = ready.then(
      () => null,
      (error: unknown) => error,
    );
    await vi.advanceTimersByTimeAsync(3_100);
    const error = await settled;
    expect(error).toBeInstanceOf(NetcodeError);
    expect((error as NetcodeError).kind).toBe('no-announcement');
    expect((error as NetcodeError).message).toContain(HOST);
    expect(player.stats().connectOutcome).toBe('noAnnouncement');
    expect(player.stats().lastError).toMatch(/no session/);
    expect(player.stats().snapshots).toBe(0);
    // It kept trying the whole time — once per ping, not once.
    expect(leaf.attempts()).toBeGreaterThan(5);
    player.close();
    server.close();
  });

  it('keeps trying after it gave up waiting: a late announcement still joins, and a new ready() resolves', async () => {
    const net = simNetwork({ latencyMs: 20 });
    const server = host(net);
    const leaf = leafLike(net.node(ALICE));
    const player = joinNetcode<Ship, never>({ transport: leaf.transport, host: HOST, label: LABEL, now: () => Date.now(), joinTimeoutMs: 1_000 });
    const first = player.ready().catch((error: unknown) => error);
    await vi.advanceTimersByTimeAsync(1_100);
    expect(((await first) as NetcodeError).kind).toBe('no-announcement');
    leaf.announce();
    const second = player.ready();
    await vi.advanceTimersByTimeAsync(800);
    await expect(second).resolves.toBeUndefined();
    expect(player.stats().connectOutcome).toBe('direct');
    expect(player.stats().lastError).toBeNull();
    expect(player.view().rock).toEqual({ x: 1 });
    player.close();
    server.close();
  });

  it('a reachable host that never answers: `timeout`', async () => {
    const net = simNetwork({ latencyMs: 20 });
    // No host on the other end: the stream opens, nothing comes back.
    const player = joinNetcode<Ship, never>({ transport: net.node(ALICE), host: HOST, label: LABEL, now: () => Date.now(), joinTimeoutMs: 2_000 });
    const settled = player.ready().catch((error: unknown) => error);
    await vi.advanceTimersByTimeAsync(2_100);
    expect(((await settled) as NetcodeError).kind).toBe('timeout');
    player.close();
  });

  it('a stream that will not open, with the host announced: `unreachable`', async () => {
    const net = simNetwork({ latencyMs: 20 });
    const node = net.node(ALICE);
    const transport: NetcodeTransport = {
      nodeIdHex: () => node.nodeIdHex(),
      onEvent: handler => node.onEvent(handler),
      connectPeer: async peer => ({ type: 'iceTimeout', peer, dialog: '1' }),
      openStream: () => {
        throw new Error('stream: refused');
      },
    };
    const player = joinNetcode<Ship, never>({ transport, host: HOST, label: LABEL, now: () => Date.now(), joinTimeoutMs: 1_000 });
    const settled = player.ready().catch((error: unknown) => error);
    await vi.advanceTimersByTimeAsync(1_100);
    const error = (await settled) as NetcodeError;
    expect(error.kind).toBe('unreachable');
    expect(error.message).toMatch(/stream: refused/);
    expect(player.stats().connectOutcome).toBe('iceTimeout');
    player.close();
  });

  it('a healthy join resolves ready(), before and after the first snapshot', async () => {
    const net = simNetwork({ latencyMs: 20 });
    const server = host(net);
    const player = joinNetcode<Ship, never>({ transport: net.node(ALICE), host: HOST, label: LABEL, now: () => Date.now() });
    const early = player.ready();
    await vi.advanceTimersByTimeAsync(500);
    await expect(early).resolves.toBeUndefined();
    await expect(player.ready()).resolves.toBeUndefined();
    expect(player.stats().connectOutcome).toBeNull(); // the sim has no connectPeer
    player.close();
    server.close();
  });

  it('close() before joining rejects a pending ready(), and any later one, as `closed`', async () => {
    const net = simNetwork({ latencyMs: 20 });
    const leaf = leafLike(net.node(ALICE));
    const player = joinNetcode<Ship, never>({ transport: leaf.transport, host: HOST, label: LABEL, now: () => Date.now() });
    const pending = player.ready().catch((error: unknown) => error);
    player.close();
    expect(((await pending) as NetcodeError).kind).toBe('closed');
    await expect(player.ready()).rejects.toMatchObject({ kind: 'closed' });
  });
});

describe('clock pings once the clock has settled', () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it('pings at pingRate until 8 samples, then at settledPingRate — and the clock stays right', async () => {
    const net = simNetwork({ latencyMs: 30, jitterMs: 4 });
    const server = hostNetcode<Ship, never>({
      transport: net.node(HOST),
      label: LABEL,
      now: () => Date.now() + 5_000,
      step: () => {},
      snapshot: () => ({}),
    });
    const node = net.node(ALICE);
    const pings: number[] = [];
    const counting: NetcodeTransport = {
      nodeIdHex: () => node.nodeIdHex(),
      onEvent: handler => node.onEvent(handler),
      openStream: options => {
        const inner = node.openStream(options) as { send(p: Uint8Array): unknown; close(): unknown };
        return {
          send: (payload: Uint8Array) => {
            if (JSON.parse(new TextDecoder().decode(payload)).k === 'p') pings.push(Date.now());
            return inner.send(payload);
          },
          close: () => inner.close(),
        };
      },
    };
    const start = Date.now();
    const player = joinNetcode<Ship, never>({ transport: counting, host: HOST, label: LABEL, now: () => Date.now() });
    await vi.advanceTimersByTimeAsync(20_000);
    const inSeconds = (from: number, to: number) => pings.filter(at => at - start >= from * 1000 && at - start < to * 1000).length;
    // Unsettled: 4/s (plus the one sent on open).
    expect(inSeconds(0, 1.5)).toBeGreaterThanOrEqual(5);
    // Settled: 1/s, not 4/s.
    expect(inSeconds(10, 20)).toBeGreaterThanOrEqual(9);
    expect(inSeconds(10, 20)).toBeLessThanOrEqual(11);
    const clock = player.stats().clock!;
    expect(clock.samples).toBeGreaterThanOrEqual(8);
    expect(Math.abs(clock.offsetMs - 5_000)).toBeLessThan(5);
    player.close();
    server.close();
  });
});
