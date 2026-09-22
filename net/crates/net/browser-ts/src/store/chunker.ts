/**
 * Snapshot chunking (brief §1.9).
 *
 * The owner serializes the **projected** state to UTF-8 JSON, splits
 * the **bytes** into `n` pieces, and sends `i = 0..n-1`. Splitting
 * bytes rather than characters is the whole reason `d` is base64: a
 * UTF-8 code point can straddle any byte boundary, and a raw-text
 * chunk containing half of one is not valid JSON string content. The
 * +33 % base64 cost is accounted for in the budget rather than
 * discovered by an oversized frame.
 *
 * **The budget is derived at runtime, never hardcoded.** It comes from
 * the wire's own `maxEventBytes()` minus the `snap` envelope's
 * reserve, so a transport whose cap changes moves the chunk size with
 * it. A store that guessed this number would be wrong on the first
 * transport that changed, and would be wrong *silently* — by emitting
 * a frame the far side refuses.
 */

import { StoreError } from './errors.js';
import type { JsonValue } from './json.js';
import { inadmissibleValue, MAX_SNAPSHOT_BYTES, MAX_SNAPSHOT_CHUNKS, utf8Length } from './wire.js';

/**
 * Room left for everything in a `snap` that is not `d`.
 *
 * 192, against a **measured worst case of 134 B** with every field at
 * its maximum spelling: 20-digit `u64` decimals, a 32-hex handle, and
 * `i`/`n` at 255. So it is a bound, not a sample of one message.
 */
export const SNAP_ENVELOPE_RESERVE = 192;

/**
 * The floor a store refuses to start below (brief §1.9).
 *
 * Not an aesthetic minimum: at smaller chunk sizes a snapshot of any
 * useful size exceeds {@link MAX_SNAPSHOT_CHUNKS}, so the store would
 * start and then fail on its first projection.
 */
export const MIN_CHUNK_BYTES = 1024;

/** The largest `man`/`delta` envelope, for the startup check (§1.9). */
export const LARGEST_FROZEN_ENVELOPE = 197;

/**
 * Raw bytes per chunk, for a transport whose event cap is
 * `maxEventBytes`.
 *
 * `/4 * 3` because base64 emits 4 characters per 3 bytes, and the
 * frame carries the encoded form: the division is on the *encoded*
 * budget and the multiplication converts it back to raw input.
 */
export function chunkBytesFor(maxEventBytes: number): number {
  return Math.floor((maxEventBytes - SNAP_ENVELOPE_RESERVE) / 4) * 3;
}

/**
 * Refuse to start rather than discover the numbers later (§1.9).
 *
 * Throws a typed `capacity` error **naming both numbers**, because
 * "the store would not start" is only actionable if it says what did
 * not fit.
 */
export function assertChunkingFits(maxEventBytes: number): number {
  if (!Number.isFinite(maxEventBytes) || !Number.isInteger(maxEventBytes) || maxEventBytes <= 0) {
    throw new StoreError(
      'capacity',
      `the transport reported no usable event size (${String(maxEventBytes)})`,
    );
  }
  if (LARGEST_FROZEN_ENVELOPE > maxEventBytes) {
    throw new StoreError(
      'capacity',
      `the frozen envelopes need ${LARGEST_FROZEN_ENVELOPE} B and the transport allows ${maxEventBytes} B`,
    );
  }
  const chunkBytes = chunkBytesFor(maxEventBytes);
  if (chunkBytes < MIN_CHUNK_BYTES) {
    throw new StoreError(
      'capacity',
      `a ${maxEventBytes} B event leaves ${chunkBytes} B per chunk, below the ${MIN_CHUNK_BYTES} B floor`,
    );
  }
  // The product, not just the floor: chunks that meet MIN_CHUNK_BYTES
  // can still be unable to carry MAX_SNAPSHOT_BYTES within
  // MAX_SNAPSHOT_CHUNKS, and the store would start and then discover
  // that on its first large projection — the exact opposite of the
  // promise above.
  if (chunkBytes * MAX_SNAPSHOT_CHUNKS < MAX_SNAPSHOT_BYTES) {
    throw new StoreError(
      'capacity',
      `a ${maxEventBytes} B event leaves ${chunkBytes} B per chunk: ${MAX_SNAPSHOT_CHUNKS} of them cover ` +
        `${chunkBytes * MAX_SNAPSHOT_CHUNKS} B, below the ${MAX_SNAPSHOT_BYTES} B snapshot bound`,
    );
  }
  return chunkBytes;
}

/** One snapshot, split and ready to emit. */
export interface Chunked {
  /** The revision-independent byte total the manifest declares. */
  readonly bytes: number;
  /** How many pieces; `n` on the wire. */
  readonly n: number;
  /** Canonical base64 pieces, in `i` order. */
  readonly pieces: readonly string[];
}

/**
 * Split a projected state for emission.
 *
 * Refuses rather than truncates: a snapshot that does not fit the
 * declared bounds is a `capacity` error at the owner, before anything
 * is sent, because half a world is not a smaller world.
 */
export function chunkSnapshot(state: unknown, chunkBytes: number): Chunked {
  if (chunkBytes < 1) {
    throw new StoreError('capacity', `a chunk budget of ${chunkBytes} B cannot carry anything`);
  }
  const json = JSON.stringify(state);
  if (json === undefined) {
    // `JSON.stringify` returns undefined for a function or a symbol.
    // Serializing that as the string "undefined" would ship a snapshot
    // the replica cannot parse.
    throw new StoreError('invalid-data', 'the projected state is not JSON');
  }
  // The admissible-value scan, over the SNAPSHOT document like every
  // other value-bearing payload. `JSON.stringify` is lossy and silent:
  // `NaN`/`Infinity` become `null` and an `undefined`-valued own key
  // disappears — the manifest would ship a document the owner never
  // held, and the delta-overflow fallback is exactly this path.
  const bad = inadmissibleValue(state as JsonValue, 'state');
  if (bad !== null) {
    throw new StoreError('invalid-data', `the projected state is not admissible JSON: ${bad}`);
  }
  const bytes = new TextEncoder().encode(json);
  if (bytes.length > MAX_SNAPSHOT_BYTES) {
    throw new StoreError(
      'capacity',
      `the projection is ${bytes.length} B and the bound is ${MAX_SNAPSHOT_BYTES} B`,
    );
  }

  const pieces: string[] = [];
  // `n >= 1` holds because `JSON.stringify` never returns the empty
  // string — the smallest document is `{}` or `null`, two bytes. That
  // matters: `n = 0` would make "every `i` in `0..n-1` present exactly
  // once" vacuously true, so an assembly could complete having
  // received nothing. It is a consequence here rather than a guard,
  // and a guard that cannot fire is not a defence.
  for (let offset = 0; offset < bytes.length; offset += chunkBytes) {
    pieces.push(base64(bytes.subarray(offset, offset + chunkBytes)));
  }
  if (pieces.length > MAX_SNAPSHOT_CHUNKS) {
    throw new StoreError(
      'capacity',
      `the projection needs ${pieces.length} chunks and the bound is ${MAX_SNAPSHOT_CHUNKS}`,
    );
  }
  return { bytes: bytes.length, n: pieces.length, pieces };
}

/**
 * Chars per bulk conversion step.
 *
 * `btoa`/`atob` traffic in binary strings, and building one a byte at
 * a time is a call per byte over up to a megabyte — against "never
 * avoidably allocate, copy, or compute". Conversion runs in bounded
 * pieces; 0x8000 stays under every engine's argument-count bound.
 */
const CONVERSION_CHARS = 0x8000;

/** Canonical base64: standard alphabet, padded, no line breaks. */
export function base64(bytes: Uint8Array): string {
  let binary = '';
  for (let offset = 0; offset < bytes.length; offset += CONVERSION_CHARS) {
    binary += String.fromCharCode(...bytes.subarray(offset, offset + CONVERSION_CHARS));
  }
  return btoa(binary);
}

/**
 * The inverse of {@link base64}.
 *
 * Canonicality is the CALLER's gate: `isCanonicalBase64` runs in the
 * wire decoder and again in `Assembly.accept`. This only decodes —
 * `atob` accepts non-canonical spellings, so a direct caller MUST run
 * that check first.
 */
export function fromBase64(text: string): Uint8Array {
  const binary = atob(text);
  // Straight into the destination: there is no bulk binary-string →
  // bytes primitive to chunk here, and no intermediate is built.
  const out = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i += 1) out[i] = binary.charCodeAt(i);
  return out;
}

/** The UTF-8 byte length of a JSON document, for a budget check. */
export function jsonByteLength(value: unknown): number {
  const json = JSON.stringify(value);
  if (json === undefined) throw new StoreError('invalid-data', 'not JSON');
  return utf8Length(json);
}
