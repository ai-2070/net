# Implementation Plan: Node/TS paid A2A task admission (Python parity)

**Status: PLANNED 2026-10-03** (branch `LZL0/node-a2a`), targeting the first
release after 0.39.0. Scope captured from a survey of the tree at
`2ecac31c0`; nothing below is implemented yet.

**Implements:** the one Node/TS gap left in the A2A row of the binding matrix —
**"A2A — paid task admission"** is `✓` for Rust and Python and `–` for Node/TS
(`README.md:350`, `docs/data/capabilities/event-bus.yaml:322`,
`.claude/skills/net-event-bus/bindings/coverage.md:102,146`). The Rust surface
was specified and accepted in [`A2A_PAID_ADMISSION_PLAN.md`](A2A_PAID_ADMISSION_PLAN.md)
(WS-A…WS-F); its WS-E shipped the frozen Python contract and explicitly
**deferred** Node paid serving ("no consumer; free behavior unchanged",
WS-E last bullet, and §6 non-goals). This plan lifts that deferral. It is the
same move [`NODE_DELEGATION_A2A_SDK_PLAN.md`](NODE_DELEGATION_A2A_SDK_PLAN.md)
made for free A2A: a napi marshaling layer over the one Rust lifecycle.

**The sentence:** a Node provider serves a catalog-driven A2A service that is
explicitly free or paid, gated by its `PaymentProvider`'s engine and journalled
under lifetime-exclusive ownership; a Node caller **prepares** (read-only),
**purchases** (durable, resumable, never re-quoted) and **submits** with the
evidence through its `CapabilityGateway` — every verdict decided by
`net_sdk::{mesh_a2a, a2a_journal}` and `net_payments::flow::a2a`, every
document that crosses byte-identical to what the Python binding returns.

**Why this is a port, not a flag:** the build already has everything. Node's
`default` features include `payments`, `payments-http`, `a2a` and `org`
(`bindings/node/Cargo.toml:33-53`), and CI's vitest build already passes
`payments,payments-http,delegation,a2a,org` (`ci.yml:3746`, `:3959`). What is
missing is source: no Node module calls `serve_a2a_configured`,
`describe_a2a`, `prepare_a2a`, `submit_task_paid` or `A2aCallerFlow`, and the
current `submitTask` doc comment says so in as many words
(`bindings/node/src/a2a.rs:275-281`).

---

## The gap (surveyed 2026-10-03)

Verified by reading `bindings/python/src/{a2a,a2a_paid,payment_provider,capability_gateway,lib}.rs`
against `bindings/node/src/{a2a,payment_provider,capability_gateway,org}.rs`,
`bindings/node/{Cargo.toml,errors.ts}` and `ci.yml`; the grep that confirms
Node never reaches the paid lifecycle is
`grep -rn 'serve_a2a_configured\|submit_task_paid\|A2aCallerFlow' bindings/node/src`
(no hits).

| Surface | Python (`bindings/python`) | Node/TS (`bindings/node`) |
|---|---|---|
| Free serve / submit / status / cancel | ✅ `a2a.rs` | ✅ `a2a.rs` (`serveA2a`, `submitTask` incl. `taskId`/`service`/`revision`, `taskStatus`, `cancelTask`) |
| Provider: `serve_a2a_configured` (catalog, gate, journal, principal, preflight) | ✅ `PaymentProvider.serve_a2a_configured` (`payment_provider.rs:748`) → `a2a_paid::serve_configured` (`a2a_paid.rs:372`) | ❌ |
| Provider operator queue: `a2a_unresolved` / `a2a_resolve(owner, task_id, state, generation=)` | ✅ `payment_provider.rs:784,811` (Weak store handle, `:389`) | ❌ |
| Requester raw: `describe_a2a`, `submit_task_paid` | ✅ `NetMesh` (`lib.rs:2057,2084`) | ❌ |
| Caller flow: `prepare_task` / `purchase_task` / `submit_task` | ✅ `CapabilityGateway` (`capability_gateway.rs:1186,1244,1267`) | ❌ |
| Caller operator: `a2a_attempts` / `a2a_resolve_attempt(task_id, outcome, provider_node=, generation=)` | ✅ `capability_gateway.rs:1286,~1360` | ❌ |
| Purchase store wiring: gateway `a2a_purchase_path` | ✅ ctor kwarg; `build_payment_flow` keeps a concrete `Arc<CallerPaymentFlow>` shared by invoke + A2A (`:718-790`) | ❌ ctor has no such arg; `build_payment_flow` erases to `Arc<dyn PaymentFlow>` (`capability_gateway.rs:337-375`) |
| Org-admitted principal: `set_a2a_org_caller` | ✅ `NetMesh` (`lib.rs:1737`) + gateway (`capability_gateway.rs:1309`) | ❌ — `a2a.rs` always builds `mesh_over(node, None)`; `OrgClient` exists (`org.rs:115`, `ArcSwapOption<net_sdk::org::OrgClient>`) |
| Typed errors: `PaymentRefused(message, schematic)`, `JournalOwnedElsewhere` | ✅ exceptions (`a2a.rs:338-361`, `a2a_paid.rs:38`) | ❌ — Node convention is prefix + `classifyError` (`errors.ts`) |
| Tests | ✅ `tests/test_a2a_paid.py` (29 cases, 1570 lines) | ❌ (`test/a2a.test.ts` covers free only) |

Three structural facts:

1. **No dependency or feature gap.** `payments` + `a2a` are already on in
   `default`, the release build and both CI node jobs. The Python compound
   gate (`#[cfg(all(feature = "a2a", feature = "payments"))] mod a2a_paid;`,
   `python/src/lib.rs:92`) is the template for Node's gate.
2. **The engine and the mesh are already in the right hands.** Node's
   `PaymentProvider` owns `engine: Arc<PaymentEngine>` and the served node
   (`payment_provider.rs:304-319`) — exactly what `EngineTaskAdmissionGate`
   and `serve_a2a_configured` take. Node's `CapabilityGateway` builds the same
   `CallerPaymentFlow` Python does, then erases its type; the only plumbing
   change is keeping the concrete `Arc` beside the erased one.
3. **Python's `a2a_paid.rs` is ~1,030 lines, and most of it is not
   Python-specific.** The status-JSON projections (`do_prepare`,
   `prepare_error_json`, `do_purchase`, `do_submit`, `do_attempts`,
   `attempt_row`, `quote_json`), the document parsers (`parse_prepared`,
   `parse_resolution`, `parse_generation`, owner JSON), `offer_for` and its
   `Unanswered`/`NotServed` split are pure Rust over `net_payments` and
   `net_sdk` types. Only the dict parsing, the executor/preflight callbacks and
   the exception mapping touch PyO3. Re-typing the rest in napi would make the
   JSON contract the thing that drifts — see D1.

## Doctrine (the crate's, restated at the Node edge)

- **No logic in bindings.** Free-vs-paid, preflight ordering, reservation,
  redemption, the once-only launch claim, the caller's CAS attempt table and
  every recovery path are decided in `net_sdk` / `net_payments`. The binding
  parses config, bridges two JS callbacks, and projects typed results.
- **One JSON contract, two bindings.** Every document a Node caller sees
  (`prepared`, `proof`, the `{status: …}` envelopes, attempt rows, unresolved
  rows) is byte-identical to Python's for the same state. Parity is
  structural (D1), and a vector test pins it.
- **Every handle is a complete document** (frozen in the parent plan's WS-E):
  a crashed Node caller reloads its stored `prepared` and submits; nothing is
  re-derived from a hash.
- **Non-custodial; keys never cross.** The payer is the node's mesh identity,
  borrowed in-process; real networks sign through the existing per-scheme JS
  signer callbacks (`payment_signer.rs`). Briefs, offers, quotes and signatures
  cross; key bytes do not.
- **Outcomes resolve, faults reject.** The gateway's paid verbs never throw on
  a payment outcome — `denied`/`unknown`/`unexecutable` are status JSON, as
  the existing gateway already does for `invoke` (`errors.ts:40-43`). Throws
  are reserved for caller-shape errors, transport, and the two named
  refusals (`PaymentRefused` on the raw path, `JournalOwnedElsewhere`).
- **Handle lifecycle.** The configured serve handle **is** the journal owner.
  `A2aServeHandle.stop()` releases the journal's `.owner` lock and the node
  clone; `PaymentProvider.close()` must also stop any configured serve it
  started, or `NetMesh.shutdown()` fails until GC (the `close()` gotcha).

---

## The design

### D1 — Hoist the binding-neutral projection before porting (recommended)

Move the pure half of `bindings/python/src/a2a_paid.rs` into one shared Rust
home that both bindings call, then make the Python module a thin caller of it
**in the same change** (its suite is the regression guard; no `.py` test
edits allowed).

- **Home:** `net_payments::flow::a2a::json` (feature `mesh`) — it projects
  `A2aCallerFlow`, `A2aPurchase`, `A2aSubmit`, `PurchaseAttempt`,
  `AttemptResolution`, `AttemptGeneration`, all of which live there; and
  `net-payments` already depends on `net-sdk`, so `A2aOffer` / `PreparedTask`
  / `TaskOwner` are in reach. Provider-side owner/state parsing for the
  journal operator verbs goes to `net_sdk::a2a_journal` (no payments types).
- **What moves:** `owner_to_json`/`owner_from_json`, the services catalog
  parser from a `serde_json::Value` (the Python dict path becomes
  `dict → Value → shared parser`, keeping its error strings), `offer_for`,
  `do_prepare`/`do_purchase`/`do_submit`/`do_attempts`/`do_resolve_attempt`,
  `unresolved_json`/`resolve_admission`'s non-GIL body, the three document
  parsers. Errors return a small `A2aJsonError { kind: Shape | Journal(..) |
  Flow(..), message }` each binding maps to its own exception/prefix.
- **What stays per-binding:** the executor + preflight callback bridges, the
  error mapping, dict/object argument plumbing, handle classes.
- **Why this is not scope creep:** AGENTS.md's "new binding surface lands in
  `net-mesh-sdk` first" exists because per-binding reimplementation drifted
  (the R1 incident). The alternative — a ~600-line napi re-typing of the
  JSON envelopes — makes every future change to the paid flow a two-binding
  edit with a silent-drift failure mode.

**Fallback if D1 is rejected:** port the projections into
`bindings/node/src/a2a_paid.rs` by hand and rely on the WS-5 cross-binding
vector test alone to catch drift. Costs roughly +500 napi lines and the
ongoing double edit.

### D2 — Node surface shape: typed objects in, JSON documents out

Following the free-A2A port's precedent (`TaskBriefJs`, not JSON blobs, for
inputs; "Landed" note under its WS-3):

- **Inputs are `#[napi(object)]`** where the Python surface took a dict:
  `services: Record<string, A2aServicePolicyJs>` (`revision`, `pricingTerms?`
  — the JSON string `buildPricingTerms` already returns — `bounds`,
  `reservationTtlSecs`, `reservationRetentionSecs`, `retentionSecs`,
  `description?`), and a `ServeA2aConfiguredOptions { principal?,
  preflight?, handlerTimeoutMs? }`. Seconds fields are `number` (u32 is ample;
  the `handlerTimeoutMs` precedent); bounds are `number`.
- **Outputs and round-tripped handles stay JSON strings**: `prepared`,
  `proof`, status envelopes, attempt rows. They are documents a caller
  persists and hands back verbatim; typing them on the JS side would invite
  re-serialization that breaks byte-exactness.
- **Names:** `js_name` pinned where napi's camelCase mangles `A2a`
  (`serveA2aConfigured`, `describeA2a`, `submitTaskPaid`, `a2aUnresolved`,
  `a2aResolve`, `a2aAttempts`, `a2aResolveAttempt`, `setA2aOrgCaller`).

### D3 — Executor and preflight callbacks

- **Executor:** reuse `NodeTaskExecutor` (`a2a.rs:101`) — same TSFN→Promise
  bridge, same `handlerTimeoutMs` deadline, same one-sided cancellation.
  `TaskBriefJs` gains optional `service` / `revision` fields (always `None` on
  the free path), the object-shaped analog of Python's keyword-only addition:
  an existing free handler ignores fields it never reads.
- **Paid-task timeout semantics (new, must be documented):** on the
  configured path a handler deadline records `Failed` for work that was
  **paid for**. That is an execution outcome the journal records as
  `Terminal` — not a refund and not `Interrupted`. The default stays 1 hour;
  the docs say a paid service with long jobs must raise it or pass `0`.
- **Preflight:** `(ownerJson, offerJson, briefJson) => Promise<string | null>`.
  `null`/`undefined` admits; a string refuses with that reason verbatim; a
  throw or rejection **refuses** with the error text (fail-closed, matching
  `PyPreflight`, `a2a_paid.rs:313-365`). A preflight has its own bounded
  deadline (proposed 30 s) that also refuses — a wedged event loop must not
  hold a `Preparing` reservation open.

### D4 — Errors: stable prefixes + typed classes in `errors.ts`

The Node binding throws plain `Error`s with stable prefixes that
`classifyError` maps to classes (`errors.ts:1-17`). Add:

- `a2a:payment_refused:` → `PaymentRefusedError { schematic?: string }`
  (raw `submitTaskPaid` only). The schematic is carried as a JSON tail after
  the prefix (`a2a:payment_refused:{"message":…,"schematic":…}`), parsed by
  the classifier — napi `Error` carries only a reason string.
- `a2a:journal_owned_elsewhere:` → `JournalOwnedElsewhereError`
  (`serveA2aConfigured`). Same doc text as the Python exception.
- Local refusals (`BriefTooLarge`, `ProofUndeliverable`) keep a distinct
  `a2a:invalid_argument:` prefix so they are never confused with a retryable
  transport failure (the Python `ValueError` vs `RuntimeError` split,
  `a2a.rs:348-358`).

### D5 — Gateway constructor: append, don't reshape

`new CapabilityGateway(...)` is already eleven positional optionals
(`capability_gateway.rs:528-540`). Append `a2aPurchasePath?: string` as the
twelfth. Same validation as Python: requires `paymentPolicyPath` (a purchase
store with no spend policy records payments nothing authorized). An options
object would be the nicer API but is a breaking change to every existing
caller; out of scope here, noted for the next gateway revision.

---

## The slices

A depends on nothing; B–D depend on A (or its fallback); E on B–D; F last.

### WS-A — Hoist the shared projection (D1)

- [ ] `net_payments::flow::a2a::json` (feature `mesh`) + the `a2a_journal`
  operator-JSON helpers in `net_sdk`, moved out of `python/src/a2a_paid.rs`
  with their doc comments.
- [ ] `python/src/a2a_paid.rs` reduced to dict plumbing, the two callback
  bridges, and error mapping over the shared functions.
- [ ] Rust unit tests on the shared module for each envelope status and each
  parser's refusal (these did not exist — the Python suite was the only
  witness of the shapes).

**Proved by:** `tests/test_a2a_paid.py` green **unedited**;
`cargo clippy -p net-payments --features mesh -- -D warnings` and
`cargo doc -p net-payments --no-deps --all-features` clean (the `--all-features`
requirement in AGENTS.md); `cargo doc -p net-python` with the hand-maintained
feature list clean.

### WS-B — Provider: `PaymentProvider.serveA2aConfigured` (`node/src/a2a_paid.rs`)

- [ ] New module gated `#[cfg(all(feature = "a2a", feature = "payments"))]`.
- [ ] `PaymentProvider.serveA2aConfigured(executor, services, journalPath,
  options?) => Promise<A2aServeHandle>`: async (journal `open` is file IO and
  takes the `.owner` lock; registration needs the tokio context — the
  "no reactor running" lesson from the free port), principal
  `"session_peer" | "same_org" | "granted"`, preflight bridge (D3),
  `JournalOwnedElsewhereError` (D4). Refuses if the provider is closed.
- [ ] `A2aServeHandle` grows a `Registered::Configured` arm (the Python
  enum, `python/src/a2a.rs:363-430`) and a `services` getter; `stop()`
  releases the journal owner.
- [ ] Operator verbs: `a2aUnresolved() => Promise<string>`,
  `a2aResolve(ownerJson, taskId, stateJson, generation?: bigint) =>
  Promise<void>` over a `Weak` store reference, refusing with the Python
  wording once no configured serve handle is live.
- [ ] `PaymentProvider.close()` stops a live configured serve.

**Proved by:** the provider-side cases of WS-F's `a2a_paid.test.ts` (catalog
refusals, journal ownership in-process and cross-process, stop releases the
journal, operator queue) — runnable from this slice with a raw Rust-side
caller, completed once WS-D lands; `cargo clippy -p net-node --all-targets`
clean.

### WS-C — Requester raw verbs on `NetMesh` (`node/src/a2a.rs`)

- [ ] `describeA2a(targetNodeId: bigint) => Promise<string>` (JSON
  `A2aOffer[]`; the legacy free path has no describe service and rejects).
- [ ] `submitTaskPaid(preparedJson, proofJson) => Promise<string>` — headers
  from the proof, brief from the prepared document, no store;
  `PaymentRefusedError` on refusal.
- [ ] `setA2aOrgCaller(orgClient: OrgClient | null)` — needs a crate-internal
  `OrgClient::shared()` accessor over its `ArcSwapOption`; `mesh_over` in the
  A2A verbs becomes the org-aware variant (Python's `mesh_over_as`,
  `python/src/a2a.rs:122`). Applies to describe/submit/status/cancel.
- [ ] Rewrite the `submitTask` doc comment that currently says paid A2A is out
  of scope for this binding.

**Proved by:** `submitTaskPaid` refusal cases (unpaid → `PaymentRefusedError`
with a parseable schematic; oversized brief → `a2a:invalid_argument:`) and a
`describeA2a` round-trip against a WS-B provider; existing `a2a.test.ts`
green unedited (free path unchanged).

### WS-D — Caller flow on `CapabilityGateway`

- [ ] `build_payment_flow` keeps the concrete `Arc<CallerPaymentFlow>` and,
  when `a2aPurchasePath` is set, builds the `A2aCallerFlow` over it (one flow,
  one spend store, one signer set — a budget spent on a paid tool is a budget
  spent on a paid task).
- [ ] Ctor arg `a2aPurchasePath` (D5) with the two loud refusals Python has.
- [ ] `prepareTask(targetNodeId, service, prompt, contextRefs?, tags?,
  taskId?)`, `purchaseTask(preparedJson)`, `submitTask(preparedJson)`,
  `a2aAttempts()`, `a2aResolveAttempt(taskId, outcomeJson, providerNode?,
  generationJson?)`, `setA2aOrgCaller(orgClient | null)` — all `async`,
  resolving to the shared envelopes; a gateway built without a purchase store
  rejects with the Python wording.
- [ ] `close()` drops the A2A flow with the rest of `Live`.

**Proved by:** the caller-side cases of WS-F's suite (prepare moves no money,
paid runs once, retry convergence, restart resumption, approval flow,
resolve-attempt); existing `capability_gateway.test.ts` and
`payment_provider.test.ts` green unedited (invoke path unchanged by keeping
the concrete flow `Arc`).

### WS-E — TS surface

- [ ] `index.d.ts` regenerates (gitignored); verify every new shape.
- [ ] `errors.ts`: the two classes + prefixes (D4), wired into
  `classifyError`; `errors.test.ts` cases.
- [ ] `sdk-ts`: forward `describeA2a` / `submitTaskPaid` / `setA2aOrgCaller`
  on the `mesh.ts` wrapper beside `serveA2a`/`submitTask` (`mesh.ts:1058`);
  re-export the error classes. `PaymentProvider` / `CapabilityGateway` are
  native re-exports, so their new methods arrive with the typings.

**Proved by:** `errors.test.ts` classification cases; `npm run typecheck:tests`
in `bindings/node` and the `sdk-ts` typecheck clean.

### WS-F — Tests, docs, matrix

- [ ] `bindings/node/test/a2a_paid.test.ts`: the twin of `test_a2a_paid.py`
  over the mock facilitator (`unsafeDevMockFacilitator: true`), case for case
  where the property is reachable from JS — free success through the catalog,
  price-it-cannot-enforce refusal, second provider on one journal (in-process
  **and** a child `node` process), stop releases the journal with the provider
  alive, operator queue refuses with no live handle, oversized brief refused
  before any quote, unknown service is rejected not busy, prepare moves no
  money, paid runs exactly once, retained-id retry converges, altered brief
  refused, lapsed reservation keeps its `admission_id`, unpaid submission
  refused with schematic, gate denial then valid retry, cross-caller proof
  worthless, production profile holds until approve, reject re-opens the key,
  post-payment revocation unresolved on both sides until resolved, malformed
  resolution refused, full provider restart reconciles, re-created gateway
  resumes from the store, paid verbs refuse a store-less gateway, same id on
  two providers resolvable by naming the provider, two identities sharing one
  store see only their own, generation-scoped resolve + its unknown-incarnation
  refusal. Plus Node-only: the preflight throw/timeout refuses (D3), a paid
  task past `handlerTimeoutMs` ends `failed` and is not re-run.
- [ ] **Cross-binding vector test:** drive one scripted lifecycle and assert
  the Node envelopes equal checked-in JSON fixtures that the Python suite
  asserts too (`tests/cross_lang_a2a_paid/`, the `cross_lang_*` convention).
  Under D1 this is a tripwire; under the fallback it is the only guard.
- [ ] Matrix flips to `✓` for Node/TS in all three sources (README table,
  `event-bus.yaml` with anchor `serveA2aConfigured`, skill `coverage.md`
  both tables); `web/src/content/docs/guides/agent-to-agent.md` "paid
  services" section gains the TS snippets; `.claude/skills/net-event-bus/a2a.md`
  §"Paid A2A" gains the Node prepare/purchase/submit example; release note
  under `net/crates/net/docs/releases/` mirrored via `npm run sync:releases`.
- [ ] CI: no feature-list change (already `payments,a2a,org`); the new vitest
  file is auto-discovered. If WS-F adds `tests/cross_lang_a2a_paid/` fixtures
  read by a Rust test, pin that test in `ci.yml` per the pin-guard rule.

**Proved by:** the suite itself; `npm run check` in `web/` (docs links +
releases sync); the skill-snippet checker.

---

### Decisions for the reviewer

1. **D1 (hoist) vs. hand-port.** Recommended: hoist. It is the only option
   where the Python suite keeps guarding the shapes Node returns.
2. **Preflight deadline value.** Proposed 30 s, not configurable in the first
   cut; configurable later via `ServeA2aConfiguredOptions` if a consumer
   needs it.
3. **Default `handlerTimeoutMs` on the configured path.** Keep the free path's
   1 hour (parity within Node) vs. default to `0` (parity with Python, where
   cancellation is the only control). Recommended: keep 1 hour and document
   it — a stranded paid task in `Running` forever is the worse failure.
4. **Gateway options object.** Deferred (D5).

### Test matrix (target)

| Area | Rust unit | vitest (built `.node`) | Parity source |
|---|---|---|---|
| shared envelopes + parsers | ✓ (new, WS-A) | via every case below | `test_a2a_paid.py` (unedited) |
| configured serve, catalog refusals | — | ✓ | `test_a2a_paid.py` |
| journal ownership (in-process + child process) | — | ✓ | `test_a2a_paid.py`, `a2a_admission_journal` |
| prepare → purchase → submit, once-only | — | ✓ (two-node) | `test_a2a_paid.py`, `a2a_paid_end_to_end` |
| approval / reject / restart / resolve | — | ✓ | `test_a2a_paid.py` |
| org-admitted principal | — | ✓ (if the org harness is reachable from vitest; else source-established, as in the parent plan) | `a2a_admission_identity` |
| preflight + handler deadline | — | ✓ (Node-only) | — |
| error classification | — | ✓ `errors.test.ts` | — |
| JSON parity | — | ✓ fixtures | `cross_lang_a2a_paid/` |

### Effort + sequencing

With D1: ~400 lines moved (not new) into the shared module, ~350 lines of new
napi (`a2a_paid.rs` + gateway/provider/`NetMesh` methods), ~60 lines of
`errors.ts`, one ~900-line vitest suite. Without D1: add ~500 napi lines.

Commit sequence, each compiling and green on its own: WS-A (Python suite
unedited) → WS-B → WS-C → WS-D → WS-E → WS-F. Run the node vitest suite once
after WS-D against a freshly built `.node` with the CI feature list
(`ci.yml:3746`), then the full pre-push checklist once before review.

## Risks

- **The hoist (WS-A) changes Python's behavior by accident.** An error string
  or JSON key order shifts in the move. *Fallback:* the Python suite is
  required to stay green unedited, and WS-F's fixtures are generated from the
  pre-move Python output before WS-A starts, so a drift fails both.
- **A JS executor outlives a cancel or a deadline on paid work.** The Promise
  can't be aborted (the documented one-sided cancellation). The journal still
  records the terminal state once, and the result is discarded; the risk is
  the handler's side effects, which is the application's to make cooperative.
  *Fallback:* none needed in the binding; the docs say so on the paid path.
- **Journal ownership across child processes on Windows/Linux differs** (the
  `LockFileEx` vs `flock(2)` legs; the parent plan witnessed only the former
  on its host). *Fallback:* the cross-process vitest case runs on the ubuntu
  CI node job, so the `flock` leg gets its first binding-level witness there.
- **Org-admitted principal is hard to drive from vitest** (needs an org
  authority harness). *Fallback:* witness it at the registration seam and
  mark it source-established, exactly as the parent plan did; the SDK's
  `a2a_admission_identity` test remains the live witness.
- **napi argument-count growth on the gateway constructor** (D5) becomes
  unreadable. *Fallback:* the options-object reshape is a named follow-up, not
  a blocker.

## Not in scope

- Go / C paid A2A (no free A2A there either; the matrix keeps `–`).
- Any change to the Rust lifecycle, the wire, or the parent plan's state
  tables — this is the Node layer plus a code move.
- Mesh-wide A2A offer search, metering/escrow/refunds, relay-transparent
  ownership — non-goals inherited from the parent plan §6.
- Reshaping the `CapabilityGateway` / `PaymentProvider` constructors into
  options objects.
