# Net CLI — current implementation and next bounded work

**Status:** Refreshed plan; implementation is not authorized by this document refresh.
**Source baseline:** `d7b749983199012978e69867f3c3694c4df96f69` on `master`, reviewed 2026-09-20.
**Package / executable:** `net-cli` / `net-mesh`; package version at the baseline is `0.36.0`.
**Goal:** Make the existing CLI's execution targets and supported workflows explicit, repair the nearest destructive failure path, then complete useful workflows using existing SDK mechanisms.
**Architecture:** Retain the existing Rust/Clap command modules and typed SDK clients. Distinguish offline authoring, persistent local stores, fresh in-process supervisors, and real mesh clients; none is an implicit substitute for another.
**Tech stack:** Rust, Clap, Tokio, `net-mesh-sdk`, the existing MCP adapter, and existing storage/transport implementations.

> For Hermes: after explicit implementation authorization, use the subagent-driven-development skill for the accepted slice, with independent review before acceptance.

This replaces the old v0.17-era five-phase proposal. Its historical command sketches and “locked decisions” are not current contracts. Consult Git history for that proposal; do not implement its unshipped inventory by rote. Existing released behavior is the compatibility baseline unless a slice below explicitly changes it.

Only this plan was edited during the refresh. Findings are based on source, test-source, and CI-configuration inspection, including two independent read-only reviews. No CLI binary, Cargo build, test suite, or deployment was run. “Present” below means reachable in the source command tree, not independently accepted at runtime or confirmed published in a registry.

## 1. Current execution model

The CLI already exists. The central distinction is **what a command actually operates on**.

| Mode | Meaning | Examples |
|---|---|---|
| Offline | Authors or inspects local files; no live mesh implied | Identity/org/subnet issuance, capability announcement artifact, saved typegen input |
| Persistent local | Opens a local on-disk store or policy | `netdb`, MCP pins, forwarding policy, staged transfer content |
| Fresh supervisor | Starts a new `MeshOsDaemonSdk` for this invocation and uses its `DeckClient` | Original admin/ICE, audit/log, peer/daemon, and snapshot families |
| Mesh client | Creates a local mesh participant and connects to an explicitly resolved remote target | Remote aggregator operations, transfer receive/admin, live typegen |
| Hosted service | Keeps a mesh participant and a service/subprocess alive | `wrap`, `mcp serve`, feature-enabled `anchor serve` |

### Source boundaries

- `net/crates/net/cli/src/main.rs:142–224,284–330` is the executable command tree and dispatch, not `commands/mod.rs` or old design prose.
- `net/crates/net/cli/src/context.rs:165–272` creates a fresh supervisor and local Deck client. Optional mesh attachment is a separate handle; it does **not** turn that Deck client into a remote Deck client.
- `context.rs:172–180` rejects `profile.endpoint` values other than `in-process`. Real mesh clients instead use `node_addr`, `node_pubkey`, `node_id`, and `psk_hex`, resolved from flags/profile.
- `context.rs:295–335` binds the generic short-lived mesh client to `127.0.0.1:0`. Loopback fixtures do not establish off-host usability. `build_attached_mesh` also exists for service paths with an explicit bind address.
- `net/crates/net/cli/src/commands/snapshot.rs` already refuses implicit deployment reads: `get` and `status` require `--local`. Several sibling fresh-supervisor commands do not yet have an equivalent gate.

**Consequences:** A local commit is not proof of a deployed-node mutation. An empty local snapshot is not proof of an empty deployment. A successful mesh handshake is not permission to invoke every service. Real remote attachment is present, but general remote Deck administration is not.

## 2. Source inventory

Paths in this table are relative to `net/crates/net/cli/`, except explicitly repo-root paths. Existing tests are evidence locations, not a claim that they ran during this refresh. Built-in `help`, completion, and man output follow the Clap tree.

| Family | Reachable verbs / behavior | Boundary and limitations | Source / existing test evidence |
|---|---|---|---|
| `version` | Version/build metadata | Offline | `src/commands/version.rs`; `tests/help.rs` |
| `identity` | `generate`, `show`, `fingerprint`, `revoke` | Local key/revocation files; no operator-registry verbs or automatic distributed revocation | `src/commands/identity.rs`; `tests/seed_publish_guard.rs`, `tests/exit_codes.rs` |
| `admin` | `drain`, `enter-maintenance`, `exit-maintenance`, `cordon`, `uncordon`, `drop-replicas`, `invalidate-placement`, `restart-all-daemons`, `clear-avoid-list` | Fresh supervisor. Dry-run previews before loading config/identity; commit requires identity | `src/commands/admin.rs`; `tests/dry_run.rs` covers previews, not remote effects |
| `ice` | `freeze-cluster`, `thaw-cluster`, `flush-avoid-lists`, `force-evict-replica`, `force-restart-daemon`, `force-cutover`, `kill-migration` | Simulation/commit against fresh supervisor; not a configured deployment operator registry | `src/commands/ice.rs`; in-file confirmation tests and `tests/exit_codes.rs` |
| `snapshot` | `get`, `status` | Fresh supervisor, explicit `--local`; no `watch` | `src/commands/snapshot.rs`; `tests/snapshot_no_false_success.rs` |
| `audit` | `recent`, `stream` | Fresh supervisor audit ring. `--since` is a sequence; time range uses `--start-ms` / `--end-ms` | `src/commands/audit.rs` |
| `log`, `failures` | `tail` | Fresh supervisor streams, not a deployed node's history. Log tail follows regardless of accepted `-f` | `src/commands/logs.rs` |
| `cap` | `show`, `query`, `nodes`, `announce` | Reads use fresh snapshot; query matches required tags. Announce creates signed JSON offline, not a live broadcast | `src/commands/cap.rs`; `tests/cap_announce.rs` |
| `peer`, `daemon` | `ls` | Fresh snapshot only; no daemon launch/shutdown/migration or peer NAT verbs | `src/main.rs:238–251`; `src/commands/peer.rs`, `src/commands/daemon.rs` |
| `netdb` | Task list/create/rename/complete/delete; memory list/store/retag/pin/unpin/delete; snapshot/restore | Persistent local RedEX directory; detailed limits below | `src/commands/netdb.rs` |
| `org` | `keygen`, `issue-cert`, `issue-floors`, `grant-dispatcher`, `grant-capability` | Offline signed artifacts, not live onboarding. Grant plus audience-secret publication is not a crash-atomic two-file transaction | `src/commands/org.rs`; `tests/org_adopt.rs`, `tests/org_grant.rs` |
| `node` | `adopt` | Local authority provisioning; no remote node lifecycle control | `src/commands/node.rs`; `tests/org_adopt.rs` |
| `subnet` | `show`, `ls`, `tree`; `keygen`, `issue-direct`, `issue-issuer`, `issue-delegated`, `issue-control-fact`, `inspect` | Reads use fresh supervisor; issuance/inspection is offline. Control facts cover descriptor, gateway advertisement, export policy, revocation floor. Inspect decodes; it does not verify signatures | `src/commands/subnet.rs`; `tests/subnet_issuance.rs` |
| `gateway` | `stats`, `exports`, `export` | Fresh context for reads. Export parses inputs but refuses because no live gateway is attached; not an implemented mutation | `src/commands/gateway.rs` |
| `channel` | `visibility`, `ls` | Fresh context without an attached deployment registry | `src/commands/channel.rs` |
| `aggregator` | `inspect`, `query`, `ls`, `spawn`, `scale` | Inspect/default list use fresh supervisor; query/spawn/scale and remote list use real typed RPC. Profile remote fields alone do not select remote `ls` | `src/commands/aggregator.rs`; `tests/aggregator_remote.rs` (query fixture is ignored) |
| `transfer` | `recv-blob`, `send-blob`, `recv-dir`, `send-dir`, `ls`, `status`, `cancel` | Receive/admin attach to mesh. Send computes references and optionally stages a local store; it neither pushes nor hosts that store after exit | `src/commands/transfer.rs`; `tests/transfer_cli_blob.rs`, `tests/transfer_cli_dir.rs`, `tests/transfer_cli_admin.rs` |
| `wrap` | Direct command with subprocess arguments | Mesh-hosted stdio MCP tools, owner-only by default with explicit policy options | `src/commands/wrap.rs`; adapter-level `net/crates/net/adapters/mcp/tests/wrap_end_to_end.rs` |
| `mcp` | `serve`, `pin approve/reject/list` | Serve is a mesh-connected stdio bridge; pin management is local | `src/commands/mcp.rs`; `tests/mcp_pin.rs` |
| `forwarding` | `enable`, `disable`, `allow`, `rm`, `audit`, `set-value` | Local caller policy; audit is a value-free policy report. Set-value requires optional `keychain` feature | `src/commands/forwarding.rs`; `Cargo.toml` |
| `typegen` | `generate`, `snapshot`, `diff` | Live discovery or offline saved input; TS/Python generation. Live path does not fetch oversized schemas | `src/commands/typegen/mod.rs`; `tests/typegen_cli_ts.rs`, `tests/typegen_cli_python.rs`, `tests/typegen_cli_diff.rs`, downstream tests |
| `anchor` | `credential mint/inspect`; feature-gated `ls`, `stats`, `serve` | Offline credentials; real remote directory/stats and hosted browser bootstrap under `rtc-bootstrap` | `src/commands/anchor.rs`; `tests/anchor_credential.rs`, `tests/anchor_ls_live.rs`, `tests/anchor_stats_live.rs` |
| `completion`, `man` | Generate shell completion / troff | Offline, derived from Clap | `src/commands/completion.rs`, `src/commands/man.rs` |

### Not exposed by the command tree

`rpc`, `blob`, `db`, and `port` have comment-only design modules. Their presence in `commands/mod.rs` does not expose commands. Do not document their old sketches as usable syntax.

Also absent: `daemon run/shutdown/log`, `snapshot watch`, identity registry editing, peer reflex/NAT management, and the old NetDB predicate DSL. The standalone `net/crates/net/src/bin/net-blob.rs` and `net/crates/net/tests/net_blob_cli.rs` remain; absorption has not occurred.

The old “a live Mesh does not exist yet” explanation in several stubs is obsolete. That removes one prerequisite, not every command's target, authority, or lifecycle design work.

## 3. Contracts to preserve, correct, or stop promising

### Naming, packaging, and dependency boundaries

- Keep executable `net-mesh`, package `net-cli`, environment prefix `NET_MESH_`, and platform config directory `net-mesh`.
- The CLI is not strictly SDK-only: `cli/Cargo.toml` deliberately includes a narrow core dependency for offline subnet issuance and links the MCP adapter. Do not expand this into arbitrary runtime internals or pretend the issuer boundary is absent.
- `webrtc`, `rtc-bootstrap`, and `keychain` are opt-in. Anchor live verbs require `rtc-bootstrap`; standard CLI packaging is not silently widened by this plan.
- No universal factory registry, new remote Deck protocol, payment layer, or agent persistence framework is required by the immediate work.

### Output and exit codes

Source contract: `src/output.rs`, `src/error.rs`, and each command renderer.

| Code | Baseline meaning |
|---|---|
| 0 | Success |
| 1 | Generic error |
| 2 | Invalid arguments |
| 3 | SDK error |
| 4 | ICE simulation blocked |
| 5 | Operator-policy rejected |
| 6 | Connection failure |
| 7 | Timeout |
| 8 | Confirmation refused |
| 10 | Reserved daemon-factory error |
| 11 | Reserved MeshDB query parse error |
| 12 | Reserved predicate parse error |
| 13 | Invalid ICE signature |
| 14 | Breaking typegen schema change with `diff --exit-code` |

Old identity codes 17/18/19 are not implemented. Reserved variants are not reachable-feature proof. Preserve actual codes unless a separately reviewed compatibility change is needed.

One-shot format resolution chooses table for TTY and JSON otherwise; streaming chooses text/NDJSON. Generic table rendering can fall back to JSON. Main prints plain `net-mesh: ...` errors on stderr, not JSON error envelopes. ICE may print preview and commit result separately; do not promise every successful command produces exactly one JSON value. On interactive stdin, ICE still requires typed `YES` even with `--yes`; dry-run exits before confirmation.

Global `--timeout` is parsed but not forwarded by `dispatch`. It is not an enforced universal deadline. Timeout propagation is a distinct follow-up, not a guarantee to copy from the old plan.

### NetDB behavior

- Store precedence: explicit `--store`, profile `netdb`, then `dirs::data_dir()/net-mesh/netdb`. The old always-explicit-store rule was not implemented.
- Task/memory creation takes a caller-supplied ID. Reads refuse a missing directory instead of creating an empty one. There is no list predicate DSL.
- Restore requires explicit origin or acknowledged origin zero. `--force` permits merge; `--clear` removes the destination before replacement. This is not transactional restore.
- **Source finding:** `run_restore` removes the destination at `netdb.rs:548–554`, before snapshot stat/read/decode at `:610–633`, and before rejecting a snapshot with neither adapter at `:656–659`.
- **Related source finding:** dispatch converts `resolve_profile(...).await` to `.ok()` at `:253–256`. A configuration parse/read/permission error can discard the configured target and allow fallback. This differs from intentionally supported absence of an optional config file.

### Evidence and docs are not implementation

- `tests/help.rs` deliberately uses structural assertions, not help goldens; its explicit family list is not a complete inventory.
- `tests/readme_commands.rs` proves selected README argv paths resolve using `--help`; it does not prove advertised effects or remote targeting.
- Snapshot's false-success fix is present. Extend its principle only where still missing; do not re-plan that fix as absent.
- `web/src/content/docs/reference/cli.md` overstates generic live-node attachment, daemon run, and metadata fetching. Transfer argv/progress descriptions need comparison with the parser. The crate README still has deployment-sounding local admin/log examples.
- Historical [transfer](TRANSFER_CLI_PLAN.md), [typegen](TYPEGEN_CLI_PLAN.md), and [aggregator](AGGREGATOR_CLI_REMOTE_ATTACH_AND_SCALE_RPC.md) plans are provenance, not proof that every example or closure claim remains correct. Their status does not override this inventory.

## 4. Recommended order

| Slice | Outcome | Reason for order |
|---|---|---|
| **CLI-1 — NetDB restore preflight** | Invalid source/configuration cannot erase or redirect a store before restoration starts | Immediate bounded data-safety correction; detailed below |
| **CLI-2 — Explicit fresh-supervisor scope** | Operator scripts cannot mistake new empty runtimes for deployed state | Extend existing `snapshot --local` precedent; keep remote Deck work out |
| **CLI-3 — Complete live typegen inputs** | Out-of-line schemas produce usable snapshots and bindings | First feature-completion slice; metadata SDK already exists |
| Later — transfer holder | Stage, host, and fetch with shipped CLI processes alone | Needs lifecycle, exposure policy, and non-loopback proof |
| Later — unary RPC | Invoke a known service from shell/CI | Reuse current Mesh and typed calls; no invented method/codec abstraction |

These are independent reviewable slices, not an all-or-nothing release train. **CLI-1 is the next proposed implementation authorization.** CLI-2/3 are follow-ups, not permission to expand CLI-1. Source safety findings take precedence over adding command families.

## 5. CLI-1 — next bounded implementation plan

### Goal and limit

For `net-mesh netdb restore`, invalid configuration or unusable snapshot input must fail before mutating the destination. Preserve origin rules, store precedence for valid configuration, merge/clear distinction, and successful output shape.

This is a **preflight ordering repair**, not atomic/crash-safe replacement, a journal, live-store migration, or a RedEX redesign. Once valid restoration begins, interruption or storage failure may still leave incomplete state. Operate only on an offline store; this slice does not establish safe concurrent writers.

### Files

Modify:
- `net/crates/net/cli/src/commands/netdb.rs` — profile error propagation, restore preflight order, affected help/error wording.
- `net/crates/net/cli/CHANGELOG.md` — behavior/safety note.

Create:
- `net/crates/net/cli/tests/netdb_restore_preflight.rs` — real CLI subprocess witnesses with disposable stores and isolated config/data locations.

No SDK/core/wire changes, new dependencies, command renames, new exit codes, or CI feature-set changes. New CLI integration files are auto-discovered. Private helper factoring within the module is implementer judgment, not an invitation to restructure unrelated commands.

### Required behavior

1. Propagate profile-loading errors instead of discarding them with `.ok()`, once in shared NetDB dispatch. Preserve the loader's optional-absent-file behavior. Unknown-profile and explicit-missing-config semantics are separate config-contract questions.
2. Resolve origin/destination without creating, opening for mutation, clearing, or restoring the destination.
3. Read the source once into owned input, enforce the existing size ceiling, decode, and reject a snapshot with neither adapter **before** destination mutation. Check the actual bytes read as well as any preliminary metadata length. This does not claim the existing ceiling bounds every decoder allocation.
4. Reuse the validated snapshot. Do not reopen the source after clearing. A valid snapshot inside the destination must be captured before its original path is deleted.
5. Only after preflight succeeds, perform existing destination checks and chosen merge/clear operation. Inspection failures remain errors, not “empty.” Error advice must distinguish merge (`--force`) from replacement (`--clear`).
6. Preserve tasks-only/memories-only restoration and origin acknowledgment. Do not fabricate a missing adapter.
7. Preflight failure returns nonzero with no restore-success payload, leaving the named destination and fallback/default store unchanged.

The guarantee covers source/configuration preflight failures. It does not promise preservation after deletion, persistent-store open, or replay begins. Atomic replacement requires a separate publication/rollback design.

### Ordered tasks and acceptance witnesses

**Task A — destructive failure RED.** Create a genuine store through CLI/public SDK fixtures, close it, and capture relative file names and bytes. Add:

- `missing_snapshot_does_not_clear_existing_store`
- `malformed_snapshot_does_not_clear_existing_store`
- `oversized_snapshot_does_not_clear_existing_store`
- `snapshot_without_adapters_does_not_clear_existing_store`
- `invalid_snapshot_does_not_create_absent_destination`

Use disposable paths and owner-only config fixtures on Unix. Use a sparse or injected-size fixture for the large ceiling, not a multi-gigabyte allocation. Assert nonzero exit, no success payload, unchanged file inventory/bytes, and readable original records. Capture bytes before reopening for logical verification so verification cannot change the comparison baseline.

**Task B — incorrect-target fallback RED.** Add `invalid_profile_does_not_fall_back_or_mutate_store`. Malformed config must fail before touching explicit, profile, or default stores; exercise a mutation and restore. Isolate platform data directories, never the real operator store. Add a meaningful Unix permission-denial case; distinguish Windows coverage.

**Task C — minimum correction.** Propagate errors and move source loading/validation ahead of destination mutation. Run the exact witnesses to GREEN. No general NetDB/config architecture cleanup.

**Task D — positive controls.** Add:

- `valid_clear_restores_snapshot_after_preflight` — old records replaced, new records readable.
- `snapshot_inside_destination_is_loaded_before_clear` — captured once and successfully restored.
- `force_without_clear_preserves_merge_semantics` — existing chains retained under the current merge contract.
- `valid_profile_store_is_honored` — configured destination still works.
- Tasks-only/memories-only, explicit origin, and origin-zero acknowledgment controls.

**Task E — inverse and regression checks.** In a disposable review worktree, restore old clear-before-read ordering: missing/malformed witnesses must fail. Restore swallowed profile errors: the profile witness must fail. Restore the candidate, run focused and existing regression targets, and document the remaining non-atomic application boundary. Never commit inverse mutations.

### Planned verification commands

Run from `net/crates/net/`. These are **implementation gates, not results of this refresh**.

```sh
# Existing crate/build target; checks CLI rather than the root lib alone.
cargo check -p net-cli --all-targets

# NEW DELIVERABLE: valid only after Task A creates this integration file.
cargo nextest run -p net-cli --test netdb_restore_preflight --no-tests=fail --retries 0

# Existing regression targets.
cargo nextest run -p net-cli --test exit_codes --test seed_publish_guard --test snapshot_no_false_success --no-tests=fail --retries 0

# One default-feature CLI sweep after focused closure, matching CI package scope.
cargo test -p net-cli
cargo fmt --all -- --check
cargo clippy -p net-cli --bin net-mesh -- -D warnings
```

Verify target discovery and nonzero counts. The CLI has no library target. Typegen downstream tests require TypeScript and mypy/Pydantic; report unrelated local skips honestly. Windows success does not establish Unix permission behavior. Required exact-head CI and repository pre-push checks remain implementation/release gates.

## 6. CLI-2 — explicit invocation scope

**Outcome:** Fresh-supervisor reads/streams and admin/ICE operations cannot masquerade as live deployment operations.

Reuse `snapshot --local`: default refusal where deployment attachment is unsupported; explicit `--local` opt-in for development-only operations with a scope diagnostic. Keep admin's offline `--dry-run` useful without implying contact with a node. Do not gate existing real mesh clients or offline issuance as local demonstrations.

Affected fresh-supervisor paths: `admin.rs`, `ice.rs`, `audit.rs`, `logs.rs`, `cap.rs`, `peer.rs`, `daemon.rs`, `subnet.rs`, `gateway.rs`, `channel.rs`, and local `aggregator.rs` branches. Keep `gateway export` unsupported. Define aggregator list selection from flags/profile; no local fallback after failed remote connection.

Acceptance: defaults refuse without plausible deployment success data; explicit local calls work and disclose scope; remote aggregator/transfer/typegen fixtures remain remote; help/man/completion and crate README/public reference agree. Correct snapshot's suggestion of `peer` as a live alternative. Preserve source/target distinctions without silently breaking existing machine payload shapes.

This changes scripts relying on implicit local contexts and needs a changelog note. It does **not** implement remote Deck or another control plane. Implementers may split by command family; do not call the CLI-wide boundary complete before covering the inventory.

## 7. CLI-3 — feature completion: live typegen

**Workflow:** Discover selected tools, capture full descriptors, stop the provider, and generate usable TypeScript/Python bindings from the snapshot.

### Reuse map

| Existing mechanism | Missing integration | Evidence |
|---|---|---|
| Remote attach and `Mesh::list_tools` | Wait for selected tools, not the first unrelated descriptor | `cli/src/commands/typegen/mod.rs:419–490` |
| `TOOL_METADATA_FETCH_SERVICE`, `ToolMetadataRequest`, `ToolMetadataResponse` | Fetch full schemas from an advertising provider | `sdk/src/tool.rs`; call shape in `sdk/tests/tool_serve_round_trip.rs:152–184` |
| `Mesh::call_typed` and JSON codec | Existing typed metadata RPC with exact provider attribution | `sdk/src/mesh_rpc.rs:462` |
| Snapshot format/offline source | Persist hydrated descriptors; keep offline regeneration independent of attachment | `cli/src/commands/typegen/mod.rs:288–344,379–414` |
| TS/Python renderers and downstream checks | Put a real live-acquisition test ahead of offline generation checks | `cli/tests/typegen_downstream_ts.rs`, `cli/tests/typegen_downstream_python.rs` |

Paths in this map are relative to `net/crates/net/`.

Current generation skips missing inline input schemas and says metadata fetch has not shipped (`mod.rs:213–230`), although the SDK service exists. Discovery stops at the first unfiltered nonempty list or five seconds (`:462–472`). Selectors are ANY within tags, ANY within IDs, and intersection between the groups; public prose incorrectly says union. Preserve implementation selection and correct docs rather than silently widen scope.

### Proposed bounded contract

- Await explicitly requested IDs within a discovery budget; unrelated arrivals cannot end that wait.
- Tag-only/unfiltered discovery is a bounded mesh observation, **not a globally complete inventory**. A timeout cannot certify absence.
- Hydrate selected incomplete descriptors at an exact advertising provider. Check returned identity/version against the selected descriptor. Establish provider provenance from live capability data; do not assume `list_tools` retains it.
- Missing requested input, failed hydration, or unusable selected input contract fails before writing final snapshot/generated output. Do not succeed after silently skipping requested work. Preserve genuinely optional output-schema semantics.
- Retain snapshot v1 unless a required provenance change needs a format bump. No new metadata service, schema IR, or renderer framework.
- Specify bounded discovery/fetch timeout without pretending global timeout already works; a global propagation repair remains separate.

Acceptance: real SDK host and CLI subprocess; oversized schemas retrieved through metadata RPC; requested tool arriving after unrelated tool; NotFound/timeout/provider mismatch failures without final output; deterministic offline regeneration after host shutdown; existing downstream language checks. Loopback tests prove integration, not cross-computer reachability.

Likely changes: `net/crates/net/cli/src/commands/typegen/mod.rs`, existing typegen tests, `net/crates/net/docs/cli/TYPEGEN.md`, `web/src/content/docs/reference/cli.md`, CLI changelog. New deliverable: `net/crates/net/cli/tests/typegen_live_metadata.rs`. Resolve exact provider/version-conflict behavior in its implementation brief; this refresh does not authorize SDK widening if existing public queries lack necessary provenance.

## 8. Deferred work and non-goals

- **Transfer holder:** Reopen staged store and use `net_sdk::transport::serve_blob_transfer`. Prove separate stage/holder/receiver CLI processes, restart, missing content, cancellation, and non-loopback connectivity. Local staging is not publication; remote administration is not enabled by default.
- **Unary RPC:** Reuse `Mesh::call`, `call_service`, `call_typed`, and current attach/profile plumbing. The SDK takes one service name, not a universal service/method pair. Start with explicit JSON and no automatic retry of ambiguous execution. Defer streaming, duplex, arbitrary codec negotiation, and generic provider hosting.
- **Remote Deck:** Requires real provider endpoints, deployed state ownership, operator verification, and client targeting. Mesh attachment alone is not remote Deck. Start only for a named workflow.
- **Aggregator query:** Its ignored integration fixture is an open evidence gap, not a proven workflow. Resolve actual addressing/lifecycle prerequisites before un-ignoring.
- **Browser enrollment:** Anchor bootstrap/credentials exist; production issuer/enrollment policy cannot be supplied by copying a permissive demo handler.
- **Config/deadlines:** Global timeout propagation, unknown-profile fallback, cross-host bind selection, and concurrent stable-identity invocations need bounded contracts and witnesses, not hidden changes inside unrelated commands.
- **Crash-safe NetDB replacement:** Staged construction, publication/rollback, and concurrent-writer ownership are outside CLI-1. Preflight does not supply those guarantees.
- **Agent migration:** No filesystem sync, session failover, memory journaling, or harness adapters. This refresh is separate from the earlier migration discussion.
- **Old wish list:** Daemon factory registries, all MeshDB/NetDB predicate commands, blob absorption, metrics/bench tools, broad SDK parity, and a universal workflow engine are not automatic obligations.

## 9. Validation and handoff

### Refresh validation

Check Markdown structure, relative links, referenced current paths, new-target labeling, and `git diff --check`; verify only this plan changed and HEAD remains pinned. No implementation, build/test run, commit, push, or release is part of this refresh.

### Implementation evidence

Use current `AGENTS.md`, `TESTS.md`, and `.github/workflows/ci.yml`, not old copied feature lists. CLI CI runs `cargo test -p net-cli` plus a separate `rtc-bootstrap` run; new CLI integration files are auto-discovered. Keep optional anchor/keychain coverage distinct from default coverage. Do not broaden build features for a narrow CLI fix.

Handoffs must name exact HEAD, changed files, executed tests/counts, inverse witnesses, platform limitations, and remaining CI. Source audit is not runtime acceptance; parser success is not a working operator workflow.

### Key references

- [CLI package](../../../net/crates/net/cli/Cargo.toml)
- [Parser and dispatch](../../../net/crates/net/cli/src/main.rs)
- [Context and mesh attachment](../../../net/crates/net/cli/src/context.rs)
- [NetDB implementation](../../../net/crates/net/cli/src/commands/netdb.rs)
- [Existing snapshot refusal witnesses](../../../net/crates/net/cli/tests/snapshot_no_false_success.rs)
- [Typegen implementation](../../../net/crates/net/cli/src/commands/typegen/mod.rs)
- [SDK metadata round trip](../../../net/crates/net/sdk/tests/tool_serve_round_trip.rs)
- [Public reference — later reconciliation required](../../../web/src/content/docs/reference/cli.md)
- [Repository test guide](../../../TESTS.md)
- [CI configuration](../../../.github/workflows/ci.yml)
