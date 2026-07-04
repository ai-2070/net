# Python — Announce a Capability

In Python the ergonomic way to put a discoverable, callable capability on the mesh
is the **tool** API. `serve_tool` registers a handler on your node and announces
it; peers fold the announcement and can then list and call it.

```python
from net_sdk import MeshNode, serve_tool

node = MeshNode(bind_addr="127.0.0.1:0", psk="42" * 32)

def web_search(req):
    return {"results": [f"first hit for '{req['query']}'"]}

handle = serve_tool(
    node,
    {"name": "web_search", "description": "Search the web.", "tags": ["web", "research"]},
    web_search,
)
# handle stays alive while the tool is served; close it to withdraw.
```

`serve_tool` / `serve_tool_async` (and the streaming variants) are top-level
functions that take the node first — the same node-first convention as the
transfer functions. The exact keyword shape of the tool descriptor is in the
`tool_calling` example; the essentials are a name, a description, and tags.

## Raw capability tags (the handle)

If you need to announce plain capability tags (hardware, region, arbitrary tags)
rather than a callable tool, that surface is reached through the node's native
handle in Python — it is not a clean `MeshNode` method the way it is in Rust/TS.
The tag/axis model itself is identical across bindings — see
[Capabilities](/docs/concepts/capabilities) and
[Capability Schema](/docs/reference/capability-schema) — but in Python prefer the
tool API above unless you specifically need raw tags.

## Policy: announced ≠ invocable

Announcing makes a capability discoverable, not open. Visibility and invocation are
separate; the boundary is enforced on invoke, not announce — see
[Invoke](/docs/sdk/python/invoke).

## Next

[Discover](/docs/sdk/python/discover).
