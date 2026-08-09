// Nanosecond timestamps must survive as exact integers.
//
// Node cast the core `u64` to `i64` and then exposed a JavaScript
// `number`. Unix-epoch nanoseconds crossed `Number.MAX_SAFE_INTEGER`
// (2^53 - 1) around 104 days past 1970, so *every* realistic value on
// these fields was already losing its low-order digits:
// `9007199254740993` came back as `9007199254740992`.
//
// These drive the SDK's projections with a stub bus, so they run
// without the native module. The projections must pass the bigint
// through untouched — a single `Number(...)` anywhere in the chain
// reintroduces the defect.

import { describe, expect, it } from 'vitest';

import type { Net as NapiNet } from '@net-mesh/core';
import { NetNode } from '../src/node';
import { EventStream } from '../src/stream';

// The exact values that distinguish a bigint path from a number one.
const CASES: Array<[string, bigint]> = [
  ['2^53 - 1 (last exactly representable)', 9_007_199_254_740_991n],
  ['2^53', 9_007_199_254_740_992n],
  ['2^53 + 1 (the classic collapse)', 9_007_199_254_740_993n],
  ['u64::MAX', 18_446_744_073_709_551_615n],
  ['a current epoch nanosecond value', 1_786_000_000_000_000_000n],
  ['zero', 0n],
];

function nodeWithStub(stub: Partial<NapiNet>): NetNode {
  const node = Object.create(NetNode.prototype) as NetNode;
  (node as unknown as { bus: NapiNet }).bus = stub as NapiNet;
  return node;
}

describe.each(CASES)('%s', (_label, ns) => {
  it('survives the ingestion receipt projection', () => {
    const node = nodeWithStub({
      ingestRawSync: () => ({ shardId: 1, timestamp: ns }) as never,
    });
    const receipt = node.emit({ a: 1 });

    expect(receipt.timestamp).toBe(ns);
    expect(typeof receipt.timestamp).toBe('bigint');
  });

  it('survives the one-shot poll projection', async () => {
    const node = nodeWithStub({
      poll: async () =>
        ({
          events: [
            {
              id: 'e1',
              raw: '{}',
              rawBytes: Buffer.from('{}'),
              insertionTs: ns,
              shardId: 0,
            },
          ],
          nextId: null,
          hasMore: false,
        }) as never,
    });

    const response = await node.poll({ limit: 1 });
    expect(response.events[0].insertionTs).toBe(ns);
    expect(typeof response.events[0].insertionTs).toBe('bigint');
  });

  it('survives the streaming projection', async () => {
    let served = false;
    const bus = {
      async poll() {
        if (served) return { events: [], nextId: null, hasMore: false };
        served = true;
        return {
          events: [
            {
              id: 'e1',
              raw: '{}',
              rawBytes: Buffer.from('{}'),
              insertionTs: ns,
              shardId: 0,
            },
          ],
          nextId: null,
          hasMore: false,
        };
      },
    } as unknown as NapiNet;

    for await (const event of new EventStream(bus, { limit: 1 })) {
      expect(event.insertionTs).toBe(ns);
      expect(typeof event.insertionTs).toBe('bigint');
      break;
    }
  });
});

describe('the precision the old surface lost', () => {
  it('2^53 and 2^53 + 1 stay distinct', () => {
    const a = 9_007_199_254_740_992n;
    const b = 9_007_199_254_740_993n;

    // As bigints these differ.
    expect(a).not.toBe(b);
    // Through a JS number they do not — this is what the field used to do.
    expect(Number(a)).toBe(Number(b));
  });

  it('u64::MAX does not become negative', () => {
    // The old path also went through `i64`, so the top of the u64
    // range wrapped to a negative number before reaching JS.
    const max = 18_446_744_073_709_551_615n;
    expect(max > 0n).toBe(true);
    expect(BigInt.asIntN(64, max)).toBe(-1n);
  });
});

describe('JSON serialization contract', () => {
  it('JSON.stringify throws on a bigint, by design', () => {
    // Documented rather than papered over: the SDK deliberately does
    // not carry a second lossy `number` field for this.
    expect(() => JSON.stringify({ insertionTs: 1n })).toThrow(TypeError);
  });

  it('the documented millisecond conversion is explicit', () => {
    const insertionTs = 1_786_000_000_123_456_789n;
    const timestampMs = Number(insertionTs / 1_000_000n);

    expect(timestampMs).toBe(1_786_000_000_123);
    expect(Number.isSafeInteger(timestampMs)).toBe(true);
  });
});
