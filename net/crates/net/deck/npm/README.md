# @net-mesh/deck

The operator **cyberdeck** — a terminal UI for the NET mesh.

Live, streaming, low-latency visibility into MeshOS, MeshDB, RedEX, and Dataforts, with signed admin actions on top. Built with ratatui + crossterm. Matrix palette, neon-green on pitch black.

![Deck — NET.MAP](https://github.com/ai-2070/net/blob/master/images/net-deck-1.png?raw=true)

## Install

```sh
# npm (per-platform binaries included)
npm install -g @net-mesh/deck
```

`@net-mesh/deck` is a thin Node.js shim — installing it pulls in the right per-platform binary package as an `optionalDependency` (npm refuses to install packages that don't match the host's `os` / `cpu` / `libc`). The shim resolves the installed package at runtime and `exec`s the bundled `net-deck` binary.

## Run

```sh
net-deck
```

## Tabs

| Tab          | What it shows                                                                       |
|--------------|-------------------------------------------------------------------------------------|
| `NET.MAP`    | Live mesh topology — nodes, RTT, health, avoid-lists, maintenance, replica heat.    |
| `NODES`      | Per-node inventory: CPU / mem / disk, saturation trend, capability set, versions.   |
| `DAEMONS`    | Per-daemon supervision — health, saturation, restarts, crash-loops, log tail.       |
| `DATAFORTS`  | Replica & placement: desired vs actual, migrations, pulls, eviction, 5-axis score.  |
| `BLOBS`      | Object inventory across every wired adapter — heat, ancestry, shard layout.         |
| `MIGRATIONS` | In-flight + recent migrations with byte progress and stall detection.               |
| `CHAINS`     | Replica density by artifact, drift, placement stability.                            |
| `GROUPS`     | Replica / fork / standby groupings.                                                 |
| `SUBNETS`    | Subnet membership and gateway routing.                                              |
| `GATEWAYS`   | Gateway daemons — bridges into the mesh from outside transports.                    |
| `AGGREGATORS`| Aggregator-daemon attach / scale state, remote-attach RPC tail.                     |
| `NRPC`       | Live nRPC call tail — request / response / failure stream across the cluster.       |
| `LOGS`       | High-speed log matrix — node → daemon → line, with filter + follow.                 |
| `AUDIT`      | RedEX-committed operator audit ledger.                                              |

![Deck — NODES](https://github.com/ai-2070/net/blob/master/images/net-deck-3.png?raw=true)
![Deck — DATAFORTS](https://github.com/ai-2070/net/blob/master/images/net-deck-5.png?raw=true)

## Admin surface — signed ops

Every admin action propagates as a signed event on the admin chain via RedEX:

- drain / cordon / uncordon node
- enter / exit maintenance
- drop replicas, invalidate placement
- restart daemons, clear avoid lists
- ICE: freeze / thaw cluster, flush avoid-lists, force-evict-replica, force-restart-daemon, force-cutover, kill-migration

Before an ICE action commits, Deck runs a **blast-radius** simulation —
*"This action affects 4 nodes, 12 replicas, and 2 daemons. Continue?"* — then signs with the operator key loaded from the maintenance node.

Operator signatures are verified against an `AdminVerifier` `OperatorRegistry` — the built-in runtime (the single-node default and the `--features demo` cluster) wires a single operator keypair at threshold 1. Multi-operator M-of-N verification requires a populated registry and a raised threshold.

## Bookmarks (multi-cluster)

Saved cluster contexts live at `$XDG_CONFIG_HOME/net-deck/bookmarks.toml` (or the platform equivalent — see [`dirs`](https://docs.rs/dirs)). First-run with no config dir yields an empty store; a corrupt file is renamed aside (`<path>.corrupt-<ms>`) and an empty store returned.

## License

MIT OR Apache-2.0. See [`LICENSE-MIT`](LICENSE-MIT) and [`LICENSE-APACHE`](LICENSE-APACHE).
