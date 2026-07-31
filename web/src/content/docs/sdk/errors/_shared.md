---
title: Errors
description: Branch on typed failure, distinguish ambiguous execution, and retry only when the capability contract permits it.
boundary: /docs/sdk/c/errors
boundaryLabel: C — return codes and the NET_ERR_ table
---

# Errors and Recovery

Failure is a typed outcome, not a silence. Every binding gives you enough
structure to decide, per failure, whether to retry, reroute, or stop — and the
rule underneath all of them is the same.

> **Retry only when the typed result and capability contract make another attempt
> safe.**

A pre-execution backpressure result can permit a delayed retry when it establishes
that the provider did not admit the work. Retrying a serialization failure reruns
the bug. Retrying a denial re-asks a question already answered. Retrying a deadline
may run the work twice.

## The five things a caller has to be able to witness

An authority boundary you cannot observe is not an authority boundary. These five
are the minimum an agent needs to behave correctly, and each fragment below
demonstrates the ones its binding exposes.

**1. An unauthorized invocation is denied.** The caller gets a distinct, typed
refusal — not a timeout, not a generic internal error. Your code must be able to
tell _you may not_ from _nobody answered_, because the correct response differs:
one may need a credential, while the other requires a routing, availability, or
deadline decision.

**2. A grant expires or is revoked, and the next call stops working.** Authority is
checked at call time against the authenticated caller origin. There is no cached
permission to go stale and no announcement to wait for — the call after the
revocation fails, and it fails as an authorization error rather than a routing
one.

**3. A deadline elapses.** You learn no answer arrived in time. You do not learn
whether the work ran. See below.

**4. A typed remote error crosses the binding boundary.** A handler that fails with
a status and a body delivers _that_ status and body to the caller, in the caller's
language, rather than a stringified exception. A handler that fails with an
untyped error delivers `Internal` — which is a decision the handler author made,
usually without noticing.

**5. Execution is ambiguous, and the binding says so.** A call that timed out, was
cancelled, or lost its transport mid-flight has an unknown outcome.

## Ambiguous execution is the one that costs money

When a deadline elapses the caller emits a cancel for that call id, so a
cooperating provider can drop the in-flight handler. "Can drop" is not "did not
run." The request may have arrived, executed, had an effect, and failed only to
get its reply back to you.

A retry after a timeout can duplicate an external effect. Use one of these
contracts:

1. **Make the operation idempotent.** Then a duplicate is harmless and this whole
   section stops mattering.
2. **Carry an idempotency key** the provider deduplicates on.
3. **Accept the duplicate**, deliberately, and write down that you did.

What does not work is treating a timeout as a failure and retrying, then treating
the second timeout the same way. That is how one intended effect becomes three.

## Retry, hedge, and circuit breaking

All four bindings offer the same three strategies over a raw call, and they solve
different problems:

- **Retry** — bounded attempts with backoff, on _retryable_ failures only. Fixes a
  transient.
- **Hedge** — race several providers when latency matters more than duplicate
  work. Costs duplicate execution by design.
- **Circuit breaker** — fast-fail a provider that is failing, instead of paying its
  deadline on every call. Fixes a sick provider, not a sick request.

Calling by **service or tool name** rather than a pinned node id is what makes
failover possible at all: the mesh picks a provider, so a substitutable capability
survives the loss of the primary. Pinning a node id opts out of that.

The end-to-end patterns are in
[Recover a Failed Workflow](/docs/guides/recover-failed-workflow), and the
wire-level codes every binding's taxonomy wraps are in
[Error Codes](/docs/reference/error-codes).

## The one rule, expanded

> Retry backpressure, and a transient no-route briefly. Treat serialization and
> config failures as bugs. Treat authorization failures as "get a new credential,"
> never as "try again." Treat shutdown, not-connected and closed streams as state
> changes that retrying will not undo. And treat a deadline as _unknown_, not as
> failure.
