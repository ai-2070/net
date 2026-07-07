# Code Review — `payments-sdk` branch (x402-native `net-payments` crate)

**Date:** 2026-07-06
**Branch:** `payments-sdk` vs `master` (merge-base `a8c18c0d`)
**Scope:** the new `net/crates/net/payments/` crate (~8,800 LoC source + ~10,000 tests/fixtures) and its integration into the MCP/mesh serving layer and the Python/Node bindings. 22 commits, ~19,655 insertions.
**Method:** six parallel subsystem passes (core, x402 wire, engine/policy, facilitator/checker, flow/signer, MCP-gate integration), with the load-bearing findings independently re-verified against the code by the reviewer.

---

## Resolution status (2026-07-06)

Every finding below is addressed on this branch — the three HIGH and all ten MEDIUM items are fixed with regression tests; the LOW set is fixed where reasonable and otherwise documented as an accepted/bounded tradeoff. Each fix is annotated inline (`> **Fixed** (commit)` / `> **Documented** (commit)`). **Everything on the *Suggested merge gate* is resolved.**

| Finding | Status | Commit | Regression test |
| --- | --- | --- | --- |
| H1 | Fixed | `81a0d3ac3` | `overpayment_retry_via_re_verify_never_auto_bills` |
| H2 | Fixed | `530f18927` | `a_cross_origin_redirect_is_refused_and_nothing_is_signed` |
| H3 | Fixed | `49c2782b6` | `delivered_amount_binds_to_the_authorized_payer` |
| M1 | Fixed | `312125a7f` | `a_bearer_scheme_reject_keeps_the_reservation` |
| M2 | Fixed | `12bf1f52c` | `replay_key_is_encoding_agnostic`, `a_reencoded_payload_never_satisfies_a_second_quote` |
| M3 | Fixed | `d90db63c2` | `a_crashed_in_flight_claim_is_reclaimable_after_the_ttl` |
| M4 | Fixed | `c697a88e5` | `read_all_dedups_a_duplicated_append_by_event_id`, `a_lost_billing_append_is_recovered_on_retry` |
| M5 | Fixed | `4dc3ab1d4` | `a_mis_keyed_pricing_config_is_rejected_at_publish` |
| M6 | Fixed | `1128807a5` | `a_free_none_tool_stays_duplicate_safe` (+ `AtMostOnce` assert on the paid test) |
| M7 | Fixed | `ad86d4d02` | eip155_checker + facilitator client tests |
| M8 | Fixed | `6420156ae` | `an_idempotency_key_from_a_foreign_scope_is_rejected` |
| M9 | Fixed | `caa1ffcf4` | `accepts_max_amount_required_alias` |
| M10 | Fixed | `1689ddd7f` | `final_depth_comes_from_the_config_pack` |
| LOW | Fixed where reasonable / documented | `638c81b56`, `c9d6fc391`, `2f8317dec` | see per-item notes |

---

## Bottom line

High-quality, carefully-architected work. The cryptographic core is sound (verified directly):

- Canonicalization is deterministic — bytewise key sort regardless of map backing, floats fail-closed, signature covers *canonical-minus-`signature`*, `verify_strict` (malleability-safe).
- `AtomicAmount` is u128 with a strict grammar (no leading zeros / signs / decimals / unicode digits / `> u128::MAX`) and checked math.
- The engine's claim→verify→settle→complete state machine holds its locks correctly, serializes concurrent completions under `mutate_json`, and freezes on underpayment / network-mismatch / reorg.
- EIP-712/EIP-3009 encoding is correct; the deterministic EIP-3009 nonce is intentional and correct.
- The byte-preservation invariant ("never re-serialize x402 for signing") is upheld throughout.
- No memory-safety bugs and no reachable panics on attacker-controlled input.
- On the enforced mesh path, an unpaid caller cannot obtain a result — the gate fails closed on every error arm.

The findings below are real but bounded. The three HIGH items are recommended merge blockers. **H1 is a confirmed, trivially-triggered violation of a stated money-path invariant** and is the single most actionable fix.

**Legend:** `[CONFIRMED]` = reviewer re-read the code and reproduced the logic path; `[PLAUSIBLE]` = reported by a subsystem pass with a concrete code citation, not independently reproduced end-to-end; `[VERIFY]` = correctness depends on an external fact (pinned spec / network posture) not resolvable from the repo.

---

## HIGH

### H1 — `re_verify` skips the amount re-check → an overpayment is auto-billed on retry  `[CONFIRMED]`
**Location:** `net/crates/net/payments/src/engine/mod.rs:760-774` (routing at `:428-432`).

`accept_payment`'s completion enforces the delivered-vs-required policy: underpay → freeze, overpay → `Exception{Overpayment}` (deliberately **not** frozen — overpayment is meant to go to manual provider policy), exact → serve (`:572-641`). But `re_verify` does **not** re-apply that check. When the tier is satisfied it bills whatever `delivered` it reconstructs from `chain.first().extra["delivered"]` (`:760-774`), trusting only the facilitator's `is_valid` boolean. The sibling method `re_verify_with_checker` *does* run `delivered.cmp(&required_amount)` (`:918-980`) — `re_verify` simply omits it.

Because an overpayment leaves the record settled-but-unbilled and unfrozen (`in_flight=false`, `chain=[Exception]`), any subsequent `accept_payment` routes `Claim::AlreadySettled → re_verify` (`:431`).

**Failure scenario (reproducible with the repo's own `OverpayingFacilitator`, `tests/lifecycle_modes.rs:507-588`):**
1. Quote for `2500`; facilitator settles `amount="9999999"` → first `accept_payment` returns `Exception{Overpayment}`, `served=false`, no billing (exactly what the test at `:554-588` asserts — and it only asserts the *first* call).
2. Caller retries (agents retry constantly). Claim → `AlreadySettled` → `re_verify`. Facilitator `verify` returns `is_valid=true`; `tier=Observed` satisfies `required_tier=Observed`; `delivered` is read back as `"9999999"`.
3. Engine emits `Served`, sets `served=true`, and publishes a **billing event for the overpaid `9999999`** — auto-satisfying an overpayment the design says must go to manual policy.

**Fix:** give `re_verify` the same under/over/exact decision `re_verify_with_checker` has, or refuse to promote a chain whose first settlement event is an `Exception` before billing.

> **Fixed** (`81a0d3ac3`): `re_verify` now re-applies the under/over/exact decision — a short-pay invalidates+freezes, an overpay stays `Exception{Overpayment}` with no billing, and only an exact `delivered` promotes to a billed serve; an already-billed record still serves idempotently (the amount was vetted when it was billed). Regression `overpayment_retry_via_re_verify_never_auto_bills` drives the `OverpayingFacilitator` through the retry path and asserts every chain event stays `Overpayment`, unserved, unbilled.

### H2 — Outbound HTTP-402 client follows redirects, leaking the signed payment and mis-scoping spend policy  `[CONFIRMED]`
**Location:** `net/crates/net/payments/src/flow/http402.rs:100-103` (client build), `:138` (unpaid GET), `:246-251` (paid retry), `:207`/`:327` (`host_of`).

The `reqwest::Client` is built with only `.timeout(...)` and no redirect policy, so it uses reqwest's default (follow up to 10 redirects). reqwest strips only `authorization`/`cookie`/`proxy-authorization`/`www-authenticate` cross-host — **not** custom headers. Both the unpaid probe and the paid retry (which attaches `PAYMENT-SIGNATURE` carrying the signed EIP-3009 authorization, a bearer instrument) silently follow 3xx to any origin.

**Failure scenario:** Caller does `fetch_paid("https://trusted.com/x")`. `trusted.com` (open-redirect or compromised) 302→`https://evil.com`. reqwest follows, so the `402` + `PAYMENT-REQUIRED` demand is authored by `evil.com`, but `host_of(url)` still returns `trusted.com` → `check_and_reserve` is evaluated against capability key `x402-http/trusted.com`, applying trusted.com's (possibly looser) `per_capability` limits/approvals to evil.com's demand. The paid retry `self.http.get(url)` re-follows the redirect and hands the signed transfer authorization to `evil.com`, whose `pay_to`/`amount` it pays, gated only by the mis-scoped policy.

**Fix:** `.redirect(reqwest::redirect::Policy::none())`; treat any non-402 3xx as failure; re-validate that the final origin equals the intended host before authoring payment.

> **Fixed** (`530f18927`): the client is built with `redirect::Policy::none()`, any non-402 3xx on the fetch is a hard failure, and the 402 demand origin is re-checked against the intended host before authoring payment. Regression `a_cross_origin_redirect_is_refused_and_nothing_is_signed` drives a cross-origin 302 and asserts nothing is signed, sent, or reserved.

### H3 — Independent chain checker binds delivery to (token, recipient, amount) only — never to the payer/authorization  `[CONFIRMED design gap]`
**Location:** `net/crates/net/payments/src/checker/eip155.rs:162-186` (delivered-amount loop) + `checker/mod.rs:43-49` (`TransferQuery` = `{token, to}`). Consumed at `engine/mod.rs:1004` (serve gated on the checker's tier — verified).

The module exists so "the facilitator is never in the trust root above `observed`." But the checker's only cross-check is "sum of ERC-20 `Transfer` logs from `q.token` to `q.to` in the facilitator-supplied `transaction`," with no binding to the EIP-3009 payer (`from`), the authorization nonce, or the fact that *this* tx is *this* quote's settlement. A malicious/buggy facilitator can return `success:true` with `transaction` = the hash of *any* real qualifying USDC transfer to the same merchant (e.g. a different customer's payment). `Eip155Checker::check` finds `Transfer(USDC → pay_to, value ≥ required)`, `status==1`, sufficient depth, and returns `Included{ tier: Final, delivered ≥ required }`; the engine marks the quote `Verified`, serves, and bills at `Confirmed`/`Final` confidence though the payer's authorized transfer for this quote never executed.

**Mitigation already present:** the engine's `consumed_transactions` guard (`engine/mod.rs:533-563`) limits reuse to one quote per tx, so the exploit needs a *stream* of distinct unrelated qualifying transfers to `payTo` — plausible for a busy merchant, but not unbounded reuse of one tx.

**Fix:** thread the authorization `from` (and ideally the nonce) into `TransferQuery`; require the matched `Transfer` log's indexed `from` topic to equal the payer.

> **Fixed** (`49c2782b6`): `TransferQuery` now carries the authorized payer, and the eip155 delivered loop requires the matched `Transfer` log's indexed `from` topic to equal it. `re_verify_with_checker` sources the payer from `payload.authorization.from`; a non-matching payer sums to an honest zero, which the engine already turns into an amount-mismatch invalidation. Regression `delivered_amount_binds_to_the_authorized_payer` proves a transfer from a different payer contributes nothing. Nonce binding via the token's `AuthorizationUsed` event is noted as a stronger follow-up — computing that topic needs keccak256, gated behind the dev-signer feature, not `http-facilitator`.

---

## MEDIUM

### M1 — Untrusted provider defeats `max_per_day` by claiming "rejected"  `[PLAUSIBLE]`
**Location:** `flow/mod.rs:457-462` (`Rejected` → `release`), `:471-474` (`Failure` → `release`); same pattern `http402.rs:278-283`. Payload handed to provider at `mod.rs:419` *before* the decision is known.

For the `exact`/EIP-3009 scheme the caller signs a self-contained pull authorization and sends it, then `release_reservation`s the per-day spend counter whenever the provider *claims* `Rejected`/`Failure`. A provider holding a valid authorization can report "rejected" while still submitting it to the facilitator/chain and collecting. Each cycle uses a fresh quote (fresh deterministic nonce), so it settles again; the counter oscillates around zero and never trips `max_per_day` — the core loss-bound control is defeated, draining the wallet in `max_per_call`-sized increments. Contrast `mod.rs:475-480`, which correctly keeps the reservation on transport ambiguity. A rejection from an untrusted party holding a bearer auth is not proof the money didn't move.

> **Fixed** (`312125a7f`): the reservation is released only for the chainless mock test scheme; every real bearer authorization keeps it (fail-closed, exactly as on transport ambiguity). A shared `reject_releases_reservation` predicate covers both the mesh flow and http402. Regression `a_bearer_scheme_reject_keeps_the_reservation` drives an exact/eip155 quote through a rejecting facilitator and asserts the day counter still reflects the spend.

### M2 — Payload-replay index keyed on carry *bytes*, not the scheme nonce  `[PLAUSIBLE]`
**Location:** `engine/mod.rs:348` (`payload.content_hash()`), `:398-405` (`consumed` check/insert), `x402/mod.rs:118-120`.

The `consumed` index maps `blake3(payload bytes) → quote_id`. x402 carries are byte-preserved, so two JSON encodings of the *same* authorization hash differently and are treated as distinct payloads. Both pass `payload.accepted == requirements` (structural, encoding-agnostic, `:344`) and the byte-keyed `consumed` check. With the shipped `MockFacilitator` (`mock.rs:69,103-108,220`), each encoding settles to a *different* tx id, so `consumed_transactions` does not collide → both quotes served for one logical payment. Real EVM is saved by the on-chain nonce (duplicate settle reverts), but the engine-level "one payload → one quote" invariant is delegated to facilitator/chain behavior. **Fix:** key replay on a canonicalized authorization / scheme nonce, not the raw carry bytes.

> **Fixed** (`12bf1f52c`): replay is keyed on the canonical payload (`X402Carry::<PaymentPayload>::replay_key`), which carries the scheme's nonce/authorization, not the preserved carry bytes — two encodings of one authorization now share a replay identity; an uncanonicalizable payload is rejected, never byte-keyed. Regressions `replay_key_is_encoding_agnostic` (unit) and `a_reencoded_payload_never_satisfies_a_second_quote` (engine) cover it.

### M3 — A crash between claim and completion permanently strands a paid quote  `[PLAUSIBLE]`
**Location:** `engine/mod.rs:361-408` (Fresh claim persists `in_flight=true` in its own committed transaction), reset only at `:511/:542/:569` or `release_claim` `:1160-1177`.

The Fresh claim writes `in_flight=true` to disk, releases the lock, then runs verify/settle with no lock held. No timeout/reaper resets it. Kill the process after `settle` moves value but before completion → on restart, every retry hits `if rec.in_flight { return Claim::InProgress }` (`:389`) forever, and the caller can't fall back to a fresh quote with the same payload (`consumed[payload]=Q1 ≠ Q2 → Replay`, `:398-401`). Value paid, never served, never billed, no path forward. **Fix:** stale-`in_flight` TTL + reaper, or a crash-recovery reconciliation sweep.

> **Fixed** (`d90db63c2`): the record stamps `in_flight_since_ns`; a claim held in-flight past a configurable TTL (default 5 min, longer than any verify+settle round-trip) is reclaimable by a retry, refreshing the clock under the lock so a concurrent retry still sees `InProgress` and only one attempt re-runs verify/settle (the deterministic nonce keeps re-settle idempotent on-chain). A legacy record with no timestamp is treated as immediately stale. Regression `a_crashed_in_flight_claim_is_reclaimable_after_the_ttl` aborts an attempt mid-verify and asserts a pre-TTL retry sees `InProgress` and a post-TTL retry reclaims and serves.

### M4 — Billing event can be lost from the log/stream  `[PLAUSIBLE]`
**Location:** `engine/mod.rs:627-629` (billing saved to state) vs `:645`/`:790-802` (`publish_billing` runs *after* the completion lock) + `:421-427` (`AlreadyServed` retry returns `Served` without publishing).

Completion durably records `rec.billing` in engine state, then appends to the JSONL `BillingLog` outside the lock. If that append fails (`EngineError::Billing`) or the process dies in between, the idempotent retry (`Claim::AlreadyServed`) returns `Served` and never re-appends. `read_all`/`export_jsonl` read only the log (`billing/mod.rs:121-161`), so the charge is invisible to accounting permanently — contradicting the "broken billing stream stops serving" comment (`:75-80`).

> **Fixed** (`c697a88e5`): the record tracks `billing_published`, set only after a durable append; the `AlreadyServed` retry re-publishes when it is unset (and a still-broken stream keeps surfacing an error rather than a false `Served`); `read_all`/`export_jsonl` dedup by `billing_event_id`, so a re-publish (or a crash between append and mark) is one charge to every reader. Regressions `read_all_dedups_a_duplicated_append_by_event_id` (unit) and `a_lost_billing_append_is_recovered_on_retry` (engine) cover it.

### M5 — Mis-keyed pricing config silently publishes a tool for free  `[CONFIRMED]`
**Location:** `adapters/mcp/src/wrap/descriptor.rs:204`; publish guard `wrap/session.rs:407`; lowering `session.rs:731-741`.

`lower_tool` looks up pricing as `ctx.pricing.get(&tool.name)`, and the only publish-time validation is "pricing non-empty ⇒ a gate exists" (`session.rs:407`). Nothing verifies each `config.pricing` **key** matched a discovered tool. Price tool `echo` but key the map with the sanitized channel id (`server__echo`), a typo, or a since-renamed tool → publish succeeds, `pricing.get("echo")` returns `None`, `echo` lowers with `pricing_terms=None`, its handler gets `paid=false`, and it serves free to every caller with no warning. **Fix:** after lowering, assert every `config.pricing` key mapped to a lowered tool, else error.

> **Fixed** (`4dc3ab1d4`): after discovery+lowering, every `config.pricing` key must equal some lowered tool's `mcp_name`, else `WrapError::PricingKeyUnmatched` before anything announces or serves. Regression `a_mis_keyed_pricing_config_is_rejected_at_publish` shows the real `echo` key publishes while a mis-keyed `ecko` is rejected and leaks nothing onto the mesh.

### M6 — Paid, uncredentialed tool retries on timeout and the payer loses the money  `[PLAUSIBLE]`
**Location:** `adapters/mcp/src/serve/gated.rs:186` (`InvokeSafety::from_credential_status` ignores `pricing_terms`/`payment_proof`); consumed by retry/failover in `mesh_gateway.rs`.

`InvokeSafety` is derived only from `credential_status`; a paid `credential_status:"none"` tool is `DuplicateSafe`, so a timed-out invoke retries and re-sends the same quote header. The provider's at-most-once engine sees the quote already redeemed and returns `ERR_PAYMENT` → caller gets `Denied` with **no result despite paying**. Fails closed (no double-serve), but the resilience layer designed to cover this race guarantees the loss for the money path. **Fix:** force `AtMostOnce` whenever `pricing_terms` is present (or a payment proof rides the call).

> **Fixed** (`1128807a5`): a paid invoke (`pricing_terms` present, or a payment proof rides the call) is forced `AtMostOnce` regardless of credential status, so a timed-out paid call surfaces rather than retrying an already-redeemed quote. The paid-invoke test now asserts `AtMostOnce`, and `a_free_none_tool_stays_duplicate_safe` confirms the override is scoped to paid calls.

### M7 — Facilitator/RPC transport hardening  `[PLAUSIBLE]`
Three related gaps on config-supplied (untrusted) endpoints:
- **No TLS/scheme enforcement** on the facilitator endpoint (`facilitator/client.rs:99-111`) — an `http://` endpoint sends the CDP bearer key (`BearerAuth`, `:78-83`) in cleartext, contradicting the file's own secret-handling doctrine.
- **No `eth_chainId` confirmation** that the RPC actually serves the CAIP-2 chain (`checker/eip155.rs:128-158`) — a swapped/typo'd RPC (Base-Sepolia URL paired with `eip155:8453`) validates a worthless testnet tx as a mainnet settlement.
- **Unbounded `response.bytes()`** (`client.rs:134,224`, `eip155.rs:75`) — a malicious/compromised facilitator or RPC can stream a multi-GB body within the timeout, exhausting memory.

> **Fixed** (`ad86d4d02`): the facilitator endpoint must be `https` (or loopback) before the bearer key rides; the eip155 checker confirms once that `eth_chainId` matches the CAIP-2 chain (`ensure_chain_id`) before trusting any receipt; and RPC/facilitator response bodies are read against a size cap (`MAX_RPC_BODY`) so a hostile endpoint can't stream an unbounded body. Covered by the eip155_checker and facilitator client tests.

### M8 — Billing idempotency key not bound to the event's own scope  `[CONFIRMED]`
**Location:** `core/billing_event.rs:75` (`derive_id`), `:83-94` (`from_json_bytes`).

`billing_event_id = H(domain, idempotency_key)` and `from_json_bytes` only checks `billing_event_id == derive_id(idempotency_key)` — pure self-consistency. Nothing recomputes `IdempotencyScope{caller,provider,capability,quote_id}.key()` from the event's own coordinates and compares. A signed event for quote `qA` can carry the idempotency key of a *different* scope `qB`; it passes tag, id-derivation, and signature (payee signs it), and a store that dedups on `billing_event_id` collides `qA`'s charge with `qB`'s event — silently dropping one distinct charge. The "one charge per {caller,provider,capability,quote}" guarantee is not enforced on decode. **Fix:** recompute the scope key from the event fields and compare.

> **Fixed** (`6420156ae`): `from_json_bytes` recomputes `IdempotencyScope{caller,provider,capability,quote_id}.key()` from the event's own fields and rejects a mismatch, so an event carrying a foreign scope's idempotency key no longer decodes. Regression `an_idempotency_key_from_a_foreign_scope_is_rejected`.

### M9 — Possible x402 interop break: `amount` vs `maxAmountRequired`  `[VERIFY]`
**Location:** `x402/requirements.rs:40-41` (`pub amount: String`, camelCased to `"amount"`).

The struct models the required amount as `amount`; the widely-deployed x402 `PaymentRequirements` names it `maxAmountRequired`. All in-repo fixtures use `amount` (`tests/cross_lang_payments/fixtures/x402/v2.0/payment_requirements.json:4`) so they are self-consistent and tests pass — this only bites the real-external-server outbound "two-way door." If a genuine x402 server sends `{"maxAmountRequired":"10000",...}` with no `amount` key, `X402Carry::<PaymentRequired>::from_bytes` fails ("missing field `amount`") and the payment is never attempted. **The pinned spec (`087922a5…`) is not in the repo — verify the field name against it before shipping the outbound path.**

> **Fixed** (`caa1ffcf4`): `PaymentRequirements` accepts `maxAmountRequired` as a serde alias on inbound parse, so a real external server's demand parses even when it omits `amount`; outbound authoring is unchanged. Regression `accepts_max_amount_required_alias`.

### M10 — `Final` at 12 confirmations, not configurable per network  `[VERIFY]`
**Location:** `checker/eip155.rs:48` (hardcoded `final_depth: 12`), `:154-156`; `facilitator/config.rs` (no `final_depth` field).

`confirmations >= 12 → Final`. On Base (OP-stack L2), 12 L2 blocks (~24s) is not L1-backed finality; L2 blocks remain reversible until their batch finalizes on L1 (minutes). The doc says "pick per network posture in the config pack," but `FacilitatorConfig` carries no override and the only live caller uses the default. A payment reversed by an L1-driven Base reorg after 12 L2 confirmations was already reported `Final`. **Fix:** carry `final_depth` in the config pack per network; set L2 postures to L1-finalization depth.

> **Fixed** (`1689ddd7f`): `final_depth` is carried per-network in the facilitator config pack and plumbed into `Eip155Checker` (`from_config`), so an L2's L1-finalization posture reaches the checker instead of the L1-scale default. Regression `final_depth_comes_from_the_config_pack`.

---

## LOW

Grouped; all fail-closed or bounded.

**Caller-side trust (bounded by the byte-locked template):**
- The flow never checks `quote.provider == expected_provider` (`flow/mod.rs:362-378`). Not exploitable for overpayment/wrong-recipient because `requirements.bytes()` is byte-locked to the announced template, but weakens quote provenance/attribution.
- On `PayResponse::Served` the flow trusts the provider-supplied `billing_event`/`transaction` verbatim without verifying the billing event's signature or quote binding (`flow/mod.rs:420-446`) — an audit/dispute-evidence gap, not fund loss.
- `fetch_paid` doesn't require `https://` (`http402.rs:136`) — an `http://` URL sends `PAYMENT-SIGNATURE` in cleartext; and `validBefore` derives from the server-controlled expiry with no upper clamp (`mod.rs:602`, `http402.rs:203`), allowing a long-lived single-use bearer authorization.

> **Fixed** (`c9d6fc391`): the flow refuses a quote whose `provider` differs from the announced terms provider; it verifies the provider-supplied billing event (signature + id-derivation + scope, plus that it binds this quote/caller/provider) before recording it as evidence, dropping it from the proof otherwise; http402 refuses to author payment over cleartext to a remote host (loopback allowed for local testing); and the EIP-3009 authorization lifetime is clamped (1h cap). Regressions `a_quote_whose_provider_differs_from_the_announced_terms_is_denied`, `payment_requires_https_except_loopback`.

**Checker / facilitator edge cases:**
- A reorged-out (previously confirmed) settlement degrades to `Pending`, never invalidated — unlike an on-chain revert (`eip155.rs:144`; engine treats `Pending` as "no answer", `engine/mod.rs:884-893`). A double-spend reorg that removes an already-served settlement is silently not flagged.
  > **Documented** (`2f8317dec`): a missing receipt is genuinely ambiguous (not-yet-mined / lag / reorged-out); distinguishing reorg-out needs stateful prior-inclusion tracking, out of scope for a receipt-only check. The known limitation is documented at the `Pending` arm; an on-chain revert (status 0) IS caught.
- `parse_hex_u128` errors terminally on a `uint256` transfer value > `u128::MAX` (`eip155.rs:100-107`) — fail-closed, but permanently stalls verification for such tokens.
  > **Accepted:** left as-is — erroring is correctly fail-closed (saturating could over-credit); such a value is absurd for the 6-decimal stablecoins in scope.
- `FacilitatorConfig::networks()` calls `dedup()` without a preceding `sort`, so non-adjacent duplicates survive (`config.rs:127-131`).
  > **Fixed** (`638c81b56`): `networks()` sorts before `dedup`. Regression `networks_dedups_non_adjacent_duplicates`.
- A `/settle` timeout is tagged `retryable=true` identically to `/verify` (`client.rs:235-243,280`), risking a double-settle absent EIP-3009 nonce idempotency at the facilitator.
  > **Accepted:** bounded by the deterministic EIP-3009 nonce (a duplicate settle reverts on-chain) plus the engine's `consumed_transactions` guard.

**Core validation warts (all fail-closed):**
- Registry decimals cross-check is skipped when `decimals` is a JSON string/float (`registry.rs:133-137`, `.and_then(as_u64)`) — decimals is UX metadata, not a money-path input.
  > **Accepted:** decimals is UX metadata, not a money-path input; left as-is.
- `ensure_tag` rejects a leading-zero version spelling (`@01`) with a nonsensical "unsupported_version: got @1, expected @1" (`versioning.rs:65,73-90`).
  > **Fixed** (`638c81b56`): `@01` (which parses to version 1) is accepted as the same version. Regression `a_leading_zero_version_spelling_is_the_same_version`.
- `check_chain` validates hash links + freeze but not event signatures or shared `quote_id`/authorized signer (`verification.rs:192-213`) — safe only if the caller verified each event via `from_json_bytes` first; the name invites misuse. Document the precondition or verify inside.
  > **Documented** (`638c81b56`): the precondition (verify each event via `from_json_bytes` first for untrusted input) is now stated in the `check_chain` doc comment.
- A quote `input_hash: Some("")` collides with `None` in `derive_terms_hash` (`quote.rs:134`) — empty string is not valid blake3 hex, so unreachable in practice.
  > **Accepted:** unreachable in practice (empty string is not valid blake3 hex); left as-is.

**x402 authoring (self-defeating, not fund loss):**
- `exact_evm` authoring helpers don't validate nonce/address formats or `valid_after < valid_before` (`schemes/exact_evm.rs:100-149`); the `to==payTo` and `value==amount` cross-checks that matter *are* present and correct.
  > **Fixed** (`638c81b56`): `typed_data` refuses an empty/inverted validity window (`validAfter >= validBefore`), which would never authorize. (Nonce/address *format* checks left to the signer/contract, which reject a malformed authorization.)
- `exact_svm::transfer_intent` accepts a reference-less `solana` network and skips CAIP re-validation when called outside the carry path (`schemes/exact_svm.rs:66-67`).
  > **Accepted:** no live native SVM paid-invoke path today; left as a follow-up for when one lands.

**Integration:**
- Native SDK `.pricing_terms(...)` can be announced with no provider-side redeem gate wired — `PaymentAdmission`/`redeem` lives only in the MCP wrap path (`sdk/src/tool.rs`, `adapter/net/mesh.rs:10108-10118`). Discovery-only today (no live native paid-invoke path), but a trap: a native tool served via `serve_rpc` would appear priced while any direct caller pays nothing. Guard or document as discovery-only until a native admission gate exists.
  > **Documented** (`2f8317dec`): `pricing_terms` is documented as discovery-only on the SDK builder — a natively `serve_rpc`'d tool advertises a price but does not enforce payment before invocation; the redeem gate lives in the MCP wrap path.
- Bearer-mode quote (`binding_sig = None`) is front-runnable by an observer of the quote id (`serve/payment.rs:22-27`, `wrap/invoke.rs:297-304`) — documented P1 tradeoff; per-channel AEAD limits observers; providers can require the binding by policy.
  > **Accepted:** unchanged — a documented P1 tradeoff; per-channel AEAD limits observers and providers can require the binding by policy.

---

## Verified sound (to scope the review)

Cross-language canonicalization determinism; u128 amount grammar/overflow; the engine's lock discipline, idempotent double-billing protection, underpayment freeze, network-mismatch freeze, transaction-replay guard, and reorg/frozen rejection across all four entry points (`accept_payment`, `re_verify`, `re_verify_with_checker`, `redeem_for_invocation`); EIP-712 domain/typehash/word-packing; the deterministic (intentional, correct) EIP-3009 nonce; production `ExternalSigner`/`ExternalSvmSigner` never hold key material and never log typed data or keys; no reachable panics on malformed RPC/HTTP/header/base64/hex input (all fallible-and-handled); the mesh gate enforces payment strictly before tool execution and fails closed on every error arm; cross-provider failover reuses the provider-bound quote so an equivalent peer correctly refuses (no double-serve); chain IDs (`eip155:8453`, `eip155:84532`, Solana mainnet-beta genesis) and Base RPC URLs are accurate; the store's cross-process advisory lock uses atomic temp+fsync+rename with loud errors on corruption and no panics-while-holding-lock.

---

## Suggested merge gate

Block on **H1, H2, H3**, plus **M1** and **M5** (both silently disable a control). The rest are good follow-ups.

> **Status:** all five gate items are resolved (H1 `81a0d3ac3`, H2 `530f18927`, H3 `49c2782b6`, M1 `312125a7f`, M5 `4dc3ab1d4`), and the remaining MEDIUM and LOW items are addressed or documented — see the *Resolution status* table above.

**Smallest / highest-value fixes:**
- **H1** — add the `delivered.cmp(&required)` decision to `re_verify` (mirror `re_verify_with_checker`). Self-contained; the failing case already has a test fixture.
- **H2** — `.redirect(Policy::none())` + final-origin re-check in `X402HttpFlow::new`/`fetch_paid`. Self-contained.
- **M5** — post-lowering assertion that every `config.pricing` key mapped to a discovered tool.

**Needs a design call:**
- **H3 / M1 / M2 / M3** — all touch the trust boundary between the engine and a lying/failing facilitator or provider (payer-binding in the checker, bearer-auth-before-decision accounting, replay keying, crash recovery). Worth resolving these together as "what does the engine assume about facilitator/provider honesty, and where is each assumption enforced independently."
