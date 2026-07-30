## Handle it — Go

Go surfaces failures as `error` values you check on every call. There is no
exception path, and — because every call crosses the cgo boundary — there are more
error returns than you would expect from the equivalent pure-Go API.

### The five witnesses

`RpcError` carries a stable `Kind` discriminator. This is the cleanest of the four
taxonomies to branch on, because the kind is a real field rather than a class or a
substring:

```go
var re *net.RpcError
if errors.As(err, &re) {
    switch re.Kind {

    // WITNESS 1 & 2 — denied; revocation lands here on the next call.
    case net.RpcKindCapabilityDenied:
        // Get a credential. A retry re-asks a question already answered.

    // WITNESS 3 & 5 — deadline elapsed. Outcome UNKNOWN, not failed.
    case net.RpcKindTimeout:
        // The work may or may not have run.

    // WITNESS 4 — a typed remote error crossed the boundary.
    case net.RpcKindServerError:
        log.Printf("provider returned: %s", re.Message)

    case net.RpcKindNoRoute:
        // Retry briefly — topology may settle.
    case net.RpcKindCodecEncode, net.RpcKindCodecDecode:
        // Your bug. Encode never left; Decode means the work ran.
    case net.RpcKindTransport:
        // Retry with backoff.
    case net.RpcKindUnknown:
        // The Rust formatter emitted a kind this binding does not know.
    }
}
```

Go splits codec failures into two kinds where Rust uses one variant with a
direction field. `RpcKindCodecEncode` means the request never reached the wire;
`RpcKindCodecDecode` means the reply arrived and could not be read — so the work
ran.

The `status=0x4001` value for a server error is inside `re.Message`, not a field.

### Sentinels and `errors.Is`

The binding exports 49 sentinel errors. The returned error wraps the sentinel with
context, so `==` will not match — use `errors.Is`:

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

**Tokens** — each one implies a different fix, which is why they are separate
rather than one authorization error: `ErrTokenExpired`, `ErrTokenNotYetValid`,
`ErrTokenInvalidFormat`, `ErrTokenInvalidSignature`, `ErrTokenNotAuthorized`,
`ErrTokenDelegationNotAllowed`, `ErrTokenDelegationExhausted`, plus `ErrIdentity`.

`ErrTokenExpired` is **witness 2 on the token path**: a grant that has run out
reports expiry rather than a generic refusal, so your code can renew instead of
escalating.

**NAT traversal** — all mean "the direct path did not open," none break
correctness, and the routed path still works: `ErrTraversalUnsupported`,
`ErrTraversalPunchFailed`, `ErrTraversalPeerNotReachable`,
`ErrTraversalReflexTimeout`, `ErrTraversalPortMapUnavailable`,
`ErrTraversalRendezvousNoRelay`, `ErrTraversalRendezvousRejected`,
`ErrTraversalTransport`.

**Organizations:** `ErrOrgAdmissionDenied`, `ErrOrgCredentials`,
`ErrOrgDiscovery`, `ErrOrgProvision`, `ErrOrgRPC`, `ErrOrgAlreadyServing`,
`ErrOrgClosed`, `ErrOrgUnclassified`.

**MeshOS:** `ErrMeshOs`, `ErrMeshOsInvalidArg`, `ErrMeshOsCallFailed`,
`ErrMeshOsAlreadyShutdown`.

### Typed error structs

Where a failure has structure rather than just an identity, the binding returns a
struct with a typed `Kind` — use `errors.As`:

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

### Returning a typed error from a handler

```go
return SummarizeResp{}, net.AppError(net.NrpcTypedBadRequest, body)
```

**Any other error a handler returns surfaces to the caller as
`status=0x4001` (`Internal`).** A handler that returns a bare `fmt.Errorf` has
thrown away the distinction the caller needs to decide whether to retry — this is
the most common way a Go provider degrades its callers' error handling.

### Recover a call

Retry, hedge and circuit-breaker strategies apply as in the other bindings, and
calling by tool or service name lets the mesh pick a provider so a substitutable
capability fails over. The end-to-end patterns are in
[Recover a Failed Workflow](/docs/guides/recover-failed-workflow).

Next: back to [the SDK index](/docs/sdk/go).
