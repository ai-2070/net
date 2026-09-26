/**
 * Netcode frames and the transport they ride.
 *
 * Every frame is small JSON tagged with the netcode instance's label
 * (`n`), so several netcode instances — and the store — can share one
 * node: a frame for another label is not this instance's. All frames ride
 * a fire-and-forget **lossy** stream (`openStream({ lossy: true })`): the
 * protocol is built for loss (inputs are sent redundantly, snapshots
 * supersede each other, pings are repeated).
 */

/** The structural transport netcode uses: `BrowserNode`, the local mesh,
 * `meshStoreTransport` on a native host. */
export interface NetcodeTransport {
  nodeIdHex(): string | null;
  openStream(options: {
    reliability: 'reliable' | 'fireAndForget';
    peer?: string;
    label?: string;
    lossy?: boolean;
  }): NetcodeStream | Promise<NetcodeStream>;
  onEvent(
    handler: (event: {
      readonly type: string;
      readonly streamId?: string;
      readonly peerNode?: string | null;
      readonly payload?: Uint8Array;
    }) => void,
  ): () => void;
  connectPeer?(peerHex: string): Promise<unknown>;
}

/** A stream the netcode sends on. */
export interface NetcodeStream {
  send(payload: Uint8Array): unknown;
  close(): unknown;
}

/** One player input as it travels: `seq` counts from 1 per player. */
export interface WireInput<I> {
  readonly seq: number;
  /** The host time the player was RENDERING when it issued this input —
   * what lag compensation rewinds to. */
  readonly seen: number;
  readonly data: I;
}

export type Frame<E, I> =
  /** Player → host: join (repeated until the first snapshot). */
  | { readonly n: string; readonly k: 'h' }
  /** Player → host: the newest inputs, the unacknowledged ones repeated. */
  | { readonly n: string; readonly k: 'i'; readonly i: readonly WireInput<I>[] }
  /** Player → host: clock ping. */
  | { readonly n: string; readonly k: 'p'; readonly t0: number }
  /** Host → player: clock pong. */
  | { readonly n: string; readonly k: 'q'; readonly t0: number; readonly t1: number; readonly t2: number }
  /** Host → player: a snapshot, and the last input seq applied for them. */
  | {
      readonly n: string;
      readonly k: 's';
      readonly tick: number;
      readonly t: number;
      readonly ack: number;
      readonly e: Readonly<Record<string, E>>;
    };

const encoder = new TextEncoder();
const decoder = new TextDecoder('utf-8', { fatal: true });

export function encodeFrame<E, I>(frame: Frame<E, I>): Uint8Array {
  return encoder.encode(JSON.stringify(frame));
}

/** A frame for `label`, or `null` for anything else (another label, not a
 * netcode frame, malformed). */
export function decodeFrame<E, I>(payload: Uint8Array | undefined, label: string): Frame<E, I> | null {
  if (payload === undefined) return null;
  let value: unknown;
  try {
    value = JSON.parse(decoder.decode(payload));
  } catch {
    return null;
  }
  if (typeof value !== 'object' || value === null) return null;
  const frame = value as { n?: unknown; k?: unknown };
  if (frame.n !== label || typeof frame.k !== 'string') return null;
  switch (frame.k) {
    case 'h':
      return frame as Frame<E, I>;
    case 'i':
      return Array.isArray((frame as { i?: unknown }).i) ? (frame as Frame<E, I>) : null;
    case 'p':
      return typeof (frame as { t0?: unknown }).t0 === 'number' ? (frame as Frame<E, I>) : null;
    case 'q': {
      const q = frame as { t0?: unknown; t1?: unknown; t2?: unknown };
      return typeof q.t0 === 'number' && typeof q.t1 === 'number' && typeof q.t2 === 'number'
        ? (frame as Frame<E, I>)
        : null;
    }
    case 's': {
      const s = frame as { tick?: unknown; t?: unknown; ack?: unknown; e?: unknown };
      return typeof s.tick === 'number' && typeof s.t === 'number' && typeof s.ack === 'number' &&
        typeof s.e === 'object' && s.e !== null
        ? (frame as Frame<E, I>)
        : null;
    }
    default:
      return null;
  }
}

/** 16 lowercase hex, or `null`. */
export function peerHex(peer: string | null | undefined): string | null {
  if (typeof peer !== 'string') return null;
  const raw = peer.startsWith('0x') ? peer.slice(2) : peer;
  return /^[0-9a-fA-F]{1,16}$/.test(raw) ? raw.toLowerCase().padStart(16, '0') : null;
}

/** The time source: `performance.now()` unless a test injects one. */
export type Now = () => number;

export const defaultNow: Now = () => globalThis.performance?.now() ?? Date.now();
