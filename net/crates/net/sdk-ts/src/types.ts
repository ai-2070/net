/**
 * Shared types for the Net SDK.
 */

import type {
  Net as NapiNet,
  EventBusOptions,
  PollOptions,
  PollResponse as NapiPollResponse,
  StoredEvent as NapiStoredEvent,
  IngestResult as NapiIngestResult,
  Stats as NapiStats,
  RedisOptions,
  JetStreamOptions,
} from '@net-mesh/core';

// Re-export NAPI types that users may need.
export type {
  RedisOptions,
  JetStreamOptions,
  NapiNet,
  EventBusOptions,
  NapiPollResponse,
  NapiStoredEvent,
  NapiIngestResult,
  NapiStats,
};

/** Transport configuration. */
export type Transport =
  | { type: 'memory' }
  | ({ type: 'redis' } & RedisOptions)
  | ({ type: 'jetstream' } & JetStreamOptions)
  | { type: 'mesh'; bind: string; peer: string; psk: string; role?: 'initiator' | 'responder'; peerPublicKey?: string; secretKey?: string; publicKey?: string; reliability?: 'none' | 'light' | 'full'; heartbeatIntervalMs?: number; sessionTimeoutMs?: number; batchedIo?: boolean; packetPoolSize?: number };

/** Configuration for creating a NetNode. */
export interface NetNodeConfig {
  /** Number of shards (defaults to CPU core count). */
  shards?: number;
  /** Ring buffer capacity per shard (must be power of 2). */
  bufferCapacity?: number;
  /** Backpressure strategy. */
  backpressure?: 'drop_newest' | 'drop_oldest' | 'fail_producer';
  /** Transport configuration. */
  transport?: Transport;
}

/** Receipt from a successful ingestion. */
export interface Receipt {
  /** The shard the event was assigned to. */
  shardId: number;
  /**
   * Insertion timestamp in **nanoseconds**, as a `bigint`.
   *
   * Not a `number`. Unix-epoch nanoseconds crossed JavaScript's
   * exact-integer ceiling (`2^53 - 1`) around 104 days past 1970, so
   * every realistic value on this field was already losing its
   * low-order digits before this changed.
   *
   * `JSON.stringify` throws on `bigint`. Convert explicitly at the
   * point of display rather than storing a lossy copy:
   *
   * ```ts
   * const timestampMs = Number(timestamp / 1_000_000n);
   * ```
   */
  timestamp: bigint;
}

/** A stored event from the bus. */
export interface StoredEvent {
  /** Backend-specific event ID. */
  id: string;
  /**
   * Raw payload decoded as UTF-8.
   *
   * Deliberately **empty** when the payload is not valid UTF-8 — the
   * native binding does not substitute a lossy decode. A payload
   * emitted through `emitBuffer()` may well not be UTF-8, so check
   * `rawBytes` rather than treating an empty `raw` as an empty event.
   */
  raw: string;
  /**
   * Raw payload bytes, exactly as ingested.
   *
   * The native binding has always preserved these; this wrapper used
   * to drop them from both `poll()` and the streaming projection, so
   * binary accepted through the wrapper's own `emitBuffer()` could not
   * be read back through the same wrapper at all.
   */
  rawBytes: Buffer;
  /**
   * Insertion timestamp in **nanoseconds**, as a `bigint`.
   *
   * Not a `number`. Unix-epoch nanoseconds crossed JavaScript's
   * exact-integer ceiling (`2^53 - 1`) around 104 days past 1970, so
   * every realistic value on this field was already losing its
   * low-order digits before this changed.
   *
   * `JSON.stringify` throws on `bigint`. Convert explicitly at the
   * point of display rather than storing a lossy copy:
   *
   * ```ts
   * const timestampMs = Number(insertionTs / 1_000_000n);
   * ```
   */
  insertionTs: bigint;
  /** Shard ID. */
  shardId: number;
}

/** Poll request options. */
export interface PollRequest {
  /** Maximum events to return. */
  limit: number;
  /** Cursor to resume from. */
  cursor?: string;
  /** JSON filter expression. */
  filter?: string;
  /** Event ordering. */
  ordering?: 'none' | 'insertion_ts';
}

/** Poll response. */
export interface PollResponseData {
  /** Events returned. */
  events: StoredEvent[];
  /** Cursor for the next poll. */
  nextId?: string;
  /** Whether more events are available. */
  hasMore: boolean;
}

/** Ingestion statistics.
 *
 * Counters cross the napi boundary as `bigint` because a long-running
 * bus can outrun `Number.MAX_SAFE_INTEGER` (2^53) over weeks at high
 * event rates. Use `Number(stats.eventsIngested)` for display when you
 * know the value fits; keep as `bigint` for arithmetic.
 */
export interface Stats {
  /** Total events ingested. */
  eventsIngested: bigint;
  /** Events dropped due to backpressure. */
  eventsDropped: bigint;
}

/** Options for subscribing to events. */
export interface SubscribeOpts {
  /** Maximum events per poll batch. */
  limit?: number;
  /** JSON filter expression. */
  filter?: string;
  /** Event ordering. */
  ordering?: 'none' | 'insertion_ts';
  /** Base poll interval in ms. */
  pollIntervalMs?: number;
  /** Maximum backoff interval in ms. */
  maxBackoffMs?: number;
}
