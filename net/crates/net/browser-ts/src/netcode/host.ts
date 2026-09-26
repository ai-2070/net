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

import { type Frame, type NetcodeStream, type NetcodeTransport, type Now, defaultNow, decodeFrame, encodeFrame, peerHex } from './wire.js';

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
  /** Admit `peer` as a player. Default: everyone. */
  authorize?(peer: string): boolean;
  /**
   * How far back {@link HostNetcode.rewind} may go, in ms. Default 200 —
   * lag compensation that trusts any claimed latency lets a player buy an
   * advantage by faking lag.
   */
  readonly maxRewindMs?: number;
  /** Drop a player silent this long, ms. Default 5000. */
  readonly playerTimeoutMs?: number;
  /** `false` to drive ticks yourself with {@link HostNetcode.tick}. Default `true`. */
  readonly autoTick?: boolean;
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
  /** Frames dropped and why. */
  readonly dropped: Readonly<Record<string, number>>;
  close(): void;
}

interface Player<I> {
  stream: NetcodeStream | null;
  opening: boolean;
  lastApplied: number;
  pending: Map<number, TickInput<I>>;
  lastHeard: number;
}

/** History kept: enough ticks to cover the rewind cap with room. */
function historySize(tickRate: number, maxRewindMs: number): number {
  return Math.max(8, Math.ceil((maxRewindMs / 1000) * tickRate) + 4);
}

/** Start the authoritative netcode host. */
export function hostNetcode<E, I>(options: HostNetcodeOptions<E, I>): HostNetcode<E> {
  const now = options.now ?? defaultNow;
  const tickRate = options.tickRate ?? 30;
  if (!(tickRate > 0 && tickRate <= 240)) throw new RangeError('tickRate must be in (0, 240]');
  const tickMs = 1000 / tickRate;
  const maxRewindMs = options.maxRewindMs ?? 200;
  const playerTimeoutMs = options.playerTimeoutMs ?? 5000;
  const players = new Map<string, Player<I>>();
  const history: { tick: number; time: number; entities: Readonly<Record<string, E>> }[] = [];
  const keep = historySize(tickRate, maxRewindMs);
  const dropped: Record<string, number> = {};
  const drop = (why: string) => {
    dropped[why] = (dropped[why] ?? 0) + 1;
  };
  let tick = 0;
  let lastTickAt = now();
  let closed = false;

  const send = (peer: string, player: Player<I>, frame: Frame<E, I>) => {
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
          .catch(() => drop('stream-open-failed'))
          .finally(() => {
            player.opening = false;
          });
      }
      drop('stream-not-open');
      return;
    }
    try {
      void Promise.resolve(player.stream.send(encodeFrame(frame))).catch(() => drop('send-failed'));
    } catch {
      drop('send-failed');
    }
  };

  const playerFor = (peer: string): Player<I> | null => {
    const existing = players.get(peer);
    if (existing) return existing;
    if (options.authorize && !options.authorize(peer)) {
      drop('unauthorized');
      return null;
    }
    const player: Player<I> = { stream: null, opening: false, lastApplied: 0, pending: new Map(), lastHeard: now() };
    players.set(peer, player);
    return player;
  };

  const stopEvents = options.transport.onEvent(event => {
    if (closed || event.type !== 'stream_data') return;
    const frame = decodeFrame<E, I>(event.payload, options.label);
    if (frame === null) return;
    const peer = peerHex(event.peerNode);
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
        return;
      case 'p':
        send(peer, player, { n: options.label, k: 'q', t0: frame.t0, t1, t2: now() });
        return;
      case 'i':
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
    history.push({ tick, time, entities });
    if (history.length > keep) history.shift();
    for (const [peer, player] of players) {
      let view = entities;
      if (options.visible) {
        const filtered: Record<string, E> = {};
        for (const [id, entity] of Object.entries(entities)) {
          if (options.visible(peer, id, entity)) filtered[id] = entity;
        }
        view = filtered;
      }
      send(peer, player, { n: options.label, k: 's', tick, t: time, ack: player.lastApplied, e: view });
    }
    tick += 1;
  };

  const timer = options.autoTick === false ? null : setInterval(runTick, tickMs);

  return {
    players: () => [...players.keys()],
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
