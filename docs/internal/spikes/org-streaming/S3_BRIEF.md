# Stage 3 — Rust organization facade

**Authorization and limits.** Authorized 2026-09-22 by the Stage 2 ACCEPT
(`S2_REVIEW_PACKET.md` @`017e7148a`) plus its S2R closure (`1d26bc4ba`,
verified) and this pinned brief. **Stage 3 as written (rows 3.1–3.3) —
nothing more.** Explicitly NOT this stage: bindings (Node/Python/Go),
browser/leaf, payments, serverless, cross-language vectors, release work,
Stage 4. The owner-declined F-S1R-2 rider remains out of scope. The core
(`mesh_rpc.rs`, `cortex/rpc.rs`, `behavior/*`, `wire/*`, `mesh.rs`) is
**frozen** — the facade rides the existing `*_bytes_deadline` seams and the
public `org` surfaces; a needed core change is a finding, STOP.

## Source of truth — read first, in this order

1. The plan's **Stage 3 table** + **§4.3** (the exact verb table: caller
   rows `call_streaming`/`call_streaming_bytes`/`call_client_stream`/
   `call_duplex` + the `*_bytes_deadline` binding seams; serve rows
   `serve_org_{streaming,client_stream,duplex}` + the `*_bytes(_node)`
   variants; `OrgStream`/`OrgStreamRaw`/`OrgClientStreamCall`/
   `OrgDuplexCall` shapes; `deadline_ms == 0` ⇒ the Q1 facade default;
   `OrgCaller`/`OrgSdkError` gain NOTHING) and §2/§3 (the semantics the
   facade surfaces: typed denials, cancellation, ownership).
2. `docs/internal/spikes/org-streaming/S1_REPORT.md` (§0 rulings; §2.1/2.3
   for the core seams the facade calls) and the two review packets (the
   evidence standard in practice).
3. `sdk/src/org/{call,serve,client,error}.rs` (the existing unary facade —
   the naming, `plan()`, `map_rpc_error`, the `OrgBytesHandler` → `OrgCaller`
   projection) and `sdk/src/org/tests_live.rs:176-273` (the fixture shape).
4. `AGENTS.md`/`TESTS.md`; the `guards/org_api_probe` doctrine (no
   exhaustive-match arm deleted; MANIFEST discipline).

## Lanes and file ownership (strict)

One lane (`S3Facade`), rows 3.1 → 3.3 **in order**, each landed green
before the next. Owns exclusively: `net/crates/net/sdk/src/org/**`,
`net/crates/net/sdk/tests/org_streaming.rs` (new),
`net/crates/net/sdk/src/tool.rs` ONLY if the §4.6 note's `ToolEvent` path
needs a facade-facing adjustment (it should not — state instead),
`net/crates/net/guards/org_api_probe/**`, `net/crates/net/docs/ORGANIZATIONS.md`
(`:124-135`). Everything else (core, other guards, `ci.yml`,
`.config/nextest.toml`, `spikes/**`, `docs/internal/**`) is Main's or
frozen. A needed change outside this set = finding, STOP.

## Frozen contracts

§4.3's verb table is binding VERBATIM (names, signatures, wrapping types).
`OrgCaller`, `OrgSdkError`, `OrgHandlerError`, `OrgAccess`,
`CoarseAdmissionReason`, `OrgProofIntent`, `CallOptions` gain NOTHING (the
plan's frozen list). The `*_bytes_deadline` seams already exist and are
frozen core — call them, do not edit them. `plan()`/`map_rpc_error` reuse
per 3.1. The recorded rulings bind: `Revoked → Denied` (the facade maps
through the frozen coarse bytes unchanged), the Q1 deadline default
(300 s when `deadline_ms == 0`), provider pinned per call.

## Slices as dispatched (full witness texts in the plan's Stage 3 table)

**3.1 Caller verbs.** Target: `sdk/src/org/{call,client,error}.rs`. Change:
the §4.3 caller rows; `plan()` reused; the provider PINNED for the call;
the facade default deadline when `deadline_ms == 0` (Q1). Witnesses (in
`sdk/tests/org_streaming.rs`, fixture from `tests_live.rs:176-273`):
`live_same_org_streaming_through_the_facade`,
`live_same_org_client_stream_through_the_facade`,
`live_same_org_duplex_through_the_facade`,
`live_cross_org_streaming_through_the_facade`,
`live_cross_org_client_stream_through_the_facade`,
`live_cross_org_duplex_through_the_facade`,
`facade_stream_against_unary_only_provider_is_not_supported`,
`dropping_org_stream_emits_one_cancel`. Inverse: resolve a second provider
mid-call → the pin witness must red.

**3.2 Provider verbs.** Target: `sdk/src/org/serve.rs`. Change: the §4.3
serve rows over `serve_org_*_bytes_node`; the `OrgCaller` projection; the
facade policy `|_| true` (the provider veto stays the caller's extension
point, as today). Witnesses: `handler_receives_verified_org_caller_not_origin`;
`revocation_surfaces_as_final_admission_denied_item`.

**3.3 Docs + probe.** Target: `docs/ORGANIZATIONS.md:124-135` (the two-verb
text → the full verb set with the deadline rule), `guards/org_api_probe`.
Change: the probe compiles the new verbs AND still the unary ones
(compile-only surface pin; MANIFEST grows by exactly the new pins; no
exhaustive-match arm deleted). Witness: the probe's own green build at its
exact commands (record it; the probe is the witness).

**Exit (plan wording):** real Rust caller/provider through the public facade
for all four shapes (unary + the three), same-org and granted, with
revocation/cancellation/ownership witnesses; the probe catches unary/public
API breakage.

## Evidence rules, CI, report (verbatim from `S1_BRIEF.md`)

Raw inverse receipts at the production site (the 3.1 pin-receipt REQUIRED;
bounded diff, exact command, exit, verbatim assertion red — never a compile
error — sha-proven restore, restored green); green-under-own-inverse =
FINDING (delete-and-replace, never re-pin; name the weakening); rosters
from source; `--retries 0 --no-tests=fail`; warm aliases (the SDK's own
feature set: `cargo nextest run -p net-mesh-sdk --features "net cortex
dataforts testing compute nat-traversal port-mapping aggregator tool macros
fixtures"` for `sdk/tests/org_streaming.rs` — the plan's named command);
`cargo fmt -p net-mesh-sdk -- --check`; executed vs source-established;
"never executed here" over inference; disk discipline. Commit prefixes
`S3.1:`…`S3.3:` (implementation + report record pairs). Append
`## 6. Stage 3` to `docs/internal/spikes/org-streaming/S1_REPORT.md` with
landed hashes, witnesses + counts, receipts, findings, never-executed.
Report the new test counts + names to Main for same-commit CI floor re-pins
(the SDK JUnit floor step pattern; Main pins). Owner-pending: none. Report
only on green at your exact head. NO stage n+1.
