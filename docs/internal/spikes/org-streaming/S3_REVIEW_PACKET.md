# Stage 3 (rows 3.1–3.3 + Exit) independent review packet

**Reviewer:** S3Review (independent; HOLD authority, no edit authority).
**Reviewed head (pinned):** `2225de011d75d6d5f9d57996530c887152368d08` on `LZL0/org-streaming`
(session tree `C:/Users/chief/orca/workspaces/net/org-streaming`, clean at review
start). **Base:** `0071fd1fc` (S3_BRIEF pin; Stage 2 accepted at `017e7148a`; S2R
closure `1d26bc4ba`/`636d80d69`). **Date:** 2026-09-23.
**Probe worktree:** `C:/Users/chief/orca/workspaces/net/org-streaming-s3rev`
(detached `2225de011`, own `CARGO_TARGET_DIR = <worktree>/net/crates/net/target`).
**Env disclosure:** all runs carried `CARGO_PROFILE_DEV_DEBUG=0
CARGO_PROFILE_TEST_DEBUG=0` (debug-info level only; every command line below is the
plan's named command verbatim; `#[track_caller]` panic locations are unaffected).
**Numbering convention:** quoted panics are pristine-at-`2225de011` test-file
numbering (every mutation's test-file delta is 0) unless marked under-append (my
temporary probe block appended 275 lines at EOF before removal).

## 1. Verdict: **ACCEPT** — with three findings (P2, P2, P3), no production defect reproduced

This is not a CI-only question and there is no production hold: the estate
reproduces exactly at the pinned head in a fresh worktree, every named witness is
a discriminating instrument (four-weakenings: NONE against any landed witness),
the §4.3 verb table holds verbatim, the frozen type list gains nothing, and the
F-S3.2-1 carve is exactly what the ruling authorized. The three findings are all
**witness-coverage gaps** (executed green-under-inverse receipts; the properties
themselves are correct today) and none blocks Stage 4. The one executed RED in
this review — my F-S3.1-2 probe — demonstrates §2.2's documented handler-drop
contract and is the adjudication instrument, not a defect (§6.1).

**Preserved credit (do not rework):** (1) the ten witnesses and their fixture;
(2) the one-plan-per-call pin discipline (`streaming_opening` + `PinnedOpening`);
(3) the `from_raw` seam exactly as ruled; (4) `map_rpc_error`/`plan()` reuse and
the error vocabulary (unchanged); (5) the probe's four pin functions and the
MANIFEST discipline; (6) the docs band's verb set + deadline rule text;
(7) the lane's F-S3.1-2 disclosure — accurate, and now EXECUTED (§6.1).
Architecture-level things I am NOT asking for: no rewrite, no new framework, no
core change, no scope expansion into Stage 4's rows, and no re-litigation of the
owner-declined F-S1R-2 rider.

Acceptance is not merge authorization; it clears the Stage-4 gate.

## 2. Reproduce ledger (executed at `2225de011`, `--no-tests=fail --retries 0`)

All commands from `net/crates/net/` in the probe worktree unless stated.

| Leg | Command | Result |
|---|---|---|
| org_streaming (the plan's named command) | `cargo nextest run --no-fail-fast --no-tests=fail --retries 0 -p net-mesh-sdk --features "net cortex dataforts testing compute nat-traversal port-mapping aggregator tool macros fixtures" --test org_streaming` | **10/10, exit 0** |
| SDK org estate | same base with `--lib --test org_exact_sensing` | **338/338, exit 0** |
| org_rpc_streaming | `cargo tf --retries 0 --test org_rpc_streaming` | **42/42, exit 0** |
| fmt | `cargo fmt -p net-mesh-sdk -- --check` | **exit 0** |
| probe, final head | `cd guards/org_api_probe && cargo metadata --locked --format-version 1` / `cargo check --locked` | **exit 0 / 0** |
| probe, frozen pin UNCHANGED (the lane's no-public-change claim) | probe tree as-at `c69d69761` (pre-S3.3 pins), same two commands; sdk+core byte-identical `c69d69761..2225de011` (empty `git diff --stat`) | **exit 0 / 0** |
| CI gate block (source-audit) | `.github/workflows/ci.yml:2427-2443` | reads `--suite org_streaming --min 10` + the **ten names verbatim** + `--self-test` fail-closed checker; `run_binary org_rpc_streaming 42` (`ci.yml:1743`) unchanged |

Roster from source (`#[tokio::test]` fns in `sdk/tests/org_streaming.rs`) == the
executed roster (set-equal; 10 names), CI's ten names == both.

## 3. Row-by-row audit and the four-weakenings check

**Weakening check (precondition fixed / window widened / assertion relaxed /
witness deleted) per witness: NONE applies to any landed Stage-3 witness.** All
ten are first-landing instruments (no earlier landed red→green history exists).
The §6.1 development disclosures — the drop witness's instrument swap
(handler-counter removed as an unsound instrument, replaced by direct
observables) and one pre-landing production-sample window extension — were
pre-landing restructures that **strengthened** the discriminator (the window now
requires the sibling's counter to move in the same span); nothing landed was
widened, relaxed, or deleted.

### 3.1 Row 3.1 — caller verbs: **MET** (8/8 named witnesses)

- `live_same_org_streaming_through_the_facade` — exact two-item identity
  (`served_by`-tagged payloads, order + multiplicity), `ran == 1`, full verified
  attribution, `last_selected_provider` = the planned provider.
- `live_same_org_client_stream_through_the_facade` (**the pin witness**) — exact
  `UploadSummary` identity (`chunks: 2, seen: [10, 20]` — ordered, with
  multiplicity — `served_by` = the planned provider) + `low.ran == 0` (the
  mid-call arrival captures nothing) + `high.ran == 1` + one-plan check. The
  scenario is genuinely adversarial: `by_entity_order` guarantees the late
  provider wins every fresh selection.
- `live_same_org_duplex_through_the_facade` — exact echo identity/order + one-run
  + attribution + one-plan.
- `live_cross_org_{streaming,client_stream,duplex}_through_the_facade` — the same
  exact-payload discipline on the granted plane with four-party attribution
  (S acted for A under B's grant on exact P); the core seams
  (`serve_rpc_granted_*`) carry the provider side at this row (correct at 3.1 —
  the facade rows did not exist yet; see §5/§8 F-S3R-1).
- `facade_stream_against_unary_only_provider_is_not_supported` — exact variant
  (`AdmissionDenied(NotSupported)`), terminal-ness (`next() == None` after), and
  the correlated zero (`ran == 0` after an observed admission refusal = no
  fallback shape, no retry; the refusal itself is observed, so this is not a
  post-hoc zero).
- `dropping_org_stream_emits_one_cancel` — precondition `in_flight == 2` asserted
  before the drop; the named transition 2→1 (both "stays 2" — no cancel — and
  "goes 0" — over-cancel — redden it); emission frozen **accompanied** by the
  sibling's counter moving in the same window (flat-while-live, exactly as
  witness discipline demands); the sibling's next item compared by exact
  identity; the count re-asserted ("exact, not a moment").

### 3.2 Row 3.2 — provider verbs: **MET** (2/2 named witnesses)

- `handler_receives_verified_org_caller_not_origin` — all three facade rows
  (`serve_org_streaming`/`_client_stream`/`_duplex`) round-trip with the
  handler's `OrgCaller` asserted field-by-field (five exact fields, `entity` =
  the ed25519 EntityId — the type cannot hold the u64 routing origin, so "not
  origin" is structural); three separate booleans asserted (no `a || b`).
- `revocation_surfaces_as_final_admission_denied_item` — a real floor raise
  through the provider's installed store (membership gen 1 → floor 2), raised
  mid-stream with the stream observed live first (precondition), the FINAL item
  exactly `Err(AdmissionDenied(Denied))` (the frozen `Revoked → Denied` byte),
  then end; `ran == 1` (retired, not zombie).

### 3.3 Row 3.3 — docs + probe: **MET**

The probe's own green build at its exact commands is the witness (exit 0/0 at
final head, exit 0/0 at its pre-S3.3 state). MANIFEST grows by exactly the 22 new
pins (7 call rows + seams, 6 serve verbs, 3 node seams, 6 wrapping types) with
zero modifications; `main.rs` +196/−5 where the five deletions are four `use`
reflows + the `unrun` array-size line (`7`→`11`) — **no exhaustive-match arm
deleted** (`pin_org_sdk_error`, `pin_org_access`, `pin_org_handler_error`,
`pin_coarse_admission_reason`, `pin_admission_denied`, `pin_org_admission` and
the `#[non_exhaustive]` fallback arm untouched — diff-verified). The four new
pin functions reference each verb as a value AND apply it with fully annotated
`OrgCaller`-first closures, pinning the wrapper item vocabulary
(`Result<_, OrgSdkError>`) by annotated `next()` awaits. Docs band
(`ORGANIZATIONS.md` "The verbs") carries the full verb set, the per-handle error
vocabulary, and the deadline rule verbatim (Owner Q1: `deadline_ms == 0` ⇒
facade default 300 s, never "no deadline"; `cancel_token == 0` ⇒ uncancellable).

## 4. The §4.3 verb-table audit, line by line — **HOLDS VERBATIM**

Every row of the plan's table against the landed `sdk/src/org/**`
(pristine-at-`2225de011` line numbers):

| §4.3 row | Landed | Site |
|---|---|---|
| `OrgClient::call<Req,Resp>(service, &Req) -> Result<Resp, OrgSdkError>` | unchanged | `call.rs:440` |
| `call_streaming<Req,Resp>(service, &Req) -> Result<OrgStream<Resp>, OrgSdkError>`; `OrgStream<Resp>: Stream<Item = Result<Resp, OrgSdkError>>` wraps `RpcStreamTyped` | exact | `call.rs:704`, struct `call.rs:205` (`inner: RpcStreamTyped<Resp>`) |
| `call_streaming_bytes -> OrgStreamRaw` | exact | `call.rs:730`, struct `call.rs:232` (`inner: RpcStream`) |
| `call_client_stream<Req,Resp>(service) -> Result<OrgClientStreamCall<Req,Resp>, _>` (`send(&Req)`, `finish(self) -> Result<Resp,_>`) | exact | `call.rs:780`, struct `call.rs:281`; `send`/`finish` at `:311`/`:327` |
| `call_duplex<Req,Resp>(service) -> Result<OrgDuplexCall<Req,Resp>, _>` (`send`, `finish_sending`, `into_split`, `Stream`) | exact | `call.rs:825`, struct `:344`, halves `OrgDuplexSink`/`OrgDuplexStream` (`:390`/`:408`) wrapping the typed halves the PUBLIC `DuplexCallTyped::into_split` returns |
| `#[doc(hidden)] call_bytes_deadline(.., deadline_ms, cancel_token)` | pre-existing, unchanged | `call.rs:503` |
| `call_streaming_bytes_deadline`, `call_client_stream_bytes_deadline`, `call_duplex_bytes_deadline`; `deadline_ms == 0` ⇒ facade default from Q1, never "none" | exact; `streaming_options` maps 0 ⇒ `DEFAULT_LIFETIME_MS = 300_000` and always sets `Some(..)` | `call.rs:756`/`:806`/`:843`; `call.rs:169`, `:175-186` |
| `Mesh::serve_org(service, OrgAccess, Fn(OrgCaller, Req) -> Fut<Result<Resp,String>>)` | unchanged | `serve.rs:172` |
| `serve_org_streaming(.., Fn(OrgCaller, Req, ResponseSinkTyped<Resp>) -> Fut<Result<(),String>>)` | exact | `serve.rs:244` |
| `serve_org_client_stream(.., Fn(OrgCaller, RequestStreamTyped<Req>) -> Fut<Result<Resp,String>>)` | exact | `serve.rs:304` |
| `serve_org_duplex(.., Fn(OrgCaller, RequestStreamTyped<Req>, ResponseSinkTyped<Resp>) -> Fut<Result<(),String>>)` | exact | `serve.rs:360` |
| `serve_org_bytes` / `serve_org_bytes_node` → `serve_org_{streaming,client_stream,duplex}_bytes(_node)` | all six landed over the same layering | `serve.rs:223`/`:422`, `:287`/`:633`, `:343`/`:658`, `:397`/`:683` |
| `OrgCaller` is the handler-facing type for all shapes (unchanged) | `Fn(OrgCaller, ..)` first in every row; one projection (`project_caller`, `serve.rs:543`) from the verified `Admitted`; `None` = loud invariant refusal, never fabricated attribution | `serve.rs:107` (`From<&Admitted>`), `:543` |
| `OrgSdkError` gains no variant; opening denial `AdmissionDenied(coarse)`; midstream retirement = final `Err(AdmissionDenied(Denied))` / `Err(Rpc(Timeout))`/`Err(Rpc(Cancelled))` | error.rs **byte-untouched** (4 variants); wrappers map every surface through the unary `map_rpc_error` (`call.rs`, `admission_reason_of` coarse-byte decode) | diff-empty `error.rs` |
| `CallOptions` gains nothing | core untouched | diff-empty |
| The provider PINNED for the call | ONE `plan()` per call in `streaming_opening` (`call.rs:672`); CS defers the open against `PinnedOpening` (`call.rs:263`), never re-resolving | — |

**Frozen type list — audited untouched (gains NOTHING):**
`OrgCaller`/`OrgAccess`/`OrgHandlerError` (`serve.rs:67-170` — zero diff hunks
between the import block and `impl Mesh`; the only serve.rs deletions are import
reflows, `Future` path simplifications, and the unary projection block refactored
into `project_caller` with the same refusal code and message); `OrgSdkError` +
domains (`error.rs` diff-empty); `OrgProofIntent` (`types.rs` diff-empty — a core
re-export, and core is diff-empty); `CoarseAdmissionReason` (core re-export);
`CallOptions` (core). `OrgClient` (`client.rs`) is diff-empty over the stage (the
S3.1 temporary `typed` shim added and removed in S3.2 nets out).

**Typed veneer public shapes — untouched.** `sdk/src/mesh_rpc.rs` over
`0071fd1fc..2225de011` is **+64/−0**: five `pub(crate) fn from_raw` constructors
and their doc comments; zero deleted lines (a visibility move would appear as a
−/+ pair). Struct definitions byte-identical.

**Probe-pin-unchanged claim — reproduced (twice):** the probe as-at `c69d69761`
(its pins pre-dating S3.3) compiles against the landed crate (`metadata --locked`
0, `check --locked` 0 — the exact commands the ruling names); the probe at final
head: 0/0.

## 5. Stage 3 Exit — the four-shape × authority-mode matrix (per cell)

"Real Rust caller/provider through the public facade for all four shapes (unary +
the three), same-org and granted, with revocation/cancellation/ownership
witnesses; the probe catches unary/public API breakage."

| Shape | same-org caller | same-org provider | granted caller | granted provider |
|---|---|---|---|---|
| unary | ✅ `live_same_org_call_through_the_facade` (tests_live, in the 338) | ✅ same (`serve_org`) | ✅ `live_cross_org_call_through_the_facade` | ✅ same (`serve_org(Granted)`) |
| server-streaming | ✅ w1 | ✅ w9 (`serve_org_streaming`) | ✅ w4 | ⚠️ **probe-only** (w4's provider rides the core seam) |
| client-streaming | ✅ w2 | ✅ w9 | ✅ w5 | ⚠️ **probe-only** |
| duplex | ✅ w3 | ✅ w9 | ✅ w6 | ⚠️ **probe-only** |

13 of 16 cells hold on landed evidence. The three ⚠️ cells (provider through the
FACADE verbs × granted × streaming shapes) are **executed GREEN by this review's
probe** (`temp_s3rev_granted_facade_rows`: all three `serve_org_*(.., Granted, ..)`
rows round-trip through a real cross-org grant) but are pinned by no landed
witness — **F-S3R-1**. Revocation (w10), cancellation/ownership (w8) and the
probe's unary/public breakage catch (row 3.3) hold. Verdict on the Exit as
written: **holds on landed evidence in 13/16 cells; the remaining 3 are
capability-proven (executed green) but unwitnessed — non-blocking, filed as
F-S3R-1 with a closure property.**

## 6. Adjudications

### 6.1 F-S3.1-2 — the protected handler is not a sound cancel-observer: **DOCUMENTED LIMITATION (not a defect); Stage-4-named rider**

**My probe (executed).** Temporary probe `temp_s3rev_f312_cancel_observer`
(275-line appended block, removed after; sha-restored) drives BOTH arms with one
instrument (`CancelObserver`: emit one item, then `ctx.cancellation.cancelled().await`,
then set `observed` — an observation possible only if the future is polled after
cancellation):

1. **CONTROL (public path, same instrument): PASS** — after dropping the caller's
   stream the public handler DOES observe (`rpc_streaming_drop_cancels_handler`'s
   mechanism; the assertion "the PUBLIC handler observes" passed in-run).
2. **Retirement observable: PASS** — the drop's cancel DID fire: the provider's
   fold `in_flight_keys` drained to 0 (asserted) before the observation check.
3. **CLAIM (protected path): EXECUTED RED** — exit **100** at
   `sdk\tests\org_streaming.rs:1688:5` (under-append numbering), verbatim:
   `S3Review probe F-S3.1-2: the PROTECTED handler must observe ctx.cancellation
   after the caller drops the stream — expected RED: the retire supervisor drops
   the handler future without a final poll`.

So: token signaled and record retired, yet the owned handler future never
resumed — the drop-without-final-poll is **executed**, upgrading the lane's
source-established claim.

**Mechanism (source-established).** `cortex/rpc.rs:5856-5945`
(`run_stream_call_supervisor`): the `select!` loop breaks on the retire arm; only
then does `cancel_token.cancel()` run (the loop can never poll `handler_fut`
again). The future is stack-pinned in the supervisor's scope and dropped at scope
end — the source comment at `:5932-5934` is verbatim what the lane quoted ("The
owned handler future drops at scope end"). This is structural, not racy: on a
`forced` retirement a protected handler can NEVER observe through
`cancelled().await` in its own future. A detached task holding a cloned token can
observe (the token is signaled). The client-streaming fold carries the same
comment (`:6142`).

**Contract check.** §2.2's table promises "cancellation signaled and the owned
future dropped" — no final poll is promised and the drop is explicit;
`ORGANIZATIONS.md`'s verb band documents caller-side vocabulary only ("Dropping a
stream handle emits exactly one CANCEL"); and the §4.3 facade handler signatures
(`Fn(OrgCaller, Req, ResponseSinkTyped<Resp>) -> ..`) carry **no** context or
cancellation token at all — a facade consumer cannot express an in-future
observer by construction. Nothing in the frozen contract is violated.

**Verdict: documented limitation** — the F-S1R-2 class (a documented behaviour
fact, disclosed rather than fixed), **not a defect** and not a hold. It earns one
**Stage-4-named rider**: no binding may promise handler-future cancellation
observation on protected streams (only token-observation from detached watchers);
in particular the Stage-4 Python row's `task.cancel()` propagation witness must
target caller-side `Rpc(Cancelled)`/stream-terminal semantics, never handler
resumption. Optional (not filed): one clarifying line in `ORGANIZATIONS.md`'s
verb band would preempt the question for external readers.

**Sub-question — `dropping_org_stream_emits_one_cancel`'s observation level vs
§2.8/§4.3: MATCHES.** §2.8's promise is "one logical terminal, not exactly one
datagram". The witness observes at the provider's own fold (a system-owned
observable, not the sender's claim): exactly one call's record retired, once, and
only that call (2→1 and stays 1 — both "no cancel" and "over-cancel" redden it),
its emission frozen while the sibling keeps producing and delivering in the same
window. The lane's stated limit — a DUPLICATE wire CANCEL frame is
indistinguishable at these seams — falls outside §2.8's promise by its own text.
§4.3's "drop = one CANCEL" is the handle-drop contract at exactly this logical
level. The observation level is correct and the limit is honestly stated.

### 6.2 F-S3.2-1 ruling compliance — **COMPLETE** (the five `pub(crate)` `from_raw`, constructors-only, zero public change)

1. **`pub(crate)` ceiling, no field visibility moved (TRUE).** `sdk/src/mesh_rpc.rs`
   over `0071fd1fc..2225de011` = **+64/−0** (raw diff retained): five hunks, each
   adding one `impl { pub(crate) fn from_raw(inner, codec) -> Self { Self { … } } }`.
   Zero deletions ⇒ no existing line changed ⇒ no visibility move, no reshaping.
   Fields remain private (`inner`/`codec`/`done`/`seen_first`/`_req`/`_resp`).
2. **Constructors only, exactly the five types (TRUE).** `RpcStreamTyped`,
   `ClientStreamCallTyped`, `DuplexCallTyped`, `RequestStreamTyped`,
   `ResponseSinkTyped` — signatures verbatim as the ruling recorded. Bodies are
   pure struct init; the init values match the pre-existing private adapters'
   construction byte-for-byte (`done: false, seen_first: false` at
   `mesh_rpc.rs:1507-1513`/`:1561-1567` vs `from_raw` `:1114-1121`). No
   behaviour change anywhere. No sixth type: `DuplexSinkTyped`/`DuplexStreamTyped`
   are unchanged (`OrgDuplexCall::into_split` wraps the halves the PUBLIC
   `DuplexCallTyped::into_split` returns — the ruling's condition 5 was never
   needed, as recorded).
3. **The record names them and the ruling (TRUE).** `S1_REPORT.md` §6.2 quotes
   all five signatures and Main's conditions 1–5.
4. **The probe's frozen-surface pin passes UNCHANGED (TRUE; reproduced).** Probe
   tree as-at `c69d69761` (pins untouched by the from_raw landing) against the
   landed sdk: `cargo metadata --locked` **exit 0**, `cargo check --locked`
   **exit 0** (sdk+core byte-identical `c69d69761..2225de011` — empty diff
   verified). The probe's own diff over the stage: MANIFEST **+22/−0** (pins
   added, none modified), `main.rs` +196/−5 (the five deletions = four `use`
   reflows + `unrun` array size `7`→`11`), `Cargo.toml` +8 (`futures` edge) —
   **no exhaustive-match arm deleted**.
5. **Zero public-API change (TRUE).** Both probe states compile the pre-existing
   pins unchanged; the typed veneer's public shapes are diff-untouched; the five
   constructors are invisible outside the crate.

## 7. Inverse receipts (raw; all in the probe worktree)

Cycle discipline: bounded diff at the PRODUCTION site → run (named red verbatim;
a compile error is never a red) → `git checkout --` restore + sha256 ==
pristine baseline → restored green. Pristine sha256 baselines:
`call.rs 04b756081d6997fd5fa6dc4788061752ee8cb9c14fb6f38da800e2fb21933c29`,
`serve.rs e1f7083cbbbbef81b8e3fc8e5fa2b46d638226304b3fc59e7946db6cefdb03ee`,
`tests/org_streaming.rs 8190f6fe05aade476eb9c23ccc66ad35d1161c3978b78eb96b07e3320c039937`,
core `mesh_rpc.rs 797e2954d6a9134748d31dacd0a7e79e767819ae1c5863ec7e78bff11ed7ce6c`,
`cortex/rpc.rs b6b161cd355fe9c350cd41e65577831103a81381272af57cf90d0d2cb1a525f3`
(never mutated). Four weakenings: **NONE** applies to any cycle below.

### 7.1 R-S3.1-pin — the REQUIRED 3.1 receipt, re-run (RED, verbatim)

Mutation (`sdk/src/org/call.rs`, `OrgClientStreamCall::send`, +6/−0 — the brief's
verbatim inverse: resolve a SECOND provider mid-call):

```diff
     pub async fn send(&mut self, value: &Req) -> Result<(), OrgSdkError> {
+        // S3Review PIN MUTATION: resolve a SECOND provider mid-call.
+        let (fresh_provider, fresh_opening) =
+            self.pinned.client.streaming_opening(&self.pinned.service, 0, 0)?;
+        self.pinned.provider = fresh_provider;
+        self.pinned.opening = fresh_opening;
+        self.inner = None;
         self.ensure_opened().await?;
```

Run: the plan's named command narrowed to
`-E 'test(=live_same_org_client_stream_through_the_facade) or test(=live_same_org_streaming_through_the_facade)'`
— **exit 100**. The control `live_same_org_streaming_through_the_facade` **PASS**
(the mutation is narrow). The named pin assertion reds, verbatim:

```
thread 'live_same_org_client_stream_through_the_facade' (160196) panicked at sdk\tests\org_streaming.rs:632:5:
assertion `left == right` failed: the provider is pinned per call: chunk two and the terminal land on the planned provider even though a second provider resolved mid-call
  left: UploadSummary { chunks: 1, seen: [20], served_by: 1976789834423662755 }
 right: UploadSummary { chunks: 2, seen: [10, 20], served_by: 1891797982238917372 }
```

(test-file delta 0 ⇒ pristine-at-`2225de011` numbering. The lane's §6.1 receipt
quotes `619:5` — correct at its stated `d63e9c615` basis: S3.2's test-file edit
inserts 13 lines ahead of the assertion (hunks `@@ -24 +1`, `@@ -35 +1`,
`@@ -298 +11`), landing it at 632:5 today. Both quotes correct at their stated
bases; the assertion text is identical. My earlier suspicion of a stale quote is
retired.) Restore: `call.rs` sha == `04b75608…`; restored run **exit 0, 1/1**.

### 7.2 Own-design witness inverses (≥2 required; 3 run)

- **M2 — `dropping_org_stream_emits_one_cancel`'s own inverse (RED).** Production
  site: core `impl Drop for RpcStream` (`mesh_rpc.rs:2299`) — the wire CANCEL
  publish replaced (bounded, +2/−5):
  `spawn_cancel_publish(...)` → `let _m2_never = (...)` (same values, no
  publish). Run `-E 'test(=dropping_org_stream_emits_one_cancel) or
  test(=live_same_org_streaming_through_the_facade)'` — **exit 100**; control
  **PASS**; red at `sdk\tests\org_streaming.rs:1233:5`, verbatim:
  `assertion left == right failed: dropping_org_stream_emits_one_cancel: the
  drop's one cancel retires exactly the one dropped call at the provider (its
  in-flight record leaves; the sibling's stays)` — `left: 2`, `right: 1`. Restore
  sha == `797e2954…`; restored **exit 0, 1/1**.
- **M3 — the NotSupported witness's own inverse (RED).** Production site:
  `map_rpc_error`'s coarse decode (`sdk/src/org/call.rs`, +1/−0) — shadow
  `let coarse = CoarseAdmissionReason::Denied;` (the coarse byte ignored). Run
  `-E 'test(=facade_stream_against_unary_only_provider_is_not_supported) or
  test(=revocation_surfaces_as_final_admission_denied_item)'` — **exit 100**; the
  control (which wants `Denied`) **PASS** (perfectly narrow); red at
  `sdk\tests\org_streaming.rs:1081:18`, verbatim:
  `a facade stream against a unary-only provider must end as
  AdmissionDenied(NotSupported); got Some(Err(AdmissionDenied(Denied)))`. Restore
  sha == `04b75608…`; restored **exit 0, 1/1**.
- **M4 — the projection witness's own inverse (RED).** Production site:
  `impl From<&Admitted> for OrgCaller` (`serve.rs:110`, +1/−1) — `entity:
  a.caller.clone()` → `a.provider.clone()` (one verified field corrupted). Run
  `-E 'test(=handler_receives_verified_org_caller_not_origin) or
  test(=live_same_org_streaming_through_the_facade)'` — **exit 100**; the control
  (core-seam attribution) **PASS**; red at `sdk\tests\org_streaming.rs:1433:5`,
  verbatim: `handler_receives_verified_org_caller_not_origin: the streaming
  handler's OrgCaller is the admission-verified five-field projection (the
  ed25519 entity id — never the caller_origin routing hash)`. Restore sha ==
  `e1f7083c…`; restored **exit 0, 1/1**.

### 7.3 Fresh lane-unused inverses (≥3 required; 3 run — the lane ran only the pin receipt)

- **M5 — the Q1 default deadline (`deadline_ms == 0` ⇒ 300 s): GREEN EVERYWHERE —
  FINDING F-S3R-2.** Production site: `streaming_options` (`call.rs:175-186`,
  +5/−1) — the `deadline_ms == 0` case now sets NO deadline (the §4.3-forbidden
  "none"). Run: the full ten — **exit 0, 10/10**; the SDK org estate — **exit 0,
  338/338**. The entire estate is green under the forbidden semantics. Weakening:
  **NONE of the four applies — this is a coverage gap** (no witness observes the
  facade default; nothing could redden). Restore sha == `04b75608…`.
- **M6 — the cross-org granted facade path: LANDED GREEN / PROBE RED — FINDING
  F-S3R-1.** Production site: the three `OrgAccess::Granted` dispatch arms
  (`serve.rs:650`, `:675`, `:700`, +3/−3) mapped to `serve_rpc_owner_scoped_*`
  instead of `serve_rpc_granted_*`. Run (a): my probe
  `temp_s3rev_granted_facade_rows` — **exit 100**, red at
  `sdk\tests\org_streaming.rs:1774:9` (under-append numbering): the granted
  admission fails at `granted streaming admitted through the facade`. Run (b):
  the landed ten — **exit 0, 10/10** (nothing in the landed estate can see the
  arm-breaking mutation). Control: the same probe at pristine production code —
  **exit 0, 1/1** (all three `serve_org_*(.., Granted, ..)` rows round-trip:
  exact payloads through a real cross-org grant). Weakening: **NONE of the four —
  coverage gap**. Restore sha == `e1f7083c…`.
- **M8 — `OrgStreamRaw`'s error mapping: GREEN EVERYWHERE — FINDING F-S3R-3.**
  Production site: `OrgStreamRaw::poll_next`'s `Err` arm (`call.rs:239-241`,
  +1/−3) — errors swallowed to `Ready(None)` (a false clean end). Run: the full
  ten — **exit 0, 10/10**. The public §4.3 row `call_streaming_bytes`/`OrgStreamRaw`
  is behaviourally unexecuted (compile-pinned only). Weakening: **NONE —
  coverage gap**. Restore sha == `04b75608…`.

### 7.4 The two adjudication probes (temporarily installed, run, removed)

- `temp_s3rev_f312_cancel_observer` — §6.1: control PASS in-run, retirement
  observable PASS in-run, claim **exit 100** at `:1688:5` (under-append).
- `temp_s3rev_granted_facade_rows` — §5/§7.3: pristine **exit 0** (the capability
  is real); under M6 **exit 100** at `:1774:9` (the probe discriminates).
- Both removed; `tests/org_streaming.rs` restored to 1550 lines, sha ==
  `8190f6fe…`.

## 8. Findings classification

All three: **P2/P3 witness-gap class** (the properties are correct at the pinned
head; what is missing is the instrument that would notice a regression). None
reproduces misbehaviour; none blocks.

1. **F-S3R-1 — the Granted arms of the three facade streaming serve rows are
   unexecuted (P2; executed gap).** `net/crates/net/sdk/src/org/serve.rs:648-651`
   (the streaming row's `match access`; siblings `:673-676` client-streaming and
   `:698-701` duplex). Witnesses 4–6 drive the granted provider side through the
   CORE seams; witnesses 9/10 drive the facade rows SameOrg only; the probe pins
   compile-only. Receipt: M6 (§7.3) — landed ten 10/10 GREEN under an
   arm-breaking mutation while my probe reds; probe GREEN pristine. Impact
   boundary: the facade's Granted dispatch for the three streaming rows only —
   the grant machinery and `serve_rpc_granted_*` are covered by witnesses 4–6,
   and the unary `serve_org_bytes_node` Granted arm by `tests_live.rs`'s
   `live_cross_org_call_through_the_facade`. Stage-4 note: the binding serve rows
   dispatch through these same arms. **Closure property:** a named witness in
   which a caller holding a cross-org capability grant completes a
   server-streaming call, a client-streaming upload, and a duplex exchange
   through handlers registered via `Mesh::serve_org_streaming` /
   `serve_org_client_stream` / `serve_org_duplex` with `OrgAccess::Granted`,
   asserting exact payloads and the four-party attribution; the inverse — each
   row's `OrgAccess::Granted` arm resolving to `serve_rpc_owner_scoped_*` —
   reddens that witness at its named assertion. (My probe is that witness,
   unlanded.)
2. **F-S3R-2 — the Q1 facade default deadline is unwitnessed (P2; executed
   gap).** `net/crates/net/sdk/src/org/call.rs:175-186` (`streaming_options`;
   the constant at `:169`). §4.3 binds "`deadline_ms == 0` ⇒ facade default from
   Owner Q1, never 'none'" — a VERBATIM-bound clause with zero coverage. Receipt:
   M5 (§7.3) — ten 10/10 + 338/338 GREEN under the forbidden "no deadline".
   Nuance for the repair: the property is observationally EQUIVALENT to core's
   own `deadline_ns == 0 ⇒ provider.default_live` under default configs, so a
   discriminating witness must run a provider whose `default_live` is materially
   shorter than 300 s and observe the call surviving past that shorter bound (or
   read the effective deadline directly). **Closure property:** a named witness
   in which a streaming call issued through a facade verb (which passes
   `deadline_ms == 0`) against a provider whose `default_live` is materially
   shorter than 300 s keeps delivering past that shorter bound — the facade's
   300 s lifetime in force — and the inverse (the `deadline_ms == 0` arm
   producing no deadline) reddens that witness at its named assertion.
3. **F-S3R-3 — `call_streaming_bytes`/`OrgStreamRaw` and
   `call_client_stream_bytes_deadline` are behaviourally unexecuted (P3;
   executed gap).** `net/crates/net/sdk/src/org/call.rs:232-255` (`OrgStreamRaw`;
   the CS seam at `:801-812`). The typed verbs DO execute
   `call_streaming_bytes_deadline` and `call_duplex_bytes_deadline`; the six
   serve bytes rows execute via the typed rows (except the Granted arms —
   F-S3R-1). Receipt: M8 (§7.3) — ten 10/10 GREEN with `OrgStreamRaw` swallowing
   errors to a false clean end. **Closure property:** a named witness that
   drains `OrgStreamRaw` through a midstream retirement and observes the final
   `Err(AdmissionDenied(Denied))` item (never a swallowed clean end), plus one
   drive of `call_client_stream_bytes_deadline` to a typed terminal; the inverse
   (`Ready(Some(Err(_)))` → `Ready(None)` in `OrgStreamRaw::poll_next`) reddens
   the first at its named assertion.

Recorded adjudications that are NOT findings: F-S3.1-2 (documented limitation,
§6.1) and the `619:5`/`632:5` panic-quote question (both correct at their stated
bases, §7.1).

## 9. Preserved credit — verified holds; do not rework

1. **The full estate reproduces at the pinned head** (executed): 10/10 (named
   command; roster from source == executed == CI's ten), 338/338, 42/42, fmt 0,
   probe 0/0 at head AND 0/0 at the pre-S3.3 probe state.
2. **The §4.3 verb table holds verbatim** (source-audited line by line, §4) —
   names, signatures, wrapping types, the seams' return of the existing raw
   handles, the Q1 mapping, the pin.
3. **The frozen type list gains nothing** (diff-audited) and the typed veneer's
   public shapes are untouched (+64/−0).
4. **F-S3.2-1's ruling compliance is complete** (§6.2) — the carve is exactly the
   five `pub(crate)` constructors.
5. **All ten named witnesses are genuinely discriminating** (M2/M3/M4 reddening
   at their own named assertions with narrow controls; four-weakenings NONE).
6. **The REQUIRED pin receipt reproduces verbatim** (§7.1).
7. **The lane's F-S3.1-2 disclosure is accurate and strengthened** — the
   drop-without-final-poll is now executed and structural (§6.1).

## 10. HOLD rows and closure properties

**None — nothing is held.** This ACCEPT holds no rows. The three findings'
closure properties are stated verbatim in §8.

## 11. Never executed here (complete)

- **Any host but this Windows workstation** — no Linux/macOS run; no
  `#[cfg(unix)]` leg.
- **CI itself** (branch unpushed; nobody pushes but Main). The `ci.yml` gate text
  is source-audited only; `check-witness-results.py --self-test` and every
  `run_binary`/`check-roster.py` step never ran here.
- **The `webrtc`/wasm graphs, the benches, and every binding** (Stage 4's rows by
  contract).
- **A wire-level duplicate-CANCEL-frame count** (the §6.1-stated limit,
  unchanged; no seam exposes frame counts).
- **Any feature set other than the named SDK set** for the org_streaming runs and
  the `cargo tf` alias set for `org_rpc_streaming`; the probe on any toolchain but
  this workstation's.
- **`cargo fmt`/clippy/doc beyond** `cargo fmt -p net-mesh-sdk -- --check`;
  no project-wide sweep was run (out of scope for a stage review).

## 12. Final-verification record

- **Pinned HEAD:** `2225de011d75d6d5f9d57996530c887152368d08`; all runs in
  `C:/Users/chief/orca/workspaces/net/org-streaming-s3rev` (detached; own
  `CARGO_TARGET_DIR`; session tree read-only apart from this packet).
- **Probe lifecycle:** two temporary tests appended to
  `sdk/tests/org_streaming.rs` (+275 lines), run (3 receipts), then removed via
  `git checkout --`; the file restored to 1550 lines, sha256
  `8190f6fe05aade476eb9c23ccc66ad35d1161c3978b78eb96b07e3320c039937` ==
  pristine.
- **Diff-check after the campaign:** `git status --short` empty in the probe
  worktree; all five mutation sites sha-identical to their pristine baselines
  (§7's list; `cortex/rpc.rs` never mutated).
- **Final green:** the ten witnesses re-run after the last restore — **10/10,
  exit 0** (`FINAL_TEN_EXIT=0`).
- **Branch movement:** none observed; no descendant attributed.

Main supersedes this packet on severity, counts, and CI state. Lane reports (the
`S1_REPORT.md` §6 record) are supporting argument, not execution receipts.
