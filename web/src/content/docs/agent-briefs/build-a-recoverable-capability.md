# Brief: Build a Recoverable Capability

**Goal.** Serve a native capability from two providers, invoke it by service name,
kill the primary mid-run, and prove the call fails over to the standby — the
"recover" half of the agent loop
([Submitted Is Not Completed](/docs/worldview/submitted-is-not-completed)).

## Prerequisites

- Rust toolchain; `cargo add net-mesh-sdk tokio serde`.
- No external mesh needed — this brief stands up its own two (or three) in-process
  `Mesh` nodes, the pattern proven by `adapters/mcp/tests/serve_end_to_end.rs`
  (`invoke_fails_over_when_the_primary_provider_goes_down`).

## Steps

1. **Serve the capability from two providers.** Build two `Mesh` nodes, register the
   same typed handler on each under one service name, and make the capability
   **substitutable** so the mesh treats them as interchangeable:
   ```rust
   let _h1 = primary.serve_rpc_typed("summarize", handler.clone())?;
   let _h2 = standby.serve_rpc_typed("summarize", handler.clone())?;
   ```

2. **Invoke by service name, not node id** — this is what makes failover possible:
   ```rust
   let resp: SummarizeResp = caller.call_service_typed("summarize", &req, opts).await?;
   ```

3. **Kill the primary** between two calls, then invoke again. Wrap the call in the
   retry helper so the transient failure during cutover is absorbed:
   ```rust
   use net_sdk::mesh_rpc_resilience::RetryPolicy;
   let resp = caller.call_service_typed_with_retry("summarize", &req, opts, &RetryPolicy::default()).await?;
   ```

## Expected output

- The first `call_service_typed` returns a result from the primary.
- After the primary is dropped, the retried `call_service_*` returns a result from
  the standby — one successful response, no caller-visible error.

## Verify (acceptance)

- [ ] The pre-kill call and the post-kill call both return a valid `SummarizeResp`.
- [ ] The post-kill result demonstrably came from the standby (tag the two handlers'
      output so you can tell them apart).
- [ ] Calling by a **pinned node id** instead of the service name does *not* fail
      over — proving the failover is a property of service-name discovery, not magic.

## Pitfalls

- **Call by service name for failover.** A call pinned to a dead node id cannot
  reroute — it just fails.
- **Retry only retryable errors.** `RetryPolicy` re-issues transient/backpressure
  failures, not application errors — retrying a `bad request` forever is not
  recovery.
- For latency-sensitive paths, prefer **hedging** (`call_service_with_hedge`) over
  retry: race a second provider instead of waiting for the first to fail.
- A `CircuitBreaker` around a repeatedly-failing target stops you from burning every
  deadline on a provider that's already down.

See [Recover a Failed Workflow](/docs/guides/recover-failed-workflow) and the
per-SDK errors pages (e.g. [Rust](/docs/sdk/rust/errors)).
