# Skills audit — payments 2c: remaining surfaces

Slice 2c of [`SKILLS_VERIFICATION_PLAN.md`](../plans/SKILLS_VERIFICATION_PLAN.md).
Audits the thirteen `.claude/skills/net-payments/` files not covered by
[2a](SKILLS_PAYMENTS_2A_SIGNER_2026-07.md) or
[2b](SKILLS_PAYMENTS_2B_OBJECT_MODEL_2026-07.md): `spend-policy`, `x402`,
`networks`, `facilitator`, `billing`, `http402`, `caller`, `provider`,
`bindings`, `testing`, `concepts`, `gotchas`, `failure-schematic`.

| | |
|---|---|
| **Source SHA** | `9885b7618` (tree unchanged from `38a425b` for `payments/src/`, `sdk/src/`) |
| **Date** | 2026-07-28 |
| **Auditor** | Claude (agent) |
| **Independent review** | not required for this slice |
| **Result** | **1 defect found and fixed.** Mechanical sweep of 13 files + targeted checks on 9 enum families and 4 config packs. |

## Method

Two passes.

1. **Mechanical.** Every `` `CapitalisedName` `` in all thirteen files (≈180
   distinct) resolved against the whole crate tree in any position — type,
   variant, const, or binding symbol. This is broader than the shipped checker,
   which only resolves Rust `pub enum` members and free identifiers.
2. **Targeted.** Enum families and config packs the files tabulate were read
   variant-by-variant, since a table of variants is what a reader
   pattern-matches on and the mechanical pass cannot tell "exists somewhere"
   from "belongs to this enum."

## Defect

**D1 — `provider.md` named a type that does not exist, and omitted the
fail-closed invariant that replaces it.** `provider.md:164` said:

> Attach `terms` to the capability announcement (native `RegisterTool` publish
> options; …)

`RegisterTool` appears nowhere in the tree. The real attachment point is
`ToolDescriptorBuilder::pricing_terms(terms_json)`
(`net/crates/net/sdk/src/tool.rs:177`).

The omission is the more serious half. The SDK enforces **"an announced price
must be an enforced price"** as a fail-closed pair, and the skill did not
mention it at all:

| Path | Descriptor priced? | Result |
|---|---|---|
| `serve_tool` / `serve_tool_streaming` | yes | refused — `ServeError::UnenforceablePricing` (`mesh_rpc.rs:5762`) |
| `serve_tool_paid` | no | refused — `ServeError::MissingPricingTerms` |

You cannot announce a price on a path with no gate (it would be discovered as
paid and served *free* to any direct caller), and you cannot gate an unannounced
price (it would refuse every caller with no way to know why). A reader following
the old text would have reached for a nonexistent API and, on finding
`serve_tool`, hit `UnenforceablePricing` with no idea why.

Fixed: correct symbol, the guard table above, and the `ToolPaymentGate` shape
(`redeem(tool_id, quote_id, binding) -> Result<(), GateDenial>`, body decoded
*before* the gate so an invalid call does not consume the quote).

## Ledger — targeted checks

| # | Claim | Authoritative source | Method | Result |
|---|---|---|---|---|
| 1 | Mock facilitator's nine injectable modes | `facilitator/mock.rs` | variant-by-variant | ✅ `Success`, `Rejected`, `Replay`, `WrongAmount`, `ExpiredRequirements`, `ReorgInvalidate`, `LateFinality`, `VerificationTimeout`, `Protocol` — all nine, no extras |
| 2 | `PaymentDecision` variants used across `caller.md` / `provider.md` / `facilitator.md` | `engine/mod.rs` | variant read | ✅ `Served`, `PendingTier`, `Exception`, `Invalidated`, `InProgress`, `Rejected`, `FacilitatorFailure` |
| 3 | `SpendDecision { Allowed, RequiresPaymentApproval, Denied }` | `policy/spend.rs` | variant read | ✅ |
| 4 | `SpendProfile { Production, DevTest }` | same | variant read | ✅ |
| 5 | Four facilitator packs, exact function names | `facilitator/packs.rs` | signature read | ✅ `x402_org_base_sepolia`, `cdp_base_mainnet`, `cdp_solana_mainnet`, `t54_xrpl_mainnet` — match the ladder's rungs 1–4 |
| 6 | Facilitator auth is secret **refs** only; the value never appears in config | `facilitator/config.rs:47-49` | field read | ✅ `Bearer { secret_ref: String }` |
| 7 | Rust, Python and Node all have a full demand **and** supply flow | `bindings/{python,node}/src/` | file + symbol presence | ✅ both have `capability_gateway.rs` (demand), `payment_provider.rs` (supply), and `publish_paid_tools` |
| 8 | Go is verifier-only — no payment flow | `go/` | grep for gateway/provider | ✅ only `payments_golden_vectors_test.go`; no flow types |
| 9 | `ToolPaymentGate::redeem(tool_id, quote_id, binding) -> Result<(), GateDenial>` | `sdk/src/tool_payment.rs:84` | signature read | ✅ |
| 10 | `EnginePaymentAdmission::new(Arc<engine>)` for MCP-gateway hosts | `payments/src/flow/mcp_gate.rs` | usage in `tests/mcp_gate_composition.rs` | ✅ exists behind the `mcp-gate` feature |

## Findings that are not defects

**The `net.payment.verification@1` / `VerificationEvent` naming split** (noted in
2b) recurs here: several files refer to envelopes by tag and to structs by name,
which is correct but means a reader grepping the tag finds prose and a reader
grepping the struct finds code. No change made — the current form is accurate.

**`networks.md`'s rung states are claims about the world, not the tree.**
"Blocked on CDP credentials", "live run pending" cannot be verified from source;
only the pack names, checker presence and tier settings were checked, and those
match. Whether a rung is genuinely live is an operational fact this audit cannot
establish and future readers should not assume it did.

## What this audit does not establish

- **`x402.md`'s byte-preservation behaviour.** `X402Carry` was verified to exist
  and to be the field type on every envelope that carries an x402 document
  (2b), but the claim that a received document is never re-serialized was read,
  not exercised. It is a good Phase 5 `mutation`-tier candidate: a mutation that
  re-encodes through Net types should kill a test.
- **`testing.md`'s conformance-suite claims.** The mock modes were verified;
  whether the cross-language golden vectors actually cover what the file says
  they cover was not re-derived.
- **`concepts.md` and `gotchas.md` doctrine.** These are argument, not API. They
  contain no checkable symbol claims beyond those swept mechanically.
