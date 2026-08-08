/**
 * Typed channels — strongly typed pub/sub over named channels.
 */

import type { Net as NapiNet } from '@net-mesh/core';
import type { SubscribeOpts, StoredEvent } from './types';
import { EventStream, TypedEventStream } from './stream';

/**
 * Maximum channel-name length in bytes. Mirrors `MAX_NAME_LEN` in
 * `net/crates/net/src/adapter/net/channel/name.rs`.
 */
export const MAX_CHANNEL_NAME_LEN = 255;

/** A channel name violated the canonical Net naming grammar. */
export class ChannelNameError extends Error {
  constructor(message: string) {
    super(message);
    this.name = 'ChannelNameError';
  }
}

const ALLOWED_CHANNEL_CHAR = /^[a-z0-9\-_./]$/;

/**
 * Validate `name` against the canonical Net channel-name grammar.
 *
 * The TypeScript mirror of `ChannelName::validate` in
 * `net/crates/net/src/adapter/net/channel/name.rs`. The Rust type is the
 * only constructor for a distributed mesh channel name, but the
 * ergonomic tagged-topic wrapper here never reaches it — `publish()`
 * embeds the name in generic EventBus JSON as `_channel`. Without this
 * check a name the mesh would reject is accepted locally and only
 * fails, or silently splits a namespace, once the same string is used
 * against a real mesh channel.
 *
 * Returns `name` unchanged so callers can validate inline.
 * @throws {ChannelNameError} on any violation.
 */
export function validateChannelName(name: string): string {
  if (typeof name !== 'string') {
    throw new ChannelNameError(
      `channel name must be a string, got ${typeof name}`
    );
  }
  if (name.length === 0) {
    throw new ChannelNameError('channel name must not be empty');
  }
  // Byte length, not UTF-16 code-unit length — Rust bounds `name.len()`.
  const byteLen = new TextEncoder().encode(name).length;
  if (byteLen > MAX_CHANNEL_NAME_LEN) {
    throw new ChannelNameError(
      `channel name too long: ${byteLen} bytes (max ${MAX_CHANNEL_NAME_LEN})`
    );
  }
  if (name.startsWith('/') || name.endsWith('/')) {
    throw new ChannelNameError("channel name must not start or end with '/'");
  }
  if (name.includes('//')) {
    throw new ChannelNameError("channel name must not contain '//'");
  }
  // Iterate by code point (spread), not by UTF-16 unit, so a surrogate
  // pair reports as one character rather than two lone halves.
  for (const ch of name) {
    // Uppercase gets its own message: `foo.bar` and `FOO.BAR` would
    // otherwise be distinct channels with distinct hashes, registry
    // entries, and ACL entries — an operator who locked down
    // `prod.deploy` would silently leave `Prod.deploy` open.
    if (ch >= 'A' && ch <= 'Z') {
      throw new ChannelNameError(
        `uppercase character '${ch}' not allowed — channel names are lowercase only`
      );
    }
    if (!ALLOWED_CHANNEL_CHAR.test(ch)) {
      throw new ChannelNameError(`invalid character '${ch}' in channel name`);
    }
  }
  // Channel names double as on-disk directory segments under the
  // `redex-disk` feature: `..` would escape the persistence root and
  // `.` would alias the current directory.
  for (const seg of name.split('/')) {
    if (seg === '.' || seg === '..') {
      throw new ChannelNameError(`path segment '${seg}' is reserved`);
    }
  }
  return name;
}

/**
 * A strongly typed channel for publishing and subscribing to events.
 *
 * @example
 * ```typescript
 * interface TemperatureReading {
 *   sensor_id: string;
 *   celsius: number;
 *   timestamp: number;
 * }
 *
 * const temps = node.channel<TemperatureReading>('sensors/temperature');
 * temps.publish({ sensor_id: 'A1', celsius: 22.5, timestamp: Date.now() });
 *
 * for await (const reading of temps.subscribe()) {
 *   console.log(`${reading.sensor_id}: ${reading.celsius}°C`);
 * }
 * ```
 */
export class TypedChannel<T> {
  private bus: NapiNet;
  private channelName: string;
  private validator?: (data: unknown) => T;
  // Filter is a constant for the lifetime of the channel; build the
  // JSON string once instead of regenerating it on every subscribe /
  // subscribeRaw call.
  private readonly filter: string;

  constructor(bus: NapiNet, channelName: string, validator?: (data: unknown) => T) {
    this.bus = bus;
    this.channelName = validateChannelName(channelName);
    this.validator = validator;
    this.filter = JSON.stringify({ path: '_channel', value: channelName });
  }

  /** The channel name. */
  get name(): string {
    return this.channelName;
  }

  /**
   * Publish a typed event to this channel.
   *
   * The event is serialized to JSON with the channel name embedded.
   */
  publish(event: T): boolean {
    const payload = JSON.stringify({
      ...event as object,
      _channel: this.channelName,
    });
    return this.bus.ingestFire(payload);
  }

  /**
   * Publish a batch of typed events to this channel.
   * Returns the number of events successfully published.
   */
  publishBatch(events: T[]): number {
    const payloads = events.map((event) =>
      JSON.stringify({
        ...event as object,
        _channel: this.channelName,
      })
    );
    return this.bus.ingestBatchFire(payloads);
  }

  /**
   * Subscribe to typed events on this channel.
   *
   * Returns an async iterable that deserializes and optionally validates
   * each event.
   */
  subscribe(opts: SubscribeOpts = {}): TypedEventStream<T> {
    const mergedOpts: SubscribeOpts = {
      ...opts,
      filter: opts.filter ?? this.filter,
    };

    const parse = this.validator
      ? (raw: string) => this.validator!(JSON.parse(raw))
      : (raw: string) => JSON.parse(raw) as T;

    return new TypedEventStream<T>(this.bus, mergedOpts, parse);
  }

  /**
   * Subscribe to raw events on this channel.
   */
  subscribeRaw(opts: SubscribeOpts = {}): EventStream {
    const mergedOpts: SubscribeOpts = {
      ...opts,
      filter: opts.filter ?? this.filter,
    };
    return new EventStream(this.bus, mergedOpts);
  }
}
