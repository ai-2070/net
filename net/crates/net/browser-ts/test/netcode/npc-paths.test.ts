// NPCs driven by paths in the store (sharded-match plan, N2): the mode
// version decides between snapshot and record, in whatever order they
// arrive; a fogged NPC never gets a record; only its owner writes one.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { shardHost } from '../../demo/sharded-arena/shard.js';
import type { ShardHost } from '../../demo/sharded-arena/shard.js';
import {
  hostNetcode,
  joinNetcode,
  npcModes,
  pathAction,
  pathAt,
  type MatchAssignment,
  type NetcodeTransport,
  type NpcPath,
} from '../../src/netcode/index.js';
import { assertHidden } from '../../src/store/visibility.js';
import { handoffLink, regionHandoffs, regionState } from '../../src/world/index.js';
import { simNetwork } from './sim.js';

const lib = { hostNetcode, joinNetcode, handoffLink, regionHandoffs, regionState, pathAt };
const LABEL = 'arena.move';
const A = '00000000000000a0';
const P = '00000000000000e1';
const NPC = 'npc-1';
const DELAY_MS = 100;
/** The NPC walks +1 x a tick at 20 Hz: 20 units/s, live or on its path. */
const SPEED = 20;

interface Npc {
  readonly x: number;
  readonly z: number;
  readonly mv?: number;
  readonly mode?: 'path' | 'live';
}

/** One host owning NPC; a spectator; the store replicated to it, in order, after a random delay. */
function arena(seed: number, options: { fogRadius?: number; npcs?: number } = {}) {
  const net = simNetwork({ latencyMs: 30, jitterMs: 20, lossRate: 0.05, seed });
  let r = seed * 7919;
  const random = () => ((r = (r * 16807) % 2147483647) / 2147483647);
  const records: Record<string, NpcPath> = {};
  const written: Record<string, NpcPath> = {};
  let storeAt = 0;
  const npcIds = Array.from({ length: options.npcs ?? 1 }, (_, i) => (i === 0 ? NPC : `npc-${i + 1}`));
  // Bytes the host sends, for the upload comparison.
  let sentBytes = 0;
  const raw = net.node(A);
  const counted: NetcodeTransport = {
    ...raw,
    openStream: async opts => {
      const stream = await raw.openStream(opts);
      return { send: (payload: Uint8Array) => ((sentBytes += payload.length), stream.send(payload)), close: () => stream.close() };
    },
  } as NetcodeTransport;
  const truth: { t: number; x: number; z: number }[] = [];
  const host: ShardHost<Npc> = shardHost<Npc, never>(lib, {
    transport: counted as never,
    label: LABEL,
    assignment: { epoch: 1, hosts: { s0: A }, owner: Object.fromEntries(npcIds.map(id => [id, 's0'])) },
    shard: 's0',
    roster: [P],
    initial: Object.fromEntries(npcIds.map((id, i) => [id, { x: 0, z: i, mv: 0, mode: 'live' as const }])),
    apply: body => body,
    advance: entities => {
      const next: Record<string, Npc> = {};
      for (const [id, npc] of Object.entries(entities)) next[id] = { ...npc, x: npc.x + 1 };
      return next;
    },
    ...(options.fogRadius === undefined ? {} : { fogRadius: options.fogRadius }),
    paths: {
      write: (npc, path) => {
        written[npc] = path;
        // A reliable, ordered store: never earlier than the write before.
        storeAt = Math.max(storeAt, Date.now() + random() * 400);
        setTimeout(() => (records[npc] = path), storeAt - Date.now());
      },
    },
    now: () => Date.now(),
  });
  // Where the host had the NPC, tick by tick — after its path is applied.
  const recorder = setInterval(() => {
    const npc = host.world()[NPC];
    truth.push({ t: Date.now(), x: npc?.x ?? Number.NaN, z: npc?.z ?? Number.NaN });
  }, 5);
  const client = joinNetcode<Npc, never>({ transport: net.node(P), host: A, label: LABEL, interpolationDelayMs: DELAY_MS, now: () => Date.now() });
  const modes = npcModes<Npc>({ interpolationDelayMs: DELAY_MS, now: () => Date.now() });
  const draw = () => modes.resolve(client.view(), records, () => client.hostNow());
  const truthAt = (t: number) => {
    let best = truth[0];
    for (const sample of truth) if (sample.t <= t) best = sample;
    return best ?? { x: Number.NaN, z: Number.NaN };
  };
  /**
   * A path from where the NPC is now that TURNS: live it walks +x, on the
   * path +z. Drawing the wrong source shows as distance, not as nothing.
   */
  const onward = () => {
    const at = Date.now();
    const { x, z } = host.world()[NPC]!;
    return [
      { t: at, x, z },
      { t: at + 10_000, x, z: z + SPEED * 10 },
    ];
  };
  const close = () => {
    clearInterval(recorder);
    client.close();
    host.close();
  };
  return { host, client, modes, draw, truthAt, onward, records, written, sentBytes: () => sentBytes, close };
}

describe('NPC paths', () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it.each([1, 2, 3, 4, 5])('path → live → path under random store/snapshot reordering stays within tolerance, no jump (seed %i)', async seed => {
    const m = arena(seed);
    await vi.advanceTimersByTimeAsync(1_500);
    // One tick plus the snapshot jitter of motion, and the frame budget:
    // what interpolation already allows a live NPC.
    const tolerance = (SPEED * (50 + 20 + 50)) / 1000;
    // What interpolation itself does between frames: a frame's motion,
    // plus one tick caught up after a late snapshot (measured: up to ~1.0
    // while plainly live). A source switch must stay inside it.
    const step = (SPEED * (10 + 50)) / 1000;
    let worst = 0;
    let worstJump = 0;
    let previous: { x: number; z: number } | null = null;
    const sample = async (ms: number) => {
      for (let t = 0; t < ms; t += 10) {
        await vi.advanceTimersByTimeAsync(10);
        const drawn = m.draw()[NPC];
        expect(drawn, 'the NPC is always drawn').toBeDefined();
        const truth = m.truthAt(Date.now() - DELAY_MS);
        worst = Math.max(worst, Math.hypot(drawn!.x - truth.x, drawn!.z - truth.z));
        if (previous !== null) worstJump = Math.max(worstJump, Math.hypot(drawn!.x - previous.x, drawn!.z - previous.z));
        previous = { x: drawn!.x, z: drawn!.z };
      }
    };
    await sample(800);
    const liveJump = worstJump;
    m.host.setNpcPath(NPC, m.onward());
    await sample(2_500); // past the announce window: snapshots no longer carry it
    m.host.setNpcLive(NPC);
    await sample(1_500);
    m.host.setNpcPath(NPC, m.onward());
    await sample(2_000);
    expect(m.records[NPC]?.mv).toBe(3);
    console.info(`seed ${seed}: plainly live jump ${liveJump.toFixed(3)}, overall jump ${worstJump.toFixed(3)}, worst error ${worst.toFixed(3)}`);
    expect(worst).toBeLessThanOrEqual(tolerance);
    expect(worstJump).toBeLessThanOrEqual(step);
    m.close();
  });

  it('saves the upload of NPCs on a path once the switch has been announced', async () => {
    const m = arena(1, { npcs: 50 });
    await vi.advanceTimersByTimeAsync(1_500);
    let before = m.sentBytes();
    await vi.advanceTimersByTimeAsync(2_000);
    const live = (m.sentBytes() - before) / 2;
    for (const id of Object.keys(m.host.world())) {
      const x = m.host.world()[id]!.x;
      m.host.setNpcPath(id, [{ t: Date.now(), x, z: 0 }, { t: Date.now() + 10_000, x: x + 200, z: 0 }]);
    }
    await vi.advanceTimersByTimeAsync(1_500); // the announce window
    before = m.sentBytes();
    await vi.advanceTimersByTimeAsync(2_000);
    const onPaths = (m.sentBytes() - before) / 2;
    console.info(`N2 upload, 50 NPCs, one viewer: live ${Math.round(live)} B/s, on paths ${Math.round(onPaths)} B/s`);
    expect(onPaths).toBeLessThan(live / 5);
    m.close();
  });

  it('never gives an NPC under fog a path record', async () => {
    const m = arena(1, { fogRadius: 50 });
    await vi.advanceTimersByTimeAsync(500);
    expect(() => m.host.setNpcPath(NPC, m.onward())).toThrow(/fog/);
    await vi.advanceTimersByTimeAsync(500);
    // Nothing a player could read: the store holds no record of it.
    assertHidden({}, { paths: m.written }, { peer: P, audience: [] }, [`paths.${NPC}`]);
    m.close();
  });
});

describe('npcModes, when a path is due and its record is not', () => {
  it('carries the last live motion forward for carryMs, then holds; the late record then takes over', () => {
    let at = 0;
    const modes = npcModes<Npc>({ now: () => at, carryMs: 500, smoothingMs: 0 });
    const clock = () => at;
    let drawn: Npc | undefined;
    // Live, walking +x at 20 units/s.
    for (; at <= 100; at += 10) drawn = modes.resolve({ [NPC]: { x: at * 0.02, z: 0, mv: 0, mode: 'live' } }, {}, clock)[NPC];
    expect(drawn!.x).toBeCloseTo(2, 6);
    // One snapshot announces the switch (mv 1, path) …
    drawn = modes.resolve({ [NPC]: { x: 2.2, z: 0, mv: 1, mode: 'path' } }, {}, clock)[NPC];
    expect(drawn!.x).toBeCloseTo(2.2, 6);
    // … then the entry is gone, and the record has not arrived: carried,
    // never dropped back to the stale mv 0 `live` it last held.
    const stale = { [NPC]: { mv: 0, mode: 'live' as const, key: null, points: [] } };
    at = 400;
    drawn = modes.resolve({}, stale, clock)[NPC];
    expect(drawn!.x).toBeCloseTo(2 + 0.02 * 300, 6);
    at = 900;
    drawn = modes.resolve({}, stale, clock)[NPC];
    expect(drawn!.x).toBeCloseTo(2 + 0.02 * 500, 6); // held at carryMs
    expect(modes.stats().carried).toBe(1);
    // The record lands: the path, at the owner's clock less the delay.
    const record = { [NPC]: { mv: 1, mode: 'path' as const, key: null, points: [{ t: 0, x: 0, z: 0 }, { t: 2_000, x: 0, z: 40 }] } };
    drawn = modes.resolve({}, record, clock)[NPC];
    expect(drawn).toMatchObject({ x: 0, mv: 1, mode: 'path' });
    expect(drawn!.z).toBeCloseTo(((900 - 100) / 2_000) * 40, 6);
  });
});

describe('the path record (D8)', () => {
  const H0 = '00000000000000a0';
  const H1 = '00000000000000a1';
  const assignment: MatchAssignment = {
    epoch: 1,
    shards: { s0: { host: H0, squads: [], npcs: [NPC] }, s1: { host: H1, squads: [] } },
    squads: {},
    leases: { s0: 0, s1: 0 },
    renewals: 0,
  };
  interface Room {
    readonly assignment: MatchAssignment;
    readonly paths: Record<string, NpcPath>;
  }
  const run = (peer: string, input: { npc: string; path: NpcPath }, state: Room) => {
    let next = state;
    const out = pathAction<Room>({ assignment: s => s.assignment })(input, {
      peer,
      getState: () => next,
      setState: update => (next = { ...next, ...update }),
    });
    return { out, next };
  };
  const path: NpcPath = { mv: 1, mode: 'path', key: null, points: [{ t: 0, x: 0, z: 0 }, { t: 10, x: 1, z: 0 }] };
  const empty: Room = { assignment, paths: {} };

  it('is written only by the host that owns the NPC, with a rising mv and a valid path', () => {
    expect(run(H0, { npc: NPC, path }, empty).next.paths[NPC]).toEqual(path);
    expect(() => run(H1, { npc: NPC, path }, empty)).toThrow(/only the host/);
    const held = run(H0, { npc: NPC, path }, empty).next;
    expect(() => run(H0, { npc: NPC, path }, held)).toThrow(/not above/);
    const backwards = { ...path, mv: 2, points: [{ t: 10, x: 0, z: 0 }, { t: 0, x: 1, z: 0 }] };
    expect(() => run(H0, { npc: NPC, path: backwards }, held)).toThrow(/back in time/);
  });
});
