## Invoke it — Python

### Call a tool

```python
from net.mesh_rpc import TypedMeshRpc
from net_sdk import call_tool

rpc = TypedMeshRpc.from_mesh(node._native)

resp = call_tool(rpc, "web_search", {"query": "how does the capability fold work"})
print(resp)
```

`call_tool(rpc, tool_id, request, opts=None)` — the first argument is the
**`TypedMeshRpc`**, not the `MeshNode` and not the native handle. Three different
objects appear on this spine and only this one is right here; see
[Announce](/docs/sdk/python/announce) for how they relate.

Async and streaming variants take the same first argument:

```python
from net_sdk import call_tool_async, call_tool_streaming

resp = await call_tool_async(rpc, "web_search", {"query": "…"})

for chunk in call_tool_streaming(rpc, "tail", {"tail": "events"}):
    handle(chunk)
```

### Serve and call over nRPC directly

```python
handle = rpc.serve("summarize", lambda req: {"summary": req["text"][:40]})

reply = rpc.call(provider_node_id, "summarize", {"text": "…"}, {"deadline_ms": 500})
```

`rpc.serve(service, handler)` takes a sync callable; an `async def` handler works
transparently when registered against `AsyncTypedMeshRpc`. `rpc.call_service(...)`
resolves by service name through the capability index rather than pinning a node.

### A request that will not decode

A handler that cannot decode its request does not raise into your code. The caller
receives a canonical typed bad-request — status `NRPC_TYPED_BAD_REQUEST`
(`0x8000`) with body `{"error": "invalid_request", "detail": ...}`:

```python
import re
from net.mesh_rpc import RpcServerError, NRPC_TYPED_BAD_REQUEST

try:
    rpc.call(node_id, "summarize", {"wrong": "shape"}, {"deadline_ms": 500})
except RpcServerError as e:
    m = re.search(r"status\s*=?\s*0x([0-9a-fA-F]+)", str(e))
    if m and int(m.group(1), 16) == NRPC_TYPED_BAD_REQUEST:
        ...   # the provider rejected the request shape — a caller bug
```

**`RpcServerError` has no `status` attribute in Python.** The status rides inside
the message text as `status=0xNNNN`, and the parser for it
(`_parse_status_from_message`) is private, so branching on a status means the
regex above. Every message the binding raises starts with a stable `nrpc:<kind>:`
prefix, which is the more robust thing to match on when you only need the kind.

**Check the classes are real before branching on them at all.** When the extension
is built without the nRPC feature, every one of them is aliased to `RpcError`,
which is itself aliased to `Exception` — so `except RpcServerError` silently
becomes `except Exception` and swallows everything. See
[Errors](/docs/sdk/python/errors).

### Verify it worked

```python
resp = call_tool(rpc, "web_search", {"query": "ping"})
assert resp["results"], "the provider answered, but with nothing"
print(f"invoked: {len(resp['results'])} result(s)")
```

Next: [Watch the event stream](/docs/sdk/python/watch).
