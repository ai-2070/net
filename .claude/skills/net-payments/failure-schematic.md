# The failure schematic — a machine-actionable verdict on denials

When a paid invocation is refused, the caller gets **two renderings of the
same verdict**: the human message (the error body — byte-identical to what the
wire has always carried) and, for schematic-aware callers, a structured
`net.payment.failure@1` object that says *which invariant refused, who can fix
it, and what recovery is safe*. Agents branch on the schematic instead of
grepping prose.

This is an **SDK wire object, not a payments envelope.** It lives in
`net_sdk::tool_payment` (unsigned, additive within `@1`), and `net-payments`'
`core/versioning.rs` cross-references the tag. It is not signed, not carried in
an `X402Carry`, and not part of the five Net envelopes (`object-model.md`).

## Where it rides

A refusal carries the schematic beside — **never instead of** — the human
message:

- **The human message** is the error body: `ERR_PAYMENT` (0x8006) application
  error, byte-identical to the pre-schematic wire.
- **The schematic** is the JSON bytes of a `FailureSchematic` on the
  `net-failure-schematic` reply header (`HDR_FAILURE_SCHEMATIC`).

```rust
pub const HDR_FAILURE_SCHEMATIC: &str = "net-failure-schematic";
pub const TAG_PAYMENT_FAILURE:   &str = "net.payment.failure@1";
```

## The object

```rust
pub struct FailureSchematic {
    pub object: String,          // always "net.payment.failure@1"
    pub code:   String,          // top-level family; v1 ships "payment"
    pub stage:  String,          // where it fired: "admission" | "redeem"
    pub reason: String,          // the specific verdict, snake_case (additive)
    pub message: String,         // redaction-safe copy of the human message (≤512 B)
    pub retryable: bool,         // coarse: could a retry succeed with no config/state change?
    pub recovery: Recovery,
    pub handler_executed: bool,  // always false for a refusal — the invariant, as data
    pub funds_moved:   String,   // MONEY fact:      "no" | "yes" | "unknown"
    pub prior_payment: String,   // INSTRUMENT fact: "none" | "pending" | "consumed" | "unknown"
    pub quote_id: Option<String>,
    pub tool_id:  Option<String>,
    pub extra: BTreeMap<String, Value>,   // unknown fields from newer producers, preserved
}

pub struct Recovery {
    pub class: String,           // CLASS_* — e.g. "new_quote_required", "security_violation"
    pub actor: String,           // ACTOR_* — who can fix it: caller_agent | caller_user | ...
    pub safe_to_retry:   bool,   // is retrying the SAME attempt part of recovery?
    pub safe_to_requote: bool,   // is a fresh quote + new payment sanctioned?
    pub next_action: Option<String>,   // advisory snake_case hint
}
```

Two facts are deliberately separate: `funds_moved` is a **money** fact (did the
payment behind this quote move funds?) and `prior_payment` is an **instrument**
fact (lifecycle of the payment attached to the quote). A refusal never charges,
so `funds_moved` is never a fresh charge caused by the rejected invocation.

Retry vocabulary, pinned: `retryable` is the *coarse verdict*;
`recovery.safe_to_retry` is the *recovery instruction* (is retrying the same
attempt recommended?); `recovery.safe_to_requote` means a fresh quote is
sanctioned — it never implies the current proof can be reused, and `false` on a
security row means *do not just buy another quote and try again*.

## The v1 reason → recovery mapping (the caller-facing contract)

| reason | stage | class | retryable | safe_to_retry | safe_to_requote | funds_moved | prior_payment |
|---|---|---|---|---|---|---|---|
| `missing_quote` | admission | new_quote_required | ❌ | ❌ | ✅ | no | none |
| `gate_missing` | admission | provider_configuration_error | ❌ | ❌ | ❌ | no | none |
| `unknown_quote` | redeem | new_quote_required | ❌ | ❌ | ✅ | no | none |
| `binding_malformed` | redeem | caller_configuration_error | ❌ | ❌ | ❌ | unknown | unknown |
| `binding_rejected` | redeem | security_violation | ❌ | ❌ | ❌ | unknown | unknown |
| `payer_record_corrupt` | redeem | provider_configuration_error | ❌ | ❌ | ❌ | unknown | unknown |
| `quote_frozen` | redeem | non_recoverable | ❌ | ❌ | ❌ | unknown | unknown |
| `not_settled` | redeem | payment_required | ✅ | ✅ | ✅ | no | none |
| `settlement_pending` | redeem | automatic_retry | ✅ | ✅ | ✅ | unknown | pending |
| `wrong_tool_binding` | redeem | security_violation | ❌ | ❌ | ❌ | unknown | unknown |
| `already_redeemed` | redeem | new_quote_required | ❌ | ❌ | ✅ | yes | consumed |
| `engine_unavailable` | redeem | provider_configuration_error | ✅ | ✅ | ✅ | unknown | unknown |

**Reserved reasons** (documented, no v1 producer — future surfaces must use
these names): `insufficient_funds`, `no_wallet_configured`,
`network_not_allowed`, `quote_expired`, `tier_below_required`,
`checker_unavailable`, `facilitator_rejected`. Reserved freeze subreasons:
`quote_frozen_{replay,wrong_chain,reorg,amount}`.

## The discipline rule (both halves)

- **Producers** emit **exactly one** schematic header, raw JSON bytes,
  single-encoded. It is rendered **once** — `net_payments::flow::redeem_via_engine`
  is the single render site for engine denials (`denial_for` + the
  admission-side `missing_quote`/`gate_missing` constructors) — never parsed
  back out of a string. Over-budget (>4096 B) → the producer sends the human
  message alone; the schematic is a sidecar, never worth failing a reply over.
- **Consumers** are **tolerant**: `FailureSchematic::from_header_bytes` returns
  the schematic only for well-formed JSON tagged `net.payment.failure@1`.
  Duplicate headers, malformed bytes, invalid UTF-8, a foreign/`@2` tag, or a
  non-object all read as **absent** — fall back to the human error body, never
  an error, never a guess. Unknown reasons and extra fields parse fine (the
  additive-tolerance contract).

## Redaction is contract

Built **only** from typed decision fields — never by inspecting an engine
error. No bearer material, no key references beyond names, no payment blobs, no
filesystem paths, no serde/transport detail, no facilitator response bodies.
The store-failure path (`engine_unavailable`) is rendered from the generic
verdict, so an on-disk path or parser detail can't leak through the schematic.

## How each surface projects it

The verdict is rendered once and consumed by every serving path — the gate can
never fork between them:

- **SDK-native paid serve** (`Mesh::serve_tool_paid` via `EngineToolPaymentGate`)
  and **MCP wrap** (`EnginePaymentAdmission`) both attach the header on refusal.
- **MCP consumers** get it projected into `structured_content` on the denied
  `CallToolResult`.
- **Python** (`CapabilityGateway.invoke`) surfaces it as a `failure` object
  beside `error` on a `denied` result — branch on `failure["reason"]` /
  `failure["recovery"]` (`bindings.md`). Absent `failure` = the refusal carried
  no schematic.
- **Operators** grep the `redeem_via_engine` `tracing::info!` emit, whose
  **typed fields** (`reason`, `stage`, `recovery_class`, `tool_id`, `message`)
  are a captured contract — a Tier-4 "logs" projection.

## Cross-language tolerance is pinned by one fixture

`failure_schematic_vectors` in `tests/cross_lang_payments/payment_vectors.json`
(generated from the real `FailureSchematic` by `gen_payments_fixtures`) drives
every language's verifier: a valid case, an unknown-reason + preserved-extras
case, and the reject cases (foreign `@2` tag, malformed JSON, non-object,
invalid UTF-8). Each language runs the **same tolerant predicate** — decode
UTF-8 JSON, accept iff an object tagged `net.payment.failure@1` — so no
per-language tolerance test can drift (`testing.md`).

## Source

- Types + vocabulary + header discipline: `net/crates/net/sdk/src/tool_payment.rs`
- Typed engine denials: `payments/src/engine/mod.rs` (`RedeemDenialReason`)
- Single render site: `payments/src/flow/mod.rs` (`denial_for`,
  `engine_unavailable_denial`, `redeem_via_engine`)
- Cross-language tolerance fixture: `tests/cross_lang_payments/payment_vectors.json`
  (`failure_schematic_vectors`)
