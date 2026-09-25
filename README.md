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
- **One identity, several authority planes.** A node is its keypair, and that identity signs what
  it advertises. Who may reach a capability is decided by permission tokens and organization
  grants issued under it; subnet membership is derived from the published tags.

## Install

One engine, several bindings — start with the package for your language:

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

## What it enables

Capabilities, authority, and state on one substrate change what you can build:

**Distance becomes a parameter, not a rewrite.** You invoke a capability the same way whether the
provider sits in-process or on another host — a consistent calling model in which location is a
placement choice. Work has to be expressed as a capability to be reached this way; once it is,
running it beside the caller or across the mesh is a deployment decision, not a rewrite.
[Discover and invoke](https://ai2070.net/docs/guides/discover-and-invoke),
[Architecture](https://ai2070.net/docs/concepts/architecture).

**Sensing and computation stop sharing a body.** A device can produce data without hosting the
intelligence that acts on it, and two sensors can address each other directly. The mesh routes
sense-to-compute and sense-to-sense, wherever each end physically is.
[Capabilities](https://ai2070.net/docs/concepts/capabilities),
[Dataforts](https://ai2070.net/docs/guides/dataforts).

**Coordination that never funnels through a coordinator.** There is no registry, broker, or leader
whose capacity becomes the ceiling. Peers observe their own neighbourhood, derive the rest, and
route, so coordination grows with the participants instead of concentrating in a control plane.
[Event bus](https://ai2070.net/docs/guides/event-bus),
[Capabilities](https://ai2070.net/docs/concepts/capabilities).

**Software that outlives its host.** A daemon is an identity, not a process pinned to a box —
addressed by what it is, placed where its capabilities are, and able to move with its history when
the hardware underneath it changes. A long job can be handed to another participant with a
lifecycle and an explicitly verified outcome.
[Daemons and placement](https://ai2070.net/docs/guides/daemons-and-placement),
[Continuity and migration](https://ai2070.net/docs/guides/continuity-and-migration),
[Task lifecycle](https://ai2070.net/docs/guides/task-lifecycle).

These show up in agent runtimes, robotics and fleet operations, industrial control, edge and IoT,
and local-first collaboration. Built end to end:
[Distributed daemon](https://ai2070.net/docs/tutorials/distributed-daemon),
[Event-sourced service](https://ai2070.net/docs/tutorials/event-sourced-service),
[Fleet telemetry](https://ai2070.net/docs/tutorials/fleet-telemetry).

## Setting up your own mesh

**A few commands, two machines.** One machine runs a node and hands out a join link. The other
uses the link and runs its own node. `up` makes the mesh key for you, so nothing secret is copied
by hand.

```bash
npm install -g @net-mesh/cli                   # installs the `net-mesh` binary

net-mesh up --enroll                           # operator: runs the node, stays in the foreground
net-mesh invite create                         # prints a `netmesh-join_` token — keep it secret
net-mesh join <TOKEN> --yes && net-mesh up     # device: use the link, then run its own node
```

## One system, end to end

An intersection has no line of sight: a building hides the cross traffic from the vehicle
approaching it. A second vehicle, coming the other way, can see. Neither can hand over its
sensors — the raw frames belong to the machine that produced them — but the observation can
travel.

**The vehicle that can see** serves the view under its own authority:

```rust
use net_sdk::mesh::MeshBuilder;
use net_sdk::org::{OrgAccess, OrgCaller};
use serde::{Deserialize, Serialize};
use schemars::JsonSchema;

const PSK: [u8; 32] = [0x42; 32];

#[derive(JsonSchema, Deserialize, Serialize)]
struct ObserveReq { intersection: String }
#[derive(JsonSchema, Deserialize, Serialize)]
struct Observation { class: String, distance_m: f32 }
#[derive(JsonSchema, Deserialize, Serialize)]
struct CornerView { objects: Vec<Observation>, confidence: f32 }

// The node authority and the fleet credentials are provisioned out of band (see
// "Private capabilities"); `onboard_sensors` / `perceive_corner` are application code.
let seeing = MeshBuilder::new("0.0.0.0:7700", &PSK)?.build().await?;

// Serve to this fleet only. `OrgAccess::Granted` admits another operator's vehicle
// holding a capability grant. Hold the handle for the provider's lifetime — dropping
// it unregisters the service.
let _serve = seeing.serve_org("intersection.observe", OrgAccess::SameOrg, |caller: OrgCaller, req: ObserveReq| async move {
    // The raw frames and the perception model never leave this vehicle.
    Ok(perceive_corner(&onboard_sensors(), &req.intersection).await?)
})?;
```

**The vehicle in the blind spot** asks by capability — no peer was configured in advance:

```rust
// The blind-spot vehicle: same mesh and PSK; its fleet credentials are provisioned
// out of band. Handshake with the seeing vehicle (accept on one side, connect on the
// other, both start) before the call.
let blind_spot = MeshBuilder::new("0.0.0.0:7700", &PSK)?.build().await?;

let org = blind_spot.org(credentials)?;                    // sees only what this fleet may see
let view: CornerView = org
    .call("intersection.observe", &ObserveReq { intersection: "5th & Main".into() })
    .await?;
// The blind spot closes on a derived observation, not on a camera feed.
```

Two sensors addressed each other; neither owned the other's hardware, and neither handed over its
data. The seeing vehicle keeps its frames and its model, and the caller receives exactly what it
asked for and nothing more. A vehicle whose operator holds no grant is not shown the capability at
all — the private announcement is opaque without the audience, so discovery finds nothing and the
call refuses locally, before anything is sent.
[Private capabilities](https://ai2070.net/docs/guides/private-capabilities),
[Security model](https://ai2070.net/docs/concepts/security-model). When the caller needs more than
the typed view — a clip, an occupancy grid — it travels as a content-addressed artifact:
[Dataforts](https://ai2070.net/docs/guides/dataforts).

## Why the architecture works

A provider has an identity; its capabilities are discovered under that identity and its owner's
authority; a caller invokes one; and the results, streams, state, and artifacts stay attached to
the work. These are not separate products joined by glue — identity, discovery, channels, typed
RPC, durable logs, folded state, and artifacts are one substrate, so authority and observation
travel with the call instead of being re-established at every boundary.
[Architecture](https://ai2070.net/docs/concepts/architecture),
[Identity](https://ai2070.net/docs/concepts/identity),
[Capabilities](https://ai2070.net/docs/concepts/capabilities),
[nRPC](https://ai2070.net/docs/guides/nrpc),
[Durable logs](https://ai2070.net/docs/guides/durable-logs),
[Folds](https://ai2070.net/docs/guides/cortex-folds),
[Dataforts](https://ai2070.net/docs/guides/dataforts),
[Daemons](https://ai2070.net/docs/guides/daemons-and-placement).

Underneath sits a flat, encrypted mesh. Three properties do most of the work.

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

The rest of the surface, one line each; every entry links to the page that goes deep.

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
