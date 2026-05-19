/**
 * Async streaming event consumption.
 */

import type { Net as NapiNet } from '@net-mesh/core';
import type { StoredEvent, SubscribeOpts } from './types';

// Starting backoff for idle polls and the inter-poll wait on
// partial-batch responses. `1` would have us re-issue an FFI poll
// hundreds of times per second on a near-drained stream that returns
// 1-2 events per call; `5` keeps us at a 200/s ceiling on that path
// while still feeling instant. Saturated streams (full `limit`
// batches) skip the sleep entirely and continue to drain at full
// speed.
const DEFAULT_POLL_INTERVAL = 5;
const DEFAULT_MAX_BACKOFF = 100;
const DEFAULT_LIMIT = 100;

/**
 * An async iterable stream of events from the bus.
 *
 * Uses adaptive polling — tight loop when events flow, exponential
 * backoff when idle.
 *
 * @example
 * ```typescript
 * for await (const event of node.subscribe({ limit: 100 })) {
 *   console.log(event.raw);
 * }
 * ```
 */
export class EventStream implements AsyncIterable<StoredEvent> {
  private bus: NapiNet;
  private opts: Required<SubscribeOpts>;
  private cursor?: string;
  private aborted = false;

  constructor(bus: NapiNet, opts: SubscribeOpts = {}) {
    this.bus = bus;
    this.opts = {
      limit: opts.limit ?? DEFAULT_LIMIT,
      filter: opts.filter ?? '',
      ordering: opts.ordering ?? 'none',
      pollIntervalMs: opts.pollIntervalMs ?? DEFAULT_POLL_INTERVAL,
      maxBackoffMs: opts.maxBackoffMs ?? DEFAULT_MAX_BACKOFF,
    };
  }

  /** Stop the stream. */
  stop(): void {
    this.aborted = true;
  }

  async *[Symbol.asyncIterator](): AsyncIterableIterator<StoredEvent> {
    let backoff = this.opts.pollIntervalMs;

    while (!this.aborted) {
      const response = await this.bus.poll({
        limit: this.opts.limit,
        cursor: this.cursor,
        filter: this.opts.filter || undefined,
        ordering: this.opts.ordering,
      });

      if (response.events.length > 0) {
        backoff = this.opts.pollIntervalMs;
        this.cursor = response.nextId ?? undefined;

        for (const event of response.events) {
          yield {
            id: event.id,
            raw: event.raw,
            insertionTs: event.insertionTs,
            shardId: event.shardId,
          };
        }

        // Partial-batch sleep: a poll that returned fewer than `limit`
        // events has drained (or nearly drained) the bus; re-issuing
        // immediately would just spam FFI calls for trickle streams.
        // A full-batch response means more events are queued, so we
        // skip the sleep and loop tight to keep up.
        if (response.events.length < this.opts.limit) {
          await sleep(this.opts.pollIntervalMs);
        }
      } else {
        // Exponential backoff when idle.
        await sleep(backoff);
        backoff = Math.min(backoff * 2, this.opts.maxBackoffMs);
      }
    }
  }
}

/**
 * A typed async iterable stream that deserializes events into `T`.
 *
 * @example
 * ```typescript
 * interface TokenEvent { token: string; index: number; }
 * for await (const token of node.subscribe<TokenEvent>({ limit: 100 })) {
 *   console.log(token.token, token.index);
 * }
 * ```
 */
export class TypedEventStream<T> implements AsyncIterable<T> {
  private inner: EventStream;
  private parse: (raw: string) => T;

  constructor(bus: NapiNet, opts: SubscribeOpts = {}, parse?: (raw: string) => T) {
    this.inner = new EventStream(bus, opts);
    this.parse = parse ?? ((raw: string) => JSON.parse(raw) as T);
  }

  /** Stop the stream. */
  stop(): void {
    this.inner.stop();
  }

  async *[Symbol.asyncIterator](): AsyncIterableIterator<T> {
    for await (const event of this.inner) {
      yield this.parse(event.raw);
    }
  }
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}
