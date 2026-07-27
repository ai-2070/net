# BUG #130 — `HorizonEncoder::might_contain` Bloom Saturation

Resolves the deferred entry from `BUG_AUDIT_2026_04_30_CORE.md:693`:

> The bloom filter is 16 bits with two 4-bit hash positions per origin
> (`h & 0xF`, `(h >> 4) & 0xF`). Each insert sets two of 16 bits — by
> the birthday bound, after only ~6-8 inserted origins the bloom is
> fully saturated and `might_contain` returns true for *every*
> `origin_hash`. At that point `potentially_concurrent` always returns
> false, meaning the system claims "every event has observed every
> other event" — defeating causal concurrency detection on any
> non-tiny mesh.

## Current state

`adapter/net/state/horizon.rs::HorizonEncoder::encode` packs a
`ObservedHorizon` into a `u32` (`CausalLink::horizon_encoded`):

- bits 31-16: 16-bit bloom of observed `origin_hash`es, two positions per insert
- bits 15-0: log-scale `max_seq` across all observed entities

The bloom math is dire. With `m = 16`, `k = 2`:

| n (origins observed) | False-positive rate of `might_contain` |
|---|---|
| 2 | ~6 % |
| 4 | ~22 % |
| 6 | ~40 % |
| 8 | ~57 % |
| 12 | ~80 % |
| 16+ | ~95 %+ (saturated) |

`potentially_concurrent` calls `might_contain` twice and ANDs the
*negations*; saturation drives it to constant `false`. Receivers
that gate conflict-resolution on `potentially_concurrent` simply
stop running the resolution path.

The seq field of the encoding is dormant: production code reads
the bloom via `might_contain` / `potentially_concurrent` only.
`HorizonEncoder::decode_seq` exists but is called only from within
`horizon.rs`'s own test module — no production caller. So the low
16 bits of `horizon_encoded` are wasted on the wire.

## Fundamental constraint

Bloom filters give **false positives** ("might contain" → maybe yes,
maybe no). The semantic we want is the opposite — false negatives are
safe (treat events as concurrent → trigger merging that wasn't
strictly needed but is harmless), false positives are dangerous (treat
events as observed → skip merging that *was* needed).

So bloom is the wrong primitive at the limit. But it's cheap and
adequate for *small* cardinalities. The fix should:

1. Keep the bloom approach for the small-cardinality common case.
2. Make it actually work up to realistic mesh sizes (call it ≤ 16
   active origins observed simultaneously per event).
3. Document a hard cardinality ceiling and a path past it (the full
   `ObservedHorizon` is exchanged out-of-band; per-event bloom is the
   approximate fast path).

## Design options

### Option A — 32-bit all-bloom (drop the unused seq encoding)
- No wire-size change to `CausalLink`; `horizon_encoded` stays `u32`.
- All 32 bits become bloom; `k = 3` hash positions per insert (Kirsch-Mitzenmacher
  double-hashing from one xxh3 output).
- FPR table:

  | n | FPR (m=32, k=3) |
  |---|---|
  | 2 | ~2 % |
  | 4 | ~10 % |
  | 6 | ~22 % |
  | 8 | ~33 % |
  | 12 | ~55 % |
  | 16 | ~70 % |

- Pro: zero wire-size change, smallest churn.
- Con: still saturates around n ≈ 12–16. Modest improvement.

### Option B — Widen `horizon_encoded` to `u64` (recommended)
- `CausalLink::horizon_encoded`: `u32` → `u64`. CausalLink grows
  24 → 28 bytes (+17 % per event).
- All 64 bits are bloom; `k = 3` positions.
- FPR table:

  | n | FPR (m=64, k=3) |
  |---|---|
  | 2 | ~0.4 % |
  | 4 | ~3 % |
  | 6 | ~7 % |
  | 8 | ~13 % |
  | 12 | ~28 % |
  | 16 | ~44 % |
  | 24 | ~70 % |

- Pro: usable up to n ≈ 16 origins at <50 % FPR; +4 bytes per event
  is modest; uses the same primitive everyone else already
  understands.
- Con: per-event wire overhead +4 B (4 % of typical 100-B event,
  17 % of CausalLink).

### Option C — Sliding window of K most-recent origins (correctness-different)
- Replace bloom with explicit list of K most-recently-observed
  `origin_hash`es (e.g. K=4, packed as `[u32; 4]` = 16 bytes).
- `might_contain` becomes membership-test (no false positives for
  recent origins; FALSE NEGATIVES for evicted origins are safe).
- Pro: false negatives instead of false positives, which is the
  *correct* error direction for this use; long-running nodes never
  saturate.
- Con: doubles CausalLink wire size (24 → 40 B for K=4). Changes
  the semantic — peers that haven't been observed *recently* look
  concurrent, which may over-trigger merging on bursty peers. Not a
  drop-in.

## Pick

**Option B.** Reasoning:

- Option A is too marginal — saturation moves from n≈8 to n≈12, but
  the property still degrades on any non-tiny mesh. We'd be back at
  this entry the next time someone runs the audit.
- Option C is the most semantically correct but is a bigger
  semantic change (false-negative direction matters for downstream
  consumers) and a much larger wire-size hit. Worth considering for
  v2 once we have production data on observed-origin cardinality.
- Option B gives us a usable bloom for realistic small/medium
  meshes (≤16 origins per event) with minimal wire overhead and
  zero semantic change. The pre-1.0 status means the +4 bytes per
  event is acceptable churn now, not a load-bearing compatibility
  hit.

We document the ~16-origin practical ceiling and reference the
out-of-band full-`ObservedHorizon` exchange path for callers that
need exact answers above that.

## Files touched

Implementation:
1. `src/adapter/net/state/horizon.rs` — `HorizonEncoder::encode` /
   `might_contain` / `potentially_concurrent` and the
   `ObservedHorizon::encode` accessor switch from `u32` to `u64`.
   `decode_seq` and `encode_seq_log` / `decode_seq_log` removed
   (production-unused; tests pinning them go too). Bloom uses
   `k = 3` positions via Kirsch-Mitzenmacher (one xxh3 output split
   into two 32-bit halves; positions are `h1`, `h1 + h2 mod 64`,
   `h1 + 2*h2 mod 64`).
2. `src/adapter/net/state/causal.rs` — `CausalLink::horizon_encoded`
   `u32` → `u64`; `CAUSAL_LINK_SIZE` 24 → 28; `to_bytes` /
   `from_bytes` adjust accordingly.
3. Every call site that constructs / pattern-matches `CausalLink`
   with `horizon_encoded: <literal>` — most are 0 and require no
   change beyond the integer type, but `cargo check` will surface
   any.
4. `src/adapter/net/continuity/cone.rs` — `horizon_encoded` field
   widens to `u64`.

Tests:
5. New unit tests on `HorizonEncoder`:
   - `bloom_fpr_at_n_8_is_under_15_percent` — Monte Carlo over
     1000 random origin sets of size 8, assert FPR < 15 %.
   - `bloom_fpr_at_n_16_is_under_50_percent` — same shape, n=16,
     FPR < 50 %.
   - `might_contain_no_false_negatives` — every inserted origin
     reports `true`. (Bloom invariant.)
   - `bloom_uses_three_hash_positions` — pin via inspection that
     small bloom values have exactly 3 set bits per insert (modulo
     hash collisions).
6. Update existing tests that pattern-match the format directly:
   - `test_empty_horizon`: `assert_eq!(h.encode(), 0)` — still
     valid (u64 is `0` too).
   - `test_might_contain` / `test_potentially_concurrent`: still
     valid (semantic unchanged).
   - `test_regression_seq_encoding_*`, `test_regression_seq_roundtrip_*`,
     `test_seq_encoding_*`, `test_horizon_encode_decode_seq_consistency`
     — all go away with `decode_seq` removal.

Audit doc:
7. `docs/BUG_AUDIT_2026_04_30_CORE.md` — mark #130 FIXED, move out
   of "Outstanding deferred" list.

## Implementation order

1. Pure-`horizon.rs` rewrite first (encode/might_contain/etc on the
   wider format, drop seq bits and the seq-encoding helpers).
2. Update `CausalLink` wire format + `CAUSAL_LINK_SIZE`.
3. Sweep call sites — `cargo check` will surface every type mismatch.
4. Add the new bloom-FPR Monte Carlo tests.
5. Run full test suite + verify no regressions (the bloom semantic
   is unchanged for the subset of behavior production cares about,
   so existing tests should still pass post-fix).
6. Mark fixed in audit doc.

## Non-goals

- Adding a generation counter or sliding-window mechanism. Option C
  is a legitimate v2 fix; for now the documented ~16-origin ceiling
  + the out-of-band full-horizon exchange is sufficient.
- Backward-compat with the old `u32` wire format. The library has
  not shipped a stable release; CausalLink wire format already
  changed in this audit pass (BUG #135 etc.) — one more change
  now, not later.
