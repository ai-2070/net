# CLI Reference — `net-mesh`

Read this when the user wants to drive Net from the shell: host a capability, connect a consumer, run a managed enrollment node and join devices to it, move a blob/directory, generate typed bindings from discovered AI tools, author authority artifacts offline, script the binary, or asks "what commands does `net-mesh` have / which scope does this run in / what do the exit codes mean." The in-context mentions elsewhere — `net-mesh transfer` in `dataforts.md`, `net-mesh typegen` in `nrpc.md`, `net-mesh wrap`/`mcp`/`forwarding` in `mcp.md`, `net-mesh org`/`node adopt` in `org.md`, `net-mesh subnet` in `subnet-auth.md` — point here for the full surface.

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
| **Offline** | Authors or inspects local files; no live mesh implied | `identity`, `org`/`subnet`/`channel` issuance, `enrollment init`, `invite inspect`, `subnet inspect`, `cap announce` artifact, `typegen` from a saved snapshot |
| **Persistent local** | Opens a local on-disk store or policy | `netdb`, `mcp` pins, `forwarding` policy, staged `transfer` content |
| **Long-lived node** | Runs one **foreground node per profile** that owns its state directory; other commands reach it over its authenticated local control endpoint | `up` (foreground), `down`/`node status`, `invite` on an `up --enroll` node, and the `org`/`subnet`/`channel` relation verbs that act on a running node |
| **Joined device** | Loads the credentials `join` installed and runs as that device | `join`/`leave`, `wrap --joined`, `mcp serve --joined` |
| **Temporary supervisor** | Starts a **new** `MeshOsDaemonSdk` for this invocation and uses its local Deck client | `admin`, `ice`, `snapshot`, `audit`, `log`/`failures` tail, `peer`/`daemon` ls, `cap` reads, `subnet` topology reads, `gateway`/`channel` reads, local `aggregator` |
| **Mesh client** | Creates a local mesh participant and connects to an **explicitly resolved remote target** | remote `aggregator` (`query`/`spawn`/`scale`/remote `ls`), `transfer` receive/admin, live `typegen` |
| **Hosted service** | Keeps a mesh participant and a service/subprocess alive | `wrap`, `mcp serve`, `relay serve`, feature-enabled `anchor serve` |

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
- Offline issuance, persistent stores, and real remote mesh clients do **not** take `--local` — nor do the managed-node, link and relay verbs (`up`/`down`/`node status`, `enrollment`, `invite`, `join`/`leave`, `relay serve`, and the `org`/`subnet`/`channel` relation verbs): a running node is named by `--state-dir`, never `--local`.
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
net-mesh wrap journey --listen --psk-hex <HEX> --inspect-target -- <COMMAND>   # resolves without binding the port; the `-- <COMMAND>` trailer is required by the parser even under inspection
```

It is available on:

- Remote clients: aggregator `ls/query/spawn/scale`, transfer `recv-blob`/`recv-dir` and receive/admin, live `typegen`, `wrap` (including `--listen`), `mcp serve`, feature-gated `anchor ls/stats`.
- Local stores: **all** NetDB verbs, saved `typegen`, `forwarding` policy verbs, `mcp` pins, `transfer send-blob/send-dir`, keychain-backed `forwarding set-value` (keychain builds only).
- Issuance/authoring: identity `generate/show/fingerprint/revoke`, `cap announce`, `node adopt`, all five `org` verbs, all subnet issuance subcommands + `inspect`, `anchor credential mint/inspect`.
- Temporary supervisors: `--local --inspect-target` on capability `show/query/nodes`, subnet `show/ls/tree`, gateway `stats/exports`, snapshot, audit, log/failures tail, peer/daemon/channel reads, aggregator inspect, and admin/ICE.

**Not available on** the managed-node and link verbs: `up`/`down`/`node status`, `enrollment`, `invite`, `join`/`leave`, `relay serve`, and the `org`/`subnet`/`channel` relation verbs (`org approve`/`invite`/`join`/`remove`/`leave`/`members`, `subnet invite`/`join`/`remove`/`leave`/`members`/`activate`, `channel serve`/`status`/`publish`/`leave`/`invite`/`join`). `--joined` also rejects `--inspect-target`. (The offline `org`/`subnet` issuance verbs *do* support it, as listed above.)

Reported fields: `mode` (`offline` / `persistent_store` / `temporary_supervisor` / remote), peer address/node ID, public fingerprints (never raw keys/PSKs), identity availability, `bind`, field provenance, ignored profile defaults, and output destination/provider ID where applicable. On admin/ICE it conflicts with `--dry-run`, reports `identity_required: true`, and never prompts or commits.

**It is resolution only, not validation** — not restore preflight, not content/schema validation, not proof a path is writable or an authority is valid. Execution retains its own gates. An unconfigured identity has no fingerprint; hosted services still need one to execute.

### Target and bind resolution

- Flags override the corresponding profile field; partial target tuples fail; a complete profile target selects remote execution just as explicit flags do. **Connection failure never falls back to a local snapshot.**
- For mixed-mode `aggregator ls`, a complete profile target selects remote RPC without `--remote`; `--local` may select a temporary supervisor despite profile defaults and discloses that, but conflicts with explicit remote targeting.
- `--bind <IP:PORT>` overrides profile `bind`, then the per-surface default applies: `127.0.0.1:0` for short-lived clients, `0.0.0.0:0` for `wrap`/`mcp serve`. Loopback-to-non-loopback attachment, mixed address families, multicast and broadcast binds, and unspecified/multicast/zero-port peers are rejected **before** network or storage effects. IPv6 peers need an IPv6 bind (`[::]:0`). A valid bind does **not** prove firewall/NAT reachability or authorization.

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
| `up` / `down` | long-lived node | Start one long-lived node for the profile (foreground); drain and stop it and verify it released its lifetime lock. |
| `enrollment` | offline | `init` — create an empty enrollment ledger bound to an issuer identity, for `up --enroll`. |
| `invite` | running `up --enroll` node (link) | `create` / `status` / `revoke` / `approve` / `deny` on the node; `inspect` is offline. |
| `join` / `leave` | joined device | Redeem a `netmesh-join_` link and run as it; leave the mesh (local, durable; not revocation). |
| `relay` | hosted service | `serve` — run a blind UDP relay, the fallback path for unreachable devices. |
| `admin` | temporary supervisor | `drain`, `enter-maintenance`, `exit-maintenance`, `cordon`, `uncordon`, `drop-replicas`, `invalidate-placement`, `restart-all-daemons`, `clear-avoid-list`. `--dry-run` previews offline; commits need an identity. |
| `ice` | temporary supervisor | `freeze-cluster`, `thaw-cluster`, `flush-avoid-lists`, `force-evict-replica`, `force-restart-daemon`, `force-cutover`, `kill-migration`. Simulation/commit; typed `YES` or `--yes`. |
| `snapshot` | temporary supervisor | `get` / `status`, both `--local`. |
| `audit` | temporary supervisor | `recent` / `stream`. `--since` is a sequence; time range uses `--start-ms`/`--end-ms`. |
| `log` / `failures` | temporary supervisor | `tail`. |
| `cap` | temporary supervisor (reads) / offline | `show` / `query` / `nodes` reads; `announce` authors a **signed JSON artifact offline**, not a broadcast. |
| `peer` / `daemon` | temporary supervisor | `ls` only. No daemon launch/shutdown/migration or NAT verbs. |
| `netdb` | persistent local | Cortex-backed tasks + memories (see below). |
| `org` | offline / running node | Offline roots: `keygen`, `issue-cert`, `issue-floors`, `grant-dispatcher`, `grant-capability`, `audience-keygen`, `approve`. Org links on the running node: `invite`, `join`, `remove`, `leave`, `members`. |
| `node` | running node / offline | `status` of this profile's node; `adopt` installs org ownership on a node's authority directory. |
| `subnet` | temporary supervisor (reads) / offline / running node | `show`/`ls`/`tree`; offline `keygen`, `issue-direct`, `issue-issuer`, `issue-delegated`, `issue-control-fact`, `inspect`; links on the running node: `invite`, `join`, `remove`, `leave`, `members`, `activate`. |
| `gateway` | temporary supervisor | `stats` / `exports`; `export` refuses (no live gateway attached). |
| `channel` | offline / running node / temporary supervisor (reads) | Offline `issue-grant`; on the running node `serve`, `status`, `publish`, `leave`, `invite`, `join`; registry reads `visibility` / `ls`. |
| `aggregator` | temporary supervisor or mesh client | `inspect`/local `ls` are temporary; remote `query`/`spawn`/`scale`/targeted `ls` use typed RPC. |
| `transfer` | mesh client (recv/admin) or local (send) | blob/dir movement (see below). |
| `wrap` | hosted service | Wrap a local stdio MCP server as owner-only mesh capabilities (`--joined` runs as an enrolled device). |
| `mcp` | hosted service / local | `serve` (`--joined`); `pin approve/reject/list`. |
| `forwarding` | persistent local | `enable`/`disable`/`allow`/`rm`/`audit`/`set-value` (keychain). |
| `typegen` | mesh client / offline | `generate`/`snapshot`/`diff` (see below). |
| `anchor` | offline or hosted | `credential mint/inspect`; feature-gated `ls`/`stats`/`serve`. |
| `completion` / `man` | offline | shell completion / troff man page from the Clap tree. |

**Not exposed:** `rpc`, `blob`, `db`, `port` are comment-only design modules — do not document their old sketches as usable syntax. Also absent: `daemon run/shutdown/log`, `snapshot watch`, identity-registry editing, peer reflex/NAT management, the old NetDB predicate DSL.

## Managed nodes, join links, and leave

`up` runs **one long-lived node per profile** in the foreground; `down` drains and stops exactly that node and verifies it did; `node status` reports it through its lifetime lock and authenticated local control endpoint. State — identity, generated PSK, lifetime lock, control endpoint — lives in the profile's node state directory (`--state-dir`, default `<platform data dir>/net-mesh/nodes/<profile>`). Commands that talk to a running node (`invite`, the `org`/`subnet`/`channel` relation verbs) take `--state-dir <DIR>`, the directory its `up` was started with.

```sh
net-mesh up --enroll                              # operator node (foreground)
net-mesh invite create                            # prints a netmesh-join_ token (a secret)
net-mesh join <TOKEN> --yes                       # on the device
net-mesh up                                       # on the device
net-mesh down                                     # drain + stop, and verify
```

- **PSK.** `up` generates and keeps the mesh PSK on first start, or takes one from `--psk-from file:<path>` (32 raw bytes or 64 hex) / `--psk-from stdin` (piped, not a terminal). A literal PSK is **never** accepted on the command line.
- **`up --enroll`** makes the node the enrollment owner: it serves join-token redemption and accepts `invite` operations. On first run it creates and keeps an issuer key, a ledger and a fixed port, and asks the router to forward that port unless `--no-port-mapping`. Extra flags: `--public-addr`, `--issuer-identity`, `--ledger`, `--domain-name`, `--relay`/`--no-relay`, `--subnet-issuer-grant`/`--subnet-issuer-key`, `--channel-grant` (repeatable).
- **`enrollment init --issuer-identity <path> [--state-dir <dir>] [--ledger <dir>]`** creates an empty enrollment ledger (default `<state-dir>/ledger`; the ledger must not exist yet) bound to an issuer identity. Offline authoring — no node required.

### Join links (`invite`)

- `invite create [--subnet <path>] [--subnet-rights <r,…>] [--org <org>] [--channel <name> --channel-rights <rights>] [--require-approval] [--for <entity>] [--ttl <dur>] [--addr <host:port>] [--out <path>]` mints a `netmesh-join_` link on the running `up --enroll` node.
- **One link, several relations, each authorized on its own**: mesh membership, a subnet attachment (`--subnet`, delegated credential issued at redemption), org membership (`--org`, always approval-gated), and a channel credential (`--channel`/`--channel-rights`, a chain `root → this node → device` minted from `--channel-grant`). Channel rights are `publish`, `subscribe` or `publish,subscribe`; subscribe sends the device to *this* node as publisher, so the channel must be served here first. `ADMIN`, wildcard and delegation are never issued.
- **Links are bearer secrets** unless bound with `--for <ENTITY>`: whoever redeems one first joins. `invite create` warns; write it privately (`--out` gives an owner-only file) or bind it to a device.
- `--require-approval` holds issuance until `invite approve <OFFER-ID> --subject <entity>` signs the pending claim; `invite deny` refuses it.
- `invite inspect <TOKEN>` is **offline** and redeems nothing; `invite status [<OFFER-ID>]` and `invite revoke <OFFER-ID>` act on the running node (`inspect` takes the token or `-` for stdin). Revoking an unredeemed invitation does **not** revoke credentials already issued.

### Join and leave

- `join <TOKEN> [--state-dir <dir>] [--yes] [--wait <dur>] [--rejoin]` redeems the link, installs the credentials and confirms a live attach on a clean device. **Direct attach is tried first**; if the link names a relay, the relay is the fallback. `--rejoin` joins again after a whole `leave`, fetching the credentials again under the issuer's current authorization.
- `leave [--state-dir <dir>] [--wait <dur>]` leaves the **whole mesh**: it records the departure, stops the running node and erases the delivered credentials, keeping the device identity. It is local and durable and **is not revocation**.
- A single relation is left with `org leave`, `subnet leave <scope>`, or `channel leave [<name>]` — each recorded first, durable, and surviving restart; a left relation is never presented, renewed or re-subscribed again. Rejoining after a relation leave takes a fresh link; the spent one stays spent.
- **Removal is the operator-side act** (`org remove` / `subnet remove`): sign a floor with the offline root and have each named node apply it, reported per node from that node's own signed attestation.

### Enrolled consumers (`--joined`)

`wrap --joined <state-dir>` and `mcp serve --joined <state-dir>` run as the enrolled device: they load the device identity, mesh PSK and enrolled contact from the join. They **refuse while `up` owns the join** (stop it first) and **refuse `--psk-hex`** (the PSK comes from the join); `--inspect-target` is not available with `--joined`. An explicit `--node-addr`/`--node-pubkey`/`--node-id` names the peer; without them the enrolled contact is used.

### Blind relay (`relay serve`)

`relay serve --bind <IP:port> [--max-registrations <n>] [--max-channels-per-registration <n>]` runs a blind UDP relay in the foreground: a device registers from its mesh socket (proving its identity key), joiners bind channels to that registration, and the relay forwards their encrypted datagrams **without being able to read them**. It holds no PSK, issuer key or mesh credential, and is not a mesh member. **UDP and TCP are served on the same port** — enrollment splices and a plain-TCP tunnel for nodes whose UDP to the relay goes unanswered share it; bind port 443 to reach networks that allow only that port. The tunnel carries the same end-to-end ciphertext and is not designed to cross proxies. A joiner that falls back to it reports `relay_transport: tcp` / `attach_path: relay_tcp`.

### What an enrolled `up` reports

Admission and subscription are **observed on the live session**, never implied by holding a credential: `joined.subnet.admitted` is the verifier's verdict on this session and `joined.channel.subscribed` is the publisher's ACK of the full chain. The node re-establishes both itself after every reconnect. The ready row reports the enrollment endpoint (`enrollment.issuer`, `enrollment.relay_transport`), the subnet this node verifies and the channels it can mint. `joined.path` reports the attach path used (`direct` / `relay` / `relay_tcp`).

## `net-mesh wrap` and `net-mesh mcp`

The MCP bridge — conceptual model in `mcp.md`. Two roles:

- **`wrap <name> [flags] -- <command...>`** builds a mesh node under the operator identity, discovers the wrapped stdio server's tools, announces them, and serves one owner-scoped nRPC handler per tool. Owner-only is keyed on the wrap node's own `origin_hash`; `--allow <origin>` widens it to named peer origins. Long-running: emits a `wrapped` event, then `tools_changed` / `server_exited` per lifecycle transition, and serves until Ctrl-C.
  - **`--listen`** starts a standalone publisher **without a bootstrap peer**. Requires a PSK (`--psk-hex` or a protected profile), honors explicit/profile `bind` and `identity`, defaults to `127.0.0.1:0`, and **rejects remote peer settings including profile defaults** — select a dedicated profile. The `wrapped` event reports the live bind (`connection.bind`), hex-string node ID, public Noise key, and origin hash; never a PSK or seed. Readiness means local tool publication, not peer reachability or permission. Wildcard binds are not dialable addresses.
  - **`--joined <state-dir>`** (`wrap` and `mcp serve`) runs the provider/consumer **as the enrolled device** instead of supplying a PSK — see [Managed nodes](#managed-nodes-join-links-and-leave) for the join-ownership and `--psk-hex` refusals.
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
- JSON mode: `send-blob` stdout is one object with `blob_ref`, `size`, `chunks`, optional `staged_to`, and `hash` **only for single-chunk content** (absent for a chunked blob — key on `blob_ref`, not `hash`); `send-dir` has `remote_ref`, `manifest_size`, optional `staged_to`.
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
- Generation walks the capability fold for `ai-tool:*` descriptors. Output is one module per tool: the tool's JSON Schema lowers to TypeScript interfaces or Pydantic v2 models, plus a typed call helper (`callAcmeWebSearch(mesh, request)` / `call_acme_web_search(mesh, request)`) and — TypeScript only — a `…Meta` constant (generated Python exports no `…Meta`; its metadata surface is `_meta.json`). Python models require **Pydantic v2**. Offline generation from a snapshot does not imply offline invocation.
- `snapshot` writes a versioned JSON file (`format_version`, `captured_at`, `source_query`, `descriptors`), stable within a `format_version`. `diff` lists added/removed tools, version bumps, and schema deltas with `[BREAKING]` markers; `--exit-code` exits **14** on any BREAKING change (CI gate).

## `net-mesh netdb`

Persistent local RedEX/CortEX store for tasks and memories.

- Store precedence: explicit `--store` → profile `netdb` → `dirs::data_dir()/net-mesh/netdb`.
- Task/memory creation takes a caller-supplied ID. Reads refuse a missing directory rather than creating an empty one.
- `restore` requires an explicit origin or acknowledged origin zero. `--force` permits merge; `--clear` removes the destination before replacement. **Restore is not transactional**: it validates configuration and the snapshot (a bounded single read, decode, nested-payload validation) **before** mutating the destination, but interruption or storage failure after that can still leave incomplete state. It is not crash-atomic replacement or concurrent-writer-safe — operate offline.
- A successful restore publishes origin-bound local checkpoints beside the RedEX logs so restored records survive process exit and ordinary reopening without the source file. **Keep the checkpoint files when copying a restored store**; older binaries cannot read this format and must not open it.
- All NetDB verbs support `--inspect-target` (resolution only — it does not open/create/clear the store, so it is not restore preflight).

## `net-mesh org` — organization capability auth (issuance and relations)

Two groups under one noun. **Offline authoring** against an **org root key** — these are *ceremonies over files*, no live node. **Relation verbs** — `invite`/`join`/`remove`/`leave`/`members` — act on a running `up` node (`--state-dir`), and every removal is signed offline. Conceptual model, and what each artifact does **not** authorize: `org.md`.

| Command | Produces |
|---|---|
| `org keygen [--out <path>] [--note <s>] [--force]` | a fresh org root keypair. Default `$XDG_CONFIG_HOME/net-mesh/orgs/org-<id>.toml`. |
| `org issue-cert --org-key <path> --member <hex> [--generation N] [--ttl-secs N] --out <path>` | a membership cert. TTL defaults ~1 year, hard-capped at 2. |
| `org issue-floors --org-key <path> --floor <MEMBER=GEN> [--floor …] --out <path>` | a signed revocation-floor bundle (monotonic; a lower floor never rolls back). |
| `org grant-dispatcher --org-key <path> --dispatcher <hex> (--capability <tag> \| --any-capability) [--ttl-secs N] --out <path>` | "entity X may act **for** this org." A→S. |
| `org grant-capability --org-key <path> --grantee-org <hex> --capability <tag> (--invoke \| --discover --audience-out <path>) (--target-node <hex> \| --target-any-owned-by <hex>) [--ttl-secs N] --out <path>` | "org A holds these rights on capability C over target T." B→A, signed by the *provider* org. |
| `org audience-keygen --org-key <path> --out <path> [--insecure-permissions]` | mints the org's shared owner audience once (owner-only) — the key members use to open the org's private announcements. Handed to members via `org approve --audience` / `node adopt --audience`. |
| `org approve <OFFER-ID> --subject <hex> --org-key <path> [--generation N] [--audience <path>] [--ttl-secs N] [--insecure-permissions] [--state-dir <dir>]` | signs a *pending* invite's membership certificate here, with the offline root, for exactly the claiming device, and hands it to the running enrolling node. The root key never reaches a node. |
| `org invite <ORG> [--state-dir <dir>] [--ttl <dur>] [--for <entity>] [--out <path>]` | a standalone org link (org membership only) for a device already on the mesh. Always approval-gated (`org approve`). |
| `org join <TOKEN> [--state-dir <dir>] [--yes]` | redeems a standalone org link through this device's running `up`; until the operator approves the node keeps asking by itself, then adopts the membership and installs it live. |
| `org remove <MEMBER> --org-key <path> --minimum-generation <N> --verifier <NODE>... [--state-dir <dir>] [--wait <dur>] [--dry-run] [--insecure-permissions]` | signs a member floor with the org root (every membership cert of that member below `--minimum-generation` is revoked) and has each named node apply it. |
| `org leave [--state-dir <dir>] [--wait <dur>]` | records this device's org departure durably, then stops the running node; its next `up` runs on the mesh without the org. Local only. |
| `org members <ORG> [--state-dir <dir>] [--verifier <NODE> --org-key <path>] [--wait <dur>] [--insecure-permissions]` | what the node issued for this org (when it enrolls) and each member's standing against its own floors — never a claim about other nodes or activity. |
| `node adopt --cert <path> (--identity <path> \| --entity <hex>) [--authority-dir <dir>] [--floors <path>] [--skew-secs N]` | installs `owner-membership.json`, `owner-audience.key`, `revocation-state.json` under `$XDG_CONFIG_HOME/net-mesh/authority`; `--floors` merges a revocation-floor bundle during adoption. |

Things that will bite:

- **Grant TTLs default to 7 days and hard-cap at 30**, rejected at issue *and* at every verifier. Renewal is re-issue + `issue-floors`, not extension.
- **`--force` is refused on both grant commands.** Grant artifacts are published no-clobber; the grant + audience secret pair is not crash-atomic, and on a case-insensitive filesystem an aliased `--out` could destroy the org key. Write to fresh paths or remove the old files explicitly. (`keygen`/`issue-cert`/`issue-floors` do accept `--force`.)
- **At least one of `--invoke` / `--discover` is required** (both may be granted together). **`--discover` requires `--audience-out`**; the audience secret is written owner-only (0600 on Unix). **On Windows the mode is unenforceable** — the file inherits the parent's NTFS DACL, and a loud warning fires unless `--accept-windows-dacl`. Point it at an owner-only parent.
- **`--accept-windows-dacl` and `--insecure-permissions` are separate on purpose.** The first suppresses a warning about a freshly written **output** secret; the second relaxes a mode check on an **input** you already control (e.g. a key checked out of git at 0644). Merging them let a Linux-motivated `--insecure-permissions` kill Windows' only warning.
- `adopt --skew-secs` is strict by default and hard-capped at the token ceiling (300 s); larger values are rejected before anything is written.
- **No default falls back to the CWD.** If the config directory can't be resolved, `keygen`/`adopt` refuse and tell you to pass an explicit path.

**Relations, removal and leave:**

- **Org membership is always approval-gated.** A device joins with a standalone `org invite` → `org join`, then `org approve` signs its certificate with the offline root; until then the node keeps asking by itself. Without `--audience` the device's audience is node-local.
- **`org members` separates *issued* from *observed*.** It reports what this node issued (when it enrolls) and each member's standing against its own floors — standing is not activity, and this node never speaks for other nodes.
- **Removal is per named node.** `org remove` signs a member floor with the offline root and hands it to each `--verifier`; each reports from its own signed attestation, and `complete` holds only when all named nodes persisted it. Unnamed nodes are never assumed.
- **Leave is local, durable, and not revocation.** `org leave` is recorded first and survives restart; the org still accepts the device's certificate until `org remove`. Rejoining takes a new link approved with the org root.

## `net-mesh subnet` — topology, authority, and relations

Three distinct groups under one noun — topology, offline authority, and live relations:

**Topology inspection** — `show`, `ls`, `tree`: read-only views against a **temporary supervisor** (`--local` required). With no node attached they print their natural empty shape.

**Authority issuance** — offline ceremonies over files, like `net-mesh org`: artifacts as canonical wire bytes the runtime admin surface installs (`subnet-auth.md` § Provisioning). Nothing in any SDK signs.

**Relations** — `invite`/`join`/`remove`/`leave`/`members`/`activate` act on a running `up` node (`--state-dir`); `remove` still signs its floor with the offline root.

| Command | Produces |
|---|---|
| `subnet keygen [--out <path>] [--note <s>] [--force]` | a subnet authority keypair — authority root **or** delegated issuer. Refuses to overwrite a different kind of secret (org key, operator identity). |
| `subnet issue-direct --root-key <path> --authority <hex> --subject <hex> --scope <path\|global> --rights attach,route,export [--topology-epoch N] [--generation N] --out <path>` | one DIRECT credential set (root → subject). |
| `subnet issue-issuer --root-key <path> --authority <hex> --issuer <hex> --scope <ceiling> --max-rights <r,…> [--topology-epoch N] --out <path>` | one bounded ISSUER grant (root → delegated issuer). One-hop depth is structural. |
| `subnet issue-delegated --issuer-grant <path> --issuer-key <path> --subject <hex> --scope <path> --rights <r,…> --out <path>` | one DELEGATED set — leaf framed *together with* its issuer grant. |
| `subnet issue-control-fact (descriptor\|gateway-advertisement\|export-policy\|revocation-floor) --root-key <path> --authority <hex> --scope <path\|global> --topology-epoch N --revision N --out <path> [kind-specific args]` | one signed control fact. |
| `subnet inspect <file>` | decode + summary of any subnet artifact, **without private material**; exits non-zero on malformed/non-canonical bytes. **Decode-only — it does not verify signatures.** |
| `subnet remove --root-key <path> --authority <hex> --scope <path\|global> --topology-epoch N --revision N --subject <hex> [--rights attach] --minimum-generation N --verifier <CONTACT>... [--psk-hex <hex> \| --state-dir <dir>] [--dry-run] [--wait <dur>] [--insecure-permissions]` | signs a subject floor with the offline root and hands it to each named verifier; reported per verifier from its own signed attestation (`--state-dir` carries the requests through the operator's node). |
| `subnet invite <SCOPE> [--rights <r,…>] [--state-dir <dir>] [--ttl <dur>] [--require-approval] [--for <entity>] [--out <path>]` | a standalone subnet link (the subnet relation only) for a device already on the mesh — redeemed over the device's own session, so **no PSK is delivered**. Needs `up --enroll` with a subnet issuer whose grant covers the scope. |
| `subnet join <TOKEN> [--state-dir <dir>] [--yes] [--switch]` | redeems a standalone subnet link through this device's running `up`; keeps and presents the credentials (the verifier's verdict is reported) and the node renews/re-presents them itself. `--switch` makes it the ACTIVE attachment despite another at the verifier. |
| `subnet members <SCOPE> [--state-dir <dir>] [--verifier <NODE> --root-key <path> --authority <hex>] [--wait <dur>] [--insecure-permissions]` | what the node issued for this scope (and its subtree) and which peers are admitted here right now — explicitly not a claim about other verifiers. |
| `subnet leave <SCOPE> [--state-dir <dir>]` | records the departure durably, never presents or renews it again, and asks the verifier over the session to drop the admission (acknowledged or reported unconfirmed). The credential is **not** revoked. |
| `subnet activate <SCOPE> [--state-dir <dir>]` | makes one stored subnet relation the ACTIVE attachment at its verifier; the previous one is withdrawn there and stays stored. |

Things that will bite:

- **`--authority` is always explicit** — an authority may trust multiple roots, so the id is never silently derived from the signing key. Passing the wrong id mints an artifact every verifier refuses as `wrong_authority`.
- **`--scope global` is the whole-authority root scope** — a deliberate word, never an "unscoped" default reachable by omission.
- **`--topology-epoch` is explicit on facts**: a fact never invents authority movement; reparenting is recorded by a new epoch.
- Issuance validates **structure and attenuation** (leaf inside issuer scope, rights within the maximum) but **not root authenticity** — successful issuance is not proof of deployability. Revocation is monotonic floors; a lower floor never rolls back.
- Key hygiene matches the org surface: 0600 secrets, `--insecure-permissions` for permissive **input** modes on Unix, `--accept-windows-dacl` for the Windows **output** warning, no CWD fallback.

**Relations, removal and leave:**

- **One active attachment per verifier.** A device holds any number of subnet relations, but only one is the active attachment at a given verifier and only it is presented. Joining a second scope there is refused unless `subnet join --switch`; `subnet activate <scope>` switches explicitly. The previous attachment is withdrawn there and stays stored; `subnet members`/status separates `active` from stored, and leaving a stored relation leaves the active one alone.
- **`subnet members` separates *issued* from *observed*.** It reports what this node issued for the scope (and subtree) and which peers are admitted here right now — never a claim about other verifiers; a member not connected here is absent, not removed.
- **Removal is per named verifier.** `subnet remove` signs a subject floor with the offline root and hands it to each `--verifier`; each reports from its own signed attestation, and `complete` holds only when every named verifier persisted it. B's next session is refused with its old credentials; a sibling in the same subnet is untouched.
- **Leave is local, durable, and not revocation.** `subnet leave <scope>` is recorded first and survives restart; the verifier is asked over the session to drop the admission (acknowledged or reported unconfirmed), and the credential stays valid until it expires or the operator removes the device.

## `net-mesh channel` — channel credentials and links

Three groups: **offline** grant authoring, **links and serving** on the running node (`--state-dir`), and **temporary-supervisor** registry reads (`visibility`/`ls`, `--local`).

| Command | What it does |
|---|---|
| `channel issue-grant --root-identity <path> --issuer <entity> --channel <name> [--rights <r,…>] [--ttl <dur>] [--out <path>] [--force]` | offline: the channel root (an operator identity file) delegates bounded publish/subscribe on one channel to an issuing node, which then mints device credentials at `join`. The root never sits on the node. |
| `channel serve <NAME> --token-root <entity>... [--state-dir <dir>]` | gates a channel on the running node — only chains anchored at a `--token-root` may subscribe. Persisted and re-applied on every `up`. |
| `channel status [--state-dir <dir>]` | the channels this node serves and, for a joined device, its own credential: subscribe ACK and publish readiness. Never a roster of other members. |
| `channel publish <NAME> --data <text> [--state-dir <dir>]` | publishes one payload (≤16 KiB) from the running node; the node's own local gate decides. Delivery counts are this node's sends, not subscriber receipts. |
| `channel leave [<NAME>] [--state-dir <dir>]` | leaves one channel relation (named, or the only active one): durable record, then an acknowledged unsubscribe and removal of exactly the installed publish credential. Named needed when several are active. |
| `channel invite <NAME> --rights <r,…> [--state-dir <dir>] [--ttl <dur>] [--require-approval] [--for <entity>] [--out <path>]` | a standalone channel link (the channel relation only) for a device already on this mesh, redeemed over its session. Needs `up --enroll` with a `--channel-grant` for the channel. |
| `channel join <TOKEN> [--state-dir <dir>] [--yes]` | redeems a standalone channel link through this device's running `up`; keeps the chain and starts using it (subscribe / publish). |
| `channel visibility <NAME>` / `channel ls` | temporary-supervisor registry reads (`--local`; `--identity`/`--node` select the supervisor). |

Things that will bite:

- **Readiness is observed, not implied.** `channel status` reports a joined device's subscribe ACK and publish readiness from the live session — a publish credential is *ready* only while this node's own channel config trusts the root, and no root is installed implicitly. `channel publish` reports the node's own gate: `gate: passed` (the production gate accepted this publish), `gate: open` (the channel is ungated here — no credential evidence), or a denial with the gate's reason. Never implied by holding a credential.
- **Channel rights** are `publish`, `subscribe`, or `publish,subscribe`. Subscribe sends the device to the issuing node as publisher, so the channel must be served there first. `ADMIN`, wildcard and delegation are never issued.
- **One credential per channel.** Rejoining after `channel leave` takes a fresh link; the spent one stays spent.
- An ungated channel is a real gate state, not a missing check: `channel publish` on it still reports `gate: open`.

## The two-node capability journey

A runnable, public acceptance example lives in `net/crates/net/cli/tests/fixtures/README.md` and is exercised by CI. It starts **two separate CLI mesh processes** on one machine (no bootstrap node): `wrap --listen` publishes a recorded Python MCP server; `mcp serve` connects from another process. It proves both permission boundaries — an unpinned consumer is refused before handler effect, explicit `mcp pin approve` grants consent, one authorized call matches exactly one provider-side record, and a pin does **not** override provider owner scope. Companion legs cover native typed calls (`serve_tool`/`call_typed`), live contract capture with offline regeneration, and a protected same-org authorization leg (denial-before-effect, then an authorized call; generated Python consumer included).

This is **two-process, two-node loopback evidence** — not two computers, not cross-org grants, not a production credential recipe, and not proof that arbitrary startup order succeeds. Invocations are never retried; a timeout does **not** prove a handler had no effect — inspect the provider record.

## Cross-references

- `dataforts.md` — the blob/dir model behind `transfer` (`BlobRef`, content-addressing, `store_dir`/`fetch_dir`, gravity).
- `nrpc.md` — the discovered-tool / typed-call surface `typegen` generates against.
- `capabilities.md` — the `ai-tool:*` capability tags `typegen` discovers.
- `mcp.md` — `wrap` / `mcp serve` / `mcp pin` / `forwarding` conceptual model.
- `org.md` — what the `net-mesh org` artifacts mean, the startup-side `install_org_authority` / `install_provider_grant_audience` calls that consume them, and the `org:<domain>:<kind>` errors. The artifacts authorize all four protected call shapes — nothing in this command tree changed for streaming — and a `Granted` streaming provider needs the same `install_provider_grant_audience` call as a unary one, staying encrypted and undiscoverable until it lands.
- `subnet-auth.md` — what the `net-mesh subnet` artifacts mean, the runtime admin surface that installs them (`install_gateway_credentials` / `declare_boundaries` / `apply_control_fact`), and the `subnet:<kind>` errors.
- `a2a.md` — the enrollment model behind `net-mesh join`/`leave` and the org/subnet/channel links (`invite → join → approve`, `InviteToken` / `JoinRequest`, no private key ever transmitted); and the runnable [enrollment journey](https://github.com/ai-2070/net/blob/master/net/crates/net/cli/tests/fixtures/enrollment/README.md).

## Further reading

- [Public CLI reference](https://ai2070.net/docs/reference/cli)
- [Two-node capability journey](https://github.com/ai-2070/net/blob/master/net/crates/net/cli/tests/fixtures/README.md)
- [Enrollment journey — managed nodes, join links, relations and leave](https://github.com/ai-2070/net/blob/master/net/crates/net/cli/tests/fixtures/enrollment/README.md)
- [Deck (Operator TUI)](https://ai2070.net/docs/reference/deck)