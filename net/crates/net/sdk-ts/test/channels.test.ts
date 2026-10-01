// Integration tests for the channel (distributed pub/sub) surface
// on `MeshNode`. Exercises register → subscribe → publish end-to-end
// plus the typed error-classification path.
//
// After the BigInt-widening work in
// `docs/SDK_MESH_PARITY_PLAN.md` Stage A, node_id crosses the napi
// boundary as `bigint`, so these tests can run handshake end-to-end
// without the old "node_id exceeds i64::MAX" failure.

import { describe, expect, it } from 'vitest';

import {
  ChannelAuthError,
  ChannelError,
  MeshNode,
  type PublishReport,
} from '../src/mesh';

const PSK = '42'.repeat(32);

// Per-test unique ports so vitest can still run these serially in the
// same process without port reuse across test cases.
let portSeed = 28_400;
function nextPortPair(): [string, string] {
  const a = portSeed++;
  const b = portSeed++;
  return [`127.0.0.1:${a}`, `127.0.0.1:${b}`];
}

async function handshake(
  a: MeshNode,
  b: MeshNode,
  bAddr: string,
): Promise<void> {
  const bPub = b.publicKey();
  const aId = a.nodeId();
  const bId = b.nodeId();
  const [, ] = await Promise.all([
    b.accept(aId),
    (async () => {
      await new Promise((r) => setTimeout(r, 50));
      await a.connect(bAddr, bPub, bId);
    })(),
  ]);
  await a.start();
  await b.start();
}

async function pair(): Promise<{
  a: MeshNode;
  b: MeshNode;
  bAddr: string;
  bId: bigint;
}> {
  const [aAddr, bAddr] = nextPortPair();
  const a = await MeshNode.create({ bindAddr: aAddr, psk: PSK });
  const b = await MeshNode.create({ bindAddr: bAddr, psk: PSK });
  await handshake(a, b, bAddr);
  return { a, b, bAddr, bId: b.nodeId() };
}

describe('MeshNode channels (standalone — no handshake)', () => {
  it('register_channel accepts valid config', async () => {
    const a = await MeshNode.create({ bindAddr: '127.0.0.1:28900', psk: PSK });
    a.registerChannel({
      name: 'sensors/temp',
      visibility: 'global',
      reliable: true,
      priority: 3,
      maxRatePps: 1000,
    });
    await a.shutdown();
  });

  it('register_channel accepts all visibility variants', async () => {
    const a = await MeshNode.create({ bindAddr: '127.0.0.1:28901', psk: PSK });
    for (const visibility of [
      'subnet-local',
      'parent-visible',
      'exported',
      'global',
    ] as const) {
      a.registerChannel({ name: `v/${visibility}`, visibility });
    }
    await a.shutdown();
  });

  it('invalid visibility surfaces as ChannelError', async () => {
    const a = await MeshNode.create({ bindAddr: '127.0.0.1:28902', psk: PSK });
    expect(() =>
      a.registerChannel({
        name: 'bad',
        // @ts-expect-error deliberately invalid string
        visibility: 'nonsense',
      }),
    ).toThrowError(ChannelError);
    await a.shutdown();
  });

  it('invalid channel name surfaces as ChannelError', async () => {
    const a = await MeshNode.create({ bindAddr: '127.0.0.1:28903', psk: PSK });
    expect(() =>
      a.registerChannel({ name: 'has spaces which are not allowed' }),
    ).toThrowError(ChannelError);
    await a.shutdown();
  });

  it('ChannelAuthError is a subclass of ChannelError', () => {
    const e = new ChannelAuthError('unauthorized');
    expect(e).toBeInstanceOf(ChannelAuthError);
    expect(e).toBeInstanceOf(ChannelError);
    expect(e).toBeInstanceOf(Error);
  });
});

describe('MeshNode channels (two-node handshake e2e)', () => {
  it('register → subscribe → publish delivers to roster', async () => {
    const { a, b, bId } = await pair();

    b.registerChannel({
      name: 'sensors/temp',
      visibility: 'global',
      reliable: true,
    });

    await a.subscribeChannel(bId, 'sensors/temp');

    // Let the publisher's roster register A.
    await new Promise((r) => setTimeout(r, 50));

    const report: PublishReport = await b.publish(
      'sensors/temp',
      Buffer.from('hello'),
      { reliability: 'reliable', onFailure: 'best_effort' },
    );

    expect(report.attempted).toBe(1);
    expect(report.delivered).toBe(1);
    expect(report.errors).toHaveLength(0);

    await a.shutdown();
    await b.shutdown();
  });

  it('subscribing to an unknown channel throws ChannelError', async () => {
    const { a, b, bId } = await pair();

    // Register `foo` so the publisher enforces ACL, then ask for `bar`.
    b.registerChannel({ name: 'foo' });

    await expect(a.subscribeChannel(bId, 'bar')).rejects.toBeInstanceOf(
      ChannelError,
    );

    await a.shutdown();
    await b.shutdown();
  });

  it('permissiveChannels reaches the native node: the same subscribe is admitted', async () => {
    // The mirror of the test above. `MeshNode.create` used to drop this
    // option (NODE_SDK_GAPS_PLAN.md S2), so the publisher stayed strict and
    // this subscribe was refused. Permissive means no registry at all: ANY
    // channel is reachable, which is why it is off by default.
    const [aAddr, bAddr] = nextPortPair();
    const a = await MeshNode.create({ bindAddr: aAddr, psk: PSK });
    const b = await MeshNode.create({ bindAddr: bAddr, psk: PSK, permissiveChannels: true });
    await handshake(a, b, bAddr);

    b.registerChannel({ name: 'foo' });
    await expect(a.subscribeChannel(b.nodeId(), 'bar')).resolves.toBeUndefined();

    await a.shutdown();
    await b.shutdown();
  });

  it('unsubscribe of non-member is idempotent', async () => {
    const { a, b, bId } = await pair();
    b.registerChannel({ name: 'chan/x' });
    await expect(a.unsubscribeChannel(bId, 'chan/x')).resolves.toBeUndefined();
    await a.shutdown();
    await b.shutdown();
  });
});
