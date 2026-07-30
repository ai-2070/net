## Discover it — Python

### List tools

`list_tools` and `watch_tools` take the **native** mesh handle:

```python
from net_sdk import list_tools

native = node._native          # no public accessor yet

for t in list_tools(native):
    print(t.tool_id, "v" + t.version, "tags=", t.tags)
```

Passing the `net_sdk.MeshNode` raises `AttributeError` — the wrapper does not
carry the tool surface.

Schemas come back as **JSON-encoded strings** on `descriptor.input_schema` and
`descriptor.output_schema`. `json.loads` them before use.

### Watch for changes

`watch_tools` is an **async** iterator, so it needs an event loop even if the rest
of your program is synchronous:

```python
import asyncio
from net_sdk import list_tools, watch_tools

async def follow(native):
    for t in list_tools(native):        # baseline, synchronous
        print("baseline", t.tool_id)

    async for change in watch_tools(native):
        match change.type:
            case "added":   print("+", change.descriptor.tool_id)
            case "removed": print("-", change.descriptor.tool_id)
            case "node_count_changed":
                print("~", change.descriptor.tool_id, change.prev_node_count,
                      "->", change.descriptor.node_count)

asyncio.run(follow(native))
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
peers = native.find_nodes({"require_tags": ["gpu"], "min_vram_gb": 24})
```

`find_nodes` is on the native handle too, and takes a plain dict rather than a
typed filter object. The predicate model is identical across bindings — see
[Capabilities](/docs/concepts/capabilities) for the full surface and the CLI
equivalent.

### Verify it worked

```python
assert any(t.tool_id == "web_search" for t in list_tools(native)), \
    "web_search did not fold — is the pair handshaked and started?"
```

Next: [Invoke a capability](/docs/sdk/python/invoke).
