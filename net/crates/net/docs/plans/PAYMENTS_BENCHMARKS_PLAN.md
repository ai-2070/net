# Payments Benchmarks Plan

> Establishes the first payment-specific benchmark targets in `net-payments`.
> **Diagnosis-first and restrained by design**: the headline is one number —
> *how much latency does Net add when a payment proof is already available?* —
> and external facilitator / chain latency is **never** blended into it.
> Companion to the SDK's nRPC bench suite (`sdk/benches/nrpc_*.rs`); reuses its
> harness conventions (`nrpc_common`, criterion + hdrhistogram split).

## Why so few benches

Payment performance matters as **the additional latency between an
otherwise-ready invocation and handler execution** — the admission tax, nothing
else. We are not building a benchmark zoo around every payment object or scheme.
Cold exact-per-request settlement for strangers is dominated by the external
rail; the local admission benchmark only becomes the economically relevant
number once high-volume callers use prepaid balances / contract accounts /
channel drawdown. So the suite is small, and the public framing is restrained.

**The public split — three layers, never merged into one number:**

| Layer | Measurement type | Where |
|---|---|---|
| Net admission overhead | controlled benchmark | `benches/admission.rs` |
| Mesh quote/pay round trips | controlled benchmark | `benches/mesh_paid_invoke.rs`, `benches/mock_lifecycle.rs` |
| External settlement latency | **observed rail telemetry** | `live-testnet` conformance, not a bench |

Metrics everywhere: **p50 / p95 / p99 + throughput**. Invalid inputs are
measured on purpose — payment admission is an adversarial public surface, and
rejection must stay cheap and bounded.

---

## Two findings from the code that shape every number

Both are load-bearing. They come from reading `engine/mod.rs`, `policy/spend.rs`,
`policy/store.rs`, and `flow/mesh.rs`.

### F1 — Admission is two gates, not one

Kyra's pipeline ("proof decoded → verified → replay → policy → admitted") is
split across two `PaymentEngine` methods, and provider policy runs at a third
point:

- **`accept_payment`** (`engine/mod.rs:445`) — the heavy, **one-time** settle
  path: `check_quote` (integrity + `verify_signature`, `mod.rs:1594`) → expiry →
  payload↔requirements match → replay/idempotency **claim under lock**
  (`mod.rs:490`) → facilitator `verify` → facilitator `settle` → completion +
  billing (`mod.rs:651`). This is where money moves.
- **`redeem_for_invocation`** (`engine/mod.rs:1490`) — the **per-invocation**
  handler gate: one locked state read-modify-write that flips a settled quote to
  `Admitted` right before the handler runs, plus optional ed25519 binding-sig
  check.
- Provider policy (`ProviderAdmissionPolicy::admit`) runs at **`issue_quote`**
  (`mod.rs:425`), *not* at admission.

**Consequence for the headline:** under the framing *"proof already available,"*
the honest per-request number is **`redeem_for_invocation`**. `accept_payment` is
reported as a **one-time settlement** cost, never folded into the per-invoke tax.
Reporting them as one number would be dishonest.

### F2 — The admission tax is I/O-bound, not crypto-bound

Both the engine state (`EngineState`: `consumed`, `consumed_transactions`,
`quotes` — `mod.rs:311`) and the spend-policy store (`SpendPolicyFile` —
`spend.rs:182`) are **single JSON files** mutated under a **cross-process `fs2`
advisory lock** with `sync_all` (fsync) + atomic rename on *every* operation
(`policy/store.rs:188`, the `mutate_json` used at `engine/mod.rs:508` and
`spend.rs:282`). There is **no in-process mutex** — two callers in the same
process serialize exactly like two processes.

Three consequences:

1. Dominant per-admission cost is `load_json + mutate + fsync + rename`, **not**
   signature verification. A bench that runs state on tmpfs measures a different
   (much smaller) number than one on disk.
2. Kyra's worry — *"whether quote/replay state, billing emission or policy locks
   serialize unrelated callers"* — is answered structurally: **yes**, there is
   one global lock per store file. The benches don't discover *whether*; they
   **quantify how much**, as tail-latency growth with concurrency.
3. The "degrades with JSON history" concern is specific: the spend store's
   `approvals` map grows unbounded (each `ApprovalRecord` carries a full base64
   quote, `spend.rs:169`), and every op re-serializes the whole file. The
   `counters` map self-prunes to ~2 days (`spend.rs:284`), so it is **not** the
   growth term.

---

## Decisions (defaults chosen; revisitable)

- **D1 — State placement.** The **headline** runs with engine/policy state on
  **tmpfs** (isolates the true Net payment CPU tax, per Kyra's "isolate the Net
  payment tax"). A **disclosed on-disk companion** column runs the same cases on
  the real filesystem (operational reality: the fsync-per-op cost a deployment
  actually pays). Every reported table names which placement it is. Env override:
  `NET_PAY_BENCH_STATE_DIR` (unset → `std::env::temp_dir()`; CI sets it to a
  tmpfs mount for the headline pass).
- **D2 — "Admission overhead" = `redeem_for_invocation`.** The per-invoke tax is
  the handler gate; `accept_payment` is reported separately as one-time
  settlement. (Follows F1.)
- **D3 — Harness split.** Criterion (`async_tokio`) for point latencies (B1
  accept/redeem/rejections, B4); custom `hdrhistogram` loop for
  concurrency/tail/throughput (B2, B3, the duplicate-storm). Same split as
  `nrpc_unary.rs` vs `nrpc_tail.rs`.

---

## Bench targets

Naming follows Kyra's numbering. B5 (duplicate-storm) folds into `admission.rs`.

### B1 — `benches/admission.rs` — provider admission overhead *(the primary bench)*

In-process, **no mesh, no network**. Mock facilitator + `AdmitAll`. Construction
recipe is `tests/native_tool_gate.rs:63`:

```rust
let engine = PaymentEngine::new(
    provider.clone(), Arc::new(MockFacilitator::new()), Arc::new(AdmitAll),
    default_mock_registry(provider.entity_id().clone()),
    dir.path().join("engine.json"),
)?;
```

Measures both gates from F1, reported separately:

- **one-time settlement** — `accept_payment` on a fresh quote+payload.
- **per-invoke tax** — `redeem_for_invocation` on an already-settled quote.

Cases (each a distinct decision path; all must stay cheap + bounded):

| case | path | expected outcome |
|---|---|---|
| valid proof | `accept_payment` happy path | `PaymentDecision::Served` |
| invalid signature | mock armed invalid / `accepted != requirements` | `Rejected{VerifyRejected}` (`mod.rs:613`) |
| expired quote | `now_ns ≥ expires_at + tolerance` | `Rejected{QuoteExpired}` (`mod.rs:456`) |
| duplicate proof | re-submit settled proof | `Served` via `Claim::AlreadyServed` (`mod.rs:577`) |
| idempotent retry | same quote, tier already met | short-circuit, no verify/settle |
| replay rejection | payload replays under a different quote | `Rejected{Replay}` / `Claim::ReplayOtherQuote` (`mod.rs:548`) |

Report a sub-cost breakdown (quote sig-verify vs claim RMW vs billing
sign+append) so the dominant term (F2) is visible.

### B5 — duplicate-storm *(folded into `admission.rs`; Kyra's flagged invariant)*

Fire the same valid proof from N concurrent tasks. Assert **and** measure:

- settlement occurs **once**;
- the handler is admitted **once**;
- every retry returns the **same** completed billing (`AlreadyServed`);
- memory stays bounded (state file size flat after first settle);
- the idempotent path is **cheaper** than first admission.

This is the money-path invariant most likely to be attacked, measured as both a
correctness assertion and a latency curve. Custom hdrhistogram harness.

### B2 — `benches/mesh_paid_invoke.rs` — paid vs unpaid nRPC *(feature `mesh`)*

Warm two-node mesh (copy `handshake` + `MeshBuilder` from
`tests/mesh_paid_capability_e2e.rs:77`). The same no-op echo handler served
twice: unpaid via `serve_rpc_typed`, paid via `serve_tool_paid` +
`EngineToolPaymentGate`. Quotes are **at-most-once** and there is **no
proof-reuse API**, so **pre-mint N distinct settled quotes in-process** (via
`issue_quote` + `accept_payment`, helper `paid_quote_id` at
`native_tool_gate.rs:25`) **outside** the timed region, then time:

```
paid invocation latency − unpaid invocation latency = admission delta
```

with payment headers `HDR_PAYMENT_QUOTE` / `HDR_PAYMENT_BINDING` on the paid
call. Concurrency axis **1 / 16 / 128** (matches nRPC bench convention). This is
where the `redeem` global-lock serialization (F2) shows up as tail growth on the
paid line while the unpaid line stays flat. Report p50/p95/p99 + throughput per
concurrency, plus the delta. Warm-up first (metadata handlers install lazily on
first serve; reply-subscription propagation — `mesh_paid_capability_e2e.rs:110`).

### B3 — `benches/spend_contention.rs` — spend-policy contention

Concurrent `check_and_reserve` (`policy/spend.rs:242`) against **one shared
store**. Pattern from `tests/spend_policy.rs:473`: N tasks, each its own
`SpendPolicyEngine` over the shared path (serialization is the fs2 lock, so this
is faithful to multi-process). Axes:

- **same capability** vs **different capabilities** (one daily budget);
- **store size: empty / 100 / 10,000** approval records — seed via
  Production-profile `check_and_reserve` (the `approvals` map is the growth term,
  F2/3).

Reports throughput + tail cost of the *no-overspend* invariant (already tested
for correctness — this measures the price of preserving it) and whether it
**degrades with JSON history**. If it does, we see it before high-frequency
agents do.

### B4 — `benches/mock_lifecycle.rs` — full lifecycle, labeled mock

Header ships the label **"in-process mock lifecycle latency — software path, not
x402/chain."** Two variants:

- **in-process**: `InProcessProvider` — quote → signed quote → reservation →
  mock settle → verify → billing → receipt.
- **mesh**: `CallerPaymentFlow::run(capability, terms)` (`flow/mod.rs:561`) →
  `CallerDecision::Paid{ quote_id, binding_sig, proof }`; the receipt is
  `proof["billing_event"]`. Proves the complete Net-native lifecycle over nRPC
  and gives a diagnostic baseline.

### B6 — external-rail telemetry *(not a bench)*

`facilitator /verify`, `/settle`, chain inclusion, finality, timeout/retry rate.
Collected via the `http-facilitator` + `live-testnet` conformance path
(`tests/live_testnet_conformance.rs`), reported as **observed rail
performance** — never merged into a headline. This plan only documents the
split; the numbers are operational telemetry gathered at enablement time.

---

## Shared harness + Cargo wiring

- **`benches/bench_common/mod.rs`** (mirrors `sdk/benches/nrpc_common`, kept out
  of Cargo auto-discovery via a directory + `autobenches = false`):
  - engine + mock construction (`build_engine(state_dir)`), respecting
    `NET_PAY_BENCH_STATE_DIR` (D1);
  - quote/proof minting (`mint_settled_quote(engine, ...) -> quote_id`);
  - the paid two-node `Pair` (paid + unpaid handler on one mesh);
  - a shared p50/p95/p99/throughput histogram reporter (hdrhistogram).
- **`payments/Cargo.toml`** `[dev-dependencies]`: add
  `criterion = { version = "0.8", features = ["async_tokio"] }`,
  `hdrhistogram = "7"`. Add `autobenches = false` to `[lib]`/`[package]`.
- `[[bench]]` targets, all `harness = false`:
  `admission` (no extra features), `spend_contention` (no extra features),
  `mock_lifecycle` + `mesh_paid_invoke` with `required-features = ["mesh"]`.

Run examples:

```
cargo bench -p net-payments --bench admission
cargo bench -p net-payments --bench spend_contention
cargo bench -p net-payments --features mesh --bench mesh_paid_invoke
cargo bench -p net-payments --features mesh --bench mock_lifecycle
```

---

## Phases (each phase = one commit)

Sequencing follows Kyra's priority. Note: her #1 (**capability propagation +
scheduler reaction**) is **out of scope for this crate** — that is the
gang-scheduler / event-bus workstream (`MESH_SCHEDULER_*` plans), not
net-payments. It leads the strategic story but does not live here.

- [ ] **P0 — Plan.** This document. *(commit: docs)*
- [ ] **P1 — Harness + wiring.** `bench_common/mod.rs`, Cargo dev-deps +
      `[[bench]]` stubs, one trivial green bar so `cargo bench --no-run` passes.
      *(commit)*
- [ ] **P2 — B1 + B5 (`admission.rs`).** The headline: accept/redeem gates, six
      cases, sub-cost breakdown, duplicate-storm invariant + latency curve.
      *(commit)*
- [ ] **P3 — B2 (`mesh_paid_invoke.rs`).** Paid vs unpaid delta at c1/16/128.
      *(commit)*
- [ ] **P4 — B3 (`spend_contention.rs`).** Contention × store-size matrix.
      *(commit)*
- [ ] **P5 — B4 (`mock_lifecycle.rs`).** In-process + mesh, mock-labeled.
      *(commit)*
- [ ] **P6 — B6 telemetry note.** Document the external-rail split + how numbers
      are gathered at enablement (no bench code). *(commit)*

**Acceptance for P1–P5:** `cargo bench --no-run` (with the relevant features)
compiles clean; each committed bench prints a p50/p95/p99 + throughput table;
the duplicate-storm invariant asserts (settles once, same billing, bounded
memory). No headline number blends external rail latency.

---

## Out of scope (say where it stands, don't invent)

- Real facilitator / chain latency as a headline number — it's observed rail
  telemetry (B6), gated behind `live-testnet`.
- Per-scheme micro-benches (EVM/SVM/XRPL signing) — not strategically useful; the
  scheme is external and the signer key never enters the process (doctrine 4).
- Prepaid-balance / channel-drawdown admission — the regime where local
  admission becomes the economically relevant number, but that mode (Mode E)
  isn't built yet.
- Capability propagation + scheduler-reaction bench — Kyra's #1 priority, but the
  `MESH_SCHEDULER_*` workstream, not this crate.
