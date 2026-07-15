# Payments — external-rail telemetry contract (P7)

> **This is a telemetry / observability contract, not a benchmark.** It
> defines how real facilitators and chains will be *observed* in operation —
> and, above all, how those observations stay **separate** from the
> controlled local benchmark tables. It contains **no numbers**: no baseline,
> threshold, or SLO is asserted here, because none exists without real
> production / staging samples. Inventing one is forbidden.
>
> The controlled suite (P2–P6) measures Net's *local* payment path with an
> in-process zero-delay mock facilitator, so external rail latency is excluded
> by construction. External settlement latency is a property of the
> facilitator, the network, and the time of day — it is **observed rail
> performance**, never a Net benchmark result.

## The reporting contract (the load-bearing rule)

- **Public controlled benchmark tables EXCLUDE external rail latency.** P2–P6
  and their docs report Net's local admission/lifecycle cost only.
- **Operational external-rail reports are SEPARATE.** They are labeled by
  facilitator / network / mode, include retries / timeouts / failures, and are
  **never** combined with local storage latency into one "mechanism" number.
- A single blended number would be a lie: it would mix a deterministic local
  transaction with a nondeterministic network round trip.

## The two-span stage model

Instrumentation keeps **provider-local** and **external-rail** spans distinct.
A stage a facilitator does not expose (many expose neither inclusion nor
finality) is simply **absent** — it is never synthesized or defaulted.

### Provider-local spans (deterministic, Net-owned)

```
request / proof received
pre-state validation
claim persisted
facilitator call dispatched        ── boundary to the external span
facilitator response received      ── boundary from the external span
local settlement completion persisted
billing identity created / published
redemption admitted
handler started
handler responded
```

### External-rail spans (nondeterministic, facilitator/chain-owned)

```
verification request
settlement submission
settlement acknowledgement
inclusion / confirmation        (only if the rail reports it)
finality                        (only if the rail defines it)
retry / backoff
```

The `facilitator call dispatched → facilitator response received` local
boundary is exactly where an external span begins and ends; the external
duration is attributed to the rail, the remainder to Net.

## Required telemetry fields

Per payment operation, at minimum:

| field | notes |
|---|---|
| facilitator | identity of the verify/settle endpoint |
| network | CAIP-2 |
| asset | CAIP-19 |
| payment mode | e.g. exact |
| binding mode | bearer / bound |
| operation | `verify` \| `settle` |
| outcome | success / rejected / error / timeout |
| retry count | attempts before terminal outcome |
| timeout count | deadline exceedances |
| provider-local duration | Net's own work on this op |
| external duration | **only when the rail reports it** |
| HTTP / RPC status class | where applicable (2xx/4xx/5xx, RPC code) |
| settlement status | rail-reported settlement state |
| confirmation / finality depth | **only if meaningful for the rail** |
| error category | mapped, not raw provider text |
| trace / correlation id | deterministic, non-secret (below) |
| billing-event id | **privacy-safe form only** (below) |

## Metric-label cardinality

The following must **not** become high-cardinality metric (e.g. Prometheus)
labels — they belong in structured traces / logs with redaction:

- raw quote ids;
- transaction hashes;
- wallet / account addresses;
- proof bytes;
- billing ids.

Metric labels stay low-cardinality (facilitator, network, asset, mode,
operation, outcome class). Per-payment identifiers live in traces, correlated
by a non-secret id.

## Security & privacy (required)

- **no credentials**, **no bearer tokens**, **no raw proofs** in telemetry;
- **no plaintext account or shipping information**;
- transaction identifiers **redacted or access-controlled**;
- correlation ids are **deterministic only where they do not expose payment
  secrets** (e.g. a salted/derived id, never the raw quote id or tx hash);
- the billing-event id appears only in a **privacy-safe form** (redacted /
  hashed / access-controlled), never as a plaintext metric label.

> Credentials remain **[REDACTED]** in every projection — config objects,
> logs, traces, and metrics alike.

## The future real-rail table (shape only)

When real production / staging samples exist, an external-rail report may show,
**per facilitator / network / mode**:

- p50 / p95 / p99 / max — **only when statistically valid**;
- success / failure counts;
- timeout rate;
- retry distribution;
- the sample window;
- facilitator / network identity;
- the confirmation / finality policy in force.

Until such samples exist, this table is **empty by design**. Do not invent a
baseline or threshold.

## Where this rides in the code

The env-gated live conformance path (`tests/live_testnet_conformance.rs`, the
`payments-live.yml` workflow) is where real facilitator/chain observations are
gathered at network-enablement time. This contract governs how those numbers
are recorded and reported — separately from, and never blended into, the
controlled P2–P6 tables.

## Related documents

- Controlled baselines: [`payments-admission-matrix.md`](payments-admission-matrix.md)
  (P2), [`payments-redeem-write-amplification.md`](payments-redeem-write-amplification.md)
  (the defect + fix + audit), [`payments-duplicate-storms.md`](payments-duplicate-storms.md)
  (P3), [`payments-spend-contention.md`](payments-spend-contention.md) (P5),
  [`payments-paid-vs-unpaid.md`](payments-paid-vs-unpaid.md) (P4),
  [`payments-mock-lifecycle.md`](payments-mock-lifecycle.md) (P6).
- Decision: [`../plans/PAYMENTS_STORAGE_DISPOSITION.md`](../plans/PAYMENTS_STORAGE_DISPOSITION.md).
- Plan: [`../plans/PAYMENTS_BENCHMARKS_PLAN.md`](../plans/PAYMENTS_BENCHMARKS_PLAN.md).
