# @net-mesh/cli

`net-mesh` — the unified command-line interface for the NET mesh.

The non-interactive counterpart to [`@net-mesh/deck`](https://www.npmjs.com/package/@net-mesh/deck): a one-shot tool for operator scripts, CI pipelines, daemon authoring, and ad-hoc cluster inspection. Same SDK underneath, same signed admin chain, no TUI.

![net-mesh](https://github.com/ai-2070/net/blob/master/images/net-cli-1.png?raw=true)

## Install

```sh
# npm (per-platform binaries included)
npm install -g @net-mesh/cli
```

`@net-mesh/cli` is a thin Node.js shim — installing it pulls in the right per-platform binary package as an `optionalDependency` (npm refuses to install packages that don't match the host's `os` / `cpu` / `libc`). The shim resolves the installed package at runtime and `exec`s the bundled `net-mesh` binary.

## Run

```sh
net-mesh --help
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
| `peer`        | Peer + NAT-traversal helpers.                                                   |
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

## License

Apache-2.0.
