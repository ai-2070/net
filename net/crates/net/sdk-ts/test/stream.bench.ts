/**
 * Microbenchmark for `EventStream` polling on a synthetic trickle
 * workload. Drives `EventStream` against a mock bus that returns 1-2
 * events per poll with a small simulated FFI cost — the exact
 * pathology the partial-batch sleep is meant to address.
 *
 * Run with:
 *   npx tsx test/stream.bench.ts
 *
 * Reports for a fixed wall-clock window:
 *   - poll count           (lower is better at fixed event volume)
 *   - events delivered     (should be unchanged)
 *   - polls per event      (lower is better)
 *   - user CPU time        (lower is better)
 *   - p50 / p99 yield-to-yield latency (should not regress)
 *
 * To compare before/after the partial-batch sleep landed: run on this
 * branch, then `git stash` the stream.ts edit, rerun, and compare.
 *
 * No NAPI binary required — the mock bus is hand-rolled.
 */

import { EventStream } from '../src/stream';
import type { StoredEvent, SubscribeOpts } from '../src/types';

// ---------------------------------------------------------------------------
// Mock bus that simulates a trickle workload: every `pollDelayUs` of
// simulated bus time, one new event becomes available. Each poll() pays
// `ffiCostUs` of synthetic await before returning whatever has accumulated.
// ---------------------------------------------------------------------------

interface PollResponse {
  events: StoredEvent[];
  nextId?: string | null;
}

class TrickleBus {
  private nextSeq = 0;
  private accumulator: StoredEvent[] = [];
  private lastDrain = process.hrtime.bigint();
  pollCount = 0;

  constructor(
    private trickleRateHz: number,
    private ffiCostUs: number,
  ) {}

  async poll(opts: { limit?: number }): Promise<PollResponse> {
    this.pollCount++;

    // Simulate FFI roundtrip cost.
    if (this.ffiCostUs > 0) {
      await new Promise<void>((resolve) =>
        setTimeout(resolve, this.ffiCostUs / 1000),
      );
    }

    // Accumulate events at `trickleRateHz` based on wall-clock delta
    // since the last drain.
    const now = process.hrtime.bigint();
    const elapsedNs = Number(now - this.lastDrain);
    const eventsToAdd = Math.floor(
      (elapsedNs / 1_000_000_000) * this.trickleRateHz,
    );
    for (let i = 0; i < eventsToAdd; i++) {
      this.accumulator.push({
        id: String(this.nextSeq++),
        raw: '{"x":1}',
        insertionTs: BigInt(Date.now()),
        shardId: 0,
      });
    }
    this.lastDrain = now;

    // Drain up to the requested limit.
    const limit = opts.limit ?? 100;
    const events = this.accumulator.splice(0, limit);
    return {
      events,
      nextId: events.length > 0 ? events[events.length - 1].id : null,
    };
  }
}

// ---------------------------------------------------------------------------
// Bench harness.
// ---------------------------------------------------------------------------

interface BenchResult {
  label: string;
  durationMs: number;
  events: number;
  polls: number;
  pollsPerEvent: number;
  userCpuMs: number;
  p50YieldLatencyUs: number;
  p99YieldLatencyUs: number;
}

async function runBench(
  label: string,
  durationMs: number,
  bus: TrickleBus,
  opts: SubscribeOpts,
): Promise<BenchResult> {
  // Duck-typed: `EventStream` only calls `bus.poll(opts)` on its bus;
  // the rest of the `NapiNet` surface is irrelevant here.
  const stream = new EventStream(bus as unknown as never, opts);

  const cpuStart = process.cpuUsage();
  const wallStart = Date.now();
  let events = 0;
  const yieldDeltas: number[] = [];
  let lastYield = process.hrtime.bigint();

  const stop = setTimeout(() => stream.stop(), durationMs);

  for await (const _ev of stream) {
    events++;
    const now = process.hrtime.bigint();
    yieldDeltas.push(Number(now - lastYield) / 1000); // µs
    lastYield = now;
  }

  clearTimeout(stop);

  const wallElapsed = Date.now() - wallStart;
  const cpu = process.cpuUsage(cpuStart);
  yieldDeltas.sort((a, b) => a - b);
  const pct = (p: number) =>
    yieldDeltas.length === 0
      ? 0
      : yieldDeltas[Math.min(yieldDeltas.length - 1, Math.floor(yieldDeltas.length * p))];

  return {
    label,
    durationMs: wallElapsed,
    events,
    polls: bus.pollCount,
    pollsPerEvent: events === 0 ? 0 : bus.pollCount / events,
    userCpuMs: cpu.user / 1000,
    p50YieldLatencyUs: pct(0.5),
    p99YieldLatencyUs: pct(0.99),
  };
}

function fmt(r: BenchResult): string {
  return [
    `─── ${r.label} ───`,
    `  wall          : ${r.durationMs.toFixed(0)} ms`,
    `  events        : ${r.events}`,
    `  polls         : ${r.polls}`,
    `  polls / event : ${r.pollsPerEvent.toFixed(2)}`,
    `  user cpu      : ${r.userCpuMs.toFixed(1)} ms`,
    `  p50 yield Δ   : ${r.p50YieldLatencyUs.toFixed(1)} µs`,
    `  p99 yield Δ   : ${r.p99YieldLatencyUs.toFixed(1)} µs`,
  ].join('\n');
}

// ---------------------------------------------------------------------------
// Scenarios.
// ---------------------------------------------------------------------------

async function main() {
  const DURATION_MS = 3000;

  console.log(
    'EventStream microbench — synthetic trickle workload\n' +
      `Each scenario runs for ${DURATION_MS} ms.\n`,
  );

  // Trickle: 200 events/sec with 50µs simulated FFI cost. With limit=100,
  // every poll returns ≤ 1-2 events — exercises the partial-batch path.
  console.log(fmt(
    await runBench(
      'trickle 200 evt/s, ffi=50µs, limit=100',
      DURATION_MS,
      new TrickleBus(200, 50),
      { limit: 100 },
    ),
  ));
  console.log();

  // Saturated: 50k events/sec — every poll fills the limit. Confirms the
  // partial-batch sleep does NOT kick in on healthy high-throughput streams.
  console.log(fmt(
    await runBench(
      'saturated 50k evt/s, ffi=50µs, limit=100',
      DURATION_MS,
      new TrickleBus(50_000, 50),
      { limit: 100 },
    ),
  ));
  console.log();

  // Idle: zero events. Confirms idle backoff still converges to maxBackoffMs.
  console.log(fmt(
    await runBench(
      'idle 0 evt/s, ffi=50µs, limit=100',
      DURATION_MS,
      new TrickleBus(0, 50),
      { limit: 100 },
    ),
  ));
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
