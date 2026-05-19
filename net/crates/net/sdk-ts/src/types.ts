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
  /** Insertion timestamp (nanoseconds). */
  timestamp: number;
}

/** A stored event from the bus. */
export interface StoredEvent {
  /** Backend-specific event ID. */
  id: string;
  /** Raw JSON payload. */
  raw: string;
  /** Insertion timestamp (nanoseconds). */
  insertionTs: number;
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
