# CLI Reference — `net-mesh`

Read this when the user wants to drive Net from the shell: host a capability, connect a consumer, move a blob/directory, generate typed bindings from discovered AI tools, author authority artifacts offline, script the binary, or asks "what commands does `net-mesh` have / which scope does this run in / what do the exit codes mean." The in-context mentions elsewhere — `net-mesh transfer` in `dataforts.md`, `net-mesh typegen` in `nrpc.md`, `net-mesh wrap`/`mcp`/`forwarding` in `mcp.md`, `net-mesh org`/`node adopt` in `org.md`, `net-mesh subnet` in `subnet-auth.md` — point here for the full surface.

---

## Install and packaging

The binary is **`net-mesh`**; the crate is **`net-cli`** (kept separate so library consumers don't pay the `clap` build cost).

```bash
cargo install net-cli                 # crates.io
cargo binstall net-cli                # prebuilt binary
npm install -g @net-mesh/cli          # per-platform binary shim
pip install net-mesh-cli              # maturin-built wheel, bundles the binary
# or from a source checkout, from net/crates/net/:
cargo build --release -p net-cli      # → target/release/net-mesh
```

Environment prefix is `NET_MESH_`; platform config directory is `net-mesh`.

**Feature gates — opt-in, not silently widened.** `webrtc`, `rtc-bootstrap`, and `keychain` are opt-in. `anchor` live verbs (`ls`/`stats`/`serve`) require `rtc-bootstrap`; `forwarding set-value` requires `keychain`. Standard packaging does not include them.

## The execution model — know what a command actually operates on

The central distinction is **what a command runs against**. Most "it looked like it worked" CLI mistakes are mistaking one scope for another.

| Mode | Meaning | Families |
|---|---|---|
| **Offline** | Authors or inspects local files; no live mesh implied | `identity`, `org` issuance, `subnet` issuance/inspect, `cap announce` artifact, `typegen` from a saved snapshot |
| **Persistent local** | Opens a local on-disk store or policy | `netdb`, `mcp` pins, `forwarding` policy, staged `transfer` content |
| **Temporary supervisor** | Starts a **new** `MeshOsDaemonSdk` for this invocation and uses its local Deck client | `admin`, `ice`, `snapshot`, `audit`, `log`/`failures` tail, `peer`/`daemon` ls, `cap` reads, `subnet` topology reads, `gateway`/`channel` reads, local `aggregator` |
| **Mesh client** | Creates a local mesh participant and connects to an **explicitly resolved remote target** | remote `aggregator` (`query`/`spawn`/`scale`/remote `ls`), `transfer` receive/admin, live `typegen` |
| **Hosted service** | Keeps a mesh participant and a service/subprocess alive | `wrap`, `mcp serve`, feature-enabled `anchor serve` |

A local commit is **not** proof of a deployed-node mutation. An empty local snapshot is **not** proof of an empty deployment. A successful mesh handshake is **not** permission to invoke every service.

### The `--local` gate on temporary supervisors

Every temporary-supervisor operation must be **explicitly opted in** and it discloses its scope on stderr — even under `--quiet`:

> **Starts a temporary supervisor for this command; does not inspect a running node.**

```sh
net-mesh snapshot get --local
net-mesh snapshot status --local
net-mesh peer ls --local
net-mesh cap query --local --tag ai-tool
```

- Migration: a script that previously ran `net-mesh peer ls` must add `--local` **only if it intentionally wants a fresh development snapshot**; the old invocation now fails with exit 2 and no result payload. No remote Deck alternative is implied.
- `admin --dry-run` stays an offline preview and needs no opt-in; ICE simulation does require `--local`. `gateway export` remains unsupported.
- Offline issuance, persistent stores, and real remote mesh clients do **not** take `--local`.
- `profile.endpoint` accepts only `in-process`; there is no remote Deck protocol. Real remote clients instead use `node_addr` / `node_pubkey` / `node_id` / `psk_hex` resolved from flags or profile.

## Global flags

Applied to every subcommand; environment fallbacks in brackets:

- `--config <PATH>` `[NET_MESH_CONFIG]` — profile file (default `$XDG_CONFIG_HOME/net-mesh/config.toml`).
- `--profile <NAME>` `[NET_MESH_PROFILE]` — named profile (default `default`).
- `--insecure-config-permissions` `[NET_MESH_INSECURE_CONFIG_PERMISSIONS]` — read a group/world-accessible or other-owned profile.
- `--output (json|yaml|ndjson|table|text)` — one-shot defaults to `table` on a TTY and `json` otherwise; streams default to `text`/NDJSON. Generic table rendering can fall back to JSON.
- `--quiet` / `-q` — suppress progress/logging (**temporary-supervisor scope disclosures remain on stderr**).
- `--verbose` / `-v` — `-v` info, `-vv` debug, `-vvv` trace; `NET_MESH_LOG=` env-filter overrides.
- `--no-color` `[NO_COLOR]` — disable ANSI in table/text output.
- `--timeout <dur>` — bounded deadline (see below).

**Colour.** `$NO_COLOR` is honoured *per the convention*: colour is off when the variable is **present and non-empty**, whatever the value (`NO_COLOR=1`, `NO_COLOR=x`, `NO_COLOR=false` all disable it; only absent or empty leaves it on).

## Config, profiles, and validation

The default profile file is **optional**. Remote commands still need a complete target and credentials.

```toml
[default]
identity        = "~/.config/net-mesh/identity.toml"
endpoint        = "in-process"
default_timeout_ms = 30000

[profiles.prod]
identity    = "~/.config/net-mesh/ops-identity.toml"
node_addr   = "10.0.0.4:7700"
node_id     = "4"                 # TOML string
node_pubkey = "abcd…"             # 64 hex
psk_hex     = "1234…"             # 64 hex
bind        = "0.0.0.0:0"         # explicit IPv4 off-host client bind
```

Before dispatch, every **explicitly selected** config file / profile is validated — including selections from `NET_MESH_CONFIG` / `NET_MESH_PROFILE` and an explicitly named `default`. Missing files, unknown profiles, and malformed/unsafe files fail before effects; flags override environment selections. Commands that do not use configuration never load the implicit default merely to execute, so an absent default config is not an error for them. Parser-only `--help` / `version` bypass dispatch.

## Inspect targeting — resolve before you act

`--inspect-target` is a **command-specific** flag (not global). It resolves the same mode/target/store/bind/identity that dispatch will consume and prints it, **without side effects**: it does not connect, start a supervisor or child process, create/mint a store or identity, read artifact payloads, or claim authorization (`authorization: "not_checked"`).

```sh
net-mesh aggregator ls --profile prod --inspect-target --output json
net-mesh netdb restore --store ./state --from ./backup.bin --clear --inspect-target
net-mesh typegen generate --language ts --from-snapshot ./tools.json --out ./generated --inspect-target
net-mesh wrap journey --listen --inspect-target   # resolves without binding the port
```

It is available on:

- Remote clients: aggregator `ls/query/spawn/scale`, transfer `recv-blob`/`recv-dir` and receive/admin, live `typegen`, `wrap` (including `--listen`), `mcp serve`, feature-gated `anchor ls/stats`.
- Local stores: **all** NetDB verbs, saved `typegen`, `forwarding` policy verbs, `mcp` pins, `transfer send-blob/send-dir`, keychain-backed `forwarding set-value` (keychain builds only).
- Issuance/authoring: identity `generate/show/fingerprint/revoke`, `cap announce`, `node adopt`, all five `org` verbs, all subnet issuance subcommands + `inspect`, `anchor credential mint/inspect`.
- Temporary supervisors: `--local --inspect-target` on capability `show/query/nodes`, subnet `show/ls/tree`, gateway `stats/exports`, snapshot, audit, log/failures tail, peer/daemon/channel reads, aggregator inspect, and admin/ICE.

Reported fields: `mode` (`offline` / `persistent_store` / `temporary_supervisor` / remote), peer address/node ID, public fingerprints (never raw keys/PSKs), identity availability, `bind`, field provenance, ignored profile defaults, and output destination/provider ID where applicable. On admin/ICE it conflicts with `--dry-run`, reports `identity_required: true`, and never prompts or commits.

**It is resolution only, not validation** — not restore preflight, not content/schema validation, not proof a path is writable or an authority is valid. Execution retains its own gates. An unconfigured identity has no fingerprint; hosted services still need one to execute.

### Target and bind resolution

- Flags override the corresponding profile field; partial target tuples fail; a complete profile target selects remote execution just as explicit flags do. **Connection failure never falls back to a local snapshot.**
- For mixed-mode `aggregator ls`, a complete profile target selects remote RPC without `--remote`; `--local` may select a temporary supervisor despite profile defaults and discloses that, but conflicts with explicit remote targeting.
- `--bind <IP:PORT>` overrides profile `bind`, then the per-surface default applies: `127.0.0.1:0` for short-lived clients, `0.0.0.0:0` for `wrap`/`mcp serve`. Loopback-to-non-loopback attachment, mixed address families, multicast binds, and unspecified/multicast/zero-port peers are rejected **before** network or storage effects. IPv6 peers need an IPv6 bind (`[::]:0`). A valid bind does **not** prove firewall/NAT reachability or authorization.

## Deadlines (`--timeout`)

`--timeout` is a **single absolute budget**, not a fresh timeout per stage; stages cannot reset it.

Supported on: remote aggregator `ls/query/spawn/scale`, transfer `ls/status/cancel/recv-blob`, live `typegen generate/snapshot` acquisition, and `mcp serve`/`wrap` startup. Everywhere else it is **refused before effects** rather than ignored (including `recv-dir`, offline generation, staging/receive-unsupported modes, and inspection).

- Zero budget refuses before execution. Expiry is exit 7 with **no success output**.
- Remote effects may already have committed; the CLI **does not retry** an ambiguous operation and prints an uncertain-effect warning.
- `recv-blob`: the deadline covers attachment and every network wait; disk writes count toward elapsed time but are not cancelled, and the final flush/rename proceeds after the complete stream is acquired. Timeout leaves the destination unchanged but may leave `<out>.partial` staging bytes. Not resumable; not a strict total wall-clock cap.
- `wrap`: the budget covers config, identity loading, attachment, and **child MCP initialization/discovery/publication**. Successful initial publication ends its scope — provider lifetime and later refreshes are not timed out. Startup cancellation drops the managed direct child (not an arbitrary process tree).
- `mcp serve`: the budget ends when the shim is ready for protocol input; it does not bound client initialization or the running session.
- Live `typegen`: one budget covers all metadata fetches. Rendering and output writes run **outside** cancellation after acquisition succeeds, so wall-clock time can exceed the budget.
- Omitting the flag retains each command's existing limits (e.g. live typegen's post-attachment 30 s acquisition and 5 s discovery). The formerly advertised global `30s` default was not enforced and was removed.

## Output framing, confirmation, and scripting

- One-shot JSON: successful operations emit exactly one JSON value + newline. Warnings/progress/prompts are on stderr. Failures exit nonzero with no success payload. Streams emit one NDJSON event per line.
- Errors are plain `net-mesh: ...` messages on stderr — **not** a JSON error envelope. Match on exit codes. Do not apply generic JSON framing to `mcp serve` protocol stdout or generated artifacts.
- **ICE**: a commit emits one result containing `preview` and `commit`; the pre-confirmation preview is diagnostic output on **stderr**.

  ```sh
  # migration from the old two-value shape:
  jq -s '.[1].commit_id'   # before
  jq '.commit.commit_id'   # after
  # dry-run still exposes .blast_hash at the top level
  ```

- **`--yes`** acknowledges the operation in both TTY and non-TTY use: it bypasses the prompt but **not** signature/admission/policy checks or required parameters. Without it, interactive stdin requires typed `YES`; unattended commits refuse with exit 8 (= confirmation refused). `--dry-run` needs no confirmation and keeps its preview-only shape.

## Exit codes (all subcommands)

| Code | Meaning |
|---|---|
| `0` | success |
| `1` | generic error |
| `2` | invalid arguments / parse failure |
| `3` | SDK error (a `net-mesh-sdk` operation failed — transfer, query, …) |
| `4` | `ice`: simulation blocked |
| `5` | `ice`: operator-policy rejected |
| `6` | connection failure (no holder, unreachable peer, session refused) |
| `7` | timeout |
| `8` | confirmation refused |
| `10` | reserved daemon-factory error |
| `11` | reserved MeshDB query parse error |
| `12` | reserved predicate parse error |
| `13` | `ice`: an operator signature failed cryptographic verification |
| `14` | `typegen diff --exit-code`: a BREAKING schema change was detected |

## Command map

| Subcommand | Scope | What it does |
|---|---|---|
| `version` | offline | SDK version + build metadata. |
| `identity` | offline | `generate` / `show` / `fingerprint` / `revoke` operator identity files. |
| `admin` | temporary supervisor | `drain`, `enter-maintenance`, `exit-maintenance`, `cordon`, `uncordon`, `drop-replicas`, `invalidate-placement`, `restart-all-daemons`, `clear-avoid-list`. `--dry-run` previews offline; commits need an identity. |
| `ice` | temporary supervisor | `freeze-cluster`, `thaw-cluster`, `flush-avoid-lists`, `force-evict-replica`, `force-restart-daemon`, `force-cutover`, `kill-migration`. Simulation/commit; typed `YES` or `--yes`. |
| `snapshot` | temporary supervisor | `get` / `status`, both `--local`. |
| `audit` | temporary supervisor | `recent` / `stream`. `--since` is a sequence; time range uses `--start-ms`/`--end-ms`. |
| `log` / `failures` | temporary supervisor | `tail`. |
| `cap` | temporary supervisor (reads) / offline | `show` / `query` / `nodes` reads; `announce` authors a **signed JSON artifact offline**, not a broadcast. |
| `peer` / `daemon` | temporary supervisor | `ls` only. No daemon launch/shutdown/migration or NAT verbs. |
| `netdb` | persistent local | Cortex-backed tasks + memories (see below). |
| `org` | offline | `keygen`, `issue-cert`, `issue-floors`, `grant-dispatcher`, `grant-capability`. |
| `node` | offline/local | `adopt` — installs org ownership on a node's authority directory. |
| `subnet` | temporary supervisor (reads) / offline | `show`/`ls`/`tree`; `keygen`, `issue-direct`, `issue-issuer`, `issue-delegated`, `issue-control-fact`, `inspect`. |
| `gateway` | temporary supervisor | `stats` / `exports`; `export` refuses (no live gateway attached). |
| `channel` | temporary supervisor | `visibility` / `ls`. |
| `aggregator` | temporary supervisor or mesh client | `inspect`/local `ls` are temporary; remote `query`/`spawn`/`scale`/targeted `ls` use typed RPC. |
| `transfer` | mesh client (recv/admin) or local (send) | blob/dir movement (see below). |
| `wrap` | hosted service | Wrap a local stdio MCP server as owner-only mesh capabilities. |
| `mcp` | hosted service / local | `serve`; `pin approve/reject/list`. |
| `forwarding` | persistent local | `enable`/`disable`/`allow`/`rm`/`audit`/`set-value` (keychain). |
| `typegen` | mesh client / offline | `generate`/`snapshot`/`diff` (see below). |
| `anchor` | offline or hosted | `credential mint/inspect`; feature-gated `ls`/`stats`/`serve`. |
| `completion` / `man` | offline | shell completion / troff man page from the Clap tree. |

**Not exposed:** `rpc`, `blob`, `db`, `port` are comment-only design modules — do not document their old sketches as usable syntax. Also absent: `daemon run/shutdown/log`, `snapshot watch`, identity-registry editing, peer reflex/NAT management, the old NetDB predicate DSL.

## `net-mesh wrap` and `net-mesh mcp`

The MCP bridge — conceptual model in `mcp.md`. Two roles:

- **`wrap <name> [flags] -- <command...>`** builds a mesh node under the operator identity, discovers the wrapped stdio server's tools, announces them, and serves one owner-scoped nRPC handler per tool. Owner-only is keyed on the wrap node's own `origin_hash`; `--allow <origin>` widens it to named peer origins. Long-running: emits a `wrapped` event, then `tools_changed` / `server_exited` per lifecycle transition, and serves until Ctrl-C.
  - **`--listen`** starts a standalone publisher **without a bootstrap peer**. Requires a PSK (`--psk-hex` or a protected profile), honors explicit/profile `bind` and `identity`, defaults to `127.0.0.1:0`, and **rejects remote peer settings including profile defaults** — select a dedicated profile. The `wrapped` event reports the live bind (`connection.bind`), hex-string node ID, public Noise key, and origin hash; never a PSK or seed. Readiness means local tool publication, not peer reachability or permission. Wildcard binds are not dialable addresses.
- **`mcp serve`** fronts the mesh to a local MCP host (Claude Code / Cursor) with fail-closed consent and human-approved pinning. Its stdout is **protocol traffic**, not ordinary command JSON. `mcp pin approve/reject/list` manage local consumer consent; a pin is not provider authority.
- **`forwarding`** is the opt-in, deny-by-default exception to credential locality; `audit` is a value-free policy report.

`net-mesh wrap --inspect-target` resolves listener/provider selection without binding a port, spawning the child, or reporting a live port/Noise key.

## `net-mesh transfer`

Seven verbs. **Receive/admin require remote attach**: `--node-addr <IP:PORT> --node-pubkey <HEX> --node-id <N> --psk-hex <HEX>`, each defaultable from the profile. Send verbs are local.

| Verb | Shape |
|---|---|
| `recv-blob` | `--blob-ref <REF> --out <PATH> [--from <NODE>] [REMOTE FLAGS]` — fetch one blob, stream to disk. |
| `send-blob` | `<PATH|-> [--store <DIR>]` — compute the `BlobRef`; optionally stage bytes. |
| `recv-dir` | `--remote-ref <REF> --out <PATH> [--from <NODE>] [--concurrency <N>] [REMOTE FLAGS]` — materialize a tree. |
| `send-dir` | `<PATH> [--store <DIR>]` — compute the manifest reference; optionally stage. |
| `ls` | `[REMOTE FLAGS]` — the target's **requester-side pending fetches** (what it is fetching), not a completed-transfer history. |
| `status` | `<TRANSFER-ID> [REMOTE FLAGS]` — unknown ID → `found: false` with exit 0. |
| `cancel` | `<TRANSFER-ID> [REMOTE FLAGS]` — `cancelled: false` is a successful query outcome, not an error exit. |

Key facts:

- `--from <NODE>` selects a content holder other than the handshaken target; it is **not** a relay flag. `--node` names the temporary local supervisor, not the remote provider.
- `recv-blob` writes `<PATH>.partial`, flushes/closes, then renames. A failed fetch leaves the previous final file untouched and may leave the partial for inspection. `recv-dir` reconstructs a sibling temp tree and renames atomically — the destination either becomes the complete tree or stays exactly as it was.
- `send-blob`/`send-dir` without `--store` only hash and print the reference (no bytes persisted). With `--store` they **stage** bytes so a running node rooted there can serve them — the process exits and neither pushes to a peer nor hosts the store. Staging is not publication/hosting; another running holder is required for retrieval.
- JSON mode: `send-blob` stdout is one object with `blob_ref`, `hash`, `size`, `chunks`, optional `staged_to`; `send-dir` has `remote_ref`, `manifest_size`, optional `staged_to`.
- Progress is on stderr only for human output; `--quiet` suppresses.

## `net-mesh typegen`

Code generation from live-discovered tool descriptors or a saved snapshot. (The optional codegen path in `nrpc.md` — the wire stays schemaless JSON.)

```
net-mesh typegen generate --language <ts|python> [--out <PATH>] [--tag <T>]... [--tool <ID>]... [--from-snapshot <PATH>]
net-mesh typegen snapshot --out <PATH> [--tag <T>]... [--tool <ID>]...
net-mesh typegen diff --from <PATH> --to <PATH> [--exit-code] [--output json|yaml]
```

- **Selectors:** ANY within `--tag`, ANY within `--tool`, and the two groups **intersect** when both are given. Neither → all observed descriptors are selected, subject to supported schemas. (Public prose once said "union" — wrong.)
- **Live acquisition:** hydrates missing input schemas from the **exact advertising provider** via `tool.metadata.fetch`, and missing output schemas when that provider advertises the metadata service (output schemas remain optional). Returned ID/version/tags and already-inline schemas must agree with the advertisement; conflicting selected advertisements fail; identical replicas select the lowest node ID, with no fallback or CLI retry. Missing/unusable input or failed hydration refuses **before** writing output. Native peers negotiate responses up to 1 MiB (packet limit stays 8 KiB); older providers may refuse over one packet and return an explicit RPC size error — the handler may have completed and the CLI does not retry.
- **Discovery:** live discovery waits up to 5 s for every explicit tool ID to pass both filters; missing IDs exit 7 **before** writing output. Tag-only/unfiltered discovery observes the full 5 s window — a bounded observation, **not** a complete mesh inventory; a timeout cannot certify absence. The remaining `--timeout` budget can shorten, not extend, that window.
- Generation walks the capability fold for `ai-tool:*` descriptors. Output is one module per tool: the tool's JSON Schema lowers to TypeScript interfaces or Pydantic v2 models, plus a typed call helper (`callAcmeWebSearch(mesh, request)` / `call_acme_web_search(mesh, request)`) and a `…Meta` constant. Python models require **Pydantic v2**. Offline generation from a snapshot does not imply offline invocation.
- `snapshot` writes a versioned JSON file (`format_version`, `captured_at`, `source_query`, `descriptors`), stable within a `format_version`. `diff` lists added/removed tools, version bumps, and schema deltas with `[BREAKING]` markers; `--exit-code` exits **14** on any BREAKING change (CI gate).

## `net-mesh netdb`

Persistent local RedEX/CortEX store for tasks and memories.

- Store precedence: explicit `--store` → profile `netdb` → `dirs::data_dir()/net-mesh/netdb`.
- Task/memory creation takes a caller-supplied ID. Reads refuse a missing directory rather than creating an empty one.
- `restore` requires an explicit origin or acknowledged origin zero. `--force` permits merge; `--clear` removes the destination before replacement. **Restore is not transactional**: it validates configuration and the snapshot (a bounded single read, decode, nested-payload validation) **before** mutating the destination, but interruption or storage failure after that can still leave incomplete state. It is not crash-atomic replacement or concurrent-writer-safe — operate offline.
- A successful restore publishes origin-bound local checkpoints beside the RedEX logs so restored records survive process exit and ordinary reopening without the source file. **Keep the checkpoint files when copying a restored store**; older binaries cannot read this format and must not open it.
- All NetDB verbs support `--inspect-target` (resolution only — it does not open/create/clear the store, so it is not restore preflight).

## `net-mesh org` — organization capability auth (issuance)

Offline authoring against an **org root key**. These are *ceremonies over files* — no live node. Conceptual model, and what each artifact does **not** authorize: `org.md`.

| Command | Produces |
|---|---|
| `org keygen --out <path> [--note <s>] [--force]` | a fresh org root keypair. Default `$XDG_CONFIG_HOME/net-mesh/orgs/org-<id>.toml`. |
| `org issue-cert --org-key <path> --member <hex> [--generation N] [--ttl-secs N] --out <path>` | a membership cert. TTL defaults ~1 year, hard-capped at 2. |
| `org issue-floors --org-key <path> --floor <MEMBER=GEN> [--floor …] --out <path>` | a signed revocation-floor bundle (monotonic; a lower floor never rolls back). |
| `org grant-dispatcher --org-key <path> --dispatcher <hex> (--capability <tag> \| --any-capability) [--ttl-secs N] --out <path>` | "entity X may act **for** this org." A→S. |
| `org grant-capability --org-key <path> --grantee-org <hex> --capability <tag> [--invoke] [--discover --audience-out <path>] (--target-node <hex> \| --target-any-owned-by <hex>) [--ttl-secs N] --out <path>` | "org A holds these rights on capability C over target T." B→A, signed by the *provider* org. |
| `node adopt --cert <path> (--identity <path> \| --entity <hex>) [--authority-dir <dir>] [--bundle <path>] [--skew-secs N]` | installs `owner-membership.json`, `owner-audience.key`, `revocation-state.json` under `$XDG_CONFIG_HOME/net-mesh/authority`. |

Things that will bite:

- **Grant TTLs default to 7 days and hard-cap at 30**, rejected at issue *and* at every verifier. Renewal is re-issue + `issue-floors`, not extension.
- **`--force` is refused on both grant commands.** Grant artifacts are published no-clobber; the grant + audience secret pair is not crash-atomic, and on a case-insensitive filesystem an aliased `--out` could destroy the org key. Write to fresh paths or remove the old files explicitly. (`keygen`/`issue-cert`/`issue-floors` do accept `--force`.)
- **`--discover` requires `--audience-out`**; the audience secret is written owner-only (0600 on Unix). **On Windows the mode is unenforceable** — the file inherits the parent's NTFS DACL, and a loud warning fires unless `--accept-windows-dacl`. Point it at an owner-only parent.
- **`--accept-windows-dacl` and `--insecure-permissions` are separate on purpose.** The first suppresses a warning about a freshly written **output** secret; the second relaxes a mode check on an **input** you already control (e.g. a key checked out of git at 0644). Merging them let a Linux-motivated `--insecure-permissions` kill Windows' only warning.
- `adopt --skew-secs` is strict by default and hard-capped at the token ceiling (300 s); larger values are rejected before anything is written.
- **No default falls back to the CWD.** If the config directory can't be resolved, `keygen`/`adopt` refuse and tell you to pass an explicit path.

## `net-mesh subnet` — topology views + authority issuance

Two deliberately distinct groups under one noun (topology is not authority):

**Topology inspection** — `show`, `ls`, `tree`: read-only views against a **temporary supervisor** (`--local` required). With no node attached they print their natural empty shape.

**Authority issuance** — offline ceremonies over files, like `net-mesh org`: artifacts as canonical wire bytes the runtime admin surface installs (`subnet-auth.md` § Provisioning). Nothing in any SDK signs.

| Command | Produces |
|---|---|
| `subnet keygen --out <path> [--note <s>] [--force]` | a subnet authority keypair — authority root **or** delegated issuer. Refuses to overwrite a different kind of secret (org key, operator identity). |
| `subnet issue-direct --root-key <path> --authority <hex> --subject <hex> --scope <path\|global> --rights attach,route,export --topology-epoch N --generation N --out <path>` | one DIRECT credential set (root → subject). |
| `subnet issue-issuer --root-key <path> --authority <hex> --issuer <hex> --scope <ceiling> --max-rights <r,…> --topology-epoch N --out <path>` | one bounded ISSUER grant (root → delegated issuer). One-hop depth is structural. |
| `subnet issue-delegated --issuer-grant <path> --issuer-key <path> --subject <hex> --scope <path> --rights <r,…> --out <path>` | one DELEGATED set — leaf framed *together with* its issuer grant. |
| `subnet issue-control-fact (descriptor\|gateway-advertisement\|export-policy\|revocation-floor) --root-key <path> --authority <hex> --scope <path\|global> --topology-epoch N --revision N --out <path> [kind-specific args]` | one signed control fact. |
| `subnet inspect <file>` | decode + summary of any subnet artifact, **without private material**; exits non-zero on malformed/non-canonical bytes. **Decode-only — it does not verify signatures.** |

Things that will bite:

- **`--authority` is always explicit** — an authority may trust multiple roots, so the id is never silently derived from the signing key. Passing the wrong id mints an artifact every verifier refuses as `wrong_authority`.
- **`--scope global` is the whole-authority root scope** — a deliberate word, never an "unscoped" default reachable by omission.
- **`--topology-epoch` is explicit on facts**: a fact never invents authority movement; reparenting is recorded by a new epoch.
- Issuance validates **structure and attenuation** (leaf inside issuer scope, rights within the maximum) but **not root authenticity** — successful issuance is not proof of deployability. Revocation is monotonic floors; a lower floor never rolls back.
- Key hygiene matches the org surface: 0600 secrets, `--insecure-permissions` for permissive **input** modes on Unix, `--accept-windows-dacl` for the Windows **output** warning, no CWD fallback.

## The two-node capability journey

A runnable, public acceptance example lives in `net/crates/net/cli/tests/fixtures/README.md` and is exercised by CI. It starts **two separate CLI mesh processes** on one machine (no bootstrap node): `wrap --listen` publishes a recorded Python MCP server; `mcp serve` connects from another process. It proves both permission boundaries — an unpinned consumer is refused before handler effect, explicit `mcp pin approve` grants consent, one authorized call matches exactly one provider-side record, and a pin does **not** override provider owner scope. Companion legs cover native typed calls (`serve_tool`/`call_typed`), live contract capture with offline regeneration, and a protected same-org authorization leg (denial-before-effect, then an authorized call; generated Python consumer included).

This is **two-process, two-node loopback evidence** — not two computers, not cross-org grants, not a production credential recipe, and not proof that arbitrary startup order succeeds. Invocations are never retried; a timeout does **not** prove a handler had no effect — inspect the provider record.

## Cross-references

- `dataforts.md` — the blob/dir model behind `transfer` (`BlobRef`, content-addressing, `store_dir`/`fetch_dir`, gravity).
- `nrpc.md` — the discovered-tool / typed-call surface `typegen` generates against.
- `capabilities.md` — the `ai-tool:*` capability tags `typegen` discovers.
- `mcp.md` — `wrap` / `mcp serve` / `mcp pin` / `forwarding` conceptual model.
- `org.md` — what the `net-mesh org` artifacts mean, the startup-side `install_org_authority` / `install_provider_grant_audience` calls, and the `org:<domain>:<kind>` errors.
- `subnet-auth.md` — what the `net-mesh subnet` artifacts mean, the runtime admin surface that installs them, and the `subnet:<kind>` errors.

## Further reading

- [Public CLI reference](https://ai2070.net/docs/reference/cli)
- [Two-node capability journey](https://github.com/ai-2070/net/blob/master/net/crates/net/cli/tests/fixtures/README.md)
- [Deck (Operator TUI)](https://ai2070.net/docs/reference/deck)