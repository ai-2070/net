// Types for `shard.js`, the shard host recipe (sharded-match plan, S2).

import type {
  NpcPath,
  PathPoint,
  pathAt,
  EntityCodec,
  HostNetcode,
  ShardAssignment,
  hostNetcode,
  joinNetcode,
} from '../../src/netcode/index.js';
import type {
  BorderAction,
  HandoffEvent,
  HandoffId,
  RegionHandoffs,
  handoffLink,
  regionHandoffs,
  regionState,
} from '../../src/world/index.js';

/** The library pieces the recipe wires; the page passes `dist`, the test `src`. */
export interface ShardLib {
  /** Needed only with `options.paths`. */
  readonly pathAt?: typeof pathAt;
  readonly hostNetcode: typeof hostNetcode;
  readonly joinNetcode: typeof joinNetcode;
  readonly handoffLink: typeof handoffLink;
  readonly regionHandoffs: typeof regionHandoffs;
  readonly regionState: typeof regionState;
}

/** An entity the recipe can fog: it has a position on the ground plane. */
export interface Placed {
  readonly x: number;
  readonly z: number;
}

export interface ShardHostOptions<E extends Placed, I> {
  readonly transport: Parameters<typeof hostNetcode>[0]['transport'] & Parameters<typeof handoffLink>[0]['transport'];
  readonly label: string;
  /** The starting assignment (its epoch a non-negative safe integer); later ones arrive through `setAssignment`. */
  readonly assignment: ShardAssignment;
  /** This host's shard name in `assignment.hosts`. */
  readonly shard: string;
  /** Every player of the match (16 hex): each joins every host. */
  readonly roster: readonly string[];
  /** The entities this shard owns at the start. */
  readonly initial?: Readonly<Record<string, E>>;
  /** Apply one player input to its entity. */
  apply(state: E, input: I): E;
  /**
   * Advance the shard's own entities one tick: physics, and NPC AI (N1).
   * `ai.known()` is the whole match as this host knows it; `ai.fire(npc,
   * target)` queues an NPC's shot on the cross-host hit path.
   */
  advance?(entities: Readonly<Record<string, E>>, ai: NpcContext<E>): Readonly<Record<string, E>>;
  /** Players see entities within this distance of their own. Default: no fog. */
  readonly fogRadius?: number;
  interest?(id: string, entity: E): string | null;
  readonly codec?: EntityCodec<E>;
  readonly tickRate?: number;
  readonly autoTick?: boolean;
  /** More cross-host actions this shard serves, beside combat's `hit`. */
  readonly actions?: Readonly<Record<string, BorderAction<E>>>;
  /** NPC paths (N2): where records are published (the lobby store's `setPath`), and how long a switch is announced. */
  readonly paths?: {
    /** Publish `path` (the lobby store's `setPath`). A throw or a rejected promise is a refusal: a path switch then reverts to live. */
    write(npc: string, path: NpcPath): unknown;
    /**
     * The record as held (this host's replica of the lobby store). Lets a
     * host that takes over a lost host's NPC retire its path record, or
     * bump past its `mv`. Optional: without it, an NPC that could not be
     * restored keeps its record.
     */
    read?(npc: string): NpcPath | undefined;
    /** Snapshots keep sending a path NPC this long after the switch, ms. Default 1000. */
    readonly announceMs?: number;
  };
  /** Handoff outcomes: `moved` at the source is when the lobby raises the epoch (S5). */
  onHandoffEvent?(event: HandoffEvent): void;
  /** Whether this host still holds its lease (S4: `leaseWatch` over its lease in the lobby store). */
  live?(): boolean;
  /** Cross-host hits (S3). A fire input carries `{ fire: { target, seenB } }`. */
  readonly combat?: ShardCombat<E>;
  /**
   * The one clock the host runs on — netcode, spectators, handoffs, hits,
   * NPC shots and paths. Default: the netcode's (`performance.now()`),
   * which is what players' `hostNow()` estimates. Pass the same clock to
   * `npcModes` / `joinShards` only when they share this node.
   */
  readonly now?: () => number;
}

/** What NPC AI gets each tick. */
export interface NpcContext<E> {
  /** Every entity this host knows: its own now, other hosts' as spectated (~100–150 ms late). */
  known(): Readonly<Record<string, E>>;
  /** Queue a shot by `npc` (one this shard owns) at `target`, decided like a player's. */
  fire(npc: string, target: string): void;
}

/** Hitscan combat rules, as both the shooter's and the target's host apply them. */
export interface ShardCombat<E> {
  /** Check a shot at the shooter's host (ammo, cooldown, alive): the shooter after firing, or a refusal reason. */
  fire(shooter: E): E | string;
  /** Does a shot from `shooter` hit `target`? Both as they were when the shooter saw them. */
  hits(shooter: E, target: E): boolean;
  /** The target after one hit. */
  damage(target: E): E;
  /** The fastest an entity moves, units/s: sizes the pose tolerance. */
  readonly maxSpeed: number;
  /** Added to the pose tolerance. Default 0.5. */
  readonly epsilon?: number;
  /** The widest history gap a pose is judged across, ms. Default three snapshot intervals. */
  readonly maxGapMs?: number;
  /** How far behind its host's clock this host renders other hosts (an NPC's `seenB`), ms. Default 100. */
  readonly viewDelayMs?: number;
  /** How far back `seenA`/`seenB` may reach, ms (D5). Default 500: covers ~140 ms one-way. */
  readonly maxRewindMs?: number;
}

/** What combat did on one host. */
export interface ShardCombatRecord {
  fired: number;
  readonly local: { hit: number; miss: number };
  /**
   * Outcomes of shots this host forwarded: `hit`, `miss`, `unresolved`,
   * `refused:<reason>`, `refused-here:<reason>`; and, outside combat,
   * `unrecovered` (a lost host's entity with no observation) and
   * `path-refused` (a path write refused).
   */
  readonly forwarded: Record<string, number>;
  /** Verdicts this host gave on claims it served: `hit`, `miss`, `lease-lost`, `not-host`, `stale-epoch`, `not-owner`, `beyond-cap`, `no-history`, `pose-mismatch`. */
  readonly served: Record<string, number>;
  /** `now - seenB` for every claim served here, ms (D5's distribution). */
  readonly seenBAgeMs: number[];
}

export interface ShardHost<E> {
  combat(): ShardCombatRecord;
  readonly shard: string;
  readonly netcode: HostNetcode<E>;
  readonly handoffs: RegionHandoffs<E>;
  world(): Readonly<Record<string, E>>;
  /** This host's clock: what `hostNow()` estimates, and what path points and hit times are on. */
  now(): number;
  /** Move `peer`'s entity to `toShard`, with its input boundary (S5). */
  movePlayer(peer: string, toShard: string): Promise<HandoffId>;
  /**
   * Install a newer assignment; `false` (nothing changes) unless its epoch
   * is a safe integer above the installed one. Entities it hands this shard
   * from a lost host are restored from observation (S6; a path NPC comes
   * back live); ones it takes away are dropped here. If it gives this
   * shard to another host, this one stops serving it. Hosts and
   * spectators follow `next.hosts`.
   */
  setAssignment(next: ShardAssignment): boolean;
  recovered(): readonly string[];
  /** Put `npc` on a path; `points` on {@link ShardHost.now}. A refused write switches it back to live under a further `mv`. */
  setNpcPath(npc: string, points: readonly PathPoint[], key?: string | null): unknown;
  setNpcLive(npc: string, key?: string | null): unknown;
  observed(shard: string): Readonly<Record<string, E>> | null;
  known(): Readonly<Record<string, E>>;
  close(): void;
}

export function regionOf(name: string): string;
export function shardHost<E extends Placed, I>(lib: ShardLib, options: ShardHostOptions<E, I>): ShardHost<E>;
