# Payments — duplicate storms (P3)

> The money-path concurrency invariants, measured **and** asserted: what
> happens when N callers race the same payment. Establishes the at-most-once
> / concurrency baseline any later lock-regime change must preserve.
>
> Bench: `net/crates/net/payments/benches/duplicate_storm.rs`,
> `NET_PAY_BENCH_STORMS=8`, concurrency 16 / 128, zero-delay **counting**
> mock facilitator, operational filesystem (macOS/APFS).

## Invariants (asserted every storm, both concurrency levels — all hold)

**Duplicate acceptance storm** (N concurrent `accept_payment`, same quote +
payload):

- the facilitator **verifies once and settles once** (counted, not inferred);
- exactly **one** billing event is created;
- every attempt that returns `Served` carries the **same** billing id —
  including the timing-tolerant losers that first returned `InProgress` and
  were retried after the storm;
- `unique_payments == storms` (one settlement per storm).

**Duplicate redemption storm** (N concurrent `redeem_for_invocation`, same
settled quote):

- exactly **one** attempt returns `Admitted`; all others `AlreadyRedeemed`;
- the handler (a counter, run only for `Admitted`) fires **exactly once**.

At-most-once holds under contention. The CAS under the shared lock is correct.

## Throughput split (why one number would lie)

| storm | conc | attempts/s | admissions/s | unique_payments/s |
|---|---:|---:|---:|---:|
| acceptance | 16 | 24 | 24 | 1.0 |
| acceptance | 128 | 26 | 26 | 0.2 |
| redemption | 16 | 26 | 2.0 | 2.0 |
| redemption | 128 | 26 | 0.2 | 0.2 |

- **Acceptance:** attempts ≈ admissions (every attempt eventually returns
  `Served`, idempotently) ≫ **unique payments** (one settlement per storm).
- **Redemption:** attempts ≫ **admissions** (one `Admitted` per storm). A
  single "throughput" would report ~26/s of "success" and hide that only one
  invocation was admitted.

## The finding: a lock-contention ceiling, independent of concurrency

| storm | conc | p50 | p95 | p99 | max |
|---|---:|---:|---:|---:|---:|
| acceptance | 16 | 280 ms | 643 ms | 695 ms | 697 ms |
| acceptance | 128 | **1.99 s** | 4.69 s | 5.16 s | 5.47 s |
| redemption | 16 | 226 ms | 591 ms | 593 ms | 642 ms |
| redemption | 128 | **1.89 s** | 4.69 s | 5.21 s | 5.47 s |

**Attempts/s** (the whole-storm lock-serialized-op rate — one `accept_payment`
call = one exclusive-lock acquisition) is pinned at **~26 attempts/s
regardless of concurrency**; it is *not* an unlabeled "payment throughput"
(admissions/s and unique-payments/s are the separate columns above).
Per-attempt p50 grows from ~280 ms (c16) to ~2 s (c128), p99 to ~5 s. Every
attempt —
including the read-only losers (`InProgress` / `AlreadyServed` /
`AlreadyRedeemed`), which after the write-amplification fix no longer fsync —
must still acquire the **one exclusive `fs2` advisory lock**, whose
acquisition uses 1 ms→50 ms exponential backoff. Under 128-way contention
that backoff dominates: the store serializes the whole storm.

So the read-only-write fix removed the *fsync* from denials, but not the
*lock acquisition*. Redemption and acceptance both hit the same wall because
both take the exclusive lock.

## What P2 + P3 establish for the storage disposition

- **P2 (serial):** exact-proof admission is ~15–55 ms and grows with the
  whole-file size (several whole-file writes per acceptance).
- **P3 (concurrent):** correctness holds, but throughput ceilings at ~26/s
  and tail latency reaches seconds under contention, because one exclusive
  lock serializes every attempt.

Both dimensions point at the same object: a single JSON file under one
exclusive lock. Incremental lock work (a shared-read fast path for denials)
would lift the *denial* concurrency but leave the acceptance write — the
actual money path — on the exclusive lock and the whole-file rewrite. The
evidence favors a partitioned / indexed store over polishing the lock
topology; **P5 (spend contention) is the remaining input** before a storage
decision, since the spend store has a different access pattern (global/day/
asset counters, approvals, housekeeping) that a partition design must not
break. This storm bench is the concurrency + at-most-once baseline that any
such change must reproduce exactly.

## Reproduce

```
cargo bench -p net-payments --bench duplicate_storm   # NET_PAY_BENCH_STORMS=8
```
