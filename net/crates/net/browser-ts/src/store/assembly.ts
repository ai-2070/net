/**
 * Snapshot assembly, and what it refuses (brief §1.12, §2).
 *
 * The rules are short and every one of them is a refusal a partial
 * snapshot would otherwise slip through:
 *
 * - `n ≥ 1`; every `i` in `0..n-1` present **exactly once**;
 * - all chunks carry the same `(h, g, r, n)`;
 * - the sum of decoded chunk lengths equals `bytes` **exactly**;
 * - **the assembled document** — not each chunk envelope — is then
 *   JSON-parsed under the same depth and duplicate-key rules and
 *   validated by the definition's `state()` before anything is
 *   published.
 *
 * That last one is the distinction the earlier draft got wrong: the
 * per-chunk checks validate the *envelopes*, not the snapshot. A
 * document can be assembled from thirteen individually perfect
 * envelopes and still be a duplicate-keyed depth bomb.
 *
 * Reclamation is equally explicit, because an assembly is the one
 * place a replica holds a megabyte on a stranger's say-so: **one**
 * in-flight per `(handle, generation)`, discarded on generation
 * change, deadline and handle expiry, never merged into a later one,
 * and bounded in total across handles so a peer cannot open many
 * cheap assemblies to hold expensive memory.
 */

import { fromBase64 } from './chunker.js';
import type { StoreErrorCode } from './errors.js';
import { parseStoreJson, type JsonValue } from './json.js';
import {
  isCanonicalBase64,
  MAX_SNAPSHOT_BYTES,
  MAX_SNAPSHOT_CHUNKS,
  type Decimal,
  type Hex,
} from './wire.js';

/** How long an assembly may stay open (brief §2). */
export const ASSEMBLY_DEADLINE_MS = 10_000;

/** Total in-flight assembly bytes one owner or replica may hold (§2). */
export const MAX_ASSEMBLY_BYTES_TOTAL = 8 * 1024 * 1024;

/** The manifest an assembly is opened against. */
export interface AssemblyManifest {
  readonly h: Hex;
  readonly g: Decimal;
  readonly r: Decimal;
  readonly n: number;
  /** Declared decoded byte total. */
  readonly bytes: Decimal;
}

/** One chunk, as decoded from the wire. */
export interface AssemblyChunk {
  readonly h: Hex;
  readonly g: Decimal;
  readonly r: Decimal;
  readonly n: number;
  readonly i: number;
  readonly d: string;
}

/** Why an assembly refused something. Stable and countable. */
export type AssemblyRefusal =
  | 'manifest-chunk-count'
  | 'manifest-bytes'
  | 'assembly-too-large'
  | 'chunk-wrong-assembly'
  | 'chunk-index'
  | 'chunk-non-canonical'
  | 'chunk-conflict'
  | 'chunk-duplicate'
  | 'byte-total-mismatch'
  | 'assembled-not-json'
  | 'assembled-invalid'
  | 'deadline';

export interface AssemblyRejected {
  readonly ok: false;
  readonly code: StoreErrorCode;
  readonly reason: AssemblyRefusal;
  /**
   * Whether this ends the assembly. A conflicting duplicate restarts
   * it (§5.1), an out-of-assembly chunk is merely dropped.
   */
  readonly fatal: boolean;
}

export type ChunkOutcome =
  | { readonly ok: true; readonly done: false; readonly have: number }
  | { readonly ok: true; readonly done: true; readonly document: JsonValue }
  | AssemblyRejected;

function reject(reason: AssemblyRefusal, fatal: boolean, code: StoreErrorCode = 'invalid-data'): AssemblyRejected {
  return { ok: false, code, reason, fatal };
}

/**
 * One in-flight snapshot assembly.
 *
 * Holds the chunks it has admitted and nothing else: no partial
 * document is exposed, and there is no accessor that would let a
 * caller read half a world.
 */
export class Assembly {
  private readonly pieces: (Uint8Array | undefined)[];
  private readonly encoded: (string | undefined)[];
  private admitted = 0;
  private held = 0;
  private closed = false;

  constructor(
    readonly manifest: AssemblyManifest,
    /** When this assembly was opened, for the deadline. */
    private readonly openedAt: number,
  ) {
    this.pieces = new Array<Uint8Array | undefined>(manifest.n);
    this.encoded = new Array<string | undefined>(manifest.n);
  }

  /** Decoded bytes currently held, for the total-bytes bound. */
  get bytesHeld(): number {
    return this.held;
  }

  /** How many distinct `i` have been admitted. */
  get have(): number {
    return this.admitted;
  }

  /** Whether this assembly has been reclaimed. */
  get reclaimed(): boolean {
    return this.closed;
  }

  /** Give up every chunk. A reclaimed assembly admits nothing. */
  reclaim(): void {
    this.closed = true;
    this.pieces.fill(undefined);
    this.encoded.fill(undefined);
    this.admitted = 0;
    this.held = 0;
  }

  /** Whether `now` is past this assembly's deadline. */
  expired(now: number): boolean {
    return now - this.openedAt >= ASSEMBLY_DEADLINE_MS;
  }

  /**
   * Admit one chunk.
   *
   * Returns the assembled document on the chunk that completes it, and
   * **only** then: `done: false` carries a count, never a partial
   * document.
   */
  accept(chunk: AssemblyChunk, validate: (raw: unknown) => unknown, now: number): ChunkOutcome {
    if (this.closed) return reject('chunk-wrong-assembly', false);
    if (this.expired(now)) {
      this.reclaim();
      return reject('deadline', true, 'timeout');
    }
    // All chunks carry the same `(h, g, r, n)`. A chunk from a
    // restarted attempt at the same revision differs in `g`, which is
    // what stops it joining this one.
    const m = this.manifest;
    if (chunk.h !== m.h || chunk.g !== m.g || chunk.r !== m.r || chunk.n !== m.n) {
      return reject('chunk-wrong-assembly', false);
    }
    if (!Number.isInteger(chunk.i) || chunk.i < 0 || chunk.i >= m.n) {
      return reject('chunk-index', false);
    }
    if (!isCanonicalBase64(chunk.d)) return reject('chunk-non-canonical', false);

    const seen = this.encoded[chunk.i];
    if (seen !== undefined) {
      // Byte-identical retransmission is ordinary and idempotent.
      if (seen === chunk.d) return { ok: true, done: false, have: this.admitted };
      // Two different payloads for one position: the assembly cannot
      // decide which is the snapshot, so it is refused and restarted.
      // Publishing either would publish a document neither peer sent.
      this.reclaim();
      return reject('chunk-conflict', true);
    }

    const bytes = fromBase64(chunk.d);
    const declared = Number(m.bytes);
    if (this.held + bytes.length > declared) {
      // Overshooting the declared total is a mismatch discoverable
      // now, before the memory is held.
      this.reclaim();
      return reject('byte-total-mismatch', true);
    }
    this.encoded[chunk.i] = chunk.d;
    this.pieces[chunk.i] = bytes;
    this.admitted += 1;
    this.held += bytes.length;

    if (this.admitted < m.n) return { ok: true, done: false, have: this.admitted };

    // Complete. Every `i` is present exactly once by construction: a
    // repeat is either idempotent or a conflict, and the count only
    // advances for a fresh index.
    if (this.held !== declared) {
      this.reclaim();
      return reject('byte-total-mismatch', true);
    }

    const joined = new Uint8Array(this.held);
    let offset = 0;
    for (const piece of this.pieces) {
      if (piece === undefined) {
        // Unreachable while `admitted === n`, and asserted rather than
        // assumed: publishing a short document would be worse than
        // refusing one.
        this.reclaim();
        return reject('byte-total-mismatch', true);
      }
      joined.set(piece, offset);
      offset += piece.length;
    }

    const text = new TextDecoder('utf-8', { fatal: true });
    let document: JsonValue;
    try {
      // THE ASSEMBLED DOCUMENT, under the same rules a single frame
      // gets: depth, duplicate keys, finite numbers. The per-chunk
      // checks above validated envelopes, which is a different claim.
      const parsed = parseStoreJson(text.decode(joined));
      if (!parsed.ok) {
        this.reclaim();
        return reject('assembled-not-json', true);
      }
      document = parsed.value;
    } catch {
      this.reclaim();
      return reject('assembled-not-json', true);
    }

    try {
      validate(document);
    } catch {
      this.reclaim();
      return reject('assembled-invalid', true);
    }

    this.reclaim();
    return { ok: true, done: true, document };
  }
}

/**
 * The assemblies one side holds, with the bounds that keep them from
 * being a memory lever.
 *
 * One in-flight per `(handle, generation)` — a second manifest for the
 * same pair replaces it rather than accumulating — and a total byte
 * ceiling across handles, because a count bound alone is not a memory
 * bound (§2).
 */
export class AssemblyTable {
  private readonly live = new Map<string, Assembly>();

  constructor(private readonly maxTotalBytes: number = MAX_ASSEMBLY_BYTES_TOTAL) {}

  private static key(h: Hex, g: Decimal): string {
    return `${h}/${g}`;
  }

  /** Bytes currently held across every open assembly. */
  get bytesHeld(): number {
    let total = 0;
    for (const assembly of this.live.values()) total += assembly.bytesHeld;
    return total;
  }

  get size(): number {
    return this.live.size;
  }

  /** The open assembly for a pair, if any. */
  get(h: Hex, g: Decimal): Assembly | undefined {
    return this.live.get(AssemblyTable.key(h, g));
  }

  /**
   * Open an assembly for a manifest.
   *
   * Refuses the **newest** when the total would exceed the ceiling:
   * the ones already in flight have a replica waiting on them, and
   * dropping those to admit an arrival is how a peer evicts another's
   * progress.
   */
  open(manifest: AssemblyManifest, now: number): Assembly | AssemblyRejected {
    if (!Number.isInteger(manifest.n) || manifest.n < 1 || manifest.n > MAX_SNAPSHOT_CHUNKS) {
      return reject('manifest-chunk-count', true);
    }
    const declared = Number(manifest.bytes);
    if (!Number.isSafeInteger(declared) || declared < 0 || declared > MAX_SNAPSHOT_BYTES) {
      return reject('manifest-bytes', true, 'capacity');
    }
    const key = AssemblyTable.key(manifest.h, manifest.g);
    // One in-flight per pair. A replacement manifest for the same pair
    // reclaims the predecessor rather than leaving it to a deadline.
    this.live.get(key)?.reclaim();
    this.live.delete(key);
    if (this.bytesHeld + declared > this.maxTotalBytes) {
      return reject('assembly-too-large', true, 'capacity');
    }
    const assembly = new Assembly(manifest, now);
    this.live.set(key, assembly);
    return assembly;
  }

  /** Reclaim one pair's assembly. */
  reclaim(h: Hex, g: Decimal): void {
    const key = AssemblyTable.key(h, g);
    this.live.get(key)?.reclaim();
    this.live.delete(key);
  }

  /**
   * Reclaim every assembly for a handle — a generation change, a
   * handle expiry, a session loss.
   */
  reclaimHandle(h: Hex): void {
    for (const [key, assembly] of [...this.live]) {
      if (key.startsWith(`${h}/`)) {
        assembly.reclaim();
        this.live.delete(key);
      }
    }
  }

  /** Reclaim everything past its deadline. Returns how many went. */
  sweep(now: number): number {
    let swept = 0;
    for (const [key, assembly] of [...this.live]) {
      if (assembly.expired(now)) {
        assembly.reclaim();
        this.live.delete(key);
        swept += 1;
      }
    }
    return swept;
  }

  /** Give up everything. */
  clear(): void {
    for (const assembly of this.live.values()) assembly.reclaim();
    this.live.clear();
  }
}
