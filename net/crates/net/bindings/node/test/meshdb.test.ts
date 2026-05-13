// Smoke tests for the MeshDB Node binding (slice 1).
//
// Exercises the factory AST (`MeshQuery.at` / `between` / `latest`),
// the in-memory ChainReader, the async runner, the cache options,
// and the pull-based stream interface (`next()` + `toArray()`).
//
// Requires the binding to have been built with the `meshdb` Cargo
// feature: `pnpm --filter @ai2070/net build --features meshdb` (or
// the equivalent in your build setup). The describe.skipIf below
// noops the whole suite if MeshDB symbols are absent.

import { describe, expect, it } from 'vitest';

let symbols: Record<string, unknown> = {};
try {
  // eslint-disable-next-line @typescript-eslint/no-require-imports
  symbols = require('../index');
} catch {
  symbols = {};
}

const hasMeshDb =
  typeof symbols.MeshQuery === 'function' &&
  typeof symbols.MeshQueryRunner === 'function' &&
  typeof symbols.InMemoryChainReader === 'function';

const d = hasMeshDb ? describe : describe.skip;

const {
  decodeAggregate: decodeAggregateFn,
  decodeJoined: decodeJoinedFn,
  decodeWindow: decodeWindowFn,
} = symbols as {
  decodeAggregate?: (row: unknown) => unknown;
  decodeJoined?: (row: unknown) => unknown;
  decodeWindow?: (row: unknown) => unknown;
};

d('MeshDB factory + runner (slice 1)', () => {
  const {
    MeshQuery,
    MeshQueryRunner,
    InMemoryChainReader,
    cachePolicyPermanent,
    cachePolicyTimeBound,
  } = symbols as {
    MeshQuery: typeof import('../index').MeshQuery;
    MeshQueryRunner: typeof import('../index').MeshQueryRunner;
    InMemoryChainReader: typeof import('../index').InMemoryChainReader;
    cachePolicyPermanent: () => unknown;
    cachePolicyTimeBound: (seconds?: number) => unknown;
  };

  const seed = (
    rows: ReadonlyArray<readonly [bigint, bigint, Uint8Array]>,
  ): InstanceType<typeof InMemoryChainReader> => {
    const r = new InMemoryChainReader();
    for (const [origin, seq, payload] of rows) {
      r.append(origin, seq, Buffer.from(payload));
    }
    return r;
  };

  it('latest returns the tip row', async () => {
    const reader = seed([
      [0xabn, 1n, Buffer.from('v1')],
      [0xabn, 2n, Buffer.from('v2')],
      [0xabn, 3n, Buffer.from('v3')],
    ]);
    const runner = new MeshQueryRunner(reader);
    const stream = await runner.execute(MeshQuery.latest(0xabn));
    const rows = await stream.toArray();
    expect(rows).toHaveLength(1);
    expect(rows[0].seq).toBe(3n);
    expect(Buffer.from(rows[0].payload).toString()).toBe('v3');
    expect(rows[0].originHash).toBe(0xabn);
  });

  it('latest on an empty chain yields an empty stream', async () => {
    const runner = new MeshQueryRunner(new InMemoryChainReader());
    const stream = await runner.execute(MeshQuery.latest(0xdeadn));
    expect(await stream.toArray()).toEqual([]);
  });

  it('at returns a single row at the given seq', async () => {
    const reader = seed([[0x01n, 7n, Buffer.from('seven')]]);
    const runner = new MeshQueryRunner(reader);
    const stream = await runner.execute(MeshQuery.at(0x01n, 7n));
    const rows = await stream.toArray();
    expect(rows).toHaveLength(1);
    expect(rows[0].seq).toBe(7n);
    expect(Buffer.from(rows[0].payload).toString()).toBe('seven');
  });

  it('at returns an empty stream when the seq is missing', async () => {
    const reader = seed([[0x01n, 1n, Buffer.from('v')]]);
    const runner = new MeshQueryRunner(reader);
    const stream = await runner.execute(MeshQuery.at(0x01n, 99n));
    expect(await stream.toArray()).toEqual([]);
  });

  it('between yields rows in seq-asc order with end exclusive', async () => {
    const reader = seed(
      [1, 2, 3, 4, 5].map((s) => [0xcdn, BigInt(s), Buffer.from(`p-${s}`)]),
    );
    const runner = new MeshQueryRunner(reader);
    const stream = await runner.execute(MeshQuery.between(0xcdn, 2n, 5n));
    const rows = await stream.toArray();
    expect(rows.map((r) => Number(r.seq))).toEqual([2, 3, 4]);
  });

  it('between rejects an inverted range at factory time', () => {
    expect(() => MeshQuery.between(0xcdn, 5n, 5n)).toThrow(/must be </);
  });

  it('stream.next() pulls rows one at a time then returns null', async () => {
    const reader = seed([
      [0xefn, 1n, Buffer.from('a')],
      [0xefn, 2n, Buffer.from('b')],
    ]);
    const runner = new MeshQueryRunner(reader);
    const stream = await runner.execute(MeshQuery.between(0xefn, 1n, 5n));
    const first = await stream.next();
    expect(first).not.toBeNull();
    expect(first!.seq).toBe(1n);
    const second = await stream.next();
    expect(second!.seq).toBe(2n);
    expect(await stream.next()).toBeNull();
    // Idempotent post-EOF.
    expect(await stream.next()).toBeNull();
  });

  it('cachePolicyPermanent / cachePolicyTimeBound build well-formed policies', () => {
    const perm = cachePolicyPermanent() as {
      kind: string;
      ttlSeconds?: number;
    };
    expect(perm.kind).toBe('permanent');
    expect(perm.ttlSeconds).toBeUndefined();
    const tb = cachePolicyTimeBound(2.5) as {
      kind: string;
      ttlSeconds?: number;
    };
    expect(tb.kind).toBe('time_bound');
    expect(tb.ttlSeconds).toBe(2.5);
  });

  it('cache-enabled runner serves cached rows on the second call', async () => {
    const reader = seed([
      [0xeen, 1n, Buffer.from('x')],
      [0xeen, 2n, Buffer.from('y')],
    ]);
    const runner = new MeshQueryRunner(reader, true);
    const q = MeshQuery.between(0xeen, 1n, 10n);
    const first = await (await runner.execute(q)).toArray();
    const second = await (await runner.execute(q)).toArray();
    expect(first.length).toBe(second.length);
    expect(first.map((r) => Number(r.seq))).toEqual(
      second.map((r) => Number(r.seq)),
    );
  });

  it('bypassCache returns authoritative results without writeback', async () => {
    const reader = seed([[0xeen, 1n, Buffer.from('x')]]);
    const runner = new MeshQueryRunner(reader, true);
    const q = MeshQuery.latest(0xeen);
    const stream = await runner.execute(q, { bypassCache: true });
    const rows = await stream.toArray();
    expect(rows).toHaveLength(1);
    expect(Buffer.from(rows[0].payload).toString()).toBe('x');
  });

  it('execute with a permanent cache policy succeeds end-to-end', async () => {
    const reader = seed([[0xefn, 1n, Buffer.from('x')]]);
    const runner = new MeshQueryRunner(reader, true);
    const q = MeshQuery.at(0xefn, 1n);
    const stream = await runner.execute(q, {
      cachePolicy: cachePolicyPermanent() as never,
    });
    const rows = await stream.toArray();
    expect(rows).toHaveLength(1);
    expect(rows[0].seq).toBe(1n);
  });

  it('reader.latestSeq returns the tip or null', () => {
    const reader = seed([
      [0xfen, 10n, Buffer.from('v')],
      [0xfen, 20n, Buffer.from('v')],
      [0xfen, 15n, Buffer.from('v')],
    ]);
    expect(reader.latestSeq(0xfen)).toBe(20n);
    expect(reader.latestSeq(0xaan)).toBeNull();
  });
});

// ---------------------------------------------------------------------
// Slice 2: composite operator factories + decoders.
// ---------------------------------------------------------------------

d('MeshDB composite operators + decoders (slice 2)', () => {
  const {
    MeshQuery,
    MeshQueryRunner,
    InMemoryChainReader,
  } = symbols as {
    MeshQuery: typeof import('../index').MeshQuery;
    MeshQueryRunner: typeof import('../index').MeshQueryRunner;
    InMemoryChainReader: typeof import('../index').InMemoryChainReader;
  };

  const decodeAggregate = decodeAggregateFn as (row: unknown) => {
    group: { kind: string; originHash?: bigint; seq?: bigint } | null;
    kind: string;
    value: number | null;
    count: bigint | null;
  } | null;
  const decodeJoined = decodeJoinedFn as (row: unknown) => {
    left: { originHash: bigint; seq: bigint; payload: Uint8Array } | null;
    right: { originHash: bigint; seq: bigint; payload: Uint8Array } | null;
  } | null;
  const decodeWindow = decodeWindowFn as (row: unknown) => {
    start: bigint;
    end: bigint;
    rows: { originHash: bigint; seq: bigint; payload: Uint8Array }[];
  } | null;

  const seed = (
    rows: ReadonlyArray<readonly [bigint, bigint, Uint8Array]>,
  ): InstanceType<typeof InMemoryChainReader> => {
    const r = new InMemoryChainReader();
    for (const [origin, seq, payload] of rows) {
      r.append(origin, seq, Buffer.from(payload));
    }
    return r;
  };

  it('count with no groupBy returns a single aggregate row', async () => {
    const chain = 0xabcdn;
    const reader = seed([1, 2, 3, 4, 5].map((s) => [chain, BigInt(s), Buffer.from('')]));
    const runner = new MeshQueryRunner(reader);
    const q = MeshQuery.count(MeshQuery.between(chain, 1n, 10n), null);
    const rows = await (await runner.execute(q)).toArray();
    expect(rows).toHaveLength(1);
    const agg = decodeAggregate(rows[0]);
    expect(agg).not.toBeNull();
    expect(agg!.group).toBeNull();
    expect(agg!.kind).toBe('count');
    expect(agg!.count).toBe(5n);
  });

  it('count with groupBy origin returns per-origin counts', async () => {
    const chain = 0xbbn;
    const reader = seed([1, 2, 3].map((s) => [chain, BigInt(s), Buffer.from('')]));
    const runner = new MeshQueryRunner(reader);
    const q = MeshQuery.count(MeshQuery.between(chain, 1n, 10n), ['origin']);
    const rows = await (await runner.execute(q)).toArray();
    expect(rows).toHaveLength(1);
    const agg = decodeAggregate(rows[0])!;
    expect(agg.kind).toBe('count');
    expect(agg.group!.kind).toBe('origin');
    expect(agg.group!.originHash).toBe(chain);
    expect(agg.count).toBe(3n);
  });

  it('sum / avg / min / max over seq', async () => {
    const chain = 0xabn;
    const reader = seed(
      [1, 3, 7, 11].map((s) => [chain, BigInt(s), Buffer.from('')]),
    );
    const runner = new MeshQueryRunner(reader);
    const base = MeshQuery.between(chain, 1n, 20n);
    const rows = async (q: import('../index').MeshQuery) =>
      (await (await runner.execute(q)).toArray()).map((r) => decodeAggregate(r)!);
    expect((await rows(MeshQuery.sum(base, 'seq', null)))[0].value).toBe(22.0);
    expect((await rows(MeshQuery.avg(base, 'seq', null)))[0].value).toBeCloseTo(5.5);
    expect((await rows(MeshQuery.min(base, 'seq', null)))[0].value).toBe(1.0);
    expect((await rows(MeshQuery.max(base, 'seq', null)))[0].value).toBe(11.0);
  });

  it('percentile picks nearest-rank value', async () => {
    const chain = 0xabn;
    const reader = seed(
      [1, 2, 3, 4, 5, 6, 7, 8, 9, 10].map((s) => [chain, BigInt(s), Buffer.from('')]),
    );
    const runner = new MeshQueryRunner(reader);
    const q = MeshQuery.percentile(
      MeshQuery.between(chain, 1n, 20n),
      'seq',
      0.9,
      null,
    );
    const rows = await (await runner.execute(q)).toArray();
    expect(decodeAggregate(rows[0])!.value).toBe(9.0);
  });

  it('percentile rejects out-of-range p', () => {
    const base = MeshQuery.latest(0xaan);
    expect(() => MeshQuery.percentile(base, 'seq', 1.5, null)).toThrow();
    expect(() => MeshQuery.percentile(base, 'seq', -0.1, null)).toThrow();
  });

  it('distinctCount over a JSON field', async () => {
    const chain = 0xcdn;
    const reader = seed([
      [chain, 1n, Buffer.from('{"user":"alice"}')],
      [chain, 2n, Buffer.from('{"user":"bob"}')],
      [chain, 3n, Buffer.from('{"user":"alice"}')],
      [chain, 4n, Buffer.from('{"user":"carol"}')],
    ]);
    const runner = new MeshQueryRunner(reader);
    const q = MeshQuery.distinctCount(
      MeshQuery.between(chain, 1n, 10n),
      'user',
      null,
    );
    const rows = await (await runner.execute(q)).toArray();
    const agg = decodeAggregate(rows[0])!;
    expect(agg.kind).toBe('distinct_count');
    expect(agg.count).toBe(3n);
  });

  it('window tumbling buckets rows in seq-asc order', async () => {
    const chain = 0xaan;
    const reader = seed(
      [1, 2, 3, 4, 5, 6, 7].map((s) => [chain, BigInt(s), Buffer.from(`p-${s}`)]),
    );
    const runner = new MeshQueryRunner(reader);
    const q = MeshQuery.window(MeshQuery.between(chain, 1n, 20n), 3n);
    const rows = await (await runner.execute(q)).toArray();
    expect(rows).toHaveLength(3);
    const bs = rows.map((r) => decodeWindow(r)!);
    expect(bs[0].start).toBe(0n);
    expect(bs[0].end).toBe(3n);
    expect(bs[0].rows.map((r) => Number(r.seq))).toEqual([1, 2]);
    expect(bs[1].rows.map((r) => Number(r.seq))).toEqual([3, 4, 5]);
    expect(bs[2].rows.map((r) => Number(r.seq))).toEqual([6, 7]);
  });

  it('window size zero is rejected', () => {
    expect(() => MeshQuery.window(MeshQuery.latest(0xaan), 0n)).toThrow(/size must/);
  });

  it('inner join on seq matches pairs', async () => {
    const a = 0x111n;
    const b = 0x222n;
    const reader = seed([
      [a, 1n, Buffer.from('a-1')],
      [a, 2n, Buffer.from('a-2')],
      [a, 3n, Buffer.from('a-3')],
      [b, 2n, Buffer.from('b-2')],
      [b, 4n, Buffer.from('b-4')],
    ]);
    const runner = new MeshQueryRunner(reader);
    const q = MeshQuery.join(
      MeshQuery.between(a, 1n, 10n),
      MeshQuery.between(b, 1n, 10n),
      'inner',
      'seq',
      null,
      null,
    );
    const rows = await (await runner.execute(q)).toArray();
    const pairs = rows.map((r) => decodeJoined(r)!);
    expect(pairs).toHaveLength(1);
    expect(Buffer.from(pairs[0].left!.payload).toString()).toBe('a-2');
    expect(Buffer.from(pairs[0].right!.payload).toString()).toBe('b-2');
  });

  it('left_outer join emits unmatched lefts', async () => {
    const a = 0x111n;
    const b = 0x222n;
    const reader = seed([
      [a, 1n, Buffer.from('a-1')],
      [a, 2n, Buffer.from('a-2')],
      [a, 3n, Buffer.from('a-3')],
      [b, 2n, Buffer.from('b-2')],
    ]);
    const runner = new MeshQueryRunner(reader);
    const q = MeshQuery.join(
      MeshQuery.between(a, 1n, 10n),
      MeshQuery.between(b, 1n, 10n),
      'left_outer',
      'seq',
      null,
      null,
    );
    const rows = await (await runner.execute(q)).toArray();
    const pairs = rows.map((r) => decodeJoined(r)!);
    expect(pairs).toHaveLength(3);
    expect(pairs.filter((p) => p.right !== null)).toHaveLength(1);
    expect(pairs.filter((p) => p.right === null)).toHaveLength(2);
  });

  it('payload-keyed inner join on JSON field', async () => {
    const a = 0x111n;
    const b = 0x222n;
    const reader = seed([
      [a, 1n, Buffer.from('{"request_id":"r-1"}')],
      [a, 2n, Buffer.from('{"request_id":"r-2"}')],
      [b, 1n, Buffer.from('{"request_id":"r-1"}')],
      [b, 2n, Buffer.from('{"request_id":"r-9"}')],
    ]);
    const runner = new MeshQueryRunner(reader);
    const q = MeshQuery.join(
      MeshQuery.between(a, 1n, 10n),
      MeshQuery.between(b, 1n, 10n),
      'inner',
      'request_id',
      null,
      null,
    );
    const rows = await (await runner.execute(q)).toArray();
    const pairs = rows.map((r) => decodeJoined(r)!);
    expect(pairs).toHaveLength(1);
    expect(Buffer.from(pairs[0].left!.payload).toString()).toBe(
      '{"request_id":"r-1"}',
    );
    expect(Buffer.from(pairs[0].right!.payload).toString()).toBe(
      '{"request_id":"r-1"}',
    );
  });

  it('sort_merge join returns same pairs as hash_broadcast', async () => {
    const a = 0x111n;
    const b = 0x222n;
    const reader = seed([
      [a, 1n, Buffer.from('a-1')],
      [a, 2n, Buffer.from('a-2')],
      [a, 5n, Buffer.from('a-5')],
      [b, 2n, Buffer.from('b-2')],
      [b, 5n, Buffer.from('b-5')],
    ]);
    const runner = new MeshQueryRunner(reader);
    const baseLeft = MeshQuery.between(a, 1n, 10n);
    const baseRight = MeshQuery.between(b, 1n, 10n);
    const hash = MeshQuery.join(
      baseLeft,
      baseRight,
      'inner',
      'seq',
      'hash_broadcast',
      null,
    );
    const sm = MeshQuery.join(
      baseLeft,
      baseRight,
      'inner',
      'seq',
      'sort_merge',
      null,
    );
    const hashSeqs = (await (await runner.execute(hash)).toArray())
      .map((r) => Number(decodeJoined(r)!.left!.seq))
      .sort();
    const smSeqs = (await (await runner.execute(sm)).toArray())
      .map((r) => Number(decodeJoined(r)!.left!.seq))
      .sort();
    expect(hashSeqs).toEqual(smSeqs);
    expect(hashSeqs).toEqual([2, 5]);
  });

  it('unknown join kind / strategy / groupBy field reject at factory', () => {
    const base = MeshQuery.latest(0xaan);
    expect(() => MeshQuery.join(base, base, 'cross', 'seq', null, null)).toThrow();
    expect(() =>
      MeshQuery.join(base, base, 'inner', 'seq', 'nested_loop', null),
    ).toThrow();
    expect(() => MeshQuery.count(base, ['payload.severity'])).toThrow();
  });

  it('decode helpers return null on shape mismatch', async () => {
    const reader = seed([[0x01n, 1n, Buffer.from('raw-bytes')]]);
    const runner = new MeshQueryRunner(reader);
    const rows = await (await runner.execute(MeshQuery.at(0x01n, 1n))).toArray();
    expect(decodeAggregate(rows[0])).toBeNull();
    expect(decodeJoined(rows[0])).toBeNull();
    expect(decodeWindow(rows[0])).toBeNull();
  });
});
