# Payments Benchmarks Plan

> First payment-specific benchmark targets for `net-payments`. **v0.2 —
> revised per review** (2026-07-15): the headline boundary is corrected
> (`redeem_for_invocation` measures *ready-settled redemption*, not
> *proof-present admission*), tmpfs is demoted from headline to diagnostic
> floor, the paid/unpaid comparison is made apples-to-apples, the
> duplicate storm is split into two invariants, and stateful benches move
> off criterion onto a custom harness with fixed-cardinality fixtures.
> Companion to the SDK's nRPC bench suite (`sdk/benches/nrpc_*.rs`).

## The framing sentence (what we publish)

> Net reports the **ready-settled invocation gate** separately from
> **exact-payment acceptance**, **paid invocation**, and **external
> settlement**. Each number states whether durable storage, facilitator
> I/O, and handler execution are included.

External facilitator / chain latency is **never** blended into a Net
controlled number. Metrics: **p50 / p95 / p99 + throughput**, and every
payment row carries its full environment metadata (§Metadata).

## Why so few benches

Payment performance matters as **the additional latency between an
otherwise-ready invocation and handler execution** — the admission tax.
We are not building a zoo around every payment object or scheme. Cold
exact-per-request settlement for strangers is dominated by the external
rail; the local admission number only becomes the economically relevant
one once callers use prepaid balances / accounts / channel drawdown —
which is a *different*, amortized shape that does not exist yet (§Out of
scope). So the suite is small and the framing is restrained.

---

## The public split — four controlled boundaries

Each is a *distinct* number with a *distinct* inclusion list. None is "the"
payment latency; the point is that they differ and we say how.

| # | Boundary | Path | Includes | Excludes |
|---|---|---|---|---|
| 1 | **Ready-settled redemption gate** | settled+billed quote → `redeem_for_invocation` admits handler | binding check (opt), settled/billed/frozen checks, capability binding, at-most-once commit | quote verify, facilitator, settlement, billing |
| 2 | **Exact-proof provider admission** *(headline)* | quote+proof received → `accept_payment` completes → `redeem_for_invocation` admits handler | parsing, quote sig, expiry+binding, replay claim, **mock verify+settle (zero-delay)**, verification-chain, billing commit+publish, redemption | external rail latency only |
| 3 | **Paid invocation delta** | paid-tool response − *equivalent* unpaid-tool response | payment admission on the paid route | everything the two routes share (dispatch, schema, serialization) |
| 4 | **Mock full lifecycle** | quote request → acceptance → redemption → handler response | the whole Net-native software path | external rail |

External rail — facilitator `verify`/`settle`, chain inclusion, finality —
is **observed telemetry**, not a bench (§B-ext).

**The headline is boundary 2, not boundary 1.** *"Proof already available"*
does **not** mean the quote is accepted, settled, and billed — a caller
holding a proof still forces the provider through `accept_payment` (sig,
replay, mock verify+settle, billing) before `redeem_for_invocation`. The
honest "how much latency does Net add" number is therefore accept **plus**
redeem (boundary 2). Boundary 1 (redeem alone) is real and worth reporting,
but it is labeled **"ready-settled invocation gate overhead"** — it is the
*shape* of the future prepaid/account/channel mode, even though today's
quote is still at-most-once.

---

## Two findings from the code that shape every number

### F1 — Admission is two gates (accepted diagnosis; boundary corrected)

- **`accept_payment`** (`engine/mod.rs:445`) — the **exact-payment
  acceptance stage**: `check_quote` (integrity + `verify_signature`,
  `mod.rs:1594`) → expiry → payload↔requirements binding → replay claim
  under lock (`mod.rs:490`) → facilitator `verify` → facilitator `settle`
  → completion + billing (`mod.rs:651`).
- **`redeem_for_invocation`** (`engine/mod.rs:1490`) — the **invocation
  gate**: optional payer-binding verify, settled/billed/frozen checks,
  capability binding, at-most-once redemption commit (one locked RMW).
- Provider policy (`ProviderAdmissionPolicy::admit`) runs at
  **`issue_quote`** (`mod.rs:425`), not at admission.

**Not "one-time settlement."** `accept_payment` is one-time *per exact paid
invocation* (the resulting quote redeems exactly once) — it is **not** a
setup cost amortized across many invocations. Call it the **exact-payment
acceptance stage**. A genuinely amortized funding stage appears only with
prepaid balance / channel drawdown (out of scope), and only there does a
cheap per-invocation admission become the relevant number.

### F2 — The admission tax is durable-store I/O, not crypto (accepted)

Engine state (`EngineState` — `mod.rs:311`) and the spend store
(`SpendPolicyFile` — `spend.rs:182`) are single JSON files mutated under a
**cross-process `fs2` advisory lock** with `sync_all` (fsync) + atomic
rename on **every** operation (`policy/store.rs:188`; used at
`engine/mod.rs:508`, `spend.rs:282`). No in-process mutex — two callers in
one process serialize like two processes. Consequences:

1. Dominant per-admission cost is `load → mutate → serialize → fsync →
   rename`, **not** signature verification.
2. Unrelated callers **do** serialize on one global lock per store file.
   The benches quantify *how much* as tail growth with concurrency.
3. This durable mutation **is** part of Net's current payment semantics.
   That is exactly why the benchmark is worth having — and why we must not
   measure it away (see D1).

---

## Decisions (revised per review)

- **D1 — State placement: operational filesystem is PRIMARY; tmpfs is a
  labeled diagnostic floor.** The durable file transaction is the current
  product path; running on tmpfs measures an environment where durability
  is unusually cheap, not "the true CPU tax." So:
  - **Primary** controlled result: the ordinary temp dir on the bench host
    (whatever `std::env::temp_dir()` resolves to) — the *complete* current
    admission cost.
  - **Secondary** diagnostic: tmpfs, run only when the operator **opts in
    and labels it** (`NET_PAY_BENCH_STATE_DIR` = the mount **and**
    `NET_PAY_BENCH_STATE_TMPFS=1` to assert memory-backed). We **never**
    infer memory-backed from a path; `temp_dir()` is **not** assumed tmpfs
    (on macOS it is not).
  - Every row reports: absolute state path, memory-backed (y/n as
    asserted), filesystem type where cheaply available, **state bytes
    before** and **after** the measured op.
- **D2 — Headline = boundary 2 (accept + redeem).** Boundary 1 (redeem
  alone) is reported separately as "ready-settled invocation gate
  overhead." (The v0.1 "redeem = headline" is withdrawn.)
- **D3 — Harness split (modified): custom hdrhistogram harness for ALL
  stateful cases**, criterion only for stateless/repeatable ones.
  - *Custom harness (stateful, single-use, fixed cardinality):*
    `accept_payment` success, `redeem` success, both duplicate storms, the
    paid-invocation delta, spend contention.
  - *Criterion (stateless / repeatable):* quote-integrity verify,
    invalid-signature rejection, canonical decode, unknown-quote redemption
    denial, expired-quote rejection.
  - Rationale: successful accept/redeem consume persistent state and are
    single-use; they need fresh prepared inputs while **holding store
    cardinality constant** — a protocol criterion's stateless `iter` loop
    cannot express.

---

## Fixtures & the stateful sampling protocol

Payment operations are **stateful and single-use**; the nRPC suite's
"just take 100 000 samples" protocol does **not** transfer.

- **Store cardinality is a controlled axis, not a side effect of sample
  count.** If a bench mints one quote per measured invocation, then sample
  count → records in store → JSON parse/serialize/fsync cost, and the
  result becomes a function of how many samples were requested. Forbidden.
- Each stateful row **prepares a fixed-cardinality store before timing**
  and holds it across the timed batch. Successful redemption may flip an
  existing `redeemed` field but must **not grow the record count**.
- Cardinality cases: **1 / 100 / 1 000** (redemption, contention).
  **10 000** is an *explicitly slow, opt-in diagnostic* only, run after
  measuring setup cost — seeding it via 10 000 durable `check_and_reserve`
  calls is ~quadratic in bytes written. If 10 000 is strategically needed,
  build a **deterministic fixture generator**; do **not** expose private
  store structures just to ease the bench.
- Every stateful row reports: record count / approval count, serialized
  file bytes, quote payload bytes, and **fixture-prep time (outside** the
  measured op).

---

## Metadata — every payment row reports

sample count · warm-up count · concurrency · runtime worker count · store
cardinality (records) · state bytes before/after · filesystem/path class +
memory-backed · mock-facilitator delay · **binding-signature on/off** ·
billing sink on/off.

- **Binding signature is its own axis for redemption** — ed25519 verify is
  optional and changes the gate cost.
- **Throughput is three numbers, not one:** attempts/s · admissions/s ·
  successful-unique-payments/s. (A duplicate storm yields high attempts/s
  but one admission — reporting a single "throughput" would lie.)

---

## Bench targets

### B1 — `benches/admission.rs` — exact-proof admission + rejection matrix

**Stateful (custom harness):** boundary-2 headline = `accept_payment`
success **then** `redeem_for_invocation` admit, plus boundary-1 = redeem
alone, at fixed store cardinality. Report **totals only** — acceptance
total and redemption total. **No internal sub-cost breakdown** (see below).

**Stateless (criterion), each a separate row with its own decision path:**

| case | input | expected |
|---|---|---|
| bad signature | corrupted provider sig on the quote | `Rejected{BadQuote}` — must **not** reach state file or facilitator (adversarial cost boundary) |
| payload mismatch | payload accepts different requirements | `Rejected{PayloadMismatch}` |
| verify rejected | mock facilitator returns invalid | `Rejected{VerifyRejected}` |
| expired | `now ≥ expires + tolerance` | `Rejected{QuoteExpired}` |
| already served | same quote + same completed payload | `Served` via `AlreadyServed` |
| replay | same payload under a *different* quote | `Rejected{Replay}` |
| quote already paid | same quote with a *different* payload | `Rejected{QuoteAlreadyPaid}` |
| in-progress | concurrent duplicate while the first is active | `InProgress` |

**No sub-cost breakdown.** A bench is a separate crate and cannot time the
internal claim RMW / completion RMW / billing construction / publish mark /
individual fsync spans. We report **total acceptance** and **total
redemption** only (v0.1-restrained). If a breakdown is ever needed, it
comes from explicit internal tracing spans + a profiling subscriber, or
justified production observability — **never** by subtracting unrelated
microbenchmarks.

### B2 — ready-settled redemption gate *(part of `admission.rs`)*

`redeem_for_invocation` on already-settled quotes (boundary 1), custom
harness. **Fixed store-cardinality axis 1 / 100 / 1 000** (records prepared
before timing; the timed redeem flips `redeemed` but does not grow the
count). **Binding-signature axis on/off.** State bytes before/after per row.

### B3 — `benches/mesh_paid_invoke.rs` — paid invocation delta *(feature `mesh`)*

**Apples-to-apples:** the same application surface both sides —
`serve_tool` (unpaid) vs `serve_tool_paid` (paid) with **identical**
request/response types, handler body, and transport config; the payment
gate is the *only* difference. (Alternatively: install the same low-level
nRPC handler twice, gate one route.) We do **not** compare `serve_rpc_typed`
against a paid tool and call the difference payment overhead — that delta
would also carry RPC-vs-tool dispatch, metadata/schema, and wrapper-path
differences. `delta = paid − unpaid` is then attributable to admission.

**Fixed state cardinality** (control it; don't let sample count drive
store size). concurrency **1 / 16 / 128**. Warm-up excluded (metadata
handlers install lazily on first serve; reply-sub propagation —
`mesh_paid_capability_e2e.rs:110`). Full metadata row.

### B4 — duplicate acceptance storm *(part of `admission.rs`)*

N concurrent `accept_payment` on the **same quote + payload**. Invariant:
facilitator `verify` once, `settle` once, **one** fresh billing event;
retries return the **same** billing event; no duplicate quote / payload /
transaction record. **Timing-tolerant:** contenders may first receive
`InProgress` — do not require every call to return `AlreadyServed`
immediately; retry after completion, then require the same billing event.
Report attempts/s vs successful-unique-payments/s (= 1).

### B5 — duplicate redemption storm *(part of `admission.rs`)*

N concurrent `redeem_for_invocation` for the **same settled quote**.
Invariant: exactly **one** `Admitted`; all others `AlreadyRedeemed`; the
bench wrapper invokes the handler **only** for `Admitted`; handler counter
ends at exactly **1**. (`redeem` does not return billing, so "same billing"
belongs to B4, not here.) Report admissions/s (= 1) vs attempts/s.

### B6 — `benches/mock_lifecycle.rs` — two numbers *(feature `mesh`)*

`CallerPaymentFlow::run()` reaches a paid caller decision + billing proof;
it does **not** by itself redeem the quote and run the paid handler. So:

- **quote-to-billing** — `run()` → billing receipt. Label: *"mesh payment
  lifecycle through billing receipt."*
- **quote-to-handler-response** — `run()` → paid tool invocation → redeem
  → handler response. The **complete** paid-capability lifecycle.

Both are useful; the first is **not** "full paid invocation." Plus an
in-process variant via `InProcessProvider`. Header labels it a software
path, not an x402/chain number.

### B7 — `benches/spend_contention.rs` — spend-policy contention

Concurrent `check_and_reserve` (`policy/spend.rs:242`) on one shared store
(pattern: `tests/spend_policy.rs:473`). Custom harness. Axes: same vs
different capability; **cardinality 0 / 100 / 1 000** (opt-in slow 10 000).
Quantifies the fs2-lock serialization and JSON-size degradation. Report
approval count, file bytes, quote payload bytes, fixture-prep time.

### B-ext — external-rail telemetry *(not a bench)*

facilitator `verify` / `settle`, chain inclusion, finality, timeout/retry
— observed via `http-facilitator` + `live-testnet` conformance
(`tests/live_testnet_conformance.rs`, `payments-live.yml`), reported as
rail performance, never in a headline. This plan only documents the split.

---

## Cargo & CI

- `autobenches = false` under **`[package]`** (not `[lib]`) — done.
- `criterion` (async_tokio) + `hdrhistogram` dev-deps; `[[bench]]` targets
  `harness = false`; mesh benches `required-features = ["mesh"]` — done.
- **CI benchmark-rot gate** in `.github/workflows/ci.yml`, appended to the
  existing `net-payments` step (shares the sdk cargo cache), compiling —
  not running — every bench:

  ```
  cargo bench -p net-payments --bench admission --no-run
  cargo bench -p net-payments --bench spend_contention --no-run
  cargo bench -p net-payments --features mesh --bench mesh_paid_invoke --no-run
  cargo bench -p net-payments --features mesh --bench mock_lifecycle --no-run
  ```

---

## Phases (revised; each phase = one commit)

Kyra's #1 strategic priority — **capability propagation + scheduler
reaction** — is **out of scope for this crate** (the `MESH_SCHEDULER_*` /
event-bus workstream). It leads the strategic story but does not live here.

- [ ] **P0 — Correct boundaries + state-placement labels.** This doc (v0.2).
- [ ] **P1 — Harness, fixed-cardinality state fixtures, targeted CI
      compilation.** `bench_common` (operational-primary state placement +
      labeling + bytes reporting + fixture builders), Cargo wiring, the
      four `--no-run` CI gates, a stateless smoke bar so `cargo bench
      --no-run` is green.
- [ ] **P2 — Acceptance + redemption totals + corrected rejection matrix.**
      Boundary-2 headline and boundary-1 gate (totals only); the eight-row
      stateless matrix.
- [ ] **P3 — Separate acceptance & redemption duplicate storms** (B4, B5).
- [ ] **P4 — Equivalent paid-tool vs unpaid-tool mesh delta** (B3).
- [ ] **P5 — Spend contention at 0 / 100 / 1 000** (+ opt-in slow 10 000) (B7).
- [ ] **P6 — Quote-to-billing + quote-to-handler-response mock lifecycle** (B6).
- [ ] **P7 — External-rail telemetry documentation** (B-ext).

**Acceptance for P1–P6:** the relevant `cargo bench --no-run` compiles
clean; each committed bench prints its p50/p95/p99 + throughput table with
the full metadata row (state path/bytes/memory-backed, cardinality, sample
+ concurrency); the storm invariants assert (B4 same billing; B5 handler
counter = 1); no headline number blends external rail latency or hides
durable-store cost.

---

## Out of scope (say where it stands, don't invent)

- **Real facilitator / chain latency as a headline** — observed rail
  telemetry (B-ext), `live-testnet`-gated.
- **Per-scheme micro-benches (EVM/SVM/XRPL signing)** — not strategically
  useful; the scheme is external and the signer key never enters the
  process (doctrine 4).
- **Prepaid-balance / account / channel-drawdown admission (Mode E)** — the
  regime with a genuinely amortized funding stage and a cheap
  per-invocation admission. Not built yet; boundary 1 previews its *shape*
  only.
- **Capability propagation + scheduler-reaction bench** — Kyra's #1
  priority, but the `MESH_SCHEDULER_*` workstream, not this crate.
