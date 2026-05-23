// End-to-end smoke for the Phase 6c capability-aggregation surface.
//
// Builds a single MeshNode, primes its capability fold via
// `_testInjectSyntheticPeerWithTags`, then exercises both
// `capabilityAggregate` and `capabilityCapacityRanking` through the
// napi boundary. Asserts the same bucketed output the Rust E2E
// suite at `tests/capability_aggregation_e2e.rs` pins.
//
// Requires the `test-helpers` napi feature for the synthetic-peer
// staging path (CI's `napi build` flags already include it).

import { afterEach, describe, expect, it } from 'vitest';

import type {
  Aggregation,
  CapacityQuery,
  GroupBy,
  TagMatcher,
} from '../src/capability-aggregation';
import { MeshNode } from '../src/mesh';

const PSK = '42'.repeat(32);

const nodes: MeshNode[] = [];
afterEach(async () => {
  while (nodes.length > 0) {
    const n = nodes.pop()!;
    try {
      await n.shutdown();
    } catch {
      // Ignore — node may already be closed.
    }
  }
});

async function primedNode(): Promise<MeshNode> {
  const node = await MeshNode.create({ bindAddr: '127.0.0.1:0', psk: PSK });
  nodes.push(node);
  // Three synthetic publishers across two regions / two GPU types,
  // matching the Rust E2E fixture so the assertions stay in sync.
  node._testInjectSyntheticPeerWithTags(BigInt(0xa), [
    'hardware.gpu',
    'hardware.gpu.h100',
    'hardware.gpu.count=8',
    'software.python=3.11',
    'scope:region:us-east',
  ]);
  node._testInjectSyntheticPeerWithTags(BigInt(0xb), [
    'hardware.gpu',
    'hardware.gpu.h100',
    'hardware.gpu.count=4',
    'software.python=3.12',
    'scope:region:us-east',
  ]);
  node._testInjectSyntheticPeerWithTags(BigInt(0xc), [
    'hardware.gpu',
    'hardware.gpu.a100',
    'hardware.gpu.count=2',
    'software.python=3.11',
    'scope:region:us-west',
  ]);
  return node;
}

describe('capabilityAggregate (E2E)', () => {
  it('counts publishers per region', async () => {
    const node = await primedNode();
    const groupBy: GroupBy = { kind: 'region' };
    const agg: Aggregation = { kind: 'count' };
    const rows = node.capabilityAggregate(undefined, groupBy, agg);
    const map = new Map(rows.map((r) => [r.bucket, r.value]));
    expect(map.get('us-east')).toBe(2n);
    expect(map.get('us-west')).toBe(1n);
  });

  it('buckets by GPU tag stem', async () => {
    const node = await primedNode();
    const matcher: TagMatcher = { kind: 'prefix', value: 'hardware.gpu' };
    const groupBy: GroupBy = { kind: 'tagStem', prefix: 'hardware.gpu' };
    const agg: Aggregation = { kind: 'count' };
    const rows = node.capabilityAggregate(matcher, groupBy, agg);
    const map = new Map(rows.map((r) => [r.bucket, r.value]));
    expect(map.get('h100')).toBe(2n);
    expect(map.get('a100')).toBe(1n);
    expect(map.get('count')).toBe(3n);
  });

  it('sums the numeric tag value per region', async () => {
    const node = await primedNode();
    const groupBy: GroupBy = { kind: 'region' };
    const agg: Aggregation = {
      kind: 'sumNumericTag',
      axisKey: 'hardware.gpu.count',
    };
    const rows = node.capabilityAggregate(undefined, groupBy, agg);
    const map = new Map(rows.map((r) => [r.bucket, r.value]));
    expect(map.get('us-east')).toBe(12n);
    expect(map.get('us-west')).toBe(2n);
  });
});

describe('capabilityCapacityRanking (E2E)', () => {
  it('breaks down state per region with summed capacity', async () => {
    const node = await primedNode();
    const query: CapacityQuery = {
      groupBy: { kind: 'region' },
      sumAxisKey: 'hardware.gpu.count',
      limit: 0,
    };
    const rows = node.capabilityCapacityRanking(query);
    expect(rows).toHaveLength(2);
    // Sorted by `available` desc.
    expect(rows[0].bucket).toBe('us-east');
    expect(rows[0].available).toBe(2n);
    expect(rows[0].summedCapacity).toBe(12n);
    expect(rows[1].bucket).toBe('us-west');
    expect(rows[1].available).toBe(1n);
    expect(rows[1].summedCapacity).toBe(2n);
  });

  it('filters by RTT — unknown publishers drop fail-closed', async () => {
    const node = await primedNode();
    const query: CapacityQuery = {
      groupBy: { kind: 'region' },
      maxRttMs: 50,
      limit: 0,
    };
    // Only 0xA has a known RTT under the threshold; 0xB + 0xC
    // resolve to None and drop.
    const rttMap = [{ nodeId: BigInt(0xa), rttMs: 10 }];
    const rows = node.capabilityCapacityRanking(query, rttMap);
    expect(rows).toHaveLength(1);
    expect(rows[0].bucket).toBe('us-east');
    expect(rows[0].available).toBe(1n);
  });

  it('truncates to limit and orders by available desc', async () => {
    const node = await primedNode();
    const query: CapacityQuery = {
      groupBy: { kind: 'region' },
      limit: 1,
    };
    const rows = node.capabilityCapacityRanking(query);
    expect(rows).toHaveLength(1);
    expect(rows[0].bucket).toBe('us-east');
  });
});
