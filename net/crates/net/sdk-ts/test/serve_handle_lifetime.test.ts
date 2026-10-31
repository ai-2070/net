// A serve handle that isn't kept referenced no longer stops its service
// (APP_INTEGRATION_BLOCKERS_PLAN.md Phase 4). The node owns the
// registration until `close()` / `stop()` or `shutdown()`. Before, the
// service silently disappeared at whatever GC finalized the handle.
//
// Each test proves the handle really was collected (a FinalizationRegistry
// callback fired) before checking the service, so a pass can't come from
// a handle that merely survived.

import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import v8 from 'node:v8';
import vm from 'node:vm';

import { afterAll, describe, expect, it } from 'vitest';

import { DeviceEnrollment, Identity, MeshNode, OperatorEnrollment } from '../src/index';

v8.setFlagsFromString('--expose-gc');
const gc = vm.runInNewContext('gc') as () => void;

const PSK = '7c'.repeat(32);
const tmp = mkdtempSync(join(tmpdir(), 'net-sdk-serve-lifetime-'));
afterAll(() => rmSync(tmp, { recursive: true, force: true }));

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

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

/** Collect until every object registered with `registry` is finalized. */
async function collect(finalized: () => boolean): Promise<void> {
  for (let i = 0; i < 50 && !finalized(); i++) {
    gc();
    await sleep(20);
  }
  expect(finalized(), 'the dropped handle was never finalized').toBe(true);
}

describe('serve handles are node-owned', () => {
  it('an unreferenced rpc.serve handle keeps serving after GC', async () => {
    const [caller, server] = await connectedPair();
    const callerRpc = caller.rpc();
    const serverRpc = server.rpc();
    let finalized = false;
    const registry = new FinalizationRegistry(() => {
      finalized = true;
    });
    try {
      // Serve inside a scope that drops the handle.
      (() => {
        const handle = serverRpc.serve<{ n: number }, { n: number }>('bs.echo', async (req) => req);
        registry.register(handle, 'serve');
      })();
      await collect(() => finalized);

      const reply = await callerRpc.call<{ n: number }, { n: number }>(
        server.nodeId(),
        'bs.echo',
        { n: 7 },
        { deadlineMs: 5000 },
      );
      expect(reply).toEqual({ n: 7 });
    } finally {
      callerRpc.raw.close();
      serverRpc.raw.close();
      // Shutdown releases the service it still holds; no close() needed.
      await caller.shutdown();
      await server.shutdown();
    }
  }, 30_000);

  it('close() still stops the service', async () => {
    const [caller, server] = await connectedPair();
    const callerRpc = caller.rpc();
    const serverRpc = server.rpc();
    try {
      const handle = serverRpc.serve<{ n: number }, { n: number }>('bs.echo', async (req) => req);
      expect(
        await callerRpc.call(server.nodeId(), 'bs.echo', { n: 1 }, { deadlineMs: 5000 }),
      ).toEqual({ n: 1 });
      handle.close();
      await expect(
        callerRpc.call(server.nodeId(), 'bs.echo', { n: 2 }, { deadlineMs: 2000 }),
      ).rejects.toThrow(/no nRPC service|not served|no_route|timeout/);
    } finally {
      callerRpc.raw.close();
      serverRpc.raw.close();
      await caller.shutdown();
      await server.shutdown();
    }
  }, 30_000);

  it('an unreferenced serveEnrollmentAuto handle keeps enrollment up after GC', async () => {
    const root = Identity.generate();
    const op = new OperatorEnrollment(
      root.toNapi(),
      join(tmp, 'devices.json'),
      join(tmp, 'revocations.json'),
    );
    const opNode = await node();
    const devNode = await node();
    let finalized = false;
    const registry = new FinalizationRegistry(() => {
      finalized = true;
    });
    try {
      await opNode.start();
      await devNode.start();
      registry.register(await opNode.serveEnrollmentAuto(op, 3600), 'enrollment');
      await collect(() => finalized);

      const rendezvous = opNode.rendezvousString();
      const device = Identity.generate().toNapi();
      const chain = await devNode.join(
        device,
        op.invite(rendezvous, 300).encode(),
        'pc',
        [],
      );
      const renewed = await devNode.renew(
        new DeviceEnrollment(device, chain, rendezvous, BigInt(Math.floor(Date.now() / 1000))),
      );
      expect(renewed.leaf.equals(device.entityId)).toBe(true);
    } finally {
      await devNode.shutdown();
      await opNode.shutdown();
    }
  }, 30_000);
});
