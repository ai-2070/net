# Code Review — NAT Traversal V2 (branch `nat-traversal-improvements`, 2026-07-11)

Scope: `git diff master...HEAD` for branch `nat-traversal-improvements`
(merge-base `727d69ebf`). ~5,700 lines of source across the rendezvous
runtime, the background direct-path upgrade (Stage 3), coordinator
auto-selection (Stage 3a), failure-reason stats + full FFI/Go/Node/Python
parity (Stage 5), the real-NAT scenario harness (Stage 4), plus incidental
fixes to `dataforts/blob`, `redex/file`, and `timestamp`.

Reference design: `docs/plans/NAT_TRAVERSAL_V2_PLAN.md`,
`docs/plans/NAT_TRAVERSAL_PLAN.md`. Prior review:
`CODE_REVIEW_2026_06_21_NAT_TRAVERSAL.md` (Findings 1–5, all resolved on this
branch).

---

## Overall assessment

High-quality, well-documented work, consistent with the subsystem's existing
posture. Verified as clean during this pass:

- **Stats plumbing** has a single `snapshot()` source of truth; the
  `#[repr(C)] NetTraversalStatsV2` struct, both C headers (`go/net.h`,
  `include/net.go.h`), and the Go/Node/Python bindings match field-for-field
  in order and width **today**.
- **`RendezvousBudgets`** is genuinely atomic — check+increment happen under
  the DashMap entry lock; the concurrent-train slot is a correct CAS. No
  TOCTOU, no slot leak on the paths reviewed.
- **`request_punch` / `await_punch_introduce`** reject+timeout arms don't
  double-count and don't leak the waiter map entry.
- The **backoff shift** is bounded (`MAX_SHIFT = 5`, `saturating_mul`), no
  overflow.
- **`select_punch_coordinator`** never selects self or the target and doesn't
  panic on an empty candidate set.
- The **blob scheme-gate** (`dataforts/blob/dispatch.rs`) is symmetric with
  `resolve` and can't be trivially bypassed.

The "NAT traversal is an optimization, not correctness" framing still holds:
**none of the findings below break the connectivity contract** — every failure
path falls back to the routed handshake. The two most severe items are (1) a
broken test harness and (2) a stale-cache pin plus lifecycle leak in the new
Stage-3 upgrade path.

### Process caveat

This review ran as a 10-angle parallel finder sweep. An account session
rate-limit terminated 6 of the 10 finder agents mid-run; the reuse, efficiency,
altitude, and conventions angles completed, and the correctness + FFI-parity
angles were re-run and **verified by hand** (findings 1–3 below were confirmed
by direct code reading). Recall on *lower-severity* correctness bugs is
therefore somewhat below a full clean run — a follow-up pass focused on the
receive-loop dispatch and session-migration races would be worthwhile before
sign-off.

---

## Findings

| # | Severity | Area | Status |
|---|---|---|---|
| 1 | 🔴 Bug | natsim Stage-4 harness | Confirmed — suite non-functional |
| 2 | 🔴 Bug | Stage-3 `upgrade_cache` lifecycle | Confirmed |
| 3 | 🟠 Leak | Stage-3 upgrade loop `Arc<Self>` | Likely — needs a decision |
| 4 | 🟡 Design | Coordinator selection is deterministic | Confirmed |
| 5 | 🟡 Design | FFI stats parity is hand-maintained | Confirmed (latent) |
| 6 | 🟡 Design | Duplicated direct/relayed predicate | Confirmed |
| 7 | 🟢 Perf | `capability_tags_for` 1+N in coordinator pick | Confirmed |
| 8 | 🟢 Perf | Upgrade scan re-fetches `peers.get()` per tick | Confirmed |
| 9 | 🟢 Perf | Eager Arc clones on punch happy path | Confirmed |
| 10 | ⚪ Note | Uncached-reflex introduce admits (documented) | By design |

---

## Resolution status — 2026-07-12 fix pass (branch `nat-traversal-improvements`)

All nine actionable findings are fixed across dedicated fix-pass commits —
each finding in its own commit except the three perf cleanups (#7–#9),
which are batched into a single commit (`67eb8e417`) — with tests where
the behavior is observable. Full suite green: lib unit tests
(4582 passed), plus the NAT integration suites (`direct_upgrade`,
`coordinator_selection`, `rendezvous_coordinator`,
`rendezvous_introduce_validation`, `nat_matrix`, `traversal_observability`,
`peer_death_clears_capability_index`) and the SDK `mesh_nat_traversal` suite.

| # | Status | Commit | Test |
|---|---|---|---|
| 1 | ✅ Fixed | `aad3ff4c1` | `NATSIM_OUTCOME_PATH=` marker parsed by `tests/natsim.rs` (verified standalone) |
| 2 | ✅ Fixed | `d524d96fe` | `failed_peer_drops_its_upgrade_cache_entry`, `single_punch_pair_is_deferred_not_terminal` |
| 3 | ✅ Fixed | `9585e1d51` | `upgrade_loop_does_not_leak_the_node` |
| 4 | ✅ Fixed | `27e92b51e` | `select_spreads_across_equal_candidates` (+ updated tier-2/3 tests) |
| 5 | ✅ Fixed | `159dc69cc` | `go/header_parity_test.go` struct-field parity + `ffi::mesh` `c_header_layout_matches_rust_repr_c` |
| 6 | ✅ Fixed | `3c88f885d` | covered by the coordinator/upgrade suites (refactor, no behavior change) |
| 7 | ✅ Fixed | `67eb8e417` | `nodes_with_capability_tag_filters_the_batch` |
| 8 | ✅ Fixed | `67eb8e417` | covered by `direct_upgrade` (candidate filter behavior unchanged) |
| 9 | ✅ Fixed | `67eb8e417` | covered by `rendezvous_coordinator` (reject-path behavior unchanged) |
| 10 | ➖ No change | — | Documented by-design tradeoff; bounded by the responder budgets. |

Design decisions worth noting:

- **#2** — the cache entry is dropped in the failure-detector `on_failure`
  callback (alongside `capability_fold` / `subscriber_chains`), not the
  slow heartbeat sweep, so it's fast and testable. `done` is reserved for
  `SkipPunch`; `SinglePunch` defers (`SINGLEPUNCH_RECHECK = 30s`).
- **#3** — followed the `spawn_capability_reannounce_loop` `Weak`-then-
  `upgrade()` pattern rather than a strong `Arc::clone(self)`, so `Drop`
  wins the refcount race.
- **#4** — spread via a freshly-seeded `RandomState` hash (the crate's own
  `dedup_state` OS-entropy trick), dependency-free; tier 1 (routing
  next-hop) stays deterministic.
- **#5** — closed both seams: C↔C (Go guard, struct fields) and Rust↔C
  (offset-level cross-check via `offset_of!`).

---

### 1 — 🔴 The natsim Stage-4 harness is broken at the outcome-parse step

**Files:** `examples/natsim_node.rs:414-415`, `tests/natsim/run_scenario.sh`
(final three lines), `tests/natsim.rs:72-77`.

The initiator writes its verdict with `serde_json::to_vec_pretty(&outcome)`,
which emits **no trailing newline** — the file ends with `}`. The scenario
runner then prints the path immediately after the file body:

```bash
cat "$OUTCOME"      # prints ...}  (no trailing newline)
echo "$OUTCOME"     # path lands on the SAME physical line as the closing brace
```

So the last physical line of stdout is `}/tmp/natsim.XXXXXX/a_outcome.json`.
The Rust wrapper parses the path as the last line:

```rust
let path = stdout.trim().lines().last().expect("outcome path on last line");
let bytes = std::fs::read(path).expect("read outcome json");   // reads "}/tmp/..."
```

`std::fs::read("}/tmp/...")` fails → **panic `"read outcome json"`**. Every
scenario that reaches the verdict step fails, so the Stage-4 real-NAT matrix
(a headline deliverable of this branch) cannot actually gate anything. Latent
because the suite needs root + netns and is unlikely to have run green yet.

**Fix (pick one):**
- Emit an unambiguous, separately-parsed marker line, e.g.
  `printf '\nOUTCOME_PATH=%s\n' "$OUTCOME"` and have `tests/natsim.rs` scan for
  the `OUTCOME_PATH=` prefix instead of "last line"; or
- Write the outcome JSON with a trailing newline (append `b"\n"` in
  `write_atomic`, or use `to_vec_pretty` + push `\n`); or
- Add a blank `echo` between `cat "$OUTCOME"` and `echo "$OUTCOME"`.

The marker-prefix option is the most robust — it stops depending on the JSON
serializer's trailing-whitespace behavior.

---

### 2 — 🔴 `upgrade_cache` entries are never cleared or removed

**File:** `adapter/net/mesh.rs:13416-13486` (mutators), `:13418` (`should_attempt`).

`upgrade_cache: Arc<DashMap<u64, UpgradeCacheEntry>>` is only ever **read**
(`13418`) or **upserted** (`13430`, `13445`, `13460`, `13476`). Nothing removes
an entry, and nothing ever clears the `done` flag. Three consequences:

1. **Direct→relay regression is never re-upgraded.** After
   `upgrade_record_done` sets `done = true` (`13452`), a peer whose direct path
   later dies (NAT rebind, peer restart) and falls back to a relay-routed
   session is pinned to the relay for the rest of the process's life —
   `upgrade_should_attempt` returns `false` forever.

2. **`done` conflates "impossible" with "not eligible right now."**
   `PairAction::SinglePunch | SkipPunch` are both marked `done` at
   `13557-13558`. But **this same PR** adds reflex-drift reclassification, which
   can later flip the local node's NAT class so that a `SinglePunch` pair
   becomes `Direct` and punchable. The `done` cache blocks the scan from ever
   revisiting it, so the session keeps paying relay hops despite a now-available
   direct path. `SkipPunch` (truly terminal) and `SinglePunch` / regressed
   (temporarily ineligible) deserve different treatment.

3. **Unbounded growth.** Entries are never dropped when a peer leaves
   `self.peers`, so a node with high peer churn accumulates `UpgradeCacheEntry`
   records without bound (small per entry, but monotonic).

**Fix:** remove the peer's `upgrade_cache` entry wherever the session/peer is
torn down (co-locate with the `self.peers` / `addr_to_node` cleanup), and split
the terminal state: keep `done` only for `SkipPunch`; for `SinglePunch` and for
a session that has regressed to relay, use a backoff/defer so the scan can
re-evaluate after the next classification.

---

### 3 — 🟠 The Stage-3 upgrade loop holds a strong `Arc<Self>` and is spawned unconditionally

**File:** `adapter/net/mesh.rs:13651` (capture), `:3977` (unconditional spawn in
`start_arc`).

`start_arc()` unconditionally calls `spawn_direct_upgrade_loop()`, which
captures `let node = Arc::clone(self)` — a **strong** self-reference held for
the task's whole life. The loop exits only when `shutdown` is set, and the only
non-explicit path that sets `shutdown` is `Drop for MeshNode` (`:14754`). But
`Drop` cannot run while a spawned task holds a strong `Arc`. So an
`Arc<MeshNode>` created with `auto_direct_upgrade = true` and then **dropped
without an explicit `shutdown().await`** never tears down: the node, its UDP
socket, and every background task leak while the 1 s scan spins forever.

Two mitigating facts, stated for honesty:
- The task early-returns and releases its Arc when `auto_direct_upgrade` is
  `false` (`:13656`), so the leak only applies when the feature is on.
- This matches the **existing** `spawn_nat_classify_loop` pattern (`:3998` also
  uses `Arc::clone(self)`), so a "must call `shutdown()` explicitly" contract
  already exists for Arc nodes that spawn the classify loop.

What's new: `start_arc` now spawns a strong-Arc loop **unconditionally**, so an
Arc node that previously relied on `Drop`-based teardown (feature on, classify
loop not separately spawned) now leaks where it didn't before.

**Fix:** use the `self_weak` + `.upgrade()` pattern already used by
`spawn_capability_reannounce_loop` (`:4180`, `:4197`) so `Drop` can win the
refcount race and tear the loop down. If a strong ref is intentional, document
the "callers of `start_arc` with `auto_direct_upgrade` MUST call `shutdown()`"
contract at the `start_arc` call site.

---

### 4 — 🟡 Coordinator auto-selection is deterministic lowest-node-id → hotspot / SPOF

**File:** `adapter/net/mesh.rs:~11906`.

For a given candidate set, `select_punch_coordinator`'s tier-2/3 pick is the
deterministic minimum `node_id`. Every requester in the mesh therefore converges
on the same lowest-id relay-capable peer, concentrating all rendezvous mediation
— and the coordinator-side per-requester budgets — on one node. That's a
throughput hotspot and a single point of failure for the punch subsystem. The
comment flags random-two-choices as a "future refinement," but the
deterministic-min is what ships, so the load-balancing story is currently a
stub layered on real selection.

**Fix:** implement the random-two-choices (or power-of-two) pick the comment
references, or at minimum jitter the selection by a per-requester hash so the
load spreads.

---

### 5 — 🟡 FFI stats parity is hand-maintained across 5+ sites and the guard doesn't cover struct fields

**Files:** `ffi/mesh.rs` (`fill_traversal_stats_v2`), `go/net.h` +
`include/net.go.h` (`net_traversal_stats_v2_t`), `go/mesh.go:~581` (struct
literal), `bindings/node/src/lib.rs`, `bindings/python/src/lib.rs`
(+ `_net.pyi`), guard: `go/header_parity_test.go`.

Parity holds **today**, but the field list is copied by hand across the core
snapshot, the FFI fill, three C/Go/binding struct layouts, and the Node/Python
mappers. The parity guard (`header_parity_test.go`) compares function
declarations and `NET_*` constants between the two C headers, but for typedefs
it records only the **name** (`typedefRe → typedefs map[string]bool`) — it never
compares struct **fields**, and it never compares either C header against the
Rust `#[repr(C)]` struct at all. So a field added or reordered in the Rust
struct but not both hand-maintained C headers is **silent ABI corruption** (cgo
reads wrong offsets → Go sees garbage) with no compile error and no test
failure. Adding one counter today means editing ~8 lists correctly.

**Fix:** add a field-level parity assertion (a small test that pins the ordered
field list + widths of `net_traversal_stats_v2_t` and asserts it equals the
Rust struct's field order), or generate the C struct + binding mappers from one
declaration. At an `unsafe`, `#[repr(C)]` cgo boundary this is worth the
codegen.

---

### 6 — 🟡 The direct-vs-relayed predicate is inlined at 3 sites with *diverging* defaults

**File:** `adapter/net/mesh.rs:11892` (coordinator eligibility), `:13529`
(`attempt_direct_upgrade`), `:13619` (`upgrade_is_loop_candidate`).

`self.addr_to_node.get(addr).map(|n| *n != peer_id).unwrap_or(?)` appears three
times. The `unwrap_or` default differs: `true` ("assume relayed") in the upgrade
paths but `false` in coordinator eligibility. A change to how routed sessions
are marked must be found and fixed in all three, and the diverging defaults mean
the copies can already **silently disagree** about whether a given session is
direct.

**Fix:** extract one `fn is_relayed_peer(&self, peer_id, addr) -> bool` (with a
single documented default) and call it from all three sites.

---

### 7 — 🟢 `select_punch_coordinator` calls `capability_tags_for` once per direct peer (1+N lock + alloc)

**File:** `adapter/net/mesh.rs:~11902`.

Inside the candidate loop, `capability_tags_for` is called per direct peer; each
call takes the capability fold-state lock and heap-allocates a
`HashSet<String>` + `Vec<String>` (cloning every tag string) merely to test
membership of one tag (`RELAY_CAPABLE_TAG`). For a node with N direct peers that
is N lock acquisitions and N full tag-set materializations, all discarded after
a single `.any()`.

**Fix:** hoist one `capability_tags_for_all(fold)` before the loop (the
codebase's own documented remedy for this 1+N pattern), or add a non-allocating
`fold_has_tag(node, RELAY_CAPABLE_TAG)` helper.

---

### 8 — 🟢 The upgrade scan re-fetches `peers.get()` for a peer already in hand every tick

**File:** `adapter/net/mesh.rs:13673` (iterate), `:13615`
(`upgrade_is_loop_candidate` re-lookup).

The scan iterates `node.peers.iter()` and, inside the filter,
`upgrade_is_loop_candidate` does a fresh `self.peers.get(&peer_id)` for the peer
already yielded by the iterator — a redundant shard-lock lookup on the map being
iterated, plus a fresh candidate `Vec`, every 1 s, forever, even in steady state
where every session is already direct/done.

**Fix:** read `entry.value().addr` directly from the iterator entry, and skip
the tick entirely when there are no relay-routed peers rather than re-collecting
a candidate `Vec` each second.

---

### 9 — 🟢 `handle_punch_request` eagerly clones two Arcs into a reject closure on the happy path

**File:** `adapter/net/mesh.rs:8866-8867`.

Every well-formed PunchRequest that the coordinator successfully mediates still
performs two `Arc::clone` refcount bumps (the peer session Arc at `8866`, the
socket Arc at `8867`) to build a `send_reject` closure that is only invoked on
the refusal branches — never on the successful-mediation path.

**Fix:** construct the reject payload/closure lazily inside the refusal branches
so the happy path pays nothing.

---

### 10 — ⚪ Note (by design): unsolicited introduce admits when no reflex is cached

**File:** `adapter/net/mesh.rs:9120-9134` (`unsolicited_introduce_permitted`).

When no signed reflex is cached for the claimed counterpart, the anti-reflection
IP check is skipped and the keep-alive train fires at the caller-supplied
`peer_reflex`. This is **intentional and documented** (the young-mesh race where
the counterpart's announcement hasn't folded yet) and is bounded by the
per-source (`punch_trains_per_window`) and global concurrent-train
(`punch_trains_concurrent_max`) budgets, so the residual reflector is small,
authenticated, and rate-limited. Recorded only so the "admit-with-budget on
cache-miss" decision stays a conscious one — the invariant "`peer_reflex`
belongs to the named counterpart" is half-enforced by design. No change
required unless the threat model tightens (in which case: fail closed on
cache-miss and let the responder re-arrive once the announcement folds).

---

## Suggested priority

1. **Finding 1** before relying on the Stage-4 suite for sign-off (it's
   currently DOA).
2. **Findings 2 and 3** next — optimization/lifecycle correctness, not
   connectivity, but both cause steady-state waste that persists for the
   process lifetime.
3. **Findings 4–6** as design cleanups (4 matters most operationally).
4. **Findings 7–9** as opportunistic perf cleanups.

## Also worth a targeted follow-up pass

Because the correctness sweep was interrupted (see process caveat), the
session-migration race surface — concurrent `attempt_direct_upgrade` vs. an
inbound session rotation, and the CAS-install (`connect_via_cas`,
`AddrInstallMode::DirectOverwrite`) under parallel load — deserves a dedicated
read before merge. The lease-before-spawn guard (`:13683`) looks correct for
same-node double-fire, but cross-node / inbound-rotation interleavings were not
exhaustively traced in this pass.
