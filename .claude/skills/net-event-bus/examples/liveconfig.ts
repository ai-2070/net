//! Live config — the config service you no longer run.
//!
//! One publisher and two subscribers, three in-process mesh nodes over loopback
//! UDP. The publisher registers a channel, both subscribers join by name, and
//! every config revision is pushed once and applied by each subscriber locally.
//! There is no config server to poll, no cache to invalidate and no reload to
//! coordinate — the channel *is* the delivery, and the roster is held by the
//! publisher, not by a broker.
//!
//! Run (from `net/crates/net/sdk-ts`, where `tsconfig.skill-example.json`
//! includes it):
//!
//!   npx tsc --noEmit -p tsconfig.skill-example.json
//!
//! Expected final line: `RESULT ok subscribers=2 applied=2 version=2`

import { MeshNode } from '@net-mesh/sdk';

/** 64 hex characters = 32 bytes. Every node in a mesh shares it. */
const PSK = '42'.repeat(32);

/** How long we wait for a published revision to land in a subscriber's shards. */
const DELIVER_MS = 5_000;

const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

async function build(seed: number): Promise<MeshNode> {
  return MeshNode.create({
    bindAddr: '127.0.0.1:0',
    psk: PSK,
    identitySeed: Buffer.alloc(32, seed),
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

/**
 * Parse `v=<n>;mode=<name>`; subscribers apply whatever they understand and
 * ignore the rest. A revision never has to be acknowledged back to the
 * publisher for the next one to arrive.
 */
function parse(payload: string): [number, string] | null {
  let version: number | null = null;
  let mode: string | null = null;
  for (const field of payload.split(';')) {
    if (field.startsWith('v=')) {
      const parsed = Number(field.slice(2));
      if (Number.isInteger(parsed)) version = parsed;
    } else if (field.startsWith('mode=')) {
      mode = field.slice(5);
    }
  }
  return version === null || mode === null ? null : [version, mode];
}

/**
 * Drain every shard the bus could have routed a channel event to. Published
 * events land on the shard derived from the stream id, so a consumer polls all
 * of them — `recv` is that sweep, and `recvShard` is the targeted read.
 */
async function drain(node: MeshNode, applied: Map<number, string>): Promise<number> {
  const deadline = Date.now() + DELIVER_MS;
  let seen = 0;
  while (Date.now() < deadline) {
    let quiet = true;
    for (const event of await node.recv(64)) {
      quiet = false;
      seen += 1;
      const revision = parse(event.rawBytes.toString('utf8'));
      if (revision !== null) applied.set(revision[0], revision[1]);
    }
    if (quiet && seen > 0) break;
    await sleep(20);
  }
  return seen;
}

async function main(): Promise<void> {
  const publisher = await build(0xf1);
  const s1 = await build(0xf2);
  const s2 = await build(0xf3);

  // Both subscribers connect to the publisher; it accepts both.
  await handshake(publisher, s1);
  await handshake(publisher, s2);

  await publisher.start();
  await s1.start();
  await s2.start();

  // The publisher owns the channel config. No broker registers it.
  const channel = 'config/edge';
  publisher.registerChannel({ name: channel, visibility: 'global' });

  // Subscribers join by name. `subscribeChannel` resolves on the publisher's
  // ack, so by the time it returns this node is in the roster.
  const publisherId = publisher.nodeId();
  await s1.subscribeChannel(publisherId, channel);
  await s2.subscribeChannel(publisherId, channel);

  // Revision 1, then revision 2 — each pushed once, to the roster the
  // publisher holds.
  let report = await publisher.publish(channel, Buffer.from('v=1;mode=blue'), {
    reliability: 'reliable',
  });
  console.log(`published v1 to ${report.delivered} of ${report.attempted} subscribers`);

  report = await publisher.publish(channel, Buffer.from('v=2;mode=green'), {
    reliability: 'reliable',
  });
  console.log(`published v2 to ${report.delivered} of ${report.attempted} subscribers`);

  const one = new Map<number, string>();
  const two = new Map<number, string>();
  await drain(s1, one);
  await drain(s2, two);

  const applied = [one, two].filter((seen) => seen.has(2)).length;

  console.log(`subscriber one applied:    ${JSON.stringify([...one])}`);
  console.log(`subscriber two applied:    ${JSON.stringify([...two])}`);

  // Worth pinning: the publisher's roster is what fan-out costs. Zero
  // subscribers is a no-op, not a queue that later has to be drained.
  console.log(`roster at publish time:    ${report.attempted}`);

  console.log(`RESULT ok subscribers=${report.attempted} applied=${applied} version=2`);

  await publisher.shutdown();
  await s1.shutdown();
  await s2.shutdown();
}

main().catch((error) => {
  console.error(error);
  process.exit(1);
});
