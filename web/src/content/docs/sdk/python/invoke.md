# Python — Invoke a Capability

Call a discovered tool with `call_tool` — it finds a provider for the named tool,
makes a typed request, and returns the result. This is the Python counterpart of
Rust's `call_tool` / TypeScript's `callTool`.

```python
from net_sdk import call_tool

resp = call_tool(node, "web_search", {"query": "how does the capability fold work"})
print(resp)
```

There's an async variant for asyncio code, plus streaming:

```python
from net_sdk import call_tool_async, call_tool_streaming

resp = await call_tool_async(node, "web_search", {"query": "…"})

for chunk in call_tool_streaming(node, "tail", {"tail": "events"}):
    handle(chunk)
```

`call_tool` and friends take the node first, then the tool name and the request
payload (a dict or a typed model). The exact argument shapes are in the
`tool_calling` example; typed request/response with deadlines and cancellation over
raw nRPC is in [Typed RPC with nRPC](/docs/guides/nrpc).

## Policy: invocation is authorized, discovery is not

Seeing a capability does not grant the right to invoke it. A provider enforces
scope at call time — an owner-only capability rejects a caller outside its scope,
verified against the authenticated origin, regardless of who can see it. For
wrapped MCP tools this is the owner-scope / consent model in
[Wrap an MCP Server](/docs/guides/wrap-mcp-server).

## Next

[Watch](/docs/sdk/python/watch).
