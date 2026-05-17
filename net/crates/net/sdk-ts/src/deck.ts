/**
 * Deck SDK — operator-side TypeScript wrapper.
 *
 * Sits on top of the napi-rs binding at `@ai2070/net`. Adds:
 *
 * - {@link DeckSdkError} typed Error subclass that parses the
 *   substrate `<<deck-sdk-kind:KIND>>MSG` envelope.
 * - Auto-JSON-parsing for `status()` and `snapshots()`.
 * - `AsyncIterable<unknown>` (parsed `MeshOsSnapshot` JSON) / `AsyncIterable<StatusSummary>`
 *   wrappers over the raw `nextSnapshot()` / `nextSummary()`
 *   methods.
 *
 * Slice 1 ships client + admin (all 9 methods) + snapshot/status
 * streams + operator identity. Audit / logs / failures land in
 * slice 2; ICE in slice 3.
 *
 * @example
 * ```ts
 * import { MeshOsDaemonSdk } from '@ai2070/net-sdk/meshos';
 * import { DeckClient, OperatorIdentity } from '@ai2070/net-sdk/deck';
 *
 * const sdk = await MeshOsDaemonSdk.start();
 * const identity = OperatorIdentity.generate();
 * const client = await DeckClient.fromMeshos(sdk, identity);
 *
 * const commit = await client.admin.enterMaintenance(0xABCDn, 600_000n);
 * console.log(`committed at ${commit.commitId} kind=${commit.eventKind}`);
 *
 * for await (const snap of client.snapshots()) {
 *   // snap is a parsed object
 *   break;
 * }
 *
 * await sdk.shutdown();
 * ```
 */

import {
  AdminCommands as NapiAdmin,
  AdminVerifier as NapiAdminVerifier,
  AuditQuery as NapiAuditQuery,
  AuditStream as NapiAuditStream,
  DeckClient as NapiClient,
  FailureStream as NapiFailureStream,
  IceCommands as NapiIceCommands,
  IceProposal as NapiIceProposal,
  LogStream as NapiLogStream,
  OperatorIdentity,
  OperatorRegistry as NapiOperatorRegistry,
  SimulatedIceProposal as NapiSimulatedIceProposal,
  SnapshotStream as NapiSnapshotStream,
  StatusSummaryStream as NapiStatusStream,
  type AvoidScopeJs,
  type ChainCommitJs,
  type DeckClientConfigJs,
  type FailureRecordJs,
  type LogFilterJs,
  type LogRecordJs,
  type MeshOsConfigJs,
  type OperatorSignatureJs,
  type StatusSummaryJs,
} from '@ai2070/net';

import { MeshOsDaemonSdk, type MeshOsConfig } from './meshos.js';

// ----------------------------------------------------------------------------
// Typed error envelope (mirrors MeshOS SDK's MeshOsSdkError)
// ----------------------------------------------------------------------------

export class DeckSdkError extends Error {
  readonly kind: string;

  constructor(kind: string, message: string) {
    super(`<<deck-sdk-kind:${kind}>>${message}`);
    this.name = 'DeckSdkError';
    this.kind = kind;
    Object.setPrototypeOf(this, DeckSdkError.prototype);
  }

  static fromCaught(err: unknown): DeckSdkError | Error {
    if (err instanceof DeckSdkError) return err;
    if (!(err instanceof Error)) {
      return new Error(String(err));
    }
    const parsed = parseEnvelope(err.message);
    if (!parsed) return err;
    return new DeckSdkError(parsed.kind, parsed.body);
  }
}

function parseEnvelope(message: string): { kind: string; body: string } | null {
  const marker = '<<deck-sdk-kind:';
  const start = message.indexOf(marker);
  if (start === -1) return null;
  const kindStart = start + marker.length;
  const end = message.indexOf('>>', kindStart);
  if (end === -1) return null;
  return { kind: message.slice(kindStart, end), body: message.slice(end + 2) };
}

async function rethrowAsync<T>(fn: () => Promise<T>): Promise<T> {
  try {
    return await fn();
  } catch (e) {
    throw DeckSdkError.fromCaught(e);
  }
}

// ----------------------------------------------------------------------------
// Typed wire-form re-exports
// ----------------------------------------------------------------------------

export { OperatorIdentity };

export interface ChainCommit {
  commitId: bigint;
  operatorId: bigint;
  eventKind: string;
  committedAtMs: bigint;
}

export interface PeerCounts {
  healthy: number;
  degraded: number;
  unreachable: number;
  unknown: number;
}

export interface DaemonCounts {
  running: number;
  starting: number;
  stopping: number;
  stopped: number;
  backingOff: number;
  crashLooping: number;
}

export interface StatusSummary {
  peers: PeerCounts;
  daemons: DaemonCounts;
  replicaChains: number;
  avoidListEntries: number;
  recentlyEmittedCount: number;
  recentFailureCount: number;
  adminAuditRingDepth: number;
  freezeRemainingMs: bigint | null;
  localMaintenanceActive: boolean;
}

export interface DeckClientConfig {
  snapshotPollIntervalMs?: bigint;
  iceSignatureThreshold?: number;
}

// ----------------------------------------------------------------------------
// AdminCommands
// ----------------------------------------------------------------------------

export class AdminCommands {
  constructor(private readonly raw: NapiAdmin) {}

  async drain(node: bigint, drainForMs: bigint): Promise<ChainCommit> {
    return chainCommitFromJs(
      await rethrowAsync(() => this.raw.drain(node, drainForMs)),
    );
  }

  async enterMaintenance(
    node: bigint,
    drainForMs?: bigint | null,
  ): Promise<ChainCommit> {
    return chainCommitFromJs(
      await rethrowAsync(() => this.raw.enterMaintenance(node, drainForMs)),
    );
  }

  async exitMaintenance(node: bigint): Promise<ChainCommit> {
    return chainCommitFromJs(
      await rethrowAsync(() => this.raw.exitMaintenance(node)),
    );
  }

  async cordon(node: bigint): Promise<ChainCommit> {
    return chainCommitFromJs(await rethrowAsync(() => this.raw.cordon(node)));
  }

  async uncordon(node: bigint): Promise<ChainCommit> {
    return chainCommitFromJs(
      await rethrowAsync(() => this.raw.uncordon(node)),
    );
  }

  async dropReplicas(node: bigint, chains: bigint[]): Promise<ChainCommit> {
    return chainCommitFromJs(
      await rethrowAsync(() => this.raw.dropReplicas(node, chains)),
    );
  }

  async invalidatePlacement(node: bigint): Promise<ChainCommit> {
    return chainCommitFromJs(
      await rethrowAsync(() => this.raw.invalidatePlacement(node)),
    );
  }

  async restartAllDaemons(node: bigint): Promise<ChainCommit> {
    return chainCommitFromJs(
      await rethrowAsync(() => this.raw.restartAllDaemons(node)),
    );
  }

  async clearAvoidList(node: bigint): Promise<ChainCommit> {
    return chainCommitFromJs(
      await rethrowAsync(() => this.raw.clearAvoidList(node)),
    );
  }
}

function chainCommitFromJs(c: ChainCommitJs): ChainCommit {
  return {
    commitId: c.commitId,
    operatorId: c.operatorId,
    eventKind: c.eventKind,
    committedAtMs: c.committedAtMs,
  };
}

function statusSummaryFromJs(s: StatusSummaryJs): StatusSummary {
  return {
    peers: { ...s.peers },
    daemons: { ...s.daemons },
    replicaChains: s.replicaChains,
    avoidListEntries: s.avoidListEntries,
    recentlyEmittedCount: s.recentlyEmittedCount,
    recentFailureCount: s.recentFailureCount,
    adminAuditRingDepth: s.adminAuditRingDepth,
    freezeRemainingMs: s.freezeRemainingMs ?? null,
    localMaintenanceActive: s.localMaintenanceActive,
  };
}

// ----------------------------------------------------------------------------
// Streams — AsyncIterable wrappers around the raw napi handles
// ----------------------------------------------------------------------------

/**
 * Wrap a raw napi snapshot stream as `AsyncIterable<unknown>` (parsed `MeshOsSnapshot` JSON).
 * The napi side emits JSON-encoded snapshots; we parse here so
 * consumers see a native object. Returns `null` from `nextSnapshot()`
 * when the underlying stream closes.
 */
function snapshotsToAsyncIterable(
  raw: NapiSnapshotStream,
): AsyncIterable<unknown> & { close: () => Promise<void> } {
  return {
    [Symbol.asyncIterator]() {
      return {
        async next(): Promise<IteratorResult<unknown>> {
          try {
            const json = await raw.nextSnapshot();
            if (json === null) return { value: undefined, done: true };
            return { value: JSON.parse(json), done: false };
          } catch (e) {
            throw DeckSdkError.fromCaught(e);
          }
        },
        async return(): Promise<IteratorResult<unknown>> {
          await raw.close();
          return { value: undefined, done: true };
        },
      };
    },
    async close() {
      await raw.close();
    },
  };
}

function statusSummariesToAsyncIterable(
  raw: NapiStatusStream,
): AsyncIterable<StatusSummary> & { close: () => Promise<void> } {
  return {
    [Symbol.asyncIterator]() {
      return {
        async next(): Promise<IteratorResult<StatusSummary>> {
          try {
            const item = await raw.nextSummary();
            if (item === null) return { value: undefined, done: true };
            return { value: statusSummaryFromJs(item), done: false };
          } catch (e) {
            throw DeckSdkError.fromCaught(e);
          }
        },
        async return(): Promise<IteratorResult<StatusSummary>> {
          await raw.close();
          return { value: undefined, done: true };
        },
      };
    },
    async close() {
      await raw.close();
    },
  };
}

// ----------------------------------------------------------------------------
// DeckClient
// ----------------------------------------------------------------------------

export class DeckClient {
  private constructor(private readonly raw: NapiClient) {}

  /**
   * Construct a deck client that owns a private supervisor
   * runtime. Mirrors the cdylib's `net_deck_client_new`
   * (operator-only mode) for consumers without a separately-
   * managed `MeshOsDaemonSdk`.
   *
   * `operatorSeed` must be exactly 32 bytes of ed25519 seed
   * material. Call `.close()` (or use `await using`) to drain
   * the supervisor at end of scope; otherwise the runtime
   * releases on GC.
   */
  static async new(
    operatorSeed: Buffer,
    meshosConfig?: MeshOsConfig,
    deckConfig?: DeckClientConfig,
  ): Promise<DeckClient> {
    return rethrowAsync(async () => {
      const meshos: MeshOsConfigJs | undefined = meshosConfig
        ? {
            thisNode: meshosConfig.thisNode,
            tickIntervalMs: meshosConfig.tickIntervalMs,
            eventQueueCapacity: meshosConfig.eventQueueCapacity,
            actionQueueCapacity: meshosConfig.actionQueueCapacity,
          }
        : undefined;
      const deck: DeckClientConfigJs | undefined = deckConfig
        ? {
            snapshotPollIntervalMs: deckConfig.snapshotPollIntervalMs,
            iceSignatureThreshold: deckConfig.iceSignatureThreshold,
          }
        : undefined;
      const raw = await NapiClient.new(operatorSeed, meshos, deck);
      return new DeckClient(raw);
    });
  }

  /**
   * Construct against a running `MeshOsDaemonSdk`. Reuses the
   * SDK's tokio runtime so streams + admin commits run on the
   * same supervisor scheduler.
   */
  static async fromMeshos(
    sdk: MeshOsDaemonSdk,
    identity: OperatorIdentity,
    config?: DeckClientConfig,
  ): Promise<DeckClient> {
    return rethrowAsync(async () => {
      const rawSdk = sdk.__rawNapiSdk();
      const cfg: DeckClientConfigJs | undefined = config
        ? {
            snapshotPollIntervalMs: config.snapshotPollIntervalMs,
            iceSignatureThreshold: config.iceSignatureThreshold,
          }
        : undefined;
      const raw = await NapiClient.fromMeshos(rawSdk, identity, cfg);
      return new DeckClient(raw);
    });
  }

  /**
   * Tear down the private supervisor runtime if this client owns
   * one (constructed via `DeckClient.new`). No-op for clients
   * built via `fromMeshos` against an externally-managed SDK.
   * Idempotent: subsequent calls return without throwing.
   */
  async close(): Promise<void> {
    await rethrowAsync(() => this.raw.shutdown());
  }

  /**
   * `await using` hook so `await using deck = await DeckClient.new(...)`
   * drains the supervisor at scope exit.
   */
  async [Symbol.asyncDispose](): Promise<void> {
    await this.close();
  }

  /** Operator identity bound to this client. */
  identity(): OperatorIdentity {
    return this.raw.identity();
  }

  /** Typed admin-event surface. */
  get admin(): AdminCommands {
    return new AdminCommands(this.raw.admin);
  }

  /** Break-glass surface. Returns `IceCommands` whose factories
   * produce `IceProposal`s. Each must be `.simulate()`-d
   * (yielding a `SimulatedIceProposal`) before `.commit(...)`. */
  get ice(): IceCommands {
    return new IceCommands(this.raw.ice);
  }

  /**
   * One-shot read of the latest `MeshOsSnapshot` (parsed JSON;
   * `unknown` because the substrate snapshot's exact shape isn't
   * mirrored in the TS surface — consumers cast or use a runtime
   * validator).
   */
  status(): unknown {
    try {
      return JSON.parse(this.raw.status());
    } catch (e) {
      throw DeckSdkError.fromCaught(e);
    }
  }

  /** One-shot read of the rolled-up `StatusSummary`. */
  statusSummary(): StatusSummary {
    return statusSummaryFromJs(this.raw.statusSummary());
  }

  /**
   * Live snapshot stream as `AsyncIterable<unknown>` (parsed `MeshOsSnapshot` JSON). JSON
   * parsing happens automatically.
   *
   * Async on the napi side because the substrate creates a
   * `tokio::time::Interval` that needs a runtime context.
   */
  async snapshots(): Promise<
    AsyncIterable<unknown> & { close: () => Promise<void> }
  > {
    return snapshotsToAsyncIterable(await this.raw.snapshots());
  }

  /** Live status-summary stream. */
  async statusSummaryStream(): Promise<
    AsyncIterable<StatusSummary> & { close: () => Promise<void> }
  > {
    return statusSummariesToAsyncIterable(await this.raw.statusSummaryStream());
  }

  // =========================================================================
  // Slice 2 — audit + logs + failures
  // =========================================================================

  /**
   * Fluent admin-audit query builder. Chain `.recent(n)` /
   * `.byOperator(id)` / `.between(start, end)` / `.forceOnly()` /
   * `.since(seq)` before calling `.collect()` (eager list of
   * parsed audit records) or `.stream()` (`AsyncIterable`).
   */
  audit(): AuditQuery {
    return new AuditQuery(this.raw.audit());
  }

  /**
   * Subscribe to the runtime's log ring. Returns an
   * `AsyncIterable<LogRecord>`. Filter fields are all optional
   * — missing fields match every record.
   */
  async subscribeLogs(
    filter?: LogFilter,
  ): Promise<AsyncIterable<LogRecord> & { close: () => Promise<void> }> {
    return rethrowAsync(async () => {
      const raw = await this.raw.subscribeLogs(
        filter ? toNapiLogFilter(filter) : null,
      );
      return logStreamToAsyncIterable(raw);
    });
  }

  /**
   * Subscribe to the executor failure ring starting at
   * `sinceSeq + 1`. Pass `0n` (or omit) to start from whatever
   * is still in the ring.
   */
  async subscribeFailures(
    sinceSeq?: bigint,
  ): Promise<AsyncIterable<FailureRecord> & { close: () => Promise<void> }> {
    return rethrowAsync(async () => {
      const raw = await this.raw.subscribeFailures(sinceSeq);
      return failureStreamToAsyncIterable(raw);
    });
  }
}

// ----------------------------------------------------------------------------
// Slice 2 — typed envelopes
// ----------------------------------------------------------------------------

export type LogLevel = 'trace' | 'debug' | 'info' | 'warn' | 'error';

export interface LogFilter {
  minLevel?: LogLevel;
  daemonId?: bigint;
  nodeId?: bigint;
  sinceSeq?: bigint;
}

function toNapiLogFilter(f: LogFilter): LogFilterJs {
  return {
    minLevel: f.minLevel,
    daemonId: f.daemonId,
    nodeId: f.nodeId,
    sinceSeq: f.sinceSeq,
  };
}

export interface LogRecord {
  seq: bigint;
  tsMs: bigint;
  level: LogLevel;
  daemonId: bigint | null;
  nodeId: bigint | null;
  message: string;
}

function logRecordFromJs(r: LogRecordJs): LogRecord {
  return {
    seq: r.seq,
    tsMs: r.tsMs,
    level: r.level as LogLevel,
    daemonId: r.daemonId ?? null,
    nodeId: r.nodeId ?? null,
    message: r.message,
  };
}

export interface FailureRecord {
  seq: bigint;
  source: string;
  reason: string;
  recordedAtMs: bigint;
}

function failureRecordFromJs(r: FailureRecordJs): FailureRecord {
  return {
    seq: r.seq,
    source: r.source,
    reason: r.reason,
    recordedAtMs: r.recordedAtMs,
  };
}

// `AdminAuditRecord` carries a nested `AdminEvent` enum which is
// JSON-shaped at the binding boundary. Type loosely; per-variant
// typed envelopes can land in a future slice when consumers ask.
export type AdminAuditRecord = Record<string, unknown>;

// ----------------------------------------------------------------------------
// Slice 2 — Log + Failure AsyncIterable wrappers
// ----------------------------------------------------------------------------

function logStreamToAsyncIterable(
  raw: NapiLogStream,
): AsyncIterable<LogRecord> & { close: () => Promise<void> } {
  return {
    [Symbol.asyncIterator]() {
      return {
        async next(): Promise<IteratorResult<LogRecord>> {
          try {
            const item = await raw.nextRecord();
            if (item === null) return { value: undefined, done: true };
            return { value: logRecordFromJs(item), done: false };
          } catch (e) {
            throw DeckSdkError.fromCaught(e);
          }
        },
        async return(): Promise<IteratorResult<LogRecord>> {
          await raw.close();
          return { value: undefined, done: true };
        },
      };
    },
    async close() {
      await raw.close();
    },
  };
}

function failureStreamToAsyncIterable(
  raw: NapiFailureStream,
): AsyncIterable<FailureRecord> & { close: () => Promise<void> } {
  return {
    [Symbol.asyncIterator]() {
      return {
        async next(): Promise<IteratorResult<FailureRecord>> {
          try {
            const item = await raw.nextRecord();
            if (item === null) return { value: undefined, done: true };
            return { value: failureRecordFromJs(item), done: false };
          } catch (e) {
            throw DeckSdkError.fromCaught(e);
          }
        },
        async return(): Promise<IteratorResult<FailureRecord>> {
          await raw.close();
          return { value: undefined, done: true };
        },
      };
    },
    async close() {
      await raw.close();
    },
  };
}

function auditStreamToAsyncIterable(
  raw: NapiAuditStream,
): AsyncIterable<AdminAuditRecord> & { close: () => Promise<void> } {
  return {
    [Symbol.asyncIterator]() {
      return {
        async next(): Promise<IteratorResult<AdminAuditRecord>> {
          try {
            const json = await raw.nextRecord();
            if (json === null) return { value: undefined, done: true };
            return { value: JSON.parse(json) as AdminAuditRecord, done: false };
          } catch (e) {
            throw DeckSdkError.fromCaught(e);
          }
        },
        async return(): Promise<IteratorResult<AdminAuditRecord>> {
          await raw.close();
          return { value: undefined, done: true };
        },
      };
    },
    async close() {
      await raw.close();
    },
  };
}

// ----------------------------------------------------------------------------
// Slice 2 — AuditQuery fluent builder
// ----------------------------------------------------------------------------

/**
 * Fluent admin-audit query builder.
 *
 * @example
 * ```ts
 * const records = await client.audit()
 *   .recent(100)
 *   .byOperator(operatorId)
 *   .forceOnly()
 *   .collect();
 *
 * for await (const record of (await client.audit().since(lastSeq).stream())) {
 *   handle(record);
 * }
 * ```
 */
export class AuditQuery {
  constructor(private readonly raw: NapiAuditQuery) {}

  recent(limit: number): AuditQuery {
    this.raw.recent(limit);
    return this;
  }

  byOperator(operatorId: bigint): AuditQuery {
    this.raw.byOperator(operatorId);
    return this;
  }

  between(startMs: bigint, endMs: bigint): AuditQuery {
    this.raw.between(startMs, endMs);
    return this;
  }

  forceOnly(): AuditQuery {
    this.raw.forceOnly();
    return this;
  }

  since(seq: bigint): AuditQuery {
    this.raw.since(seq);
    return this;
  }

  /** Eager — returns a list of audit records (JSON-parsed into
   * native objects). */
  async collect(): Promise<AdminAuditRecord[]> {
    return rethrowAsync(async () => {
      const raw = this.raw.collect();
      return raw.map((s) => JSON.parse(s) as AdminAuditRecord);
    });
  }

  /** Async iterator over audit records. */
  async stream(): Promise<
    AsyncIterable<AdminAuditRecord> & { close: () => Promise<void> }
  > {
    return rethrowAsync(async () => auditStreamToAsyncIterable(await this.raw.stream()));
  }
}

// ----------------------------------------------------------------------------
// Slice 3 — ICE break-glass surface
//
// Typestate: `IceProposal` has no `commit` method. Only the
// `SimulatedIceProposal` returned from `IceProposal.simulate()`
// exposes `commit(signatures)`. Direct commit is unreachable at
// the class level — same shape as the substrate's compile-time
// typestate.
// ----------------------------------------------------------------------------

/** Avoid-list flush scope discriminated union. */
export type AvoidScope =
  | { kind: 'global' }
  | { kind: 'local'; node: bigint }
  | { kind: 'onPeer'; peer: bigint };

function avoidScopeToJs(scope: AvoidScope): AvoidScopeJs {
  switch (scope.kind) {
    case 'global':
      return { kind: 'global', node: undefined, peer: undefined };
    case 'local':
      return { kind: 'local', node: scope.node, peer: undefined };
    case 'onPeer':
      return { kind: 'onPeer', node: undefined, peer: scope.peer };
  }
}

/** Signature pair carried by ICE commits. `signature` must be 64
 * ed25519 bytes (the substrate verifier rejects malformed sigs
 * with kind `signature_invalid`). */
export interface OperatorSignature {
  operatorId: bigint;
  signature: Buffer;
}

function operatorSignatureToJs(sig: OperatorSignature): OperatorSignatureJs {
  return {
    operatorId: sig.operatorId,
    signature: sig.signature,
  };
}

// `BlastRadius` is JSON-shaped at the binding boundary.
export type BlastRadius = Record<string, unknown>;

export class IceCommands {
  constructor(private readonly raw: NapiIceCommands) {}

  freezeCluster(ttlMs: bigint): IceProposal {
    return rethrow(() => new IceProposal(this.raw.freezeCluster(ttlMs)));
  }

  flushAvoidLists(scope: AvoidScope): IceProposal {
    return rethrow(() =>
      new IceProposal(this.raw.flushAvoidLists(avoidScopeToJs(scope))),
    );
  }

  forceEvictReplica(chain: bigint, victim: bigint): IceProposal {
    return rethrow(
      () => new IceProposal(this.raw.forceEvictReplica(chain, victim)),
    );
  }

  /** Propose force-restarting a daemon. `id` is the registry-
   * local daemon id; `name` is `MeshDaemon::name()`. */
  forceRestartDaemon(id: bigint, name: string): IceProposal {
    return rethrow(
      () => new IceProposal(this.raw.forceRestartDaemon(id, name)),
    );
  }

  forceCutover(chain: bigint, target: bigint): IceProposal {
    return rethrow(
      () => new IceProposal(this.raw.forceCutover(chain, target)),
    );
  }

  killMigration(migration: bigint): IceProposal {
    return rethrow(() => new IceProposal(this.raw.killMigration(migration)));
  }

  thawCluster(): IceProposal {
    return new IceProposal(this.raw.thawCluster());
  }
}

/** Pre-simulation ICE proposal. No `commit` method —
 * `simulate()` must run first. */
export class IceProposal {
  constructor(private readonly raw: NapiIceProposal) {}

  get issuedAtMs(): bigint {
    return this.raw.issuedAtMs;
  }

  /** Pre-execution preview. Consumes the proposal — subsequent
   * `simulate()` calls throw `DeckSdkError(kind: "already_simulated")`. */
  async simulate(): Promise<SimulatedIceProposal> {
    return rethrowAsync(async () => new SimulatedIceProposal(await this.raw.simulate()));
  }
}

/** A simulated ICE proposal. Only class exposing `commit`. */
export class SimulatedIceProposal {
  constructor(private readonly raw: NapiSimulatedIceProposal) {}

  get issuedAtMs(): bigint {
    return this.raw.issuedAtMs;
  }

  /** Pre-execution blast radius, parsed from the binding's JSON. */
  async blastRadius(): Promise<BlastRadius> {
    return rethrowAsync(async () => JSON.parse(await this.raw.blastRadius()) as BlastRadius);
  }

  /** Blake3 digest of the blast radius (32 bytes). */
  async blastHash(): Promise<Buffer> {
    return rethrowAsync(() => this.raw.blastHash());
  }

  /** Commit with operator signatures. Consumes the proposal —
   * subsequent calls throw `already_committed`. */
  async commit(signatures: OperatorSignature[]): Promise<ChainCommit> {
    return rethrowAsync(async () => {
      const raw = await this.raw.commit(signatures.map(operatorSignatureToJs));
      return chainCommitFromJs(raw);
    });
  }
}

// `rethrow` for sync entry points — mirrors `rethrowAsync`.
function rethrow<T>(fn: () => T): T {
  try {
    return fn();
  } catch (e) {
    throw DeckSdkError.fromCaught(e);
  }
}

// ----------------------------------------------------------------------------
// Operator-policy verifier surface — OperatorRegistry + AdminVerifier
// ----------------------------------------------------------------------------

/** Cluster operator-policy registry. Authoring tool for the
 * substrate's known-operator set; offline-friendly verifier for
 * bundles before invoking
 * {@link SimulatedIceProposal.commit}.
 *
 * Mutations are thread-safe at the napi binding layer. */
export class OperatorRegistry {
  /** @internal — exposed so {@link AdminVerifier} can re-use the
   * underlying napi handle without round-tripping the public-key
   * set through JS. */
  readonly raw: NapiOperatorRegistry;

  constructor(raw?: NapiOperatorRegistry) {
    this.raw = raw ?? new NapiOperatorRegistry();
  }

  /** Insert an operator's 32-byte ed25519 public key under
   * `operatorId`. */
  insert(operatorId: bigint, publicKey: Buffer): void {
    rethrow(() => this.raw.insert(operatorId, publicKey));
  }

  /** Register an `OperatorIdentity`'s public key under its
   * derived operator id. */
  register(identity: OperatorIdentity): void {
    rethrow(() => this.raw.register(identity));
  }

  /** `true` iff `operatorId` is registered. */
  contains(operatorId: bigint): boolean {
    return rethrow(() => this.raw.contains(operatorId));
  }

  /** Number of registered operators. */
  get size(): number {
    return rethrow(() => this.raw.size);
  }

  /** `true` iff no operators are registered. */
  isEmpty(): boolean {
    return rethrow(() => this.raw.isEmpty());
  }

  /** Verify a single signature over `payload`. Throws
   * `DeckSdkError` with the substrate's stable kind discriminator
   * (`not_authorized`, `signature_invalid`, etc.). */
  verify(signature: OperatorSignature, payload: Buffer): void {
    rethrow(() =>
      this.raw.verify(operatorSignatureToJs(signature), payload),
    );
  }

  /** Verify every signature in the bundle and confirm at least
   * `threshold` *distinct* operator ids signed `payload`. */
  verifyBundle(
    signatures: OperatorSignature[],
    payload: Buffer,
    threshold: number,
  ): void {
    rethrow(() =>
      this.raw.verifyBundle(
        signatures.map(operatorSignatureToJs),
        payload,
        threshold,
      ),
    );
  }
}

/** Substrate-side admin commit verifier. Bundles an
 * {@link OperatorRegistry} snapshot with the cluster's signature
 * threshold + freshness/skew/ICE-cooldown windows. Useful for
 * offline unit testing of operator-policy decisions.
 *
 * Constructors snapshot the registry at build time — later
 * mutations on the source registry are not reflected. Rebuild
 * the verifier after every policy change. */
export class AdminVerifier {
  private constructor(private readonly raw: NapiAdminVerifier) {}

  /** Build a verifier with `threshold` minimum signatures and
   * the substrate defaults (300s freshness, 30s future-skew,
   * 300s ICE cooldown). `threshold = 0` is clamped to `1`. */
  static new(registry: OperatorRegistry, threshold: number): AdminVerifier {
    return rethrow(
      () => new AdminVerifier(new NapiAdminVerifier(registry.raw, threshold)),
    );
  }

  /** Build with explicit freshness + future-skew windows and
   * the default ICE cooldown. */
  static withFreshness(
    registry: OperatorRegistry,
    threshold: number,
    freshnessWindowMs: bigint,
    futureSkewMs: bigint,
  ): AdminVerifier {
    return rethrow(
      () =>
        new AdminVerifier(
          NapiAdminVerifier.withFreshness(
            registry.raw,
            threshold,
            freshnessWindowMs,
            futureSkewMs,
          ),
        ),
    );
  }

  /** Build with every policy knob explicit. Primarily for tests
   * that need a short cooldown window. */
  static withFullPolicy(
    registry: OperatorRegistry,
    threshold: number,
    freshnessWindowMs: bigint,
    futureSkewMs: bigint,
    iceCooldownMs: bigint,
  ): AdminVerifier {
    return rethrow(
      () =>
        new AdminVerifier(
          NapiAdminVerifier.withFullPolicy(
            registry.raw,
            threshold,
            freshnessWindowMs,
            futureSkewMs,
            iceCooldownMs,
          ),
        ),
    );
  }

  get threshold(): number {
    return this.raw.threshold;
  }

  get freshnessWindowMs(): bigint {
    return this.raw.freshnessWindowMs;
  }

  get futureSkewMs(): bigint {
    return this.raw.futureSkewMs;
  }

  get iceCooldownMs(): bigint {
    return this.raw.iceCooldownMs;
  }
}
