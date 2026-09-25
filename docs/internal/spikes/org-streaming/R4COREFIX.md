# R4COREFIX — F-S4Vectors-1: org-scoped discovery vs OS-process boundaries

Lane: `R4CoreFix` (core fix lane, Stage 4 Wave 2 follow-up).
Defect under review: **F-S4Vectors-1 (HIGH)** — "org-scoped/private discovery does
not cross OS-process boundaries", filed by `S4Vectors` with the localization
inference scoped to three core sites (`mesh.rs:22095` SendEmission.scoped cache,
`org_scoped_store.rs:733` scoped-ingest `verify_refused`, `mesh.rs:22453`
consumer-grant query chain).

## 0. Verdict

**The premise does not hold: scoped discovery crosses OS-process boundaries
correctly.** A new two-OS-process witness over the scoped plane runs green at
core level, under both the test-harness config and the exact Python-binding node
config. The mixed pair's red is caused by **the provider harness discarding the
`ServeHandle` returned by `serve_org_streaming`**: `ServeHandle::drop` is the
registration's lifetime (`mesh_rpc.rs:468-489`: `unregister_rpc_inbound` +
`rpc_local_services.remove_if(service, registration_id)` + retire live protected
streams), so the granted service is RAII-deregistered **before the first
announce**, `granted_snapshot()` stays empty, `announce_attempt` takes its
`(owner_scoped.is_empty() && granted.is_empty())` early arm, and
`SendEmission.scoped` seals and ships **zero envelopes, forever**. The caller's
private plane is then honestly empty (`0 private candidate(s) considered`).

The observed "cross-process" correlation is a **confound**: every in-process
green harness binds the handle (`test_org_live.py`: `handle =
net.serve_org_streaming(...)` with `handle.close()` in `finally`; the Rust
witnesses hold `let _serve: ServeHandle = ...`), while both cross-process
harnesses (`mixed_pair/provider.py`, and therefore the Go row that spawns it)
discard it. The variable was never the process boundary — it was which script
runs.

**No core change is required or made** (`git diff --stat net/crates/net/src/`
empty at report time — the core is byte-identical to `LZL0/org-streaming` HEAD).
The full fix turned out to be THREE consumer-side lifetime corrections in the
harness stack, all owned and applied by `S4Vectors` (`a919a26f0` + their
provider.py handle fix): (1) the discarded `ServeHandle`, (2) the teardown
race (DRAINED handshake), (3) the spurious double-accept arm + the silent
`start()` refusal it tripped. The acceptance row is GREEN two-sided (§3.4) and
the two-OS-process witness is green with its inverse (§2.4).

## 1. Root-cause analysis — why the scoped chain "lost" candidates

### 1.1 The chain, as built

```
serve_rpc_granted_streaming ─┐
serve_rpc_granted (unary) ───┴─► rpc_local_services.insert(svc, id, GrantedAudience)
                                          │
announce_attempt ─► granted_snapshot() ◄──┘   (mesh.rs:42903-42907 early arm)
        │
        ├─► granted_envelopes() seals ScopedCapabilityAnnouncement(s)   [SITE 1 region]
        ▼
local_emission.scoped ─► send_emission_to ─► SUBPROTOCOL_SCOPED_CAPABILITY_ANN
        ▼
decide_scoped_relay ─► ingest_scoped_announcement ─► verify_scoped_ingest  [SITE 2 region]
        ▼
ScopedDiscoveryState (store + index)
        ▼
capture_cold / granted_providers_at ─► consumer.get(grant_id) + find_capabilities_for_grant
                                                                    [SITE 3 region]
        ▼
OrgClient plan (capture_private → derive_captured) → "N private candidate(s) considered"
```

### 1.2 The failing input to the chain

`mixed_pair/provider.py` calls

```python
net.serve_org_streaming(mesh, sc["service"], "granted", handler, None)   # handle DISCARDED
```

The pyo3 `serve_org_streaming` returns the registration's `ServeHandle`
(module docstring: "Teardown order: `org_client.close()` →
`serve_handle.close()` → `mesh.shutdown()`"). Discarding the return value drops
the handle **at end of statement**, and `impl Drop for ServeHandle`
(`mesh_rpc.rs:468-489`) then removes the dispatcher and the
`rpc_local_services` entry and retires the registration's protected streams —
documented RAII ("Service name to remove from `rpc_local_services` on Drop",
`mesh_rpc.rs:412`).

Consequences, in order:

1. `rpc_local_services.granted_snapshot()` is empty from the first announce on.
2. `announce_attempt` (mesh.rs:42903-42907) takes
   `if owner_scoped.is_empty() && granted.is_empty() { (…, Vec::new(), None, None) }`
   — the scoped build block is never entered, so `granted_envelopes` never runs
   and `local_emission.scoped` is `[]`.
3. `send_emission_to` ships `public=… scoped=0` on every send — which is
   exactly the instrumented receipt (§2.4).
4. The caller's store never receives a row; `capture_cold`'s granted query
   returns 0 rows under a *present* consumer pin (`grant lookup HIT
   (installed=1)`, `pinned=true`, 0 rows); `derive_captured` reports
   `considered = 0` → `org:discovery:no_authorized_provider … (0 private
   candidate(s) considered)`.

Sites 2 and 3 are never reached with anything to refuse or miss; site 1's
region behaves correctly for its input (an empty catalog projects an empty
scoped emission — the fail-safe direction).

### 1.3 Why it LOOKED like an OS-process boundary

| Surface | Handle | Plane | Result |
|---|---|---|---|
| `test_org_live.py` (in-process, Python↔Python) | bound (`handle = …`) | scoped | GREEN |
| `sdk/src/org/tests_live.rs` (in-process, Rust) | held (`let _serve`) | scoped | GREEN |
| `tests/org_scoped_cross_process.rs` (two OS processes, Rust) | held (`let _serve`) | scoped | **GREEN** |
| `mixed_pair/provider.py` (cross-process, Py↔Py and Go↔Py) | **discarded** | scoped | RED |
| S4Vectors' isolation (`caller.py ↔ provider.py`) | **discarded** | scoped | RED |

Every green holds the handle; every red discards it. The process boundary is
unrelated — the Rust two-process witness disproves it directly.

## 2. Receipts (verbatim where quoted)

### 2.1 Pre-fix RED — the S4Vectors evidence (cited, not re-derived)

`S4_VECTORS.md` §3.3 (`logs/go_mixed_pair.log`):

```
provider: handshake done, starting mesh + announce loop
provider: serving, announcing
    org_streaming_opening_vectors_test.go:646: discovery did not converge within 1m0s (last: org:discovery:no_authorized_provider: org discovery: no authorized provider for capability 96dd6b947a2ed4c4129fc663b526a6ffd9495521ec57c76c617b6cc76b28c209 (0 private candidate(s) considered))
--- FAIL: TestStreamingOpeningVectors_MixedPair_GoCallerPythonProvider (60.93s)
```

and the Python↔Python single-variable isolation
(`logs/python_cross_process_isolation.log`): `CALLER FAIL the call never
converged: OrgDiscoveryError('org:discovery:no_authorized_provider: … (0 private
candidate(s) considered)')` with `probe: discovered_nodes=1` (public plane
healthy).

### 2.2 Pre-fix RED — my acceptance-row re-run at this head (fresh consumer artifacts)

Commands (the contract's prerequisites, then the row verbatim):

```
cargo build --release -p net-ffi                                   # a5be938384f1734f2bbda170ca91a7bb361edea140f18837acf977976f4c7ad5  net.dll
python -m maturin build --no-default-features --features net,cortex,compute,groups,meshdb,meshos,deck,aggregator,tool,consent,mcp,delegation,publish,a2a,payments,payments-http,org,dataforts,extension-module
                                                                   # c8f8a062c99acdb1e263d73e000cc0e43da83f5677bf2e778ae0ff4e130750da  wheel
python -m pip install --force-reinstall --no-deps <wheel>
cd go && RUN_INTEGRATION_TESTS=1 RUN_MIXED_CROSS_PROCESS=1 go test -run 'TestStreamingOpeningVectors_MixedPair_GoCallerPythonProvider' -v -timeout 15m
```

Output (`r4corefix` row re-run):

```
=== RUN   TestStreamingOpeningVectors_MixedPair_GoCallerPythonProvider
provider: handshake done, starting mesh + announce loop
provider: serving, announcing
    org_streaming_opening_vectors_test.go:656: discovery did not converge within 1m0s (last: org:discovery:no_authorized_provider: org discovery: no authorized provider for capability 96dd6b947a2ed4c4129fc663b526a6ffd9495521ec57c76c617b6cc76b28c209 (0 private candidate(s) considered))
--- FAIL: TestStreamingOpeningVectors_MixedPair_GoCallerPythonProvider (66.65s)
FAIL
exit status 1
FAIL	github.com/ai-2070/net/go	67.028s
```

Identical signature to §2.1 — the defect reproduces at this head.

### 2.3 The three named sites — probe receipts (instrumented wheel)

Ten temporary probes (`[xproc-diag]`, since fully reverted — `git diff` on
`net/crates/net/src/` is empty) were placed at the three named sites plus the
emission build, the wire send, the relay decision, and the ingest dispositions.
The Python↔Python pair (`mixed_pair/caller.py --manifest <fresh %TEMP% scenario>
--vectors …`) under that wheel produced
`docs/internal/spikes/org-streaming/r4corefix-diag-pythonpair.log` (verbatim
extract; the raw dump was removed in `093be643a` (DOCS-5), so this extract is
the record — the dump itself: `git show 239dea825:docs/internal/spikes/org-streaming/r4corefix-diag-pythonpair.log`):

```
[xproc-diag] send_emission_to node=0x2cb5fa683e11dfe1 public=732 scoped=0
[xproc-diag] send_emission_to node=0x47785a06eb370a63 public=732 scoped=0
[xproc-diag] capture_cold: grant lookup HIT (installed=1)
[xproc-diag] capture_cold: granted rows for grant 437a5c57 pinned=true
…
[xproc-diag] send_emission_to node=0x2cb5fa683e11dfe1 public=733 scoped=0
CALLER FAIL the call never converged: OrgDiscoveryError('org:discovery:no_authorized_provider: … (0 private candidate(s) considered)')
```

Reading of the receipt:

| Site | Probe | Observation | Verdict |
|---|---|---|---|
| Site 1 — `SendEmission.scoped` cache (`mesh.rs:22095` region) | `send_emission_to … scoped=0` on every send; `emission built` / `granted_envelopes` probes **never fired** | the scoped build block was never entered: `granted_snapshot()` empty ⇒ early arm `scoped: Vec::new()` | correct behavior for its input; the **input** (the emission catalog) was empty because the registration was gone |
| Site 2 — scoped ingest `verify_refused` (`org_scoped_store.rs:733` region) | no `scoped frames received`, no ingest dispositions | never reached — no envelope ever shipped | innocent |
| Site 3 — consumer-grant query chain (`mesh.rs:22453` region) | `capture_cold: grant lookup HIT (installed=1)`; `granted rows … pinned=true` with 0 rows | the consumer credential was installed and the pin matched; the STORE was empty | innocent |

### 2.4 Acceptance witness #2 — `scoped_discovery_crosses_an_os_process_boundary`

File: `net/crates/net/tests/org_scoped_cross_process.rs` (NEW).
Names: `scoped_discovery_crosses_an_os_process_boundary` (the witness) and
`xproc_scoped_provider_child` (`#[ignore]` — the spawned provider role, run only
via `current_exe --ignored --exact`, the `tests/natsim.rs` multi-process idiom).

Shape: the test binary re-executes itself as a second OS process. The child
plays the org-B provider (adopted authority + provider grant audience +
`serve_rpc_granted`, **handle held**); the parent plays the org-A caller
(adopted authority + consumer grant audience) and drives the real protected
call. Asserted fail-closed, both sides:

1. the child's emission carries the granted envelope (`RESULT ok calls=1
   emitted=<n>` with `emitted >= 1` — the `SendEmission.scoped` cache site);
2. the parent's private plane CONSIDERS the child's provider entity
   (`granted_capability_providers` returns exactly the one child entity — the
   ingest + consumer-lookup sites), with the intake counters
   (`org_scoped_ingest_counts`) printed on any red;
3. the protected call is admitted and the child's handler observes the exact
   five-field `Admitted` attribution (call/serve attributed, verified on BOTH
   sides).

GREEN at this head (no core change):

```
running 2 tests
test xproc_scoped_provider_child ... ignored
test scoped_discovery_crosses_an_os_process_boundary ... ok
test result: ok. 1 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.46s
```

This run also re-ran under the **Python-binding node config** (default
`min_announce_interval` 10 s, default socket buffers, `heartbeat_interval_ms=200`
— the exact `mixed_pair` `_mesh` construction): green, 0.46 s. The witness
therefore pins the real property at the config where the defect was reported.

**Inverse (mutate the ASSERTED property → the named red → restore → green):**
the asserted property is "the child's granted candidate is considered on the
parent across the OS-process boundary". The mutation reproduces the real defect
shape inside the child — **discard the `ServeHandle`** (`let _dropped: () =
match provider.serve_rpc_granted(...) { Ok(_h) => (), … }`), exactly what the
pre-fix `provider.py` did — so `ServeHandle::Drop` deregisters the granted
service before the first announce and the asserted property cannot hold. With
the mutation applied, the NAMED assertion goes red:

```
test scoped_discovery_crosses_an_os_process_boundary ... FAILED
panicked at …: F-S4Vectors-1: scoped discovery did not cross the OS-process boundary
```

(`r4corefix-inverse-red.log`, removed in `093be643a` — the excerpt above is the
record, raw: `git show 239dea825:docs/internal/spikes/org-streaming/r4corefix-inverse-red.log`; the child's own side fails closed
too — `RESULT fail callback-loss emitted=…`). Restoring the handle hold returns
the witness to green (`r4corefix-witness-green.log`, removed in `093be643a` —
the GREEN block above is the record, raw: `git show 239dea825:docs/internal/spikes/org-streaming/r4corefix-witness-green.log`). The same mutation in the real harness is
the whole defect history: §2.1 (S4Vectors' runs) and §2.2 (my verbatim row
re-run at this head with fresh artifacts — the fix-reverted state) are the named
row red with the handle discarded, and §2.3 is the mechanism-level red
(`scoped=0` on every emission). Post-fix GREEN: §3.

### 2.5 Change ledger

| # | Change | File | Receipt |
|---|---|---|---|
| 1 | Fix: bind + hold the serve handle through the provider's announce loop | `net/crates/net/tests/cross_lang_org/mixed_pair/provider.py` (S4Vectors-owned; applied by them on my root-cause request — see §5 coordination) | §3 (row GREEN), §3 inverse |
| 2 | Acceptance witness #2 (new) | `net/crates/net/tests/org_scoped_cross_process.rs` | §2.4 |
| 3 | This report + diagnostic receipt | `docs/internal/spikes/org-streaming/R4COREFIX.md`, `r4corefix-diag-pythonpair.log` (removed in `093be643a`; the §2.3 extract is the record) | F16 hashes in §8 (recomputed from `239dea825`, the last tree holding the logs) |
| 4 | Core (`mesh.rs`, `behavior/org_scoped_store.rs`) | **no change** — ten temporary probes added and fully reverted | `git diff --stat net/crates/net/src/` empty |

## 3. Acceptance receipts (post-fix)

### 3.1 Row re-run, round 1 — the handle fix closes F-S4Vectors-1

With S4Vectors' `handle = net.serve_org_streaming(...)` fix (provider.py 8656 B,
sha `e42017e2…`) and clean artifacts (wheel `net_mesh-0.36.0-cp310-cp310-win_amd64.whl`
installed; `go/net.dll` `003d2de7909b444c8c3faa0744c06ee8fbcc011e94a1bdc3043d55cb9ea482a5`
re-staged from `net/crates/net/target/release/net.dll` — same clean core source),
the verbatim command:

```
cd go && RUN_INTEGRATION_TESTS=1 RUN_MIXED_CROSS_PROCESS=1 go test -run 'TestStreamingOpeningVectors_MixedPair_GoCallerPythonProvider' -v -timeout 15m
```

produced (`r4corefix-row-green.log`, round 1 — verbatim extract; the log was
removed in `093be643a`, so this extract is the record — raw:
`git show 239dea825:docs/internal/spikes/org-streaming/r4corefix-row-green.log`):

```
=== RUN   TestStreamingOpeningVectors_MixedPair_GoCallerPythonProvider
provider: handshake done, starting mesh + announce loop
provider: serving, announcing
    org_streaming_opening_vectors_test.go:679: recv: org:rpc:server_error: rpc: server returned status 0x0005: server observed CANCEL during streaming handler execution
--- FAIL: TestStreamingOpeningVectors_MixedPair_GoCallerPythonProvider (31.51s)
FAIL
```

**F-S4Vectors-1 is closed by this round**: the `no_authorized_provider … (0
private candidate(s) considered)` signature is GONE — discovery converges and
the protected call plans, opens and reaches the handler (31.5 s < the 60 s
convergence budget; the failure is now at `recv:`, the streaming drain — past
discovery, past admission).

### 3.2 Round 1 exposed a second, latent defect — teardown race (finding 6)

`0x0005: server observed CANCEL during streaming handler execution` — the new
`finally: handle.close()` retires the registration's live protected streams
(`ServeHandle::drop`, mesh_rpc.rs:487-489, Q3/C9 §2.2: "cancellation signalled,
semaphores closed, pump aborted + joined, one terminal" — and that terminal IS
the 0x0005) while the handler's three chunk publishes and the automatic eof
terminal are still draining toward the caller. Same bug CLASS as the first
defect: registration lifetime vs call lifetime, masked in-process because
`test_org_live.py`'s single script sequence consumes `list(stream)` before its
`finally: handle.close()`. S4Vectors fixed it with the DRAINED stdin handshake
(option a): `DRAINED\n` after the caller's recv loop reaches clean eof;
provider.py blocks on that line (120 s watchdog, fail-closed) before printing
`RESULT ok` and closing the handle — provider.py sha `22ce91e0…`, caller.py
`2c0963ba…`, go row `1840f4b1…`.

### 3.3 Round 2 exposed the real Python-pair killer — the silent start refusal
(finding 8)

Round 2's Go row reached the streaming exchange (`0x0006: response pump failed`
at recv — see finding 9) but the Python↔Python pair stayed red at discovery with
the ORIGINAL signature. The instrumented probe wheel (fresh scenario) produced
the receipt-grade aggregate that closes the case
(`r4corefix-diag-round3.log`, removed in `093be643a` — the aggregate below is
the record, raw: `git show 239dea825:docs/internal/spikes/org-streaming/r4corefix-diag-round3.log`):

```
8  send_emission_to node=<caller-id>  scoped=1     (provider shipping envelopes)
7  send_emission_to node=<provider-id> scoped=0    (caller shipping public-only)
7  recv-head subprotocol=0xc00 from=<caller-id>   (the PROVIDER receives all 7)
0  recv-head … (the CALLER receives NOTHING — no id at all)
0  frames received / SCOPED SEND FAILED           (no send errors anywhere)
59 capture_cold rows pinned=true installed=1      (59 plans, 0 rows, every time)
```

One-way traffic: the caller's receive loop never processes a single packet. The
mechanism (`mesh.rs` `start_inner`, ~24335): caller.py arms a spurious
`mesh.accept(provider_node)` thread (the double-accept arm) while also
connecting out; the provider never initiates, so the accept never completes;
`caller.py`'s `t.join(timeout=10)` TIMES OUT SILENTLY (it lacks provider.py's
`t.is_alive()` fail-closed check) and leaves `accept_in_flight > 0` — and
`start_inner` then rolls back and REFUSES to spawn the receive/dispatch loop
("MeshNode::start() called while an accept() is in flight"), surfacing the
refusal only as `tracing::warn!` while the binding's `NetMesh.start()` returns
`Ok(())`. The caller consequently processes zero inbound packets: the scoped
envelope never ingests, `capture_cold`'s granted rows stay 0 under an
installed, pinned consumer grant — exactly the observed
`0 private candidate(s) considered` — and `discovered_nodes=1` was the caller's
SELF-index all along. Cross-checks: the Go row (connect-only) converges
discovery; the Rust witness (connect-only) is green; `test_org_live.py` arms
accept only on the provider. S4Vectors fixed it in `a919a26f0` (caller.py
sha `0dd156c0…`, connect-only — the arm removed, the mechanism quoted in the
code comment where it used to be).

### 3.4 ROUND 3 — the acceptance row GREEN, two-sided PASS

With all three harness fixes in (S4Vectors: `a919a26f0` caller.py `0dd156c0…`
connect-only; provider.py `22ce91e0…` handle held + DRAINED teardown; go row
`1840f4b1…` DRAINED writer) and clean artifacts rebuilt inside the cycle
(probe-free core, `git diff --stat net/crates/net/src/` empty; fresh wheel
installed), the verbatim command from the acceptance contract:

```
cd go && RUN_INTEGRATION_TESTS=1 RUN_MIXED_CROSS_PROCESS=1 go test -run 'TestStreamingOpeningVectors_MixedPair_GoCallerPythonProvider' -v -timeout 15m
```

produced (`r4corefix-row-round3-green.log`, verbatim; removed in `093be643a`,
so this block is the record — raw: `git show 239dea825:docs/internal/spikes/org-streaming/r4corefix-row-round3-green.log`):

```
=== RUN   TestStreamingOpeningVectors_MixedPair_GoCallerPythonProvider
provider: handshake done, starting mesh + announce loop
provider: serving, announcing
provider: awaiting DRAINED
--- PASS: TestStreamingOpeningVectors_MixedPair_GoCallerPythonProvider (19.35s)
PASS
ok  	github.com/ai-2070/net/go	19.760s
```

**TWO-SIDED PASS SIGNATURE:**
- the Go line `--- PASS: TestStreamingOpeningVectors_MixedPair_GoCallerPythonProvider`
  ✓ (above);
- the provider's `RESULT ok calls=1 chunks=3` — consumed and EXACTLY matched by
  the row itself at `org_streaming_opening_vectors_test.go:711-714`
  (`result != "RESULT ok calls=1 chunks=…"` is a `t.Fatalf`), and the row's
  `cmd.Wait()` returned clean — so the provider line held verbatim; it travels
  on the pipe to the orchestrator and does not echo to the parent terminal.

The row's own asserts also held along the way: the chunk bytes ==
`scenarios.mixed_pair.chunks_hex` byte-for-byte, `expect_terminal == eof`, and
provider.py's handler facts == `expect_handler` (it exits non-zero on any
mismatch or handler-never-fires). 19.35 s — discovery, admission, three
chunks, clean eof, and the DRAINED teardown all inside one budget.

**Acceptance witness #1: GREEN. Acceptance witness #2: GREEN (§2.4). The
retraction of F-S4Vectors-1 stands on the executed record: the scoped chain
never had a cross-process defect; three consumer-side lifetime bugs in the
harness stack (discarded serve handle → teardown race → double-accept +
silent start) each masqueraded as one, and each is now fixed at its owner with
receipts above.**

## 4. Findings

1. **ServeHandle is the registration's lifetime, and discarding it is silent.**
   `ServeHandle::drop` (mesh_rpc.rs:468-489) removes the service from
   `rpc_local_services` and retires its streams. A caller that drops the handle
   gets a service that *deregisters itself before its first announce* — with no
   error anywhere. The Python module docstring documents the teardown order, but
   nothing enforces or warns on immediate drop. This is the class behind
   F-S4Vectors-1; the one-line fix closes this instance.
2. **The audience-secret loader is a security gate with a loud failure — and a
   Windows parent-DACL trap.** A diagnostic repro under `C:\tmp` failed closed
   at `OrgCredentials` with
   `audience secret file … has a permissive ACL (grants access to untrusted
   principal S-1-5-32-545 …); refusing to treat a possibly-disclosed audience key
   as installed` (`BUILTIN\Users` inherited from the parent directory). S4Vectors'
   outdirs under `%TEMP%` pass (their runs reached the convergence loop, so their
   secrets loaded — their red was never the ACL path). Combined with
   `test_org_live.py`'s standing warning about `tempfile.mkdtemp`'s "Owner
   Rights" ACE, the operational rule is: scenario outdirs go under the user
   temp via ordinary directory creation — never `mkdtemp`-style protected DACLs,
   never a directory with group/user ACEs. Receipt:
   `r4corefix-diag-pythonpair.log` run 1 (the `C:\tmp` refusal; log removed in
   `093be643a`, raw: `git show 239dea825:docs/internal/spikes/org-streaming/r4corefix-diag-pythonpair.log`).
3. **S4Vectors' localization inference named the right chain but the wrong
   layer.** All three named sites are probe-innocent (§2.3); the drop is the
   catalog input to site 1's region. The lane's own caveat held: "A same-code
   divergence observable only across process boundaries is [inference]".
4. **Caller probe calls in `mixed_pair/caller.py` are mistyped** (their file):
   `mesh.find_nodes(tag)` needs a filter dict (`TypeError: 'str' object is not
   an instance of 'dict'`) and `find_nodes_scoped` needs `(filter, scope)`. The
   probes print errors instead of localizing state. Informational only — worth a
   cleanup in their lane.
5. **`xproc-diag` instrumentation pattern:** ten `eprintln!` probes across the
   three named sites localized the drop in ONE instrumented run (~1 min),
   versus further inference. Fully reverted; the receipt log preserves them.
6. **Second latent defect (round 1 exposure): serve-handle teardown races the
   caller's drain.** `ServeHandle::drop`/close retires live protected streams
   with the 0x0005 CANCEL terminal by design (§2.2); a provider that closes the
   registration before the caller's stream drains converts chunk×3 + eof into a
   single 0x0005 at the caller's first recv. The class is "registration
   lifetime vs call lifetime" — the same class as the discarded-handle defect,
   which is why the in-process harnesses (one sequence owns both sides:
   consume-then-close) never see either. Cross-process harnesses must sequence
   teardown AFTER the caller's drain; S4Vectors owns that shape (see §3.2).
7. **Diagnostic trap: an aged scenario dir reproduces the ORIGINAL signature.**
   `gen_org_scenario`'s chain has `SCENARIO_TTL_SECS = 3600`; once the dir is
   older than its TTL, `grant_active_for_emission` skips the born-expired grant
   at every emission, so the provider seals nothing and the caller reports the
   exact `no_authorized_provider … (0 private candidate(s) considered)` —
   indistinguishable from the discarded-handle defect at the terminal. A
   re-run against a >1h-old scenario hit exactly this (70.7 s red) and was
   cleared by regenerating. Operational rule for every future co-run: mint the
   scenario FRESH inside the run (the Go row already does; ad-hoc repros must
   too).
8. **`MeshNode::start()` can return success while starting NOTHING** (Main's
   ruling ledger: finding 7). When an `accept()` is in flight, `start_inner`
   rolls the `started` flag back and refuses to spawn the receive/dispatch
   loop — surfacing the refusal only as `tracing::warn!`, while the bindings'
   `NetMesh.start()` returns `Ok(())`. A node in that state processes zero
   inbound packets while its sends work perfectly — one-way invisibility that
   hid defect #3 for the whole saga and required probe wheels to find; it is
   the fail-open API surface behind all three harness defects misreading as
   core. **Ruled FILED, NOT fixed in this stage** (Main, round-2 ruling): the
   zero-core-change freeze stands and a public-surface change is
   compatibility-ledger territory (C1–C12: additive alternatives + approval
   status) — an OWNER decision, as with the F-S3.2-1 carve. The two
   alternatives, for the owner:

   - **Breaking:** `MeshNode::start()` returns `Result<(), …>` and the bindings
     raise on the refusal (Python `NetMesh.start()` → `PyResult<()>` already
     has the error channel). Cleanest semantics — "start" must never silently
     no-op — but it changes the public signature in every language binding and
     the compatibility ledger must carry it.
   - **Additive (recommended for this cycle):** keep `start()`'s signature and
     idempotent semantics; add `try_start()` / `start_with_report()` beside it
     returning the refusal, and document the silent-refusal caveat on `start()`
     as a known limitation until the next major. Zero breakage; the harnesses
     that need liveness guarantees adopt the additive door immediately.

   The documentation-level statement (start() preconditions + the silent-refusal
   caveat) is Main's release-notes task and lands at stage close; the owner
   recommendation rides the release acceptance verdict. NOT touched here.
9. **Third latent defect (round 2 exposure): streaming pump completion race.**
   With the DRAINED handshake in place, the row reached the streaming exchange
   and failed at the first recv with `0x0006: response pump failed`
   (`StreamCallOutput::Open → PumpFailed`, cortex/rpc.rs:3231/3616): the `def`
   handler's completion (`handler_returned`) is raced by the pump task's exit
   (the biased select's `pump_done` break shadows a ready handler arm once the
   sink's mpsc sender drops at blocking-task end), and `RpcResponseSink::send`
   silently drops chunks on a finished gate or a closed receiver (rpc.rs:2274).
   Visible only in the cross-process row so far; the in-process cells order
   consume-then-close on one sequence and never race it. Referred to S4Vectors'
   coordination queue alongside the `0x0006` row evidence.

## 5. Coordination record

- `Main`: acceptance contract received verbatim (row command, prerequisites,
  pass signature `--- PASS: TestStreamingOpeningVectors_MixedPair_` +
  `RESULT ok calls=1 chunks=3`); status + narrowing reported mid-lane.
- `S4Vectors`: shared-artifact HOLD requested and honored (they paused runs
  while I rebuilt wheel/dll); root cause + the exact one-line fix delivered for
  their file; they applied it (provider.py 8656 B, sha
  `e42017e2bce226ed5fa22b4e92db7ba3262cd3949d2ba677287dff73f0409698`) and
  retracted F-S4Vectors-1 unprompted in their correction commit `47a9e345f`
  ("S4Vectors: correct F-S4Vectors-1 …" — go-row comment + `S4_VECTORS.md`
  §3.3/§4 retraction; gofmt+vet clean), crediting the instrumented-wheel trace
  for separating their two red scripts from the boundary theory. Their lane
  commits: `5da931259`, `8adc9fc86`, `baa79cf1f`, `47a9e345f`. The co-run
  handoff: after my row PASS they execute the verbatim row once for their own
  §3.3 green receipt. Their message supplied the C-ABI
  `ChannelConfigRegistry` note (exonerated as THE mechanism by their own
  permissive-both isolation, Go-row background only).
- `Main` (round-2 ruling): the row green awaits S4Vectors' caller.py fix
  (remove the double-accept arm or fail-closed `is_alive` on the join); my
  three-defect diagnostic chain (handle discard → teardown race → double-accept
  + silent start) confirmed receipt-grade complete; the `start()` fail-open
  surface FILED as their finding 7 = my finding 8 with the two alternatives
  laid out for the OWNER (breaking vs additive; compatibility-ledger C1–C12
  territory) — NOT fixed this stage under the zero-core-change freeze; the
  documentation statement is Main's release-notes task; the recommendation
  rides the release acceptance verdict. "Your discipline (no core touch without
  my word) was correct and is credited."
- Binding/SDK trees, `ci.yml`, `sdk/examples/**`, `tests/cross_lang_org/**`
  (beyond the coordinated provider.py/caller.py fixes): untouched by me.

## 6. Never executed here

- The reverse mixed pair (Python caller ↔ Go provider) and the same-org access
  mode over two processes — other lanes' rows.
- Browser/wasm runtimes (S4Browser's tree).
- Pure SDK consumers (`sdk-ts`, `sdk-py`) — other Wave-2 lanes.
- CI pinning of the new witness (Main pins `ci.yml`; counts+names in §7).
- Windows-ACL matrix beyond the one observed refusal (finding 2) — no formal
  ACL test added (out of scope for this defect).
- The full `org_routing` floor/roster suite re-run is Main's project-wide pass;
  my scoped proof is the witness + the row (per the validation division).

## 7. Counts + names for pins (for Main)

New file `net/crates/net/tests/org_scoped_cross_process.rs` (feature-gated
`#![cfg(all(feature = "net", feature = "cortex"))]`, like
`integration_nrpc_protected.rs`):

- `scoped_discovery_crosses_an_os_process_boundary` — 1 named witness, green.
- `xproc_scoped_provider_child` — 1 name, `#[ignore]` skip-gated provider role
  (runs only as the spawned child; same idiom as S4Vectors' `RUN_MIXED_CROSS_PROCESS`
  gate).

Suggested floor: `org_scoped_cross_process` MIN=1 passing + 1 ignored (never 0
passing). No existing floor/roster moves; the plan's `org_routing_wiring_tests`
MIN=93, `behavior::org_routing::` MIN=24, REG_MIN=62/STATE_MIN=41,
GATE_MIN=60/MESH_MIN=67 are untouched by this lane.

## 8. F16 — size + sha256 after writes

Final ledger at commit time (this file's own hash is recorded in the commit
message — a document cannot contain its own digest):

- `net/crates/net/tests/org_scoped_cross_process.rs` (NEW witness)
- `docs/internal/spikes/org-streaming/r4corefix-diag-pythonpair.log` (§2.3 receipt)
- `docs/internal/spikes/org-streaming/r4corefix-inverse-red.log` (§2.4 inverse)
- `docs/internal/spikes/org-streaming/r4corefix-witness-green.log` (§2.4 green)
- `docs/internal/spikes/org-streaming/r4corefix-row-green.log` (§3.1 round 1)

[DOCS-5 note, added after the fact] Every `r4corefix-*.log` named in this
report was deleted in `093be643a` (the raw probe dumps; the load-bearing
excerpts are embedded verbatim in §2.2–§2.4 and §3.1–§3.4, which are now the
record). The last tree that holds them is `239dea825`; each is retrievable with
`git show 239dea825:docs/internal/spikes/org-streaming/<name>`. Size (bytes) +
sha256 of each log as committed, recomputed from that tree (the round-3
diagnostic matches the digest recorded at `86d2212b9` below):

| Log | Bytes | sha256 |
|---|---|---|
| `r4corefix-diag-pythonpair.log` | 9449 | `ea34fce2e8de86877b15d6bf67fceba2dd22e46cb950ce2a08cc943059fcf3be` |
| `r4corefix-inverse-red.log` | 2932 | `9a9bbf4338f74048d375961cf558024e62378b198b0fbce2a3f7cf1baed0a71d` |
| `r4corefix-witness-green.log` | 619 | `e481fefcd4272cf862d6ab0626bd85555a49ae59d106d600489d6142a1708f2b` |
| `r4corefix-row-green.log` | 464 | `bf9a76e3cdfd6e33030294099d9797f7f89d686c6e7f8ddbd725ea54c4b6d4ac` |
| `r4corefix-diag-round3.log` | 3127 | `c3d08d0212f3861845b27d9cd8e8bf4e151ee58c8e3e528c8263d04caf260fe7` |
| `r4corefix-row-round3-green.log` | 308 | `47eb8d13845f0d19a39d7e347d8dc9b05d03a42e96cd4f7279d284385f24ded4` |

Consumer artifacts referenced by the receipts: pre-fix `net.dll`
`a5be938384f1734f2bbda170ca91a7bb361edea140f18837acf977976f4c7ad5` and wheel
`c8f8a062c99acdb1e263d73e000cc0e43da83f5677bf2e778ae0ff4e130750da`; post-fix
`go/net.dll` `003d2de7909b444c8c3faa0744c06ee8fbcc011e94a1bdc3043d55cb9ea482a5`
(same clean core source — `git diff --stat net/crates/net/src/` empty).

Follow-up commit `86d2212b9` (finding 8 + round-3 receipts) carried at ITS
commit time: `R4COREFIX.md` 29604 `92f1b7fdc9f0a0ad05ce0e713f7f82de6000a31817e447f2d06f6b9f843bedd1`,
`r4corefix-diag-round3.log` 3127
`c3d08d0212f3861845b27d9cd8e8bf4e151ee58c8e3e528c8263d04caf260fe7`.
