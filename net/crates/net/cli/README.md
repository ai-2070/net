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

For a runnable source-checkout example, start with the
[two-node capability journey](tests/fixtures/README.md). It starts separate
`wrap --listen` and `mcp serve` processes, proves both permission boundaries,
and checks the returned value against a provider-side invocation record.
It runs on loopback on one machine; it is not off-host acceptance evidence.
The same guide includes native Rust typed calls, live contract capture,
generated Python consumption over a real-mesh fixture adapter, and offline
Python/TypeScript regeneration. A separate protected native leg proves org
admission denial before handler effect and authorized native and generated Python
calls (through a Rust SDK fixture adapter). It keeps
org admission, public native tools, and MCP's owner/pin policy distinct.

```sh
# Generate an identity file; this does not grant remote invocation authority.
net-mesh identity generate --out ~/.config/net-mesh/identity.toml

# Inspect publisher and consumer options before supplying your mesh target.
net-mesh wrap --help
net-mesh mcp serve --help

# Offline generation from an existing descriptor snapshot; no mesh required.
net-mesh typegen generate --language ts --from-snapshot tools.json --out ./generated
```

`wrap` keeps the publisher subprocess alive and is owner-only by default. Consumers need the configured target and appropriate permission; an identity or successful handshake alone is not permission. Live typegen fetches missing schemas from the exact advertising provider: native peers negotiate responses up to 1 MiB (packet limit stays 8 KiB), older providers may still refuse responses above a single packet, and mismatched or unusable contracts are refused before any output. These examples are entry points, not an accepted cross-computer deployment recipe.

### Starting the first publisher

`wrap --listen` starts without a bootstrap peer. Supply an operator identity,
a 32-byte PSK through `--psk-hex` or a protected profile, and the stdio server
command after `--`. Listener bind precedence is `--bind`, profile `bind`, then
`127.0.0.1:0`. Remote peer settings conflict with `--listen`, including profile
defaults; select a dedicated profile. The ordinary attached mode is unchanged.

With `--output ndjson`, the initial `wrapped` event includes `connection.bind`
(the actual port), `connection.node_id` (hex string), `connection.node_pubkey`
(the live public Noise key), and `connection.origin_hash` (hex string). Give
the first three to the consumer's `--node-addr`, `--node-id`, and
`--node-pubkey`; supply the same PSK separately. The event contains no PSK or
identity seed. The Noise key belongs to this running process: use a fresh
readiness event after restarting it. A wildcard bind is not a dialable address;
choose a reachable interface address yourself. Cross-host reachability and
firewall configuration are not established by the loopback journey.

Readiness means the local tool services were published, not that a peer has
discovered them or has permission to invoke. `--allow <consumer-origin>` widens
the provider policy only for the named identity; `mcp pin approve` is separate
consumer consent and cannot override that policy. `--inspect-target` validates
listener selection without binding a port or starting the child; it cannot
report a live port or Noise key. Startup `--timeout` ends at publication; it
does not stop a healthy published service later.

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
- `--timeout <dur>` — optional total budget (e.g. `500ms`, `1h30m`) for remote aggregator `ls/query/spawn/scale`, transfer `ls/status/cancel/recv-blob`, live typegen `generate/snapshot` acquisition, and `mcp serve`/`wrap` startup. One absolute deadline covers dispatch/configuration, connection and RPC/discovery stages; it is not reset per stage. Typegen rendering and output writes run outside cancellation after acquisition succeeds, so wall-clock completion can exceed the budget. Zero budget refuses before execution. Expiry exits 7 without a success payload; remote effects may already have committed, and the CLI does not retry the operation. Omitting the flag retains existing command/SDK limits—the previously advertised global `30s` default was not enforced and has been removed. Other commands, including transfer directory receive/staging, offline typegen, local modes and target inspection, currently reject an explicit timeout before effects. Do not use it as a service lifetime limit or an interruptible local-mutation guarantee.

For `wrap`, startup also includes child MCP initialization, tool discovery and publication. Startup timeout drops the managed direct child and emits no `wrapped` event. Once published, the provider and subsequent refreshes outlive the startup budget. This is not a process-tree supervisor for arbitrary descendants launched by the wrapped program.

For `mcp serve`, the budget ends when the shim is ready to process protocol input. It covers identity loading and mesh attachment, not waiting for the client's initialize request or the running session. Startup failure emits no protocol output; a started shim continues until EOF or operator shutdown.

Blob receive applies the same deadline to attachment and each network wait. Disk writes count toward elapsed budget but are not cancelled; final rename proceeds normally after the complete stream is acquired. Timeout leaves the destination unchanged but may leave `<out>.partial` staging bytes. This is not a resumable download or a guarantee that total wall-clock time stays within the budget.

Errors are plain `net-mesh: ...` messages on stderr. ICE commits emit one result containing `preview` and `commit`; the pre-confirmation preview is diagnostic output on stderr. Refusal or failure emits no success payload on stdout. ICE `--yes` skips prompting on both interactive and noninteractive stdin, but does not bypass signature or policy checks. Without it, interactive stdin requires typed `YES`; unattended commits refuse with exit 8. Dry-run needs no confirmation and retains its preview-only shape. `mcp serve` stdout is protocol traffic.

ICE scripting migration: replace parsing two successive JSON values (for example `jq -s '.[1].commit_id'`) with `jq '.commit.commit_id'`. Read the successful simulation from `.preview`; for a dry-run, `jq '.blast_hash'` is unchanged. Do not parse the diagnostic preview on stderr as a committed result.

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

Feature-enabled `anchor serve --inspect-target` resolves mesh/HTTPS/RTC/STUN binds, TLS file paths or ACME cache/challenge settings, and the public issuer fingerprint. It reads profile configuration but not PSK/TLS files; it does not start sockets, order certificates or generate an identity. Normal serving consumes the same resolved selections. The standalone anchor keeps its existing ephemeral identity and ignores profile identity/remote/bind defaults; inspection reports that identity as unavailable. Defaults remain mesh `0.0.0.0:0`, HTTPS `0.0.0.0:8443`, RTC on the mesh IP with an ephemeral port, and no second STUN socket. `:0` is a requested ephemeral port, not a bound address; runtime-announced endpoints, TLS validity, authority and reachability are not verified by inspection.

Identity `generate/show/fingerprint/revoke` and `cap announce` also support `--inspect-target`. Generate reports an unavailable identity and either explicit `destination` or a `destination_pattern` containing `<generated-operator-id>`; it does not generate a key merely to guess the runtime filename. Show/fingerprint inspect the source path without reading its contents or calculating its subject fingerprint. Revoke reports the selected floor store and a public issuer fingerprint without opening the store or changing floors; it does not prove revocation has propagated.

`cap announce --inspect-target` reads the explicit `--key` through its normal secret-file gate, reports that signer's public fingerprint and the file/stdout destination, then exits without signing or emitting announcement bytes. The profile identity is unused; a conflicting `--node-id` still fails. Inspection is not validation of all announcement policies/tags or publication to a mesh. Normal execution retains its signing and output behavior.

`node adopt --inspect-target` reports the certificate/floors input paths, explicit/default authority directory, three authority filenames and public node fingerprint. With `--identity`, it reads the identity through the normal file gate; the identity is a subject, not a signer. Inspection does not read certificate/floors payloads, open the authority directory or install ownership. Skew and entity selection are checked, but certificate validity, directory permissions and authorization are not.

All five `org` verbs support `--inspect-target`. Keygen reports an unavailable identity and either an explicit destination or an `org-<generated-org-id-prefix>.toml` filename pattern, without generating a key. Issuance/grant inspection reads the explicit org key through its normal permission/parse gate and reports the signer's public fingerprint and output path. Discovery grants also report `audience_destination` without minting an audience secret. Profile identity/remote defaults are unused. Grant `--force` refusal and discovery/audience-output pairing still apply; inspection does not check all grant policy, TTL, aliases or output permissions and does not approve issuance. Normal publication safeguards are unchanged.

Subnet `keygen`, `issue-direct`, `issue-issuer`, `issue-delegated`, all four `issue-control-fact` subcommands and `inspect` support `--inspect-target`. Keygen reports an explicit destination or a runtime filename pattern without generating a key. Issuance loads the selected root/issuer key through its normal permission/parse gate and reports the public signer fingerprint and destination. Delegated issuance also reports `issuer_grant_source` without reading it. Artifact `inspect --inspect-target` reports only the source path without decoding it. Profile identity/remote defaults are unused. Inspection does not validate grant/signing authority, delegation containment, policy, TTL, path aliases or output permissions; normal issuance keeps those checks and publication safeguards.

`anchor credential mint/inspect --inspect-target` reports offline input/output selection without reading PSK or credential payloads. Mint loads the actual issuer through its normal identity-file gate and reports its public fingerprint, optional output file and PSK source (`file` or `inline`), never the PSK value. Normal mint includes the secret credential on stdout even with `--out`; inspection makes this explicit with `credential_stdout_on_execution: true` but emits no credential itself. Credential inspect reports a file path or inline provenance, never inline contents. Profile identity/remote defaults are unused. Inspection does not validate credential content, TTL, bootstrap URL, trust-domain match or output permissions.

Capability `show/query/nodes`, subnet `show/ls/tree` and gateway `stats/exports` support `--local --inspect-target`. Inspection reports `mode: temporary_supervisor`, `supervisor_node_id` and the actual configured identity fingerprint (`--identity` > profile identity), or an unavailable identity when execution would generate one. It validates the profile endpoint but does not start a supervisor or generate an ephemeral identity. Remote profile target/bind defaults are ignored and disclosed, not connected to. `--local` remains required; `--node` names the temporary supervisor, not a remote deployment. Inspection does not query topology or capability state, validate all filters or approve authority.

The remaining temporary-supervisor commands—snapshot get/status, audit recent/stream, log/failures tail, peer/daemon/channel reads, aggregator inspect, and admin/ICE operations—also accept `--local --inspect-target`. Streams exit after this one-shot report. Admin/ICE report `identity_required: true` and an unavailable identity when none is configured; inspection does not simulate, prompt or commit. `--inspect-target` conflicts with their `--dry-run`, whose existing behavior is unchanged. Temporary read views report `identity_required: false`; local aggregator listing uses the same view.

With the optional `keychain` feature, `forwarding set-value <ref> --inspect-target` validates the ref name and reports `keychain_service`, `keychain_account` and stdin source provenance without reading stdin or accessing the OS store. This is destination selection, not proof that the keychain is available or durable. The flag is unavailable for this verb in builds without `keychain`. Inspection remains command-specific, not a global flag.

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
