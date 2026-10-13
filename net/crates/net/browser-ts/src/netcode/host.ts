/**
 * `hostNetcode` — the authoritative side of netcode model 2: a fixed-rate
 * tick loop that applies each player's inputs, broadcasts snapshots on the
 * lossy carrier, answers clock pings, and keeps a short history for lag
 * compensation.
 *
 * ```ts
 * const net = hostNetcode({
 *   transport: node,               // or meshStoreTransport(mesh) on a dedicated host
 *   label: 'my-game.movement',
 *   tickRate: 30,
 *   step: ({ inputs }) => {        // apply this tick's inputs to your world
 *     for (const [peer, list] of inputs) for (const { data } of list) move(world, peer, data);
 *   },
 *   snapshot: () => world.ships,   // entity id → state, sent to every player
 * });
 * ```
 */

import {
  type Frame,
  type NetcodeStream,
  type NetcodeTransport,
  type Now,
  MAX_SNAPSHOT_CHUNKS,
  chunkOf,
  defaultNow,
  decodeFrame,
  encodeFrame,
  eventPeer,
} from './wire.js';
import { type EntityCodec, encodeBinarySnapshot, nameBytes } from './codec.js';

/** One input as `step` receives it. */
export interface TickInput<I> {
  readonly seq: number;
  /** The host time the player was rendering when it issued the input. */
  readonly seen: number;
  readonly data: I;
}

/** What `step` gets each tick. */
export interface TickContext<I> {
  readonly tick: number;
  /** Host clock, ms, at this tick. */
  readonly time: number;
  /** ms since the previous tick (1000 / tickRate, nominally). */
  readonly dtMs: number;
  /** Each player's inputs since the last tick, oldest first, each applied
   * exactly once however many times the lossy carrier repeated it. */
  readonly inputs: ReadonlyMap<string, readonly TickInput<I>[]>;
}

/** What {@link HostNetcode.rewind} returns. */
export interface Rewound<E> {
  /** The entities as of the returned `time`. */
  readonly entities: Readonly<Record<string, E>>;
  /** The host time rewound to — `seen`, unless the cap clamped it. */
  readonly time: number;
  /** `true` when `seen` was further back than `maxRewindMs` allows. */
  readonly clamped: boolean;
}

/** {@link hostNetcode}'s argument. */
export interface HostNetcodeOptions<E, I> {
  readonly transport: NetcodeTransport;
  /** Names this netcode instance; players join the same label. */
  readonly label: string;
  /** Ticks per second. Default 30. */
  readonly tickRate?: number;
  /** Advance the world one tick. */
  step(context: TickContext<I>): void;
  /** The world as players should see it: entity id → state. */
  snapshot(tick: number): Readonly<Record<string, E>>;
  /** Which entities `peer` receives. Default: all. */
  visible?(peer: string, id: string, entity: E): boolean;
  /**
   * Interest management: the key an entity is found under (a grid cell, a
   * room; `null`: always delivered). A player that states interest keys
   * (`joinNetcode({ interest })` / `setInterest`) then receives only the
   * entities whose key it named — after `visible`, which stays the
   * permission; interest is only a filter. A player that states none
   * receives everything `visible` allows.
   */
  interest?(id: string, entity: E): string | null;
  /** Admit `peer` as a player. Default: everyone. */
  authorize?(peer: string): boolean;
  /**
   * How far back {@link HostNetcode.rewind} may go, in ms. Default 200 —
   * lag compensation that trusts any claimed latency lets a player buy an
   * advantage by faking lag.
   */
  readonly maxRewindMs?: number;
  /**
   * Largest snapshot frame, bytes. Default 8000 — under one event, so a
   * snapshot never rides as fragments (on the lossy carrier one lost
   * fragment loses them all). A larger snapshot is sent as independent
   * chunks; a lost chunk costs the entities it carried, for one tick.
   */
  readonly maxFrameBytes?: number;
  /** Drop a player silent this long, ms. Default 5000. */
  readonly playerTimeoutMs?: number;
  /** `false` to drive ticks yourself with {@link HostNetcode.tick}. Default `true`. */
  readonly autoTick?: boolean;
  /**
   * Compact snapshots (`codec.ts`): players whose `joinNetcode` has a codec
   * with the same fingerprint receive binary snapshots; everyone else JSON.
   * An entity the codec refuses sends that player JSON for that tick
   * (`dropped['codec-write-failed']`).
   */
  readonly codec?: EntityCodec<E>;
  /**
   * Sharded matches (`joinShards`): the assignment epoch this host is at,
   * read every tick. Each snapshot carries it (`ep`), so a player draws
   * this host's entities only at the epoch it has installed; an input
   * frame stamped with another epoch is dropped (`dropped['stale-epoch']`).
   * An input frame with no epoch is applied, as from any plain player.
   */
  epoch?(): number;
  /**
   * Sharded matches: whether this host still holds its lease (`leaseWatch`
   * in `assignment.ts`), asked every tick. While `false` the host neither
   * simulates nor sends — a host cut off from the lobby must stop before
   * the lobby gives its entities to another — and inputs that arrive are
   * dropped (`dropped['lease-lost']`), not saved for later. Clock pings are
   * still answered.
   */
  live?(): boolean;
  readonly now?: Now;
}

/** The running host. */
export interface HostNetcode<E> {
  /** Players currently connected, 16-hex peer ids. */
  players(): readonly string[];
  /** The tick about to run. */
  readonly currentTick: number;
  /** Run one tick now (what the auto loop calls). */
  tick(): void;
  /**
   * Lag compensation: the entities as a player saw them at host time
   * `seen` (from {@link TickInput.seen}), from this host's own history —
   * never further back than `maxRewindMs` before now.
   */
  rewind(seen: number): Rewound<E>;
  /**
   * Stop taking `peer`'s inputs, and say how far they were applied: the
   * half of a planned move (sharded matches, S5) that pairs with the
   * entity's frozen state. Call it between ticks — the state you freeze in
   * the same synchronous turn then contains exactly inputs `1..lastApplied`.
   * Later inputs from `peer` are dropped unapplied and unacknowledged
   * (`dropped['released']`), so the player keeps them for the new owner.
   */
  releasePlayer(peer: string): { readonly lastApplied: number };
  /**
   * Take `peer`'s inputs from `lastApplied + 1` on: the other half of a
   * planned move. Inputs at or below it — already in the state the entity
   * arrived with — are never applied here.
   *
   * `{ resume: true }` is the crash-recovery form (S6), for an entity
   * restored from an observation with no boundary to hand over: the first
   * input frame from `peer` sets the boundary at its newest input and none
   * of it is applied. The player's pending inputs are dropped (they were
   * made against a state that was rolled back) and it carries on from there.
   */
  adoptPlayer(peer: string, from: { readonly lastApplied: number } | { readonly resume: true }): void;
  /** Frames dropped and why. */
  readonly dropped: Readonly<Record<string, number>>;
  /** The last error a send or stream open raised, for diagnosis; `null` if none. */
  readonly lastSendError: string | null;
  close(): void;
}

interface Player<I> {
  stream: NetcodeStream | null;
  opening: boolean;
  lastApplied: number;
  pending: Map<number, TickInput<I>>;
  lastHeard: number;
  /** The interest keys it stated (`null`: none, so everything) and their version. */
  interest: ReadonlySet<string> | null;
  interestVersion: number;
  /** Its codec fingerprint matches the host's: it gets binary snapshots. */
  binary: boolean;
  /** Binary indexes whose name it has been sent at least once. */
  named: Set<number>;
  /** Indexes it asked to have named again (`u` frames). */
  requested: Set<number>;
  /** Released by a planned move: inputs are not taken until adopted again. */
  released: boolean;
  /** Adopted after a crash: the next input frame sets the boundary, and none of it is applied. */
  resuming: boolean;
}

/** History kept: enough ticks to cover the rewind cap with room. */
function historySize(tickRate: number, maxRewindMs: number): number {
  return Math.max(8, Math.ceil((maxRewindMs / 1000) * tickRate) + 4);
}

/**
 * One snapshot as frames: one, or — over `maxBytes` — chunks by
 * {@link chunkOf}, as many as it takes for each to fit (up to
 * {@link MAX_SNAPSHOT_CHUNKS}; past that the chunks are sent over-size and
 * ride as fragments).
 */
export function snapshotFrames<E>(
  label: string,
  tick: number,
  time: number,
  ack: number,
  entities: Readonly<Record<string, E>>,
  maxBytes: number,
  interestVersion = 0,
  epoch?: number,
): Frame<E, never>[] {
  // The interest version the view was filtered by, echoed so the player
  // stops repeating its `w` frame; and the epoch, when the host has one.
  const iv = {
    ...(interestVersion > 0 ? { iv: interestVersion } : {}),
    ...(epoch === undefined ? {} : { ep: epoch }),
  };
  const whole: Frame<E, never> = { n: label, k: 's', tick, t: time, ack, e: entities, ...iv };
  const size = encodeFrame(whole).length;
  if (size <= maxBytes) return [whole];
  const ids = Object.keys(entities);
  let of = Math.min(MAX_SNAPSHOT_CHUNKS, Math.max(2, Math.ceil(size / (maxBytes * 0.75))));
  for (;;) {
    const parts: Record<string, E>[] = Array.from({ length: of }, () => ({}));
    for (const id of ids) parts[chunkOf(id, of)]![id] = entities[id]!;
    const frames = parts.map(
      (e, c): Frame<E, never> => ({ n: label, k: 's', tick, t: time, ack, e, c, of, ...iv }),
    );
    if (of >= MAX_SNAPSHOT_CHUNKS || frames.every(frame => encodeFrame(frame).length <= maxBytes)) {
      return frames;
    }
    of = Math.min(MAX_SNAPSHOT_CHUNKS, of * 2);
  }
}

/** Start the authoritative netcode host. */
export function hostNetcode<E, I>(options: HostNetcodeOptions<E, I>): HostNetcode<E> {
  const now = options.now ?? defaultNow;
  const tickRate = options.tickRate ?? 30;
  if (!(tickRate > 0 && tickRate <= 240)) throw new RangeError('tickRate must be in (0, 240]');
  const tickMs = 1000 / tickRate;
  const maxRewindMs = options.maxRewindMs ?? 200;
  const maxFrameBytes = Math.max(256, options.maxFrameBytes ?? 8000);
  const playerTimeoutMs = options.playerTimeoutMs ?? 5000;
  const players = new Map<string, Player<I>>();
  const history: { tick: number; time: number; entities: Readonly<Record<string, E>> }[] = [];
  const keep = historySize(tickRate, maxRewindMs);
  const dropped: Record<string, number> = {};
  const drop = (why: string) => {
    dropped[why] = (dropped[why] ?? 0) + 1;
  };
  let tick = 0;
  let lastError: string | null = null;
  let lastTickAt = now();
  let closed = false;

  const send = (peer: string, player: Player<I>, frame: Frame<E, I>) => sendBytes(peer, player, encodeFrame(frame));
  const sendBytes = (peer: string, player: Player<I>, payload: Uint8Array) => {
    if (player.stream === null) {
      if (!player.opening) {
        player.opening = true;
        Promise.resolve(
          options.transport.openStream({ reliability: 'fireAndForget', peer, label: options.label, lossy: true }),
        )
          .then(stream => {
            if (closed || !players.has(peer)) {
              stream.close();
              return;
            }
            player.stream = stream;
          })
          .catch(error => {
            lastError = String((error as Error)?.message ?? error);
            drop('stream-open-failed');
          })
          .finally(() => {
            player.opening = false;
          });
      }
      drop('stream-not-open');
      return;
    }
    // A failed send retires the stream: the session under it may have
    // been replaced (a reconnect, a relayed → direct upgrade), and a stream
    // handle is fenced to the session it was opened on — every later send
    // on it would fail too. The next send reopens on the current session.
    const stream = player.stream;
    const retire = (error?: unknown) => {
      lastError = String((error as Error)?.message ?? error);
      drop('send-failed');
      if (player.stream === stream) {
        player.stream = null;
        try {
          stream.close();
        } catch {
          // Already unusable.
        }
      }
    };
    try {
      void Promise.resolve(stream.send(payload)).catch(retire);
    } catch (error) {
      retire(error);
    }
  };

  const playerFor = (peer: string): Player<I> | null => {
    const existing = players.get(peer);
    if (existing) return existing;
    if (options.authorize && !options.authorize(peer)) {
      drop('unauthorized');
      return null;
    }
    const player: Player<I> = {
      stream: null,
      opening: false,
      lastApplied: 0,
      pending: new Map(),
      lastHeard: now(),
      interest: null,
      interestVersion: 0,
      binary: false,
      named: new Set(),
      requested: new Set(),
      released: false,
      resuming: false,
    };
    players.set(peer, player);
    return player;
  };

  // Binary ids: an index per entity id, in order of first appearance, never
  // reused while this host runs — so a stale snapshot cannot name the wrong
  // entity.
  const codec = options.codec;
  const indexOf = new Map<string, number>();
  const nameOf: Uint8Array[] = [];
  const indexFor = (id: string): number => {
    let index = indexOf.get(id);
    if (index === undefined) {
      index = nameOf.length;
      indexOf.set(id, index);
      nameOf.push(nameBytes(id));
    }
    return index;
  };
  const noteFingerprint = (player: Player<I>, fingerprint: string | undefined) => {
    if (codec !== undefined) player.binary = fingerprint === codec.fingerprint;
  };

  /** One player's view as binary frames, chunked to fit; names it needs included. */
  const binaryFrames = (
    player: Player<I>,
    tick: number,
    time: number,
    view: Readonly<Record<string, E>>,
    epoch: number | undefined,
  ): Uint8Array[] => {
    const header = {
      tick,
      t: time,
      ack: player.lastApplied,
      ...(player.interestVersion > 0 ? { iv: player.interestVersion } : {}),
      ...(epoch === undefined ? {} : { ep: epoch }),
    };
    const ids = Object.keys(view);
    const named = (index: number) => player.named.has(index) && !player.requested.has(index);
    const build = (of: number): Uint8Array[] => {
      const parts = Array.from({ length: of }, () => ({ names: new Map<number, Uint8Array>(), entities: new Map<number, E>() }));
      for (const id of ids) {
        const index = indexFor(id);
        const part = parts[of === 1 ? 0 : chunkOf(id, of)]!;
        part.entities.set(index, view[id]!);
        if (!named(index)) part.names.set(index, nameOf[index]!);
      }
      return parts.map((part, c) =>
        encodeBinarySnapshot(options.label, of === 1 ? header : { ...header, c, of }, part.names, part.entities, codec!),
      );
    };
    let frames = build(1);
    if (frames[0]!.length > maxFrameBytes) {
      let of = Math.min(MAX_SNAPSHOT_CHUNKS, Math.max(2, Math.ceil(frames[0]!.length / (maxFrameBytes * 0.75))));
      for (;;) {
        frames = build(of);
        if (of >= MAX_SNAPSHOT_CHUNKS || frames.every(frame => frame.length <= maxFrameBytes)) break;
        of = Math.min(MAX_SNAPSHOT_CHUNKS, of * 2);
      }
    }
    // Built: their names count as sent. A lost one is asked for again.
    for (const id of ids) {
      const index = indexOf.get(id)!;
      player.named.add(index);
      player.requested.delete(index);
    }
    return frames;
  };

  const stopEvents = options.transport.onEvent(event => {
    if (closed || event.type !== 'stream_data') return;
    const frame = decodeFrame<E, I>(event.payload, options.label);
    if (frame === null) return;
    const peer = eventPeer(event.peerNode);
    if (peer === null) {
      drop('no-authenticated-peer');
      return;
    }
    const player = playerFor(peer);
    if (player === null) return;
    const t1 = now();
    player.lastHeard = t1;
    switch (frame.k) {
      case 'h':
        noteFingerprint(player, frame.f);
        return;
      case 'p':
        noteFingerprint(player, frame.f);
        send(peer, player, { n: options.label, k: 'q', t0: frame.t0, t1, t2: now() });
        return;
      case 'u':
        // Only indexes this host has assigned; anything else is noise.
        for (const index of frame.u) if (index < nameOf.length) player.requested.add(index);
        return;
      case 'w':
        // The lossy carrier reorders and repeats: only a newer version counts.
        if (frame.v > player.interestVersion) {
          player.interestVersion = frame.v;
          player.interest = new Set(frame.w);
        }
        return;
      case 'i':
        if (options.epoch !== undefined && frame.ep !== undefined && frame.ep !== options.epoch()) {
          drop('stale-epoch');
          return;
        }
        if (player.released) {
          drop('released');
          return;
        }
        if (player.resuming) {
          player.resuming = false;
          let newest = player.lastApplied;
          for (const input of frame.i) if (Number.isInteger(input.seq)) newest = Math.max(newest, input.seq);
          player.lastApplied = newest;
          dropped['resumed'] = (dropped['resumed'] ?? 0) + frame.i.length;
          return;
        }
        for (const input of frame.i) {
          if (!Number.isInteger(input.seq) || input.seq <= player.lastApplied) continue;
          if (!player.pending.has(input.seq)) {
            player.pending.set(input.seq, { seq: input.seq, seen: Number(input.seen) || 0, data: input.data });
          }
        }
        return;
      default:
        drop('unexpected-frame');
    }
  });

  const runTick = () => {
    if (closed) return;
    const time = now();
    const dtMs = tick === 0 ? tickMs : time - lastTickAt;
    lastTickAt = time;
    if (options.live !== undefined && !options.live()) {
      for (const player of players.values()) {
        if (player.pending.size > 0) {
          dropped['lease-lost'] = (dropped['lease-lost'] ?? 0) + player.pending.size;
          // Applied as if seen, so a repeat of these inputs is not applied later either.
          player.lastApplied = Math.max(player.lastApplied, ...player.pending.keys());
          player.pending.clear();
        }
      }
      return;
    }
    for (const [peer, player] of players) {
      if (time - player.lastHeard > playerTimeoutMs) {
        player.stream?.close();
        players.delete(peer);
      }
    }
    const inputs = new Map<string, TickInput<I>[]>();
    for (const [peer, player] of players) {
      if (player.pending.size === 0) continue;
      const ordered = [...player.pending.values()].sort((a, b) => a.seq - b.seq);
      player.pending.clear();
      // A gap (inputs lost despite redundancy) is skipped, not waited on:
      // a tick cannot stall for one player's packet.
      player.lastApplied = ordered.at(-1)!.seq;
      inputs.set(peer, ordered);
    }
    options.step({ tick, time, dtMs, inputs });
    const entities = options.snapshot(tick);
    const epoch = options.epoch?.();
    history.push({ tick, time, entities });
    if (history.length > keep) history.shift();
    for (const [peer, player] of players) {
      let view = entities;
      const wanted = options.interest && player.interest;
      if (options.visible || wanted) {
        const filtered: Record<string, E> = {};
        for (const [id, entity] of Object.entries(entities)) {
          if (options.visible && !options.visible(peer, id, entity)) continue;
          if (wanted) {
            let key: string | null;
            try {
              key = options.interest!(id, entity);
            } catch {
              key = null;
            }
            if (typeof key === 'string' && !wanted.has(key)) continue;
          }
          filtered[id] = entity;
        }
        view = filtered;
      }
      if (codec !== undefined && player.binary) {
        let binary: Uint8Array[] | null = null;
        try {
          binary = binaryFrames(player, tick, time, view, epoch);
        } catch (error) {
          // The codec refused an entity: this player gets JSON this tick,
          // not a snapshot missing a field.
          lastError = String((error as Error)?.message ?? error);
          drop('codec-write-failed');
        }
        if (binary !== null) {
          for (const payload of binary) sendBytes(peer, player, payload);
          continue;
        }
      }
      const frames = snapshotFrames(
        options.label,
        tick,
        time,
        player.lastApplied,
        view,
        maxFrameBytes,
        player.interestVersion,
        epoch,
      );
      for (const frame of frames) send(peer, player, frame);
    }
    tick += 1;
  };

  const timer = options.autoTick === false ? null : setInterval(runTick, tickMs);

  return {
    players: () => [...players.keys()],
    releasePlayer(peer: string) {
      const player = players.get(peer);
      if (player === undefined) return { lastApplied: 0 };
      // Received but not yet applied: not in the frozen state, so not
      // acknowledged — the player still holds them, for the new owner.
      player.pending.clear();
      player.released = true;
      return { lastApplied: player.lastApplied };
    },
    adoptPlayer(peer: string, from: { readonly lastApplied: number } | { readonly resume: true }) {
      const player = playerFor(peer);
      if (player === null) return;
      player.released = false;
      if ('resume' in from) {
        player.resuming = true;
        player.pending.clear();
        return;
      }
      player.lastApplied = Math.max(player.lastApplied, from.lastApplied);
      for (const seq of [...player.pending.keys()]) if (seq <= player.lastApplied) player.pending.delete(seq);
    },
    get currentTick() {
      return tick;
    },
    tick: runTick,
    rewind(seen: number): Rewound<E> {
      const floor = now() - maxRewindMs;
      const clamped = seen < floor;
      const time = clamped ? floor : seen;
      // The newest recorded state at or before `time`.
      let chosen = history[0];
      for (const entry of history) {
        if (entry.time <= time) chosen = entry;
        else break;
      }
      return { entities: chosen?.entities ?? {}, time, clamped };
    },
    dropped,
    get lastSendError() {
      return lastError;
    },
    close() {
      if (closed) return;
      closed = true;
      if (timer !== null) clearInterval(timer);
      stopEvents();
      for (const player of players.values()) player.stream?.close();
      players.clear();
    },
  };
}
