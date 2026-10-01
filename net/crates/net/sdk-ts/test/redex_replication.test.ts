// Redex replication and greedy dataforts through @net-mesh/sdk
// (NODE_SDK_GAPS_PLAN.md S8, C1, C2).
//
// - The nine Redex methods forward with an SDK MeshNode, and `replication`
//   reaches the native file config.
// - A replicated openFile no longer aborts the process (it spawned tokio
//   tasks on the JS thread, which has no reactor).
// - C1: a channel written on one node arrives on the other with no
//   hand-driven roles; before, a replicated channel never left Idle.
// - C2: disableReplication releases the node, so shutdown succeeds; before,
//   only garbage-collecting the Redex did.

import { describe, expect, it } from 'vitest';

import { MeshNode, Redex } from '../src/index';

const PSK = '7a'.repeat(32);
const node = () => MeshNode.create({ bindAddr: '127.0.0.1:0', psk: PSK });
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

async function connectedPair(): Promise<[MeshNode, MeshNode]> {
  const a = await node();
  const b = await node();
  await Promise.all([
    b.accept(a.nodeId()),
    (async () => {
      await sleep(50);
      await a.connect(b.localAddr(), b.publicKey(), b.nodeId());
    })(),
  ]);
  await a.start();
  await b.start();
  return [a, b];
}

async function waitFor(what: string, f: () => boolean, ms = 8000): Promise<void> {
  const deadline = Date.now() + ms;
  while (!f()) {
    if (Date.now() > deadline) throw new Error(`timed out: ${what}`);
    await sleep(50);
  }
}

describe('Redex replication', () => {
  it('a replicated config without enableReplication is rejected', () => {
    const redex = new Redex();
    expect(() => redex.openFile('sdk/repl-off', { replication: { factor: 1 } })).toThrow(
      /redex/,
    );
    expect(redex.replicationRuntimeCount()).toBe(0);
    expect(redex.replicationPrometheusText()).toBe('');
  });

  it('enableReplication takes a MeshNode and a replicated channel spawns its runtime', async () => {
    const n = await node();
    const redex = new Redex();
    redex.enableReplication(n);
    redex.enableReplication(n); // idempotent

    // Before the binding fix this call aborted the Node process:
    // "there is no reactor running".
    const file = redex.openFile('sdk/repl', {
      replication: { heartbeatMs: 150n, placement: 'pinned', pinnedNodes: [n.nodeId()] },
    });
    expect(redex.replicationRuntimeCount()).toBe(1);
    expect(redex.replicationPrometheusText()).toContain('channel="sdk/repl"');

    // The channel is still a working local log.
    expect(file.append(Buffer.from('x'))).toBe(0n);
    expect(file.readRange(0n, 1n)).toHaveLength(1);

    // C2: without disableReplication the Redex holds the node and
    // shutdown is refused ("outstanding references exist").
    redex.disableReplication();
    redex.disableReplication(); // idempotent
    expect(redex.replicationRuntimeCount()).toBe(0);
    await n.shutdown();
  });

  it('a channel written on one node is replicated to the other', async () => {
    const [a, b] = await connectedPair();
    const redexA = new Redex();
    const redexB = new Redex();
    redexA.enableReplication(a);
    redexB.enableReplication(b);
    const replication = {
      heartbeatMs: 150n,
      placement: 'pinned',
      pinnedNodes: [a.nodeId(), b.nodeId()],
      leaderPinned: a.nodeId(),
    };
    const fileA = redexA.openFile('sdk/repl-pair', { replication });
    const fileB = redexB.openFile('sdk/repl-pair', { replication });

    await waitFor('A elected leader', () =>
      /dataforts_leader_changes_total\{channel="sdk\/repl-pair"\} 1/.test(
        redexA.replicationPrometheusText(),
      ),
    );
    for (let i = 0; i < 8; i++) fileA.append(Buffer.from(`event-${i}`));
    await waitFor('B caught up', () => fileB.readRange(0n, 8n).length === 8);
    expect(fileB.readRange(7n, 8n)[0].payload.toString()).toBe('event-7');

    redexA.disableReplication();
    redexB.disableReplication();
    await a.shutdown();
    await b.shutdown();
  }, 30_000);
});

describe('Redex greedy dataforts', () => {
  it('enable, add gravity, and disable again with a MeshNode', async () => {
    const n = await node();
    const redex = new Redex();
    expect(redex.greedyCachedChannelCount()).toBe(0);

    // Gravity needs greedy first.
    expect(() => redex.enableGravityForGreedy(n)).toThrow(/redex/);

    redex.enableGreedyDataforts(n, { perChannelCapBytes: 1_048_576n });
    redex.enableGravityForGreedy(n, { tickIntervalMs: 100n });
    expect(redex.greedyCachedChannelCount()).toBe(0);
    expect(typeof redex.greedyPrometheusText()).toBe('string');

    redex.disableGravityForGreedy();
    redex.disableGreedyDataforts();
    redex.disableGreedyDataforts(); // idempotent
    expect(redex.greedyPrometheusText()).toBe('');
    await n.shutdown();
  });
});
