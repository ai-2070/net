# RedEX — userspace deterministic scheduler

## Status

**Parked — v2+.** Design captured; not implemented.

RedEX v1 uses a raw `AtomicU64::fetch_add` seq allocator. That's nondeterministic under concurrent writers, but v1's workloads are mostly one main writer or a few threads in one process, and none of the consumers that would *benefit* from determinism exist yet:

- No DST harness (fault-injection + simulated time).
- No RedEX replication protocol / replica convergence path.
- No CortEX fold runner that would demand reproducible replay.

Until at least one of those lands, the scheduler is complexity without payoff. We accept nondeterministic seq under contention for v1; it doesn't hurt initial use cases.

**When to revisit.** When we build a DST harness OR start work on RedEX replication OR CortEX wants cross-node fold reproducibility. At that point, re-open this doc, consider the refinements in §Refinements on revisit below, and implement.

## Refinements on revisit

Before implementing what's below, reconsider:

1. **Don't over-specify `(origin_hash, local_tick)` yet.** In many workloads, "deterministic within a single process" is enough — just use submission order from a single thread, or one scheduler per process with a simple FIFO. The full `(origin_hash, local_tick)` pattern is more relevant once multiple independent components submit to the same file. Pick the weaker primitive first; extend if needed.
2. **Consider single-threaded-per-process over per-file MPSC.** Instead of per-file MPSC + sort, a single `Sched` per process that owns all RedEX writes for that process may be simpler and sufficient. It deterministically orders submissions from within the process; cross-process convergence still needs protocol work, which is v2+ anyway.
3. **The API shape below (`RedexScheduler::submit` + `drive_step`) is fine** as a target if the full design is warranted. Keep the `Backpressure` integration.

The full design below is preserved as-is for reference. Treat it as a starting point, not a spec, when revisiting.

---

Companion to [`REDEX_PLAN.md`](REDEX_PLAN.md).

## Why determinism is a correctness property, not a nice-to-have

RedEX v1 allocates sequence numbers via `AtomicU64::fetch_add` on the write path. That's lock-free and fast, but it's **nondeterministic under concurrent submissions** — the appender whose CAS wins first gets the next seq, and "who wins first" depends on OS scheduling, CPU core counts, cache-line contention, and luck. Replay the same workload twice, get two different logs.

That's fine for "I just want to store events". It is **not fine** for:

1. **Replay debugging.** Reproducing a bug means feeding the same inputs back in and getting the same log. Without a deterministic scheduler, the same test case produces different seq orderings across runs, and the bug may or may not reappear.
2. **Deterministic simulation testing (DST).** FoundationDB / TigerBeetle-style fault injection requires that the system produces bit-identical output given bit-identical input — across fault seeds, across CPU counts, across runs. A CAS-race seq allocator breaks this by definition.
3. **Replica convergence (v1.1+).** When RedEX replicates, two replicas that receive the same set of events should produce the same log. Non-deterministic seq allocation means two replicas can disagree on the order of concurrent appends even with identical input streams — forcing them into the partition-healing path for what should be a trivially-convergent case.
4. **CortEX fold reproducibility.** The folded materialized view is a pure function of the log. If the log is nondeterministic, so is the view. Two nodes running the same fold over the same event set should land on the same state.

The scheduler is the primitive that gives RedEX all four properties at once, in userspace, without kernel scheduling tricks and without replacing the v1 append path wholesale.

## What v1 already provides

- **`RedexFile::append(payload) -> u64`** — raw CAS-seq allocator. Fast, nondeterministic under races.
- **Per-file `AtomicU64`** — the sequence counter. Remains the source of truth for assigned seq; the scheduler just decides *which submission gets which next seq*.
- **Per-file `HeapSegment` / `DiskSegment`** — payload storage. Unchanged.

The scheduler is additive: callers who want determinism go through it; callers who don't (bench mode, one-shot appends) can still call the raw path.

## Goals

- **Replay determinism.** Given the same sequence of submissions (in submission order, with the same metadata), produce the same log on every run — across CPU counts, OS schedulers, thread-pool configurations.
- **Convergence determinism.** Two RedEX replicas that receive the same input event multiset in the same logical order (v1.1 responsibility) produce identical logs without invoking partition healing.
- **Predictable throughput.** Scheduler hop is a single MPSC push + a single drain step; sub-microsecond per event under uncontended load. Not as fast as the raw CAS path, but within one order of magnitude.
- **Back-pressure compatible.** The scheduler's submission queue integrates with `StreamError::Backpressure` so callers see pressure when the scheduler falls behind.
- **DST-friendly.** The scheduler exposes a "drive one step" API that DST harnesses can call to advance virtual time deterministically.

## Non-goals

- **OS-level scheduling.** This is userspace. No kernel involvement, no `SCHED_FIFO`, no preemption. "Userspace deterministic scheduler" in the TigerBeetle / FoundationDB sense — a library-level ordering primitive.
- **Global ordering across files.** The scheduler orders appends *within* a single `RedexFile`. Cross-file ordering is a caller concern; RedEX never coordinated cross-file and this plan doesn't either.
- **Preserving wall-clock arrival order.** Wall clock is nondeterministic across runs. The scheduler orders by caller-supplied logical metadata, not by when the submission reached the queue.
- **Consensus / replica coordination.** v1.1's replication protocol is a separate problem. The scheduler makes the *local* ordering deterministic; replica agreement on that ordering still needs a protocol.
- **Total ordering across unrelated processes.** If two `MeshNode`s with disjoint `RedexFile`s run the same scheduler logic, their logs don't share a total order. They each have their own deterministic local order.

## Design

### 1. Shape: multi-producer, single-consumer, ordered drain

```
┌──────────┐
│ writer 1 │──┐
│ writer 2 │  │       ┌──────────────────┐      ┌────────────┐
│ writer 3 │──┼──────>│ RedexScheduler   │─────>│ RedexFile  │
│   ...    │  │       │  (bounded MPSC   │      │ seq + push │
│ writer N │──┘       │   + drainer)     │      └────────────┘
└──────────┘          └──────────────────┘
```

- **N producers** push `Submission { payload, origin_hash, local_tick }` into a bounded MPSC queue. Push is lock-free.
- **One consumer** (the "drainer") pops, orders, and forwards to `RedexFile::append_raw`. The consumer is a dedicated task (one per file) or a shared worker pool that round-robins across files.
- **`RedexFile::append_raw`** becomes the unsafe-for-multi-writer lower-level entry point the scheduler drives. Callers who go through the scheduler never call `append_raw` directly.

One consumer per file means the consumer has exclusive access to the `AtomicU64` seq counter, so the CAS loop becomes a plain `+= 1` — still atomic for cross-thread visibility but without contention.

### 2. Ordering policy

The scheduler's job is to decide the order in which submissions are handed to `append_raw`. Two layered rules:

**2a. Within a batch (one drain step).** The drainer pops up to `batch_size` submissions from the MPSC in one pass, then sorts them by `(origin_hash, local_tick)`. Ties on `(origin_hash, local_tick)` are impossible by construction — each origin increments its own `local_tick` monotonically, and cross-origin ties break on `origin_hash`.

**2b. Across batches.** Batches are processed in the order the drainer polled them. Within a single process, one task drains; batch order is well-defined. Under DST or replay, the harness drives step boundaries explicitly.

**Why origin_hash is the tiebreaker, not submission order:** submission order (MPSC dequeue order) depends on MPMC queue internals, which can vary across runtimes and CPU counts. `origin_hash` is a property of the *submission itself*, stable across any execution.

The `local_tick` is a per-origin monotonic counter: every time a writer calls `submit`, it increments its own tick. Two submissions from the same origin therefore have distinct `local_tick` values and no tie. Cross-origin submissions use `origin_hash` as the tiebreak so the ordering is well-defined even when two writers submit concurrently with the same wall time.

### 3. Submission API

```rust
pub struct RedexScheduler { /* handle */ }

pub struct Submission {
    /// The payload to append.
    pub payload: Bytes,
    /// Origin of this submission. Determines sort order across writers.
    pub origin_hash: u32,
    /// Per-origin monotonic counter. Writers own their own tick; the
    /// scheduler does not synthesize one — that would reintroduce the
    /// nondeterminism we're trying to remove.
    pub local_tick: u64,
}

impl RedexScheduler {
    /// Submit an event for scheduling. Returns `Backpressure` if the
    /// scheduler queue is at capacity; `NotConnected` if the scheduler
    /// was shut down.
    pub async fn submit(&self, s: Submission) -> Result<u64, SchedulerError>;

    /// Drive one drain step. Returns the number of events scheduled in
    /// the step. Normally the scheduler's own task calls this on a
    /// loop; DST harnesses call it explicitly to advance virtual time.
    pub fn drive_step(&self) -> usize;
}
```

- `submit` returns the seq the event will receive (known at enqueue time under §4's design) or `Backpressure`. Caller-visible seq enables "I submitted, here's my receipt" semantics without waiting for the drain.
- `drive_step` is the DST escape hatch. In production, the scheduler's own tokio task calls it in a loop; in tests, the harness calls it N times to advance deterministically.

### 4. Sequence assignment

Seq is assigned at **drain time**, not submit time. Submission: "I want to append, here's my payload + ordering metadata." Drain: "Now I'll sort these and assign seqs in order."

This means the `submit` return value is NOT the assigned seq directly — it's a `Receipt` containing a future/channel that resolves once the drain processes the submission. Callers who need the seq await the receipt; fire-and-forget callers drop it.

```rust
pub struct Receipt {
    rx: oneshot::Receiver<u64>,
}

impl Future for Receipt { /* resolves to the assigned seq */ }
```

Trade-off: adds a oneshot per submission. In the hot path for workloads that don't await the receipt, the scheduler can opt to drop the sender side entirely (receipt never resolves, caller doesn't care). Benchmark to confirm; likely negligible vs the payload memcpy.

### 5. Back-pressure

Scheduler submission queue has a configurable capacity (default 4096 per file). When full, `submit` returns `SchedulerError::Backpressure`. Maps cleanly to the existing `StreamError::Backpressure` for callers that go through the stream helpers.

Drain side does not block on storage — the `RedexFile::append_raw` call is a memcpy into a heap segment and a `Vec::push` on the index. Sub-microsecond under all but pathological conditions.

### 6. Determinism contract

**Guaranteed deterministic across runs:**

- Given the same ordered set of submissions (same `(origin_hash, local_tick)` pairs with the same payloads), the scheduler produces the same log: same seq assignments, same payload offsets, same checksums.
- This holds across CPU counts, thread pool sizes, and OS schedulers.

**NOT guaranteed deterministic:**

- Wall-clock time of individual appends.
- Per-run memory allocation addresses (use mmap + stable offsets if this matters).
- OS-level scheduling of the drainer task itself.
- Crash-recovered state if the crash happens mid-drain with unsynced disk segments (durability is orthogonal to determinism).

**Caller responsibility:** the caller supplies `(origin_hash, local_tick)`. If the caller hands in the same tick twice from the same origin, the second submission is a bug. If the caller derives `local_tick` from wall clock, determinism is lost at the caller, not at the scheduler. The scheduler itself is purely reactive to the metadata it's given.

### 7. DST integration

For fault-injection testing, the harness:

1. Disables the scheduler's own drain task.
2. Submits all events into the MPSC.
3. Calls `drive_step()` to advance one batch at a time.
4. Between steps, injects faults (drop payloads, reorder the segment file, simulate partial writes).
5. Asserts invariants (seq monotonic, payload checksums match, tail cursor stays within bounds).

Seeded RNG for any random choice inside the scheduler (currently none, but this is the hook for future tiebreak extensions). Same seed → same ordering → same faults on the same events.

## Implementation steps

1. **`Submission` + `Receipt` types** in `redex/scheduler.rs`. Unit tests: receipt resolves; dropped receipt doesn't leak.
2. **Bounded MPSC.** `tokio::sync::mpsc` or `crossbeam-channel` — pick the one with lower push cost under contention (bench both). Capacity config passed through `RedexFileConfig`.
3. **Drain step.** Pop up to `batch_size`, sort in-place by `(origin_hash, local_tick)`, assign seqs monotonically, call `RedexFile::append_raw` for each. Unit test: out-of-order submissions produce in-order log.
4. **Drain task.** Per-file tokio task that loops calling `drive_step` until shutdown. `RedexFile::close` signals shutdown; the task drains the queue then exits.
5. **`RedexFile::append_raw` refactor.** Rename current `append` to `append_raw`, make it `pub(crate)`. New `append` routes through the scheduler. Legacy fire-and-forget callers keep working (they get a dropped receipt).
6. **DST harness hook.** Feature-gate `#[cfg(test)]` or `#[cfg(feature = "dst")]` access to `drive_step` + submission-queue inspection.
7. **Back-pressure plumbing.** `SchedulerError::Backpressure` flows up through `RedexFile::append` as a distinct variant or folds into `RedexError::Backpressure`.
8. **Docs.** `REDEX.md` (caller-facing) gets a "Determinism" subsection. `TRANSPORT.md`'s Routing section-style "Philosophy" for why ordering is worth the hop.

## Tests

- **Unit: ordering policy.** Submit out-of-order `(origin_hash, local_tick)` pairs; assert the log comes out sorted.
- **Unit: backpressure.** Fill queue to capacity; assert next `submit` returns `Backpressure`; drain one; assert next `submit` succeeds.
- **Reproducibility regression.** Submit the same N events twice (two fresh schedulers, two fresh files); assert both produce byte-identical index + payload segments. Run with `--test-threads=1` and with high parallelism; both pass.
- **DST smoke.** Submit 1000 events from 10 virtual writers with deterministic `local_tick` schedules; `drive_step` in chunks of 50; assert the log matches a pre-computed expected sort.
- **Receipt correctness.** Submit, await receipt; assert the returned seq matches the file's `read_range` output for that event.
- **Benchmark.** Compare `append` (through scheduler) vs `append_raw` (direct) throughput. Target: scheduler <10× slower under single-threaded load; faster than raw CAS under heavy contention (one producer per core) because there's no CAS storm on the seq counter.

## Risks and open questions

- **Receipt overhead.** One oneshot per submission for callers that don't await feels wasteful. Mitigation: `submit_fire_and_forget(s)` variant that skips the receipt entirely. Only pay for what you use.
- **MPSC choice.** `tokio::sync::mpsc` is async-native but has slightly higher single-push cost than `crossbeam-channel`. For a latency-first path, `crossbeam` may win; but cross-task wakeups still need a Notify. Bench and pick.
- **Cross-file ordering.** If a caller needs "events to file A happen-before events to file B", the scheduler can't help — it's per-file. Cross-file determinism needs a higher-level primitive (a single scheduler shared across files, or explicit sequence points). Out of scope; flagged.
- **DST completeness.** Making the scheduler deterministic under DST is necessary but not sufficient for full DST of RedEX — segment allocation, retention sweeps, and replica protocol all need deterministic variants. This plan handles the append-ordering axis only.
- **Replica convergence.** The scheduler makes local ordering deterministic; *two* replicas agreeing on what got scheduled in what order still needs v1.1's replication protocol to agree on the input stream. Scheduler is necessary, not sufficient.
- **API impact on existing `append`.** Renaming the current `append` to `append_raw` is a breaking change for any pre-v1 caller. At v1's release, RedEX has no external users yet — this is cheap. Document in the v1 changelog and move on.

## Summary

A thin MPSC + sort-on-drain + single-consumer layer in front of RedEX's seq allocator. Writers submit `(payload, origin_hash, local_tick)`; the scheduler assigns seqs in `(origin_hash, local_tick)` order at drain time. Given the same inputs, produces the same log across every run, CPU count, and runtime. DST-friendly (`drive_step` hook). Back-pressure compatible. Throughput within 10× of raw `AtomicU64::fetch_add` under contention; much better than raw under a CAS storm because contention moves off the counter and onto the MPSC.

Determinism is the primitive this adds. Everything else — replay, replica convergence, CortEX fold reproducibility — is a downstream consequence.
