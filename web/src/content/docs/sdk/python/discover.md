# Python — Discover Capabilities

List the tools a node can see on the mesh with `list_tools`, and react to changes
with `watch_tools`:

```python
from net_sdk import list_tools, watch_tools

# `node` is a MeshNode handshaked with a peer that served tools.
for t in list_tools(node):                      # baseline snapshot
    print(t.tool_id, "v" + str(t.version), "tags=", t.tags)

async for change in watch_tools(node):          # pushed on fold mutation
    print(change)  # a tool was added, removed, or its publisher count changed
```

Folding is asynchronous — an announcement takes a moment to propagate — but you
don't poll for it: the watch is event-driven off the capability fold's change
signal, so a `ToolListChange` arrives the moment the fold mutates and an idle mesh
costs zero periodic work. The optional `interval=` is a staleness ceiling (a
safety-net re-diff at least that often), **not** a poll rate. Announcements
propagate multi-hop (bounded by a hop count), so a tool can come from a node
several hops away.

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
