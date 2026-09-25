# Net v0.37 — "Money Talks"

*Named after AC/DC's 1990 single, whose whole argument is that the transaction says what the talk does not.*

**Status: drafted as the slice lands.** The version bump has not happened yet; this note is assembled per `RELEASE_STEPS.md` step 4 and describes what is in the tree.

One track lands: **an A2A task can be sold.** A native agent-to-agent service is now *explicitly* free or paid by provider configuration, and a paid task is prepared before any money moves, purchased against that exact reservation, submitted with the evidence, and launched once under a durable claim.

Nothing about the free path changes. `serve_a2a`, `submit_task`, `task_status` and `cancel_task` are byte-identical on the wire, and a deployment that configures no catalog cannot tell this release from the last one.

The organizing observation continues the series. v0.34 was *a boundary layered over an identity that never travels*; v0.35 was *a claim layered over evidence nobody took*; v0.36 was *an authority layered over an admission that never meant one*. v0.37 is **a charge layered over work nobody had yet agreed to do.** The ordering is the entire design: redemption before validation consumes a quote for work that was always going to be refused, and a launch before a durable claim buys a second run out of a crash. So the sequence is fixed — **prepare → purchase → submit → claim → launch** — and every refusal that can be reached without spending money is reached before a quote exists.

---

## Free or paid is the provider's choice, and paid never degrades

`Mesh::serve_a2a_configured(registry, executor, config)` is the strict sibling of `serve_a2a`. Its catalog maps a service id to `A2aServicePolicy::Free(offer)` or `A2aServicePolicy::Paid(offer)`, and a brief must name a service in it. Three serve-time refusals keep the "paid" label honest, all fail-closed and all mirroring what `serve_tool_paid` already did for tools:

| Misconfiguration | Refusal |
|---|---|
| `Paid` with no `net.pricing.terms@1` document | `ServeError::MissingPricingTerms` |
| `Free` carrying pricing terms | `ServeError::UnenforceablePricing` |
| any `Paid` entry without a payment gate, or without an admission journal | `ServeError::A2aPaidMisconfigured` |

A configured paid service therefore never starts in a state where it would serve for free. A catalog of only `Free` entries needs neither a gate nor a journal and links no payment code at all — but free means *no payment*, not *no policy*: free services still run the application `TaskPreflight` and still enforce `max_in_flight`, at prepare **and** on a direct submit that never prepared.

Two uncharged services are added beside the three existing verbs: `net.a2a.describe` publishes the offers (service id, revision, bounds, pricing terms, retention windows) and `net.a2a.prepare` validates a brief, runs the preflight, reserves capacity and mints the provider-side `admission_id` that a purchase binds to. `describe`, `prepare`, `status` and `cancel` never touch the payment gate.

## The purchase is bound to one reservation of exactly one brief

No new signature scheme and no new transcript. The quote's existing `input_hash` carries `purchase_hash(admission_id, commitment)`, where the commitment is a canonical typed JSON hash over the offer, the service, the revision, the task id, the prompt, the context refs and the tags. `input_hash` already participates in `terms_hash` and therefore in `quote_id`, so the caller's existing binding signature over `quote_id ‖ tool_id` transitively proves the payer authorized *this purchase of this work under this reservation*.

The consequence is the one that matters: the same proof replayed under another owner or another reservation computes a different expected hash at the provider and is refused with `input_binding_mismatch` — before the idempotency arm, before anything runs. `PaymentEngine::redeem_for_task` is the new verb; the binding is mandatory there (bearer presentation is never enough for a long-running side effect) and redemption is idempotent **per purchase hash**, which is what lets a provider that crashed between the engine write and its own journal write reconcile on retry instead of charging twice.

## The caller keeps a durable attempt, and resumes it rather than re-quoting

`A2aCallerFlow` splits the old monolithic `run` into `prepare_task` → `purchase_task` → `submit_task` over a durable `a2a-purchases.json` store, with exactly one authoritative attempt per `(caller, provider node, task id)` and every transition a CAS under the store lock.

- `prepare_task` moves no money and reserves no spend. It is what lets a caller *display a price* without spending anything.
- `purchase_task` consumes the stored quote and the stored payload — never fresh ones. Concurrent callers converge on one payload and one billing event; a lost pay reply is resumed by re-sending the byte-identical payload, and the engine answers with the original verdict.
- A refusal is classified by whether an authorization ever left the process. `RefusedUnexposed` is proven non-payment and may be prepared again; `RefusedExposed` is **not** proof of non-settlement for a bearer scheme, keeps its spend reservation, and surfaces as `denied { funds_ambiguous: true }` with an operator resolution as the only exit. That distinction is the one the existing `a_solana_reject_keeps_the_reservation` witness has always made, now stated in the attempt's own state rather than inferred.

`CallerPaymentFlow::run` is composed from the new staged verbs (`quote_bound`, `reserve_spend`, `author`, `pay_exact`) and is behaviorally unchanged; the existing MCP-gate, HTTP-402 and spend-policy suites are the guard.

## Nothing runs twice, and nothing runs unpaid

The provider's durable admission record is two tables in one file with different lifetimes: the **result** table ages out under retention, and the **launch ledger** is never pruned automatically. The launch claim writes `Launched` and its ledger entry in one atomic replace *before* the executor is spawned, so a claim-write failure answers a retryable `journal_unavailable` having started nothing, and a retry after a result was retired answers `Retired` — the redeem step is never reached once the ledger knows the task.

The journal takes an fs2 exclusive lock on a stable `<path>.owner` sidecar at `open` and holds it for the journal's whole lifetime, handed to every serve handle, handler and spawned executor future. A second owner — in this process or another — is refused with `A2aJournalError::OwnedElsewhere`, so a misconfigured pair of writers over one set of admissions fails at serve time instead of interleaving.

## What this release asks operators to know

Four facts are load-bearing enough to belong in the note rather than only in rustdoc.

- **The unresolved-financial queue accumulates until an operator resolves it. By design.** Records in the three unresolved classes — `Paid` but never launched, `Launched` with no recorded outcome, and `Reconcile` (an admission revoked after payment) — are **never pruned automatically**, by `prune`, by `forget`, or by result retention. Money moved, or may have, and a store that quietly discarded that is worse than one that grows. `A2aAdmissionJournal::unresolved()` (Rust) and `PaymentProvider.a2a_unresolved()` (Python) are the operator's queue, and `resolve(owner, task_id, state)` / `a2a_resolve(...)` is the only exit — after which the ordinary resolved-result retention applies. On the caller side the same shape holds: `unknown`, `denied { funds_ambiguous: true }` and `unexecutable` attempts are never auto-pruned, and `a2a_resolve_attempt` is their exit. If nobody drains these queues, they are unbounded.
- **The journal lock is a local-filesystem contract only.** Advisory locking over NFS or SMB is not dependable — the same caveat `payment-engine.json` already carries — so a journal on a network share is not protected by the ownership guarantee, and two writers there would not be refused. The sidecar is part of the store: back it up with the journal and delete it with the journal. An operator who removes `<path>.owner` by hand defeats the exclusion, exactly as deleting a lock file by hand always has.
- **A late paid submit is answered `no_reservation`, and the caller's own attempt is the reconciliation evidence.** An unpaid-as-far-as-the-provider-knows reservation is kept for `reservation_retention_secs`. That term is **explicit configuration with no implemented default** — every offer states it, and the 7-day figure in this release's examples is a recommendation, not a fallback. It is committed in the signed offer and should be orders of magnitude beyond any quote TTL. Note also that retention is an **age-out policy applied when `prune` is called**, not an automatic sweep: nothing in the provider or the caller runs it for you. A caller that paid and then submitted *after* that window gets `no_reservation` — whose money facts are `funds_moved: unknown` / `prior_payment: unknown`, because a provider holding no record cannot claim that no money moved. The caller's `PurchaseAttempt` moves `Paid → PaidUnexecutable { proof, billing, refusal }`, retains its evidence, and is what reconciliation is performed from. No automatic refund is built.
- **`TaskState::Interrupted` does not decode on an older requester — in any language.** It is the one status-wire addition of this slice: a terminal state naming *which* ambiguity a restart left behind (`paid_not_started`, `outcome_unknown`, `admission_revoked`), produced **only** by the configured path. `TaskState` is a serde-tagged enum, so a Rust requester built before this release reads a status reply carrying it as `A2aFlowError::Decode` rather than as a record.

  **Correction to an earlier draft of this note:** the Python and Node bindings are *not* transparently forward-compatible. They return status as JSON, but the decode happens in the embedded Rust `TaskState` before the JSON is produced, so a **previously built** wheel or addon fails the same way a previously built Rust requester does. Passing the tag through requires a binding built from this release or later. The claim of transparent passthrough was untested; treating a native binding as a thin JSON shim is the mistake behind it. A deployment that serves only `serve_a2a` never emits one.

## What a restart surfaces

| Record found | `status` answers | An identical retry does | Money |
|---|---|---|---|
| `Reserved`, unexpired | `null` | redeem (idempotent) → claim → launch once | at most one charge |
| `Reserved`, expired | `null` | re-acquire capacity, or a retryable `Busy` with the gate never called | unchanged |
| `Paid`, never launched | `interrupted { paid_not_started }` | resumes on the **recorded** payment, no second redeem, launches once | reconciled, not repurchased |
| `Launched`, no outcome | `interrupted { outcome_unknown }` | acknowledges the task and never relaunches | evidence retained; operator resolves |
| `Reconcile` | `interrupted { admission_revoked }` | the same `admission_revoked` refusal, no state change | operator resolves |
| `Terminal` | as recorded, until retention | acknowledges the task | — |
| ledger only | `null` | free: in-body `Retired`; paid: the structured `retired` refusal → `PaidUnexecutable` | evidence retained; operator resolves |

Settlement and acceptance are not atomic, and this release does not pretend they are. The payer's evidence is the billing event plus its own attempt; the provider's evidence is the journal; and the failure schematic vocabulary already had the words for it (`funds_moved: unknown`).

## Refusal shapes

Payment and admission refusals are the `ERR_PAYMENT` application error with one `net-failure-schematic` reply header and a human body — byte-identical in shape to a paid tool's refusal. The reasons this slice adds: `missing_quote`, `binding_required`, `binding_rejected`, `no_reservation`, `admission_revoked`, `journal_unavailable` (the one retryable row), `input_binding_mismatch` and `retired`. Everything non-financial stays an in-body `TaskAck { accepted: false }` — unknown service, stale revision, bounds exceeded, `Busy`.

`Retired` is deliberately on **both** sides of that line, and it is the one case where the free and paid wires differ. A free submitter meeting a retired task gets the in-body ack it always got: nothing financial happened, so there is nothing to retain and nobody to escalate to. A *paid* submitter holds a proof for work the launch ledger will never run again, and prose cannot carry that — it read as retryable and stranded the charge as `Paid` for ever with no operator exit. So the paid path answers the structured `retired` reason instead, terminal in both directions (`safe_to_retry` and `safe_to_requote` both false, `funds_moved` / `prior_payment` `unknown` — the ledger proves the work ran, not what became of this payment), and the caller's attempt moves to `PaidUnexecutable` with its evidence kept.

---

## Org-scoped protected streaming — known limitations

Three known limitations ride in with the org-scoped protected streaming track. All three are deliberate and owner-bound — filed, not fixed, in this release — and all three fail silently: the failure mode is a success return.

**`MeshNode::start()` can return success while starting nothing.** `start()` is idempotent, but it must not be called while an `accept()` is in flight. If it is, the start is refused: the started flag is rolled back and the receive/dispatch loop is never spawned. The only trace of the refusal is a `tracing::warn!` (`"MeshNode::start() called while an accept() is in flight — … Retry start() after accept() returns."`); `start()` itself returns `()`, so every caller reads the call as a success. A node in that state processes **zero inbound packets** while its sends work perfectly — one-way invisibility that looks healthy from the inside and answers nothing on the mesh. The refusal is retryable: once the `accept()` returns, a fresh `start()` proceeds normally. This is filed as an owner-bound known limitation (compatibility-ledger territory), with two alternatives on the table for the owner: **breaking** — `start()` returns `Result<(), …>` and the bindings raise on the refusal (the Python `NetMesh.start()` → `PyResult<()>` already has the error channel), the cleanest semantics, since "start" must never silently no-op, but it changes the public signature in every language binding and the compatibility ledger must carry it; or **additive** (recommended for this cycle) — keep `start()`'s signature and idempotent semantics, add `try_start()` / `start_with_report()` beside it that return the refusal, and carry the silent-refusal caveat on `start()` as a known limitation until the next major. Zero breakage; harnesses that need liveness guarantees adopt the additive door immediately. Until one is chosen, treat a successful `start()` as "may have started nothing" and verify inbound liveness before relying on it.

**The Python binding's `NetMesh.start()` returns `Ok(())` while starting nothing.** The same refusal, one seam further out: the pyo3 binding calls `MeshNode::start()` and returns `Ok(())` unconditionally, so a Python caller has no channel on which to observe the refusal at all — the process logs the `tracing::warn!`, the node sits dark on inbound exactly as above, and `start()` has reported success. It is one face of the same ruling and is resolved by the same decision: the breaking alternative raises from `NetMesh.start()`; the additive one gives it the reporting door.

**Cross-org revocation floors are "absent means zero".** For a cross-org (`CrossOrgGranted`) caller, the revocation floor is looked up in the provider's own revocation state — and floors exist there only if the provider's operators have imported the caller org's signed revocation bundles. If they have not (the default), the floor is `0` and the revocation check silently evaluates to "permit" for **every** cross-org caller: a compromised member of the foreign org keeps invoking the provider's protected capability for the life of its membership certificate (up to `MAX_ORG_CERT_TTL_SECS` — two years), and the capability grant the provider issued is not floorable either (floors cover membership certificates, not grants). This is deliberate, and it is documented at the exact line of the check: denying on an absent foreign floor set would break every cross-org deployment that has not built bundle distribution, and the check cannot distinguish "no floors were ever needed" from "distribution is broken". The only live provider-side kill switch for cross-org callers is the `provider_policy` closure at the end of admission. Operators who need cross-org revocation must import the foreign orgs' revocation bundles (distribution is an operational surface — it is not automatic) or refuse the callers in `provider_policy`. Do not assume the floor check revokes a cross-org caller.

---

## Breaking changes

**Wire**

- **`TaskState` gains `interrupted`**, configured path only. Older requesters fail to decode it — Rust builds **and previously built Python/Node bindings alike**, because the decode happens in the embedded Rust `TaskState` before any JSON is produced. Only a binding built from this release or later passes the tag through. (An earlier draft of this note said the bindings were transparently forward-compatible; see the correction above.)
- Everything else is additive and decodes on an older build: the two new uncharged services, the two optional `TaskBrief` fields (`service`, `revision`) that a legacy free server ignores, the two payment request headers, and the optional signed `QuoteRequest.input_hash`.

**Rust API consumers**

- **`ProviderChannel::quote` takes a trailing `input_hash: Option<&str>`.** Every implementation and test stub needs the parameter; passing `None` is byte-identical to today's signed request.
- **`RedeemDecision::Admitted` carries a `payer: EntityId`**, and `RedeemDenialReason` gains `InputBindingMismatch`.
- **`CallerDecision::Failed` carries `quote_id: Option<String>`**, so a failure can be correlated with the quote it failed on.
- `Mesh::serve_a2a` and `TaskRegistry::submit` are unchanged; `TaskRegistry` additionally exposes `reserve` / `with_terminal_hook` for the split admission window.

**Python**

- `CapabilityGateway` gains an `a2a_purchase_path` constructor keyword and the `prepare_task` / `purchase_task` / `submit_task` / `a2a_attempts` / `a2a_resolve_attempt` verbs; `PaymentProvider` gains `serve_a2a_configured` / `a2a_unresolved` / `a2a_resolve`. `NetMesh.submit_task` accepts a caller-retained `task_id`. New exceptions: `PaymentRefused`, `JournalOwnedElsewhere`.

**Node / TypeScript**

- `submitTask` accepts an optional `taskId`, plus optional `service` / `revision` addressing a catalog entry. Those two are **free-path only**: a catalog's paid entry refuses the uncharged submit verb with `ERR_PAYMENT` / `no_reservation` before the executor runs — structurally, before the payment gate is even consulted, because there is no admission record to redeem against (`missing_quote` is the different case where a reservation exists but carries no quote header). **Paid A2A is deferred in both directions** — no serving, and no caller verbs either (`describeA2a`, prepare/purchase and `submitTaskPaid` have no Node twin). Free behavior is unchanged.

**Go**

- No A2A surface, unchanged.

---

## How to upgrade

1. **Doing nothing is a supported choice.** Free A2A is untouched. If you never call `serve_a2a_configured`, nothing in this release can reach your deployment.
2. **If you sell a task, put the journal on a local filesystem** and treat `<path>.owner` as part of the store. One live owner per journal; a second one is refused at serve time.
3. **Drain the unresolved queues.** Wire `unresolved()` / `a2a_unresolved()` and `a2a_attempts()` into whatever you already page on. Nothing prunes them for you, and that is the design.
4. **Decide your `reservation_retention_secs` deliberately.** It is the window in which a caller who paid but never submitted keeps its admission; after it, a paid submit is `no_reservation` and reconciliation is manual.
5. **Retiring a revision has no supported in-catalog procedure — plan around it.** Changing `revision` after a prepare strands paid quotes as `StaleRevision`, and the obvious mitigation ("keep the old revision in the catalog for at least `reservation_ttl_secs`") **is not executable as written**: catalog lookup resolves one revision per `service_id`, so the old and new revisions cannot coexist under the same service. Until that changes, the options are to publish the new revision under a **new `service_id`** and retire the old one after its reservations drain, or to accept the refund obligation. No automatic refund exists.
6. **Rust callers implementing `ProviderChannel` add the `input_hash` parameter**, and anyone matching on `RedeemDecision::Admitted` or `CallerDecision::Failed` adds the new field.
7. **Upgrade every requester that polls a configured provider's status** — Rust *and* the Python/Node bindings — before that provider serves paid services, or they will read `interrupted` as a decode error. A previously built native binding is not forward-compatible here; see the note above.

---

Dual-licensed under [MIT](../../LICENSE-MIT) **OR** [Apache-2.0](../../LICENSE-APACHE), at your option.
