/**
 * The event surface.
 *
 * The wasm node hands the page one JSON **string** per event through
 * `on_event`. This module turns that string into a typed, discriminated
 * object and fans it out to typed listeners and/or an async iterator.
 *
 * Two conventions, both deliberate:
 *
 * - **Tag values stay verbatim** (`channel_message`, not
 *   `channelMessage`). They are the leaf's own protocol vocabulary and
 *   appear in Rust logs, browser logs and Playwright assertions; a
 *   translation layer would only add a place to disagree. Field names
 *   *are* camelCased, because they are TypeScript property accesses.
 * - **`u64` fields are exact decimal strings.** `JSON.parse` rounds
 *   integer literals above 2^53 — `18446744073709551615` parses to
 *   `18446744073709552000` — which would silently mis-route a page
 *   filtering `channel_message` by hash. {@link parseEvent} re-quotes
 *   big integer literals in the known `u64` keys *before* parsing, so
 *   no precision is lost whether the leaf emits numbers or strings.
 */

import { AsyncQueue } from './async-queue.js';
import { fromWasmError, type LeafError } from './errors.js';

/**
 * The `u64` keys in the leaf's event JSON, as they appear on the wire.
 *
 * Exported so a mismatch with the Rust side is one grep away: any new
 * 64-bit field the leaf emits must be listed here or it will be
 * rounded by `JSON.parse`.
 */
export const U64_EVENT_KEYS = [
  'node_id',
  'peer_node',
  'channel_hash',
  'origin_hash',
  'stream_id',
  'dialog',
  'not_after',
  'call_id',
  'seq',
  'generation',
  'version',
  'from',
  'to',
] as const;

/** The node finished its handshake with an anchor and has a session. */
export interface ConnectedEvent {
  readonly type: 'connected';
  /** This node's mesh id, hex. */
  readonly nodeIdHex: string;
  /** The anchor's mesh id, exact decimal. */
  readonly peerNode: string;
  /**
   * The `rtc_addr` that anchor published, verbatim from its
   * `GET /rtc/anchor`. The subject of the STUN probe that turns an
   * ICE timeout into `udp-blocked`; `null` when the anchor published
   * none.
   */
  readonly rtcAddr: string | null;
}

/** The session went away. */
export interface DisconnectedEvent {
  readonly type: 'disconnected';
  readonly reason: string;
}

/** A message on a subscribed channel. */
export interface ChannelMessageEvent {
  readonly type: 'channel_message';
  /** The channel's hash, exact decimal. */
  readonly channelHash: string;
  /** The publisher's origin hash, exact decimal. */
  readonly originHash: string;
  readonly payload: Uint8Array;
}

/** A frame on an open stream, after the consumer-side `seq` reorder. */
export interface StreamDataEvent {
  readonly type: 'stream_data';
  /** The stream's id, exact decimal. */
  readonly streamId: string;
  readonly seq: number;
  readonly payload: Uint8Array;
}

/** A signed capability announcement the leaf ingested. */
export interface AnnouncementEvent {
  readonly type: 'announcement';
  /** The announcing node's mesh id, exact decimal. */
  readonly nodeId: string;
  readonly capabilities: readonly string[];
  readonly rtcAddr: string | null;
  /** Whether the leaf verified the signature. */
  readonly verified: boolean;
}

/** A session-independent signalling envelope addressed to this leaf. */
export interface SignalEvent {
  readonly type: 'signal';
  /** Sender mesh id, exact decimal. */
  readonly from: string;
  /** Recipient mesh id, exact decimal. */
  readonly to: string;
  /** Dialog id, exact decimal. */
  readonly dialog: string;
  readonly kind: string;
  readonly payload: Uint8Array;
  /** Replay bound, exact decimal. */
  readonly notAfter: string;
}

/** An nRPC reply arrived for an in-flight call. */
export interface RpcResponseEvent {
  readonly type: 'rpc_response';
  /** The call's id, exact decimal. */
  readonly callId: string;
  readonly status: number;
}

/** The leaf dropped an inbound frame, and why. */
export interface DroppedEvent {
  readonly type: 'dropped';
  readonly reason: string;
  readonly subprotocol: number;
}

/** An RTC attempt failed, with the leaf's own typed classification. */
export interface RtcFailureEvent {
  readonly type: 'rtc_failure';
  /** Re-typed from the leaf's `Display` text by {@link fromWasmError}. */
  readonly error: LeafError;
}

// `leader_changed` is NOT here: the leaf's own `on_event` never emits
// it — leader election is §8, so `LeaderChangedEvent` is defined once,
// in `src/leader/events.ts`, and reaches a page through
// `SessionEvent`. Declaring it in both unions would put two spellings
// of one tag on the package root, where the explicit export silently
// shadows the star one.

/**
 * A tag this version of the package does not know, carried through
 * intact rather than dropped — a newer leaf must not go silent against
 * an older page.
 */
export interface UnknownEvent {
  readonly type: 'unknown';
  /** The tag as it arrived, or `null` when the JSON had none. */
  readonly tag: string | null;
  /** The parsed object, or the raw text when it was not JSON at all. */
  readonly raw: unknown;
}

/** Every event the leaf can deliver. */
export type LeafEvent =
  | ConnectedEvent
  | DisconnectedEvent
  | ChannelMessageEvent
  | StreamDataEvent
  | AnnouncementEvent
  | SignalEvent
  | RpcResponseEvent
  | DroppedEvent
  | RtcFailureEvent
  | UnknownEvent;

/** `LeafEvent` narrowed by its tag. */
export type LeafEventOf<T extends LeafEvent['type']> = Extract<LeafEvent, { type: T }>;

/**
 * Parse one event JSON string. Never throws: malformed input becomes
 * an {@link UnknownEvent}, because this runs inside a callback the
 * wasm module invokes and an exception there would unwind through
 * Rust.
 */
export function parseEvent(json: string): LeafEvent {
  let raw: unknown;
  try {
    raw = parseJsonPreservingU64(json);
  } catch {
    return { type: 'unknown', tag: null, raw: json };
  }
  if (raw === null || typeof raw !== 'object') {
    return { type: 'unknown', tag: null, raw };
  }

  // Every read below goes through a checked accessor, so an index
  // signature is the whole of what this assertion claims.
  const fields = raw as Record<string, unknown>;
  const tag = typeof fields.type === 'string' ? fields.type : null;

  switch (tag) {
    case 'connected':
      return {
        type: 'connected',
        nodeIdHex: str(fields, 'node_id_hex'),
        peerNode: str(fields, 'peer_node'),
        rtcAddr: optionalStr(fields, 'rtc_addr'),
      };
    case 'disconnected':
      return { type: 'disconnected', reason: str(fields, 'reason') };
    case 'channel_message':
      return {
        type: 'channel_message',
        channelHash: str(fields, 'channel_hash'),
        originHash: str(fields, 'origin_hash'),
        payload: bytes(fields, 'payload'),
      };
    case 'stream_data':
      return {
        type: 'stream_data',
        streamId: str(fields, 'stream_id'),
        seq: num(fields, 'seq'),
        payload: bytes(fields, 'payload'),
      };
    case 'announcement':
      return {
        type: 'announcement',
        nodeId: str(fields, 'node_id'),
        capabilities: strArray(fields, 'capabilities'),
        rtcAddr: optionalStr(fields, 'rtc_addr'),
        verified: fields.verified === true,
      };
    case 'signal':
      return {
        type: 'signal',
        from: str(fields, 'from'),
        to: str(fields, 'to'),
        dialog: str(fields, 'dialog'),
        kind: str(fields, 'kind'),
        payload: bytes(fields, 'payload'),
        notAfter: str(fields, 'not_after'),
      };
    case 'rpc_response':
      return {
        type: 'rpc_response',
        callId: str(fields, 'call_id'),
        status: num(fields, 'status'),
      };
    case 'dropped':
      return {
        type: 'dropped',
        reason: str(fields, 'reason'),
        subprotocol: num(fields, 'subprotocol'),
      };
    case 'rtc_failure':
      return { type: 'rtc_failure', error: fromWasmError(str(fields, 'error')) };
    default:
      return { type: 'unknown', tag, raw: fields };
  }
}

/** A listener's cancel handle. Calling it twice is harmless. */
export type Unsubscribe = () => void;

/**
 * The fan-out behind `node.on` / `node.onEvent` / `node.events()`.
 *
 * One `on_event` callback is registered with the wasm node for the
 * node's lifetime; everything above it is TypeScript. A throwing
 * listener is reported and skipped — it must not take the others down,
 * and it must not unwind into Rust.
 *
 * Generic over the event union so §8's session surface, whose union is
 * `LeafEvent` plus its five lifecycle tags, reuses this fan-out instead
 * of growing a second one beside it. The default parameter keeps the
 * direct surface's type exactly as it was.
 */
export class EventHub<E extends { readonly type: string } = LeafEvent> {
  private readonly typed = new Map<string, Set<(event: E) => void>>();
  private readonly any = new Set<(event: E) => void>();
  private readonly queues = new Set<AsyncQueue<E>>();

  /** Listen for one tag. */
  on<T extends E['type']>(type: T, handler: (event: Extract<E, { type: T }>) => void): Unsubscribe {
    const handlers = this.typed.get(type) ?? new Set<(event: E) => void>();
    this.typed.set(type, handlers);
    // Variance the compiler cannot unify: the map erases the tag, and
    // `dispatch` only ever hands a handler the tag it registered for.
    const erased = handler as (event: E) => void;
    handlers.add(erased);
    return () => {
      handlers.delete(erased);
    };
  }

  /** Listen for every event. */
  onAny(handler: (event: E) => void): Unsubscribe {
    this.any.add(handler);
    return () => {
      this.any.delete(handler);
    };
  }

  /**
   * Consume events as an async iterable.
   *
   * Events that arrive while the consumer is not awaiting are buffered
   * in arrival order, and buffering stops the moment the loop is left
   * (`break`, `return`, or an exception) — so a page that stops
   * consuming stops paying. A page that starts a loop and never awaits
   * it will grow the buffer; leave the loop instead of abandoning it.
   */
  events(): AsyncIterableIterator<E> {
    const queue: AsyncQueue<E> = new AsyncQueue(() => this.queues.delete(queue));
    this.queues.add(queue);
    return queue;
  }

  /**
   * Feed one raw JSON string in — the wasm callback's only job.
   *
   * `parse` defaults to {@link parseEvent}; a surface with a wider
   * union passes its own parser. Like `parseEvent`, a parser handed
   * here must never throw: this runs inside a callback the wasm module
   * invokes, and an exception would unwind through Rust.
   */
  deliver(json: string, parse?: (json: string) => E): void {
    // Without a parser the events are `LeafEvent`s, which is exact for
    // the default `E` and a subset of any wider union built on it —
    // the only instantiations that exist. A surface whose union is not
    // a superset of `LeafEvent` must pass its own parser.
    this.dispatch(parse ? parse(json) : (parseEvent(json) as unknown as E));
  }

  /** Feed one already-parsed event in. */
  dispatch(event: E): void {
    for (const handler of this.typed.get(event.type) ?? []) {
      invoke(handler, event);
    }
    for (const handler of this.any) {
      invoke(handler, event);
    }
    for (const queue of this.queues) {
      queue.push(event);
    }
  }

  /** Release every listener and end every iterator. */
  close(): void {
    this.typed.clear();
    this.any.clear();
    for (const queue of [...this.queues]) {
      queue.end();
    }
    this.queues.clear();
  }
}

function invoke<E>(handler: (event: E) => void, event: E): void {
  try {
    handler(event);
  } catch (error) {
    // A listener's bug is not the node's: report it where a page can
    // see it and keep the remaining listeners running. Throwing here
    // would unwind through the wasm callback frame.
    console.error('[@net-mesh/browser] event listener threw', error);
  }
}

/**
 * `JSON.parse`, minus the `u64` rounding.
 *
 * Unquoted integer literals under the {@link U64_EVENT_KEYS} keys are
 * re-quoted first, so a 64-bit id survives as an exact decimal string.
 * A value the leaf already quoted is left alone, which is why this is
 * a no-op against the current Rust side and still a guard against a
 * regression there.
 */
export function parseJsonPreservingU64(json: string): unknown {
  const keys = U64_EVENT_KEYS.join('|');
  const exact = json.replace(new RegExp(`("(?:${keys})"\\s*:\\s*)(\\d+)`, 'g'), '$1"$2"');
  return JSON.parse(exact);
}

function str(fields: Record<string, unknown>, key: string): string {
  const value = fields[key];
  if (typeof value === 'string') return value;
  if (typeof value === 'number' || typeof value === 'boolean') return String(value);
  return '';
}

function optionalStr(fields: Record<string, unknown>, key: string): string | null {
  const value = fields[key];
  return typeof value === 'string' && value.length > 0 ? value : null;
}

function num(fields: Record<string, unknown>, key: string): number {
  const value = fields[key];
  if (typeof value === 'number') return value;
  if (typeof value === 'string') {
    const parsed = Number(value);
    return Number.isFinite(parsed) ? parsed : 0;
  }
  return 0;
}

function strArray(fields: Record<string, unknown>, key: string): readonly string[] {
  const value = fields[key];
  if (!Array.isArray(value)) return [];
  return value.filter((item): item is string => typeof item === 'string');
}

function bytes(fields: Record<string, unknown>, key: string): Uint8Array {
  const value = fields[key];
  if (value instanceof Uint8Array) return value;
  if (typeof value !== 'string') return new Uint8Array(0);
  return fromBase64(value);
}

/**
 * Standard padded base64 to bytes.
 *
 * `atob`/`btoa` are the one decoder present in every browser this
 * package targets *and* in every Node the package supports (>=20), so
 * there is no second path to keep honest.
 */
export function fromBase64(text: string): Uint8Array {
  const binary = atob(text);
  const out = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i += 1) out[i] = binary.charCodeAt(i);
  return out;
}

/** Bytes to standard padded base64. */
export function toBase64(data: Uint8Array): string {
  let binary = '';
  for (const byte of data) binary += String.fromCharCode(byte);
  return btoa(binary);
}
