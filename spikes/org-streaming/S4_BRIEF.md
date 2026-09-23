# Stage 4 — all supported SDKs and unified release acceptance

**Authorization and limits.** Authorized 2026-09-22 by the Stage 3 ACCEPT
(`docs/internal/spikes/org-streaming/S3_REVIEW_PACKET.md`, pinned head
`2225de011`) — that verdict governs Stage 3's acceptance and the Stage 4
gate, and its three findings are non-blocking (the S3R repair round closes
them in parallel; it owns `sdk/src/org/**` exclusively and NOTHING in this
stage touches those files). **Stage 4 as written (the plan's Stage 4 table +
§4.4/§4.5/§4.6 + the SDK release matrix) — nothing more.** The owner-declined
F-S1R-2 rider stays out of scope. **NAMED STAGE-4 ITEM (the reviewer's
F-S3.1-2 rider):** every binding's handler surface must DOCUMENT the §2.2
handler-drop contract (the retire supervisor may drop the handler future
without a final poll — cancellation is observed through the retirement
observables, never assumed as a handler-side event); Python's
`task.cancel()` witness and each binding's error/teardown semantics must
state this level explicitly. **F-S3R-1 context:** the Granted facade serve
arms these bindings dispatch through are being witnessed by the parallel
repair; bindings must not modify those arms (their file set is forbidden to
you).

## Source of truth — read first, in this order

1. The plan's **Stage 4 table** (the real-artifact evidence per binding),
   **§4.4** (the per-language verb specs — Node/Python/Go-C naming, the
   handle types, the error-classification seams, the Go one-commit rule),
   **§4.5** (browser/leaf: never weaken authority in WASM, no second crypto,
   attribution through direct/proxied callbacks and leader replacement,
   suspension retires ownership — no stream-lease extension),
   **§4.6** (protected tool path = the facade verbs with `ToolEvent`; no
   separate tool API), the **SDK release matrix** (the supported list; the
   workflow column is inventory, not a criterion), and the ledger (C1–C12).
2. `docs/internal/spikes/org-streaming/S1_REPORT.md` §6 + the four review
   packets (the evidence standard in practice) and `S0_MAPPING.md` (the
   browser/leaf obstacles + resolutions — the portable extraction target,
   the leaf gaps, the three JS surfaces, the fixture consumption paths).
3. Each binding's existing unary surface (the naming/idiom template) and the
   frozen core seams it calls (`*_bytes_deadline`, the typed handles).
4. `AGENTS.md` (the single-cdylib rule, the ABI/header one-commit rule,
   the Windows traps) + `TESTS.md`.

## Lanes and dispatch waves (file-disjoint)

**Wave 1 — dispatch concurrently (independent):**

| Lane | Owns exclusively | Row |
|---|---|---|
| `S4Node` | `net/crates/net/bindings/node/**` (incl. `org.rs`, `org.ts`), `net/crates/net/bindings/node/test/**` | Node |
| `S4Python` | `net/crates/net/bindings/python/**`, `net/crates/net/bindings/python/tests/**` | Python |
| `S4Go` | `net/crates/net/bindings/go/**` (org-ffi + the shared handle module), `go/**`, `net/crates/net/include/**`, `net/crates/net/adapters/mcp/examples` ONLY if the C skill example lands there (state first) | Go/C |
| `S4Browser` | `net/crates/net/browser-ts/**`, `net/crates/net/leaf/**`, `net/crates/net/tests/rtc_browser/**` | Browser/leaf (Q5) |

**Wave 2 — dispatch after wave 1 lands (dependencies named):**

| Lane | Owns exclusively | Depends on | Row |
|---|---|---|---|
| `S4TsSdk` | `net/crates/net/sdk-ts/**` | `S4Node` (the `@net-mesh/core` verbs) | Pure SDK TS (Q6) |
| `S4PySdk` | `net/crates/net/sdk-py/**` | `S4Python` (the wheel's verbs) | Pure SDK Python (Q6) |
| `S4Vectors` | `net/crates/net/tests/cross_lang_org/**`, `net/crates/net/sdk/examples/**` (the generator), per-runtime consumer files ONLY inside each lane's wave-2 handoff (coordinate: vectors land first, consumers follow) | all wave-1 verb sets | Cross-language |

Everything not in these tables is Main's or frozen (core, `sdk/src/org/**` =
the S3R lane's, `ci.yml`, `.config/nextest.toml`, `spikes/**`,
`docs/internal/**`). A needed change outside your row = finding, STOP.

## Per-lane demands (the plan's table + §4.4 verbatim)

**Node:** napi verbs `OrgClient.callStreamingBytes/callClientStreamBytes/
callDuplexBytes` returning the EXISTING `RpcStream`/`ClientStreamCall`/
`DuplexSink`+`DuplexStream`; `serveOrgStreaming/serveOrgClientStream/
serveOrgDuplex` on `org_serve_runtime()` composing the `OrgCaller` projection
(`bindings/node/src/org.rs:490-501`) with the `[caller, req, sink]` TSFN
argument shape (`mesh_rpc.rs:1409-1425`); typed wrappers in `org.ts` reusing
`TypedRpcStream` etc.; `OrgServeHandle` gains the runtime handle
(`mesh_rpc.rs:634-644`); midstream errors through `classifyOrgError`.
Evidence: `bindings/node/test/org_live.test.ts` siblings per shape; the
built addon on the CI feature list (`ci.yml:3406-3454`); consumer-compile.

**Python:** `OrgClient.call_streaming/call_client_stream/call_duplex` (sync,
returning `RpcStream`…) AND an `AsyncOrgClient` with the async forms
returning the `Async*` classes (none exists today — `org.rs:216-261`);
`serve_org_streaming/…client_stream/…duplex` passing `caller_dict` +
`ResponseSinkSend`/`RequestStreamRecv`; `.pyi` parity (the
`test_stub_drift.py` discipline); midstream errors through `org_err_to_py`.
Evidence: `tests/test_org_live.py` siblings (sync AND async); the
wheel-acceptance profile (`ci.yml:3856-3857`); the `task.cancel()`
propagation witness (stating the F-S3.1-2 handler-drop level).

**Go/C:** `net_org_call_streaming/_client_stream/_duplex` returning the
existing `RpcStreamHandleC`/`ClientStreamCallHandleC`/`DuplexCallHandleC`
(the handle types move to a module both `rpc-ffi` and `org-ffi` name — one
`libnet`, the single-cdylib rule); `net_org_set_{streaming,client_streaming,
duplex}_handler_dispatcher` (rpc-ffi's fn types + a leading
`*const NetOrgCaller`); `net_org_serve_*`; `NET_ORG_ABI_VERSION` bump;
`exports.baseline --update`, `include/net_org.h`, `go/org.go` preamble,
header-parity, callback-buffer ownership — **ALL IN ONE COMMIT**. Go:
`OrgClient.CallStreaming/CallClientStream/CallDuplex` returning the existing
handles (reusing `streamHandleGuard` + the ctx-cancel watcher);
`ServeOrgStreaming`… generics beside `ServeOrg`; midstream errors through
`parseOrgError`. Evidence: `go/org_test.go` live siblings with
`RUN_INTEGRATION_TESTS=1` + cgo ON; the C skill example against the single
`libnet`; `check-ffi-exports.py`, header-parity, callback-buffer scripts
green.

**Pure SDKs (Q6):** thin re-exports/forwarding only (no protocol logic).
TS (`S4TsSdk`): `sdk-ts` exposes the org verbs — remember the
`package.json` `exports` entry (S0_MAPPING obstacle 8) — and the witness is
a CONSUMER PROGRAM that calls AND serves all four shapes through
`@net-mesh/sdk` alone (the `check-ts-consumer.sh` pattern but EXECUTABLE —
"Nothing here runs" is not evidence). Python (`S4PySdk`): a consumer/runtime
run that calls and serves through `net_sdk` alone (an executed
`net_sdk`-only example under the skill-examples runner — a TS import check
does NOT cover it, Q6).

**Browser/leaf (Q5, `S4Browser`):** all four org-scoped call AND serve
shapes in the public browser SDK incl. the shared-session/leader-proxy
surface; portable proof/admission helpers extracted per
`S0_MAPPING.md`'s obstacles 1–6 (the tokio-free module set, time through
`net_wire::clock`, randomness as caller-supplied bytes, revocation facts
FED over the control plane — `org_revocation` stays native); leaf
caller/provider lifecycle for all four shapes; WASM exports; no separate TS
crypto, no native Tokio port, no silent native-only gate. Evidence: REAL
Chromium AND Firefox; browser→native AND native→browser for every shape;
independent browser identities for browser→browser; same-org AND granted
authorization; wrong-peer/old-session refusal; backpressure; half-close;
revocation; tab/leader teardown — **no native-library test substituted for
leaf execution**.

**Cross-language (`S4Vectors`):** new
`tests/cross_lang_org/streaming_opening_vectors.json` generated by the
`gen_org_error_fixtures` pattern; every runtime consumes the vector file
(today only Rust consumes `golden_vectors_streaming.json`); each runtime
against Rust in BOTH roles; one mixed non-Rust pair. Malformed/unknown
errors, narrowing IDs, callback loss or decoder disagreement must not
become success.

## Evidence rules, CI, report (verbatim from `S1_BRIEF.md`)

Raw inverse receipts at the production site per property (four weakenings;
green-under-own-inverse = FINDING, never re-pin); rosters from source;
`--retries 0 --no-tests=fail`; each lane runs ITS OWN artifact's toolchain
(napi build + `npm test`; maturin + pytest; `go test` with cgo on;
wasm-pack + the browser matrix; the skill-examples runner) — warm, per-crate
fmt only, no project-wide sweeps mid-flight (Main sweeps at stage end);
executed vs source-established; "never executed here" over inference; disk
discipline (size+sha after every write under 5 GB free — three ENOSPC
episodes this session). Commit prefixes `S4Node:`/`S4Python:`/`S4Go:`/
`S4Browser:`/`S4TsSdk:`/`S4PySdk:`/`S4Vectors:`. Append `## 8. Stage 4` to
`docs/internal/spikes/org-streaming/S1_REPORT.md` (per-lane sub-sections:
landed hashes, witnesses + counts, receipts, findings, never-executed).
Report counts + names to Main for same-commit CI floor re-pins (the
binding-suite patterns; Main pins). Owner-pending: none. Report only on
green at your exact head. NO stage n+1 (this IS the final stage; its exit is
the release acceptance gate).

## Exit (plan wording — the release gate)

Exact-head CI green; artifact/declaration/header/error parity; unary
compatibility; **every SDK × shape × role cell executed from a packaged or
CI-built artifact. A missing cell blocks the release.**
