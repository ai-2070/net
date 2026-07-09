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

The per-day counter is a **lock-held read-modify-write** on the shared store:
coarse and correct beats clever and racy, so two processes hammering the cap can
never overspend.

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
