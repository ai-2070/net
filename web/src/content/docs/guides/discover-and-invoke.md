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
net cap query --tag gpu --tag vram:24      # nodes that advertise BOTH tags
net cap nodes                              # every (node, capabilities) the index knows
net cap show                               # the local node's own capabilities
```

`--tag` is required and repeatable; a node matches only when its advertised set
contains **every** tag you list. Announcements propagate multi-hop across the mesh
(bounded by a hop count), so `net cap query` can return a node several hops away,
not just a direct neighbor.

From the SDK, the same query returns node ids you can call directly:

```rust
use net_sdk::capabilities::CapabilityFilter;

let filter = CapabilityFilter { require_gpu: true, min_vram_gb: Some(24), ..Default::default() };
let nodes = mesh.find_nodes(&filter).await?;   // node ids that match, right now
```

For richer predicates (numeric thresholds, semver, AND/OR/NOT), see the capability
predicate surface in [Capabilities](/docs/concepts/capabilities).

## Invoke: a capability is an nRPC service

Discovery is advisory — it tells you *who can*. To actually do the work, call the
capability. A native capability is served over **nRPC** (typed request/response on
the mesh):

```rust
use net_sdk::mesh::MeshBuilder;
use net_sdk::mesh_rpc::CallOptions;
use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Serialize, Deserialize)]
struct SummarizeReq { text: String }
#[derive(Serialize, Deserialize)]
struct SummarizeResp { summary: String }

// Provider side: announce + serve a capability.
let provider = MeshBuilder::new("127.0.0.1:0", &psk)?.build().await?;
let _handle = provider.serve_rpc_typed("summarize", |req: SummarizeReq| async move {
    Ok::<_, String>(SummarizeResp { summary: summarize(&req.text) })
})?;

// Caller side: discover a provider, then make a typed call with a deadline.
let caller = MeshBuilder::new("127.0.0.1:0", &psk)?.build().await?;
// (handshake / join the mesh — see the harness note below)
let resp: SummarizeResp = caller.call_typed(
    provider_node_id,
    "summarize",
    &SummarizeReq { text: "…".into() },
    CallOptions::default().with_deadline(Duration::from_millis(500)),
).await?;
```

`serve_rpc_typed` / `call_typed` are the same primitive across the SDKs
(TS/Python/Go/C wrap the same core — see [Typed RPC with nRPC](/docs/guides/nrpc)).
The call is typed on both ends, deadlined, and cancellable; there is no separate
RPC broker, sidecar, or IDL step.

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
[`net mcp serve`](/docs/guides/expose-net-as-mcp), which performs the same nRPC
invocation under the hood, gated by the pin/consent flow.

Next: [Recover a Failed Workflow](/docs/guides/recover-failed-workflow) — because
discovering and invoking is only half the job; the other half is what happens when
the call fails.
