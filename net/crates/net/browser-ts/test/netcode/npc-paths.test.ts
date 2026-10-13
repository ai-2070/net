// NPCs driven by paths in the store (sharded-match plan, N2): the mode
// version decides between snapshot and record, in whatever order they
// arrive; a fogged NPC never gets a record; only its owner writes one.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { shardHost } from '../../demo/sharded-arena/shard.js';
import type { ShardCombat, ShardHost } from '../../demo/sharded-arena/shard.js';
import {
  hostNetcode,
  joinNetcode,
  joinShards,
  npcModes,
  pathAction,
  pathAt,
  type MatchAssignment,
  type NetcodeTransport,
  type NpcPath,
  type ShardAssignment,
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
function arena(seed: number, options: { fogRadius?: number; npcs?: number; defaultClock?: boolean; refuse?: 'throw' | 'reject' } = {}) {
  // `defaultClock`: no `now` passed to the host, the client or npcModes — as a page runs.
  const clock = options.defaultClock === true ? {} : { now: () => Date.now() };
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
        // A store that refuses: the action throws, or its promise rejects.
        if (options.refuse === 'throw') throw new Error('refused');
        if (options.refuse === 'reject') return Promise.reject(new Error('refused'));
        written[npc] = path;
        // A reliable, ordered store: never earlier than the write before.
        storeAt = Math.max(storeAt, Date.now() + random() * 400);
        setTimeout(() => (records[npc] = path), storeAt - Date.now());
      },
    },
    ...clock,
  });
  // Where the host had the NPC, tick by tick — after its path is applied.
  const recorder = setInterval(() => {
    const npc = host.world()[NPC];
    truth.push({ t: Date.now(), x: npc?.x ?? Number.NaN, z: npc?.z ?? Number.NaN });
  }, 5);
  const client = joinNetcode<Npc, never>({ transport: net.node(P), host: A, label: LABEL, interpolationDelayMs: DELAY_MS, ...clock });
  const modes = npcModes<Npc>({ interpolationDelayMs: DELAY_MS, ...clock });
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
    // Points are on the HOST's clock.
    const at = host.now();
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

  it.each([
    [1, false],
    [2, false],
    [3, false],
    [4, false],
    [5, false],
    [1, true],
  ])('path → live → path under random store/snapshot reordering stays within tolerance, no jump (seed %i, default clock %s)', async (seed, defaultClock) => {
    const m = arena(seed, { defaultClock });
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
      m.host.setNpcPath(id, [{ t: m.host.now(), x, z: 0 }, { t: m.host.now() + 10_000, x: x + 200, z: 0 }]);
    }
    await vi.advanceTimersByTimeAsync(1_500); // the announce window
    before = m.sentBytes();
    await vi.advanceTimersByTimeAsync(2_000);
    const onPaths = (m.sentBytes() - before) / 2;
    console.info(`N2 upload, 50 NPCs, one viewer: live ${Math.round(live)} B/s, on paths ${Math.round(onPaths)} B/s`);
    expect(onPaths).toBeLessThan(live / 5);
    m.close();
  });

  it.each(['throw', 'reject'] as const)('a refused path write (%s) switches the NPC back to live: still sent, still drawn', async refuse => {
    const m = arena(1, { refuse });
    await vi.advanceTimersByTimeAsync(1_500);
    const attempt = (async () => m.host.setNpcPath(NPC, m.onward()))();
    await expect(attempt).rejects.toThrow('refused');
    // Back to live under a further mode version: a client that saw the
    // switch announced (mv 1, path) follows mv 2 back to the snapshots.
    expect(m.host.world()[NPC]).toMatchObject({ mv: 2, mode: 'live' });
    expect(m.host.combat().forwarded['path-refused']).toBe(1);
    // Past the announce window the host still sends it, and moves it live
    // (+x), not along the refused route (+z).
    await vi.advanceTimersByTimeAsync(2_000);
    const x = m.host.world()[NPC]!.x;
    const drawn = m.draw()[NPC];
    expect(drawn).toMatchObject({ mv: 2, mode: 'live' });
    expect(Math.abs(drawn!.x - x)).toBeLessThan(SPEED * 0.3);
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

describe('npcModes bookkeeping (SHARD-9)', () => {
  interface Hp extends Npc {
    readonly hp: number;
  }
  const path = { mv: 1, mode: 'path' as const, key: null, points: [{ t: 0, x: 0, z: 0 }, { t: 10_000, x: 0, z: 100 }] };

  it('forgets an NPC neither source shows any more', () => {
    let at = 0;
    const modes = npcModes<Npc>({ now: () => at, carryMs: 500, smoothingMs: 100 });
    for (let i = 0; i < 50; i += 1) modes.resolve({ [`npc-${i}`]: { x: 0, z: 0, mv: 0, mode: 'live' } }, {}, () => at);
    expect(modes.stats().tracks).toBe(50);
    at = 1_000;
    modes.resolve({}, {}, () => at);
    expect(modes.stats().tracks).toBe(0);
  });

  it('a record whose owner clock is not known yet is held, not counted as carried', () => {
    let at = 0;
    const modes = npcModes<Npc>({ now: () => at, smoothingMs: 0 });
    modes.resolve({ [NPC]: { x: 3, z: 0, mv: 0, mode: 'live' } }, {}, () => at);
    at = 50;
    const drawn = modes.resolve({}, { [NPC]: path }, () => null)[NPC];
    expect(drawn).toMatchObject({ x: 3, z: 0 });
    expect(modes.stats().carried).toBe(0);
    // The clock arrives: drawn from the path.
    at = 100;
    expect(modes.resolve({}, { [NPC]: path }, () => 1_000)[NPC]).toMatchObject({ x: 0, mv: 1, mode: 'path' });
  });

  it('in the announce window, the path pose is built on the fresh entry (hp and all), not the last live one', () => {
    let at = 0;
    const modes = npcModes<Hp>({ now: () => at, smoothingMs: 0 });
    modes.resolve({ [NPC]: { x: 0, z: 0, hp: 100, mv: 0, mode: 'live' } }, {}, () => at);
    at = 50;
    // The switch, announced: it was hit since (hp 60). The record is here too.
    const drawn = modes.resolve({ [NPC]: { x: 0, z: 0.5, hp: 60, mv: 1, mode: 'path' } }, { [NPC]: path }, () => 1_000)[NPC];
    expect(drawn).toMatchObject({ hp: 60, mv: 1, mode: 'path' });
    // After the window the entry is gone: still the freshest state seen.
    at = 100;
    expect(modes.resolve({}, { [NPC]: path }, () => 1_100)[NPC]).toMatchObject({ hp: 60 });
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

  it('finds the owner by exact id: a string `npcs` is no substring match, a prototype key no record', () => {
    // `npcs: 'npc-12'` (not an array) must not make H0 the owner of npc-1.
    const stringNpcs = { ...assignment, shards: { ...assignment.shards, s0: { host: H0, squads: [], npcs: 'npc-12' } } } as unknown as MatchAssignment;
    expect(() => run(H0, { npc: NPC, path }, { assignment: stringNpcs, paths: {} })).toThrow(/only the host/);
    // An NPC called `constructor` holds no record until one is written.
    const named = { ...assignment, shards: { ...assignment.shards, s0: { host: H0, squads: [], npcs: ['constructor'] } } };
    expect(run(H0, { npc: 'constructor', path }, { assignment: named, paths: {} }).out).toEqual({ mv: 1 });
  });
});

// SHARD-2: a path NPC leaves players' snapshots, never the fight. Host A
// owns NPC (on a path); host B owns player P.
describe('a path NPC stays in the fight', () => {
  const B = '00000000000000b0';
  interface Fighter extends Npc {
    readonly hp: number;
  }
  interface Shot {
    readonly fire?: { readonly target: string; readonly seenB: number };
  }
  const combat: ShardCombat<Fighter> = {
    fire: shooter => (shooter.hp > 0 ? shooter : 'dead'),
    hits: (shooter, target) => Math.hypot(shooter.x - target.x, shooter.z - target.z) <= 30,
    damage: target => ({ ...target, hp: target.hp - 10 }),
    maxSpeed: 40,
  };

  function fight(npcHp = 100) {
    const net = simNetwork({ latencyMs: 25, jitterMs: 10 });
    let assignment: ShardAssignment = { epoch: 1, hosts: { s0: A, s1: B }, owner: { [NPC]: 's0', [P]: 's1' } };
    const records: Record<string, NpcPath> = {};
    let npcShots = 0;
    const host = (id: string, shard: string, initial: Record<string, Fighter>) =>
      shardHost<Fighter, Shot>(lib, {
        transport: net.node(id),
        label: LABEL,
        assignment,
        shard,
        roster: [P],
        initial,
        apply: body => body,
        combat,
        advance: (entities, ai) => {
          const next: Record<string, Fighter> = {};
          // A dead NPC leaves the world.
          for (const [entity, body] of Object.entries(entities)) if (entity !== NPC || body.hp > 0) next[entity] = body;
          if (next[NPC] !== undefined && npcShots > 0 && ai.known()[P] !== undefined) {
            npcShots -= 1;
            ai.fire(NPC, P);
          }
          return next;
        },
        paths: { write: (npc, path) => void (records[npc] = path), read: npc => records[npc], announceMs: 300 },
      });
    const a = host(A, 's0', { [NPC]: { x: 10, z: 0, hp: npcHp, mv: 0, mode: 'live' } });
    const b = host(B, 's1', { [P]: { x: 0, z: 0, hp: 100 } });
    const player = joinShards<Fighter, Shot>({ transport: net.node(P), label: LABEL, assignment, local: { id: P, predict: body => body } });
    const modes = npcModes<Fighter>();
    const resolveNpc = () => modes.resolve(player.view(), records, () => player.hostNow('s0'))[NPC];
    // The render loop: npcModes follows the NPC frame by frame.
    let drawn: Fighter | undefined;
    const frames = setInterval(() => (drawn = resolveNpc()), 16);
    const draw = () => drawn;
    const shootNpc = () => player.input({ fire: { target: NPC, seenB: player.hostNow('s0')! - 100 } });
    const onPath = () => {
      const at = a.now();
      // Stands still on its route: within range of P whatever the timing.
      a.setNpcPath(NPC, [{ t: at, x: 10, z: 0 }, { t: at + 60_000, x: 10, z: 1 }]);
    };
    const lose = () => {
      for (const other of [B, P]) {
        net.block(A, other, 60_000);
        net.block(other, A, 60_000);
      }
    };
    const reassign = () => {
      assignment = { ...assignment, epoch: 2, owner: { ...assignment.owner, [NPC]: 's1' } };
      b.setAssignment(assignment);
      player.setAssignment(assignment);
    };
    const close = () => {
      clearInterval(frames);
      player.close();
      a.close();
      b.close();
    };
    return { net, a, b, player, records, draw, shootNpc, onPath, fireNpc: () => (npcShots += 1), lose, reassign, close };
  }

  beforeEach(() => {
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it('is hit, and shoots, after its switch is no longer announced', async () => {
    const m = fight();
    await vi.advanceTimersByTimeAsync(2_000);
    m.onPath();
    await vi.advanceTimersByTimeAsync(1_000); // past announceMs: players no longer get it
    expect(m.player.view()[NPC]).toBeUndefined();
    m.shootNpc();
    m.fireNpc();
    await vi.advanceTimersByTimeAsync(1_000);
    expect(m.a.combat().served).toEqual({ hit: 1 });
    expect(m.b.combat().served).toEqual({ hit: 1 });
    expect(m.a.world()[NPC]!.hp).toBe(90);
    expect(m.b.world()[P]!.hp).toBe(90);
    m.close();
  });

  it('is restored live when its host is lost', async () => {
    const m = fight();
    await vi.advanceTimersByTimeAsync(2_000);
    m.onPath();
    await vi.advanceTimersByTimeAsync(1_000);
    m.lose();
    await vi.advanceTimersByTimeAsync(200);
    m.reassign();
    expect(m.b.recovered()).toEqual([NPC]);
    // Back live under a new mode version, and so is its record: no client
    // draws the lost host's route.
    expect(m.b.world()[NPC]).toMatchObject({ mode: 'live', mv: 2 });
    expect(m.records[NPC]).toMatchObject({ mode: 'live', mv: 2, points: [] });
    await vi.advanceTimersByTimeAsync(1_000);
    expect(m.draw()).toMatchObject({ mode: 'live', mv: 2 });
    m.close();
  });

  it('has its record retired when its host is lost and nobody observed it', async () => {
    const m = fight();
    // B never hears A: it has nothing to restore the NPC from.
    m.net.block(A, B, 60_000);
    await vi.advanceTimersByTimeAsync(2_000);
    m.onPath();
    await vi.advanceTimersByTimeAsync(1_000);
    m.lose();
    m.reassign();
    expect(m.b.recovered()).toEqual([]);
    expect(m.b.combat().forwarded.unrecovered).toBe(1);
    // The record no longer says `path`: clients stop drawing its route.
    expect(m.records[NPC]).toMatchObject({ mode: 'live', mv: 2, points: [] });
    await vi.advanceTimersByTimeAsync(1_000);
    expect(m.draw()).toBeUndefined();
    m.close();
  });

  it('has its record retired when it dies, and clients stop drawing it', async () => {
    const m = fight(10);
    await vi.advanceTimersByTimeAsync(2_000);
    m.onPath();
    await vi.advanceTimersByTimeAsync(1_000);
    expect(m.draw()).toBeDefined();
    m.shootNpc();
    await vi.advanceTimersByTimeAsync(500);
    expect(m.a.world()[NPC]).toBeUndefined();
    expect(m.records[NPC]).toMatchObject({ mode: 'live', mv: 2, points: [] });
    await vi.advanceTimersByTimeAsync(1_000);
    expect(m.draw()).toBeUndefined();
    m.close();
  });
});
