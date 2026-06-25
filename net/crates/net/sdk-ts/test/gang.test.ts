// Integration tests for the gang-claim scheduler wrapper on MeshNode:
// reserve/release (including unheld→lost) and a single-node
// publish→match→claim round-trip. Mirrors the Rust-side surface test in
// `net/crates/net/sdk/tests/gang_surface.rs`.

import { describe, expect, it } from 'vitest';

import { MeshNode } from '../src/mesh';

const PSK = '5b'.repeat(32);

function nowUs(): bigint {
  return BigInt(Date.now()) * 1000n;
}

describe('gang scheduler (MeshNode)', () => {
  it('reserves, releases, and reports lost for an unheld island', async () => {
    const node = await MeshNode.create({ bindAddr: '127.0.0.1:0', psk: PSK });
    const until = nowUs() + 60_000_000n;

    expect(await node.reserveIsland(0xa0n, until)).toBe('won');
    expect(await node.releaseIsland(0xa0n)).toBe('won');
    // Releasing an island this node never held → lost (not a false won).
    expect(await node.releaseIsland(0xbeefn)).toBe('lost');
  });

  it('publishes, matches, and claims its own island (single node)', async () => {
    const node = await MeshNode.create({ bindAddr: '127.0.0.1:0', psk: PSK });

    await node.announceCapabilities({ tags: ['gpu:h100'] });
    await node.publishIslandTopology({
      id: 0xd0n,
      units: [0, 1, 2, 3, 4, 5, 6, 7],
      capabilities: ['model:a1'],
      load: 0.1,
      p50LatencyUs: 800,
    });

    const criteria = {
      tagsAll: ['gpu:h100'],
      minUnits: 8,
      selection: 'least_loaded',
    };
    expect(node.matchIslands(criteria)).toEqual([0xd0n]);

    const claimed = await node.claimIsland(criteria, nowUs() + 60_000_000n);
    expect(claimed).toBe(0xd0n);
  });
});
