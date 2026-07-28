---
title: Discover and Invoke
description: "The core agent loop, without an MCP host in the middle: ask the mesh who can do the work, then make a typed call and get a typed result."
---
# Discover and Invoke

The core agent loop, without an MCP host in the middle: ask the mesh who can do
the work, then make a typed call and get a typed result. This is the **native**
path — richer than the [MCP bridge](/docs/guides/wrap-mcp-server), because a native
capability can stream, fail as a typed event, move artifacts, and recover.

## Discover: query by capability, not by host

A node **announces** what it can do (tags, schema, availability); every peer folds
that announcement into a local capability index; you query the index by what you
need. From the CLI:

```
net-mesh cap query --tag hardware.gpu --tag hardware.gpu.vram_gb=24
net-mesh cap nodes    # every (node, capabilities) the index knows
net-mesh cap show     # the local node's own capabilities
```

`--tag` is required and repeatable; a node matches only when its advertised set
contains **every** tag you list. Announcements propagate multi-hop across the mesh
(bounded by a hop count), so `net-mesh cap query` can return a node several hops
away, not just a direct neighbor.

Two things to get right, because the CLI will not warn you:

- **Tags are matched as exact strings, against the canonical wire form.** The
  index holds what the node announced — `hardware.gpu`, `hardware.gpu.vram_gb=24`
  — so `--tag gpu` or `--tag vram:24` match nothing at all. A typed builder
  (`HardwareCapabilities::new().with_gpu(...)`) emits the canonical form; see
  [Capabilities](/docs/concepts/capabilities) for the full key schema.
- **The CLI is separator-sensitive; the SDK is not.** `cap query` does plain set
  membership, so a node that announced `hardware.gpu.vram_gb=24` is not found by
  `--tag hardware.gpu.vram_gb:24`. The SDK's `has_tag` deliberately treats `=`
  and `:` as equivalent — the CLI path does not go through it. Match the
  separator the announcer used, or query from the SDK.
- **There is no threshold matching here.** `--tag hardware.gpu.vram_gb=24` means
  *exactly 24*, not *at least 24* — a 80 GB H100 does not match. For "≥ 24 GB"
  you need a predicate (`num_at_least`) through the SDK, below.

From the SDK, the same query returns node ids you can call directly:

```rust
use net_sdk::capabilities::CapabilityFilter;

let filter = CapabilityFilter { require_gpu: true, min_vram_gb: Some(24), ..Default::default() };
let nodes: Vec<u64> = mesh.find_nodes(&filter);   // sync — node ids that match, right now
```

For richer predicates (numeric thresholds, semver, AND/OR/NOT), see the capability
predicate surface in [Capabilities](/docs/concepts/capabilities).

## Invoke: a capability is an nRPC service

Discovery is advisory — it tells you *who can*. To actually do the work, call the
capability. A native capability is served over **nRPC** (typed request/response on
the mesh):

```rust
use net_sdk::mesh::MeshBuilder;
use net_sdk::mesh_rpc::{CallOptions, CallOptionsTyped, Codec};
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

#[derive(Serialize, Deserialize)]
struct SummarizeReq { text: String }
#[derive(Serialize, Deserialize)]
struct SummarizeResp { summary: String }

// Provider side: announce + serve a capability.
let provider = MeshBuilder::new("127.0.0.1:0", &psk)?.build().await?;
let _handle = provider.serve_rpc_typed("summarize", Codec::Json, |req: SummarizeReq| async move {
    Ok::<_, String>(SummarizeResp { summary: summarize(&req.text) })
})?;

// Caller side: discover a provider, then make a typed call with a deadline.
let caller = MeshBuilder::new("127.0.0.1:0", &psk)?.build().await?;
// (handshake / join the mesh — see the harness note below)
let resp: SummarizeResp = caller.call_typed(
    provider_node_id,
    "summarize",
    &SummarizeReq { text: "…".into() },
    CallOptionsTyped {
        raw: CallOptions { deadline: Some(Instant::now() + Duration::from_millis(500)), ..Default::default() },
        ..Default::default()
    },
).await?;
```

`serve_rpc_typed` / `call_typed` are the same primitive across the SDKs
(TS/Python/Go/C wrap the same core — see [Typed RPC with nRPC](/docs/guides/nrpc)).
The call is typed on both ends, deadlined, and cancellable; there is no separate
RPC broker, sidecar, or IDL step.

### Discovering tools from other languages

The discovery half of the loop has a first-class surface in every binding. Same
fold underneath, same advisory semantics — a snapshot plus a change stream:

```typescript
import { listTools, watchTools, callTool } from "@net-mesh/sdk";

for (const t of await listTools(rpc)) {
  console.log(t.toolId, t.tags);
}

for await (const change of watchTools(rpc)) {
  console.log("fold changed:", change);
}
```

```python
from net_sdk import list_tools, watch_tools

for t in list_tools(node):                 # baseline snapshot
    print(t.tool_id, t.tags)

async for change in watch_tools(node):     # pushed on fold mutation
    print(change)
```

```go
tools, err := rpc.ListTools()
if err != nil {
    log.Fatal(err)
}
for _, t := range tools {
    fmt.Println(t.ToolID, t.Tags)
}
```

`watchTools` / `watch_tools` are **event-driven off the capability fold's change
signal**, not polls — an idle mesh costs nothing, and a change arrives the moment
the fold mutates. Where these APIs take an `interval`, it's a staleness ceiling
(a safety-net re-diff), not a poll rate.

Go has no watch iterator; it lists and re-lists. Python's capability-*filter*
node discovery (`find_nodes`, "GPU nodes with ≥24 GB VRAM") is reachable only
through the native handle rather than a clean `MeshNode` method — the tool API
above is the path for most Python agent code.

## A complete, runnable two-node loop today

The end-to-end wrap → discover → invoke loop across two nodes — including the
mesh handshake, owner-scope enforcement, and the invoke round-trip — is
demonstrated as a runnable test in `adapters/mcp/tests/wrap_end_to_end.rs`
(`wrap_discover_and_invoke_across_two_nodes`) and, for the MCP-host path,
`adapters/mcp/tests/serve_end_to_end.rs`
(`gateway_searches_describes_and_invokes_across_two_nodes`). Those are the
authoritative, copy-from templates for standing up two `Mesh` nodes, joining them,
and driving the loop — start there rather than assembling the handshake by hand.

## Invoke through an MCP host

If your agent lives in an MCP host, you don't call `call_typed` directly — the
host calls the `net_invoke_capability` meta-tool exposed by
[`net-mesh mcp serve`](/docs/guides/expose-net-as-mcp), which performs the same nRPC
invocation under the hood, gated by the pin/consent flow.
