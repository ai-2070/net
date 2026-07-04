# TypeScript — Errors and Recovery

Failure is a typed outcome, not a silence. The golden rule matches every binding:
**only backpressure is safe to retry blindly.**

## Classed errors on the mesh

Stream and connection failures are classed errors you discriminate with
`instanceof`:

```typescript
import { MeshNode, BackpressureError, NotConnectedError } from '@net-mesh/sdk';

try {
  await node.send(/* … */);
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

The full taxonomy is the [Error Codes](/docs/reference/error-codes) reference.

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
