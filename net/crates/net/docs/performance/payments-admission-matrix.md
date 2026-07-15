# Payments — admission matrix (P2 baseline)

> The headline the plan promised: what latency Net adds when a payment proof
> is already available. Measured end to end at **concurrency 1** with a
> **zero-delay mock facilitator** (external-rail latency excluded by design),
> on the **operational filesystem** (macOS/APFS, ordinary temp dir — decision
> D1). Raw baseline, preserved for comparison with later store-architecture
> work.
>
> Bench: `net/crates/net/payments/benches/admission_matrix.rs`,
> `NET_PAY_BENCH_SAMPLES=40`. Full metadata (records/bytes before→after,
> memory-backing, facilitator delay, fixture-prep) is printed per row by the
> bench; the tables below give p50 / p99 / throughput.

## Boundary 2 — exact-proof provider admission (the headline)

`accept_payment` **+** `redeem_for_invocation`. Acceptance inserts a record,
so this is a **transition** (`N → N+1`), restore-per-sample.

| transition | store bytes | p50 | p99 | throughput |
|---|---:|---:|---:|---:|
| 0 → 1 | 0 → 3 KB | **14.7 ms** | 25.5 ms | 66 /s |
| 99 → 100 | 306 KB → 309 KB | 20.2 ms | 21.6 ms | 48 /s |
| 999 → 1 000 | 3.09 MB → 3.09 MB | **55.2 ms** | 57.8 ms | 18 /s |

**This is the real payment number:** ~15 ms on an empty store, ~55 ms at
1 000 records. It is ~3× the redeem-only number below, because
`accept_payment` performs **several** whole-file durable writes (replay
claim → completion → billing), each serializing + fsync'ing the entire store.

## Boundary 1 — ready-settled redemption gate

`redeem_for_invocation` alone, cardinality held `N → N`, restored to an
un-redeemed baseline before every sample. Separately labeled — this is the
*shape* of a future prepaid/channel mode, not today's exact-payment path.

| cardinality | store bytes | p50 | p99 | throughput |
|---|---:|---:|---:|---:|
| 1 | 3 KB | 4.8 ms | 5.5 ms | 198 /s |
| 100 | 309 KB | 7.8 ms | 8.9 ms | 124 /s |
| 1 000 | 3.09 MB | 19.1 ms | 25.3 ms | 46 /s |

One durable write; scales with file size (4.8 → 19.1 ms as records 1 → 1000).

## Rejection matrix (`accept_payment` failure classes, card ~100)

| row | class | p50 | throughput | writes |
|---|---|---:|---:|---:|
| payload_mismatch | pre-state | **43 µs** | 21 k /s | 0 |
| expired | pre-state | 43 µs | 23 k /s | 0 |
| bad_quote | pre-state | 43 µs | 23 k /s | 0 |
| already_served | reads state | 7.7 ms | 118 /s | ~1 |
| replay | replay state | 8.2 ms | 113 /s | ~1 |
| quote_already_paid | quote state | 7.8 ms | 114 /s | ~1 |
| verify_rejected | claims + releases | **14.1 ms** | 68 /s | ~2 |

`already_served` reports `unique_payments/s = 0` (it returns the prior
billing but settles nothing new). `in_progress` is a race — a concurrent
active claim — measured in the P3 acceptance storm, not here.

## What the matrix establishes (the storage-work evidence)

The question was: for high-volume paid inference, is the dominant cost denial
lock contention, acceptance persistence, completion/billing persistence,
redemption persistence, or whole-file growth? The matrix answers it:

1. **Acceptance persistence dominates.** The headline is ~15–55 ms, ~3× the
   redeem-only cost, because acceptance does **several** whole-file writes
   (claim → completion → billing). The single-file store is the bottleneck,
   not the individual lock.
2. **Whole-file growth amplifies everything that touches state.** Every
   state-touching row scales with record count: redeem 4.8 → 19.1 ms, accept
   14.7 → 55.2 ms as records go 1 → 1 000. A 3 MB JSON re-serialized per write
   is the tax.
3. **Pre-state rejections are cheap and flat (~43 µs), size-independent.** The
   adversarial fast path (bad quote / expired / payload mismatch) never loads
   the store, so it stays bounded regardless of history — the cost boundary
   is well defended there.
4. **A verify rejection is as expensive as a full acceptance (~14 ms):** it
   claims durable state, then releases it — two writes. A caller who can
   drive facilitator-verify failures forces claim+release persistence per
   attempt. (Lesser than the — now fixed — redeem write-amplification, since
   it requires a valid quote+payload, but the same category. Tracked with the
   accept/spend read-only audit.)

**Implication:** the next storage move is not more incremental locking around
one JSON file — it is to stop re-serializing the whole store per write.
Acceptance's multi-write persistence and whole-file growth are the
high-volume ceiling; a partitioned / indexed / append-structured store is the
direction the numbers point to. This baseline is the comparison point for
that work.

## Reproduce

```
cargo bench -p net-payments --bench admission_matrix   # NET_PAY_BENCH_SAMPLES=40
```
