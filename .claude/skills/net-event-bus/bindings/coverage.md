# Binding coverage — what each language can actually do

Read this **before promising a surface exists**. Net is one mesh with five
bindings, and they are not at parity. Writing Go code that announces a
capability is fine; writing Go code that hands a task to an agent is not, because
there is no Go A2A surface at all.

This matrix is the answer to "does binding X support operation Y", and it is the
only place that answer is maintained. Subsystem chapters describe *how* an
operation works; they are not a coverage record.

## Where each binding lives

| Binding | Install as | Import as | Source |
|---|---|---|---|
| Rust | `net-mesh-sdk` | `net_sdk` | `net/crates/net/sdk/src/` |
| Node / TS | `@net-mesh/sdk` over `@net-mesh/core` | `@net-mesh/sdk` | `net/crates/net/sdk-ts/src/` over `net/crates/net/bindings/node/` |
| Python | `net-mesh-sdk` over `net-mesh` | `net_sdk` over `net` | `net/crates/net/sdk-py/src/net_sdk/` over `net/crates/net/bindings/python/` |
| Go | `github.com/ai-2070/net/go` | `net` | `go/` |
| C | — (link `libnet`) | `net.h` and friends | `net/crates/net/include/` |

**The Go row means the shipped module only.** A second Go tree exists at
`net/crates/net/bindings/go/net/` — a reference implementation with no `go.mod`,
meant to be vendored or copied into your own module. It covers surfaces the
shipped module does not. Nothing below describes it; if a cell says `not
exposed`, check there before concluding the work has not been done.

### C is eleven headers over one library, not one SDK

A `supported` C cell tells you a symbol exists. It does not tell you that you can
reach it from the translation unit you are already in.

| Header | Guard | Surface | Link against |
|---|---|---|---|
| `net.h` | `NET_SDK_H` | Event bus | `libnet` |
| `net.go.h` | `NET_SDK_H` | Mesh, capabilities, channels, compute | `libnet` |
| `net_cortex.h` | `NET_CORTEX_H` | RedEX, CortEX, NetDb | `libnet` |
| `net_transport.h` | `NET_TRANSPORT_H` | Blob + directory transfer | `libnet` |
| `net_rpc.h` | `NET_RPC_H` | nRPC | `libnet` |
| `net_meshdb.h` | `NET_MESHDB_H` | Federated queries | `libnet` |
| `net_meshos.h` | `NET_MESHOS_H` | Daemon authoring | `libnet` |
| `net_deck.h` | `NET_DECK_H` | Operator surface | `libnet` |
| `net_org.h` | `NET_ORG_H` | Organization capability auth | `libnet` |
| `net_subnet.h` | `NET_SUBNET_H` | Subnet authority — exported serve, gateway provisioning | `libnet` |
| `net_mcp.h` | `NET_MCP_H` | MCP bridge, consent / pin surface | `libnet` |

**`net.h` and `net.go.h` share the `NET_SDK_H` guard, and `net.go.h` is not a
superset.** Include one and the other silently vanishes. `net_ingest_raw_ex`,
`net_poll_ex` and `net_stats_ex` are `net.h`-only, while every `net_mesh_*`
symbol in the matrix below — announce, discovery, channels, streams, the island
scheduler — lives in `net.go.h`. So "event bus" and "capabilities" are both
`supported` in C and still cannot be used in the same translation unit. Split
across two, or you will get an implicit-declaration error that reads like a
missing feature flag.

## How to read a cell

Two independent dimensions, because one column could not answer "is Go payments
partial, or verify-only?" — those are not competing values, they qualify each
other.

| Column | Values |
|---|---|
| **Status** | `supported` · `partial` · `experimental` · `not exposed` · `n/a` |
| **Mode** | *(blank)* · `poll` · `verify-only` · `core-only` |

A mode is written after the status: `supported · core-only`.

- **`core-only`** — the operation exists, but only on the low-level binding
  (`@net-mesh/core`, `net`), not the ergonomic wrapper. You must import from the
  lower package. This is the single most common way to be wrong about Net in
  Node and Python.
- **`poll`** — no push or async-iteration form; you drive it on an interval.
- **`n/a` means "this operation makes no sense here", not "this binding lacks
  it."** A missing surface that *could* be built is `not exposed`. The
  difference matters: `n/a` tells a reader the gap is permanent and stops anyone
  asking for it.

<!-- coverage:status -->

| Operation | Rust | Node / TS | Python | Go | C |
|---|---|---|---|---|---|
| Event bus — ingest + poll | supported | supported | supported | supported | supported · poll |
| Consumer-side filter DSL | supported | supported | supported | not exposed | supported |
| Distributed mesh channels — register / subscribe / publish | supported | supported | supported · core-only | supported | supported |
| Tagged EventBus topics (node.channel) | n/a | supported | supported | n/a | n/a |
| Channel subscribe with a full TokenChain | not exposed | not exposed | not exposed | not exposed | not exposed |
| Delegated publish chain (set_publish_chain) | not exposed | not exposed | not exposed | not exposed | not exposed |
| Prefix-matched channel ACL registration | supported | not exposed | not exposed | not exposed | not exposed |
| Channel policy — origin binding + queue-group policy | supported | not exposed | not exposed | not exposed | not exposed |
| Queue-group channel subscription | not exposed | not exposed | not exposed | not exposed | not exposed |
| Channel token roots (require_token anchoring) | supported | supported | supported · core-only | supported | supported |
| Distributed batch fan-out (publish_many) | supported | not exposed | not exposed | not exposed | not exposed |
| Membership rejection reason (AckReason taxonomy) | supported | partial | partial | partial | partial |
| Permissive channel registry (opt out of strict default) | not exposed | supported · core-only | supported · core-only | not exposed | not exposed |
| Mesh streams | supported | supported | supported | supported | supported |
| Capability announce | supported | supported | supported | supported | supported |
| Capability discovery | supported | supported | supported | supported | supported |
| nRPC — typed request/response + streaming | supported | supported | supported | supported | supported |
| Gang-claim scheduler | supported | supported | supported | supported | supported |
| A2A — agent task handoff | supported | supported · core-only | supported · core-only | not exposed | not exposed |
| Organization capability auth | supported | supported | supported | supported | supported |
| Subnet gateway provisioning | supported | supported | supported | supported | supported |
| Subnet-exported nRPC serve | supported | supported | supported | supported | supported |
| Subnet-exported organization call | supported | supported | supported | supported | supported |
| MCP bridge | supported | supported | supported | supported | supported |
| Dataforts — blobs | supported | supported | supported | partial | supported |
| RedEX — durable log | supported | supported | supported | supported | supported |
| CortEX folds / NetDB | supported | supported | supported | supported | supported |
| MeshDB — federated queries | supported | supported | supported | supported | supported |
| Compute / groups / daemons | supported | supported | supported · core-only | supported | supported |
| Deck — operator surface | supported | supported | supported | supported | supported |
| Redis Streams dedup | supported | supported | supported · core-only | supported | supported |

## Evidence

One anchor per positive cell — a symbol CI resolves in that binding's tree.
Names differ per binding on purpose; that difference *is* the reason an agent
should not infer one binding's API from another's.

<!-- coverage:anchors -->

| Operation | Rust | Node / TS | Python | Go | C |
|---|---|---|---|---|---|
| Event bus — ingest + poll | `emit` | `ingestFire` | `ingest` | `Ingest` | `net_ingest_raw` |
| Consumer-side filter DSL | `pred` | `predicateDebugReport` | `evaluate_predicate` | — | `net_predicate_evaluate` |
| Distributed mesh channels — register / subscribe / publish | `subscribe_channel` | `subscribeChannel` | `subscribe_channel` | `SubscribeChannel` | `net_mesh_subscribe_channel_with_token` |
| Tagged EventBus topics (node.channel) | — | `TypedChannel` | `TypedChannel` | — | — |
| Channel subscribe with a full TokenChain | — | — | — | — | — |
| Delegated publish chain (set_publish_chain) | — | — | — | — | — |
| Prefix-matched channel ACL registration | `register_channel_prefix` | — | — | — | — |
| Channel policy — origin binding + queue-group policy | `with_subscriber_origin_binding` | — | — | — | — |
| Queue-group channel subscription | — | — | — | — | — |
| Channel token roots (require_token anchoring) | `with_token_roots` | `tokenRoots` | `token_roots` | `TokenRoots` | `net_mesh_register_channel` |
| Distributed batch fan-out (publish_many) | `publish_many` | — | — | — | — |
| Membership rejection reason (AckReason taxonomy) | `AckReason` | `ChannelAuthError` | `ChannelAuthError` | `ErrChannelAuth` | `NET_ERR_CHANNEL_AUTH` |
| Permissive channel registry (opt out of strict default) | — | `permissiveChannels` | `permissive_channels` | — | — |
| Mesh streams | `open_stream` | `openStream` | `open_stream` | `OpenStream` | `net_mesh_open_stream` |
| Capability announce | `announce_capabilities` | `announceCapabilities` | `announce_capabilities` | `AnnounceCapabilities` | `net_mesh_announce_capabilities` |
| Capability discovery | `find_best_node` | `findBestNode` | `find_best_node` | `FindBestNode` | `net_mesh_find_best_node` |
| nRPC — typed request/response + streaming | `call_typed` | `TypedMeshRpc` | `call_streaming` | `NewTypedMeshRpc` | `net_rpc_call` |
| Gang-claim scheduler | `claim_island` | `claimIsland` | `claim_island` | `ClaimIsland` | `net_mesh_claim_island` |
| A2A — agent task handoff | `serve_a2a` | `serveA2a` | `serve_a2a` | — | — |
| Organization capability auth | `serve_org` | `serveOrgTyped` | `serve_org_typed` | `ServeOrgBytes` | `net_org_call` |
| Subnet gateway provisioning | `install_gateway_credentials_node` | `installSubnetGatewayCredentials` | `install_subnet_gateway_credentials` | `InstallSubnetGatewayCredentials` | `net_subnet_install_gateway_credentials` |
| Subnet-exported nRPC serve | `serve_subnet_exported` | `serveSubnetExported` | `serve_subnet_exported` | `ServeSubnetExported` | `net_subnet_serve_exported` |
| Subnet-exported organization call | `call_exported` | `callExported` | `call_exported` | `CallExportedBytes` | `net_org_call_exported` |
| MCP bridge | `serve_tool` | `tool` | `tool` | `McpError` | `net_mcp_classify` |
| Dataforts — blobs | `fetch_blob` | `fetchBlob` | `fetch_blob` | `NewMeshBlobAdapter` | `net_fetch_blob` |
| RedEX — durable log | `redex` | `RedexFile` | `redex` | `NewRedex` | `net_redex_file_append` |
| CortEX folds / NetDB | `cortex` | `cortex` | `netdb` | `OpenNetDb` | `net_fold_query_client_query_latest` |
| MeshDB — federated queries | `meshdb` | `meshdb` | `meshdb` | `MeshDB` | `net_meshdb_decode_payload_json` |
| Compute / groups / daemons | `groups` | `groups` | `daemon_count` | `Groups` | `net_compute_fork_group_fork_count` |
| Deck — operator surface | `deck` | `deck` | `deck` | `Deck` | `net_deck_admin_cordon` |
| Redis Streams dedup | `RedisStreamDedup` | `RedisStreamDedup` | `RedisStreamDedup` | `RedisStreamDedup` | `net_redis_dedup_new` |

## Why the negative cells are negative

Machine-checking absence is not possible — see the note at the end — so each one
carries a reason instead.

**Go has no consumer-side filter DSL.** The predicate surface
(`evaluate_predicate`, the `where` RPC header, trace and debug reports) has no Go
equivalent. `CapabilityFilter` in `go/mesh.go` is channel *authorisation*, a
different thing, and `go/meshdb.go`'s "filter predicates" are MeshDB query
predicates. Filter in your handler, or call from a binding that has it. This is
`not exposed`, not `n/a` — nothing about Go makes a predicate API unnatural.

**Go and C have no A2A.** Neither tree contains an A2A symbol of any kind. A2A
could perfectly well have a C ABI — it does not have one *yet*, which is exactly
why this is `not exposed` and not `n/a`.

**Go blobs are `partial`.** `MeshBlobAdapter` covers `Store` / `Fetch` /
`Exists` and the overflow controls, which is enough to put bytes in and get them
back. What is missing is the discovery-driven path — no equivalent of
`fetch_blob_discovered`, so Go cannot fetch a blob it has only a reference to
without knowing who holds it.

**Two rows are called "channels" and they are different mechanisms.**
*Tagged EventBus topics* (`node.channel("name")`) tag a locally-ingested event
with `_channel` and filter on it — no roster, no registration, no
authorization, no delivery to another process on its own. `n/a` in Rust, Go and
C because those bindings deliberately discriminate on the consumer instead;
this is a convenience layer, not a missing feature. *Distributed mesh channels*
are the real thing and exist in all five. Do not read a claim about one as
evidence about the other — `concepts.md` § Channel opens with the split.

**Python distributed channels are `core-only`.** `register_channel`,
`subscribe_channel` and `publish_channel` live on the low-level `net.NetMesh` /
`AsyncNetMesh` binding. The ergonomic `net_sdk.MeshNode` does not wrap them, so
a Python program that only imports `net_sdk` cannot reach the channel surface
at all. Same for `permissive_channels`, which is a constructor argument on the
low-level binding.

Capability announce and discovery **are** on the wrapper —
`announce_capabilities`, `find_nodes` / `find_nodes_scoped`, `find_best_node` /
`find_best_node_scoped` — so those rows are no longer `core-only`. They used to
require reaching through the private `node._native`, which the published Python
guides documented as the supported route. The **tool** surface (`list_tools`,
`watch_tools`, `serve_tool`) still takes the native handle.

**Delegated channel credentials are `not exposed` everywhere.** Core has both
halves — `MeshNode::subscribe_channel_with_chain(TokenChain)` and
`MeshNode::set_publish_chain` — and no public SDK or binding reaches either.
Every published surface accepts exactly one `PermissionToken`. So an
owner → delegator → subscriber chain cannot subscribe, and a delegated
publisher cannot install its chain, through anything a caller can import. Mint
a token directly to the subscriber instead, or drive the core `MeshNode` from
Rust. This is `not exposed`, not `n/a`: the core methods exist and want
wrapping.

**Prefix ACLs, origin binding, and queue-group policy are Rust-only or
core-only.** `Mesh::register_channel_prefix` is on the Rust SDK alone. The
`ChannelConfig` builders `with_subscriber_origin_binding` and
`with_queue_group_policy` are reachable from Rust because the SDK re-exports
the core type; the Python, Node, C and Go registration DTOs simply omit those
fields, so a non-Rust operator cannot express the policy. Queue-group
*subscription* (`subscribe_channel_in_queue_group_with_token`) is on the core
`MeshNode` only, not even on the Rust SDK. The nRPC defaults that use these
internally stay protected either way — what you cannot do from a binding is
install custom policy of your own.

**Distributed batch fan-out is Rust-only.** `Mesh::publish_many` sends the
payload slice as one batch per subscriber and returns a single `PublishReport`.
The other bindings publish one payload at a time; looping single publishes
changes batching, per-call overhead and report granularity, so it is not a
drop-in substitute. Note the ergonomic `publishBatch` / `publish_batch` on
tagged topics is *local ingestion*, not this.

**Rejection reasons are `partial` outside Rust.** The wire protocol
distinguishes `Unauthorized`, `UnknownChannel`, `RateLimited` and
`TooManyChannels`; Rust preserves it in `SdkError::ChannelRejected(Option<AckReason>)`.
Node and Python collapse it to authorization-versus-generic, and C maps
everything except unauthorized to `NET_ERR_CHANNEL`, which is why Go has only
`ErrChannelAuth` and `ErrChannel`. A caller outside Rust therefore cannot tell
"retry later" (rate-limited) from "fix your config" (unknown channel) from
"raise a limit" (too many channels).

**Channel-registry strictness is not symmetric, and the opt-out is test-shaped.**
Raw core `MeshNode` is permissive when no registry is installed; the Rust SDK,
C and Go install a strict empty registry; Python and Node are strict by default
and expose `permissive_channels` / `permissiveChannels` to opt out. That flag
exists for the dynamic channel names the enrollment nRPC path needs — treat it
as a test and bootstrap affordance, not as ordinary channel behaviour, and
prefer registering the channels you actually use.

**Go and C subnet gateway provisioning is `supported`.** Every *runtime*
administration verb is present — installing gateway credential sets, declaring
boundaries, applying signed control facts — and both bindings also declare the
node's subnet **trust anchors**.

`subnet_authorities`, `subnet_attachment`, `subnet_control_channel`, and
`subnet_exports` are construction-time state, supplied on Go's `MeshConfig` and
in the JSON C already passes to `net_mesh_new`. The conversion lives in the core
(`net::adapter::net::subnet::provision`) so every constructor reaches it,
including base `libnet`'s, with `net_sdk::subnet` re-exporting it so there is
still one definition. A standalone Go or C program can stand up a subnet
gateway on its own.

## Same operation, different shape

All `supported`, and still not interchangeable. Two worth knowing before you
generate code:

**Discovery has two shapes everywhere, and only the spelling differs.** Every
binding offers both the list (`find_nodes` / `findNodes` / `FindNodes` /
`net_mesh_find_nodes`, plus scoped variants) and the single winner
(`find_best_node` / `findBestNode` / `FindBestNode` /
`net_mesh_find_best_node`). The list leaves the choice to you; the
single-winner form applies the requirement's weights and returns one node.

How "no match" comes back is what differs. Rust returns `Option<u64>`, Node
`bigint | null`, Python `int | None`; Go returns `(uint64, bool, error)` and C
writes an `out_has_match` flag, because neither can express absence in a `u64`
where `0` is a valid node id. Transliterating a Rust snippet gets you the right
method name in Node and Python, and a compile error in Go.

**nRPC is a free function in Rust and a class everywhere else.** Rust has
`call_typed` on the mesh; Node, Python and Go route through a `TypedMeshRpc`
handle you construct first (`NewTypedMeshRpc` in Go), and C goes through
`net_rpc_call`, in the same `libnet` as everything else.

## What CI proves here, precisely

Every anchor above resolves in its binding's tree, and the status vocabulary is
closed. That is all.

It does **not** prove `supported`. And it deliberately does not infer absence,
because a missing symbol is weak evidence: a binding may alias, project under
another name, expose dynamically, surface through generated declarations, or
carry a low-level FFI symbol the ergonomic SDK withholds. Every one of those
came up while this matrix was being written — `call_typed` in Rust is `call` on
`TypedMeshRpc` everywhere else, Go's blob fetch is `MeshBlobAdapter.Fetch`, and
Go's RedEX lives in `cortex.go` with no `redex.go` at all. A rule that inferred
absence would have reported four bindings as broken.

So the matrix is editorially authoritative and the check guards its evidence. If
you change a cell, change its anchor.
