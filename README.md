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

Application code below is marked as such; every **Net** call is the real surface, and the flows
are runnable — the commands and source links are at the end of this section.

**The provider** serves the capability from the machine that holds the credential:

```rust
use net_sdk::macros::tool;
use net_sdk::mesh::MeshBuilder;

#[derive(JsonSchema, Deserialize, Serialize)]
struct SummarizeReq { doc_id: String }
#[derive(JsonSchema, Deserialize, Serialize)]
struct SummarizeResp { summary: String }

#[tool(description = "Summarize an internal document.", tag = "docs")]
async fn summarize_document(req: SummarizeReq) -> Result<SummarizeResp, String> {
    // Application code, not Net: this reads a credential that never leaves the machine.
    let summary = summarize_with(&internal_docs_credential(), &req.doc_id).await?;
    Ok(SummarizeResp { summary })
}

let provider = MeshBuilder::new("0.0.0.0:7700", &PSK)?.build().await?;
let _handle = summarize_document_register(&provider)?;   // unregisters on drop
provider.announce_capabilities(Default::default()).await?;
```

**The caller** discovers by capability — not by address — and invokes it:

```rust
// Nothing was configured with the provider's hostname. List what's live…
for t in caller.list_tools(None) {
    println!("{} v{}  tags={:?}", t.tool_id, t.version, t.tags);
}

// …then invoke by name. The mesh routes the call to whichever peer serves it.
let resp: SummarizeResp = caller
    .call_tool("summarize_document", &SummarizeReq { doc_id: "q3-plan".into() })
    .await?;
```

**The tool path above is open to any peer that can discover it.** To restrict *who may call a
capability*, serve it as an nRPC service through the org facade. The capability is then announced
privately, org membership is the gate, and the handler receives the verified requester:

```rust
// Provider: serve it to this org only. `OrgAccess::Granted` instead admits a
// cross-org caller holding a capability grant.
mesh.serve_org("summarize.document", OrgAccess::SameOrg, |caller: OrgCaller, req: SummarizeReq| async move {
    Ok(summarize_with(&internal_docs_credential(), &req.doc_id).await?)
})?;

// Caller: bind org credentials once, then call the service.
let org = mesh.org(credentials)?;
let resp: SummarizeResp = org.call("summarize.document", &req).await?;
```

A caller whose org holds no grant never reaches the handler and gets no error *from the
provider*: the private announcement is opaque without the audience, so the call fails **locally**,
before anything is sent, as `OrgSdkError::Discovery`. A membership revoked mid-flight turns the
next call into `OrgSdkError::AdmissionDenied`. Both are pinned by runnable tests:

```bash
cargo run  --example tool_calling --features net,macros   # announce → discover → invoke
cargo test -p net-mesh-sdk org::tests_live               # private discovery, no-grant refusal, revocation
```

In one scenario: the caller **selected a capability, not a machine**; the handler ran **where the
credential lives**; the result came back **typed**; and the authority check **refused before any
bytes were sent**. Sources: [`tool_calling.rs`](net/crates/net/sdk/examples/tool_calling.rs),
[`org/tests_live.rs`](net/crates/net/sdk/src/org/tests_live.rs). Deeper:
[Private capabilities](https://ai2070.net/docs/guides/private-capabilities),
[Security model](https://ai2070.net/docs/concepts/security-model); larger results travel as
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

The substrate under all of that is a flat, encrypted mesh. Three properties do most of the work;
the rest follows.

**Identity outlives a path.** A node is its keypair, and its address is incidental. If a route
breaks, traffic is rerouted and the participants keep the same identity — there is no session to
resume, because nothing was bound to the socket in the first place. Read:
[Architecture](https://ai2070.net/docs/concepts/architecture),
[Events and causality](https://ai2070.net/docs/concepts/events-and-causality).

**Bounded buffers, explicit overload behaviour.** Every node has a fixed-capacity ring buffer.
When it fills, the node drops — oldest or newest, per configuration — instead of growing an
unbounded queue or blocking its producer, and `stats().events_dropped` surfaces it. A node that
cannot keep up goes quiet, and its neighbours route around it. Net's hot path needs those
drop-not-queue semantics, which is why it carries its own transport over UDP rather than layering
on TCP. Read: [Event bus](https://ai2070.net/docs/guides/event-bus).

**No trusted middle.** Relay nodes forward encrypted bytes they cannot read, and there is no
special node whose absence stops the mesh. Read:
[Security model](https://ai2070.net/docs/concepts/security-model).

The performance story — what is fast, and what the numbers do and do not include — is scoped in
[Performance](#performance).

## Worldview

Net's unit is a **capability offered by a provider, together with the authority and live state
needed to use it**. Other systems organize distributed work around different objects — HTTP
around an endpoint, MCP around a tool a configured host may call, NATS around a subject, Zenoh
around a key expression. Net addresses capabilities under identity and authority.

That makes it a substrate beneath applications, not a replacement for their workflows or business
model: a workspace, fleet console, agent runtime, or industrial application can use Net and keep
its own interface, approvals, and user experience.

- [The Agentic Mesh](https://ai2070.net/docs/worldview/agentic-mesh) — the problem from an application's point of view.
- [When to use Net](https://ai2070.net/docs/worldview/right-and-wrong-use-cases) — the fit boundary, including when HTTP, MCP, NATS, or an ordinary database is the simpler choice.
- [How Net relates to other systems](https://ai2070.net/docs/worldview/how-net-compares) — a compact comparison by abstraction, topology, and trust boundary.
- [Net and MCP](https://ai2070.net/docs/worldview/mcp-vs-net) · [Connecting HTTP systems](https://ai2070.net/docs/worldview/rest-vs-net) · [Net and NATS](https://ai2070.net/docs/worldview/nats-vs-net) · [Net and Zenoh](https://ai2070.net/docs/worldview/zenoh-vs-net).

Discovery, invocation, and outcome are separate: finding a provider does not authorize a call, and
a successful invocation is not proof that the real-world outcome holds. See [Submitted is not
completed](https://ai2070.net/docs/guides/submitted-is-not-completed).

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
[`net/crates/net/BENCHMARKS.md`](net/crates/net/BENCHMARKS.md). The rows below are **local
operation microbenchmarks** on an M1 Max: each measures one operation in isolation, not the cost
of a packet path. None includes NIC transfer, wire latency, or propagation, and summing them
would not produce a round trip. Desktop-class figures and per-subsystem tables — multi-hop,
encryption, capability folds, SDK ingestion, binary size — are in the linked file.

| Operation, measured in isolation | M1 Max |
|---|---|
| Header serialize — encode the 64-byte header | 2.19 ns / 456M ops/sec |
| Routing lookup (hit) — resolve a next hop from the local routing table | 37.73 ns / 26.5M ops/sec |
| 1-hop forward — the forwarding path for a single hop | 61.66 ns / 16.2M ops/sec |
| Heartbeat — process one heartbeat from a known peer | 39.76 ns / 25.2M ops/sec |
| Evaluate alternates — pick a replacement from local state | 257.51 ns / 3.88M ops/sec |

The last two are **local computations over local state**; they are not measurements of detecting
a failure across the network or of completing distributed recovery. The full table separates
heartbeat processing, status check, circuit-breaker check, and alternate selection.

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
