/**
 * Groups surface — `ReplicaGroup` / `ForkGroup` / `StandbyGroup`.
 *
 * Stage 2 of `SDK_GROUPS_SURFACE_PLAN.md`. Thin wrappers over the
 * NAPI classes that add:
 *
 * - Typed `GroupError` extending `DaemonError` with a `kind`
 *   discriminator parsed from the Rust side's stable
 *   `daemon: group: <kind>[: detail]` error prefix.
 * - `Buffer | Uint8Array` interop for `groupSeed`.
 * - `MigrationOptions`-style config shapes with camelCase fields.
 *
 * @example
 * ```typescript
 * import { DaemonRuntime, Identity, ReplicaGroup } from '@ai2070/net-sdk';
 *
 * // Register the factory the group will invoke for each member.
 * rt.registerFactory('counter', () => new CounterDaemon());
 *
 * // Spawn a 3-member replica group. `spawn` is async — the
 * // factory TSFN round-trip runs on a tokio worker so the Node
 * // main thread stays free to execute JS factory callbacks.
 * const group = await ReplicaGroup.spawn(rt, 'counter', {
 *   replicaCount: 3,
 *   groupSeed: Buffer.alloc(32, 0x11),  // 32 bytes
 *   lbStrategy: 'round-robin',
 * });
 *
 * // Route a request.
 * const origin = group.routeEvent({ routingKey: 'user:42' });
 * await rt.deliver(origin, someEvent);
 * ```
 *
 * @packageDocumentation
 */

import {
  ReplicaGroup as NapiReplicaGroup,
  ForkGroup as NapiForkGroup,
  StandbyGroup as NapiStandbyGroup,
  type ReplicaGroupConfigJs,
  type ForkGroupConfigJs,
  type StandbyGroupConfigJs,
  type GroupHealthJs,
  type GroupHostConfigJs,
  type MemberInfoJs,
  type ForkRecordJs,
  type RequestContextJs,
  StrategyJs,
} from '@ai2070/net';

import { getNapiRuntime } from './_internal.js';
import { DaemonError } from './compute';
import type { CausalEvent, DaemonRuntime } from './compute';

// ----------------------------------------------------------------------------
// GroupError — typed subclass of DaemonError
// ----------------------------------------------------------------------------

/**
 * Stable machine-readable discriminator for group-layer failures.
 * Parsed from the Rust side's `daemon: group: <kind>[: detail]`
 * prefix; use `err.kind` in catch blocks rather than parsing the
 * message string by hand.
 */
export type GroupErrorKind =
  | 'not-ready'
  | 'factory-not-found'
  | 'no-healthy-member'
  | 'placement-failed'
  | 'registry-failed'
  | 'invalid-config'
  | 'daemon'
  | 'unknown';

/**
 * Typed group failure. Subclass of {@link DaemonError} so
 * `catch (e: DaemonError)` still matches.
 */
export class GroupError extends DaemonError {
  readonly kind: GroupErrorKind;
  /** Optional detail string carried by `placement-failed` /
   *  `registry-failed` / `invalid-config` / `daemon` variants. */
  readonly detail?: string;
  /** `factory-not-found` carries the requested kind name. */
  readonly requestedKind?: string;

  constructor(
    kind: GroupErrorKind,
    message: string,
    extras: { detail?: string; requestedKind?: string } = {},
  ) {
    super(message);
    this.name = 'GroupError';
    this.kind = kind;
    this.detail = extras.detail;
    this.requestedKind = extras.requestedKind;
    Object.setPrototypeOf(this, GroupError.prototype);
  }
}

/**
 * Lift a caught unknown error into the right typed exception.
 * The Rust side emits `daemon: group: <kind>[: detail]`; unknown
 * kinds fall back to `kind: 'unknown'` with the raw body.
 * Non-group errors pass through as plain `DaemonError` (or the
 * original throw if the message doesn't start with `daemon:`).
 */
function toGroupError(e: unknown): never {
  const msg = (e as Error | undefined)?.message ?? String(e);
  if (msg.startsWith('daemon:')) {
    const body = msg.slice('daemon:'.length).trim();
    if (body.startsWith('group:')) {
      throw parseGroupError(body, msg.slice('daemon:'.length).trim());
    }
    throw new DaemonError(body);
  }
  throw e;
}

function parseGroupError(body: string, fullMessage: string): GroupError {
  // Body shape after `daemon: ` stripping:
  //   group: <kind>
  //   group: <kind>: <detail>
  const afterPrefix = body.slice('group:'.length).trim();
  const firstColon = afterPrefix.indexOf(':');
  const kind =
    firstColon === -1 ? afterPrefix : afterPrefix.slice(0, firstColon).trim();
  const rest =
    firstColon === -1 ? '' : afterPrefix.slice(firstColon + 1).trim();

  switch (kind) {
    case 'not-ready':
    case 'no-healthy-member':
      return new GroupError(kind, fullMessage);
    case 'factory-not-found':
      return new GroupError(kind, fullMessage, { requestedKind: rest });
    case 'placement-failed':
    case 'registry-failed':
    case 'invalid-config':
    case 'daemon':
      return new GroupError(kind, fullMessage, { detail: rest });
    default:
      return new GroupError('unknown', fullMessage);
  }
}

// ----------------------------------------------------------------------------
// Public types — re-exports of NAPI POJOs with ergonomic names
// ----------------------------------------------------------------------------

/**
 * Load-balancing strategy for inbound group events.
 *
 * - `round-robin` — rotate across healthy members.
 * - `consistent-hash` — stable routing on `routingKey`.
 * - `least-load` — pick the member with the lowest utilization.
 * - `least-connections` — pick the member with the fewest in-flight calls.
 * - `random` — uniformly-random healthy pick.
 */
export type GroupStrategy = StrategyJs;

/** Per-member metadata. */
export type GroupMemberInfo = MemberInfoJs;

/** Aggregate health surface. */
export type GroupHealth = GroupHealthJs;

/** Lineage record for a single fork. */
export type ForkRecord = ForkRecordJs;

/** Routing context handed to `routeEvent`. */
export type RequestContext = RequestContextJs;

/** Per-daemon host config applied to every group member. */
export type GroupHostConfig = GroupHostConfigJs;

/** Config for a replica group. */
export interface ReplicaGroupConfig extends Omit<ReplicaGroupConfigJs, 'groupSeed'> {
  /** 32-byte seed. Accepts `Buffer` or `Uint8Array`. */
  groupSeed: Buffer | Uint8Array;
}

/** Config for a fork group. */
export type ForkGroupConfig = ForkGroupConfigJs;

/** Config for a standby group. */
export interface StandbyGroupConfig
  extends Omit<StandbyGroupConfigJs, 'groupSeed'> {
  /** 32-byte seed. Accepts `Buffer` or `Uint8Array`. */
  groupSeed: Buffer | Uint8Array;
}

function toBuffer(seed: Buffer | Uint8Array): Buffer {
  return Buffer.isBuffer(seed) ? seed : Buffer.from(seed);
}

// ----------------------------------------------------------------------------
// ReplicaGroup
// ----------------------------------------------------------------------------

/**
 * N interchangeable copies of a daemon. Each replica has a
 * deterministic identity derived from `groupSeed + index`;
 * the group load-balances inbound events across healthy members
 * and auto-replaces members on node failure.
 */
export class ReplicaGroup {
  private readonly inner: NapiReplicaGroup;

  /** @internal */
  constructor(inner: NapiReplicaGroup) {
    this.inner = inner;
  }

  /**
   * Spawn a replica group bound to `runtime`. `kind` must have
   * been registered via {@link DaemonRuntime.registerFactory};
   * the group calls the factory once per replica at spawn and
   * again on scale-up / failure replacement.
   *
   * Async because the underlying SDK `spawn` runs on a tokio
   * worker — the factory TSFN round-trip needs the Node main
   * thread free to execute the JS factory callback, so a sync
   * main-thread spawn would deadlock.
   *
   * Throws {@link GroupError} on `not-ready`, `factory-not-found`,
   * `placement-failed`, `invalid-config`, or `registry-failed`.
   */
  static async spawn(
    runtime: DaemonRuntime,
    kind: string,
    config: ReplicaGroupConfig,
  ): Promise<ReplicaGroup> {
    try {
      const napi = await getNapiRuntime(runtime).spawnReplicaGroup(kind, {
        replicaCount: config.replicaCount,
        groupSeed: toBuffer(config.groupSeed),
        lbStrategy: config.lbStrategy,
        hostConfig: config.hostConfig,
      });
      return new ReplicaGroup(napi);
    } catch (e) {
      return toGroupError(e);
    }
  }

  /** Route to the best-available replica; returns the target
   *  `origin_hash` which the caller feeds to `runtime.deliver`. */
  routeEvent(ctx: RequestContext = {}): bigint {
    try {
      return this.inner.routeEvent(ctx);
    } catch (e) {
      return toGroupError(e);
    }
  }

  /** Resize the group to `n` members. The kind is fixed at
   *  spawn time and not accepted here — see the class docstring
   *  for why. Async because growing invokes the factory (TSFN)
   *  once per new replica. */
  async scaleTo(n: number): Promise<void> {
    try {
      await this.inner.scaleTo(n);
    } catch (e) {
      toGroupError(e);
    }
  }

  /** Replace all members on `failedNodeId` onto other nodes.
   *  Returns the indices of replicas that were respawned.
   *  Reuses the group's spawn kind. */
  async onNodeFailure(failedNodeId: bigint): Promise<number[]> {
    try {
      return await this.inner.onNodeFailure(failedNodeId);
    } catch (e) {
      return toGroupError(e);
    }
  }

  onNodeRecovery(recoveredNodeId: bigint): void {
    try {
      this.inner.onNodeRecovery(recoveredNodeId);
    } catch (e) {
      toGroupError(e);
    }
  }

  get health(): GroupHealth {
    return this.inner.health;
  }

  get groupId(): number {
    return this.inner.groupId;
  }

  get replicas(): GroupMemberInfo[] {
    return this.inner.replicas;
  }

  get replicaCount(): number {
    return this.inner.replicaCount;
  }

  get healthyCount(): number {
    return this.inner.healthyCount;
  }
}

// ----------------------------------------------------------------------------
// ForkGroup
// ----------------------------------------------------------------------------

/**
 * N independent daemons forked from a common parent at
 * `forkSeq`. Unique identities, shared ancestry via `ForkRecord`.
 */
export class ForkGroup {
  private readonly inner: NapiForkGroup;

  /** @internal */
  constructor(inner: NapiForkGroup) {
    this.inner = inner;
  }

  /** Fork `config.forkCount` new daemons from `parentOrigin` at
   *  `forkSeq`. Each fork gets a fresh unique keypair + a
   *  `ForkRecord` linking it to the parent. Async for the same
   *  deadlock-avoidance reason as {@link ReplicaGroup.spawn}. */
  static async fork(
    runtime: DaemonRuntime,
    kind: string,
    parentOrigin: bigint,
    forkSeq: bigint,
    config: ForkGroupConfig,
  ): Promise<ForkGroup> {
    try {
      const napi = await getNapiRuntime(runtime).spawnForkGroup(
        kind,
        parentOrigin,
        forkSeq,
        config,
      );
      return new ForkGroup(napi);
    } catch (e) {
      return toGroupError(e);
    }
  }

  routeEvent(ctx: RequestContext = {}): bigint {
    try {
      return this.inner.routeEvent(ctx);
    } catch (e) {
      return toGroupError(e);
    }
  }

  async scaleTo(n: number): Promise<void> {
    try {
      await this.inner.scaleTo(n);
    } catch (e) {
      toGroupError(e);
    }
  }

  async onNodeFailure(failedNodeId: bigint): Promise<number[]> {
    try {
      return await this.inner.onNodeFailure(failedNodeId);
    } catch (e) {
      return toGroupError(e);
    }
  }

  onNodeRecovery(recoveredNodeId: bigint): void {
    try {
      this.inner.onNodeRecovery(recoveredNodeId);
    } catch (e) {
      toGroupError(e);
    }
  }

  get health(): GroupHealth {
    return this.inner.health;
  }

  get parentOrigin(): bigint {
    return this.inner.parentOrigin;
  }

  get forkSeq(): bigint {
    return this.inner.forkSeq;
  }

  get forkRecords(): ForkRecord[] {
    return this.inner.forkRecords;
  }

  /** `true` iff every fork's `ForkRecord` verifies against its
   *  parent. Core performs the signature + sentinel checks. */
  verifyLineage(): boolean {
    return this.inner.verifyLineage();
  }

  get members(): GroupMemberInfo[] {
    return this.inner.members;
  }

  get forkCount(): number {
    return this.inner.forkCount;
  }

  get healthyCount(): number {
    return this.inner.healthyCount;
  }
}

// ----------------------------------------------------------------------------
// StandbyGroup
// ----------------------------------------------------------------------------

/**
 * Active-passive replication. One active processes events; N−1
 * standbys hold snapshots and catch up via {@link sync}. On
 * active failure, {@link promote} (or automatic failover via
 * {@link onNodeFailure}) picks the most-synced standby.
 *
 * **Automatic replay buffering.** The group installs a
 * post-delivery observer on its active member's origin at spawn
 * and re-points it on promote / failover. Every
 * `runtime.deliver(group.activeOrigin, event)` automatically
 * feeds the standby replay buffer — no paired
 * `onEventDelivered` call required from the caller. The method
 * remains on the class for test scenarios that simulate a gap
 * without a live runtime; production code should ignore it.
 */
export class StandbyGroup {
  private readonly inner: NapiStandbyGroup;

  /** @internal */
  constructor(inner: NapiStandbyGroup) {
    this.inner = inner;
  }

  static async spawn(
    runtime: DaemonRuntime,
    kind: string,
    config: StandbyGroupConfig,
  ): Promise<StandbyGroup> {
    try {
      const napi = await getNapiRuntime(runtime).spawnStandbyGroup(kind, {
        memberCount: config.memberCount,
        groupSeed: toBuffer(config.groupSeed),
        hostConfig: config.hostConfig,
      });
      return new StandbyGroup(napi);
    } catch (e) {
      return toGroupError(e);
    }
  }

  /** `origin_hash` of the current active. Target for inbound
   *  events; standbys don't process inputs. */
  get activeOrigin(): bigint {
    return this.inner.activeOrigin;
  }

  /** Snapshot the active and push to every standby. Returns the
   *  sequence number the sync caught up through. */
  async sync(): Promise<bigint> {
    try {
      return await this.inner.syncStandbys();
    } catch (e) {
      return toGroupError(e);
    }
  }

  /**
   * **Test-only.** Manually push an event into the replay
   * buffer. Production code does NOT need to call this — the
   * post-delivery observer installed at `spawn` / `promote`
   * automatically feeds the buffer on every
   * `runtime.deliver(group.activeOrigin, event)`. Exposed so
   * tests can simulate a gap between the last sync and a
   * failure without driving a live runtime. Not part of the
   * stable public API.
   *
   * @internal
   */
  onEventDelivered(event: CausalEvent): void {
    try {
      this.inner.onEventDelivered(event);
    } catch (e) {
      toGroupError(e);
    }
  }

  /** Promote the most-synced standby to active. Reuses the
   *  group's spawn kind — no external parameter, so callers
   *  can't accidentally promote with the wrong factory. Call
   *  manually for planned failover; {@link onNodeFailure} calls
   *  automatically when the active's node fails. */
  async promote(): Promise<bigint> {
    try {
      return await this.inner.promote();
    } catch (e) {
      return toGroupError(e);
    }
  }

  /** Handle node failure. Returns the new active's `origin_hash`
   *  if the active was on `failedNodeId`; `null` if only standbys
   *  were affected. Reuses the group's spawn kind. */
  async onNodeFailure(failedNodeId: bigint): Promise<bigint | null> {
    try {
      const r = await this.inner.onNodeFailure(failedNodeId);
      return r ?? null;
    } catch (e) {
      return toGroupError(e);
    }
  }

  onNodeRecovery(recoveredNodeId: bigint): void {
    try {
      this.inner.onNodeRecovery(recoveredNodeId);
    } catch (e) {
      toGroupError(e);
    }
  }

  get health(): GroupHealth {
    return this.inner.health;
  }

  get activeHealthy(): boolean {
    return this.inner.activeHealthy;
  }

  get activeIndex(): number {
    return this.inner.activeIndex;
  }

  /** `"active"` | `"standby"` | `null` (out-of-range). */
  memberRole(index: number): 'active' | 'standby' | null {
    const r = this.inner.memberRole(index);
    return (r as 'active' | 'standby' | null) ?? null;
  }

  syncedThrough(index: number): bigint | null {
    return this.inner.syncedThrough(index) ?? null;
  }

  get bufferedEventCount(): number {
    return this.inner.bufferedEventCount;
  }

  get groupId(): number {
    return this.inner.groupId;
  }

  get members(): GroupMemberInfo[] {
    return this.inner.members;
  }

  get memberCount(): number {
    return this.inner.memberCount;
  }

  get standbyCount(): number {
    return this.inner.standbyCount;
  }
}
