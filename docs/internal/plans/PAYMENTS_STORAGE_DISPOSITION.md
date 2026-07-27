# Payments Storage Disposition

> **Status:** DECISION — logical atomicity and partitionability
> **Implementation:** NOT AUTHORIZED
> **Storage engine:** NOT SELECTED
> **Evidence:** P2 + P3 + P5 (`docs/performance/payments-admission-matrix.md`,
> `payments-duplicate-storms.md`, `payments-spend-contention.md`)
>
> This is a **classification** document. It fixes the logical concurrency
> domains and the cross-domain invariants any future payment store must
> preserve. It does **not** design a store, choose an engine, or authorize
> implementation.

## Why now

The benchmark suite produced three findings that, together, close the
question of whether the current whole-file JSON store is the right target
architecture:

- **P2 — serial growth:** exact-proof admission is ~15–55 ms and grows with
  the whole state file, because acceptance performs several whole-file
  durable writes (claim → completion → billing).
- **P3 — duplicate contention:** correctness holds under c16/c128, but one
  global exclusive lock caps concurrency at ~26 attempts/s with seconds of
  tail — even for read-only losers that no longer fsync.
- **P5 — independent reservations:** operations that share **no** limiting
  counter (different `(day, network, asset)`) serialize *identically* to
  maximally-contended same-counter operations.

The load-bearing result is stronger than "the store is slow":

> **Operations that are independent under payment policy are coupled by the
> physical storage lock.** The current serialization boundary is broader than
> the legitimate authority boundary.

That is a structural statement about *authority*, not throughput, and it is
what makes a disposition possible without any further measurement.

---

## The logical domains

The disposition covers **both** the payment engine store and the spend-policy
store. Each domain lists its primary identity, the state it owns, and the
**local** atomic operations it must support. "Local" means: operations on
*unrelated* identities in a domain, and operations in *different* domains,
must not be forced through one global lock.

### 1. Payment lifecycle domain

**Primary identity:** `quote_id`.

**State:** acceptance claim/lease; verification + settlement state;
facilitator transaction identity; redemption state; billing identity +
publication state; stale-claim recovery metadata.

**Required local atomic operations:**

```
unclaimed        → in-flight claim
in-flight        → settled / billed
settled          → redeemed
stale in-flight  → reclaimed (TTL, refreshed clock)
claim            → released after verification failure
```

**Invariant:** unrelated quotes must not require one global lock. Two quotes
with different `quote_id` and no shared replay/settlement identity are
independent execution domains.

### 2. Replay / settlement uniqueness domain

Quote-local storage alone is insufficient: the engine forbids one facilitator
transaction or one payment proof from being reused across *different* quotes.
That is a relationship **between** quotes, so it cannot live purely inside a
per-quote partition.

**Required invariant:**

> Two concurrent quotes presenting the same replay identity (payload replay
> key, or settlement transaction id) cannot both settle — even if their quote
> records are physically partitioned.

The disposition does **not** prescribe the mechanism (unique index,
transactional key, reservation row, or otherwise). It states the invariant.

### 3. Billing / outbox domain

Settlement completion and durable billing publication state must not become
separable in a way that permits any of:

- a settled payment with permanently lost billing;
- two billing identities for one payment;
- replay creating duplicate billing;
- republish changing the billing identity.

**Required relationship (crash-consistent):**

```
settlement completion
+ stable billing-event identity
+ durable publication / republish state
```

This does **not** require the external billing-sink call to occur inside a
database transaction. It requires a durable **outbox-style fact** (or
equivalent) so publication can be retried idempotently after a crash, without
minting a new billing identity. (Witnessed today by
`a_lost_billing_append_is_recovered_on_retry`.)

### 4. Spend-counter domain

**Evidence-supported atomic key:** `(day, network, asset)`.

Within that domain a decision may read: the aggregate amount for the
asset/day; capability-specific policy/counters; a parent/global limit; the
current reservation; and stale state relevant to that row.

**Required invariant (one atomic decision):**

```
read applicable parent + capability limits
→ test reservation
→ update all affected counters
```

- **P5b** establishes that capabilities sharing the same `(day, network,
  asset)` parent **cannot** be blindly isolated into transactions that ignore
  the shared parent cap — the check-and-set spans the shared counter.
- **P5c** establishes that different assets without a shared counter are
  **legitimate independent execution domains** and must not serialize merely
  because they inhabit the same store.

### 5. Approval domain

**Primary identity:** quote / approval key.

**Required invariants:**

- one pending approval for duplicate requests;
- identical already-pending observations are **clean** (no write);
- approval state is durable;
- an approval cannot authorize multiple spend reservations accidentally;
- a denial does not reserve spend prematurely.

Keep the approval record **logically separate** from the counter row. But the
**approval-consumption → counter-reservation** transition is a **cross-domain
invariant** requiring explicit design: an approved quote's retry must reserve
exactly once. The eventual design must provide **either**:

- one transaction spanning approval and counter state; **or**
- an idempotent reservation protocol giving equivalent crash/concurrency
  safety.

The disposition does **not** choose between them.

### 6. Housekeeping domain

Classify housekeeping (stale-counter pruning) as **bounded transactional
cleanup over each affected partition**.

**Required:**

- stale removal persists;
- live counters are not altered;
- a nominal denial remains **dirty** when cleanup occurred (the task-#13 trap);
- the next equivalent transaction is **clean** (no repeat cleanup write);
- cleanup is idempotent;
- cleanup cannot create accounting gaps.

**Correction to the earlier proposal:**

> Housekeeping must be transactionally consistent with every counter
> partition it mutates. **Global all-partitions atomicity is not established
> or required by current evidence.**

P5e proves only that *when a transaction discovers stale state, the cleanup is
a real mutation and must persist*. It does **not** prove that all stale
counters across every partition must disappear atomically in one global sweep.
A future implementation may perform bounded, idempotent, per-partition cleanup
without reintroducing a global transaction.

---

## The partitionability conclusion (locked)

> The current whole-file JSON lock is **not** a legitimate authority boundary.
> Different `(day, network, asset)` accounting domains and unrelated payment
> quotes may proceed independently, subject only to explicit cross-key
> invariants — replay uniqueness, shared parent limits, billing durability,
> and approval consumption.

This is a statement about **logical concurrency domains**, not file layout.
"One file per asset" is *not* the conclusion — physical files are already an
engine/design choice, deliberately out of scope here.

---

## Required non-decisions

This disposition does **not** select, imply, or authorize any of:

- a storage engine (SQLite, LMDB, RedEX, or another);
- relational vs. append-structured storage;
- file count or directory layout;
- schema encoding;
- migration strategy;
- compaction strategy;
- process ownership model;
- the RPC boundary;
- an implementation schedule.

> **No storage implementation is authorized by this document.**

---

## Shared-read fast path (task #12) — default disposition

**DO NOT IMPLEMENT as the primary next step.** The evidence is now sufficient
to say why:

- it improves clean-**denial** concurrency only;
- it does **not** remove successful-acceptance whole-file rewrites;
- it does **not** let independent spend domains proceed concurrently;
- it adds shared→exclusive recheck and writer-starvation complexity;
- it optimizes the lock topology of a storage architecture the evidence now
  disfavors.

**Escape hatch (narrow):**

> Reconsider only as a **tactical stopgap** if replacement storage is
> explicitly deferred **and** measured denial traffic independently justifies
> the complexity.

This is more decisive than an indefinite "frozen," without irreversibly
deleting the option.

---

## Future implementation acceptance contract

Any replacement store must pass these gates. This document does not implement
them; it **requires** them of whatever comes next.

### Correctness (must reproduce, exactly)

- **P3 acceptance storm:** verify once; settle once; one billing identity;
  retry-safe `InProgress`.
- **P3 redemption storm:** exactly one `Admitted`; exactly one handler
  execution.
- **P5 near-limit race:** exactly `K` admitted; no overspend.
- shared-parent capability accounting (P5b);
- independent-asset concurrency (P5c);
- one pending approval under contention (P5d);
- approval consumption cannot double-reserve;
- stale-claim recovery (TTL reclaim);
- verification-failure claim release;
- billing republish (idempotent, stable identity);
- corrupt-state fail-closed behavior.

### Durability & crash safety (required of the replacement; not implemented now)

Crash-safety must be demonstrated for a crash between each pair of steps:

```
claim creation
verification completion
settlement completion
billing-state creation
redemption CAS
approval consumption
counter reservation
housekeeping mutation
```

The disposition does not add crash injection now, but a replacement must
provide it.

### Performance (compared on the same boundaries)

Re-run, on the replacement, the existing measurement boundaries: P2
fixed-cardinality serial admission; P3 c16/c128 duplicate storms; P5
same-counter contention; P5 shared-parent contention; P5 independent-asset
contention; approval contention.

The most important proof is **not** a lower p50. It is:

> **Independent domains scale materially better, while same-domain
> no-overspend remains exact.**

---

## Disposition

| item | disposition |
|---|---|
| Whole-file JSON store | **REJECTED** as the target architecture |
| Global exclusive lock | **REJECTED** as the authority boundary |
| Shared-read optimization (#12) | **NOT PURSUED** except tactical stopgap |
| Logical partitioning | **REQUIRED** |
| Storage engine | **UNDECIDED** |
| Implementation | **NOT AUTHORIZED** |
| Next benchmark phase | **P4** |

## Sequence after landing

```
P2 / P3 / P5 evidence:      COMPLETE
Storage disposition:        THIS DOCUMENT (land)
Shared-read #12:            DO NOT IMPLEMENT
Partition implementation:   NOT AUTHORIZED
P4 paid/unpaid delta:       NEXT
P6 lifecycle:               AFTER P4
P7 telemetry:               LAST
```

Land this disposition, then proceed to **P4** (paid vs. unpaid mesh delta)
without beginning the storage replacement.
