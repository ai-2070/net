/**
 * Regression: a filtered subscription must advance past filtered windows.
 *
 * SDK audit 2026-10-11. When every event a poll fetched was filtered out,
 * the core still returns a `nextId` past that window (and `hasMore`).
 * `EventStream` only adopted `nextId` when events came back, so it re-read
 * the same window forever: `TypedChannel.subscribe()` (always filtered)
 * behind a run of non-matching events never delivered.
 */
import { describe, expect, it } from 'vitest';
import { EventStream } from '../src/stream';

type Resp = { events: { id: string; raw: string }[]; nextId: string | null; hasMore: boolean };

function bus(script: (cursor: string | undefined, call: number) => Resp) {
  const calls: (string | undefined)[] = [];
  return {
    calls,
    poll: async (req: { cursor?: string }) => {
      calls.push(req.cursor);
      if (calls.length > 20) throw new Error(`stream is stuck re-polling: ${calls}`);
      return script(req.cursor, calls.length);
    },
  };
}

const ev = (id: string) => ({ id, raw: id });
const opts = { filter: '{"x":1}', pollIntervalMs: 0, maxBackoffMs: 0 };

describe('EventStream cursor', () => {
  it('advances past windows whose events were all filtered out', async () => {
    const b = bus((cursor) => {
      const step = cursor === undefined ? 0 : Number(cursor.slice(1));
      return step < 3
        ? { events: [], nextId: `c${step + 1}`, hasMore: true }
        : { events: [ev('match')], nextId: 'c4', hasMore: false };
    });
    const stream = new EventStream(b as never, opts);
    for await (const e of stream) {
      expect(e.raw).toBe('match');
      break;
    }
    expect(b.calls).toEqual([undefined, 'c1', 'c2', 'c3']);
  });

  it('keeps its cursor when a stalled poll returns no nextId', async () => {
    const b = bus((_cursor, call) =>
      call === 1
        ? { events: [ev('a')], nextId: 'k1', hasMore: false }
        : call === 2
          ? { events: [], nextId: null, hasMore: false }
          : { events: [ev('b')], nextId: 'k2', hasMore: false },
    );
    const seen: string[] = [];
    for await (const e of new EventStream(b as never, opts)) {
      seen.push(e.raw);
      if (seen.length === 2) break;
    }
    expect(seen).toEqual(['a', 'b']);
    expect(b.calls).toEqual([undefined, 'k1', 'k1']);
  });
});

describe('EventStream resume', () => {
  it('exposes its cursor and resumes from SubscribeOpts.cursor', async () => {
    const b = bus((cursor) =>
      cursor === 'k1'
        ? { events: [ev('b')], nextId: 'k2', hasMore: false }
        : { events: [ev('a')], nextId: 'k1', hasMore: false },
    );
    const first = new EventStream(b as never, opts);
    for await (const e of first) {
      expect(e.raw).toBe('a');
      break;
    }
    expect(first.cursor).toBe('k1');
    const resumed = new EventStream(b as never, { ...opts, cursor: first.cursor });
    for await (const e of resumed) {
      expect(e.raw).toBe('b');
      break;
    }
  });
});
