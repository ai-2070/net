/**
 * NetNode — the main SDK handle.
 *
 * Every computer, device, and application is a NetNode.
 * There are no clients, no servers, no coordinators.
 */

import { Net as NapiNet } from '@net-mesh/core';
import type {
  NetNodeConfig,
  Transport,
  Receipt,
  PollRequest,
  PollResponseData,
  Stats,
  SubscribeOpts,
  StoredEvent,
} from './types';
import { EventStream, TypedEventStream } from './stream';
import { TypedChannel } from './channel';

/**
 * A node on the Net mesh.
 *
 * @example
 * ```typescript
 * const node = await NetNode.create({ shards: 4 });
 *
 * node.emit({ token: 'hello', index: 0 });
 *
 * for await (const event of node.subscribe()) {
 *   console.log(event.raw);
 * }
 *
 * await node.shutdown();
 * ```
 */
export class NetNode {
  private bus: NapiNet;

  private constructor(bus: NapiNet) {
    this.bus = bus;
  }

  /**
   * Create a new NetNode.
   */
  static async create(config: NetNodeConfig = {}): Promise<NetNode> {
    const options = buildOptions(config);
    const bus = await NapiNet.create(options);
    return new NetNode(bus);
  }

  /**
   * Create a NetNode from an existing NAPI Net instance.
   */
  static fromNapi(bus: NapiNet): NetNode {
    return new NetNode(bus);
  }

  // ---- Ingestion ----

  /**
   * Emit a typed event (serializes to JSON).
   */
  emit(event: object): Receipt | null {
    const json = JSON.stringify(event);
    const result = this.bus.ingestRawSync(json);
    return { shardId: result.shardId, timestamp: result.timestamp };
  }

  /**
   * Emit a raw JSON string.
   */
  emitRaw(json: string): Receipt | null {
    const result = this.bus.ingestRawSync(json);
    return { shardId: result.shardId, timestamp: result.timestamp };
  }

  /**
   * Emit a raw Buffer (fastest path, zero-copy to Rust).
   */
  emitBuffer(data: Buffer): boolean {
    return this.bus.push(data);
  }

  /**
   * Emit a batch of typed events. Returns number ingested.
   */
  emitBatch(events: object[]): number {
    const jsons = events.map((e) => JSON.stringify(e));
    return this.bus.ingestRawBatchSync(jsons);
  }

  /**
   * Emit a batch of raw JSON strings. Returns number ingested.
   */
  emitRawBatch(jsons: string[]): number {
    return this.bus.ingestRawBatchSync(jsons);
  }

  /**
   * Fire-and-forget ingestion (no return value, maximum speed).
   */
  fire(json: string): boolean {
    return this.bus.ingestFire(json);
  }

  /**
   * Fire-and-forget batch ingestion. Returns count ingested.
   */
  fireBatch(jsons: string[]): number {
    return this.bus.ingestBatchFire(jsons);
  }

  // ---- Consumption ----

  /**
   * One-shot poll for events.
   */
  async poll(request: PollRequest): Promise<PollResponseData> {
    const response = await this.bus.poll({
      limit: request.limit,
      cursor: request.cursor,
      filter: request.filter,
      ordering: request.ordering,
    });
    return {
      events: response.events.map((e) => ({
        id: e.id,
        raw: e.raw,
        insertionTs: e.insertionTs,
        shardId: e.shardId,
      })),
      nextId: response.nextId ?? undefined,
      hasMore: response.hasMore,
    };
  }

  /**
   * Poll a single event (convenience).
   */
  async pollOne(): Promise<StoredEvent | null> {
    const response = await this.poll({ limit: 1 });
    return response.events[0] ?? null;
  }

  /**
   * Subscribe to an async stream of events.
   *
   * @example
   * ```typescript
   * for await (const event of node.subscribe({ limit: 100 })) {
   *   console.log(event.raw);
   * }
   * ```
   */
  subscribe(opts?: SubscribeOpts): EventStream {
    return new EventStream(this.bus, opts);
  }

  /**
   * Subscribe to a typed stream of events.
   *
   * Each event is automatically deserialized from JSON.
   *
   * @example
   * ```typescript
   * for await (const token of node.subscribeTyped<TokenEvent>()) {
   *   console.log(token.token, token.index);
   * }
   * ```
   */
  subscribeTyped<T>(opts?: SubscribeOpts): TypedEventStream<T> {
    return new TypedEventStream<T>(this.bus, opts);
  }

  /**
   * Create a typed channel for pub/sub.
   *
   * @example
   * ```typescript
   * const temps = node.channel<TemperatureReading>('sensors/temperature');
   * temps.publish({ sensor_id: 'A1', celsius: 22.5, timestamp: Date.now() });
   * ```
   */
  channel<T>(name: string, validator?: (data: unknown) => T): TypedChannel<T> {
    return new TypedChannel<T>(this.bus, name, validator);
  }

  // ---- Lifecycle ----

  /** Get ingestion statistics. */
  stats(): Stats {
    return this.bus.stats();
  }

  /** Get the number of active shards. */
  shards(): number {
    return this.bus.numShards();
  }

  /** Flush all pending batches to the adapter. */
  async flush(): Promise<void> {
    await this.bus.flush();
  }

  /** Gracefully shut down the node. */
  async shutdown(): Promise<void> {
    await this.bus.shutdown();
  }

  /**
   * Get the underlying NAPI binding (escape hatch).
   */
  get napi(): NapiNet {
    return this.bus;
  }
}

/** Convert SDK config to NAPI EventBusOptions. */
function buildOptions(config: NetNodeConfig) {
  const options: Record<string, unknown> = {};

  if (config.shards !== undefined) options.numShards = config.shards;
  if (config.bufferCapacity !== undefined) options.ringBufferCapacity = config.bufferCapacity;
  if (config.backpressure !== undefined) options.backpressureMode = config.backpressure;

  if (config.transport) {
    const t = config.transport;
    switch (t.type) {
      case 'memory':
        // No adapter config — uses noop.
        break;
      case 'redis':
        options.redis = {
          url: t.url,
          prefix: t.prefix,
          pipelineSize: t.pipelineSize,
          poolSize: t.poolSize,
          connectTimeoutMs: t.connectTimeoutMs,
          commandTimeoutMs: t.commandTimeoutMs,
          maxStreamLen: t.maxStreamLen,
        };
        break;
      case 'jetstream':
        options.jetstream = {
          url: t.url,
          prefix: t.prefix,
          connectTimeoutMs: t.connectTimeoutMs,
          requestTimeoutMs: t.requestTimeoutMs,
          maxMessages: t.maxMessages,
          maxBytes: t.maxBytes,
          maxAgeMs: t.maxAgeMs,
          replicas: t.replicas,
        };
        break;
      case 'mesh':
        options.net = {
          bindAddr: t.bind,
          peerAddr: t.peer,
          psk: t.psk,
          role: t.role ?? 'initiator',
          peerPublicKey: t.peerPublicKey,
          secretKey: t.secretKey,
          publicKey: t.publicKey,
          reliability: t.reliability,
          heartbeatIntervalMs: t.heartbeatIntervalMs,
          sessionTimeoutMs: t.sessionTimeoutMs,
          batchedIo: t.batchedIo,
          packetPoolSize: t.packetPoolSize,
        };
        break;
    }
  }

  return options;
}
