// Types for `shard.js`, the shard host recipe (sharded-match plan, S2).

import type {
  EntityCodec,
  HostNetcode,
  ShardAssignment,
  hostNetcode,
  joinNetcode,
} from '../../src/netcode/index.js';
import type { BorderAction, RegionHandoffs, handoffLink, regionHandoffs, regionState } from '../../src/world/index.js';

/** The library pieces the recipe wires; the page passes `dist`, the test `src`. */
export interface ShardLib {
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
  /** Static for now (no lobby, no leases): S4 makes it the lobby's record. */
  readonly assignment: ShardAssignment;
  /** This host's shard name in `assignment.hosts`. */
  readonly shard: string;
  /** Every player of the match (16 hex): each joins every host. */
  readonly roster: readonly string[];
  /** The entities this shard owns at the start. */
  readonly initial?: Readonly<Record<string, E>>;
  /** Apply one player input to its entity. */
  apply(state: E, input: I): E;
  /** Advance the shard's own entities one tick (NPCs, physics). */
  advance?(entities: Readonly<Record<string, E>>): Readonly<Record<string, E>>;
  /** Players see entities within this distance of their own. Default: no fog. */
  readonly fogRadius?: number;
  interest?(id: string, entity: E): string | null;
  readonly codec?: EntityCodec<E>;
  readonly tickRate?: number;
  readonly autoTick?: boolean;
  /** More cross-host actions this shard serves, beside combat's `hit`. */
  readonly actions?: Readonly<Record<string, BorderAction<E>>>;
  /** Whether this host still holds its lease (S4: `leaseWatch` over its lease in the lobby store). */
  live?(): boolean;
  /** Cross-host hits (S3). A fire input carries `{ fire: { target, seenB } }`. */
  readonly combat?: ShardCombat<E>;
  readonly now?: () => number;
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
  /** How far back `seenA`/`seenB` may reach, ms (D5). Default 300. */
  readonly maxRewindMs?: number;
}

/** What combat did on one host. */
export interface ShardCombatRecord {
  fired: number;
  readonly local: { hit: number; miss: number };
  /** Outcomes of shots this host forwarded: `hit`, `miss`, `unresolved`, `refused:<reason>`, `refused-here:<reason>`. */
  readonly forwarded: Record<string, number>;
  /** Verdicts this host gave on claims it served: `hit`, `miss`, `stale-epoch`, `not-owner`, `beyond-cap`, `no-history`, `pose-mismatch`. */
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
  known(): Readonly<Record<string, E>>;
  close(): void;
}

export function regionOf(name: string): string;
export function shardHost<E extends Placed, I>(lib: ShardLib, options: ShardHostOptions<E, I>): ShardHost<E>;
