## Net CLI — implementation plan

> Single `net` command-line tool that wraps the Rust SDK for one-shot operator commands, CI scripting, daemon authoring, and ad-hoc cluster inspection. The non-interactive counterpart to Deck-the-TUI: same substrate surfaces, same operator-policy gates, scriptable shape. Builds on [`MESHOS_SDK_PLAN.md`](MESHOS_SDK_PLAN.md), [`DECK_SDK_PLAN.md`](DECK_SDK_PLAN.md), [`MESHDB_PLAN.md`](MESHDB_PLAN.md), and the existing `net-blob` CLI ([`DATAFORTS_BLOB_STORAGE_PLAN.md`](DATAFORTS_BLOB_STORAGE_PLAN.md)).

## Status

**Not started.** A `cli/` workspace member exists with an empty `src/` directory and the workspace `cli` feature flag is wired (`crates/net/Cargo.toml:default = []`, `cli = ["dataforts", "redex-disk", "dep:clap"]`). One ad-hoc binary lives today at `src/bin/net-blob.rs` for the blob-storage CLI surface; it stays as-is until the unified CLI absorbs it in Phase 4.

**Activation gate.** The Rust SDK + four-language MeshOS / Deck SDK bindings are feature-complete as of the v0.17 cycle. The CLI sits on top of `ai2070-net-sdk` (no new substrate surface required). A real consumer workflow — a CI bot, a ChatOps script, an SRE runbook — that needs a non-TUI surface drives Phase 1 activation.

**Substrate prereqs** (all in code today):

- **Rust SDK** at `crates/net/sdk/` — `MeshOsDaemonSdk`, `DeckClient` + `AdminCommands` / `IceCommands` / `AuditQuery`, `MeshOsSnapshot`, `OperatorIdentity` / `OperatorRegistry` / `AdminVerifier`, `OperatorIdentity::sign_proposal` / `sign_payload`, log / failure / audit streams.
- **MeshDB SDK** — `MeshQuery` AST + sync `MeshQueryRunner` for ad-hoc `net db` queries.
- **Identity layer** at `src/adapter/net/identity/` — `EntityKeypair::from_bytes` / `generate`, file-store conventions used by Deck-the-binary's startup.
- **Existing `net-blob`** at `src/bin/net-blob.rs` + `tests/net_blob_cli.rs` — the model for an `argv → SDK call → JSON output` pipeline; Phase 4 folds its commands into the unified surface.

## Frame

Deck-the-binary is the operator's interactive console — modeful, terminal-rendered, optimized for situational awareness during incidents. It's a poor fit for:

- **CI / automation** — a step in a release pipeline that wants to commit `enter_maintenance` on a target node, wait for the snapshot to reflect `EnteringMaintenance`, and exit with a status code.
- **Shell pipelines** — `net audit recent --by-operator 0xABCD --since 1h --json | jq '.[] | select(.event_kind == "force_cutover")'`.
- **One-shot daemon authoring** — `net daemon run --name echo --seed-file ~/.net/echo.seed` to spin a Rust daemon implementation against a running MeshOS supervisor without writing a `main()`.
- **Cluster inspection from a bastion** — `net snapshot get --output yaml`, `net db latest --chain 0x… --decode`.

The CLI is **the same SDK surface as Deck and the four-language bindings, exposed as a flat argv shape with JSON / YAML / table output and a stable exit-code contract**. Every subcommand maps 1:1 onto an SDK call; nothing in the CLI bypasses the SDK or invents new substrate semantics.

## Why this exists

Five reasons for a written plan rather than "we'll wire clap into the cli crate when someone asks":

1. **The non-goals are load-bearing.** The CLI must refuse to expose anything the SDK already refuses (no direct chain mutation, no scheduler control, no daemon-registry surgery, no admin-event bypass). Calling these out up front keeps a future contributor from accidentally adding a `net debug raw-commit` escape hatch.

2. **Subcommand layout is a contract.** Once `net admin drain` exists, any rename breaks every CI script. The plan locks the top-level subcommand surface (`net daemon` / `net admin` / `net ice` / `net audit` / `net snapshot` / `net log` / `net db` / `net identity` / `net blob`) and the flag conventions (`--node`, `--operator`, `--output`, `--format`) so the matrix is stable across phases.

3. **Output format matters for pipeable scripts.** Every read subcommand supports `--output (json|yaml|table)`; every write subcommand returns a `ChainCommit` JSON object on stdout (or a typed error envelope). The plan locks both — drift between subcommands ("most return JSON but `net audit` returns table by default") burns consumers.

4. **Exit codes are the script contract.** `0 = success`, `1 = generic error`, `2 = invalid argument / unknown subcommand`, `3 = SDK error (kind discriminator in stderr)`, `4 = ICE simulation blocked the commit`, `5 = operator policy / signature verification rejected the commit`, `10+ = subcommand-specific`. The plan defines the full table and pins it so consumers can `case $? in 3) ...; 4) ...` reliably.

5. **CLI is the operator-authoring on-ramp.** A Rust daemon developer who wants to test against a real supervisor today has to write `MeshOsDaemonSdk::start` boilerplate by hand. `net daemon run --kind <factory>` should accept a factory the user registered via a registration crate (`net-daemon-factories`) and drive the lifecycle, so the developer can iterate on `Daemon` impls without owning the runtime.

## What ships

A single `net` binary, layered over `ai2070-net-sdk`. Subcommand layout:

```
net <subcommand> [<flags>] [<positional>]

Subcommands:
  daemon    Author + run + observe a MeshOS daemon.
              run        Spawn a registered daemon implementation against a
                         supervisor (in-process or remote-attached).
              ls         List registered daemons (local supervisor only).
              shutdown   Drive a graceful shutdown of a daemon by id.
              log        Tail a daemon's substrate log stream.

  admin     Signed admin-chain commits (Deck `AdminCommands` × 9).
              drain                <NODE>
              enter-maintenance    <NODE> [--drain-for <DUR>]
              exit-maintenance     <NODE>
              cordon               <NODE>
              uncordon             <NODE>
              drop-replicas        <NODE> <CHAIN>...
              invalidate-placement <NODE>
              restart-all-daemons  <NODE>
              clear-avoid-list     <NODE>

  ice       Break-glass operator surface (Deck `IceCommands` × 7).
              freeze-cluster       --ttl <DUR>
              thaw-cluster
              flush-avoid-lists    --scope (global|local:<NODE>|on-peer:<NODE>)
              force-evict-replica  <CHAIN> <VICTIM>
              force-restart-daemon <DAEMON-ID> <NAME>
              force-cutover        <CHAIN> <TARGET>
              kill-migration       <MIGRATION-ID>
              (every command runs simulate → preview → confirm → commit)

  audit     Read-only operator-audit queries (Deck `AuditQuery`).
              recent       [-n N] [--since <DUR>] [--by-operator <ID>]
                           [--force-only]
              stream       (same filters; emits a JSON line per record)

  snapshot  `MeshOsSnapshot` reads.
              get        [--output (json|yaml)]
              watch      [--interval <DUR>] [--output ndjson]
              status     (typed summary — peers / daemons / freeze state)

  log       Substrate log stream (`subscribe_logs`).
              tail       [-f] [--min-level <LVL>] [--daemon <ID>]
                         [--node <ID>] [--output (text|json)]

  failures  Substrate failure stream (`subscribe_failures`).
              tail       [--since-seq <N>] [--output (text|json)]

  db        MeshDB queries (federated query plane).
              run        --query <Q> | --query-file <PATH>
              latest     --chain <CHAIN>
              between    --chain <CHAIN> --start <T1> --end <T2>
              tail       --chain <CHAIN> [--from <SEQ>]
              filter     --chain <CHAIN> --where <EXPR>
              aggregate  --chain <CHAIN> --kind (sum|count|min|max|avg)
                         --field <FIELD> [--window <DUR>] [--group-by <KEY>]
              plan       --query <Q> | --query-file <PATH>
                         (print the ExecutionPlan without executing)

  netdb     NetDB local KV adapters (Cortex-backed tasks + memories).
              tasks ls                                  [--filter <EXPR>]
              tasks create   --title <T> [--note <N>]
              tasks complete <ID>
              tasks rename   <ID> --title <T>
              tasks delete   <ID>
              memories ls                               [--filter <EXPR>]
              memories store --content <C> [--tag <TAG>...]
              memories retag <ID> --tag <TAG>...
              memories pin   <ID>
              memories unpin <ID>
              memories delete <ID>
              snapshot --out <PATH>                    # export the full store
              restore  --from <PATH>                   # import an exported store

  rpc       Typed RPC (nRPC) client surface.
              call       <SERVICE> <METHOD>
                         [--node <ID>] [--payload <JSON>|--payload-file <PATH>]
                         [--routing (latency|round-robin|sticky)]
                         [--timeout <DUR>]
              stream     <SERVICE> <METHOD>
                         [--payload <JSON>|--payload-file <PATH>]
                         (emits one ndjson line per chunk; Ctrl-C cancels)
              discover   <SERVICE>           # nodes advertising `nrpc:<SERVICE>`
              services                        # every nRPC service in the index

  cap       Capability advertisement + discovery.
              announce   --tags <TAG>... | --from-file <PATH>
                         (replaces the local node's advertised `CapabilitySet`)
              show       [--node <ID>]                # default = local node
              query      --tags <TAG>... [--require-gpu] [--min-memory-gb <N>]
                         [--min-vram-gb <N>] [--model <ID>] [--tool <ID>]
                         (compiles to a `CapabilityFilter`; returns matching node ids)
              nodes                                    # every (node, caps) tuple
                                                       # known to the local index

  peer      Peer + NAT-traversal helpers.
              ls                              # every peer with rtt / health /
                                              # nat-class / reflex from the
                                              # proximity graph
              reflex     [--node <ID>]                # default = local reflex_addr
              nat                                     # local node's nat_type
              reclassify-nat                          # force a classifier sweep
              set-reflex    <ADDR>                    # install a reflex override
              clear-reflex                            # drop the override

  port      Port-mapping + reachability helpers.
              gateway                                 # detected IPv4 gateway +
                                                      # local interface ip
              probe-peer    <NODE>                    # active reflex probe via
                                                      # the coordinator-mediated
                                                      # path; returns the source
                                                      # address observed
              try-map       --internal-port <P>
                            [--protocol (nat-pmp|upnp|auto)]
                            [--ttl <DUR>]
                            [--keep]                  # leave the mapping installed
                                                      # (default: revoke on exit)

  identity  Operator + entity identity authoring.
              generate   [--out <PATH>]
              show       <PATH>
              fingerprint <PATH>
              registry add    <REGISTRY> <OP-ID> <PUBKEY-HEX>
              registry remove <REGISTRY> <OP-ID>
              registry list   <REGISTRY>

  blob      Dataforts blob CLI (existing `net-blob` absorbed in Phase 4).
              put     <PATH> [--chain <CHAIN>]
              get     <REF> [--out <PATH>]
              ls      [--chain <CHAIN>]
              rm      <REF>

  version   Print the SDK version + git revision the binary was built against.
  help      Built-in clap help (every subcommand has `-h` / `--help`).
```

### Global flags

Apply to every subcommand unless explicitly suppressed.

| Flag | Default | Purpose |
|---|---|---|
| `--config <PATH>` | `$XDG_CONFIG_HOME/net/config.toml` | Profile file with the substrate connection target + identity path. |
| `--profile <NAME>` | `default` | Named connection profile within the config file. |
| `--node <ID>` | (none) | Target node id for `admin`/`ice` subcommands. |
| `--output <FMT>` | `json` (machine-friendly) | One of `json` / `yaml` / `ndjson` / `table` / `text`. |
| `--quiet` / `-q` | off | Suppress progress diagnostics on stderr. |
| `--verbose` / `-v` | off | Tracing-subscriber `info` (`-vv` = debug, `-vvv` = trace). |
| `--no-color` | follows `$NO_COLOR` | Disable ANSI colour in table / text output. |
| `--timeout <DUR>` | 30s | Global per-call timeout. |
| `--dry-run` | off | For `admin` / `ice`: build the envelope, print it, refuse to commit. |

### Output format contracts

- **`json`** — single value on stdout, terminated by newline. Default for `admin` / `ice` / `db` / one-shot reads.
- **`ndjson`** — one JSON object per line. Default for streaming reads (`audit stream`, `snapshot watch`, `log tail`).
- **`yaml`** — for `snapshot get` and `identity show` where the structure is large + human-readable matters.
- **`table`** — ASCII / unicode bordered table. Default for `admin` / `audit recent` / `daemon ls` when stdout is a TTY.
- **`text`** — plain lines. Default for `log tail` when stdout is a TTY; `--output json` switches to ndjson.

Auto-detection rule: when `--output` is not specified, the binary picks `table` / `text` for TTY stdout and `json` / `ndjson` for non-TTY stdout. `--output` overrides.

### Exit codes (locked)

| Code | Meaning | Notes |
|---|---|---|
| 0 | Success. | Includes the dry-run path printing the envelope and exiting. |
| 1 | Generic runtime error. | Filesystem / IO / unexpected panic caught by the top-level handler. |
| 2 | Invalid arguments. | Clap's default code; passed through. |
| 3 | SDK error (substrate-side rejection). | Kind discriminator + message printed to stderr. |
| 4 | ICE simulation blocked. | `simulate_ice_proposal` rejected the proposal before commit. |
| 5 | Operator policy / signature verification rejected. | Maps every `VerifyError` variant onto this code; full kind + message on stderr. |
| 6 | Connection / handshake failure. | `start` couldn't reach the substrate node. |
| 7 | Timeout. | `--timeout` elapsed before the SDK call resolved. |
| 8 | Confirmation refused. | TTY user said "no" at the ICE preview prompt. |
| 10–16 | Subcommand-specific. | `net daemon` uses 10 for "factory not found", `net db` uses 11 for "query parse failed", etc. |
| 17 | Identity file not found. | `--identity <PATH>` points at a non-existent path. Pre-flight check before SDK init. |
| 18 | Identity unreadable. | File exists but permission denied, points at a directory, or otherwise can't be opened. Pre-flight. |
| 19 | Identity malformed. | File is readable but isn't a valid identity TOML / seed can't be parsed. Pre-flight. |
| 20+ | Reserved for future subcommand-specific codes. | |

## Design

### 1. Layout

The unified CLI lives at `crates/net/cli/` (the existing empty workspace member). Wires:

```
crates/net/cli/
├── Cargo.toml         # bin = "net", path = "src/main.rs"
├── src/
│   ├── main.rs        # clap entrypoint + global flag plumbing.
│   ├── commands/
│   │   ├── daemon.rs
│   │   ├── admin.rs
│   │   ├── ice.rs
│   │   ├── audit.rs
│   │   ├── snapshot.rs
│   │   ├── log.rs
│   │   ├── failures.rs
│   │   ├── db.rs
│   │   ├── identity.rs
│   │   ├── blob.rs    # Absorbs `src/bin/net-blob.rs`.
│   │   └── version.rs
│   ├── config.rs      # Profile file parsing, env var fallback.
│   ├── output.rs      # `--output` dispatch: json / yaml / ndjson / table / text.
│   ├── confirm.rs     # ICE preview + interactive y/N gate.
│   ├── error.rs       # Exit-code mapping + stderr formatting.
│   └── prelude.rs     # Re-exports the SDK types every command imports.
└── tests/
    ├── help.rs        # `--help` output stable across subcommands.
    ├── admin.rs       # In-process supervisor + CLI invocation via `assert_cmd`.
    ├── ice.rs         # Simulate-then-commit + dry-run + confirm-refused.
    ├── audit.rs       # JSON + ndjson + table output round-trip.
    └── exit_codes.rs  # Every documented code is reachable via a controlled fixture.
```

`crates/net/cli/Cargo.toml`:
```toml
[package]
name = "net-cli"
version = "0.17.0"
publish = false

[[bin]]
name = "net"
path = "src/main.rs"

[dependencies]
ai2070-net-sdk = { path = "../sdk", features = ["meshos", "deck", "meshdb", "dataforts"] }
clap = { version = "4", features = ["derive", "env"] }
tokio = { version = "1", features = ["macros", "rt-multi-thread", "sync", "time", "signal"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
serde_yaml = "0.9"
tracing-subscriber = { version = "0.3", features = ["env-filter", "fmt"] }
color-eyre = "0.6"
comfy-table = "7"             # Pretty terminal tables.
dialoguer = "0.11"            # Interactive confirm prompts.
toml = "0.8"                  # Config file parsing.
dirs = "5"                    # XDG paths.

[dev-dependencies]
assert_cmd = "2"              # spawn the binary in tests.
predicates = "3"
```

### 2. Routing model

`main.rs` parses the global flags, builds a `CliContext` (config + connection + identity + tracing), and dispatches the subcommand. Each command module exposes a `run(ctx: &CliContext, args: <Args>) -> ExitCode` function; no command opens its own runtime or wires its own identity loading.

The `CliContext` lifecycle:

1. Parse global flags + config.
2. Lazy-initialize the tokio runtime (`#[tokio::main(flavor = "multi_thread")]` on `main`).
3. Lazy-load the operator identity from the resolved path.
4. Lazy-attach to the substrate node (in-process for tests via `--in-process`, remote attach via `--endpoint` once the substrate ships a remote-attach surface — Phase 5 deferred).

Most commands need only steps 1–3. `admin` / `ice` / `daemon run` need step 4.

### 3. ICE preview workflow

`net ice <action>` always runs simulate → preview → confirm → commit, mirroring Deck-the-binary's UI gate:

1. Build the `IceProposal` via the SDK.
2. Call `simulate()` → get `BlastRadius`.
3. Render the blast radius as a table (TTY) or JSON (non-TTY).
4. **TTY**: prompt `"Continue? (type 'YES' to confirm)"`. Anything other than literal `YES` exits with code 8.
   **Non-TTY**: require `--yes` flag; absence is exit code 8.
5. `--dry-run` short-circuits before step 4 and exits 0 with the envelope on stdout.
6. Commit; emit the resulting `ChainCommit` on stdout.

For `--operator-key <PATH>` flows (multi-operator signing): the CLI loads the local operator's key, signs the simulated proposal via `OperatorIdentity::sign_proposal`, and prints the signature as a JSON dict. The `net ice <action> --sig <JSON>` form accepts pre-collected signatures from co-operators.

### 4. Daemon authoring on-ramp

Three modes for `net daemon run`:

- **Default** — `--kind <FACTORY-ID>` resolves a factory registered in a downstream crate via the `net_daemon_factories::register!` macro. The macro itself is a Phase 4 invention — **Phase 4 begins with designing and implementing it** (factory ID → constructor binding, scope contract, missing-factory error story); the CLI iterates registered factories at startup. Pinning the registration shape now keeps the daemon on-ramp consistent and prevents ad-hoc per-binary registration logic.
- **Embedded** — `--exec <BINARY>` spawns a subprocess that talks to the CLI via stdio + a tiny line protocol. Lets non-Rust daemons piggyback on the CLI's lifecycle wiring. Deferred to Phase 4.
- **Probe** — `--probe` runs a no-op daemon and just reports lifecycle events on stdout. Useful for testing the supervisor connection without a real factory.

### 5. nRPC client surface

`net rpc` is the client-side wrapper over the SDK's typed-RPC layer (`crates/net/sdk/src/mesh_rpc.rs`). Server-side hosting goes through `net daemon run --kind <FACTORY-ID>` so handlers live with the daemon-authoring on-ramp rather than a separate subcommand.

- **`net rpc call <SERVICE> <METHOD>`** — wraps `MeshAdapter::call_service(service, payload, opts)` (or `call(node, …)` when `--node` is set). Payload arrives as JSON (via `--payload <JSON>` or `--payload-file <PATH>`), gets re-encoded with the codec the service declares, and the reply body is decoded back to JSON on stdout. `--routing` maps to `CallOptions::routing_policy` (`latency` / `round-robin` / `sticky`). `--timeout` overrides `CallOptions::timeout`. Errors map onto exit code 3 with the `RpcError` kind discriminator on stderr (`NoRoute` / `Timeout` / `RemoteError` / etc.).
- **`net rpc stream <SERVICE> <METHOD>`** — wraps `call_streaming(...)`. Each chunk decodes to a JSON object emitted as one ndjson line; `Ctrl-C` propagates as `RpcStream::cancel()` so the server-side sink terminates cleanly.
- **`net rpc discover <SERVICE>`** — wraps `find_service_nodes(service)`. Output table (TTY) or ndjson (non-TTY) listing the advertising node ids.
- **`net rpc services`** — enumerate every `nrpc:<service>` tag in the local capability index. Useful for discovery from a bastion ("what's actually wired up?").

### 6. Capability + proximity surface

`net cap` and `net peer` wrap the substrate's capability-system and proximity-graph reads + writes (`adapter/net/behavior/capability.rs`, `adapter/net/behavior/proximity.rs`, plus the `MeshAdapter` accessors). Both are local-node operations — they read or modify what this node advertises / observes about its mesh neighbors.

**`net cap` — capability advertisement + discovery:**

- **`announce`** — wraps `MeshAdapter::announce_capabilities(set)`. `--tags <TAG>...` accepts a list of free-form tag strings the substrate folds into a `CapabilitySet` via `add_tag` chaining (same shape every binding's `requiredCapabilities` route accepts). `--from-file <PATH>` reads a TOML / JSON file with the full structured shape (GPU vendor + VRAM, model declarations, accelerator entries) for richer adverts. Replaces the local node's advertised set; partial updates use the existing `announce_capabilities_with(...)` builder under the hood (`--add-tag` / `--remove-tag` follow-up flags are an explicit deferred slice).
- **`show`** — local case: print the substrate's last-announced set for this node. `--node <ID>`: read from `ProximityGraph::nodes_with_capabilities()` and emit the matching peer's advertised set. Output `json` / `yaml` / `table` per the global `--output` rule.
- **`query`** — compiles the flag set into a `CapabilityFilter` (the substrate type at `behavior/capability.rs:~2134`) and calls `CapabilityIndex::query(filter)` via `MeshAdapter::capability_index().query(...)`. Returns the matching node id list. Useful for sanity-checking placement decisions ("which nodes would actually pass this filter today?"). Flags map 1:1: `--require-gpu` → `CapabilityFilter::require_gpu`, `--min-memory-gb` → `min_memory_gb`, `--model <ID>` → `require_models.push(ID)`, etc.
- **`nodes`** — wraps `ProximityGraph::nodes_with_capabilities()`. Emits `(node_id, CapabilitySet)` tuples — the "what does the whole mesh look like, capability-wise" snapshot. Table form is one node per row with tag count + a tag-summary column; ndjson form emits the full set per node.

**`net peer` — peer + NAT-traversal helpers:**

- **`ls`** — wraps `ProximityGraph::all_nodes()`. One row per peer: `node_id`, `rtt_ms`, `health` (Healthy / Degraded / Unreachable / Unknown), `nat_class`, `reflex_addr`, `hops`. Output `table` (TTY) or `ndjson` (non-TTY).
- **`reflex`** — wraps `MeshAdapter::peer_reflex_addr(node)`. With no `--node`, returns the local node's `reflex_addr()`. Exits 3 with kind `peer_unknown` if the peer has no observed reflex yet (probe + retry hint in stderr).
- **`nat`** — wraps `MeshAdapter::nat_type()` + `reflex_addr()` for the local node. Emits `{ nat_class, reflex_addr, source }` where `source` is `probe` / `override` / `none`.
- **`reclassify-nat`** — wraps `MeshAdapter::reclassify_nat()`. Awaits the next classifier sweep, then prints the post-sweep nat-class. Useful after a network move.
- **`set-reflex <ADDR>`** — wraps `set_reflex_override(addr)`. Pins `nat_class = "open"` and `reflex_addr = Some(addr)` until cleared. Documented as an optimization (not correctness — routed-handshake still works without it).
- **`clear-reflex`** — wraps `clear_reflex_override()`. The next probe sweep repopulates `reflex_addr` naturally.

Both subcommand groups are read-only from a cluster-state perspective with one local-state exception: `cap announce` and `peer set-reflex` mutate what *this node* tells the rest of the mesh, but never commit to the admin chain. They're "tell my peers about my own state" surfaces, not "tell other nodes what to do" surfaces — which keeps them firmly outside the admin-commit path that requires operator signing.

### 7. Port-mapping + reachability helpers

`net port` wraps the substrate's port-mapping stack (`adapter/net/traversal/portmap/`) and a thin set of reachability probes. Scoped tightly to what the mesh actually exposes — this is **not a generic TCP/UDP port scanner**. Operators reach for it when diagnosing "why isn't this node externally reachable" or "does my router support UPnP/NAT-PMP at all."

- **`net port gateway`** — wraps `default_ipv4_gateway()` + `local_ipv4_for_gateway(gw)`. Prints `{ gateway, local_iface_ip, source }` where `source` is `routing-table` (resolved from the OS) or `unavailable`. First step when port-mapping isn't working: is the gateway even detectable?
- **`net port probe-peer <NODE>`** — wraps `MeshAdapter::probe_reflex(peer)`. Sends a coordinator-mediated reflex probe to `peer` and prints the source address the coordinator observed for this node ("here's what the rest of the mesh sees you as"). Exits 3 with kind `transport` if the socket fails, `unavailable` if no coordinator session is up. Distinct from `net peer reflex <NODE>` — that's a cached lookup; this actively probes.
- **`net port try-map --internal-port <P>`** — ad-hoc port-map attempt against the local gateway. Picks NAT-PMP (`Protocol::NatPmp`) when `--protocol nat-pmp`, UPnP (`Protocol::UpnpIgd`) when `upnp`, both-in-order when `auto`. On success prints the `PortMapping` (`{ external, internal_port, ttl_ms, protocol }`); on failure prints the `PortMappingError` kind (`unavailable` / `timeout` / `transport` / `refused`) with exit code 3. **Default behaviour revokes the mapping on exit** — this is a "does my router even support this?" diagnostic, not a way to permanently install a mapping outside the running mesh's lifecycle. `--keep` overrides for the rare case an operator wants the mapping installed without spinning up a full node. Requires the `port-mapping` cargo feature in the CLI build.

Substrate-side prereq for `try-map`: the existing `PortMapperClient` impls (NAT-PMP / UPnP-IGD) are accessible through the traversal module today, but there's no public one-shot helper that constructs a client, runs `probe → install → remove`, and returns the `PortMapping`. The CLI needs a small `sdk-side` adapter (`net_sdk::traversal::try_map_once(protocol, internal_port, ttl)`) wrapping the existing impls; that's a half-day's work, no substrate-internal changes required.

**This adapter must land before Phase 2 starts.** Phase 2 is blocked on it — implementing `net port try-map` against a missing SDK helper would either fork the traversal logic into the CLI (inconsistent with locked decision 1: "no command bypasses the SDK") or block the rest of Phase 2 behind the diagnostic. The fix is to merge `try_map_once` into `net-sdk` as the first substrate-side patch of the Phase 2 cycle, before any CLI work begins.

### 8. MeshDB query surface

`net db` wraps the SDK's MeshDB federated query plane (`crates/net/sdk/src/meshdb.rs`). The existing `run` / `latest` / `between` / `tail` slice covers raw query execution; this section adds the three composer-shortcut subcommands that let operators write common shapes without authoring a full `MeshQuery` JSON document.

- **`net db run --query <Q>` / `--query-file <PATH>`** — parses a full `MeshQuery` JSON payload, hands it to `MeshQueryPlanner::plan(query)`, then to `LocalMeshQueryExecutor::execute(plan)`. Streams `ResultRow`s as ndjson on stdout. The shape of the JSON matches the `MeshQuery` serde repr — same envelope every Rust consumer constructs.
- **`net db latest --chain <CHAIN>`** — shorthand for `MeshQuery::latest(chain)`. Returns the freshest row from the chain.
- **`net db between --chain <CHAIN> --start <T1> --end <T2>`** — shorthand for `MeshQuery::between(chain, t1, t2)`. Bounded timeline scan.
- **`net db tail --chain <CHAIN> [--from <SEQ>]`** — `MeshQuery::latest(chain)` driven through `ResultStream` as ndjson; closes on `Ctrl-C` or chain end.
- **`net db filter --chain <CHAIN> --where <EXPR>`** — shorthand for `MeshQuery::latest(chain).filter(parse(expr))`. `<EXPR>` accepts a minimal CEL-style boolean grammar (see locked decision 12):

  ```
  EXPR  := TERM (("&&" | "||") TERM)*
  TERM  := KEY OP VALUE
  OP    := "==" | "!=" | "<" | "<=" | ">" | ">="
  KEY   := identifier
  VALUE := number | string | boolean
  ```

  Example: `--where "node_id == 3 && health != 'Unhealthy'"`. Implemented via `evalexpr` in Phase 2. No user-defined functions, no nested expressions, no `in` / `!` / set membership in v1 — those are deferred until a concrete use case forces the surface. Parser lives in the CLI; compiles to an `Expr` the substrate already accepts. A misformed expression exits with subcommand-specific code 12.
- **`net db aggregate --chain <CHAIN> --kind <KIND> --field <FIELD> [--window <DUR>] [--group-by <KEY>]`** — composes `MeshQuery::latest(chain).aggregate(...)` using `NumericAggregateKind` for `sum|count|min|max|avg`. `--window` wraps the aggregate in a `WindowSpec` (sliding by default); `--group-by` adds a `GroupKey`. Prints one row per group as ndjson.
- **`net db plan --query <Q> | --query-file <PATH>`** — runs `MeshQueryPlanner::plan(query)` but **does not** execute. Emits the `ExecutionPlan` as JSON — useful for understanding planner decisions (which chains get scanned, whether the cache layer engages, what the join watermark resolves to) before kicking off an expensive query.

Output defaults: `tail`, `between`, `aggregate`, `filter`, `run` emit ndjson; `latest` emits a single JSON object on stdout. `plan` emits a single JSON `ExecutionPlan` object.

### 9. NetDB local-store surface

`net netdb` wraps `NetDb` (`adapter/net/netdb`) — the Cortex-backed local KV store every daemon can use for tasks + memories. Audience is **daemon developers + agents debugging local state**, not cluster operators; operators don't directly touch tasks / memories in production. The subcommand group lives in the same binary because the SDK already wires NetDB and a separate `netdb-cli` would duplicate config + identity loading for marginal cleanliness.

Both `tasks` and `memories` are independent adapters with `with_tasks()` / `with_memories()` builder gates on the substrate; `net netdb` opens whichever the operator's `--store <PATH>` (or config-file `netdb.path`) requests. The XDG location `$XDG_DATA_HOME/net/netdb.redex` is a convention for bootstrapping, **not** an implicit default — see locked decision 11. Every invocation must name the store explicitly; the CLI never auto-discovers.

**Tasks** — wraps `TasksAdapter` (`cortex/tasks/adapter.rs`):

- **`tasks ls [--filter <EXPR>]`** — read `state().read().tasks()` and stream as ndjson. `--filter` uses the same predicate DSL as `net db filter` (compiled to a `TasksFilter`).
- **`tasks create --title <T> [--note <N>]`** — wraps `TasksAdapter::create(title, note)`. Prints the new `TaskId` + the WriteToken; exit code 3 on substrate-side failure (`CortexAdapterError` kinds).
- **`tasks complete <ID>`** — wraps `complete(id, now_ns)`.
- **`tasks rename <ID> --title <T>`** — wraps `rename(id, new_title)`.
- **`tasks delete <ID>`** — wraps `delete(id)`.

**Memories** — wraps `MemoriesAdapter` (`cortex/memories/adapter.rs`):

- **`memories ls [--filter <EXPR>]`** — read `state().read().memories()`. `--filter` compiles to `MemoriesFilter`.
- **`memories store --content <C> [--tag <TAG>...]`** — wraps `store(content, tags, now_ns)`. Prints new `MemoryId` + WriteToken.
- **`memories retag <ID> --tag <TAG>...`** — wraps `retag(id, tags)`.
- **`memories pin <ID>`** / **`memories unpin <ID>`** — wraps `pin` / `unpin`.
- **`memories delete <ID>`** — wraps `delete(id)`.

**Snapshot / restore** — wraps `NetDb::snapshot()` (returns `NetDbSnapshot`) and `NetDbBuilder::build_from_snapshot(...)`:

- **`snapshot --out <PATH>`** — serialize the live snapshot via `NetDbSnapshot::encode()` (postcard) to `<PATH>`. Useful for backup + cross-machine replication during daemon development.
- **`restore --from <PATH>`** — `NetDbSnapshot::decode(bytes)` then `NetDbBuilder::build_from_snapshot(snapshot)`. Fails fast if the target store is non-empty unless `--force` is set.

Every `net netdb` command requires `--store <PATH>` (or config-file equivalent) — no exceptions, no auto-discovery, even on the read-only `tasks ls` / `memories ls` / `snapshot` slice. This is the locked decision 11 surface: NetDB paths are deliberate resources, not magical globals. SDKs auto-configure internally; the CLI is explicit. Scripts that target NetDB must pin the store, which is exactly the safety contract operators need.

### 10. Config file shape

```toml
# ~/.config/net/config.toml

[default]
endpoint = "in-process"                       # or "tcp://<host>:<port>" once remote attach lands
identity = "~/.config/net/identities/op.toml"
log_level = "info"

[profiles.staging]
endpoint = "in-process"
identity = "~/.config/net/identities/staging-op.toml"
default_timeout_ms = 60000

[profiles.production]
endpoint = "in-process"
identity = "~/.config/net/identities/prod-op.toml"
ice_signature_threshold = 2                   # advisory; the substrate enforces
default_timeout_ms = 30000
```

`--config` / `--profile` / `NET_PROFILE` env var resolve the active profile. Every individual flag overrides the profile value.

### 11. Identity store

Operator identities are stored as TOML files:

```toml
# ~/.config/net/identities/op.toml
operator_id = "0x1234..."
seed_hex    = "..."                  # 64 hex chars (32-byte ed25519 seed)
created_at  = "2026-05-17T12:34:56Z"
note        = "Production operator for the deck-fleet cluster"
```

`net identity generate --out <PATH>` writes a new one. `net identity show <PATH>` prints the operator id + public key + creation date (never the seed). `net identity fingerprint <PATH>` prints a short SHA256-truncated identifier for inclusion in audit dashboards.

The seed file is read-only by `chmod 600`; the CLI errors with kind `permissive_mode` if the file is world-readable. Pattern lifted from `ssh-keygen`.

### 12. Wire / FFI contracts

The CLI inherits everything from the Rust SDK — no new wire formats. JSON output uses serde's default representation; specifically:

- `ChainCommit` →
  ```json
  {
    "commit_id": "0x1234...",
    "operator_id": "0x5678...",
    "event_kind": "enter_maintenance",
    "committed_at_ms": 1717084800000
  }
  ```
- `BlastRadius` → same shape every binding emits (the substrate's serde repr).
- `MeshOsSnapshot` → forwarded verbatim; `--output yaml` rewraps for readability.
- Errors → `{"kind": "...", "message": "..."}` on stderr, exit code per the table above.

### 13. Tests

Three layers:

1. **Help-text golden tests** — `assert_cmd::Command::cargo_bin("net").arg("--help")` plus every subcommand's `--help`. Goldens stored under `tests/golden/help/*.txt`; flag drift triggers a CI failure.
2. **In-process integration** — every subcommand under `tests/`. Each test spins a `net_sdk::testing::ClusterHarness`, drives the CLI via `assert_cmd`, and checks stdout / stderr / exit code. ~40 cases planned for Phase 1.
3. **Exit-code coverage** — `tests/exit_codes.rs` enumerates every documented code (0–8, sample 10–11), invokes a fixture that produces that code, and asserts the binary exits with it. Pinned so future contributors can't quietly broaden the meaning of an exit code.

### 14. Documentation

- **`docs/net-cli.md`** — operator-facing reference. Each subcommand's flags, output shape, example invocation, exit codes, env vars. Format lifted from `git-scm.com`-style man pages.
- **`docs/net-cli-cookbook.md`** — recipes. Common CI patterns (drain-and-wait, audit-since-deploy, ICE preview in dry-run), shell snippets, jq one-liners.
- **`--help` text** — `clap`'s built-in help is the canonical short form; the markdown docs link out for deeper context.
- **`man net`** — generated from clap via `clap_mangen` in CI; shipped under `docs/man/`.

## Locked decisions

Lock these so phase implementations don't relitigate:

1. **CLI is the same SDK surface as Deck and the four-language bindings.** Nothing in the CLI bypasses the SDK or invents new substrate semantics. If a command can't be expressed as an SDK call, it doesn't ship.

2. **Output is structured by default for non-TTY stdout, table for TTY.** Auto-detection follows `is-terminal::is_terminal(stdout)`. `--output` always wins. Goldened.

3. **Exit codes are a stable contract.** The table above is locked; broadening a code's meaning (e.g. reusing 4 for non-ICE failures) requires a major version bump.

4. **ICE writes go through simulate + confirm.** No `--yes` flag bypasses the simulate step. `--dry-run` skips the commit; `--yes` skips the interactive prompt only. Substrate-side `SimulationRequired` enforcement is the backstop. `--dry-run` and `--yes` compose: `net ice <action> --dry-run --yes` simulates, auto-confirms without prompting, prints the envelope, and exits 0. This matches `kubectl` / `terraform` conventions and is the canonical CI shape.

5. **Identity files are TOML with the seed inline.** Format pinned to the layout in §6. Re-using the SSH-style separate-pubkey-file split was considered and rejected; the CLI's audience is operators who already manage TOML config.

6. **Subcommand names are kebab-case** (`enter-maintenance`, `flush-avoid-lists`). Hyphen-vs-underscore drift is the #1 CLI papercut; pinning now keeps consumer scripts working across versions.

7. **No interactive REPL mode.** Deck is the interactive UI; the CLI is one-shot. A consumer who wants a REPL drives the SDK directly.

8. **No raw chain mutation.** Even with `--unsafe-i-know-what-im-doing` or any escape hatch — those don't exist. Every write rides the admin or ICE commit path.

9. **No daemon-registry surgery.** `net daemon ls` reads the snapshot; there's no `net daemon force-unregister`. Drain or force-restart through the admin / ICE paths instead.

10. **`net-blob` is absorbed in Phase 4.** Until then, both binaries ship; after, `net-blob` becomes an alias that prints a deprecation warning and forwards to `net blob`.

11. **NetDB paths are always explicit.** `net netdb` never auto-discovers a store. Every invocation must pass `--store <PATH>` (or set `netdb.path` in the config profile). The XDG location `$XDG_DATA_HOME/net/netdb.redex` is a bootstrapping convention, not an implicit global — auto-discovery would create the illusion of mutable global state, which is the wrong default for a distributed-OS CLI that operators script against. SDKs auto-configure; the CLI is explicit.

12. **The `--where` predicate is a minimal CEL-style subset.** Grammar locked at `EXPR := TERM (("&&" | "||") TERM)*`, `TERM := KEY OP VALUE`, `OP := "==" | "!=" | "<" | "<=" | ">" | ">="`. No user-defined functions, no nested expressions, no `in` / `!` / set membership in v1. Implemented via `evalexpr` in Phase 2; broader grammar (e.g. `in`, parentheses, negation) requires a new locked-decision entry and a minor version bump. This keeps help text, error messages, parser, and tests in lockstep.

13. **`--dry-run` and `--yes` compose.** See decision 4 above — restated here so consumers searching this section find the rule. They are not mutually exclusive; combined, the CLI simulates, auto-confirms without prompting, prints the envelope, and exits 0.

14. **Identity is loaded and validated before SDK construction.** `--identity <PATH>` triggers a pre-flight: file existence, readability, parse success. Failures get dedicated exit codes — 17 (not found), 18 (unreadable / permission denied / not a regular file), 19 (malformed). Collapsing all three onto the generic exit-3 SDK-error bucket was rejected as too blunt for scripted use. Structured JSON errors on stderr in non-TTY mode (`{"kind": "identity_not_found" | "identity_unreadable" | "identity_malformed", "path": "...", "message": "..."}`).

## Phases

Activation order, dependency-driven:

- **Phase 1 — Scaffolding + read-only surface.** `cli/Cargo.toml` + `src/main.rs` with clap routing for `net version`, `net identity (generate|show|fingerprint)`, `net snapshot get`, `net snapshot status`, `net audit recent` / `audit stream`, `net log tail`, `net failures tail`, `net daemon ls`, `net cap (show|query|nodes)`, `net peer (ls|reflex|nat)`, `net port gateway`, `net db (run|latest|between|tail|filter|aggregate|plan)`, `net netdb (tasks ls|memories ls|snapshot)`. Config + global flags + output dispatch. Exit-code table. Help-text goldens. Wires nothing that mutates substrate or local-node state — purely read + identity authoring.

- **Phase 2 — Admin write surface + nRPC client + local-state writes + port-map diag + NetDB mutations.** **Blocked on `net_sdk::traversal::try_map_once` landing first** (see §7); the SDK helper must merge before any Phase 2 CLI work begins so `net port try-map` rides the SDK rather than forking traversal logic into the CLI. Once that prereq is in: `net admin (drain|enter-maintenance|exit-maintenance|cordon|uncordon|drop-replicas|invalidate-placement|restart-all-daemons|clear-avoid-list)`. `--dry-run` support (composes with `--yes` per locked decision 4/13). Identity loading + commit envelope construction (pre-flight per locked decision 14; exit codes 17/18/19). `net rpc (call|stream|discover|services)` — Tier 1 client surface over `MeshAdapter::call_service` / `call_streaming` / `find_service_nodes`. `net cap announce` + `net peer (reclassify-nat|set-reflex|clear-reflex)` — local-state writes that update what this node advertises but never commit to the admin chain. `net port (probe-peer|try-map)` — wraps the now-landed `try_map_once` helper; requires the CLI build to enable the `port-mapping` cargo feature. `net netdb tasks (create|complete|rename|delete)` + `net netdb memories (store|retag|pin|unpin|delete)` + `net netdb restore` — local-store mutations on a developer's NetDB; `--force` gates `restore` over a non-empty store. `--where` predicate parser (`evalexpr` integration per locked decision 12). Integration tests for every admin variant + ≥3 per `rpc` / `cap` / `peer` / `port` / `db` / `netdb` command (happy path, error path, output-format variant).

- **Phase 3 — ICE break-glass surface.** `net ice (freeze-cluster|thaw-cluster|flush-avoid-lists|force-evict-replica|force-restart-daemon|force-cutover|kill-migration)`. Simulate → preview table → confirm gate → commit. `--yes` for non-interactive flows; `--sig` for pre-collected signatures. `net identity registry (add|remove|list)`. ICE-specific integration tests + the exit-code-8 coverage.

- **Phase 4 — Daemon run + blob absorption.** **Starts with designing and implementing the `net_daemon_factories::register!` macro** — factory ID → constructor binding, scope contract (process-wide registration via `inventory`-style linker hooks vs explicit per-binary opt-in), missing-factory error story (exit code 10 per the table), duplicate-factory handling. Once the macro exists: `net daemon run --kind <FACTORY-ID>` iterates the registered inventory. `net daemon shutdown`. `net daemon log`. `net blob` absorbs `net-blob`'s `put` / `get` / `ls` / `rm` surface. `net-blob` becomes a forwarding shim.

- **Phase 5 — Remote attach. INDEFINITELY DEFERRED.** `--endpoint mesh://<bootstrap-peer>:<port>?psk=<file>` for operator commands from a different machine. The substrate already has the wire layer (encrypted UDP + channel-auth + nRPC + capability-based service discovery); what's missing is the Deck nRPC service surface (`DeckService.{Snapshots, Status, AdminDrain, …, IceSimulate, IceCommit, AuditQuery, LogTail, FailureTail}`) plus server-side handlers each substrate node would register. Client wrapper would be a `RemoteDeckClient` that mirrors `DeckClient`'s method names but routes through `mesh.call_service(…)`. Real scope: ~2–3 days of substrate work, not a fundamental architectural change.

Status: indefinitely deferred — no consumer workflow drives this today. The strongest pull would be Deck-the-TUI rendering a real production cluster from an operator's laptop (currently it only sees its own in-process supervisor or the `--features demo` synthetic cluster); multi-operator ICE coordination is the second-strongest. Revive when one of those is concrete.

Phases 1–4 land independently of each other; Phase 5 stays parked until a consumer needs it.

### Indefinitely deferred

These surfaces are designed-but-unscheduled. The notes pin the shape so a future reviewer doesn't have to re-derive it; revival waits on a consumer workflow that justifies the work.

- **`net rpc metrics` + `net rpc trace`** — nRPC observability subcommands. `metrics` wraps `rpc_metrics_snapshot()` with `--output (json|prom|table)` (the SDK already provides `prometheus_text()`) and a `--watch <DUR>` polling mode. `trace` issues a single call with tracing turned on and emits the per-hop `RpcCallEvent` stream as ndjson alongside the reply. Both are useful for debugging routing decisions + latency; both are deferred until consumers ask, because (a) the Prometheus surface is already accessible from any Rust consumer of the SDK, and (b) per-call tracing's signal-to-noise is low without a UI on top.
- **`net rpc bench`** — load-test driver (concurrency × duration × payload → p50/p95/p99 latency, success rate, error breakdown by `RpcError` kind). Deferred — bench tooling is better served by `criterion` or a dedicated harness than by a CLI subcommand.
- **`net rpc serve --exec <BINARY>`** — host a service via a subprocess shim that talks to the CLI over stdio. Deferred — defining the stdio line protocol isn't worth it until a real cross-language consumer needs it; the Rust path goes through `net daemon run --kind <FACTORY-ID>` instead.
- **`net port probe-tcp <HOST> <PORT>`** — one-shot TCP connect probe with `--timeout`, reporting `open` / `refused` / `timeout` / `unreachable` + round-trip ms. Useful for ChatOps reachability checks ("is the coordinator's advertised endpoint reachable from here"). Deferred — operators reaching for this can use `nc -zv` / `curl --connect-timeout` / `timeout 5 bash -c '</dev/tcp/host/port'`; adding a CLI subcommand that wraps `tokio::net::TcpStream::connect` adds surface area without doing anything the OS doesn't already do. Revive only if a runbook needs structured (JSON) output that the existing tools don't emit.
- **`net db cache (stats|clear)`** — observability + control for the `LruResultCache` the MeshDB executor optionally wires via `LocalMeshQueryExecutor::with_cache(...)`. `stats` would print hit / miss / eviction counts + total entries; `clear` would reset the cache. Deferred — the cache is opt-in and operators rarely tune it directly; consumers that need the metrics already get them through the SDK's tracing-span surface. Revive when a real workflow needs CLI access to cache state.

## Non-goals

Per the scope brief, the CLI is not:

- An interactive TUI (that's Deck).
- A scheduler / replica-manipulation surface.
- A topology / node-lifecycle surface (no `net node add` / `net node remove`).
- A raw chain mutation tool (no escape hatches).
- A workflow / scripting engine (compose via shell pipelines + jq; the CLI emits structured output).
- A monitoring / alerting product (consumers pipe the output to their existing systems).
- A daemon-registry editor.
- A capability-system editor (capabilities flow through the SDK's `publish_capabilities` path — stubbed substrate-side today).

The CLI is **the operator-and-developer command surface, exposed as argv**. Everything else stays in the SDK, in Deck, or in adjacent tooling.

## Interaction surfaces

The CLI interacts with one substrate system per layer:

- **Rust SDK** (`ai2070-net-sdk`) — every read + write. The CLI never reaches around the SDK.
- **Local filesystem** — config + identity files; the binary follows XDG.
- **Local terminal** — TTY detection, ANSI colour, interactive confirm prompts.

It explicitly does NOT interact with:

- **The substrate's `MeshOsRuntime` directly.** The SDK is the only path.
- **The Deck binary.** They're peers; CLI doesn't shell out to Deck.
- **The four-language bindings.** Those serve other-language consumers; the CLI is Rust-only.

## Test surface

- **Per-subcommand integration tests** — `cargo test -p net-cli` runs the harness-backed suite. Each subcommand has ≥3 tests (happy path, typed-error path, output-format variant).
- **Help-text goldens** — `tests/golden/help/*.txt` pinned; updates require a deliberate `cargo test -- --bless` (or a CI workflow check that surfaces drift in the PR diff).
- **Exit-code coverage** — every documented exit code has a fixture that produces it; `tests/exit_codes.rs` enumerates the assertions.
- **`man net` generation** — CI runs `clap_mangen` and diffs against the committed `docs/man/net.1`; manpage drift surfaces as a PR diff.
- **Cross-binding parity** — out of scope (the CLI is a single-language tool); the four-language SDK bindings have their own parity test.

---

*v0.17 cycle release candidate. Gates on a real consumer workflow (CI bot / ChatOps / SRE runbook). Phases 1–4 land independently; Phase 5 (remote attach) is indefinitely deferred — the substrate already has the wire layer (encrypted UDP + nRPC + capability discovery), what's missing is the Deck nRPC service surface, and no consumer drives that today. The `net-blob` binary stays in place until Phase 4 absorbs it.*
