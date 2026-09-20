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

**Starts a temporary supervisor for this command; does not inspect a running node.** This applies to admin/ICE, snapshot, audit/log/failures, peer/daemon listings, capability reads, subnet topology reads, gateway/channel reads, and local aggregator inspection. These paths now require explicit `--local`, with a scope notice on stderr even under `--quiet`. Admin `--dry-run` remains an offline preview and needs no opt-in; ICE simulation does require it. Gateway export remains unsupported. Offline issuance and real remote clients do not take this flag.

```sh
net-mesh snapshot get --local
net-mesh snapshot status --local
```

Upgrading? [CHANGELOG.md](CHANGELOG.md) records what an operator or a CI
script has to do differently. Temporary-supervisor scripts must now add
`--local` intentionally; this does not turn them into deployment operations.

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
| `aggregator`  | Temporary inspect/list with `--local`; remote query/spawn/scale and list selected by flags or profile. Remote verbs and `ls` support `--inspect-target`. |
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
- `--quiet` / `-q` — suppress progress/logging; temporary-supervisor scope disclosures remain on stderr.
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
node_id    = "4"
node_pubkey = "abcd…"      # 64 hex
psk_hex     = "1234…"      # 64 hex
bind        = "0.0.0.0:0" # explicit IPv4 off-host client bind
```

Operator identity files are authored by `net-mesh identity generate` — ed25519 seed + public key + SHA-256 fingerprint, the same format the deck loads from the maintenance node. Every signed `admin` / `ice` command picks the identity up from the active profile (or `--identity`).

Every dispatched command validates explicitly selected config files and profiles before doing work, including offline commands and selections from `NET_MESH_CONFIG` / `NET_MESH_PROFILE`. Missing files, unknown profiles and malformed/unsafe config files fail; flags override environment selections. Remove an obsolete selector if the command does not need it. Commands that do not use configuration still do not load the implicit default merely to execute; profile-backed commands allow its absence. Parser-only help/version flags remain available without dispatch.

### Inspect mesh targeting

```sh
net-mesh aggregator ls --profile prod --inspect-target --output json
net-mesh aggregator ls --profile prod --output json
net-mesh aggregator ls --profile prod --local --inspect-target --output json
```

With a complete profile target, `aggregator ls` now uses remote RPC even without `--remote`. Flags override individual profile fields; partial tuples fail. `--local` selects a temporary supervisor despite profile defaults and discloses that choice, but conflicts with explicit remote flags. Remote failure never falls back locally.

`--inspect-target` is available on aggregator `ls/query/spawn/scale`, transfer receive/admin, live typegen, `wrap`, `mcp serve`, and feature-gated anchor `ls/stats`. It reports mode, peer address/id, public fingerprints, identity availability, bind and provenance, plus output destination/provider ID where applicable. It reads configured files but does not connect, start a supervisor or child process, create output, mint an identity, or check authorization. Execution consumes the same resolved target and bind. An unconfigured identity has no fingerprint; hosted services still require one to execute. `mcp serve --inspect-target` emits ordinary one-shot output and exits without starting the MCP protocol.

For these clients, `--bind <IP:PORT>` overrides profile `bind`, then the existing default applies: `127.0.0.1:0` for short-lived clients, `0.0.0.0:0` for `wrap`/`mcp serve`. A loopback bind with a non-loopback peer is rejected before attachment; select a reachable local interface or explicitly opt into wildcard binding. IPv6 peers require an IPv6 bind, for example `[::]:0`. No default exposure is widened, and a valid bind does not prove firewall/NAT reachability or authorization. The non-loopback integration test uses two participants on one runner, not two computers.

All NetDB commands and saved typegen input also support inspection:

```sh
net-mesh netdb tasks ls --profile prod --inspect-target --output json
net-mesh netdb restore --store ./state --from ./backup.bin --clear --inspect-target
net-mesh typegen generate --language ts --from-snapshot ./tools.json --out ./generated --inspect-target
```

NetDB reports `mode: persistent_store`, the store (`--store` > profile `netdb` > data-directory default), and snapshot input/output paths where applicable. Saved typegen reports `mode: offline` and its input/output paths. Both report unused remote defaults and `identity.state: unused`: they do not consume a signing identity. Inspection reads profile configuration but does not open/create/clear a store or read artifact payloads. Missing input/output paths can therefore be inspected. This is resolution inspection, not restore preflight, content validation or proof that a path is writable. Execution retains its existing validation gates. Offline typegen continues to reject explicit remote target/bind flags.

Forwarding policy (`enable/disable/allow/rm/audit`), MCP pins (`approve/reject/list`), and transfer `send-blob/send-dir` also support `--inspect-target`. Policy/pin inspection reports the existing per-user default or explicit `--store`/`--pin-store`; profile `netdb` does not select these stores. It does not load their contents, acquire mutation locks or change consent. Transfer inspection reports source and optional staging store: persistent-store mode with `--store`, offline mode without it. `send-blob -` reports stdin without reading it; directory inspection does not walk the source. Staging still does not host or publish bytes. Inspection is not argument/content/policy validation or authorization approval.

Inspection is not yet CLI-wide: offline issuance, other temporary-supervisor commands, standalone `anchor serve`, and keychain-backed `forwarding set-value` remain outside this surface.

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
