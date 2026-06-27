/**
 * MeshNode — the multi-peer encrypted mesh handle.
 *
 * Wraps the NAPI `NetMesh` with ergonomic TypeScript APIs: typed
 * `StreamConfig`, classed `BackpressureError` / `NotConnectedError`
 * for `instanceof`-based pattern matching, and the `send_with_retry`
 * / `send_blocking` helpers from the Rust core.
 *
 * @example
 * ```typescript
 * import { MeshNode, BackpressureError, Reliability } from '@net-mesh/sdk';
 *
 * const node = await MeshNode.create({
 *   bindAddr: '127.0.0.1:9000',
 *   psk: '0'.repeat(64),
 * });
 *
 * await node.connect('127.0.0.1:9001', peerPubkey, 0x2222n);
 * node.start();
 *
 * const stream = node.openStream(0x2222n, {
 *   streamId: 7n,
 *   reliability: 'reliable',
 *   windowBytes: 256,
 * });
 *
 * try {
 *   await node.sendOnStream(stream, [Buffer.from('hello')]);
 * } catch (e) {
 *   if (e instanceof BackpressureError) {
 *     // daemon chose: drop, buffer, or retry
 *   } else {
 *     throw e;
 *   }
 * }
 * ```
 */

import { NetMesh as NapiNetMesh } from '@net-mesh/core';
import type { IslandCriteria, IslandTopologyInput } from '@net-mesh/core';

export type { IslandCriteria, IslandTopologyInput } from '@net-mesh/core';

/** Outcome of a reserve/release/claim. */
export type ClaimOutcome = 'won' | 'lost';

/** Selection policy for {@link MeshNode.matchIslands}. */
export type SelectionPolicy = 'least_loaded' | 'pack' | 'load_band' | 'lowest_id';

import { setNapiMesh } from './_internal.js';
import {
  capabilityFilterToNapi,
  capabilitySetToNapi,
  scopeFilterToNapi,
  type CapabilityFilter,
  type CapabilitySet,
  type ScopeFilter,
} from './capabilities';
import {
  aggregationToJson,
  capacityQueryToJson,
  groupByToJson,
  tagMatcherToJson,
  type Aggregation,
  type AggregateRow,
  type CapacityQuery,
  type CapacityRow,
  type GroupBy,
  type TagMatcher,
} from './capability-aggregation';
import type { SubnetId, SubnetPolicy } from './subnets';
import type { Token } from './identity';

/** Reliability mode chosen at stream-open time. */
export type Reliability = 'fire_and_forget' | 'reliable';

/** Per-stream configuration for {@link MeshNode.openStream}. */
export interface StreamConfig {
  /**
   * Caller-chosen stream identifier. Opaque `bigint` at the transport
   * layer; no value range has reserved meaning.
   */
  streamId: bigint;
  /** Reliability mode. Default: `'fire_and_forget'`. */
  reliability?: Reliability;
  /**
   * Initial send-credit window in bytes. Leave unset to inherit the
   * core's `DEFAULT_STREAM_WINDOW_BYTES` (64 KB) — v2 backpressure
   * is ON out of the box. Pass `0` to restore the v1 unbounded-queue
   * behavior on this stream.
   */
  windowBytes?: number;
  /**
   * Fair-scheduler weight. `1` = equal share; higher = proportionally
   * more packets per round. Default: `1`.
   */
  fairnessWeight?: number;
}

/** Per-stream stats snapshot. */
export interface StreamStats {
  txSeq: bigint;
  rxSeq: bigint;
  inboundPending: bigint;
  lastActivityNs: bigint;
  active: boolean;
  /** Cumulative Backpressure rejections since stream opened. */
  backpressureEvents: bigint;
  /**
   * Bytes of send credit still available. `0` means the next send
   * will be rejected as Backpressure. Receiver-driven `StreamWindow`
   * grants replenish this counter.
   */
  txCreditRemaining: number;
  /**
   * Configured initial credit window in bytes. `0` disables
   * backpressure entirely on this stream (escape hatch).
   */
  txWindow: number;
  /** Cumulative StreamWindow grants received from the peer. */
  creditGrantsReceived: bigint;
  /** Cumulative StreamWindow grants emitted to the peer. */
  creditGrantsSent: bigint;
}

/**
 * Thrown by {@link MeshNode.sendOnStream} / `sendWithRetry` /
 * `sendBlocking` when the stream's per-stream in-flight window is
 * full. **The event was NOT sent.** Caller decides whether to drop,
 * retry, or buffer at the app layer — see the "Back-pressure" section
 * in `docs/TRANSPORT.md` for the three canonical patterns.
 */
export class BackpressureError extends Error {
  constructor(detail?: string) {
    super(detail ?? 'stream would block (queue full)');
    this.name = 'BackpressureError';
    Object.setPrototypeOf(this, BackpressureError.prototype);
  }
}

/**
 * Thrown when the stream's peer session is gone (peer never
 * connected, disconnected, or the stream was closed). Distinct from
 * {@link BackpressureError} because this is a "connection lost", not
 * "too fast".
 */
export class NotConnectedError extends Error {
  constructor(detail?: string) {
    super(detail ?? 'stream not connected');
    this.name = 'NotConnectedError';
    Object.setPrototypeOf(this, NotConnectedError.prototype);
  }
}

/**
 * Translate a napi-thrown error into one of the typed stream error
 * classes if it matches the stable prefix contract from the binding.
 * Anything else is passed through unchanged.
 */
function toStreamError(e: unknown): never {
  const msg = (e as Error | undefined)?.message ?? '';
  // Prefixes are part of the binding's stable contract; see
  // `bindings/node/src/lib.rs` (`ERR_BACKPRESSURE_PREFIX` /
  // `ERR_NOT_CONNECTED_PREFIX`).
  if (msg.startsWith('stream would block')) {
    throw new BackpressureError(msg);
  }
  if (msg.startsWith('stream not connected')) {
    throw new NotConnectedError(msg);
  }
  throw e;
}

/**
 * Options for {@link MeshNode.subscribeChannel}. Struct form so
 * future knobs (timeout override, priority) don't break callers.
 */
export interface SubscribeOptions {
  /**
   * Token to present to the publisher. The publisher verifies the
   * ed25519 signature, checks the subject matches the subscribing
   * peer's `EntityId`, and installs the token in its local cache
   * before running `can_subscribe`. A matching token satisfies
   * `requireToken` channels end-to-end.
   */
  token?: Token;
}

/** Options for {@link MeshNode.create}. */
export interface MeshNodeConfig {
  /** Local bind address (e.g. `"127.0.0.1:9000"`). */
  bindAddr: string;
  /** Hex-encoded 32-byte pre-shared key (64 hex chars). */
  psk: string;
  /** Heartbeat interval in milliseconds. Default: 5000. */
  heartbeatIntervalMs?: number;
  /** Session timeout in milliseconds. Default: 30000. */
  sessionTimeoutMs?: number;
  /** Inbound shard count. Default: 4. */
  numShards?: number;
  /**
   * Capability-index GC sweep interval in milliseconds. Default:
   * 60_000. Shorter values make TTL-driven eviction more responsive
   * at the cost of extra CPU; primarily useful in tests.
   */
  capabilityGcIntervalMs?: number;
  /**
   * Drop inbound `CapabilityAnnouncement` packets without a
   * signature. Default: false. Signature *validity* is not yet
   * enforced end-to-end — this is presence-only policy today.
   */
  requireSignedCapabilities?: boolean;
  /**
   * Pin this node to a specific subnet. Omitted = no restriction
   * (`SubnetId::GLOBAL`). Visibility checks on the publish +
   * subscribe paths compare against this value.
   */
  subnet?: SubnetId;
  /**
   * Policy that derives each peer's subnet from their capability
   * announcements. Mesh-wide policy consistency is assumed —
   * mismatched policies lead to asymmetric views of peer subnets.
   */
  subnetPolicy?: SubnetPolicy;
  /**
   * 32-byte ed25519 seed. When set, the mesh's keypair is
   * derived from this seed — so its `entityId` and `nodeId`
   * are reproducible across restarts, and a caller-side
   * `Identity.fromSeed(seed)` can issue tokens that validate
   * against this mesh. Treat as secret material.
   */
  identitySeed?: Buffer;
}

/**
 * An opaque stream handle. Pass back to `sendOnStream` /
 * `sendWithRetry` / `sendBlocking` / `closeStream`. You normally
 * don't need to read the fields — they're exposed for diagnostics.
 */
export interface MeshStream {
  readonly peerNodeId: bigint;
  readonly streamId: bigint;
  /** @internal napi-backed native handle. */
  readonly _native: unknown;
}

/**
 * A node on the Net mesh with full stream multiplexing + backpressure
 * support.
 */
export class MeshNode {
  private native: NapiNetMesh;

  private constructor(native: NapiNetMesh) {
    this.native = native;
    // Register on the WeakMap so sibling SDK modules can reach
    // the native pointer without a public escape-hatch method on
    // the class instance. See `./_internal.ts` for why.
    setNapiMesh(this, native);
  }

  /**
   * **Test-only.** Inject a synthetic peer entry into the local
   * capability index so vitest suites can stage multi-candidate
   * placement for `ReplicaGroup` / `ForkGroup` / `StandbyGroup`
   * tests without a full 3-node handshake.
   *
   * Not part of the stable API; do NOT use in production code —
   * the real mesh surface is `announceCapabilities`. Gated at the
   * NAPI layer behind the `test-helpers` cargo feature; release
   * builds of `@net-mesh/core` do not export `testInjectSyntheticPeer`
   * and this method will throw if called against such a build.
   *
   * @internal
   */
  _testInjectSyntheticPeer(nodeId: bigint): void {
    const native = this.native as unknown as {
      testInjectSyntheticPeer?: (nodeId: bigint) => void;
    };
    if (typeof native.testInjectSyntheticPeer !== 'function') {
      throw new Error(
        'testInjectSyntheticPeer: NAPI build missing `test-helpers` feature',
      );
    }
    native.testInjectSyntheticPeer(nodeId);
  }

  /**
   * Test-only — same shape as {@link _testInjectSyntheticPeer} but
   * stages the synthetic peer with the supplied canonical tag
   * strings. Used by the Phase 6c aggregation smoke tests to set
   * up multi-bucket fixtures without spinning up multiple meshes.
   *
   * @internal
   */
  _testInjectSyntheticPeerWithTags(nodeId: bigint, tags: string[]): void {
    const native = this.native as unknown as {
      testInjectSyntheticPeerWithTags?: (
        nodeId: bigint,
        tags: string[],
      ) => void;
    };
    if (typeof native.testInjectSyntheticPeerWithTags !== 'function') {
      throw new Error(
        'testInjectSyntheticPeerWithTags: NAPI build missing `test-helpers` feature',
      );
    }
    native.testInjectSyntheticPeerWithTags(nodeId, tags);
  }

  /** Create and configure a new mesh node. */
  static async create(config: MeshNodeConfig): Promise<MeshNode> {
    const native = await NapiNetMesh.create({
      bindAddr: config.bindAddr,
      psk: config.psk,
      heartbeatIntervalMs: config.heartbeatIntervalMs,
      sessionTimeoutMs: config.sessionTimeoutMs,
      numShards: config.numShards,
      capabilityGcIntervalMs: config.capabilityGcIntervalMs,
      requireSignedCapabilities: config.requireSignedCapabilities,
      subnet: config.subnet,
      subnetPolicy: config.subnetPolicy,
      identitySeed: config.identitySeed,
    });
    return new MeshNode(native);
  }

  /** 32-byte ed25519 entity id for this mesh. */
  entityId(): Buffer {
    return this.native.entityId();
  }

  /** Hex-encoded Noise static public key. */
  publicKey(): string {
    return this.native.publicKey();
  }

  /** This node's id. */
  nodeId(): bigint {
    return this.native.nodeId();
  }

  /** Connect to a peer as initiator. */
  async connect(peerAddr: string, peerPublicKey: string, peerNodeId: bigint): Promise<void> {
    await this.native.connect(peerAddr, peerPublicKey, peerNodeId);
  }

  /** Accept an incoming connection as responder. Returns the peer's wire address. */
  async accept(peerNodeId: bigint): Promise<string> {
    return await this.native.accept(peerNodeId);
  }

  /** Start the receive loop / heartbeats / router. */
  async start(): Promise<void> {
    await this.native.start();
  }

  /** Number of connected peers. */
  peerCount(): number {
    return this.native.peerCount();
  }

  // ─── Stream API ──────────────────────────────────────────────────

  /**
   * Open (or look up) a logical stream to a connected peer. Repeated
   * calls for the same `(peer, streamId)` are idempotent; the first
   * open wins and later differing configs are logged and ignored.
   */
  openStream(peerNodeId: bigint, config: StreamConfig): MeshStream {
    const native = this.native.openStream(peerNodeId, {
      streamId: config.streamId,
      reliability: config.reliability,
      windowBytes: config.windowBytes,
      fairnessWeight: config.fairnessWeight,
    });
    return {
      peerNodeId,
      streamId: config.streamId,
      _native: native,
    };
  }

  /** Close a stream. Idempotent. */
  closeStream(peerNodeId: bigint, streamId: bigint): void {
    this.native.closeStream(peerNodeId, streamId);
  }

  /**
   * Send a batch of events on an explicit stream. Throws
   * {@link BackpressureError} when the stream's in-flight window is
   * full (no events sent — caller decides what to do),
   * {@link NotConnectedError} when the peer session is gone, or a
   * plain `Error` for underlying transport failures.
   */
  async sendOnStream(stream: MeshStream, events: Buffer[]): Promise<void> {
    try {
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      await this.native.sendOnStream(stream._native as any, events);
    } catch (e) {
      toStreamError(e);
    }
  }

  /**
   * Send events, retrying on {@link BackpressureError} with 5 ms → 200 ms
   * exponential backoff up to `maxRetries` times. Transport errors and
   * `NotConnectedError` are re-thrown immediately (they're not a
   * pressure signal).
   */
  async sendWithRetry(
    stream: MeshStream,
    events: Buffer[],
    maxRetries = 8,
  ): Promise<void> {
    try {
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      await this.native.sendWithRetry(stream._native as any, events, maxRetries);
    } catch (e) {
      toStreamError(e);
    }
  }

  /**
   * Block the calling task until the send succeeds or a transport
   * error occurs. Retries {@link BackpressureError} with 5 ms → 200 ms
   * exponential backoff up to 4096 times (~13 min worst case) —
   * effectively "block until the network lets up" under practical
   * workloads, but with a hard upper bound so runaway pressure can't
   * hang the caller forever. Use {@link sendWithRetry} for a tighter
   * bound.
   */
  async sendBlocking(stream: MeshStream, events: Buffer[]): Promise<void> {
    try {
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      await this.native.sendBlocking(stream._native as any, events);
    } catch (e) {
      toStreamError(e);
    }
  }

  /** Snapshot per-stream stats. `null` if the peer or stream isn't open. */
  streamStats(peerNodeId: bigint, streamId: bigint): StreamStats | null {
    const raw = this.native.streamStats(peerNodeId, streamId);
    if (!raw) return null;
    // The napi binding marshals u64 fields as `BigInt` so values that
    // exceed `Number.MAX_SAFE_INTEGER` — especially `lastActivityNs`,
    // Unix-epoch nanoseconds always above 2^53 — survive the boundary
    // without a precision trap. The u32 fields stay as regular numbers.
    return {
      txSeq: raw.txSeq,
      rxSeq: raw.rxSeq,
      inboundPending: raw.inboundPending,
      lastActivityNs: raw.lastActivityNs,
      active: raw.active,
      backpressureEvents: raw.backpressureEvents,
      txCreditRemaining: raw.txCreditRemaining,
      txWindow: raw.txWindow,
      creditGrantsReceived: raw.creditGrantsReceived,
      creditGrantsSent: raw.creditGrantsSent,
    };
  }

  // =========================================================
  // Channels (distributed pub/sub)
  // =========================================================

  /**
   * Register a channel on this node. Subscribers who ask to join are
   * validated against `config` before being added to the roster.
   *
   * Mirrors the core `ChannelConfig` field-for-field. v1 omits
   * `publishCaps` / `subscribeCaps` — those land with the security
   * plan's identity surface.
   */
  registerChannel(config: ChannelConfig): void {
    try {
      this.native.registerChannel({
        name: config.name,
        visibility: config.visibility,
        reliable: config.reliable,
        requireToken: config.requireToken,
        tokenRoots: config.tokenRoots,
        priority: config.priority,
        maxRatePps: config.maxRatePps,
        publishCaps: config.publishCaps
          ? capabilityFilterToNapi(config.publishCaps)
          : undefined,
        subscribeCaps: config.subscribeCaps
          ? capabilityFilterToNapi(config.subscribeCaps)
          : undefined,
      });
    } catch (e) {
      toChannelError(e);
    }
  }

  /**
   * Ask `publisherNodeId` to add this node to `channel`'s subscriber
   * set. Blocks until the publisher's `Ack` arrives or the
   * membership-ack timeout elapses.
   *
   * Pass `opts.token` to present a
   * {@link Token PermissionToken} issued by the publisher — required
   * when the channel was registered with `tokenRoots` (token
   * enforcement) or when your caps alone don't satisfy
   * `subscribeCaps`. The publisher verifies the presented token chain
   * — it must root at one of the channel's `tokenRoots`, bind at its
   * leaf to this node's entity id, and authorize `subscribe` — before
   * admitting the subscribe. The credential must be presented on every
   * subscribe; a previously-accepted one is not reused for a later
   * bare subscribe.
   *
   * Throws a {@link ChannelAuthError} or {@link ChannelError} on
   * rejection; network-level failures propagate as plain `Error`.
   */
  async subscribeChannel(
    publisherNodeId: bigint,
    channel: string,
    opts?: SubscribeOptions,
  ): Promise<void> {
    try {
      await this.native.subscribeChannel(
        publisherNodeId,
        channel,
        opts?.token?.bytes,
      );
    } catch (e) {
      toChannelError(e);
    }
  }

  /** Mirror of {@link subscribeChannel}. Idempotent on the publisher side. */
  async unsubscribeChannel(publisherNodeId: bigint, channel: string): Promise<void> {
    try {
      await this.native.unsubscribeChannel(publisherNodeId, channel);
    } catch (e) {
      toChannelError(e);
    }
  }

  /**
   * Publish one payload to every subscriber of `channel`. Returns a
   * {@link PublishReport} describing per-peer outcomes.
   */
  async publish(
    channel: string,
    payload: Buffer,
    config?: PublishConfig,
  ): Promise<PublishReport> {
    try {
      const raw = await this.native.publish(channel, payload, {
        reliability: config?.reliability,
        onFailure: config?.onFailure,
        maxInflight: config?.maxInflight,
      });
      return {
        attempted: raw.attempted,
        delivered: raw.delivered,
        errors: raw.errors.map((e: { nodeId: bigint; message: string }) => ({
          nodeId: e.nodeId,
          message: e.message,
        })),
      };
    } catch (e) {
      toChannelError(e);
    }
  }

  /**
   * Announce this node's capabilities to every directly-connected
   * peer. Self-indexes too, so `findNodes` on this same node matches
   * on the announcement. Multi-hop propagation is deferred — peers
   * more than one hop away will not see the announcement.
   */
  async announceCapabilities(caps: CapabilitySet): Promise<void> {
    await this.native.announceCapabilities(capabilitySetToNapi(caps));
  }

  /**
   * Query the local capability index. Returns node ids (including
   * our own `nodeId()` if self matches) whose latest announcement
   * matches `filter`.
   */
  findNodes(filter: CapabilityFilter): bigint[] {
    return this.native.findNodes(capabilityFilterToNapi(filter));
  }

  // ---- Gang-claim resource-island scheduler ----
  //
  // The peer-aware Thunderdome surface. `Reserved` is optimistic/AP;
  // the CP `→ Active` commit is a separate (Rust-only) primitive.

  /**
   * Publish this node's island-topology record (its `host` is forced
   * to this node). Self-indexed locally so this node's own scheduler
   * sees it, then broadcast to peers; returns the peer fan-out count.
   */
  async publishIslandTopology(island: IslandTopologyInput): Promise<number> {
    return this.native.publishIslandTopology(island);
  }

  /**
   * Match islands against `criteria` over this node's capability +
   * island folds (read-only; no claim). Best island first. Empty when
   * nothing matched.
   */
  matchIslands(criteria: IslandCriteria): bigint[] {
    return this.native.matchIslands(criteria);
  }

  /**
   * Reserve `island` (optimistic AP CAS) until `untilUnixUs`
   * (wall-clock micros). `'won'` if this node now holds it, `'lost'`
   * if already held by someone with a live reservation.
   */
  async reserveIsland(island: bigint, untilUnixUs: bigint): Promise<ClaimOutcome> {
    return (await this.native.reserveIsland(island, untilUnixUs)) as ClaimOutcome;
  }

  /**
   * Release `island` this node holds. `'lost'` if this node wasn't the
   * holder.
   */
  async releaseIsland(island: bigint): Promise<ClaimOutcome> {
    return (await this.native.releaseIsland(island)) as ClaimOutcome;
  }

  /**
   * Match + reserve the first available island in one call. Returns
   * its id, or `null` when nothing matched or every match was
   * contended in this node's view.
   */
  async claimIsland(criteria: IslandCriteria, untilUnixUs: bigint): Promise<bigint | null> {
    return this.native.claimIsland(criteria, untilUnixUs);
  }

  /**
   * Scoped variant of {@link findNodes}. Filters candidates through
   * a {@link ScopeFilter} derived from each peer's `scope:*`
   * reserved tags (e.g. `scope:tenant:oem-123`,
   * `scope:region:eu-west`, `scope:subnet-local`).
   *
   * Untagged peers stay visible under most filters by design;
   * peers tagged `scope:subnet-local` only show up under
   * `{ kind: 'sameSubnet' }`. See `docs/SCOPED_CAPABILITIES_PLAN.md`
   * for the full table.
   *
   * @example
   * ```typescript
   * // GPU pool for a specific tenant.
   * const peers = node.findNodesScoped(
   *   { requireTags: ['model:llama3-70b'] },
   *   { kind: 'tenant', tenant: 'oem-123' },
   * );
   * ```
   */
  findNodesScoped(filter: CapabilityFilter, scope: ScopeFilter): bigint[] {
    return this.native.findNodesScoped(
      capabilityFilterToNapi(filter),
      scopeFilterToNapi(scope),
    );
  }

  /**
   * Bucketed aggregation over the local capability fold —
   * `Fold::aggregate(matcher, groupBy, agg)`. Composes a matcher,
   * a bucket-derivation, and a per-bucket reduction into a
   * lex-sorted `[bucket, value][]`. Phase 6c-A of
   * `MULTIFOLD_PHASE_6C_CAPACITY_AGGREGATION.md`.
   *
   * `matcher === undefined` walks every entry.
   *
   * @example
   * ```typescript
   * // Top GPU types by count.
   * const rows = node.capabilityAggregate(
   *   { kind: 'prefix', value: 'hardware.gpu' },
   *   { kind: 'tagStem', prefix: 'hardware.gpu' },
   *   { kind: 'count' },
   * );
   * for (const r of rows) console.log(r.bucket, r.value);
   * ```
   */
  capabilityAggregate(
    matcher: TagMatcher | undefined,
    groupBy: GroupBy,
    aggregation: Aggregation,
  ): AggregateRow[] {
    const matcherJson = matcher ? tagMatcherToJson(matcher) : null;
    const groupByJson = groupByToJson(groupBy);
    const aggregationJson = aggregationToJson(aggregation);
    return this.native.capabilityAggregate(
      matcherJson,
      groupByJson,
      aggregationJson,
    );
  }

  /**
   * Capacity-ranked materialized view —
   * `Fold::capacity_ranking(query, rttLookup)`. Per-bucket state
   * breakdown + latency gate + optional summed numeric capacity,
   * sorted by `available` desc (ties broken on bucket asc) and
   * truncated to `query.limit`. Phase 6c-B.
   *
   * `rttEntries` is the materialized RTT map. `undefined`/empty
   * disables the RTT filter regardless of `query.maxRttMs`. Per
   * the plan, a `ThreadsafeFunction` closure variant is a follow-
   * up; the map shape matches the Go / C wrappers and lines up
   * with what operators typically have cached from the proximity
   * graph.
   *
   * @example
   * ```typescript
   * // Top 5 GPU types available within 50 ms latency.
   * const rttMap = [
   *   { nodeId: 0x1234n, rttMs: 25 },
   *   { nodeId: 0x5678n, rttMs: 75 },
   * ];
   * const rows = node.capabilityCapacityRanking(
   *   {
   *     matcher: { kind: 'prefix', value: 'hardware.gpu' },
   *     groupBy: { kind: 'tagStem', prefix: 'hardware.gpu' },
   *     maxRttMs: 50,
   *     sumAxisKey: 'hardware.gpu.count',
   *     limit: 5,
   *   },
   *   rttMap,
   * );
   * ```
   */
  capabilityCapacityRanking(
    query: CapacityQuery,
    rttEntries?: Array<{ nodeId: bigint; rttMs: number }>,
  ): CapacityRow[] {
    const queryJson = capacityQueryToJson(query);
    return this.native.capabilityCapacityRanking(
      queryJson,
      rttEntries ?? null,
    );
  }

  /** Shutdown the mesh node. */
  async shutdown(): Promise<void> {
    await this.native.shutdown();
  }
}

// =====================================================
// Channel types and errors
// =====================================================

export type Visibility =
  | 'subnet-local'
  | 'parent-visible'
  | 'exported'
  | 'global';

export type OnFailure = 'best_effort' | 'fail_fast' | 'collect';

/** Channel configuration — mirror of the core `ChannelConfig`. */
export interface ChannelConfig {
  /** Canonical channel name. Crosses the boundary as a string. */
  name: string;
  /** Default: `'global'`. */
  visibility?: Visibility;
  /** Default reliability for streams on this channel. */
  reliable?: boolean;
  /**
   * When true, subscribers must present a valid
   * `PermissionToken` whose subject matches their entity id.
   * On its own (no `tokenRoots`) this fails every authorization
   * closed — pass `tokenRoots` to anchor a root of trust.
   */
  requireToken?: boolean;
  /**
   * Root(s) of trust for token authorization: 32-byte entity ids
   * whose signature may root a presented token chain. Setting this
   * turns on token enforcement and anchors the channel — a chain is
   * only honored if its root link was issued by one of these
   * entities (e.g. the publisher that issues subscribe tokens).
   */
  tokenRoots?: Buffer[];
  /** Priority (0 = lowest). */
  priority?: number;
  /** Rate cap in packets per second. */
  maxRatePps?: number;
  /**
   * Capability filter the publisher itself must satisfy before
   * fan-out. `publish` rejects with a `channel:` error on
   * mismatch.
   */
  publishCaps?: CapabilityFilter;
  /**
   * Capability filter each subscriber must satisfy.
   * `subscribeChannel` throws a `ChannelAuthError` on mismatch.
   */
  subscribeCaps?: CapabilityFilter;
}

/** Publish-fanout config — mirror of the core `PublishConfig`. */
export interface PublishConfig {
  /** Default: `'fire_and_forget'`. */
  reliability?: Reliability;
  /** Default: `'best_effort'`. */
  onFailure?: OnFailure;
  /** Max concurrent per-peer sends. Default 32. */
  maxInflight?: number;
}

/** Per-peer report returned by {@link MeshNode.publish}. */
export interface PublishReport {
  attempted: number;
  delivered: number;
  errors: Array<{ nodeId: bigint; message: string }>;
}

/**
 * Raised when a channel operation fails for a reason other than
 * auth. The napi binding emits `"channel: ..."` prefixed errors that
 * the SDK classifies into {@link ChannelAuthError} (unauthorized) or
 * this class (everything else).
 */
export class ChannelError extends Error {
  constructor(detail?: string) {
    super(detail ?? 'channel error');
    this.name = 'ChannelError';
    Object.setPrototypeOf(this, ChannelError.prototype);
  }
}

/**
 * Raised when a Subscribe / Unsubscribe request is rejected because
 * the subscriber isn't authorized on the publisher's channel config.
 */
export class ChannelAuthError extends ChannelError {
  constructor(detail?: string) {
    super(detail ?? 'channel: unauthorized');
    this.name = 'ChannelAuthError';
    Object.setPrototypeOf(this, ChannelAuthError.prototype);
  }
}

function toChannelError(e: unknown): never {
  const msg = (e as Error | undefined)?.message ?? '';
  if (msg.startsWith('channel: unauthorized')) {
    throw new ChannelAuthError(msg);
  }
  if (msg.startsWith('channel:')) {
    throw new ChannelError(msg);
  }
  throw e;
}
