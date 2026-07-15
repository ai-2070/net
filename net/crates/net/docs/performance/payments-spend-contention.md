# Payments — spend contention (P5)

> `check_and_reserve` under concurrency, to find the **smallest legitimate
> atomic accounting boundary** before any store replacement — not to optimize
> the current store. Barrier-synchronized storms, restore-per-sample,
> operational filesystem (macOS/APFS). Every accounting invariant is asserted
> each sample.
>
> Bench: `net/crates/net/payments/benches/spend_contention.rs`,
> `NET_PAY_BENCH_SAMPLES=64` (p99 withheld below that credible count).

## Invariants (asserted every sample — all hold)

- **P5a same counter (ample):** every valid request reserves; the counter
  equals the exact sum of admitted amounts — no lost updates, no duplicate
  accounting.
- **P5a same counter (near-limit K, with K ≠ 1, N-1):** exactly **K** admitted,
  **N-K** denied; final reserved == `K × amount` == cap; **overspend == 0**.
- **P5b different capabilities, shared (day,asset) counter:** aggregate spend
  is exact across capabilities — distinct capability traffic contends on one
  atomic counter.
- **P5c independent (different asset):** all admit; each independent counter
  holds exactly its one amount.
- **P5d same approval key:** nothing reserves; exactly **one** pending
  approval, no duplicates, no premature reservation.
- **P5e housekeeping:** the stale-counter prune is dirty and persists on an
  otherwise-clean denial; the denial result is preserved; once clean, an
  equivalent denial does **not** rewrite (the task-#13 trap survives).

## The decisive result: the file lock, not accounting, imposes the coupling

| part | shared counter? | c16 attempts/s | c16 p50 | c128 attempts/s | c128 p50 |
|---|---|---:|---:|---:|---:|
| P5a same counter | yes (max) | 26 | 235 ms | 20 | 3.09 s |
| P5b diff caps, shared counter | yes | 26 | 235 ms | 20 | 3.15 s |
| **P5c independent, diff asset** | **no** | **25** | **235 ms** | **20** | **3.14 s** |

P5c reservations share **no** limiting counter — different asset, different
capability, logically independent under policy. They serialize **exactly as
hard** as P5a's maximally-contended same-counter reservations: identical
throughput, identical tail. The one global `fs2` file lock imposes the
coupling; required accounting authority does not.

> Operations that are independent under policy are nevertheless serialized by
> storage.

Secondary confirmations:

- **History cardinality has a modest effect** next to the lock: P5a ample c16
  p50 grows 235 ms → 311 ms as the approval bulk goes 0 → 1 000 records; the
  lock contention dominates.
- **P5d is faster under contention** (c128 p50 ~1.0 s vs ~3.1 s for the
  reserve storms) because — after the task-#13 read-only fix — the 127
  identical already-pending observations are **clean** (lock + load, no fsync);
  only the first caller writes. The read-only audit visibly helps here.

## What P5 tells the storage disposition (atomicity domains)

The smallest legitimate atomic units, from the assertions:

- **The `(day, network, asset)` counter row is the accounting unit.** It is
  **shared across capabilities** (P5b — "partition by capability" alone is
  invalid whenever capabilities share an asset counter) and **independent
  across assets** (P5c — different assets need no shared authority).
- **Approvals are a separate state machine**, keyed by quote id: one record
  per key, concurrent identical inserts collapse to one (P5d).
- **Housekeeping** (stale-counter prune) is a maintenance write on the
  counter space that must remain transactional and persist (P5e).
- A near-limit reservation is a **check-and-set on a single counter row**
  (P5a) — it does not need a transaction spanning multiple counters, unless a
  parent/child cap is introduced that spans them (not present today; P5b's
  shared cap lives on the *same* row, not a separate parent).

Combined with P2 (serial acceptance ~15–55 ms, several whole-file writes) and
P3 (~26 attempts/s ceiling, seconds of tail under one exclusive lock), P5
completes the picture: **the whole-file store under one lock imposes false
serialization on independent accounting.** A store partitioned by
`(day, network, asset)` counter row (plus a quote-keyed approval space and a
transactional housekeeping sweep) would let independent traffic proceed
concurrently while preserving every invariant asserted here.

This does **not** choose a storage engine, and partition implementation is not
yet authorized — it is the atomicity-domain classification the storage
disposition needs.

## Reproduce

```
cargo bench -p net-payments --bench spend_contention   # NET_PAY_BENCH_SAMPLES=64
```
