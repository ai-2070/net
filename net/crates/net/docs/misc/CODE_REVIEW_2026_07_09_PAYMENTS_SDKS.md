# Code Review — Payments Language SDKs (node + python bindings, golden vectors)

**Date:** 2026-07-09
**Branch:** `net-payments-sdks`
**Diff reviewed:** merge base `51f839cd3` → branch tip `e83236ca0` (49 files, +6958 / −182)
**Scope:** the multi-language payments SDK surface — the Node/napi bindings
(`capability_gateway.rs`, `payment_provider.rs`, `payment_http.rs`,
`payment_signer.rs`, `publish.rs`, `lib.rs`, `tool.rs`, `errors.ts`), the
Python/pyo3 bindings (`capability_gateway.rs`, `payment_provider.rs`,
`payment_http.rs`, `publish.rs`, `lib.rs`, `_net.pyi`, `__init__.py`), the
cross-language golden vectors (Rust source-of-truth
`payments/tests/payments_golden_vectors.rs` + `examples/gen_payments_fixtures.rs`,
`tests/cross_lang_payments/payment_vectors.json`, and the node/python/go
verifiers), and the skill docs. The money-moving core (`net-payments` crate) is
**not** in this diff — the bindings project it.

**Method:** 8 independent finder angles (line-by-line node & python Rust,
removed-behavior audit, cross-file tracer against the `net-payments` /
`net-mcp` core, language-pitfall specialist, wrapper + golden-vector
consistency, cleanup, altitude), then reviewer re-read + verification of every
survivor against the source: the gateway result-JSON emission vs. its
doc-comments and the `ToolDescriptorJs` / `descriptor_to_camel_json`
precedents; `invoke`'s argument handling vs. `gated_invoke`
(`adapters/mcp/src/serve/gated.rs`) and the Python twin; the failure-schematic
tolerant-parse mirrors vs. `FailureSchematic::from_header_bytes`
(`sdk/src/tool_payment.rs`); the spend-profile parse sites and their
reachability through the constructor.

---

## Summary

**Status: 2 cross-language behavior divergences worth fixing before merge, plus
one test-fidelity gap and four cleanup/altitude items.** The port is otherwise
faithful and careful — the per-scheme signer wiring (eip155 / solana / xrpl) has
no mixups, monetary amounts stay opaque atomic-unit strings end-to-end (no
floats, no JS-number truncation), timestamps are `u64` nanoseconds throughout,
FFI lock discipline clones `Arc`s out before every `await` (no guard across
await), there are no `unwrap`/`panic!` on caller input, base64 is `STANDARD` on
both sides, the fail-closed payment gate ("a paid capability with no flow
configured is denied, never served unpaid") is preserved in both languages, and
the removed-behavior audit found no dropped guard, weakened test, or flipped
default (CI feature lists and validation only widened).

The two divergences (PS-1, PS-2) are silent node-vs-python behavior differences
on the consent / invocation path — the kind the golden vectors were built to
prevent but don't cover, because the vectors pin *wire objects* and not the
bindings' hand-rolled *status-JSON* projections (PS-6).

| ID | Severity | Title | Verdict |
| --- | --- | --- | --- |
| PS-1 | HIGH | Gateway result JSON is snake_case, but the doc-comments (and the rest of the node binding) promise camelCase — a JS consumer following the JSDoc reads `undefined`, a fail-open consent hazard | `[CONFIRMED]` |
| PS-2 | HIGH | Node `invoke()` rejects JSON `null` arguments, diverging from the SDK's deliberate `null → {}` normalization *and* from the Python twin (which forwards it) | `[CONFIRMED]` |
| PS-3 | MEDIUM | Failure-schematic golden mirrors (node/python/go) over-accept wrong-typed optional fields vs. the Rust reference, and no fixture exercises the case | `[CONFIRMED]` |
| PS-4 | LOW | Spend-profile parsing is triplicated per language and one copy fails *open* (unknown → `Production`); currently unreachable but fragile | `[CONFIRMED]` |
| PS-5 | CLEANUP | Node `publishPaidTools` copy-pastes the whole `spawn_publish_tools` body; Python parameterized one helper instead | `[CONFIRMED]` |
| PS-6 | ALTITUDE | Status-vocabulary / result-JSON projection is hand-rolled per binding, uncovered by golden vectors — the mechanism behind PS-1..PS-3 and one already-present message drift | `[CONFIRMED]` |
| PS-7 | LOW | Duplicate-key JSON resolution is unpinned and diverges (Rust first-wins vs. last-wins elsewhere) | `[PLAUSIBLE]` |

**Legend:** `[CONFIRMED]` = reviewer re-read the code and reproduced the logic
path. `[PLAUSIBLE]` = mechanism is real, trigger is edge/parser-version
dependent.

---

## Checked and clean (not findings)

- **Money & time path.** Monetary amounts are opaque atomic-unit strings
  (`"2500"`), never floats or JS numbers; core uses `u128 AtomicAmount`.
  Timestamps are `u64` ns end-to-end (`now_ns() -> u64`,
  `spent_today(..., now_ns: u64)`) — no `as i64`, no truncation.
- **Signer scheme wiring.** node & python both map `signer→"eip155"`,
  `signer_svm→"solana"`, `signer_xrpl→"xrpl"`; svm returns base64, xrpl returns
  hex; each error message and namespace matches its scheme. No copy-paste swap.
- **FFI concurrency.** `parking_lot` guards are always cloned out before
  `.await` (node `live_handles`, `fetch_paid`); python uses the established
  `py.detach` + `block_on` / `runtime.spawn` + `future_into_py` patterns; the
  signer `spawn_blocking` + `Python::attach` bridge releases the GIL and cannot
  deadlock. No `unwrap`/`panic!` on caller-controlled input.
- **Fail-closed publish.** `publish_paid_tools` rejects empty pricing and any
  unpriced tool in both languages; the free path folds empty pricing + `None`
  admission, so a paid tool cannot leak onto the free path.
- **Removed-behavior audit.** `std::sync::Mutex → parking_lot::Mutex` in the
  test helper is behavior-neutral; `ci.yml` only *adds* `publish,payments,
  payments-http`; python `mesh_publish_tools` was split into a
  signature-compatible wrapper + `_configured` (caller unchanged); the negative
  signer tests were *extended*, not weakened; `permissive_channels` defaults to
  `false` (preserves the old always-installed ACL).
- **Base64 / encoding.** Settlement base64 is `STANDARD` on both sides, matching
  the x402 core; no base64url drift. The canonical writer rejects floats and
  sorts keys bytewise (ASCII keys, so JS `.sort()` agrees — documented, with the
  Rust verifier as tie-breaker).
- **`.pyi` / `errors.ts` signatures.** All new constructor kwargs, arg order,
  optionality, and return types match the Rust exports; the new `GatewayError`
  class + `gateway:` prefix classify correctly.

---

## HIGH

### PS-1 — Gateway result JSON is snake_case, but its docs (and the rest of the node binding) promise camelCase `[CONFIRMED]`

**Location:** `net/crates/net/bindings/node/src/capability_gateway.rs` —
doc-comments at `:609`, `:622`, `:681`, `:692`; emission at `:96-97`, `:190`,
`:375`, `:391`.

The gateway method doc-comments — which napi propagates into the generated
`.d.ts` JSDoc that TypeScript consumers read in-editor — advertise **camelCase**
keys:

- `:609` — `search()`: "each row carries `requiresApproval`"
- `:622` — `describe()`: "the full schema + `requiresApproval` + `pricingTerms`"
- `:681`, `:692` — `approvePayment()` / `rejectPayment()`:
  `{"status":"ok","quoteId":...,"changed":bool}`

But the methods emit **snake_case**, because the result is a hand-built
`json!({...})` string and napi does not rewrite keys inside a returned string:

```rust
"requires_approval": requires_approval,   // :190  (search rows)
"pricing_terms": d.pricing_terms,         // :97   (describe)
"quote_id": quote_id,                     // :375 / :391 (approve / reject)
```

**Failure scenario:** a JS consumer follows the JSDoc and writes
`if (row.requiresApproval) promptForApproval()`. The real key is
`requires_approval`, so `row.requiresApproval` is `undefined` (falsy) → **a
consent-gated capability is treated as not requiring approval and invoked
without a human prompt (fail-open).** Likewise `JSON.parse(await
gw.approvePayment(id)).quoteId` is `undefined` (real key `quote_id`), silently
breaking any operator flow that echoes/re-approves by that field, while
`.changed` still reads correctly so the mistake is easy to miss.

This is also an **internal** inconsistency: the node binding camelCases the same
concept everywhere else. `listTools()` returns `ToolDescriptorJs` whose
`pricing_terms` field is auto-camelCased by napi to `pricingTerms` (`tool.rs:49`,
pinned by `descriptor_to_js_carries_pricing_terms`), and `watchTools()` emits
`pricingTerms` via `descriptor_to_camel_json` (`tool.rs:305` test). So the same
field is `pricingTerms` on the tool-list/watch paths and `pricing_terms` on
`gateway.describe()`.

**Fix:** pick one regime and align docs + tests to it. Either camelCase the
gateway JSON (matches node convention, the docs, and the `descriptor_to_camel_json`
precedent) or correct every gateway docstring to snake_case (matches Python, for
cross-language parity). Given the fail-open risk on `requiresApproval`, prefer
whichever the consuming agents already assume, and add a test that pins the
emitted key names.

### PS-2 — Node `invoke()` rejects JSON `null` arguments, diverging from the SDK contract and the Python twin `[CONFIRMED]`

**Location:** `net/crates/net/bindings/node/src/capability_gateway.rs:665`;
contract at `adapters/mcp/src/serve/gated.rs:104-113`; Python twin at
`net/crates/net/bindings/python/src/capability_gateway.rs:937`.

Node's `invoke()` short-circuits any non-object JSON before dispatch:

```rust
// capability_gateway.rs (node) :662-670
if !args.is_object() {
    return Ok(err_json("invalid_arguments", "arguments must be a JSON object"));
}
```

But the SDK's `gated_invoke` — "the one place every demand-side caller routes
through" — **deliberately** treats `null` as a valid no-argument invocation:

```rust
// gated.rs :104-113
// A no-argument invocation can arrive as JSON `null`: the host omitted
// `arguments` on a promoted pinned tool ... normalize `null` to `{}` here.
let tool_args = if tool_args.is_null() { json!({}) } else { tool_args };
```

The Python `invoke` (`:937`) parses `arguments_json` and forwards the `Value`
straight to `do_invoke` with **no `is_object()` check** (pinned by
`requires_payment_approval_passes_through_untouched` and friends).

**Failure scenario:** invoking a zero-argument capability with an explicit
`null` (e.g. a promoted pinned tool where the host omitted `arguments`, which
deserializes to `Value::Null`) **succeeds via the SDK and the Python binding but
fails on Node** with `{"status":"invalid_arguments"}`. The `[]` / `true` / `"x"`
cases likewise return `invalid_arguments` on Node vs. the gate's own
`validation_error` (from `validate_args`) on Python — different status strings
for the same input.

**Fix:** drop the `is_object()` short-circuit and let `gated_invoke` normalize
`null → {}` and validate the rest against the schema, matching the SDK and
Python. (If a caller-shape pre-check is genuinely wanted, push it into the shared
SDK so all three languages agree.)

---

## MEDIUM

### PS-3 — Failure-schematic golden mirrors over-accept wrong-typed optional fields `[CONFIRMED]`

**Location:** `net/crates/net/bindings/node/test/payments_golden_vectors.test.ts:89`
(`hasSchematicShape`); twins at
`net/crates/net/bindings/python/tests/test_payments_golden_vectors.py:132`
(`_has_schematic_shape`) and `go/payments_golden_vectors_test.go:318`
(`paymentsHasSchematicShape`). Reference:
`net/crates/net/sdk/src/tool_payment.rs:262-264`, `:287`.

The fixture's stated tolerance contract is that each language mirrors
`FailureSchematic::from_header_bytes`, which is a **full typed serde
deserialize**:

```rust
// tool_payment.rs :287
let parsed: Self = serde_json::from_slice(bytes).ok()?;
(parsed.object == TAG_PAYMENT_FAILURE).then_some(parsed)
```

`quote_id` / `tool_id` are `Option<String>` (`:262-264`) and
`recovery.next_action` is `Option<String>` (`:180`). But the three mirrors only
type-check the **required** fields — they never inspect the typed optionals.

**Failure scenario:** a header with every required field correct but e.g.
`"quote_id": 42` (a number). Rust's `serde_json::from_slice` fails deserializing
`Option<String>` from a number → `from_header_bytes` returns `None` → **REJECT**
(fall back to the human error body). The node/python/go mirrors don't look at
`quote_id` → **ACCEPT**. Opposite verdicts, and no case in
`payment_vectors.json` has a wrong-typed optional, so the golden suite stays
green on an imperfect mirror — exactly the cross-language divergence the vectors
exist to catch.

**Fix:** add a `quote_id_wrong_type` (and `next_action_wrong_type`) fixture case
with `accepted:false`, and tighten the three mirrors to type-check the optionals
when present. Better still, expose the core `from_header_bytes` through the
bindings so there is nothing to hand-mirror (see PS-6).

---

## LOW / CLEANUP / ALTITUDE

### PS-4 — Spend-profile parsing is triplicated per language, and one copy fails open `[CONFIRMED]`

**Location (node):** `capability_gateway.rs:269` (`parse_profile`), `:349`
(`spend_engine`), `payment_http.rs:88`. **(python):**
`capability_gateway.rs:300` (`spend_engine`), `:621` (`build_payment_flow`),
`payment_http.rs:114` (`build_flow`).

The `production` / `dev_test` / `dev-test` / `devtest` → `SpendProfile`
vocabulary is hand-rolled at three sites per language, with **two different
behaviors**: `parse_profile` / `build_payment_flow` **error** on an unknown
profile; `spend_engine` (used by the operator approval verbs) **silently
defaults unknown → `Production`** (node `:352`, python `:304`).

This is currently unreachable: the constructor validates eagerly —
`new()` → `build_payment_flow(...)?` → `parse_profile(&config.profile)?`
(node `capability_gateway.rs:572` → `:329`) — before any store is opened, and
the approval verbs only run when a policy path was supplied. So a bad profile
fails construction before `spend_engine` can see it. But it is a fail-*open*
divergence one refactor away (e.g. if the flow ever builds lazily), and the
duplication means adding a profile alias or tightening validation touches six
sites.

**Fix:** hoist a single `SpendProfile::parse` into `net_payments::policy::spend`
and call it from every binding site, including `spend_engine`, so the fallible
behavior is the only behavior.

### PS-5 — Node `publishPaidTools` copy-pastes the whole `spawn_publish_tools` body `[CONFIRMED]`

**Location:** `net/crates/net/bindings/node/src/payment_provider.rs:283`; the
solved pattern is `net/crates/net/bindings/python/src/publish.rs`
(`mesh_publish_tools_configured`).

~45 lines are duplicated verbatim from `publish.rs::spawn_publish_tools`:
options defaulting, the `allowAnyCaller` overrides `ownerOrigin` rule,
`local_lowering_context`, `build_tool_invoker`, and the whole `env.spawn_future`
body (`mesh_over` → `WrapConfig::owner_only` → `OwnerScope::any` →
`ServerPublisher::publish_tools` → `LocalPublicationHandle::wrap`), adding only
pricing + `payment_admission`. Python already parameterized one helper
(`mesh_publish_tools_configured(pricing, payment_admission)`) called by both the
free and paid paths.

**Cost:** the node side now maintains publish / owner-scope logic in two places.
The recent "`allowAnyCaller` overrides invalid `ownerOrigin`" fix
(`ad5c8b5d1`) is exactly the kind of change that lands in one path and silently
misses the other.

**Fix:** parameterize `spawn_publish_tools` with `pricing` + `payment_admission`
and have `publishPaidTools` call it, mirroring the Python structure.

### PS-6 — Status-vocabulary / result-JSON projection is hand-rolled per binding, uncovered by golden vectors `[CONFIRMED]`

**Location:** node & python `payment_http.rs::outcome_to_result`; the gateway
`gateway_status` / `err_json` / `detail_to_json` / `outcome_to_json` /
approval-verb bodies; `payment_provider.rs::author_pricing_terms`.

The `X402HttpOutcome` → status-JSON projection (`fetched` / `paid` /
`requires_payment_approval` / `denied` / `provider_refused` / `transport_error`),
the gateway status strings + field names, the approval-verb result shapes, and
the pricing-terms authoring recipe are each written **twice** (node + python),
both sides Rust-consuming-the-same-core-enums. The golden vectors pin the *wire
objects* (envelopes, failure schematic, CAIP) but **not** these status-JSON
contracts, so a one-sided field rename or a new enum arm handled in only one
binding ships an undetected node/python divergence. This is the mechanism behind
PS-1, PS-2, and PS-3.

A concrete, already-present drift: the `RequiresApproval` human `message` differs
between the two bindings — Python
(`capability_gateway.rs:174-177`) includes "Request it with
net_request_capability;"; Node (`capability_gateway.rs:120-125`) dropped that
sentence. (The machine-readable fields match, so it's low-impact today, but it
proves the point.)

**Fix:** lift the projections onto the shared core types (e.g.
`X402HttpOutcome::to_status_json`, `GatedOutcome::to_status_json`,
`GatewayError::status_str`, a `PricingTerms::author(...)` helper) so both
languages derive one contract, and extend the golden vectors to pin the
status-JSON shapes.

### PS-7 — Duplicate-key JSON resolution is unpinned and diverges `[PLAUSIBLE]`

**Location:** the four verifiers /
`net/crates/net/payments/examples/gen_payments_fixtures.rs`.

A header (or envelope) repeating a key resolves **first-wins** under Rust's
`#[serde(flatten)]` deserializer path but **last-wins** under Go `encoding/json`,
JS `JSON.parse`, and Python `json.loads`. For the failure-schematic tag check, a
header like `{"object":"net.payment.failure@1", ..., "object":"net.payment.failure@2"}`
could accept under Rust (reads `@1`) and reject under the other three (read
`@2`), or vice-versa depending on order.

Extreme edge case and the exact resolution is parser-version dependent, so this
is `[PLAUSIBLE]`, not a confirmed live break. **Fix (optional):** add a
`duplicate_key` fixture case to pin the intended behavior explicitly rather than
leave it to each parser's default.

---

## Recommendation

Fix **PS-1** and **PS-2** before merge — both are silent, cross-language
behavior differences on the consent / invocation path that no test currently
catches. **PS-3** and **PS-7** are cheap fixture additions that would have caught
this class of bug. **PS-4/5/6** are the deeper remedy: move the per-language
projections and parsers into the shared `net-payments` core so the node and
python bindings can't drift, and let the golden vectors pin the status-JSON — not
just the wire objects.
