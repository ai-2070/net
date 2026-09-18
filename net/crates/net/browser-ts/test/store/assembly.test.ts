/**
 * Chunking and assembly (brief §1.9, §1.12, §2) — slice B1.
 *
 * Driven through an **in-process transport double**: the owner chunks
 * a real projection, every piece is encoded and decoded by the real
 * codec, and the replica assembles from what comes out. No transport,
 * as the slice requires — but no shortcut around the codec either,
 * because the last two reviews found their defects in the shape of
 * results rather than in the rules, and a test that hand-built its
 * chunks would have skipped exactly that.
 *
 * Each refusal row names the observable a consumer sees: nothing
 * published, the assembly reclaimed, the bytes given back, the
 * previous view untouched. "It returned an error" is not one.
 */

import { describe, expect, it } from 'vitest';

import {
  Assembly,
  AssemblyTable,
  ASSEMBLY_DEADLINE_MS,
  type AssemblyChunk,
  type AssemblyManifest,
  type ChunkOutcome,
} from '../../src/store/assembly.js';
import {
  assertChunkingFits,
  base64,
  chunkBytesFor,
  chunkSnapshot,
  MIN_CHUNK_BYTES,
  SNAP_ENVELOPE_RESERVE,
} from '../../src/store/chunker.js';
import { StoreError } from '../../src/store/errors.js';
import { decodeMessage, encodeMessage, MAX_SNAPSHOT_CHUNKS } from '../../src/store/wire.js';

const H = 'a'.repeat(32);
const INC = 'f'.repeat(16);
/** Today's wire: `maxEventBytes()` from `accb8f2f9`. */
const MAX_EVENT_BYTES = 8104;

/** A validator with the shape a definition's `state()` has. */
function plainRecord(raw: unknown): Record<string, unknown> {
  if (typeof raw !== 'object' || raw === null || Array.isArray(raw)) {
    throw new Error('not a record');
  }
  return raw as Record<string, unknown>;
}

/**
 * The double: chunk, encode, decode, assemble.
 *
 * Returns what the replica ends up with, plus the frames that crossed,
 * so a test can assert on both sides.
 */
function carry(
  state: unknown,
  options: {
    chunkBytes?: number;
    g?: string;
    r?: string;
    now?: number;
    /** Mutate the decoded chunks before the replica sees them. */
    interfere?: (chunks: AssemblyChunk[]) => AssemblyChunk[];
    validate?: (raw: unknown) => unknown;
  } = {},
) {
  const chunkBytes = options.chunkBytes ?? chunkBytesFor(MAX_EVENT_BYTES);
  const g = options.g ?? '1';
  const r = options.r ?? '418';
  const now = options.now ?? 0;
  const chunked = chunkSnapshot(state, chunkBytes);

  // The manifest and every chunk go through the real encoder, so a
  // frame this test believes in is a frame the wire would accept.
  const manifestText = encodeMessage({
    k: 'man',
    h: H,
    inc: INC,
    g,
    r,
    n: chunked.n,
    bytes: String(chunked.bytes),
  });
  const frames = chunked.pieces.map((d, i) =>
    encodeMessage({ k: 'snap', h: H, g, r, i, n: chunked.n, d }),
  );
  for (const frame of [manifestText, ...frames]) {
    expect(frame.length).toBeLessThanOrEqual(MAX_EVENT_BYTES);
  }

  const manifestFrame = decodeMessage(manifestText, { maxBytes: MAX_EVENT_BYTES, as: 'replica' });
  if (!manifestFrame.ok || manifestFrame.message.k !== 'man') {
    throw new Error('the manifest did not decode');
  }
  const manifest = manifestFrame.message as AssemblyManifest & { n: number };

  let chunks = frames.map((frame) => {
    const decoded = decodeMessage(frame, { maxBytes: MAX_EVENT_BYTES, as: 'replica' });
    if (!decoded.ok || decoded.message.k !== 'snap') throw new Error('a chunk did not decode');
    return decoded.message as AssemblyChunk;
  });
  if (options.interfere) chunks = options.interfere([...chunks]);

  const table = new AssemblyTable();
  const opened = table.open(manifest, now);
  if (!(opened instanceof Assembly)) return { table, opened, outcomes: [], chunked, frames };

  const outcomes = chunks.map((chunk) => opened.accept(chunk, options.validate ?? plainRecord, now));
  return { table, opened, outcomes, chunked, frames, manifest };
}

/** The document the replica assembled, or null if it published nothing. */
function published(outcomes: readonly ChunkOutcome[]): unknown {
  for (const outcome of outcomes) {
    if (outcome.ok && outcome.done) return outcome.document;
  }
  return null;
}

function refusals(outcomes: readonly ChunkOutcome[]): string[] {
  return outcomes.filter((o) => !o.ok).map((o) => (o as { reason: string }).reason);
}

describe('the budget is derived from the transport, not assumed', () => {
  it('matches the brief’s arithmetic at today’s wire', () => {
    // floor((8104 − 192) / 4) * 3 = 5934
    expect(SNAP_ENVELOPE_RESERVE).toBe(192);
    expect(chunkBytesFor(MAX_EVENT_BYTES)).toBe(5934);
  });

  it('moves with the transport', () => {
    // Not a hardcoded 5934: a smaller cap yields a smaller chunk.
    // floor((4096 − 192) / 4) * 3 = 2928
    expect(chunkBytesFor(4096)).toBe(2928);
    expect(chunkBytesFor(MAX_EVENT_BYTES * 2)).toBeGreaterThan(chunkBytesFor(MAX_EVENT_BYTES));
  });

  it('refuses to start when the numbers do not fit, naming both', () => {
    expect(() => assertChunkingFits(MAX_EVENT_BYTES)).not.toThrow();

    // Below the chunk floor.
    const tooSmall = 1024;
    expect(chunkBytesFor(tooSmall)).toBeLessThan(MIN_CHUNK_BYTES);
    try {
      assertChunkingFits(tooSmall);
      throw new Error('expected a refusal');
    } catch (error) {
      expect(error).toBeInstanceOf(StoreError);
      expect((error as StoreError).code).toBe('capacity');
      // Both numbers, because "it would not start" is only actionable
      // if it says what did not fit.
      expect((error as StoreError).message).toContain(String(tooSmall));
      expect((error as StoreError).message).toContain(String(MIN_CHUNK_BYTES));
    }

    // Smaller than the frozen envelopes themselves.
    expect(() => assertChunkingFits(100)).toThrow(/197/);
    // And an unavailable cap is not a zero.
    expect(() => assertChunkingFits(Number.NaN)).toThrow(/no usable event size/);
    expect(() => assertChunkingFits(0)).toThrow(/no usable event size/);
  });

  it('keeps every frame inside the cap at the derived size', () => {
    // The reserve is a bound, so a full chunk plus the largest
    // plausible envelope still fits. `carry` asserts this per frame.
    const big = { pad: 'x'.repeat(20_000) };
    const { frames } = carry(big);
    expect(frames.length).toBeGreaterThan(3);
    for (const frame of frames) expect(frame.length).toBeLessThanOrEqual(MAX_EVENT_BYTES);
  });
});

describe('a snapshot survives the round trip', () => {
  it('carries a multi-chunk projection byte-for-byte', () => {
    const state = {
      ship: { heading: 31.5, sails: { main: true } },
      crew: Object.fromEntries(Array.from({ length: 400 }, (_, i) => [`m${i}`, `name-${i}`])),
      log: Array.from({ length: 500 }, (_, i) => i),
    };
    const { outcomes, chunked } = carry(state);

    expect(chunked.n).toBeGreaterThan(1);
    expect(published(outcomes)).toEqual(state);
    expect(refusals(outcomes)).toEqual([]);
  });

  it('carries multi-byte characters split across a chunk boundary', () => {
    // The reason `d` is base64: a code point can straddle any byte
    // boundary, and a raw-text chunk holding half of one is not valid
    // JSON string content.
    const glyph = '\u4e2d\u6587';
    const state = { text: glyph.repeat(4000) };
    const { outcomes, chunked } = carry(state, { chunkBytes: 101 });

    expect(chunked.n).toBeGreaterThan(50);
    expect(published(outcomes)).toEqual(state);
  });

  it('carries the smallest projections as one chunk', () => {
    // `n >= 1` because JSON is never the empty string: `{}` is two
    // bytes. `n = 0` would make "every `i` present" vacuously true, so
    // an assembly could complete having received nothing.
    for (const smallest of [{}, { a: null }]) {
      const { outcomes, chunked } = carry(smallest);
      expect(chunked.n).toBe(1);
      expect(chunked.bytes).toBeGreaterThan(0);
      expect(published(outcomes)).toEqual(smallest);
    }
  });

  it('declares the decoded byte total, not the encoded one', () => {
    const state = { pad: 'x'.repeat(10_000) };
    const { chunked } = carry(state);
    expect(chunked.bytes).toBe(new TextEncoder().encode(JSON.stringify(state)).length);
    // Base64 is longer; declaring that would fail the total check.
    expect(chunked.pieces.join('').length).toBeGreaterThan(chunked.bytes);
  });
});

describe('the chunker refuses rather than truncates', () => {
  it('refuses a projection over the byte bound', () => {
    const huge = { pad: 'x'.repeat(1024 * 1024 + 1) };
    try {
      chunkSnapshot(huge, chunkBytesFor(MAX_EVENT_BYTES));
      throw new Error('expected a refusal');
    } catch (error) {
      expect((error as StoreError).code).toBe('capacity');
      expect((error as StoreError).message).toMatch(/1048576/);
    }
  });

  it('refuses a projection needing more than 255 chunks', () => {
    const state = { pad: 'x'.repeat(4000) };
    expect(() => chunkSnapshot(state, 10)).toThrow(/255/);
    // Half a world is not a smaller world: nothing partial comes back.
    expect(() => chunkSnapshot(state, 10)).toThrow(StoreError);
  });

  it('refuses a state that is not JSON', () => {
    expect(() => chunkSnapshot(() => 1, 1024)).toThrow(/not JSON/);
  });
});

describe('assembly consistency', () => {
  const manifest = (over: Partial<AssemblyManifest> = {}): AssemblyManifest => ({
    h: H,
    g: '1',
    r: '418',
    n: 2,
    bytes: '6',
    ...over,
  });
  const chunk = (over: Partial<AssemblyChunk> = {}): AssemblyChunk => ({
    h: H,
    g: '1',
    r: '418',
    n: 2,
    i: 0,
    d: base64(new TextEncoder().encode('{"a":')),
    ...over,
  });

  it('refuses a manifest with no chunks', () => {
    const table = new AssemblyTable();
    const outcome = table.open(manifest({ n: 0 }), 0);
    expect((outcome as { reason?: string }).reason).toBe('manifest-chunk-count');
    expect(table.size).toBe(0);
  });

  it('refuses a manifest over the chunk bound', () => {
    const table = new AssemblyTable();
    expect((table.open(manifest({ n: MAX_SNAPSHOT_CHUNKS + 1 }), 0) as { reason?: string }).reason).toBe(
      'manifest-chunk-count',
    );
    expect(table.open(manifest({ n: MAX_SNAPSHOT_CHUNKS }), 0)).toBeInstanceOf(Assembly);
  });

  it('refuses a manifest declaring more bytes than the bound', () => {
    const table = new AssemblyTable();
    const outcome = table.open(manifest({ bytes: String(1024 * 1024 + 1) }), 0);
    expect((outcome as { reason?: string }).reason).toBe('manifest-bytes');
    expect((outcome as { code?: string }).code).toBe('capacity');
  });

  it('drops a chunk from another assembly without ending this one', () => {
    const table = new AssemblyTable();
    const open = table.open(manifest(), 0) as Assembly;

    for (const wrong of [
      chunk({ h: 'b'.repeat(32) }),
      chunk({ g: '2' }),
      chunk({ r: '419' }),
      chunk({ n: 3 }),
    ]) {
      const outcome = open.accept(wrong, plainRecord, 0);
      expect(outcome.ok).toBe(false);
      if (!outcome.ok) {
        expect(outcome.reason).toBe('chunk-wrong-assembly');
        // Not fatal: someone else's chunk must not kill this assembly.
        expect(outcome.fatal).toBe(false);
      }
    }
    expect(open.reclaimed).toBe(false);
    expect(open.have).toBe(0);
  });

  it('refuses an index outside 0..n-1', () => {
    const open = new AssemblyTable().open(manifest(), 0) as Assembly;
    for (const i of [-1, 2, 2.5]) {
      const outcome = open.accept(chunk({ i }), plainRecord, 0);
      expect((outcome as { reason?: string }).reason, String(i)).toBe('chunk-index');
    }
  });

  it('is idempotent for a byte-identical retransmission', () => {
    const open = new AssemblyTable().open(manifest(), 0) as Assembly;
    const first = open.accept(chunk(), plainRecord, 0);
    const again = open.accept(chunk(), plainRecord, 0);

    expect(first.ok && !first.done).toBe(true);
    expect(again.ok && !again.done).toBe(true);
    // One position, one chunk: a retransmission must not count twice
    // or the byte total would never match.
    expect(open.have).toBe(1);
    expect(open.bytesHeld).toBe(5);
  });

  it('refuses and restarts on a conflicting duplicate, publishing nothing', () => {
    const open = new AssemblyTable().open(manifest(), 0) as Assembly;
    open.accept(chunk(), plainRecord, 0);

    const outcome = open.accept(
      chunk({ d: base64(new TextEncoder().encode('{"b":')) }),
      plainRecord,
      0,
    );

    expect(outcome.ok).toBe(false);
    if (!outcome.ok) {
      expect(outcome.reason).toBe('chunk-conflict');
      expect(outcome.fatal).toBe(true);
    }
    // The assembly is gone and its bytes are given back: neither
    // payload is the snapshot, so publishing either publishes a
    // document no peer sent.
    expect(open.reclaimed).toBe(true);
    expect(open.have).toBe(0);
    expect(open.bytesHeld).toBe(0);
  });

  it('refuses a non-canonical chunk payload', () => {
    const open = new AssemblyTable().open(manifest(), 0) as Assembly;
    // Decodable but non-canonical: the spare bits are not zero.
    const outcome = open.accept(chunk({ d: 'QR==' }), plainRecord, 0);
    expect((outcome as { reason?: string }).reason).toBe('chunk-non-canonical');
  });

  it('refuses an overshoot at the chunk that overshoots, not at completion', () => {
    // Declared 6, three chunks of 5. The refusal must land on the
    // SECOND chunk: waiting for completion would hold more memory than
    // the manifest declared, which is the bound this check is for.
    const open = new AssemblyTable().open(manifest({ n: 3 }), 0) as Assembly;
    const first = open.accept(chunk({ n: 3 }), plainRecord, 0);
    expect(first.ok).toBe(true);
    expect(open.bytesHeld).toBe(5);

    const second = open.accept(chunk({ n: 3, i: 1 }), plainRecord, 0);

    expect(second.ok).toBe(false);
    if (!second.ok) expect(second.reason).toBe('byte-total-mismatch');
    expect(open.reclaimed).toBe(true);
    // Never held more than was declared.
    expect(open.bytesHeld).toBe(0);
  });

  it('refuses a short total on the completing chunk', () => {
    const open = new AssemblyTable().open(manifest({ bytes: '20' }), 0) as Assembly;
    open.accept(chunk(), plainRecord, 0);
    const outcome = open.accept(chunk({ i: 1 }), plainRecord, 0);

    expect((outcome as { reason?: string }).reason).toBe('byte-total-mismatch');
    expect(open.reclaimed).toBe(true);
  });
});

describe('the ASSEMBLED document is validated, not the envelopes', () => {
  it('refuses a document with duplicate keys, assembled from valid chunks', () => {
    // Every chunk envelope is canonical; the document they form is
    // not. Per-chunk checks validate envelopes, which is a different
    // claim from validating the snapshot.
    const text = '{"a":1,"a":2}';
    const { outcomes } = carryText(text);

    expect(published(outcomes)).toBeNull();
    expect(refusals(outcomes)).toContain('assembled-not-json');
  });

  it('refuses a document past the depth bound', () => {
    const deep = `${'['.repeat(20)}1${']'.repeat(20)}`;
    const { outcomes } = carryText(deep);
    expect(refusals(outcomes)).toContain('assembled-not-json');
  });

  it('refuses a document with a non-finite number', () => {
    const { outcomes } = carryText('{"n":1e999}');
    expect(refusals(outcomes)).toContain('assembled-not-json');
  });

  it('refuses invalid UTF-8 even where lossy decoding would be valid JSON', () => {
    // `{"a":"<0xff>"}` is not UTF-8. A lossy decoder turns the byte
    // into U+FFFD and yields a perfectly valid document, so the
    // assembly would PUBLISH a value the owner never sent. A fatal
    // decoder refuses instead.
    const bytes = new Uint8Array([
      ...new TextEncoder().encode('{"a":"'),
      0xff,
      ...new TextEncoder().encode('"}'),
    ]);
    expect(JSON.parse(new TextDecoder('utf-8').decode(bytes))).toEqual({ a: '\ufffd' });

    const open = new AssemblyTable().open(
      { h: H, g: '1', r: '418', n: 1, bytes: String(bytes.length) },
      0,
    ) as Assembly;
    const outcome = open.accept(
      { h: H, g: '1', r: '418', n: 1, i: 0, d: base64(bytes) },
      plainRecord,
      0,
    );

    expect((outcome as { reason?: string }).reason).toBe('assembled-not-json');
    expect(outcome.ok).toBe(false);
  });

  it('refuses a document the definition rejects', () => {
    const { outcomes } = carryText('[1,2,3]');
    expect(published(outcomes)).toBeNull();
    expect(refusals(outcomes)).toContain('assembled-invalid');
  });

  it('publishes a document that passes both (control)', () => {
    const { outcomes } = carryText('{"ship":{"heading":90}}');
    expect(published(outcomes)).toEqual({ ship: { heading: 90 } });
  });

  /** Carry an exact document text, split small so it really assembles. */
  function carryText(text: string) {
    const bytes = new TextEncoder().encode(text);
    const size = Math.max(1, Math.ceil(bytes.length / 3));
    const pieces: string[] = [];
    for (let offset = 0; offset < bytes.length || pieces.length === 0; offset += size) {
      pieces.push(base64(bytes.subarray(offset, offset + size)));
    }
    const table = new AssemblyTable();
    const open = table.open(
      { h: H, g: '1', r: '418', n: pieces.length, bytes: String(bytes.length) },
      0,
    ) as Assembly;
    const outcomes = pieces.map((d, i) =>
      open.accept({ h: H, g: '1', r: '418', n: pieces.length, i, d }, plainRecord, 0),
    );
    return { outcomes };
  }
});

describe('reclamation', () => {
  const manifest: AssemblyManifest = { h: H, g: '1', r: '418', n: 3, bytes: '9' };
  const piece = (i: number): AssemblyChunk => ({
    h: H,
    g: '1',
    r: '418',
    n: 3,
    i,
    d: base64(new TextEncoder().encode('abc')),
  });

  it('holds one assembly per (handle, generation)', () => {
    const table = new AssemblyTable();
    const first = table.open(manifest, 0) as Assembly;
    first.accept(piece(0), plainRecord, 0);
    expect(table.size).toBe(1);

    // A replacement manifest for the same pair reclaims the
    // predecessor rather than leaving it to a deadline.
    const second = table.open(manifest, 0) as Assembly;
    expect(first.reclaimed).toBe(true);
    expect(table.size).toBe(1);
    expect(table.bytesHeld).toBe(0);
    expect(second).not.toBe(first);

    // A different generation is a different assembly, not a conflict.
    table.open({ ...manifest, g: '2' }, 0);
    expect(table.size).toBe(2);
  });

  it('reclaims every assembly for a handle on a generation change', () => {
    const table = new AssemblyTable();
    // Each chunk must match its own assembly, or it is dropped as
    // another's and holds nothing.
    (table.open(manifest, 0) as Assembly).accept(piece(0), plainRecord, 0);
    (table.open({ ...manifest, g: '2' }, 0) as Assembly).accept(
      { ...piece(0), g: '2' },
      plainRecord,
      0,
    );
    const other = table.open({ ...manifest, h: 'b'.repeat(32) }, 0) as Assembly;
    other.accept({ ...piece(0), h: 'b'.repeat(32) }, plainRecord, 0);
    expect(table.bytesHeld).toBe(9);

    table.reclaimHandle(H);

    // This handle's memory is given back; another handle's is not.
    expect(table.size).toBe(1);
    expect(table.bytesHeld).toBe(3);
    expect(other.reclaimed).toBe(false);
  });

  it('reclaims on the deadline and publishes nothing', () => {
    const table = new AssemblyTable();
    const open = table.open(manifest, 0) as Assembly;
    open.accept(piece(0), plainRecord, 0);
    open.accept(piece(1), plainRecord, 0);

    expect(open.expired(ASSEMBLY_DEADLINE_MS - 1)).toBe(false);
    expect(table.sweep(ASSEMBLY_DEADLINE_MS - 1)).toBe(0);

    expect(table.sweep(ASSEMBLY_DEADLINE_MS)).toBe(1);
    expect(open.reclaimed).toBe(true);
    expect(table.bytesHeld).toBe(0);

    // A chunk arriving after reclamation completes nothing: the
    // partial is not resurrected.
    const late = open.accept(piece(2), plainRecord, ASSEMBLY_DEADLINE_MS + 1);
    expect(late.ok).toBe(false);
    expect(open.have).toBe(0);
  });

  it('admits nothing after an explicit reclamation, before any deadline', () => {
    const table = new AssemblyTable();
    const open = table.open(manifest, 0) as Assembly;
    open.accept(piece(0), plainRecord, 0);

    table.reclaim(H, '1');

    // Well inside the deadline: the refusal must come from being
    // reclaimed, not from expiring.
    const late = open.accept(piece(1), plainRecord, 1);
    expect(late.ok).toBe(false);
    if (!late.ok) expect(late.reason).toBe('chunk-wrong-assembly');
    expect(open.have).toBe(0);
    expect(open.bytesHeld).toBe(0);
  });

  it('never merges a partial into a later assembly', () => {
    const table = new AssemblyTable();
    const abandoned = table.open(manifest, 0) as Assembly;
    abandoned.accept(piece(0), plainRecord, 0);
    abandoned.accept(piece(1), plainRecord, 0);
    table.reclaim(H, '1');

    // A restart at the same revision is a different generation, so the
    // abandoned attempt's chunks cannot join it — and even its own
    // fresh assembly starts empty.
    const restarted = table.open({ ...manifest, g: '2' }, 0) as Assembly;
    expect(restarted.have).toBe(0);
    expect(restarted.accept(piece(2), plainRecord, 0).ok).toBe(false);
  });

  it('bounds total bytes across handles, refusing the newest', () => {
    // A count bound is not a memory bound: the ceiling is on bytes.
    const table = new AssemblyTable(1000);
    // Two chunks, so it stays open: a single-chunk assembly completes
    // and gives its bytes back before the ceiling is ever tested.
    const first = table.open({ h: H, g: '1', r: '1', n: 2, bytes: '900' }, 0);
    expect(first).toBeInstanceOf(Assembly);
    (first as Assembly).accept(
      { h: H, g: '1', r: '1', n: 2, i: 0, d: base64(new Uint8Array(300)) },
      () => undefined,
      0,
    );
    expect(table.bytesHeld).toBe(300);

    const second = table.open({ h: 'c'.repeat(32), g: '1', r: '1', n: 1, bytes: '800' }, 0);

    expect((second as { reason?: string }).reason).toBe('assembly-too-large');
    expect((second as { code?: string }).code).toBe('capacity');
    // The one already in flight has a replica waiting on it, so the
    // ARRIVAL is refused rather than the incumbent evicted.
    expect(table.size).toBe(1);
    expect(table.get(H, '1')).toBeDefined();
  });

  it('admits a new assembly once the bytes come back (control)', () => {
    const table = new AssemblyTable(1000);
    const first = table.open({ h: H, g: '1', r: '1', n: 2, bytes: '900' }, 0) as Assembly;
    first.accept({ h: H, g: '1', r: '1', n: 2, i: 0, d: base64(new Uint8Array(300)) }, () => undefined, 0);
    expect(table.open({ h: 'c'.repeat(32), g: '1', r: '1', n: 1, bytes: '800' }, 0)).not.toBeInstanceOf(
      Assembly,
    );

    table.reclaimHandle(H);

    expect(table.open({ h: 'c'.repeat(32), g: '1', r: '1', n: 1, bytes: '800' }, 0)).toBeInstanceOf(
      Assembly,
    );
  });

  it('exposes no partial document', () => {
    const table = new AssemblyTable();
    const open = table.open(manifest, 0) as Assembly;
    const first = open.accept(piece(0), plainRecord, 0);
    const second = open.accept(piece(1), plainRecord, 0);

    // The only shape an incomplete assembly returns is a count.
    for (const outcome of [first, second]) {
      expect(outcome.ok).toBe(true);
      if (outcome.ok) expect(outcome.done).toBe(false);
      expect(outcome).not.toHaveProperty('document');
    }
    expect(open.have).toBe(2);
  });
});
