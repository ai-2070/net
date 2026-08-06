// Single-winner capability discovery — `findBestNode` /
// `findBestNodeScoped`.
//
// Scoring semantics belong to the substrate and are pinned there by
// the four inverse witnesses in `capability_bridge.rs`; duplicating
// all four axes here would test the same code twice through a thinner
// lens. What these tests own is the TS side of the boundary: the DTO
// conversion, that non-finite weights are refused and finite ones are
// clamped rather than refused, and that a weight set in TypeScript
// still reaches the scorer and changes the winner.

import { afterEach, describe, expect, it } from 'vitest';

import {
  capabilityRequirementToNapi,
  type CapabilityRequirement,
} from '../src/capabilities';
import { MeshNode } from '../src/mesh';

const PSK = '42'.repeat(32);

let portSeed = 29_800;
function nextPort(): string {
  return `127.0.0.1:${portSeed++}`;
}

const nodes: MeshNode[] = [];
afterEach(async () => {
  while (nodes.length > 0) {
    const n = nodes.pop()!;
    try {
      await n.shutdown();
    } catch {
      // Ignore — the test may have already closed the node.
    }
  }
});

async function waitUntil(fn: () => boolean, timeoutMs = 5_000): Promise<boolean> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (fn()) return true;
    await new Promise((r) => setTimeout(r, 25));
  }
  return fn();
}

/**
 * One querying node connected to two announcing peers.
 *
 * The querier announces nothing, so it never self-indexes into its
 * own fold and the only candidates are the two peers.
 */
async function trio(): Promise<{ q: MeshNode; peers: [MeshNode, MeshNode] }> {
  const qAddr = nextPort();
  const p1Addr = nextPort();
  const p2Addr = nextPort();
  const q = await MeshNode.create({ bindAddr: qAddr, psk: PSK });
  const p1 = await MeshNode.create({ bindAddr: p1Addr, psk: PSK });
  const p2 = await MeshNode.create({ bindAddr: p2Addr, psk: PSK });
  nodes.push(q, p1, p2);

  await Promise.all([
    p1.accept(q.nodeId()),
    p2.accept(q.nodeId()),
    (async () => {
      await new Promise((r) => setTimeout(r, 50));
      await q.connect(p1Addr, p1.publicKey(), p1.nodeId());
      await q.connect(p2Addr, p2.publicKey(), p2.nodeId());
    })(),
  ]);
  await Promise.all([q.start(), p1.start(), p2.start()]);
  return { q, peers: [p1, p2] };
}

/**
 * Stage the two peers so the STRONGER one has the HIGHER node id.
 *
 * Node ids are derived from a fresh keypair per node, so which peer
 * sorts first is not knowable until runtime — hence the sort. Without
 * it, a run where the strong peer happened to hold the lower id would
 * pass even if the weight did nothing at all, because the lowest id is
 * exactly what an unweighted query returns.
 */
async function stageByVram(
  peers: [MeshNode, MeshNode],
  weakVramGb: number,
  strongVramGb: number,
): Promise<{ lowId: bigint; highId: bigint }> {
  const sorted = [...peers].sort((a, b) => (a.nodeId() < b.nodeId() ? -1 : 1));
  const [low, high] = sorted;
  await low.announceCapabilities({
    hardware: { gpu: { vendor: 'nvidia', model: 'weak', vramGb: weakVramGb } },
    tags: ['gpu-pool'],
  });
  await high.announceCapabilities({
    hardware: { gpu: { vendor: 'nvidia', model: 'strong', vramGb: strongVramGb } },
    tags: ['gpu-pool'],
  });
  return { lowId: low.nodeId(), highId: high.nodeId() };
}

const POOL: CapabilityRequirement = { filter: { requireTags: ['gpu-pool'] } };

describe('capabilityRequirementToNapi', () => {
  it('carries every weight through and leaves omitted ones undefined', () => {
    const napi = capabilityRequirementToNapi({
      filter: { requireTags: ['gpu'] },
      preferMoreMemory: 0.25,
      preferMoreVram: 0.5,
      preferFasterInference: 0.75,
    });
    expect(napi.filter.requireTags).toEqual(['gpu']);
    expect(napi.preferMoreMemory).toBe(0.25);
    expect(napi.preferMoreVram).toBe(0.5);
    expect(napi.preferFasterInference).toBe(0.75);
    // Omitted, not zeroed — the native side applies the default, so
    // the converter must not invent one that could drift from it.
    expect(napi.preferLoadedModels).toBeUndefined();
  });

  it('converts a bare filter with no weights', () => {
    const napi = capabilityRequirementToNapi({ filter: {} });
    expect(napi.preferMoreMemory).toBeUndefined();
    expect(napi.preferMoreVram).toBeUndefined();
    expect(napi.preferFasterInference).toBeUndefined();
    expect(napi.preferLoadedModels).toBeUndefined();
  });
});

describe('MeshNode.findBestNode', () => {
  it('returns null when nothing matches', async () => {
    const q = await MeshNode.create({ bindAddr: nextPort(), psk: PSK });
    nodes.push(q);
    expect(q.findBestNode({ filter: { requireTags: ['nobody-has-this'] } })).toBeNull();
  });

  it('picks the higher-VRAM peer over the lower node id', async () => {
    const { q, peers } = await trio();
    const { lowId, highId } = await stageByVram(peers, 8, 80);

    const seen = await waitUntil(() => q.findNodes(POOL.filter).length === 2);
    expect(seen).toBe(true);

    // Unweighted: every match scores the same, so the tie-break
    // decides and the lower id wins. This half is what makes the
    // next assertion mean something.
    expect(q.findBestNode(POOL)).toBe(lowId);

    // Same fold, same candidates — only the weight changes.
    expect(q.findBestNode({ ...POOL, preferMoreVram: 1 })).toBe(highId);
  });

  it('accepts finite out-of-range weights and clamps them', async () => {
    const { q, peers } = await trio();
    const { lowId, highId } = await stageByVram(peers, 8, 80);
    expect(await waitUntil(() => q.findNodes(POOL.filter).length === 2)).toBe(true);

    // 5 clamps to 1 in the substrate, so it selects like a full
    // weight rather than throwing — one clamp contract shared with
    // Rust, Go, C and Python.
    expect(q.findBestNode({ ...POOL, preferMoreVram: 5 })).toBe(highId);
    // -1 clamps to 0, which is "don't consult this axis".
    expect(q.findBestNode({ ...POOL, preferMoreVram: -1 })).toBe(lowId);
  });

  it('rejects NaN and infinite weights instead of clamping them', async () => {
    const q = await MeshNode.create({ bindAddr: nextPort(), psk: PSK });
    nodes.push(q);
    // A NaN weight would survive clamping and then lose every score
    // comparison, so a requirement written as "strongly prefer VRAM"
    // would silently select as if unweighted. Infinity clamps to a
    // value the caller never wrote. Both throw at the boundary.
    for (const bad of [NaN, Infinity, -Infinity]) {
      expect(() => q.findBestNode({ ...POOL, preferMoreVram: bad })).toThrow(/finite/);
      expect(() => q.findBestNode({ ...POOL, preferMoreMemory: bad })).toThrow(/finite/);
      expect(() =>
        q.findBestNode({ ...POOL, preferFasterInference: bad }),
      ).toThrow(/finite/);
      expect(() => q.findBestNode({ ...POOL, preferLoadedModels: bad })).toThrow(/finite/);
    }
  });
});

describe('MeshNode.findBestNodeScoped', () => {
  it('narrows before scoring, so the strongest out-of-scope peer cannot win', async () => {
    const { q, peers } = await trio();
    const sorted = [...peers].sort((a, b) => (a.nodeId() < b.nodeId() ? -1 : 1));
    const [red, blue] = sorted;
    // The stronger GPU sits in the other tenant AND on the higher
    // node id, so neither capacity nor the tie-break can hand it back
    // if the scope is honoured.
    await red.announceCapabilities({
      hardware: { gpu: { vendor: 'nvidia', model: 'weak', vramGb: 8 } },
      tags: ['gpu-pool', 'scope:tenant:red'],
    });
    await blue.announceCapabilities({
      hardware: { gpu: { vendor: 'nvidia', model: 'strong', vramGb: 80 } },
      tags: ['gpu-pool', 'scope:tenant:blue'],
    });
    expect(await waitUntil(() => q.findNodes(POOL.filter).length === 2)).toBe(true);

    const weighted = { ...POOL, preferMoreVram: 1 };
    expect(q.findBestNode(weighted)).toBe(blue.nodeId());
    expect(q.findBestNodeScoped(weighted, { kind: 'tenant', tenant: 'red' })).toBe(
      red.nodeId(),
    );
  });

  it('returns null when the scope admits nobody', async () => {
    const { q, peers } = await trio();
    await stageByVram(peers, 8, 80);
    expect(await waitUntil(() => q.findNodes(POOL.filter).length === 2)).toBe(true);

    expect(
      q.findBestNodeScoped(POOL, { kind: 'tenant', tenant: 'nobody' }),
    ).not.toBeNull();
    // Both peers are untagged, and untagged peers resolve to Global —
    // which `tenant` admits by design. `globalOnly` is the filter that
    // would exclude a tenant-tagged peer, so use a scope that can
    // genuinely empty the set: subnet-local peers under `sameSubnet`
    // with no policy configured.
    expect(q.findBestNodeScoped(POOL, { kind: 'sameSubnet' })).toBeNull();
  });
});
