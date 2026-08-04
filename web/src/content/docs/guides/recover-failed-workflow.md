---
title: Recover a failed workflow
description: "Use typed failure outcomes, retry contracts, provider selection, and task state without guessing whether an external effect occurred."
---
# Recover a failed workflow

Recovery starts by identifying what failed and whether the operation may already
have produced an external effect. A transport error, provider rejection, handler
failure, and unknown outcome require different responses.

nRPC exposes typed failures such as unavailable provider, deadline exceeded,
cancellation, and handler error. The application combines that result with the
capability's idempotency and verification contract before retrying or selecting
another provider.

## Retry a known transient failure

```rust
use std::time::{Duration, Instant};
use net_sdk::mesh_rpc::{CallOptions, CallOptionsTyped};
use net_sdk::mesh_rpc_resilience::RetryPolicy;

let resp: SummarizeResp = caller.call_typed_with_retry(
    provider_node_id,
    "summarize",
    &req,
    CallOptionsTyped {
        raw: CallOptions {
            deadline: Some(Instant::now() + Duration::from_millis(500)),
            ..Default::default()
        },
        ..Default::default()
    },
    &RetryPolicy::default(),
).await?;
```

`RetryPolicy` applies bounded attempts and backoff to failures classified as
retryable. It does not make an operation idempotent. For calls that can change an
external system, supply an idempotency key or reconcile an unknown outcome before
trying again.

## Hedge only when duplicate execution is acceptable

A hedged call starts a second provider after a delay and takes the first result:

```rust
use net_sdk::mesh_rpc::CallOptionsTyped;
use net_sdk::mesh_rpc_resilience::HedgePolicy;

let resp: SummarizeResp = caller.call_service_typed_with_hedge(
    "summarize",
    &req,
    CallOptionsTyped::default(),
    &HedgePolicy::default(),
).await?;
```

Use hedging for read-only or explicitly deduplicated operations. Do not hedge an
unprotected payment, order, device mutation, or other effect that may execute twice.

## Stop sending work to an unhealthy provider

A long-lived `CircuitBreaker` opens after repeated failures and rejects new calls
to that target during a cooldown:

```rust
use net_sdk::mesh_rpc_resilience::{CircuitBreaker, CircuitBreakerConfig};

let breaker = CircuitBreaker::new(CircuitBreakerConfig::default());
```

The breaker limits repeated waits and load against a failing provider. It does not
verify whether prior calls took effect.

## Select another provider

A substitutable capability may have several providers. Calling by service name
allows the mesh to choose among the providers currently announced for that service:

```rust
let resp: SummarizeResp = caller
    .call_service_typed("summarize", &req, CallOptionsTyped::default())
    .await?;
```

Calling a pinned node ID intentionally bypasses that selection. The end-to-end
failover path is exercised in
`adapters/mcp/tests/serve_end_to_end.rs` by
`invoke_fails_over_when_the_primary_provider_goes_down`.

Provider failover is safe only when another provider is authorized and capable of
continuing the same operation. Stateful work may require a durable task record,
lease, or artifact handoff rather than a fresh call.

## Recover multi-step work from task state

For staged or long-running work, publish each transition through the task lifecycle
rather than reconstructing it from logs:

```text
accepted
running
artifact_written
provider_lost
rescheduled
verified
```

The event trail shows which stage completed, which provider owned it, and which
artifacts already exist. Recovery can then continue from known state instead of
repeating the whole workflow.

See [Task lifecycle](/docs/guides/task-lifecycle) and
[Submitted is not completed](/docs/guides/submitted-is-not-completed).
