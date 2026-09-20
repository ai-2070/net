# Net CLI — current implementation and next bounded work

**Status:** Implementation authorized in the working session. CLI-1 preflight/durable restoration, DOC-0, and CLI-2A are implemented and locally verified; CLI-2B is next. Platform CI and the requested two-node CI journey remain outstanding; off-host readiness is not claimed.
**Source baseline:** `d7b749983199012978e69867f3c3694c4df96f69` on `master`, reviewed 2026-09-20.
**Package / executable:** `net-cli` / `net-mesh`; package version at the baseline is `0.36.0`.
**Goal:** Enable capability publishers and consumers to expose, discover, and invoke an authorized capability across processes and computers, know where each operation runs, and capture reusable typed contracts. Repair destructive preflight behavior and misleading execution semantics on the way, using existing SDK mechanisms.
**Primary audience:** Capability publishers and consumers. MCP is one compatibility path; native typed consumption is also part of the destination. Artifact transfer supports this workflow, so live typegen precedes the transfer holder.
**Plan revision checkout:** `66771b27775fb8b029a7d081e51c360b0e96ed37`; the CLI, SDK, and public CLI reference are unchanged from the source baseline above. This revision changes the plan only, not shipped behavior or public documentation.
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

Global `--timeout` is parsed but not forwarded by `dispatch`. It is not an enforced universal deadline. These are baseline behaviors, not desired permanent contracts: CLI-DX below owns timeout propagation, output framing, and confirmation changes. Do not present its planned behavior as shipped.

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

## 4. Developer destination and recommended order

### End-to-end journey — define now, accept as prerequisites close

A new developer must be able to complete one useful authorized capability workflow from public instructions, identify where every command ran, and distinguish a local artifact from an effect on the mesh without reading this plan or source code.

1. **Prepare two participants.** State runtime/build prerequisites, feature requirements, identity, bind address, target selection, and authority setup. Explain identity and permissions at the step that needs them; do not begin with temporary-supervisor administration.
2. **Expose a small capability.** Start a provider with a deterministic response and an observable provider-side invocation record. The MCP compatibility route uses `wrap` with a supplied small stdio tool server. A native SDK provider/caller example must also exercise the existing typed capability path; no new generic CLI RPC command is a prerequisite.
3. **Discover and invoke from another process.** For the compatibility route, use `mcp serve` and a supplied runnable MCP client. Identify the actual invoking participant and any pin/consent or provider policy involved. Keep defaults restrictive; do not substitute blanket admission for a working permission setup.
4. **Prove the result and boundary.** Match the caller's response to the provider's invocation record and identity. Show an unauthorized attempt refused before handler effect, then an authorized positive control. Creating a signed file or completing a handshake is not invocation success.
5. **Capture and reuse the contract.** After CLI-3, capture full descriptors, generate and exercise typed consumption while the provider is available, then stop it and reproduce the generated artifacts from the saved snapshot. Offline generation does not imply offline invocation.
6. **Clean up and explain failure.** Document stop/cleanup, unreachable provider, denied invocation, deadline, and uncertain-effect behavior. No automatic retry after an ambiguous effect.

**Topology acceptance (user clarification, 2026-09-20):** Scaffold two nodes in CI using the existing three-node test pattern. The current `three_node_integration` binary is pinned by `.github/workflows/ci.yml` and runs loopback UDP adapters on one runner, not three computers. The journey fixture should similarly create real identities/connections, with separate CLI processes for publisher/consumer behavior, bounded readiness, and cleanup. This is the requested automated acceptance topology; external host access is not a prerequisite for completing it. Report multi-node/process integration honestly, not as between-computer evidence. Non-loopback bind handling remains CLI-2B work; actual off-host validation is still required before making off-host readiness claims.

**Deliverables:** Rework onboarding in `net/crates/net/cli/README.md` and link it from `web/src/content/docs/reference/cli.md`; supply runnable fixtures and a harness under `net/crates/net/cli/tests/fixtures/` (new deliverables), with new subprocess coverage `net/crates/net/cli/tests/capability_workflow.rs`. Reuse `net/crates/net/adapters/mcp/tests/wrap_end_to_end.rs` and `net/crates/net/sdk/tests/tool_serve_round_trip.rs` as evidence starting points, not substitutes for the public journey. Record the two-node CI commands, versions, topology, and actual results as acceptance evidence. Public instructions must remain limited to behavior already accepted at that release.

### Sequence and ownership

| Slice | Outcome | Reason for order |
|---|---|---|
| **DOC-0 — Immediate expectation correction** | Public reference and README describe shipped behavior | Separate docs-only change alongside CLI-1; do not wait for feature completion |
| **CLI-1 — NetDB restore preflight and persistence** | Reject bad inputs before mutation; restored records survive reopening | Preflight committed; persistence defect found by positive controls |
| **CLI-2A — Explicit fresh-supervisor scope** | New empty runtimes cannot be mistaken for deployed state | Extend existing `snapshot --local` precedent with explicit explanation |
| **CLI-2B — Target resolution** | Resolve mode, target, identity, and supported bind consistently before acting | Separate review unit; no remote Deck or context-management framework |
| **CLI-DX — Automation contract** | Predictable framing, deadlines, and noninteractive confirmation | Explicit compatibility change after targeting; not hidden in CLI-1 |
| **CLI-3 — Complete live typegen inputs** | Capture full contracts and regenerate bindings offline | First feature-completion slice for the selected publisher/consumer audience |
| **Journey acceptance** | Documented MCP and native typed paths demonstrate authorized capability use | Specify now and exercise incrementally; two-computer evidence gates off-host claims |
| Later — transfer holder | Stage, host, and fetch with shipped CLI processes alone | Supporting workflow; needs lifecycle, exposure policy, and non-loopback proof |
| Later — unary RPC | Invoke a known service from shell/CI | Reuse current Mesh and typed calls; no invented method/codec abstraction |

These are independent reviewable slices, not an all-or-nothing release train. The user authorized committing the preflight patch, updating this plan, fixing and committing durable restoration, then continuing the plan. CLI-1 persistence is committed as `0d9683356`; continue DOC-0 and subsequent slices in order. Retain CLI-3's identifier for existing references. Implementers may further split coherent review units without weakening cumulative acceptance. Use the user-selected two-node CI topology above; local fixtures do not establish off-host readiness.

### DOC-0 — correct expectations before adding features

**Implemented evidence (2026-09-20):** README onboarding no longer presents temporary administration as deployment operations. README/reference now distinguish execution scopes, staging from hosting, actual transfer argv/output, inline-only typegen acquisition and selector intersection, decode-only subnet inspection, and current timeout/error/confirmation limits. Seven README/parser/help tests passed. After installing locked web dependencies, `npm run check` passed (180 documents' links, release mirror consistency, and TypeScript). These checks establish syntax/document consistency, not a working capability journey.

Files: `web/src/content/docs/reference/cli.md` and `net/crates/net/cli/README.md`. Compare each affected example with the parser and execution target. Remove claims of generic live-node attachment, daemon execution, and completed metadata hydration; state current limits rather than publishing planned behavior. Describe `subnet inspect` as decoding/inspection, not signature verification. Distinguish transfer staging from hosting/publication and correct argv against the parser.

Use the wording **“Starts a temporary supervisor for this command; does not inspect a running node.”** wherever a development-only supervisor is presented. Demote those examples from onboarding. Do not describe `--local` alone as sufficient explanation or publish new scope gates before they ship.

Acceptance: inspect every corrected claim against its implementation; check examples' argv and links; run applicable docs checks from `web/` using `npm run check`, separating unrelated baseline failures. Parser/help tests validate syntax only. DOC-0 does not claim an unexecuted journey works, nor does it wait for all later CLI slices to finish.

## 5. CLI-1 — preflight repair and durable restoration

### Implementation status — 2026-09-20

Preflight patch: `5924174fb` (parent `ad540dcbe5e461e31b5d84ca1482aaa708820a48`). Changed only CLI `netdb.rs`, its changelog, and the new `netdb_restore_preflight.rs` subprocess suite. Profile errors now propagate; a single bounded source read and envelope validation precede destination mutation. Destination inspection failures remain errors.

Windows evidence: full default CLI suite **237 passed, 5 failed, 1 existing ignored**; the new suite accounts for 12 passes and all 5 failures. CLI all-target check, strict binary clippy, and package formatting passed. Workspace-wide formatting failed to launch due to Windows command-length error 206. Linux default-directory isolation and Unix permission-gate tests remain platform-specific CI work. Both inverse checks succeeded: clear-before-read breaks the missing/malformed preservation tests, and swallowed profile errors break the configuration test. Inverse edits were removed and the disposable checkout deleted. Reusing a target directory across checkouts left a stale inverse executable; the reported full-suite result is from a package-clean candidate rebuild.

**Discovered pre-existing defect:** a valid snapshot restores into the SDK's in-memory adapter state, but `netdb restore` exits without persisting that initial state for the next ordinary open. A separate CLI read returns no restored records. The snapshot fixture successfully restores the expected record through the public SDK in memory, ruling out an empty input fixture. The five active failing controls cover clear, source-inside-destination, profile destination, tasks-only, and memories-only restoration. They must stay active until the defect is fixed.

### Authorized persistence extension

**Implemented evidence (2026-09-20):** Tasks and memories now publish origin-bound local checkpoints beside their RedEX logs; ordinary opens load the restored state and replay subsequent destination events. Public snapshot encoding and CLI success payloads are unchanged. Nested payload validation happens before clear; persistence/flush failures produce no restore-success output. Keep checkpoint files when copying a restored store; older binaries cannot read this format and must not open it. This remains offline-only and is not an atomic multi-adapter replacement.

Final Windows default CLI sweep: **247 passed, 0 failed, 1 existing ignored** (the unrelated aggregator query fixture), including **22** restore-preflight/persistence tests. The existing NetDB, CortEX tasks, memories, and adapter integration binaries passed **85 tests** with the repository's broad test alias. Controls cover independent process reopen without the source file, subsequent writes/deletion, retained post-snapshot log tail, absent adapters, wrong-origin/corrupt checkpoint rejection, and Windows checkpoint replacement failure. CLI all-target checking, package formatting, changed-core-file formatting, strict combined CLI/core clippy, root default/no-default clippy, and root all-features rustdoc passed. A narrower root rustdoc invocation hit a pre-existing feature-gated RTC link; the prescribed all-features gate passed. Unix-specific witnesses and exact-head CI/full repository pre-push checks remain outstanding.

Implement the smallest coherent persistence/reopen correction using the existing NetDB/CortEX/RedEX mechanisms. CLI, SDK, and core storage changes are allowed where necessary for this defect; unrelated wire, networking, and command families remain outside CLI-1. Preserve the public snapshot encoding and ordinary command output unless a concrete incompatibility is discovered and documented.

- A successful restore must survive process exit and ordinary reopening, without the original source file. Persist the restored state and the replay position together; do not reconstruct domain events and silently lose timestamps, deletion state, provenance, or sequence semantics.
- Reopening and subsequent writes must preserve restored records and replay new events exactly once across further restarts. Keep origin attribution explicit and guard against accidentally applying a checkpoint under another origin.
- Preserve the existing `--force` fold/merge boundary, including adapters absent from the incoming snapshot. `--clear` removes prior state. Cover both adapters and post-restore writes; do not claim arbitrary conflict resolution beyond the existing fold contract.
- Validate nested adapter input before destructive replacement using existing decoders where possible. Do not turn an invalid embedded payload into another clear-before-validation path.
- Fail persistence errors before emitting restore success. Full crash-atomic replacement and concurrent writers remain separate work; do not claim them merely because a checkpoint file is atomically published.
- Add focused persistence/reopen witnesses at the owning layer and retain the CLI subprocess acceptance controls. Run the appropriate existing storage family once after focused closure, plus the CLI sweep and touched-crate lint/doc gates. Commit the defect fix separately from preflight and this plan update.

### Goal and limit

For `net-mesh netdb restore`, invalid configuration or unusable snapshot input must fail before mutating the destination. Preserve origin rules, store precedence for valid configuration, merge/clear distinction, and successful output shape.

The original preflight ordering repair is committed. The authorized extension adds persistence and reopening as described above; it is not atomic/crash-safe replacement, live-store migration, or a general RedEX redesign. Once valid restoration begins, interruption or storage failure may still leave incomplete state. Operate only on an offline store; this slice does not establish safe concurrent writers.

### Files

Modify:
- `net/crates/net/cli/src/commands/netdb.rs` — profile error propagation, restore preflight order, affected help/error wording.
- `net/crates/net/cli/CHANGELOG.md` — behavior/safety note.

Create:
- `net/crates/net/cli/tests/netdb_restore_preflight.rs` — real CLI subprocess witnesses with disposable stores and isolated config/data locations.

The original preflight patch required no SDK/core/wire changes. The persistence extension may touch the owning SDK/core storage modules and their tests as necessary; identify exact files during the implementation survey. No unrelated dependencies, command renames, new exit codes, or CI feature-set changes. New CLI integration files are auto-discovered; new root integration binaries must be pinned according to repository policy.

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

## 6. CLI-2 — execution scope and target resolution

### CLI-2A — explicit fresh-supervisor scope

**Implemented evidence (2026-09-20):** Shared `commands/scope.rs` provides a per-command `--local` opt-in and unsuppressible stderr disclosure. All 36 temporary-supervisor operations are covered, including admin commits and ICE simulations; admin offline previews, offline issuance, persistent stores, and real remote clients remain outside the gate. Snapshot no longer recommends `peer` as a live alternative. Local/explicit-remote aggregator selections conflict, while profile-based selection remains CLI-2B. README, reference, changelog, and generated help descriptions record the migration without changing result payloads.

`tests/temporary_scope.rs` first demonstrated three failing witnesses against the baseline (missing default scope refusal, help disclosure, and opt-in disclosure). Final focused suite: **8 passed**, covering the 36-leaf refusal/help matrix, explicit local reads and signed admin/ICE simulations, streams with logging disabled, offline preview, conflicts, unsupported gateway export, and generated man/completion. The full default CLI regression sweep passed **254 tests**, with **1 existing ignored** aggregator query fixture; the final eighth scope test was added and passed in the subsequent focused run. Existing remote aggregator/transfer and offline typegen/downstream tests remained green. CLI all-target check, strict binary clippy, CLI rustdoc with warnings denied, package formatting, and web checks passed. Exact-head CI, optional-feature coverage, and full repository pre-push checks remain outstanding.

**Outcome:** Fresh-supervisor reads/streams and admin/ICE operations cannot masquerade as live deployment operations.

Reuse `snapshot --local`: default refusal where deployment attachment is unsupported; explicit `--local` opt-in for development-only operations. Help and scope diagnostics must say: **“Starts a temporary supervisor for this command; does not inspect a running node.”** Emit diagnostics on stderr, not inside existing result payloads. Keep admin's offline `--dry-run` useful without implying contact with a node. Do not gate existing real mesh clients or offline issuance as local demonstrations.

Affected fresh-supervisor paths under `net/crates/net/cli/src/commands/`: `admin.rs`, `ice.rs`, `audit.rs`, `logs.rs`, `cap.rs`, `peer.rs`, `daemon.rs`, `subnet.rs`, `gateway.rs`, `channel.rs`, and local `aggregator.rs` branches. Keep `gateway export` unsupported. Also update `snapshot.rs`, parser/help where necessary, CLI changelog, README, and public reference.

Acceptance: defaults refuse without plausible deployment success data; explicit local calls work and disclose scope; remote aggregator/transfer/typegen fixtures remain remote; help/man/completion and docs agree. Correct snapshot's suggestion of `peer` as a live alternative. Add subprocess scope witnesses alongside `net/crates/net/cli/tests/snapshot_no_false_success.rs`; preserve existing machine result shapes. This deliberately changes scripts relying on implicit local contexts: document the opt-in migration. It does not implement remote Deck. Cover the full inventory before claiming CLI-wide closure.

### CLI-2B — one understandable target-resolution contract

**Outcome:** A user can determine the resolved execution mode, target, and public identity before the operation, using the same resolution that dispatch consumes.

Files likely to change under `net/crates/net/cli/src/`: `context.rs`, `config.rs`, `main.rs`, `commands/aggregator.rs`, and relevant mesh-client constructors; CLI changelog, README, and public reference. New deliverable: `net/crates/net/cli/tests/target_resolution.rs`. Reuse current configuration and mesh builders rather than adding a context registry or control plane.

Required behavior:

- Explicit flags override corresponding selected-profile values. Reject incomplete or contradictory effective target tuples before connection or mutation. Never silently fall back to a temporary supervisor after remote resolution or connection failure.
- For mixed-mode commands such as `aggregator ls`, a complete remote target resolved from the profile selects remote execution just as explicit remote flags do. No resolved remote target means the explicit temporary-supervisor gate applies. Reject conflicting local and explicit remote selections. An explicit local opt-in may select local despite profile remote defaults, but must disclose that choice.
- Explicit target flags on commands that cannot use them fail with an actionable error instead of suggesting remote execution. Ordinary offline commands need not fail merely because the selected profile contains unrelated remote defaults; their inspection must identify offline mode and unused defaults.
- Unknown explicitly selected profiles and missing explicitly selected config files fail; optional absence of the default config retains documented defaults. Loading/permission/parse failures remain errors. Keep CLI-1's narrower profile-error fix independent.
- Provide side-effect-free resolution inspection through the same resolver as dispatch: mode, target/store or artifact destination, public identity fingerprint or unavailable state, bind address where relevant, and flag/profile/default provenance. Do not connect, mint identities, create stores, start supervisors, print keys/PSKs, or claim authorization succeeded. Prefer a small flag or existing diagnostic surface; select and document syntax in the implementation brief rather than inventing an unimplemented command here.
- Resolve the selected journey's cross-host bind using existing mesh configuration. Expose only a documented supported selection; reject incompatible loopback-only routing early. Do not silently widen hosted-service exposure, admission policy, or imply universal NAT reachability.

Acceptance matrix: flags-only, profile-only, overrides, partial targets, explicit local with remote defaults, local/explicit-remote conflicts, unavailable peer, unsupported target flags, malformed/missing explicit config, unknown explicit profile, offline operations, and secret-free inspection with no filesystem/network side effects. Assert the execution consumes the inspected resolution. Include a real remote aggregator control and non-loopback evidence for the chosen capability journey.

Compatibility: profile-only `aggregator ls` changes from local to remote; unsafe fallback becomes an error. Record old/new behavior and migration examples in the changelog and public docs. Do not silently reinterpret identities or automatically retry against another target.

### CLI-DX — automation contract (separate review unit)

Files likely to change under `net/crates/net/cli/src/`: `main.rs`, `output.rs`, `error.rs`, `commands/ice.rs`, and commands consuming deadlines/output; CLI changelog, README, and public reference. New deliverable: `net/crates/net/cli/tests/automation_contract.rs`. Reuse existing renderers and error codes; do not add a general job framework.

- **Framing:** In JSON mode, successful one-shot operations emit exactly one JSON value and a newline; warnings/progress/prompts remain on stderr. For ICE commit, emit one result containing preview and commit outcome rather than concatenated JSON values; dry-run emits its preview only. Streams use one documented NDJSON event per line. Failures exit nonzero with no success payload; prior stream events remain valid. Keep plain secret-free stderr errors unless a separately specified structured-error mode is selected. Do not apply generic JSON framing to `mcp serve` protocol stdout, generated source/binary artifacts, or other explicitly documented protocol streams.
- **Deadlines:** Carry the parsed timeout through bounded connection/discovery/invocation stages as a remaining budget, not a fresh full timeout per stage. Document exactly what it bounds for each command. Long-running services/streams use it for startup/attachment only, not an implicit lifetime limit. Local noninterruptible mutation must finish or report its actual failure; do not kill restoration halfway through merely to claim timeout compliance. If a command cannot honor an explicit deadline, reject that combination before effects rather than ignore it. Timeout is exit 7 where applicable, not proof that a remote effect was cancelled; never automatically retry an ambiguous operation.
- **Confirmation:** `--yes` acknowledges the documented operation in both TTY and non-TTY use; it bypasses prompting, not policy/admission or required parameters. Without it, interactive execution may prompt on stderr; unattended execution refuses without blocking. Preserve dry-run's no-confirmation behavior.
- **Compatibility:** Preserve existing ordinary one-shot payloads and exit codes. Treat ICE framing, confirmation, timeout enforcement, and targeted stream changes as explicit behavior changes, with release notes and before/after scripting examples. Do not hide these inside the safety fix. No indefinite dual-output compatibility framework is required.

Acceptance: parse complete stdout (not its first line) for one-shot JSON; validate NDJSON event-by-event; assert clean protocol stdout and secret-free stderr; test TTY/non-TTY with/without `--yes`, refusal, policy denial, dry-run, slow connection/handler/discovery, an already partially consumed deadline, and continued service lifetime after startup. Use a handler-side effect counter to prove no retry after timeout. Verify unsupported deadlines fail before mutation. Planned gate after the new deliverables exist, from `net/crates/net/`: `cargo nextest run -p net-cli --test target_resolution --test automation_contract --no-tests=fail --retries 0`. CLI-3 consumes this deadline contract if already merged; otherwise its local budget remains explicit and must later converge, not compete with it.

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
- Specify bounded discovery/fetch timeout using CLI-DX remaining-budget semantics when available. Until that slice lands, keep the local budget explicit without pretending global timeout already works; do not reset a fresh full budget per metadata fetch.

Acceptance: real SDK host and CLI subprocess; oversized schemas retrieved through metadata RPC; requested tool arriving after unrelated tool; NotFound/timeout/provider mismatch failures without final output; deterministic offline regeneration after host shutdown; existing downstream language checks. Loopback tests prove integration, not cross-computer reachability.

Likely changes: `net/crates/net/cli/src/commands/typegen/mod.rs`, existing typegen tests, `net/crates/net/docs/cli/TYPEGEN.md`, `web/src/content/docs/reference/cli.md`, CLI changelog. New deliverable: `net/crates/net/cli/tests/typegen_live_metadata.rs`. Resolve exact provider/version-conflict behavior in its implementation brief; this refresh does not authorize SDK widening if existing public queries lack necessary provenance.

## 8. Deferred work and non-goals

- **Transfer holder:** Reopen staged store and use `net_sdk::transport::serve_blob_transfer`. Prove separate stage/holder/receiver CLI processes, restart, missing content, cancellation, and non-loopback connectivity. Local staging is not publication; remote administration is not enabled by default.
- **Unary RPC:** Reuse `Mesh::call`, `call_service`, `call_typed`, and current attach/profile plumbing. The SDK takes one service name, not a universal service/method pair. Start with explicit JSON and no automatic retry of ambiguous execution. Defer streaming, duplex, arbitrary codec negotiation, and generic provider hosting.
- **Remote Deck:** Requires real provider endpoints, deployed state ownership, operator verification, and client targeting. Mesh attachment alone is not remote Deck. Start only for a named workflow.
- **Aggregator query:** Its ignored integration fixture is an open evidence gap, not a proven workflow. Resolve actual addressing/lifecycle prerequisites before un-ignoring.
- **Browser enrollment:** Anchor bootstrap/credentials exist; production issuer/enrollment policy cannot be supplied by copying a permissive demo handler.
- **Concurrent stable-identity invocations:** Require their own lifecycle/ownership contract if the selected journey needs them; do not assume concurrent processes may reuse one identity safely. Target/profile/bind resolution and deadlines are no longer indefinitely deferred: CLI-2B and CLI-DX own them.
- **Crash-safe NetDB replacement:** Staged construction, publication/rollback, and concurrent-writer ownership are outside CLI-1. Preflight does not supply those guarantees.
- **Agent migration:** No filesystem sync, session failover, memory journaling, or harness adapters. This refresh is separate from the earlier migration discussion.
- **Old wish list:** Daemon factory registries, all MeshDB/NetDB predicate commands, blob absorption, metrics/bench tools, broad SDK parity, and a universal workflow engine are not automatic obligations.

## 9. Validation and handoff

### Refresh validation

Check Markdown structure, relative links, referenced current paths, new-target labeling, and `git diff --check`; verify only this plan changed and HEAD remains at the plan-revision checkout recorded above. Preserve the separate original source baseline. No implementation, build/test run, commit, push, or release is part of this refresh.

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
- [Public reference — immediate DOC-0 correction](../../../web/src/content/docs/reference/cli.md)
- [Repository test guide](../../../TESTS.md)
- [CI configuration](../../../.github/workflows/ci.yml)
