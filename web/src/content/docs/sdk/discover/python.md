## Discover it — Python

### Find nodes

Node discovery is on the wrapper:

```python
ids = node.find_nodes({"require_tags": ["gpu"]})
best = node.find_best_node({"filter": {"require_tags": ["gpu"]},
                            "prefer_more_vram": 1.0})
```

`find_nodes` returns a list, possibly empty. `find_best_node` returns one id or
`None` — and `0` is a real node id, so test `is None` rather than truthiness.

### List tools

The **tool** view is on the node too:

```python
for t in node.list_tools():
    print(t.tool_id, "v" + t.version, "tags=", t.tags)
```

The module-level `net_sdk.list_tools(mesh)` is the layer underneath: it takes
the native handle, not the `MeshNode`.

Schemas come back as **JSON-encoded strings** on `descriptor.input_schema` and
`descriptor.output_schema`. `json.loads` them before use.

### Watch for changes

`watch_tools` is an **async** iterator, so it needs an event loop even if the rest
of your program is synchronous:

```python
import asyncio

async def follow(node):
    for t in node.list_tools():         # baseline, synchronous
        print("baseline", t.tool_id)

    async for change in node.watch_tools():
        match change.type:
            case "added":   print("+", change.descriptor.tool_id)
            case "removed": print("-", change.descriptor.tool_id)
            case "node_count_changed":
                print("~", change.descriptor.tool_id, change.prev_node_count,
                      "->", change.descriptor.node_count)

asyncio.run(follow(node))
```

`interval=` is a debounce ceiling **in seconds** — Python takes seconds where
TypeScript takes milliseconds and Go takes a `time.Duration`. Leave it unset for
pure event-driven behaviour.

The subscription is taken when `watch_tools` is *called*, not on the first
iteration, so a change published between the two is still observed. That also
means the returned iterator holds a live substrate watch: consume it, or break out
so its `finally` closes it.

### Filtering nodes by capability

```python
peers = node.find_nodes({"require_tags": ["gpu"], "min_vram_gb": 24})
```

`find_nodes` takes a plain dict rather than a typed filter object. The predicate model is identical across bindings — see
[Capabilities](/docs/concepts/capabilities) for the full surface and the CLI
equivalent.

### Picking one node

```python
target = node.find_best_node({
    "filter": {"require_tags": ["gpu"]},
    "prefer_more_vram": 1.0,
})
```

`find_best_node` applies the requirement's weights and returns one winner
instead of the whole matching set. The four weights — `prefer_more_memory`,
`prefer_more_vram`, `prefer_faster_inference`, `prefer_loaded_models` — each
score one axis of what a candidate announced about itself: system memory, GPU
VRAM, model inference speed, and the share of its models already loaded. Every
key of the dict is optional. They must be **finite**: `nan` and `inf` raise
`ValueError`, a
non-numeric weight raises `TypeError`, and finite values outside `[0.0, 1.0]`
are clamped by the substrate. Ties, including the case where every weight is
omitted, resolve to the lowest matching node id.

`None` means nothing matched. `0` is a real node id, so test `is None` rather
than truthiness. `find_best_node_scoped(requirement, scope)` applies the scope
first, so a peer outside it cannot win on capacity.

Same local-index read as `find_nodes`: synchronous, no network, and only peers
whose announcements have already arrived. `AsyncNetMesh` carries all four
methods and they stay synchronous there — awaiting the returned `list` or `int`
raises `TypeError`.

### Verify it worked

```python
assert any(t.tool_id == "web_search" for t in node.list_tools()), \
    "web_search did not fold — is the pair handshaked and started?"
```

Next: [Invoke a capability](/docs/sdk/python/invoke).
