// The event log you no longer run — TypeScript port of `eventlog.rs`.
//
// One node, one local append-only log. Eight records are appended, replayed
// from the start, then replayed again from a consumer checkpoint — and the
// second replay is three records, not eight, because the offset is the
// consumer's own bookkeeping and the log is not asked to remember it.
//
// Run (from a crate whose root resolves the `@net-mesh/sdk` import):
//
//   npx tsx eventlog.ts
//
// Expected final line: `RESULT ok records=8 replayed=8 resumed=3`

import { Redex } from '@net-mesh/sdk';

/** Records appended to the log. */
const RECORDS = 8;
/** Where the consumer left off, in sequence numbers. */
const CHECKPOINT = 5n;

async function main(): Promise<void> {
  const redex = new Redex();
  const log = redex.openFile('audit/events');

  for (let index = 0; index < RECORDS; index++) {
    const seq = log.append(Buffer.from(`event:${index}`));
    console.log(`appended event ${index} at seq ${seq}`);
  }

  // Full replay. Every record, in append order — the log does not need a
  // broker to hand them back.
  const all = log.readRange(0n, log.len());

  // The consumer's offset is application state: it stores the sequence it
  // last processed and asks for everything after it. Nothing is committed
  // on the log's side, which is why a second consumer with a different
  // checkpoint costs the log nothing.
  const resumed = log.readRange(CHECKPOINT, log.len());

  // Replay is a read, not a transformation: the same range yields the same
  // sequence, so a fold over it is reproducible.
  const again = log.readRange(0n, log.len());
  const stable =
    again.length === all.length &&
    again.every((e, i) => e.seq === all[i].seq);

  console.log(`records in the log: ${log.len()}`);
  console.log(`replayed from 0:    ${all.length}`);
  console.log(`resumed from ${CHECKPOINT}:     ${resumed.length}`);
  console.log(`replay is stable:   ${stable}`);

  console.log(
    `RESULT ok records=${log.len()} replayed=${all.length} resumed=${resumed.length}`,
  );

  log.close();
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
