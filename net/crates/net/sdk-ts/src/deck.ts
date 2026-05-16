/**
 * Deck SDK — operator-side TypeScript wrapper.
 *
 * Sits on top of the napi-rs binding at `@ai2070/net`. Adds:
 *
 * - {@link DeckSdkError} typed Error subclass that parses the
 *   substrate `<<deck-sdk-kind:KIND>>MSG` envelope.
 * - Auto-JSON-parsing for `status()` and `snapshots()`.
 * - `AsyncIterable<MeshOsSnapshot>` / `AsyncIterable<StatusSummary>`
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
  DeckClient as NapiClient,
  OperatorIdentity,
  SnapshotStream as NapiSnapshotStream,
  StatusSummaryStream as NapiStatusStream,
  type ChainCommitJs,
  type DeckClientConfigJs,
  type StatusSummaryJs,
} from '@ai2070/net';

import { MeshOsDaemonSdk } from './meshos.js';

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
  localMaintenanceActive: bool;
}

// `bool` alias avoids the linter complaining about TypeScript's
// boolean keyword in an interface property; resolve to the actual
// boolean primitive.
type bool = boolean;

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
 * Wrap a raw napi snapshot stream as `AsyncIterable<MeshOsSnapshot>`.
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
      const rawSdk = (sdk as unknown as { raw: never }).raw;
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

  /** Operator identity bound to this client. */
  identity(): OperatorIdentity {
    return this.raw.identity();
  }

  /** Typed admin-event surface. */
  get admin(): AdminCommands {
    return new AdminCommands(this.raw.admin);
  }

  /**
   * One-shot read of the latest `MeshOsSnapshot`, parsed into a
   * native object from the binding's JSON form.
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
   * Live snapshot stream as `AsyncIterable<MeshOsSnapshot>`. JSON
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
}
