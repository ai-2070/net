# Verification — tiers, the independent checker, reorgs, idempotency

Verification is where Net's value-add over x402 lives: **confidence is a tier,
not a boolean**, it is **chained and immutable**, and anything above a
facilitator receipt comes from an **independent on-chain check** so the
facilitator never has to be in anyone's trust root. Read `object-model.md` §4
for the envelope shape; this file is the model and the moving parts.

## The tier enum (`core/verification.rs`)

```rust
pub enum VerificationTier { Observed, Confirmed(u32), Final }
// ordering: Observed < Confirmed(n) < Confirmed(n+1) < Final
// wire form: "observed" | {"confirmed":6} | "final"
tier.satisfies(&minimum) -> bool
```

A **fixed protocol enum, canonical across all networks.** Adapters map their
chain semantics *into* it (Solana commitment levels, EVM confirmations, XRPL
validation) — chain-specific states never leak up into policy. The two rules
that shape everything:

1. **A facilitator receipt justifies `Observed`, full stop.** The x402 v2 spec
   gives facilitators no way to report finality. `HttpFacilitator` and
   `MockFacilitator` both cap `verify`/`settle` at `Observed`.
2. **`Confirmed(n)` and `Final` come only from the independent chain check** of
   the tx hash — the `ChainChecker` below. Policy picks the required tier per
   capability (`FacilitatorConfig.required_tier`, or the provider's
   `required_tier` on the invocation path).

The provider requires a tier before serving; a settled payment below that tier
returns `PaymentDecision::PendingTier { reached, required }` and the handler
does not run until a re-verify reaches the bar.

## The independent chain checker (`checker/`)

```rust
pub trait ChainChecker: Send + Sync {
    fn reference(&self) -> VerifierRef;   // endpoint "independent-chain-check:<rpc>"
    async fn check(&self, network: &str, transaction: &str, query: Option<&TransferQuery>) -> Result<ChainVerdict, CheckerError>;
}

pub struct TransferQuery { pub token: String, pub to: String }   // for the delivered-amount cross-check
pub enum ChainVerdict {
    Pending,
    Included { tier: VerificationTier, delivered: Option<String> },
    Reverted,
}
pub struct CheckerError { pub message: String, pub retryable: bool }   // ::retryable(msg) / ::terminal(msg)
```

**`eip155` implementation** (`checker/eip155.rs`, feature `http-facilitator`):

```rust
let checker = Eip155Checker::new("eip155:8453", "https://mainnet.base.org")?
    .with_final_depth(12);   // default 12 — depth at which Confirmed rolls to Final
```

It uses JSON-RPC `eth_getTransactionReceipt` + `eth_blockNumber` head-depth
arithmetic, and (for the delivered-amount cross-check) sums the ERC-20
`Transfer` log values to the `to` address. The facilitator is **not** in the
trust root for anything above `observed`.

Engine integration:

```rust
engine.re_verify_with_checker(quote_id, &checker, required_tier, now_ns).await?;
```

The facilitator receipt stays `Observed`; the checker **upgrades the chain**
with `Verified@Confirmed(n)` / `Verified@Final` events (a distinct
`VerifierRef`, `endpoint = "independent-chain-check:<rpc>"`). The envelope
objects are unchanged — this is config, not new types.

**Delivered-amount cross-check at `final`:** where the chain exposes it (the
ERC-20 `Transfer` log value vs the quoted amount), verification checks the
amount **delivered**, never sent — fees are the payer's problem, not a
short-pay the provider silently eats. A mismatch is
`InvalidationReason::AmountMismatch`.

## Chains, reorgs, and the freeze invariant

Verification events chain **per quote**. Each `VerificationEvent.prev` is the
blake3 chain-hash of the previous event's canonical bytes (including its
signature — a link commits to the signed *fact*, not just content).

```rust
VerificationEvent::chain_hash(&self) -> Result<String, EnvelopeError>
check_chain(&[VerificationEvent]) -> Result<(), EnvelopeError>
```

`check_chain` enforces two things: every `prev` matches its predecessor's
chain-hash, **and any event after an `Invalidated` status is a protocol
violation.** That second rule is the reorg-freeze made structural:

- `VerificationStatus::Invalidated { reason: Reorg }` is a **first-class
  outcome.** When a previously-verified settlement's chain history is
  reorganized out, the engine emits the invalidation event and **freezes
  further serving against that quote** (`QuoteStatus.frozen` is set;
  `redeem_for_invocation` refuses).
- `InvalidationReason`: `Reorg`, `Expired`, `Replay`, `AmountMismatch`,
  `Rejected`. `InvalidationReason::from_facilitator_reason(&str)` maps the
  x402 facilitator vocabulary into this **closed** set; an unknown reason
  collapses to `Rejected` and the verbatim string is preserved in the event's
  `extra` (never lost, never widening the enum).

**Billing events are immutable.** A reorg does not rewrite the billing event
that was emitted; later invalidation/adjustment/refund events *reference* it.
Event-sourced all the way down. There are no automatic refunds in v1.

## Exceptions (not pass, not fail)

```rust
pub enum ExceptionKind { Overpayment }
```

Overpayment is a verification **exception** returned as
`PaymentDecision::Exception { kind: Overpayment }` for provider policy to
handle manually. **The verifier never auto-satisfies on overpayment** and
there are no automatic refunds in v1 — exact-amount policy is the v1 default.

## Idempotency & replay (why retries are safe)

- **Consumed-payload replay index** (persistent, in the engine store): one
  payload satisfies exactly one quote. A replay across process restarts still
  bounces (`RejectReason::Replay`).
- **Consumed-transactions index:** the facilitator-receipt-replay guard — a
  facilitator (or a replayed response) presenting the same
  `network|transaction` for a second quote is invalidated. One on-chain
  settlement never serves twice.
- **Idempotency key** `{caller, provider, capability, quote}`
  (`IdempotencyScope::key()`): same-key retry returns the *same*
  `billing_event_id` — one settle, one serve, one billing event.
  `accept_payment` is safe to retry on timeout; concurrent same-key attempts
  get `PaymentDecision::InProgress` (an `in_flight` mark, not a double-settle).

## Time

No global clock. Quote expiry uses signer timestamps (ns since epoch) with a
bounded policy tolerance (`PaymentEngine::with_expiry_tolerance_ns`).
Verification uses block/ledger time where available. In tests, inject a fixed
`Clock` to drive expiry deterministically.
