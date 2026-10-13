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
  /** Cross-host actions this shard serves (S3's hit claims). */
  readonly actions?: Readonly<Record<string, BorderAction<E>>>;
  readonly now?: () => number;
}

export interface ShardHost<E> {
  readonly shard: string;
  readonly netcode: HostNetcode<E>;
  readonly handoffs: RegionHandoffs<E>;
  world(): Readonly<Record<string, E>>;
  known(): Readonly<Record<string, E>>;
  close(): void;
}

export function regionOf(name: string): string;
export function shardHost<E extends Placed, I>(lib: ShardLib, options: ShardHostOptions<E, I>): ShardHost<E>;
