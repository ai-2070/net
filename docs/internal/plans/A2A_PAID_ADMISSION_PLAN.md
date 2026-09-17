# Implementation Plan: Optional paid admission for native A2A

**Implements:** the Net-side prerequisites of the Hermes plan `hermes-net/docs/plans/AGENT_TO_AGENT_PAYMENTS.md` — readiness gates **G2** (authenticated buyer context + request binding), **G3** (admission before financial side effects), **G4** (read-only prepare / exact-provider purchase) — so `hermes-net` can sell bounded work over native A2A instead of building a parallel paid-job protocol.

**The sentence:** an A2A service is *explicitly* free or paid by provider configuration; a paid task is **prepared** (validated, capacity-reserved, provider-minted admission id) before any money moves, **purchased** against that exact reservation with a durable caller-side attempt that is resumed rather than re-quoted, **submitted** with the evidence, and **launched once** under a lifetime-exclusive journal owner — and a crash anywhere between money and work leaves a recoverable record rather than a second charge or a second run.

**Status (2026-09-17): DRAFT, revision 2 — not started.** Revision 1 was held in review for six contract gaps; each is closed below and indexed in §0.1. Source-grounded against this worktree.

**Not a new payments system.** No new signature scheme, no new settlement path, no new store engine. This slice composes `net-payments`' existing `PaymentEngine` / `CallerPaymentFlow` with the SDK's `TaskRegistry` through SDK-owned admission and evidence contracts.

---

## 0. Baseline (what the code does today)

| Fact | Evidence |
|---|---|
| `Mesh::serve_a2a(registry, executor)` registers `net.a2a.task` / `net.a2a.status` / `net.a2a.cancel` on the context-bearing `serve_rpc` path (request headers readable). | `sdk/src/mesh_a2a.rs:203-229` |
| `SubmitHandler::call` decodes the brief and immediately calls `TaskRegistry::submit`, which records `Accepted` **and** `tokio::spawn`s the executor under one lock scope. Every outcome is answered in-body as `TaskAck { accepted, reason }` with `RpcStatus::Ok`. | `mesh_a2a.rs:129-158`, `sdk/src/a2a.rs:349-424` |
| Idempotency is identical-brief-per-`(owner, task_id)`; a different brief under a reused id is `SubmitRejection::IdReusedForDifferentBrief` (only variant). Two owners may share an id. | `a2a.rs:365-370,294-309` |
| Owner = `TaskOwner::Peer(ctx.session_peer)`: the AEAD-authenticated **deliverer**, documented as *not* an end-to-end origin under relaying. `TaskOwner` is `Copy + Hash`, variants `Local`, `Peer(u64)`. | `mesh_a2a.rs:14-39,88-91`, `a2a.rs:279-290` |
| `TaskRegistry` is a `HashMap` behind `parking_lot::Mutex` — in-memory; terminal records evicted after `TERMINAL_RECORD_TTL_SECS = 3600`; `forget()` drops a record immediately. | `a2a.rs:314-316,256,522-527` |
| `TaskState::Requested` exists in the wire enum; nothing produces it today. | `a2a.rs:37-56` |
| Task ids are always minted by `TaskBrief::new` (`random_id()`) in both bindings; the Python/Node caller cannot supply one. | `a2a.rs:99-102`, `bindings/python/src/a2a.rs:114`, `bindings/node/src/a2a.rs:271` |
| `RpcContext` carries `session_peer: u64`, `caller_origin: u64` (routing metadata, never authorize on it), `payload.headers: Vec<(String, Vec<u8>)>`, and `org_admission: Option<Admitted>` — the **only** verified end-to-end principal (`Admitted.caller: EntityId`), present only behind PROTECTED-service admission. | `src/adapter/net/cortex/rpc.rs:1494-1571`, `behavior/org_admission.rs:303-314` |
| Native paid tools: `Mesh::serve_tool_paid` wraps the handler in `PaidToolHandler`; reads `HDR_PAYMENT_QUOTE` / `HDR_PAYMENT_BINDING`; calls `ToolPaymentGate::redeem(tool_id, quote_id, binding)`; refuses with `RpcStatus::Application(ERR_PAYMENT)` + `HDR_FAILURE_SCHEMATIC` returned as `Ok(payload)` (the `RpcHandlerError` channel flattens headers). `serve_tool` refuses a priced descriptor (`UnenforceablePricing`); `serve_tool_paid` refuses an unpriced one (`MissingPricingTerms`). | `sdk/src/tool.rs:337-339,419-478,1385-1487`, `sdk/src/tool_payment.rs:47-91` |
| Engine redeem: `PaymentEngine::redeem_for_invocation(tool_id, quote_id, binding) -> RedeemDecision` checks binding-required → unknown quote → 64-byte sig by `rec.caller_hex` over `invocation_binding_transcript(quote_id, tool_id)` → frozen → settled/billed → tool binding (`rec.capability.split_once('/')` tail == `tool_id`) → `rec.redeemed` → sets `redeemed = true`; one `mutate_json_if_changed` on `payment-engine.json` under the fs2 sidecar lock. | `payments/src/engine/mod.rs:2021-2143`, `policy/store.rs:252-263` |
| The binding transcript signs **only** `quote_id ‖ tool_id` (domain `net.payments.invocation_binding@1`). | `engine/mod.rs:276-285`, signed at `flow/mod.rs:886-895` |
| `PaymentQuote.input_hash: Option<String>` is the *designed* input-commitment carrier and participates in `terms_hash` → `quote_id`. Today `issue_quote` passes `None`; `QuoteRequest` has no input-hash field; `QuoteRecord` does not persist it; nothing verifies it at redeem. | `core/quote.rs:36-74,131-168`, `engine/mod.rs:801-810`, `core/quote_request.rs:73-93`, `engine/mod.rs:300-343` |
| Engine acceptance is payload-idempotent: `QuoteRecord.{idempotency_key, payload_hash, in_flight, in_flight_since_ns}` and `EngineState.consumed` (payload hash → quote) make a re-sent identical payload resolve to the original verdict; `in_flight_ttl_ns` reclaims stale claims. | `engine/mod.rs:300-360,684` |
| Caller side: `CallerPaymentFlow::run(capability, pricing_terms) -> CallerDecision::{Paid{quote_id, binding_sig, proof} \| RequiresPaymentApproval \| Denied \| Failed{message, retryable}}` — a single monolithic verb: quote → spend `check_and_reserve` → author payload → `pay`. `Failed` carries **no** quote id. Resumes an approved held quote via `SpendPolicyEngine::approved_quote(capability)`. `ProviderChannel::quote` exists standalone; no public quote-only verb on the flow or the Python gateway. `MeshPaymentChannel` resolves the provider node from `<node_id>/<capability>`. | `flow/mod.rs:599-947`, `flow/mesh.rs:295-309,322-386` |
| `net-payments` depends on `net-sdk` (`EngineToolPaymentGate: net_sdk::tool_payment::ToolPaymentGate`), never the reverse. | `flow/mesh.rs:425-447`, `payments/Cargo.toml` |
| Python `PaymentProvider` owns the engine (`AdmitAll`, `serve_payments`, billing log) and exposes `publish_paid_tools`; `CapabilityGateway` exposes `invoke` (payment internal), `approve_payment`, `reject_payment`, `pending_payments`. `sdk-py` (`net_sdk`) has no A2A and no payments surface — A2A lives in `bindings/python` + `bindings/node` only. | `bindings/python/src/payment_provider.rs:417-483,613-668`, `capability_gateway.rs:976-1040` |
| Durable primitives already in the SDK crate: `PinStore::mutate` (JSON file, temp+fsync+rename, 0600/DACL, fs2 sidecar `.lock` **per call**) and `RedexFile` (re-exported under `cortex`). `fs2` supports `try_lock_exclusive` held for an object's lifetime. | `sdk/src/pins.rs:202-325`, `sdk/src/cortex.rs:70-73` |
| Tests: `sdk/tests/a2a_task_ownership.rs` (3), `sdk/tests/tool_serve_paid.rs` (`RecordingGate` scripted gate idiom), `bindings/python/tests/test_a2a.py`, `bindings/node/test/a2a.test.ts`. SDK integration tests are auto-discovered by the `rust-sdk-tests` nextest job. | `ci.yml:2170-2267` |

### 0.1 Review findings (revision 1 → 2) and where each is closed

| # | Finding | Closed in |
|---|---|---|
| 1 | Validation happened before *redemption*, not before *payment*; executor-side schema validation is post-settlement | **D2** — `net.a2a.prepare` runs validation + application preflight + capacity reservation before any quote; the purchase is bound to that reservation; post-payment revocation enters reconciliation (D6) |
| 2 | Idempotent redemption allowed duplicate execution across owners and after result pruning | **D3** — purchase hash includes the provider-minted `admission_id` (owner-bound); **D6** — launch ledger is never pruned and is separate from result retention; witnesses in WS-C |
| 3 | Caller could re-quote after a lost pay reply; `Failed` lacks the quote id | **D4** — durable caller `PurchaseAttempt` written *before* pay, resumed by re-sending the identical payload; `prepare`/`purchase` split; `Failed` carries `quote_id` |
| 4 | Commitment framing ambiguous across array boundaries | **D3** — canonical typed JSON, domain-separated; boundary-shift negative vectors |
| 5 | No atomic once-only launch claim; epoch ≠ liveness | **D6** — lifetime-exclusive journal ownership (OS lock held for the owner's life) + CAS state transitions; write-failure semantics; overlapping-instance witness |
| 6 | `TaskPaymentProof` reversed the crate dependency; Python `submit_task_paid` lacked the brief | **D5** — `PreparedTask` + `TaskPaymentProof` live in the SDK; one frozen Rust↔Python `prepare / purchase / submit` contract with complete JSON handles |

### 0.2 The five requirements, mapped to today's gaps

| Requirement | Gap in today's code |
|---|---|
| 1. Free vs paid is an explicit provider choice | One serving path, always free; no catalog; no pricing carrier; no discovery. |
| 2. Admission separated from launch | `submit` reserves + spawns atomically; no preflight, no reservation, no await-for-duplicates, no release. |
| 3. Payment bound to the work | Transcript binds `quote_id ‖ tool_id`; `input_hash` unused end-to-end. |
| 4. Caller + discovery path | No offer/describe; no quote-only verb; no durable purchase attempt; `submit_task` cannot carry headers or a caller-chosen id; refusals have no in-band shape. |
| 5. Ownership + durability | Deliverer-as-owner only; registry in-memory; no launch claim; no paid-but-not-started state. |

---

## 1. Design decisions (locked)

### D1 — Provider configuration decides free vs paid; the caller never selects

- **`Mesh::serve_a2a(registry, executor)` is unchanged** — the backward-compatible free path. No payment dependency, no journal, no catalog.
- **New: `Mesh::serve_a2a_configured(registry, executor, A2aServiceConfig)`** — the strict, catalog-driven path:

```rust
pub struct A2aServiceConfig {
    /// service_id → policy. A brief must name a service in this map.
    pub services: BTreeMap<String, A2aServicePolicy>,
    /// Required iff any service is `Paid`; refused at serve time otherwise.
    pub payment: Option<Arc<dyn TaskAdmissionGate>>,
    /// Application preflight (schema/size/availability). Runs at prepare and again at submit.
    pub preflight: Option<Arc<dyn TaskPreflight>>,
    /// Who a submission is attributed to (D5).
    pub principal: A2aPrincipal,
    /// Required iff any service is `Paid` (D6). Opening it takes lifetime-exclusive ownership.
    pub journal: Option<A2aAdmissionJournal>,
}
pub enum A2aServicePolicy { Free(A2aOffer), Paid(A2aOffer) }
pub struct A2aOffer {
    pub service_id: String,
    pub revision: String,
    pub description: Option<String>,
    /// `net.pricing.terms@1` canonical JSON; `Some` iff Paid.
    pub pricing_terms: Option<String>,
    pub bounds: A2aBounds,           // max_prompt_bytes, max_context_refs, max_tags, max_tag_bytes, max_in_flight
    pub reservation_ttl_secs: u64,   // how long a prepared-but-unsubmitted reservation holds capacity (≥ quote TTL)
    pub retention_secs: u64,         // published terminal-RESULT retention (status/result reads)
}
#[async_trait]
pub trait TaskPreflight: Send + Sync {
    /// Application-level admission: schema, size, availability, provider authority. Pure — no side effects.
    async fn preflight(&self, owner: TaskOwner, offer: &A2aOffer, brief: &TaskBrief) -> Result<(), String>;
}
```

- **Serve-time invariants (fail closed, mirror `serve_tool_paid`):**
  - `Paid(offer)` with `pricing_terms.is_none()` → `ServeError::MissingPricingTerms(service_id)`.
  - `Free(offer)` with `pricing_terms.is_some()` → `ServeError::UnenforceablePricing(service_id)`.
  - any `Paid` and (`payment.is_none()` or `journal.is_none()`) → new `ServeError::A2aPaidMisconfigured(String)` (core `mesh_rpc.rs`, beside `MissingPricingTerms`). **A configured paid service never degrades to free.**
  - a catalog of only `Free` entries needs neither gate nor journal (still zero payment deps).
- **The brief names the service:** `TaskBrief` gains `#[serde(default)] service: Option<String>` and `#[serde(default)] revision: Option<String>`. Legacy `serve_a2a` ignores both. Configured mode rejects `None`/unknown service (`SubmitRejection::UnknownService`) and a non-current revision (`StaleRevision`) before anything else. The derive ignores unknown JSON fields, so old free servers accept new briefs.
- One node serves one A2A handler set (`AlreadyServing`); "both free and paid" on a node is two catalog entries, never two serving paths.

### D2 — Prepare (validate + reserve) before money; submit (redeem + claim + launch) after

Two provider verbs, both on the configured path. **`net.a2a.prepare` is uncharged and side-effect-free except for a bounded capacity reservation.** Nothing in the purchase can start until it has run, because the purchase hash (D3) needs the `admission_id` it mints.

**`net.a2a.prepare`** (`A2A_PREPARE_SERVICE`):

```
P1 authenticate   owner = principal(ctx)                                        (D5)
P2 validate       decode brief; service ∈ catalog; revision current; A2aBounds
P3 preflight      preflight.preflight(owner, offer, brief)?  → in-body rejection, nothing reserved
P4 resolve        journal.lookup(owner, task_id):
                    ledger hit (already launched, result may be retired)  → PrepareReply::Retired
                    record with same commitment                           → PrepareReply::Reservation (idempotent; same admission_id)
                    record with different commitment                      → IdReusedForDifferentBrief
                    none → capacity: in_flight(service) < max_in_flight else PrepareReply::Busy (nothing written)
                           mint admission_id (16 random bytes, hex); journal CAS insert
                           Reserved { admission_id, commitment, expires_at = now + reservation_ttl_secs }
P5 reply          AdmissionReservation { task_id, admission_id, commitment, purchase_hash, capability,
                                          pricing_terms: Option<String>, expires_at }
```

`Free` services also serve prepare (no pricing; reservation optional for the caller — a free `submit` without prior prepare reserves inline). `Paid` services **require** a prior reservation: a paid submit whose `(owner, task_id)` has no journal record is refused in-body (`NoReservation`) before touching the gate — that caller cannot have a matching quote anyway.

**`net.a2a.task`** (configured mode):

```
S1 authenticate   owner = principal(ctx)
S2 validate       decode; service/revision/bounds (cheap re-check)
S3 reserve        registry.reserve(owner, brief):
                    Existing(id)  → ack(id)                          [no gate call]
                    Pending(rx)   → await rx → same ack / refusal     [converge]
                    Reserved(t)   → continue
S4 resolve        journal.lookup(owner, task_id):
                    ledger hit                     → t.release(); in-body Retired { task_id }
                    Reserved/Paid w/ same commitment → continue (expired Reserved: re-acquire capacity; Busy → refuse retryable, NOT redeemed)
                    none (Paid service)            → t.release(); in-body NoReservation
                    none (Free service)            → journal Reserved inline (no admission_id needed)
S5 recheck        preflight.preflight(owner, offer, brief) again (authority may have changed since prepare).
                    Free → failure is an in-body rejection, reservation released.
                    Paid → failure is RECONCILIATION (D6): journal Released { reason, claimed_quote_id };
                           reply ERR_PAYMENT schematic `admission_revoked` (funds_moved=unknown, prior_payment=unknown,
                           retryable=false, safe_to_requote=false, next_action=contact_provider_operator). Gate never called.
S6 payment        Free → skip.
                  Paid → HDR_PAYMENT_QUOTE + HDR_PAYMENT_BINDING required (bearer refused: `binding_required`);
                         gate.redeem(TaskPaymentClaim { tool_id, quote_id, binding, expected_input_hash: purchase_hash })
                           Err(denial) → t.release(); journal Released{denial.reason}; reply ERR_PAYMENT + schematic
                           Ok(evidence) → journal CAS Reserved → Paid { quote_id, payer }
                                          (OrgAdmitted: evidence.payer must == admitted.caller, else `binding_rejected` posture)
S7 launch claim   journal CAS (Reserved|Paid) → Launched  — durable BEFORE spawn; ledger entry appended in the same write.
                    write failure → t.release(); journal unchanged; reply retryable `journal_unavailable` (nothing ran;
                    a retry resumes at S4: redeem is idempotent for this purchase hash — D3)
S8 launch         t.launch(executor) → ack(id)
```

Retry semantics:

| Situation | Result |
|---|---|
| Same owner + id + identical brief, task live (any state) | `Existing` → original id; no gate call, no second spawn. |
| Same owner + id + different brief | `IdReusedForDifferentBrief` at P4/S3 — before any payment. |
| Two identical submissions racing | Second gets `Pending`, returns the first's verdict; one redeem, one launch. |
| Invalid / oversized / unavailable / unauthorized task | Refused at **P2–P4, before a quote exists** — nothing to reconcile. |
| Provider becomes unavailable or revokes between prepare and submit (paid) | S4 `Busy` (before redeem, retryable with the same proof) or S5 reconciliation (D6) — never an "unpaid rejection". |
| Gate denies | Reservation released; retry with a valid quote for the *same* admission starts over at S3. |
| Same quote, different brief or different owner | Engine `input_binding_mismatch` (D3); reservation released. |
| Retry after the result was retired (retention passed / `forget`) | `Retired { task_id }` in-body; ledger prevents redeem and relaunch. |

**Why not "redeem before today's `submit`":** that consumes the quote before the id-conflict check and turns an identical retransmit into `already_redeemed`. The ordering prepare → reserve → redeem → claim → launch is the point; engine-side idempotency per purchase hash (D3) covers the crash window between redeem and the journal write.

### D3 — The purchase is bound to one reservation of the exact work, through the quote's `input_hash`

No new transcript, no new signature scheme. `quote_id = blake3(provider ‖ caller ‖ terms_hash ‖ issued_at)` and `terms_hash` covers `input_hash`; the existing caller-signed binding over `quote_id ‖ tool_id` therefore transitively proves the payer authorized *this purchase of this work under this reservation*.

**Commitment and purchase hash — canonical typed encoding** (SDK `a2a.rs`, feature `net`; `blake3` and `serde_json` already deps):

```rust
/// Serialized as JSON with fields in this declaration order; arrays are JSON arrays,
/// strings are JSON strings — framing is unambiguous by construction.
#[derive(Serialize)]
struct TaskCommitmentV1<'a> {
    object: &'static str,          // "net.a2a.commitment@1"
    offer_hash: &'a str,           // blake3 hex of OfferV1 (below)
    service_id: &'a str,
    revision: &'a str,
    task_id: &'a str,
    prompt: &'a str,
    context_refs: &'a [String],
    tags: &'a [String],
}
#[derive(Serialize)]
struct OfferV1<'a> { object: &'static str /* "net.a2a.offer@1" */, service_id, revision, pricing_terms: Option<&str>, bounds: &A2aBounds, reservation_ttl_secs: u64, retention_secs: u64 }
#[derive(Serialize)]
struct PurchaseV1<'a> { object: &'static str /* "net.a2a.purchase@1" */, admission_id: &'a str, commitment: &'a str }

pub fn task_commitment(offer: &A2aOffer, brief: &TaskBrief) -> String   // blake3 hex over serde_json::to_vec(TaskCommitmentV1)
pub fn purchase_hash(admission_id: &str, commitment: &str) -> String       // blake3 hex over serde_json::to_vec(PurchaseV1)
```

- `serde_json` emits struct fields in declaration order, escapes strings canonically, and has no floats here; the `object` field domain-separates each hash. Encoding is fixed for `@1`; a change mints `@2`.
- **Quote binding:** `QuoteRequest.input_hash = purchase_hash(admission_id, commitment)`. Because `admission_id` is provider-minted per `(owner, task_id)` reservation, the same proof presented under another owner or another reservation computes a different expected hash at the provider → `input_binding_mismatch` — *before* the idempotency arm. This is what closes the cross-peer reuse hole without binding the u64 peer into the hash.
- **tool_id / capability:** `tool_id = "net.a2a.task/{service_id}"`; capability on the quote = `"{node_id}/net.a2a.task/{service_id}"`. `MeshPaymentChannel::provider_node` splits on the first `/` (node id ✓); the engine's tool-binding check uses the `split_once('/')` tail (`net.a2a.task/{service_id}` ✓). No engine change for tool binding.
- **Vectors (WS-A):** golden hex for a fixed offer+brief; one-field flips for every bound field; and negative framing vectors: `refs=["artifact:a"], tags=["tag:b"]` vs `refs=[], tags=["artifact:a","tag:b"]`; `refs=["a","b"]` vs `refs=["a,b"]`; `prompt="x", tags=[]` vs `prompt="x\"", tags=[]`-style escape cases; empty-vs-missing `context_refs`. All must differ.

**Engine changes (`net-payments`):**
1. `QuoteRequest` gains `#[serde(default, skip_serializing_if = "Option::is_none")] input_hash: Option<String>` (covered by the canonical signed bytes; absent → byte-identical to today) + `with_input_hash`.
2. `serve_payments` quote handler threads `request.input_hash` → `ProviderChannel::quote(.., input_hash: Option<&str>)` → `InProcessProvider` → `PaymentEngine::issue_quote(.., input_hash)` → `PaymentQuote::new(.., input_hash, ..)`. Trait param addition; impls: `MeshPaymentChannel`, `InProcessProvider`, test stubs.
3. `QuoteRecord` gains `#[serde(default)] input_hash: Option<String>` and `#[serde(default)] redeemed_for: Option<String>` (a pre-change `payment-engine.json` loads unchanged — fixture test).
4. New `PaymentEngine::redeem_for_task(tool_id, quote_id, binding: &[u8], expected_input_hash: &str) -> Result<RedeemDecision, EngineError>` sharing the private check closure with `redeem_for_invocation`, three deltas: binding mandatory (no `require_invocation_binding` opt-out for tasks); after the tool-binding check, `rec.input_hash != Some(expected)` → `RedeemDenialReason::InputBindingMismatch`; `rec.redeemed` is **idempotent per purchase hash** — `rec.redeemed_for == Some(expected)` → `Admitted` (no dirty write), anything else → `AlreadyRedeemed`. Admission writes `redeemed = true, redeemed_for = Some(expected)`. `RedeemDecision::Admitted` gains `payer: EntityId`.
   *Why idempotent redemption is safe now:* the hash it is idempotent on names one reservation of one owner's one task; at-most-once **execution** is owned by the journal launch claim + ledger (D6), never by redemption. It exists only so a provider that crashed between the engine write and the journal write reconciles on retry instead of failing `already_redeemed` or charging again.
5. `flow::denial_for` gains `input_binding_mismatch` (stage `redeem`, class `security_violation`, actor `caller_operator`, `retryable=false`, `safe_to_retry=false`, `safe_to_requote=false`, `funds_moved=unknown`, `prior_payment=unknown`, no `next_action` — the `wrong_tool_binding` posture). SDK `FailureSchematic` doc table + `failure_vocab` gain the handler-authored A2A reasons: `admission_revoked`, `no_reservation`, `journal_unavailable` (stage `admission`), and `input_binding_mismatch`. The vocabulary is SDK-owned (`payments/src/core/versioning.rs:37-42`); no envelope registry change.

**SDK admission contract** (new ungated `sdk/src/a2a_payment.rs`, the A2A twin of `tool_payment.rs`; free A2A never links `net-payments`):

```rust
pub struct TaskPaymentClaim<'a> {
    pub tool_id: &'a str,               // "net.a2a.task/{service_id}"
    pub quote_id: &'a str,
    pub binding: &'a [u8],              // mandatory
    pub expected_input_hash: &'a str,   // purchase_hash(admission_id, commitment)
}
pub struct TaskPaymentEvidence { pub quote_id: String, pub payer: [u8; 32] }
#[async_trait]
pub trait TaskAdmissionGate: Send + Sync {
    async fn redeem(&self, claim: TaskPaymentClaim<'_>) -> Result<TaskPaymentEvidence, GateDenial>;
}
```
`net-payments` (feature `mesh`) provides `EngineTaskAdmissionGate` → `flow::redeem_task_via_engine` → `redeem_for_task`, the single denial-render site (the `EngineToolPaymentGate` shape).

### D4 — Caller: prepare (read-only) → purchase (durable attempt, resumable) → submit (evidence)

**Discovery:** uncharged `A2A_DESCRIBE_SERVICE = "net.a2a.describe"` → `Vec<A2aOffer>`; requester `Mesh::describe_a2a(node)`. Provider selection is exact-node; mesh-wide A2A search is out of scope (G4 wants exact-provider purchase).

**Caller flow** (`net-payments`, new `flow/a2a.rs`, feature `mesh`) with a durable **purchase store** (`a2a-purchases.json`, the `policy/store.rs` JSON+lock idiom, beside `payment-policy.json`):

```rust
pub struct PurchaseAttempt {
    pub provider_node: u64, pub prepared: PreparedTask,           // SDK type (D5) — carries the brief
    pub quote_bytes: Vec<u8>, pub quote_id: String, pub quote_expires_at_ns: u64,
    pub payload_bytes: Option<Vec<u8>>,                            // authored x402 payload, byte-exact
    pub state: PurchaseState, pub updated_at_ns: u64,
}
pub enum PurchaseState {
    Quoted,                                   // prepare done; nothing reserved on the spend side
    AwaitingApproval,                         // spend policy held the quote for an operator
    Paying,                                   // payload authored + persisted; pay request may be in flight
    Paid { proof: TaskPaymentProof, billing: serde_json::Value },
    Unknown { last_error: String },           // pay reply lost / transport failure after send
    Refused { reason: String },               // provider/facilitator refused; funds not moved per engine reply
}
```

- `A2aCallerFlow::prepare_task(node, &offer, &brief) -> Result<PreparedTask, A2aFlowError>` — **read-only**: provider `net.a2a.prepare` → `AdmissionReservation`; `ProviderChannel::quote` with `input_hash = purchase_hash`; persist `Quoted` keyed `(node, task_id)`. If an unresolved attempt already exists for the key with the same commitment, return it (no new quote). No spend reservation, no payment. Closes G4 "display a price without spending".
- `A2aCallerFlow::purchase_task(node, task_id) -> A2aPurchase::{ Paid(TaskPaymentProof) | RequiresPaymentApproval{quote_id, policy_reason, approve_hint} | Denied{policy_reason} | Unknown{quote_id} | Failed{quote_id, message, retryable} }` — consumes **the stored attempt's quote**, never a fresh one:
  - `Quoted`/`AwaitingApproval` → `spend.check_and_reserve(stored quote)` (pending → persist `AwaitingApproval`, return); author payload; **persist `Paying { payload_bytes }` before `pay`**; `pay(stored quote, stored payload)`.
  - `Paying`/`Unknown` → re-send the **identical stored payload** (`accept_payment` is payload-idempotent via `EngineState.consumed` / `idempotency_key`; the engine returns the original verdict, `SettlementPending` keeps it in `Unknown` with `retryable=true`). Never a new quote, never a new payload, while an attempt is unresolved.
  - `Paid` → return the stored proof (idempotent).
  - Quote expired while `Quoted` (never paid) → the only re-quote path, explicit: `prepare_task` again → same `admission_id` (provider-idempotent) → new quote; the old attempt is superseded in place.
- `CallerPaymentFlow` refactor: split `run` into `quote_bound(capability, terms, input_hash) -> Quote`, `reserve_spend(&quote) -> SpendDecision`, `author(&quote) -> payload`, `pay_exact(&quote_bytes, &payload_bytes) -> PayOutcome`; `run` composes them (behavior unchanged, existing tests pass). `CallerDecision::Failed` gains `quote_id: Option<String>`. The approved-quote resume checks the held quote's `input_hash` matches (an approval is for one exact purchase).

**Submission:** `Mesh::submit_task_paid(node, &PreparedTask, &TaskPaymentProof) -> Result<TaskAck, A2aFlowError>` sends `prepared.brief` with `HDR_PAYMENT_QUOTE` / `HDR_PAYMENT_BINDING` via `CallOptionsExt::with_request_header`, using raw `Mesh::call` so reply headers are readable. `submit_task` (free) unchanged.

**Refusals on the wire:** payment/admission refusals from the configured `SubmitHandler` are `RpcStatus::Application(ERR_PAYMENT)` + `HDR_FAILURE_SCHEMATIC` + human body — byte-identical to `PaidToolHandler` (`tool.rs:1411-1420`). Non-payment rejections stay in-body `TaskAck { accepted: false, reason }`. Requester maps the application error to `A2aFlowError::PaymentRefused { message, schematic: Option<FailureSchematic> }` (`FailureSchematic::from_header_bytes`, the `mesh_gateway.rs:470` idiom).

**Uncharged verbs:** `describe`, `prepare`, `status`, `cancel` never touch the gate. Terminal *results* are retained `retention_secs` and readable from the journal after registry eviction/restart; the ledger outlives them (D6).

### D5 — Evidence types and principal

**Types live in the SDK** (`net-payments` depends on `net-sdk`, never the reverse):

```rust
// sdk/src/a2a.rs (feature net)
pub struct AdmissionReservation { pub task_id: String, pub admission_id: String, pub commitment: String,
    pub purchase_hash: String, pub capability: String, pub pricing_terms: Option<String>, pub expires_at: u64 }
pub struct PreparedTask { pub provider_node: u64, pub brief: TaskBrief, pub offer_hash: String,
    pub reservation: AdmissionReservation }            // complete: everything submit needs
// sdk/src/a2a_payment.rs (ungated)
pub struct TaskPaymentProof { pub quote_id: String, pub binding_sig: Vec<u8> }   // transport evidence only
```
Billing evidence (`serde_json::Value`) stays in the payments-side `PurchaseAttempt`, not on the wire type.

**Principal.**
```rust
pub enum A2aPrincipal {
    /// `TaskOwner::Peer(ctx.session_peer)`. Supported topology: DIRECT sessions only (documented;
    /// a relay is the deliverer and owns what it forwards).
    SessionPeer,
    /// Serve prepare/task/status/cancel/describe as PROTECTED (`serve_rpc_owner_scoped` /
    /// `serve_rpc_granted` per `OrgAccess`) so `ctx.org_admission` is `Some`:
    /// `TaskOwner::Entity(admitted.caller)` — a verified end-to-end principal.
    OrgAdmitted(OrgAccess),
}
```
- `TaskOwner` gains `Entity([u8; 32])` (stays `Copy + Hash`; `TaskRecord` carries no owner, wire unaffected).
- Every paid admission records `evidence.payer`. `SessionPeer` ⇒ *payer authorized this exact purchase (binding sig over a quote whose id commits to `admission_id` + commitment); requester = delivering peer*. `OrgAdmitted` ⇒ **`payer == admitted.caller` enforced** (mismatch → `binding_rejected` posture, reservation released). Requester side for `OrgAdmitted` uses the existing `CallOptions::org_proof_intent` / `Mesh::org(..)` path.

### D6 — Durable admission: lifetime-exclusive ownership, CAS transitions, ledger ≠ retention

**Ownership contract (chosen: enforced exclusive owner, not distributed fencing).** `A2aAdmissionJournal::open(path)` takes an fs2 **exclusive lock on the journal file itself and holds it for the journal's lifetime** (not the per-write sidecar of `PinStore`). A second open — same process or another — fails (`A2aJournalError::OwnedElsewhere`), so `serve_a2a_configured` on an owned journal fails at serve time with `A2aPaidMisconfigured`. OS advisory locks release on process death; a crashed owner's successor therefore acquires cleanly and treats what it finds conservatively (table below). Documented limit: local filesystems only (network filesystems do not honor the lock — same caveat `payment-engine.json` carries). No epoch field; liveness is the lock.

**Records — two tables in one file, different lifetimes:**

```rust
pub struct AdmissionRecord {                       // RESULT table — pruned after retention_secs (terminal/released only)
    pub owner: TaskOwner, pub task_id: String, pub service_id: String, pub revision: String,
    pub admission_id: Option<String>,              // None for free inline reservations
    pub commitment: String, pub brief: TaskBrief,
    pub state: AdmissionState, pub updated_at: u64,
}
pub enum AdmissionState {
    Reserved { expires_at: u64 },
    Paid     { quote_id: String, payer: [u8; 32] },
    Launched { quote_id: Option<String>, payer: Option<[u8; 32]> },
    Terminal { quote_id: Option<String>, payer: Option<[u8; 32]>, state: TaskState },
    Released { reason: String, claimed_quote_id: Option<String> },   // incl. post-payment reconciliation entries
}
pub struct LaunchLedgerEntry {                     // LEDGER — never pruned automatically
    pub owner: TaskOwner, pub task_id: String, pub admission_id: Option<String>,
    pub commitment: String, pub quote_id: Option<String>, pub launched_at: u64,
}
```

- **CAS API:** `reserve(owner, task_id, |existing| …) -> Reserved|Existing|Conflict`, `transition(owner, task_id, from: &[StateTag], to: AdmissionState) -> Result<(), Conflict|Io>`, `claim_launch(owner, task_id) -> Result<(), Conflict|Io>` (writes `Launched` **and** the ledger entry in one atomic file replace), `lookup`, `ledger_has(owner, task_id)`, `prune_results(now)`. Each call: in-process mutex → load → check `from` → mutate → temp+fsync+rename. A `Conflict` is a programming error surfaced to the handler as `journal_unavailable` (retryable); an `Io` failure leaves the file untouched (rename is atomic) and the handler follows S7's rule: **nothing runs unless the claim write returned Ok.**
- **Ledger vs retention:** `forget()`, `evict_terminal()`, and `prune_results()` remove **result** records only. `ledger_has` is consulted at P4/S4 and answers `Retired` — a retired result can never make its payment reusable for execution, because the redeem step is never reached once the ledger has the `(owner, task_id)`. Ledger entries are tiny (< 200 B) and bounded by the number of launches ever; the engine's `consumed_transactions` has the same never-prune policy.
- **Write points:** P4 (`Reserved`), S4 free-inline (`Reserved`), S5 paid-revoked (`Released{claimed}`), S6 (`Paid` / `Released`), S7 (`claim_launch`), registry terminal hook (`Terminal`) via `TaskRegistry::with_terminal_hook` (configured mode only).

**Recovery on `open()` (the successor owner):**

| Found | Status reply | On identical retry | Money |
|---|---|---|---|
| `Reserved` unexpired | unknown (`null`) — prepare returns the same reservation | resume at S4 | none moved unless the caller paid; the quote is still unredeemed or idempotently redeemable |
| `Reserved` expired | prepare/submit re-acquire capacity or `Busy` (retryable) | same | same |
| `Paid` (crash between redeem and claim) | `TaskState::Interrupted { detail: "paid_not_started" }` | resume at S7 → launch once (work provably never claimed) | reconciled, not repurchased |
| `Launched` without `Terminal` | `Interrupted { detail: "outcome_unknown" }` | `Existing` → the interrupted record; **never relaunched** | evidence retained; operator reconciles |
| `Terminal` / `Released` | as recorded, until retention | `Existing` / `Retired` | — |

`TaskState::Interrupted { detail: String }` is a new terminal variant produced **only** by the configured path; `cancel` on it returns `false`; it is never auto-pruned while `Launched` remains unresolved in the ledger's view (an operator's `resolve(owner, task_id, TaskState)` on the journal is the only exit). Rust requesters on older builds fail to decode it (serde tag); Python/Node return the JSON string untouched. The one wire addition of this slice.

**Settlement and acceptance are not atomic and are not made to look so.** The payer's evidence is the billing event + the caller's `PurchaseAttempt`; the provider's evidence is the journal; the schematic vocabulary already expresses `funds_moved=unknown`.

---

## 2. Workstreams

A and B are independent; C depends on A+B; D–F on C.

### WS-A — Registry and commitment (SDK `a2a.rs`)

- [ ] `Admission::{Existing, Pending(watch::Receiver<Option<Result<String, String>>>), Reserved(AdmissionTicket)}`; `TaskRegistry::reserve`; `AdmissionTicket::{launch, release}` (`launch` = the current spawn body verbatim incl. `PanicGuard` + biased `select!`); `Drop` of an unconsumed ticket = release; `TaskRegistry::with_terminal_hook`.
- [ ] `submit` = `reserve` + `launch`; `Pending` in the sync free path → `Ok(id)` (entry exists; only identical retransmits race today) — documented. Free wire byte-identical.
- [ ] `TaskState::Interrupted { detail }`; `TaskOwner::Entity([u8; 32])`; `SubmitRejection::{UnknownService, StaleRevision{expected, got}, BoundsExceeded{field, limit}, NoReservation, Retired, Busy}`; `TaskBrief::{service, revision, with_service, with_task_id}`.
- [ ] `A2aOffer`, `A2aBounds`, `AdmissionReservation`, `PreparedTask`; `task_commitment`, `purchase_hash`, `A2aOffer::hash()` — canonical typed JSON per D3.
- [ ] Per-entry retention override used by eviction; global TTL remains the default.
- [ ] Tests: `reserve_then_release_leaves_no_entry`, `a_dropped_ticket_releases_the_reservation`, `concurrent_identical_reserves_share_one_verdict`, `a_ticket_launches_exactly_once`, `interrupted_is_terminal_and_uncancellable`, `commitment_golden_vector` (fixed hex), `commitment_flips_for_every_bound_field`, `commitment_frames_array_boundaries` (the four negative vectors in D3), `purchase_hash_flips_with_admission_id`.

**Acceptance:** existing `a2a.rs` tests, `a2a_task_ownership.rs`, both binding suites unchanged and green; `submit` callers compile without edits.

### WS-B — Engine and caller flow (`net-payments`)

- [ ] `QuoteRequest.input_hash` + `with_input_hash`; threading through `serve_payments` → `ProviderChannel::quote` → `InProcessProvider` → `issue_quote` → `PaymentQuote::new`.
- [ ] `QuoteRecord.{input_hash, redeemed_for}`; fixture loads a pre-change `payment-engine.json` byte-for-byte.
- [ ] `PaymentEngine::redeem_for_task`, `RedeemDenialReason::InputBindingMismatch` (`"input_binding_mismatch"`), `RedeemDecision::Admitted { payer }`; one private closure shared with `redeem_for_invocation`.
- [ ] `flow::denial_for` row; SDK `tool_payment.rs` doc table + `failure_vocab` reasons (`input_binding_mismatch`, `admission_revoked`, `no_reservation`, `journal_unavailable`).
- [ ] `CallerPaymentFlow` staged verbs (`quote_bound`, `reserve_spend`, `author`, `pay_exact`), `run` composed from them; `CallerDecision::Failed.quote_id`; approved-quote resume checks `input_hash`.
- [ ] `flow/a2a.rs` (feature `mesh`): `PurchaseAttempt`/`PurchaseState` + `a2a-purchases.json` store; `A2aCallerFlow::{prepare_task, purchase_task, stored_attempt}`; `A2aPurchase`; `EngineTaskAdmissionGate: net_sdk::a2a_payment::TaskAdmissionGate`; `redeem_task_via_engine`.
- [ ] Tests `payments/tests/a2a_task_redeem.rs` (mock facilitator): `a_quote_carries_the_purchase_hash_into_its_id`, `redeem_for_task_refuses_a_mismatched_purchase_hash` (funds untouched, `redeemed` false), `redeem_for_task_is_idempotent_for_the_same_purchase_hash`, `a_second_purchase_hash_on_a_redeemed_quote_is_already_redeemed`, `a_bearer_task_redeem_is_binding_required_even_when_the_engine_allows_bearer_tools`, `an_approved_quote_resumes_only_for_its_own_purchase_hash`.
- [ ] Tests `payments/tests/a2a_caller_purchase.rs` (scripted `ProviderChannel` with fault injection): `prepare_task_moves_no_money_and_reserves_no_spend`, `purchase_consumes_the_prepared_quote_not_a_fresh_one`, `a_lost_pay_reply_is_resumed_by_resending_the_identical_payload` (channel swallows the first reply; second `purchase_task` sends byte-identical payload; engine `consumed` map returns the original verdict; exactly one billing event), `a_pending_settlement_purchase_stays_unknown_then_resolves_to_paid`, `a_caller_restart_mid_purchase_resumes_the_stored_attempt` (reopen the store from disk in `Paying`), `purchase_never_requotes_while_an_attempt_is_unresolved` (quote channel call count stays 1 across N retries), `an_expired_unpaid_quote_is_requoted_only_through_prepare_with_the_same_admission_id`.

**Acceptance:** `redeem_for_invocation`, `tool_serve_paid.rs`, `native_tool_gate.rs`, `mcp_gate_composition.rs`, admission-matrix bench unchanged in behavior; `cargo clippy -p net-payments --features mesh -- -D warnings` clean.

### WS-C — Configured serving path (SDK `mesh_a2a.rs`, `a2a_payment.rs`, `a2a_journal.rs`)

- [ ] `a2a_payment.rs` (ungated): `TaskPaymentClaim`, `TaskPaymentEvidence`, `TaskAdmissionGate`, `TaskPaymentProof`, `A2A_PREPARE_SERVICE`, `A2A_DESCRIBE_SERVICE`.
- [ ] `A2aServiceConfig`, `A2aServicePolicy`, `A2aPrincipal`, `TaskPreflight`; `ServeError::A2aPaidMisconfigured` in core.
- [ ] `A2aAdmissionJournal` per D6: lifetime-exclusive lock at `open`, result table + ledger, CAS `reserve`/`transition`/`claim_launch`, `prune_results`, `resolve` (operator), recovery pass at open; `#[cfg(feature = "testing")] fail_next_write()` fault hook.
- [ ] `Mesh::serve_a2a_configured`: validation per D1; `PrepareHandler` (P1–P5), configured `SubmitHandler` (S1–S8), `StatusHandler` (registry → journal fallback → `null`), `CancelHandler`, `DescribeHandler`; `OrgAdmitted` registers through `serve_rpc_owner_scoped` / `serve_rpc_granted` and reads `ctx.org_admission`.
- [ ] Requester: `Mesh::describe_a2a`, `Mesh::prepare_a2a(node, &brief) -> AdmissionReservation` (raw prepare; the payments `A2aCallerFlow` composes it), `Mesh::submit_task_paid(node, &PreparedTask, &TaskPaymentProof)`, `A2aFlowError::PaymentRefused`.
- [ ] Integration tests `sdk/tests/a2a_paid_admission.rs` (`#![cfg(all(feature = "net", feature = "cortex", feature = "testing"))]`; scripted `RecordingTaskGate` and `ScriptedPreflight` in the `RecordingGate` idiom; executor with a run counter and a barrier):
  - configuration: `a_free_configured_service_runs_without_a_gate_or_journal`, `a_paid_service_refuses_to_start_without_a_gate`, `..._without_a_journal`, `..._without_pricing`, `a_free_service_refuses_pricing`, `a_second_server_on_the_same_journal_refuses_to_start` (two `open`s on one path, in-process)
  - preflight before money: `prepare_rejects_invalid_oversized_or_unauthorized_briefs_without_reserving`, `prepare_is_idempotent_and_returns_the_same_admission_id`, `prepare_refuses_a_different_brief_under_the_same_id`, `prepare_reports_busy_at_max_in_flight_and_frees_on_expiry`
  - refusals: `an_unpaid_submit_to_a_paid_service_is_refused_before_the_executor` (ERR_PAYMENT, `missing_quote`, run counter 0, journal `Released`), `a_bearer_submit_is_refused_binding_required`, `a_paid_submit_without_a_reservation_is_refused_before_the_gate`, `a_revoked_preflight_after_payment_enters_reconciliation_not_unpaid_rejection` (gate never called; `Released{claimed_quote_id}`; schematic `admission_revoked`)
  - once: `a_paid_submit_redeems_once_and_runs_once`, `an_identical_retry_returns_the_original_task_without_a_second_redeem`, `concurrent_identical_submits_converge_on_one_admission`, `an_altered_brief_under_the_same_id_is_rejected_before_payment`, `a_stale_revision_is_rejected_before_payment`, `a_gate_denial_releases_the_reservation_and_a_valid_retry_launches_once`
  - duplicate-execution witnesses (finding 2): `a_reused_proof_from_another_peer_is_refused_and_never_launches` (three nodes; peer B replays A's headers + brief; B's own prepare mints a different `admission_id` → gate sees a different expected hash → `input_binding_mismatch`; without a prepare → `NoReservation`; run counter stays 1), `a_retry_after_result_retention_never_relaunches_or_redeems` (retention 0 + `forget`; identical retry → `Retired`; gate call count and run counter unchanged)
  - launch claim (finding 5): `a_failed_claim_write_never_launches_and_a_retry_launches_once` (`fail_next_write` before S7: reply retryable `journal_unavailable`, run counter 0, journal still `Paid`; retry → idempotent redeem → claim → one run)
  - ownership: `another_peer_cannot_read_or_cancel_a_paid_task`, `status_cancel_prepare_and_describe_are_uncharged`
  - recovery: `a_restart_after_payment_before_claim_surfaces_paid_not_started_and_resumes_once`, `a_restart_after_claim_surfaces_outcome_unknown_and_never_reruns`, `describe_a2a_lists_offers_with_pricing_and_bounds`

**Acceptance:** all green under the `rust-sdk-tests` feature set; legacy A2A tests untouched.

### WS-D — End-to-end with the real engine (`net-payments`, feature `mesh`)

- [ ] `payments/tests/a2a_paid_end_to_end.rs`: provider = `PaymentEngine` + mock facilitator + `EngineTaskAdmissionGate` + journal (tempdir); requester = `A2aCallerFlow` over `MeshPaymentChannel` + `SpendPolicyEngine` + purchase store (tempdir). Tests: `prepare_then_purchase_then_submit_runs_the_task_once`, `a_pending_approval_resumes_after_approve_payment_for_the_same_purchase_only`, `a_lost_pay_reply_is_reconciled_without_a_second_quote_or_charge` (one billing event), `a_lost_submit_reply_is_reconciled_by_resubmitting_the_same_proof`, `a_provider_restart_between_redeem_and_claim_reconciles_the_original_payment` (drop `ServeHandle`s + registry + journal owner; re-serve over the same `payment-engine.json` + journal path), `a_caller_restart_between_pay_and_submit_resumes_from_the_purchase_store`.

### WS-E — Python bindings (frozen contract) and Node parity where cheap

Every handle is a **complete JSON document**; nothing is resolved from a hash. `<prepared>` = `PreparedTask` JSON (includes `brief`, `reservation`, `offer_hash`, `provider_node`); `<proof>` = `TaskPaymentProof` JSON.

| Verb | Signature | Returns |
|---|---|---|
| `NetMesh.serve_a2a(callback)` | unchanged | `A2aServeHandle` |
| `NetMesh.submit_task(target_node_id, prompt, context_refs=[], tags=[], *, task_id=None, service=None, revision=None)` | caller-retained id (`None` → random) | `task_id` |
| `NetMesh.describe_a2a(target_node_id)` | — | JSON `A2aOffer[]` |
| `PaymentProvider.serve_a2a_configured(callback, services: dict[str, dict], journal_path: str, *, principal="session_peer", preflight=None)` | `services[id] = {revision, pricing_terms|None, bounds{…}, reservation_ttl_secs, retention_secs, description}`; `preflight: async (owner_json, offer_json, brief_json) -> None | str`; executor callback gains keyword `service`, `revision` | `A2aServeHandle`; raises on misconfiguration (never serves free) |
| `CapabilityGateway.prepare_task(target_node_id, service, prompt, context_refs=[], tags=[], task_id=None)` | read-only; persists the attempt | JSON `{status: ok|rejected|busy|retired, prepared: <prepared>, quote: {quote_id, amount, network, asset, expires_at_ns}}` |
| `CapabilityGateway.purchase_task(prepared_json)` | consumes the stored quote; resumable | JSON `{status: paid|requires_payment_approval|denied|unknown|failed, task_id, quote_id, proof: <proof>?, policy_reason?, approve_hint?, retryable?}` |
| `NetMesh.submit_task_paid(prepared_json, proof_json)` | headers from proof, brief from prepared | `task_id`; raises `PaymentRefused(message, schematic_json)` |
| `CapabilityGateway.approve_payment / reject_payment / pending_payments` | unchanged | as today |
| `CapabilityGateway(..., a2a_purchase_path=None)` | new ctor kwarg for the purchase store | — |

- [ ] `_net.pyi` + `python/net/__init__.py` exports (`PaymentRefused`); `bindings/python/tests/test_a2a_paid.py` mirrors WS-C/WS-D over the mock facilitator: free success, unpaid refusal with schematic, prepare-before-money (invalid brief rejected with no quote), paid runs once, retained-id identical retry, altered brief, cross-peer proof reuse, retry after retention, lost pay reply resume (fault-injected channel via a test-only flag), provider restart ambiguity (re-created `PaymentProvider` over the same paths), caller restart (re-created gateway over the same purchase path).
- [ ] Node: `submitTask` gains optional `taskId` (retained ids). Paid serving on Node **deferred** (no consumer; free behavior unchanged).

### WS-F — Docs, CI, release

- [ ] `sdk/src/mesh_a2a.rs` / `a2a.rs` module docs (principal table; prepare/purchase/submit; wire addition `Interrupted`); `tool_payment.rs` header names the A2A twin; `a2a_journal.rs` documents the local-filesystem lock limit.
- [ ] `web/src/content/docs/guides/agent-to-agent.md` ("paid services": prepare → purchase → submit, recovery table), `docs/data/capabilities/event-bus.yaml` A2A matrix, release note under `net/crates/net/docs/releases/` mirrored via `npm run sync:releases`, `SECURITY_DEFAULTS_0.35.md` §8 relay caveat cross-references `A2aPrincipal`.
- [ ] `.claude/skills/net-event-bus` A2A section: Python prepare/purchase/submit example (CI executes skill examples — also a witness).
- [ ] CI: SDK tests auto-discovered (`ci.yml:2267`) — add `testing` to that job's feature list if absent (needed for `fail_next_write`); confirm the `net-payments` job runs with `mesh`; `test_a2a_paid.py` auto-discovers under the existing maturin feature list (`ci.yml:2851-2863`). No new core `tests/*.rs` ⇒ no pin-guard change.
- [ ] AGENTS.md pre-push checklist incl. `cargo doc -D warnings` for `net-payments`, `sdk`, `bindings/python`.

---

## 3. Public surface summary

**Rust (SDK):** unchanged `serve_a2a`, `submit_task`, `task_status`, `cancel_task`, `TaskRegistry::submit`. New: `serve_a2a_configured`, `describe_a2a`, `prepare_a2a`, `submit_task_paid`; `A2aServiceConfig`, `A2aServicePolicy`, `A2aOffer`, `A2aBounds`, `A2aPrincipal`, `TaskPreflight`, `A2aAdmissionJournal`; `AdmissionReservation`, `PreparedTask`; `TaskRegistry::{reserve, with_terminal_hook}`, `Admission`, `AdmissionTicket`; `TaskBrief::{service, revision, with_service, with_task_id}`; `TaskOwner::Entity`; `TaskState::Interrupted`; `SubmitRejection::{UnknownService, StaleRevision, BoundsExceeded, NoReservation, Retired, Busy}`; `A2aFlowError::PaymentRefused`; `a2a_payment::{TaskAdmissionGate, TaskPaymentClaim, TaskPaymentEvidence, TaskPaymentProof, A2A_PREPARE_SERVICE, A2A_DESCRIBE_SERVICE}`; `task_commitment`, `purchase_hash`. Core: `ServeError::A2aPaidMisconfigured`.

**Rust (`net-payments`, feature `mesh` unless noted):** `QuoteRequest::with_input_hash` (ungated); `ProviderChannel::quote(.., input_hash)`; `PaymentEngine::{issue_quote(.., input_hash), redeem_for_task}`; `RedeemDecision::Admitted { payer }`; `RedeemDenialReason::InputBindingMismatch`; `CallerPaymentFlow::{quote_bound, reserve_spend, author, pay_exact}`, `CallerDecision::Failed.quote_id`; `flow::a2a::{A2aCallerFlow, PurchaseAttempt, PurchaseState, A2aPurchase, EngineTaskAdmissionGate}`.

**Python:** table in WS-E.

**Wire:** `net.a2a.prepare`, `net.a2a.describe` (new, uncharged); `net.a2a.task` accepts `net-payment-quote` / `net-payment-quote-sig` and may answer `Application(0x8006)` + `net-failure-schematic`; `TaskBrief.{service, revision}` (optional); `TaskState::interrupted` (configured path only); `QuoteRequest.input_hash` (optional, signed).

---

## 4. Acceptance matrix

| Invariant | Witness |
|---|---|
| Free success unchanged | legacy `mesh_a2a.rs` tests, `a2a_task_ownership.rs`, `test_a2a.py`, `a2a.test.ts` — no edits |
| Intentionally free needs no payments | `a_free_configured_service_runs_without_a_gate_or_journal`; `net_sdk` builds `a2a_payment` without `net-payments` |
| Paid never becomes free | the four `a_paid_service_refuses_to_start_*` tests, `a_free_service_refuses_pricing` |
| Validation before money, not before redemption | `prepare_rejects_invalid_oversized_or_unauthorized_briefs_without_reserving`, `prepare_task_moves_no_money_and_reserves_no_spend` |
| Post-payment failure is reconciliation | `a_revoked_preflight_after_payment_enters_reconciliation_not_unpaid_rejection` |
| Unpaid refusal, structured | `an_unpaid_submit_to_a_paid_service_is_refused_before_the_executor` (+ Python schematic passthrough) |
| Paid execution once | `a_paid_submit_redeems_once_and_runs_once`, `prepare_then_purchase_then_submit_runs_the_task_once` |
| Identical retry / lost replies | `an_identical_retry_returns_the_original_task_without_a_second_redeem`, `a_lost_pay_reply_is_reconciled_without_a_second_quote_or_charge`, `a_lost_submit_reply_is_reconciled_by_resubmitting_the_same_proof` |
| Never re-quote an ambiguous purchase | `purchase_never_requotes_while_an_attempt_is_unresolved`, `a_pending_settlement_purchase_stays_unknown_then_resolves_to_paid`, `a_caller_restart_mid_purchase_resumes_the_stored_attempt` |
| Altered request | `an_altered_brief_under_the_same_id_is_rejected_before_payment`, `redeem_for_task_refuses_a_mismatched_purchase_hash`, `commitment_frames_array_boundaries` |
| No duplicate execution across owners / after retention | `a_reused_proof_from_another_peer_is_refused_and_never_launches`, `a_retry_after_result_retention_never_relaunches_or_redeems` |
| Once-only launch claim | `a_failed_claim_write_never_launches_and_a_retry_launches_once`, `a_second_server_on_the_same_journal_refuses_to_start`, `concurrent_identical_submits_converge_on_one_admission` |
| Owner-only, uncharged control | `another_peer_cannot_read_or_cancel_a_paid_task`, `status_cancel_prepare_and_describe_are_uncharged` |
| Restart ambiguity | `a_restart_after_payment_before_claim_*`, `a_restart_after_claim_*`, `a_provider_restart_between_redeem_and_claim_reconciles_the_original_payment`, `a_caller_restart_between_pay_and_submit_resumes_from_the_purchase_store` |
| Approval is per-purchase | `an_approved_quote_resumes_only_for_its_own_purchase_hash`, `a_pending_approval_resumes_after_approve_payment_for_the_same_purchase_only` |

Behavioral witnesses only — no source-string assertions; concurrency staged with barriers on the scripted gate/executor, never sleeps.

---

## 5. Hermes gate closure (reassessed)

| Gate | Provided by this slice | Hermes still owns |
|---|---|---|
| G2 | `A2aPrincipal` (direct-session restriction or org-admitted `EntityId`), payer recorded on every paid admission and matched under `OrgAdmitted`, canonical `task_commitment` over service/revision/brief/bounds/terms, provider-minted `admission_id` binding the purchase to one owner's reservation, verification before redeem and before launch | mapping local request ids to `task_id`; choosing the topology |
| G3 | `net.a2a.prepare` validates + runs the application `TaskPreflight` + reserves capacity **before a quote exists**; capacity/bounds/authority refusals are unpaid in-body rejections; authority re-check at submit; post-payment revocation/capacity loss → `Released{claimed_quote_id}` reconciliation with an `admission_revoked` schematic, never an unpaid rejection | the `TaskPreflight` implementation (service schema, single-slot policy) |
| G4 | `prepare_task` (read-only, exact node, exact reservation), `purchase_task` consuming the stored quote with a durable resumable attempt, `submit_task_paid` with evidence; no provider switch, no re-quote, no asset change after prepare | freezing intent before prepare |
| G5 | unchanged (rail qualification outside this slice) | — |

---

## 6. Risks and non-goals

- **`ProviderChannel::quote` trait change** touches every impl and stub in `net-payments`; wire stays additive (`input_hash` optional in the signed request).
- **`CallerPaymentFlow` refactor into staged verbs** is the largest diff in WS-B; `run` must remain byte-for-byte equivalent (existing `mcp_gate` / `http402` tests are the guard).
- **Revision retirement vs. in-flight reservations:** changing `revision` after prepare strands paid quotes (`StaleRevision` at S2, funds moved). Mitigation is operational: keep the previous revision in the catalog ≥ `reservation_ttl_secs`, or accept the refund obligation. No automatic refund is built.
- **Journal lock on network filesystems** is not a liveness proof; documented as local-FS only (the `payment-engine.json` caveat).
- **`TaskState::Interrupted` decode on old Rust requesters** — configured path only; called out in the release note.
- **`submit` + `Pending`** in the sync free path returns the id without awaiting the verdict (today's callers only race identical retransmits; the entry already exists). Configured mode always awaits.
- **Idempotent redemption widens `redeem_for_task` only.** `redeem_for_invocation` keeps strict at-most-once; the two share one closure so the difference is one visible branch, and the hash it is idempotent on names one reservation of one owner's one task.
- **Non-goals:** metering/escrow/refunds; resumable execution across restart; mesh-wide A2A offer search; Node paid serving; relay-transparent ownership under `SessionPeer`; distributed (multi-owner) journal fencing; a new storage engine (`PAYMENTS_STORAGE_DISPOSITION.md` governs any replacement).
