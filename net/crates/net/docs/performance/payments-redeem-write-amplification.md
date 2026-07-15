# Payments — redemption-gate write amplification (a defect the benchmark found)

> The very first payment benchmark smoke bar exposed a concrete systems
> defect before we published any aggregate number: **read-only redemption
> denials perform a full durable store write (serialize + fsync + rename).**
> That is write amplification and a denial-of-service surface. This note
> records the before-fix baseline, the fix, and the after-fix rerun.
>
> Bench: `net/crates/net/payments/benches/redeem_matrix.rs`. Host: macOS,
> APFS, ordinary temp dir (the operational filesystem — the primary result
> per decision D1 in `docs/plans/PAYMENTS_BENCHMARKS_PLAN.md`, **not** tmpfs).

## The defect

`redeem_for_invocation` (`engine/mod.rs:1490`) runs its whole decision inside
`mutate_json` (`policy/store.rs:188`). `mutate_json` **always** calls
`save_json` — serialize the entire JSON store, `fsync`, atomic-rename — on
the way out. But only one branch of the closure mutates state
(`rec.redeemed = true` → `Admitted`, `mod.rs:1568`); every other branch is a
read-only `Denied{..}` (`UnknownQuote`, `WrongToolBinding`,
`BindingRejected`, `AlreadyRedeemed`, `NotSettled`, `QuoteFrozen`, …).

So a redemption **denial** — including the earliest-exit `UnknownQuote`,
which touches no record at all — still:

```
acquire the cross-process advisory lock
load + parse the complete JSON store
serialize the complete store
write a temp file, flush, fsync
atomic-rename over the store
```

A caller who sprays random quote ids forces a global-lock + whole-file
`fsync` per attempt, without holding any quote. That is a DoS surface, and
it competes for the same lock legitimate redemptions need.

## Before-fix baseline

`NET_PAY_BENCH_SAMPLES=120`, denial rows repeatable; `valid_admitted`
single-use (samples = fresh seeded quotes) at concurrency 16. Latencies µs.

### Per-op cost scales with store size (concurrency 1)

Every outcome — denial or admission — costs the same durable write, and it
grows with the JSON file:

| store (records / bytes) | denial p50 | denial p99 | throughput |
|---|---:|---:|---:|
| 1 / 3.1 KB | ~5,040 µs | ~7,500 µs | ~200 /s |
| 100 / 309 KB | ~6,070 µs | ~7,400 µs | ~165 /s |
| 1 000 / 3.09 MB | **~19,530 µs** | ~24,000 µs | **~51 /s** |

The ~200/s ceiling at one record is one serialized fsync transaction per
op; at 1 000 records it is ~51/s — the whole 3 MB file is re-serialized and
re-synced on every call, denial or not.

### Global lock serializes unrelated callers (store = 1 record)

| concurrency | denial p50 | denial p95 | throughput |
|---|---:|---:|---:|
| 1 | ~5,040 µs | ~5,500 µs | ~200 /s |
| 16 | ~5,080 µs | **~900,000 µs** | ~95 /s |
| 128 | ~2,900,000 µs | ~5,700,000 µs | **~20 /s** |

At 128 concurrent sprayed denials the p50 is ~2.9 s and throughput collapses
to ~20/s — every contender waits behind the fsync holding the one lock.

The denial rows (`unknown` / `wrong_tool` / `invalid_binding` /
`already_redeemed`) are indistinguishable from each other and from
`valid_admitted`: all pay the write. That is the amplification.

## The fix

A conditional-save transaction that only writes when the closure reports it
changed state:

```rust
pub async fn mutate_json_if_changed<T, R, F>(path: &Path, f: F) -> Result<R, StoreError>
where
    T: DeserializeOwned + Serialize + Default,
    F: FnOnce(&mut T) -> (R, bool),   // (verdict, dirty)
{
    let _guard = LockGuard::acquire(path).await?;
    let mut state: T = load_json(path).await?;
    let (result, dirty) = f(&mut state);
    if dirty {
        save_json(path, &state).await?;
    }
    Ok(result)
}
```

`redeem_for_invocation` switches to it: every `Denied{..}` returns
`dirty = false`; only `Admitted` (after `rec.redeemed = true`) returns
`dirty = true`. The load, decision, and conditional save all stay under the
**same** advisory lock, so the check-and-set (settled-and-not-yet-redeemed →
mark redeemed) is still atomic across processes: **at-most-once is
unchanged**; we only stop rewriting the file when nothing changed.

Guarded by a regression test (`tests/redeem_denial_no_write.rs`): a denial
leaves the store inode unchanged (no rename), an admission changes it.

### Follow-up audit (tracked, not in this change)

The same read-only-branch analysis applies to other `mutate_json` closures
and should follow: `accept_payment`'s claim outcomes (`InProgress`,
`AlreadyServed`; `engine/mod.rs:508`) and hard spend-policy denials that
create no approval and alter no counter (`spend.rs:282`). Each needs its
own per-branch dirty determination — done deliberately, not by reflex.

## After-fix rerun

_(filled in after the fix lands — same command, same host.)_
