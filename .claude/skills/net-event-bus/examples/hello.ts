// Minimal sanity check for @net-mesh/sdk.
// Run: npx tsx hello.ts
//
// What it proves: the SDK installs, a node starts, you can publish an event
// to a named channel, a subscriber receives it, and shutdown is clean.

import { NetNode } from '@net-mesh/sdk';

interface Hello { msg: string }

async function main() {
  const node = await NetNode.create({ shards: 1 });
  const ch = node.channel<Hello>('hello/world');

  // Start the subscriber FIRST. Subscriptions are hot — late subscribers miss events.
  const stream = ch.subscribe();
  const received = (async () => {
    for await (const ev of stream) return ev;
  })();

  // Small tick so the iterator's first poll lands before we publish.
  await new Promise(r => setTimeout(r, 10));

  ch.publish({ msg: 'hello, mesh' });

  const ev = await received;
  console.log('received:', ev);

  await node.shutdown();
}

main().catch(e => { console.error(e); process.exit(1); });
