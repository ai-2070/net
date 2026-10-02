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
| `up` / `down` | Start one long-lived node for the profile (foreground); drain and stop it.      |
| `invite`      | Create / inspect / list / revoke / approve / deny join links on `up --enroll`.  |
| `join` / `leave` | Join a mesh from a link and run as it; leave it (local, durable).            |
| `enrollment`  | Enrollment ledger setup for `up --enroll`.                                      |
| `relay`       | Run a blind UDP relay (`serve`), the fallback path for unreachable devices.     |
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
| `org`         | Offline org root tools; org links (`invite` / `approve` / `join`), `remove`, `leave`, `members`. |
| `node`        | `status` of this profile's node; ownership provisioning (`adopt`).             |
| `subnet`      | Offline authority issuance; subnet links (`invite` / `join`), `remove`, `leave`, `members`; temporary topology reads. |
| `gateway`     | Temporary-context reads; `export` refuses without a live gateway. |
| `channel`     | Offline `issue-grant`; standalone links (`invite` / `join`); `serve`, `status`, `publish`, `leave` on the running node; registry reads (`visibility`, `ls`). |
| `aggregator`  | Temporary inspect/list with `--local`; remote query/spawn/scale and list selected by flags or profile. Remote verbs and `ls` support `--inspect-target`. |
| `transfer`    | Receive/admin via mesh; send computes references or stages local content, not hosting or publication. |
| `wrap`        | Wrap a local stdio MCP server as owner-only mesh capabilities (`--joined` runs as an enrolled device). |
| `mcp`         | MCP bridge — expose mesh capabilities to a local MCP host (`serve`, `--joined`). |
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
- `1` — generic error
- `2` — invalid arguments / parse error
- `3` — SDK error
- `4` — ICE simulation blocked
- `5` — operator-policy reject
- `6` — connection failure
- `7` — timeout
- `8` — confirmation refused
- `10` — `daemon`: factory id not registered
- `11` — `db`: query JSON failed to parse
- `12` — `db`: predicate DSL (`--where` / `--filter`) failed to parse
- `13` — `ice`: a supplied operator signature failed verification
- `14` — `typegen diff --exit-code`: a breaking schema change was detected

## License

MIT OR Apache-2.0. See [`LICENSE-MIT`](LICENSE-MIT) and [`LICENSE-APACHE`](LICENSE-APACHE).
