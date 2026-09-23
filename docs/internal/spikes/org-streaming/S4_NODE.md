# Stage 4 — per-lane report: S4Node (the Node row, `@net-mesh/core`)

`## 8.1 S4Node — Node bindings (`net/crates/net/bindings/node/**`)`

**Lane:** S4Node. **Pinned brief:** `spikes/org-streaming/S4_BRIEF.md` @`0f2f2d69c`.
**Base:** `0f2f2d69c` (Stage 3 accepted at `2225de011`; the S3R repair lane ran in
parallel on `sdk/src/org/**` + `sdk/tests/**` — never touched here). Date:
2026-09-23. Owner-pending: none. Host: this Windows workstation only.

## What landed (source-established; F16 size+sha per file)

- **§4.4's Node verbs, verbatim.** `OrgClient.callStreamingBytes(service,
  request, deadlineMs?, cancelToken?) -> RpcStream`,
  `callClientStreamBytes(service, deadlineMs?, cancelToken?) ->
  ClientStreamCall`, `callDuplexBytes(service, deadlineMs?, cancelToken?) ->
  [DuplexSink, DuplexStream]` — each over the FROZEN
  `call_{streaming,client_stream,duplex}_bytes_deadline` seam and returning the
  EXISTING napi handle classes (`bindings/node/src/mesh_rpc.rs`), per §4.4's
  "no new stream wrapper per binding" (the duplex verb returns the split
  halves directly because `into_split` is synchronous on the core handle).
  Seam contract verbatim: `deadlineMs == 0` ⇒ the facade's default lifetime
  (Owner Q1, 300 s), NEVER "no deadline"; `cancelToken == 0` ⇒ uncancellable
  (reserve via `MeshRpc.reserveCancelToken()` on the same node, fire
  `MeshRpc.cancelCall(token)`); neither argument is an authorization input.
- **`serveOrgStreaming` / `serveOrgClientStream` / `serveOrgDuplex`** — sync
  free verbs registered on `org_serve_runtime()` over the FROZEN
  `serve_org_{streaming,client_stream,duplex}_bytes_node` seams, composing the
  ONE `org_caller_js` projection (factored out of `dispatch_to_js` — the
  `org.rs:490-501` projection the brief names) with `mesh_rpc.rs:1409-1425`'s
  hand-written `ToNapiValue` array marshaling: `[caller, req, sink]` /
  `[caller, stream]` / `[caller, stream, sink]`, `OrgCaller` FIRST in every
  handler, `ts_args_type` tuples generated verbatim into `index.d.ts`
  (verified). Handler shape `(args) => Promise<Buffer>`; the Promise resolving
  is the "handler done" signal (streaming/duplex ignore the Buffer; the
  client-streaming Buffer is the terminal response body), mirroring
  `serveStreaming`/`serveClientStream`/`serveDuplex`.
- **`OrgServeHandle` gains the runtime handle** (`mesh_rpc.rs:634-644`'s
  `ServeHandle` pattern): the handle carries the runtime its registration ran
  on and `close()` enters it before dropping the inner handle.
- **Midstream errors route through `classifyOrgError` (§4.4).** The raw org
  handles render call outcomes through the ONE `OrgSdkError::to_wire()`
  vocabulary via the binding-side mirror of the facade's `map_rpc_error`
  (`mesh_rpc.rs::org_err_from_inner`: status `0x0009` + the single coarse
  reason byte ⇒ `org:admission_denied:<bucket>` with the facade's
  least-informative-bucket fallback; everything else ⇒ `org:rpc:<the frozen
  nRPC kind>`). Local binding-usage refusals (`nrpc:stream_closed`) are not
  call outcomes and keep the `nrpc:` usage vocabulary. The `org.ts` typed
  layer then throws the `classifyOrgError` OUTPUT at every call-outcome throw
  site (openings and midstream alike) — `TypedOrgClient.call`'s existing
  contract, extended across the stream handles via thin raw-seam shims that
  wrap the reused typed classes (no second stream type).
- **`org.ts` typed wrappers (§4.4), reusing the existing typed classes:**
  `TypedOrgClient.callStreaming/callClientStream/callDuplex` return
  `TypedRpcStream` / `TypedClientStreamCall` / `TypedDuplexSink` +
  `TypedDuplexStream`; `serveOrgStreamingTyped` / `serveOrgClientStreamTyped` /
  `serveOrgDuplexTyped` wrap the raw serve verbs with JSON and hand the
  handler `TypedRequestStream` / `TypedResponseSink` (the `[caller, req, sink]`
  tuples). `OrgCallOptions` (`deadlineMs` / `cancelToken`) is the only new
  input type.
- **The F-S3.1-2 NAMED ITEM — the handler-drop contract documented at the
  stated level on every handler surface** (the three Rust serve verbs and the
  three `org.ts` typed serve rows): the retire supervisor polls the handler
  future and may DROP it WITHOUT a final poll on caller cancel / deadline /
  revocation / teardown — a handler must not assume cancellation is a
  handler-side event (its promise may never settle; `finally` is not
  guaranteed to run). Cancellation is observed through the RETIREMENT
  OBSERVABLES: the caller's terminal stream outcome (classified via
  `classifyOrgError` — or, for a caller-initiated cancel, the retired
  stream's `null` EOF, executed below), the exactly-one CANCEL from a dropped
  call handle, and `OrgServeHandle.close()`; the sink/request-stream are
  released by the Rust bridge when the handler promise settles OR the future
  drops — never on a V8 GC, never on a handler-side `finally`.
- **`test-helpers`-gated `testMintSameOrgScenario(outdir)`** (NOT exported to
  production consumers — the `test_inject_synthetic_peer` precedent): mints
  the same-org chain no generator writes — two adopted node authorities in
  ONE org with the §3.4 out-of-band owner audience staged across both
  authority dirs + the caller's membership and wide-open dispatcher grant —
  the Node twin of the Rust fixture's `fast_mesh(.., shared_audience)`.
  `gen_org_scenario` remains the GRANTED fixture (same manifest every
  language's live cell loads).
- **Shipped-declaration defect repaired (in-row, pre-existing).** The
  consumer-compile probe (below) reddened on `index.d.ts` naming
  `GreedyConfigJs`/`DataGravityConfigJs` which only declared under
  `dataforts` — the repo's documented napi-derive cfg trap (`lib.rs`'s
  test-helpers comment), surfacing as `TS2304` under a consumer's
  `skipLibCheck: false` in every shipped (non-dataforts) build. Fixed at the
  source (`src/cortex.rs`): the two inert config-POD declarations are
  ungated with a why-note + `#[allow(dead_code)]` (in a feature-off build
  nothing constructs them, but the declaration must exist); their consumer
  methods stay feature-gated.

## Witnesses and counts (executed)

Toolchain: the row's own — `npx napi build --platform --no-default-features
--features net,nat-traversal,cortex,compute,groups,meshos,deck,meshdb,
aggregator,tool,consent,mcp,publish,payments,payments-http,delegation,a2a,
org,test-helpers` (the CI binding job's exact feature list,
`ci.yml:3589` at head — the brief's `3406-3454` line numbers predate head
drift) → `npm run build:ts` → `npx vitest run` (vitest, default zero
retries). All executed on this workstation.

**Green estate preservation (the full `npx vitest run`, exact head):**
**39 test files passed | 2 skipped (41); 606 tests passed | 9 skipped (615)**;
exit 0. The 2 skipped files / 9 skipped tests are the suite's pre-existing
integration-gated cells (RUN_INTEGRATION_TESTS), unchanged by this lane.

**Roster FROM SOURCE (the 10 new cells; `bindings/node/test/**`):**

| # | file | test name | what it pins |
|---|---|---|---|
| 1 | `org_live.test.ts` | `live_same_org_streaming_call_and_serve` | SS call AND serve, same-org: exact streamed payloads + the five verified `OrgCaller` facts |
| 2 | `org_live.test.ts` | `live_same_org_client_stream_call_and_serve` | CS call AND serve, same-org: exact terminal summary + verified attribution |
| 3 | `org_live.test.ts` | `live_same_org_duplex_call_and_serve` | DX call AND serve, same-org: exact echoes + verified attribution |
| 4 | `org_live.test.ts` | `a_stream_call_against_a_unary_only_provider_is_refused_not_supported` | the raw `org:` wire vocabulary + `classifyOrgError`; the coarse-byte mirror pin (`reason === 'not_supported'`) |
| 5 | `org_live.test.ts` | `midstream_outcomes_route_through_classify_org_error` | (a) cancel retirement: the retired call's items are DISCARDED and the stream ends `null`, never a late item (+ the ended stream stays ended); (b) a midstream terminal error (handler rejection) throws the classifyOrgError OUTPUT — `OrgError{domain:'rpc', kind:'server_error'}` |
| 6 | `org_live.test.ts` | `live_granted_streaming_call_and_serve` | SS call AND serve, granted (the `gen_org_scenario` manifest) + verified attribution |
| 7 | `org_live.test.ts` | `live_granted_client_stream_call_and_serve` | CS call AND serve, granted + verified attribution |
| 8 | `org_live.test.ts` | `live_granted_duplex_call_and_serve` | DX call AND serve, granted + verified attribution |
| 9 | `org_live.test.ts` | `closing_client_and_serve_handle_lets_both_meshes_shut_down_cleanly` | bounded cleanup: the documented `close() → close() → shutdown()` order lets BOTH `mesh.shutdown()` calls resolve strictly (the failure mode is the "outstanding references exist" rejection) |
| 10 | `consumer_compile.test.ts` | `a_consumer_program_compiles_against_the_shipped_org_surface` | the consumer-compile probe: `tsc --noEmit --skipLibCheck:false` over `test/consumer/org_streaming_consumer.ts` — a consumer program pinning the typed verbs AND the generated `index.d.ts` signatures (`OrgClient.callStreamingBytes` etc., the `[caller, req, sink]` serve tuples, `OrgCallOptions`, the error classes) with full annotations; clean (empty) output asserted |

The pre-existing X2 cell (`a Node caller invokes a Granted capability a Node
provider serves, from generated artifacts`) is preserved verbatim and passes
in the same run. Per-shape × call-and-serve × same-org-and-granted = 3 shapes
× 2 authority modes × (call+serve in one sibling) = cells 1–8. The
`OrgCaller`-projection property rides every shape sibling's named
`verified …` assertions (the inverse receipt below).

**Consumer-compile probe (the acceptance's third cell).** `test/consumer/
org_streaming_consumer.ts` + `test/consumer/tsconfig.json`
(`skipLibCheck: false`, the careful-consumer check) + the vitest driver. It
compiled CLEAN at the exact head (cell 10 green inside the 606).

## Inverse receipt (executed, raw) — R-S4Node-projection

Property: **a handler's `OrgCaller` fields are the admission-verified facts,
never caller-claimed or routing-derived data** (the §4.4 projection rule).
Production site: `bindings/node/src/org.rs::org_caller_js` — the ONE
projection every handler shape (unary + the three streaming) projects
through.

- **Baseline** (F16): `sha256 cd89f826cb6a48845488b1b0befacb25a54831703dcb943312ed89fdd9cd1454`
  for `bindings/node/src/org.rs`. The witness file was sha-locked throughout:
  `sha256 f6e929e7a0553e053899b42215e3e92afc3f2ddbc4a3583e9b21add613cd825c`
  for `test/org_live.test.ts` (before, during, and after — the receipt's zero
  test-side delta).
- **The applied inverse** (the brief's verbatim example shape — "the
  `OrgCaller` projection reading origin instead of verified facts"): a
  bounded diff at `org_caller_js` (delta +11/−0 to `org.rs`, 0 to the test
  file) making the projection REPORT substitute bytes (`[0xEE; 32]` for the
  five id fields, `is_same_org: true`) instead of the verified
  `net_sdk::org::OrgCaller` facts — discarding the verified attribution for
  unverified fabrications, which is what reading routing origin would amount
  to at this seam (the projection's only input IS the verified caller).
- **Run** (mutation build via `npx napi build` on the same feature list):
  `npx vitest run test/org_live.test.ts -t "live_same_org"` — **3 failed |
  7 skipped**, exit non-zero. ALL THREE shape siblings redden at the NAMED
  assertion, verbatim:

  ```
  AssertionError: verified caller entity: expected 'eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee…' to be '2152f8d19b791d24453242e15f2eab6cb7cff…' // Object.is equality
     304|   expect(hex(caller.entity), 'verified caller entity').toBe(expected.entity)
     305|   expect(hex(caller.actingOrg), 'verified acting org').toBe(expected.actingOrg)
     306|   expect(hex(caller.providerOrg), 'verified provider org').toBe(expected.providerOrg)
  ```

  (`live_same_org_streaming_call_and_serve`,
  `live_same_org_client_stream_call_and_serve`,
  `live_same_org_duplex_call_and_serve` — each its own run-to-red at
  `verified caller entity`, `org_live.test.ts:304`.) The payload assertions
  upstream of the attribution pin still passed under the mutation — the
  oracle discriminates exactly the verified-projection property.
- **Restore:** reverse edit; `sha256 cd89f826cb6a48845488b1b0befacb25a54831703dcb943312ed89fdd9cd1454`
  == baseline (byte-identical restore); rebuilt; restored-green run at the
  same tree: `npx vitest run test/org_live.test.ts test/consumer_compile.test.ts`
  — **2 files, 11/11 passed** (X2 + the 9 new cells), exit 0.
- **Four weakenings: NONE.** No precondition fixed, no window widened, no
  assertion relaxed, no witness deleted — the witness file's sha stayed
  `f6e929e7…` across the entire receipt and the mutation touched one
  function body at the production site.

## Findings (state, not decide)

1. **The per-handle retirement vocabulary nuance (executed).** The plan's
   per-handle line "deadline/cancel retirement `Rpc(Timeout)`/`Rpc(Cancelled)`"
   is the FACADE item vocabulary. At the Node raw/typed handle, a
   CALLER-INITIATED cancel retires the call to a clean `null` EOF (the §2.2
   retirement-discard rule: buffered items are discarded, never drained
   late) — there is no `Err(Cancelled)` item to classify for a self-cancel.
   `org:rpc:cancelled` remains the vocabulary for a cancel that races an
   in-flight observation, and `org:rpc:<kind>` classification of midstream
   terminal ERRORS is witnessed by cell 5(b). Deadline-expiry midstream kind
   at the Node surface was NOT executed here (see never-executed).
2. **The shipped-declaration cfg trap has a second surface class (repaired
   in-row).** `GreedyConfigJs`/`DataGravityConfigJs` (above). The consumer
   probe is what caught it; the probe's `skipLibCheck: false` is the
   standing guard for the next instance.

## Never executed here (complete)

- The `org:rpc:timeout` midstream rendering (deadline-expiry retirement) at
  the Node surface — derived from the vocabulary mirror, never executed; the
  deadline leg of cell 5 was replaced by the executed cancel-EOF + handler
  rejection observables.
- Midstream revocation at the Node surface (`org:admission_denied:denied` as
  a STREAM terminal) — witnessed at the Rust facade (S3's
  `revocation_surfaces_as_final_admission_denied_item`); the Node mirror's
  `0x0009` arm is executed through the CS lazy-open denial (cell 4).
- A wire-level CANCEL-frame count (no seam exposes frame counts).
- The `dataforts`-ENABLED build of the ungated config declarations (the
  structs compile unconditionally — `cargo check` was executed only for the
  CI binding feature list, which excludes `dataforts`; the lint matrix's
  dataforts set was never run here).
- `npm run typecheck:tests`, `npm run build` (release), the
  `check-ts-consumer.sh` CI job, `ci.yml`/floor re-pins (Main's by contract),
  and CI itself (branch unpushed).
- Any host but this Windows workstation; Node ≠ the system Node
  (`C:\nvm4w\nodejs\node.exe`, Node ≥ 20 per `engines`).
- Every other Stage 4 row (other lanes by contract).

## CI floor note (Main pins, never a lane)

The binding-suite pattern gains these 10 names (roster above) across
`test/org_live.test.ts` (9 new) and `test/consumer_compile.test.ts` (1 new
file). Full-suite counts at head: **615 vitest tests** (606 pass + 9
pre-existing integration-skips), 41 files.

## Disclosure (disk — the 4th ENOSPC episode)

One `edit` write to `test/org_live.test.ts` failed mid-episode with ENOSPC
and TRUNCATED the file to 0 bytes; detected immediately and broadcast to
Main. Restored byte-exact from HEAD (`git restore` — 188 lines / 7272 bytes /
`sha256 bd7377a589a3e834017e5da518704573d330a9b9989753d2222d375b9c7d7c05`,
clean `git diff`), then re-applied. Free-space readings through the episode:
29 GB → 0 → 20 GB (after Main's reclamation + this lane's own npm-cache
purge under the split-deletion policy). F16 size+sha256 recorded and verified
after every subsequent write (ledger in the lane's session record).

## F16 file ledger (final, sha256)

| file | lines | bytes | sha256 |
|---|---|---|---|
| `bindings/node/src/org.rs` | 1396 | 60814 | `cd89f826cb6a48845488b1b0befacb25a54831703dcb943312ed89fdd9cd1454` |
| `bindings/node/src/mesh_rpc.rs` | 2601 | 107141 | `f476a9414adf153d9ea0874587c14262c29a838acfd456f1db033c08a8a8f5e7` |
| `bindings/node/src/cortex.rs` | 2805 | 98424 | `36797785b5538f81adc7d597b6d722a6ead769ec43b54ca5f7a8070c159a5b32` |
| `bindings/node/org.ts` | 591 | 19939 | `2552b7a51e1b4c0b8d0d2460c2442f796f26293b51c9b2b1cdb6bfe508ee6b73` |
| `bindings/node/test/org_live.test.ts` | 798 | 28424 | `f6e929e7a0553e053899b42215e3e92afc3f2ddbc4a3583e9b21add613cd825c` |
| `bindings/node/test/consumer_compile.test.ts` | 34 | 1329 | `85f4b68c8b92668665c3d209c5fbf16ca9a95ec832de3d1fdf3e8c1ddbdbde7b` |
| `bindings/node/test/consumer/org_streaming_consumer.ts` | 207 | 6327 | `46262f2fecc36d2c233333b507c57770437abe95386b929626a60f65fbc60053` |
| `bindings/node/test/consumer/tsconfig.json` | 18 | 602 | `d2b912bc7ee0d752825c354efb0a8d3d72b521fedddf409c7f38d10130b883e8` |
