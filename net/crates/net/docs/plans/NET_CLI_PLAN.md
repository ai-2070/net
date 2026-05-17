## Net CLI — implementation plan

> Single `net` command-line tool that wraps the Rust SDK for one-shot operator commands, CI scripting, daemon authoring, and ad-hoc cluster inspection. The non-interactive counterpart to Deck-the-TUI: same substrate surfaces, same operator-policy gates, scriptable shape. Builds on [`MESHOS_SDK_PLAN.md`](MESHOS_SDK_PLAN.md), [`DECK_SDK_PLAN.md`](DECK_SDK_PLAN.md), [`MESHDB_PLAN.md`](MESHDB_PLAN.md), and the existing `net-blob` CLI ([`DATAFORTS_BLOB_STORAGE_PLAN.md`](DATAFORTS_BLOB_STORAGE_PLAN.md)).

## Status

**Not started.** A `cli/` workspace member exists with an empty `src/` directory and the workspace `cli` feature flag is wired (`crates/net/Cargo.toml:default = []`, `cli = ["dataforts", "redex-disk", "dep:clap"]`). One ad-hoc binary lives today at `src/bin/net-blob.rs` for the blob-storage CLI surface; it stays as-is until the unified CLI absorbs it in Phase 4.

**Activation gate.** The Rust SDK + four-language MeshOS / Deck SDK bindings are feature-complete as of the v0.17 cycle. The CLI sits on top of `ai2070-net-sdk` (no new substrate surface required). A real consumer workflow — a CI bot, a ChatOps script, an SRE runbook — that needs a non-TUI surface drives Phase 1 activation.

**Substrate prereqs** (all in code today):

- **Rust SDK** at `crates/net/sdk/` — `MeshOsDaemonSdk`, `DeckClient` + `AdminCommands` / `IceCommands` / `AuditQuery`, `MeshOsSnapshot`, `OperatorIdentity` / `OperatorRegistry` / `AdminVerifier`, `OperatorIdentity::sign_proposal` / `sign_payload`, log / failure / audit streams.
- **MeshDB SDK** — `MeshQuery` AST + sync `MeshQueryRunner` for ad-hoc `net db` queries.
- **Identity layer** at `src/adapter/net/identity/` — `EntityKeypair::from_bytes` / `generate`, file-store conventions used by Deck-the-binary's startup.
- **Existing `net-blob`** at `src/bin/net-blob.rs` + `tests/net_blob_cli.rs` — the model for an `argv → SDK call → JSON output` pipeline; Phase 4 folds its commands into the unified surface.

## Frame

Deck-the-binary is the operator's interactive console — modeful, terminal-rendered, optimized for situational awareness during incidents. It's a poor fit for:

- **CI / automation** — a step in a release pipeline that wants to commit `enter_maintenance` on a target node, wait for the snapshot to reflect `EnteringMaintenance`, and exit with a status code.
- **Shell pipelines** — `net audit recent --by-operator 0xABCD --since 1h --json | jq '.[] | select(.event_kind == "force_cutover")'`.
- **One-shot daemon authoring** — `net daemon run --name echo --seed-file ~/.net/echo.seed` to spin a Rust daemon implementation against a running MeshOS supervisor without writing a `main()`.
- **Cluster inspection from a bastion** — `net snapshot get --output yaml`, `net db latest --chain 0x… --decode`.

The CLI is **the same SDK surface as Deck and the four-language bindings, exposed as a flat argv shape with JSON / YAML / table output and a stable exit-code contract**. Every subcommand maps 1:1 onto an SDK call; nothing in the CLI bypasses the SDK or invents new substrate semantics.

## Why this exists

Five reasons for a written plan rather than "we'll wire clap into the cli crate when someone asks":

1. **The non-goals are load-bearing.** The CLI must refuse to expose anything the SDK already refuses (no direct chain mutation, no scheduler control, no daemon-registry surgery, no admin-event bypass). Calling these out up front keeps a future contributor from accidentally adding a `net debug raw-commit` escape hatch.

2. **Subcommand layout is a contract.** Once `net admin drain` exists, any rename breaks every CI script. The plan locks the top-level subcommand surface (`net daemon` / `net admin` / `net ice` / `net audit` / `net snapshot` / `net log` / `net db` / `net identity` / `net blob`) and the flag conventions (`--node`, `--operator`, `--output`, `--format`) so the matrix is stable across phases.

3. **Output format matters for pipeable scripts.** Every read subcommand supports `--output (json|yaml|table)`; every write subcommand returns a `ChainCommit` JSON object on stdout (or a typed error envelope). The plan locks both — drift between subcommands ("most return JSON but `net audit` returns table by default") burns consumers.

4. **Exit codes are the script contract.** `0 = success`, `1 = generic error`, `2 = invalid argument / unknown subcommand`, `3 = SDK error (kind discriminator in stderr)`, `4 = ICE simulation blocked the commit`, `5 = operator policy / signature verification rejected the commit`, `10+ = subcommand-specific`. The plan defines the full table and pins it so consumers can `case $? in 3) ...; 4) ...` reliably.

5. **CLI is the operator-authoring on-ramp.** A Rust daemon developer who wants to test against a real supervisor today has to write `MeshOsDaemonSdk::start` boilerplate by hand. `net daemon run --kind <factory>` should accept a factory the user registered via a registration crate (`net-daemon-factories`) and drive the lifecycle, so the developer can iterate on `Daemon` impls without owning the runtime.

## What ships

A single `net` binary, layered over `ai2070-net-sdk`. Subcommand layout:

```
net <subcommand> [<flags>] [<positional>]

Subcommands:
  daemon    Author + run + observe a MeshOS daemon.
              run        Spawn a registered daemon implementation against a
                         supervisor (in-process or remote-attached).
              ls         List registered daemons (local supervisor only).
              shutdown   Drive a graceful shutdown of a daemon by id.
              log        Tail a daemon's substrate log stream.

  admin     Signed admin-chain commits (Deck `AdminCommands` × 9).
              drain                <NODE>
              enter-maintenance    <NODE> [--drain-for <DUR>]
              exit-maintenance     <NODE>
              cordon               <NODE>
              uncordon             <NODE>
              drop-replicas        <NODE> <CHAIN>...
              invalidate-placement <NODE>
              restart-all-daemons  <NODE>
              clear-avoid-list     <NODE>

  ice       Break-glass operator surface (Deck `IceCommands` × 7).
              freeze-cluster       --ttl <DUR>
              thaw-cluster
              flush-avoid-lists    --scope (global|local:<NODE>|on-peer:<NODE>)
              force-evict-replica  <CHAIN> <VICTIM>
              force-restart-daemon <DAEMON-ID> <NAME>
              force-cutover        <CHAIN> <TARGET>
              kill-migration       <MIGRATION-ID>
              (every command runs simulate → preview → confirm → commit)

  audit     Read-only operator-audit queries (Deck `AuditQuery`).
              recent       [-n N] [--since <DUR>] [--by-operator <ID>]
                           [--force-only]
              stream       (same filters; emits a JSON line per record)

  snapshot  `MeshOsSnapshot` reads.
              get        [--output (json|yaml)]
              watch      [--interval <DUR>] [--output ndjson]
              status     (typed summary — peers / daemons / freeze state)

  log       Substrate log stream (`subscribe_logs`).
              tail       [-f] [--min-level <LVL>] [--daemon <ID>]
                         [--node <ID>] [--output (text|json)]

  failures  Substrate failure stream (`subscribe_failures`).
              tail       [--since-seq <N>] [--output (text|json)]

  db        MeshDB queries.
              run        --query <Q> | --query-file <PATH>
              latest     --chain <CHAIN>
              between    --chain <CHAIN> --start <T1> --end <T2>
              tail       --chain <CHAIN> [--from <SEQ>]

  identity  Operator + entity identity authoring.
              generate   [--out <PATH>]
              show       <PATH>
              fingerprint <PATH>
              registry add    <REGISTRY> <OP-ID> <PUBKEY-HEX>
              registry remove <REGISTRY> <OP-ID>
              registry list   <REGISTRY>

  blob      Dataforts blob CLI (existing `net-blob` absorbed in Phase 4).
              put     <PATH> [--chain <CHAIN>]
              get     <REF> [--out <PATH>]
              ls      [--chain <CHAIN>]
              rm      <REF>

  version   Print the SDK version + git revision the binary was built against.
  help      Built-in clap help (every subcommand has `-h` / `--help`).
```

### Global flags

Apply to every subcommand unless explicitly suppressed.

| Flag | Default | Purpose |
|---|---|---|
| `--config <PATH>` | `$XDG_CONFIG_HOME/net/config.toml` | Profile file with the substrate connection target + identity path. |
| `--profile <NAME>` | `default` | Named connection profile within the config file. |
| `--node <ID>` | (none) | Target node id for `admin`/`ice` subcommands. |
| `--output <FMT>` | `json` (machine-friendly) | One of `json` / `yaml` / `ndjson` / `table` / `text`. |
| `--quiet` / `-q` | off | Suppress progress diagnostics on stderr. |
| `--verbose` / `-v` | off | Tracing-subscriber `info` (`-vv` = debug, `-vvv` = trace). |
| `--no-color` | follows `$NO_COLOR` | Disable ANSI colour in table / text output. |
| `--timeout <DUR>` | 30s | Global per-call timeout. |
| `--dry-run` | off | For `admin` / `ice`: build the envelope, print it, refuse to commit. |

### Output format contracts

- **`json`** — single value on stdout, terminated by newline. Default for `admin` / `ice` / `db` / one-shot reads.
- **`ndjson`** — one JSON object per line. Default for streaming reads (`audit stream`, `snapshot watch`, `log tail`).
- **`yaml`** — for `snapshot get` and `identity show` where the structure is large + human-readable matters.
- **`table`** — ASCII / unicode bordered table. Default for `admin` / `audit recent` / `daemon ls` when stdout is a TTY.
- **`text`** — plain lines. Default for `log tail` when stdout is a TTY; `--output json` switches to ndjson.

Auto-detection rule: when `--output` is not specified, the binary picks `table` / `text` for TTY stdout and `json` / `ndjson` for non-TTY stdout. `--output` overrides.

### Exit codes (locked)

| Code | Meaning | Notes |
|---|---|---|
| 0 | Success. | Includes the dry-run path printing the envelope and exiting. |
| 1 | Generic runtime error. | Filesystem / IO / unexpected panic caught by the top-level handler. |
| 2 | Invalid arguments. | Clap's default code; passed through. |
| 3 | SDK error (substrate-side rejection). | Kind discriminator + message printed to stderr. |
| 4 | ICE simulation blocked. | `simulate_ice_proposal` rejected the proposal before commit. |
| 5 | Operator policy / signature verification rejected. | Maps every `VerifyError` variant onto this code; full kind + message on stderr. |
| 6 | Connection / handshake failure. | `start` couldn't reach the substrate node. |
| 7 | Timeout. | `--timeout` elapsed before the SDK call resolved. |
| 8 | Confirmation refused. | TTY user said "no" at the ICE preview prompt. |
| 10–19 | Subcommand-specific. | `net daemon` uses 10 for "factory not found", `net db` uses 11 for "query parse failed", etc. |

## Design

### 1. Layout

The unified CLI lives at `crates/net/cli/` (the existing empty workspace member). Wires:

```
crates/net/cli/
├── Cargo.toml         # bin = "net", path = "src/main.rs"
├── src/
│   ├── main.rs        # clap entrypoint + global flag plumbing.
│   ├── commands/
│   │   ├── daemon.rs
│   │   ├── admin.rs
│   │   ├── ice.rs
│   │   ├── audit.rs
│   │   ├── snapshot.rs
│   │   ├── log.rs
│   │   ├── failures.rs
│   │   ├── db.rs
│   │   ├── identity.rs
│   │   ├── blob.rs    # Absorbs `src/bin/net-blob.rs`.
│   │   └── version.rs
│   ├── config.rs      # Profile file parsing, env var fallback.
│   ├── output.rs      # `--output` dispatch: json / yaml / ndjson / table / text.
│   ├── confirm.rs     # ICE preview + interactive y/N gate.
│   ├── error.rs       # Exit-code mapping + stderr formatting.
│   └── prelude.rs     # Re-exports the SDK types every command imports.
└── tests/
    ├── help.rs        # `--help` output stable across subcommands.
    ├── admin.rs       # In-process supervisor + CLI invocation via `assert_cmd`.
    ├── ice.rs         # Simulate-then-commit + dry-run + confirm-refused.
    ├── audit.rs       # JSON + ndjson + table output round-trip.
    └── exit_codes.rs  # Every documented code is reachable via a controlled fixture.
```

`crates/net/cli/Cargo.toml`:
```toml
[package]
name = "net-cli"
version = "0.17.0"
publish = false

[[bin]]
name = "net"
path = "src/main.rs"

[dependencies]
ai2070-net-sdk = { path = "../sdk", features = ["meshos", "deck", "meshdb", "dataforts"] }
clap = { version = "4", features = ["derive", "env"] }
tokio = { version = "1", features = ["macros", "rt-multi-thread", "sync", "time", "signal"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
serde_yaml = "0.9"
tracing-subscriber = { version = "0.3", features = ["env-filter", "fmt"] }
color-eyre = "0.6"
comfy-table = "7"             # Pretty terminal tables.
dialoguer = "0.11"            # Interactive confirm prompts.
toml = "0.8"                  # Config file parsing.
dirs = "5"                    # XDG paths.

[dev-dependencies]
assert_cmd = "2"              # spawn the binary in tests.
predicates = "3"
```

### 2. Routing model

`main.rs` parses the global flags, builds a `CliContext` (config + connection + identity + tracing), and dispatches the subcommand. Each command module exposes a `run(ctx: &CliContext, args: <Args>) -> ExitCode` function; no command opens its own runtime or wires its own identity loading.

The `CliContext` lifecycle:

1. Parse global flags + config.
2. Lazy-initialize the tokio runtime (`#[tokio::main(flavor = "multi_thread")]` on `main`).
3. Lazy-load the operator identity from the resolved path.
4. Lazy-attach to the substrate node (in-process for tests via `--in-process`, remote attach via `--endpoint` once the substrate ships a remote-attach surface — Phase 5 deferred).

Most commands need only steps 1–3. `admin` / `ice` / `daemon run` need step 4.

### 3. ICE preview workflow

`net ice <action>` always runs simulate → preview → confirm → commit, mirroring Deck-the-binary's UI gate:

1. Build the `IceProposal` via the SDK.
2. Call `simulate()` → get `BlastRadius`.
3. Render the blast radius as a table (TTY) or JSON (non-TTY).
4. **TTY**: prompt `"Continue? (type 'YES' to confirm)"`. Anything other than literal `YES` exits with code 8.
   **Non-TTY**: require `--yes` flag; absence is exit code 8.
5. `--dry-run` short-circuits before step 4 and exits 0 with the envelope on stdout.
6. Commit; emit the resulting `ChainCommit` on stdout.

For `--operator-key <PATH>` flows (multi-operator signing): the CLI loads the local operator's key, signs the simulated proposal via `OperatorIdentity::sign_proposal`, and prints the signature as a JSON dict. The `net ice <action> --sig <JSON>` form accepts pre-collected signatures from co-operators.

### 4. Daemon authoring on-ramp

Three modes for `net daemon run`:

- **Default** — `--kind <FACTORY-ID>` resolves a factory registered in a downstream crate via the `net_daemon_factories::register!` macro (Phase 3 invention). The CLI iterates registered factories at startup.
- **Embedded** — `--exec <BINARY>` spawns a subprocess that talks to the CLI via stdio + a tiny line protocol. Lets non-Rust daemons piggyback on the CLI's lifecycle wiring. Deferred to Phase 4.
- **Probe** — `--probe` runs a no-op daemon and just reports lifecycle events on stdout. Useful for testing the supervisor connection without a real factory.

### 5. Config file shape

```toml
# ~/.config/net/config.toml

[default]
endpoint = "in-process"                       # or "tcp://<host>:<port>" once remote attach lands
identity = "~/.config/net/identities/op.toml"
log_level = "info"

[profiles.staging]
endpoint = "in-process"
identity = "~/.config/net/identities/staging-op.toml"
default_timeout_ms = 60000

[profiles.production]
endpoint = "in-process"
identity = "~/.config/net/identities/prod-op.toml"
ice_signature_threshold = 2                   # advisory; the substrate enforces
default_timeout_ms = 30000
```

`--config` / `--profile` / `NET_PROFILE` env var resolve the active profile. Every individual flag overrides the profile value.

### 6. Identity store

Operator identities are stored as TOML files:

```toml
# ~/.config/net/identities/op.toml
operator_id = "0x1234..."
seed_hex    = "..."                  # 64 hex chars (32-byte ed25519 seed)
created_at  = "2026-05-17T12:34:56Z"
note        = "Production operator for the deck-fleet cluster"
```

`net identity generate --out <PATH>` writes a new one. `net identity show <PATH>` prints the operator id + public key + creation date (never the seed). `net identity fingerprint <PATH>` prints a short SHA256-truncated identifier for inclusion in audit dashboards.

The seed file is read-only by `chmod 600`; the CLI errors with kind `permissive_mode` if the file is world-readable. Pattern lifted from `ssh-keygen`.

### 7. Wire / FFI contracts

The CLI inherits everything from the Rust SDK — no new wire formats. JSON output uses serde's default representation; specifically:

- `ChainCommit` →
  ```json
  {
    "commit_id": "0x1234...",
    "operator_id": "0x5678...",
    "event_kind": "enter_maintenance",
    "committed_at_ms": 1717084800000
  }
  ```
- `BlastRadius` → same shape every binding emits (the substrate's serde repr).
- `MeshOsSnapshot` → forwarded verbatim; `--output yaml` rewraps for readability.
- Errors → `{"kind": "...", "message": "..."}` on stderr, exit code per the table above.

### 8. Tests

Three layers:

1. **Help-text golden tests** — `assert_cmd::Command::cargo_bin("net").arg("--help")` plus every subcommand's `--help`. Goldens stored under `tests/golden/help/*.txt`; flag drift triggers a CI failure.
2. **In-process integration** — every subcommand under `tests/`. Each test spins a `net_sdk::testing::ClusterHarness`, drives the CLI via `assert_cmd`, and checks stdout / stderr / exit code. ~40 cases planned for Phase 1.
3. **Exit-code coverage** — `tests/exit_codes.rs` enumerates every documented code (0–8, sample 10–11), invokes a fixture that produces that code, and asserts the binary exits with it. Pinned so future contributors can't quietly broaden the meaning of an exit code.

### 9. Documentation

- **`docs/net-cli.md`** — operator-facing reference. Each subcommand's flags, output shape, example invocation, exit codes, env vars. Format lifted from `git-scm.com`-style man pages.
- **`docs/net-cli-cookbook.md`** — recipes. Common CI patterns (drain-and-wait, audit-since-deploy, ICE preview in dry-run), shell snippets, jq one-liners.
- **`--help` text** — `clap`'s built-in help is the canonical short form; the markdown docs link out for deeper context.
- **`man net`** — generated from clap via `clap_mangen` in CI; shipped under `docs/man/`.

## Locked decisions

Lock these so phase implementations don't relitigate:

1. **CLI is the same SDK surface as Deck and the four-language bindings.** Nothing in the CLI bypasses the SDK or invents new substrate semantics. If a command can't be expressed as an SDK call, it doesn't ship.

2. **Output is structured by default for non-TTY stdout, table for TTY.** Auto-detection follows `is-terminal::is_terminal(stdout)`. `--output` always wins. Goldened.

3. **Exit codes are a stable contract.** The table above is locked; broadening a code's meaning (e.g. reusing 4 for non-ICE failures) requires a major version bump.

4. **ICE writes go through simulate + confirm.** No `--yes` flag bypasses the simulate step. `--dry-run` skips the commit; `--yes` skips the interactive prompt only. Substrate-side `SimulationRequired` enforcement is the backstop.

5. **Identity files are TOML with the seed inline.** Format pinned to the layout in §6. Re-using the SSH-style separate-pubkey-file split was considered and rejected; the CLI's audience is operators who already manage TOML config.

6. **Subcommand names are kebab-case** (`enter-maintenance`, `flush-avoid-lists`). Hyphen-vs-underscore drift is the #1 CLI papercut; pinning now keeps consumer scripts working across versions.

7. **No interactive REPL mode.** Deck is the interactive UI; the CLI is one-shot. A consumer who wants a REPL drives the SDK directly.

8. **No raw chain mutation.** Even with `--unsafe-i-know-what-im-doing` or any escape hatch — those don't exist. Every write rides the admin or ICE commit path.

9. **No daemon-registry surgery.** `net daemon ls` reads the snapshot; there's no `net daemon force-unregister`. Drain or force-restart through the admin / ICE paths instead.

10. **`net-blob` is absorbed in Phase 4.** Until then, both binaries ship; after, `net-blob` becomes an alias that prints a deprecation warning and forwards to `net blob`.

## Phases

Activation order, dependency-driven:

- **Phase 1 — Scaffolding + read-only surface.** `cli/Cargo.toml` + `src/main.rs` with clap routing for `net version`, `net identity (generate|show|fingerprint)`, `net snapshot get`, `net snapshot status`, `net audit recent` / `audit stream`, `net log tail`, `net failures tail`, `net daemon ls`. Config + global flags + output dispatch. Exit-code table. Help-text goldens. Wires nothing that mutates substrate state — purely read + identity authoring.

- **Phase 2 — Admin write surface.** `net admin (drain|enter-maintenance|exit-maintenance|cordon|uncordon|drop-replicas|invalidate-placement|restart-all-daemons|clear-avoid-list)`. `--dry-run` support. Identity loading + commit envelope construction. Integration tests for every admin variant.

- **Phase 3 — ICE break-glass surface.** `net ice (freeze-cluster|thaw-cluster|flush-avoid-lists|force-evict-replica|force-restart-daemon|force-cutover|kill-migration)`. Simulate → preview table → confirm gate → commit. `--yes` for non-interactive flows; `--sig` for pre-collected signatures. `net identity registry (add|remove|list)`. ICE-specific integration tests + the exit-code-8 coverage.

- **Phase 4 — Daemon run + blob absorption.** `net daemon run --kind <FACTORY-ID>` with the `net_daemon_factories::register!` macro inventory. `net daemon shutdown`. `net daemon log`. `net blob` absorbs `net-blob`'s `put` / `get` / `ls` / `rm` surface. `net-blob` becomes a forwarding shim.

- **Phase 5 — Remote attach.** `--endpoint tcp://host:port` for talking to a substrate node from a different process. **Substrate-side prereq**: a remote-attach handshake doesn't exist yet — this phase is gated on `MESHOS_PLAN.md`'s remote-attach work landing first.

Phases 1–4 land independently of each other; Phase 5 has a hard substrate prereq.

## Non-goals

Per the scope brief, the CLI is not:

- An interactive TUI (that's Deck).
- A scheduler / replica-manipulation surface.
- A topology / node-lifecycle surface (no `net node add` / `net node remove`).
- A raw chain mutation tool (no escape hatches).
- A workflow / scripting engine (compose via shell pipelines + jq; the CLI emits structured output).
- A monitoring / alerting product (consumers pipe the output to their existing systems).
- A daemon-registry editor.
- A capability-system editor (capabilities flow through the SDK's `publish_capabilities` path — stubbed substrate-side today).

The CLI is **the operator-and-developer command surface, exposed as argv**. Everything else stays in the SDK, in Deck, or in adjacent tooling.

## Interaction surfaces

The CLI interacts with one substrate system per layer:

- **Rust SDK** (`ai2070-net-sdk`) — every read + write. The CLI never reaches around the SDK.
- **Local filesystem** — config + identity files; the binary follows XDG.
- **Local terminal** — TTY detection, ANSI colour, interactive confirm prompts.

It explicitly does NOT interact with:

- **The substrate's `MeshOsRuntime` directly.** The SDK is the only path.
- **The Deck binary.** They're peers; CLI doesn't shell out to Deck.
- **The four-language bindings.** Those serve other-language consumers; the CLI is Rust-only.

## Test surface

- **Per-subcommand integration tests** — `cargo test -p net-cli` runs the harness-backed suite. Each subcommand has ≥3 tests (happy path, typed-error path, output-format variant).
- **Help-text goldens** — `tests/golden/help/*.txt` pinned; updates require a deliberate `cargo test -- --bless` (or a CI workflow check that surfaces drift in the PR diff).
- **Exit-code coverage** — every documented exit code has a fixture that produces it; `tests/exit_codes.rs` enumerates the assertions.
- **`man net` generation** — CI runs `clap_mangen` and diffs against the committed `docs/man/net.1`; manpage drift surfaces as a PR diff.
- **Cross-binding parity** — out of scope (the CLI is a single-language tool); the four-language SDK bindings have their own parity test.

---

*v0.17 cycle release candidate. Gates on a real consumer workflow (CI bot / ChatOps / SRE runbook). Phases 1–4 land independently; Phase 5 (remote attach) waits on substrate work. The `net-blob` binary stays in place until Phase 4 absorbs it.*
