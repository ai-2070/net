/**
 * Compute surface — `MeshDaemon` + `DaemonRuntime`.
 *
 * Stage 3 of `SDK_COMPUTE_SURFACE_PLAN.md`. Sub-step 1 lands the
 * skeleton: a caller can build a runtime against an existing
 * {@link MeshNode}, register a factory (stored but not yet
 * invoked), start the runtime, and shut it down. Event delivery,
 * migration, and snapshot/restore land in subsequent sub-steps.
 *
 * @example
 * ```ts
 * import { MeshNode, DaemonRuntime } from '@ai2070/net-sdk';
 *
 * const mesh = await MeshNode.create({ bindAddr: '127.0.0.1:0', psk: '...' });
 * const rt = DaemonRuntime.create(mesh);
 *
 * // Sub-step 1: register a factory shape the TS side can see.
 * // Sub-step 2+ will actually invoke the returned object on
 * // events delivered by Rust.
 * rt.registerFactory('echo', () => ({
 *   name: 'echo',
 *   process: (event) => [event.payload],
 * }));
 *
 * await rt.start();
 * // ... daemons would run here (sub-step 3+) ...
 * await rt.shutdown();
 * ```
 */

import {
  DaemonRuntime as NapiDaemonRuntime,
  DaemonHandle as NapiDaemonHandle,
  MigrationHandle as NapiMigrationHandle,
} from '@ai2070/net';

import { getNapiMesh, setNapiRuntime } from './_internal.js';
import { Identity } from './identity.js';
import { MeshNode } from './mesh.js';

// ----------------------------------------------------------------------------
// Errors — `daemon:` prefix dispatch, mirrors identity/token/cortex pattern.
// ----------------------------------------------------------------------------

/**
 * Base class for daemon-layer errors: factory registration, runtime
 * lifecycle, spawn/stop, migration. The Rust side prefixes every
 * message with `daemon:`; this file peels the prefix and rethrows
 * the typed class so TS callers can `catch (e: DaemonError)`.
 */
export class DaemonError extends Error {
  constructor(message: string) {
    super(message);
    this.name = 'DaemonError';
    Object.setPrototypeOf(this, DaemonError.prototype);
  }
}

/**
 * Stable discriminator for migration-layer failures. Use
 * `err.kind` in catch blocks rather than parsing messages.
 *
 * - `not-ready` — target runtime exists but hasn't called
 *   `start()`. Retriable; source auto-retries by default.
 * - `factory-not-found` — target has no factory for the daemon's
 *   origin_hash. Terminal — target mis-configured.
 * - `compute-not-supported` — target node is a bare `Mesh` with
 *   no `DaemonRuntime`. Terminal.
 * - `state-failed` — snapshot encode/decode or restore failed.
 *   Terminal. `detail` carries the underlying message.
 * - `already-migrating` — a migration is already in flight for
 *   the same origin_hash. Terminal on the duplicate attempt.
 * - `identity-transport-failed` — envelope signature/unseal
 *   failure. Terminal. `detail` carries the underlying message.
 * - `not-ready-timeout` — source exhausted its NotReady-retry
 *   budget. Terminal. `attempts` is the retry count.
 * - `daemon-not-found` — orchestrator couldn't locate the daemon
 *   on the source node. Carries `originHash`.
 * - `target-unavailable` — target node ID isn't in the source's
 *   peer table. Carries `nodeId`.
 * - `wrong-phase` — internal phase-machine violation (shouldn't
 *   surface in practice; carries expected + actual phase).
 * - `snapshot-too-large` — snapshot exceeds the transfer limit;
 *   carries `size` and `max`.
 */
export type MigrationErrorKind =
  | 'not-ready'
  | 'factory-not-found'
  | 'compute-not-supported'
  | 'state-failed'
  | 'already-migrating'
  | 'identity-transport-failed'
  | 'not-ready-timeout'
  | 'daemon-not-found'
  | 'target-unavailable'
  | 'wrong-phase'
  | 'snapshot-too-large'
  | 'unknown';

/**
 * Typed migration failure. Subclass of {@link DaemonError} so
 * `catch (e: DaemonError)` still matches; callers who want to
 * discriminate use `e instanceof MigrationError` + `e.kind`.
 *
 * **Retriability:** only `kind === 'not-ready'` is retriable
 * (the source SDK auto-retries on this by default). Everything
 * else is terminal — a caller's own retry loop won't help.
 */
export class MigrationError extends DaemonError {
  readonly kind: MigrationErrorKind;
  /** Number of NotReady retries on `not-ready-timeout`. */
  readonly attempts?: number;
  /** Daemon origin on `daemon-not-found` / `already-migrating`. */
  readonly originHash?: number;
  /** Node ID on `target-unavailable`. */
  readonly nodeId?: bigint;
  /** Size / max on `snapshot-too-large`. */
  readonly size?: number;
  readonly max?: number;
  /** Underlying string detail on `state-failed` / `identity-transport-failed`. */
  readonly detail?: string;

  constructor(
    kind: MigrationErrorKind,
    message: string,
    extras: {
      attempts?: number;
      originHash?: number;
      nodeId?: bigint;
      size?: number;
      max?: number;
      detail?: string;
    } = {},
  ) {
    super(message);
    this.name = 'MigrationError';
    this.kind = kind;
    this.attempts = extras.attempts;
    this.originHash = extras.originHash;
    this.nodeId = extras.nodeId;
    this.size = extras.size;
    this.max = extras.max;
    this.detail = extras.detail;
    Object.setPrototypeOf(this, MigrationError.prototype);
  }
}

/**
 * Parse a `migration: <kind>[: <detail>]` body (already stripped
 * of the `daemon:` prefix) into a typed {@link MigrationError}.
 * Unknown kinds fall back to `kind: 'unknown'` with the raw body
 * as the message — defensive default so the error surface stays
 * typed even if the Rust side adds new variants.
 */
function parseMigrationError(body: string, fullMessage: string): MigrationError {
  // Body shape (after stripping `migration: `):
  //   <kind>
  //   <kind>: <detail>
  // For some kinds detail is parsed; for others it's free text.
  const afterPrefix = body.slice('migration:'.length).trim();
  const firstColon = afterPrefix.indexOf(':');
  const kind =
    firstColon === -1 ? afterPrefix : afterPrefix.slice(0, firstColon).trim();
  const rest =
    firstColon === -1 ? '' : afterPrefix.slice(firstColon + 1).trim();

  switch (kind) {
    case 'not-ready':
    case 'factory-not-found':
    case 'compute-not-supported':
    case 'already-migrating': {
      // `already-migrating` may also carry an originHash on the
      // orchestrator path; parse it when present.
      if (kind === 'already-migrating' && rest) {
        return new MigrationError(kind, fullMessage, {
          originHash: parseMaybeHex(rest),
        });
      }
      return new MigrationError(kind, fullMessage);
    }
    case 'state-failed':
    case 'identity-transport-failed':
      return new MigrationError(kind, fullMessage, { detail: rest });
    case 'not-ready-timeout':
      return new MigrationError(kind, fullMessage, {
        attempts: Number.parseInt(rest, 10),
      });
    case 'daemon-not-found':
      return new MigrationError(kind, fullMessage, {
        originHash: parseMaybeHex(rest),
      });
    case 'target-unavailable':
      return new MigrationError(kind, fullMessage, {
        nodeId: parseMaybeHexBigInt(rest),
      });
    case 'wrong-phase':
      return new MigrationError(kind, fullMessage, { detail: rest });
    case 'snapshot-too-large': {
      const [sizeStr, maxStr] = rest.split(':').map((s) => s.trim());
      return new MigrationError(kind, fullMessage, {
        size: Number.parseInt(sizeStr ?? '', 10),
        max: Number.parseInt(maxStr ?? '', 10),
      });
    }
    default:
      return new MigrationError('unknown', fullMessage);
  }
}

function parseMaybeHex(s: string): number | undefined {
  const trimmed = s.trim();
  if (!trimmed) return undefined;
  const n = trimmed.startsWith('0x')
    ? Number.parseInt(trimmed.slice(2), 16)
    : Number.parseInt(trimmed, 10);
  return Number.isFinite(n) ? n : undefined;
}

function parseMaybeHexBigInt(s: string): bigint | undefined {
  const trimmed = s.trim();
  if (!trimmed) return undefined;
  try {
    return trimmed.startsWith('0x') ? BigInt(trimmed) : BigInt(trimmed);
  } catch {
    return undefined;
  }
}

function toDaemonError(e: unknown): never {
  const msg = (e as Error | undefined)?.message ?? String(e);
  if (msg.startsWith('daemon:')) {
    const body = msg.slice('daemon:'.length).trim();
    if (body.startsWith('migration:')) {
      throw parseMigrationError(body, msg.slice('daemon:'.length).trim());
    }
    throw new DaemonError(body);
  }
  throw e;
}

/**
 * Phase a migration is currently in. Order is monotonic:
 * `snapshot` → `transfer` → `restore` → `replay` → `cutover` →
 * `complete`. Once complete (or aborted), the orchestrator drops
 * its record and {@link MigrationHandle.phase} returns `null`.
 */
export type MigrationPhase =
  | 'snapshot'
  | 'transfer'
  | 'restore'
  | 'replay'
  | 'cutover'
  | 'complete';

/**
 * Options for {@link DaemonRuntime.startMigrationWith}. Omit any
 * field to take the runtime default.
 */
export interface MigrationOptions {
  /**
   * Seal the daemon's ed25519 seed into the outbound snapshot so
   * the target keeps full signing capability. Default `true`;
   * set `false` for pure compute daemons that only consume events
   * and don't need to sign anything on the target.
   */
  readonly transportIdentity?: boolean;
  /**
   * Retry budget for `NotReady` targets, in milliseconds. Default
   * 30_000 (30 s). Pass `0` to disable retry — the first
   * `NotReady` surfaces as a terminal failure.
   */
  readonly retryNotReadyMs?: bigint;
}

// ----------------------------------------------------------------------------
// MeshDaemon shape — what user factories return.
// ----------------------------------------------------------------------------

/**
 * A causal event delivered to a daemon's `process`. Sub-step 3
 * will plumb this through NAPI; sub-step 1 declares the shape so
 * the factory signature is callable today.
 */
export interface CausalEvent {
  /** 64-bit origin hash of the emitting entity. */
  readonly originHash: bigint;
  /** Sequence number in the emitter's causal chain. */
  readonly sequence: bigint;
  /** Opaque payload bytes. */
  readonly payload: Buffer;
}

/**
 * User-implemented daemon. The object returned by the factory
 * passed to {@link DaemonRuntime.registerFactory}.
 *
 * `process` is synchronous by contract — do not return a Promise.
 * Snapshot/restore are optional; stateless daemons omit them.
 */
export interface MeshDaemon {
  /** Stable human-readable name. Used only for diagnostics. */
  readonly name: string;
  /**
   * Handle one inbound event. Return zero or more output payloads
   * (buffers); each is wrapped in a fresh causal link by the host.
   *
   * Must be synchronous — the core's `process` contract is sync,
   * and the TSFN bridge in sub-step 3 blocks the calling tokio
   * task until this returns.
   */
  process(event: CausalEvent): Buffer[];
  /** Optional: serialize current state for migration / persistence. */
  snapshot?(): Buffer | null;
  /** Optional: restore state from a snapshot produced by `snapshot`. */
  restore?(state: Buffer): void;

  /**
   * Phase 6 of `CAPABILITY_SYSTEM_SDK_PLAN.md` — hard placement
   * requirements declared at factory time. Drives the substrate's
   * `MeshDaemon::required_capabilities`; missing tags veto
   * placement (`StandardPlacement` returns `None` for any
   * candidate node missing a required tag).
   *
   * Static — captured once when the factory returns; not
   * re-queried per placement decision. Omit for "runs anywhere"
   * defaults.
   *
   * Example:
   * ```ts
   * rt.registerFactory('inference', () => ({
   *   name: 'inference',
   *   process: (ev) => doWork(ev.payload),
   *   requiredCapabilities: {
   *     tags: ['hardware.gpu', 'hardware.gpu.vram_gb=24'],
   *   },
   *   optionalCapabilities: {
   *     tags: ['hardware.gpu.vram_gb=80'],
   *   },
   * }));
   * ```
   */
  requiredCapabilities?: import('./capabilities').CapabilitySet;
  /**
   * Phase 6 of `CAPABILITY_SYSTEM_SDK_PLAN.md` — soft placement
   * preferences. Factor into per-axis scoring; missing optional
   * tags don't veto placement (unlike `requiredCapabilities`).
   * Omit for "no preferences" default.
   */
  optionalCapabilities?: import('./capabilities').CapabilitySet;
}

/** A zero-arg function returning a {@link MeshDaemon} or a Promise of one. */
export type DaemonFactory = () => MeshDaemon | Promise<MeshDaemon>;

/**
 * Runtime statistics for a single daemon. Read via
 * {@link DaemonHandle.stats}.
 *
 * All counters are monotonic for the daemon's lifetime. They reset
 * to zero when the daemon is stopped and respawned — the core
 * rebuilds the host, including on {@link DaemonRuntime.spawnFromSnapshot}.
 */
export interface DaemonStats {
  /** Total events processed since spawn. */
  readonly eventsProcessed: bigint;
  /** Total output events emitted since spawn. */
  readonly eventsEmitted: bigint;
  /** Total processing errors surfaced from `process`. */
  readonly errors: bigint;
  /** Number of snapshots taken (manual + auto combined). */
  readonly snapshotsTaken: bigint;
}

/**
 * Host configuration for a daemon. Omit a field to take the
 * runtime default.
 */
export interface DaemonHostConfig {
  /**
   * Auto-snapshot cadence in events processed. `0` (the default) =
   * manual snapshots only.
   */
  readonly autoSnapshotInterval?: bigint;
  /** Maximum events to buffer before forcing a snapshot. */
  readonly maxLogEntries?: number;
  /**
   * Maximum time (milliseconds) the Rust side will wait for a JS
   * `process` / `snapshot` / `restore` callback to return before
   * surfacing a `DaemonError` with a timeout message. Default
   * `60_000` (60 s).
   *
   * **Why it exists.** The core daemon registry holds a per-daemon
   * mutex across `process`. If a user callback re-enters the runtime
   * synchronously on that same daemon, or the Node main thread is
   * blocked and the TSFN callback can't fire, an unbounded wait
   * would deadlock silently. A bounded wait converts the deadlock
   * into a typed error so the daemon's event becomes one failure
   * instead of a frozen runtime.
   *
   * Set a shorter value (e.g. 500) in tests that intentionally
   * stall the callback and assert the timeout path. Set a longer
   * value for daemons that legitimately do heavy sync work per
   * event.
   */
  readonly callbackTimeoutMs?: number;
}

// ----------------------------------------------------------------------------
// DaemonHandle — thin wrapper over the NAPI handle.
// ----------------------------------------------------------------------------

/**
 * Handle to a running daemon. Returned by
 * {@link DaemonRuntime.spawn}; pass its `originHash` back to
 * {@link DaemonRuntime.stop} to tear the daemon down.
 *
 * Cloning the JS object shares the same underlying daemon.
 * Dropping the handle does **not** stop the daemon — callers must
 * call `stop` explicitly.
 */
export class DaemonHandle {
  private readonly inner: NapiDaemonHandle;

  /** @internal */
  constructor(inner: NapiDaemonHandle) {
    this.inner = inner;
  }

  /**
   * 64-bit hash of the daemon's identity — the key used by the
   * registry, factory registry, and migration dispatcher.
   */
  get originHash(): bigint {
    return this.inner.originHash;
  }

  /**
   * Full 32-byte `EntityId` (ed25519 public key) of the daemon's
   * identity. Returned as a `Buffer` to match the convention used
   * by `Identity.entityId`.
   */
  get entityId(): Buffer {
    return this.inner.entityId;
  }

  /**
   * Current runtime statistics for this daemon. Reads a live
   * atomic snapshot from the registry — cheap enough to poll.
   *
   * Throws {@link DaemonError} if the daemon has been stopped.
   */
  stats(): DaemonStats {
    try {
      return this.inner.stats();
    } catch (e) {
      return toDaemonError(e);
    }
  }
}

// ----------------------------------------------------------------------------
// MigrationHandle — observe and abort an in-flight migration.
// ----------------------------------------------------------------------------

/**
 * Handle to an in-flight migration. Returned by
 * {@link DaemonRuntime.startMigration} /
 * {@link DaemonRuntime.startMigrationWith}.
 *
 * Dropping the handle does NOT cancel the migration — the
 * orchestrator keeps driving it to completion in the background.
 * Keep the handle to observe phase transitions or request abort.
 */
export class MigrationHandle {
  private readonly inner: NapiMigrationHandle;

  /** @internal */
  constructor(inner: NapiMigrationHandle) {
    this.inner = inner;
  }

  /** 64-bit origin hash of the daemon being migrated. */
  get originHash(): bigint {
    return this.inner.originHash;
  }

  /** Node ID of the source (currently hosting) node. */
  get sourceNode(): bigint {
    return this.inner.sourceNode;
  }

  /** Node ID of the target (post-cutover) node. */
  get targetNode(): bigint {
    return this.inner.targetNode;
  }

  /**
   * Current migration phase, or `null` once the migration has
   * left the orchestrator's records (terminal success or abort).
   * Callers distinguish success from abort by remembering the
   * last non-null phase they observed.
   */
  phase(): MigrationPhase | null {
    const p = this.inner.phase();
    return (p as MigrationPhase | null) ?? null;
  }

  /**
   * Async iterator that yields each distinct migration phase as
   * the orchestrator transitions through them, and terminates
   * cleanly once the migration reaches a terminal state (either
   * `complete` on success, or abort / failure — the orchestrator
   * record is gone either way).
   *
   * **Usage pattern:**
   * ```ts
   * const mig = await rt.startMigration(origin, a, b);
   * const phases: MigrationPhase[] = [];
   * for await (const phase of mig.phases()) {
   *   phases.push(phase);
   * }
   * // Inspect `phases.at(-1)` — `'complete'` vs anything else
   * // distinguishes success from abort / failure.
   * ```
   *
   * **Call site ordering:** iterate as soon as the handle is
   * returned. If you await `wait()` first and then call
   * `phases()`, the orchestrator record may already be cleared
   * and the iterator yields nothing.
   *
   * **Sampling cadence:** polls every 50 ms — matching the Rust
   * SDK's `wait()` cadence. Phase transitions faster than that
   * may be missed; acceptable for Stage 1 since real migrations
   * spend hundreds of ms per phase on network round-trips. A
   * broadcast-channel push replacement is documented as future
   * work in `DAEMON_IDENTITY_MIGRATION_PLAN.md`.
   */
  async *phases(): AsyncGenerator<MigrationPhase, void, void> {
    let last: MigrationPhase | null = null;
    while (true) {
      const current = this.phase();
      if (current === null) {
        // Orchestrator cleaned up — terminal state reached.
        return;
      }
      if (current !== last) {
        yield current;
        last = current;
      }
      await new Promise((r) => setTimeout(r, 50));
    }
  }

  /**
   * Block until the migration reaches a terminal state. Resolves
   * on `complete`; rejects with {@link DaemonError} on abort or
   * structured failure (target unavailable, restore failed, etc.).
   *
   * No wall-clock timeout — a migration stalled against an
   * unresponsive peer blocks indefinitely. Use
   * {@link MigrationHandle.waitWithTimeout} for a bound.
   */
  async wait(): Promise<void> {
    try {
      await this.inner.wait();
    } catch (e) {
      toDaemonError(e);
    }
  }

  /**
   * Like {@link wait} with a caller-controlled timeout (in
   * milliseconds). On timeout the orchestrator record is aborted
   * and the promise rejects with {@link DaemonError}.
   */
  async waitWithTimeout(timeoutMs: bigint): Promise<void> {
    try {
      await this.inner.waitWithTimeout(timeoutMs);
    } catch (e) {
      toDaemonError(e);
    }
  }

  /**
   * Request cancellation of the migration. Best-effort: past
   * `cutover` the routing flip cannot be undone cleanly, and
   * this call resolves without aborting.
   */
  async cancel(): Promise<void> {
    try {
      await this.inner.cancel();
    } catch (e) {
      toDaemonError(e);
    }
  }
}

// ----------------------------------------------------------------------------
// DaemonRuntime — thin wrapper over the NAPI class.
// ----------------------------------------------------------------------------

/**
 * Per-mesh compute runtime. Holds the kind-keyed factory table and
 * drives the `Registering → Ready → ShuttingDown` lifecycle.
 *
 * Construct via {@link create}; the runtime shares the given mesh's
 * underlying `MeshNode` (no second socket). Shutting down the
 * runtime does NOT shut down the mesh — the caller owns that.
 */
export class DaemonRuntime {
  private readonly inner: NapiDaemonRuntime;

  /**
   * TS-side factory table, keyed by `kind`. `registerFactory`
   * inserts here; `spawn` looks up and invokes. Duplicates the
   * kind set that lives on the NAPI side — the NAPI copy drives
   * migration-targeting and the `already registered` check at
   * registration time; this map is what actually gets *called*.
   */
  private readonly factories: Map<string, DaemonFactory> = new Map();

  private constructor(inner: NapiDaemonRuntime) {
    this.inner = inner;
    // Register on the WeakMap so sibling SDK modules (currently
    // `groups`) can reach the native pointer without a public
    // escape-hatch method on the class instance. See
    // `./_internal.ts` for the rationale.
    setNapiRuntime(this, inner);
  }

  /**
   * Build a compute runtime against an existing {@link MeshNode}.
   */
  static create(mesh: MeshNode): DaemonRuntime {
    try {
      return new DaemonRuntime(NapiDaemonRuntime.create(getNapiMesh(mesh)));
    } catch (e) {
      return toDaemonError(e);
    }
  }

  /**
   * Promote to `Ready`. Installs the migration subprotocol handler.
   * Idempotent on an already-ready runtime; rejects on a runtime
   * that has been shut down.
   */
  async start(): Promise<void> {
    try {
      await this.inner.start();
    } catch (e) {
      toDaemonError(e);
    }
  }

  /**
   * Tear down the runtime. Drains daemons, clears factory
   * registrations, uninstalls the migration handler. Idempotent:
   * a second call on an already-shut-down runtime is a no-op.
   */
  async shutdown(): Promise<void> {
    try {
      await this.inner.shutdown();
    } catch (e) {
      toDaemonError(e);
    }
  }

  /**
   * `true` iff the runtime has transitioned to `Ready` and has not
   * yet begun shutting down.
   */
  isReady(): boolean {
    return this.inner.isReady();
  }

  /** Number of daemons currently registered with the runtime. */
  daemonCount(): number {
    return this.inner.daemonCount();
  }

  /**
   * Register a factory closure under `kind`. The factory returns a
   * {@link MeshDaemon}-shaped object. Second registration of the
   * same `kind` throws {@link DaemonError}.
   *
   * Sub-step 1 stores the factory but does not invoke it — event
   * dispatch to daemon `process` lands in sub-step 3.
   *
   * ## Migration targeting
   *
   * `registerFactory` alone is **not sufficient** to accept
   * inbound migrations — it registers the kind-to-factory mapping
   * only on the SDK side. Migrations lookup by `origin_hash`, not
   * by kind. Future sub-steps will surface `expectMigration` and
   * `registerMigrationTargetIdentity` for that wiring.
   */
  registerFactory(kind: string, factory: DaemonFactory): void {
    try {
      // Register on NAPI so the kind is tracked for migration
      // targeting and so the `already registered` check there
      // fires on duplicate calls before we mutate our own map.
      // The NAPI side stores a TSFN of the factory but doesn't
      // invoke it — the actual invocation happens on the TS side
      // at `spawn` time (see `spawn` below).
      this.inner.registerFactory(kind, factory as unknown as () => unknown);
      this.factories.set(kind, factory);
    } catch (e) {
      toDaemonError(e);
    }
  }

  /**
   * Spawn a daemon of `kind` under the given {@link Identity}.
   *
   * Invokes the user-supplied factory (registered via
   * {@link DaemonRuntime.registerFactory}), extracts the
   * returned daemon's `process` / `snapshot` / `restore`
   * methods, and hands each to NAPI as a separate JS function.
   * NAPI builds a `ThreadsafeFunction` per method so the
   * eventual event-dispatch path (sub-step 3) can call them
   * from any tokio task.
   *
   * **Sub-step 2b** (current): method TSFNs are stored on the
   * Rust side but **not yet invoked**. `process` / `snapshot` /
   * `restore` behave as no-ops. Sub-step 3 wires the full
   * round-trip so events land in the JS daemon.
   *
   * `kind` must have been registered first — spawning an
   * unregistered kind throws {@link DaemonError}.
   */
  async spawn(
    kind: string,
    identity: Identity,
    config?: DaemonHostConfig,
  ): Promise<DaemonHandle> {
    const factory = this.factories.get(kind);
    if (!factory) {
      throw new DaemonError(
        `no factory registered for kind '${kind}'`,
      );
    }

    // Invoke the factory in JS. Accepts both sync and async
    // factories per the `DaemonFactory` type. The returned
    // instance owns its own state (closures, class fields); the
    // method bindings below capture `this` so per-instance state
    // survives across calls.
    const instance = await factory();

    // Method extraction. `snapshot` / `restore` are optional —
    // stateless daemons omit them. `bind(instance)` preserves
    // `this` inside user code when NAPI invokes the function
    // off the main thread via the TSFN.
    //
    // Shape conversion for `process`: the SDK `MeshDaemon.process`
    // returns `Buffer[]`; NAPI's generated type is
    // `(arg: CausalEventJs) => Buffer[]`. Signatures match in
    // practice — the Rust side marshals a full `CausalEventJs`,
    // and the SDK's `MeshDaemon` contract requires `Buffer[]`.
    const process = instance.process.bind(instance) as (
      event: CausalEvent,
    ) => Buffer[];
    const snapshot = instance.snapshot
      ? (instance.snapshot.bind(instance) as () => Buffer | null)
      : undefined;
    const restore = instance.restore
      ? (instance.restore.bind(instance) as (state: Buffer) => unknown)
      : undefined;

    try {
      const handle = await this.inner.spawn(
        kind,
        identity.toNapi(),
        process,
        snapshot,
        restore,
        config
          ? {
              autoSnapshotInterval: config.autoSnapshotInterval,
              maxLogEntries: config.maxLogEntries,
              callbackTimeoutMs: config.callbackTimeoutMs,
            }
          : undefined,
      );
      return new DaemonHandle(handle);
    } catch (e) {
      return toDaemonError(e);
    }
  }

  /**
   * Spawn a daemon of `kind` from a previously-taken snapshot.
   * Parallel to {@link DaemonRuntime.spawn} but seeds the
   * daemon's initial state from `snapshotBytes` by calling its
   * `restore` method before any events land.
   *
   * `snapshotBytes` must be the exact `Buffer` returned by a
   * prior call to {@link DaemonRuntime.snapshot}; mismatched or
   * corrupted bytes surface as `daemon: snapshot decode failed`.
   *
   * `kind` must be registered and the caller's {@link Identity}
   * must match the snapshot's `entityId` — a mismatch throws
   * {@link DaemonError} before any side effects.
   */
  async spawnFromSnapshot(
    kind: string,
    identity: Identity,
    snapshotBytes: Buffer,
    config?: DaemonHostConfig,
  ): Promise<DaemonHandle> {
    const factory = this.factories.get(kind);
    if (!factory) {
      throw new DaemonError(`no factory registered for kind '${kind}'`);
    }

    // Same factory-decomposition dance as `spawn`: invoke in JS,
    // extract methods, hand them to NAPI as separate functions.
    // The daemon's initial (pre-restore) state is built by the
    // factory here; the core's `from_snapshot` will then call
    // `restore` on the bridge with `snapshotBytes`.
    const instance = await factory();
    const process = instance.process.bind(instance) as (
      event: CausalEvent,
    ) => Buffer[];
    const snapshot = instance.snapshot
      ? (instance.snapshot.bind(instance) as () => Buffer | null)
      : undefined;
    const restore = instance.restore
      ? (instance.restore.bind(instance) as (state: Buffer) => unknown)
      : undefined;

    try {
      const handle = await this.inner.spawnFromSnapshot(
        kind,
        identity.toNapi(),
        snapshotBytes,
        process,
        snapshot,
        restore,
        config
          ? {
              autoSnapshotInterval: config.autoSnapshotInterval,
              maxLogEntries: config.maxLogEntries,
              callbackTimeoutMs: config.callbackTimeoutMs,
            }
          : undefined,
      );
      return new DaemonHandle(handle);
    } catch (e) {
      return toDaemonError(e);
    }
  }

  /**
   * Take a snapshot of a running daemon by `originHash`. Returns
   * the daemon's serialized state bytes, or `null` if the daemon
   * is stateless (no `snapshot` method, or it returned `null`).
   *
   * The returned `Buffer` is opaque to the caller — the wire
   * format is the core's `StateSnapshot` encoding, including
   * version headers and the chain link at the snapshot point.
   * Feed it unchanged to {@link DaemonRuntime.spawnFromSnapshot}
   * to restore the daemon on another node or after a restart.
   */
  async snapshot(originHash: bigint): Promise<Buffer | null> {
    try {
      const buf = await this.inner.snapshot(originHash);
      return buf ?? null;
    } catch (e) {
      return toDaemonError(e);
    }
  }

  /**
   * Stop a daemon, removing it from the runtime's registry.
   * Idempotent during `ShuttingDown`; rejects with
   * {@link DaemonError} during `Registering` or when the origin
   * is unknown.
   */
  async stop(originHash: bigint): Promise<void> {
    try {
      await this.inner.stop(originHash);
    } catch (e) {
      toDaemonError(e);
    }
  }

  /**
   * Deliver a single causal event to a live daemon and return
   * the daemon's output buffers. Routes through the core
   * `DaemonRegistry::deliver` → `MeshDaemon::process` path,
   * which invokes the JS `process(event)` callback registered
   * at spawn time and waits for its return.
   *
   * Direct ingress — Stage 1 convenience. Mesh-dispatched
   * delivery (via the causal subprotocol on an inbound packet)
   * lands in a later stage; this method stays as test sugar + a
   * manual-trigger surface.
   *
   * Throws {@link DaemonError} if `originHash` doesn't match a
   * live daemon, if the daemon's `process` throws, or if the
   * runtime is shutting down.
   */
  async deliver(
    originHash: bigint,
    event: CausalEvent,
  ): Promise<Buffer[]> {
    try {
      return await this.inner.deliver(originHash, {
        originHash: event.originHash,
        sequence: event.sequence,
        payload: event.payload,
      });
    } catch (e) {
      return toDaemonError(e);
    }
  }

  /**
   * Initiate a migration for the daemon identified by
   * `originHash`, moving it from `sourceNode` to `targetNode`.
   *
   * Returns a {@link MigrationHandle} whose `wait()` resolves
   * when the migration reaches a terminal state. On local-source
   * migrations (`sourceNode === mesh.nodeId`) the snapshot is
   * taken synchronously inside this call; on remote-source
   * migrations the orchestrator drives the state machine via
   * inbound wire messages.
   *
   * Both node IDs are `u64` — pass as `bigint` to avoid silent
   * precision loss past 2^53.
   */
  async startMigration(
    originHash: bigint,
    sourceNode: bigint,
    targetNode: bigint,
  ): Promise<MigrationHandle> {
    try {
      const handle = await this.inner.startMigration(
        originHash,
        sourceNode,
        targetNode,
      );
      return new MigrationHandle(handle);
    } catch (e) {
      return toDaemonError(e);
    }
  }

  /**
   * {@link startMigration} with caller-supplied options. Use this
   * to opt out of identity transport (when the daemon doesn't
   * need to sign on the target) or to tune the NotReady-retry
   * budget.
   */
  async startMigrationWith(
    originHash: bigint,
    sourceNode: bigint,
    targetNode: bigint,
    opts: MigrationOptions,
  ): Promise<MigrationHandle> {
    try {
      const handle = await this.inner.startMigrationWith(
        originHash,
        sourceNode,
        targetNode,
        {
          transportIdentity: opts.transportIdentity,
          retryNotReadyMs: opts.retryNotReadyMs,
        },
      );
      return new MigrationHandle(handle);
    } catch (e) {
      return toDaemonError(e);
    }
  }

  /**
   * Declare that a migration will land on this node for the given
   * `originHash` of `kind`. Registers a placeholder factory; the
   * migration snapshot's identity envelope supplies the real
   * keypair at restore time.
   *
   * Must be called BEFORE the source initiates the migration —
   * the target dispatcher checks for a factory entry when the
   * inbound `SnapshotReady` lands, and rejects with
   * `FactoryNotFound` if nothing is registered.
   *
   * The source must migrate with `transportIdentity: true`
   * (default). Without the envelope the dispatcher emits
   * `IdentityTransportFailed` because the placeholder has no
   * keypair. Use {@link registerMigrationTargetIdentity} for the
   * explicit public-identity-migration case.
   */
  expectMigration(
    kind: string,
    originHash: bigint,
    config?: DaemonHostConfig,
  ): void {
    try {
      this.inner.expectMigration(
        kind,
        originHash,
        config
          ? {
              autoSnapshotInterval: config.autoSnapshotInterval,
              maxLogEntries: config.maxLogEntries,
              callbackTimeoutMs: config.callbackTimeoutMs,
            }
          : undefined,
      );
    } catch (e) {
      toDaemonError(e);
    }
  }

  /**
   * Pre-register a target-side identity for a migration that
   * will NOT carry an identity envelope (source used
   * `transportIdentity: false`). The target holds the matching
   * {@link Identity}; the dispatcher restores the daemon with
   * that identity instead of overriding it from an envelope.
   *
   * For the common envelope-transport case, prefer
   * {@link expectMigration} — the caller doesn't need to know
   * the daemon's private key ahead of time.
   */
  registerMigrationTargetIdentity(
    kind: string,
    identity: Identity,
    config?: DaemonHostConfig,
  ): void {
    try {
      this.inner.registerMigrationTargetIdentity(
        kind,
        identity.toNapi(),
        config
          ? {
              autoSnapshotInterval: config.autoSnapshotInterval,
              maxLogEntries: config.maxLogEntries,
              callbackTimeoutMs: config.callbackTimeoutMs,
            }
          : undefined,
      );
    } catch (e) {
      toDaemonError(e);
    }
  }

  /**
   * Query the orchestrator's current migration phase for
   * `originHash`, or `null` if no migration is in flight for
   * that origin. Works on any node — source, target, or an
   * observer that heard the migration on the mesh.
   */
  migrationPhase(originHash: bigint): MigrationPhase | null {
    const p = this.inner.migrationPhase(originHash);
    return (p as MigrationPhase | null) ?? null;
  }
}
