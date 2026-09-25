# Stage 4 — per-lane report: S4TsSdk (the Pure SDKs Q6 row, TS — `@net-mesh/sdk`)

`## 8.6 S4TsSdk — pure TypeScript SDK (`net/crates/net/sdk-ts/**`)`

**Lane:** S4TsSdk. **Pinned brief:** `docs/internal/spikes/org-streaming/S4_BRIEF.md`. This
worktree's copy is pinned at `0f2f2d69c` ("Pin the Stage 4 brief"); commit
`02f2d6c9c` (Main's pin hash) is ABSENT from this worktree, and Main's
verbatim copy at `local://S4_BRIEF.md` is **byte-identical** to the worktree
copy (md5 `0adc61d3f9bf9b4fecf6bf9e65bbcb8e`, both) — the pinned content is
what governed this lane. **Base:** `692b389610a9b187491d06d9a92fec2d30982bf9`
(Stage 4 wave 1 landed; the S3R repair lane owns `sdk/src/org/**`, never
touched here). Landed as one `S4TsSdk:`-prefixed commit (implementation +
this record), exact hash reported to Main with the counts. Date: 2026-09-23.
Owner-pending: none. Host: this Windows workstation only (Node v24.19.0).

## What landed (source-established; F16 size+sha per file)

- **The `@net-mesh/sdk` org facade (`src/org/index.ts`) — Q6's thin
  re-export/forwarding layer over the LANDED `@net-mesh/core/org` typed
  wrappers. No protocol logic**: proof minting, discovery, admission,
  provider selection, routing and retirement stay the native layer's.
  What the wrapper level adds is exactly four things:
  1. **Mesh-handle resolution** — the SDK's `MeshNode` unwraps to its
     native `NetMesh` through the SDK-internal `WeakMap`
     (`src/_internal.ts`), the one place that bridge exists; a raw
     `NetMesh` passes through (the typed layer's `mesh: unknown`
     convention). Without this the typed verbs are unreachable from a
     `MeshNode`-only consumer program (the `_internal` seal hides the
     native pointer by design).
  2. **The §4.4 verbs, named per the plan's TS mapping**: `OrgClient`
     (`bind`/`call` (the PRESERVED unary)/`callExported`/
     `callStreaming`/`callClientStream`/`callDuplex`/`actingOrg`/
     `caller`/`isClosed`/`close` + `typed` drop-through) forwarding over
     `TypedOrgClient`; `serveOrg` / `serveOrgStreaming` /
     `serveOrgClientStream` / `serveOrgDuplex` forwarding over
     `serveOrg*Typed`. **The frozen rule holds at this level too — no new
     stream wrapper**: every verb returns the SAME `TypedRpcStream` /
     `TypedClientStreamCall` / `TypedDuplexSink` + `TypedDuplexStream` /
     `TypedRequestStream` / `TypedResponseSink` classes (re-exported, and
     pinned `instanceof` in the witnesses).
  3. **The `classifyOrgError` mirror at the wrapper level** — every
     call-outcome and registration throw site of the facade routes
     through `classifyOrgError` (idempotent over the typed layer's own
     classification), and the mirror (`classifyOrgError` + the five error
     classes) is re-exported so an `@net-mesh/sdk` consumer sees the
     frozen `org:` taxonomy without importing `@net-mesh/core`.
  4. **The ONE attribution projection** — `projectVerifiedCaller`, the
     single production site through which every serve wrapper hands the
     admission-verified `OrgCaller` to the handler (verified facts, never
     routing origin — `org.rs:612-614`'s rule, stated at the SDK's
     wrapper level). This is the projection/semantic site the REQUIRED
     inverse receipt mutates.
- **The §4.4 seam contract documented at the SDK's call surfaces**
  (module + per-verb docs, inherited semantics): ONE authorized provider
  pinned PER CALL (one request-bound proof, exactly-once send, never a
  facade-level retry); `deadlineMs` `0`/omitted ⇒ the facade's default
  lifetime (Owner Q1, 300 s) — NEVER "no deadline" (D3); `cancelToken`
  `0`/omitted ⇒ uncancellable (reserve via
  `MeshRpc.reserveCancelToken()`, fire `MeshRpc.cancelCall(token)`);
  neither field is an authorization input. Plus the documented teardown
  order `orgClient.close() → serveHandle.close() → await mesh.shutdown()`
  (the failure mode being the "outstanding references exist" rejection).
- **The F-S3.1-2 NAMED ITEM — the §2.2 handler-drop contract documented
  at the SDK's handler surfaces**, at the Wave-1 lanes' named level: the
  full established wording on `serveOrgStreaming` and the referenced
  statement on `serveOrgClientStream` / `serveOrgDuplex` — the retire
  supervisor may DROP the handler future WITHOUT a final poll (caller
  cancel, deadline, revocation, teardown); a handler must not assume
  cancellation is a handler-side event (its promise may never settle,
  `finally` is not guaranteed to run); cancellation is observed through
  the retirement observables (the caller's terminal stream outcome
  classified via `classifyOrgError`, the exactly-one CANCEL from a
  dropped call handle, `OrgServeHandle.close()`); the sink/request-stream
  is released by the Rust bridge when the handler settles OR its future
  drops — never on a handler-side `finally`.
- **The shipped-manifest entry (S0_MAPPING obstacle 8)** —
  `package.json` `exports` gains `"./org"` → `dist/org/index.{js,d.ts}`,
  beside `"."` and `"./tool"`; `src/index.ts` gains the org export block
  (root parity with the subpath). The consumer program executes BOTH
  entries (`MeshNode` + `OrgError` from the root; the org surface from
  `@net-mesh/sdk/org`), so a dropped `exports` entry or root/subpath name
  drift redden the executable rows, not just the compile probe.
- **The EXECUTABLE consumer program** —
  `test/org_consumer/org_streaming_consumer.ts`: a real external consumer
  (imports `@net-mesh/sdk` alone + node stdlib — never `@net-mesh/core`,
  never the source tree) that CREATES two meshes from issuer-produced
  credential files on disk, handshakes, installs authorities, and CALLS
  AND SERVES all four shapes with named assertions. The
  `check-ts-consumer.sh` pattern, but EXECUTED.
- **The driver (`test/org_live.test.ts`)** — stages the consumer project
  the `check-ts-consumer.sh` way (fresh COPIES of the packaged
  `@net-mesh/sdk` `files` set + `@net-mesh/core` in a temp node_modules,
  copy-not-symlink), **rebuilds the consumer artifact (the SDK `dist`)
  inside every execution cycle** (the spot-check rule's "rebuild the
  consumer artifact inside the cycle"), compiles the consumer with
  `skipLibCheck: false` (the careful-consumer declaration check — the
  same compile emits the executable), mints the two fixtures (same-org:
  the `test-helpers` `testMintSameOrgScenario`; granted: the
  `gen_org_scenario` cargo example — the same issuance chain every
  language's live cell loads), and executes one fresh consumer process
  per witness row.

## Witnesses and counts (executed)

Toolchain: the row's own — the `sdk-ts-tests` job's exact sequence
(`ci.yml:3657-3793`): `npx napi build --platform --no-default-features
--features redis,net,cortex,compute,groups,meshos,deck,meshdb,aggregator,
tool,org,test-helpers,dataforts` (2m12s) → `npm install` (41 packages) →
`npm run build` (tsc clean) → `npx vitest run` (vitest 5, default zero
retries) — plus `.github/scripts/check-ts-consumer.sh`. All executed on
this workstation.

**Green estate preservation (the full `npx vitest run`, exact head):**
**31 test files passed (31); 567 tests passed (567); 0 failed | 0
skipped**; exit 0; 46.6 s. The 30 pre-existing files (556 tests) pass
unchanged — the sdk-ts suite carries no integration-gated skips. The 11
new cells are this lane's (junit evidence on disk: `scr_tssdk_full_junit.xml`).

**Roster FROM SOURCE (the 11 new cells; `test/org_live.test.ts`, each
executing `test/org_consumer/org_streaming_consumer.ts` in its own
process against the staged packaged pair):**

| # | cell name | what it pins |
|---|---|---|
| 1 | `consumer_program_compiles_against_the_shipped_org_surface` | the careful-consumer check: `tsc --noEmit`-equivalent with `skipLibCheck: false` over staged COPIES of the packaged `@net-mesh/sdk` + `@net-mesh/core` — clean compile asserted (empty output); the same compile emits the executable consumer |
| 2 | `same_org_unary_call_and_serve` | **the PRESERVED unary** (same-org): `serveOrg` + `OrgClient.call` live two-mesh round trip, exact payload + the five verified `OrgCaller` facts |
| 3 | `same_org_streaming_call_and_serve` | SS call AND serve: exact streamed payloads + verified attribution + `instanceof TypedRpcStream` (the frozen no-new-stream-wrapper rule) |
| 4 | `same_org_client_stream_call_and_serve` | CS call AND serve: exact terminal summary + verified attribution + `instanceof TypedClientStreamCall` + the request stream's documented EMPTY metadata (`callerOrigin`/`callId`/`deadlineNs`/`headers` = `0n`/`0n`/`0n`/`[]` — attribution rides the `OrgCaller`, **never origin**) |
| 5 | `same_org_duplex_call_and_serve` | DX call AND serve: exact echoes + verified attribution + `instanceof TypedDuplexSink`/`TypedDuplexStream` + empty stream metadata |
| 6 | `granted_unary_call_and_serve` | **the PRESERVED unary** (granted — the `gen_org_scenario` manifest): exact payload + verified cross-org attribution (`sameOrg:false`, split org ids) |
| 7 | `granted_streaming_call_and_serve` | SS call AND serve, granted + verified attribution |
| 8 | `granted_client_stream_call_and_serve` | CS call AND serve, granted + verified attribution |
| 9 | `granted_duplex_call_and_serve` | DX call AND serve, granted + verified attribution |
| 10 | `midstream_outcomes_surface_an_org_error_never_a_false_clean_eof` | the §4.4 error route: a handler rejection midstream surfaces from `next()` as the classifyOrgError OUTPUT — `OrgError{domain:'rpc', kind:'server_error'}` — explicitly **never a false clean `null` EOF**; + the wrapper-level mirror round-trips (stable re-classification) |
| 11 | `closing_client_and_serve_handle_lets_both_meshes_shut_down_cleanly` | strict disposal: the documented `close() → close() → shutdown()` order lets BOTH `mesh.shutdown()` calls resolve STRICTLY (the failure mode is the "outstanding references exist" rejection) |

Per-shape × call AND serve × same-org AND granted = the three streaming
shapes × 2 authority modes × (call+serve in one cell) + the preserved
unary × 2 modes = cells 2–9. The verified-caller attribution (**never
origin**) rides every shape cell's `expectVerifiedCaller` (the five facts
+ `capability.length === 32`, exact); the CS/DX cells additionally pin
the org seam's EMPTY request-stream metadata — source-established at
`org.rs:953-959` and EXECUTED here. Every row runs live two-mesh round
trips (fresh `MeshNode` pair + a2a handshake + `installOrgAuthority` per
process; scoped-discovery convergence via forced announcements + fresh
call per retry — a signed proof binds one call id, the facade never
retries underneath).

Also executed at this head: `.github/scripts/check-ts-consumer.sh` —
**GREEN** (`ok @net-mesh/core + @net-mesh/sdk declarations are
self-consistent`); `check-skill-example-ts.sh`'s TYPE-CHECK phase —
**GREEN for all 9 TypeScript skill examples** against the SDK source
(its EXECUTE phase never ran here — see never-executed / F-S4TsSdk-2).

## Inverse receipt (executed, raw) — R-S4TsSdk-projection

Property: **a handler's `OrgCaller` fields are the admission-verified
facts, never caller-claimed or routing-derived data** (the §4.4 / D6
projection rule at the SDK's wrapper level). Production site:
`net/crates/net/sdk-ts/src/org/index.ts::projectVerifiedCaller` — the ONE
projection every serve wrapper (unary + the three streaming) routes
through.

- **Baseline** (F16): `sha256
  ad3ce24a22198b96d5510e51cd943659e0c3d341194ebeb9cd4019a78cdeb41c`
  for `src/org/index.ts` (17087 B). The witness files were sha-locked
  throughout: `sha256
  71614289c4fa14dc220eec1300f2be824239e100e7d41e97c5d7ed76c083dba9`
  for `test/org_live.test.ts` and `sha256
  8f5ed365e0a25db8caae8b10490462d38e83c891ad34ad9e5fa686e48deb343b`
  for `test/org_consumer/org_streaming_consumer.ts` (before, during, and
  after — the receipt's zero test-side delta).
- **The applied inverse** (the brief's verbatim example shape — "the
  `OrgCaller` projection reading origin instead of verified facts"): a
  bounded diff at `projectVerifiedCaller` (delta +14/−1 to
  `src/org/index.ts`, mutated `sha256
  1226607b2b1b2b692f389dcc182c09b30295982c71d9125ff022a6f498f9889c`,
  0 to both test files) making the projection REPORT substitute bytes
  (`Buffer.alloc(32, 0xEE)` for the five id fields, `isSameOrg: true`)
  instead of the verified `OrgCaller` facts — discarding the verified
  attribution for unverified fabrications, which is what reading routing
  origin would amount to at this seam (the projection's only input IS
  the verified caller).
- **Rebuild of the consumer artifact inside the cycle**: `npm run build`
  (tsc → the SDK `dist` the staged copies are cut from) re-run between
  mutation and execution AND between restore and execution — plus the
  driver's own rebuild step, so every execution cycle consumed the
  artifact built from the tree as mutated/restored.
- **Run** (`npx vitest run test/org_live.test.ts -t "same_org"`):
  **4 failed | 7 skipped (11)**, exit non-zero. ALL FOUR shape siblings
  redden at the NAMED assertion, verbatim:

  ```
  Error: consumer scenario same_org_streaming_call_and_serve failed
  --- stderr ---
  FAILED verified caller entity: expected '2152f8d19b791d24453242e15f2eab6cb7cffa7b6a5ed30097960e069881db12' but the handler saw 'eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee'
  ```

  (`same_org_unary_call_and_serve` [1/4],
  `same_org_streaming_call_and_serve` [2/4],
  `same_org_client_stream_call_and_serve` [3/4],
  `same_org_duplex_call_and_serve` [4/4] — each its own run-to-red at
  `verified caller entity`, the consumer's named assertion.) The payload
  assertions upstream of the attribution pin still passed under the
  mutation — the oracle discriminates exactly the verified-projection
  property.
- **Restore:** reverse edit; `sha256
  ad3ce24a22198b96d5510e51cd943659e0c3d341194ebeb9cd4019a78cdeb41c`
  == baseline (byte-identical restore); rebuilt; restored-green run at
  the same tree: `npx vitest run test/org_live.test.ts -t "same_org"` —
  **4 passed | 7 skipped (11)**, exit 0.
- **Four weakenings: NONE.** No precondition fixed, no window widened,
  no assertion relaxed, no witness deleted — both witness-file shas
  stayed constant across the entire receipt and the mutation touched one
  function body at the production site.

## Findings (state, not decide)

1. **F-S4TsSdk-1 (context, resolved in-lane): the sdk-ts toolchain's
   core build list differs from the on-disk binding build.** The
   `sdk-ts-tests` CI job (`ci.yml:3745-3749`) builds `bindings/node`
   with `--no-default-features --features
   redis,net,cortex,compute,groups,meshos,deck,meshdb,aggregator,tool,
   org,test-helpers,dataforts`; the build artifacts that were on disk at
   lane start came from S4Node's binding-job list (no `redis`, no
   `dataforts`). With those artifacts `sdk-ts`'s `npm run build` fails
   on PRE-EXISTING files (`mesh.ts:870-903`
   `serveBlobTransfer`/`fetchBlob`/`fetchBlobDiscovered`/`storeDir`/
   `fetchDir` TS2339; `redis-dedup.ts:49` `RedisStreamDedup` TS2305;
   `transport.ts:26-30` `Transfer*` TS2305) — nothing this lane changed.
   Resolved by rebuilding the gitignored build artifacts with the exact
   `sdk-ts-tests` list before running this lane's toolchain (sources
   untouched; S4Vectors notified before the rebuild — its consumer is
   native-free and unaffected; Main informed).
2. **F-S4TsSdk-2 (state, not decide): `check-skill-example-ts.sh`'s
   EXECUTE phase is not Windows-portable as run here.** The type-check
   phase is green for all 9 TypeScript examples (against the SDK source
   including this lane's exports); the execute phase fails every example
   identically BEFORE any example code runs — the script's Python-runner
   `subprocess.Popen` raises `FileNotFoundError: [WinError 2]` at
   `CreateProcess` (the child executable is not resolvable under
   WindowsStore Python 3.10 on this host). CI (ubuntu) runs that step's
   execute phase; nothing about the SDK surface is implicated.

## Never executed here (complete)

- The skill examples' EXECUTE phase (F-S4TsSdk-2's host boundary) —
  their type-check phase is green.
- The `org:rpc:timeout` midstream rendering (deadline-expiry retirement)
  at the SDK surface — inherited vocabulary mirror, never executed (the
  Node row carries the same gap).
- An explicit `deadlineMs > 0` expiry and a midstream `cancelToken`
  CANCEL retirement THROUGH the SDK facade — both forwarded verbatim and
  executed at the Node binding row; this row's midstream evidence is the
  org-vocabulary pin (cell 10).
- Midstream revocation (`OrgAdmissionDeniedError` as a stream terminal)
  at the SDK surface — witnessed at the Rust facade (S3) and the Node
  opening-refusal leg; not executed here.
- `callExported` (the subnet-exported call) through the SDK facade —
  forwarded drop-in, but its round trip belongs to the subnet estate;
  not executed here.
- The handler-DROP behavior itself (a retire supervisor dropping a
  handler future without a final poll) — this lane's F-S3.1-2 obligation
  is the named DOCUMENTATION level at the SDK's handler surfaces
  (source-established, delivered); the behavior is witnessed at the core
  and Python rows.
- The raw-`NetMesh` pass-through arm of `nativeMesh` — the consumer
  program exercises the `MeshNode` arm (the SDK's handle) end-to-end;
  the pass-through is source-established.
- A consumer run from an `npm pack` tarball (the release gate's
  "packaged" level) — the rows execute staged COPIES of the shipped
  `files` set emitted by `npm run build` (the "CI-built artifact"
  level), which is what the release gate's wording admits and what the
  driver rebuilds inside every cycle.
- Publishing/registry operations, `npm run test:watch`, and a
  `dataforts`-enabled runtime exercise of the rebuilt core beyond what
  the rows touch.
- Any host but this Windows workstation (Node v24.19.0); CI itself
  (branch unpushed); `ci.yml`/floor re-pins (Main's by contract).
- Every other Stage 4 row (other lanes by contract).

## CI floor note (Main pins, never a lane)

The `sdk-ts-tests` vitest floor becomes **567** (31 test files; 556 at
lane start + 11 new). REQUIRED names — all 11 in
`net/crates/net/sdk-ts/test/org_live.test.ts`:

```
consumer_program_compiles_against_the_shipped_org_surface
same_org_unary_call_and_serve
same_org_streaming_call_and_serve
same_org_client_stream_call_and_serve
same_org_duplex_call_and_serve
granted_unary_call_and_serve
granted_streaming_call_and_serve
granted_client_stream_call_and_serve
granted_duplex_call_and_serve
midstream_outcomes_surface_an_org_error_never_a_false_clean_eof
closing_client_and_serve_handle_lets_both_meshes_shut_down_cleanly
```

## F16 file ledger (final, sha256)

| file | lines | bytes | sha256 |
|---|---|---|---|
| `net/crates/net/sdk-ts/src/org/index.ts` | 487 | 17087 | `ad3ce24a22198b96d5510e51cd943659e0c3d341194ebeb9cd4019a78cdeb41c` |
| `net/crates/net/sdk-ts/src/index.ts` | 429 | 9258 | `04a9363bef186c3e91c820de9e34e2e47afc935eda921f2d8830eafda79bfa5a` |
| `net/crates/net/sdk-ts/package.json` | 60 | 1464 | `35a0d4809ee7daa399b983d0bb444f923040c2a91740f0b5e15f390fc7bdfb0d` |
| `net/crates/net/sdk-ts/test/org_live.test.ts` | 232 | 8704 | `71614289c4fa14dc220eec1300f2be824239e100e7d41e97c5d7ed76c083dba9` |
| `net/crates/net/sdk-ts/test/org_consumer/org_streaming_consumer.ts` | 782 | 26498 | `8f5ed365e0a25db8caae8b10490462d38e83c891ad34ad9e5fa686e48deb343b` |
| `net/crates/net/sdk-ts/test/org_consumer/tsconfig.json` | 23 | 797 | `c67c06fd93c2492b45d33b974e582fbc86fc3c1239f735222202c3d14529b35d` |

## Disclosure (durable evidence + scratch)

Run evidence on disk at the repo root (untracked, alongside the
session's other `scr_*` logs): `scr_tssdk_junit.xml` (green estate run),
`scr_tssdk_mut_junit.xml` (the applied-RED run),
`scr_tssdk_restore_junit.xml` (the restored-GREEN run),
`scr_tssdk_full_junit.xml` (the full-suite acceptance run). `npm install`
generated `net/crates/net/sdk-ts/node_modules/` +
`package-lock.json`; per `ci.yml:6416-6417` the repo ignores
package-lock.json ("no committed lockfile (same as sdk-ts)") — left
untracked, not staged. No disk-pressure episode this lane.
