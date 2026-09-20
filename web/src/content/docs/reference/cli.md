---
title: CLI Reference
description: "The net-mesh binary exposes the substrate's operator surface."
---
# CLI Reference

The `net-mesh` binary provides capability hosting/consumption, typed contract generation, local stores, and offline authority tools. `wrap` hosts a stdio MCP server as mesh capabilities; `mcp serve` bridges mesh capabilities to a local MCP client. `daemon` only lists a temporary snapshot: there is no `daemon run` command.

The `net-mesh` binary is produced by the `net-cli` crate (kept separate so library consumers don't pay the `clap` build cost). Install it with `cargo install net-cli`, or build from source with `cargo build --release -p net-cli` and run from `target/release/net-mesh`.

Execution scope is command-specific. Identity/org/subnet issuance, capability announcement artifacts, and saved typegen input are offline. NetDB, MCP pins, forwarding policy, and staged transfers use local persistent files. Transfer receive/admin, live typegen, and remote aggregator operations use explicit mesh attachment; mesh attachment does not make the Deck client remote.

For admin/ICE, snapshot, audit/log/failures, peer/daemon listings, capability reads, subnet topology reads, gateway/channel reads, and local aggregator inspection: **Starts a temporary supervisor for this command; does not inspect a running node.** Snapshot requires `--local`; the other families do not yet require that opt-in. Admin `--dry-run` is an offline preview. Gateway export is unsupported. Profile `endpoint` accepts only `in-process`; omitting remote flags does not attach these commands to an existing deployment. Profile remote fields alone currently do not select remote `aggregator ls`; use `--remote`.

Global `--timeout` is currently parsed but not forwarded by dispatch: it does not enforce a universal deadline. One-shot output defaults to table on a TTY and JSON otherwise; streams default to text/NDJSON. ICE can emit separate preview and commit JSON values and still prompts for typed `YES` on interactive stdin even with `--yes`. `mcp serve` stdout is protocol traffic, not ordinary command JSON.

`--no-color` is global. `$NO_COLOR` is honored per [the convention](https://no-color.org): color is disabled when the variable is **present and non-empty**, whatever its value — `NO_COLOR=1`, `NO_COLOR=x`, and `NO_COLOR=false` all disable it, and only absent or empty leaves it on.

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

Code generation from live-discovered tool descriptors or a saved snapshot. The current live path uses inline schemas; it does not fetch oversized metadata through `tool.metadata.fetch`. Generation skips missing inline input schemas and unsupported schemas with diagnostics. A successful generation is not proof that every requested tool was captured.

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

Selectors match ANY within tags and ANY within tool IDs, but both groups must match when both are supplied. With neither, all observed descriptors are selected, subject to supported schemas. Live discovery takes remote-attach flags (`--node-addr`, `--node-pubkey`, `--node-id`, `--psk-hex`), each defaultable in the profile; `--from-snapshot` needs none. Live observation currently stops at the first nonempty unfiltered tool list or five seconds, then filters: it is not a complete inventory or a wait for all requested IDs.

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
