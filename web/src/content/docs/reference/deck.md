---
title: Deck (Operator TUI)
description: "net-deck is the operator TUI — a live view of the mesh with signed admin actions on top."
---
# Deck — the operator TUI

`net-deck` is the operator cyberdeck: a terminal UI over a live mesh, with
signed admin actions on top. It's the answer to "what is actually happening in
this cluster right now" — MeshOS, MeshDB, RedEX, and Dataforts in one place,
streaming rather than polled.

It ships as its own binary and its own crate. Nothing else in Net depends on it,
and it depends on the same SDK surfaces you'd use yourself.

## Install

```sh
cargo install net-deck            # crates.io
cargo binstall net-deck           # prebuilt binary, no compile
npm install -g @net-mesh/deck     # per-platform binary shim
pip install net-deck              # maturin wheel, bundles the binary
```

Prebuilt tarballs for Linux (glibc + musl, x86-64 + aarch64), macOS (x86-64 +
aarch64), and Windows (x86-64 + aarch64) are published on the
[releases page](https://github.com/ai-2070/net/releases) under the `deck-v*`
tag prefix.

## Run

```sh
net-deck                                  # attach to an in-process MeshOsRuntime
cargo run -p net-deck --features demo     # boot a 5-node in-process cluster
```

The `demo` feature is dev-only and wires `net_sdk::testing::ClusterHarness` —
real daemons, real migrations, real blob adapters, and a real `RpcObserver`
feeding the nRPC tail. It's the fastest way to see the UI against something
that behaves like a cluster.

## Tabs

| Tab | What it shows |
| --- | --- |
| `NET.MAP` | Live mesh topology — nodes, RTT, health, avoid-lists, maintenance, replica heat |
| `NODES` | Per-node inventory: CPU / mem / disk, saturation trend, capability set, versions |
| `DAEMONS` | Per-daemon supervision — health, saturation, restarts, crash-loops, log tail |
| `DATAFORTS` | Replica and placement: desired vs actual, migrations, pulls, eviction, 5-axis score |
| `BLOBS` | Object inventory across every wired adapter — heat, ancestry, shard layout |
| `MIGRATIONS` | In-flight and recent migrations with byte progress and stall detection |
| `REPLICAS` | Replica density by artifact, drift, placement stability |
| `GROUPS` | Replica / fork / standby groupings |
| `SUBNETS` | Subnet membership and gateway routing |
| `GATEWAYS` | Gateway daemons — bridges into the mesh from outside transports |
| `AGGREGATORS` | Aggregator-daemon attach / scale state, remote-attach RPC tail |
| `NRPC` | Live nRPC call tail — request / response / failure across the cluster |
| `LOGS` | High-speed log matrix — node → daemon → line, with filter and follow |
| `AUDIT` | RedEX-committed operator audit ledger |
| `FAILURES` | Recent failures across daemons, migrations, blob pulls |

## Admin actions are signed events

Every admin action propagates as a **signed event on the admin chain** via
RedEX. That's the design decision worth understanding: there is no privileged
side channel and no mutable control table. An operator action is an append to a
log, signed by the operator key, and the `AUDIT` tab is that log read back.

The available actions:

- drain / cordon / uncordon a node
- enter / exit maintenance
- drop replicas, invalidate placement
- restart daemons, clear avoid lists
- **ICE** (break-glass): force-drain, force-evict, force-restart, force-cutover,
  freeze / thaw

Before an ICE action commits, Deck runs a **blast-radius simulation** — *"This
action affects 4 nodes, 12 replicas, and 2 daemons. Continue?"* — and then signs
with the operator key loaded from the maintenance node. The high-authority
break-glass paths support multi-operator signing and lockout timers.

Because actions are signed events rather than RPCs, an action taken during a
partition converges like any other event, and the audit trail is the same
artifact on every node.

## Multi-cluster bookmarks

Saved cluster contexts live at `$XDG_CONFIG_HOME/deck/bookmarks.toml` (or the
platform equivalent — see [`dirs`](https://docs.rs/dirs)). A first run with no
config directory yields an empty store; a malformed file is surfaced on stderr
rather than silently ignored.

## See also

- [CLI reference](/docs/reference/cli) — the non-interactive surface.
- [Daemons and placement](/docs/guides/daemons-and-placement) — what the
  `DAEMONS` and `DATAFORTS` tabs are showing you.
- [Blob storage (Dataforts)](/docs/guides/dataforts) — the placement model
  behind the 5-axis score.
