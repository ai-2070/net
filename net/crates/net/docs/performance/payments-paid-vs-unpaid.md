# Payments — paid vs. unpaid nRPC delta (P4)

> The admission overhead an application actually pays: the same tool served
> two ways — `serve_tool` (unpaid) vs `serve_tool_paid` (paid) — with
> identical request/response types, identical handler body, and the same JSON
> codec and transport. The **only** difference is the payment gate, so
> `delta = paid_p50 − unpaid_p50` is attributable to payment admission
> (the provider-side redeem).
>
> Bench: `net/crates/net/payments/benches/mesh_paid_invoke.rs`
> (`--features mesh`), warm two-node loopback mesh, `NET_PAY_BENCH_SAMPLES=150`,
> binding off (bearer), **fixed store cardinality = 450 records** (all quotes
> pre-minted up front; redeem adds no record), operational filesystem.

## Method

Quotes are at-most-once and there is no proof-reuse API, so N distinct settled
quotes are pre-minted in-process (issue + accept on the provider engine)
**outside** the timed region and attached one-per-call as the
`net-payment-quote` header. Every quote is pre-minted before any timing, so
the engine store stays at a constant 450 records for every paid row — store
size is not a hidden variable of concurrency (see P2 for its size-scaling).
Concurrency 1 / 16 / 128.

## The delta

| conc | unpaid p50 | paid p50 | paid p95 | **delta (p50)** |
|---|---:|---:|---:|---:|
| 1 | 78 µs | 10.7 ms | 11.8 ms | **10.7 ms** |
| 16 | 307 µs | 11.3 ms | **1.88 s** | 11.0 ms |
| 128 | 1.66 ms | **2.92 s** | 6.40 s | **2.9 s** |

Throughput: unpaid ~11 k/s (c1) → ~50 k/s (c128) — ordinary nRPC scales with
concurrency. Paid ~140/s (c1) → ~23/s (c128) — it *anti*-scales.

## Findings

1. **A ~10.7 ms per-invocation payment tax at c1** (at a 450-record / 1.4 MB
   store): the redeem gate's `fs2` lock + JSON read-modify-write + fsync. At a
   smaller store it is smaller (P2's boundary-1 redeem is ~4.8 ms at 1 record,
   ~19 ms at 1 000) — the tax scales with store size, and this run fixes it at
   450 records.
2. **The delta is not a fixed cost — it collapses under concurrency.** The
   median holds ~11 ms at c1/c16, but the paid **tail** explodes (p95 1.88 s at
   c16) and by c128 even the **median** is ~2.9 s. Meanwhile the unpaid path
   stays at ~1.7 ms and 50 k/s. The entire gap is the redeem serializing on the
   store's one exclusive lock.
3. **Apples-to-apples isolates payment.** Both sides are tool calls
   (`serve_tool` vs `serve_tool_paid`, same handler, same JSON codec), so
   RPC-vs-tool dispatch, schema, and wrapper costs cancel in the delta. What
   remains is the gate.

## Ties to the storage disposition

P4 is the application-facing shadow of P2/P3/P5: ordinary nRPC does ~50 k/s at
c128; the paid path is capped at ~23/s by the redeem's exclusive lock on the
whole-file store. The payment admission overhead is dominated by the store
lock, not by crypto or transport — the same conclusion the
`PAYMENTS_STORAGE_DISPOSITION.md` draws. A partitioned store (per
`(day, network, asset)` + per-quote lifecycle, per the disposition) would let
independent paid invocations proceed concurrently; this delta is the headline
that store replacement must move (the median must stay near the c1 tax, and
the tail must stop scaling with concurrency).

External facilitator / chain latency is excluded by design (zero-delay mock),
per the plan's public split — this is Net admission overhead, not settlement.

## Reproduce

```
cargo bench -p net-payments --features mesh --bench mesh_paid_invoke   # NET_PAY_BENCH_SAMPLES=150
```
