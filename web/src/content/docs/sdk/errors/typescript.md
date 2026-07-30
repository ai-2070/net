## Handle it — TypeScript

Every error the SDK throws is a subclass of `Error`, so `instanceof` is always the
discriminator.

### The five witnesses

```typescript
import {
  RpcError,
  RpcCapabilityDeniedError,
  RpcTimeoutError,
  RpcServerError,
  RpcCancelledError,
  RpcCodecError,
} from '@net-mesh/core/errors';

try {
  await clientRpc.callService('summarize', req, { deadlineMs: 500 });
} catch (e) {
  // WITNESS 1 & 2 — denied; revocation lands here on the next call.
  if (e instanceof RpcCapabilityDeniedError) {
    // Get a credential. A retry re-asks a question already answered.
  }
  // WITNESS 3 & 5 — deadline elapsed. Outcome UNKNOWN, not failed.
  else if (e instanceof RpcTimeoutError) {
    // The work may or may not have run. Idempotency, or accept the duplicate.
  }
  // WITNESS 4 — a typed remote error crossed the boundary.
  else if (e instanceof RpcServerError) {
    console.error(`provider returned ${e.status}`);
  }
  else if (e instanceof RpcCodecError)   { /* your bug — do not retry */ }
  else if (e instanceof RpcCancelledError) { /* you did this */ }
  else if (e instanceof RpcError)        { /* the rest of the family */ }
  else throw e;
}
```

`RpcServerError` **has a `status` property** in TypeScript. That is worth naming
because Python's does not — there the status is only inside the message string.

`classifyError(e)` maps a raw thrown value to the right subclass when you are
catching something that came through an untyped path:

```typescript
import { classifyError } from '@net-mesh/core/errors';
import { NRPC_TYPED_BAD_REQUEST } from '@net-mesh/core/mesh_rpc';

const typed = classifyError(e);
if (typed instanceof RpcServerError && typed.status === NRPC_TYPED_BAD_REQUEST) {
  // the provider rejected the request shape — a caller bug
}
```

### The nRPC family

All extend `RpcError`, so catching the base handles the whole family:

| Class | Fires when | Retry? |
|---|---|---|
| `RpcTimeoutError` | Deadline elapsed before a response | Outcome unknown — see above |
| `RpcNoRouteError` | No provider currently reachable | Yes — topology may settle |
| `RpcTransportError` | Connection failed mid-call | Yes, with backoff |
| `RpcServerError` | Handler returned a typed error status | No — inspect `.status` |
| `RpcCancelledError` | Cancelled by the caller or a dropped stream | No |
| `RpcCodecError` | Request or response failed to encode/decode | No — bug |
| `RpcCapabilityDeniedError` | Caller lacks the capability to invoke | No — get a credential |

### Classed errors on the mesh

```typescript
import { BackpressureError, NotConnectedError } from '@net-mesh/sdk';

try {
  await node.sendOnStream(stream, payloads);
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

Note that the **bus** `emit` does not throw under backpressure — it returns `null`.
`BackpressureError` is the reliable mesh-stream path only.

### Organizations, compute, everything else

**Organizations** all extend `OrgError`: `OrgAdmissionDeniedError`,
`OrgCredentialsError`, `OrgDiscoveryError`, `OrgUnclassifiedError`.

**Compute** — `MigrationError` and `GroupError` both extend `DaemonError`.

| Class | Surface |
|---|---|
| `BackpressureError` | Stream window full — the one blindly-retryable case |
| `NotConnectedError` | Connection lost; a state change, not a retry |
| `ChannelError` / `ChannelAuthError` | Channel publish/subscribe; the latter is an authorization refusal |
| `BreakerOpenError` | Circuit breaker open — the provider is being fast-failed |
| `CortexError` / `NetDbError` / `RedexError` | Storage and folded state |
| `FoldQueryClientError` / `RegistryClientError` | Fold-query and aggregator-registry RPC |
| `MeshOsSdkError` / `DeckSdkError` | Daemon authoring and the operator surface |
| `IdentityError` / `TokenError` | Keys, signing, permission tokens |
| `GatewayError` | Capability gateway, including payment refusals |
| `ToolCallParseError` | A tool descriptor or call payload that will not parse |

### Recover a call

```typescript
import { RetryPolicy, HedgePolicy, CircuitBreaker } from '@net-mesh/core/mesh_rpc';

await clientRpc.callWithRetry(nodeId, 'summarize', req,
  new RetryPolicy({ maxAttempts: 4, initialBackoffMs: 50 }));

await clientRpc.callWithHedgeTo([nodeA, nodeB, nodeC], 'summarize', req,
  new HedgePolicy({ maxParallel: 3, hedgeDelayMs: 50 }));

const breaker = new CircuitBreaker({ failureThreshold: 5, resetAfterMs: 1000 });
await breaker.call(() => clientRpc.call(nodeId, 'summarize', req, { deadlineMs: 500 }));
```

Next: back to [the SDK index](/docs/sdk/typescript).
