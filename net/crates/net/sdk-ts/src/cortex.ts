/**
 * CortEX + NetDb — typed event-sourced state with reactive watches.
 *
 * Wraps the native `@net-mesh/core` CortEX bindings with ergonomic
 * TypeScript APIs: `AsyncIterable`-shaped watches (so `for await`
 * works naturally), typed errors via `CortexError` / `NetDbError`
 * pattern matching, and the `snapshotAndWatch` primitive whose race
 * fix landed on v2 (see `docs/STORAGE_AND_CORTEX.md`).
 *
 * @example
 * ```typescript
 * import { NetDb, TaskStatus, CortexError } from '@net-mesh/sdk';
 *
 * const db = await NetDb.open({ originHash: 0xABCDEF01, withTasks: true });
 * const tasks = db.tasks!;
 *
 * try {
 *   tasks.create(1n, 'write docs', 100n);
 *   await tasks.waitForSeq(seq);
 * } catch (e) {
 *   if (e instanceof CortexError) { /* handle adapter-level error *\/ }
 *   else { throw e; }
 * }
 *
 * // "Paint what's here now, then react to changes":
 * const { snapshot, updates } = await tasks.snapshotAndWatch({
 *   status: TaskStatus.Pending,
 * });
 * render(snapshot);
 * for await (const next of updates) render(next);
 * ```
 */

import {
  Redex as NapiRedex,
  NetDb as NapiNetDb,
  TasksAdapter as NapiTasksAdapter,
  MemoriesAdapter as NapiMemoriesAdapter,
  TasksSnapshotAndWatch as NapiTasksSnapshotAndWatch,
  MemoriesSnapshotAndWatch as NapiMemoriesSnapshotAndWatch,
  TaskWatchIter as NapiTaskWatchIter,
  MemoryWatchIter as NapiMemoryWatchIter,
  RedexFile as NapiRedexFile,
  RedexTailIter as NapiRedexTailIter,
  WorkflowAdapter as NapiWorkflowAdapter,
  ShardGroup as NapiShardGroup,
  TriggerEngine as NapiTriggerEngine,
  TaskStatus,
  TasksOrderBy,
  MemoriesOrderBy,
} from '@net-mesh/core';

import type {
  Task,
  Memory,
  TaskFilter,
  MemoryFilter,
  NetDbOpenConfig,
  NetDbBundle,
  CortexSnapshot,
  RedexEventJs,
  RedexFileConfigJs,
  WorkflowTaskState,
  WorkflowStatusCounts,
  JoinResult,
  TriggerAction,
} from '@net-mesh/core';

// Re-export the NAPI value types so callers get them from one place.
export {
  TaskStatus,
  TasksOrderBy,
  MemoriesOrderBy,
};

// Re-export NAPI type-only declarations.
export type {
  Task,
  Memory,
  TaskFilter,
  MemoryFilter,
  NetDbOpenConfig,
  NetDbBundle,
  CortexSnapshot,
  WorkflowTaskState,
  WorkflowStatusCounts,
  JoinResult,
  TriggerAction,
};

/**
 * Workflow task lifecycle status. The state machine the
 * {@link WorkflowAdapter} drives — distinct from the cortex tasks-model
 * {@link TaskStatus}.
 */
export type WorkflowTaskStatus =
  | 'submitted'
  | 'running'
  | 'waiting'
  | 'blocked'
  | 'done'
  | 'failed';

// Typed error classes shipped by `@net-mesh/core/errors`. Re-exported here
// so SDK consumers can `import { CortexError } from '@net-mesh/sdk'`
// without a second package path. `classifyError` is what the wrappers
// below use internally.
export { CortexError, NetDbError } from '@net-mesh/core/errors';
import { classifyError } from '@net-mesh/core/errors';

/**
 * Raised on `redex:` prefixed failures: append / tail / read / sync /
 * close, invalid channel names, mutually-exclusive config options.
 * Extends `Error`; catch with `instanceof RedexError`.
 */
export class RedexError extends Error {
  constructor(detail?: string) {
    super(detail ?? 'redex error');
    this.name = 'RedexError';
    Object.setPrototypeOf(this, RedexError.prototype);
  }
}

/** Classify a napi-thrown error. Mirrors `@net-mesh/core/errors` for the
 *  `redex:` prefix which that package does not yet recognize. */
function classifyWithRedex(e: unknown): unknown {
  const classified = classifyError(e);
  if (classified !== e) return classified; // cortex: / netdb: already handled
  const msg = (e as Error | undefined)?.message ?? '';
  if (msg.startsWith('redex:')) return new RedexError(msg);
  return e;
}

// ---------------------------------------------------------------------------
// Redex manager
// ---------------------------------------------------------------------------

/** Construction options for {@link Redex}. */
export interface RedexOptions {
  /**
   * Root directory for disk-backed files. Adapters opened with
   * `persistent: true` write to `<persistentDir>/<channel>/{idx,dat}`
   * and replay from disk on reopen. Omit for in-memory only.
   */
  persistentDir?: string;
}

/**
 * Local RedEX manager. Holds the set of open files on this process.
 * Cheap to share; adapters borrow it by reference.
 */
export class Redex {
  /** @internal */
  readonly napi: NapiRedex;

  constructor(opts?: RedexOptions) {
    this.napi = new NapiRedex(opts?.persistentDir);
  }

  /**
   * Open (or get) a raw RedEX file for domain-agnostic persistent
   * logging. Returns the same handle across repeat calls with the
   * same `name`; `config` is honored only on first open.
   *
   * Use this when you want an append-only event log without the
   * CortEX fold / typed-adapter layer. Appends, tailing, and range
   * reads work directly over the file.
   *
   * With `config.persistent = true`, this manager must have been
   * constructed with a `persistentDir`.
   */
  openFile(name: string, config?: RedexFileConfig): RedexFile {
    try {
      const napiCfg = toNapiFileConfig(config);
      return new RedexFile(this.napi.openFile(name, napiCfg ?? null));
    } catch (e) {
      throw classifyWithRedex(e);
    }
  }
}

// ---------------------------------------------------------------------------
// Raw RedEX file — domain-agnostic event log
// ---------------------------------------------------------------------------

/** Configuration for {@link Redex.openFile}. Mirrors the core
 *  `RedexFileConfig` field-for-field; the two fsync options are
 *  mutually exclusive (leave both unset for the default
 *  `FsyncPolicy::Never`). */
export interface RedexFileConfig {
  /** Disk-backed storage. Requires `Redex` to have been constructed
   *  with `persistentDir`. Default: `false` (heap only). */
  persistent?: boolean;
  /** Fsync after every N appends (`1` fsyncs on every append).
   *  Mutually exclusive with `fsyncIntervalMs`. Ignored unless
   *  `persistent: true`. */
  fsyncEveryN?: bigint;
  /** Fsync on a timer (milliseconds). Mutually exclusive with
   *  `fsyncEveryN`. Ignored unless `persistent: true`. */
  fsyncIntervalMs?: number;
  /** Retain at most N events. */
  retentionMaxEvents?: bigint;
  /** Retain at most N bytes of payload. */
  retentionMaxBytes?: bigint;
  /** Drop entries older than this many milliseconds at the next
   *  retention sweep. */
  retentionMaxAgeMs?: bigint;
}

function toNapiFileConfig(
  c: RedexFileConfig | undefined,
): RedexFileConfigJs | undefined {
  if (!c) return undefined;
  return {
    persistent: c.persistent,
    fsyncEveryN: c.fsyncEveryN,
    fsyncIntervalMs: c.fsyncIntervalMs,
    retentionMaxEvents: c.retentionMaxEvents,
    retentionMaxBytes: c.retentionMaxBytes,
    retentionMaxAgeMs: c.retentionMaxAgeMs,
  };
}

/** A materialized RedEX event. */
export interface RedexEvent {
  seq: bigint;
  payload: Buffer;
  /** Low-28-bit xxh3 truncation stamped at append time. */
  checksum: number;
  /** True if stored inline in the 20-byte entry record. */
  isInline: boolean;
}

function toRedexEvent(raw: RedexEventJs): RedexEvent {
  return {
    seq: raw.seq as bigint,
    payload: raw.payload,
    checksum: raw.checksum,
    isInline: raw.isInline,
  };
}

/** Raw RedEX file handle. Append, tail, range-read without the
 *  CortEX adapter layer. */
export class RedexFile {
  /** @internal */
  readonly napi: NapiRedexFile;

  constructor(inner: NapiRedexFile) {
    this.napi = inner;
  }

  /** Append one payload. Returns the assigned sequence number. */
  append(payload: Buffer): bigint {
    try {
      return this.napi.append(payload);
    } catch (e) {
      throw classifyWithRedex(e);
    }
  }

  /** Append a batch atomically. Returns the seq of the first event
   *  (subsequent events are `first + 0, first + 1, ...`), or `null`
   *  for an empty batch (no seq is allocated when there's nothing to
   *  append). */
  appendBatch(payloads: Buffer[]): bigint | null {
    try {
      return this.napi.appendBatch(payloads);
    } catch (e) {
      throw classifyWithRedex(e);
    }
  }

  /** Read the half-open range `[start, end)` from the in-memory
   *  index. Only retained entries are returned. */
  readRange(start: bigint, end: bigint): RedexEvent[] {
    try {
      return this.napi.readRange(start, end).map(toRedexEvent);
    } catch (e) {
      throw classifyWithRedex(e);
    }
  }

  /** Number of retained events (post-retention eviction). Returned
   *  as `bigint` because event counts can exceed `Number.MAX_SAFE_INTEGER`
   *  (~9 P) in theory — though in practice they'll fit in a `number`,
   *  the lossless type is the safe surface. */
  len(): bigint {
    return this.napi.len() as bigint;
  }

  /** Open a live tail. Yields every event with `seq >= fromSeq`
   *  (default `0n`) — atomically backfills the retained range and
   *  then streams appends. Breaking out of `for await` releases the
   *  native iterator via `return()`. */
  async tail(fromSeq?: bigint): Promise<AsyncIterable<RedexEvent>> {
    try {
      const iter: NapiRedexTailIter = await this.napi.tail(fromSeq ?? null);
      const raw: RawWatchIter<RedexEvent> = {
        // The napi tail emits `redex:` errors for tail-time failures
        // (file closed mid-stream, decode failures on reopen, etc.).
        // Without classifying here, those surface as bare `Error`s
        // — callers doing `instanceof RedexError` miss them.
        async next() {
          try {
            const v = await iter.next();
            return v === null ? null : toRedexEvent(v);
          } catch (e) {
            throw classifyWithRedex(e);
          }
        },
        close: () => iter.close(),
      };
      return wrapWatchIter(raw);
    } catch (e) {
      throw classifyWithRedex(e);
    }
  }

  /** Explicit fsync. Always fsyncs regardless of policy; no-op on
   *  heap-only files. */
  sync(): void {
    try {
      this.napi.sync();
    } catch (e) {
      throw classifyWithRedex(e);
    }
  }

  /** Close the file. Outstanding tail iterators end cleanly on
   *  their next emission. */
  close(): void {
    try {
      this.napi.close();
    } catch (e) {
      throw classifyWithRedex(e);
    }
  }
}

// ---------------------------------------------------------------------------
// AsyncIterable wrapper for pull-based NAPI watch iterators
// ---------------------------------------------------------------------------

/**
 * Minimal shape the NAPI watch iterators expose (`TaskWatchIter`,
 * `MemoryWatchIter`, and the `SnapshotAndWatch` variants). Pull a
 * batch via `next()`; `null` means the iterator has ended or been
 * closed. `close()` terminates early — idempotent.
 */
interface RawWatchIter<T> {
  next(): Promise<T | null>;
  close(): void;
}

/**
 * Turn a pull-based NAPI iterator into an `AsyncIterable` that plays
 * nicely with `for await (...)`. The `return()` hook (fired by `break`
 * / `throw` inside the loop) calls `close()` so native resources are
 * released deterministically — dropping the loop is enough, no manual
 * cleanup needed.
 */
function wrapWatchIter<T>(raw: RawWatchIter<T>): AsyncIterable<T> {
  return {
    [Symbol.asyncIterator](): AsyncIterator<T> {
      let done = false;
      const finish = (): IteratorResult<T> => ({ value: undefined as unknown as T, done: true });
      return {
        async next(): Promise<IteratorResult<T>> {
          if (done) return finish();
          const v = await raw.next();
          if (v === null) {
            done = true;
            return finish();
          }
          return { value: v, done: false };
        },
        async return(): Promise<IteratorResult<T>> {
          if (!done) {
            done = true;
            raw.close();
          }
          return finish();
        },
      };
    },
  };
}

/** Return shape of `snapshotAndWatch` on every adapter. */
export interface SnapshotAndWatch<T> {
  /** Initial filter result captured atomically with the watcher. */
  readonly snapshot: T[];
  /**
   * Subsequent filter results. Drops only leading emissions that
   * equal `snapshot`; any divergent initial emission (caused by a
   * mutation racing construction) is forwarded through.
   */
  readonly updates: AsyncIterable<T[]>;
}

// ---------------------------------------------------------------------------
// Tasks adapter
// ---------------------------------------------------------------------------

/**
 * Typed tasks adapter. CRUD plus `listTasks` / `watch` /
 * `snapshotAndWatch` for reactive consumers.
 */
export class TasksAdapter {
  /** @internal */
  readonly napi: NapiTasksAdapter;

  constructor(inner: NapiTasksAdapter) {
    this.napi = inner;
  }

  /**
   * Open a standalone tasks adapter against a `Redex`. For bundled
   * tasks + memories access, prefer {@link NetDb.open}.
   */
  static async open(
    redex: Redex,
    originHash: bigint,
    opts?: { persistent?: boolean },
  ): Promise<TasksAdapter> {
    try {
      const inner = await NapiTasksAdapter.open(redex.napi, originHash, opts?.persistent ?? null);
      return new TasksAdapter(inner);
    } catch (e) {
      throw classifyError(e);
    }
  }

  /** Restore from a snapshot captured via {@link TasksAdapter.snapshot}. */
  static async openFromSnapshot(
    redex: Redex,
    originHash: bigint,
    snapshot: CortexSnapshot,
    opts?: { persistent?: boolean },
  ): Promise<TasksAdapter> {
    try {
      const inner = await NapiTasksAdapter.openFromSnapshot(
        redex.napi,
        originHash,
        snapshot.stateBytes,
        snapshot.lastSeq ?? null,
        opts?.persistent ?? null,
      );
      return new TasksAdapter(inner);
    } catch (e) {
      throw classifyError(e);
    }
  }

  create(id: bigint, title: string, nowNs: bigint): bigint {
    try {
      return this.napi.create(id, title, nowNs);
    } catch (e) {
      throw classifyError(e);
    }
  }

  rename(id: bigint, newTitle: string, nowNs: bigint): bigint {
    try {
      return this.napi.rename(id, newTitle, nowNs);
    } catch (e) {
      throw classifyError(e);
    }
  }

  complete(id: bigint, nowNs: bigint): bigint {
    try {
      return this.napi.complete(id, nowNs);
    } catch (e) {
      throw classifyError(e);
    }
  }

  delete(id: bigint): bigint {
    try {
      return this.napi.delete(id);
    } catch (e) {
      throw classifyError(e);
    }
  }

  /** Total count in the materialized state (ignores any filter). */
  count(): number {
    try {
      return this.napi.count();
    } catch (e) {
      throw classifyError(e);
    }
  }

  /** Wait for the fold task to have applied every event up through `seq`. */
  async waitForSeq(seq: bigint): Promise<void> {
    try {
      return await this.napi.waitForSeq(seq);
    } catch (e) {
      throw classifyError(e);
    }
  }

  /** Snapshot query over the materialized state. */
  listTasks(filter?: TaskFilter | null): Task[] {
    try {
      return this.napi.listTasks(filter ?? null);
    } catch (e) {
      throw classifyError(e);
    }
  }

  /** Capture a serialized state snapshot for {@link TasksAdapter.openFromSnapshot}. */
  snapshot(): CortexSnapshot {
    try {
      return this.napi.snapshot();
    } catch (e) {
      throw classifyError(e);
    }
  }

  /**
   * Reactive watch. Yields the current filter result first, then once
   * per fold tick where the result differs from the previous emission.
   * Breaking out of `for await` calls `close()` automatically.
   */
  async watch(filter?: TaskFilter | null): Promise<AsyncIterable<Task[]>> {
    try {
      const iter: RawWatchIter<Task[]> = await this.napi.watchTasks(filter ?? null);
      return wrapWatchIter(iter);
    } catch (e) {
      throw classifyError(e);
    }
  }

  /**
   * Atomic "paint what's here now, then react to changes." Returns the
   * snapshot and an `AsyncIterable` of subsequent filter results.
   *
   * Prefer this to calling `listTasks` + `watch` separately — they
   * race each other, and a mutation landing between the two reads
   * would be silently lost.
   */
  async snapshotAndWatch(
    filter?: TaskFilter | null,
  ): Promise<SnapshotAndWatch<Task>> {
    try {
      const combined: NapiTasksSnapshotAndWatch =
        await this.napi.snapshotAndWatchTasks(filter ?? null);
      const iter: RawWatchIter<Task[]> = {
        next: () => combined.next(),
        close: () => combined.close(),
      };
      return {
        snapshot: combined.snapshot,
        updates: wrapWatchIter(iter),
      };
    } catch (e) {
      throw classifyError(e);
    }
  }
}

// ---------------------------------------------------------------------------
// Memories adapter
// ---------------------------------------------------------------------------

/**
 * Typed memories adapter. CRUD plus `listMemories` / `watch` /
 * `snapshotAndWatch`.
 */
export class MemoriesAdapter {
  /** @internal */
  readonly napi: NapiMemoriesAdapter;

  constructor(inner: NapiMemoriesAdapter) {
    this.napi = inner;
  }

  static async open(
    redex: Redex,
    originHash: bigint,
    opts?: { persistent?: boolean },
  ): Promise<MemoriesAdapter> {
    try {
      const inner = await NapiMemoriesAdapter.open(redex.napi, originHash, opts?.persistent ?? null);
      return new MemoriesAdapter(inner);
    } catch (e) {
      throw classifyError(e);
    }
  }

  static async openFromSnapshot(
    redex: Redex,
    originHash: bigint,
    snapshot: CortexSnapshot,
    opts?: { persistent?: boolean },
  ): Promise<MemoriesAdapter> {
    try {
      const inner = await NapiMemoriesAdapter.openFromSnapshot(
        redex.napi,
        originHash,
        snapshot.stateBytes,
        snapshot.lastSeq ?? null,
        opts?.persistent ?? null,
      );
      return new MemoriesAdapter(inner);
    } catch (e) {
      throw classifyError(e);
    }
  }

  store(
    id: bigint,
    content: string,
    tags: string[],
    source: string,
    nowNs: bigint,
  ): bigint {
    try {
      return this.napi.store(id, content, tags, source, nowNs);
    } catch (e) {
      throw classifyError(e);
    }
  }

  retag(id: bigint, tags: string[], nowNs: bigint): bigint {
    try {
      return this.napi.retag(id, tags, nowNs);
    } catch (e) {
      throw classifyError(e);
    }
  }

  pin(id: bigint, nowNs: bigint): bigint {
    try {
      return this.napi.pin(id, nowNs);
    } catch (e) {
      throw classifyError(e);
    }
  }

  unpin(id: bigint, nowNs: bigint): bigint {
    try {
      return this.napi.unpin(id, nowNs);
    } catch (e) {
      throw classifyError(e);
    }
  }

  delete(id: bigint): bigint {
    try {
      return this.napi.delete(id);
    } catch (e) {
      throw classifyError(e);
    }
  }

  count(): number {
    try {
      return this.napi.count();
    } catch (e) {
      throw classifyError(e);
    }
  }

  async waitForSeq(seq: bigint): Promise<void> {
    try {
      return await this.napi.waitForSeq(seq);
    } catch (e) {
      throw classifyError(e);
    }
  }

  listMemories(filter?: MemoryFilter | null): Memory[] {
    try {
      return this.napi.listMemories(filter ?? null);
    } catch (e) {
      throw classifyError(e);
    }
  }

  snapshot(): CortexSnapshot {
    try {
      return this.napi.snapshot();
    } catch (e) {
      throw classifyError(e);
    }
  }

  async watch(filter?: MemoryFilter | null): Promise<AsyncIterable<Memory[]>> {
    try {
      const iter: RawWatchIter<Memory[]> = await this.napi.watchMemories(filter ?? null);
      return wrapWatchIter(iter);
    } catch (e) {
      throw classifyError(e);
    }
  }

  /** Atomic snapshot + delta stream. See {@link TasksAdapter.snapshotAndWatch}. */
  async snapshotAndWatch(
    filter?: MemoryFilter | null,
  ): Promise<SnapshotAndWatch<Memory>> {
    try {
      const combined: NapiMemoriesSnapshotAndWatch =
        await this.napi.snapshotAndWatchMemories(filter ?? null);
      const iter: RawWatchIter<Memory[]> = {
        next: () => combined.next(),
        close: () => combined.close(),
      };
      return {
        snapshot: combined.snapshot,
        updates: wrapWatchIter(iter),
      };
    } catch (e) {
      throw classifyError(e);
    }
  }
}

// ---------------------------------------------------------------------------
// NetDb facade
// ---------------------------------------------------------------------------

/**
 * Unified NetDB handle. Bundles `TasksAdapter` + `MemoriesAdapter`
 * under one object. Open with both models for the common case, or
 * with only one if the other isn't needed.
 */
export class NetDb {
  /** @internal */
  readonly napi: NapiNetDb;

  private constructor(inner: NapiNetDb) {
    this.napi = inner;
  }

  static async open(config: NetDbOpenConfig): Promise<NetDb> {
    try {
      const inner = await NapiNetDb.open(config);
      return new NetDb(inner);
    } catch (e) {
      throw classifyError(e);
    }
  }

  static async openFromSnapshot(
    config: NetDbOpenConfig,
    bundle: NetDbBundle,
  ): Promise<NetDb> {
    try {
      const inner = await NapiNetDb.openFromSnapshot(config, bundle);
      return new NetDb(inner);
    } catch (e) {
      throw classifyError(e);
    }
  }

  /** The tasks adapter, or `null` if `withTasks` wasn't set at open. */
  get tasks(): TasksAdapter | null {
    const t = this.napi.tasks;
    return t ? new TasksAdapter(t) : null;
  }

  /** The memories adapter, or `null` if `withMemories` wasn't set. */
  get memories(): MemoriesAdapter | null {
    const m = this.napi.memories;
    return m ? new MemoriesAdapter(m) : null;
  }

  /** Snapshot every enabled model into one bundle. */
  snapshot(): NetDbBundle {
    try {
      return this.napi.snapshot();
    } catch (e) {
      throw classifyError(e);
    }
  }

  /** Close every enabled adapter. Idempotent. */
  close(): void {
    this.napi.close();
  }
}

/**
 * Task-lifecycle adapter — a single-writer RedEX chain folded into
 * per-task `{ step, status, attempts }`.
 *
 * The companion to the gang-claim scheduler (the `Mesh` island
 * methods): the gang decides *who* holds an exclusive GPU island; this
 * plans what happens *after* it is held. Its status is the
 * {@link WorkflowTaskStatus} lifecycle — distinct from the cortex
 * tasks-model {@link TaskStatus}.
 */
export class WorkflowAdapter {
  readonly napi: NapiWorkflowAdapter;

  constructor(inner: NapiWorkflowAdapter) {
    this.napi = inner;
  }

  /** Open against a Redex manager. */
  static async open(
    redex: Redex,
    originHash: bigint,
    opts?: { persistent?: boolean },
  ): Promise<WorkflowAdapter> {
    try {
      const inner = await NapiWorkflowAdapter.open(
        redex.napi,
        originHash,
        opts?.persistent ?? null,
      );
      return new WorkflowAdapter(inner);
    } catch (e) {
      throw classifyError(e);
    }
  }

  /** Open from a snapshot, skipping replay up through `lastSeq`. */
  static async openFromSnapshot(
    redex: Redex,
    originHash: bigint,
    snapshot: CortexSnapshot,
    opts?: { persistent?: boolean },
  ): Promise<WorkflowAdapter> {
    try {
      const inner = await NapiWorkflowAdapter.openFromSnapshot(
        redex.napi,
        originHash,
        snapshot.stateBytes,
        snapshot.lastSeq ?? null,
        opts?.persistent ?? null,
      );
      return new WorkflowAdapter(inner);
    } catch (e) {
      throw classifyError(e);
    }
  }

  /** Submit a new task — enters at step 0, `submitted`. */
  submit(id: bigint): bigint {
    try {
      return this.napi.submit(id);
    } catch (e) {
      throw classifyError(e);
    }
  }

  /** Set a task's status. A terminal task is never moved. */
  transition(id: bigint, status: WorkflowTaskStatus): bigint {
    try {
      return this.napi.transition(id, status);
    } catch (e) {
      throw classifyError(e);
    }
  }

  /** Mark the task `running`. */
  start(id: bigint): bigint {
    try {
      return this.napi.start(id);
    } catch (e) {
      throw classifyError(e);
    }
  }

  /** Park the task `waiting`. */
  wait(id: bigint): bigint {
    try {
      return this.napi.wait(id);
    } catch (e) {
      throw classifyError(e);
    }
  }

  /** Park the task `blocked`. */
  block(id: bigint): bigint {
    try {
      return this.napi.block(id);
    } catch (e) {
      throw classifyError(e);
    }
  }

  /** Mark the task `done` (terminal success). */
  complete(id: bigint): bigint {
    try {
      return this.napi.complete(id);
    } catch (e) {
      throw classifyError(e);
    }
  }

  /** Mark the task `failed` (terminal failure). */
  fail(id: bigint): bigint {
    try {
      return this.napi.fail(id);
    } catch (e) {
      throw classifyError(e);
    }
  }

  /** Advance the step cursor (bumps step, resets attempts). */
  advance(id: bigint): bigint {
    try {
      return this.napi.advance(id);
    } catch (e) {
      throw classifyError(e);
    }
  }

  /** Retry the current step. Never resurrects a `done` task. */
  retry(id: bigint): bigint {
    try {
      return this.napi.retry(id);
    } catch (e) {
      throw classifyError(e);
    }
  }

  /** Delete a task, reclaiming its whole linked subtree. */
  delete(id: bigint): bigint {
    try {
      return this.napi.delete(id);
    } catch (e) {
      throw classifyError(e);
    }
  }

  /** Record a parent→child lineage edge (idempotent). */
  link(parent: bigint, child: bigint): bigint {
    try {
      return this.napi.link(parent, child);
    } catch (e) {
      throw classifyError(e);
    }
  }

  /** Request cancellation — a worker-observed signal. */
  requestCancel(id: bigint): bigint {
    try {
      return this.napi.requestCancel(id);
    } catch (e) {
      throw classifyError(e);
    }
  }

  /** Current state for `id`, or `null` if unknown. */
  get(id: bigint): WorkflowTaskState | null {
    try {
      return this.napi.get(id);
    } catch (e) {
      throw classifyError(e);
    }
  }

  /** Has cancellation been requested for `id`? */
  isCancelRequested(id: bigint): boolean {
    try {
      return this.napi.isCancelRequested(id);
    } catch (e) {
      throw classifyError(e);
    }
  }

  /** `id` plus all transitive descendants — the delete subtree. */
  subtree(id: bigint): bigint[] {
    try {
      return this.napi.subtree(id);
    } catch (e) {
      throw classifyError(e);
    }
  }

  /** Roll-up of task counts per status. */
  statusCounts(): WorkflowStatusCounts {
    try {
      return this.napi.statusCounts();
    } catch (e) {
      throw classifyError(e);
    }
  }

  /** Capture a state snapshot for restore via `openFromSnapshot`. */
  snapshot(): CortexSnapshot {
    try {
      return this.napi.snapshot();
    } catch (e) {
      throw classifyError(e);
    }
  }

  /** Block until every event up through `seq` has folded. */
  async waitForSeq(seq: bigint): Promise<void> {
    try {
      await this.napi.waitForSeq(seq);
    } catch (e) {
      throw classifyError(e);
    }
  }

  /** Fan out: submit every shard task. Returns the last append seq. */
  fanOut(group: ShardGroup): bigint {
    try {
      return this.napi.fanOut(group.napi);
    } catch (e) {
      throw classifyError(e);
    }
  }

  /** Try to join: submit the reduce once every shard is done, surface
   *  failed shards, or report pending. Idempotent. */
  tryJoin(group: ShardGroup): JoinResult {
    try {
      return this.napi.tryJoin(group.napi);
    } catch (e) {
      throw classifyError(e);
    }
  }
}

// ---- Tier 2: shards + triggers --------------------------------------------

/** A map-reduce shard group: the shard task ids + the reduce task id. */
export class ShardGroup {
  readonly napi: NapiShardGroup;

  constructor(shards: bigint[], reduce: bigint) {
    this.napi = new NapiShardGroup(shards, reduce);
  }
}

/** The pure trigger engine, bound to a {@link WorkflowAdapter} (it reads
 *  the adapter's state internally). Arm a dependency, then drive it with
 *  {@link onTaskChange} — it returns the satisfied actions for the
 *  caller to apply (it starts no tasks itself). */
export class TriggerEngine {
  readonly napi: NapiTriggerEngine;

  constructor(wf: WorkflowAdapter) {
    this.napi = new NapiTriggerEngine(wf.napi);
  }

  /** Arm AfterTask(task) -> action (fires when `task` is done). */
  armAfterTask(task: bigint, action: TriggerAction): void {
    this.napi.armAfterTask(task, action.kind, action.id);
  }

  /** Arm AfterTerminal(task) -> action (fires on done OR failed). */
  armAfterTerminal(task: bigint, action: TriggerAction): void {
    this.napi.armAfterTerminal(task, action.kind, action.id);
  }

  /** `task` changed: return the fired actions (evaluated against the
   *  bound adapter's current state). */
  onTaskChange(task: bigint, tick?: bigint): TriggerAction[] {
    return this.napi.onTaskChange(task, tick ?? null);
  }

  /** Arm AtTick(tick) -> action (fires once the clock reaches `tick`). */
  armAtTick(tick: bigint, action: TriggerAction): void {
    this.napi.armAtTick(tick, action.kind, action.id);
  }

  /** The clock advanced to `now`: return the AtTick actions now due. */
  onTick(now: bigint): TriggerAction[] {
    return this.napi.onTick(now);
  }

  /** Total armed (not-yet-fired) triggers. */
  armedCount(): number {
    return this.napi.armedCount();
  }
}
