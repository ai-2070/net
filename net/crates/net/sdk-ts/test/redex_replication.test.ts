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

import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

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

describe('Redex replication with standard placement', () => {
  it('both nodes become replicas of a factor-2 channel with no node list', async () => {
    // `standard` (the default) used to start with an empty replica set and
    // never replicate. Each node now advertises candidacy when it opens the
    // channel, and both pick the same set.
    const [a, b] = await connectedPair();
    const redexA = new Redex();
    const redexB = new Redex();
    redexA.enableReplication(a);
    redexB.enableReplication(b);
    // leaderPinned only names who leads once both are in the set; the set
    // itself comes from placement, not a node list.
    const replication = { factor: 2, heartbeatMs: 150n, leaderPinned: a.nodeId() };
    const fileA = redexA.openFile('sdk/repl-standard', { replication });
    const fileB = redexB.openFile('sdk/repl-standard', { replication });
    const t0 = Date.now();
    // Each node joins once it sees both candidates. Capability changes are
    // rate-limited to one announce per window (10 s by default), so this
    // can take a window.
    await waitFor(
      'A elected leader',
      () =>
        /dataforts_leader_changes_total\{channel="sdk\/repl-standard"\} 1/.test(
          redexA.replicationPrometheusText(),
        ),
      30_000,
    );
    for (let i = 0; i < 4; i++) fileA.append(Buffer.from(`event-${i}`));
    await waitFor('B caught up', () => fileB.readRange(0n, 4n).length === 4, 10_000);
    expect(fileB.readRange(3n, 4n)[0].payload.toString()).toBe('event-3');
    // B never led: it joined a full set, not a set of one.
    expect(redexB.replicationPrometheusText()).toMatch(
      /dataforts_leader_changes_total\{channel="sdk\/repl-standard"\} 0/,
    );
    console.log(`standard placement converged in ${Date.now() - t0} ms`);

    redexA.disableReplication();
    redexB.disableReplication();
    await a.shutdown();
    await b.shutdown();
  }, 60_000);
});

describe('Redex.openFile with an interval fsync policy', () => {
  it('opens without a mesh (it spawns the fsync timer)', () => {
    // RedexFile spawns its fsync timer on open. From the JS thread, with no
    // mesh runtime adopted, that panicked and aborted the process.
    const dir = mkdtempSync(join(tmpdir(), 'net-sdk-fsync-'));
    try {
      const redex = new Redex({ persistentDir: dir });
      const file = redex.openFile('sdk/fsync', { persistent: true, fsyncIntervalMs: 50 });
      expect(file.append(Buffer.from('durable'))).toBe(0n);
      file.close();
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
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
