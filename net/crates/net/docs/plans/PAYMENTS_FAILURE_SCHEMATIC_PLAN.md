# Implementation Plan: Payments — Failure schematic (machine-actionable failure semantics)

**Implements:** the 2026-07-08 brainstorm ([`docs/BRAINSTORMING.md`](../BRAINSTORMING.md)) — primary error stays human/actionable, a structured **failure schematic** rides alongside it so agents, demos, and reviews can see *which invariant the system protected* and *what recovery is allowed*. Kyra's framing, adopted verbatim as the thesis: error as routing instruction, not dead end.

**The sentence:** every payment refusal carries, next to its human message, a versioned machine-readable verdict — code, stage, reason, recovery — built from the engine's *typed* decision at a single render site, never parsed back out of strings.

**Review (2026-07-08, Kyra): approved — "build it"** — with tightening edits, all incorporated below before WS-1 froze anything:

- The field set stays close to the original v1 shape (`retryable`, `recovery.safe_to_retry`, `funds_moved` — no `current_attempt_charged`/`funds_status` variants): ambiguity is resolved by **definitions, not renames**. "For v1, clarity beats perfect ontology."
- `retryable` vs `recovery.safe_to_retry` pinned: coarse operation verdict vs recovery instruction (Retry semantics block below).
- `funds_moved` pinned: the payment state known for this quote/proof — never a fresh charge caused by the rejected invocation.
- One field survived the minimalism pass on a second look: `prior_payment` (`none | pending | consumed | unknown`) — the **instrument**-lifecycle fact beside `funds_moved`'s **money** fact. It routes recovery in ways the money fact can't: `none` → pay it, `pending` → wait, `consumed` → requote.
- `engine_unavailable` → actor `provider_operator`, conservative class — the scrub can't distinguish transient from broken, so don't overpromise recovery.
- `binding_malformed` → conservative (a new quote doesn't fix a client serialization bug).
- `quote_frozen` → `non_recoverable` (a freeze often signals replay/wrong-chain; typed freeze subreasons reserved).
- `not_settled` split from `settlement_pending` — "never paid" and "paid, awaiting confidence" route differently; the record's event chain distinguishes them typed.
- Reserved reason vocabulary recorded now so future surfaces align; header discipline pinned (exactly one; duplicates/malformed → human-error fallback).

---

## Ground truth (as researched 2026-07-08)

What exists today, with receipts — the design leans on all four:

1. **The taxonomy is already typed at the source.** The engine decides in enums, not strings: `RejectReason` (8 variants, `engine/mod.rs`), `InvalidationReason` (5), `ExceptionKind`, `FacilitatorErrorKind` (4, with a `retryable` bit), `PendingTier{reached, required}`. Only the redeem gate (`redeem_for_invocation` — 8 distinct denial conditions) and freeze reasons are bare `String`s, each produced at exactly one site. Promotion, not invention.
2. **The wire already has the channel.** The nRPC response frame is `status u16 + headers + body` (`cortex/rpc.rs`); error replies *can* carry headers per the wire layout — the code just hardcodes `headers: vec![]` on every error fold arm and discards `resp.headers` when building `RpcError::ServerError`. Three source edits, zero wire change, old/new peers interoperate.
3. **Every projection surface is flat text today.** ERR_PAYMENT (0x8006) degrades to a UTF-8 body → `GatewayError::Denied(String)` → MCP `CallToolResult::text_error` (whose `structured_content` field exists and is `None` on every error) → Python `{status: "denied", error: "..."}`. No consumer parses structure from errors — we are introducing the first one, so the shape is ours to pin.
4. **Both redeem gates already render through one function.** `flow::redeem_via_engine` is the single-sourced engine→gate mapping for the native (`ToolPaymentGate`) and MCP (`PaymentAdmission`) paths (`2019a158a`), and it is the one place `EngineError` is scrubbed to `"payment engine unavailable (fail-closed)"`. The schematic renders there, inheriting both properties for free.

**Doctrine this plan must not bend:** fail-closed scrubbing (no paths, no serde detail, no facilitator bodies in anything caller-facing — the store-corruption test pins it); billing events exist only when money moved (a failure schematic is an ephemeral verdict, never a `BillingLog` entry; `net.payment.dispute@1` stays reserved for P5); byte-preservation (the schematic is a *Net* object under the `versioning.rs` regime — it never embeds re-serialized x402 material); views tolerate unknown fields.

---

## The object: `net.payment.failure@1`

A versioned JSON object in house style (`object` tag first, `#[serde(default, skip_serializing_if)]` optionals, `#[serde(flatten)] extra` for additive forward-compat). **Unsigned in v1** — it is a diagnostic verdict traveling inside an authenticated mesh session, not an accounting record; signing is an `extra`-compatible later addition if a use case demands it.

```json
{
  "object": "net.payment.failure@1",
  "code": "payment",
  "stage": "redeem",
  "reason": "already_redeemed",
  "message": "quote already redeemed — one payment, one serve",
  "retryable": false,
  "recovery": {
    "class": "new_quote_required",
    "actor": "caller_agent",
    "safe_to_retry": false,
    "safe_to_requote": true,
    "next_action": "request_new_quote"
  },
  "handler_executed": false,
  "funds_moved": "yes",
  "prior_payment": "consumed",
  "quote_id": "q_…",
  "tool_id": "paid_echo"
}
```

Field rules:

- `code` — stable top-level family, string. v1 ships `"payment"` only (maps to the 0x8006 band); the shape deliberately generalizes to `policy`/`approval`/`delegation` later without a new object.
- `stage` — where in the lifecycle the refusal fired: `admission | quote | claim | verify | settle | completion | redeem` (provider-side, v1) plus reserved caller-side values `authoring | caller_policy` (produced by the demand side in a later phase, not v1).
- `reason` — specific, snake_case, **string-typed on the wire**. Producers use Rust enums (no typos); consumers must tolerate unknown reasons (forward compat — new reasons are additive within `@1`).
- `message` — the same human string that rides the error body, truncated to a fixed cap (proposed 512 B) inside the schematic; the body carries it in full. Single-sourced from the same `Display` so the two can never disagree.
- `recovery.class` — one of `automatic_retry | payment_required | new_quote_required | user_action_required | operator_approval_required | provider_configuration_error | caller_configuration_error | network_transient | security_violation | non_recoverable`. `payment_required` = "the quote exists — pay it, then retry"; a routing distinct from requoting.
- `recovery.actor` — who can *fix* it: `caller_agent | caller_user | caller_operator | provider_operator`.
- `handler_executed` — always `false` for anything these stages refuse: the invariant, stated as data.
- `funds_moved` — `no | yes | unknown`: the **money** fact — whether the payment associated with this quote/proof is known to have moved funds. **Never** a fresh charge caused by this rejected invocation (a refusal never charges). `unknown` is deliberate on binding-failure rows: a failed possession proof learns nothing about payment state.
- `prior_payment` — `none | pending | consumed | unknown`: the **instrument** fact — the lifecycle state of the payment attached to the quote. `none` = no settlement recorded (never paid); `pending` = settled, not yet billed (awaiting tier/re-verify, or held as an exception); `consumed` = billed and redeemed; `unknown` = the record can't say, and deliberately on binding-failure rows. The pair answers the agent's two distinct questions: *did money move?* and *can I still use this instrument?*

### Retry semantics

- `retryable` — the coarse verdict for the failed operation: whether retrying it may succeed **without changing configuration or user/operator state**.
- `recovery.safe_to_retry` — the recovery instruction: whether retrying the same attempt is part of the recommended recovery.
- `recovery.safe_to_requote` — the agent may request a fresh quote and attempt a new payment. It does not imply the current proof can be reused; `false` on security rows means *do not just buy another quote and try again*.
- **Redaction (Kyra's avoid-list, promoted to contract):** no bearer material, no key references beyond names, no payment blobs, no filesystem paths, no serde/transport detail, no facilitator response bodies. The schematic is built **only from typed fields** of the engine decision — never by inspecting an `EngineError`.

## Carrier: a reply header, not a JSON body

The human message stays the error body, byte-for-byte as today — every existing consumer, log line, and `err.contains(...)` test keeps working. The schematic rides a new reply header:

- `HDR_FAILURE_SCHEMATIC = "net-failure-schematic"` (constant next to `HDR_PAYMENT_QUOTE` in `net_sdk::tool_payment`), value = the schematic JSON bytes.
- Header values cap at 4096 B on the wire — adopted as a *feature*: the schematic must stay small, ids/hashes ride truncated where needed, and a max-size test pins that the largest producible schematic fits.
- **Header discipline (review-pinned):** producers emit **exactly one** schematic header, value = raw JSON bytes, single-encoded (never a JSON string containing JSON). Consumers: more than one `net-failure-schematic` header → ignore the schematic entirely and fall back to the human error (no ambiguity to exploit); malformed JSON or invalid UTF-8 → the same fallback. Both rules tested in WS-4.
- Rejected alternative, recorded: JSON-as-body (the `ERR_TOOL` precedent). It works only when every reader knows the trick from day one; here it would turn the primary message into JSON on every legacy surface, violating "primary = human".

---

## Workstream 1 — vocabulary + the object

The types, before any wiring.

- [ ] `FailureSchematic` (+ `Recovery`) in `net_sdk::tool_payment` — serde struct per the shape above, `TAG_PAYMENT_FAILURE = "net.payment.failure@1"`, `HDR_FAILURE_SCHEMATIC`. The SDK already owns the wire vocabulary (ERR_PAYMENT, header names); this is wire vocabulary, not payment parsing — the "SDK never verifies payments" doctrine holds. Cross-reference the tag from `payments/src/core/versioning.rs` prose so the registry stays discoverable.
- [ ] `GateDenial { message: String, schematic: FailureSchematic }` in the same module — the new refusal type for both gate traits (WS-3 changes the signatures; the type lands first).
- [ ] Payments core: promote the redeem-gate strings to `RedeemDenialReason` (`unknown_quote | binding_malformed | binding_rejected | payer_record_corrupt | quote_frozen | not_settled | settlement_pending | wrong_tool_binding | already_redeemed`); `RedeemDecision::Denied` gains the typed reason. **`Display` preserves today's exact strings** — wire messages and every existing assertion stay put — with one review-sanctioned exception: the `not_settled`/`settlement_pending` split (typed at the source: `rec.chain` empty vs non-empty while `billing` is `None`) mints a *new* message for the pending case; the never-paid case keeps today's string verbatim.
- [ ] Pin the reason↔recovery mapping table (draft below) as committed prose next to the types. It is a **caller-facing contract reviewed like a money-path decision** — agents will branch on it.
- [ ] Golden JSON fixture for the schematic (the `gen_payments_fixtures.rs` idiom), plus a tolerance test: a schematic with an unknown `reason`/extra fields deserializes fine.

Mapping (v1, redeem + admission stages — tightened per review):

| reason | stage | class | actor | retryable | safe_to_retry | safe_to_requote | funds_moved | prior_payment |
|---|---|---|---|---|---|---|---|---|
| `missing_quote` | admission | `new_quote_required` | caller_agent | false | false | true | no | none |
| `gate_missing` | admission | `provider_configuration_error` | provider_operator | false | false | false | no | none |
| `unknown_quote` | redeem | `new_quote_required` | caller_agent | false | false | true | no | none |
| `binding_malformed` | redeem | `caller_configuration_error` | caller_operator | false | false | false | unknown | unknown |
| `binding_rejected` | redeem | `security_violation` | caller_operator | false | false | false | unknown | unknown |
| `payer_record_corrupt` | redeem | `provider_configuration_error` | provider_operator | false | false | false | unknown | unknown |
| `quote_frozen` | redeem | `non_recoverable` | caller_operator | false | false | false | unknown | unknown |
| `not_settled` | redeem | `payment_required` | caller_agent | true | true | true | no | none |
| `settlement_pending` | redeem | `automatic_retry` | caller_agent | true | true | true | unknown | pending |
| `wrong_tool_binding` | redeem | `security_violation` | caller_operator | false | false | false | unknown | unknown |
| `already_redeemed` | redeem | `new_quote_required` | caller_agent | false | false | true | yes | consumed |
| `engine_unavailable` | redeem | `provider_configuration_error` | provider_operator | true | true | true | unknown | unknown |

Row notes (the review's reasoning, kept next to the contract): `binding_malformed` is a client serialization bug — a new quote doesn't fix it (`next_action: fix_payment_client`). `quote_frozen` often signals replay/wrong-chain/reorg — "get a new quote" understates it; typed freeze subreasons (`quote_frozen_replay | _wrong_chain | _reorg | _amount`) are **reserved** pending a typed freeze tag in the store record. `engine_unavailable`'s actor is the provider (the caller can't fix engine availability); retry is permitted but nothing stronger is promised — the scrub can't distinguish transient from broken. `not_settled` vs `settlement_pending`: the record's event chain distinguishes "never paid" (pay the quote, then retry) from "paid, awaiting confidence" (wait and retry after re-verification).

Reserved reasons (documented now, no v1 producer — future surfaces must use these names, per review): `insufficient_funds`, `no_wallet_configured`, `network_not_allowed`, `quote_expired`, `tier_below_required`, `checker_unavailable`, `facilitator_rejected` — the caller-side authoring and pay-path stages of WS-5.

**Acceptance:** the object round-trips through serde with golden bytes; unknown-reason tolerance proven; the mapping table is committed prose; nothing is wired yet and the full suite is untouched.

## Workstream 2 — substrate: error replies carry headers

The three edits the wire already permits (`cortex/rpc.rs` + adapter `mesh_rpc.rs` — substrate, reviewed accordingly):

- [x] ~~`RpcHandlerError::Application` gains `headers: Vec<RpcHeader>`~~ ~~The response fold carries those headers through~~ **Resolved at build: unnecessary.** The fold's `Ok(payload)` arm passes a handler-authored `RpcResponsePayload` through **verbatim** — status, headers, and body — so an error reply with headers was already expressible: `RpcHandlerError` is only a *convenience* channel ("doesn't fit the application's normal `Ok(RpcResponsePayload)` channel", its own doc). WS-3's producers author refusals as `Ok(payload)` with the Application status + schematic header; the convenience channel stays header-less by design. This removed both planned substrate edits and the churn at the ~54 `Application` construction sites across core, tests, and three bindings.
- [x] Caller side: `RpcError::ServerError` gains `headers: Vec<(String, Vec<u8>)>` instead of discarding `resp.headers` — populated at all four wire-mapping sites (unary, client-streaming finish, both stream error arms). The one public-enum break, taken at once: ~30 mechanical updates (matches gain `..`, test constructions gain `headers: vec![]`) across core tests, SDK tests, the MCP gateway, and the go/python/node bindings.
- [x] Tests: `rpc_error_replies_carry_headers_to_the_caller` (`integration_nrpc_mesh`) — the full-fidelity channel round-trips a schematic-shaped header byte-intact over a live pair; the legacy convenience channel yields empty headers (both interop directions). Cap enforcement is untouched code (encode path unchanged), covered by the existing codec tests.

**Acceptance:** a handler can attach reply headers to an application error and the caller observes them; zero wire-format change (byte layout untouched — proven by the existing codec tests still passing unmodified). ✅ — met with zero *substrate* change at all: the server half always could; the caller just stopped discarding.

> **Landed** with the plan's central assumption inverted in the cheap direction: the plan budgeted three substrate edits, but the server half needed *nothing* — only the caller discarded error-reply headers. One residual noted for WS-3: producers switching from the convenience channel to `Ok(payload)` must keep the message in the body themselves (the fold no longer does it for them).

## Workstream 3 — producers: render once, attach everywhere

- [x] Both gate traits change refusal type: `ToolPaymentGate::redeem` and `PaymentAdmission::redeem` return `Result<(), GateDenial>`. Two real impls (`EngineToolPaymentGate`, `EnginePaymentAdmission`) plus scripted test gates — compiler-driven, small blast radius (one extra consumer surfaced: the `mcp_gate_composition` test's gateway mirror).
- [x] `flow::redeem_via_engine` renders `RedeemDenialReason` → `FailureSchematic` — the **single render site** for both gates (`denial_for` + `engine_unavailable_denial`, message and schematic minted together from the typed reason). The `Err(EngineError)` arm produces the fixed `engine_unavailable` schematic **from nothing but the generic verdict**; the store-corruption test now asserts the *schematic's serialized bytes* leak no path/"corrupt"/parser detail either.
- [x] `PaidToolHandler` (SDK): missing-header arm authors `FailureSchematic::missing_quote` (constructor lives with the type); both refusal arms attach the schematic via `HDR_FAILURE_SCHEMATIC`; body message unchanged. *(WS-2 residual honored: refusals ride the full-fidelity `Ok(payload)` channel — the handler keeps the message in the body itself.)*
- [x] MCP wrap `invoke.rs`: same treatment for its three arms (`gate_missing`, `missing_quote`, gate denial pass-through) via a wire-identical `payment_refusal` twin.
- [x] Size honesty: `every_redeem_denial_renders_within_the_header_budget` renders all nine typed reasons (including a fat repeated freeze-reason) + `engine_unavailable` and asserts each encodes under the 4096 B cap; an over-budget schematic degrades to message-alone (`header_entry` → `None`), never truncated JSON.
- [x] Ordering unchanged and re-asserted: bad body still refuses **before** the gate (no schematic minted — asserted — and the quote untouched).

**Acceptance:** `tool_serve_paid.rs` and the MCP invoke tests observe, on every payment refusal, an unchanged human message **plus** a decodable schematic whose `reason` matches the typed cause; the scrubbing test passes with its new schematic assertions. ✅ — plus the risk-table pin landed as a unit test: security rows assert `retryable == safe_to_retry == safe_to_requote == false` with no `next_action`.

## Workstream 4 — projections

- [ ] Demand-side MCP gateway: on `RpcError::ServerError`, decode `HDR_FAILURE_SCHEMATIC` **tolerantly** (absent/malformed → behave exactly as today); `GatewayError::Denied` carries `Option<FailureSchematic>`; the shim surfaces it as the error `CallToolResult`'s `structured_content` while `text` stays the human `denied_message`. Kyra's "compact primary + expandable detail", materialized in the field MCP already has.
- [ ] Python bindings: the outcome JSON gains `"failure": {…schematic…}` beside the existing `error` string when present; the `status` vocabulary is untouched.
- [ ] Logs: the emission point gains structured `tracing` fields (`reason`, `stage`, `recovery_class`) so operators grep verdicts, not prose.
- [ ] Tests: end-to-end MCP — a denied paid call yields `is_error: true`, human text, and `structured_content.object == "net.payment.failure@1"`; a legacy provider (no header) yields today's exact behavior; duplicate or malformed schematic headers → schematic ignored, human path intact (the discipline rule); Python round-trip for the `failure` field.

**Acceptance:** an agent driving the MCP surface can branch on `reason`/`recovery.class` without string-matching prose; every surface degrades gracefully against peers that predate the header.

## Workstream 5 — deferred, with entry notes (recorded, not built)

- **Pay-path recovery fields:** `PayResponse` (`flow/mod.rs`) is already serde-tagged structured JSON — align its vocabulary with the schematic's and add `recovery` there rather than wrapping it in a second object. Do after v1 proves the recovery taxonomy.
- **Caller-side authoring failures** (`insufficient_funds`, `no_wallet_configured`, `network_not_allowed`, approval-required — the brainstorm's demand-side rows): produced by the HTTP-402 door / Python gateway / spend-policy layer using the *same* object with the reserved `authoring`/`caller_policy` stages. Entry: when the Python HTTP-402 surface work (deferred N4a) lands.
- **Failure event log:** an append-only record of refusals for provider analytics. Explicitly **not** `BillingLog` (doctrine: billing events only when money moved) and not `net.payment.dispute@1` (reserved, P5). Entry: demand-driven.
- **CLI `--explain` / Hermes rendering:** projection-only work on surfaces outside this repo's current scope.
- **Signing the schematic:** only if a schematic ever needs to outlive its session as evidence; `extra`-compatible.

---

## Rollout order

1. **WS-1** first (pure types + prose; reviewable in isolation, nothing behavioral).
2. **WS-2** in parallel if staffed separately (substrate-only, no payments knowledge needed) — otherwise second.
3. **WS-3** once both land (it consumes the types and the channel).
4. **WS-4** last; each projection is independently shippable.

## Non-goals (v1)

Caller-side producers (deferred with entry notes above), any change to the human message strings, any new `RpcStatus` codes, schematic on success paths, persistence of failures, signing, `code` families beyond `"payment"`, HTTP-402 door surfaces, retiring the `ERR_TOOL` body-JSON precedent (it predates this and stays).

## Risks

| Risk | Containment |
|---|---|
| Schematic leaks internal detail (paths, serde, facilitator bodies) | Built only from typed decision fields at one render site; the `EngineError` arm renders from the generic verdict by construction; store-corruption scrub test extended to the schematic; redaction list is contract, reviewed caller-facing |
| Reason strings become API and then churn | That is the point — but vocabulary is additive-only within `@1`, consumers must tolerate unknowns (tested), breaking change mints `@2` per `versioning.rs` doctrine |
| 4096 B header cap overflows on a pathological schematic | Message capped at 512 B inside the object, ids truncated, max-size test pins the largest producible schematic under the cap |
| Substrate edits destabilize the RPC core | Byte layout untouched (headers are already in the response wire spec); edits are fold/enum plumbing; both interop directions tested; existing codec tests must pass unmodified |
| `RpcError::ServerError` / gate-trait breaks ripple to unknown consumers | Public-enum and trait changes taken once, compiler-driven; two real gate impls in-repo; Python binding consumes `GatewayError`, not `RpcError`, and is updated in WS-4 |
| Schematic and human message drift apart | `message` single-sourced from the same `Display` at the render site; a test asserts body == schematic message (modulo the length cap) |
| Recovery advice is wrong and an agent acts on it (e.g. retries a security violation) | The mapping table is reviewed as a money-path decision before WS-3 wires it; `security_violation` rows pin `safe_to_retry == safe_to_requote == false` in tests |
