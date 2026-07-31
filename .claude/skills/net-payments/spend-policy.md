# Spend policy — the caller-side gate, budgets, approvals

The spend policy engine (`policy/spend.rs`) is the caller's enforcement point:
**it runs before anything leaves the machine.** Same engine in every process,
same shared state, regardless of client. The model requests invocation; this
engine decides. Displaying a price never implies authorization to spend it.

Enforcement is **two-sided** — this is the *caller's* half; the provider's
half is `ProviderAdmissionPolicy` + the serve gate (`provider.md`).

## The engine

```rust
use net_payments::policy::spend::{SpendPolicyEngine, SpendProfile, SpendDecision, SpendLimits};

let engine = SpendPolicyEngine::new(policy_path, SpendProfile::Production)
    .with_unsafe_mock_auto_allow(false);   // dev escape hatch — see below
```

`SpendProfile` (default `Production`): `Production` | `DevTest`. The store at
`policy_path` is a locked per-user JSON file (pins pattern);
`policy::store::default_payment_policy_path()` gives the standard location
(`<local data>/net-mesh/payment-policy.json`).

Bindings that take the profile as a **string** (Python's `spend_profile`, the
Node / Python HTTP-402 config) all route through the one canonical parser —
`SpendProfile::parse` / `FromStr` in `policy::spend`. It accepts `"production"`
and `"dev_test"` (aliases `"dev-test"` / `"devtest"`) and rejects anything else
with `UnknownSpendProfile` naming the offending value — **no silent fallback,
no case-folding.** Don't hand-roll the string→enum match: the divergence this
single-sourcing removed was one copy (an operator verb) defaulting an unknown
profile while the flow-construction copies errored.

## Limits

```rust
pub struct SpendLimits {
    pub max_per_call: Option<AtomicAmount>,
    pub max_per_day:  Option<AtomicAmount>,
    pub allowed_networks: Vec<String>,   // CAIP-2 — empty means none allowed
    pub allowed_assets:   Vec<String>,   // CAIP-19 / x402 locator
}

engine.configure(|defaults, per_capability| {
    defaults.max_per_call = Some(AtomicAmount::from_u128(10_000));
    defaults.max_per_day  = Some(AtomicAmount::from_u128(1_000_000));
    defaults.allowed_networks = vec!["eip155:8453".into()];
    per_capability.insert("prov/expensive-tool".into(), SpendLimits { max_per_call: Some(AtomicAmount::from_u128(50)), ..Default::default() });
}).await?;
```

`configure` takes `FnOnce(&mut SpendLimits, &mut BTreeMap<String, SpendLimits>)`
— `defaults` plus per-capability overrides.

## The default posture (fail-closed)

- **Real networks deny by default.** A real network is spendable only when
  explicitly listed in `allowed_networks` *and* a signer + facilitator config
  exist for it. This is the operator's explicit production consent, and it is
  the first rung of the enablement ladder in `networks.md`.
- **The mock network auto-allows only under `SpendProfile::DevTest` or the
  explicit `.with_unsafe_mock_auto_allow(true)` flag.** Demos must not train
  the policy path wrong. In `Production` without the unsafe flag, even mock
  payments go through the approval path.

## The decision

```rust
let decision: SpendDecision = engine.check_and_reserve(&quote, &registry, now_ns).await?;
pub enum SpendDecision {   // serde snake_case
    Allowed,
    RequiresPaymentApproval { quote_id, policy_reason, approve_hint },
    Denied { policy_reason },
}
```

`check_and_reserve` is a **single locked read-modify-write**: it checks the
per-call and per-day limits *and* reserves the per-day counter atomically, so
two concurrent processes hammering `max_per_day` can never overspend (this is
a tested invariant). `SpendError`: `Store(StoreError)`, `Malformed(String)`.

If the flow fails before settlement, call
`engine.release_reservation(&quote, now_ns)` to give the reserved counter
back. `engine.spent_today(network, asset, now_ns)` reads the current
per-day total.

## Approvals — the same shape as consent

`RequiresPaymentApproval { quote_id, policy_reason, approve_hint }` mirrors the
consent-request shape exactly. The prompt renders in agent UX (Hermes,
OpenClaw, the Python gateway); **the decision lives in shared policy state, not
in the model.** The operator surface:

```rust
engine.approve(quote_id).await?;   // -> bool (flipped a pending approval)
engine.reject(quote_id).await?;    // -> bool
engine.pending().await?;           // -> Vec<String>  (quote_ids awaiting approval)
engine.approved_quote(capability).await?;   // -> Option<(quote_id, quote_bytes)>  (held, for re-run)
engine.clear_approval(quote_id).await?;
```

After `approve`, the caller flow's next `run` picks up the held approved quote
via `approved_quote()` and redeems it — no re-quote. `ApprovalState` is
`Pending | Approved`; `ApprovalRecord { state, capability, quote_b64 }`.

**Reachable from Python.** These operator verbs are now thin wrappers on
the `CapabilityGateway`: `approve_payment(quote_id)` / `reject_payment(quote_id)`
/ `pending_payments()` / `spent_today(network, asset)`, over the *same* shared
store — closing the `approve_hint` loop so an agent embedding the Python SDK can
resolve its own `requires_payment_approval` under operator policy without
leaving Python (`bindings.md`). The store, lock protocol, and transition stay in
Rust; the binding only marshals the typed result to status-JSON.

**Pinning is capability consent, not spending consent** — a pinned paid tool
still hits spend policy on every invocation.

## Budgets & delegation (the doctrine, forward-looking)

Per-agent and per-delegation-chain budgets ride the same engine. The
inheritance rule is doctrine: **child budget ≤ parent's remaining, always** —
spend rolls up the chain. Spending on credit is still spending; caller-side
policy applies regardless of settlement mode. Per-subagent spend attribution
via the delegation chain is not wired up yet; the enforcement primitive is
this engine.

## Why the store is locked, not a daemon (yet)

Spend counters are a lock-held RMW on the shared store — honest and correct at
v1 rates. Rolling budgets are the first legitimate case for a shared-daemon
backend; it would migrate **behind this same API**, invisible to callers. Do
not reach for a daemon before contention demands it.

### Read-only decisions no longer rewrite the store

A denial that changes nothing durable now **skips the write entirely**
(`store::mutate_json_if_changed`: the closure returns `(verdict, dirty)` and
the store is serialized + fsync'd + renamed only when `dirty`). Load, decision,
and conditional save stay under the **same advisory lock**, so the
check-and-set is still atomic and at-most-once is unchanged.

This started as a real DoS surface, not a micro-optimization: every redemption
*denial* used to pay a full durable write, so a caller spraying quote ids
forced one global-lock + fsync per attempt and collapsed throughput to ~20/s
under concurrency. The same audit then extended to `accept_payment`'s claim
transaction and to `check_and_reserve`.

The dirty determination is per-branch, and two of the cases are subtle enough
to be worth knowing if you touch this code:

- **A "clean" denial can still be dirty.** Housekeeping prunes counters past
  the retention horizon *inside* the same transaction — if retention removed
  anything, the transaction must persist even though the verdict was a denial.
- **An identical already-pending approval is a no-op.** `require` sets dirty
  only when it actually *inserts* a new pending record; re-requesting approval
  for the same quote id changes nothing.

Writes that remain unconditional: completion, `release_claim`, and billing
republish are separate calls, so a `verify_rejected` still persists **both**
its claim and its release — both are semantically real.

### The quote is canonicalized lazily, inside the transaction

`require` holds the quote's canonical bytes beside a **new** pending approval so
a post-approval retry redeems that exact provider-signed quote. That work — a
full `to_value`, a recursive canonical write, and a base64 over the whole
envelope — happens only on the branch that needs it. The `Allowed` path and the
already-pending observation (the common case under a duplicate storm) do none of
it.

Two things follow that are easy to get wrong if you touch this:

- **`check_and_reserve` can return `SpendError::Malformed`.** The store closure
  has no error channel (`FnOnce(&mut T) -> (R, bool)`), so a canonicalization
  failure parks in an `Option<String>` and is re-raised *after* the transaction.
  No approval is recorded on that path — an approval holding no quote bytes is
  one a retry cannot redeem — and nothing is reserved. Don't "simplify" this
  into a `Denied` decision: a denial is a policy verdict, and this is not one.
- **It runs under the cross-process lock**, where it previously ran ahead of the
  transaction. Accepted deliberately: approvals are operator-gated and rare, and
  the hot paths now do none of the work. Don't move it back out behind a
  speculative check of `s.approvals` — that costs a second read on every call to
  save a rare one.

### The store file is compact JSON

`save_json` writes `to_vec`, not `to_vec_pretty`: the store is machine-shared
state, and pretty-printing cost ~11% more bytes to serialize, write, and fsync
on *every* dirty mutation, on a path whose cost is already linear in file size.
Both formats parse identically, so a rolling upgrade never sees a corrupt store.
If you are eyeballing `payment-policy.json` or `payment-engine.json`, pipe it
through `jq`. **This is not the canonical envelope encoding** — that is
`core::canonical`, a different encoder pinned by the cross-language golden
vectors, and it is untouched by this.

### What the contention benchmarks actually found

Measured, not designed: the atomic accounting unit is the
**`(day, network, asset)` counter row**. Distinct capabilities sharing one
`(day, asset)` counter genuinely contend on it; different assets have no shared
counter at all. Approvals are a separate quote-keyed state machine.

But **logical independence buys you nothing today.** Fully independent traffic
(different assets, no shared counter) benchmarks *identically* to maximum
same-counter contention — ~26 ops/s and ~235 ms tail at concurrency 16, ~20/s
and ~3.1 s at 128. The coupling is the **global file lock**, not accounting
authority. So: don't promise a customer that sharding by asset or capability
will scale spend checks, and don't design around per-capability parallelism
until the store backend changes. No partitioned engine is authorized yet.

## Further reading

- [Spend Policy & Approvals](https://ai2070.net/docs/payments/spend-policy-and-approvals)
