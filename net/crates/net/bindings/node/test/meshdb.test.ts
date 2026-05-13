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
