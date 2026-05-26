# NET CLI

`net-mesh` — the unified command-line interface for the NET mesh.

The non-interactive counterpart to [`net-deck`](https://github.com/ai-2070/net/tree/master/net/crates/net/deck): a one-shot tool for operator scripts, CI pipelines, daemon authoring, and ad-hoc cluster inspection. Same SDK underneath, same signed admin chain, no TUI.

![net-mesh](https://github.com/ai-2070/net/blob/master/images/net-cli-1.png?raw=true)

## Install

```sh
# crates.io
cargo install net-cli

# prebuilt binary (no compile)
cargo binstall net-cli

# npm (per-platform binary shim)
npm install -g @net-mesh/cli

# PyPI (maturin-built wheel, bundles the binary)
pip install net-mesh-cli
```

The crate is `net-cli` but the binary it installs is **`net-mesh`**. Prebuilt tarballs for linux (glibc + musl, x86_64 + aarch64), macOS (x86_64 + aarch64), and Windows (x86_64 + aarch64) are published to the [GitHub Releases page](https://github.com/ai-2070/net/releases) under the `cli-v*` tag prefix.

## Quick start

```sh
# Generate an operator identity (ed25519 seed + pubkey + fingerprint)
net-mesh identity generate --out ~/.config/net-mesh/identity.toml

# One-shot snapshot read (auto-formatted JSON for non-TTY, table for TTY)
net-mesh snapshot show

# Tail substrate logs as ndjson, follow mode, filtered to one daemon
net-mesh log tail --daemon 0x000007 --follow --output ndjson

# Signed admin commit — drain a node, propagated on the admin chain via RedEX
net-mesh admin drain --node 0x1a2b --drain-for 10m

# Break-glass ICE — simulate blast radius first, commit only after confirm
net-mesh ice force-restart --daemon 0x000007 --simulate
net-mesh ice force-restart --daemon 0x000007 --confirm
```

## Subcommand surface

| Subcommand    | What it does                                                                    |
|---------------|---------------------------------------------------------------------------------|
| `version`     | SDK version + build metadata.                                                   |
| `identity`    | Generate / inspect / fingerprint operator identity files.                       |
| `admin`       | Signed admin-chain commits — drain, cordon, maintenance, drop-replicas, etc.    |
| `ice`         | Break-glass ICE — simulate then commit force-drain / evict / restart / cutover. |
| `snapshot`    | One-shot `MeshOsSnapshot` reads (and `--watch` for streaming).                  |
| `audit`       | Read-only queries against the RedEX-committed audit ledger.                     |
| `log tail`    | Substrate log stream (`--follow`, `--daemon`, `--level`).                       |
| `failures tail` | Substrate failure stream — same shape as `log tail`.                          |
| `cap`         | Capability advertisement + discovery.                                           |
| `peer`        | Peer + NAT-traversal helpers (`peer ls` today; reflex/NAT in Phase 2).          |
| `daemon`      | Per-daemon listing from the local snapshot.                                     |
| `netdb`       | NetDB local KV adapter — Cortex-backed tasks + memories.                        |
| `subnet`      | Hierarchical subnet inspection (`show`, `ls`, `tree`).                          |
| `gateway`     | `SubnetGateway` stats + export-table operator surface.                          |
| `channel`     | `ChannelConfigRegistry` inspection (`visibility`, `ls`).                        |
| `aggregator`  | `AggregatorDaemon` inspection + remote query.                                   |
| `completion`  | Emit a shell-completion script (`bash` / `zsh` / `fish` / `powershell`).        |
| `man`         | Emit the troff(1) man page on stdout.                                           |

## Global flags

Applied to every subcommand; environment-variable fallbacks in brackets:

- `--config <path>` `[NET_MESH_CONFIG]` — profile file (default `$XDG_CONFIG_HOME/net-mesh/config.toml`).
- `--profile <name>` `[NET_MESH_PROFILE]` — named profile within the config file.
- `--output (json|yaml|ndjson|table|text)` — auto-detects `table`/`text` on TTY and `json`/`ndjson` off-TTY.
- `--quiet` / `-q` — suppress stderr diagnostics.
- `--verbose` / `-v` — `-v` info, `-vv` debug, `-vvv` trace. `NET_MESH_LOG=` env-filter overrides.
- `--no-color` `[NO_COLOR]` — disable ANSI in table / text output.
- `--timeout <dur>` — global per-call timeout (e.g. `500ms`, `1h30m`). Default `30s`.

## Config + identity

The profile file is optional — every flag has a sensible default. When present, it lives at `$XDG_CONFIG_HOME/net-mesh/config.toml` (or the platform equivalent) and looks like:

```toml
[default]
identity        = "~/.config/net-mesh/identity.toml"
endpoint        = "in-process"
default_timeout_ms = 30000

[profiles.prod]
identity   = "~/.config/net-mesh/ops-identity.toml"
node_addr  = "10.0.0.4:7700"
node_pubkey = "abcd…"      # 64 hex
psk_hex     = "1234…"      # 64 hex
```

Operator identity files are authored by `net-mesh identity generate` — ed25519 seed + public key + SHA-256 fingerprint, the same format the deck loads from the maintenance node. Every signed `admin` / `ice` command picks the identity up from the active profile (or `--identity`).

## Exit codes

Typed via `ExitCodeKind`. Scripts can match on the discriminator:

- `0` — success
- `2` — usage / parse error
- `3` — config / identity load failure
- `4` — substrate refused the action (auth, ICE threshold, etc.)
- `5` — timeout
- `6` — transport error
- non-zero otherwise — see `src/error.rs` for the full table

## Shell completion + man page

```sh
net-mesh completion bash > /etc/bash_completion.d/net-mesh
net-mesh completion zsh  > "${fpath[1]}/_net-mesh"
net-mesh man             > /usr/local/share/man/man1/net-mesh.1
```

Release tarballs ship these pre-generated under `share/bash-completion/...` and `share/man/man1/`.

## License

Apache-2.0. See [`LICENSE`](../LICENSE).
