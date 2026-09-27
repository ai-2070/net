/**
 * `persistStore` / `restoreStore` — a dedicated host's store document,
 * snapshotted to RedEX and restored on start.
 *
 * ```ts
 * import { Redex, meshStoreTransport, persistStore, restoreStore } from '@net-mesh/sdk';
 * import { hostStore } from '@net-mesh/browser';
 *
 * const redex = new Redex({ persistentDir: './state' });
 * const file = redex.openFile('game/world', { persistent: true, retentionMaxEvents: 4n });
 * const restored = restoreStore(file, world);          // null on a first start
 * const host = hostStore({ definition: world, initialState: restored?.state ?? fresh(), … });
 * const saving = persistStore(host, { file, intervalMs: 5_000 });
 * // … on shutdown:
 * saving.close();                                      // one last snapshot, then stop
 * ```
 *
 * A snapshot is the whole document as JSON, tagged with the definition's id
 * and version. `persistStore` appends one on an interval **only when the
 * document changed** (the store keeps unchanged documents by identity, so
 * "changed" is one comparison), and again on `close()` / `flush()`. Keep the
 * file's retention small (`retentionMaxEvents`): only the newest snapshot is
 * ever read back, and older ones are only a fallback if the newest does not
 * validate.
 *
 * `restoreStore` reads the retained snapshots newest first and returns the
 * first whose id and version match the definition AND that passes its
 * `state` validator. A snapshot from another store or another version is
 * skipped, never coerced: migrations between versions are the application's.
 *
 * Structural, like {@link meshStoreTransport}: this package does not depend on
 * `@net-mesh/browser`, so the host and definition are described by the few
 * members used here.
 */

import type { RedexFile } from './cortex';

/** What {@link persistStore} reads from a hosted store handle. */
export interface PersistableStore<S> {
  readonly definition: PersistableDefinition<S>;
  getState(): S;
}

/** What {@link restoreStore} and {@link persistStore} read from a store definition. */
export interface PersistableDefinition<S> {
  readonly id: string;
  readonly version: number;
  /** The definition's whole-document validator. */
  readonly state: (value: unknown) => S;
}

/** Options for {@link persistStore}. */
export interface PersistStoreOptions {
  /** Where snapshots go: a RedEX file, `persistent: true` to survive a restart. */
  readonly file: RedexFile;
  /** Snapshot interval, ms (default 5000). `0` disables the timer: `flush()` only. */
  readonly intervalMs?: number;
  /** Fsync after each snapshot (default `true`). */
  readonly sync?: boolean;
  /** Wall clock for the snapshot's `at` (default `Date.now`). */
  readonly now?: () => number;
  /** Told about a snapshot that could not be written; the next tick tries again. */
  readonly onError?: (error: unknown) => void;
}

/** The running snapshotter {@link persistStore} returns. */
export interface StorePersistence {
  /** Snapshot now if the document changed since the last one; `true` if one was written. */
  flush(): boolean;
  /** One last {@link flush}, then stop the timer. Idempotent. */
  close(): void;
  /** Snapshots written so far. */
  readonly snapshots: number;
}

/** A snapshot {@link restoreStore} found. */
export interface RestoredStore<S> {
  /** The document, validated by the definition. */
  readonly state: S;
  /** When it was taken (the snapshotter's clock, ms). */
  readonly at: number;
  /** Its RedEX sequence number. */
  readonly seq: bigint;
}

const KIND = 'net-store-snapshot';
const FORMAT = 1;

interface SnapshotFrame {
  readonly k: typeof KIND;
  readonly v: typeof FORMAT;
  readonly id: string;
  readonly version: number;
  readonly at: number;
  readonly state: unknown;
}

/**
 * Snapshot `store`'s document to `options.file` on an interval and on
 * `close()`. See the module comment.
 */
export function persistStore<S>(store: PersistableStore<S>, options: PersistStoreOptions): StorePersistence {
  const { file } = options;
  const now = options.now ?? Date.now;
  const sync = options.sync ?? true;
  const intervalMs = options.intervalMs ?? 5_000;
  if (!Number.isFinite(intervalMs) || intervalMs < 0) {
    throw new RangeError(`persistStore: intervalMs must be a non-negative number, got ${String(intervalMs)}`);
  }
  // The document last written. The store keeps an unchanged document by
  // identity, so this one comparison is the whole dirty check.
  let saved: S | undefined;
  let snapshots = 0;
  let closed = false;

  const flush = (): boolean => {
    const state = store.getState();
    if (saved !== undefined && Object.is(state, saved)) return false;
    const frame: SnapshotFrame = {
      k: KIND,
      v: FORMAT,
      id: store.definition.id,
      version: store.definition.version,
      at: now(),
      state,
    };
    file.append(Buffer.from(JSON.stringify(frame), 'utf8'));
    if (sync) file.sync();
    saved = state;
    snapshots += 1;
    return true;
  };

  const tick = (): void => {
    try {
      flush();
    } catch (error) {
      options.onError?.(error);
    }
  };
  const timer = intervalMs > 0 ? setInterval(tick, intervalMs) : null;
  // A snapshotter is not a reason to keep the process alive.
  (timer as { unref?: () => void } | null)?.unref?.();

  return {
    flush: () => {
      if (closed) return false;
      return flush();
    },
    close: () => {
      if (closed) return;
      closed = true;
      if (timer !== null) clearInterval(timer);
      flush();
    },
    get snapshots() {
      return snapshots;
    },
  };
}

/**
 * The newest retained snapshot in `file` for `definition`, validated; or
 * `null` when there is none. See the module comment.
 */
export function restoreStore<S>(file: RedexFile, definition: PersistableDefinition<S>): RestoredStore<S> | null {
  // Every retained entry; retention bounds how many that is.
  const events = file.readRange(0n, (1n << 64n) - 1n);
  for (let i = events.length - 1; i >= 0; i -= 1) {
    const event = events[i]!;
    let frame: Partial<SnapshotFrame>;
    try {
      frame = JSON.parse(event.payload.toString('utf8')) as Partial<SnapshotFrame>;
    } catch {
      continue;
    }
    if (
      frame === null ||
      typeof frame !== 'object' ||
      frame.k !== KIND ||
      frame.v !== FORMAT ||
      frame.id !== definition.id ||
      frame.version !== definition.version ||
      typeof frame.at !== 'number'
    ) {
      continue;
    }
    let state: S;
    try {
      state = definition.state(frame.state);
    } catch {
      continue;
    }
    return { state, at: frame.at, seq: event.seq };
  }
  return null;
}
