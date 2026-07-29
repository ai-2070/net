---
title: Errors
description: "The Python SDK's error taxonomy: failure is a typed outcome you can branch on, never a silence."
---
# Python — Errors and Recovery

Failure is a typed outcome, not a silence. The rule matches every binding: **only
backpressure is safe to retry blindly.**

## Classed exceptions on the mesh

`BackpressureError` and `NotConnectedError` are raised by the **reliable
mesh-stream send** path on `MeshNode` — `send_on_stream` (and `send_blocking`) —
not by the bus `emit` (which drops under load rather than raising). Catch them by
type:

```python
from net_sdk import MeshNode, BackpressureError, NotConnectedError

try:
    node.send_on_stream(stream, [payload])   # `stream` is an open MeshStream
except BackpressureError:
    # window full — the only blindly-retry-safe case
    ...
except NotConnectedError:
    # connection lost — a state change, not a retry
    ...
```

`MeshNode.send_with_retry(...)` retries `BackpressureError` for you (5 ms → 200 ms
backoff), so prefer it over hand-rolling the loop above.

- **`BackpressureError`** — the ring buffer/window was full. Retry with backoff, or
  slow the producer. The *only* error a blind retry can fix.
- **`NotConnectedError`** — the session is gone. A state change; retrying won't undo
  it.
- Serialization / config failures are bugs — retrying reruns the bug.

## The full exception taxonomy

Everything below derives from `Exception`. Two families have a base class you
can catch to handle the whole group; the rest are flat.

**nRPC** — all derive from `RpcError`, so `except RpcError` covers the family.
Import from `net` (not `net_sdk`):

| Exception | Fires when | Retry? |
|---|---|---|
| `RpcTimeoutError` | Deadline elapsed before a response | Yes, with backoff |
| `RpcNoRouteError` | No provider currently reachable | Yes — topology may settle |
| `RpcTransportError` | Connection failed mid-call | Yes, with backoff |
| `RpcServerError` | Handler returned a typed error status | No |
| `RpcAppError` | Application-level failure; carries `(status, body)` | No |
| `RpcCancelledError` | Call cancelled | No |
| `RpcCodecError` | Encode/decode failure | No — bug |
| `RpcCapabilityDeniedError` | Caller lacks the capability | No — get a credential |

> **Gotcha.** These classes only exist when the extension is built with the
> nRPC feature. Without it the module falls back to aliasing every one of them
> to `RpcError`, which is itself aliased to `Exception` — so `except
> RpcTimeoutError` silently becomes `except Exception` and swallows everything.
> If you branch on RPC error type, confirm the feature is compiled in.

**Organizations** — all derive from `OrgError`:
`OrgAdmissionDeniedError`, `OrgCredentialsError`, `OrgDiscoveryError`,
`OrgUnclassifiedError`.

**Channels** — `ChannelAuthError` derives from `ChannelError`.

**Flat, deriving straight from `Exception`:**

| Exception | Surface |
|---|---|
| `BackpressureError` | Stream window full — the one blindly-retryable case |
| `NotConnectedError` | Session gone; a state change |
| `CortexError` / `NetDbError` / `RedexError` | Storage and folded state |
| `MeshDbError` | Federated query layer |
| `BlobError` | Blob publish/resolve |
| `DaemonError` / `MigrationError` / `GroupError` | Compute, migration, replica groups |
| `MeshOsSdkError` / `DeckSdkError` | Daemon authoring and the operator surface |
| `IdentityError` / `TokenError` | Keys, signing, permission tokens |
| `FoldQueryClientError` / `RegistryClientError` | Fold-query and registry RPC |
| `PinsError` | MCP pin approval surface |

Note that `MigrationError` and `GroupError` are flat here, where the TypeScript
SDK nests both under `DaemonError` — catch them individually in Python.

The wire-level codes these wrap — core ingestion/consumer/adapter errors plus
token, scaling and stream errors — are in the
[Error Codes](/docs/reference/error-codes) reference.

## Recover an nRPC call

Retry, hedge, and circuit-breaker helpers wrap the raw call — the same three
strategies as Rust and TypeScript. Calling a tool or service by **name** (rather
than a pinned node id) lets the mesh pick a provider, so a substitutable capability
fails over to a standby when the primary dies. The end-to-end patterns and the
exact helper surface are in
[Recover a Failed Workflow](/docs/guides/recover-failed-workflow) and
[Typed RPC with nRPC](/docs/guides/nrpc).

## The one rule

> Retry `BackpressureError`. Treat serialization/config errors as bugs, auth errors
> as "get a new credential," and `NotConnectedError` / closed streams as state
> changes retrying won't fix.
