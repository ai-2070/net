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

  it('parseMeshDbErrorKind decodes the <<meshdb-kind:...>> prefix', async () => {
    // The Rust binding embeds the structured error kind in the
    // reason string as `<<meshdb-kind:KIND>>MSG`. Exercise the
    // parser directly to pin the contract (a runtime trigger
    // for substrate-side errors needs surfaces the SDK doesn't
    // expose yet — capability-index gating, federated transport,
    // configurable budgets — so we cover the plumbing here).
    const { parseMeshDbErrorKind } = await import('../meshdb');
    const synthetic = new Error('<<meshdb-kind:planner_error>>plan rejected');
    const decoded = parseMeshDbErrorKind(synthetic);
    expect(decoded).not.toBeNull();
    expect(decoded?.kind).toBe('planner_error');
    expect(decoded?.message).toBe('plan rejected');

    const plain = new Error('some unrelated error');
    expect(parseMeshDbErrorKind(plain)).toBeNull();
    expect(parseMeshDbErrorKind('not an error')).toBeNull();
    expect(parseMeshDbErrorKind(null)).toBeNull();
  });

  it('cachePolicyTimeBound rejects non-finite / negative ttlSeconds at the factory', () => {
    // Regression: pre-fix, the factory stashed any number
    // and the converter silently rewrote bad values to 5.0.
    // Matches Python / Go which validate at construction.
    expect(() => cachePolicyTimeBound(-1)).toThrow();
    expect(() => cachePolicyTimeBound(Number.NaN)).toThrow();
    expect(() => cachePolicyTimeBound(Number.POSITIVE_INFINITY)).toThrow();
  });

  it('execute rejects a hand-rolled cachePolicy with a negative ttlSeconds', async () => {
    const reader = seed([[0xefn, 1n, Buffer.from('x')]]);
    const runner = new MeshQueryRunner(reader, true);
    const q = MeshQuery.at(0xefn, 1n);
    await expect(
      runner.execute(q, {
        // Direct literal that bypasses the factory.
        cachePolicy: { kind: 'time_bound', ttlSeconds: -1 } as never,
      }),
    ).rejects.toThrow();
  });

  it('execute rejects a hand-rolled cachePolicy with an unknown kind', async () => {
    const reader = seed([[0xefn, 1n, Buffer.from('x')]]);
    const runner = new MeshQueryRunner(reader, true);
    const q = MeshQuery.at(0xefn, 1n);
    await expect(
      runner.execute(q, {
        cachePolicy: { kind: 'forever', ttlSeconds: undefined } as never,
      }),
    ).rejects.toThrow();
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

// ---------------------------------------------------------------------
// Slice 3: Predicate + Filter factory.
// ---------------------------------------------------------------------

d('MeshDB Filter + Predicate (slice 3)', () => {
  const {
    MeshQuery,
    MeshQueryRunner,
    InMemoryChainReader,
    predicateEquals,
    predicateExists,
    predicateNumericAtLeast,
    predicateNumericInRange,
    predicateStringPrefix,
    predicateStringMatches,
    predicateAnd,
    predicateOr,
    predicateNot,
  } = symbols as {
    MeshQuery: typeof import('../index').MeshQuery;
    MeshQueryRunner: typeof import('../index').MeshQueryRunner;
    InMemoryChainReader: typeof import('../index').InMemoryChainReader;
    predicateEquals: (field: string, value: string) => unknown;
    predicateExists: (field: string) => unknown;
    predicateNumericAtLeast: (field: string, threshold: number) => unknown;
    predicateNumericInRange: (field: string, min: number, max: number) => unknown;
    predicateStringPrefix: (field: string, prefix: string) => unknown;
    predicateStringMatches: (field: string, pattern: string) => unknown;
    predicateAnd: (children: unknown[]) => unknown;
    predicateOr: (children: unknown[]) => unknown;
    predicateNot: (child: unknown) => unknown;
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

  it('equals on synthetic seq keeps matching rows', async () => {
    const chain = 0xcafen;
    const reader = seed([1, 2, 3].map((s) => [chain, BigInt(s), Buffer.from(`p-${s}`)]));
    const runner = new MeshQueryRunner(reader);
    const q = MeshQuery.filter(
      MeshQuery.between(chain, 1n, 10n),
      predicateEquals('seq', '2') as never,
    );
    const rows = await (await runner.execute(q)).toArray();
    expect(rows).toHaveLength(1);
    expect(rows[0].seq).toBe(2n);
    expect(Buffer.from(rows[0].payload).toString()).toBe('p-2');
  });

  it('numeric_at_least on seq keeps upper rows', async () => {
    const chain = 0xcafen;
    const reader = seed([1, 2, 3, 4, 5].map((s) => [chain, BigInt(s), Buffer.from('')]));
    const runner = new MeshQueryRunner(reader);
    const q = MeshQuery.filter(
      MeshQuery.between(chain, 1n, 10n),
      predicateNumericAtLeast('seq', 3.0) as never,
    );
    const seqs = (await (await runner.execute(q)).toArray()).map((r) => Number(r.seq));
    expect(seqs).toEqual([3, 4, 5]);
  });

  it('equals on a JSON payload field', async () => {
    const chain = 0xc0den;
    const reader = seed([
      [chain, 1n, Buffer.from('{"severity":"low"}')],
      [chain, 2n, Buffer.from('{"severity":"high"}')],
      [chain, 3n, Buffer.from('{"severity":"high"}')],
      [chain, 4n, Buffer.from('not-json')],
    ]);
    const runner = new MeshQueryRunner(reader);
    const q = MeshQuery.filter(
      MeshQuery.between(chain, 1n, 10n),
      predicateEquals('severity', 'high') as never,
    );
    const seqs = (await (await runner.execute(q)).toArray()).map((r) => Number(r.seq));
    expect(seqs).toEqual([2, 3]);
  });

  it('and / or / not composition', async () => {
    const chain = 0xc0den;
    const reader = seed([
      [chain, 1n, Buffer.from('{"severity":"high","region":"us"}')],
      [chain, 2n, Buffer.from('{"severity":"high","region":"eu"}')],
      [chain, 3n, Buffer.from('{"severity":"low","region":"us"}')],
      [chain, 4n, Buffer.from('{"severity":"high","region":"us"}')],
    ]);
    const runner = new MeshQueryRunner(reader);
    const andQ = MeshQuery.filter(
      MeshQuery.between(chain, 1n, 10n),
      predicateAnd([
        predicateEquals('severity', 'high'),
        predicateEquals('region', 'us'),
      ]) as never,
    );
    const andSeqs = (await (await runner.execute(andQ)).toArray()).map((r) =>
      Number(r.seq),
    );
    expect(andSeqs).toEqual([1, 4]);

    const orQ = MeshQuery.filter(
      MeshQuery.between(chain, 1n, 10n),
      predicateOr([
        predicateEquals('region', 'eu'),
        predicateEquals('severity', 'low'),
      ]) as never,
    );
    const orSeqs = (await (await runner.execute(orQ)).toArray()).map((r) =>
      Number(r.seq),
    );
    expect(orSeqs.sort()).toEqual([2, 3]);

    const notQ = MeshQuery.filter(
      MeshQuery.between(chain, 1n, 10n),
      predicateNot(predicateEquals('severity', 'high')) as never,
    );
    const notSeqs = (await (await runner.execute(notQ)).toArray()).map((r) =>
      Number(r.seq),
    );
    expect(notSeqs).toEqual([3]);
  });

  it('numeric_in_range filters by inclusive bounds', async () => {
    const chain = 0xc0den;
    const reader = seed(
      [1, 2, 3, 4, 5].map((s) => [
        chain,
        BigInt(s),
        Buffer.from(`{"latency_ms":${s * 10}}`),
      ]),
    );
    const runner = new MeshQueryRunner(reader);
    const q = MeshQuery.filter(
      MeshQuery.between(chain, 1n, 10n),
      predicateNumericInRange('latency_ms', 20.0, 40.0) as never,
    );
    const seqs = (await (await runner.execute(q)).toArray()).map((r) => Number(r.seq));
    expect(seqs).toEqual([2, 3, 4]);
  });

  it('numeric_in_range rejects inverted bounds at factory', () => {
    expect(() => predicateNumericInRange('x', 10.0, 5.0)).toThrow();
  });

  it('string_prefix + string_matches', async () => {
    const chain = 0xc0den;
    const reader = seed([
      [chain, 1n, Buffer.from('{"user":"alice","path":"/api/users"}')],
      [chain, 2n, Buffer.from('{"user":"bob","path":"/api/jobs"}')],
      [chain, 3n, Buffer.from('{"user":"alfred","path":"/healthz"}')],
    ]);
    const runner = new MeshQueryRunner(reader);
    const prefixQ = MeshQuery.filter(
      MeshQuery.between(chain, 1n, 10n),
      predicateStringPrefix('user', 'al') as never,
    );
    const prefixSeqs = (await (await runner.execute(prefixQ)).toArray()).map((r) =>
      Number(r.seq),
    );
    expect(prefixSeqs).toEqual([1, 3]);

    const matchesQ = MeshQuery.filter(
      MeshQuery.between(chain, 1n, 10n),
      predicateStringMatches('path', '/api/') as never,
    );
    const matchesSeqs = (await (await runner.execute(matchesQ)).toArray()).map((r) =>
      Number(r.seq),
    );
    expect(matchesSeqs).toEqual([1, 2]);
  });

  it('predicateExists rejects rows without the field', async () => {
    const chain = 0xc0den;
    const reader = seed([
      [chain, 1n, Buffer.from('{"severity":"high"}')],
      [chain, 2n, Buffer.from('{"other":"x"}')],
      [chain, 3n, Buffer.from('{"severity":"low"}')],
    ]);
    const runner = new MeshQueryRunner(reader);
    const q = MeshQuery.filter(
      MeshQuery.between(chain, 1n, 10n),
      predicateExists('severity') as never,
    );
    const seqs = (await (await runner.execute(q)).toArray()).map((r) => Number(r.seq));
    expect(seqs).toEqual([1, 3]);
  });

  it('filter pipelined with aggregate count', async () => {
    const chain = 0xc0den;
    const reader = seed([
      [chain, 1n, Buffer.from('{"severity":"high"}')],
      [chain, 2n, Buffer.from('{"severity":"high"}')],
      [chain, 3n, Buffer.from('{"severity":"low"}')],
      [chain, 4n, Buffer.from('{"severity":"high"}')],
    ]);
    const runner = new MeshQueryRunner(reader);
    const highs = MeshQuery.filter(
      MeshQuery.between(chain, 1n, 10n),
      predicateEquals('severity', 'high') as never,
    );
    const rows = await (await runner.execute(MeshQuery.count(highs, null))).toArray();
    expect(decodeAggregateFn!(rows[0])).toMatchObject({ kind: 'count', count: 3n });
  });
});

// ---------------------------------------------------------------------
// Slice 4: fluent QueryBuilder.
// ---------------------------------------------------------------------

d('MeshDB QueryBuilder (slice 4)', () => {
  const {
    MeshQuery,
    MeshQueryRunner,
    InMemoryChainReader,
    predicateEquals,
  } = symbols as {
    MeshQuery: typeof import('../index').MeshQuery;
    MeshQueryRunner: typeof import('../index').MeshQueryRunner;
    InMemoryChainReader: typeof import('../index').InMemoryChainReader;
    predicateEquals: (field: string, value: string) => unknown;
  };

  const decodeAggregate = decodeAggregateFn as (row: unknown) => {
    kind: string;
    count: bigint | null;
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

  it('builder() returns a chainable builder; empty .build() rejects', () => {
    const b = MeshQuery.builder();
    expect(b).toBeDefined();
    expect(() => b.build()).toThrow(/no source/);
  });

  it('count on empty builder rejects', () => {
    expect(() => MeshQuery.builder().count(null)).toThrow(/no source/);
  });

  it('.at().build() round-trips through the runner', async () => {
    const reader = seed([[0xabn, 7n, Buffer.from('v')]]);
    const runner = new MeshQueryRunner(reader);
    const q = MeshQuery.builder().at(0xabn, 7n).build();
    const rows = await (await runner.execute(q)).toArray();
    expect(rows).toHaveLength(1);
    expect(rows[0].seq).toBe(7n);
  });

  it('.between().filter().count().build() end-to-end', async () => {
    const chain = 0xc0den;
    const reader = seed([
      [chain, 1n, Buffer.from('{"severity":"high"}')],
      [chain, 2n, Buffer.from('{"severity":"low"}')],
      [chain, 3n, Buffer.from('{"severity":"high"}')],
      [chain, 4n, Buffer.from('{"severity":"high"}')],
      [chain, 5n, Buffer.from('{"severity":"low"}')],
    ]);
    const runner = new MeshQueryRunner(reader);
    const q = MeshQuery.builder()
      .between(chain, 1n, 10n)
      .filter(predicateEquals('severity', 'high') as never)
      .count(null)
      .build();
    const rows = await (await runner.execute(q)).toArray();
    expect(decodeAggregate(rows[0])).toMatchObject({ kind: 'count', count: 3n });
  });

  it('.between().window(size).build() emits buckets', async () => {
    const chain = 0xaan;
    const reader = seed(
      [1, 2, 3, 4, 5, 6, 7].map((s) => [chain, BigInt(s), Buffer.from('')]),
    );
    const runner = new MeshQueryRunner(reader);
    const q = MeshQuery.builder().between(chain, 1n, 20n).window(3n).build();
    const rows = await (await runner.execute(q)).toArray();
    expect(rows).toHaveLength(3);
  });

  it('builder methods are per-step immutable (aliased builders diverge)', () => {
    const base = MeshQuery.builder().between(0xaan, 1n, 10n);
    const a = base.count(null);
    const b = base.filter(predicateEquals('seq', '1') as never);
    // base should still build as a between query.
    expect(() => base.build()).not.toThrow();
    expect(() => a.build()).not.toThrow();
    expect(() => b.build()).not.toThrow();
  });

  it('builder.join with prebuilt right side', async () => {
    const a = 0x111n;
    const b = 0x222n;
    const reader = seed([
      [a, 1n, Buffer.from('a-1')],
      [a, 2n, Buffer.from('a-2')],
      [b, 1n, Buffer.from('b-1')],
      [b, 2n, Buffer.from('b-2')],
    ]);
    const runner = new MeshQueryRunner(reader);
    const rightSide = MeshQuery.builder().between(b, 1n, 10n).build();
    const q = MeshQuery.builder()
      .between(a, 1n, 10n)
      .join(rightSide, 'inner', 'seq', null, null)
      .build();
    const rows = await (await runner.execute(q)).toArray();
    expect(rows).toHaveLength(2);
  });

  it('source methods reset prior pipeline state', async () => {
    const chain = 0xaan;
    const reader = seed([
      [chain, 1n, Buffer.from('v')],
      [chain, 2n, Buffer.from('v')],
    ]);
    const runner = new MeshQueryRunner(reader);
    // .at(99) sets a nonexistent source; switching to .between
    // resets and the count picks up cleanly.
    const q = MeshQuery.builder()
      .at(chain, 99n)
      .between(chain, 1n, 5n)
      .count(null)
      .build();
    const rows = await (await runner.execute(q)).toArray();
    expect(decodeAggregate(rows[0])).toMatchObject({ kind: 'count', count: 2n });
  });
});

// ---------------------------------------------------------------------
// Lineage emit: pre-walked entries form. The SDK doesn't itself walk
// the fork-of: graph; callers hand in entries in walk order.
// ---------------------------------------------------------------------

d('MeshDB lineage_emit', () => {
  const { MeshQuery, MeshQueryRunner, InMemoryChainReader } = symbols as {
    MeshQuery: typeof import('../index').MeshQuery;
    MeshQueryRunner: typeof import('../index').MeshQueryRunner;
    InMemoryChainReader: typeof import('../index').InMemoryChainReader;
  };

  it('emits one row per entry in walk order', async () => {
    const runner = new MeshQueryRunner(new InMemoryChainReader());
    const q = (
      MeshQuery as unknown as {
        lineageEmit: (
          origin: bigint,
          entries: Array<{ originHash: bigint; depth: number; tipSeq?: bigint | null }>,
          direction: string,
        ) => InstanceType<typeof MeshQuery>;
      }
    ).lineageEmit(
      0xaan,
      [
        { originHash: 0xaan, depth: 0, tipSeq: 5n },
        { originHash: 0xbbn, depth: 1, tipSeq: 3n },
        { originHash: 0xccn, depth: 2 },
      ],
      'back',
    );
    const stream = await runner.execute(q);
    const rows = await stream.toArray();
    expect(rows.map((r: { originHash: bigint; seq: bigint }) => [r.originHash, r.seq])).toEqual([
      [0xaan, 5n],
      [0xbbn, 3n],
      [0xccn, 0n],
    ]);
    expect(rows.every((r: { payload: Uint8Array }) => r.payload.length === 0)).toBe(true);
  });

  it('accepts forward direction', async () => {
    const runner = new MeshQueryRunner(new InMemoryChainReader());
    const q = (
      MeshQuery as unknown as {
        lineageEmit: (
          origin: bigint,
          entries: Array<{ originHash: bigint; depth: number; tipSeq?: bigint | null }>,
          direction: string,
        ) => InstanceType<typeof MeshQuery>;
      }
    ).lineageEmit(0xaan, [{ originHash: 0xaan, depth: 0, tipSeq: 1n }], 'forward');
    const stream = await runner.execute(q);
    const rows = await stream.toArray();
    expect(rows.map((r: { originHash: bigint; seq: bigint }) => [r.originHash, r.seq])).toEqual([
      [0xaan, 1n],
    ]);
  });

  it('rejects an unknown direction', () => {
    expect(() =>
      (
        MeshQuery as unknown as {
          lineageEmit: (
            origin: bigint,
            entries: Array<{ originHash: bigint; depth: number }>,
            direction: string,
          ) => unknown;
        }
      ).lineageEmit(0xaan, [{ originHash: 0xaan, depth: 0 }], 'sideways'),
    ).toThrow();
  });

  it('empty entries yield an empty stream', async () => {
    const runner = new MeshQueryRunner(new InMemoryChainReader());
    const q = (
      MeshQuery as unknown as {
        lineageEmit: (
          origin: bigint,
          entries: Array<{ originHash: bigint; depth: number }>,
          direction: string,
        ) => InstanceType<typeof MeshQuery>;
      }
    ).lineageEmit(0xaan, [], 'back');
    const stream = await runner.execute(q);
    const rows = await stream.toArray();
    expect(rows).toEqual([]);
  });
});

// ---------------------------------------------------------------------
// AsyncIterable shim: `for await (const row of stream) { ... }`.
// ---------------------------------------------------------------------

d('MeshDB AsyncIterable shim', () => {
  const {
    MeshQuery,
    MeshQueryRunner,
    InMemoryChainReader,
  } = symbols as {
    MeshQuery: typeof import('../index').MeshQuery;
    MeshQueryRunner: typeof import('../index').MeshQueryRunner;
    InMemoryChainReader: typeof import('../index').InMemoryChainReader;
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

  it('for await iterates rows after shim import', async () => {
    // Import the shim. Idempotent — re-imports are no-ops.
    // eslint-disable-next-line @typescript-eslint/no-require-imports
    require('../meshdb');
    const reader = seed(
      [1, 2, 3, 4].map((s) => [0xabn, BigInt(s), Buffer.from(`p-${s}`)]),
    );
    const runner = new MeshQueryRunner(reader);
    const stream = await runner.execute(MeshQuery.between(0xabn, 1n, 10n));
    const seen: number[] = [];
    // The shim attaches Symbol.asyncIterator on the prototype;
    // `for await` picks it up.
    for await (const row of stream as unknown as AsyncIterable<{
      seq: bigint;
    }>) {
      seen.push(Number(row.seq));
    }
    expect(seen).toEqual([1, 2, 3, 4]);
  });

  it('shim is idempotent: re-import does not break anything', async () => {
    // eslint-disable-next-line @typescript-eslint/no-require-imports
    require('../meshdb');
    // eslint-disable-next-line @typescript-eslint/no-require-imports
    require('../meshdb');
    const reader = seed([[0xabn, 1n, Buffer.from('v')]]);
    const runner = new MeshQueryRunner(reader);
    const stream = await runner.execute(MeshQuery.latest(0xabn));
    const seen: bigint[] = [];
    for await (const row of stream as unknown as AsyncIterable<{
      seq: bigint;
    }>) {
      seen.push(row.seq);
    }
    expect(seen).toEqual([1n]);
  });

  it('break inside for-await releases the backing row buffer', async () => {
    // Regression: pre-fix, the AsyncIterable shim defined only
    // `next()`. Breaking out of the loop left the backing Vec
    // pinned on the AsyncMutex until JS GC fired. The new
    // `return()` hook drains it eagerly via `release()`.
    // eslint-disable-next-line @typescript-eslint/no-require-imports
    require('../meshdb');
    const reader = seed(
      [1, 2, 3, 4, 5, 6].map((s) => [0xabn, BigInt(s), Buffer.from(`p-${s}`)]),
    );
    const runner = new MeshQueryRunner(reader);
    const stream = await runner.execute(MeshQuery.between(0xabn, 1n, 10n));
    const seen: number[] = [];
    for await (const row of stream as unknown as AsyncIterable<{
      seq: bigint;
    }>) {
      seen.push(Number(row.seq));
      if (seen.length === 2) {
        break;
      }
    }
    expect(seen).toEqual([1, 2]);
    // After release, toArray must report an empty drain.
    const drained = await (
      stream as unknown as { toArray(): Promise<unknown[]> }
    ).toArray();
    expect(drained).toEqual([]);
  });

  it('exception inside for-await releases the backing row buffer', async () => {
    // The shim's `throw()` hook calls release before rethrowing.
    // eslint-disable-next-line @typescript-eslint/no-require-imports
    require('../meshdb');
    const reader = seed(
      [1, 2, 3, 4, 5].map((s) => [0xabn, BigInt(s), Buffer.from(`p-${s}`)]),
    );
    const runner = new MeshQueryRunner(reader);
    const stream = await runner.execute(MeshQuery.between(0xabn, 1n, 10n));
    const seen: number[] = [];
    const sentinel = new Error('stop after 2');
    await expect(
      (async () => {
        for await (const row of stream as unknown as AsyncIterable<{
          seq: bigint;
        }>) {
          seen.push(Number(row.seq));
          if (seen.length === 2) {
            throw sentinel;
          }
        }
      })(),
    ).rejects.toBe(sentinel);
    expect(seen).toEqual([1, 2]);
    const drained = await (
      stream as unknown as { toArray(): Promise<unknown[]> }
    ).toArray();
    expect(drained).toEqual([]);
  });
});
