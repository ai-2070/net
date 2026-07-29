---
title: Errors
description: "The TypeScript SDK's error taxonomy: failure is a typed outcome you can branch on, never a silence."
---
# TypeScript — Errors and Recovery

Failure is a typed outcome, not a silence. The golden rule matches every binding:
**only backpressure is safe to retry blindly.**

## Classed errors on the mesh

Stream and connection failures are classed errors you discriminate with
`instanceof`:

```typescript
import { MeshNode, BackpressureError, NotConnectedError } from '@net-mesh/sdk';

try {
  await node.sendOnStream(stream, [/* … */]);
} catch (e) {
  if (e instanceof BackpressureError) {
    // window full — the only blindly-retry-safe case (or use sendWithRetry)
  } else if (e instanceof NotConnectedError) {
    // connection lost — a state change, not a retry
  } else {
    throw e;
  }
}
```

For nRPC, caller failures come back with a stable `nrpc:` prefix; `classifyError`
maps a raw error to a typed subclass so you can branch on it:

```typescript
import { classifyError, RpcServerError } from '@net-mesh/core/errors';
import { NRPC_TYPED_BAD_REQUEST } from '@net-mesh/core/mesh_rpc';

try {
  await clientRpc.call(nodeId, 'summarize', req, { deadlineMs: 500 });
} catch (e) {
  const typed = classifyError(e);
  if (typed instanceof RpcServerError && typed.status === NRPC_TYPED_BAD_REQUEST) {
    // typed bad-request from the handler — a bug in the request, not a retry
  }
}
```

## The full class taxonomy

Every error the SDK throws is a subclass of `Error`, so `instanceof` is always
the discriminator. The hierarchy is shallow and deliberate — where a family
exists, catch the base to handle the whole family.

**nRPC** — all extend `RpcError`, so `catch (e) { if (e instanceof RpcError) }`
covers the family:

| Class | Fires when | Retry? |
|---|---|---|
| `RpcTimeoutError` | Deadline elapsed before a response | Yes, with backoff |
| `RpcNoRouteError` | No provider currently reachable for the target | Yes — topology may settle |
| `RpcTransportError` | Connection failed mid-call | Yes, with backoff |
| `RpcServerError` | Handler returned a typed error status | No — inspect `.status` |
| `RpcCancelledError` | Call cancelled by the caller or a dropped stream | No |
| `RpcCodecError` | Request or response failed to encode/decode | No — bug |
| `RpcCapabilityDeniedError` | Caller lacks the capability to invoke | No — get a credential |

**Organizations** — all extend `OrgError`:

| Class | Fires when |
|---|---|
| `OrgAdmissionDeniedError` | Node refused admission to the org |
| `OrgCredentialsError` | Missing, expired or malformed org credential |
| `OrgDiscoveryError` | Org-scoped discovery failed |
| `OrgUnclassifiedError` | Org failure the vocabulary doesn't name yet |

**Compute** — `MigrationError` and `GroupError` both extend `DaemonError`.

**Everything else** extends `Error` directly:

| Class | Surface |
|---|---|
| `BackpressureError` | Stream window full — the one blindly-retryable case |
| `NotConnectedError` | Connection lost; a state change, not a retry |
| `ChannelError` / `ChannelAuthError` | Channel publish/subscribe; the latter is an authorization refusal |
| `BreakerOpenError` | Circuit breaker is open — the provider is being fast-failed |
| `CortexError` / `NetDbError` / `RedexError` | Storage and folded state |
| `FoldQueryClientError` / `RegistryClientError` | Fold-query and aggregator-registry RPC |
| `MeshOsSdkError` / `DeckSdkError` | Daemon authoring and the operator surface |
| `IdentityError` / `TokenError` | Keys, signing, permission tokens |
| `GatewayError` | Capability gateway, including payment refusals |
| `ToolCallParseError` | A tool descriptor or call payload that won't parse |

The wire-level codes these wrap are in the
[Error Codes](/docs/reference/error-codes) reference.

## Recover an nRPC call

The resilience helpers wrap the raw call — same three strategies as Rust:

```typescript
import { RetryPolicy, HedgePolicy, CircuitBreaker } from '@net-mesh/core/mesh_rpc';

// retry only retryable failures, bounded attempts + backoff
await clientRpc.callWithRetry(nodeId, 'summarize', req, new RetryPolicy({ maxAttempts: 4, initialBackoffMs: 50 }));

// race several providers when latency matters more than duplicate work
await clientRpc.callWithHedgeTo([nodeA, nodeB, nodeC], 'summarize', req, new HedgePolicy({ maxParallel: 3, hedgeDelayMs: 50 }));

// fast-fail a sick provider instead of waiting on every deadline
const breaker = new CircuitBreaker({ failureThreshold: 5, resetAfterMs: 1000 });
await breaker.call(() => clientRpc.call(nodeId, 'summarize', req, { deadlineMs: 500 }));
```

Calling by **service name** (`callService`) lets the mesh pick a provider, so a
substitutable capability fails over to a standby when the primary dies. The
end-to-end patterns are in
[Recover a Failed Workflow](/docs/guides/recover-failed-workflow).

## The one rule

> Retry `BackpressureError`. Treat serialization/config errors as bugs, auth errors
> as "get a new credential," and `NotConnectedError` / closed streams as state
> changes retrying won't fix.
