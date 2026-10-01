// The MeshNode methods S5 added, live through @net-mesh/sdk
// (NODE_SDK_GAPS_PLAN.md S5).
//
// `mesh_node_surface.test.ts` proves each method forwards; this proves the
// forwarded calls reach real behaviour on real UDP loopback: NAT state, an
// A2A task round trip, placement-filter registration, and enrollment over
// the mesh (join, then renew), the round trip S3 deferred to S5.
//
// Every import is from the SDK root. Nothing skips: CI's sdk-ts build
// compiles nat-traversal, a2a and delegation, so a missing native method is
// a failure.

import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

import { afterAll, describe, expect, it } from 'vitest';

import { DeviceEnrollment, Identity, MeshNode, OperatorEnrollment } from '../src/index';

const PSK = '5d'.repeat(32);
const tmp = mkdtempSync(join(tmpdir(), 'net-sdk-mesh-live-'));
let seq = 0;
afterAll(() => rmSync(tmp, { recursive: true, force: true }));

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

// nRPC reply channels are dynamic per caller, which enrollment's strict-mode
// registry rejects; the native docs require permissiveChannels for join /
// renew / serveEnrollmentAuto.
const node = () =>
  MeshNode.create({ bindAddr: '127.0.0.1:0', psk: PSK, permissiveChannels: true });

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

describe('MeshNode S5 methods, live', () => {
  it('NAT state: unknown before classification, open under a reflex override', async () => {
    const n = await node();
    try {
      expect(n.natType()).toBe('unknown');
      expect(n.reflexAddr()).toBeNull();

      n.setReflexOverride('203.0.113.7:4000');
      expect(n.natType()).toBe('open');
      expect(n.reflexAddr()).toBe('203.0.113.7:4000');

      n.clearReflexOverride();
      expect(n.reflexAddr()).toBeNull();

      expect(() => n.setReflexOverride('not an address')).toThrow(/invalid reflex override/);
      const stats = n.traversalStats();
      expect(stats).toBeTypeOf('object');
      expect(n.discoveredNodes()).toBeTypeOf('number');
    } finally {
      await n.shutdown();
    }
  });

  it('serveA2a / submitTask / taskStatus complete a task across two nodes', async () => {
    const [requester, executor] = await connectedPair();
    const handle = await executor.serveA2a(async () => 'blob://sdk-result');
    try {
      const execId = executor.nodeId();
      // The first call can lose its reply while the fresh pair settles.
      let taskId: string | undefined;
      let lastErr: unknown;
      for (let i = 0; i < 5 && taskId === undefined; i++) {
        try {
          taskId = await requester.submitTask(execId, 'summarize', []);
        } catch (e) {
          lastErr = e;
          await sleep(100);
        }
      }
      if (taskId === undefined) throw lastErr;

      const deadline = Date.now() + 6000;
      let rec: { state: { state: string; result_ref?: string } } | undefined;
      while (Date.now() < deadline) {
        const raw = await requester.taskStatus(execId, taskId);
        rec = raw === null ? undefined : JSON.parse(raw);
        if (rec?.state.state === 'completed') break;
        await sleep(50);
      }
      expect(rec?.state.state).toBe('completed');
      expect(rec?.state.result_ref).toBe('blob://sdk-result');
      // Finished: a cancel reports nothing in flight.
      expect(await requester.cancelTask(execId, taskId)).toBe(false);
    } finally {
      handle.stop();
      await requester.shutdown();
      await executor.shutdown();
    }
  }, 25_000);

  it('placement filters register, report and unregister', async () => {
    const n = await node();
    try {
      const id = 'sdk-live-filter';
      expect(n.hasPlacementFilter(id)).toBe(false);
      expect(n.registerPlacementFilter(id, (c) => c.tags.includes('gpu'))).toBe(true);
      expect(n.hasPlacementFilter(id)).toBe(true);
      // A duplicate id is refused rather than replacing the predicate.
      expect(n.registerPlacementFilter(id, () => true)).toBe(false);
      expect(n.unregisterPlacementFilter(id)).toBe(true);
      expect(n.hasPlacementFilter(id)).toBe(false);
      expect(n.unregisterPlacementFilter(id)).toBe(false);
    } finally {
      await n.shutdown();
    }
  });

  it('a device enrolls over the mesh, then renews its grant', async () => {
    const root = Identity.generate();
    const dir = join(tmp, `op-${seq++}`);
    const op = new OperatorEnrollment(
      root.toNapi(),
      join(dir, 'devices.json'),
      join(dir, 'revocations.json'),
    );
    const opNode = await node();
    const devNode = await node();
    try {
      await opNode.start();
      const handle = await opNode.serveEnrollmentAuto(op, 3600);
      expect(handle.serving).toBe(true);

      const rendezvous = opNode.rendezvousString();
      const invite = op.invite(rendezvous, 300);

      const device = Identity.generate().toNapi();
      await devNode.start();
      const chain = await devNode.join(device, invite.encode(), 'pc', ['region:office']);
      expect(chain.leaf.equals(device.entityId)).toBe(true);
      expect(chain.root.equals(root.toNapi().entityId)).toBe(true);

      const devices = await op.devices();
      expect(devices.map((d) => d.name)).toEqual(['pc']);

      const enrollment = new DeviceEnrollment(
        device,
        chain,
        rendezvous,
        BigInt(Math.floor(Date.now() / 1000)),
      );
      const renewed = await devNode.renew(enrollment);
      expect(renewed.leaf.equals(device.entityId)).toBe(true);
      expect(renewed.root.equals(root.toNapi().entityId)).toBe(true);

      handle.stop();
      expect(handle.serving).toBe(false);
    } finally {
      await devNode.shutdown();
      await opNode.shutdown();
    }
  }, 30_000);
});
