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

The full taxonomy — the core ingestion/consumer/adapter errors plus subsystem
errors (token, scaling, stream) — is the
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
