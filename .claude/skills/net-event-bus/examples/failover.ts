// The sidecar you no longer run: call a service, not a host.
//
// Two providers serve the same service name. A caller addresses the **service**
// — never a node id — and when the provider that answered first goes away, the
// next call lands on the survivor, with a bounded retry absorbing the transient
// while the dead provider is still in the roster.
//
// Rust has a retry helper (`call_service_typed_with_retry`) that does this for
// you; the SDK has no equivalent, so the loop below is written by hand. That is
// the only difference between this file and its Rust sibling.
//
// Run (from `net/crates/net/sdk-ts`, where `tsconfig.skill-example.json`
// includes it):
//
//   npx tsc --noEmit -p tsconfig.skill-example.json
//
// Expected final line: `RESULT ok providers=2 moved=1 served=2`

import { MeshNode } from '@net-mesh/sdk';
import type { TypedMeshRpc } from '@net-mesh/core/mesh_rpc';

/** 64 hex characters = 32 bytes. Every node in a mesh shares it. */
const PSK = '42'.repeat(32);

/** The roster still lists a dead provider until the fold converges. */
const RETRY_ATTEMPTS = 6;
const RETRY_INTERVAL_MS = 250;
const CALL_DEADLINE_MS = 500;

const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

interface Work {
  units: number;
}
interface Answer {
  served_by: string;
  units: number;
}

async function build(seed: number): Promise<MeshNode> {
  return MeshNode.create({
    bindAddr: '127.0.0.1:0',
    psk: PSK,
    identitySeed: Buffer.alloc(32, seed),
    heartbeatIntervalMs: 200,
  });
}

async function handshake(responder: MeshNode, initiator: MeshNode): Promise<void> {
  const addr = responder.localAddr();
  const pub = responder.publicKey();
  const responderId = responder.nodeId();
  await Promise.all([
    responder.accept(initiator.nodeId()),
    (async () => {
      await sleep(50);
      await initiator.connect(addr, pub, responderId);
    })(),
  ]);
}

async function main(): Promise<void> {
  const caller = await build(0xc1);
  const providerOne = await build(0xc2);
  const providerTwo = await build(0xc3);

  await handshake(caller, providerOne);
  await handshake(caller, providerTwo);
  await caller.start();
  await providerOne.start();
  await providerTwo.start();

  // Two servers, one service name. `serve` advertises the service for each,
  // which is the whole registration.
  const oneId = `0x${providerOne.nodeId().toString(16)}`;
  const twoId = `0x${providerTwo.nodeId().toString(16)}`;
  const rpcOne = providerOne.rpc();
  const rpcTwo = providerTwo.rpc();
  const callerRpc = caller.rpc();

  const serveOne = rpcOne.serve<Work, Answer>('work', (req) => ({
    served_by: oneId,
    units: req.units,
  }));
  const serveTwo = rpcTwo.serve<Work, Answer>('work', (req) => ({
    served_by: twoId,
    units: req.units,
  }));

  // The caller discovers the service by name. It learns *who can serve it*,
  // never whom to prefer — that is what makes the next part work.
  const deadline = Date.now() + 5_000;
  while (Date.now() < deadline) {
    if (callerRpc.findServiceNodes('work').length === 2) break;
    await sleep(25);
  }
  const providers = callerRpc.findServiceNodes('work').length;
  console.log(`providers advertising \`work\`: ${providers}`);

  // Call the service, not a host.
  const first = await callerRpc.callService<Work, Answer>(
    'work',
    { units: 2 },
    { deadlineMs: 1_000 },
  );
  console.log(`first call served by:  ${first.served_by}`);

  // Take that provider out of the mesh entirely — the hard version of a
  // deploy: not a drain, a death. Its RPC handles must be closed first: an
  // outstanding handle makes shutdown fail with "outstanding references exist".
  const doomedIsOne = first.served_by === oneId;
  const doomed = doomedIsOne ? providerOne : providerTwo;
  const doomedServe = doomedIsOne ? serveOne : serveTwo;
  const survivorServe = doomedIsOne ? serveTwo : serveOne;
  const doomedRpc = doomedIsOne ? rpcOne : rpcTwo;
  const survivorRpc = doomedIsOne ? rpcTwo : rpcOne;

  await doomedServe.close();
  await doomedRpc.raw.close();
  await doomed.shutdown();
  console.log(`took ${first.served_by} out`);

  let second: Answer | null = null;
  let lastError: unknown = null;
  for (let attempt = 0; attempt < RETRY_ATTEMPTS && second === null; attempt += 1) {
    try {
      second = await callerRpc.callService<Work, Answer>(
        'work',
        { units: 5 },
        { deadlineMs: CALL_DEADLINE_MS },
      );
    } catch (error) {
      lastError = error;
      await sleep(RETRY_INTERVAL_MS);
    }
  }
  if (second === null) throw lastError ?? new Error('no provider answered after the death');
  console.log(`after the death served by: ${second.served_by}`);

  const moved = second.served_by !== first.served_by ? 1 : 0;
  const served = (first.units === 2 ? 1 : 0) + (second.units === 5 ? 1 : 0);
  console.log(`RESULT ok providers=${providers} moved=${moved} served=${served}`);

  // The survivor's handles, then the caller's.
  await survivorServe.close();
  await survivorRpc.raw.close();
  await callerRpc.raw.close();
  await caller.shutdown();
}

main().catch((error) => {
  console.error(error);
  process.exit(1);
});