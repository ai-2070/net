// Redex replication and greedy dataforts through @net-mesh/sdk
// (NODE_SDK_GAPS_PLAN.md S8).
//
// What this proves: the nine Redex methods forward with an SDK MeshNode,
// `replication` reaches the native file config, and a replicated openFile
// no longer aborts the process (it spawned tokio tasks on the JS thread,
// which has no reactor).
//
// What it can't prove yet, both core gaps recorded in the plan (N8):
// - a replicated channel never leaves Idle in production, so data doesn't
//   actually cross nodes without hand-driven role transitions;
// - core has no way to undo `enableReplication`, so the node stays pinned
//   until the Redex is garbage-collected and `shutdown()` is refused.
//   The replication test therefore doesn't shut its node down.

import { describe, expect, it } from 'vitest';

import { MeshNode, Redex } from '../src/index';

const PSK = '7a'.repeat(32);
const node = () => MeshNode.create({ bindAddr: '127.0.0.1:0', psk: PSK });

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
    file.close();
  });
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
