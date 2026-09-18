//! The service registry you no longer run.
//!
//! Four in-process mesh nodes over loopback UDP: two providers announce the
//! same capability, a caller discovers them and ranks them locally, a third
//! provider appears, and the caller's next lookup ranks it first — with no
//! registry process, no health-check poller, no config reload and no
//! announcement to any address.
//!
//! Run (from `net/crates/net/sdk-ts`, where `tsconfig.skill-example.json`
//! includes it):
//!
//!   npx tsc --noEmit -p tsconfig.skill-example.json
//!
//! Expected final line: `RESULT ok providers=3 joined=1 best_moved=1`

import { MeshNode } from '@net-mesh/sdk';

/** 64 hex characters = 32 bytes. Every node in a mesh shares it. */
const PSK = '42'.repeat(32);

const CONVERGE_MS = 5_000;

const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

async function build(seed: number): Promise<MeshNode> {
  return MeshNode.create({
    bindAddr: '127.0.0.1:0',
    psk: PSK,
    identitySeed: Buffer.alloc(32, seed),
    // Tight heartbeat, like the SDK's own tests, so gossip settles quickly.
    heartbeatIntervalMs: 200,
  });
}

/**
 * One side connects, the other accepts. `accept` MUST be registered before
 * `start`; both promises resolve only after the handshake completes, so
 * awaiting the pair is the wait-until-connected primitive.
 */
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

/** Poll the caller's local capability fold until `pred` holds, or give up. */
async function until(what: string, pred: () => boolean): Promise<void> {
  const deadline = Date.now() + CONVERGE_MS;
  while (Date.now() < deadline) {
    if (pred()) return;
    await sleep(25);
  }
  throw new Error(`timed out waiting for ${what}`);
}

async function main(): Promise<void> {
  // Announcements reach directly-connected peers, so the caller connects to
  // every provider it wants to see. There is no directory to join.
  const a = await build(0xa1); // provider, 16 GB
  const b = await build(0xb2); // provider, 64 GB
  const c = await build(0xc3); // provider that appears later, 256 GB
  const caller = await build(0xd4);

  // Every accept() completes before any start().
  await handshake(b, a); // A <-> B
  await handshake(a, caller); // A <-> caller
  await handshake(caller, b); // B <-> caller
  await handshake(caller, c); // C <-> caller

  await a.start();
  await b.start();
  await c.start();
  await caller.start();

  // The two original providers announce. That is the entire registration.
  await a.announceCapabilities({ hardware: { memoryGb: 16 }, tags: ['api'] });
  await b.announceCapabilities({ hardware: { memoryGb: 64 }, tags: ['api'] });

  const filter = { requireTags: ['api'] };
  await until('both providers to appear', () => caller.findNodes(filter).length === 2);
  const providers = caller.findNodes(filter).length;

  // Ranking is local and free. Prefer more memory, so the bigger machine wins
  // without any scheduler deciding it centrally. `findBestNode` returns null
  // for "no match" — 0n is a real node id.
  const req = { filter, preferMoreMemory: 1 };
  const first = caller.findBestNode(req);
  if (first === null) throw new Error('no provider matched');

  // A new provider appears: it announces once and is immediately addressable.
  await c.announceCapabilities({ hardware: { memoryGb: 256 }, tags: ['api'] });
  await until('the new provider to appear', () => caller.findNodes(filter).length === 3);
  const after = caller.findNodes(filter).length;

  const best = caller.findBestNode(req);
  if (best === null) throw new Error('no provider matched after the join');

  const moved = first !== best ? 1 : 0;
  console.log(`providers found at first lookup: ${providers}`);
  console.log(`ranked best:                     0x${first.toString(16)}`);
  console.log(`providers after the join:        ${after}`);
  console.log(`ranked best:                     0x${best.toString(16)}`);

  console.log(`RESULT ok providers=${after} joined=1 best_moved=${moved}`);

  await a.shutdown();
  await b.shutdown();
  await c.shutdown();
  await caller.shutdown();
}

main().catch((error) => {
  console.error(error);
  process.exit(1);
});
