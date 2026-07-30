## Handle it — Python

### Read this before you branch on any exception type

When the extension is built **without** the nRPC feature, every RPC exception
class is aliased to `RpcError`, which is itself aliased to `Exception`. So
`except RpcTimeoutError` silently becomes `except Exception` and swallows
everything, including the bugs you wanted to surface.

```python
from net.mesh_rpc import RpcError, RpcTimeoutError

assert RpcTimeoutError is not RpcError, "nRPC feature not compiled in"
```

Confirm the feature before writing per-type handlers, or write one handler for
`RpcError` and accept the coarser branch. This is the single sharpest gotcha in
the Python binding.

### The five witnesses

```python
import re
from net.mesh_rpc import (
    RpcError, RpcCapabilityDeniedError, RpcTimeoutError,
    RpcServerError, RpcCodecError, RpcCancelledError,
    NRPC_TYPED_BAD_REQUEST,
)

try:
    rpc.call_service("summarize", req, {"deadline_ms": 500})

# WITNESS 1 & 2 — denied; revocation lands here on the next call.
except RpcCapabilityDeniedError:
    ...   # get a credential — a retry re-asks a question already answered

# WITNESS 3 & 5 — deadline elapsed. Outcome UNKNOWN, not failed.
except RpcTimeoutError:
    ...   # the work may or may not have run

# WITNESS 4 — a typed remote error crossed the boundary.
except RpcServerError as e:
    m = re.search(r"status\s*=?\s*0x([0-9a-fA-F]+)", str(e))
    status = int(m.group(1), 16) if m else None
    if status == NRPC_TYPED_BAD_REQUEST:
        ...   # the provider rejected the request shape

except RpcCodecError:
    ...   # your bug — do not retry
except RpcCancelledError:
    ...   # you did this
```

**`RpcServerError` has no `status` attribute.** The status is embedded in the
message as `status=0xNNNN` and the only parser is private
(`_parse_status_from_message`), so branching on a status means the regex above.
TypeScript exposes `.status` as a property; Python does not, and that asymmetry is
real rather than an omission in this page.

Every message the binding raises begins with a stable `nrpc:<kind>:` prefix shared
with the Node and Go bindings — match on that when you only need the kind.

### The nRPC family

All derive from `RpcError`. Import from `net`, **not** `net_sdk`:

| Exception | Fires when | Retry? |
|---|---|---|
| `RpcTimeoutError` | Deadline elapsed before a response | Outcome unknown — see above |
| `RpcNoRouteError` | No provider currently reachable | Yes — topology may settle |
| `RpcTransportError` | Connection failed mid-call | Yes, with backoff |
| `RpcServerError` | Handler returned a typed error status | No |
| `RpcAppError` | Application-level failure; carries `(status, body)` | No |
| `RpcCancelledError` | Call cancelled | No |
| `RpcCodecError` | Encode/decode failure | No — bug |
| `RpcCapabilityDeniedError` | Caller lacks the capability | No — get a credential |

`RpcAppError` is the one class that *does* carry its status as an argument — it is
what a handler raises to signal an application status, and it is constructed
`RpcAppError(status, body)`.

### Classed exceptions on the mesh

```python
from net_sdk import MeshNode, BackpressureError, NotConnectedError

try:
    node.send_on_stream(stream, [payload])
except BackpressureError:
    ...   # window full — the only blindly-retry-safe case
except NotConnectedError:
    ...   # connection lost — a state change, not a retry
```

These are raised by the **reliable mesh-stream send** path (`send_on_stream`,
`send_blocking`), not by the bus `emit`, which drops under load rather than
raising. `MeshNode.send_with_retry(...)` retries `BackpressureError` for you with a
5 ms → 200 ms backoff; prefer it over hand-rolling that loop.

### Organizations, channels, everything else

**Organizations** derive from `OrgError`: `OrgAdmissionDeniedError`,
`OrgCredentialsError`, `OrgDiscoveryError`, `OrgUnclassifiedError`.
**Channels** — `ChannelAuthError` derives from `ChannelError`.

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

`MigrationError` and `GroupError` are **flat** in Python, where TypeScript nests
both under `DaemonError`. Catch them individually here.

### Recover a call

`call_with_retry` and `call_with_hedge_to` are methods on `TypedMeshRpc`. The
default retry predicate treats a `RpcServerError` whose status will not parse as
**retryable** and emits a `RuntimeWarning` — fail-open by design, so a formatter
change cannot silently disable retry. If you see that warning, the message format
has drifted, not your code.

Next: back to [the SDK index](/docs/sdk/python).
