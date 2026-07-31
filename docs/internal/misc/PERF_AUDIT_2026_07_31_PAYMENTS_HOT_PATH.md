# Performance Audit — Payments Hot Path (2026-07-31)

Source: code inspection of the `net-payments` crate — the provider lifecycle engine
(`src/engine/mod.rs`), the locked store (`src/policy/store.rs`), the caller flow
(`src/flow/mod.rs`), the spend engine (`src/policy/spend.rs`), and the facilitator /
chain-checker HTTP clients (`src/facilitator/client.rs`, `src/checker/transport.rs`) —
plus a sweep of the existing payments perf evidence (`docs/internal/performance/payments-*.md`)
and `docs/internal/plans/PAYMENTS_STORAGE_DISPOSITION.md` to establish what is already
identified, measured, or gated.

Latency figures quoted below are from the repo's own benches (P2/P3/P4/P5/P6) unless
marked otherwise. The byte figures in §2 are **modeled** — measured on a synthetic
`EngineState` built to the real field shapes, not on a captured production store; the
*ratio* is the claim, not the absolute size.

> **Status: §1 decided (Kyra) and implemented 2026-07-31; §2/§3/§5 implemented; §4
> withdrawn.** This is a survey answering "what payments hot-path headroom is available
> *without* the storage replacement" — because the dominant bottleneck is already
> characterized and explicitly gated. **§1 removes unbounded growth of fat lifecycle
> records and sharply reduces the growth slope; settlement-uniqueness tombstones remain
> unbounded by design — indexing later bounds the work per operation, not their
> cardinality.** §2–§5 shave constants.

The headline conclusion, consistent with P2/P3/P4/P5: **payment admission is
storage-bound, not crypto- or transport-bound**, and the whole-file JSON store under one
`fs2` lock is the cause. That conclusion is already locked in the storage disposition,
which marks logical partitioning **REQUIRED** and implementation **NOT AUTHORIZED**, and
marks the shared-read fast path (#12) **NOT PURSUED except tactical stopgap**. This audit
therefore deliberately proposes neither.

What it does add: the measured cost of every state-touching operation is roughly linear
in *store bytes*, and **nothing today bounds that number on the engine side.** The size
term is attackable within authorized scope, independently of the locking term, and the
win survives whichever store eventually lands.

It does **not** make total storage bounded, and this doc does not claim otherwise.
Settlement-uniqueness tombstones are permanent by design. The change removes the fat and
sharply reduces the growth slope; the future indexed store makes uniqueness lookup and
mutation independent of total tombstone cardinality. Actually bounding tombstone storage
would require an additional scheme-level invariant proving when a historical settlement
identity can safely be forgotten. Bounded storage is not worth purchasing by silently
weakening replay protection.

Recommended order of attack is at the bottom.

---

## 1. (Highest leverage) The engine store has no retention — the cost term grows without bound

**The gap.** `EngineState`'s three maps (`src/engine/mod.rs:311-325`) — `consumed`,
`consumed_transactions`, `quotes` — are insert-only. The sole removal path in the entire
engine is the unsettled-claim rollback in `release_claim`
(`src/engine/mod.rs:1660-1671`), which exists to undo a claim whose verify/settle failed.
A quote record that reached its terminal state (settled → billed → published → redeemed)
is never removed.

The spend store, by contrast, *does* prune: `check_and_reserve` drops counters beyond
`COUNTER_RETAIN_DAYS` on every pass (`src/policy/spend.rs:292-297`), with the P5e
dirty-flag discipline so a prune correctly marks an otherwise-clean transaction dirty.
The engine has no equivalent. The asymmetry looks unintentional rather than designed.

**Why it dominates.** Every state-touching operation deserializes the whole file, and
every dirty one re-serializes + fsyncs it. Both scale with total bytes
(`payments-admission-matrix.md`, `payments-redeem-write-amplification.md`):

| store | redeem p50 | accept p50 | redeem throughput |
|---|---:|---:|---:|
| 1 rec / 3.1 KB | 4.8 ms | 14.7 ms | ~198 /s |
| 100 rec / 309 KB | 7.8 ms | — | ~124 /s |
| 1 000 rec / 3.09 MB | 19.1 ms | 55.2 ms | ~46 /s |

Every bench holds cardinality **fixed** by construction — P4 pins 450 records and says so
explicitly, precisely so store size is not a hidden variable of concurrency. That is
correct methodology, and it also means **no bench measures the thing that actually
happens in production: the store never stops growing.** A provider serving 1 000 paid
calls/day adds ~3 MB/day permanently, and every subsequent accept and redeem is slower
than the last, forever. The P4 headline (paid invocation at ~23 admissions/s at c128) is
measured at a fixed 450 records; in production that row degrades monotonically.

**Why it is safe to fix now.** The durability half of the predicate already exists in the
record: `QuoteRecord.billing_published` (`src/engine/mod.rs:298-306`) is set only after
the billing event has durably landed in the attached `BillingLog` — the real audit surface
(durable JSONL + export). So "already durable elsewhere" is expressible today. The
decision below supplies the rest of the predicate, which needs one new persisted field and
a hard expiry floor.

### Decision (2026-07-31, Kyra): three retention policies, not two

The audit originally proposed two horizons. That was wrong in one respect — it treated
both replay maps as one class of state with one (longer) horizon. They have different
security roles and need different answers:

| State | Retention decision |
|---|---|
| Terminal `QuoteRecord` | Prune **6 hours after authoritative quote expiry**, provided it is billed, published, redeemed, and not in flight |
| `consumed` payload hash | Prune **atomically with its terminal quote record** |
| `consumed_transactions` settlement identity | **Retain indefinitely** — a compact security tombstone |

**Exact pruning predicate.** Persist `expires_at_ns` on `QuoteRecord`, then permit
deletion only when:

```text
billing.is_some()
&& billing_published
&& redeemed
&& !in_flight
&& now_ns >= expires_at_ns + expiry_tolerance_ns + 6 hours
```

**Why expiry is the hard floor.** If the engine deletes a record while its signed quote is
still valid, resubmitting that quote recreates the record — `accept_payment`'s claim
closure falls through to a fresh insert when `s.quotes.get_mut(&quote_id)` misses
(`src/engine/mod.rs:560-567`) — and the lifecycle can eventually permit a second
redemption. The replay maps do not preserve the complete terminal/idempotency outcome, so
they cannot substitute for the record here. Expiry plus tolerance is the floor; the 6
hours sit on top of it.

**Sizing.** At the audit's example volume of 1 000 calls/day, six hours retains ~250 full
records ≈ **0.77 MB** at the observed 3.09 KB/record — instead of days or months of fat
records riding every whole-file transaction.

**Why six hours.** It is comfortably above the Base pack's ~1-hour final-depth posture
(`FINAL_DEPTH_BASE = 1800` L2 blocks ≈ 1h at 2s/block,
`src/facilitator/packs.rs:62-67`); Solana and XRPL reach deterministic finality faster and
carry no depth knob at all (`final_depth` deliberately absent,
`src/facilitator/packs.rs:144,171`). This is an **operational re-verification grace** — it
is *not* a proof that every reorg was observed, and must not be described as one.

### Why settlement guards get no finite horizon

The audit's original claim — that replay guards need to outlive quote expiry plus the
reorg window — misidentified the security boundary. A historical settlement is still a
historical transfer after finality, and today the generic engine/checker path does not
universally establish:

```text
settlement transaction occurred during this quote's validity interval
```

Nor do all schemes bind the transaction uniquely to `quote_id`:

- **SVM** — the transaction is not quote-ID-bound (the payload is an opaque wallet blob;
  the engine falls back to the facilitator's settle-time payer claim,
  `src/engine/mod.rs:676-685`).
- **EVM** — the EIP-3009 authorization has a validity window and nonce, but the
  independent checker can still validate an old included transaction without comparing its
  block time against the new quote.
- **XRPL** — has an invoice binding, but the generic replay invariant must not depend on
  every deployment generating globally unique invoice IDs correctly.
- A malicious or defective facilitator could present an old settlement against a fresh
  quote carrying otherwise identical requirements.

So expiring `consumed_transactions` after 30, 90, or 365 days would mean: **one payment
can serve twice, provided the attacker waits long enough.** That is not an acceptable
trade for store size.

The transaction map is therefore a **permanent uniqueness index**, not ordinary retention
state. It stays small per entry relative to a full `QuoteRecord`, but it is the reason
this change reduces the growth slope rather than bounding total storage. The future
storage design makes uniqueness lookup and mutation independent of total tombstone
cardinality — unique indexed rows/keys instead of repeated whole-file parsing — which
bounds the *work per operation*, not the cardinality itself.

The payload hash plays a different role: it protects claim-time concurrency *before* the
settlement transaction is known. Once a successful terminal quote has an enduring
transaction tombstone, the payload entry can leave with the full record.

### Implementation constraints

1. Persist expiry as an **optional migration field**:

   ```rust
   #[serde(default)]
   expires_at_ns: Option<u64>,
   ```

   Fresh records store `Some(quote.expires_at_ns)`; pruning requires `Some(expiry)`. A
   mandatory `u64` without a default would make existing stores fail deserialization
   (`StoreError::Corrupt`, fail-closed — the store never silently resets); defaulting to
   `0` would make every legacy record immediately eligible. Compute the deadline with
   saturating arithmetic:

   ```rust
   expiry
       .saturating_add(expiry_tolerance_ns)
       .saturating_add(TERMINAL_RETENTION_NS)
   ```

2. Legacy records missing it remain **unprunable by default**. Do not infer authoritative
   expiry from x402's advisory `maxTimeoutSeconds` — the envelope's expiry governs, the
   x402 timeout is advisory.
3. Prune the quote and its matching `consumed` payload entry in the **same locked
   mutation**, and make the payload removal **owner-checked**: remove
   `consumed[payload_hash]` only if it still maps to the quote being deleted
   (`consumed[payload_hash] == quote_id`). If ownership differs — corruption, or an
   unexpected migration state — fail closed or retain the entry. Never erase another
   quote's replay guard merely because the retiring record names that payload hash.
4. **Never** prune `consumed_transactions` in this change.
5. A prune must mark an otherwise-clean operation **dirty**, preserving the P5e discipline
   (the pattern at `src/policy/spend.rs:292-297`).
6. Horizon configured on `PaymentEngine` alongside `expiry_tolerance_ns` /
   `in_flight_ttl_ns`, pruned opportunistically inside the existing locked mutation.

### Required regressions

- no terminal record is removed before quote expiry;
- pruning persists during an otherwise read-only operation;
- an expired, pruned quote cannot recreate lifecycle state;
- an old transaction remains rejected against a fresh quote after the original full record
  and payload hash are gone;
- a second identical pass after pruning is clean (no repeat write);
- legacy records without authoritative expiry remain intact.

The read-only-writes audit (`tests/read_only_writes_audit.rs`) must stay green throughout.

### Implementation notes (landed 2026-07-31, `94469d244`)

All six constraints and all six regressions landed, plus the flagged seventh
(owner-checked co-prune). Tests are in `payments/tests/engine_retention.rs`; 172 crate
tests pass, clippy clean on lib + tests. Three things worth recording:

- **Sweep sites.** Retention runs in `accept_payment`'s claim transaction — the operation
  that *mints* records also retires them, mirroring the spend engine's counter prune in
  `check_and_reserve` — folded into that transaction's `dirty` flag per constraint 5.
  `PaymentEngine::prune_terminal_records(now_ns)` exposes it for a provider that stops
  accepting but keeps redeeming. **The redeem gate was deliberately not chosen** as a
  sweep site: `redeem_for_invocation` takes no `now_ns` and there is no global clock
  (expiry uses signer timestamps), so sweeping there would have meant an API break.
- **The tombstone regression is driven end to end**, not asserted on state. The mock
  derives its transaction id from the payload bytes, and a payload binds the requirements
  but not the quote id — so an identical payload against a different quote genuinely
  re-settles the same transaction, and with the payload guard pruned the tombstone is
  demonstrably the last line of defense. It required a *second engine over the same store
  with a fresh mock*: the mock keeps its own settled-payload index and refuses the replay
  before the engine sees it, which would have silently made the test prove nothing. A
  forgetful facilitator is also the more faithful threat model, since the facilitator is
  deliberately not in the trust root.
- **The dirty/clean inode witness is unix-gated**, matching `read_only_writes_audit.rs`
  and `redeem_denial_no_write.rs`. Those two suites — and this one's P5e assertion — do
  **not execute on Windows**; they need a Linux/macOS run to be exercised.

**Relationship to the storage disposition.** This is not a store replacement and does not
pre-empt one. Partitioning attacks the *locking* term; retention attacks the *size* term.
They are orthogonal, and a partitioned store still wants retention. Nothing in the
disposition's "required non-decisions" list is touched — it names compaction strategy as
out of scope for *that* document, not as forbidden work. The permanent uniqueness index
is handed forward to that work as a stated requirement.

---

## 2. `save_json` pretty-prints every store write — ~11% wasted bytes on every fsync

`src/policy/store.rs:151` serializes with `serde_json::to_vec_pretty`. Every dirty
mutation of **both** stores therefore serializes, writes, and fsyncs a pretty-printed
document.

**Modeled measurement.** A synthetic 450-record `EngineState` built to the real field
shapes (byte-preserved requirements + payload carries, one signed verification event, one
signed billing event, both replay maps populated):

```
pretty  (to_vec_pretty) : 2,066,593 bytes
compact (to_vec)        : 1,842,471 bytes
reduction               : 10.8%  (1.12x)
```

The model runs ~4.6 KB/record against the ~3.1 KB/record the benches observe, so it is
somewhat fat in absolute terms; the ~11% ratio is what transfers. That is ~11% off the
serialize + write + fsync leg of every dirty transaction, at every store size.

**Per-record byte composition (compact), for §1's sizing:**

| component | bytes | share |
|---|---:|---:|
| `payload_b64` | 1 190 | 31.9% |
| `billing` | 946 | 25.3% |
| `chain` (1 event) | 664 | 17.8% |
| `requirements_b64` | 535 | 14.3% |
| other fields | 401 | 10.7% |

Note `payload_b64` embeds `accepted` (the full requirements again), so the requirements
are stored twice per record. That is **not** removable — byte-preservation forbids
re-serializing a received x402 document, and deduplicating it would reintroduce exactly
the envelope-drift bug class the carry exists to prevent. Retention (§1) is the correct
lever on this, not de-duplication.

**Risk: low.** `to_vec_pretty` appears exactly once in production code (the other hit,
`tests/lifecycle_modes.rs:259`, is unrelated). The cross-language golden vectors pin
`src/core/canonical.rs`, a completely separate encoder that is untouched by this. Both
stores parse either format, so the change is forward- and backward-compatible with
existing on-disk state. The only real cost is less readable state files when debugging by
eye.

---

## 3. The caller path canonicalizes a quote it usually discards

`src/policy/spend.rs:272-277` computes `canonical_bytes(quote)` plus a base64 encode on
**every** `check_and_reserve` call. The result (`quote_b64`) is consumed only inside the
`require` closure (`src/policy/spend.rs:311-328`), which runs only when inserting a *new*
pending approval — held so a post-approval retry redeems that exact provider-signed
quote.

On the common `Allowed` path, and on every already-pending observation, this is a full
`serde_json::to_value` of the quote plus a recursive canonical write plus a base64
encode, entirely discarded. Make it lazy inside `require`.

One implementation note: `canonical_bytes` returns `Result`, and the
`mutate_json_if_changed` closure signature is `FnOnce(&mut T) -> (R, bool)` with no error
channel — so the lazy version either routes a canonicalization failure into a `Denied`
decision or hoists it behind a `OnceCell`. Worth deciding deliberately rather than
reaching for the first shape that compiles.

---

## 4. Two full spend-file parses per payment — **WITHDRAWN (2026-07-31)**

> **This finding does not hold.** It was withdrawn at implementation time; the proposed
> fix is not viable and the two parses are structurally necessary. Recorded rather than
> deleted so the reasoning is not rediscovered. The description below is the original
> finding; the correction follows it.


`CallerPaymentFlow::run` calls `approved_quote()` at `src/flow/mod.rs:594`, which does a
lock-free whole-file `load_json` + parse (`src/policy/spend.rs:516-534`). It then calls
`check_and_reserve` at `src/flow/mod.rs:687`, which loads and parses the same file again
under the lock.

The spend file is not small: its `approvals` map holds **base64-encoded full quotes**
(`quote_b64`, per §3). And the redeem write-amplification doc gives the calibration —
"parsing 3 MB of JSON is ~5.5 ms" — so a parse is not free at scale.

On the common path (no pending approval for this capability) the first read finds nothing
and is pure overhead. Folding the approved-quote lookup into the same locked transaction
removes it, and has the side benefit of closing the read-then-lock window between the two
loads.

**Correction — why this is wrong.** The two calls have a hard data dependency, in the
wrong direction for folding:

- `approved_quote` decides **which quote is used at all**. If it returns a held approved
  quote that is still valid, that quote *is* the one paid, and the provider is never
  contacted (`src/flow/mod.rs:594-622`); only on `None`, or on an expired/unparseable
  hold, does the flow fetch a fresh provider-signed quote.
- `check_and_reserve` takes that resulting quote as its **input**.

So the second transaction cannot subsume a lookup that must complete before its own input
exists — and before the provider call that might produce it.

The second read is also not redundant: it must happen **inside** the lock. Reusing the
snapshot from the first read would reintroduce exactly the lost-update race the store's
regime exists to prevent (`concurrent_mutations_do_not_lose_updates`), and would break the
no-overspend guarantee P5a asserts. Two parses are the correct cost of one lock-free
lookup plus one atomic check-and-reserve.

What remains true from the original finding: the spend file is fatter than it needs to be
because `approvals` holds base64-encoded full quotes. That is a size observation, already
covered by §3's laziness (fewer approvals written) and not a redundant-read one.

---

## 5. `accept_payment` builds a record it discards on every duplicate

`src/engine/mod.rs:492-507` constructs the full `QuoteRecord` — including two base64
encodes of the requirements and payload carries (~1.7 KB of base64 in the §2 model) —
*before* the claim closure runs. The record is used only in the final insert branch
(`src/engine/mod.rs:566`); every other `Claim` outcome discards it.

Every non-fresh row in the rejection matrix pays this: `already_served` (7.7 ms),
`replay` (8.2 ms), `quote_already_paid` (7.8 ms). Under P3's duplicate storms, 127 of 128
concurrent attempts pay it. It is small next to the ~8 ms of I/O those rows spend, but it
is free to remove — move the construction into the branch that inserts it.

---

## What came back clean

- **Pre-state rejections** (`payload_mismatch`, `expired`, `bad_quote`) are ~43 µs and
  size-independent — they never load the store. The adversarial fast path is already well
  defended; nothing to do.
- **HTTP clients pool correctly.** `HttpFacilitator` (`src/facilitator/client.rs:115`)
  and `RpcTransport` (`src/checker/transport.rs:38`) each build one `reqwest::Client` per
  instance and reuse it, so connections and the pinned-TLS config are not rebuilt per
  call. `tls_roots::tls_config()` (parsing ~150 Mozilla roots) is likewise per-client, not
  per-request.
- **`verify_rejected`'s two writes** (claim + release, ~14.1 ms) are semantically real —
  in-flight persistence for concurrency, release because value did not move. The
  read-only-writes audit deliberately excludes them; that judgment still looks right.

---

## Recommended order of attack

All landed 2026-07-31 on `performance-x402`, one commit each:

| finding | commit | state |
|---|---|---|
| §1 engine retention | `94469d244` | implemented + 10 regressions |
| §2 compact store writes | `a0f21e849` | implemented + 1 regression |
| §5 lazy `QuoteRecord` | `0c0d773be` | implemented |
| §3 lazy `quote_b64` | `327c68b63` | implemented |
| §4 folded spend read | `4f5ddedb8` | **withdrawn** — not implementable as described |

172 crate tests pass; clippy clean on lib + tests. Two caveats on the verification, both
environmental rather than about the changes:

- The **unix-gated suites do not run on Windows** — `read_only_writes_audit.rs`,
  `redeem_denial_no_write.rs`, and this work's P5e inode witness all need a Linux/macOS
  run. The dirty-flag discipline is asserted but unexercised here.
- `benches/spend_contention.rs` **does not compile on Windows** (uses `std::os::unix`
  unconditionally). Pre-existing, unrelated to this work, and the reason
  `cargo clippy --all-targets` fails on this platform.

None of these move the c128 tail — that is the lock, and it is the storage disposition's
problem. What they move is the per-operation constant and, in §1's case, how fast that
constant grows.

## Reproduce

The byte figures in §2 are modeled, not captured; the model is described inline above and
was not committed. The latency figures are the existing benches:

```
cargo bench -p net-payments --bench admission_matrix     # P2, size scaling
cargo bench -p net-payments --bench redeem_matrix        # redemption × cardinality
cargo bench -p net-payments --bench spend_contention     # P5
cargo bench -p net-payments --features mesh --bench mesh_paid_invoke   # P4
```
