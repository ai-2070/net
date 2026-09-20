---
title: CLI Reference
description: "The net-mesh binary exposes the substrate's operator surface."
---
# CLI Reference

The `net-mesh` binary provides capability hosting/consumption, typed contract generation, local stores, and offline authority tools. `wrap` hosts a stdio MCP server as mesh capabilities; `mcp serve` bridges mesh capabilities to a local MCP client. `daemon` only lists a temporary snapshot: there is no `daemon run` command.

The `net-mesh` binary is produced by the `net-cli` crate (kept separate so library consumers don't pay the `clap` build cost). Install it with `cargo install net-cli`, or build from source with `cargo build --release -p net-cli` and run from `target/release/net-mesh`.

Execution scope is command-specific. Identity/org/subnet issuance, capability announcement artifacts, and saved typegen input are offline. NetDB, MCP pins, forwarding policy, and staged transfers use local persistent files. Transfer receive/admin, live typegen, and remote aggregator operations use explicit mesh attachment; mesh attachment does not make the Deck client remote.

For admin/ICE, snapshot, audit/log/failures, peer/daemon listings, capability reads, subnet topology reads, gateway/channel reads, and local aggregator inspection: **Starts a temporary supervisor for this command; does not inspect a running node.** These operations require `--local` and disclose scope on stderr, including with `--quiet`. Admin `--dry-run` remains an offline preview without this requirement; ICE simulation requires it. Gateway export is unsupported. Profile `endpoint` accepts only `in-process`; it does not provide remote Deck attachment. For `aggregator ls`, a complete remote target from flags or profile selects remote RPC; combining `--local` with explicit remote targeting is refused.

Migration: a script that previously ran `net-mesh peer ls` must use `net-mesh peer ls --local` only if it intentionally wants a fresh development snapshot. The old invocation now fails with exit 2 and no result payload. No remote Deck alternative is implied. Offline issuance, persistent stores, and real remote clients keep their existing syntax and scope.

Explicit `--timeout <duration>` currently bounds remote aggregator `ls/query/spawn/scale`, transfer `ls/status/cancel/recv-blob`, live typegen `generate/snapshot` acquisition, and `mcp serve`/`wrap` startup with one absolute budget across dispatch/configuration, connection and RPC/discovery work. Typegen rendering and output writes run outside cancellation after acquisition succeeds; wall-clock completion can exceed the budget. Zero budget refuses before execution; expiry exits 7 without success output. Remote effects may already have committed; the CLI does not retry the operation. Omission preserves existing command/SDK limits, not the formerly advertised but unenforced global `30s` default. Other commands, including transfer directory receive/staging, offline typegen, local modes and target inspection, reject explicit timeouts before effects; other service startup and remote commands remain follow-up work.

For `wrap`, startup also includes child MCP initialization, tool discovery and publication. Startup timeout drops the managed direct child and emits no `wrapped` event. Once published, the provider and subsequent refreshes outlive the startup budget. This is not a process-tree supervisor for arbitrary descendants launched by the wrapped program.

For `mcp serve`, the budget ends when the shim is ready to process protocol input. It covers identity loading and mesh attachment, not waiting for the client's initialize request or the running session. Startup failure emits no protocol output; a started shim continues until EOF or operator shutdown.

Blob receive applies the same deadline to attachment and each network wait. Disk writes count toward elapsed budget but are not cancelled; final rename proceeds normally after the complete stream is acquired. Timeout leaves the destination unchanged but may leave `<out>.partial` staging bytes. This is not a resumable download or a guarantee that total wall-clock time stays within the budget.

One-shot output defaults to table on a TTY and JSON otherwise; streams default to text/NDJSON. ICE commits emit one `{preview, commit}` result; the pre-confirmation preview goes to stderr, and failure/refusal emits no success payload on stdout. `--yes` skips the ICE prompt in TTY and non-TTY use without bypassing signature or policy checks. Without it, TTY use requires typed `YES` and unattended commits exit 8. Dry-run retains its preview-only shape without confirmation. `mcp serve` stdout is protocol traffic, not ordinary command JSON.

ICE script migration: replace `jq -s '.[1].commit_id'` with `jq '.commit.commit_id'`. Successful simulation data is under `.preview`; dry-run still exposes `.blast_hash` at the top level. Stderr previews are not commit receipts.

`--no-color` is global. `$NO_COLOR` is honored per [the convention](https://no-color.org): color is disabled when the variable is **present and non-empty**, whatever its value — `NO_COLOR=1`, `NO_COLOR=x`, and `NO_COLOR=false` all disable it, and only absent or empty leaves it on.

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
net-mesh typegen generate --language ts --from-snapshot ./tools.json --out ./generated --inspect-target
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

In JSON mode stdout is one object containing `blob_ref`, `hash`, `size`, `chunks`, and optional `staged_to` metadata, not a bare reference followed by another JSON line. Extract `blob_ref` with a JSON parser.

### `recv-dir`

Materialize a directory from its manifest:

```sh
net-mesh transfer recv-dir --remote-ref <REF> --out <PATH> [--from <NODE>] [--concurrency <N>] [REMOTE FLAGS]
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
- A `…Meta` constant carrying the descriptor metadata: tool id, version, description, streaming flag, stateless flag, estimated time, tags.

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

Offline authoring of organization capability-auth credentials against an org root key. These commands are ceremonies over files: they need no live node and do not connect to the mesh. The conceptual model is in [Organizations](/docs/concepts/organizations); the end-to-end flow is in [Private capabilities](/docs/guides/private-capabilities).

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
                              [--invoke] [--discover --audience-out <PATH>]
                              (--target-node <HEX> | --target-any-owned-by <HEX>) [--ttl-secs <N>]
```

`--discover` mints a fresh audience secret and **requires** `--audience-out`; only the secret's 32-byte commitment rides inside the signed grant, so the raw discovery key never touches the wire.

Both grant commands default to a 7-day TTL, hard-capped at 30 days and rejected at issue and at every verifier.

Three behaviors of these two commands surprise people:

- **`--force` is refused.** Grant artifacts are published no-clobber. The grant and its audience secret are written as a pair and the write is not crash-atomic, and on a case-insensitive filesystem an aliased `--out` (`ORG.TOML` vs `org.toml`) could destroy the org key itself. Write to fresh paths, or remove the old files explicitly. (`keygen`, `issue-cert`, and `issue-floors` do accept `--force`.)
- **On Windows the audience secret's 0600 mode is unenforceable.** The file inherits its parent directory's NTFS DACL, and a loud warning fires unless you pass `--accept-windows-dacl`. Point `--audience-out` at an owner-only parent directory.
- **`--accept-windows-dacl` and `--insecure-permissions` are separate flags on purpose.** The first suppresses a warning about a freshly written *output* secret; the second relaxes a mode check on an *input* you already control, such as an org key checked out of git at 0644. They were one flag once, and operators who added it on Linux carried it to Windows and silently killed the only warning that platform has.

## `net-mesh subnet`

`show`, `ls`, and `tree` read a temporary supervisor's topology view. **Starts a temporary supervisor for this command; does not inspect a running node.** The issuance commands below author subnet authority offline: signed credentials and control facts for protected attachment, routing, and export. Signed artifacts use framed **canonical wire bytes**, not a JSON mirror. `inspect` decodes an artifact; it does not verify its signature.

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

## `net-mesh node`

### `adopt`

Install org ownership on a node. This is the one org-adjacent command that writes to a node's authority directory.

```
net-mesh node adopt --cert <PATH> (--identity <PATH> | --entity <HEX>)
                    [--authority-dir <DIR>] [--bundle <PATH>] [--skew-secs <N>]
```

Adoption writes three separately versioned files — `owner-membership.json`, `owner-audience.key`, and `revocation-state.json` — under `$XDG_CONFIG_HOME/net-mesh/authority` by default. `--bundle` optionally merges a revocation-floor bundle during adoption. `--skew-secs` is the clock-skew tolerance for the certificate window check: **strict by default**, and hard-capped at the token module's 300-second ceiling, with larger values rejected before anything is written.

Like `keygen`, this command refuses rather than falling back to the working directory when the config directory cannot be resolved — the authority directory holds `owner-audience.key`, the raw owner discovery key.

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
