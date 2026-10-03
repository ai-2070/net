# Implementation Plan: Node/TS paid A2A task admission (Python parity)

**Status: PLANNED, revision 2.3 — 2026-10-03** (r2.2 re-reviewed HOLD for bounded follow-up; S1–S4 repaired, awaiting sign-off) (branch `LZL0/node-a2a`),
targeting the first release after 0.39.0. Scope captured from a survey of the
tree at `2ecac31c0`; nothing below is implemented yet. Revision 1
(`b71d27c4a`) was reviewed **HOLD** (architecture retained); r2 repairs the
seven findings in place. Each repair is tagged **r2 (R*n*)** next to the text
it changed, and the reviewer's four decisions are adopted (§Decisions).
Awaiting re-review before implementation.

**r2.1 (2026-10-03, author decision on R7):** the reviewer offered two
repairs for R7: document the core-only boundary, or add a supported ergonomic
provider surface. r2 chose the first. r2.1 switches to the second, because
the first turned out to leave `@net-mesh/sdk` users with **no** path at all:
`MeshNode.create` is the SDK's only mesh factory, and the SDK seals the native
handle on purpose (`sdk-ts/src/_internal.ts`), so an SDK user cannot produce
the native `NetMesh` either constructor requires. That is already true of the
`CapabilityGateway` the SDK root re-exports today (`consent.ts:27-33`): it is
exported and unconstructible from an SDK mesh, a pre-existing defect this
port would otherwise inherit. See D7 and WS-E.

**r2.2 (2026-10-03): the Python SDK has the same gap, and r2.1 said
otherwise.** r2.1 claimed Python's `net_sdk` "has no payments or A2A surface",
repeating the parent plan's baseline, and that claim is stale. `net_sdk` today
forwards `submit_task_paid` / `describe_a2a` on `MeshNode`
(`sdk-py/src/net_sdk/mesh.py:592-604`) and re-exports `CapabilityGateway` /
`AsyncCapabilityGateway` (`consent.py:51-55`). But the gateway's constructor
extracts a native `net.NetMesh` (`bindings/python/src/capability_gateway.rs:1005-1008`),
so passing a `net_sdk.MeshNode` is a `TypeError`. `PaymentProvider` is not in
`net_sdk` at all, and there is no `set_a2a_org_caller` on the SDK mesh. Every
paid example therefore imports the raw `net` wheel
(`.claude/skills/net-event-bus/a2a.md:122`, `examples/a2a_paid.py:35`). It is
the TS defect in a milder form: Python has a reachable escape hatch
(`node._native`), but it is a private name with no stability promise, which is
exactly what `mesh.py:739-745` says application code must not be pushed onto.
The fix is WS-G.

**r2.3 (2026-10-03): re-review of r2.2 (`bfb474b4e`), HOLD for a bounded
follow-up.** The reviewer accepted the r2 architecture, the TS factories and
the Python handle-adaptation goal, and raised three findings plus a prose
reconciliation. All four were verified against the source and repaired in
place, tagged **r2.3 (Sn)**:

| # | Finding (r2.2) | Severity | Where repaired | Status |
|---|---|---|---|---|
| S1 | The R1 recovery witness used a pointer that does not exist (`/0/provider_node`; caller rows nest it under `key`) and treated a caller `generation` as a universal selector; the precision control needs a genuinely non-representable id | Medium | D2a, WS-F R1 bullet | repaired in r2.3 |
| S2 | WS-G conflated adapting an `AsyncMeshNode` with async paid-A2A support; the native `AsyncCapabilityGateway` has no paid verbs and refuses `a2a_purchase_path` | Medium | WS-G | repaired in r2.3 |
| S3 | An installed wheel does not make an `sdk-py` pytest live: `conftest.py` stubs `net` unconditionally | Medium | WS-G | repaired in r2.3 |
| S4 | Active prose still contradicted the revised contract (byte-identical, "read-only" prepare, D7 heading, idle-only stop/refusal wording, core-only examples, D1 fallback, sequencing without WS-G) | — | headline, D1, D7, WS-F, sequencing | repaired in r2.3 |

The reviewer's accepted boundary holds: no reopening of the Rust
payment/admission lifecycle; WS-A (with the pre-move compatibility test and
vector capture) is the coherent first implementation slice once this text is
signed off.

**Review ledger.**

| # | Finding (r1) | Severity | Where repaired | Status |
|---|---|---|---|---|
| R1 | JSON-string handles are not a lossless JS handoff: `prepared`/`proof` are nested objects in the envelope, and `provider_node` (also owner ids, generations, `expires_at_ns`) is u64 and corrupts under `JSON.parse` → `JSON.stringify` | High | D2a (new), Doctrine, WS-C/D/F | repaired in r2 |
| R2 | `stop()`/`close()` promised journal release, but launched executors and terminal writes retain the owner | High | Doctrine, D6 (new), WS-B, WS-F | repaired in r2 |
| R3 | A 30 s preflight deadline loses the race to the caller's earlier-started 30 s `A2A_CALL_TIMEOUT` | Medium | D3, WS-F | repaired in r2 |
| R4 | Catalog bounds/durations narrowed from u64 to `number`/u32 with no validation policy | Medium | D2, WS-B, WS-F | repaired in r2 |
| R5 | The catalog parser is not binding-neutral; a whole-dict `dict → Value` move changes Python input behavior | Medium | D1, WS-A | repaired in r2 |
| R6 | The source-only org fallback is unnecessary — `org_live.test.ts` already drives a live org harness | Medium | WS-C/D, WS-F, Risks | repaired in r2 |
| R7 | `PaymentProvider` is not exported from `sdk-ts`; both ctors take a native `NetMesh` | Medium | D7 (new, amended r2.1), WS-E | repaired in r2; r2.1 adds the ergonomic surface |

One defect found while checking R1 that the review did not name:
`quote.expires_at_ns` in the `prepare_task` envelope (`a2a_paid.rs:575-591`) is
a u64 nanosecond timestamp (~1.8×10¹⁸), so even the *display* fields of the
envelope lose precision under `JSON.parse`. The D2a reader covers it.

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
under lifetime-exclusive ownership; a Node caller **prepares** (no funds
move, though it does reserve capacity, obtain a quote and persist the
attempt), **purchases** (durable, resumable, never re-quoted) and **submits**
with the evidence through its `CapabilityGateway` — every verdict decided by
`net_sdk::{mesh_a2a, a2a_journal}` and `net_payments::flow::a2a`, every
document that crosses carrying the same schema and values as the Python
binding's (r2.3, S4: r1 said "byte-identical"; see Doctrine for why values,
not bytes, are the contract).

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
  rows) has the same schema and the same values as Python's for the same
  state — the frozen envelope schema is kept, with `prepared` and `proof`
  nested as objects. Parity is structural (D1), and a vector test pins it.
  **r2 (R1):** r1 said "byte-identical", which conflated two things. Output
  equality is equality of JSON *values*. Exact *bytes* matter only where
  something is signed, and those bytes never round-trip through JS: the quote
  and payment payload live base64-encoded in the purchase store
  (`quote_bytes`/`payload_bytes`), and the brief commitment is Rust's
  canonical typed JSON (parent plan D3), so outer whitespace or key order in a
  `prepared` document carries no evidence.
- **Every handle is a complete document** (frozen in the parent plan's WS-E):
  a crashed Node caller reloads its stored `prepared` and submits; nothing is
  re-derived from a hash. **r2 (R1):** in JS a handle is handed on as the
  string the D2a reader extracts, never as a `JSON.parse`d object re-serialized
  with `JSON.stringify` — that path silently rounds u64 fields above 2⁵³.
- **Non-custodial; keys never cross.** The payer is the node's mesh identity,
  borrowed in-process; real networks sign through the existing per-scheme JS
  signer callbacks (`payment_signer.rs`). Briefs, offers, quotes and signatures
  cross; key bytes do not.
- **Outcomes resolve, faults reject.** The gateway's paid verbs never throw on
  a payment outcome — `denied`/`unknown`/`unexecutable` are status JSON, as
  the existing gateway already does for `invoke` (`errors.ts:40-43`). Throws
  are reserved for caller-shape errors, transport, and the two named
  refusals (`PaymentRefused` on the raw path, `JournalOwnedElsewhere`).
- **Handle lifecycle.** ~~The configured serve handle **is** the journal
  owner; `stop()` releases the `.owner` lock.~~ **r2 (R2):** that was wrong.
  `stop()` / `PaymentProvider.close()` **retire the registration** and drop
  the binding's own references (the serve handles, the node clone, the
  binding's store reference). Journal ownership ends only when every Rust
  writer has let go. A launched executor holds the owner (`OwnedExecutor`,
  `sdk/src/mesh_a2a.rs:2363-2378`) and the terminal hook holds store + owner
  through its write (`:2555-2579`), by design, so a second owner can never
  open the journal while a paid task can still record its outcome. The
  binding does not force-unlock and does not promise immediate release while
  application work remains. See D6. `PaymentProvider.close()` must still stop
  any configured serve it started, or `NetMesh.shutdown()` fails until GC (the
  `close()` gotcha).

---

## The design

### D1 — Hoist the binding-neutral projection before porting (adopted at review)

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
- **What moves:** `owner_to_json`/`owner_from_json`, `offer_for`,
  `do_prepare`/`do_purchase`/`do_submit`/`do_attempts`/`do_resolve_attempt`,
  `unresolved_json`/`resolve_admission`'s non-GIL body, the three document
  parsers. Errors return a small `A2aJsonError { kind: Shape | Journal(..) |
  Flow(..), message }` each binding maps to its own exception/prefix.
- **What stays per-binding:** the executor + preflight callback bridges, the
  error mapping, dict/object argument plumbing, handle classes — **and the
  services catalog parser.** **r2 (R5):** r1 moved the catalog parser too, via
  `dict → Value → shared parser`. That does not preserve behavior. Python
  treats a missing key and `None` alike (`a2a_paid.rs:127-130`), extracts with
  PyO3's per-field diagnostics (`:133-165`), requires real dicts for an entry
  and its `bounds` (`:168-182,208-218`), and reads only recognized keys
  (`:219-227`), so a service dict carrying an unused `object()` is accepted
  today and would fail to serialize under a whole-dict conversion. Each
  binding therefore keeps its own field selection, extraction and messages,
  and hands the SDK the typed `BTreeMap<String, A2aServicePolicy>` it already
  takes. `A2aOffer` is an SDK type, so nothing neutral is left to share.
- **Why this is not scope creep:** AGENTS.md's "new binding surface lands in
  `net-mesh-sdk` first" exists because per-binding reimplementation drifted
  (the R1 incident). The alternative — a ~600-line napi re-typing of the
  JSON envelopes — makes every future change to the paid flow a two-binding
  edit with a silent-drift failure mode.

~~**Fallback if D1 is rejected:** port the projections into
`bindings/node/src/a2a_paid.rs` by hand …~~ **Historical (r2.3, S4):** D1 was
adopted at review; there is no hand-port path.

### D2 — Node surface shape: typed objects in, JSON documents out

Following the free-A2A port's precedent (`TaskBriefJs`, not JSON blobs, for
inputs; "Landed" note under its WS-3):

- **Inputs are `#[napi(object)]`** where the Python surface took a dict:
  `services: Record<string, A2aServicePolicyJs>` (`revision`, `pricingTerms?`
  — the JSON string `buildPricingTerms` already returns — `bounds`,
  `reservationTtlSecs`, `reservationRetentionSecs`, `retentionSecs`,
  `description?`), and a `ServeA2aConfiguredOptions { principal?,
  preflight?, handlerTimeoutMs? }`.
- **r2 (R4): the five bounds and three durations are `bigint`, checked to
  u64.** r1 made them `number`/u32 on the grounds that u32 was "ample". That
  narrows the core domain: all eight are u64 (`sdk/src/a2a.rs:275-287,313-320`)
  and Python extracts them as u64 (`a2a_paid.rs:133-143,183-188,219-227`).
  These terms are part of the offer commitment, so a silent truncation would
  change the offer hash.
  Each field goes through `common::bigint_u64` (`bindings/node/src/common.rs:27-52`)
  via the field-naming `u64_arg` wrapper, which refuses negatives and values
  past `u64::MAX` with the field named. A `number` where a `bigint` is
  declared is a napi type refusal, so fractions, `NaN` and `±Infinity` cannot
  be coerced. Limits stricter than u64 (e.g. bounds that would be
  undeliverable, `ServeError::A2aUndeliverableBounds`) are for core to refuse,
  the same way for every binding. `handlerTimeoutMs` stays a u32 `number`: it is
  a Node-local execution knob, not an offer term.
- **Outputs stay JSON strings in the frozen Python schema**: status
  envelopes, attempt rows, unresolved rows, offers. Nested handles are read
  out with the D2a reader, not `JSON.parse`.
- **Names:** `js_name` pinned where napi's camelCase mangles `A2a`
  (`serveA2aConfigured`, `describeA2a`, `submitTaskPaid`, `a2aUnresolved`,
  `a2aResolve`, `a2aAttempts`, `a2aResolveAttempt`, `setA2aOrgCaller`).

### D2a — Lossless handoff of nested documents and u64 fields (r2, R1)

The envelope is JSON, and `JSON.parse` turns every number into an IEEE
double. Fields above 2⁵³ round silently — the reviewer's probe on Node v26.7.0
turned `provider_node: 9007199254740993` into `…992`. In the paid surface those
fields are `prepared.provider_node`, the `Peer(u64)` owner in journal rows,
recovery `generation`s, the caller attempt row's `key.provider_node`, and
`quote.expires_at_ns`. A corrupted `provider_node` points the purchase or
submit at the wrong node; a corrupted owner or generation resolves the wrong
record or none.

Two Rust-backed free functions are the only supported way to read those
documents in JS:

- `a2aDocument(json: string, pointer: string): string` — the sub-document at
  an RFC 6901 JSON Pointer, re-serialized by `serde_json` (whose `Value` keeps
  u64 exactly). `a2aDocument(env, '/prepared')`, `a2aDocument(env, '/proof')`.
  Rejects with `a2a:invalid_argument:` when the pointer names nothing.
- `a2aU64(json: string, pointer: string): bigint` — a u64 leaf as `bigint`,
  for display or to pass to a `bigint` parameter.

**r2.3 (S1): the exact frozen paths, per row kind.** r2 guessed at row
shapes and got one wrong. The real ones:

| Row source | Field | Shape | Read with | Passed to |
|---|---|---|---|---|
| `prepareTask` envelope | `/prepared`, `/proof` (from `purchaseTask`) | object | `a2aDocument` | `purchaseTask` / `submitTask` / `submitTaskPaid` |
| `prepareTask` envelope | `/quote/expires_at_ns` | u64 | `a2aU64` | display |
| caller `a2aAttempts()` row *i* | `/i/key/provider_node` (nested under `key`: `PurchaseAttempt.key: PurchaseKey`, `payments/src/flow/a2a.rs:142-148`; Python tests read `a['key']['provider_node']`) | u64 | `a2aU64` | `a2aResolveAttempt(…, providerNode)` |
| caller `a2aAttempts()` row *i* | `/i/retained` | bool | `JSON.parse` is safe | selects the dispatch below |
| caller `a2aAttempts()` row *i* | `/i/generation` | object `{seq, incarnation}` | `a2aDocument` | `a2aResolveAttempt(…, generationJson)` **only when `retained` is true** |
| provider `a2aUnresolved()` row *i* | `/i/owner` | object (`{"kind":"peer",…}` carries a u64) | `a2aDocument` | `a2aResolve(ownerJson, …)` |
| provider `a2aUnresolved()` row *i* | `/i/generation` | u64 scalar | `a2aU64` | `a2aResolve(…, generation)` |

**Caller generation is a dispatch, not a universal selector.** Supplying
`generationJson` always routes to `resolve_superseded_attempt`, the
archive-only exit (`bindings/python/src/a2a_paid.rs:956-965,1000-1005`). So a
`retained: false` row is resolved by **omitting** `generationJson`. Passing
the live row's own generation does not resolve it; it is refused as naming
no archived incarnation. A `retained: true` row is resolved by passing its
complete `generation` document. The provider's journal generation is a
different thing, a scalar that names one exact incarnation in
`a2aResolve`.

Every verb that takes a handle takes that string: `purchaseTask(preparedJson)`,
`submitTask(preparedJson)`, `submitTaskPaid(preparedJson, proofJson)`,
`a2aResolve(ownerJson, taskId, stateJson, generation?: bigint)`,
`a2aResolveAttempt(taskId, outcomeJson, providerNode?: bigint,
generationJson?: string)`. r1 left `providerNode` untyped; it is now `bigint`.
Status strings and messages are safe to read with `JSON.parse`. The docs name
exactly which fields are not, and every example uses the reader for those.

Considered and rejected: changing the envelope to carry handles as
pre-serialized strings (breaks the frozen Python schema the reviewer asked to
keep); a lossless JS JSON library (a new runtime dependency for a problem
Rust already solves, and every caller would have to remember to use it).

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
  deadline that also refuses, because a wedged event loop must not hold a
  `Preparing` reservation open.
- **r2 (R3): the preflight budget is 5 s, not 30 s.** r1's 30 s equalled
  `A2A_CALL_TIMEOUT` (`sdk/src/mesh_a2a.rs:185`). The caller's deadline starts
  before dispatch (`:445-452`) and the provider's preflight starts later, so a
  never-settling callback would surface as a **caller transport timeout**
  before the refusal could arrive. Those are two different outcomes, and the
  r1 test would have conflated them. 5 s covers TSFN dispatch plus Promise
  settlement and leaves the reply ~25 s of headroom under the current
  control-call budget. That is headroom, not a guarantee: a slow network can
  still turn a refusal into a caller timeout. The docs present the two
  separately. A received refusal is a definite "not admitted"; a transport
  timeout is "unknown, retry the same verb". Once the budget expires the
  callback's future is dropped, so a late `null` from JS has nowhere to land
  and cannot revive the refused attempt.
- **Preflight at submit.** The configured submit re-checks authority after
  payment (parent plan S5). A preflight timeout there is a refusal *after*
  money, so it lands as `Reconcile` with the `admission_revoked` schematic and
  stays unresolved until an operator resolves it. It is never an unpaid
  rejection and never a launch.

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

### D6 — Stop, close and operator access follow the Rust ownership contract (r2, R2)

- `A2aServeHandle.stop()` unregisters the five services and drops the
  handle's `Mesh` and `ServeHandle`s (the Python `stop`,
  `python/src/a2a.rs:370-380,410-414`). It does not drain launched work.
- The provider keeps a `Weak` store reference for the operator verbs, as
  Python does (`payment_provider.rs:389`). **Decision: operator access follows
  store lifetime, not registration lifetime**, as in Python. While a
  launched task still holds the store, `a2aUnresolved()` keeps answering, which
  is when an operator most needs it. Once the last writer drops it, the verbs
  refuse with the Python wording. The docs and the refusal text say "no live
  journal", not "no live serve handle"; r1's "once no configured serve handle
  is live" described only the idle case. A registration-liveness check
  separate from the store was considered and rejected: it would hide unresolved
  rows during exactly the window they are still being written.
- A second `serveA2aConfigured` on the same path, after a `stop()`, keeps
  getting `JournalOwnedElsewhereError` until the last writer exits. The docs
  call this expected and give the remedy as "let running tasks finish (or
  cancel them)", never "delete the `.owner` sidecar".

### D7 — Paid A2A from both `@net-mesh/core` and `@net-mesh/sdk` (r2 R7, amended r2.1)

r1 said `PaymentProvider` / `CapabilityGateway` "are native re-exports" of the
ergonomic SDK, so new methods would arrive with the typings. Half of that was
wrong. `CapabilityGateway` is re-exported (`sdk-ts/src/consent.ts:27-33`), but
`PaymentProvider` is exported nowhere in `sdk-ts/src`, and **both**
constructors take a native `NetMesh` (`payment_provider.rs:365-375`,
`capability_gateway.rs:528-540`). The `sdk-ts` `MeshNode` keeps its native
handle private (`mesh.ts:443-449`), so it cannot supply one.

~~Decision (the smaller option, which keeps scope fixed): paid A2A is
documented as a `@net-mesh/core` surface … An ergonomic `sdk-ts` provider
surface … is a named follow-up and out of scope.~~

**r2.1 decision: both entry points are supported.** `@net-mesh/core` stays
the native surface unchanged (everything in WS-B…WS-D). `@net-mesh/sdk` gains
an ergonomic layer that **adapts handles and nothing else**. It follows the
SDK's existing adaptation precedent (`aggregator.ts:57-68`,
`org/index.ts:122-129`): accept `MeshNode | NetMesh`, resolve a `MeshNode`
through the sealed `getNapiMesh`, and hand back the **native** object. No
method is re-implemented or forwarded one by one, so the SDK cannot drift
from the binding.

- **Factories, not wrapper classes.** `createPaymentProvider(mesh, options)`
  and `createCapabilityGateway(mesh, options)` return the native
  `PaymentProvider` / `CapabilityGateway`. Subclassing a napi class from JS
  was rejected: napi-rs's construction path does not reliably support
  `extends`, and a subclass would add a second type for the same object.
  Forwarding wrappers were rejected because the provider and the gateway
  together carry ~20 methods, and each one would be a drift point.
- **Options objects at the SDK layer only.** The factories take named
  options (`PaymentProviderOptions { statePath, billingLogPath?,
  facilitatorUrl?, facilitatorAuthToken?, unsafeDevMockFacilitator?,
  requireInvocationBinding? }`; `CapabilityGatewayOptions { pinStorePath?,
  paymentPolicyPath?, paymentProfile?, paymentUnsafeMockAutoAllow?,
  paymentSigner?: { address, sign }, paymentSignerSvm?, paymentSignerXrpl?,
  a2aPurchasePath? }`) and map them onto the native positional constructors
  in one function each. This gives SDK users the options-object API that D5
  defers for core, without breaking a single core caller.
- **Org identity adaptation.** `setA2aOrgCaller(target, org)` accepts
  `target: MeshNode | NetMesh | CapabilityGateway` and
  `org: OrgClient (SDK) | TypedOrgClient | native OrgClient | null`. It
  resolves the SDK client through `org.typed.raw`, the typed client through
  `.raw`, and calls the native setter on the right slot (D6's two-slot
  rule: a mesh target sets the `NetMesh` slot, a gateway target sets the
  gateway's own slot). The native setters cannot take the SDK wrapper
  themselves, because napi only accepts its own class instances.
- **`MeshNode` forwards** for the raw requester verbs, in the existing
  `Parameters<NapiNetMesh[...]>` style beside `serveA2a`/`submitTask`
  (`mesh.ts:1058-1078`): `describeA2a`, `submitTaskPaid`.
- **Re-exports:** `a2aDocument`, `a2aU64` (D2a) and the D4 error classes
  plus `classifyError` from the SDK root, so an SDK user never imports
  `@net-mesh/core` for paid A2A. Errors stay plain prefixed `Error`s until
  `classifyError(e)` is applied, exactly as at the core layer; the docs say
  so rather than implying that exporting the classes converts anything.
- **Lifecycle is unchanged and stated:** the factories return native objects
  that retain the node, so `stop()`/`close()` (D6) must run before
  `MeshNode.shutdown()`, the same as for every other native handle the SDK
  hands out.
- ~~**Beyond Python parity, deliberately.** Python's ergonomic `net_sdk` has
  no payments or A2A surface either …~~ **r2.2:** wrong; see the r2.2 note
  under Status. Python's SDK makes the same promise and gets the same
  adaptation in WS-G. It needs only the handle half, because Python already
  has keyword arguments (no options mapping) and arbitrary-precision ints (no
  D2a reader).

---

## The slices

A depends on nothing; B–D depend on A; E on B–D; F last. G (Python SDK,
r2.2) depends only on A and is in scope for the same release.

### WS-A — Hoist the shared projection (D1)

- [x] `net_payments::flow::a2a::json` (feature `mesh`), moved out of
  `python/src/a2a_paid.rs` with their doc comments. **Deviation:** the
  provider-side operator helpers (`owner_to_json` / `owner_from_json`,
  `unresolved_json`, `resolve_admission`) and `parse_principal` went into the
  same `net-payments` module, **not** `net_sdk::a2a_journal`. Every provider
  verb that uses them hangs off `PaymentProvider`, which only exists with
  `payments`, so a second home in the SDK would only have split one boundary
  in two. One error type, `A2aBoundaryError { Invalid | Failed | Journal }`,
  maps to Python's `ValueError` / `RuntimeError` / journal mapping exactly
  as before.
- [x] **First, before anything moves (r2, R5):**
  `bindings/python/tests/test_a2a_paid_config_compat.py`, written and green
  against today's code, pinning the input behaviors the unedited suite does
  not reach: an unused extra key holding a non-JSON value (`object()`) is
  ignored; missing and `None` are equivalent for `pricing_terms` and
  `description`; a non-dict service entry and a non-dict `bounds` are refused
  with today's messages; a wrong-typed field (string for a u64, negative, float)
  is refused with today's field-named message.
- [x] `python/src/a2a_paid.rs` reduced to dict plumbing (its catalog parser
  stays here, unchanged), the two callback bridges, and error mapping over the
  shared functions (1,034 → ~490 lines; the wrappers keep their names and
  signatures, so `capability_gateway.rs` / `payment_provider.rs` are
  untouched).
- [~] Rust unit tests on the shared module: **parsers done, envelopes not.**
  7 tests cover every parser refusal, the owner round trip (incl. a u64 above
  2⁵³ kept exactly), and the principal vocabulary. The per-status envelopes
  need a live `A2aCallerFlow` + mesh, so they are witnessed by the Python
  suites and the cross-binding fixture instead, not by a Rust unit test.
- [x] **Cross-binding fixture captured before the move** (pulled forward from
  WS-F): `bindings/python/tests/test_a2a_paid_cross_lang.py` writes / checks
  `tests/cross_lang_a2a_paid/envelopes.json`, the shapes of 11 documents
  (happy path + post-payment revocation), with per-run values masked by type.
  **Defect found in the capture, fixed:** `unresolved_empty` was captured as
  a `launched` row. A launched task stays in the provider's unresolved class
  until the terminal hook writes its row, and that write lands *after* the
  registry already reports `completed`. Waiting on task state was therefore
  not enough (1 failure in 3 runs, then 2 in 12). The fixture now polls the
  queue itself until it drains (0 failures in 20 runs). Only that one entry
  was corrected by hand; the other ten are the pre-move capture. **The Node
  suite must use the same poll.**

**Proved by:** `tests/test_a2a_paid.py` green **unedited** and
`test_a2a_paid_config_compat.py` green unedited across the move;
`cargo clippy -p net-payments --features mesh -- -D warnings` and
`cargo doc -p net-payments --no-deps --all-features` clean (the `--all-features`
requirement in AGENTS.md); `cargo doc -p net-python` with the hand-maintained
feature list clean.

> **Status: WS-A landed 2026-10-03 (uncommitted at time of writing).**
> Witnessed on a `maturin develop` build with `ci.yml:4152`'s feature list:
> before the move, `test_a2a_paid.py` (27) + `test_a2a_paid_config_compat.py`
> (16) = 43 passed. After it: those two plus `test_a2a_paid_cross_lang.py`,
> `test_a2a_history_boundary.py` and `test_a2a.py` all pass (49 rows), with
> no edits to any pre-existing test; the paid trio passed 20/20 consecutive
> runs. `cargo test -p net-payments --features mesh --lib flow::a2a::json`:
> 7 passed. Gates clean: `cargo clippy --all-features --lib --bins -D warnings`
> and `--all-targets` (CI's `-A` set) on `net-payments`, the `ffi-clippy`
> python entry (`--all-targets -D warnings`, `ci.yml:5076`), rustdoc
> `-D warnings` for `net-payments --all-features` and `net-python`, and
> rustfmt. One rustdoc defect was found and fixed: an outer `///` on
> `pub mod json;` made rustdoc resolve the module's inner docs in the
> parent's scope. No CI change is needed: the `python-tests` job runs the
> whole `tests/` directory, and the new files pick up automatically.

### WS-B — Provider: `PaymentProvider.serveA2aConfigured` (`node/src/a2a_paid.rs`)

- [ ] New module gated `#[cfg(all(feature = "a2a", feature = "payments"))]`.
- [ ] `PaymentProvider.serveA2aConfigured(executor, services, journalPath,
  options?) => Promise<A2aServeHandle>`: async (journal `open` is file IO and
  takes the `.owner` lock; registration needs the tokio context — the
  "no reactor running" lesson from the free port), principal
  `"session_peer" | "same_org" | "granted"`, preflight bridge (D3),
  `JournalOwnedElsewhereError` (D4). Refuses if the provider is closed.
- [ ] Catalog fields as checked `bigint` (D2, r2 R4), each refusal naming
  the service and field.
- [ ] `A2aServeHandle` grows a `Registered::Configured` arm (the Python
  enum, `python/src/a2a.rs:363-430`) and a `services` getter; `stop()`
  retires the registration and drops the handle's references (D6). **r2
  (R2):** r1 said it "releases the journal owner".
- [ ] Operator verbs: `a2aUnresolved() => Promise<string>`,
  `a2aResolve(ownerJson, taskId, stateJson, generation?: bigint) =>
  Promise<void>` over a `Weak` store reference, answering while any writer
  holds the store and refusing once none does (D6).
- [ ] `PaymentProvider.close()` stops a live configured serve.

**Proved by:** the provider-side cases of WS-F's `a2a_paid.test.ts` (catalog
refusals incl. the R4 numeric cases, journal ownership in-process and
cross-process, the parked-executor ownership witness, operator queue) — runnable from this slice with a raw Rust-side
caller, completed once WS-D lands; `cargo clippy -p net-node --all-targets`
clean.

### WS-C — Requester raw verbs on `NetMesh` (`node/src/a2a.rs`)

- [ ] `describeA2a(targetNodeId: bigint) => Promise<string>` (JSON
  `A2aOffer[]`; the legacy free path has no describe service and rejects).
- [ ] `submitTaskPaid(preparedJson, proofJson) => Promise<string>` — headers
  from the proof, brief from the prepared document, no store;
  `PaymentRefusedError` on refusal.
- [ ] `setA2aOrgCaller(orgClient: OrgClient | null)` — needs a crate-internal
  `OrgClient::shared()` accessor over its `ArcSwapOption`. **r2 (R6):** the
  raw verbs build a fresh SDK `Mesh` per call, so the installed caller must
  live in a slot on `NetMesh` itself (Python's `a2a_org_caller` field,
  `lib.rs:1384`), and every fresh wrapper reads it. `mesh_over` becomes the
  org-aware variant (Python's `mesh_over_as`, `python/src/a2a.rs:122`).
  Sharing the core node does not share the slot. Applies to
  describe/submit/submitTaskPaid/status/cancel.
- [ ] `a2aDocument` / `a2aU64` free functions (D2a).
- [ ] Rewrite the `submitTask` doc comment that currently says paid A2A is out
  of scope for this binding.

**Proved by:** `submitTaskPaid` refusal cases (unpaid → `PaymentRefusedError`
with a parseable schematic; oversized brief → `a2a:invalid_argument:`) and a
`describeA2a` round-trip against a WS-B provider; the D2a reader cases;
existing `a2a.test.ts` green unedited (free path unchanged).

### WS-D — Caller flow on `CapabilityGateway`

- [ ] `build_payment_flow` keeps the concrete `Arc<CallerPaymentFlow>` and,
  when `a2aPurchasePath` is set, builds the `A2aCallerFlow` over it (one flow,
  one spend store, one signer set — a budget spent on a paid tool is a budget
  spent on a paid task).
- [ ] Ctor arg `a2aPurchasePath` (D5) with the two loud refusals Python has.
- [ ] `prepareTask(targetNodeId, service, prompt, contextRefs?, tags?,
  taskId?)`, `purchaseTask(preparedJson)`, `submitTask(preparedJson)`,
  `a2aAttempts()`, `a2aResolveAttempt(taskId, outcomeJson, providerNode?:
  bigint, generationJson?)`, `setA2aOrgCaller(orgClient | null)` — all
  `async`, resolving to the shared envelopes; a gateway built without a
  purchase store rejects with the Python wording. `targetNodeId` is `bigint`.
- [ ] **r2 (R6):** the gateway's setter updates the org-caller slot of the
  gateway's **own persistent** SDK `Mesh`, the one its `A2aCallerFlow`
  composes over (Python: `self.state.mesh.set_a2a_org_caller`). It does not
  touch the `NetMesh` slot of WS-C. Two slots, two setters, both witnessed.
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
- [ ] ~~`sdk-ts`: forward `describeA2a` / `submitTaskPaid` / `setA2aOrgCaller`
  …; `PaymentProvider` / `CapabilityGateway` are native re-exports, so their
  new methods arrive with the typings.~~ **r2 (R7):** false for
  `PaymentProvider`, and the ctors need a native `NetMesh` anyway.
- [ ] **r2.1 (D7): the `@net-mesh/sdk` ergonomic layer.** New
  `sdk-ts/src/payments.ts`, exported from the SDK root:
  `createPaymentProvider`, `createCapabilityGateway`, the two options types,
  `setA2aOrgCaller`, and re-exports of `a2aDocument`, `a2aU64`,
  `PaymentRefusedError`, `JournalOwnedElsewhereError`, `classifyError`.
  `MeshNode` gains `describeA2a` / `submitTaskPaid` forwards. The existing
  root `CapabilityGateway` re-export stays as it is (no symbol changes type).
  Its doc comment points SDK users at `createCapabilityGateway`.
- [ ] A type-level guard that each options mapping covers the native
  constructor exactly. The test asserts the mapped tuple type equals
  `ConstructorParameters<typeof NapiPaymentProvider>` /
  `<typeof NapiCapabilityGateway>` minus the mesh, so a parameter added to a
  native ctor without an options key fails `typecheck`, not a user.
- [ ] A consumer-compile case (the `test/consumer/` + `consumer_compile.test.ts`
  precedent) that compiles **and runs** the exact documented imports against
  the built package entry points: `NetMesh`, `PaymentProvider`,
  `CapabilityGateway`, `a2aDocument`, `a2aU64` from `@net-mesh/core`, and
  `classifyError`, `PaymentRefusedError`, `JournalOwnedElsewhereError` from
  `@net-mesh/core/errors`. It asserts that a native rejection is a plain
  prefixed `Error` until `classifyError` turns it into the class.

**Proved by:** `errors.test.ts` classification cases; the consumer case above;
`npm run typecheck:tests` in `bindings/node` clean; and for r2.1:
- `sdk-ts/test/paid_a2a.test.ts`, live, SDK-only imports: two
  `MeshNode.create` nodes; `createPaymentProvider(meshNode, { …,
  unsafeDevMockFacilitator: true })` → `serveA2aConfigured`;
  `createCapabilityGateway(meshNode, { paymentPolicyPath, a2aPurchasePath,
  paymentUnsafeMockAutoAllow: true })` → prepare → `a2aDocument` → purchase →
  submit, task runs once; a native `NetMesh` passed to the same factories
  also works (both arms of the adaptation). Teardown is `stop()` →
  `close()` ×2 → `MeshNode.shutdown()`, and **shutdown must succeed**, which
  proves the factories retain nothing beyond the native objects' documented
  references.
- the same-org case of R6 rerun from the SDK with an SDK `OrgClient`
  passed to `setA2aOrgCaller` for both a `MeshNode` target and a gateway
  target, on the generated scenario `sdk-ts/test/org_live.test.ts` already
  uses; plus `null` clears and denies before launch.
- an options-mapping case per option, each reaching the native behavior it
  names (e.g. omitting both facilitator options throws the native "a
  settlement backend must be chosen" error; `a2aPurchasePath` without
  `paymentPolicyPath` throws the native refusal), so a mis-ordered mapping
  fails a test.
- `package_entry_points.test.ts` and `trust_surfaces.test.ts` extended with
  the new root exports; `sdk-ts` typecheck clean. CI: no change. The `sdk-ts-tests` job
  (`ci.yml:3850`) already builds the native module with
  `payments,payments-http,delegation,a2a,org` (`ci.yml:3959`), and the new
  suite is auto-discovered.

### WS-F — Tests, docs, matrix

- [ ] `bindings/node/test/a2a_paid.test.ts`: the twin of `test_a2a_paid.py`
  over the mock facilitator (`unsafeDevMockFacilitator: true`), case for case
  where the property is reachable from JS — free success through the catalog,
  price-it-cannot-enforce refusal, second provider on one journal (in-process
  **and** a child `node` process), stop releases the journal with the provider
  alive *in the idle case* (no launched task or terminal write still holding
  the owner; the under-work case is the R2 witness below), the operator queue
  refuses *once no writer retains the store* (D6: it keeps answering while
  one does), oversized brief refused
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
  refusal. Plus Node-only: the preflight throw refuses (D3), a paid
  task past `handlerTimeoutMs` ends `failed` and is not re-run.
- [ ] **r2 additions** (each named for the finding it witnesses):
  - **R1 lossless handoff** (rewritten in r2.3, S1). Ids: the harness
    searches keypairs until the derived node id is **not representable as a
    double**: `BigInt(Number(id)) !== id`. Being above
    `Number.MAX_SAFE_INTEGER` is not enough, because some larger integers are
    exactly representable (every even integer in [2⁵³, 2⁵⁴), for one). For a
    uniform u64 id the check almost always passes on the first key. Both the
    provider node and the paying caller's peer id are chosen this way, so the
    provider-side `owner` carries one too. Three legs follow, each with its own
    setup, because a successfully submitted attempt is not eligible for
    operator resolution:
    1. **Happy path:** prepare → `a2aDocument(env, '/prepared')` → purchase →
       `a2aDocument(env, '/proof')` → raw `submitTaskPaid`; the task runs once
       on the right provider.
    2. **Caller recovery, live vs retained.** Stage one key holding both a
       retained incarnation and its live replacement, with the live
       replacement driven to `unexecutable` so it is genuinely eligible for the
       same `closed` resolution. This uses the production-store seeder that
       `bindings/python/tests/test_a2a_history_boundary.py` builds and runs
       (it archives real mock-rail evidence through `retain_superseded`;
       the Python boundary cannot manufacture the supersession race either,
       and that test says so). Then: `a2aAttempts` → for the `retained: true`
       row, `a2aU64('/i/key/provider_node')` + `a2aDocument('/i/generation')`
       → `a2aResolveAttempt(taskId, closed, providerNode, generationJson)`.
       Assert the retained row is closed and the live row is **unchanged**
       (its `a2aDocument('/j')` string equal before and after). Then resolve
       the live row by **omitting** `generationJson` and assert it closes.
       Passing the live row's own generation is asserted to be refused. This
       is the routing discrimination the Python C5 witness carries: wrong
       routing would close the live row and fail the test.
    3. **Provider recovery:** a post-payment revocation (the preflight-at-submit
       path) leaves a `Reconcile` row → `a2aUnresolved` →
       `a2aDocument('/i/owner')` + `a2aU64('/i/generation')` → `a2aResolve`
       with that exact scalar generation, closing that incarnation only (twin
       of `test_an_admission_is_resolvable_by_its_exact_generation`).

    **Negative control:** for the same `prepared` document,
    `JSON.stringify(JSON.parse(prepared))` changes `provider_node`. The
    selection rule above guarantees it does, so the test cannot pass
    vacuously.
  - **R2 ownership under work:** park a launched executor on a barrier;
    `stop()` the handle and `close()` the provider; a second
    `serveA2aConfigured` on the same journal (in-process and child process) is
    still `JournalOwnedElsewhereError` and `a2aUnresolved()` still answers;
    release the executor, await its terminal row; the reopen then succeeds.
    The idle stop/reopen case stays as a separate test.
  - **R3 preflight timing:** a never-settling preflight during **prepare**
    yields a received refusal inside the caller's budget, with no quote, no
    spend reservation and no launch. A never-settling preflight at **submit
    after purchase** yields no launch and an unresolved `Reconcile` record
    with the paid evidence intact on both sides. In both cases the callback
    then resolves `null` late, and the refused attempt stays refused.
  - **R4 catalog numerics:** `0n` where core accepts it; the largest value core
    accepts per field; `u64::MAX` behaving exactly as in Rust/Python; `-1n`
    and `2n**64n` refused naming the field; `1.5`, `NaN`, `Infinity` and a
    plain `number` refused by type, never coerced. The `describeA2a` offer
    hash for a fixed catalog equals the Python fixture's for the same input.
  - **R6 live same-org principal** (`principal: "same_org"`), on the
    generated scenario `org_live.test.ts` already mints
    (`:120-205`, same-org setup `:273-310`). The raw `NetMesh` setter and the
    gateway setter each drive a real prepare → purchase → submit, plus
    `taskStatus`/`cancelTask`, and the preflight sees the admitted entity as
    the owner. With no identity installed, and again after clearing it with
    `null`, the call is denied before launch (run counter 0). Teardown stops
    handles, closes gateway/provider/`OrgClient` and shuts the meshes down. The
    cross-org `granted` principal stays explicitly qualified as covered only
    by the SDK's `a2a_admission_identity`, unless this harness's cross-org
    scenario extends to it cheaply.
- [ ] **Cross-binding vector test:** drive one scripted lifecycle and assert
  the Node envelopes equal checked-in JSON fixtures that the Python suite
  asserts too (`tests/cross_lang_a2a_paid/`, the `cross_lang_*` convention),
  volatile fields (ids, timestamps) masked. **r2 (R1):** equality is of JSON
  values read losslessly (via `a2aDocument`/`a2aU64` in Node), not of bytes,
  and never via `JSON.parse`. The fixtures are captured from today's Python
  output before WS-A starts. Under D1 this is a tripwire; under the fallback
  it is the only guard.
- [ ] Matrix flips to `✓` for Node/TS in all three sources (README table,
  `event-bus.yaml` with anchor `serveA2aConfigured`, skill `coverage.md`
  both tables); `web/src/content/docs/guides/agent-to-agent.md` "paid
  services" section gains the TS snippets; `.claude/skills/net-event-bus/a2a.md`
  §"Paid A2A" gains the Node prepare/purchase/submit example. r2.3 (S4):
  the **ergonomic example uses the SDK route** (`MeshNode.create`,
  `createPaymentProvider`, `createCapabilityGateway`, and `a2aDocument` /
  `classifyError` from `@net-mesh/sdk`). The `@net-mesh/core` route is shown
  beside it as the native variant (D7: both are supported). Both read handles
  with `a2aDocument`, never `JSON.parse`/`JSON.stringify` (D2a), and the
  paid-timeout and one-sided cancellation caveats sit beside them (D3); release note
  under `net/crates/net/docs/releases/` mirrored via `npm run sync:releases`.
- [ ] CI: no feature-list change (already `payments,a2a,org`); the new vitest
  file is auto-discovered. If WS-F adds `tests/cross_lang_a2a_paid/` fixtures
  read by a Rust test, pin that test in `ci.yml` per the pin-guard rule.

**Proved by:** the suite itself; `npm run check` in `web/` (docs links +
releases sync); the skill-snippet checker.

---

### WS-G — Python SDK handle adaptation (r2.2)

Independent of the Node slices; it may land any time after WS-A. Same rule
as D7: **adapt handles and return native objects; implement nothing.**

- [ ] `sdk-py/src/net_sdk/payments.py`, re-exported from `net_sdk`:
  `create_payment_provider(mesh, state_path, **kwargs)`,
  `create_capability_gateway(mesh, **kwargs)` and
  `create_async_capability_gateway(mesh, **kwargs)` (scope in the matrix
  below). Each resolves `mesh`
  through the existing `_native_mesh` (`mesh.py:1153-1166`, which accepts
  `MeshNode`, `AsyncMeshNode` or a raw `NetMesh` and raises a clear
  `TypeError` otherwise) and passes `**kwargs` through untouched. Because
  nothing is mapped, a keyword the native constructor gains later works with
  no SDK edit, and the native constructor stays the single source of truth for
  validation. Imports are guarded like `consent.py`, so a wheel built without
  `payments` / `a2a` still imports `net_sdk`.
- [ ] Re-export `PaymentProvider`, `PaymentRefused` and
  `JournalOwnedElsewhere` from `net_sdk` (guarded), so a paid-A2A program
  never imports `net`.
- [ ] `MeshNode.set_a2a_org_caller(org)` / `AsyncMeshNode` twin, and a module
  helper `set_a2a_org_caller(gateway, org)`. Both accept a
  `net_sdk.org.OrgClient` (unwrapped via its `.raw`, `org/__init__.py:119-128`),
  a native `OrgClient`, or `None`, and set the right slot: the `NetMesh` slot
  for a mesh, the gateway's own slot for a gateway (D6's two-slot rule). The
  gateway helper accepts **only the sync `CapabilityGateway`**. Given an
  `AsyncCapabilityGateway` it raises `TypeError`, naming the sync gateway as
  the paid-A2A surface.
- [ ] **r2.3 (S2): the async matrix, stated rather than implied.** Adapting
  an `AsyncMeshNode` is not async paid A2A. The native
  `AsyncCapabilityGateway` refuses `a2a_purchase_path` and has no paid verbs
  or org setter (`bindings/python/src/capability_gateway.rs:1402-1440`,
  methods `:1452-1566`); paid A2A is sync-gateway-only by the native
  contract, driven from asyncio with `asyncio.to_thread`.

  | Mesh handed in | Factory | Returns | Paid A2A? |
  |---|---|---|---|
  | `MeshNode` / `AsyncMeshNode` / `NetMesh` | `create_capability_gateway(…, a2a_purchase_path=…)` | native sync `CapabilityGateway` | yes; from asyncio, `await asyncio.to_thread(gw.prepare_task, …)` and the same for purchase/submit/attempts/resolve |
  | `MeshNode` / `AsyncMeshNode` / `NetMesh` | `create_async_capability_gateway(…)` | native `AsyncCapabilityGateway` | **no.** It adapts the handle for the existing `search`/`describe`/`invoke` surface only; `a2a_purchase_path=` raises the native `TypeError` |
  | any | `create_payment_provider(…)` | native `PaymentProvider` | provider side (sync verbs, as natively) |

  No SDK-side async wrapper around the financial verbs is added. Wrapping
  them in an automatic thread hop would be new behavior on a money path, and
  native async paid parity is not owed by this plan (named in Not in scope).
- [ ] **No D2a analog, on purpose.** Python ints are arbitrary precision and
  `json.loads` / `json.dumps` round-trip u64 exactly, so R1 is a JavaScript
  defect. The Python docs keep the frozen pattern
  (`json.dumps(env["prepared"])`). A regression case pins it (a
  `provider_node` above 2⁵³ survives that round-trip).
- [ ] Docs: `.claude/skills/net-event-bus/a2a.md` §"Paid A2A" and the
  agent-to-agent guide show the `net_sdk` form first; the raw-`net` form
  stays valid and is kept as the low-level variant.
  `examples/a2a_paid.py` (CI-executed) is left on raw `net`, so it keeps
  witnessing the native surface unchanged.

**Proved by:** `sdk-py/tests/test_paid_a2a_sdk.py` driving an **SDK-only
consumer subprocess**. r2.3 (S3): r2.2 said "live against the built
extension", but that does not follow from installing a wheel.
`sdk-py/tests/conftest.py:50-90` installs an auto-stub `net` module whenever
`net` is not already imported, without first trying a real wheel (the reviewer's
probe saw `net.__file__ is None` and a generated `NetMesh`), so an in-process
pytest here can silently test the stub. The vehicle follows the org facade
precedent (`tests/test_org_streaming.py:5-10,60-106`):

- `sdk-py/examples/paid_a2a_consumer.py` imports only the standard library
  and `net_sdk`: no `net` import and no private attribute. It runs in a fresh
  interpreter, outside the stubbed pytest process, and does the live work:
  two `net_sdk.MeshNode`s →
  `create_payment_provider(...).serve_a2a_configured(...)` →
  `create_capability_gateway(..., a2a_purchase_path=...)` → prepare → purchase
  → submit, with the task running once. It runs once with an `AsyncMeshNode`
  (paid verbs via `asyncio.to_thread`, per the matrix) and once with a raw
  `NetMesh`. An SDK `OrgClient` goes to both setters on the same-org
  scenario, and `None` clears and denies before launch. Teardown ends in
  `shutdown()` succeeding. Each row prints a structured receipt that the
  pytest side asserts.
- **Native evidence before any row, failing on a stub or a partial surface.**
  A probe subprocess (the `_PROBE` shape) separates three cases. No loadable
  extension means **skip**: that is the native-free `sdk-py-tests` job, which
  keeps its existing skip behavior. An extension whose facade fails to import
  means **fail**. An extension that loads but lacks any required name means
  **fail**: `PaymentProvider`, `CapabilityGateway.prepare_task`,
  `NetMesh.set_a2a_org_caller`, `PaymentRefused`, `JournalOwnedElsewhere`,
  each checked one by one against the native module and the facade. The
  consumer additionally asserts that `net._net.__file__` names a real
  extension file.
- Negative controls, in the consumer: a `net_sdk.MeshNode` passed to the
  **native** `net.CapabilityGateway` still raises `TypeError` (native
  unchanged); an unknown keyword to a factory raises the native constructor's
  own error; `create_async_capability_gateway(..., a2a_purchase_path=...)`
  raises; and `set_a2a_org_caller(async_gw, org)` raises `TypeError` (S2).
- **CI: executed and floored, not only declared.** A new step in the
  `python-tests` job after `Install the sdk-py wrapper` (`ci.yml:4367-4368`),
  modeled on `Witness roster + floor — sdk-py org facade` (`:4386-4412`):
  `check-roster.py --mode decl` pins every row name. Then
  `python -m pytest -q tests/test_paid_a2a_sdk.py` **runs** them, and the step
  fails unless the passed count reaches the floor. A run that skipped (stub or
  no wheel) reports 0 passed and fails the floor. `ci.yml:4152` already builds
  that job's extension with `payments,a2a,org`.

### Decisions (resolved at review, r2)

1. **D1 — hoist.** Adopted: the shared projection, with Python-specific
   extraction (incl. the catalog parser) kept at the edge (R5). No
   hand-maintained second JSON implementation.
2. **Preflight deadline.** Adopted: 5 s, below the 30 s control-call budget,
   not configurable in the first cut; transport timeout documented separately
   from a received refusal (R3).
3. **Default `handlerTimeoutMs` on the configured path.** Adopted: keep 1 hour.
   A paid timeout is documented as a terminal execution failure, never a
   refund. One-sided JS cancellation stays explicit: a discarded Promise
   result does not stop external effects. No core lifecycle change.
4. **Gateway constructor.** Adopted: append-only for this port; no
   options-object migration.
5. **Operator access lifetime** (new, D6): follows store lifetime, as in
   Python.
6. **Package boundary** (new, D7): ~~`@net-mesh/core` only for this port.~~
   **r2.1:** both. Core stays native, and `@net-mesh/sdk` gains factory +
   handle-adaptation helpers that return native objects (no forwarding
   wrappers).
7. **Python SDK** (r2.2, WS-G): same adaptation, handle half only. It needs no
   options mapping and no lossless reader.
8. **Python async boundary** (r2.3, S2): paid A2A stays sync-gateway-only,
   driven from asyncio by `to_thread`. `create_async_capability_gateway` is a
   handle adapter for search/describe/invoke and nothing more. Option 1 of
   the reviewer's three.

### Test matrix (target)

| Area | Rust unit | vitest (built `.node`) | Parity source |
|---|---|---|---|
| shared envelopes + parsers | ✓ (new, WS-A) | via every case below | `test_a2a_paid.py` (unedited) |
| configured serve, catalog refusals | — | ✓ | `test_a2a_paid.py` |
| journal ownership (in-process + child process), incl. parked-executor witness | — | ✓ | `test_a2a_paid.py`, `a2a_admission_journal` |
| lossless handoff above 2⁵³ (D2a) | ✓ (pointer reader) | ✓ | — |
| catalog numeric domain (R4) | — | ✓ | Python fixture (offer hash) |
| Python input compatibility (R5) | — | — | `test_a2a_paid_config_compat.py` (new, pre-move) |
| documented imports against built entry points (R7) | — | ✓ consumer case | — |
| SDK factories + handle adaptation (r2.1), `MeshNode` and native arms, clean `shutdown()` | — | ✓ `sdk-ts/test/paid_a2a.test.ts` | — |
| SDK `OrgClient` → both org slots (r2.1) | — | ✓ live same-org | R6 |
| options ↔ native ctor coverage (r2.1) | — | ✓ type-level + per-option | — |
| Python SDK factories + org setter (r2.2), via SDK-only consumer subprocess with native-evidence probe (r2.3) | — | — | ✓ `sdk-py/tests/test_paid_a2a_sdk.py`, executed + floored in `python-tests` |
| Python async matrix (r2.3, S2) | — | — | ✓ to_thread arm + async-gateway refusals |
| caller live-vs-retained routing over a seeded store (r2.3, S1) | — | ✓ | `test_a2a_history_boundary.py` (C5) |
| prepare → purchase → submit, once-only | — | ✓ (two-node) | `test_a2a_paid.py`, `a2a_paid_end_to_end` |
| approval / reject / restart / resolve | — | ✓ | `test_a2a_paid.py` |
| org-admitted principal, same-org (R6) | — | ✓ live, both setters | `a2a_admission_identity` |
| org-admitted principal, cross-org `granted` | — | qualified (SDK witness only) unless cheap | `a2a_admission_identity` |
| preflight timing at prepare and at submit (R3) + handler deadline | — | ✓ (Node-only) | — |
| error classification | — | ✓ `errors.test.ts` | — |
| JSON parity | — | ✓ fixtures | `cross_lang_a2a_paid/` |

### Effort + sequencing

With D1: ~350 lines moved (not new) into the shared module (r2: the catalog
parser no longer moves), ~400 lines of new napi (`a2a_paid.rs` +
gateway/provider/`NetMesh` methods + the D2a reader + the catalog parser),
~60 lines of `errors.ts`, one ~1,100-line vitest suite (r2 witnesses
included), one small Python compatibility test. r2.1 adds ~200 lines of
`sdk-ts` (`payments.ts`, two `MeshNode` forwards, the options mappings) and a
~300-line `sdk-ts` live suite. r2.2 adds ~80 lines of `net_sdk`
(`payments.py` + two setters) and a ~200-line live pytest.

Commit sequence, each compiling and green on its own: capture the
cross-binding fixtures and land `test_a2a_paid_config_compat.py` against
today's code → WS-A (both Python suites unedited) → WS-B → WS-C → WS-D →
WS-E → WS-G → WS-F (r2.3, S4: WS-G is in the release; it may land any time
after WS-A, but WS-F's docs and matrix flip describe it, so it lands before
them). Run the node vitest suite once after WS-D against a freshly built
`.node` with the CI feature list (`ci.yml:3746`), the WS-G consumer once
against a `maturin develop` build with `ci.yml:4152`'s list, then the full
pre-push checklist once before review.

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
- ~~**Org-admitted principal is hard to drive from vitest** … *Fallback:*
  source-established.~~ **r2 (R6):** withdrawn. `org_live.test.ts` already
  mints a scenario and drives protected calls, so the same-org path gets a live
  witness. The remaining risk is that the scenario mint (`cargo run --example
  gen_org_scenario`) is slow and its certs expire, so the suite mints per run
  as `org_live.test.ts` does. *Fallback* for a flaky mint is that suite's
  existing timeout budget, not a weaker witness.
- **Someone reads stop() as "the journal is free".** (r2, R2) An operator
  restarts a provider while a paid task runs and gets
  `JournalOwnedElsewhereError`. *Fallback:* the error text and docs say why
  and what to do (let tasks finish or cancel them); the binding never offers a
  force-unlock.
- **The SDK options mapping drifts from a native positional constructor**
  (r2.1). A new native parameter gets no option, or two are swapped.
  *Fallback:* the type-level `ConstructorParameters` guard fails `typecheck`
  on a missing one; the per-option behavior cases catch a swap.
- **A caller uses `JSON.parse` on a handle anyway.** (r2, R1) *Fallback:*
  every example and doc snippet uses the reader, each verb's doc comment
  names its u64 fields, and the D2a negative-control test shows the failure
  concretely.
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
- ~~An ergonomic `sdk-ts` paid-A2A / `PaymentProvider` surface (D7).~~
  **r2.1:** now in scope (WS-E); the Python twin is in scope as of r2.2 (WS-G).
  Still out of scope: forwarding wrapper classes over the native
  provider/gateway in either SDK.
- Native async paid A2A in Python (`AsyncCapabilityGateway` paid verbs), and
  any SDK-side automatic async wrapper over the financial verbs (r2.3, S2).
- Force-unlocking or draining the journal on `stop()`/`close()` (D6).
