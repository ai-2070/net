# Go — Errors and Recovery

Go surfaces failures as `error` values you check on every call — there's no
exception path. The rule matches every binding: **only backpressure is safe to
retry blindly.**

```go
if err := bus.IngestRaw(payload); err != nil {
    // Inspect the error: backpressure is retryable; serialization/config are bugs;
    // a shutdown/not-connected error is a state change retrying won't fix.
    log.Printf("ingest failed: %v", err)
}
```

- **Backpressure** — the ring buffer/window was full. Retry with backoff, or slow
  the producer. The only case a blind retry fixes.
- **Serialization / config** — a bug. Retrying reruns it.
- **Shutdown / not-connected** — a state change; retrying won't undo it.

The binding maps the core error kinds to Go errors with stable prefixes (the
`RegistryClientError` / `FoldQueryClientError` types carry a typed `Kind`), so you
can branch on the kind where you need to. The full taxonomy is the
[Error Codes](/docs/reference/error-codes) reference.

## Recover an RPC call

Retry, hedge, and circuit-breaker strategies apply the same way as the other
bindings — and calling a tool or service by **name** lets the mesh pick a provider,
so a substitutable capability fails over to a standby when the primary dies. The
end-to-end patterns are in
[Recover a Failed Workflow](/docs/guides/recover-failed-workflow).

## The one rule

> Retry on backpressure. Treat serialization/config errors as bugs, auth errors as
> "get a new credential," and shutdown / not-connected as state changes retrying
> won't fix.

## Back to the spine

The whole loop: [announce](/docs/sdk/go/announce) → [discover](/docs/sdk/go/discover)
→ [invoke](/docs/sdk/go/invoke) → [watch](/docs/sdk/go/watch) →
[artifacts](/docs/sdk/go/artifacts) → recover. Same spine in the
[other bindings](/docs/sdk/rust).
