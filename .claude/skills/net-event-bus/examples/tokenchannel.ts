// Scoped credentials, not an open channel — TypeScript port of `tokenchannel.rs`.
//
// Two nodes. The publisher owns a channel whose subscriber ACL is rooted at
// its own entity id; a subscriber holding a token minted for *its* entity id,
// for *this* channel, with the subscribe scope is admitted. The same
// subscriber asking without the token is refused.
//
// Run (from a crate whose root resolves the `@net-mesh/sdk` import):
//
//   npx tsx tokenchannel.ts
//
// Expected final line: `RESULT ok granted=1 refused=1`

import { Identity, MeshNode } from '@net-mesh/sdk';
import type { TokenScope } from '@net-mesh/sdk';

/** 32 bytes exactly — a PSK, not a passphrase. Every node in a mesh shares it.
 *  Hex-encoded (64 hex chars) is the SDK's wire form. */
const PSK = '42'.repeat(32);

/** The token's lifetime. Long enough for the example, short enough that a
 *  minted credential is never a standing secret. */
const TOKEN_TTL_SECONDS = 300;

/** A single subscribe-scoped grant. */
const SCOPE: TokenScope = 'subscribe';

function delay(ms: number): Promise<void> {
  const { promise, resolve } = Promise.withResolvers<void>();
  setTimeout(resolve, ms);
  return promise;
}

async function build(
  seedByte: number,
): Promise<{ node: MeshNode; identity: Identity }> {
  const seed = Buffer.alloc(32, seedByte);
  const identity = Identity.fromSeed(seed);
  const node = await MeshNode.create({
    bindAddr: '127.0.0.1:0',
    psk: PSK,
    identitySeed: seed,
  });
  return { node, identity };
}

async function handshake(
  responder: MeshNode,
  initiator: MeshNode,
  responderAddr: string,
): Promise<void> {
  const responderPub = responder.publicKey();
  const responderId = responder.nodeId();
  const initiatorId = initiator.nodeId();
  await Promise.all([
    responder.accept(initiatorId),
    (async () => {
      await delay(50);
      await initiator.connect(responderAddr, responderPub, responderId);
    })(),
  ]);
}

async function main(): Promise<void> {
  const channel = 'config/gated';

  const { node: publisher, identity: publisherIdentity } = await build(0xe1);
  const { node: subscriber, identity: subscriberIdentity } = await build(0xe2);

  const publisherAddr = publisher.localAddr();
  await handshake(publisher, subscriber, publisherAddr);
  await publisher.start();
  await subscriber.start();

  // Both nodes announce before anything is gated. A token's leaf binds to
  // the subscribing peer's EntityId, and the publisher only learns that
  // EntityId from a signature-verified announcement — so a subscriber that
  // has announced nothing is unauthorized no matter what it presents.
  await publisher.announceCapabilities({});
  await subscriber.announceCapabilities({});

  // Wait for the publisher to index it; the announcement is what populates
  // the publisher's peer-entity map.
  const subscriberNodeId = subscriber.nodeId();
  const deadline = Date.now() + 2_000;
  while (Date.now() < deadline) {
    if (publisher.findNodes({}).includes(subscriberNodeId)) {
      break;
    }
    await delay(25);
  }

  // The channel's subscriber ACL is rooted at the publisher's own entity id.
  // `tokenRoots` is what turns token enforcement on, so this is one
  // declaration rather than a flag plus a trust anchor that can disagree.
  publisher.registerChannel({
    name: channel,
    visibility: 'global',
    tokenRoots: [publisherIdentity.entityId],
  });
  console.log(
    `channel gated on a token rooted at 0x${publisherIdentity.originHash.toString(16)}`,
  );

  // A credential scoped three ways: to this subscriber's entity id, to this
  // channel, and to the subscribe action alone. It cannot publish, and it is
  // useless to any other node.
  const token = publisherIdentity.issueToken({
    subject: subscriberIdentity.entityId,
    scope: [SCOPE],
    channel,
    ttlSeconds: TOKEN_TTL_SECONDS,
  });
  console.log('issued a subscribe-only token to the subscriber');

  // Without it: refused. The publisher answers and says no, which is a
  // different outcome from "the publisher never answered".
  const refused = await subscriber
    .subscribeChannel(publisher.nodeId(), channel)
    .then(() => false)
    .catch(() => true);
  console.log(`bare subscribe refused:            ${refused}`);

  // With it: admitted. The credential is presented on the subscribe request
  // itself, not negotiated once per connection.
  const granted = await subscriber
    .subscribeChannel(publisher.nodeId(), channel, { token })
    .then(() => true)
    .catch(() => false);
  console.log(`token-carrying subscribe admitted: ${granted}`);

  console.log(
    `RESULT ok granted=${Number(granted)} refused=${Number(refused)}`,
  );

  await publisher.shutdown();
  await subscriber.shutdown();
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
