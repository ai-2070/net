// Minimal sanity check for @net-mesh/sdk (Node / TypeScript).
//
// Run: npx tsx hello.ts
//
// What it proves: the package installs, a node starts, a typed event is
// accepted, and shutdown is clean.
//
// WHAT IT DELIBERATELY DOES NOT DO: receive the event back.
//
// The default transport is memory, which selects the **Noop adapter**. Events
// flow producer -> ring buffer -> drain worker -> adapter, and the Noop adapter
// counts batches and discards them (`adapter/noop.rs`: "Just count, don't
// store"; its `poll_shard` returns an empty result). So on memory transport
// `subscribe()` never yields — a round-trip example here would hang, not fail.
//
// To actually receive events you need an adapter that retains them: Redis or
// JetStream, or the mesh transport between two nodes. See `mesh.md`.

import { NetNode } from '@net-mesh/sdk';

interface Hello {
  msg: string;
}

async function main(): Promise<void> {
  const node = await NetNode.create({ shards: 1 });

  const ch = node.channel<Hello>('hello/world');
  // `publish` goes through the fire path: it returns a boolean and never
  // throws. `false` means the bus rejected the event.
  const accepted = ch.publish({ msg: 'hello, mesh' });
  if (!accepted) throw new Error('the bus did not accept the event');

  // Counters are bigint, and count at the *producer* boundary — they say the
  // bus accepted the event, not that anything received or stored it.
  const stats = node.stats();
  console.log(`accepted: ingested=${stats.eventsIngested} dropped=${stats.eventsDropped}`);

  await node.shutdown();
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
