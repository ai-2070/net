// A seeded node keeps its Noise key — and so its rendezvous address —
// across restarts (APP_INTEGRATION_BLOCKERS_PLAN.md Phase 1). Before the
// fix, `publicKey()` was new on every `MeshNode.create`, so every machine
// that had paired with this one lost it on restart.

import { randomBytes } from 'node:crypto';

import { describe, expect, it } from 'vitest';

import { MeshNode } from '../src/index';

const PSK = '6e'.repeat(32);

async function keyOf(config: { identitySeed?: Buffer; noiseStaticKey?: Buffer }) {
  const node = await MeshNode.create({ bindAddr: '127.0.0.1:0', psk: PSK, ...config });
  try {
    return { key: node.publicKey(), rendezvous: node.rendezvousString().split('|')[1] };
  } finally {
    await node.shutdown();
  }
}

describe('Noise static key', () => {
  it('follows the identity seed across restarts', async () => {
    const seed = randomBytes(32);
    const first = await keyOf({ identitySeed: seed });
    const second = await keyOf({ identitySeed: seed });
    expect(second.key).toEqual(first.key);
    // The rendezvous string a joiner saves names this key.
    expect(second.rendezvous).toEqual(first.rendezvous);
    expect((await keyOf({ identitySeed: randomBytes(32) })).key).not.toEqual(first.key);
  });

  it('an explicit noiseStaticKey wins and is stable on its own', async () => {
    const noiseStaticKey = randomBytes(32);
    const a = await keyOf({ identitySeed: randomBytes(32), noiseStaticKey });
    const b = await keyOf({ identitySeed: randomBytes(32), noiseStaticKey });
    expect(b.key).toEqual(a.key);
  });

  it('is fresh per process without a seed', async () => {
    expect((await keyOf({})).key).not.toEqual((await keyOf({})).key);
  });

  it('rejects a key that is not 32 bytes', async () => {
    await expect(
      MeshNode.create({ bindAddr: '127.0.0.1:0', psk: PSK, noiseStaticKey: Buffer.alloc(31) }),
    ).rejects.toThrow(/noise_static_key must be 32 bytes, got 31/);
  });
});
