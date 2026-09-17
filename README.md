# Net

[![License: MIT OR Apache-2.0](https://img.shields.io/badge/License-MIT%20OR%20Apache--2.0-blue.svg)](#license)
[![codecov](https://codecov.io/gh/ai-2070/net/graph/badge.svg?token=AOBMOF6LE4)](https://codecov.io/gh/ai-2070/net)

**Net connects agents, services, and devices into a capability mesh.**

Discover what another machine can do, invoke it through typed RPC, and move artifacts between
participants while resource owners keep control of access. Underneath is a latency-first
encrypted mesh — identity, discovery, channels, typed RPC, durable logs, folded state, and
artifacts share one substrate, and the same substrate runs vehicular, industrial, robotics, and
edge workloads.

- **No broker, no registry, no coordinator.** Peers find each other by what they can do.
- **Work runs where the resource lives.** A credential never leaves the machine that holds it;
  the caller invokes a capability, not a host.
- **One identity and policy surface.** The key that names a node also signs what it advertises,
  who may reach it, and which subnet that traffic belongs to.

Where this fits and where it doesn't: [When to Use Net](https://ai2070.net/docs/worldview/right-and-wrong-use-cases).

## One system, end to end

A document-processing capability lives on a machine that holds an internal docs-API credential.
The credential must not travel. A caller on another machine wants a summary.

**The provider** declares the capability and serves it from the machine that holds the
credential:

```rust
use net_sdk::macros::tool;
use net_sdk::mesh::MeshBuilder;

#[derive(JsonSchema, Deserialize, Serialize)]
struct SummarizeReq { doc_id: String }
#[derive(JsonSchema, Deserialize, Serialize)]
struct SummarizeResp { summary: String }

#[tool(description = "Summarize an internal document.", tag = "docs")]
async fn summarize_document(req: SummarizeReq) -> Result<SummarizeResp, String> {
    let cred = internal_docs_credential();        // application secret; stays on this machine
    let summary = summarize_with(&cred, &req.doc_id).await?;
    Ok(SummarizeResp { summary })
}

let provider = MeshBuilder::new("0.0.0.0:7700", &PSK)?.build().await?;
let _handle = summarize_document_register(&provider)?;   // unregisters on drop
provider.announce_capabilities(Default::default()).await?;
```

**The caller** asks for the capability by name — not by address — and gets a typed result:

```rust
// Nothing was configured with the provider's hostname. Discover what's live…
for t in caller.list_tools(None) {
    println!("{} v{}  tags={:?}", t.tool_id, t.version, t.tags);
}

// …then invoke it. The mesh routes the call to whichever peer serves it.
let resp: SummarizeResp = caller
    .call_tool("summarize_document", &SummarizeReq { doc_id: "q3-plan".into() })
    .await?;
```

**When it goes wrong, it says so.** A call carries a deadline and returns an error — it does not
hang forever. If no peer still serves the capability, or the capability was served org-private
and is therefore not discoverable to this caller, the call fails rather than quietly succeeding:

```rust
match caller.call_tool::<SummarizeReq, SummarizeResp>("summarize_document", &req).await {
    Ok(resp)  => println!("{}", resp.summary),
    Err(e)    => eprintln!("cannot summarize right now: {e}"),  // provider gone, or not authorized
}
```

What this shows: the caller **selected a capability, not a machine**; the handler ran **where the
credential lives**; the result came back **typed**; the failure path is a real error, not a
promise of transparent recovery. Authorization is a first-class part of the same system — a
provider can serve a capability **org-private**, so an unauthorized caller never discovers it:
[Private capabilities](https://ai2070.net/docs/guides/private-capabilities),
[Security model](https://ai2070.net/docs/concepts/security-model). Larger results travel as
content-addressed artifacts: [Dataforts](https://ai2070.net/docs/guides/dataforts).

## Install

```bash
cargo add net-mesh-sdk                        # Rust
npm install @net-mesh/sdk @net-mesh/core      # TypeScript / Node
pip install net-mesh-sdk                      # Python
go get github.com/ai-2070/net/go              # Go
```

Published names and source imports differ on purpose: the crates/registries use
`net-mesh*` / `@net-mesh/*`, while source imports are `net_sdk`, `@net-mesh/sdk`, and
`from net_sdk import ...`. Lower-level bindings that skip the SDK ergonomics are in
[SDKs](#sdks). Full per-language setup:
[Install](https://ai2070.net/docs/start/install), [Quickstart](https://ai2070.net/docs/start/quickstart).

## Why the pieces belong together

The interesting part is not any one primitive — it is that they compose without glue, because
they share a substrate:

| Piece | What it gives you | Read |
|---|---|---|
| Identity | A node *is* its ed25519 keypair; delegable permission tokens gate access | [Identity](https://ai2070.net/docs/concepts/identity), [Organizations](https://ai2070.net/docs/concepts/organizations) |
| Discovery | Capabilities announced and indexed locally; no registry to run | [Capabilities](https://ai2070.net/docs/concepts/capabilities), [Discover and invoke](https://ai2070.net/docs/guides/discover-and-invoke) |
| Channels | Named pub/sub that is a name you match on, not a broker you connect to | [Channels](https://ai2070.net/docs/concepts/channels), [Event bus](https://ai2070.net/docs/guides/event-bus) |
| Typed RPC | Request/response on the same transport — no second stack, no sidecar | [nRPC](https://ai2070.net/docs/guides/nrpc) |
| Durable logs | An append-only stream that *is* the state, per-node and per-file | [RedEX](https://ai2070.net/docs/guides/durable-logs), [Storage stack](https://ai2070.net/docs/concepts/storage-stack) |
| Folded state | A local, reactive view of that log — a value in your program, not a server | [Folds](https://ai2070.net/docs/guides/cortex-folds), [NetDB](https://ai2070.net/docs/guides/netdb-queries) |
| Artifacts | Content-addressed blobs that follow the reads, with read-your-writes | [Dataforts](https://ai2070.net/docs/guides/dataforts) |
| Execution | Stateful daemons addressed by identity, placed by capability, moved live | [Daemons](https://ai2070.net/docs/guides/daemons-and-placement), [Agent identity](https://ai2070.net/docs/concepts/agent-identity) |

## Why the mesh

The substrate under all of that is a flat, encrypted, latency-first mesh. Two design choices do
most of the work; the rest follows.

**State, not connections.** Traditional networking makes the connection the primary object: it
breaks, the relationship breaks. Net propagates state. Connections are ephemeral transport, and
identity lives in the event chain rather than in a socket — so a path breaking is a routing
change, not a session loss. Read: [Events and causality](https://ai2070.net/docs/concepts/events-and-causality),
[Architecture](https://ai2070.net/docs/concepts/architecture).

**Drop instead of queue.** In a best-effort network, queues absorb bursts and a delivery
guarantee is a virtue. At nanosecond timescales a queue is just added latency: a node that
accepts work it cannot process has broken its own self-preservation. Net nodes drop what they
cannot handle and go silent; neighbors observe the silence and route around it. That is why Net
runs its own transport over UDP — the two queue models are incompatible at the buffer level, not
merely different in tuning.

**What "fast" means here.** Net's per-packet *scheduling* — process, route, encrypt, queue for
transmission — is measured in nanoseconds. Those numbers **exclude** NIC transfer, wire latency,
and speed-of-light propagation; they show the software layer is no longer the bottleneck, not
that a round trip is instant. The strongest end-to-end claims in this project are scoped in
[Benchmarks](#performance). A fuller argument for the model is in the
[worldview docs](https://ai2070.net/docs/worldview).

## What's in the box

A compressed tour; each links to the page that goes deep.

| Surface | One line | Read |
|---|---|---|
| Subnets | Boundaries derived from capability tags, enforced at the channel — not VLANs | [Subnets](https://ai2070.net/docs/concepts/subnets) |
| MeshDB | Federated queries across nodes over the same facade | [Federated queries](https://ai2070.net/docs/guides/netdb-queries#federated-queries-meshdb) |
| Scheduler | Atomic gang-claim of a contended resource, with a task lifecycle on top | [Gang scheduler](https://ai2070.net/docs/guides/gang-scheduler), [Task lifecycle](https://ai2070.net/docs/guides/task-lifecycle) |
| MCP bridge | Wrap a stdio MCP server into mesh capabilities, or serve the mesh as MCP | [Wrap MCP](https://ai2070.net/docs/guides/wrap-mcp-server), [Expose as MCP](https://ai2070.net/docs/guides/expose-net-as-mcp) |
| Payments | x402 pricing, quotes, settlement, spend policy — signed facts around a call | [Net payments](https://ai2070.net/docs/payments/what-net-payments-is) |
| A2A | Hand a long job to an agent that doesn't share your memory | [Agent to agent](https://ai2070.net/docs/guides/agent-to-agent) |
| Subprotocols | Opaque forwarding, version negotiation, a protocol runtime not a fixed protocol | [Subprotocol IDs](https://ai2070.net/docs/reference/subprotocol-ids) |
| Delegation | Child seeds and revocation for delegated identity | [Agent identity](https://ai2070.net/docs/concepts/agent-identity) |
| Operator surface | MeshOS supervision and the Deck TUI | [Deck](https://ai2070.net/docs/reference/deck) |
| Security | No plaintext on relays, no clock dependency, no trusted intermediary | [Security model](https://ai2070.net/docs/concepts/security-model) |

## Performance

The full measured set and methodology live in
[`net/crates/net/BENCHMARKS.md`](net/crates/net/BENCHMARKS.md). The numbers below are
**packet-scheduling** measurements on an M1 Max — time to process, route, encrypt, and queue a
packet for transmission. They do **not** include NIC transfer, wire latency, or propagation, and
they are not end-to-end latencies.

| Operation | M1 Max |
|---|---|
| Header serialize | 2.19 ns / 456M ops/sec |
| Routing lookup (hit) | 37.73 ns / 26.5M ops/sec |
| 1-hop forward | 61.66 ns / 16.2M ops/sec |
| Heartbeat (existing node) | 39.76 ns / 25.2M ops/sec |
| Recovery — evaluate alternates | 257.51 ns / 3.88M ops/sec |

Read the failure-detection rows precisely: **heartbeat processing** (39.76 ns), **status check**
(15.10 ns), **circuit-breaker check** (9.55 ns), and **alternate selection** (257.51 ns) are
local computations over local state. They are not measurements of detecting a failure across the
network or of completing distributed recovery. Desktop-class figures and per-subsystem tables —
multi-hop, encryption, capability folds, SDK ingestion, binary size — are in the linked file.

## SDKs

All SDKs wrap the same Rust core. The SDK is the developer experience; the engine is Rust.

| SDK | Package | Install |
|-----|---------|---------|
| **Rust** | [`net-mesh-sdk`](https://crates.io/crates/net-mesh-sdk) ([source](net/crates/net/sdk)) | `cargo add net-mesh-sdk` |
| **TypeScript** | [`@net-mesh/sdk`](https://www.npmjs.com/package/@net-mesh/sdk) ([source](net/crates/net/sdk-ts)) | `npm install @net-mesh/sdk @net-mesh/core` |
| **Python** | [`net-mesh-sdk`](https://pypi.org/project/net-mesh-sdk/) ([source](net/crates/net/sdk-py)) | `pip install net-mesh-sdk` |
| **C** | [`net.h`](net/crates/net/include/net.h) ([source](net/crates/net/include)) | `cargo build --release --features ffi,net` |
| **Go** | [`go`](go/) | `go get github.com/ai-2070/net/go` |

Lower-level bindings (skip the SDK ergonomics, talk directly to the engine):

| Binding | Package | Install |
|---------|---------|---------|
| **Rust core** | [`net-mesh`](https://crates.io/crates/net-mesh) | `cargo add net-mesh` |
| **Node binding** | [`@net-mesh/core`](https://www.npmjs.com/package/@net-mesh/core) | `npm install @net-mesh/core` |
| **Python binding** | [`net-mesh`](https://pypi.org/project/net-mesh/) | `pip install net-mesh` |

## Claude Code Skill

Net looks like Kafka or NATS from the outside and is not one underneath; an agent working from
surface familiarity will write integration code that runs and is quietly wrong. Install the
skills first:

```bash
npx skills add ai-2070/net-claude-skill -g     # drop -g for the current project only
```

Pair them with [`opensrc`](https://github.com/vercel-labs/opensrc) so the agent can read Net's
real source instead of guessing a signature — one fetch covers all five bindings:

```bash
npx -y opensrc@latest path ai-2070/net
```

Full install options: [Claude Skills](https://ai2070.net/docs/start/claude-skills).

## Origin

Net is loosely inspired by the Net from *Cyberpunk 2077* — a flat, encrypted mesh where every
device is a first-class node. Not affiliated with CD Projekt Red or R. Talsorian Games; this is
an engineering take on the concept, not a licensed adaptation. Implementation details, the module
map, and code examples live in the [crate README](net/crates/net/README.md).

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or <http://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or <http://opensource.org/licenses/MIT>)

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in
this project shall be licensed as MIT OR Apache-2.0, at the recipient's option. Contributions are
subject to the [Contributor License Agreement](CONTRIBUTING.md#contributor-license-agreement),
which must be signed before a first contribution is merged.
