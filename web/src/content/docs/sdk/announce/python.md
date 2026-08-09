## Announce it — Python

### Capabilities are on the wrapper; tools are one layer down

`net_sdk.MeshNode` now carries the capability lifecycle directly —
`announce_capabilities`, `find_nodes` / `find_nodes_scoped`, and
`find_best_node` / `find_best_node_scoped`:

```python
from net_sdk import MeshNode

node = MeshNode(bind_addr="127.0.0.1:9001", psk="42" * 32)
node.announce_capabilities({"tags": ["gpu"], "hardware": {"memory_gb": 64}})
```

The **tool** surface still lives on the native handle, and so does nRPC:

```python
from net.mesh_rpc import TypedMeshRpc

native = node._native                       # tools/nRPC only
rpc = TypedMeshRpc.from_mesh(native)
```

Reaching a private attribute is not a recommendation, it is the current state of
the binding for those surfaces. It is called out here rather than hidden because
the alternative is a reader concluding Python cannot serve tools — it can, one
layer down.

### Serve a tool

```python
from net_sdk import serve_tool, descriptor_for, add_tool_capabilities_to_announce

def web_search(req):
    return {"results": [f"first hit for '{req['query']}'"]}

handle = serve_tool(
    rpc,
    {"name": "web_search", "description": "Search the web.", "tags": ["web", "research"]},
    web_search,
)
```

`serve_tool(rpc, options_or_descriptor, handler)` — the first argument is the
**`TypedMeshRpc`**. The options argument accepts a `ToolDescriptor`, a dict of
`descriptor_for` keyword arguments (must include `name`), or a bare name string
with the rest passed as keywords.

`serve_tool_async` is the asyncio variant; `serve_tool_streaming` serves a tool
that emits multiple chunks.

### Announce the tool — the step `serve_tool` does not do

```python
caps = add_tool_capabilities_to_announce(
    {"tags": []},
    [descriptor_for("web_search", description="Search the web.",
                    tags=["web", "research"])],
)
native.announce_capabilities(caps)
```

`add_tool_capabilities_to_announce` adds an `ai-tool:<tool_id>` tag and a `tools[]`
entry to a capability-set dict, and returns the same dict for chaining. **Without
it the handler is served and invisible.** The binding's own docstring calls this a
v1 convenience that becomes optional once pyo3 exposes `tool_registry()` — until
then it is required, not optional.

### Tags without a tool

```python
native.announce_capabilities({"tags": ["gpu", "inference", "region:eu-west"]})
```

The capability set is a plain dict in Python — no builder, no typed class.

### Verify it worked

From a peer that folded the announcement:

```python
import time
from net_sdk import list_tools

agent_native = agent._native

deadline = time.monotonic() + 3.0
while time.monotonic() < deadline and not list_tools(agent_native):
    time.sleep(0.02)

assert list_tools(agent_native), "the announcement did not fold"
```

`list_tools` also takes the native handle. Passing the `MeshNode` raises
`AttributeError: 'MeshNode' object has no attribute 'list_tools'` — which reads
like a missing feature and is a wrong argument.

Next: [Discover a capability](/docs/sdk/python/discover).
