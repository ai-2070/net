# Python — Discover Capabilities

List the tools a node can see on the mesh with `list_tools`:

```python
import time
from net_sdk import list_tools

# `node` is a MeshNode handshaked with a peer that served tools.
deadline = time.monotonic() + 3.0
while time.monotonic() < deadline and len(list_tools(node)) < 1:
    time.sleep(0.02)          # folding is asynchronous — poll until it appears

for t in list_tools(node):
    print(t.tool_id, "v" + str(t.version), "tags=", t.tags)
```

Folding is asynchronous — an announcement takes a moment to propagate, so poll
until the tool you expect appears rather than assuming it's there on the first
call. Announcements propagate multi-hop (bounded by a hop count), so a tool can
come from a node several hops away.

Tool descriptors lower to provider tool-call formats via the `openai` helpers, so a
discovered tool feeds straight into a chat-completion request.

## Filtering nodes by capability

Capability-filter node discovery (the `find_nodes` surface, e.g. "GPU nodes with
≥24 GB VRAM") is available through the node's native handle in Python rather than a
clean `MeshNode` method. The filter model is identical across bindings — see
[Capabilities](/docs/concepts/capabilities) for the predicate surface and the CLI
equivalent (`net cap query --tag …`). For most Python agent code, the tool API
above is the path you want.

Discovery is **advisory** — it tells you who *can*, with no exclusivity.
