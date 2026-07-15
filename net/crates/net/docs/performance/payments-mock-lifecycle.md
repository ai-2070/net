# Payments — full mock lifecycle (P6)

> The whole software path composed, with **two timer endpoints from the same
> start**. A controlled composition measurement — **not** a payment-network
> benchmark.
>
> **Facilitator: in-process zero-delay mock. External rail latency excluded;
> chain inclusion / finality excluded.** The "billing identity" is precisely
> *the stable billing-event id `accept_payment` returns*; no external
> publication or sink durability is claimed (the harness observes none).
>
> Bench: `net/crates/net/payments/benches/mock_lifecycle.rs`,
> `NET_PAY_BENCH_SAMPLES=128`, fixed 100-record operational-filesystem fixture,
> restored outside timing before every sample.

## The two boundaries (same start)

- **Boundary A — quote request → stable billing identity:** issue quote →
  mock proof → `accept_payment` (verify + settle) → the billing-event id.
- **Boundary B — quote request → paid handler response:** …accepted + billed
  → `redeem_for_invocation` → handler executes → response.

| conc | A (quote→billing) p50 | B (quote→handler) p50 | lifecycle/s |
|---|---:|---:|---:|
| 1 | 14.0 ms | 20.2 ms | 48 |
| 16 | 347 ms | 353 ms | 21 |

At c1, the redeem + handler adds ~6 ms on top of accept+billing (~14 → ~20 ms)
— consistent with P2's boundary-2 (accept+redeem ~20 ms at 100 records) and
boundary-1 (redeem ~8 ms at 100 records). At c16 the 16 concurrent lifecycles
serialize on the engine's exclusive lock (~350 ms), the same ceiling P2/P3/P4
established; c128 is deliberately not repeated.

## Invariants (asserted every sample — all hold)

Per lifecycle (aggregate == batch size under concurrency):

- quote identity + payload binding correct;
- facilitator **verifies exactly once**;
- facilitator **settles exactly once**;
- **one** stable billing identity;
- **one** redemption admission;
- handler **executes exactly once**;
- returned handler result exact; quote ends settled + billed + redeemed;
- final state cardinality as expected; no timeout / ambiguous terminal state.

Replay witness (outside timing — correctness, not latency): after a full
lifecycle, redeeming again is denied (`AlreadyRedeemed`) and **the handler
does not run again**; re-accepting the same proof returns `Served` with the
**same** billing identity (`AlreadyServed`).

## Notes

- p99 is withheld below a credible sample count (128 samples here).
- This measures the **current** implementation — no storage/locking/state/
  billing/facilitator changes. Per the storage disposition, P6 is the
  end-to-end regression boundary a future store replacement must reproduce.

## Reproduce

```
cargo bench -p net-payments --bench mock_lifecycle   # NET_PAY_BENCH_SAMPLES=128
```
