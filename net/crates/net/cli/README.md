# NET CLI

`net-mesh` — the unified command-line interface for the NET mesh.

Tools for capability publishers and consumers: host a stdio MCP server on the mesh, connect an MCP client, generate typed contracts, and manage local artifacts and stores. Execution scope depends on the command; mesh attachment does not provide remote Deck administration.

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
# Generate an identity file; this does not grant remote invocation authority.
net-mesh identity generate --out ~/.config/net-mesh/identity.toml

# Inspect publisher and consumer options before supplying your mesh target.
net-mesh wrap --help
net-mesh mcp serve --help

# Offline generation from an existing descriptor snapshot; no mesh required.
net-mesh typegen generate --language ts --from-snapshot tools.json --out ./generated
```

`wrap` keeps the publisher subprocess alive and is owner-only by default. Consumers need the configured target and appropriate permission; an identity or successful handshake alone is not permission. Live typegen currently uses inline schemas and does not fetch oversized metadata. These examples are entry points, not an accepted cross-computer deployment recipe.

### Temporary-supervisor development commands

**Starts a temporary supervisor for this command; does not inspect a running node.** This applies to admin/ICE, audit/log/failures, peer/daemon listings, capability reads, subnet topology reads, gateway/channel reads, and local aggregator inspection. `snapshot` already requires explicit `--local`; sibling commands do not yet have that safety gate. Admin `--dry-run` is an offline preview. Gateway export is unsupported.

```sh
net-mesh snapshot get --local
net-mesh snapshot status --local
```

Upgrading? [CHANGELOG.md](CHANGELOG.md) records what an operator or a CI
script has to do differently — 0.35 makes `--local` mandatory on both
`snapshot` verbs, which will fail scripts that call them.

## Subcommand surface

| Subcommand    | What it does                                                                    |
|---------------|---------------------------------------------------------------------------------|
| `version`     | SDK version + build metadata.                                                   |
| `identity`    | Generate / inspect / fingerprint operator identity files.                       |
| `admin`       | Offline previews or signed commits against a temporary supervisor. |
| `ice`         | Simulate/commit break-glass operations against a temporary supervisor. |
| `snapshot`    | One-shot substrate reads, both requiring `--local`: `get` prints the `MeshOsSnapshot`; `status` prints the typed `StatusSummary`. |
| `audit`       | Read/stream the temporary supervisor's audit ring. |
| `log tail`    | Substrate log stream (`--follow`, `--daemon`, `--min-level`).                   |
| `failures tail` | Substrate failure stream — same shape as `log tail`.                          |
| `cap`         | Temporary snapshot reads; `announce` authors a signed artifact offline, not a broadcast. |
| `peer`        | `ls` reads the temporary snapshot; no NAT-management verbs. |
| `daemon`      | Per-daemon listing from the local snapshot.                                     |
| `netdb`       | NetDB local KV adapter — Cortex-backed tasks + memories.                        |
| `org`         | Organization root authority authoring (keygen / issue-cert / issue-floors).     |
| `node`        | Node ownership provisioning (`adopt`).                                          |
| `subnet`      | Temporary topology reads; offline authority issuance and decode-only inspection. |
| `gateway`     | Temporary-context reads; `export` refuses without a live gateway. |
| `channel`     | `ChannelConfigRegistry` inspection (`visibility`, `ls`).                        |
| `aggregator`  | Temporary inspect/default list; remote query, spawn, scale, and explicitly remote list. |
| `transfer`    | Receive/admin via mesh; send computes references or stages local content, not hosting or publication. |
| `wrap`        | Wrap a local stdio MCP server as owner-only mesh capabilities.                  |
| `mcp`         | MCP bridge — expose mesh capabilities to a local MCP host (`serve`).            |
| `forwarding`  | Caller-side credential/header forwarding policy + audit (deny-by-default).      |
| `typegen`     | Generate typed language bindings from discovered tool descriptors.              |
| `completion`  | Emit a shell-completion script (`bash` / `zsh` / `fish` / `powershell`).        |
| `man`         | Emit the troff(1) man page on stdout.                                           |

## Global flags

Applied to every subcommand; environment-variable fallbacks in brackets:

- `--config <path>` `[NET_MESH_CONFIG]` — profile file (default `$XDG_CONFIG_HOME/net-mesh/config.toml`).
- `--profile <name>` `[NET_MESH_PROFILE]` — named profile within the config file.
- `--insecure-config-permissions` `[NET_MESH_INSECURE_CONFIG_PERMISSIONS]` — read the profile even when it is group/world-accessible or owned by another user.
- `--output (json|yaml|ndjson|table|text)` — auto-detects `table`/`text` on TTY and `json`/`ndjson` off-TTY.
- `--quiet` / `-q` — suppress stderr diagnostics.
- `--verbose` / `-v` — `-v` info, `-vv` debug, `-vvv` trace. `NET_MESH_LOG=` env-filter overrides.
- `--no-color` `[NO_COLOR]` — disable ANSI in table / text output.
- `--timeout <dur>` — parsed duration (e.g. `500ms`, `1h30m`; default `30s`), currently not forwarded by dispatch. It is not an enforced universal deadline.

Errors are plain `net-mesh: ...` messages on stderr. ICE may emit separate preview and commit JSON values; do not assume all commands have single-value framing. ICE still requires typed `YES` on interactive stdin even with `--yes`; dry-run needs no confirmation. `mcp serve` stdout is protocol traffic.

## Config + identity

The default profile file is optional. Remote commands still need a complete target and credentials. The file lives at `$XDG_CONFIG_HOME/net-mesh/config.toml` (or the platform equivalent):

```toml
[default]
identity        = "~/.config/net-mesh/identity.toml"
endpoint        = "in-process"
default_timeout_ms = 30000

[profiles.prod]
identity   = "~/.config/net-mesh/ops-identity.toml"
node_addr  = "10.0.0.4:7700"
node_id    = 4
node_pubkey = "abcd…"      # 64 hex
psk_hex     = "1234…"      # 64 hex
```

Operator identity files are authored by `net-mesh identity generate` — ed25519 seed + public key + SHA-256 fingerprint, the same format the deck loads from the maintenance node. Every signed `admin` / `ice` command picks the identity up from the active profile (or `--identity`).

## Exit codes

Typed via `ExitCodeKind`. Scripts can match on the discriminator:

- `0` — success
- `1` — generic error
- `2` — invalid arguments / parse error
- `3` — SDK error
- `4` — ICE simulation blocked
- `5` — operator-policy reject
- `6` — connection failure
- `7` — timeout
- `8` — confirmation refused
- `10` — reserved daemon-factory error
- `11` — reserved MeshDB query parse error
- `12` — reserved predicate parse error
- `13` — `ice`: a supplied operator signature failed verification
- `14` — `typegen diff --exit-code`: a breaking schema change was detected

## Shell completion + man page

```sh
net-mesh completion bash > /etc/bash_completion.d/net-mesh
net-mesh completion zsh  > "${fpath[1]}/_net-mesh"
net-mesh man             > /usr/local/share/man/man1/net-mesh.1
```

Release tarballs ship these pre-generated under `share/bash-completion/...` and `share/man/man1/`.

## License

MIT OR Apache-2.0. See [`LICENSE-MIT`](../../../../LICENSE-MIT) and [`LICENSE-APACHE`](../../../../LICENSE-APACHE).
