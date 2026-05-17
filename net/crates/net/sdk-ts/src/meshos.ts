/**
 * MeshOS daemon-author SDK — TypeScript wrapper.
 *
 * Sits on top of the napi-rs binding at `@ai2070/net`. Adds:
 *
 * - {@link MeshOsDaemon} interface for daemon implementors.
 * - Typed {@link DaemonControl} / {@link MaintenanceState} unions
 *   the binding emits as plain POJOs.
 * - {@link MeshOsSdkError} typed Error subclass that parses the
 *   substrate `<<meshos-sdk-kind:KIND>>MSG` envelope into a
 *   structured `.kind` field.
 * - `AsyncIterable<DaemonControl>` over the raw `nextControl()`
 *   napi method.
 *
 * Slice 1 (Phase 3) ships: `start` / `registerDaemon` /
 * `nextControl` / `tryNextControl` / `publishLog` /
 * `gracefulShutdown` / `metadata` / `refreshMetadata`. Capability
 * publishing is a substrate-side stub (`publishCapabilities`
 * returns without committing). Snapshot / restore work via the
 * daemon object's optional methods; the supervisor invokes them
 * on migration.
 *
 * @example
 * ```ts
 * import { Identity } from '@ai2070/net';
 * import { MeshOsDaemonSdk, type MeshOsDaemon } from '@ai2070/net-sdk/meshos';
 *
 * const daemon: MeshOsDaemon = {
 *   name: 'echo',
 *   process: (event) => [event.payload],
 * };
 *
 * const sdk = MeshOsDaemonSdk.start();
 * const handle = sdk.registerDaemon(daemon, Identity.generate());
 * handle.publishLog('info', 'started');
 *
 * for await (const ev of handle.controlEvents()) {
 *   if (ev.kind === 'Shutdown') break;
 * }
 *
 * await handle.gracefulShutdown(5_000n);
 * await sdk.shutdown();
 * ```
 */

import {
  MeshOsDaemonHandle as NapiHandle,
  MeshOsDaemonSdk as NapiSdk,
  type CausalEventJs,
  type DaemonControlJs,
  type MetadataViewJs,
  type MaintenanceStateJs,
  type PeerSnapshotJs,
  type PeerSnapshotEntryJs,
  type MeshOsConfigJs,
  type Identity,
  type CapabilitySetJs,
} from '@ai2070/net';

// ----------------------------------------------------------------------------
// Typed error envelope
// ----------------------------------------------------------------------------

/**
 * Typed error raised by the MeshOS SDK. The Rust side embeds a
 * `<<meshos-sdk-kind:KIND>>MSG` envelope in the message; this
 * class parses the kind out so callers can branch programmatically
 * instead of regexing the message.
 */
export class MeshOsSdkError extends Error {
  readonly kind: string;

  constructor(kind: string, message: string) {
    super(`<<meshos-sdk-kind:${kind}>>${message}`);
    this.name = 'MeshOsSdkError';
    this.kind = kind;
    Object.setPrototypeOf(this, MeshOsSdkError.prototype);
  }

  /**
   * Parse the envelope on a caught napi error. Returns the typed
   * subclass when the envelope is present; falls through to the
   * original error otherwise.
   */
  static fromCaught(err: unknown): MeshOsSdkError | Error {
    if (err instanceof MeshOsSdkError) return err;
    if (!(err instanceof Error)) {
      return new Error(String(err));
    }
    const parsed = parseEnvelope(err.message);
    if (!parsed) return err;
    return new MeshOsSdkError(parsed.kind, parsed.body);
  }
}

function parseEnvelope(message: string): { kind: string; body: string } | null {
  const marker = '<<meshos-sdk-kind:';
  const start = message.indexOf(marker);
  if (start === -1) return null;
  const kindStart = start + marker.length;
  const end = message.indexOf('>>', kindStart);
  if (end === -1) return null;
  return { kind: message.slice(kindStart, end), body: message.slice(end + 2) };
}

/**
 * Run an operation, rethrowing the envelope as a typed
 * `MeshOsSdkError` whose `.kind` callers can branch on.
 */
function rethrow<T>(fn: () => T): T {
  try {
    return fn();
  } catch (e) {
    throw MeshOsSdkError.fromCaught(e);
  }
}

async function rethrowAsync<T>(fn: () => Promise<T>): Promise<T> {
  try {
    return await fn();
  } catch (e) {
    throw MeshOsSdkError.fromCaught(e);
  }
}

// ----------------------------------------------------------------------------
// Public types — match the napi-emitted POJO shapes
// ----------------------------------------------------------------------------

export type LogLevel = 'trace' | 'debug' | 'info' | 'warn' | 'error';

export type DaemonControlShutdown = { kind: 'Shutdown'; gracePeriodMs: bigint };
export type DaemonControlDrainStart = { kind: 'DrainStart'; gracePeriodMs: bigint };
export type DaemonControlDrainFinish = { kind: 'DrainFinish' };
export type DaemonControlBackpressureOn = { kind: 'BackpressureOn'; level: number };
export type DaemonControlBackpressureOff = { kind: 'BackpressureOff' };
export type DaemonControlUnknown = { kind: 'Unknown' };

export type DaemonControl =
  | DaemonControlShutdown
  | DaemonControlDrainStart
  | DaemonControlDrainFinish
  | DaemonControlBackpressureOn
  | DaemonControlBackpressureOff
  | DaemonControlUnknown;

export type MaintenanceStateActive = { kind: 'Active' };
export type MaintenanceStateEntering = {
  kind: 'EnteringMaintenance';
  sinceMs: bigint;
  deadlineRemainingMs: bigint | null;
};
export type MaintenanceStateSteady = { kind: 'Maintenance'; sinceMs: bigint };
export type MaintenanceStateExiting = { kind: 'ExitingMaintenance'; sinceMs: bigint };
export type MaintenanceStateDrainFailed = {
  kind: 'DrainFailed';
  sinceMs: bigint;
  reason: string;
};
export type MaintenanceStateRecovery = { kind: 'Recovery'; sinceMs: bigint };
export type MaintenanceStateUnknown = { kind: 'Unknown' };

export type MaintenanceState =
  | MaintenanceStateActive
  | MaintenanceStateEntering
  | MaintenanceStateSteady
  | MaintenanceStateExiting
  | MaintenanceStateDrainFailed
  | MaintenanceStateRecovery
  | MaintenanceStateUnknown;

export type PeerHealth = 'Healthy' | 'Degraded' | 'Unreachable' | 'Unknown';
export type PeerMaintenance =
  | 'Active'
  | 'EnteringMaintenance'
  | 'Maintenance'
  | 'ExitingMaintenance'
  | 'DrainFailed'
  | 'Recovery'
  | 'Unknown';

export interface PeerSnapshot {
  rttMs: bigint | null;
  health: PeerHealth | null;
  maintenance: PeerMaintenance | null;
  cpuLoad1m: number | null;
  memUsedBytes: bigint | null;
  memTotalBytes: bigint | null;
  diskUsedBytes: bigint | null;
  diskTotalBytes: bigint | null;
  saturationTrend: number | null;
  capabilitySet: string[];
  softwareVersion: string | null;
  forkedFrom: bigint | null;
}

export interface MetadataView {
  nodeId: bigint;
  daemonId: bigint;
  daemonName: string;
  maintenanceState: MaintenanceState;
  /** Keyed by peer node id. The binding emits a list of entries
   * so BigInt keys round-trip cleanly; we materialize a Map here. */
  peers: Map<bigint, PeerSnapshot>;
}

export interface CausalEvent {
  originHash: bigint;
  sequence: bigint;
  payload: Buffer;
}

// ----------------------------------------------------------------------------
// Daemon interface — what a user implements
// ----------------------------------------------------------------------------

/**
 * A MeshOS-supervised daemon. Required: `name` (string property) +
 * `process(event)` (returns `Buffer[]` per inbound event, or `[]`
 * for sinks).
 *
 * Optional methods (the substrate falls back to defaults if
 * absent):
 *  - `snapshot()` — returns the daemon's serialized state, or
 *    `null` when stateless.
 *  - `restore(state: Buffer)` — re-seed state at migration target.
 *  - `onControl(event: DaemonControl)` — trait callback; the SDK
 *    *also* delivers the same event over `controlEvents()`. Use
 *    whichever delivery model fits.
 */
export interface MeshOsDaemon {
  name: string;
  process(event: CausalEvent): Buffer[];
  snapshot?(): Buffer | null;
  restore?(state: Buffer): void;
  onControl?(event: DaemonControl): void;
}

// ----------------------------------------------------------------------------
// Config
// ----------------------------------------------------------------------------

export interface MeshOsConfig {
  thisNode?: bigint;
  tickIntervalMs?: bigint;
  eventQueueCapacity?: number;
  actionQueueCapacity?: number;
}

export interface MeshOsDaemonSdkOptions {
  /** Per-daemon control-channel capacity. Default 8 events. */
  controlCapacity?: number;
  /** Max time (ms) the bridge waits for a JS callback (`process`
   * / `snapshot` / `restore` / `onControl`) to respond. Default
   * 60_000. */
  callbackTimeoutMs?: number;
}

// ----------------------------------------------------------------------------
// MeshOsDaemonSdk — entry point
// ----------------------------------------------------------------------------

export class MeshOsDaemonSdk {
  private constructor(private readonly raw: NapiSdk) {}

  /**
   * @internal Accessor for sibling SDK modules (currently the
   * Deck SDK's `DeckClient.fromMeshos`) that need to compose
   * against the underlying napi handle. Not part of the public
   * API; calling it from consumer code is unsupported.
   */
  __rawNapiSdk(): NapiSdk {
    return this.raw;
  }

  /**
   * Start the SDK with optional config + the substrate's
   * `LoggingDispatcher`. Async so the napi-side factory runs in
   * the napi tokio context (a nested local runtime would deadlock).
   */
  static async start(
    config?: MeshOsConfig,
    options?: MeshOsDaemonSdkOptions,
  ): Promise<MeshOsDaemonSdk> {
    return rethrowAsync(async () => {
      const raw = await NapiSdk.start(
        config ? toNapiConfig(config) : undefined,
        options?.controlCapacity,
        options?.callbackTimeoutMs,
      );
      return new MeshOsDaemonSdk(raw);
    });
  }

  /**
   * Register a daemon under the supplied identity. The daemon
   * object must satisfy {@link MeshOsDaemon}; the binding eagerly
   * resolves `process` (and optional `snapshot` / `restore` /
   * `onControl`) into TSFNs at registration time, so a missing
   * `process` raises immediately rather than later on the first
   * event.
   */
  async registerDaemon(
    daemon: MeshOsDaemon,
    identity: Identity,
  ): Promise<MeshOsDaemonHandle> {
    return rethrowAsync(async () => {
      // The napi `registerDaemon` accepts a plain object with the
      // typed callable fields. Cast through `unknown` because the
      // generated d.ts references an internal `DaemonObjectTsfns`
      // type that isn't reified at the napi boundary.
      const handle = await this.raw.registerDaemon(
        daemon as unknown as never,
        identity,
      );
      return new MeshOsDaemonHandle(handle);
    });
  }

  /** Diagnostic counter — total control events the router dropped
   * across every registered daemon because a daemon's channel was
   * full. */
  async droppedControlEvents(): Promise<bigint> {
    return rethrowAsync(() => this.raw.droppedControlEvents());
  }

  /** Tear down the wrapped runtime. Subsequent calls throw
   * `MeshOsSdkError(kind: "already_shutdown")`. */
  async shutdown(): Promise<void> {
    await rethrowAsync(() => this.raw.shutdown());
  }
}

// ----------------------------------------------------------------------------
// MeshOsDaemonHandle — per-daemon handle
// ----------------------------------------------------------------------------

export class MeshOsDaemonHandle {
  constructor(private readonly raw: NapiHandle) {}

  /** Substrate identifier (origin hash). Stable across the
   * handle's lifetime, readable after shutdown. */
  get daemonId(): bigint {
    return this.raw.daemonId;
  }

  /** Daemon's `name` at registration. Readable after shutdown. */
  get daemonName(): string {
    return this.raw.daemonName;
  }

  /** Cached metadata view. Refresh via {@link refreshMetadata}. */
  async metadata(): Promise<MetadataView> {
    return rethrowAsync(async () => fromNapiMetadata(await this.raw.metadata()));
  }

  /** Rebuild the metadata view from the runtime's latest snapshot. */
  async refreshMetadata(): Promise<MetadataView> {
    return rethrowAsync(async () =>
      fromNapiMetadata(await this.raw.refreshMetadata()),
    );
  }

  /** Block until the next control event arrives, the runtime
   * shuts down, or `timeoutMs` elapses. Resolves to the event or
   * `null` on timeout / shutdown. */
  async nextControl(timeoutMs?: bigint): Promise<DaemonControl | null> {
    return rethrowAsync(async () => {
      const ev = await this.raw.nextControl(timeoutMs);
      return ev ? fromNapiControl(ev) : null;
    });
  }

  /** Non-blocking control-event receive. Returns the next event
   * or `null` if the channel is empty. */
  async tryNextControl(): Promise<DaemonControl | null> {
    return rethrowAsync(async () => {
      const ev = await this.raw.tryNextControl();
      return ev ? fromNapiControl(ev) : null;
    });
  }

  /** Async-iterable view over control events. Terminates when
   * `gracefulShutdown` runs or the runtime exits. Use:
   *
   * ```ts
   * for await (const ev of handle.controlEvents()) {
   *   if (ev.kind === 'Shutdown') break;
   * }
   * ```
   */
  controlEvents(): AsyncIterable<DaemonControl> {
    const handle = this;
    return {
      [Symbol.asyncIterator]() {
        return {
          async next(): Promise<IteratorResult<DaemonControl>> {
            try {
              const ev = await handle.nextControl();
              if (ev === null) return { value: undefined, done: true };
              return { value: ev, done: false };
            } catch (e) {
              // After `gracefulShutdown` the napi handle throws
              // `already_shutdown`. Terminate cleanly.
              const err = MeshOsSdkError.fromCaught(e);
              if (err instanceof MeshOsSdkError && err.kind === 'already_shutdown') {
                return { value: undefined, done: true };
              }
              throw err;
            }
          },
          async return(): Promise<IteratorResult<DaemonControl>> {
            return { value: undefined, done: true };
          },
        };
      },
    };
  }

  /** Publish a log line tagged with this daemon's id. Non-blocking
   * on the substrate side; the napi call is async because every
   * handle method serializes on the inner mutex. Throws
   * `MeshOsSdkError(kind: "queue_full" | "loop_closed")` when the
   * substrate's log ring is saturated. */
  async publishLog(level: LogLevel, message: string): Promise<void> {
    await rethrowAsync(() => this.raw.publishLog(level, message));
  }

  /** Publish (or update) the daemon's capability set. Slice 1 is
   * a substrate-side stub — the call returns without committing. */
  async publishCapabilities(caps?: CapabilitySetJs | null): Promise<void> {
    await rethrowAsync(() => this.raw.publishCapabilities(caps ?? null));
  }

  /** Drive a graceful shutdown. Sends
   * `Shutdown { gracePeriodMs }` on the daemon's control channel,
   * parks for `graceMs`, then unregisters. Consumes the handle —
   * subsequent calls throw `already_shutdown`. */
  async gracefulShutdown(graceMs?: bigint): Promise<void> {
    await rethrowAsync(() => this.raw.gracefulShutdown(graceMs));
  }
}

// ----------------------------------------------------------------------------
// POJO converters
// ----------------------------------------------------------------------------

function toNapiConfig(c: MeshOsConfig): MeshOsConfigJs {
  return {
    thisNode: c.thisNode,
    tickIntervalMs: c.tickIntervalMs,
    eventQueueCapacity: c.eventQueueCapacity,
    actionQueueCapacity: c.actionQueueCapacity,
  };
}

function fromNapiControl(ev: DaemonControlJs): DaemonControl {
  switch (ev.kind) {
    case 'Shutdown':
      return { kind: 'Shutdown', gracePeriodMs: ev.gracePeriodMs ?? 0n };
    case 'DrainStart':
      return { kind: 'DrainStart', gracePeriodMs: ev.gracePeriodMs ?? 0n };
    case 'DrainFinish':
      return { kind: 'DrainFinish' };
    case 'BackpressureOn':
      return { kind: 'BackpressureOn', level: ev.level ?? 0 };
    case 'BackpressureOff':
      return { kind: 'BackpressureOff' };
    default:
      return { kind: 'Unknown' };
  }
}

function fromNapiMaintenance(m: MaintenanceStateJs): MaintenanceState {
  switch (m.kind) {
    case 'Active':
      return { kind: 'Active' };
    case 'EnteringMaintenance':
      return {
        kind: 'EnteringMaintenance',
        sinceMs: m.sinceMs ?? 0n,
        deadlineRemainingMs: m.deadlineRemainingMs ?? null,
      };
    case 'Maintenance':
      return { kind: 'Maintenance', sinceMs: m.sinceMs ?? 0n };
    case 'ExitingMaintenance':
      return { kind: 'ExitingMaintenance', sinceMs: m.sinceMs ?? 0n };
    case 'DrainFailed':
      return {
        kind: 'DrainFailed',
        sinceMs: m.sinceMs ?? 0n,
        reason: m.reason ?? '',
      };
    case 'Recovery':
      return { kind: 'Recovery', sinceMs: m.sinceMs ?? 0n };
    default:
      return { kind: 'Unknown' };
  }
}

function fromNapiPeer(p: PeerSnapshotJs): PeerSnapshot {
  return {
    rttMs: p.rttMs ?? null,
    health: (p.health as PeerHealth | null) ?? null,
    maintenance: (p.maintenance as PeerMaintenance | null) ?? null,
    // napi-derive emits `cpu_load_1m` as `cpuLoad1M` (camelCase
    // capitalizes digit-adjacent letters); accept that form.
    cpuLoad1m: p.cpuLoad1M ?? null,
    memUsedBytes: p.memUsedBytes ?? null,
    memTotalBytes: p.memTotalBytes ?? null,
    diskUsedBytes: p.diskUsedBytes ?? null,
    diskTotalBytes: p.diskTotalBytes ?? null,
    saturationTrend: p.saturationTrend ?? null,
    capabilitySet: p.capabilitySet,
    softwareVersion: p.softwareVersion ?? null,
    forkedFrom: p.forkedFrom ?? null,
  };
}

function fromNapiMetadata(v: MetadataViewJs): MetadataView {
  const peers = new Map<bigint, PeerSnapshot>();
  for (const entry of v.peers as PeerSnapshotEntryJs[]) {
    peers.set(entry.nodeId, fromNapiPeer(entry.snapshot));
  }
  return {
    nodeId: v.nodeId,
    daemonId: v.daemonId,
    daemonName: v.daemonName,
    maintenanceState: fromNapiMaintenance(v.maintenanceState),
    peers,
  };
}

// Re-export the convenience for callers who already imported from this module.
export type { CausalEventJs };
