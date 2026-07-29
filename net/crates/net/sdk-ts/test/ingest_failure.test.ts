// The ingestion failure contract, pinned.
//
// `bindings/typescript.md` in the net-event-bus skill tells an integrator that
// the TypeScript binding has three distinct failure conventions on the same
// operation, and that mixing them up is the standard bug here:
//
//   emit / emitRaw                          -> throws
//   emitBatch / emitRawBatch                -> short count; throws on shutdown
//   publish / publishBatch / fire /
//   fireBatch / emitBuffer                  -> false or short count, never throws
//
// It also says the `Receipt | null` on `emit` is *vestigial* — that you should
// wrap in try/catch rather than null-check — which is a claim about a return
// type contradicting the runtime behaviour, and exactly the kind of thing a
// reader cannot verify by looking at the signature.
//
// All of that was true when written and provable only by reading
// `sdk-ts/src/node.ts` against `bindings/node/src/lib.rs`: `emit` and `emitRaw`
// call `ingestRawSync`, which is `Result<IngestResult>` on the Rust side and so
// throws; `fire` calls `ingestFire`, which is a bare `bool`. Nothing asserted
// it, so any of it could have flipped silently.
//
// These tests use shutdown as the failure trigger rather than backpressure:
// under `drop_oldest` / `drop_newest` a full buffer is *not* an error (the
// binding evicts and returns Ok), and under `fail_producer` the buffer state is
// timing-dependent. Shutdown is the one deterministic ingestion failure.

import { describe, expect, it } from 'vitest';

import { NetNode } from '../src/node';

async function shutNode() {
  const node = await NetNode.create({ shards: 1, bufferCapacity: 1024 });
  await node.shutdown();
  return node;
}

describe('ingestion failure conventions', () => {
  it('emit throws rather than returning null', async () => {
    const node = await shutNode();
    // If this ever returns null instead of throwing, the skill's advice to
    // try/catch is wrong and the `| null` has become load-bearing.
    expect(() => node.emit({ msg: 'x' })).toThrow();
  });

  it('emitRaw throws rather than returning null', async () => {
    const node = await shutNode();
    expect(() => node.emitRaw('{"msg":"x"}')).toThrow();
  });

  it('emit returns a Receipt, never null, on the success path', async () => {
    const node = await NetNode.create({ shards: 1, bufferCapacity: 1024 });
    try {
      const receipt = node.emit({ msg: 'x' });
      // The vestigial-null claim: there is no code path returning null.
      expect(receipt).not.toBeNull();
      expect(typeof receipt?.shardId).toBe('number');
    } finally {
      await node.shutdown();
    }
  });

  it('emitBatch throws on shutdown', async () => {
    const node = await shutNode();
    expect(() => node.emitBatch([{ msg: 'x' }])).toThrow();
  });

  it('fire returns false instead of throwing', async () => {
    const node = await shutNode();
    // The whole point of the fire path: it reports failure in-band.
    expect(node.fire('{"msg":"x"}')).toBe(false);
  });

  it('fireBatch returns a short count instead of throwing', async () => {
    const node = await shutNode();
    expect(node.fireBatch(['{"msg":"x"}', '{"msg":"y"}'])).toBe(0);
  });
});

describe('stats counters', () => {
  it('are bigint, not number', async () => {
    const node = await NetNode.create({ shards: 1, bufferCapacity: 1024 });
    try {
      node.emit({ msg: 'x' });
      const stats = node.stats();
      // `examples/observe.ts` compares against 0n on the strength of this.
      // A silent widening back to `number` would make that comparison a
      // TypeError at runtime while still type-checking.
      expect(typeof stats.eventsIngested).toBe('bigint');
      expect(typeof stats.eventsDropped).toBe('bigint');
    } finally {
      await node.shutdown();
    }
  });
});
