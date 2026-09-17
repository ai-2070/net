# Implementation Plan: Optional paid admission for native A2A

**Implements:** the Net-side prerequisites of the Hermes plan `hermes-net/docs/plans/AGENT_TO_AGENT_PAYMENTS.md` — readiness gates **G2** (authenticated buyer context + request binding), **G3** (admission before financial side effects), **G4** (read-only prepare / exact-provider purchase) — so `hermes-net` can sell bounded work over native A2A instead of building a parallel paid-job protocol.

**The sentence:** an A2A service is *explicitly* free or paid by provider configuration; a paid submission is admitted in stages (authenticate → validate → reserve → redeem a quote bound to the exact brief → durable launch claim → launch once), retries converge on one task, and a crash between money and work leaves a recoverable record rather than a second charge or a blind rerun.

**Status (2026-09-17): DRAFT — not started.** Source-grounded against this worktree; every cited line is current as of writing.

**Not a new payments system.** No new signature scheme, no new settlement path, no new store engine. This slice composes `net-payments`' existing `PaymentEngine` / `CallerPaymentFlow` with the SDK's `TaskRegistry` through a new SDK-owned admission contract.

---

## 0. Baseline (what the code does today)

| Fact | Evidence |
|---|---|
| `Mesh::serve_a2a(registry, executor)` registers `net.a2a.task` / `net.a2a.status` / `net.a2a.cancel` on the context-bearing `serve_rpc` path (headers are already readable in handlers). | `sdk/src/mesh_a2a.rs:203-229` |
| `SubmitHandler::call` decodes the brief and immediately calls `TaskRegistry::submit`, which records `Accepted` **and** `tokio::spawn`s the executor under one lock scope. Every outcome is answered in-body as `TaskAck { accepted, reason }` with `RpcStatus::Ok`. | `mesh_a2a.rs:129-158`, `sdk/src/a2a.rs:349-424` |
| Idempotency is identical-brief-per-`(owner, task_id)`; a different brief under a reused id is `SubmitRejection::IdReusedForDifferentBrief` (the only variant). Two owners may share an id. | `a2a.rs:365-370`, `a2a.rs:294-309` |
| Owner = `TaskOwner::Peer(ctx.session_peer)`: the AEAD-authenticated **deliverer**, documented as *not* an end-to-end origin under relaying. `TaskOwner` is `Copy + Hash`, variants `Local`, `Peer(u64)`. | `mesh_a2a.rs:14-39,88-91`, `a2a.rs:279-290` |
| `TaskRegistry` is a `HashMap` behind `parking_lot::Mutex` — purely in-memory; terminal records evicted after `TERMINAL_RECORD_TTL_SECS = 3600`. | `a2a.rs:314-316,256` |
| `TaskState::Requested` exists in the wire enum but nothing produces it today. | `a2a.rs:37-56` |
| Task ids are always minted by `TaskBrief::new` (`random_id()`) in both bindings; the Python/Node caller cannot supply one. | `a2a.rs:99-102`, `bindings/python/src/a2a.rs:114`, `bindings/node/src/a2a.rs:271` |
| `RpcContext` carries `session_peer: u64`, `caller_origin: u64` (routing metadata, never authorize on it), `payload.headers: Vec<(String, Vec<u8>)>`, and `org_admission: Option<Admitted>` — the **only** verified end-to-end principal (`Admitted.caller: EntityId`), present only behind PROTECTED-service admission. | `src/adapter/net/cortex/rpc.rs:1494-1571`, `behavior/org_admission.rs:303-314` |
| Native paid tools: `Mesh::serve_tool_paid` wraps the handler in `PaidToolHandler`, reads `HDR_PAYMENT_QUOTE` / `HDR_PAYMENT_BINDING`, calls `ToolPaymentGate::redeem(tool_id, quote_id, binding)`, and refuses with `RpcStatus::Application(ERR_PAYMENT)` + `HDR_FAILURE_SCHEMATIC` returned as `Ok(payload)` (the `RpcHandlerError` channel flattens headers). `serve_tool` refuses a priced descriptor (`ServeError::UnenforceablePricing`); `serve_tool_paid` refuses an unpriced one (`MissingPricingTerms`). | `sdk/src/tool.rs:337-339,419-478,1385-1487`, `sdk/src/tool_payment.rs:47-91` |
| Engine redeem: `PaymentEngine::redeem_for_invocation(tool_id, quote_id, binding) -> RedeemDecision` checks binding-required → unknown quote → 64-byte sig verified by `rec.caller_hex` over `invocation_binding_transcript(quote_id, tool_id)` → frozen → settled/billed → tool binding (`rec.capability.split_once('/')` tail == `tool_id`) → `rec.redeemed` → flips `redeemed = true`. All inside one `mutate_json_if_changed` on `payment-engine.json` under the fs2 sidecar lock. | `payments/src/engine/mod.rs:2021-2143`, `policy/store.rs:252-263` |
| The binding transcript signs **only** `quote_id ‖ tool_id` (domain `net.payments.invocation_binding@1`). | `engine/mod.rs:276-285`, signed at `flow/mod.rs:886-895` |
| `PaymentQuote.input_hash: Option<String>` is the *designed* carrier for an input commitment ("quote-small-invoke-big fails verification when present") and participates in `terms_hash` → `quote_id`. Today `issue_quote` always passes `None`; `QuoteRequest` has no input-hash field; `QuoteRecord` does not persist it; nothing verifies it at redeem. | `core/quote.rs:36-74,131-168`, `engine/mod.rs:801-810`, `core/quote_request.rs:73-93`, `engine/mod.rs:300-343` |
| Caller side: `CallerPaymentFlow::run(capability, pricing_terms) -> CallerDecision::{Paid{quote_id, binding_sig, proof} \| RequiresPaymentApproval \| Denied \| Failed}`; resumes an approved held quote via `SpendPolicyEngine::approved_quote(capability)`. `ProviderChannel::quote` exists standalone (no pay) but no public quote-only verb exists on the flow or the Python gateway. `MeshPaymentChannel` resolves the provider node from `<node_id>/<capability>`. | `flow/mod.rs:599-947`, `flow/mesh.rs:295-309,322-386` |
| Python `PaymentProvider` owns the engine (`AdmitAll`, `serve_payments`, billing log) and exposes `publish_paid_tools`; `CapabilityGateway` exposes `invoke` (payment internal), `approve_payment`, `reject_payment`, `pending_payments`. `sdk-py` (`net_sdk`) has no A2A and no payments surface — A2A lives in `bindings/python` + `bindings/node` only. | `bindings/python/src/payment_provider.rs:417-483,613-668`, `capability_gateway.rs:976-1040` |
| Durable primitives already in the SDK crate without new deps: `PinStore::mutate` (JSON file, temp+fsync+rename, 0600/DACL, cross-process `.lock` via `fs2`) and `RedexFile` (re-exported under `cortex`). | `sdk/src/pins.rs:202-325`, `sdk/src/cortex.rs:70-73` |
| Tests: `sdk/tests/a2a_task_ownership.rs` (3 tests), `sdk/tests/tool_serve_paid.rs` (`RecordingGate` scripted gate idiom), `bindings/python/tests/test_a2a.py`, `bindings/node/test/a2a.test.ts`. SDK integration tests are auto-discovered by the `rust-sdk-tests` nextest job (not pinned by name). | `ci.yml:2170-2267` |

### The five gaps, mapped

| Requirement | Gap in today's code |
|---|---|
| 1. Free vs paid is an explicit provider choice | Only one serving path, always free; no service catalog; A2A has no pricing carrier and no discovery. |
| 2. Admission separated from launch | `submit` reserves + spawns atomically; no reservation, no await-for-concurrent-duplicate, no release. |
| 3. Payment bound to the work | Transcript binds `quote_id ‖ tool_id`; `input_hash` slot unused end-to-end. |
| 4. Caller + discovery path | No offer/describe; no quote-only verb; `submit_task` cannot carry headers or a caller-chosen id; payment refusals have no in-band shape. |
| 5. Ownership + durability | Deliverer-as-owner only; registry is in-memory; no launch claim; no paid-but-not-started state. |

---

## 1. Design decisions (locked)

### D1 — Provider configuration decides free vs paid; the caller never selects

- **`Mesh::serve_a2a(registry, executor)` is unchanged** — the backward-compatible free path. No payment dependency, no journal, no catalog, no new behavior.
- **New: `Mesh::serve_a2a_configured(registry, executor, A2aServiceConfig)`** — the strict, catalog-driven path:

```rust
pub struct A2aServiceConfig {
    /// service_id → policy. A brief must name a service in this map.
    pub services: BTreeMap<String, A2aServicePolicy>,
    /// Required iff any service is `Paid`. Refused at serve time otherwise.
    pub payment: Option<Arc<dyn TaskAdmissionGate>>,
    /// Who a submission is attributed to (see D5).
    pub principal: A2aPrincipal,
    /// Required iff any service is `Paid` (see D5).
    pub journal: Option<A2aAdmissionJournal>,
}
pub enum A2aServicePolicy { Free(A2aOffer), Paid(A2aOffer) }
pub struct A2aOffer {
    pub service_id: String,
    pub revision: String,
    pub description: Option<String>,
    /// `net.pricing.terms@1` canonical JSON; `Some` iff Paid.
    pub pricing_terms: Option<String>,
    pub bounds: A2aBounds,           // max_prompt_bytes, max_context_refs, max_tags, max_tag_bytes
    pub retention_secs: u64,         // published terminal-record retention (status/result reads)
}
```

- **Serve-time invariants (fail closed, mirror `serve_tool_paid`):**
  - `Paid(offer)` with `offer.pricing_terms.is_none()` → `ServeError::MissingPricingTerms(service_id)`.
  - `Free(offer)` with `pricing_terms.is_some()` → `ServeError::UnenforceablePricing(service_id)`.
  - any `Paid` and `payment.is_none()` or `journal.is_none()` → new `ServeError::A2aPaidMisconfigured(String)` (core `mesh_rpc.rs`, beside `MissingPricingTerms`). **A configured paid service never degrades to free.**
  - a catalog of only `Free` entries needs neither gate nor journal (strict naming, still zero payment deps).
- **The brief names the service:** `TaskBrief` gains `#[serde(default)] service: Option<String>` and `#[serde(default)] revision: Option<String>`. Legacy `serve_a2a` ignores both (free, unchanged). Configured mode rejects `None`/unknown service (`SubmitRejection::UnknownService`) and a revision that is not the catalog's current one (`SubmitRejection::StaleRevision`) **before** any payment step. Unknown JSON fields are already ignored by the derive (no `deny_unknown_fields`), so old free servers accept new briefs.
- One node serves one A2A handler set (`AlreadyServing`), so "both free and paid" on a node is expressed as two catalog entries, never two serving paths.

### D2 — Staged admission; launch is a separate, once-only step

New `TaskRegistry` API (the existing `submit` becomes `reserve` + `launch` and keeps its signature/behavior for the free path):

```rust
pub enum Admission {
    /// Same owner + id + identical brief already admitted (or terminal): hand back the id.
    Existing(String),
    /// Same owner + id + identical brief is mid-admission in another call: await its verdict.
    Pending(watch::Receiver<Option<Result<String, String>>>),
    /// Fresh reservation: the entry exists in `TaskState::Requested`, nothing is running.
    Reserved(AdmissionTicket),
}
impl TaskRegistry {
    pub fn reserve(&self, owner: TaskOwner, brief: TaskBrief) -> Result<Admission, SubmitRejection>;
}
impl AdmissionTicket {
    /// Spawn exactly once: `Requested → Accepted → Running → terminal`. Consumes the ticket.
    pub fn launch(self, executor: Arc<dyn TaskExecutor>) -> String;
    /// Withdraw the reservation (validation/payment refused). Consumes the ticket, removes the entry,
    /// and resolves `Pending` waiters with `Err(reason)`.
    pub fn release(self, reason: String);
}
// Drop of an unconsumed ticket == release("admission abandoned") — a panic or early return
// between reserve and launch can never strand a `Requested` entry.
```

Handler sequence for `net.a2a.task` in configured mode:

```
1. authenticate    owner = principal(ctx)                 (D5)
2. validate        service ∈ catalog, revision current, bounds, brief decodes
3. reserve         registry.reserve(owner, brief)
                     Existing(id)  → ack(id)                       [no payment touched]
                     Pending(rx)   → await rx → ack / refuse       [converge]
                     Reserved(t)   → continue
4. journal         Reserved{owner, task_id, service, revision, commitment, brief}
5. payment         Free → skip.  Paid → require HDR_PAYMENT_QUOTE + HDR_PAYMENT_BINDING (bearer refused),
                   gate.redeem(TaskPaymentClaim{ tool_id, quote_id, binding, commitment })
                     Err(denial) → t.release(); journal Released; reply ERR_PAYMENT + schematic
                     Ok(evidence) → journal Paid{quote_id, payer}
6. launch claim    journal Launched  (durable BEFORE spawn)
7. launch          t.launch(executor)  → ack(id)
```

Retry semantics this yields:

| Situation | Result |
|---|---|
| Same owner + id + identical brief, task exists (any state) | `Existing` → original id; no gate call, no second spawn. |
| Same owner + id + different brief | `IdReusedForDifferentBrief` at step 3, before any payment. |
| Two identical submissions racing | Second gets `Pending` and returns the first's verdict; one redeem, one launch. |
| Gate denies | Reservation released; an honest retry with a fresh/valid quote starts over at step 3. |
| Same quote presented for a *different* brief | Engine: `input_binding_mismatch` (D3); reservation released. |

**Why not "redeem before today's `submit`":** that consumes the quote before the id-conflict check (the D3 mismatch would then have already happened at the engine) and turns an identical retransmit into `already_redeemed`. Ordering reserve → redeem → launch is the whole point; the engine-side idempotency in D3 covers the crash window between redeem and the journal write.

### D3 — The purchase is bound to the exact work through the quote's `input_hash`

No new transcript, no new signature scheme. The commitment rides the slot the quote already has for it, so the **quote id itself commits to the work** (`quote_id = blake3(provider ‖ caller ‖ terms_hash ‖ issued_at)`, `terms_hash` covers `input_hash`), and the existing caller-signed binding over `quote_id ‖ tool_id` transitively proves the payer authorized *this* brief.

- **Commitment** (SDK, `sdk/src/a2a.rs`, feature `net`, `blake3` already a dep):
  `task_commitment(offer_hash, &brief) -> String` = blake3 hex over domain `net.a2a.commitment@1` ‖ len-prefixed fields: `offer_hash`, `service_id`, `revision`, `task_id`, `prompt`, each `context_refs[i]`, each `tags[i]`. `offer_hash` = blake3 over the canonical `A2aOffer` (service, revision, pricing terms, bounds, retention) — i.e. the agreed bounds and terms. Provider recomputes from its live catalog entry + the received brief; caller computes from the described offer + its brief. Any drift ⇒ mismatch.
- **tool_id / capability shape:** `tool_id = "net.a2a.task/{service_id}"`; capability on the quote = `"{node_id}/net.a2a.task/{service_id}"`. `MeshPaymentChannel::provider_node` splits on the first `/` (node id ✓); the engine's tool-binding check uses `split_once('/')` tail (`net.a2a.task/{service_id}` ✓). No engine change needed for the tool binding.
- **Engine changes (`net-payments`):**
  1. `QuoteRequest` gains `#[serde(default, skip_serializing_if = "Option::is_none")] input_hash: Option<String>` (covered by the canonical signed bytes; older callers omit it → `None`, byte-identical wire). `QuoteRequest::new` unchanged + `with_input_hash`.
  2. `serve_payments` quote handler threads `request.input_hash` → `ProviderChannel::quote(..., input_hash: Option<&str>)` → `InProcessProvider` → `PaymentEngine::issue_quote(caller, capability, template, input_hash)` → `PaymentQuote::new(.., input_hash, ..)`. (Trait param addition; impls: `MeshPaymentChannel`, `InProcessProvider`, test stubs.)
  3. `QuoteRecord` gains `#[serde(default)] input_hash: Option<String>` and `#[serde(default)] redeemed_for: Option<String>` (old `payment-engine.json` loads unchanged).
  4. New `PaymentEngine::redeem_for_task(tool_id, quote_id, binding: &[u8], expected_input_hash: &str) -> Result<RedeemDecision, EngineError>` sharing the private check closure with `redeem_for_invocation`, with three deltas: binding is mandatory (no `require_invocation_binding` opt-out for tasks); after the tool-binding check, `rec.input_hash != Some(expected)` → `RedeemDenialReason::InputBindingMismatch`; and `rec.redeemed` is **idempotent per commitment** — `rec.redeemed_for == Some(expected)` → `Admitted` (no dirty write), anything else → `AlreadyRedeemed`. Admission writes `redeemed = true, redeemed_for = Some(expected)`. `RedeemDecision::Admitted` gains `payer: EntityId` so the SDK can record the payer without parsing payment objects.
     *Why idempotent redemption is safe:* at-most-once **execution** is owned by the journal launch claim (D5), not by redemption. Redemption-per-commitment lets a provider that crashed between the engine write and the journal write reconcile the original payment on retry instead of failing "already redeemed" or charging again.
  5. `flow::denial_for` gains the `input_binding_mismatch` row: stage `redeem`, class `security_violation`, actor `caller_operator`, `retryable=false`, `safe_to_retry=false`, `safe_to_requote=false`, `funds_moved=unknown`, `prior_payment=unknown`, no `next_action` (same posture as `wrong_tool_binding`). The SDK's `FailureSchematic` doc table and `failure_vocab` gain the reason name (the vocabulary is SDK-owned — `payments/src/core/versioning.rs:37-42` — no envelope registry change).
- **SDK admission contract** (new ungated module `sdk/src/a2a_payment.rs`, the A2A twin of `tool_payment.rs`; free A2A never links `net-payments`):

```rust
pub struct TaskPaymentClaim<'a> {
    pub tool_id: &'a str,      // "net.a2a.task/{service_id}"
    pub quote_id: &'a str,
    pub binding: &'a [u8],     // mandatory: bearer mode is refused for tasks
    pub commitment: &'a str,   // task_commitment(...) hex
}
pub struct TaskPaymentEvidence { pub quote_id: String, pub payer: [u8; 32] }
#[async_trait]
pub trait TaskAdmissionGate: Send + Sync {
    async fn redeem(&self, claim: TaskPaymentClaim<'_>) -> Result<TaskPaymentEvidence, GateDenial>;
}
```
  `net-payments` (feature `mesh`) provides `EngineTaskAdmissionGate` → `flow::redeem_task_via_engine` → `redeem_for_task`, the single denial-render site (same shape as `EngineToolPaymentGate`).

### D4 — Discovery, quote-only prepare, submission with evidence, structured refusals

- **Discovery:** new uncharged service `A2A_DESCRIBE_SERVICE = "net.a2a.describe"` served by the configured path only, answering `Vec<A2aOffer>` (JSON). Requester: `Mesh::describe_a2a(node) -> Vec<A2aOffer>`. Provider selection stays exact-node (the requester already addresses A2A by node id); mesh-wide search of A2A offers is **out of scope** (Hermes plan G4 wants exact-provider purchase).
- **Caller flow** (`net-payments`, new `flow/a2a.rs`, feature `mesh`):
  - `CallerPaymentFlow::run_bound(capability, pricing_terms, input_hash: Option<&str>)`; `run` delegates with `None`. The approved-held-quote resume path must check the held quote's `input_hash == input_hash` (else re-quote) — an operator approval is for one exact commitment.
  - `A2aCallerFlow::quote_task(node, &offer, &brief) -> TaskQuote { quote_id, requirements, expires_at_ns }` — read-only: `ProviderChannel::quote` with the commitment, **no spend reservation, no payment**. Closes G4's "display a price without spending".
  - `A2aCallerFlow::prepare_task(node, &offer, &brief) -> A2aPurchase::{ Paid(TaskPaymentProof) | RequiresPaymentApproval{quote_id, policy_reason, approve_hint} | Denied{policy_reason} | Failed{message, retryable} }` where `TaskPaymentProof { task_id, quote_id, binding_sig: Vec<u8>, commitment, proof: Value }` — existing spend policy / operator approval / pending machinery unchanged (`approve_payment(quote_id)` then `prepare_task` again resumes the held quote).
- **Submission:** `Mesh::submit_task_paid(node, &brief, &TaskPaymentProof) -> Result<TaskAck, A2aFlowError>` attaches `HDR_PAYMENT_QUOTE` / `HDR_PAYMENT_BINDING` via `CallOptionsExt::with_request_header` and uses raw `Mesh::call` so reply headers are readable. `submit_task` (free) is unchanged.
- **Refusals on the wire:** payment refusals from the configured `SubmitHandler` are `RpcStatus::Application(ERR_PAYMENT)` + `HDR_FAILURE_SCHEMATIC` + human body — **byte-identical to `PaidToolHandler`** (`tool.rs:1411-1420`). Non-payment rejections stay in-body `TaskAck { accepted: false, reason }` (wire-compatible with today's clients). Requester maps the application error to `A2aFlowError::PaymentRefused { message, schematic: Option<FailureSchematic> }` (parsed with `FailureSchematic::from_header_bytes`, the `mesh_gateway.rs:470` idiom).
- **Uncharged verbs:** `status`, `cancel`, `describe` never touch the gate. Terminal records in configured mode are retained for `offer.retention_secs` (replaces the global `TERMINAL_RECORD_TTL_SECS` for that entry) and remain readable from the journal after eviction/restart.

### D5 — Ownership principal and durable admission

**Principal.**
```rust
pub enum A2aPrincipal {
    /// `TaskOwner::Peer(ctx.session_peer)`. Supported topology: DIRECT sessions only
    /// (documented restriction; a relay is the deliverer and would own what it forwards).
    SessionPeer,
    /// Serve the three services + describe as PROTECTED (`serve_rpc_owner_scoped` /
    /// `serve_rpc_granted` per `OrgAccess`), so `ctx.org_admission` is `Some`:
    /// `TaskOwner::Entity(admitted.caller)` — a verified end-to-end principal.
    OrgAdmitted(OrgAccess),
}
```
- `TaskOwner` gains `Entity([u8; 32])` (stays `Copy + Hash`; `TaskRecord` does not carry the owner, so the wire is unaffected).
- In **both** modes a paid admission additionally records `evidence.payer` (from the binding-verified quote) and the relationship is explicit in the journal: `SessionPeer` ⇒ *payer authorized this exact commitment; requester = delivering peer*; `OrgAdmitted` ⇒ **`payer == admitted.caller` is enforced** (mismatch → refuse with `binding_rejected` posture, reservation released). Under `SessionPeer`, the binding signature is still end-to-end proof that the payer authorized this exact work — a relay cannot substitute the brief — the documented limit is only that the relay, not the buyer, owns status/cancel.
- Requester side for `OrgAdmitted`: the existing `CallOptions::org_proof_intent` (`mesh_rpc.rs:203`) / `Mesh::org(..)` path supplies the proof; nothing new.

**Durability — `A2aAdmissionJournal`** (`sdk/src/a2a_journal.rs`, feature `net`; the `PinStore::mutate` idiom: one JSON file, temp+fsync+rename, owner-only perms, `fs2` sidecar `.lock`; no new deps):

```rust
pub struct AdmissionRecord {
    pub owner: TaskOwner, pub task_id: String, pub service_id: String, pub revision: String,
    pub commitment: String, pub brief: TaskBrief,
    pub state: AdmissionState, pub updated_at: u64, pub epoch: u64,   // epoch = serve() start marker
}
pub enum AdmissionState {
    Reserved,
    Paid     { quote_id: String, payer: [u8; 32] },
    Launched { quote_id: Option<String>, payer: Option<[u8; 32]> },
    Terminal { quote_id: Option<String>, payer: Option<[u8; 32]>, state: TaskState },
    Released { reason: String },
}
```
Write points: step 4 (Reserved), step 5 success (Paid) / failure (Released), step 6 (Launched), and from the registry's final `set_state` (Terminal) via a `TaskRegistry::with_terminal_hook` (configured mode only). Records in `Terminal`/`Released` are pruned after `retention_secs`; `Paid`/`Launched` are **never pruned automatically** (financial ambiguity is an operator's to resolve).

**Restart recovery** (on `serve_a2a_configured` start, and lazily on status/submit for a record whose `epoch` ≠ current):

| Journal state on restart | Status reply | On identical retry | Money |
|---|---|---|---|
| `Reserved` (crash between reserve and redeem) | unknown (`null`) | re-run admission from step 3; `redeem_for_task` is idempotent per commitment, so a redeem that *did* land before the crash reconciles instead of `already_redeemed` | at most one charge |
| `Paid` (crash between redeem and launch claim) | new `TaskState::Interrupted { detail: "paid_not_started" }` | resume at step 6 → launch once (work provably never started) | reconciled, not repurchased |
| `Launched` (crash after claim, executor may have run) | `Interrupted { detail: "outcome_unknown" }` | return the existing record; **never relaunch** | evidence retained |
| `Terminal` | the recorded terminal state (until retention expires) | `Existing` | — |

`TaskState::Interrupted { detail: String }` is a new terminal variant produced **only** by the configured path. `cancel` on it returns `false`. Rust requesters on older builds fail to decode it (serde tag); Python/Node return the JSON string untouched. Documented as the one wire addition of this slice.

**Crash between settlement and task acceptance is not atomic and is not made to look so.** The payer's evidence is the billing event (settled at pay time); the provider's evidence is the journal; the schematic vocabulary (`funds_moved`, `prior_payment`) already expresses "unknown".

---

## 2. Workstreams

Ordered by dependency; A and B are independent and can run in parallel, C depends on A+B, D–F on C.

### WS-A — Registry: reserve / launch / release, `Requested` state, `Interrupted` (SDK `a2a.rs`)

- [ ] `Admission`, `AdmissionTicket` (Drop = release), `TaskRegistry::reserve`, `AdmissionTicket::launch` (the current spawn body verbatim, incl. `PanicGuard` and the biased `select!`), `AdmissionTicket::release` (resolves `Pending` waiters).
- [ ] `submit` = `reserve` then `launch` for `Reserved`; `Existing` → id; `Pending` → *sync API cannot await*: keep `submit` returning the id after waiting is impossible, so `submit` treats `Pending` as `Ok(id)` (the entry exists; today's callers only ever race identical retransmits) — document it. Free wire behavior is otherwise byte-identical.
- [ ] `TaskState::Interrupted { detail }` (`is_terminal = true`, `label = "interrupted"`); `TaskOwner::Entity([u8; 32])`; `SubmitRejection::{UnknownService, StaleRevision { expected, got }, BoundsExceeded { field, limit }}`; `TaskBrief::{service, revision}` + `with_service(service, revision)`; `TaskBrief::with_task_id` (caller-retained ids).
- [ ] `task_commitment(offer_hash, &brief)` + `A2aOffer::hash()` in `a2a.rs` (blake3, length-prefixed, domain-separated exactly like `invocation_binding_transcript`).
- [ ] Per-entry retention override (`retention_secs`) used by the eviction pass; global TTL remains the default.
- [ ] Unit tests (`a2a.rs mod tests`): `reserve_then_release_leaves_no_entry`, `a_dropped_ticket_releases_the_reservation`, `concurrent_identical_reserves_share_one_verdict`, `a_ticket_launches_exactly_once`, `interrupted_is_terminal_and_uncancellable`, `commitment_changes_with_every_bound_field` (offer, service, revision, id, prompt, refs, tags — one flip each), `commitment_is_stable_across_encodings`.

**Acceptance:** `duplicate_submit_is_idempotent`, `cancel_stops_a_running_task`, and `sdk/tests/a2a_task_ownership.rs` pass unchanged; `submit` callers (`mesh_a2a.rs`, both bindings) compile without edits.

### WS-B — Engine: commitment-bound quotes and task redemption (`net-payments`)

- [ ] `QuoteRequest.input_hash` (+ `with_input_hash`), threaded through `serve_payments` → `ProviderChannel::quote(.., input_hash)` → `InProcessProvider` → `issue_quote(.., input_hash)` → `PaymentQuote::new`. Existing tests pass `None`.
- [ ] `QuoteRecord.{input_hash, redeemed_for}` with `#[serde(default)]`; a fixture test loads a pre-change `payment-engine.json` byte-for-byte.
- [ ] `PaymentEngine::redeem_for_task` + `RedeemDenialReason::InputBindingMismatch` (`wire_reason = "input_binding_mismatch"`) + `RedeemDecision::Admitted { payer }`; private shared closure so the two entry points cannot drift (the N-4 lesson from `PAYMENTS_P2_GAP_PLAN.md`).
- [ ] `flow::denial_for` row; `sdk/src/tool_payment.rs` doc table + `failure_vocab::REASON_INPUT_BINDING_MISMATCH`.
- [ ] `CallerPaymentFlow::run_bound`; approved-quote resume checks `input_hash`.
- [ ] `flow/a2a.rs` (feature `mesh`): `A2aCallerFlow { quote_task, prepare_task }`, `TaskPaymentProof`, `EngineTaskAdmissionGate: net_sdk::a2a_payment::TaskAdmissionGate`, `redeem_task_via_engine`.
- [ ] Tests (`payments/tests/a2a_task_redeem.rs`, mock facilitator): `a_quote_carries_the_commitment_into_its_id` (two commitments ⇒ two quote ids from the same template), `redeem_for_task_refuses_a_mismatched_commitment` (funds unchanged, `redeemed` still false), `redeem_for_task_is_idempotent_for_the_same_commitment`, `a_second_commitment_on_a_redeemed_quote_is_already_redeemed`, `a_bearer_task_redeem_is_binding_required_even_when_the_engine_allows_bearer_tools`, `an_approved_quote_resumes_only_for_its_own_commitment`, `quote_task_moves_no_money_and_reserves_no_spend`.

**Acceptance:** `redeem_for_invocation` behavior, `tool_serve_paid.rs`, `native_tool_gate.rs`, `mcp_gate_composition.rs`, and the admission-matrix bench compile and pass with no semantic change; `cargo clippy -p net-payments --features mesh -- -D warnings` clean.

### WS-C — Configured serving path (SDK `mesh_a2a.rs`, `a2a_payment.rs`, `a2a_journal.rs`)

- [ ] `a2a_payment.rs` (ungated): `TaskPaymentClaim`, `TaskPaymentEvidence`, `TaskAdmissionGate`; `A2A_DESCRIBE_SERVICE`.
- [ ] `A2aServiceConfig`, `A2aServicePolicy`, `A2aOffer`, `A2aBounds`, `A2aPrincipal`; `ServeError::A2aPaidMisconfigured` in core.
- [ ] `A2aAdmissionJournal` (`open(path)`, `record`, `get`, `recover(epoch)`, `prune(retention)`), `PinStore` idiom; `TaskRegistry::with_terminal_hook`.
- [ ] `Mesh::serve_a2a_configured` — validation per D1; registers `SubmitHandler` (configured variant running the D2 sequence), `StatusHandler` (registry → journal fallback → `null`), `CancelHandler`, `DescribeHandler`; for `OrgAdmitted` registers through `serve_rpc_owner_scoped` / `serve_rpc_granted` and reads `ctx.org_admission`.
- [ ] Requester: `Mesh::describe_a2a`, `Mesh::submit_task_paid`, `A2aFlowError::PaymentRefused { message, schematic }`.
- [ ] Startup recovery per D5 table (marks `Interrupted`, never launches).
- [ ] Integration tests `sdk/tests/a2a_paid_admission.rs` (`#![cfg(all(feature = "net", feature = "cortex"))]`, scripted `RecordingTaskGate` in the `RecordingGate` idiom, executor with a run counter and a barrier):
  - `a_free_configured_service_runs_without_a_gate_or_journal`
  - `a_paid_service_refuses_to_start_without_a_gate` / `..._without_a_journal` / `..._without_pricing` / `a_free_service_refuses_pricing`
  - `an_unpaid_submit_to_a_paid_service_is_refused_before_the_executor` (ERR_PAYMENT, reason `missing_quote`, stage `admission`, run counter 0, journal has `Released`)
  - `a_bearer_submit_is_refused_binding_required`
  - `a_paid_submit_redeems_once_and_runs_once`
  - `an_identical_retry_returns_the_original_task_without_a_second_redeem` (gate call count stays 1)
  - `concurrent_identical_submits_converge_on_one_admission` (gate blocked on a barrier while N submits race; 1 redeem, 1 run, N identical acks)
  - `an_altered_brief_under_the_same_id_is_rejected_before_payment` (gate never called)
  - `a_stale_revision_is_rejected_before_payment`
  - `a_gate_denial_releases_the_reservation_and_a_fresh_quote_retries`
  - `an_unknown_service_or_oversized_brief_is_rejected_in_body`
  - `another_peer_cannot_read_or_cancel_a_paid_task` (three-node, mirrors `a2a_task_ownership.rs`)
  - `describe_a2a_lists_offers_with_pricing_and_bounds`
  - `a_restart_after_payment_before_launch_surfaces_paid_not_started_and_resumes_once` (serve, journal pre-seeded `Paid`, new registry: status = `Interrupted{paid_not_started}`; identical retry launches once, gate idempotent)
  - `a_restart_after_launch_surfaces_outcome_unknown_and_never_reruns`
  - `status_and_cancel_are_uncharged` (gate call count unchanged across polls/cancel)

**Acceptance:** every test above green under the `rust-sdk-tests` feature set; legacy `mesh_a2a.rs` tests and both existing A2A binding suites unchanged and green.

### WS-D — End-to-end with the real engine (`net-payments`, feature `mesh`)

- [ ] `payments/tests/a2a_paid_end_to_end.rs`: two `Mesh` nodes, provider = `PaymentEngine` + mock facilitator + `EngineTaskAdmissionGate` + journal in a tempdir; requester = `A2aCallerFlow` over `MeshPaymentChannel` + `SpendPolicyEngine`. Tests: `quote_then_prepare_then_submit_runs_the_task_once`, `a_pending_approval_resumes_after_approve_payment_for_the_same_brief_only`, `a_lost_submit_reply_is_reconciled_by_resubmitting_the_same_proof` (drop the first reply; second submit returns the original id; billing log has one event), `a_relaunch_after_provider_restart_reconciles_the_original_payment` (provider process boundary simulated by dropping `ServeHandle`s + registry and re-serving over the same `payment-engine.json` + journal).

### WS-E — Python bindings (`bindings/python`) and Node parity where cheap

Python (`hermes-net` is Python; these are the surfaces its plan Tasks 6/7/9 need):

- [ ] `NetMesh.submit_task(target_node_id, prompt, context_refs=[], tags=[], *, task_id=None, service=None, revision=None)` — caller-retained id; `None` → random. `serve_a2a(callback)` unchanged.
- [ ] `NetMesh.describe_a2a(target_node_id) -> str` (JSON `A2aOffer[]`).
- [ ] `PaymentProvider.serve_a2a_configured(callback, services: dict[str, dict], journal_path: str, principal: str = "session_peer") -> A2aServeHandle` — builds `A2aServiceConfig` with `EngineTaskAdmissionGate::new(self.engine)`; `services[id] = {"revision", "pricing_terms" | None, "bounds": {...}, "retention_secs", "description"}`. Executor callback gains `service` and `revision` keyword args (positional contract `(task_id, prompt, context_refs, tags)` preserved).
- [ ] `CapabilityGateway.quote_task(target_node_id, service, prompt, context_refs, tags, task_id) -> str` (read-only; `{status: ok, quote_id, amount, network, asset, expires_at_ns, task_id, commitment}`) and `prepare_task(...) -> str` (`{status: paid | requires_payment_approval | denied | failed, task_id, proof}`); `NetMesh.submit_task_paid(target_node_id, proof_json) -> str` (raises `PaymentRefused` carrying `schematic` JSON). Approvals reuse `approve_payment` / `reject_payment` / `pending_payments`.
- [ ] `_net.pyi` + `python/net/__init__.py` exports; `bindings/python/tests/test_a2a_paid.py` mirroring the WS-C matrix over the mock facilitator (free success, unpaid refusal with schematic, paid runs once, retained-id identical retry, altered brief, other-peer denied, restart ambiguity with a re-created provider over the same paths).
- [ ] Node: `submitTask` gains an optional `taskId` (parity for retained ids; two-line change). Paid serving on Node is **deferred** (no consumer; free behavior unchanged).

### WS-F — Docs, CI, release

- [ ] `sdk/src/mesh_a2a.rs` / `a2a.rs` module docs (ownership section gains the principal table; wire addition `Interrupted`); `tool_payment.rs` header names the A2A twin.
- [ ] `web/src/content/docs/guides/agent-to-agent.md` (surface table + "paid services" section), `docs/data/capabilities/event-bus.yaml` A2A matrix, release note under `net/crates/net/docs/releases/` mirrored via `npm run sync:releases`, `net/crates/net/docs/SECURITY_DEFAULTS_0.35.md` §8 relay caveat cross-references `A2aPrincipal`.
- [ ] `.claude/skills/net-event-bus` A2A section: paid prepare/submit example (Python) — CI executes skill examples, so this is also a witness.
- [ ] CI: SDK tests are auto-discovered (`ci.yml:2267`); confirm the `net-payments` job's feature list includes `mesh` for the new test files (add `--features mesh` if the job pins features); `test_a2a_paid.py` auto-discovers under the existing maturin feature list (`ci.yml:2851-2863` already has `a2a` + payments). No new `tests/*.rs` in the core crate ⇒ no pin-guard change.
- [ ] Pre-push checklist from AGENTS.md incl. `cargo doc` with `-D warnings` for `net-payments`, `sdk`, `bindings/python`; `rustdoc::private_intra_doc_links` on the new pub items.

---

## 3. Public surface summary

**Rust (SDK):**
- unchanged: `Mesh::serve_a2a`, `submit_task`, `task_status`, `cancel_task`, `TaskRegistry::submit`.
- new: `Mesh::serve_a2a_configured`, `Mesh::describe_a2a`, `Mesh::submit_task_paid`; `A2aServiceConfig`, `A2aServicePolicy`, `A2aOffer`, `A2aBounds`, `A2aPrincipal`, `A2aAdmissionJournal`; `TaskRegistry::{reserve, with_terminal_hook}`, `Admission`, `AdmissionTicket`; `TaskBrief::{service, revision, with_service, with_task_id}`; `TaskOwner::Entity`; `TaskState::Interrupted`; `SubmitRejection::{UnknownService, StaleRevision, BoundsExceeded}`; `A2aFlowError::PaymentRefused`; `a2a_payment::{TaskAdmissionGate, TaskPaymentClaim, TaskPaymentEvidence, A2A_DESCRIBE_SERVICE}`; `task_commitment`.
- core: `ServeError::A2aPaidMisconfigured`.

**Rust (`net-payments`, feature `mesh` unless noted):**
- `QuoteRequest::with_input_hash` (ungated); `ProviderChannel::quote(.., input_hash)`; `PaymentEngine::{issue_quote(.., input_hash), redeem_for_task}`; `RedeemDecision::Admitted { payer }`; `RedeemDenialReason::InputBindingMismatch`; `CallerPaymentFlow::run_bound`; `flow::a2a::{A2aCallerFlow, TaskQuote, A2aPurchase, TaskPaymentProof, EngineTaskAdmissionGate}`.

**Python:** `NetMesh.submit_task(..., task_id=, service=, revision=)`, `NetMesh.describe_a2a`, `NetMesh.submit_task_paid`, `PaymentProvider.serve_a2a_configured`, `CapabilityGateway.{quote_task, prepare_task}`.

**Wire:** `net.a2a.describe` (new); `net.a2a.task` additionally accepts `net-payment-quote` / `net-payment-quote-sig` request headers and may answer `Application(0x8006)` + `net-failure-schematic`; `TaskBrief.{service, revision}` (optional); `TaskState::interrupted` (configured path only); `QuoteRequest.input_hash` (optional, signed).

---

## 4. Acceptance matrix (the wire tests the slice must prove)

| Invariant | Witness |
|---|---|
| Free success unchanged | legacy `mesh_a2a.rs` tests, `a2a_task_ownership.rs`, `test_a2a.py`, `a2a.test.ts` — no edits |
| Intentionally free needs no payments | `a_free_configured_service_runs_without_a_gate_or_journal`; `net_sdk` builds `a2a_payment` without `net-payments` |
| Paid never becomes free | `a_paid_service_refuses_to_start_without_a_gate` and the pricing/journal siblings |
| Unpaid refusal, structured | `an_unpaid_submit_to_a_paid_service_is_refused_before_the_executor` (+ Python schematic passthrough) |
| Paid execution once | `a_paid_submit_redeems_once_and_runs_once`, `quote_then_prepare_then_submit_runs_the_task_once` |
| Identical retry | `an_identical_retry_returns_the_original_task_without_a_second_redeem`, `a_lost_submit_reply_is_reconciled_by_resubmitting_the_same_proof` |
| Altered request | `an_altered_brief_under_the_same_id_is_rejected_before_payment`, `redeem_for_task_refuses_a_mismatched_commitment` |
| Concurrent convergence | `concurrent_identical_submits_converge_on_one_admission` |
| Owner-only control | `another_peer_cannot_read_or_cancel_a_paid_task`, `status_and_cancel_are_uncharged` |
| Restart ambiguity | the two `a_restart_after_*` tests, `a_relaunch_after_provider_restart_reconciles_the_original_payment` |
| Quote is read-only | `quote_task_moves_no_money_and_reserves_no_spend` |
| Approval is per-commitment | `an_approved_quote_resumes_only_for_its_own_commitment` |

Behavioral witnesses only — no source-string assertions, no sleeps as mutual-exclusion evidence (barriers on the scripted gate/executor).

---

## 5. Hermes gate closure

| Gate | What this slice provides | Hermes still owns |
|---|---|---|
| G2 | `A2aPrincipal` (direct-session restriction or org-admitted `EntityId`), payer recorded on every paid admission, `task_commitment` over service/revision/brief/bounds/terms, provider-side verification before redeem/start | its journal's mapping of local request ids to `task_id`; choosing the topology |
| G3 | validation and reservation strictly before `redeem`; capacity/bounds refusals are unpaid `TaskAck` rejections; post-payment failures are `Interrupted`/journal states, never "unpaid validation failure" | service schema validation beyond `A2aBounds` (the executor callback validates the prompt/payload it accepts) |
| G4 | `quote_task` (read-only), `prepare_task` (exact node, exact commitment), `submit_task_paid` (evidence, no re-quote) | freezing intent before quoting; never switching provider after a quote |
| G5 | unchanged (rail qualification is outside this slice) | — |

---

## 6. Risks and non-goals

- **`ProviderChannel::quote` trait change** touches every impl and test stub in `net-payments`; kept additive on the wire (`input_hash` optional in the signed request) so old callers keep working.
- **Revision retirement vs. in-flight quotes:** a provider that changes `revision` after a buyer paid strands that quote (`StaleRevision`, funds moved). Mitigation is operational and documented: keep the previous revision in the catalog for at least the quote TTL, or accept the refund obligation. No automatic refund path is built.
- **`TaskState::Interrupted` decode on old Rust requesters** — only emitted by the configured path; called out in the release note.
- **`submit` + `Pending`** in the sync free path returns the id without awaiting the verdict (today's callers only race identical retransmits; the entry already exists). Configured mode always awaits.
- **Idempotent redemption widens `redeem_for_task` only.** `redeem_for_invocation` keeps strict at-most-once; the two share one closure so the difference is one visible branch.
- **Non-goals:** metering/escrow/refunds; resumable execution across restart; mesh-wide A2A offer search; Node paid serving; relay-transparent ownership under `SessionPeer`; a new storage engine (the JSON+lock store is deliberate — `PAYMENTS_STORAGE_DISPOSITION.md` governs any replacement).
