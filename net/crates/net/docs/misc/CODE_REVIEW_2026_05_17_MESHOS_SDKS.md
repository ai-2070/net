# Code review — `meshos-sdks` branch vs `master`

**Date:** 2026-05-17
**Branch:** `meshos-sdks`
**Base:** `master`
**Scope:** 67 files, +26,653 / −1,649 LOC, 53 commits ahead.

Reviewed in three phases:

1. **Phase 1** — `net` + `net-sdk` core (ClusterHarness, bridge probes, RpcObserver, mesh/mesh_rpc adapter changes, Cargo.toml feature reshuffle).
2. **Phase 2** — `deck` demo refactor (`runtime.rs` split into `demo/*` modules, widget tweaks, plan docs). *Pending.*
3. **Phase 3** — cross-language SDK surfaces (Node/Python/Go FFI + TS/Py/Go/C SDKs + C headers + binding tests + CI). *Pending.*

---

## Phase 1 — `net` + `net-sdk` core

### Files reviewed

New:

- `sdk/src/testing/{cluster,mod,probes}.rs` (~880 LOC)
- `sdk/tests/{cluster_harness,cluster_replica_group,cluster_supervisor,rpc_observer}.rs` (~640 LOC)
- `src/adapter/net/cortex/rpc_observer.rs` (118 LOC)

Modified:

- `sdk/src/{compute,mesh,mesh_rpc,lib}.rs`
- `sdk/tests/mesh_nat_traversal.rs`
- `src/adapter/net/cortex/mod.rs`
- `src/adapter/net/{mesh,mesh_rpc}.rs`
- `Cargo.toml`, `sdk/Cargo.toml`

### Strengths

- **Documentation is excellent.** Every new module carries a why-this-exists header, every public field has a contract comment, and there are explicit pointers back to `DECK_DEMO_HARNESS_PLAN.md` sections for design rationale. Reference-quality.
- **Test coverage of the harness is broad** — happy path, `n=0/1/2/5`, boot budget regression, custom-config path, anchor-out-of-range, empty-predicate, factory-fires-exactly-once, handles-outlive-spawn. Few obvious holes.
- **Spawn rollback is a real contract**, not a TODO. `spawn_where` reverse-drains successful registrations with a 200 ms grace before surfacing the failure (`cluster.rs:497-533, 653-657`). Documented in the `pub` doc on `spawn_per_node`.
- **`ClusterError` is structured**, not stringly-typed: `Handshake { from, to, reason }`, `Spawn { node_index, reason }`, `Timeout { what, budget_ms }`. Easy to assert against.
- **The `mesh_nat_traversal.rs` correction is the right fix.** The previous test encoded a misunderstanding of `record_relay_fallback`'s semantics (it fires only on direct→relay fallback, not on every Direct path); the updated assertion + comment lock in the substrate's actual behavior.

### Bugs

#### 1. Double-Arc indirection on `MeshNode::rpc_observer` — `mesh.rs:1208, 4647-4665`

Originally flagged as a fixable bug; on closer look this is a structural limit of `arc_swap` 1.9.1, not a code smell.

```rust
// cortex/rpc_observer.rs:107
pub type RpcObserverHandle = Arc<dyn RpcObserver>;

// mesh.rs:1208
rpc_observer: Arc<ArcSwapOption<...::RpcObserverHandle>>,
//                              ^ already an Arc — this is ArcSwapOption<Arc<dyn RpcObserver>>

// mesh.rs:4651
self.rpc_observer.store(Some(Arc::new(obs)))  // wraps the already-Arc'd observer
```

I tried changing the field to `Arc<ArcSwapOption<dyn RpcObserver>>` so the cell would store `Option<Arc<dyn RpcObserver>>` directly. `arc_swap`'s `RefCnt for Arc<T>` requires `T: Sized`, so the trait-object form fails to compile (`the size for values of type (dyn RpcObserver + 'static) cannot be known at compilation time… required for Arc<dyn RpcObserver> to implement RefCnt`). Compare `set_migration_handler` on `migration_handler: Arc<ArcSwapOption<MigrationSubprotocolHandler>>` — the substrate's other examples all happen to store sized concrete types, which is why this pattern works elsewhere.

**Resolution.** Keep the double-`Arc`. Cleanups landed:
- Field doc now spells out the constraint explicitly so the next reader doesn't repeat the same investigation.
- Setter collapses `match observer { Some(o) => …, None => … }` to `store(observer.map(Arc::new))`.
- Getter keeps the `(*arc).clone()` deref but with a one-line comment pointing to the field doc.

Hot-path cost: one extra `Deref` per load (cheap) + one extra small allocation per install (one-time). Migrating off `arc_swap` to a crate that supports `?Sized` inners (e.g. building on `parking_lot::RwLock<Option<RpcObserverHandle>>`) is the only way to drop the indirection, and the trade-off (read-lock on every fire vs an atomic load) isn't worth it.

### Code-quality nits

#### 2. `pub` fields on `ClusterNode` — `cluster.rs:83-106`

`pub mesh`, `pub sdk: Option<...>`, `pub daemon_runtime: Option<...>` invite callers to `.take()` out of order and skip the shutdown drain sequence (`shutdown()` is documented to drain `daemon_runtime` before `sdk`). If a test does `harness.nth(0).sdk.take()` directly, the harness's drop / shutdown silently no-ops on that slot. `pub(crate)` + accessors (`fn sdk(&self) -> &MeshOsDaemonSdk`) would close that footgun while keeping the read paths the tests already use.

#### 3. `Drop for ClusterHarness` always prints to stderr — `cluster.rs:640-644`

Fires on every panic-induced drop. In a test run where one test panics, the log will swap a real assertion message for a generic "you forgot shutdown" hint that mostly drowns it out. A `tracing::warn!` would be quieter and structured; `eprintln!` is a reasonable v1 choice given no tracing init in tests.

#### 4. `expected_peers` includes the local node — `cluster.rs:327-328`

Every probe then filters out `local` per-tick (`probes.rs:53, 84, 124`). Could be pre-filtered once at install time: build a `Vec` per node that omits its own id. Tiny: a few cycles × 100 ms tick × N nodes. Don't bother unless touching that code anyway.

#### 5. `RpcCallEvent.method: String` allocates per fire — `rpc_observer.rs:66`

When an observer is installed, every call allocates a `String` for the method name even though most methods are `'static`. `Cow<'static, str>` would zero-allocate the common case. The hot-path comment ("must be cheap") makes this worth thinking about, but for v1 deck-demo traffic it's irrelevant.

#### 6. `migration_orchestrator_arc()` is `pub(crate) #[cfg(feature = "testing")]` — `compute.rs:1656-1661`

Locks the accessor to the SDK crate's own `testing` build. The CI commit log notes this was the source of one build failure (`c0beaf02 Fix CI: gate migration_orchestrator_arc behind the testing feature`). Fine as-is, but worth a tracking note: if anyone outside the SDK wants the orchestrator handle, they'll need to widen this.

#### 7. `RuntimeShutdownError` formatted via `Debug` — `cluster.rs:568-572`

Comment says it doesn't implement `Display`. Surfaces as `{:?}` in error chains. Either teach `RuntimeShutdownError` `Display` (better) or document the convention.

#### 8. `shutdown()` short-circuits on the first SDK error — `cluster.rs:567-573`

If one SDK errors during shutdown, the rest of the `results` iteration aborts. Runtimes have already been drained sequentially in the earlier loop (`for rt in &runtimes { let _ = rt.shutdown().await; }`), so socket cleanup happens regardless, but the error reporting is "first failure wins" rather than "report all failures." A `results.into_iter().collect::<Result<Vec<_>, _>>()` would behave the same; to surface every failure, accumulate.

#### 9. `Mesh::inner()` doc says "not intended for downstream consumers" but is `pub` — `sdk/src/mesh.rs:~379`

The integration tests rely on it (`tests/rpc_observer.rs:51, 57, 59, 61`), and integration tests are treated as external crates by Cargo. Either drop "not intended for downstream consumers" from the doc or move test helpers behind `#[cfg(feature = "testing")]` accessors. Pure doc nit.

### Behavioral / design questions

#### 10. `default` features expansion in `sdk/Cargo.toml` is a breaking change for Rust consumers

Previous default: `[]`. New default: `["net", "nat-traversal", "cortex", "compute", "groups", "meshos", "dataforts", "meshdb"]`. The comment justifies it for bindings ("downloaded the SDK and it just works"), and the `agent` alias is duplicated so a `default-features = false` user can reach the same shape via one flag. But anyone pulling the SDK with empty defaults previously now silently picks up the entire stack — compile time, binary size, transitive deps. Worth a CHANGELOG note and possibly a major-version bump if the SDK has external consumers.

#### 11. Replica-group health asserted synchronously — `cluster_replica_group.rs:55-57`

`group.replica_count()` and `group.healthy_count()` are asserted in the same expression as `spawn_replica_group(...)`. That assumes member registration + health bookkeeping is synchronous in `ReplicaGroup::spawn`. If the underlying registration is fire-and-forget into a tokio task, this test could be flaky under load. Flagged for verification rather than asserted as a bug.

#### 12. Pairwise handshake is serialized — `cluster.rs:253-297`

Comment explains why: parallel-everything races the substrate's handshake state machine. Cost = ~50 ms × N²/2. At N=9 (deck demo) that's ~1.8 s of boot. At N=20 it'd be ~10 s. If the harness ever wants to scale, the substrate-side limit is the real constraint, not the harness loop.

#### 13. `wait_for` polling cadence is 25 ms — `cluster.rs:72-73`

For a 100 ms tick interval, that's 4 polls per tick, fine. If `meshos_tick_interval` is tightened in a future config, the harness will waste cycles on the snapshot-stable barrier (polling faster than the data changes). Tying `poll_interval ≥ meshos_tick_interval / 4` would make the relationship explicit, but it's not a correctness issue.

### Recommended actions

| Severity | Action |
|---|---|
| Done | Document `arc_swap`'s `Sized` constraint inline + tidy setter (item 1). |
| Done | `ClusterNode` fields → `pub(crate)` + accessor methods (item 2). |
| Breaking | Confirm default-features expansion (item 10) is the intent; add a CHANGELOG note. |
| Nice-to-have | `Cow` method name (5), `RuntimeShutdownError: Display` (7), `tracing::warn!` in drop (3). |
| Verify | Replica-group sync health assertion (11). |

No blockers. Phase 1 is in good shape.

---

## Phase 2 — `deck` demo refactor

### Files reviewed

New:

- `deck/src/demo/{mod,cluster,daemons,dataforts,migrator,rpc_chatter,spawn}.rs` (~1,025 LOC)
- `docs/plans/DECK_DEMO_PLAN.md` (135 LOC)
- `docs/plans/DECK_DEMO_HARNESS_PLAN.md` (312 LOC)

Modified:

- `deck/src/{runtime,main}.rs`
- `deck/src/widgets/{footer,status_bar}.rs`
- `deck/Cargo.toml`
- `docs/plans/{DECK_SDK_PLAN,MESHOS_SDK_PLAN}.md` (small status updates)

### Strengths

- **Clean module split.** Each demo submodule has a single purpose: `cluster.rs` (config), `daemons.rs` (impls), `dataforts.rs` (blob seeds), `migrator.rs` (Phase 3 loop), `rpc_chatter.rs` (Phase 4 observer), `spawn.rs` (orchestration). Reads top-down.
- **`runtime.rs` shrunk to ~95 LOC** — the single-node path is now minimal and obviously correct, with the multi-node complexity isolated under `mod demo`.
- **Demo `Harness` mirrors single-node `Harness` surface** (`deck()` / `blob_adapters()` / `this_node()`) so `main.rs` branches on `cfg!(feature = "demo")` only at the spawn site, not throughout the app.
- **Smoke test (`spawn.rs:369-430`) exercises the full vertical.** Boots cluster → waits 3 s → asserts 8 remote peers, 10 local daemons by name, ≥2 log lines, 9 blob adapters, non-empty NRPC tail. Strong coverage for a single test.
- **Plan docs are reference-quality.** `DECK_DEMO_PLAN` + `DECK_DEMO_HARNESS_PLAN` lay out the why, the phases, the locked decisions, and the deferred work. Easy onboarding doc for someone landing on this code cold.
- **The AI-inference vocabulary is a thoughtful UX choice.** The corpus + per-node `gpu-{idx}` tag in `spawn.rs:293, 315-332` makes the demo look like real workload rather than synthetic chatter, with no RNG dependency (deterministic tick-derived numbers).

### Bugs / Real issues

#### 1. `AdminVerifier` is built and discarded — `spawn.rs:142-146`

```rust
let operator_keypair = EntityKeypair::generate();
let mut registry = OperatorRegistry::new();
registry.register(&operator_keypair);
let _verifier = Arc::new(AdminVerifier::new(Arc::new(registry), 1));
```

`_verifier` (intentional discard prefix) is never installed on any node. Compare `runtime.rs:74-81` (single-node path) which passes `Some(verifier)` into `MeshOsDaemonSdk::start_with_verifier_and_migration_source`. The harness hardcodes `verifier: None` (`cluster.rs:354-359`), so under `--features demo` no node has an operator verifier wired.

Practical impact: ICE actions from the deck (`[K]` to cancel a migration, `[F]` to force-freeze) — explicitly called out as a demo feature in `DECK_DEMO_PLAN.md` Phase 3 ("Operator can `[K]` from the MIGRATIONS tab… ICE commit goes through the real signing path against the operator's demo-identity") — won't go through the real signed-commit path. They either bypass verification or fail outright depending on what the substrate does with `verifier: None`.

**Fix options:**

- Extend `ClusterConfig` with an optional `Arc<AdminVerifier>` and have the harness install it on each node.
- Or expose a `ClusterNode::set_verifier(...)` accessor and have the demo install it post-boot.

#### 2. Truncated / mismatched comments in `main.rs:46-66`

The comment block reads as if two paragraphs got mashed together with their first halves deleted:

```rust
// `demo` boots a real multi-node cluster via
// NRPC tail — built BEFORE the harness so the demo's
// observer bridge can be wired into it during spawn.
// Non-demo builds use it for the samples-logs injector
// (when that feature is also on) or leave it empty.
let nrpc_tail = streams::NrpcTail::new(streams::NRPC_TAIL_CAP);

// `net_sdk::testing::ClusterHarness`; otherwise the
// single-node `runtime::spawn` path runs with an empty
// cluster view ready for real-cluster wiring.
#[cfg(feature = "demo")]
let harness = demo::spawn(nrpc_tail.clone()).await?;
```

The first sentence trails off ("boots a real multi-node cluster via" — via what?), and the second comment starts mid-sentence (no lead-in for "`net_sdk::testing::ClusterHarness`"). Also references `samples-logs` which was deleted in this same commit set.

Likely an editor-merge artifact. Worth a pass through `main.rs` to re-flow the comments.

#### 3. "Idempotent" doc on `migrator::install_factories` is inverted — `migrator.rs:90-91`

```rust
/// Register `demo.migratable` on every node's DaemonRuntime.
/// Idempotent — `register_factory` rejects duplicate kinds on
/// the same runtime, so callers must invoke this exactly once
/// per harness lifetime.
```

If the underlying call **rejects** duplicates, the function is by definition **not** idempotent (an idempotent op succeeds N times). The body of the comment is correct ("call exactly once") — just the lead word is wrong. Reword to "Single-shot" or "Not idempotent — call exactly once."

### Code-quality nits

#### 4. Stale "5-node" comments after bump to 9 — `spawn.rs:41, 357, 379` and `migrator.rs:127`

`DEMO_NODE_COUNT` moved from 5 to 9 (`cluster.rs:15`) — captured in code paths and the test assertions (`snap.peers.len() == 8`, `harness.blob_adapters.len() == 9`), but several comments still say "5":

- `spawn.rs:41`: "the 5 nodes don't all emit on the same tick"
- `spawn.rs:357`: "Boots the real 5-node cluster"
- `spawn.rs:376-379`: references "the other 4 peers" — should say 8
- `migrator.rs:127`: "the demo always boots 5 but guard against future single-node use"

#### 5. Hardcoded `Vec::with_capacity(9)` in `spawn.rs:195`

`heartbeat_handles` uses `cluster.len()` (line 151); `group_handles` uses `9`. Either both should use `cluster.len()`, or both should use `DEMO_NODE_COUNT` from `cluster.rs`. Mixed conventions invite drift if the count moves again.

#### 6. `rpc_chatter::install_responders` hardcodes responder count of 2 — `rpc_chatter.rs:104-122, 129`

```rust
for idx in 0..2 { ... }                                                           // responders
let responder_ids: Vec<u64> = harness.nodes().iter().take(2).map(...).collect();  // requesters target the same 2
```

Magic 2 in two places. If `DEMO_NODE_COUNT` ever drops below 2 (the migrator already guards `total < 2`), responders would be empty and requesters would silently never get traffic. Could extract a `const RESPONDER_COUNT: usize = 2` and guard like the migrator does.

#### 7. `spawn.rs:280-284` jitter math is correct but verbose

```rust
let jitter_ms = ((tick.wrapping_mul(11) ^ node_id) % 300) as i64 - 150;
let interval = HEARTBEAT_BASE_INTERVAL
    .saturating_add(Duration::from_millis(jitter_ms.max(0) as u64))
    .saturating_sub(Duration::from_millis((-jitter_ms).max(0) as u64));
```

Reads as cryptic. A clearer shape:

```rust
let interval = if jitter_ms >= 0 {
    HEARTBEAT_BASE_INTERVAL + Duration::from_millis(jitter_ms as u64)
} else {
    HEARTBEAT_BASE_INTERVAL.saturating_sub(Duration::from_millis(-jitter_ms as u64))
};
```

Functionally identical for `|jitter_ms| ≤ 150` (current bound), more obvious intent.

#### 8. `fill_template` silently drops `{}` placeholders past index 2 — `spawn.rs:336-351`

Intentional per the comment, but a debug-only `assert!(iter.next().is_none())` would catch corpus-edit typos that add a third placeholder.

#### 9. `dataforts.rs:79-83` swallows publish failures

```rust
if let Ok(blob) = publish_blob_ref(...).await {
    stored.push(blob);
}
```

If every publish fails (broken adapter), the loop completes silently with `stored.is_empty()` and the fetch loop skips. The demo would render an adapter with zero blobs and the operator would have no signal why. A `tracing::warn!` (or `eprintln!` matching the migrator's style) on the failure arm would surface the issue.

#### 10. Tests boot a 9-node cluster every run — `spawn.rs:369`

`#[tokio::test]` with `sleep(3 s)` on top of ~2 s boot = ~5 s minimum per run. Fine as an integration test but worth flagging — if the deck test suite ever grows, consider `#[ignore]`-by-default + a CI lane that opts in.

### Recommended actions

| Severity | Action |
|---|---|
| Done | `ClusterConfig.verifier` field installed on every node's SDK; demo wires it via `build_cluster(verifier)`; regression test in `cluster_harness.rs::verifier_threads_through_to_every_node` (item 1). |
| Done | Re-flow `main.rs:46-66` comments + drop the `samples-logs` reference (item 2). |
| Done | Reword `migrator::install_factories` doc to "Single-shot" (item 3). |
| Done | Sweep "5-node" comments + capacity literals → `DEMO_NODE_COUNT` / derived counts (items 4, 5); extract `RESPONDER_COUNT` constant (item 6). |
| Done | Simplify jitter math, add `debug_assert!` on extra `{}` in `fill_template`, surface dataforts publish failures via stderr (items 7, 8, 9). |

No blockers. The demo refactor is well-structured, well-tested, and well-documented. The verifier wiring (item 1) is the only thing worth fixing before "real signing" appears in any demo script.

---

## Phase 3 — cross-language SDK surfaces

### Files reviewed

The largest phase — ~23K LOC across 5 languages × 2 surfaces (MeshOS + Deck) + tests + CI. Reviewed canonical surfaces and C examples directly; dispatched parallel agents for cross-binding consistency and native-SDK lifetime review.

Canonical C headers (read directly):

- `include/net_meshos.h` (524 LOC) — MeshOS daemon-author SDK
- `include/net_deck.h` (899 LOC) — Deck operator-side SDK
- `examples/{meshos,deck}.c` — exercise the canonical surfaces end-to-end

Rust FFI implementations:

- `bindings/node/src/{deck,meshos,lib,identity}.rs` (~2,750 LOC) — napi-rs
- `bindings/python/src/{deck,meshos,lib}.rs` (~2,880 LOC) — pyo3
- `bindings/go/{deck,meshos}-ffi/src/lib.rs` (~5,640 LOC) — cdylib

Native SDKs:

- `sdk-ts/src/{deck,meshos}.ts` (~1,470 LOC)
- `sdk-py/src/net_sdk/{deck,meshos}.py` (~1,100 LOC)
- `bindings/go/net/{deck,meshos}.go` (~3,000 LOC)

Tests + CI:

- `bindings/node/test/{deck,meshos}.test.ts` (697 + 414 LOC)
- `bindings/python/tests/test_{deck,meshos}.py` (803 + 575 LOC)
- `.github/workflows/ci.yml` (+125 LOC)

### Strengths

- **C headers are reference-quality.** Every function has an ownership contract, every status code is enumerated, the thread-local last-error envelope (`<<deck-sdk-kind:KIND>>MSG` / `<<meshos-sdk-kind:KIND>>MSG`) is documented up-front, and the typestate ICE flow (two distinct opaque pointer types) is enforced at the C boundary itself.
- **C examples are tight.** `examples/{meshos,deck}.c` walk the canonical workflow (sdk_start → register → publish → control RX → graceful_shutdown → free), exercise the `last_error` helpers, and stay under 230 LOC each. Easy to copy as a starting point.
- **CI matrix expanded thoughtfully.** New `ffi-tests` job runs `cargo test --lib` for every FFI shim (`bindings/{node,python,go-{compute,rpc,meshdb,deck}-ffi}`) — previously the `ffi-clippy` matrix linted these but their internal `#[cfg(test)]` modules went silently uncovered. The node entry threads `RUSTFLAGS="-C link-arg=-Wl,--unresolved-symbols=ignore-all"` to defer napi runtime symbols (commented in-line). Doctests added for the SDK with the broadened feature set.
- **Typestate preserved across all 4 surfaces.** `IceProposal` vs `SimulatedIceProposal` are distinct types in C (opaque pointers), Go, Python (via class separation; mypy/pyright catches misuse), TypeScript (compile-time), and Node FFI. Calling `commit()` on an unsimulated proposal fails at compile time in TS/Go, at type-check time in Python, and at link/import time in C.
- **Error envelope is consistent.** All four FFIs emit the same `<<deck-sdk-kind:KIND>>MSG` / `<<meshos-sdk-kind:KIND>>MSG` envelope. Node uses `deck_err`/`sdk_err` helpers; Python builds typed exceptions (`DeckSdkError`, `MeshOsSdkError`) with `.kind` + `.message` attributes; Go provides typed `*DeckSdkError`/`*MeshOsSdkError` plus sentinel errors (`ErrDeckEndOfStream`, etc.) for `errors.Is` routing — the most thoughtful of the three.
- **Go binding is the cleanest of the three native SDKs.** Every handle has a `Free()` method registered with `runtime.SetFinalizer` at construction; `Free` clears the finalizer + nils the pointer to be idempotent; `Close()` is exposed as a synonym on streams.

### Bugs / Real issues

#### 1. Surface gap: `net_deck_client_new` standalone constructor missing from Node + Python

The C header (`net_deck.h:224-233`) advertises a standalone factory that builds a private supervisor runtime from just an operator seed + config — the "operator-only mode" prominently documented at `net_deck.h:26-33`. Go exposes it. Node (`bindings/node/src/deck.rs:524-555`, `DeckClient.fromMeshos`) and Python (`bindings/python/src/deck.rs:549-585`, `PyDeckClient.from_meshos`) only expose the `from_meshos` path that requires a pre-built `MeshOsDaemonSdk`. Consumers in those languages can't reach the documented operator-only mode.

#### 2. `build_core_proposal` silently falls back to `thaw_cluster` on unknown variants

In all three FFIs (`bindings/go/deck-ffi/src/lib.rs:1789-1792`, `bindings/node/src/deck.rs:1076-1080`, equivalent in Python):

```rust
// `#[non_exhaustive]` substrate enum — new variants fall
// back so bindings stay forward-compatible.
_ => client.ice().thaw_cluster(),
```

The comment is exactly backwards. If a new variant lands in the substrate before the binding is updated, every saved proposal carrying that new variant will commit as a `ThawCluster` — the most destructive ICE action. Should error with kind `unknown_action`.

#### 3. `_simulate` consumed-state sentinel reads back as a valid timestamp

`bindings/go/deck-ffi/src/lib.rs:2096-2107` marks a consumed proposal by setting `issued_at_ms = u64::MAX`. But `_ice_proposal_issued_at_ms` (`:2030-2034`) happily returns `u64::MAX` to the caller after simulate without flagging the husk. A consumer that pins `issued_at_ms` from the proposal then re-reads after simulating sees a wildly different number with no error. The simulated form uses a separate `consumed: bool` field — same shape should apply here.

Also, the docstring at `:2037-2041` claims `_simulate` "consumes by taking it out of the box" but the implementation clones the action and leaves the original in the box until `_proposal_free` runs. Stale doc, not a hazard.

#### 4. `commit` re-runs `simulate` from scratch in all three FFIs

Both `SimulatedIceProposal.commit` (`bindings/node/src/deck.rs:1340-1346`, `bindings/python/src/deck.rs:1453-1457`, `bindings/go/deck-ffi/src/lib.rs:2240-2244`) re-run `proposal.simulate().await` inside the commit instead of using the already-computed simulator state. Each binding stores the raw `IceActionProposal` and re-simulates from scratch on commit.

If the cluster snapshot moves between `simulate()` and `commit()`, the blast-radius and signed payload diverge from what `commit` actually validates against. The substrate may consider this acceptable (the action + signatures bind to `(issued_at_ms, blast_hash)` deterministically; re-sim should produce the same `blast_radius` for the same snapshot), but in tests with a noisy supervisor it's a real race window. Worth a substrate-side audit before claiming the simulated handle is load-bearing.

#### 5. TypeScript handle classes lack any explicit teardown

- `DeckClient` (`sdk-ts/src/deck.ts:313`) — no `close()`, no `[Symbol.asyncDispose]`. The cdylib supervisor inside the napi handle only releases when GC'd.
- `IceProposal` / `SimulatedIceProposal` (`sdk-ts/src/deck.ts:740, 755`) — same.

`MeshOsDaemonSdk` does have `shutdown()` (`sdk-ts/src/meshos.ts:319`) and `MeshOsDaemonHandle.gracefulShutdown` (`:429`), but neither implements `Symbol.asyncDispose`, so `await using` doesn't work — surprising given the file is otherwise modern TS. A long-running Node script that builds 100 proposals and commits one leaks 99 supervisor refs until GC.

#### 6. Python `DeckClient` lacks `__enter__`/`__exit__`; inconsistent with `MeshOsDaemonSdk`

`MeshOsDaemonSdk` and `MeshOsDaemonHandleWrapper` both implement `__enter__`/`__exit__` (`sdk-py/src/net_sdk/meshos.py:281-294, 449-458`). `DeckClient` (`sdk-py/src/net_sdk/deck.py:424`) has no `__enter__`/`__exit__`, no `close()`, no `__del__` — same shape applies to `IceProposal` (`:576`) and `SimulatedIceProposal` (`:595`), and to `SnapshotStream` / `StatusSummaryStream` / `LogStream` / `FailureStream` (they have `close()` but no context-manager dunder). The package establishes the `with` pattern on the daemon side then drops it on the deck side; surprising for callers.

#### 7. Go `pumpControlEvents` goroutine can outlive the handle

`bindings/go/net/meshos.go:1060-1092` spawns the pump on first `ControlEvents()` call; it only exits on `ctx.Done()` or a terminal FFI error. If a consumer calls `ControlEvents(context.Background())` then `Free()`s the handle without cancelling, the goroutine keeps polling on a freed pointer. The `NextControl` null-guard returns `ErrMeshOsInvalidArg`, which is not the documented terminal sentinel, so the pump may not stop. Either treat `ErrMeshOsInvalidArg` as a stop condition or have `Free` cancel an internal context.

#### 8. `subscribe_failures(since_seq)` is required in Python, optional in Node

C requires it (`net_deck.h:465-468`). Python takes `since_seq: u64` (`bindings/python/src/deck.rs:691`). Node accepts `Option<BigInt>` (`bindings/node/src/deck.rs:657`). Pick one — the C surface and Python agree; Node is the outlier.

#### 9. Audit-stream timeout absent from Node and Python

`NetDeckAuditStream` takes `timeout_ms` in C (`net_deck.h:534-538`) and Go (`bindings/go/deck-ffi/src/lib.rs:1621-1685`). Node `AuditStream.nextRecord` (`bindings/node/src/deck.rs:855-869`) and Python `PyAuditStream.__next__` (`bindings/python/src/deck.rs:~1010`) take no timeout — they block indefinitely. Workable but asymmetric vs the other stream surfaces (snapshot / status-summary / log / failure) which do honor a timeout in every binding.

#### 10. `ffi_guard!` policy overstated

`net_deck.h:93-96` and `net_meshos.h:89-92` claim "every FFI entry point" is wrapped by `catch_unwind`. Real gaps in `deck-ffi`:

- Audit-query setters: `net_deck_audit_query_recent` (`:1427`), `_by_operator` (`:1439`), `_between` (`:1451`), `_force_only` (`:1464`), `_since` (`:1473`).
- Operator-identity simple accessors, admin-verifier getters.
- `meshos-ffi`: `net_meshos_process_emit` (`:402`) and `_snapshot_emit` (`:453`).

None of these can realistically panic, so the omission is benign — but the policy statement is wrong. Either wrap them or soften the doc to "every entry point that calls into the substrate."

### Code-quality nits

#### 11. Audit-query setters skip `clear_last_error_inner()`

`bindings/go/deck-ffi/src/lib.rs:1427-1479` — pure setters returning `NET_DECK_ERR_NULL` only on bad pointer, never setting the kind. A consumer that reads `net_deck_last_error_kind()` after a successful setter sees the previous unrelated error.

#### 12. TS exports a missing type name in docstrings

`sdk-ts/src/deck.ts:9, 250, 357, 374` — docstrings claim `AsyncIterable<MeshOsSnapshot>`, but there is no exported `MeshOsSnapshot` type anywhere in the SDK. The actual return is `AsyncIterable<unknown>`. Either define and export the type or change the docstrings.

#### 13. TS reaches across module privacy

`sdk-ts/src/deck.ts:327` — `(sdk as unknown as { raw: never }).raw` reaches into `MeshOsDaemonSdk`'s `private readonly raw`. Works because TS `private` is structural at runtime, but the cast string is opaque and breaks if `MeshOsDaemonSdk` is ever refactored. An explicit internal accessor (`/** @internal */ get rawNapiSdk()`) would be cleaner.

#### 14. Magic poll cadences

- Python `__anext__` (`sdk-py/src/net_sdk/meshos.py:418`) — `asyncio.sleep(0.01)`. Open-coded.
- Python `anext_control` (`sdk-py/src/net_sdk/meshos.py:382-394`) — `poll_ms = 10`. Same value, different code path, neither named.
- Go `meshos.go:1077` — `h.NextControl(50)`, the 50ms cadence justified only in the docstring on `:1058`.

Should be named module-level constants.

#### 15. Python `_caps` keyword leak

`sdk-py/src/net_sdk/meshos.py:438` — pyo3 binding's parameter is `_caps`; the wrapper passes through as `_caps=caps`. Underscore-prefixed kwargs across a public boundary is a Rust-side smell; the wrapper should either alias explicitly or have the binding use a public name.

#### 16. TS `type bool = boolean` alias

`sdk-ts/src/deck.ts:146-152` adds a local `type bool = boolean` solely so one field reads non-noisily for a linter. Use `boolean` directly.

#### 17. Python `_HAS_VERIFIER = False` doesn't prune `__all__`

`sdk-py/src/net_sdk/deck.py:108-109` — when the wheel lacks the verifier symbols, the `OperatorRegistry`/`AdminVerifier` names are never bound but remain in `__all__`. `from net_sdk.deck import *` will then raise `AttributeError`.

#### 18. Option drift between TS / Py / Go

TS exposes `callbackTimeoutMs` in `MeshOsDaemonSdkOptions` (`sdk-ts/src/meshos.ts:256`); Python's `start()` exposes only `control_capacity` (`sdk-py/src/net_sdk/meshos.py:238`); Go's `MeshOsConfig` has neither (`bindings/go/net/meshos.go:432-447`). Probably intentional (the callback timeout is napi-specific because JS callbacks can stall the supervisor) but worth confirming and documenting.

#### 19. Go stream `Close()` + `Next()` race

`s.Close()` and `s.Free()` both call into `C.net_deck_*_stream_free` (e.g. `bindings/go/net/deck.go:1013-1019`). If a caller closes the stream while another goroutine is blocked in `Next(timeout)`, there's a use-after-free against `s.ptr`. Either guard with a sync.Mutex or document "no concurrent Next + Close."

#### 20. `audit_stream_next(timeout_ms=0)` semantically inconsistent

In `net_deck_snapshot_stream_next` / `_log_stream_next` (`net_deck.h:347, 453`), `timeout_ms == 0` is unbounded. In `net_deck_audit_stream_next` (`bindings/go/deck-ffi/src/lib.rs:1644-1683`), `timeout_ms == 0` means "no timeout wrap," and `None` from `inner.next().await` maps to `NET_DECK_ERR_END_OF_STREAM` rather than `OK` with NULL. The C header is silent on this distinction.

#### 21. Mutex `.unwrap()` on poisoned lock

`bindings/go/meshos-ffi/src/lib.rs:1327` and similar handle-guarded accesses unwrap the `Mutex` guard. Any prior panic on another thread leaves the lock poisoned; the next FFI call re-panics, gets caught by `ffi_guard!`, and the handle is permanently dead. Consider `lock().unwrap_or_else(|e| e.into_inner())` if recovery is desirable.

#### 22. Minor Go cgo patterns

- `bindings/go/net/meshos.go:769` — `unsafe.Pointer(uintptr(cgoHandle))`. The documented `cgo.Handle` idiom; `go vet` may flag the pattern. Worth a comment that the runtime keeps the value alive.
- `bindings/go/net/deck.go:1143` — manual pointer arithmetic into `**C.char`. `unsafe.Slice(records, int(count))` (Go 1.17+) is idiomatic; arithmetic form is harder to audit.

### Test coverage gaps

Test breadth is good (~125 tests across TS/Py for the two SDKs, plus C examples) but several common-failure axes aren't covered:

- **No concurrency tests** in any binding (parallel admin commits, simultaneous stream + shutdown, two `registerDaemon` calls in parallel).
- **No GC / finalization tests** on Node/Python bindings — relevant given the `DeckClient` lifetime gaps (items 5, 6).
- **No shutdown-while-iterating** tests on async streams.
- **No filter-actually-filters** test for `subscribeLogs` (binding accepts a filter; nothing asserts it's honored).
- **No `droppedControlEvents` counter test** on the MeshOS surface (the FFI exposes it; the C header documents it).
- Python has the best async coverage — an iterator stop-on-shutdown test (`tests/test_meshos.py:511`) and an anext-timeout test (`:558`) that the TS suite lacks.

### Recommended actions

| Severity | Action |
|---|---|
| Done | Node `DeckClient.new(seed, meshosConfig?, deckConfig?)` and Python `DeckClient(seed, meshos_config=None, deck_config=None)` factories now construct a standalone client owning a private supervisor (parity with the cdylib's `net_deck_client_new`). Regression tests in both language test suites (item 1). |
| Done | `build_core_proposal` returns `Err("unknown_action")` for unmapped variants in all three FFIs (Node / Python / Go); callers `?`-bubble through their respective error envelopes (item 2). |
| Done | `NetDeckIceProposal` now carries a `consumed: bool` flag (matching `SimulatedIceProposal`); `issued_at_ms` survives `_simulate` consumption. Regression: `ice_issued_at_ms_survives_consumption_by_simulate` (item 3). |
| Verify | Audit whether `commit` re-running `simulate` is safe under cluster-state movement (item 4). |
| Done (partial) | TS `DeckClient.close()` + `[Symbol.asyncDispose]` drain the private supervisor (only fires for clients built via `DeckClient.new`; no-op for `fromMeshos`). `IceProposal`/`SimulatedIceProposal` close() not added — they hold no heavy resources and GC naturally (item 5). |
| Done (partial) | Python `PyDeckClient.close()` drains the owned SDK (no-op for `from_meshos`); `sdk-py` wrapper exposes `from_seed` factory + `close()` + `__enter__`/`__exit__`. ICE proposal context-manager support not added — same rationale as TS (item 6). |
| Done | `MeshOsDaemonHandle.Free()` closes an internal `pumpStop` channel that `pumpControlEvents` selects on alongside the caller's `ctx`. Removes the use-after-free race when `Free` ran without ctx cancellation (item 7). |
| Wontfix | Verified — Python's `subscribe_failures` carries `#[pyo3(signature = (since_seq=0))]` so it defaults to 0 the same way Node's `Option<BigInt>` does. Behavior already matches; the "Python requires it" claim in the review was a misread. C is the strict outlier (no defaults possible) but that's expected (item 8). |
| Wontfix | The asymmetry is structural, not a deck oversight: Node and Python use async streams (caller cancellable via `Promise.race`/`AbortController` and `asyncio.wait_for` respectively) — *every* stream-next on those bindings (snapshot, status-summary, log, failure, audit) is timeout-less. Only C/Go's cdylib idiom requires per-call `timeout_ms`. Adding it to AuditStream alone would be inconsistent with the other Node/Python streams (item 9). |
| Done (Rust FFI) | Softened the `catch_unwind` doc claim in `net_deck.h` + `net_meshos.h` to "every entry point that calls into the substrate" (item 10); audit-query setters now `clear_last_error_inner()` on success (item 11); `meshos-ffi` `.lock().unwrap()` calls upgraded to `.unwrap_or_else(\|e\| e.into_inner())` for graceful poison recovery (item 21). |
| Polish (remaining) | Fix missing `MeshOsSnapshot` type export (item 12); replace TS `(sdk as unknown as { raw: never }).raw` hack (item 13); name the magic poll cadences (item 14); rename Python `_caps` kwarg (item 15); drop `type bool` alias (item 16); prune `__all__` when verifier missing (item 17); document or align option drift (item 18); fix Go stream Close/Next race (item 19); document or align audit-stream timeout semantics (item 20); improve Go cgo idioms (item 22). |
| Tests | Add concurrency, GC, shutdown-while-iterating, and filter-correctness tests (test gaps section). |

No blockers for the surface itself — the design is sound. Items 1, 2, 3, 5, 6 are the ones I'd want fixed before tagging a "v1" of any of these SDKs.

---

## Cross-phase summary

| Severity | Phase | Item |
|---|---|---|
| Real bug | 1 | Double-Arc in `MeshNode::rpc_observer` (Phase 1 item 1) |
| Real bug | 2 | `AdminVerifier` built and discarded in demo (Phase 2 item 1) |
| Real bug | 3 | `build_core_proposal` silent fallback to `thaw_cluster` (Phase 3 item 2) |
| Real bug | 3 | `_simulate` sentinel reads back as valid timestamp (Phase 3 item 3) |
| Surface gap | 3 | `net_deck_client_new` missing from Node + Python (Phase 3 item 1) |
| Footgun | 3 | TS/Python `DeckClient` + ICE proposals lack explicit teardown (Phase 3 items 5, 6) |
| Footgun | 3 | Go `pumpControlEvents` outlives handle (Phase 3 item 7) |
| Footgun | 1 | `pub` fields on `ClusterNode` invite out-of-order shutdown (Phase 1 item 2) |
| Breaking | 1 | SDK `default` features expansion needs CHANGELOG (Phase 1 item 10) |
| Verify | 3 | `commit` re-runs `simulate` — race window? (Phase 3 item 4) |
| Verify | 1 | Replica-group sync health assertion (Phase 1 item 11) |

**Bottom line:** the branch is in shippable shape. The architecture is sound, the test coverage is broad, the docs are reference-quality. The 4 real bugs above are localized fixes (≤50 LOC each); the footguns are missing-method additions; the surface gaps need binding-side glue, not new substrate work. No blocker for merging once items 1.1, 2.1, 3.1, 3.2, 3.3 land.
