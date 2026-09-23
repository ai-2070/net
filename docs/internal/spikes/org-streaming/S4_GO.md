# Stage 4 — Go/C row (`S4Go`)

## 8.2 S4Go — Go/C: the C ABI streaming verbs + the Go binding

**Lane:** S4Go. **Pinned brief:** `spikes/org-streaming/S4_BRIEF.md` @`0f2f2d69c`.
**Base:** `0f2f2d69c` (Stage 3 accepted at `2225de011`; S3R runs in parallel and
owns `sdk/src/org/**` — never touched here). This file is the per-lane record
Main's file-state ruling asks for; `## 8. Stage 4` in `S1_REPORT.md` is Main's
index over the four lane files and was NOT edited by this lane.

### What landed (source-established)

The §4.4 Go/C verb set, verbatim names and existing handle types, over the
frozen Stage-3 seams (`call_streaming_bytes_deadline` /
`call_client_stream_bytes_deadline` / `call_duplex_bytes_deadline`,
`serve_org_{streaming,client_stream,duplex}_bytes_node`) — called, never
edited (core + `sdk/src/org/**` untouched):

1. **The shared handle module** (`net/crates/net/bindings/go/rpc-ffi/src/handles.rs`)
   — §4.4 "types move to a module both `rpc-ffi` and `org-ffi` can name":
   `RpcStreamHandleC` / `ClientStreamCallHandleC` / `DuplexCallHandleC`
   (+ `DuplexSinkHandleC` / `DuplexStreamHandleC` /
   `RpcRequestStreamHandleC` / `RpcResponseSinkHandleC`) live in ONE module of
   the `net-rpc-ffi` rlib, re-exported and named by `net-org-ffi` — one
   `libnet`, one handle vocabulary (the single-cdylib rule), no second stream
   wrapper per binding. The exported `net_rpc_*` operations drive and free
   handles an org verb minted.
2. **Per-handle error-wire style** (`ErrWireFn` on every caller-side handle).
   `rpc-ffi` constructs with its nRPC `<kind>:` formatter; `org-ffi` with
   `OrgSdkError::to_wire` — the single source of the `org:` vocabulary. An org
   stream's midstream `out_err` is therefore the same wire
   `net_org_call`'s failures speak (`org:rpc:<nrpc-kind>: <detail>`), and Go's
   `parseOrgError` classifies both identically. Without this, midstream errors
   on shared handles would parse as `unknown` and destroy the rpc-domain
   distinction `net_org.h` documents.
3. **`net_org_call_streaming` / `net_org_call_client_stream` /
   `net_org_call_duplex`** — return the existing handle types;
   `deadline_ms == 0` is the facade's 300 s default (never "no deadline");
   `cancel_token == 0` uncancellable; opening failures carry the
   `org:<domain>:<kind>` wire and the domain codes.
4. **`net_org_set_{streaming,client_streaming,duplex}_handler_dispatcher`**
   — fn types = `rpc-ffi`'s handler fn types + a leading
   `*const NetOrgCaller`, over the shared per-call handles. Fails closed
   without a registered callback deallocator (callback-buffer ownership),
   like the unary dispatcher.
5. **`net_org_serve_{streaming,client_stream,duplex}`** — the unary
   `net_org_serve` contract (arc consumed, `NET_ORG_ACCESS_*`, pre-reserved
   `handler_id`, `NetOrgServeHandle` out) with the shape dispatchers.
6. **`NET_ORG_ABI_VERSION` 0x0001 → 0x0002** (additive), with
   `exports.baseline --update`, `include/net_org.h`, the `go/org.go` cgo
   preamble, and the header↔Rust parity surfaces landing in ONE commit —
   the one-commit rule.
7. **Go:** `OrgClient.CallStreaming/CallClientStream/CallDuplex` returning the
   existing `*RpcStream` / `*ClientStreamCall` / `*DuplexCall` (the shared C
   handles wrapped through `new*FromOrg` constructors in `mesh_rpc.go`, so
   `streamHandleGuard` + the ctx-cancel watcher + `Close`/finalizer semantics
   are literally the public surface's — `rpc` pin nil: the `OrgClient` is the
   owning surface); `ServeOrgStreamingBytes/…ClientStreamBytes/…DuplexBytes`
   and the typed generics `ServeOrgStreaming/…ClientStream/…Duplex[Req,Resp]`
   beside `ServeOrg` (JSON over the EXISTING `TypedRequestStream` /
   `TypedResponseSink` — no new wrapper); midstream errors through
   `parseOrgError` (`RpcStream.orgErrors` + `parseStreamError`, inherited by
   `Split`'s halves); the three trampolines carry the FFI-01
   `recoverCallback` guard as their first statement (the estate's
   `callback_recover_test.go` roster-guard demands it and refuses vacuous
   passes).
8. **Header hygiene for single-TU consumers:** `net_org.h`'s
   `net_compute_mesh_arc_t` forward-typedef now names `net.go.h`'s struct tag
   (`net_compute_mesh_arc_s`) — the two headers' identical typedefs are
   C11-compatible, which the C skill example (all three headers in one TU)
   exercises at compile time.

**F-S3.1-2 (the named Stage-4 item), the handler-drop level stated
explicitly** at every layer of this binding's handler surface: the
`include/net_org.h` dispatch section, the `org-ffi` dispatch section and
serve-verb docs, the `go/org.go` serve section and the three handler type
docs, and the live-test section — "the retire supervisor may drop the
handler future WITHOUT a final poll; cancellation is observed ONLY through
the retirement observables (`net_rpc_request_stream_next` /
`net_rpc_response_sink_send` returning `NET_RPC_ERR_STREAM_DONE`; the Go
wrappers' `Recv`/`Send` returning `ErrStreamDone`), never assumed as a
handler-side event or a final-polled drop; handlers MUST exit cooperatively
there."

### Landed commits (executed)

| commit | content |
|---|---|
| `14d5b87f3` | `S4Go: the §4.4 Go/C verbs — shared streaming handles, net_org ABI 0x0002, Go binding + live cells` — 11 files, +2996/−396. The one-commit ABI set (stamp + `exports.baseline --update` + `include/net_org.h` + the `go/org.go` preamble + the parity surfaces) plus the carve's post-sweep example deltas, all named in the message. |
| (this record) | `S4Go: Stage 4 Go/C report record` — the pair's report commit. |

### Findings (state, not decide)

**F-S4Go-1 — Go stream handlers that drain aborted the WHOLE PROCESS
(executed; fixed at the mechanism).** The first CS live run died with
`FATAL: rpc FFI called from inside a tokio runtime context; aborting to
avoid runtime-in-runtime panic across the FFI boundary` — the deliberate
fail-loud in `rpc-ffi`'s `block_on` guard. Path: a served handler's Go
callback runs the user handler, whose documented pattern is to drain
(`RequestStreamRecv.Recv` → `net_rpc_request_stream_next` → `Runtime::
block_on`), and the handler bridges delivered that callback on
`tokio::task::spawn_blocking` — a thread with the runtime context
ENTERED. Every binding-side `block_on` aborts in-context by design (a
panic cannot cross the C ABI), so any draining handler was a
process-kill reachable through the DOCUMENTED Go pattern
(`mesh_rpc.go`'s `ClientStreamingHandler`: "Drains the request stream").
The unary paths never exposed it (bytes in / bytes out, no FFI
re-entry). Fixed at the mechanism, not the witness: the shared
`spawn_handler_thread` (a dedicated OS thread per handler invocation,
same `Result`-join shape) now backs all EIGHT handler bridges in this
row — `org-ffi`'s four (unary + three shapes) and `rpc-ffi`'s four
(unary + client-streaming + duplex + streaming) — so a Go handler
re-enters the FFI exactly as any goroutine does. What proves the abort
is GONE (executed, at the assertion level): the CS/DX live cells' served
handlers drain INSIDE the callback — `runClientStreamLive`'s handler
loop over `stream.Recv()` must observe both uploaded items and the clean
terminal before its terminal-aggregate assertion
(`if pong.N != 3 { t.Fatalf(...) }` fails unless the handler's
in-callback `net_rpc_request_stream_next` re-entry completed the `1+2`
count), and `runDuplexLive`'s handler Recv/Send loop is the exact path
its `len(got) != 2` and echo-order assertions
(`got[0].N != 1 || got[1].N != 4`) depend on. Under the old
`spawn_blocking` dispatch those same bodies died at the `block_on`
abort — the first CS run's verbatim `FATAL: rpc FFI called from inside a
tokio runtime context; aborting to avoid runtime-in-runtime panic across
the FFI boundary` is the executed inverse — and after
`spawn_handler_thread` the identical assertions pass (6/6 live cells,
the handler path exercised in every one).

**F-S4Go-2 — no discovery preflight seam on the binding surface
(source-established).** The first live run failed cleanly with
`org:discovery:no_authorized_provider … (0 private candidate(s)
considered)`: the scoped/private announcements ride the announce path at
the core's re-announce cadence (`max(capability_reannounce_interval,
min_announce_interval)`, `min_announce_interval` defaulting to 10 s),
and nothing on the Go/cgo surface can (a) shorten
`min_announce_interval` (`MeshNodeConfig` has the knob; `net_mesh_new`'s
JSON does not expose it — the Rust live harnesses set 50 ms "so the test
converges promptly") or (b) poll discovery readiness
(`OrgClient::authorized_candidates` is Rust-internal). The Go cells
therefore converge through `convergeOrgCall` — a bounded retry on the
EXACT local `discovery/no_authorized_provider` class, the Go form of
`tests_live.rs`'s `converge_discovery` precondition (legitimate: that
refusal sends nothing and mints no proof, so the facade's no-retry rule
is untouched) — plus the caller node starting first so the provider's
start-time announce cannot race the caller's receive loop. A published
preflight observable would let every binding wait on the state the
assertion is about instead of on the call outcome.

### The C skill example — carve provenance and receipts

Per Main's bounded-carve ruling (option (b)): ONE new file
`.claude/skills/net-event-bus/examples/net_org_streaming.c` + ONE additive
`docs/data/examples.yaml` entry — nothing else in those paths. The pair
landed at **`c62ecd740`** inside S4Node's index-wide impl commit (content
disclosed intact); the S4Go impl commit carries exactly ONE delta over the
swept copy — the `--manifest-path net/crates/net/Cargo.toml` fix for the
repo-root runner (the skill-examples runner executes from the repo root,
which has no root `Cargo.toml`; without the fix the example's scenario
generation fails) — named in the commit message per Main's file-state
ruling). The example demonstrates §4.4's Go/C surface as a consumer: it
boots a throwaway cross-org chain with the in-repo generator (credentials
are ISSUED material — the example mints none), serves
`net_org_serve_streaming` + a shape dispatcher carrying the verified
`net_org_caller_t`, calls `net_org_call_streaming`, drains the SHARED
handle through `net_rpc_stream_next`, and asserts item correlation,
explicit completion, and handler-side attribution.

Post-sweep deltas over `c62ecd740`'s copy, all disclosed in `14d5b87f3`'s
message (the carve boundary — ONE new file + ONE additive manifest entry —
is unchanged): (1) the `--manifest-path net/crates/net/Cargo.toml` fix
named in Main's file-state ruling; (2) the bounded discovery-convergence
precondition on the opening call (F-S4Go-2's `convergeOrgCall` form —
retrying only the LOCAL `no_authorized_provider` class, bounded at 60 s);
(3) caller-first start order; (4) the `<time.h>` / `<windows.h>` includes
behind the sleep helper (gcc 16 makes implicit declarations hard errors).
Each was proved necessary by an executed failing run before the fix.

### Witnesses and counts (executed)

Roster FROM SOURCE (`go/org_test.go`) — the Stage-4 live cells, 6 named
tests (three shapes x two authority modes), each a live two-node
call-AND-serve through the C ABI with the handler-side verified-caller
attribution assertion (`(*livePair).assertCaller`: exact acting-org hex,
exact provider-org hex, exact `IsSameOrg`, exactly-one handler call):

| test | shape | authority | result |
|---|---|---|---|
| `TestLiveGrantedStreamingFromAGeneratedScenario` | server-streaming | granted (cross-org) | PASS |
| `TestLiveGrantedClientStreamFromAGeneratedScenario` | client-streaming | granted (cross-org) | PASS |
| `TestLiveGrantedDuplexFromAGeneratedScenario` | duplex | granted (cross-org) | PASS |
| `TestLiveSameOrgStreamingFromAGeneratedScenario` | server-streaming | same-org | PASS |
| `TestLiveSameOrgClientStreamFromAGeneratedScenario` | client-streaming | same-org | PASS |
| `TestLiveSameOrgDuplexFromAGeneratedScenario` | duplex | same-org | PASS |

Command (cwd `go/`, cgo ON, mingw gcc, the built test binary):
`RUN_INTEGRATION_TESTS=1 go test -run 'TestLiveGranted|TestLiveSameOrg'
-v -timeout 40m` — **6/6 PASS** (1.96s / 0.79s / 0.78s / 0.82s / 0.83s /
0.79s at the final head). Per-shape content assertions: SS = 3 correlated
items + explicit terminal; CS = `1+2` aggregate across the upload
half-close (proves the in-callback drain); DX = 2 correlated echoes +
half-close + clean terminal.

Green-estate preservation: the full `go test` suite from `go/` cwd —
**PASS** (includes `header_parity_test.go`'s net.h/net.go.h functional
surface, `callback_recover_test.go`'s FFI-01 trampoline roster guard
(minimum 10 guarded; the three new trampolines carry the first-statement
`recoverCallback` guard), the `abi_stability_*` guards, and the 10
pre-existing `TestOrg*` units).

Rust units (`cargo test -p net-rpc-ffi -p net-org-ffi`), five `test
result` lines verbatim:
```
test result: ok. 20 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 5.41s
test result: ok. 45 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
```
20 = `net-org-ffi`'s tests mod (incl. `header_numeric_contract_matches_rust`
— `net_org.h`'s `#define`s incl. `NET_ORG_ABI_VERSION 0x0002` == the Rust
consts; `check_abi_version_requires_exact_equality`;
`net_org_caller_layout_is_five_32_byte_ids`;
`abi_version_is_independent_and_exact`, advanced to 0x0002 with its
0x0001 -> 0x0002 history in-doc at this bump); 45 + 1 = `net-rpc-ffi`'s
tests incl. `runtime_entry_tests`; the two zero rows are the doc-test
targets (no rustdoc examples in either crate).

### Inverse receipt (executed, raw)

Property: the handler receives the VERIFIED caller projection
(attribution) — never caller-claimed data. Witness:
`TestLiveGrantedStreamingFromAGeneratedScenario` via
`(*livePair).assertCaller` (go/org_test.go:773).

Restore-equality baseline — `net/crates/net/bindings/go/org-ffi/src/lib.rs`
sha256 `b6b865552c824e96a28dd3e2432f31b0c3dd8553fad9e033e920796736014713`.

Production-site mutation (the projection EVERY shape's dispatcher feeds
Go), `impl From<&OrgCaller> for NetOrgCaller`, org-ffi/src/lib.rs:424-425:

```diff
-            acting_org: *c.acting_org.as_bytes(),
-            provider_org: *c.provider_org.as_bytes(),
+            acting_org: *c.provider_org.as_bytes(),
+            provider_org: *c.acting_org.as_bytes(),
```

Command: `cargo build --release -p net-ffi` (Finished, 1m21s) then
`RUN_INTEGRATION_TESTS=1 go test -run
TestLiveGrantedStreamingFromAGeneratedScenario -test.v`. **Exit 1.**
Verbatim failure:

```
=== RUN   TestLiveGrantedStreamingFromAGeneratedScenario
    org_test.go:773: handler saw acting org 55154f42065ea5a1bea05463826be2684eb92df92c100027aabaae57ca554207, want bc7cbcb5636375fa1d82434d466724d92377f53b980695dd49d26d0ce12205a5 (verified projection)
--- FAIL: TestLiveGrantedStreamingFromAGeneratedScenario (0.80s)
FAIL
```

Red for the RIGHT reason: the swapped projection delivered the provider's
org where the caller's was required — the attribution assertion itself,
not a crash, not a compile error.

Restore: the reverse diff. `sha256sum` = baseline — **byte-identical**.
Rebuild (1m22s) + same command. **Exit 0.**

```
=== RUN   TestLiveGrantedStreamingFromAGeneratedScenario
--- PASS: TestLiveGrantedStreamingFromAGeneratedScenario (0.80s)
PASS
```

All four recorded: the diff, the command + exit under mutation, the
verbatim failure, the restored run. The four weakenings (witness-
discipline) named for the lane's earlier red -> green on these cells: the
first run failed on `org:discovery:no_authorized_provider` and was
repaired by **#1 the precondition was fixed** (`convergeOrgCall`, the Go
form of `tests_live.rs`'s `converge_discovery`, plus caller-first start
order) — the assertions are byte-identical through that repair. #2 window
widening, #3 assertion relaxation, #4 witness deletion: all absent (no
assertion text, bound, or roster row changed to obtain green).

### Guard scripts and parity (executed)

- `python .github/scripts/check-ffi-exports.py --artifact
  net/crates/net/target/release/net.dll` -> `baseline count: 578` /
  `\u2713 net.dll: export set matches the baseline` (the 9 new
  `net_org_call_streaming/_client_stream/_duplex`,
  `net_org_set_{streaming,client_stream,duplex}_handler_dispatcher`,
  `net_org_serve_{streaming,client_stream,duplex}` symbols present; 9/9
  name-matched in the baseline).
- `python .github/scripts/check-rpc-abi-parity.py` -> all four surfaces
  `ok` (nrpc, org, meshos, compute) — header<->implementation signatures
  agree incl. the new fn typedefs (named `NetOrg{Streaming,ClientStreaming,
  Duplex}HandlerFn` on BOTH sides).
- `python .github/scripts/check-callback-buffer-ownership.py` -> `No Rust
  libc::free calls across 9 Go FFI source files.`
- `python .github/scripts/check-c-doc-snippets.py` -> `All 2 published C
  program(s) compile.`
- `python .github/scripts/check-header-count.py` -> 11 headers tracked,
  all enumerating files agree.
- `python .github/scripts/skill_examples.py --validate` -> ok (the
  org-streaming entry: every binding in files-or-absent, `run.expect`
  valid + end-anchored, the file git-tracked).
- `go vet ./...` clean; `cargo fmt -p net-rpc-ffi -p net-org-ffi --
  --check` clean.
- C example compile+LINK against the single `libnet` (the runner's exact
  command shape):
  `gcc -o net/crates/net/target/c_org_streaming
  .claude/skills/net-event-bus/examples/net_org_streaming.c -I
  net/crates/net/include -L net/crates/net/target/release -lnet -lpthread
  -ldl -lm` -> **exit 0**, no diagnostics (net.go.h + net_rpc.h +
  net_org.h in one TU).
- Skill-examples runner EXECUTION (the carve's run evidence) — command
  named verbatim: **`.github/scripts/run-skill-examples.sh --lang c`**
  with `NET_LIB_DIR=<...>/net/crates/net/target/release` (invoked under
  `C:/Program Files/Git/bin/bash` — `bash` on this host's PATH resolves to
  `C:\Windows\system32\bash.exe`, the WSL launcher). Executed here: the
  manifest entry IS picked up as its own `org-streaming` row and PASSES
  the runner's compile+link step against the single `libnet` (the row did
  not report "did not compile or link"). The runner's C RUN phase cannot
  go green on THIS host for ANY row — environmental, not row content:
  five pre-existing rows (`jobqueue`, `objectstore`, `liveconfig`,
  `tokenchannel`, `failover`) do not compile at all (POSIX-only
  `<arpa/inet.h>`), and every row that compiles is written to
  `$WORK/c-<id>` EXTENSIONLESS, which Windows `CreateProcess` will not
  load (it appends `.exe`) — `hello`, `event-log` and `org-streaming` all
  report "exited non-zero" from an unspawnable binary. Green full-runner
  execution is CI-ubuntu's (`run-skill-examples.sh --lang go` and
  `--lang c` against `libnet.so` in the `go-tests` job) — never executed
  here. The runner's own steps ARE green for this entry once the binary
  is runnable (the Windows spawn quirk aside), verbatim:
  ```
  gcc -o net/crates/net/target/c_org_streaming.exe .claude/skills/net-event-bus/examples/net_org_streaming.c -I net/crates/net/include -L net/crates/net/target/release -lnet -lpthread -ldl -lm
  gcc link exit=0
  wrote C:\Users\chief\AppData\Local\Temp/net-org-streaming-194596\manifest.json (service "customer.read", provider org 55154f42065ea5a1bea05463826be2684eb92df92c100027aabaae57ca554207, caller org bc7cbcb5636375fa1d82434d466724d92377f53b980695dd49d26d0ce12205a5)
  RESULT ok chunks=3 attribution=verified
  run-exit=0
  ```
  — the stdout line matches the entry's `run.expect`
  `RESULT ok chunks=3 attribution=verified$` under the same
  end-anchored `grep -qE` semantics `assert_run` applies. The example
  file at execution: 19728 bytes, sha256
  `9c5031f2b686a0426e55ccd047e7c689e7092bf8ce4c3a96957088ff6f509b1d`.

### Never executed here (complete)

- **Linux / macOS execution** of every receipt above — this host is
  Windows 11 / mingw gcc / MSVC-built `net.dll` (+ a locally generated
  `libnet.dll.a` import library so `-lnet` resolves; build-dir artifacts
  only, nothing tracked). CI's ubuntu `go-tests` job runs
  `run-skill-examples.sh --lang go` and `--lang c` against `libnet.so`;
  those runs are Main's pinning surface.
- **`check-ffi-exports.py`'s ELF and Mach-O paths** — only the PE path
  (`net.dll`) executed here.
- **`RUN_INTEGRATION_TESTS=1` for the pre-existing Go live suites**
  (e.g. `subnet_live_test.go`) — the estate run above covers the unit
  surface; the pre-existing live suites were not re-run (unchanged code,
  and each is its own scenario-generation cost).
- **A second inverse (the four weakenings applied as mutations)** — the
  receipt above is one production-site inverse per the inverse rule; the
  weakenings are NAMED from the diff discipline, not executed as
  mutations.
- **Timeout / handler-drop execution at the C level** (a handler mid-
  emission at retirement observing `NET_RPC_ERR_STREAM_DONE`): the F-S3.1-
  2 level is DOCUMENTED at every layer of this surface (its named
  requirement) but the C example exercises the happy path only; the
  retirement-observable behavior is witnessed at the Rust tier (Stage 3)
  and through the Go cells' ErrStreamDone drains.

Disk discipline: writes were size+sha256-verified after every write (F16)
across three disk-pressure episodes; no ENOSPC hit in this lane. Last df
reading: 56 GB free at the record commit (post-reclamation steady state;
two earlier readings at 16 GB and 28 GB are noted in the coordination
log; this lane's writes were size+sha256-verified throughout (F16), no
ENOSPC hit).

