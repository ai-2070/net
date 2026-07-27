---
title: "Spend Policy & Approvals"
description: The model requests an invocation; it does not decide whether to spend.
---
# Spend policy & approvals

The model requests an invocation; it does not decide whether to spend. That
decision lives in a **spend policy engine** that runs caller-side, in shared
policy state, before anything leaves. Approvals render in the agent's UX, but the
verdict is policy, not prompt.

## The policy engine decides, not the model

Every paid invocation clears the spend policy first. The engine returns one of
three structured outcomes:

- **allowed** — policy admits the spend silently (and the per-day counter has
  already reserved it);
- **requires payment approval** — policy wants a human: a pending approval record
  is written to the shared store, and the caller surfaces the quote + reason +
  how to approve;
- **denied** — policy refuses outright (no approval path — e.g. a network that
  isn't enabled).

## Budgets

Limits are per `(network, asset)`, in atomic units of the allowed asset:

- **`max_per_call`** — require approval above this single-call amount;
- **`max_per_day`** — require approval once the per-day total would exceed this;
- **`allowed_networks` / `allowed_assets`** — the enablement allowlist.

### Setting a cap

The engine is constructed against a store path and a profile, and `configure`
takes the write under the store's lock:

```rust
use net_payments::core::units::AtomicAmount;
use net_payments::policy::spend::{SpendPolicyEngine, SpendProfile};

let policy = SpendPolicyEngine::new(&spend_path, SpendProfile::Production);
policy
    .configure(|defaults, _| {
        defaults.max_per_call = Some(AtomicAmount::from_u128(1000));
    })
    .await?;
```

Amounts are `AtomicAmount` — atomic units of the asset, never a float. The
profile is the fail-closed switch: `SpendProfile::Production` makes even mock
spends require approval, `SpendProfile::DevTest` auto-allows the mock network
under the limits. Neither profile lets a real network spend unless it's listed
in `allowed_networks`.

### Approving a held quote

When policy returns `RequiresPaymentApproval`, nothing has been spent and the
pending record carries the quote's canonical bytes. Approval is an
**operator-only** verb, deliberately reachable from a different surface than
the flow that asked for it:

```rust
let policy = SpendPolicyEngine::new(&spend_path, SpendProfile::Production);
let approved: bool = policy.approve(&quote_id).await?;
```

Re-running the flow after approval redeems the exact quote the human saw.
Approving quote *X* never authorizes a later quote *Y*, so a caller cannot get
an approval for a cheap quote and spend it on an expensive one.

The per-day counter is a **lock-held read-modify-write** on the shared store:
coarse and correct beats clever and racy, so two processes hammering the cap can
never overspend.

### What the store does, and doesn't, write

A decision that changes nothing durable **skips the write entirely**. Load,
decision, and conditional save all stay under the same advisory lock, so the
check-and-set is still atomic and at-most-once behavior is unchanged — only the
serialize-fsync-rename is skipped.

This is a security property, not a micro-optimization. Every redemption *denial*
used to pay a full durable write, which meant a caller spraying quote ids could
force one global-lock acquisition plus one fsync per attempt and collapse
throughput. Denials are cheap now, and if you extend the engine, keep them that
way: a durable write on a read-only branch is a denial-of-service regression.

Two edges are worth knowing if you touch this code. A nominally "clean" denial is
still dirty when housekeeping pruned an expired counter inside the same
transaction. And re-requesting approval for a quote that already has an identical
pending record changes nothing, so it stays clean. Completion, claim release, and
billing republish are separate calls and remain unconditional.

### Don't promise throughput from logical independence

Contention benchmarks put the atomic accounting unit at the
`(day, network, asset)` counter row — distinct capabilities sharing one counter
genuinely contend, different assets share nothing, and approvals are a separate
quote-keyed state machine.

But logically independent traffic currently benchmarks *identically* to maximum
same-counter contention. The coupling is the global file lock, not accounting
authority. Sharding by asset or capability buys nothing today, so don't design
around per-capability parallelism until the store backend changes.

## Fail-closed by default

- **Real networks deny by default.** A real network spends only when explicitly
  listed in `allowed_networks`; an empty allowlist enables nothing real, and no
  profile, flag, or approval bypasses network enablement.
- **Mock auto-allows only under a dev/test profile** (or an explicit unsafe
  flag). In the production profile, every mock spend still needs an approval — so
  demos don't train the policy path wrong.

## The approval surface (operator, not model)

Approval mirrors the consent split. The engine (model-reachable) writes only a
**pending** record when it returns *requires payment approval*. Moving a record
to **approved** is an **operator-only** verb — the model must not approve its own
future spending. The gateway exposes the operator verbs `approve` / `reject` /
`pending` / `spent_today`; approval of quote *X* authorizes *X*, never a later
quote *Y* (the pending record carries the quote's canonical bytes).

## Roadmap: delegation-chain budgets

Per-delegation-chain budgets — where a child agent's budget is bounded by its
parent's remaining allowance (*child ≤ parent's remaining, always*) — are a
**forward-looking doctrine, not shipped behavior** (P5 territory). Today the
engine enforces per-`(network, asset)` limits + the approval split above; treat
chain inheritance as roadmap, not a current guarantee.
