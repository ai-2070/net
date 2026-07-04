# Recover a Failed Workflow

Discovering and invoking a capability is only half the job. The other half is what
happens when the call fails — because in a mesh of providers that come and go,
*something will*. This is where the agentic model earns its keep over a bare
request/response call: **failure is a typed outcome you can act on**, not a silence
behind a green button ([Submitted Is Not Completed](/docs/guides/submitted-is-not-completed)).

## Failure is a fact, not a timeout

An nRPC call surfaces a typed error, not an ambiguous hang: no provider serving the
name, a deadline exceeded, a handler that returned an application error, a
cancellation. Your code decides what each means. The three built-in recovery
strategies below all ride the same typed-error surface — no sidecar, no external
retry proxy.

## Retry — re-issue on a transient failure

```rust
use net_sdk::mesh_rpc_resilience::RetryPolicy;

let resp: SummarizeResp = caller.call_typed_with_retry(
    provider_node_id,
    "summarize",
    &req,
    CallOptions::default().with_deadline(Duration::from_millis(500)),
    &RetryPolicy::default(),          // bounded attempts + backoff; retries only retryable errors
).await?;
```

`RetryPolicy` retries only failures that can succeed on a re-issue (transient
transport, backpressure) and leaves genuine application errors alone — retrying a
`bad request` forever is not recovery. Tune attempts and backoff on the policy.

## Hedge — race a second provider

When latency matters more than duplicate work, fire a backup call after a short
delay and take whichever returns first:

```rust
use net_sdk::mesh_rpc_resilience::HedgePolicy;

let resp: SummarizeResp = caller.call_service_with_hedge(
    "summarize",                      // by service name — the mesh picks providers
    &req,
    CallOptions::default(),
    &HedgePolicy::default(),
).await?;
```

## Circuit breaker — stop hammering a dead provider

A long-lived `CircuitBreaker` trips after repeated failures to a target and
fast-fails subsequent calls until a cooldown, so a sick provider doesn't drag your
whole workflow down:

```rust
use net_sdk::mesh_rpc_resilience::{CircuitBreaker, CircuitBreakerConfig};

let breaker = CircuitBreaker::new(CircuitBreakerConfig::default());
// wrap calls through the breaker; when open, calls fail fast instead of waiting on the deadline
```

## Failover — let the mesh pick another provider

Because discovery is by capability, not by host, a capability declared
**substitutable** can be served by more than one provider. When the primary goes
down, an invoke by *service name* fails over to another advertising provider — the
mesh reroutes, the call succeeds, and the caller sees one result. This is
demonstrated end-to-end in `adapters/mcp/tests/serve_end_to_end.rs`
(`invoke_fails_over_when_the_primary_provider_goes_down`): the primary is killed
mid-run and the next invoke lands on the standby.

Call by service name (not a pinned node id) to get this for free:

```rust
let resp: SummarizeResp = caller.call_service_typed("summarize", &req, opts).await?;
```

## Multi-step work: the task lifecycle

For work that is more than one call — fan-out/fan-in, retries with state, staged
progress — the task-lifecycle layer models each stage as an observable event, so
"which step failed, and can I replay it?" is a question you answer from the event
trail rather than reconstruct from logs. See the scheduler / workflow surface for
the `WorkflowAdapter` and task-lifecycle events; the principle is the one from
[Submitted Is Not Completed](/docs/guides/submitted-is-not-completed): **distinct
outcomes are distinct events**, so a partial failure is a first-class, subscribable
fact your agent can recover from.
