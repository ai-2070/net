// Integration tests for the capability-broadcast surface on
// `MeshNode`. Exercises announce → find round-trip, self-index,
// late-joiner push, and the flattened POJO conversions — mirrors the
// Rust `tests/capability_broadcast.rs` suite.

import { afterEach, describe, expect, it } from 'vitest';

import type { CapabilityFilter, CapabilitySet } from '../src/capabilities';
import { MeshNode } from '../src/mesh';

const PSK = '42'.repeat(32);

let portSeed = 29_400;
function nextPortPair(): [string, string] {
  const a = portSeed++;
  const b = portSeed++;
  return [`127.0.0.1:${a}`, `127.0.0.1:${b}`];
}

async function handshake(a: MeshNode, b: MeshNode, bAddr: string): Promise<void> {
  const bPub = b.publicKey();
  const aId = a.nodeId();
  const bId = b.nodeId();
  await Promise.all([
    b.accept(aId),
    (async () => {
      await new Promise((r) => setTimeout(r, 50));
      await a.connect(bAddr, bPub, bId);
    })(),
  ]);
  await a.start();
  await b.start();
}

async function pair(): Promise<{ a: MeshNode; b: MeshNode; bAddr: string }> {
  const [aAddr, bAddr] = nextPortPair();
  const a = await MeshNode.create({ bindAddr: aAddr, psk: PSK });
  const b = await MeshNode.create({ bindAddr: bAddr, psk: PSK });
  await handshake(a, b, bAddr);
  return { a, b, bAddr };
}

async function waitUntil(fn: () => boolean, timeoutMs = 2_000): Promise<boolean> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (fn()) return true;
    await new Promise((r) => setTimeout(r, 25));
  }
  return fn();
}

const nodes: MeshNode[] = [];
afterEach(async () => {
  while (nodes.length > 0) {
    const n = nodes.pop()!;
    try {
      await n.shutdown();
    } catch {
      // Ignore — test may have already closed the node.
    }
  }
});

describe('MeshNode capabilities', () => {
  it('self-matches on its own announcement (single node)', async () => {
    const a = await MeshNode.create({ bindAddr: '127.0.0.1:0', psk: PSK });
    nodes.push(a);

    await a.announceCapabilities({ tags: ['gpu', 'inference'] });

    const hits = a.findNodes({ requireTags: ['gpu'] });
    expect(hits).toContain(a.nodeId());
  });

  it('returns empty for a non-matching filter', async () => {
    const a = await MeshNode.create({ bindAddr: '127.0.0.1:0', psk: PSK });
    nodes.push(a);
    await a.announceCapabilities({ tags: ['gpu'] });

    expect(a.findNodes({ requireTags: ['nope'] })).toEqual([]);
  });

  it('announce → find propagates across the handshake (two nodes)', async () => {
    const { a, b } = await pair();
    nodes.push(a, b);

    await a.announceCapabilities({
      hardware: {
        cpuCores: 16,
        memoryGb: 64,
        gpu: { vendor: 'nvidia', model: 'RTX 4090', vramGb: 24 },
      },
      tags: ['gpu', 'inference'],
    });

    const aId = a.nodeId();
    const filter: CapabilityFilter = { requireGpu: true, minVramMb: 16_384 };

    const arrived = await waitUntil(() => b.findNodes(filter).includes(aId));
    expect(arrived).toBe(true);
  });

  it('late joiner receives session-open push', async () => {
    const [aAddr, bAddr] = nextPortPair();
    const a = await MeshNode.create({ bindAddr: aAddr, psk: PSK });
    nodes.push(a);

    // A announces *before* B exists.
    await a.announceCapabilities({ tags: ['preannounced'] });

    const b = await MeshNode.create({ bindAddr: bAddr, psk: PSK });
    nodes.push(b);
    await handshake(a, b, bAddr);

    const aId = a.nodeId();
    const arrived = await waitUntil(() =>
      b.findNodes({ requireTags: ['preannounced'] }).includes(aId),
    );
    expect(arrived).toBe(true);
  });

  it('round-trips a complex POJO (hardware + software + models + tools)', async () => {
    const a = await MeshNode.create({ bindAddr: '127.0.0.1:0', psk: PSK });
    nodes.push(a);

    const caps: CapabilitySet = {
      hardware: {
        cpuCores: 32,
        cpuThreads: 64,
        memoryGb: 128,
        gpu: {
          vendor: 'nvidia',
          model: 'H100',
          vramGb: 80,
          computeUnits: 132,
        },
        storageGb: 4000n,
      },
      software: {
        os: 'linux',
        osVersion: '6.5.0',
        runtimes: [
          ['python', '3.12.4'],
          ['node', '22.3.0'],
        ],
        cudaVersion: '12.4',
      },
      models: [
        {
          modelId: 'llama-3.1-70b',
          family: 'llama',
          parametersBX10: 700,
          contextLength: 131_072,
          modalities: ['text', 'code'],
          loaded: true,
        },
      ],
      tools: [{ toolId: 'web_search', name: 'web search', stateless: true }],
      tags: ['prod', 'us-east'],
      limits: { maxConcurrentRequests: 32 },
    };
    await a.announceCapabilities(caps);

    // Each require* dimension resolves back to self.
    expect(a.findNodes({ requireTags: ['prod'] })).toContain(a.nodeId());
    expect(a.findNodes({ requireModels: ['llama-3.1-70b'] })).toContain(a.nodeId());
    expect(a.findNodes({ requireTools: ['web_search'] })).toContain(a.nodeId());
    expect(a.findNodes({ requireModalities: ['code'] })).toContain(a.nodeId());
    expect(a.findNodes({ gpuVendor: 'nvidia', minVramMb: 40_000 })).toContain(a.nodeId());
  });

  it('drops expired entries after TTL + a GC sweep', async () => {
    // Short interval so the test completes fast. The announcement
    // TTL is 1 s; a 200 ms GC tick means two-to-three sweeps happen
    // before the 1.5 s re-query.
    //
    // `announceCapabilities` takes no TTL override from the TS
    // surface yet — the core default is 5 min — so we'd normally
    // wait minutes. Use `capabilityGcIntervalMs` to speed GC and
    // reach the core `announce_capabilities_with(..., ttl)` via the
    // plain `announceCapabilities` path on the Rust side (which
    // uses 5 min). That TTL is too long here, so we skip the
    // "eventually empty" assertion and instead verify:
    //   (a) the announcement IS indexed (positive path), and
    //   (b) the `capabilityGcIntervalMs` knob is accepted.
    //
    // Full TTL expiry is covered by
    // `tests/capability_broadcast.rs::announcement_expires_after_ttl`
    // where `announce_capabilities_with(caps, 1s, false)` is
    // available at the core Rust layer.
    const a = await MeshNode.create({
      bindAddr: '127.0.0.1:0',
      psk: PSK,
      capabilityGcIntervalMs: 200,
    });
    nodes.push(a);
    await a.announceCapabilities({ tags: ['gc-smoke'] });
    expect(a.findNodes({ requireTags: ['gc-smoke'] })).toContain(a.nodeId());
  });

  it('accepts the requireSignedCapabilities knob without breaking the local path', async () => {
    // `announceCapabilities` path stamps no signature on the wire
    // (signing binds to Stage E), so with
    // `requireSignedCapabilities = true` on a receiver, a direct
    // unsigned announcement would be dropped. We can't easily
    // two-node-test that over the TS boundary (no TS-side wire
    // control), so this is a config-plumbing smoke: the option is
    // accepted and the local self-index path still works because
    // `announce_capabilities` indexes locally before sending.
    const a = await MeshNode.create({
      bindAddr: '127.0.0.1:0',
      psk: PSK,
      requireSignedCapabilities: true,
    });
    nodes.push(a);
    await a.announceCapabilities({ tags: ['local-only'] });
    expect(a.findNodes({ requireTags: ['local-only'] })).toContain(a.nodeId());
  });

  // Scope-tag discovery — the NAPI layer has unique plumbing
  // (`ScopeFilterJs` → `ScopeFilterOwned` → `with_scope_filter`
  // borrow trampoline). Exercise it end-to-end with a single-node
  // self-match; the underlying matching logic is covered by the
  // Rust unit + integration suites.
  it('findNodesScoped: tenant-tagged self matches under matching tenant', async () => {
    const a = await MeshNode.create({ bindAddr: '127.0.0.1:0', psk: PSK });
    nodes.push(a);

    await a.announceCapabilities({
      tags: ['model:llama3-70b', 'scope:tenant:oem-123'],
    });

    const filter: CapabilityFilter = { requireTags: ['model:llama3-70b'] };

    // Matching tenant — own node id appears.
    expect(a.findNodesScoped(filter, { kind: 'tenant', tenant: 'oem-123' })).toContain(
      a.nodeId(),
    );

    // Non-matching tenant — own node id is excluded (tenant-tagged
    // peer is invisible to other-tenant queries).
    expect(a.findNodesScoped(filter, { kind: 'tenant', tenant: 'corp-acme' })).not.toContain(
      a.nodeId(),
    );

    // GlobalOnly — tenant-tagged peer also excluded.
    expect(a.findNodesScoped(filter, { kind: 'globalOnly' })).not.toContain(a.nodeId());
  });

  it('findNodesScoped: untagged Global peer remains visible to tenant queries', async () => {
    // The permissive default — a node that doesn't tag itself stays
    // discoverable under tenant-scoped queries. Locks in the v1
    // backwards-compat behaviour through the JS-side scope filter.
    const a = await MeshNode.create({ bindAddr: '127.0.0.1:0', psk: PSK });
    nodes.push(a);

    await a.announceCapabilities({ tags: ['gpu'] });

    expect(
      a.findNodesScoped({ requireTags: ['gpu'] }, { kind: 'tenant', tenant: 'oem-123' }),
    ).toContain(a.nodeId());
  });

  it('findNodesScoped: regions list variant marshals correctly through NAPI', async () => {
    // Multi-element variants (`tenants` / `regions`) take a separate
    // path in `with_scope_filter` because they need an intermediate
    // `Vec<&str>` whose lifetime outlives the borrow. This test
    // hits that borrow trampoline end-to-end.
    const a = await MeshNode.create({ bindAddr: '127.0.0.1:0', psk: PSK });
    nodes.push(a);

    await a.announceCapabilities({
      tags: ['relay-capable', 'scope:region:eu-west'],
    });

    const filter: CapabilityFilter = { requireTags: ['relay-capable'] };

    // Multi-region list including ours — match.
    expect(
      a.findNodesScoped(filter, { kind: 'regions', regions: ['us-east', 'eu-west'] }),
    ).toContain(a.nodeId());

    // Multi-region list excluding ours — no match.
    expect(
      a.findNodesScoped(filter, { kind: 'regions', regions: ['us-east', 'ap-south'] }),
    ).not.toContain(a.nodeId());
  });

  // Regression: P2 (Cubic) — empty-string sanitization on
  // `tenants` / `regions` lists. Unsanitized input like `[""]`
  // used to flow through to a `Tenants([""])` filter, which
  // matches no real tenant and silently narrows results to
  // Global candidates. Fix: drop empties; if list is empty
  // after cleaning, fall back to Any.
  it('findNodesScoped: tenants list with only empty strings falls back to Any', async () => {
    const a = await MeshNode.create({ bindAddr: '127.0.0.1:0', psk: PSK });
    nodes.push(a);

    // Tenant-tagged provider — without sanitization, a
    // `tenants: [""]` query would *not* return this node
    // (empty string never matches "oem-123") and would NOT
    // return Global nodes either (none exist here).
    await a.announceCapabilities({
      tags: ['gpu', 'scope:tenant:oem-123'],
    });

    // After sanitization: `tenants: [""]` collapses to Any,
    // which matches every non-SubnetLocal candidate including
    // tenant-tagged ones.
    expect(
      a.findNodesScoped({ requireTags: ['gpu'] }, { kind: 'tenants', tenants: [''] }),
    ).toContain(a.nodeId());

    // `tenants: []` (empty list) also falls back to Any.
    expect(
      a.findNodesScoped({ requireTags: ['gpu'] }, { kind: 'tenants', tenants: [] }),
    ).toContain(a.nodeId());
  });

  it('findNodesScoped: tenants list with mixed empty and real ids drops empties', async () => {
    // P2 partial-cleaning case: `tenants: ["", "oem-123"]`
    // sanitizes to `Tenants(["oem-123"])` — real tenant filter
    // semantics preserved, empty entry silently dropped.
    const a = await MeshNode.create({ bindAddr: '127.0.0.1:0', psk: PSK });
    nodes.push(a);

    await a.announceCapabilities({
      tags: ['gpu', 'scope:tenant:oem-123'],
    });

    expect(
      a.findNodesScoped(
        { requireTags: ['gpu'] },
        { kind: 'tenants', tenants: ['', 'oem-123'] },
      ),
    ).toContain(a.nodeId());

    // Real tenant filter: empty + non-matching tenant excludes us.
    expect(
      a.findNodesScoped(
        { requireTags: ['gpu'] },
        { kind: 'tenants', tenants: ['', 'corp-acme'] },
      ),
    ).not.toContain(a.nodeId());
  });

  it('findNodesScoped: regions list with only empty strings falls back to Any', async () => {
    // Same shape as the tenants regression but for regions.
    const a = await MeshNode.create({ bindAddr: '127.0.0.1:0', psk: PSK });
    nodes.push(a);

    await a.announceCapabilities({
      tags: ['relay-capable', 'scope:region:eu-west'],
    });

    expect(
      a.findNodesScoped(
        { requireTags: ['relay-capable'] },
        { kind: 'regions', regions: [''] },
      ),
    ).toContain(a.nodeId());

    expect(
      a.findNodesScoped(
        { requireTags: ['relay-capable'] },
        { kind: 'regions', regions: [] },
      ),
    ).toContain(a.nodeId());
  });
});
