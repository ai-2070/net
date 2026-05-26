# NET Deck

The operator **cyberdeck** — a terminal UI for the NET mesh.

Live, streaming, low-latency visibility into MeshOS, MeshDB, RedEX, and Dataforts, with signed admin actions on top. Built with ratatui + crossterm. Matrix palette, neon-green on pitch black.

![Deck — NET.MAP](https://github.com/ai-2070/net/blob/master/images/net-deck-1.png?raw=true)

## Install

```sh
# crates.io
cargo install net-deck

# prebuilt binary (no compile)
cargo binstall net-deck

# npm (per-platform binary shim)
npm install -g @net-mesh/deck

# PyPI (maturin-built wheel, bundles the binary)
pip install net-deck
```

Prebuilt tarballs for linux (glibc + musl, x86_64 + aarch64), macOS (x86_64 + aarch64), and Windows (x86_64 + aarch64) are published to the [GitHub Releases page](https://github.com/ai-2070/net/releases) under the `deck-v*` tag prefix.

## Run

```sh
# Attach to a single-node in-process MeshOsRuntime
net-deck

# Boot a real 5-node in-process MeshOS cluster (dev-only)
cargo run -p net-deck --features demo
```

The `demo` feature wires `net_sdk::testing::ClusterHarness` — real daemons, real migrations, real blob adapters, a real `RpcObserver` feeding the NRPC tail. See `crates/net/docs/plans/DECK_DEMO_PLAN.md`.

## Tabs

| Tab          | What it shows                                                                       |
|--------------|-------------------------------------------------------------------------------------|
| `NET.MAP`    | Live mesh topology — nodes, RTT, health, avoid-lists, maintenance, replica heat.    |
| `NODES`      | Per-node inventory: CPU / mem / disk, saturation trend, capability set, versions.   |
| `DAEMONS`    | Per-daemon supervision — health, saturation, restarts, crash-loops, log tail.       |
| `DATAFORTS`  | Replica & placement: desired vs actual, migrations, pulls, eviction, 5-axis score.  |
| `BLOBS`      | Object inventory across every wired adapter — heat, ancestry, shard layout.         |
| `MIGRATIONS` | In-flight + recent migrations with byte progress and stall detection.               |
| `REPLICAS`   | Replica density by artifact, drift, placement stability.                            |
| `GROUPS`     | Replica / fork / standby groupings.                                                 |
| `SUBNETS`    | Subnet membership and gateway routing.                                              |
| `GATEWAYS`   | Gateway daemons — bridges into the mesh from outside transports.                    |
| `AGGREGATORS`| Aggregator-daemon attach / scale state, remote-attach RPC tail.                     |
| `NRPC`       | Live nRPC call tail — request / response / failure stream across the cluster.       |
| `LOGS`       | High-speed log matrix — node → daemon → line, with filter + follow.                 |
| `AUDIT`      | RedEX-committed operator audit ledger.                                              |
| `FAILURES`   | Recent failures across daemons, migrations, blob pulls.                             |

![Deck — DATAFORTS](https://github.com/ai-2070/net/blob/master/images/net-deck-3.png?raw=true)
![Deck — LOGS](https://github.com/ai-2070/net/blob/master/images/net-deck-7.png?raw=true)

## Admin surface — signed ops

Every admin action propagates as a signed event on the admin chain via RedEX:

- drain / cordon / uncordon node
- enter / exit maintenance
- drop replicas, invalidate placement
- restart daemons, clear avoid lists
- ICE: force-drain, force-evict, force-restart, force-cutover, freeze / thaw

Before an ICE action commits, Deck runs a **blast-radius** simulation —
*"This action affects 4 nodes, 12 replicas, and 2 daemons. Continue?"* — then signs with the operator key loaded from the maintenance node. Multi-operator signing and lockout timers are available for the high-authority break-glass paths.

![Deck — ICE / Admin](https://github.com/ai-2070/net/blob/master/images/net-deck-9.png?raw=true)

## Bookmarks (multi-cluster)

Saved cluster contexts live at `$XDG_CONFIG_HOME/deck/bookmarks.toml` (or the platform equivalent — see [`dirs`](https://docs.rs/dirs)). First-run with no config dir yields an empty store; a malformed file is surfaced via stderr.

## License

Apache-2.0. See [`LICENSE`](../LICENSE).
