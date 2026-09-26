---
title: CLI Reference
description: "The net-mesh binary exposes the substrate's operator surface."
---
# CLI Reference

For a runnable two-node loopback publisher/consumer example, see the
[CLI capability journey](https://github.com/ai-2070/net/blob/master/net/crates/net/cli/tests/fixtures/README.md).
`wrap --listen` starts the first publisher without a peer, defaults to loopback,
and requires a PSK and identity. Its `wrapped` event includes live connection
details for a consumer; local pins do not override provider authorization.
The guide also exercises native typed calls and generated Python consumption,
then reproduces Python/TypeScript artifacts offline after provider shutdown.
The contract example uses a public test service; a separate protected native
leg proves same-org denial-before-effect and authorized native/generated Python
calls through a Rust SDK fixture adapter, using an offline contract.

The `net-mesh` binary provides capability hosting/consumption, typed contract generation, local stores, and offline authority tools. `wrap` hosts a stdio MCP server as mesh capabilities; `mcp serve` bridges mesh capabilities to a local MCP client. `daemon` only lists a temporary snapshot: there is no `daemon run` command.

Managed nodes are a separate surface: `up` runs one long-lived node per profile in the foreground, `down` drains and stops exactly that node, and `node status` reports it. This is not the temporary supervisor below; see [Managed nodes, join links and leave](#managed-nodes-join-links-and-leave).

The `net-mesh` binary is produced by the `net-cli` crate (kept separate so library consumers don't pay the `clap` build cost). Install it with `cargo install net-cli`, or build from source with `cargo build --release -p net-cli` and run from `target/release/net-mesh`.

Execution scope is command-specific. Identity/org/subnet issuance, capability announcement artifacts, and saved typegen input are offline. NetDB, MCP pins, forwarding policy, and staged transfers use local persistent files. Transfer receive/admin, live typegen, and remote aggregator operations use explicit mesh attachment; mesh attachment does not make the Deck client remote.

For admin/ICE, snapshot, audit/log/failures, peer/daemon listings, capability reads, subnet topology reads, gateway/channel reads, and local aggregator inspection: **Starts a temporary supervisor for this command; does not inspect a running node.** These operations require `--local` and disclose scope on stderr, including with `--quiet`. Admin `--dry-run` remains an offline preview without this requirement; ICE simulation requires it. Gateway export is unsupported. Profile `endpoint` accepts only `in-process`; it does not provide remote Deck attachment. For `aggregator ls`, a complete remote target from flags or profile selects remote RPC; combining `--local` with explicit remote targeting is refused. Commands that address this profile's managed node — `invite *`, `channel serve/status/publish/leave`, `subnet join/leave/activate`, `org join/leave/members` and `node status` — talk to the running `up` node through its authenticated local control endpoint (`--state-dir`), not a temporary supervisor.

Migration: a script that previously ran `net-mesh peer ls` must use `net-mesh peer ls --local` only if it intentionally wants a fresh development snapshot. The old invocation now fails with exit 2 and no result payload. No remote Deck alternative is implied. Offline issuance, persistent stores, and real remote clients keep their existing syntax and scope.

Explicit `--timeout <duration>` currently bounds remote aggregator `ls/query/spawn/scale`, transfer `ls/status/cancel/recv-blob`, live typegen `generate/snapshot` acquisition, and `mcp serve`/`wrap` startup with one absolute budget across dispatch/configuration, connection and RPC/discovery work. Typegen rendering and output writes run outside cancellation after acquisition succeeds; wall-clock completion can exceed the budget. Zero budget refuses before execution; expiry exits 7 without success output. Remote effects may already have committed; the CLI does not retry the operation. Omission preserves existing command/SDK limits, not the formerly advertised but unenforced global `30s` default. Other commands, including transfer directory receive/staging, offline typegen, local modes and target inspection, reject explicit timeouts before effects; other service startup and remote commands remain follow-up work.

For `wrap`, startup also includes child MCP initialization, tool discovery and publication. Startup timeout drops the managed direct child and emits no `wrapped` event. Once published, the provider and subsequent refreshes outlive the startup budget. This is not a process-tree supervisor for arbitrary descendants launched by the wrapped program.

For `mcp serve`, the budget ends when the shim is ready to process protocol input. It covers identity loading and mesh attachment, not waiting for the client's initialize request or the running session. Startup failure emits no protocol output; a started shim continues until EOF or operator shutdown.

Blob receive applies the same deadline to attachment and each network wait. Disk writes count toward elapsed budget but are not cancelled; final rename proceeds normally after the complete stream is acquired. Timeout leaves the destination unchanged but may leave `<out>.partial` staging bytes. This is not a resumable download or a guarantee that total wall-clock time stays within the budget.

One-shot output defaults to table on a TTY and JSON otherwise; streams default to text/NDJSON. ICE commits emit one `{preview, commit}` result; the pre-confirmation preview goes to stderr, and failure/refusal emits no success payload on stdout. `--yes` skips the ICE prompt in TTY and non-TTY use without bypassing signature or policy checks. Without it, TTY use requires typed `YES` and unattended commits exit 8. Dry-run retains its preview-only shape without confirmation. `mcp serve` stdout is protocol traffic, not ordinary command JSON.

ICE script migration: replace `jq -s '.[1].commit_id'` with `jq '.commit.commit_id'`. Successful simulation data is under `.preview`; dry-run still exposes `.blast_hash` at the top level. Stderr previews are not commit receipts.

`--no-color` is global. `$NO_COLOR` is honored per [the convention](https://no-color.org): color is disabled when the variable is **present and non-empty**, whatever its value — `NO_COLOR=1`, `NO_COLOR=x`, and `NO_COLOR=false` all disable it, and only absent or empty leaves it on.

## Managed nodes, join links and leave

`up` runs one long-lived node per profile in the foreground, and `down`
drains and stops exactly that node. `node status` reports it, verified
through its lifetime lock and authenticated control endpoint. The mesh PSK
is generated and kept on first start, or supplied with `--psk-from
file:<path>` or `stdin`, never on the command line.

With `up --enroll` the node hands out `netmesh-join_` links (bearer secrets
unless bound with `--for`). A clean device runs `join <token>` and then
`up`. It attaches direct first and falls back to a relay started with
`relay serve`. Once attached through the relay, it moves to the direct
path by itself when that path starts answering. The relay serves UDP and
TCP on one port. A node whose UDP to the relay gets no answer falls back to
a plain TCP tunnel on that port, reported as `relay_transport: tcp` and
`attach_path: relay_tcp`. Binding the relay on 443 covers networks that
allow only that port. The tunnel is not designed to traverse proxies.

| Command | What it does |
|---|---|
| `invite create [--subnet <path>] [--org <org>] [--channel <name> --channel-rights <rights>]` | One link carrying mesh membership plus each named relation, each authorized on its own. |
| `invite inspect / status / revoke / approve / deny` | Offline inspection; the ledger view; operator decisions for `--require-approval` links. |
| `join <token>` / `leave` | Redeem and install a link; leave the whole mesh (local and durable, not revocation). |
| `subnet invite / join / leave / remove / members` | Standalone subnet links; leave one relation; remove one subject (per-verifier attestations); issued vs observed members. |
| `org invite / approve / join / leave / remove / members` | Org links (always approved with the offline org root); leave; remove one member; member standing. |
| `channel issue-grant` | Offline: the channel root delegates publish/subscribe on one channel to the enrolling node (`up --channel-grant`). |
| `channel invite / join` | Standalone channel links for a device already on the mesh (one active credential per channel; a rejoin takes a fresh link). |
| `channel serve / status / publish / leave` | Gate a channel on the running node; credential state, subscribe ACK and publish readiness; one publish through the node's own gate; leave the channel relation. |
| `wrap --joined <dir>` / `mcp serve --joined <dir>` | Run a provider or consumer as the enrolled device. `up` must be stopped. An explicit `--node-addr/--node-pubkey/--node-id` names the peer. |

Every root stays offline:
- `subnet issue-issuer` and `channel issue-grant` delegate bounded issuance
  to the node;
- `org approve` signs memberships;
- `subnet remove` and `org remove` sign floors.

What the reports mean:
- Admission, subscription and publish are reported from the live session
  and the node's own gate, not implied by holding credentials. The node
  re-establishes admission and subscription after every reconnect.
- Removal is reported per named node from its own signed attestation.
- Leaving is recorded first and survives restart. It is not revocation.

The runnable three-participant
[enrollment journey](https://github.com/ai-2070/net/blob/master/net/crates/net/cli/tests/fixtures/enrollment/README.md)
lists every command in order. It is one machine on loopback, not off-host or
NAT evidence.

### The node state directory and `up` flags

Each profile has one state directory (`--state-dir`, default
`<platform data dir>/net-mesh/nodes/<profile>`) holding the node identity
seed, the mesh PSK, the lifetime lock and the control endpoint. Every
command that talks to a running node names it with `--state-dir` — `invite
*`, `channel serve/status/publish/leave`, `subnet join/leave/activate`,
`org join/leave/members`, and `node status` — as given to `up`.

Beyond `--psk-from`, `up` takes `--bind <IP:PORT>` (mesh bind; defaults to
the profile `bind`, else `0.0.0.0:0`) and `--identity <PATH>` (defaults to
the profile identity, else one generated and kept in the state directory).
With `--enroll` it also takes `--public-addr <HOST:PORT>` (the address
signed into tokens by default), `--issuer-identity <PATH>`, `--ledger
<DIR>`, `--no-port-mapping`, `--domain-name <NAME>`, `--relay <HOST:PORT>`
or `--no-relay`, repeatable `--channel-grant <PATH>`, and the subnet issuer
pair `--subnet-issuer-grant <PATH>` / `--subnet-issuer-key <PATH>`.
`--subnet-leaf-ttl` (default `24h`) and `--subnet-generation` tune the
delegated subnet credentials.

`net-mesh enrollment init --issuer-identity <PATH> [--ledger <DIR>]
[--state-dir <DIR>]` prepares an enrollment ledger offline (the ledger
directory must not exist yet), before the first `up --enroll` uses it.

## `net-mesh aggregator ls`

```sh
net-mesh aggregator ls --profile prod --inspect-target --output json
net-mesh aggregator ls --profile prod --output json
net-mesh aggregator ls --profile prod --local --output json
```

`--inspect-target` reports the same resolved target and bind normal execution consumes, without connecting, starting a supervisor/child process, creating output, or generating an identity. Output includes mode, peer address/node ID, public fingerprints (not raw keys), identity availability, bind, field provenance, ignored profile defaults, and `authorization: "not_checked"`. It reads configured files but creates no store or identity. Available on aggregator `ls/query/spawn/scale`, transfer receive/admin, live typegen, `wrap`, `mcp serve`, and feature-gated anchor `ls/stats`; destinations and provider IDs are included where applicable. `mcp serve --inspect-target` emits ordinary one-shot output and exits without starting the MCP protocol. Hosted services report an unavailable identity if none is configured, but still require it to execute.

Flags override corresponding profile fields. A complete profile tuple selects remote list RPC without `--remote`; partial tuples fail. `--local` may override profile remote defaults and explicitly reports that they were ignored. It cannot be combined with explicit remote targeting or binding. Connection failure does not produce a local snapshot.

`--bind <IP:PORT>` overrides profile `bind` on these remote clients. Existing defaults are unchanged: `127.0.0.1:0` for short-lived clients, `0.0.0.0:0` for `wrap`/`mcp serve`. Loopback-to-non-loopback attachment is rejected early; explicitly select a reachable local interface or wildcard (`0.0.0.0:0` for IPv4, `[::]:0` for IPv6). Bind and peer address families must agree. This does not alter admission policy or guarantee firewall/NAT reachability. The automated non-loopback witness uses two participants on one runner, not two computers.

Other inspection support is described below. Commands not listed here do not yet support this surface.

Every dispatched command now validates explicit `--config` / `--profile` selections before work, including offline admin previews and selections from `NET_MESH_CONFIG` / `NET_MESH_PROFILE`. Missing files, unknown profiles, parse failures and permission failures are errors; flags override environment selections. Remove obsolete selectors rather than relying on them being ignored. Commands that do not use configuration still do not load an implicit default just to execute; profile-backed commands allow its absence. Parser-only help/version flags bypass dispatch.

## Inspect local stores and saved typegen input

```sh
net-mesh netdb tasks ls --store ./state --inspect-target --output json
net-mesh netdb restore --store ./state --from ./backup.bin --clear --inspect-target
net-mesh typegen generate --language ts --from-snapshot ./tools.json \
  --out ./generated --inspect-target
```

All NetDB verbs support `--inspect-target`. The result identifies `mode: persistent_store`, the resolved store (`--store` > profile `netdb` > data-directory default), and snapshot source/destination where applicable. Normal dispatch uses the inspected store selection. Saved typegen reports `mode: offline`, source and destination, and rejects explicit remote target/bind flags.

These local operations report unused remote defaults, `identity.state: unused`, null remote target/bind, and `authorization: not_checked`. Inspection reads profile configuration but neither signing identities nor artifact payloads. It does not open, create or clear stores, so nonexistent inputs and outputs can be inspected safely. It is not snapshot/restore preflight, content validation, or a writability guarantee; normal execution retains those checks.

Forwarding policy (`enable/disable/allow/rm/audit`) and MCP pins (`approve/reject/list`) also support inspection. Store selection remains their own per-user default or explicit `--store`/`--pin-store`, not profile `netdb`. Inspection does not load store contents, create locks or modify policy/consent. Keychain-backed `forwarding set-value` does not support it.

Transfer `send-blob/send-dir --inspect-target` reports the source and optional staging store without reading files/stdin, walking directories or creating a store. With `--store` the mode is `persistent_store`; without it, `offline`. For `send-blob -`, source provenance is `stdin`. Staging is not publication or hosting. These inspection paths do not validate mutation arguments or approve authority.

`node adopt --inspect-target` reports certificate/floors paths, the explicit/default authority directory, its three filenames and the public node fingerprint. `--identity` uses the normal identity-file gate, but identifies the adoption subject rather than a signer. Inspection checks skew/entity selection without reading certificate/floors payloads, opening authority state or installing ownership. It does not validate certificates, directory permissions or authorization.

All five `org` verbs support `--inspect-target`. Keygen reports its explicit destination or an unresolved `org-<generated-org-id-prefix>.toml` filename pattern without generating a key. Issuance/grant commands load the explicit org key through the normal permission/parse gate and report the signer fingerprint and destination; discovery grants include `audience_destination` without minting a secret. Profile identity/remote defaults are unused. Grant `--force` refusal and discovery/audience-output pairing still apply. This does not validate all policy, TTL, path aliases or output permissions, or authorize issuance; execution retains its publication safeguards.

Subnet `keygen`, `issue-direct`, `issue-issuer`, `issue-delegated`, all four `issue-control-fact` subcommands and artifact `inspect` accept `--inspect-target`. Keygen reports an explicit destination or unresolved filename pattern without generating a key. Issuance loads the actual root/issuer key through the normal permission/parse gate, reporting its public fingerprint and output path; delegated issuance reports `issuer_grant_source` without reading it. Artifact inspection reports the source path without decoding the artifact. Profile identity/remote defaults are unused. This does not establish signing authority, valid delegation, policy, TTL, path-alias safety or output permissions; execution retains those checks.

`anchor credential mint/inspect --inspect-target` reports offline selection without reading PSK or credential payloads. Mint loads the explicit issuer through the normal identity-file gate and reports its fingerprint, optional output file and PSK source kind/path, never a PSK value. `credential_stdout_on_execution: true` warns that normal mint prints the secret credential even with `--out`; inspection does not mint or print one. Credential inspect reports file or inline provenance without inline contents. Profile identity/remote defaults are unused. Content, TTL, URL, trust-domain and output-permission validation remain execution checks.

Capability `show/query/nodes`, subnet `show/ls/tree` and gateway `stats/exports` accept `--local --inspect-target`. They report temporary-supervisor mode, `supervisor_node_id` and identity selection (`--identity` > profile), without starting a supervisor. Configured keys use the normal loader; an ephemeral identity is reported unavailable rather than generated. The profile endpoint is validated; remote target/bind defaults are ignored and disclosed. `--local` remains mandatory. This inspects context selection, not deployment state, query results or authority.

Snapshot get/status, audit recent/stream, log/failures tail, peer/daemon/channel reads, aggregator inspect and admin/ICE operations also support `--local --inspect-target`. Inspection exits before streaming, simulation, prompts or commits. Admin/ICE report that execution requires a configured identity; inspection does not generate one. Their `--inspect-target` and `--dry-run` flags conflict. All temporary context reports include `identity_required` and `supervisor_node_id`, including local aggregator listing.

In a `keychain` build, `forwarding set-value <ref> --inspect-target` reports the validated account name, default keychain service and stdin source without reading a value or accessing the OS store. It does not establish backend availability or persistence. Builds without that feature reject this inspection flag. Inspection is command-specific, not global.

## Inspect identity and announcement artifacts

Identity `generate/show/fingerprint/revoke` supports `--inspect-target`. Generate reports an unavailable identity and the explicit output path, or a `destination_pattern` containing `<generated-operator-id>` when the filename depends on the not-yet-generated identity. The pattern is not a concrete writable destination. Show/fingerprint report the source path without reading the file or computing a subject fingerprint. Revoke reports the actual explicit/default revocation store and a public issuer fingerprint without opening the store or raising floors; inspection does not establish enforcement or propagation.

`cap announce --inspect-target` loads the explicit signing key through the normal permission/parse gate, reports its public fingerprint and the file/stdout destination, and exits before signing or writing an announcement. It does not use profile identity or remote defaults. An incompatible `--node-id` is refused before either inspection or signing. Other tag/policy validation remains part of execution; target inspection is not publication or authority approval. Ordinary output formats and overwrite/revocation safeguards are unchanged.

## Admit browsers for your games

`anchor serve --game <id>` turns a standalone anchor into one browser players can join: it serves enrollment and issues an **anonymous credential per visitor** at `POST <url>/credential` with the body `{"game": "<id>"}`. The reply is `{credentialB64, bootstrapUrl, game}` — what `@net-mesh/browser`'s `openSession()` takes; `requestCredential({ anchorUrl, game })` makes the request for a page.

```sh
net-mesh anchor serve --psk-file psk.hex --url https://anchor.example.com \
  --tls-cert cert.pem --tls-key key.pem --allow-origin https://game.example.com \
  --issuer-identity issuer.json --game my-game --game other-game:120
```

- **One anchor, several games.** Repeat `--game`. Each game has its own enrollment root, derived from the `--issuer-identity` key, so every instance started with the same key file admits the same visitors, and a visitor's grant says which game admitted it. Ids are lowercase letters, digits, `.`, `-` and `_`.
- **Limits.** Issuance is capped per game (`--game ID:N`, default 600 credentials a minute) and per source IP (`--credentials-per-minute`, default 30). Refusals are typed: `unknown_game` (404), `rate_limited` (429), `malformed_request` (400).
- **Counters.** `--game-stats-secs N` prints every game's credentials issued and refused and enrollments admitted and refused as a JSON line every N seconds. The start report lists `games` and the `credential_endpoint`.
- **What a credential is.** Its invite binds to the first browser identity that enrolls with it; that identity may reconnect with it for 12 hours (a reload with `rememberedIdentity()`, a promoted leader tab), and any other is refused. One credential is one player.
- **Proved end to end** by `examples/anchor-acceptance`: two real browsers join a lobby through this command.
- **Not yet enforced:** that a game's players cannot announce, discover or route into another game on the same anchor. Run one anchor per game until that lands.

`--credential-issuer` becomes optional with `--issuer-identity` (it is that key's public half); given both, they must agree. `--game` requires `--issuer-identity`.

## Inspect standalone anchor serving

With `rtc-bootstrap`, add `--inspect-target` to an otherwise configured `anchor serve` invocation. It reports mesh/HTTPS/RTC/STUN bind selections, TLS certificate/key paths or ACME cache/challenge settings, and a public credential-issuer fingerprint. No PSK/TLS file is read, socket opened, certificate ordered or identity generated. Profile configuration is read for selection validation/disclosure; profile identity, remote target and bind defaults remain unused by standalone serving.

The same resolved values drive execution. Existing defaults remain mesh `0.0.0.0:0`, HTTPS `0.0.0.0:8443`, RTC on the mesh IP with an ephemeral port, and no second STUN socket. The identity remains ephemeral and is reported unavailable during inspection. Port `0` means runtime allocation, not an observed bound endpoint; null advertised addresses with runtime provenance are unresolved until startup. Inspection is not TLS/content validation, authorization approval or proof of reachability. Malformed listener addresses and a STUN public override without its bind fail before mesh startup.

## `net-mesh transfer`

Seven verbs. Receive and administration commands create a mesh client and require a remote target: `--node-addr <IP:PORT> --node-pubkey <HEX> --node-id <N> --psk-hex <HEX>`, with corresponding profile defaults supported. `--from <NODE>` selects a content holder other than the handshaken target; it is not a relay flag. `--node` names the temporary local supervisor, not the remote provider.

Progress appears on stderr only for human output with an interactive stderr; `--quiet` suppresses it. Sized blob fetches use a byte bar, unknown-size fetches and directory fetches use a spinner. Use global `--output json` for structured results.

### `recv-blob`

Fetch verified chunks into a file:

```sh
net-mesh transfer recv-blob --blob-ref <REF> --out <PATH> [--from <NODE>] [REMOTE FLAGS]
```

Writes go to `<PATH>.partial`, then flush/close and rename to the final path. A failed fetch leaves the previous final file untouched and may leave the partial file for inspection. Local I/O failures and SDK failures need not have the same exit code.

### `send-blob`

Compute a reference, optionally staging bytes in a local persistent store:

```sh
net-mesh transfer send-blob <PATH> [--store <DIR>]
```

Use `-` for stdin. Without `--store`, no content is persisted. With it, chunks are written locally as they are hashed. The process exits: it neither pushes to a peer nor keeps a holder serving that directory. Another running holder is required for remote retrieval.

In JSON mode stdout is one object containing `blob_ref`, `size`, `chunks`, and optional `staged_to` metadata, not a bare reference followed by another JSON line. `hash` is present **only for single-chunk content** — a chunked blob omits it — so extract `blob_ref` with a JSON parser rather than keying on `hash`.

### `recv-dir`

Materialize a directory from its manifest:

```sh
net-mesh transfer recv-dir --remote-ref <REF> --out <PATH> [--from <NODE>] \
                           [--concurrency <N>] [REMOTE FLAGS]
```

The SDK builds a temporary directory and renames it into place on success. `--concurrency 0` selects the SDK default. There is no `--dest` or `--inflight-budget-bytes` CLI option.

### `send-dir`

Compute a manifest reference, optionally staging the manifest and content locally:

```sh
net-mesh transfer send-dir <PATH> [--store <DIR>]
```

This is local preparation, not publication or hosting. There is no `--exclude` option. JSON output contains `remote_ref`, `manifest_size`, and optional `staged_to`.

### `ls`

List the remote target's requester-side pending fetches, not transfers it is serving or a completed-transfer history:

```sh
net-mesh transfer ls --output json [REMOTE FLAGS]
```

### `status`

Inspect a requester-side transfer on the target:

```sh
net-mesh transfer status <TRANSFER-ID> [REMOTE FLAGS]
```

An unknown ID reports `found: false` with exit 0. Do not assume throughput/history fields.

### `cancel`

Request cancellation on the target:

```sh
net-mesh transfer cancel <TRANSFER-ID> [REMOTE FLAGS]
```

The response reports whether cancellation occurred. `cancelled: false` is a successful query outcome, not an error exit; this command does not promise retention in a transfer history.

## `net-mesh typegen`

Code generation from live-discovered tool descriptors or a saved snapshot. Live acquisition fetches missing input schemas from the exact advertising provider via `tool.metadata.fetch`; it also fetches missing output schemas when that provider advertises the metadata service. Output schemas remain optional. Returned ID/version/tags and already-inline schemas must agree with the advertisement. Conflicting selected advertisements fail; identical replicas choose the lowest node ID without fallback or CLI retry. Missing/unusable input or failed hydration rejects the operation before writing output. Offline generation retains its legacy warning-and-skip behavior for incomplete or unsupported snapshots.

The remaining explicit `--timeout` budget covers all metadata fetches, not a new budget per tool. Without that flag, post-attachment acquisition has a single 30-second limit (discovery still at most five seconds); SDK RPC limits also apply. Snapshot v1 is unchanged. Updated native peers negotiate responses up to 1 MiB including encoded status and headers, without increasing the 8 KiB packet limit; roughly 22 KB live contracts and subsequent offline regeneration are tested. Incomplete responses never become successful metadata. Older providers may still refuse responses above the single-packet limit. Over-limit responses return an explicit RPC size error; the handler may have completed, and the CLI does not retry. Final file publication is outside cancellation and is not crash-atomic.

### `generate`

Generate bindings for one or more discovered tools.

```
net-mesh typegen generate --language <LANG> [--out <PATH>] [SELECTOR]
```

| Argument | Description |
|---|---|
| `--language <LANG>` | Output language: `ts` or `python` |
| `--out <PATH>` | Output directory (default `./generated`) |
| `--tag <TAG>` | Repeatable — include a tool if *any* of its tags match (e.g. `--tag weather --tag location`) |
| `--tool <TOOL_ID>` | Repeatable — include a tool by exact id (e.g. `--tool acme/web-search`) |
| `--from-snapshot <PATH>` | Regenerate from a saved snapshot instead of querying the mesh |
| `--node <ID>` | Local supervisor node label, not remote provider selection |

Selectors match ANY within tags and ANY within tool IDs, but both groups must match when both are supplied. With neither, all observed descriptors are selected, subject to supported schemas. Live discovery takes remote-attach flags (`--node-addr`, `--node-pubkey`, `--node-id`, `--psk-hex`), each defaultable in the profile; `--from-snapshot` needs none. Live discovery waits up to five seconds for every explicit tool ID to pass both filters; missing IDs exit 7 before writing snapshot/generated output. Without explicit IDs it observes the full five-second window, not a complete mesh inventory. The remaining global `--timeout` budget can shorten, but not extend, that observation. Offline snapshot filtering remains a subset operation.

Output is one module per tool. The tool's JSON Schema lowers to TypeScript interfaces (for `ts`) or Pydantic v2 models (for `python`); each module also exports:

- A typed call helper: `callAcmeWebSearch(mesh, request)` for TS, `call_acme_web_search(mesh, request)` for Python.
- A `…Meta` constant carrying the descriptor metadata: tool id, version, description, streaming flag, stateless flag, estimated time, tags. TypeScript only — generated Python exports no `…Meta` constant (its per-tool `__init__.py` exports the models, `call_*`, `TOOL_ID`, `VERSION`); its metadata surface is the package's `_meta.json`.

TypeScript output includes per-tool `.ts` modules, an index, and `meta.json`; its call helpers use a structural client interface, without importing a runtime SDK package. Python output includes models, `.pyi` stubs, call helpers, package initializers, and `_meta.json`; models require Pydantic v2 and helpers accept a structural client protocol.

### `snapshot`

Capture the current matching descriptor set into a versioned snapshot file.

```
net-mesh typegen snapshot --out <PATH> [SELECTOR]
```

Selectors (`--tag`, `--tool`) match `generate`. The snapshot is a JSON file with a `format_version`, a `captured_at` timestamp, the `source_query` (which selectors were used), and the captured `descriptors`. Snapshots are stable across substrate releases within the same `format_version`.

### `diff`

Show what changed between two snapshots.

```
net-mesh typegen diff --from <PATH> --to <PATH> [--exit-code]
```

Output lists added tools, removed tools, version bumps, and schema deltas (added/removed/changed fields on requests and responses), with `[BREAKING]` markers. By default the command exits `0`; pass `--exit-code` to exit `14` when any BREAKING change is detected (for gating CI). The structured report is available under `--output json` / `yaml`.

## `net-mesh org`

Offline authoring of organization capability-auth credentials against an org root key, plus the link, removal and standing verbs. The authoring ceremonies — `keygen`, `issue-cert`, `issue-floors`, `grant-dispatcher`, `grant-capability`, `audience-keygen` — need no live node and do not connect to the mesh. The link/standing verbs — `approve`, `invite`, `join`, `remove`, `leave`, `members` — talk to a running node through its control endpoint (`--state-dir`). The conceptual model is in [Organizations](/docs/concepts/organizations); the end-to-end flow is in [Private capabilities](/docs/guides/private-capabilities).

### `keygen`

Generate a fresh org root keypair. This is the key everything else is signed with; it belongs offline, never on a node.

```
net-mesh org keygen [--out <PATH>] [--note <TEXT>] [--force]
```

Defaults to `$XDG_CONFIG_HOME/net-mesh/orgs/org-<id>.toml`. If the platform config directory cannot be resolved the command **refuses** rather than falling back to the working directory — this file holds a private key, and silently writing it wherever the operator happened to be standing (a git checkout, a CI workspace) is the failure mode worth an error message.

### `issue-cert`

Issue a membership certificate: "this node belongs to this org."

```
net-mesh org issue-cert --org-key <PATH> --member <HEX> --out <PATH>
                        [--generation <N>] [--ttl-secs <N>] [--force]
```

`--member` is a 32-byte ed25519 public key as 64 hex chars (a leading `0x` is accepted). TTL defaults to the recommended ~1 year and is hard-capped at 2 years — rejected at issue *and* at every verifier. `--generation` stamps a revocation generation into the certificate; issue at a generation at or above the org's current floor for that member.

### `issue-floors`

Issue a signed revocation-floor bundle. Every certificate issued to a listed member below its floor generation is revoked.

```
net-mesh org issue-floors --org-key <PATH> --floor <MEMBER=GEN> [--floor …] --out <PATH>
```

`--floor` is repeatable and required. Nodes merge bundles **monotonically**: a lower floor never rolls back a higher one, including across a restart. This is the revocation mechanism — v1 renewal is re-issue plus a raised floor, not extension in place.

### `grant-dispatcher`

Issue a dispatcher grant: "this entity may act **for** this org," over one capability or all of them.

```
net-mesh org grant-dispatcher --org-key <PATH> --dispatcher <HEX> --out <PATH>
                              (--capability <TAG> | --any-capability) [--ttl-secs <N>]
```

Signed by the org the dispatcher acts for. The caller carries it inside the per-call admission proof. Holding one is never invocation authority on its own.

### `grant-capability`

Issue a capability grant: "org A holds these rights on this capability over this target," signed by the *provider* org.

```
net-mesh org grant-capability --org-key <PATH> --grantee-org <HEX> --capability <TAG> --out <PATH>
                              (--invoke | --discover --audience-out <PATH>)
                              (--target-node <HEX> | --target-any-owned-by <HEX>) [--ttl-secs <N>]
```

`--discover` mints a fresh audience secret and **requires** `--audience-out`; only the secret's 32-byte commitment rides inside the signed grant, so the raw discovery key never touches the wire. At least one of `--invoke` or `--discover` is required (the parenthesized group marks it); both may be granted together.

Both grant commands default to a 7-day TTL, hard-capped at 30 days and rejected at issue and at every verifier.

Three behaviors of these two commands surprise people:

- **`--force` is refused.** Grant artifacts are published no-clobber. The grant and its audience secret are written as a pair and the write is not crash-atomic, and on a case-insensitive filesystem an aliased `--out` (`ORG.TOML` vs `org.toml`) could destroy the org key itself. Write to fresh paths, or remove the old files explicitly. (`keygen`, `issue-cert`, and `issue-floors` do accept `--force`.)
- **On Windows the audience secret's 0600 mode is unenforceable.** The file inherits its parent directory's NTFS DACL, and a loud warning fires unless you pass `--accept-windows-dacl`. Point `--audience-out` at an owner-only parent directory.
- **`--accept-windows-dacl` and `--insecure-permissions` are separate flags on purpose.** The first suppresses a warning about a freshly written *output* secret; the second relaxes a mode check on an *input* you already control, such as an org key checked out of git at 0644. They were one flag once, and operators who added it on Linux carried it to Windows and silently killed the only warning that platform has.

### `audience-keygen`

Mint the org's shared owner audience once — the key every member uses to open (and be found in) the org's private announcements.

```
net-mesh org audience-keygen --org-key <PATH> --out <PATH>
```

Written owner-only and kept with the org root. `org approve --audience` and `node adopt --audience` hand it to members; without it a member's audience stays node-local.

### `invite` / `join` / `approve`

A standalone org link carries org membership only, for a device already on the mesh.

```
net-mesh org invite <ORG> --state-dir <DIR> [--ttl <DURATION>] [--for <ENTITY>] [--out <PATH>]
net-mesh org join <TOKEN> --state-dir <DIR> [--yes]
net-mesh org approve <OFFER-ID> --subject <ENTITY> --org-key <PATH> --state-dir <DIR> \
                    [--generation <N>] [--audience <PATH>] [--ttl-secs <N>]
```

Org links are **always approval-gated**: until the operator runs `org approve`, the joining device's node keeps asking by itself, and once the membership is signed (with the offline org root) the device adopts it and installs it live. `--generation` re-admits a member after a revocation floor; `--audience` delivers the shared owner audience; `--ttl-secs` sets the certificate lifetime.

### `remove` / `members`

```
net-mesh org remove <MEMBER> --org-key <PATH> --minimum-generation <N> \
                    --verifier <NODE> [--verifier …] [--state-dir <DIR>] [--dry-run]
net-mesh org members <ORG> --state-dir <DIR> \
                    [--verifier <NODE> --org-key <PATH> …]
```

`remove` signs a floor here with the offline org root — every membership certificate of the member below `--minimum-generation` is revoked — and has each named verifier (`self`, or `ENTITY_HEX@HOST:PORT#NOISE_PUBKEY_HEX`) apply it. Each verifier's own signed attestation is reported, and `complete` holds only when every named verifier persisted the floor; unnamed nodes are never assumed. `members` reports what the node of `--state-dir` issued for the org and each member's standing against its own floors — explicitly not a global roster or a claim about activity.

### `leave`

```
net-mesh org leave --state-dir <DIR>
```

Records the departure durably, then stops the running node; its next `up` runs on the mesh without the org. Local only: the org still accepts the certificate until `org remove`, and leaving is not revocation. Rejoining takes a new link approved with the org root.

## `net-mesh subnet`

`show`, `ls`, and `tree` read a temporary supervisor's topology view. **Starts a temporary supervisor for this command; does not inspect a running node.** The issuance commands below author subnet authority offline: signed credentials and control facts for protected attachment, routing, and export. Signed artifacts use framed **canonical wire bytes**, not a JSON mirror. `inspect` decodes an artifact; it does not verify its signature. The V3 link and membership verbs below (`invite`, `join`, `leave`, `remove`, `members`, `activate`) act on a running node through its control endpoint instead.

### `keygen`

Generate a subnet authority keypair — usable as an authority root or as a delegated issuer. It signs grants, issuer grants, revocation floors, and control facts; it belongs offline.

```
net-mesh subnet keygen [--out <PATH>] [--note <TEXT>] [--force]
```

Defaults to `$XDG_CONFIG_HOME/net-mesh/subnets/subnet-<id>.toml`, written owner-only, and refuses rather than falling back to the working directory when the config directory cannot be resolved. The summary prints the public entity id — never the seed. `--force` replaces a subnet key only; it always refuses to replace a different kind of secret (an org key, an operator identity), however the path is spelled.

### `issue-direct`

Issue one direct credential set: authority root → subject.

```
net-mesh subnet issue-direct --root-key <PATH> --authority <HEX> --subject <HEX>
                             --scope <PATH|global> --rights <attach,route,export>
                             --out <PATH> [--topology-epoch <N>] [--generation <N>]
                             [--not-before <UNIX>] [--ttl-secs <N>] [--force]
```

`--authority` is explicit on purpose: an authority may trust several roots, so the id is never silently derived from the signing key. `--scope global` is the **whole-authority root scope** — it covers every present and future path under the authority, and is never an "unscoped" default. Rights are a comma-separated subset of `attach`, `route`, `export`; anything else is refused. TTL defaults to 7 days, hard-capped at 30 by the core — rejected at issue *and* at every verifier.

### `issue-issuer` and `issue-delegated`

One provisioning hop, structurally: the root signs a bounded issuer grant, and only that issuer signs leaves — there is no depth flag and no second hop.

```
net-mesh subnet issue-issuer   --root-key <PATH> --authority <HEX> --issuer <HEX>
                               --scope <PATH|global> --max-rights <…> --out <PATH> [.]
net-mesh subnet issue-delegated --issuer-grant <PATH> --issuer-key <PATH> --subject <HEX>
                               --scope <PATH|global> --rights <…> --out <PATH> [.]
```

`issue-delegated` writes **one complete framed credential set** containing both the issuer grant and the leaf. A leaf scope escaping the issuer scope or rights exceeding the issuer maximum are refused up front with the core's own predicates — and re-checked by every verifier regardless.

:::caution[Issuance validates structure and attenuation, not root authenticity]
`issue-delegated` checks that the leaf stays inside the issuer grant it was handed. It does **not** verify that grant's signature against a trusted authority root. Successful issuance is not proof of deployability. `net-mesh subnet inspect` lets you inspect decoded fields and authority IDs, but cannot authenticate them; signature verification against the trusted authority is a separate requirement.
:::

### `issue-control-fact`

Author one signed control fact, written as the outer `SubnetControlFact` frame a node's `apply` door consumes.

```
net-mesh subnet issue-control-fact <descriptor|gateway-advertisement|export-policy|revocation-floor>
                                   --root-key <PATH> --authority <HEX> --scope <PATH|global>
                                   --topology-epoch <N> --revision <N> --out <PATH> [kind-specific flags]
```

The topology epoch is **explicit**: a fact never invents authority movement — reparenting is an operator decision recorded by a new epoch. `revocation-floor` takes `--minimum-generation`; `gateway-advertisement` takes `--gateway`/`--gateway-node`; `export-policy` takes repeatable `--channel` (a canonical channel *name*, or exactly lowercase `0x` + 16 lowercase hex digits — other hex-looking forms are refused, since a shortened value is indistinguishable from the collidable 16-bit wire hint).

### `inspect`

Decode and summarize any subnet artifact file — credential set, issuer grant, or control fact — without private material. Malformed or non-canonical bytes exit non-zero. Pointing it at a key file is refused, and no output path ever renders a seed.

```
net-mesh subnet inspect <FILE>
```

### `remove` / `members`

```
net-mesh subnet remove --root-key <PATH> --authority <HEX> --scope <PATH> \
                       --topology-epoch <N> --revision <N> --subject <HEX> \
                       --minimum-generation <N> --verifier <CONTACT> [--verifier …] \
                       [--rights <attach>] [--state-dir <DIR>] [--dry-run]
net-mesh subnet members <SCOPE> --state-dir <DIR> \
                        [--verifier <NODE> --root-key <PATH> --authority <HEX> …]
```

`remove` signs a subject floor with the offline root and hands it to each named verifier (`self`, or a contact), reporting each verifier's own signed attestation; `complete` holds only when every named verifier persisted the floor, and unnamed ones are never assumed. `members` reports what the node of `--state-dir` issued for the scope and the peers admitted to it there right now — not a global roster.

### `invite` / `join` / `leave` / `activate`

```
net-mesh subnet invite <SCOPE> --state-dir <DIR> [--rights <RIGHTS>] [--require-approval]
net-mesh subnet join <TOKEN> --state-dir <DIR> [--yes] [--switch]
net-mesh subnet leave <SCOPE> --state-dir <DIR>
net-mesh subnet activate <SCOPE> --state-dir <DIR>
```

A standalone subnet link carries the subnet relation only, for a device already on the mesh; the device redeems it over its own session with the node it enrolled with (no PSK is delivered), and the verifier's verdict is reported. `--require-approval` holds issuance until `invite approve`. A device may hold several subnet relations, but only **one active attachment per verifier** is presented: joining a second scope at the same verifier is refused unless `subnet join --switch` is given, and `subnet activate <scope>` switches explicitly — the previous attachment is withdrawn there and stays stored. `leave` records the departure durably (the relation is never presented or renewed again) and asks the verifier to drop the admission; the credential is not revoked.

## `net-mesh node`

### `status`

Report the state of this profile's `net-mesh up` node.

```
net-mesh node status --state-dir <DIR>
```

Liveness comes from the node's lifetime lock and a reply from its authenticated control endpoint — never from a PID or a file's mere presence; a control file without a held lock is reported as stale metadata. The report carries the node's id, entity, public key, bind, PSK **source** (never the PSK), and trust domain; the enrollment endpoint and issuer when it serves enrollment; a joined device's live link and current subnet-leaf expiry; and the adopted org, with `org_state` when a membership is not installed (revoked or invalid).

### `adopt`

Install org ownership on a node. This is the one org-adjacent command that writes to a node's authority directory.

```
net-mesh node adopt --cert <PATH> (--identity <PATH> | --entity <HEX>)
                    [--authority-dir <DIR>] [--floors <PATH>] [--audience <PATH>]
                    [--skew-secs <N>] [--insecure-permissions]
```

Adoption writes three separately versioned files — `owner-membership.json`, `owner-audience.key`, and `revocation-state.json` — under `$XDG_CONFIG_HOME/net-mesh/authority` by default. `--floors` optionally merges a revocation-floor bundle (as written by `net-mesh org issue-floors`) during adoption; the merge runs in certificate pre-write validation, so a certificate the resulting floors would immediately revoke never adopts. `--audience` installs the org's shared owner audience (from `org audience-keygen`) instead of a node-local one, so this node can discover other members' private services. `--skew-secs` is the clock-skew tolerance for the certificate window check: **strict by default**, and hard-capped at the token module's 300-second ceiling, with larger values rejected before anything is written. `--insecure-permissions` permits a permissive mode on the identity file on Unix, and is only meaningful alongside `--identity`.

Like `keygen`, this command refuses rather than falling back to the working directory when the config directory cannot be resolved — the authority directory holds `owner-audience.key`, the raw owner discovery key.

## `net-mesh channel`

Channel credentials for the running node and the offline root. `visibility <name>` and `ls` read a temporary supervisor's channel registry (**Starts a temporary supervisor for this command; does not inspect a running node.**); every other verb acts on the running node or offline.

### `issue-grant`

Offline, on the operator's machine: the channel root — an operator identity file — signs one delegate grant on one canonical channel to an issuing node.

```
net-mesh channel issue-grant --root-identity <PATH> --issuer <ENTITY> --channel <NAME>
                             --out <PATH> [--rights <publish,subscribe>] [--ttl <DURATION>] [--force]
```

`--issuer` is the enrolling node's full 64-hex `issuer`, reported by `up --enroll` and `node status`. The root never reaches the node. `--rights` (default `publish,subscribe`) bounds what the node may grant onward, and every device credential expires with the grant's `--ttl` (default `30d`).

### `serve` / `status` / `publish` / `leave`

On the running node.

```
net-mesh channel serve <NAME> --token-root <ENTITY> [--token-root …] --state-dir <DIR>
net-mesh channel status --state-dir <DIR>
net-mesh channel publish <NAME> --data <TEXT> --state-dir <DIR>
net-mesh channel leave [<NAME>] --state-dir <DIR>
```

`serve` gates a channel so only chains anchored at the given token root(s) may subscribe; it is persisted and re-registered on every `up`. `status` shows the channels this node serves and, for a joined device, its channel credential — subscribe ACK and publish readiness, never a roster of other members. `publish` sends one payload through the node's own local gate: `gate: passed` means this node's production gate accepted it, `gate: open` means the channel is ungated here (no credential evidence), and a denial carries the gate's reason; delivery counts are this node's sends, not subscriber receipts. `leave` records the departure durably, then acknowledges the unsubscribe and removes exactly the installed publish credential (`publish_stop: confirmed` or `unconfirmed`); it is not revocation.

### `invite` / `join`

Standalone channel links add a channel to a device already on the mesh.

```
net-mesh channel invite <NAME> --rights <RIGHTS> --state-dir <DIR> [--require-approval]
net-mesh channel join <TOKEN> --state-dir <DIR> [--yes]
```

One link carries one channel and its rights (`publish`, `subscribe` or `publish,subscribe`); subscribe sends the device to this node as publisher, so the channel must be served here first. `--require-approval` holds issuance until `invite approve`. A device holds **one active credential per channel**: a second is refused until the active one is left, and a rejoin after `channel leave` takes a fresh link — the spent one stays spent.

## Exit codes

Across all `net-mesh` subcommands:

| Code | Meaning |
|---|---|
| `0` | Success |
| `1` | Generic error |
| `2` | Invalid arguments / parse failure |
| `3` | SDK error (a `net-sdk` operation failed — transfer, query, …) |
| `4` | `net-mesh ice`: simulation blocked |
| `5` | `net-mesh ice`: operator policy rejected |
| `6` | Connection failure (no holder, unreachable peer, session refused) |
| `7` | Timeout |
| `8` | Confirmation refused (a required confirmation was declined) |
| `10` | Reserved daemon-factory error |
| `11` | Reserved MeshDB query parse error |
| `12` | Reserved predicate parse error |
| `13` | `net-mesh ice`: an operator signature failed cryptographic verification |
| `14` | `net-mesh typegen diff --exit-code`: a BREAKING change was detected |

Errors are plain `net-mesh: ...` messages on stderr, not a JSON error-envelope contract. Use exit codes for failure handling.
