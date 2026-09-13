# Test execution performance audit — 2026-09-13

Where test time is actually spent, locally and in CI, measured rather than
reasoned about. `TESTS.md` in the repository root carries the guidance that
follows from this; this file carries the numbers, so the guidance can be
re-derived when the numbers move.

**Measurement environment.** Windows 11, `x86_64-pc-windows-msvc`, 24 logical
cores, rustc 1.98.1, cargo-nextest 0.9.143, warm `target/` unless stated. CI
figures come from GitHub Actions run `34722972183` (the last green run on
`LZL0/webrtc-transport` at the time of audit) and from the repository's own
`actions/caches` API.

---

## 1. Local: compilation dominates, execution does not

Against the `integration-net-core` family (47 integration binaries, 469
tests), which is the largest thing a developer runs in one command:

| operation | time |
|---|---|
| executing all 469 tests (nextest, 24 threads) | **11.2 s** |
| sum of individual test durations | 174.8 s (so ~15× parallel speedup) |
| nextest + cargo overhead around the run | 4.9 s |
| warm `cargo build` of the 47 binaries, nothing changed | 2 s |
| recompile `net-mesh` after a lib edit | 9.4 s |
| relink one test binary after a test-file edit | 1.68 s |
| cold build of the 47 binaries (lib + links) | 98 s |
| **first build of a feature set not built before** | **87–98 s** |
| switching back to a previously built feature set | ~12 s |

Single-binary detail (`three_node_integration`, 66 tests — the binary with
the most fixed `sleep` in the tree):

| operation | time |
|---|---|
| execution | 5.2 s |
| warm re-run, end to end | 6.3 s |
| same command when the feature graph is cold | 107 s |

Interpretation: the suite's own runtime is not the problem. Cargo fingerprints
per feature set, so alternating between the sets CI pins recompiles the root
crate from scratch each time; that single behaviour outweighs everything else
a developer can control.

### 1.1 Per-binary and per-process costs

| operation | time |
|---|---|
| direct `--list` of one 17.2 MB test binary | 0.005 s |
| direct run of one test in that binary | 0.105 s |
| `cargo nextest list`, 1–12 binaries (warm) | 0.8–1.0 s (flat) |

Enumeration is NOT per-binary-expensive once the binaries exist; the earlier
hypothesis that ~170 binaries make listing slow is not supported. What does
cost is any invocation that has to build a target set cargo has not built in
that exact configuration before.

---

## 2. Debug info

`[profile.dev] debug = true` → `"line-tables-only"` (`net/crates/net/Cargo.toml`).

| | full debug info | line tables |
|---|---|---|
| relink `three_node_integration` (warm lib) | 3.18 s | **1.68 s** (−47 %) |
| binary size | 19.4 MB | 17.2 MB |
| side files | 71 MB `.pdb` per binary | — |
| execution, same 66 tests | 5.21 s | 5.21 s |

Artifact mass before the change, in one `target/debug/deps`: **69 `.pdb`
totalling 6.9 GB against 24 `.exe` totalling 667 MB**; whole `target/` reached
24–38 GB. Two builds during this audit died with
`rustc-LLVM ERROR: IO failure on output stream: no space on device`, so this
is a correctness-of-the-workflow issue as much as a speed one.

---

## 3. Levers measured and REFUTED

Recorded so they are not retried.

**Optimising dependencies** — `[profile.dev.package."*"] opt-level = 2`, on the
theory that ed25519/x25519/chacha at `-O0` tax every handshake:

| suite | deps `-O0` | deps `-O2` |
|---|---|---|
| three_node_integration | 5.21 s | 5.11 s |
| channel_auth (Noise XX handshakes) | 2.63 s | 2.46 s |
| capability_auth_conformance | 1.05 s | 0.96 s |
| parallel_bind_stress | 0.39 s | 0.26 s |

2–10 % of execution for a **+24 % cold build** (1 m 27 s → 1 m 48 s). The
suites are timer-bound, not crypto-bound. Net negative in CI, where most
graphs are cold.

**A faster linker** — `-Clinker=rust-lld` on `x86_64-pc-windows-msvc`: relink
**2.0 s vs 1.68 s**, cold build 95 s vs 98 s. No win; rustc already defaults to
`rust-lld` on `x86_64-unknown-linux-gnu`, so there was nothing to take there
either.

---

## 4. Fixed sleeps in the test tree

652 `sleep(Duration::from_{secs,millis}(…))` sites under `net/crates/net/tests/`,
≈303 s of nominal serial sleep (excluding four `from_secs(10_000_000)` TTL
constants that are not waits). Distribution: 94 sites ≥1 s, 140 at
200–999 ms, 418 below 200 ms.

Worst binaries by nominal sleep: `three_node_integration.rs` 19.5 s (20 sites),
`sdk/tests/sensing_consumer.rs` 8.0 s, `sdk/tests/org_exact_sensing.rs` 6.6 s,
`integration_net.rs` 4.0 s, `route_withdraw.rs:127` 4.0 s,
`reflex_override.rs:538` 3.8 s, `rtc_admission.rs` 3.6 s,
`direct_upgrade.rs` 2.4 s.

Why this is a lower priority than the nominal total suggests: nextest runs
tests in parallel processes, so the worst sleeper's 19.5 s nominal shows up as
5.2 s of wall clock. The sleeps matter for flake, and for serial `cargo test`
runs, more than for local wall clock.

Structural notes for anyone who does attack them:

- A poll helper already exists — `tests/common/mod.rs::await_condition`
  (50 ms poll) and `poll_until` (25 ms) — but only 16 of ~170 integration
  binaries `mod common;`, and 11 private re-implementations exist elsewhere.
- Category (a) sleeps (positive "delivery happened" assertions) convert to
  polling with no loss of meaning: `three_node_integration` (20 sites),
  `integration_net` (7), `direct_upgrade` (8), `sensing_fallback.rs:176`.
- Category (c) sleeps are load-bearing NEGATIVE assertions ("nothing happened
  within X") — `route_withdraw.rs:127`, `reflex_override.rs:538`,
  `rtc_admission.rs` ×5. Shrinking the literal makes them vacuous; they must
  be re-expressed as multiples of a *configured* timer.
- Compressed clocks are available for most of the mesh: `heartbeat_interval`,
  `session_timeout`, `min_announce_interval`, `announce_debounce`,
  `sensing_interest_ttl` and others are `MeshNodeConfig` fields. Free win:
  every use of `SENSING_SAMPLE_INTERVAL` (`behavior/org_sensing_demand.rs:137`,
  hardcoded 2 s) is `.min(node.sensing_interest_ttl())`, so lowering that
  config field already compresses it. `rtc/admission.rs:30 PROVISIONAL_TTL`
  has no knob.
- Virtual time is a dead end for `tests/*.rs`: `start_paused` only
  auto-advances when every task is idle, and a live `MeshNode` never is —
  documented at `src/adapter/net/org_routing_wiring_tests.rs:5084-5088`. All
  ~45 `start_paused` sites are in-`src` unit tests.

---

## 5. CI: run 34722972183 (last green)

Wall clock **19.0 min**, 50 jobs, **177 runner-minutes**. There is no `needs:`
edge in `ci.yml`, so wall clock is simply the slowest job.

| job | time | of which |
|---|---|---|
| Rust SDK tests | **19.0 m** | suite 5.53 m; named-witness re-run steps 7.3 m; SDK WebRTC lint/doc 2.37 m; payments 2.68 m |
| Windows security tests | 13.9 m | org units 6.70 m; net-cli suite 4.62 m; clippy 1.37 m |
| Python wheel acceptance | 13.5 m | wheel build (LTO, cgu=1) 8.52 m; full pytest suite 4.37 m |
| Go bindings tests | 10.5 m | libnet cdylib (release+LTO) 6.32 m; go test 3.00 m |
| WebRTC feature | 10.4 m | lib units 2.33 m; harnesses 2.03 m; wire consumer graph 2.03 m; witness inventory 0.82 m |
| Python bindings tests | 8.5 m | maturin develop 2.93 m; pytest 4.87 m |
| Unit tests | 8.0 m | lib suite 3.32 m; doctests 1.40 m; deck 1.80 m; witness step 0.58 m |
| Documentation | 7.5 m | 11 rustdoc passes, 0.57–1.53 m each |

### 5.1 Named-witness re-run fan-out (removed 2026-09-13)

Counted before removal: 58 exact `cargo test` re-runs in `unit-tests`
(`ci.yml:757`), 27 + 22 + 5 nextest re-runs in `rust-sdk-tests`, ~64 in
`integration-cortex`, and 100 `cargo nextest list` calls in `webrtc-feature`
(12 floors + 87 pinned names + 1 probe). Their measured cost was **not**
uniform: 0.58 m in `unit-tests` and 0.82 m in `webrtc-feature`, but **7.3 m in
`rust-sdk-tests`**, which is the critical path.

### 5.2 Feature-graph fragmentation

28 distinct root-crate feature graphs across the workflow. Within the
integration families, 12 declared graphs collapsed to **6 effective** ones and
now to **4** (one per family), because no family step passes
`--no-default-features` — every "narrow" set was `default` plus a delta, and
`dataforts` already implies `redex-disk` (`Cargo.toml:205-207`). The only real
distinguishers were `port-mapping` and `batched-ingress`, both two-level gated
with default-off config fields (`mesh.rs:2890`, `mesh.rs:2842`).

Cost of a graph switch, measured locally: 87–98 s of root-crate rebuild plus a
relink of every pinned binary in that step.

### 5.3 sccache and rust-cache, from real logs

| job | sccache hit rate | rust-cache |
|---|---|---|
| Rust SDK tests | 95.06 % (1519/1817 requests, 79 Rust misses) | — |
| Unit tests | 55.64 % (227 hits, 181 Rust misses) | — |
| Clippy | not enabled | full match, 360 MB |
| FFI tests (go-rpc-ffi) | not enabled | full match, 210 MB |

### 5.4 Cache pool state (`actions/caches` API, two snapshots 1 h apart)

| | snapshot 1 | snapshot 2 |
|---|---|---|
| entries | 1198 | 970 |
| total | 11.51 GB | 10.50 GB |

**229 entries evicted in one hour with zero new entries created**: 225 sccache
objects and 4 rust-cache archives. Composition at snapshot 2: rust-cache 27
entries / 7.56 GB (mean 280 MB, max 594 MB); sccache 940 objects / 2.06 GB
(380 ever re-read); npm 0.87 GB.

Two structural facts:

1. **The default branch holds no Rust cache.** master carries 36 sccache
   objects and one npm archive; zero `v0-rust-*`. A new branch inherits
   nothing and cold-builds every Rust job on its first push.
2. **One manifest edit rotates all keys at once.** Keys are
   `v0-rust-<shared-key>-<os>-<arch>-<env-hash>-<manifest-hash>`; the manifest
   segment moved from `5edda219` (last green run) to `2f85fcf5`. `git log`
   shows 4 commits in 30 h touching a `Cargo.toml`/`Cargo.lock` under the
   globbed root. Each mints a fresh ~7.5 GB generation while the previous one
   lingers → 15 GB against a 10 GB quota → the eviction above.

Observed env-hash segments prove the sharing limit empirically: `6ff13d87`
(no sccache), `08d98f61` (sccache jobs), `2113753f` (Windows). Jobs in
different families cannot share a `shared-key` no matter what it says, because
the env hash is part of both the primary key and the restore prefix.

---

## 6. Defects found while measuring (both since FIXED)

**Narrow feature configurations did not compile.** `cargo check --lib
--no-default-features --features <set>`, as first measured:

| set | result |
|---|---|
| `<none>`, `net tool`, `cortex`, `netdb`, `meshdb`, `meshos` | built |
| `net`, `net nat-traversal`, `net batched-ingress`, `net regex`, `redex`, `redex redex-disk`, `dataforts` | **failed** |

One defect, four sites, all the same shape — an nRPC surface named from code
that is not itself nRPC-gated:

- `channel/config.rs` imported `mesh_rpc::ServeError` unconditionally, while
  `mesh_rpc` is `#[cfg(feature = "cortex")]` (`adapter/net/mod.rs`);
- `mesh.rs`'s `RpcInboundDispatcherMap` alias named
  `cortex::RpcInboundDispatcher` with no gate, though every initialisation of
  the fields it types was already cortex-gated;
- `DispatchCtx::rpc_local_services` was gated on `redex` while its type
  `LocalServiceRegistry` is gated on `cortex` (`cortex` implies `redex`, not
  the reverse) — that is the `redex` row;
- `dataforts::blob::transfer_rpc`, an nRPC service definition, was exposed
  from a feature that does not imply `cortex` — that is the `dataforts` row.

Fixed in `2a5ac182a`: the four sites carry the gate of the surface they name,
`capability_is_locally_private` answers `false` by construction without
`cortex` (a node with no nRPC serves nothing privately, and the sensing plane
still has to ask), and all thirteen configurations are now must-build rows in
`narrow-feature-check`. The ratchet step that asserted the seven kept failing
is gone with the defect.

**Four integration test targets did not compile on this branch** (as of
`caed88ac9`): `aggregator_fold_query`, `gang_alloc_witness`, `gang_claim_node`,
`sensing_scheduler_bridge` — `missing fields rtc_addr and rtc_bootstrap in
initializer of CapabilityMembership` (fields are unconditional at
`behavior/capability.rs:2425,2434`). `54befc493` had repaired the ~20
`#[cfg(test)]` constructors but not these four integration ones. Consequence
for the local loop: any broad `cargo nextest run` aborted before running
anything. Fixed in `a70b33f17`; the four targets run green (17 tests).

---

## 7. Changes landed from this audit

- `[profile.dev] debug = "line-tables-only"` (§2).
- `[profile.default.junit]` in `.config/nextest.toml` plus
  `.github/scripts/check-witness-results.py`: one verified result set replaces
  the per-name re-run fan-out (§5.1). The check is stricter than what it
  replaced — it rejects a `<flakyFailure>`, i.e. a witness that only passed on
  a retry, which a per-name re-run could not observe.
- One feature graph per integration family, plus the `narrow-feature-check`
  job that states the no-default-features proofs honestly (§5.2, §6).
- `cargo t` / `cargo tl` aliases in `.cargo/config.toml` to keep a local
  session on one fingerprint (§1).
- `ffi-tests` 9 cache keys → 3 and `ffi-clippy` 15 → 9, merging only rows with
  an identical env hash and an identical build, with one designated saver each
  (§5.4).

## 8. Open, not done

- Nothing from §6 — both defects are fixed (`a70b33f17`, `2a5ac182a`).
- Sleep conversion (§4): ~31–36 s of nominal sleep is convertible with zero
  witness risk; the rest needs config knobs, not smaller literals.
- CI wall clock (§5): the four ceilings above 10 min are each dominated by one
  serial phase and could be split across parallel jobs, since the workflow has
  no `needs:` edges. Not attempted — runner-minute cost and coverage
  equivalence need deciding first (a Windows job is billed 2×; an LTO wheel
  suite is not the same artifact as a debug-extension suite).
- Cache: the eviction described in §5.4 is driven by branch scoping × key
  count, not by key naming. A `save-if` discipline would fix the pool but make
  every branch push cold; that trade was not taken.
