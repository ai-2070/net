# Net CLI V2/V3 Branch Code Review

**Date:** 2026-09-22
**Repository:** `ai-2070/net`
**Reviewed commit:** `7735a8a9fe1cae96cf495f8a959a7f5f8ce0a80f`
**Merge base:** `66771b27775fb8b029a7d081e51c360b0e96ed37` (`master...HEAD`)
**Branch:** `LZL0/net-cli`
**Scope:** 102 files, +14,625 / −936. nRPC bounded large-unary-response delivery (core, mesh glue, cross-language golden vectors); net-cli V2/V3 (explicit target resolution, deadline budgets, temporary-supervisor scope, NetDB restore preflight, bounded MCP/wrap startup, live typegen, automation-safe ICE); SDK enrollment policy and the protected enrollment snapshot owner; restored adapter state persistence; CI wiring, nextest overrides, CLI CHANGELOG/README, `net-event-bus` skills, `web` CLI reference, internal plan documents.

## Executive summary

The branch is well-built. Deadline budgets, restore preflight, child lifecycle, target resolution, wire bounds, enrollment ownership, and CI floor discipline all survive direct attack. The evidence base is genuine witness work, not test-shaped prose.

Two defects in shipped code block the merge:

1. A drop window between `register_large` and the RAII guard can permanently strand the patch-introduced fragment budget, and after roughly eight incidents every future large response on the node fails forever.
2. The restore checkpoint — the only durable copy of restored state — is published without the crash-safe write discipline the crate already implements elsewhere, so a power loss on Windows can resurrect a stale checkpoint or silently drop restored entities.

A third, documentation-level defect is patch-introduced: the showcase `wrap --inspect-target` example cannot be run as printed. Two further High findings are pre-existing skill-doc wire claims that mislead a decoder or a C consumer; they are the direct siblings of the two doc bugs this branch already fixed (`7d0c5c7a7`) and belong in the same sweep.

The remaining twenty-one findings are hardening: six witness oracles that cannot fully discriminate their claimed outcome, seven stale or over-broad documentation claims, and eight minor code issues (duplicated resolution logic with drifted exit codes, a fail-closed policy that hangs instead of refusing, dead code, polling loops).

**Recommended disposition:** fix findings 1–3 before merge. Findings 4–5 are cheap and in the class the branch is already cleaning up; take them in the same pass. The witness and documentation findings may be scheduled, but each should get a decision — several describe a claim the test or the document currently makes and the code does not honor.

---

## Findings at a glance

| # | Finding | Severity | Provenance | Status |
|---|---|---|---|---|
| 1 | Fragment budget stranded by a drop during publish | High | patch-introduced | Open |
| 2 | Restore checkpoint publish is not crash-durable | High | patch-introduced | Open |
| 3 | `wrap --inspect-target` skill example cannot run | High | patch-introduced | Open |
| 4 | BlobRef Small URI documented with a length prefix it does not have | High | pre-existing | Open |
| 5 | `NET_ERR_BLOB_VTABLE_INVALID` does not exist | High | pre-existing | Open |
| 6 | Enrollment lock/snapshot open hangs on a planted FIFO | Medium | patch-introduced | Open |
| 7 | Bind/PSK resolution duplicated; the two copies already drifted | Medium | patch-introduced | Open |
| 8 | Unreachable `temporary_supervisor` arm in `aggregator ls` inspection | Medium | patch-introduced | Open |
| 9 | Per-fragment credit wait spins at 1 ms while holding a pump slot | Medium | patch-introduced | Open |
| 10 | Nested adapter state decoded twice per checkpointed restart | Medium | patch-introduced | Open |
| 11 | `RpcClientPending::register` left with no production caller | Medium | patch-introduced | Open |
| 12 | Keychain inspection branch runs in no CI job | Medium | patch-introduced | Open |
| 13 | Force-restore merge witness observes only the memories side | Medium | patch-introduced | Open |
| 14 | Child non-spawn is probed with a nonexistent binary | Medium | patch-introduced | Open |
| 15 | Man/completion scope oracle does not discriminate the caveat | Medium | patch-introduced | Open |
| 16 | Golden vectors pin the response length, not its byte layout | Medium | patch-introduced | Open |
| 17 | Delayed-duplicate oracle is a post-hoc zero | Medium | patch-introduced | Open |
| 18 | `heartbeat_aead` witness scans source text | Medium | patch-rewritten | Open |
| 19 | Harness waits have no slack; ambient `NET_MESH_CONFIG` not scrubbed | Medium | patch-introduced | Open |
| 20 | V2 plan lines assert retired baseline behavior | Medium | patch-falsified | Open |
| 21 | `dataforts.md` wire, ABI, and metrics drift | Medium | pre-existing | Open |
| 22 | `TYPEGEN.md` promises broader than the generator delivers | Medium | pre-existing | Open |
| 23 | Synopsis and export-claim drift in `web` CLI reference and skill | Medium | pre-existing | Open |
| 24 | Go golden-vector test cannot detect mis-tiled fragment rows | Low | patch-introduced | Open |
| 25 | Enrollment witnesses wired as one arm of a `--no-tests=fail` union | Low | patch-introduced | Open |
| 26 | Unbracketed optional flags in re-emitted command rows | Low | patch-rewritten | Open |

**Provenance.** `patch-introduced` and `patch-rewritten` are this branch's doing. `patch-falsified` marks a line this branch left standing while contradicting it. `pre-existing` marks text unchanged on both sides of the diff, listed because the branch's own doc-correctness commits target exactly this bug class and the brief was to hunt for siblings.

---

## Method and limits

Read-only review: `git diff master...HEAD`, source and document reading, and targeted structural searches. **No build, test, lint, or format command was run**, so this document makes no claim about compilation or test outcomes. Line numbers were read at the reviewed commit and will shift as the branch moves. Every finding names the concrete evidence behind it and the failure mode it enables; findings whose oracle cannot distinguish the claimed outcome name the bug that would pass it.

Five reviewers covered disjoint slices — core large-response delivery, the CLI command surface, the CLI test and fixture corpus, SDK enrollment plus core storage, and CI/docs/skills — with a cross-cutting pass over the large-response concurrency paths, panic hygiene in `cli/src`, dependency placement, and the CI witness pins.

---

## Explicitly verified sound

Recorded so the findings are not read as a general indictment of the diff.

- **Large-response lifecycle.** Fragment bounds are enforced before allocation (`total`/`index`/body length, 8 MiB aggregate reserved via `checked_add` before `vec![0; total]`) and at send (1 MiB cap before fragmenting; `u32` total and `u16` index truncations are safe at 1 MiB / 256 fragments; `try_publish_to_peer_bound` pre-checks `EventFrame::LEN_SIZE`-inclusive payload against `MAX_PAYLOAD_SIZE`). Every pump and pending-entry exit path releases its state, including deadline expiry, cancel token, session eviction, receive-lifetime retirement, and node shutdown. The in-flight cancellation entry is removed under an `Arc::ptr_eq` guard, so a late delivery cannot remove a reused call-id entry. Malicious-peer paths — wrong peer/call/session, unsolicited fragments at legacy waiters, inconsistent totals, contradictory duplicates, nested envelopes, trailing bytes, an unfragmented success interrupting fragments — all terminate with the budget released; reordered and duplicate delivery completes exactly once.
- **Negotiation is forward-compatible.** `cortex/rpc.rs:278` retains "consumers MUST ignore unknown bits", so a pre-branch peer ignores the new flag rather than rejecting the request. Fragmentation is gated `large_emit.filter(|_| large_response_enabled && needs_transfer(&resp))` (`cortex/rpc.rs:2155`), and `bound_response_packet` (`mesh_rpc.rs:3186`) is a second defense for callers that did not opt in: it preserves the call identity, never echoes application bytes, and never invites a retry.
- **Deadlines.** One absolute, never-reset instant per operation. `Deadline::run` refuses an expired budget before polling, so a zero or exhausted budget cannot start effects. Timeouts map to `ExitCodeKind::Timeout` (7) with the honest "may have committed; did not retry" text and are never swallowed into a generic error. Unsupported `--timeout` combinations fail before execution.
- **NetDB restore.** Read, stat, bounded read, decode, and validation all precede the first destination mutation; a source inside `--clear` destination is handled by capturing bytes first; `RedexFile::close` is idempotent so the post-close flush cannot spuriously fail a committed restore.
- **Child lifecycle.** `StdioMcpClient` spawns with `kill_on_drop(true)`, inherits stderr (no pipe to fill and deadlock), and drains stdout through a reader task. Startup timeout, error return, and panic unwind all drop the in-publish client and the local serve handle (`ServeHandle::Drop` unregisters) and kill the child.
- **Target resolution.** All remote callers funnel through `resolve_attach_args`; partial and missing selectors hard-error; `wrap --listen` rejects remote peer settings including profile defaults; mixed `--local`/`--remote`/`--from-snapshot` are explicit errors; no name-claiming selector reaches a path sink. The cutover is clean — the old `require_explicit_local` is gone, `scope.rs` owns the required `--local` gate for all 36 leaves, and store/authority/pin path resolution is single-sourced.
- **Enrollment storage and invitation policy.** The snapshot owner is protected at the type boundary (private fields, non-`Clone`, `&mut self` writes, `Busy` on second ownership) and at the FS boundary (`ensure_secure_authority_dir`, uid/mode/nlink/ACL checks, `open_regular_nofollow`, atomic publish with `Uncertain` fencing on post-rename failure). It stores opaque bytes; no path in the slice installs ownership or trust from an unverified claim. `InvitationPolicy::new()` applies exactly the 24 h default TTL with `Preauthorized`; zero and fractional TTLs are rejected; fields are private with no wire or persistence form; `check_redemption_at` is pure, treats expiry as exclusive, and fails closed on backward clock jumps. SDK and core types have disjoint semantics and no shared constants to drift.
- **Restored adapter state.** `checkpoint::store` runs on restore and `checkpoint::load` on ordinary reopen in both `tasks` and `memories`; the `last_seq` clamp is stored and reloaded symmetrically, and its `Some × None → None` case correctly stops a shorter destination log from skipping its own later appends. Version, origin, decode, and `u64::MAX` sentinel mismatches all fail loud; `store` refuses to overwrite a corrupt or foreign-origin checkpoint. (Publish durability is finding 2 — the read/verify side is sound.)
- **CI gate integrity.** Every floor is byte-identical `master` → `HEAD` and absent from the diff (`MIN=196`, `MIN=93`, `MIN=24`, `REG_MIN=62`, `STATE_MIN=41`, `GATE_MIN=60`, `MESH_MIN=67`) and every witness module behind them is untouched. No root-crate `tests/*.rs` lost its pin; the new SDK test file is auto-discovered and correctly exempt. The four journey-witness grep pins name real top-level `#[tokio::test]` fns, so libtest prints `test <name> ... ok` and a missing, renamed, or ignored witness fails the step loudly. The nextest override only tightens (`retries = 0` under a blanket `= 2`), matches the real module path, and cannot shadow the security-critical `binary()` rules. The Windows additions pin Python 3.12 and `pydantic>=2,<3` and fail loudly on install failure; the journey witnesses assert `import pydantic` before anything rather than self-skipping.
- **CHANGELOG and `NRPC_LARGE_RESPONSES.md`.** The breaking changes are announced with migration text (explicit `--local` scope, explicit target tuples). Every spot-checked claim matches code: the `wrap --listen` triple, `wrapped.connection` fields, exit 2/7, ICE's single `{preview, commit}` result, 1 MiB / 8 KiB / 22 KB, the oversized-response error text, the 5 s / 30 s budgets, anchor credentials on stdout, the `recv-blob` `.partial` preservation, and the 21 `require_local` call sites. `NRPC_LARGE_RESPONSES.md` matches the code to the constant. The two earlier doc bugs stay fixed (`node adopt --floors`; `send-blob` hash for single-chunk matches only).
- **Panic hygiene and dependencies.** No new `unwrap`/`expect`/`panic!` is reachable from CLI input in `cli/src` production code — every hit is `#[cfg(test)]`, and the two pre-existing production sites (`commands/node.rs:129`, `commands/org.rs:742`) keep their invariants. `portable-pty`, `postcard`, and `async-trait` land in `[dev-dependencies]` only and do not reach the shipped binary.

---

# 1. High — A drop during the request publish strands the fragment budget permanently

`net/crates/net/src/adapter/net/mesh_rpc.rs:6022-6031` registers the fragment-owning pending entry; the RAII cleanup is not installed until `:6107`:

```rust
let rx = pending.register_large(call_id, target_node_id, expected_session);
// ... build frame ...
if let Err(e) = self.publish_to_peer(...).await {   // the only await in the gap
    pending.cancel(call_id);                        // error arm only
    ...
}
remember_cancel_publish_runtime();
let mut guard = UnaryCallGuard { ... };
```

A future dropped inside that `publish_to_peer(...).await` — a hedge loser, a `select!`-cancelled call, a cancelled `JoinHandle`, the exact cases `UnaryCallGuard`'s own comment claims to cover — runs none of the three cleanup paths: not `pending.cancel` (present only in the publish-error arm), not `UnaryCallGuard::drop`, not the 100 ms session watchdog (created after the guard). `RpcClientPending::senders` has no sweep; the entry lives for the node's lifetime.

Before this branch the stranded entry held a oneshot sender. This branch attaches `PendingEntry::Unary { session: Some(..), assembly: None }`, which `deliver_session` will happily grow: fragments for that call id allocate an `Assembly` whose reservation is charged to the node-global `fragment_bytes` budget (`AGGREGATE = 8 * MAX`). When the server transfer aborts mid-stream — transfer deadline expiring while the handler ran long, caller session churn, a pump send error, any path where the last fragment never arrives — up to 1 MiB is retained forever. After roughly eight incidents every future large response on the node fails with `"aggregate reassembly budget exhausted"` (`cortex/rpc_large_response.rs`, `Assembly::accept`), with no recovery short of restart.

**Required closure**

- Install the pending-entry cleanup — the guard, or a minimal RAII registration — immediately after `register_large`, before the publish await.
- Consider a bounded sweep of `RpcClientPending::senders` for entries whose receiver is gone, so no single missed path can exhaust a node-global budget.
- Extend `partial_call_cleanup` with the drop-during-publish case, asserted through the same `(1, 22_007) -> (0, 0)` leak-vs-release transition the existing six ends use.

---

# 2. High — The restore checkpoint is published without crash-safe durability

`net/crates/net/src/adapter/net/cortex/checkpoint.rs:124-133` hand-rolls the durable publish of `cortex.snapshot`: temp write, then `std::fs::rename`, then a `#[cfg(unix)]`-only parent `sync_all`. On Windows neither durability step exists.

The crate already solved this. `org_revocation::write_atomic_phased` exists precisely because `std::fs::rename` on Windows is `MoveFileExW` with `MOVEFILE_REPLACE_EXISTING` and no write-through, leaving the directory entry in the volume metadata cache — the module's own §13 analysis — and publishes through `rename_write_through` (`MOVEFILE_WRITE_THROUGH`) instead.

The restored state lives **only** in this checkpoint; the channel log holds post-restore appends alone. A power loss after `store` returned `Ok` lets the next `open_with_config` → `checkpoint::load` silently:

- find a stale earlier checkpoint with the same origin, rehydrate pre-restore state, replay the tail onto the wrong base, and skip every event at or below the stale `last_seq`; or
- find no checkpoint and rebuild `TasksState::new()` / `MemoriesState::new()` from the log alone, silently dropping the restored entities that `persistent_restore_reopens_full_state_and_replays_later_writes` exists to protect.

Secondary, same root cause: on Unix a post-rename parent-fsync failure returns a plain `RedexError::io` **after** the rename landed, so `build_from_snapshot` reports failure while the checkpoint is already published. The `WritePhase::PostRename` / `StorageError::Uncertain` fail-closed modeling that `enrollment_storage.rs` — the same patch — applies is missing here.

**Required closure**

- Publish through `org_revocation::write_atomic_phased` (`pub(crate)`, already does temp creation, `MOVEFILE_WRITE_THROUGH`, parent fsync, and pre/post-rename phase reporting).
- Surface a post-rename failure as durability-uncertain rather than as a plain IO error on a published file.
- Witness the Windows path: a test that fails if the publish uses plain `rename` is the inverse this repair needs.

---

# 3. High — The showcased `wrap --inspect-target` example cannot be run

`.claude/skills/net-event-bus/cli.md:100` prints:

```
net-mesh wrap journey --listen --inspect-target   # resolves without binding the port
```

`net/crates/net/cli/src/commands/wrap.rs:151-153` declares `#[arg(last = true, required = true, value_name = "COMMAND")] pub command: Vec<String>`, so clap rejects the invocation at parse time with "the following required arguments were not provided: `<-- COMMAND>`" (exit 2). Amended with `-- <cmd>`, `resolve_listener` runs before the inspection branch and errors `"--listen requires --psk-hex or profile psk_hex"` (`wrap.rs:454`).

The file's own shape at `cli.md:202` (`wrap <name> [flags] -- <command...>`) marks the trailer as required. The showcase example in the rewritten inspect-targeting section is the one command in its block that cannot run. The branch history already carries two doc-correctness commits for exactly this class (`7d0c5c7a7`).

**Required closure**

- Print a runnable form: `net-mesh wrap journey --listen --psk-hex <HEX> --inspect-target -- <COMMAND>`, or state that the trailer and the PSK are elided for brevity.
- Sweep the remaining examples in the rewritten section against the clap trees; this one is the only failure found, but it is the headline example.

---

# 4. High — The BlobRef Small URI has no length prefix, and the skill says it does

`.claude/skills/net-event-bus/dataforts.md:167` documents `uri: [u8] // length-prefixed; adapter dispatch key`. `net/crates/net/src/adapter/net/dataforts/blob/blob_ref.rs:15-16` states the opposite — "No length prefix on the Small URI — the encoded form lives inside an event payload whose length is already framed by the substrate" — and the same file's wire table (`:11-12`) lays Small out as `[hash 32][size 8][uri …]`.

A consumer writing a decoder from the skill block reads a length byte that is actually the first URI byte and misparses the URI boundary of every Small ref printed by `net-mesh transfer send-blob`. Unchanged by this diff; direct sibling of the `send-blob` hash claim the branch fixed.

**Required closure**

- Correct the field comment to `uri: [u8] // to end of ref; adapter dispatch key`, matching the wire table.

---

# 5. High — `NET_ERR_BLOB_VTABLE_INVALID` does not exist

`.claude/skills/net-event-bus/dataforts.md:218` says partial vtables return `NET_ERR_BLOB_VTABLE_INVALID`. The constant band in `net/crates/net/src/ffi/blob.rs:70-99` has no such name — `HASH_MISMATCH = -114`, `BACKEND = -115`, `UNSUPPORTED_SCHEME = -116`, `PANIC = -117`, … `UNAUTHORIZED = -120` — and the partial-vtable null check at `blob.rs:857-858` returns `NET_ERR_BLOB_BACKEND`.

A C or cgo consumer matching on the documented constant for a partial vtable can never fire and misclassifies the registration failure.

**Required closure**

- Replace the constant with `NET_ERR_BLOB_BACKEND`, and add the name to whatever check holds documented `NET_*` constants to `ffi/blob.rs` — the repo already runs that pattern for token wire size (`.github/scripts/check-token-wire-size.py`).

---

# 6. Medium — A planted FIFO turns enrollment storage into an unkillable hang

`net/crates/net/src/adapter/net/behavior/enrollment_storage.rs:208-213` acquires the owner lock through `checked_file`, which opens via `org_revocation::open_regular_nofollow`. That helper sets `O_NOFOLLOW` but not `O_NONBLOCK`, and its `is_file()` type check runs only **after** `opts.open(path)` returns. Opening a FIFO (`mkfifo enrollment.lock`) with `O_RDONLY` blocks in `open(2)` until a writer appears, so `EnrollmentStorage::open` hangs forever at `locked()`. `read` and `replace_using` hang the same way when `enrollment.snapshot` is the planted FIFO — `replace_using`'s `symlink_metadata` match admits it and calls `checked_file`.

Every other lock inode in the crate opens through `open_lock_file`, whose documented policy is exactly this: no-follow, `O_NONBLOCK` (a planted FIFO fails or returns instead of blocking the open forever), and a type check on the **opened descriptor** — used by `OrgRevocationStore`'s `.lock` sidecar (`org_revocation.rs:2528`) and `org_authority::lock_ceremony` (`org_authority.rs:1082`). The module returns `StorageError::Security` for every other planted-inode shape, and its own tests plant a symlink and a permissive mode and require refusal; `hardlinked_snapshot_is_refused` covers `nlink`. The FIFO is the one tampering shape that becomes a process hang rather than a refusal.

**Required closure**

- Open the lock inode with the crate's `open_lock_file` policy — `O_NONBLOCK` on Unix is inert for regular files — keeping `create_new` creation and `try_lock` → `Busy` semantics.
- Apply the same policy to the snapshot inode in `replace_using`, and plant a FIFO in the existing refusal test set.

---

# 7. Medium — Bind and PSK resolution are duplicated, and the two copies already disagree

`resolve_listener` (`net/crates/net/cli/src/commands/wrap.rs:423-458`) re-implements `--bind` / profile-bind / PSK parsing that `resolve_attach_args` + `validate_bind` (`net/crates/net/cli/src/context.rs:431-483`) already own. Both copies are new in this patch and have already drifted:

- **Broadcast.** The listener path rejects it (`wrap.rs:444-449`, `"listener bind cannot be multicast or broadcast"`); `validate_bind` checks only `if bind.ip().is_multicast()` (`context.rs:470`) and defers the rest to the mesh builder.
- **PSK parse errors.** Swallowed into a generic `invalid_args` on the listener path (`wrap.rs:456`); surfaced with the underlying cause on the attach path (`context.rs:398`).

The same malformed selector — `--bind 255.255.255.255:0` — is a usage error (exit 2) on `net-mesh wrap --listen` and a connection failure (exit 6) on `net-mesh wrap` / `net-mesh mcp serve` attach. Scripts keying on exit codes see two different classes for one input, and any future bind-validation fix lands in one resolver and not the other. This is the "second convention beside an existing one" the codebase otherwise avoids.

**Required closure**

- Route `resolve_listener`'s bind and PSK literals through one shared parse/validate helper used by `validate_bind` as well.
- Pick one exit-code class for a malformed bind literal and assert it for both verbs.

---

# 8. Medium — The `temporary_supervisor` arm of `aggregator ls` inspection is unreachable

In `run_ls`'s inspection path, `if remote.is_none() { return super::scope::inspect_temporary(…).await; }` (`net/crates/net/cli/src/commands/aggregator.rs:411-419`) already returns for the only case in which `remote` is `None`, so the ternary at `:420-424` — `let mode = if remote.is_some() { "remote" } else { "temporary_supervisor" };` — can only ever evaluate `"remote"`.

The failure mode is drift, not behavior: an editor tracing where `"mode": "temporary_supervisor"` output for `aggregator ls` comes from will not find it at `target::inspect` (it comes from `scope::inspect_temporary` above) and may change one inspection emitter — provenance keys, `ignored_profile_remote_defaults` handling — in a way that silently never reaches the other, so `--inspect-target` output diverges between the two modes of one verb.

**Required closure**

- Pass `"remote"` directly to `crate::target::inspect` and delete the ternary.

---

# 9. Medium — Fragment credit waits spin at 1 ms while holding one of eight pump slots

`try_publish_to_peer_bound` (`net/crates/net/src/adapter/net/mesh.rs:41791+`) waits for TX credit with a polling loop:

```rust
TxAdmit::WindowFull
    if fragment_session.is_some()
        && tokio::time::Instant::now() < credit_deadline =>
{
    tokio::time::sleep(Duration::from_millis(1)).await;
}
```

Per fragment, that is up to 1 s of 1 ms polls (256 fragments per 1 MiB response), executed while the caller — `deliver_large_response` — holds one of `Semaphore::new(8)` node-wide logical response pumps (`mesh.rs:11606`, constructed at `:14081`). One peer whose TX window stays full can occupy all eight slots in poll loops and stall every large response on the node, while burning scheduler time. The 30 s transfer ceiling bounds the total but does not prevent the contention.

The design is documented ("Eight node-wide logical response pumps, with no fragment queue") and the refusal on exhaustion is honest, but 8 is a hardcoded constant with no configuration, and the ninth concurrent transfer is refused with `"sender capacity exhausted; no fragments sent"` **after** its handler has already committed.

**Required closure**

- Replace the 1 ms poll with credit-available notification (the session already has window bookkeeping), or at minimum back off the interval.
- Name the pump count as a constant with the reasoning, and consider surfacing exhaustion as a distinct status rather than the generic `Internal` the caller cannot distinguish from a handler failure.

---

# 10. Medium — Nested adapter state is decoded twice per checkpointed restart

`decode_snapshot` fully decodes and discards the nested state — `let _: TasksState = postcard::from_bytes(&payload.inner)` (`net/crates/net/src/adapter/net/cortex/tasks/adapter.rs:93-95`, and `let _: MemoriesState = …` at `memories/adapter.rs:94`) — and `open_snapshot` then passes `&payload.inner` to `CortexAdapter::open_from_snapshot`, which decodes the same bytes as the same type again. Restore pays a third parse (`NetDbSnapshot::validate` → `validate_snapshot` → `decode_snapshot`) first.

Every ordinary restart that recovers a checkpoint therefore parses and allocates the entire adapter state twice, in a code path that explicitly avoids lesser duplication elsewhere ("A separate synchronous `read_range` walk would double startup IO/CPU on large logs"). The error on corrupt bytes is the same loud decode failure either way.

**Required closure**

- Move the nested-state check into `validate_snapshot` — the API that promises to validate embedded adapter state without opening a store — and let `decode_snapshot` return the payload after the `last_seq == Some(u64::MAX)` position check.

---

# 11. Medium — `RpcClientPending::register` is now reachable only from tests

The unary call path always uses `register_large` (`mesh_rpc.rs:6031`, setting `flags | large_response::FLAG` at `:5891`), and the streaming paths use `register_client_streaming` / `register_streaming` / `register_duplex`. Every remaining caller of plain `register` is a `#[cfg(test)]` unit (`cortex/rpc.rs:7040` onward, `rpc_large_response.rs:406`).

The plain path is still the one that rejects unsolicited fragments with `Internal` and zero budget charge, so it is a meaningful legacy-waiter behavior — but as production code it is dead, and the cutover left it behind without saying which callers are expected to use it.

**Required closure**

- Either delete `register` in favor of `register_large(.., session)` with a `None` assembly, or mark it explicitly as the legacy-waiter path with its test-only reach stated in the doc comment. A clean cutover should not leave an unlabelled half-migrated entry point.

---

# 12. Medium — The keychain inspection branch runs in no CI job

`net/crates/net/cli/tests/local_target_inspection.rs:1383-1392` puts the new keychain-mode assertions behind `if cfg!(feature = "keychain") {` — `assert_eq!(view["keychain_service"], "net-mesh-forwarding")`, `assert_eq!(view["provenance"]["source"], "stdin")`, and the `forwarding set-value "INVALID REF"` refusal. `keychain` is explicitly non-default (`cli/Cargo.toml`: "NOT a default: it pulls the `keyring` dependency"), and no CI job builds `net-cli` with it: `cli-tests` runs default features plus `--features rtc-bootstrap`, the Windows job runs default features, and `ci.yml:5029` merely clippy-compiles `net-mesh-mcp --features keychain` ("a compile/lint check only, not a test run").

The `else` branch (`assert_eq!(out.status.code(), Some(2))`) is what always executes. Half the witness is silently dropped on every run — the exact silent-shrink class the repo documents for empty test binaries. Wrong `keychain_service` / `keychain_account`, wrong stdin provenance, a flipped `ignored_profile_remote_defaults`, or `forwarding set-value` accepting an invalid keychain account all pass every CI run.

**Required closure**

- Add a `cargo test -p net-cli --features keychain` step with this test name-pinned, as the branch already does for the four journey witnesses. The branch is inspect-only and never writes to a keyring, so it can run headless the way the `rtc-bootstrap` pass does.

---

# 13. Medium — The force-restore merge witness observes only the memories side

`force_without_clear_preserves_merge_semantics` (`net/crates/net/cli/tests/netdb_restore_preflight.rs:494-500`) restores a tasks-only snapshot (holding task 2) into a destination holding task 1 and memory 3, then asserts exactly one thing: `assert_eq!(f.ids(&dest, "memories", "17"), [3]);`. The task side of the merge is never read.

Neither the arrival of the snapshot's task 2 nor the survival of the destination's task 1 is witnessed, and no other test covers it: `valid_clear_restores_snapshot_after_preflight` covers `--clear` replace semantics and asserts `ids == [2]` only, and `restored_records_and_subsequent_writes_survive_repeated_processes` restores into an empty destination. A `net db restore --force` that silently merged nothing, or that dropped the destination's task records, exits 0 with the memory chain intact and passes. For a destructive restore path named for merge semantics, silent record loss is the plausible bug and it ships green.

**Required closure**

- Add `assert_eq!(f.ids(&dest, "tasks", "17"), [1, 2]);` after line 500.

---

# 14. Medium — "Child must not spawn" is probed with a binary that could not spawn anyway

`wrap_attachment_expiry_precedes_child_spawn` (`net/crates/net/cli/tests/automation_contract.rs:458-467`) claims the attachment deadline expires before the wrapped child spawns, but its only probe is the sentinel `"nonexistent-child-must-not-spawn"` observed through `assert_eq!(out.status.code(), Some(7))` and `assert!(out.stdout.is_empty())`. A nonexistent binary makes "spawn attempted and failed" and "never spawned" produce identical output.

A regression that spawns the wrapped program before or concurrently with attachment, and folds any spawn outcome into the same deadline report (exit 7, empty stdout), passes while the invariant — no untrusted program runs before the mesh is attached — is violated. The same sentinel pattern backs the "before execution" / "no child process" claims in `wrap_listen.rs` (`"nonexistent-listener-test-child"`) and `remote_inspection.rs` (`"this-program-must-not-be-started"`).

The patch already contains the direct instrument: `tests/fixtures/wrap_startup.rs` reports process lifetime over a control socket ("The control socket also witnesses process lifetime").

**Required closure**

- Probe non-spawn with a fixture that creates a marker file on startup and assert `!marker.exists()` after the deadline run. Keep the exit-code assertion as the secondary signal.

---

# 15. Medium — The man/completion scope oracle cannot discriminate the caveat

`generated_man_and_completion_include_local_scope` (`net/crates/net/cli/tests/temporary_scope.rs:273-282`) claims the generated man page and shell completion carry the local-scope caveat, but the man oracle is one word anywhere in the document — `assert!(text.contains("Temporary") || text.contains("temporary"))` — and the completion oracle is `text.contains("--local")` over the entire script.

Any unrelated occurrence satisfies the claim. A man generator that drops the per-leaf long help (where the scope sentence provably lives — `every_temporary_leaf_help_explains_scope` pins the full `SCOPE` constant in leaf help) still passes as long as some section mentions "temporary", and a completion script carrying `--local` for one arbitrary command passes regardless of the 35 temporary-scope leaves. The claimed regression — generated docs silently losing the temporary-supervisor scope explanation — is exactly what slips through.

**Required closure**

- Reuse the sibling's discriminating form: assert the whitespace-normalized `SCOPE` sentence appears in the man output, and that a sample leaf's completion entry contains `--local`.

---

# 16. Medium — Golden vectors pin the encoded response length but not its byte layout

`net/crates/net/tests/cross_lang_nrpc/golden_vectors_large_response.json:9-14` records `body_length`, `body_byte`, and `encoded_response_bytes: 22007`, while all four tests hard-code the envelope arrangement independently: Rust asserts `response.encoded_len()` and round-trips through its own codec; Go builds `make([]byte, 7+fixture.Response.Length)` with `PutUint16` at offset 0 and `PutUint32` at offset 3; Node writes `writeUInt16LE(status, 0)` / `writeUInt8(0, 2)` / `writeUInt32LE(len, 3)`; Python packs `'<HBI'`.

These hard-codes match `RpcResponsePayload::encode_into` today (`cortex/rpc.rs:904-915`), so there is no live disagreement. But a Rust envelope refactor that reorders or resizes the status / count / length fields keeps `encoded_len` at 22007 and every Rust decode round-trip green, while the three language tests keep passing against their self-derived bytes — leaving pure-language readers written to these pins misparsing every response. The fixture's own `header_value_hex` shows the fix convention.

**Required closure**

- Record the encoded prefix bytes as hex in the fixture and assert them in all four tests. Narrow the "pins the byte layout" sentence in `NRPC_LARGE_RESPONSES.md` to the fragment envelope and chunking until then.

---

# 17. Medium — The delayed-duplicate oracle is a post-hoc zero

`net/crates/net/src/adapter/net/mesh_rpc_large_response_tests.rs:229-232` claims "A delayed duplicate cannot recreate a retired pending entry" by re-sending the first fragment after completion and then asserting `pending.retained_for_test() == (0, 0)` — but only after `release.notify_one()` and both `shutdown()` calls.

The first fragment's earlier use (`:163-168`) explicitly spins until it "must reach real reply dispatcher" before asserting. The duplicate has no such reaching precondition, and `caller.shutdown()` plausibly discards undelivered ingress. The oracle cannot distinguish "duplicate rejected at the dispatcher, no state created" from "duplicate never processed before the assert". Under a regression where `deliver_session` allocates assembly state for a vacant call id — the exact property named — the witness stays green whenever the packet lands after the final assert, which is the likely case. The transport-level `PeerPublishOutcome::Sent` is not evidence of dispatcher consumption.

**Required closure**

- Give the duplicate the same correlation the first fragment gets (send a control fragment for a live call and wait for its observable effect in the same window before asserting `(0, 0)`), or drop the sub-claim and rely on `wrong_peer_call_and_session_cannot_allocate_or_finish_pending_call`, which does discriminate it.

---

# 18. Medium — `heartbeat_aead` witness asserts source text, not behavior

`publish_to_peer_propagates_reliable_to_packet_flags` (`net/crates/net/src/adapter/net/mesh.rs:52364-52390`) is a source-text scan: `include_str!("mesh.rs")`, `find("async fn try_publish_to_peer_bound(")`, then `body.contains("if reliable") && body.contains("PacketFlags::RELIABLE")`. This branch retargeted the scan to the new delegate instead of replacing it.

Asserting that a source file contains two strings is not a consumer-observable claim: it passes for dead code, a different call path, or a `reliable` flag computed and then ignored. It is also the shape the repository's own test doctrine rejects ("assert what a consumer observes", never source text). The retarget does improve the scan window — `tail.find("\n    }")` reaches the real method end instead of a fixed 6000-byte window — but the oracle remains non-behavioral.

**Required closure**

- Replace with a behavioral witness: publish through the delegate with `reliable = true` and `false` and assert the packet flag observed at the receiving session, or delete the pin and let the transport tests own the flag.

---

# 19. Medium — Harness waits carry no slack, and two harnesses inherit ambient config

Two robustness gaps in the new test harnesses, both able to turn a green run red on a loaded machine or a developer workstation.

**Zero-slack deadline wait.** `rpc_deadline.rs:93-97` waits `timeout(Duration::from_secs(2), handler.entered.notified())` for the remote handler to be entered — a window equal to the CLI's `--timeout 2s` budget under test, with no slack for process spawn, config parse, mesh build, and routed handshake. Every sibling harness in this patch budgets 5–15 s for those phases (`wrap_startup.rs` 5 s around the same 2 s budget; `capability_workflow.rs` 15 s). `cargo test -p net-cli` runs ~20 subprocess-spawning tests concurrently on one runner; a startup delay over 2 s panics the harness, and if the CLI's own deadline fires first, `assert_eq!(handler.effects.load(Ordering::SeqCst), 1)` misreports scheduling delay as "the remote effect never happened" — a misleading red on the one test guarding the no-retry contract. Widen only the harness wait to ~10 s; the 2 s deadline under test stays unchanged.

**Ambient `NET_MESH_CONFIG`.** `transfer_cli_blob.rs:169-178` drives the binary through `cli_cmd` (`:63`), which sets `HOME` / `XDG_CONFIG_HOME` / `USERPROFILE` to the tempdir but never removes `NET_MESH_CONFIG` / `NET_MESH_PROFILE`. Every sibling suite added in this patch does — `local_target_inspection.rs:19-20`, `automation_contract.rs:11-12`, `netdb_restore_preflight.rs:32-33` ("Do not inherit a developer's profile or permission override"). The same patch's `environment_selections_are_validated_and_flags_override_them` proves the binary honors `NET_MESH_CONFIG`, so a developer's exported config changes the new test's pinned outcomes (`assert_eq!(code, 7)`, `stderr.contains("does not prove cancellation")`). `transfer_cli_admin.rs:69` has the same gap at the `--timeout` call sites this branch added. Two `env_remove` calls per `cli_cmd` fix it.

**Required closure**

- Widen the `rpc_deadline.rs` harness wait; add the two `env_remove` calls to both transfer harnesses.

---

# 20. Medium — V2 plan lines assert baseline behavior this branch retired

`docs/internal/plans/NET_CLI_PLAN_V2.md` carries six present-tense claims that this branch falsified or rewrote without retiring the text. V3 explicitly inherits "V2's … deadline budget", so the stale contract lines misdirect the in-flight work. One of them ends "Do not present its planned behavior as shipped" while doing exactly that.

- `:112` — "Global `--timeout` is parsed but not forwarded by `dispatch`. It is not an enforced universal deadline." This branch ships `main.rs:311-322` (`Deadline::after(timeout)` → `dispatch_inner(cli, Some(deadline))`) and `deadline.rs` one-absolute-budget enforcement. *(patch-rewritten: the diff replaces the old line and retains the claim.)*
- `:110` — "On interactive stdin, ICE still requires typed `YES` even with `--yes`." `ice.rs:385-387` returns on `yes_flag` before the TTY check, witnessed by `tty_with_yes_never_prompts`. *(patch-falsified.)*
- `:491` — "says metadata fetch has not shipped … Discovery stops at the first unfiltered nonempty list or five seconds." `typegen/live.rs:78-88` fetches missing schemas; `typegen/mod.rs:537-550` observes the full window for unfiltered discovery. *(patch-falsified.)*
- `:119` — the `run_restore` ordering finding (destination removed before snapshot decode) is the defect this branch fixed; `netdb.rs:620-622` now validates before any mutation. *(patch-falsified.)*
- `:120` — the `resolve_profile(...).ok()` fallback finding is likewise fixed (`netdb.rs:273` propagates). *(patch-falsified.)*
- `:36` — "Several sibling fresh-supervisor commands do not yet have an equivalent gate." `commands/scope.rs:81-98` now gates every fresh-supervisor command. *(patch-falsified.)*

**Required closure**

- Rewrite each line to the shipped behavior, or mark it `Historical — fixed by <commit>` the way the SDK audit records retired findings. Present tense in a plan document is read as current behavior and invites re-fixing a fixed defect or reinstating a retired rule.

---

# 21. Medium — `dataforts.md` drifts from the wire format, the ABI, and the metrics

Seven claims in `.claude/skills/net-event-bus/dataforts.md`, all unchanged by this diff and all misleading a consumer who writes code from them.

- `:164` — `version: u8 // currently 1`. `blob_ref.rs:68/73/78` define `BLOB_REF_VERSION_V1 = 0x01`, `BLOB_REF_VERSION_V2_MANIFEST = 0x02`, `BLOB_REF_VERSION_V3_TREE = 0x03`, and every chunked `send-blob` / `send-dir` reference encodes version 2. "Currently 1" cannot decode a Manifest ref at all.
- `:176` — unique tmp suffixes documented as `<hash>.<pid>.<atomic>.<nanos>.tmp`; `dataforts/blob/fs.rs:350` builds `.{pid}-{counter}-{nanos}.tmp`. Anything globbing staging files by the documented pattern misses the real temps.
- `:245` — `DirStats { files: usize, bytes: u64 }` for Rust; `dataforts/dir.rs:204-213` declares `files`, `dirs`, `symlinks`, `bytes`. (The Node `{files, bytes}` and Python `(files, bytes)` rows in the same table are correct — those bindings really do project to two fields.)
- `:247` — tells C consumers the transport calls are "not declared in `include/net.h` … the header not yet regenerated". `net/crates/net/include/net_transport.h` ships `net_serve_blob_transfer`, `net_fetch_blob`, `net_fetch_blob_discovered`, `net_store_dir`, `net_fetch_dir`, `net_dir_manifest_read`, and the `NET_ERR_TRANSFER_*` band. The guidance invites hand-declared signature drift instead of `#include <net_transport.h>`.
- `:358` — "Each has its own Prometheus counter". `dataforts/greedy/metrics.rs:22-23` defines one metric with a `reason` label, and `dataforts.md:88` in the same file says so correctly. The gotcha sends operators to per-reason metric names that do not exist.

**Required closure**

- Correct each claim to the symbol it describes. These five are candidates for the same derived-check treatment as `check-token-wire-size.py`: a constant and struct-shape checker would have caught four of the five.

---

# 22. Medium — `TYPEGEN.md` promises broader than the generator delivers

- `net/crates/net/docs/cli/TYPEGEN.md:211-212` — "Don't. Every file starts with an 'Auto-generated … Do not edit by hand' header." `typegen/python.rs:549-571` emits `call.py`, per-tool `__init__.py`, `models.pyi`, `meta.json`, and `_meta.json` with no header; only the package/root initializers carry one (`python.rs:577+`). The files a user is most likely to hand-edit are exactly the headerless ones.
- `:188-189` — "`allOf` (TS intersection / Python inheritance for object combinations)". `typegen/python.rs:242-244` lowers allOf to inheritance only for a top-level `Schema::Intersection` whose parts are all `Schema::Ref`; every other intersection — nested, property-level, or with inline object members — becomes `Any` (`python.rs:170-174`). A user with nested allOf silently gets `Any` fields where the doc promises inheritance typing.
- `:153-156` — the `typegen diff` sample transcript omits the version line the renderer always emits when versions differ (`diff.rs:114-119`) and shows fields in an order the `BTreeMap` iteration cannot produce (`diff.rs:195-225`). The documented transcript cannot be reproduced by the shown command.

**Required closure**

- Add the header to the remaining generated files, or narrow the claim to the files that carry one.
- Narrow the `allOf` claim to top-level allOf-of-`$ref`s and state the `Any` fallback.
- Regenerate the sample from an actual run.

---

# 23. Medium — Synopsis and export claims drift in the `web` CLI reference and the skill

- `web/src/content/docs/reference/cli.md:283-285` shows `org grant-capability [--invoke] [--discover --audience-out <PATH>]` with both groups optional under the notation, but `commands/org.rs:657-662` refuses with `"at least one of --invoke or --discover is required"`. The same synopsis marks the sibling groups `(--target-node <HEX> | --target-any-owned-by <HEX>)` and `(--capability | --any-capability)` with required-one-of notation and leaves this one unstated. (Same gap at `.claude/skills/net-event-bus/cli.md:267`.)
- `web/src/content/docs/reference/cli.md:205-208` claims each generated module exports a `…Meta` constant with descriptor metadata. That is TypeScript-only (`typegen/ts.rs:356-370`); generated Python per-tool `__init__.py` exports `"{req_name}", "{resp_name}", "call_{base}", "TOOL_ID", "VERSION"` (`typegen/python.rs:574-580`). A developer consuming generated Python who follows the claim gets an `ImportError` and cannot read streaming / stateless / estimated-time metadata from the package.

**Required closure**

- Mark the `--invoke` / `--discover` requirement with the notation the file already uses elsewhere.
- Scope the `…Meta` claim to TypeScript output and name `_meta.json` as the Python metadata surface.

---

# 24. Low — The Go golden-vector test cannot detect mis-tiled fragment rows

`go/nrpc_large_response_vectors_test.go:51-58` validates each fragment row only against values recomputed from that same row (`PutUint32(header, uint32(len(encoded)))`, `PutUint16(header[4:], uint16(piece.Index))`, then hex and `min(CHUNK, remaining)` length compares) and never reassembles the bodies. Node asserts `Buffer.concat(assembled)` equals `encoded`; Python asserts `assembled == encoded`; Go has no such oracle.

A fixture whose rows duplicate one index and omit another — or a drift making `emit` indices non-sequential — keeps every Go assertion green (`piece.Index` is read from the row under test) while Rust, Node, and Python go red, so the Go CI gate alone cannot hold the fragment-tiling half of the wire contract.

**Required closure**

- Append each row's body to an assembled buffer in the existing loop and compare against `encoded` after it, failing with "fragment bodies do not tile the encoded response in index order".

---

# 25. Low — The enrollment witnesses can silently drop out of the CI union

`ci.yml:5113-5116` wires the new `enrollment_storage` ownership and durability witnesses as a third arm of a union — `test(/^adapter::net::behavior::org/) + test(/^adapter::net::org_admission_gate/) + test(/^adapter::net::behavior::enrollment_storage::/)` — run under `--no-tests=fail`, which fails only when the **whole union** selects nothing. A rename or feature-gating of `enrollment_storage` alone drops that arm to zero while the two org arms still match and the step stays green: the exact silent-shrink failure the step's own comment (`:5107-5112`) cites the guard for.

The same patch name-pins the four journey witnesses "so they cannot silently vanish" (`ci.yml:4922-4929`) and pins these very witnesses `retries = 0` as security-critical in `nextest.toml`. The union pin is the one place the new evidence can evaporate unobserved.

**Required closure**

- Give `enrollment_storage` its own step with `--no-tests=fail`, or add a count floor in the style of the neighboring `MIN=` steps.

---

# 26. Low — Optional flags read as required in the re-emitted command rows

Rows this branch re-emitted carried a notation defect into the new text.

- `.claude/skills/net-event-bus/cli.md:263` and `:289` write `org keygen --out <path> …` / `subnet keygen --out <path> …` with `--out` unbracketed, while `commands/org.rs:89-90` and `subnet.rs:451-452` declare `pub out: Option<PathBuf>` and the rows' own cells document the default destination. Bracket it: `org keygen [--out <path>] …`.
- `.claude/skills/net-event-bus/cli.md:290-291` show `--topology-epoch N --generation N` unbracketed on `subnet issue-direct` and `--topology-epoch N` on `subnet issue-issuer`, while `subnet.rs:501-508` / `:561-566` declare both with `default_value_t`. (`issue-control-fact`'s `--topology-epoch N --revision N` at `:293` is genuinely required — `subnet.rs:683-688` — and the doc is right there.)

**Required closure**

- Bracket the defaulted flags to match the table's own convention (`[--generation N]`, `[--ttl-secs N]`).

---

## Outside this diff

Noted for awareness, not attributable to this branch and not counted above: a `SIGINT` delivered before the serve loop first polls `ctrl_c()` terminates without `Drop` and orphans the wrapped child. Pre-existing in the MCP/wrap lifecycle, unchanged by this patch, and the same class as finding 1 — cleanup reachable only after the first poll of a future.

## Disposition summary

| Group | Count | Disposition |
|---|---|---|
| High — patch-introduced (1–3) | 3 | Fix before merge |
| High — pre-existing (4–5) | 2 | Take in the same doc sweep; siblings of `7d0c5c7a7` |
| Medium — code (6–11) | 6 | Schedule; 6 and 7 are policy/consistency defects, 8–11 are hygiene |
| Medium — witnesses (12–19) | 8 | Decide per item; each names the bug that currently passes |
| Medium — docs and records (20–23) | 4 | Schedule with the documentation sweep |
| Low (24–26) | 3 | Opportunistic |

Nothing in the reviewed surface was fabricated or stubbed. The two High code findings are both about a cleanup or durability path that is correct on the happy branch and absent on an edge branch — the same shape as the lifecycle work the branch did well elsewhere, which is why they read as oversights rather than design faults.
