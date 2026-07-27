# Net

High-performance encrypted mesh runtime — the engine crate.

Most people should depend on a **SDK**, not on this crate directly:

```bash
cargo add net-mesh-sdk                       # Rust
npm install @net-mesh/sdk @net-mesh/core     # TypeScript / Node
pip install net-mesh-sdk                     # Python
go get github.com/ai-2070/net/go             # Go
```

**Docs: <https://ai2070.net/docs>** ·
[Concepts](https://ai2070.net/docs/concepts/architecture) ·
[API reference](https://docs.rs/net-mesh) ·
[Design philosophy and benchmarks](../../../README.md)

## Architecture

The protocol-level docs live alongside this crate, in [`docs/`](docs/). They
describe what the wire actually does — the level below the user-facing guides
on the docs site.

| Doc | Covers |
|---|---|
| [`TRANSPORT.md`](docs/TRANSPORT.md) | Wire format, the 64-byte header, encryption, packet pools, sessions, fair scheduling, multi-hop forwarding, routing, reliability, adaptive batching, failure detection, NAT traversal, swarm discovery |
| [`IDENTITY.md`](docs/IDENTITY.md) | Entity identity, origin binding, permission tokens and delegation |
| [`CHANNELS.md`](docs/CHANNELS.md) | Named hierarchical channels and capability-based authorization |
| [`BEHAVIOR.md`](docs/BEHAVIOR.md) | Capability announcements and indexing, diffs, node metadata, schema registry, autonomy rules, context fabric, load balancing, proximity graph, safety envelopes |
| [`SUBNETS.md`](docs/SUBNETS.md) | The four-level subnet hierarchy and gateway visibility |
| [`SUBPROTOCOLS.md`](docs/SUBPROTOCOLS.md) | The subprotocol registry, ID space, version negotiation, opaque forwarding |
| [`STORAGE_AND_CORTEX.md`](docs/STORAGE_AND_CORTEX.md) | RedEX logs, CortEX folds, NetDB, durability |
| [`COMPUTE.md`](docs/COMPUTE.md) | Daemons, capability-based placement, six-phase migration |
| [`STATE.md`](docs/STATE.md) | Distributed state |
| [`SENSING.md`](docs/SENSING.md) | Capability sensing and interest coalescing |
| [`CONTINUITY.md`](docs/CONTINUITY.md) | Observational continuity |
| [`CONTESTED.md`](docs/CONTESTED.md) | Contested environments |
| [`CONFIG_REPLICATION.md`](docs/CONFIG_REPLICATION.md) | RedEX replication configuration |
| [`CAPABILITIES_SCHEMA.md`](docs/CAPABILITIES_SCHEMA.md) | The canonical capability axis schema — CI fails on drift |
| [`AGENT_TOOLS.md`](docs/AGENT_TOOLS.md) | AI tool calling |
| [`cli/`](docs/cli/) | `net transfer`, `net-mesh typegen` |

Design plans, code reviews and audits are in
[`docs/internal/`](../../../docs/internal/) at the repo root — working history,
not documentation.

## Features

No features are enabled by default. Opt in explicitly.

| Feature | Flag | Dependencies |
|---|---|---|
| Redis Streams | `redis` | `redis` |
| NATS JetStream | `jetstream` | `async-nats` |
| Net transport | `net` | `chacha20poly1305`, `snow`, `blake2`, `dashmap`, `socket2`, `ed25519-dalek` |
| NAT traversal (classifier + rendezvous + `connect_direct`) | `nat-traversal` | `net` |
| Port mapping (NAT-PMP inlined + UPnP-IGD) | `port-mapping` | `nat-traversal`, `igd-next` |
| Regex filters | `regex` | `regex` |
| C FFI | `ffi` | — |
| RedEX (local append-only log) | `redex` | `net`, `tokio-stream`, `postcard` |
| RedEX disk durability | `redex-disk` | `redex` |
| CortEX (adapter core + tasks + memories) | `cortex` | `redex` |
| NetDB (unified query façade) | `netdb` | `cortex` |
| Dataforts (greedy + gravity + blob + RYW) | `dataforts` | `cortex`, `blake3`, `xxhash-rust` |
| MeshDB (federated query AST + planner + executor) | `meshdb` | `cortex` |
| MeshOS (cluster-behavior engine + behavior snapshot) | `meshos` | `cortex` |

## Building

```bash
cargo build --release                      # core only, no adapters
cargo build --release --features net       # transport only (~2 MB)
cargo build --release --features redis     # + Redis adapter
cargo build --release --all-features       # everything
```

## Tests

```bash
cargo test --all-features --lib            # unit tests
```

Integration suites are per-feature — `--test three_node_integration --features net`,
`--test integration_redex --features "redex redex-disk"`,
`--test integration_netdb --features netdb`, and so on. See
[`tests/`](tests/) for the full set, and `.github/workflows/ci.yml` for the
matrix CI actually runs.

## Benchmarks

```bash
cargo bench --all-features
```

See [`BENCHMARKS.md`](BENCHMARKS.md) for published numbers and methodology.

## Building with Claude Code

Net looks like Kafka or NATS from the outside, and the model underneath is
different enough that an agent working from surface familiarity will write
integration code that runs and is quietly wrong. Install the skills first:

```bash
git clone https://github.com/ai-2070/net-claude-skill.git /tmp/net-claude-skill
mkdir -p ~/.claude/skills
cp -R /tmp/net-claude-skill/net-event-bus /tmp/net-claude-skill/net-payments ~/.claude/skills/
```

Restart Claude Code and run `/skills` — **net-event-bus** and **net-payments**
should be listed. Full install options — project-scoped, symlinked to stay
current — in [Claude Skills](https://ai2070.net/docs/start/claude-skills).

## Links

[Docs](https://ai2070.net/docs) ·
[Concepts](https://ai2070.net/docs/concepts/architecture) ·
[API reference](https://docs.rs/net-mesh) ·
[GitHub](https://github.com/ai-2070/net)

## License

MIT OR Apache-2.0

### Third-party license notice

The default `net` feature links [`ring`](https://crates.io/crates/ring) for the
packet-path ChaCha20-Poly1305 AEAD. *ring* is distributed under an ISC-style
license together with notices for portions derived from BoringSSL and
OpenSSL/SSLeay; see the
[ring LICENSE](https://github.com/briansmith/ring/blob/main/LICENSE) for the
full text. Binary distributions that bundle the compiled library (the published
Python wheels, npm prebuilds, Go FFI static libraries, and release binaries)
include *ring*'s object code and should retain that notice alongside their own
license files.
