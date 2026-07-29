---
title: Errors
description: "Go surfaces failures as error values you check on every call — there's no exception path."
---
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

## Sentinels and `errors.Is`

The binding exports 49 sentinel errors. Compare with `errors.Is` — the
returned error wraps the sentinel with context, so `==` will not match:

```go
if errors.Is(err, net.ErrBackpressure) {
    // the one case a blind retry fixes
}
```

**Bus and lifecycle:** `ErrBackpressure`, `ErrIngestionFailed`, `ErrPollFailed`,
`ErrInitFailed`, `ErrShuttingDown`, `ErrInvalidJSON`, `ErrNullPointer`,
`ErrBufferTooSmall`, `ErrUnknown`.

**Mesh and streams:** `ErrMeshInit`, `ErrMeshHandshake`, `ErrMeshTransport`,
`ErrNotConnected`, `ErrStreamEnded`, `ErrStreamTimeout`, `ErrChannel`,
`ErrChannelAuth`.

**Storage:** `ErrRedex`, `ErrNetDb`, `ErrCortexClosed`, `ErrCortexFold`.

**Tokens** — the whole reason a permission check failed, which matters because
each one implies a different fix: `ErrTokenExpired`, `ErrTokenNotYetValid`,
`ErrTokenInvalidFormat`, `ErrTokenInvalidSignature`, `ErrTokenNotAuthorized`,
`ErrTokenDelegationNotAllowed`, `ErrTokenDelegationExhausted`, plus
`ErrIdentity`.

**NAT traversal** — all of these mean "the direct path didn't open," and none
of them break correctness; the routed path still works:
`ErrTraversalUnsupported`, `ErrTraversalPunchFailed`,
`ErrTraversalPeerNotReachable`, `ErrTraversalReflexTimeout`,
`ErrTraversalPortMapUnavailable`, `ErrTraversalRendezvousNoRelay`,
`ErrTraversalRendezvousRejected`, `ErrTraversalTransport`.

**Organizations:** `ErrOrgAdmissionDenied`, `ErrOrgCredentials`,
`ErrOrgDiscovery`, `ErrOrgProvision`, `ErrOrgRPC`, `ErrOrgAlreadyServing`,
`ErrOrgClosed`, `ErrOrgUnclassified`.

**MeshOS:** `ErrMeshOs`, `ErrMeshOsInvalidArg`, `ErrMeshOsCallFailed`,
`ErrMeshOsAlreadyShutdown`.

## Typed error structs

Where a failure has structure rather than just an identity, the binding returns
a struct with a typed `Kind` field — use `errors.As`:

```go
var ge *net.GroupError
if errors.As(err, &ge) {
    switch ge.Kind {
    // ... GroupErrorKind values
    }
}
```

`GroupError` (`GroupErrorKind`), `MigrationError` (`MigrationErrorKind`),
`RegistryClientError` (`RegistryErrorKind`), `FoldQueryClientError`
(`FoldQueryErrorKind`), plus `DaemonError`, `DeckError`, `McpError`,
`MeshOsSdkError`, `OrgError`, `RpcError`, `RpcCallStatusError` and
`DuplicateKindError`.

The wire-level codes these wrap are in the
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
