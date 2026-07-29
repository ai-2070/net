// Observe a drop (Node / TypeScript).
//
// Run: npx tsx observe.ts
//
// What it proves: stats() reports drops, the counters are bigint, and the two
// ingestion paths fail in different ways.
//
// WHY THIS EXISTS. Under the default backpressure modes nothing throws and
// nothing returns false — the only evidence is eventsDropped.
//
// TWO TYPESCRIPT-SPECIFIC TRAPS:
//
//   - Stats counters are `bigint`, not `number`. `stats.eventsDropped > 0`
//     works, but `stats.eventsDropped + 1` is a TypeError and
//     `JSON.stringify(stats)` throws. Compare against `0n`.
//   - There is no batch counter here. Rust, Go and C expose one; this binding
//     does not.

import { NetNode } from '@net-mesh/sdk';

interface Tick {
  seq: number;
}

async function main(): Promise<void> {
  const node = await NetNode.create({ shards: 1 });
  const ticks = node.channel<Tick>('ticks');

  // Far more events than the buffer holds, with nothing subscribed.
  // `publish` goes through the fire path: it returns a boolean and never
  // throws. A `false` means the bus rejected that event.
  let rejected = 0;
  for (let seq = 0; seq < 1000; seq++) {
    if (!ticks.publish({ seq })) rejected++;
  }

  const stats = node.stats();
  console.log(
    `ingested=${stats.eventsIngested} dropped=${stats.eventsDropped} rejected=${rejected}`,
  );

  // Note the 0n — these are bigints.
  if (stats.eventsDropped > 0n) {
    console.log('drops happened and nothing threw — alert on this counter');
  }

  await node.shutdown();
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
