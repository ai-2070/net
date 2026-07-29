// What the drop counters actually tell you (Node / TypeScript).
//
// Run: npx tsx observe.ts
//
// THE POINT OF THIS FILE, AND IT IS NOT WHAT YOU EXPECT.
//
// The usual advice — "backpressure is silent, so watch eventsDropped" — is
// wrong for the one mode where it matters most, and that mode is the default.
//
// Both counters sit at the **producer boundary**: they record what the bus
// accepted or refused from you, not what survived to an adapter.
//
//   - drop_oldest (the default) evicts to make room, so publish always
//     succeeds and eventsDropped never moves. Events are lost and nothing
//     in-process says so.
//   - drop_newest and fail_producer refuse the producer instead, so the
//     publish call and eventsDropped both see it.
//
// The exact counts vary between runs and between bindings — the drain worker
// races the producer. The shape is what matters, not the numbers.
//
// TWO TYPESCRIPT-SPECIFIC TRAPS ALONG THE WAY:
//
//   - Counters are `bigint`, not `number`. Compare against `0n`;
//     `stats.eventsDropped + 1` is a TypeError and `JSON.stringify(stats)`
//     throws.
//   - There is no batch counter here. Rust, Go and C expose one; this binding
//     does not.

import { NetNode } from '@net-mesh/sdk';

interface Tick {
  seq: number;
}

type Mode = 'drop_oldest' | 'drop_newest' | 'fail_producer';

async function measure(mode: Mode): Promise<void> {
  // bufferCapacity must be a power of two and >= 1024 — both enforced at
  // construction, and neither is caught by the type checker.
  const node = await NetNode.create({ shards: 1, bufferCapacity: 1024, backpressure: mode });
  const ticks = node.channel<Tick>('ticks');

  // `publish` goes through the fire path: boolean, never throws.
  let rejected = 0;
  for (let seq = 0; seq < 20000; seq++) {
    if (!ticks.publish({ seq })) rejected++;
  }

  const stats = node.stats();
  console.log(
    `${mode.padEnd(14)} ingested=${stats.eventsIngested} dropped=${stats.eventsDropped} rejected=${rejected}`,
  );

  await node.shutdown();
}

async function main(): Promise<void> {
  console.log('20,000 events into a 1024-slot buffer, nothing consuming:\n');
  await measure('drop_oldest');
  await measure('drop_newest');
  await measure('fail_producer');
  console.log('\ndrop_oldest loses events silently — and it is the default.');
  console.log('The other two refuse the producer, so publish() and the counter agree.');
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
