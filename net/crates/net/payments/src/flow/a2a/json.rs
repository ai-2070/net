//! The language-binding boundary for paid A2A, as JSON
//! (`docs/internal/plans/NODE_A2A_PAID_ADMISSION_PLAN.md` D1 / WS-A).
//!
//! Every binding that exposes the paid lifecycle — Python today, Node next
//! — hands the same typed results across the same JSON documents: the
//! `{status: …}` envelopes of prepare / purchase / submit, the attempt and
//! unresolved-admission rows an operator reads, and the documents an
//! operator hands back (`prepared`, an owner, an outcome, a generation).
//! Those projections used to live in the Python binding, so a second
//! binding would have had to re-type them, and the JSON contract — the
//! thing a crashed caller reloads and resubmits — would have been the
//! thing that drifted. They live here once instead.
//!
//! **What does not live here, on purpose.** Each binding's argument
//! extraction (Python's service-catalog dict parser, Node's typed objects)
//! stays at its own edge: how a language spells "absent", which values it
//! refuses, and the field-named messages it gives are that language's
//! contract, and routing them through a generic JSON parser would change
//! what a binding accepts (review finding R5). So do the executor and
//! preflight callback bridges, and the mapping of [`A2aBoundaryError`]
//! onto each language's exceptions.
//!
//! Doctrine #1 holds: nothing here decides anything. The verdicts are
//! [`A2aCallerFlow`]'s and the admission journal's; this module parses
//! documents in and projects typed results out.
//!
//! **H8.** Briefs, offers, quotes and signatures cross; keys never do.

use std::sync::Arc;

use serde_json::{json, Value};

use net_sdk::a2a::{A2aOffer, PreparedTask, TaskBrief, TaskOwner, TaskState};
use net_sdk::a2a_journal::{now_secs, A2aJournalError, SharedAdmissionStore};
use net_sdk::mesh::Mesh;
use net_sdk::mesh_a2a::A2aPrincipal;
use net_sdk::org::OrgAccess;

use crate::core::quote::PaymentQuote;
use crate::flow::{CallerPaymentFlow, Clock};

use super::{
    A2aCallerFlow, A2aPrepareError, A2aPurchase, A2aPurchaseStore, A2aSubmit, AttemptGeneration,
    AttemptResolution, MeshA2aChannel, PurchaseAttempt,
};

/// Why a boundary verb did not produce its document.
///
/// Three classes, because a binding renders them differently and a caller
/// acts on them differently.
#[derive(Debug, thiserror::Error)]
pub enum A2aBoundaryError {
    /// The caller handed the boundary something it refuses: a document of
    /// the wrong shape, or a selector that names nothing (Python
    /// `ValueError`). Nothing was written.
    #[error("{0}")]
    Invalid(String),
    /// A store read, a store write or an encode failed underneath (Python
    /// `RuntimeError`).
    #[error("{0}")]
    Failed(String),
    /// The admission journal refused. Kept typed so a binding can give
    /// [`A2aJournalError::OwnedElsewhere`] its own error class — it is the
    /// one journal failure an operator acts on directly.
    #[error(transparent)]
    Journal(A2aJournalError),
}

fn invalid(message: impl Into<String>) -> A2aBoundaryError {
    A2aBoundaryError::Invalid(message.into())
}

fn store_failed(e: impl std::fmt::Display) -> A2aBoundaryError {
    A2aBoundaryError::Failed(format!("a2a purchase store: {e}"))
}

// ---------------------------------------------------------------------------
// Owners and principals
// ---------------------------------------------------------------------------

/// `TaskOwner` as the journal persists it: the tagged object an operator
/// reads out of an admission record, so an unresolved row's `owner` and the
/// resolve verb's owner argument are the same shape.
///
/// The journal's own encoding is private to `net_sdk::a2a_journal`; this is
/// the boundary's parser for it, and the round-trip is pinned by the
/// bindings' suites (an unresolved row's `owner` is fed straight back into
/// the resolve verb).
pub fn owner_to_json(owner: TaskOwner) -> Value {
    match owner {
        TaskOwner::Local => json!({ "kind": "local" }),
        TaskOwner::Peer(node) => json!({ "kind": "peer", "node": node }),
        TaskOwner::Entity(id) => json!({ "kind": "entity", "entity": hex::encode(id) }),
    }
}

/// Parse an owner document produced by [`owner_to_json`].
pub fn owner_from_json(raw: &str) -> Result<TaskOwner, A2aBoundaryError> {
    let value: Value =
        serde_json::from_str(raw).map_err(|e| invalid(format!("owner_json is not JSON: {e}")))?;
    let kind = value.get("kind").and_then(Value::as_str).ok_or_else(|| {
        invalid(
            "owner_json needs a \"kind\" of \"local\", \"peer\" or \"entity\" — \
             pass the `owner` field of an a2a_unresolved() row verbatim",
        )
    })?;
    match kind {
        "local" => Ok(TaskOwner::Local),
        "peer" => value
            .get("node")
            .and_then(Value::as_u64)
            .map(TaskOwner::Peer)
            .ok_or_else(|| invalid("owner_json kind=peer needs a numeric \"node\"")),
        "entity" => {
            let hex_id = value
                .get("entity")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid("owner_json kind=entity needs a hex \"entity\""))?;
            let bytes = hex::decode(hex_id)
                .map_err(|e| invalid(format!("owner_json entity is not hex: {e}")))?;
            let id: [u8; 32] = bytes
                .try_into()
                .map_err(|_| invalid("owner_json entity must be 32 hex-encoded bytes"))?;
            Ok(TaskOwner::Entity(id))
        }
        other => Err(invalid(format!(
            "owner_json kind {other:?} is not one of local / peer / entity"
        ))),
    }
}

/// The principal vocabulary, spelled as the org serve surface already
/// spells it (`"same_org"` / `"granted"`). One spelling for every binding.
pub fn parse_principal(principal: &str) -> Result<A2aPrincipal, A2aBoundaryError> {
    match principal {
        "session_peer" => Ok(A2aPrincipal::SessionPeer),
        "same_org" => Ok(A2aPrincipal::OrgAdmitted(OrgAccess::SameOrg)),
        "granted" => Ok(A2aPrincipal::OrgAdmitted(OrgAccess::Granted)),
        other => Err(invalid(format!(
            "principal {other:?} is not one of \"session_peer\" (the \
             AEAD-authenticated session peer; direct sessions only), \
             \"same_org\" or \"granted\" (the entity an organization admission \
             proof names, which additionally enforces payer == caller)"
        ))),
    }
}

// ---------------------------------------------------------------------------
// Provider side: the operator queue
// ---------------------------------------------------------------------------

/// The unresolved-financial queue as a JSON array of `AdmissionRecord`s.
pub async fn unresolved_json(store: &SharedAdmissionStore) -> Result<String, A2aBoundaryError> {
    let records = store
        .unresolved()
        .await
        .map_err(A2aBoundaryError::Journal)?;
    serde_json::to_string(&records)
        .map_err(|e| A2aBoundaryError::Failed(format!("encode unresolved records: {e}")))
}

/// Resolve one unresolved-financial admission to a terminal state.
///
/// `generation` names the **exact** incarnation, as it appears on the row
/// in the unresolved queue. One `(owner, task id)` can carry two charges —
/// a redemption retained against a superseded admission and the live
/// replacement that took its place — and the store refuses a key-only
/// resolution of such a key rather than closing one of them at random.
/// Passing the generation off the row the operator is looking at closes
/// that row and nothing else.
pub async fn resolve_admission(
    store: &SharedAdmissionStore,
    owner_json: &str,
    task_id: String,
    state_json: &str,
    generation: Option<u64>,
) -> Result<(), A2aBoundaryError> {
    let owner = owner_from_json(owner_json)?;
    let state: TaskState = serde_json::from_str(state_json).map_err(|e| {
        invalid(format!(
            "state_json is not a TaskState document (e.g. \
             {{\"state\":\"completed\",\"result_ref\":\"blob://x\"}} or \
             {{\"state\":\"failed\",\"error\":\"...\"}}): {e}"
        ))
    })?;
    let Some(generation) = generation else {
        return store
            .resolve(owner, &task_id, state, now_secs())
            .await
            .map_err(A2aBoundaryError::Journal);
    };
    // The identity comes from the store's own row, never from the caller:
    // the admission id and the commitment are part of what a resolution
    // re-presents, and an operator supplies the one field that
    // distinguishes two incarnations.
    let queue = store
        .unresolved()
        .await
        .map_err(A2aBoundaryError::Journal)?;
    let found = queue
        .into_iter()
        .find(|r| r.owner == owner && r.task_id == task_id && r.generation == generation)
        .ok_or_else(|| {
            invalid(format!(
                "no unresolved admission of task {task_id:?} has generation \
                 {generation}; read the `generation` field off the row you mean in \
                 a2a_unresolved()"
            ))
        })?;
    store
        .resolve_exact(&found.identity(), state, now_secs())
        .await
        .map_err(A2aBoundaryError::Journal)
}

// ---------------------------------------------------------------------------
// Caller side: the paid-A2A flow
// ---------------------------------------------------------------------------

/// Build the caller's paid-A2A flow over a gateway's own payment flow.
///
/// One `CallerPaymentFlow` serves both the invoke gate and this: the same
/// caller identity, the same spend-policy store, the same signers — so a
/// budget spent on a paid tool is a budget spent for a paid task.
pub fn build_flow(
    payments: Arc<CallerPaymentFlow>,
    mesh: Arc<Mesh>,
    purchase_path: &str,
    clock: Arc<dyn Clock>,
) -> Arc<A2aCallerFlow> {
    Arc::new(A2aCallerFlow::new(
        payments,
        Arc::new(MeshA2aChannel::new(mesh)),
        Arc::new(A2aPurchaseStore::new(purchase_path)),
        clock,
    ))
}

/// Decode the `prepared` document a caller hands back. A malformed one is a
/// caller-shape error, exactly like a malformed capability id — it is not
/// an outcome of the purchase.
pub fn parse_prepared(prepared_json: &str) -> Result<PreparedTask, A2aBoundaryError> {
    serde_json::from_str(prepared_json).map_err(|e| {
        invalid(format!(
            "prepared_json is not a PreparedTask document (pass the `prepared` \
             field of prepare_task's result verbatim): {e}"
        ))
    })
}

/// Why an offer lookup produced no offer.
///
/// The two arms are different **actions**, not two spellings of one
/// failure. A catalog that never answered may answer the next call. A
/// catalog that answered *without* this service will keep answering
/// without it until the provider is reconfigured, so there is nothing for
/// a caller to wait for. Collapsing them told an operator to retry forever
/// against a service that does not exist.
enum OfferLookup {
    /// `describe_a2a` never produced a catalog — transport, no route, a
    /// provider at capacity.
    Unanswered(String),
    /// The provider answered its catalog, and `service` is not in it.
    NotServed(String),
}

/// The offer for `service` out of what the provider serves.
async fn offer_for(
    mesh: &Mesh,
    provider_node: u64,
    service: &str,
) -> Result<A2aOffer, OfferLookup> {
    let offers = mesh
        .describe_a2a(provider_node)
        .await
        .map_err(|e| OfferLookup::Unanswered(format!("describe_a2a: {e}")))?;
    offers
        .into_iter()
        .find(|o| o.service_id == service)
        .ok_or_else(|| {
            OfferLookup::NotServed(format!(
                "provider {provider_node} serves no service named {service:?}"
            ))
        })
}

/// Project the stored attempt's quote onto the caller-visible price.
///
/// Read off the attempt rather than recomputed: those are the
/// provider-signed bytes the purchase will pay against, so the figure
/// displayed is the figure that gets authorized.
fn quote_json(attempt: &PurchaseAttempt) -> Value {
    let Some(bytes) = attempt.quote_bytes.as_deref() else {
        return Value::Null;
    };
    let Ok(quote) = serde_json::from_slice::<PaymentQuote>(bytes) else {
        return Value::Null;
    };
    let requirements = quote.requirements.view();
    json!({
        "quote_id": quote.quote_id,
        "amount": requirements.amount,
        "network": requirements.network,
        "asset": requirements.asset,
        "expires_at_ns": quote.expires_at_ns,
    })
}

/// `{"status": ..., "message": ..., "retryable": ...}` for an outcome with
/// no typed arm.
///
/// `retryable` is passed in rather than assumed: it is the field a caller
/// actually acts on, and two outcomes reported under one status line do
/// not have to share the answer.
fn status_json(status: &str, message: impl std::fmt::Display, retryable: bool) -> String {
    json!({ "status": status, "message": message.to_string(), "retryable": retryable }).to_string()
}

/// `prepare_task`: validate + reserve + quote, moving no money.
pub async fn prepare_json(
    flow: &A2aCallerFlow,
    mesh: &Mesh,
    provider_node: u64,
    service: String,
    brief: TaskBrief,
) -> String {
    let offer = match offer_for(mesh, provider_node, &service).await {
        Ok(offer) => offer,
        // Nothing was reserved and nothing was quoted either way; what
        // differs is whether a retry can ever change the answer. A catalog
        // that did not answer is `busy`. A catalog that answered without
        // this service is a dead end — the same `rejected` the wire gives
        // for a service the provider refuses or does not price.
        Err(OfferLookup::Unanswered(message)) => return status_json("busy", message, true),
        Err(OfferLookup::NotServed(message)) => return status_json("rejected", message, false),
    };
    let brief = brief.with_service(offer.service_id.clone(), offer.revision.clone());
    let task_id = brief.task_id.clone();
    match flow.prepare_task(provider_node, &offer, &brief).await {
        Ok(prepared) => {
            let quote = match flow.stored_attempt(provider_node, &task_id).await {
                Ok(Some(attempt)) => quote_json(&attempt),
                Ok(None) => Value::Null,
                Err(e) => return status_json("conflict", format!("purchase store: {e}"), true),
            };
            json!({ "status": "ok", "prepared": prepared, "quote": quote }).to_string()
        }
        Err(e) => prepare_error_json(e),
    }
}

/// The prepare statuses, by what a caller should do next.
///
/// `busy` is "nothing was reserved, nothing was quoted, retry" — which
/// covers a provider at capacity, a transport failure, a contended prepare
/// lease and a retryable quote failure alike. `conflict` is "this id
/// already means something else on this provider"; `rejected` and
/// `retired` are dead ends.
fn prepare_error_json(e: A2aPrepareError) -> String {
    let message = e.to_string();
    match e {
        A2aPrepareError::Busy
        | A2aPrepareError::Transport(_)
        | A2aPrepareError::InFlight { .. } => {
            json!({ "status": "busy", "message": message, "retryable": true })
        }
        A2aPrepareError::Quote { retryable, .. } => {
            let status = if retryable { "busy" } else { "rejected" };
            json!({ "status": status, "message": message, "retryable": retryable })
        }
        A2aPrepareError::Rejected { .. }
        | A2aPrepareError::Unpriced { .. }
        | A2aPrepareError::ReservationMismatch { .. }
        // A local size refusal: no packet was sent, nothing was reserved,
        // and no retry or fresh quote can help. It used to fold into
        // `Transport` and render as `busy`/retryable, which told an
        // operator to keep re-sending a brief the wire can never carry.
        | A2aPrepareError::BriefTooLarge { .. } => {
            json!({ "status": "rejected", "message": message, "retryable": false })
        }
        A2aPrepareError::Retired { task_id } => {
            json!({ "status": "retired", "task_id": task_id, "message": message })
        }
        A2aPrepareError::Existing { task_id } | A2aPrepareError::Reconciliation { task_id } => {
            json!({ "status": "conflict", "task_id": task_id, "message": message })
        }
        A2aPrepareError::Attempt(_) => json!({ "status": "conflict", "message": message }),
    }
    .to_string()
}

/// `purchase_task`: consume the stored quote and pay for it.
pub async fn purchase_json(flow: &A2aCallerFlow, provider_node: u64, task_id: &str) -> String {
    match flow.purchase_task(provider_node, task_id).await {
        A2aPurchase::Paid {
            task_id,
            proof,
            billing,
        } => json!({
            "status": "paid",
            "task_id": task_id,
            "quote_id": proof.quote_id,
            "proof": proof,
            "billing": billing,
        }),
        A2aPurchase::RequiresPaymentApproval {
            quote_id,
            policy_reason,
            approve_hint,
        } => json!({
            "status": "requires_payment_approval",
            "task_id": task_id,
            "quote_id": quote_id,
            "policy_reason": policy_reason,
            "approve_hint": approve_hint,
        }),
        A2aPurchase::Denied {
            quote_id,
            policy_reason,
            funds_ambiguous,
        } => json!({
            "status": "denied",
            "task_id": task_id,
            "quote_id": quote_id,
            "policy_reason": policy_reason,
            "funds_ambiguous": funds_ambiguous,
        }),
        A2aPurchase::Unknown { quote_id } => json!({
            "status": "unknown",
            "task_id": task_id,
            "quote_id": quote_id,
        }),
        A2aPurchase::Failed {
            quote_id,
            message,
            retryable,
        } => json!({
            "status": "failed",
            "task_id": task_id,
            "quote_id": quote_id,
            "message": message,
            "retryable": retryable,
        }),
    }
    .to_string()
}

/// `submit_task`: send the brief plus the **stored** proof.
pub async fn submit_json(flow: &A2aCallerFlow, provider_node: u64, task_id: &str) -> String {
    match flow.submit_task(provider_node, task_id).await {
        A2aSubmit::Accepted { task_id } => json!({ "status": "accepted", "task_id": task_id }),
        A2aSubmit::Retry { message } => json!({
            "status": "retry",
            "task_id": task_id,
            "message": message,
        }),
        A2aSubmit::Unexecutable { refusal } => json!({
            "status": "unexecutable",
            "task_id": task_id,
            "message": refusal.message,
            "schematic": refusal.reason.map(|reason| json!({
                "reason": reason,
                "safe_to_retry": refusal.safe_to_retry,
                "safe_to_requote": refusal.safe_to_requote,
            })),
        }),
    }
    .to_string()
}

/// Every stored attempt **this flow's caller identity owns** — the
/// operator's queue.
///
/// The purchase store is a file keyed by `(caller, provider node, task
/// id)` and [`A2aCallerFlow::attempts`] returns every row in it, including
/// rows written by a *different* caller identity sharing the same path (two
/// gateways over one machine-shared store, or one operator rotating a
/// delegated identity). Showing those on this gateway's queue invites an
/// operator to resolve an attempt it cannot possibly have paid for, so the
/// boundary filters to the identity whose money is at stake.
///
/// Filtered by comparing each row's own key against the key **this flow**
/// would mint for that row's provider and task: the caller half is the only
/// field that can differ, and the comparison never has to name it.
///
/// Each row carries `"retained"`: a complete key can hold both the live
/// attempt and the retained evidence of a superseded incarnation, and those
/// two are closed by different arguments — the live one by the key alone, a
/// retained one by passing its `generation`, which routes to the exit that
/// writes only the archive. A queue whose rows cannot be told apart is a
/// queue an operator cannot act on.
///
/// The label is the **class of the map each row was read out of**, and each
/// row is read out of that map by the class-addressed verb that closes it:
/// `stored_attempt` for the live class, `retained_attempts` for the
/// archive. It is never recovered afterwards by comparing a row against the
/// archive listing.
///
/// Recovering it by value would be unsound twice over. The two listings are
/// two separate loads of one file — a store built for cross-process
/// siblings — so a write or a prune landing between them leaves an archive
/// row that no longer equals what the queue read, and that charge is then
/// labelled live: an operator's key-only call closes the live attempt
/// instead, and the archived charge stays open with nothing left pointing
/// at it. And equality cannot separate the classes even within one load,
/// because no field of a record says which map holds it — the archive entry
/// for an incarnation is seeded from that incarnation's live disposition,
/// so the two are built to agree rather than to differ. One store read per
/// key, each through the verb that addresses exactly one class, is the
/// price of a label that cannot be wrong.
pub async fn attempts_json(flow: &A2aCallerFlow) -> Result<String, A2aBoundaryError> {
    let mut rows: Vec<Value> = Vec::new();
    for (provider_node, task_id) in my_keys(flow).await? {
        if let Some(live) = flow
            .stored_attempt(provider_node, &task_id)
            .await
            .map_err(store_failed)?
        {
            rows.push(attempt_row(&live, false)?);
        }
    }
    let archive = flow.retained_attempts().await.map_err(store_failed)?;
    for attempt in &archive {
        // The identity filter `mine` applies, applied to the archive too:
        // it is one file as well, and a charge a different caller identity
        // paid for is not this gateway's to show or to close.
        if attempt.key == flow.key(attempt.key.provider_node, &attempt.key.task_id) {
            rows.push(attempt_row(attempt, true)?);
        }
    }
    serde_json::to_string(&rows)
        .map_err(|e| A2aBoundaryError::Failed(format!("encode purchase attempts: {e}")))
}

/// One attempt as a queue row: its stored fields, plus the `retained` label
/// the class it was read from decided.
fn attempt_row(attempt: &PurchaseAttempt, retained: bool) -> Result<Value, A2aBoundaryError> {
    let mut row = serde_json::to_value(attempt)
        .map_err(|e| A2aBoundaryError::Failed(format!("encode purchase attempt: {e}")))?;
    if let Value::Object(fields) = &mut row {
        fields.insert("retained".to_string(), Value::Bool(retained));
    }
    Ok(row)
}

/// The `(provider node, task id)` of every key this flow's caller identity
/// owns, deduplicated, in the order the store lists them.
///
/// A key is the only thing taken from the class-blind listing — never a
/// class, which is what the listing cannot answer.
async fn my_keys(flow: &A2aCallerFlow) -> Result<Vec<(u64, String)>, A2aBoundaryError> {
    let mut keys: Vec<(u64, String)> = Vec::new();
    for attempt in mine(flow).await? {
        let key = (attempt.key.provider_node, attempt.key.task_id);
        if !keys.contains(&key) {
            keys.push(key);
        }
    }
    Ok(keys)
}

/// The stored attempts belonging to this flow's caller identity.
async fn mine(flow: &A2aCallerFlow) -> Result<Vec<PurchaseAttempt>, A2aBoundaryError> {
    let attempts = flow.attempts().await.map_err(store_failed)?;
    Ok(attempts
        .into_iter()
        .filter(|a| a.key == flow.key(a.key.provider_node, &a.key.task_id))
        .collect())
}

/// Parse the operator's `outcome_json` into an [`AttemptResolution`].
///
/// `resolution` tags the arm because `closed` carries an `outcome` field of
/// its own, and an internally-tagged `outcome` would collide with it.
pub fn parse_resolution(outcome_json: &str) -> Result<AttemptResolution, A2aBoundaryError> {
    let value: Value = serde_json::from_str(outcome_json)
        .map_err(|e| invalid(format!("outcome_json is not JSON: {e}")))?;
    let resolution = value
        .get("resolution")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            invalid(
                "outcome_json needs a \"resolution\": \
                 {\"resolution\":\"paid\",\"proof\":<proof>,\"billing\":{...}} | \
                 {\"resolution\":\"not_paid\",\"reason\":\"...\"} | \
                 {\"resolution\":\"closed\",\"outcome\":\"refunded\",\"evidence\":{...}}",
            )
        })?;
    match resolution {
        "paid" => {
            let proof = value.get("proof").cloned().ok_or_else(|| {
                invalid("resolution=paid needs the \"proof\" the payment produced")
            })?;
            let proof = serde_json::from_value(proof).map_err(|e| {
                invalid(format!(
                    "resolution=paid proof is not a TaskPaymentProof: {e}"
                ))
            })?;
            Ok(AttemptResolution::Paid {
                proof,
                billing: value.get("billing").cloned().unwrap_or(Value::Null),
            })
        }
        "not_paid" => Ok(AttemptResolution::NotPaid {
            reason: value
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or("operator established that no money moved")
                .to_string(),
        }),
        "closed" => Ok(AttemptResolution::Closed {
            outcome: value
                .get("outcome")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    invalid(
                        "resolution=closed needs an \"outcome\" naming what the \
                         operator established (e.g. \"refunded\", \"written_off\", \
                         \"executed_elsewhere\")",
                    )
                })?
                .to_string(),
            evidence: value.get("evidence").cloned().unwrap_or(Value::Null),
        }),
        other => Err(invalid(format!(
            "outcome_json resolution {other:?} is not one of paid / not_paid / closed"
        ))),
    }
}

/// Parse the operator's `generation_json` into an [`AttemptGeneration`] —
/// the `generation` object exactly as it appears on an attempt row.
///
/// Both halves are load-bearing and neither is derivable from the other:
/// `seq` restarts at 1 after a prune, and `incarnation` is unique per
/// creation. So this takes the whole object rather than a number, and a
/// partial one is refused rather than completed with a guess.
pub fn parse_generation(generation_json: &str) -> Result<AttemptGeneration, A2aBoundaryError> {
    let value: Value = serde_json::from_str(generation_json)
        .map_err(|e| invalid(format!("generation is not a JSON document: {e}")))?;
    serde_json::from_value(value).map_err(|e| {
        invalid(format!(
            "generation is not an attempt generation (the {{\"seq\": <int>, \
             \"incarnation\": \"<hex>\"}} object on an a2a_attempts() row): {e}"
        ))
    })
}

/// Resolve one attempt.
///
/// A purchase key is `(caller, provider node, task id)`. The caller half is
/// the flow's own identity, so `provider_node` is the only part an operator
/// has to supply — and when it is supplied the key is **complete** and the
/// attempt is resolved directly, with no search.
///
/// `provider_node = None` is the convenience path: the id is resolved
/// against this caller's own rows. It cannot be used when the same id names
/// attempts on two **providers**, because guessing which purchase to close
/// would close the wrong one — and the refusal hands the operator the exact
/// `provider_node` values to choose from, all of which are valid keys for
/// *this* verb.
///
/// Rows sharing one complete key are **not** such a collision, and must not
/// be reported as one: a retained superseded incarnation sits beside its
/// live replacement under the same `(provider_node, task_id)`, and listing
/// that node twice tells an operator nothing. `generation` selects between
/// them — absent it closes the live attempt, and supplied it closes exactly
/// that retained incarnation through the generation-scoped exit, which
/// writes only the archive. A key whose live attempt is gone and whose
/// retained rows are the only ones left is refused with their generations
/// named, because the key-only verb has nothing to close there.
pub async fn resolve_attempt(
    flow: &A2aCallerFlow,
    task_id: &str,
    provider_node: Option<u64>,
    generation: Option<AttemptGeneration>,
    resolution: AttemptResolution,
) -> Result<(), A2aBoundaryError> {
    let provider_node = match provider_node {
        Some(node) => node,
        None => {
            let attempts = mine(flow).await?;
            let mut nodes: Vec<u64> = attempts
                .iter()
                .filter(|a| a.key.task_id == task_id)
                .map(|a| a.key.provider_node)
                .collect();
            nodes.sort_unstable();
            nodes.dedup();
            match nodes.as_slice() {
                [one] => *one,
                [] => return Err(invalid(format!("no purchase attempt for task {task_id:?}"))),
                many => {
                    return Err(invalid(format!(
                        "task {task_id:?} names attempts on providers {many:?}; pass \
                         provider_node=<one of them> to name the purchase to resolve"
                    )));
                }
            }
        }
    };
    if let Some(generation) = generation {
        return flow
            .resolve_superseded_attempt(provider_node, task_id, &generation, resolution)
            .await
            .map(|_| ())
            .map_err(|e| A2aBoundaryError::Failed(format!("resolve_superseded_attempt: {e}")));
    }
    if flow
        .stored_attempt(provider_node, task_id)
        .await
        .map_err(store_failed)?
        .is_none()
    {
        let retained: Vec<String> = flow
            .retained_attempts()
            .await
            .map_err(store_failed)?
            .iter()
            .filter(|a| a.key.provider_node == provider_node && a.key.task_id == task_id)
            .map(|a| a.generation.to_string())
            .collect();
        if !retained.is_empty() {
            return Err(invalid(format!(
                "task {task_id:?} on provider {provider_node} has no live attempt; what is \
                 left is retained evidence of superseded incarnations ({}) — pass the \
                 `generation` object off the row you mean in a2a_attempts()",
                retained.join(", ")
            )));
        }
    }
    flow.resolve_attempt(provider_node, task_id, resolution)
        .await
        .map(|_| ())
        .map_err(|e| A2aBoundaryError::Failed(format!("resolve_attempt: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_documents_round_trip_for_every_kind() {
        for owner in [
            TaskOwner::Local,
            TaskOwner::Peer(u64::MAX),
            TaskOwner::Entity([0xab; 32]),
        ] {
            let doc = owner_to_json(owner).to_string();
            assert_eq!(owner_from_json(&doc).unwrap(), owner, "{doc}");
        }
    }

    #[test]
    fn a_peer_owner_keeps_a_u64_node_exactly() {
        // The value a JavaScript double would round (2^53 + 1).
        let doc = owner_to_json(TaskOwner::Peer(9_007_199_254_740_993)).to_string();
        assert_eq!(doc, r#"{"kind":"peer","node":9007199254740993}"#);
    }

    #[test]
    fn malformed_owner_documents_are_invalid_not_failed() {
        for (raw, needle) in [
            ("not json", "owner_json is not JSON"),
            (r#"{"node":1}"#, "owner_json needs a \"kind\""),
            (r#"{"kind":"peer"}"#, "kind=peer needs a numeric \"node\""),
            (
                r#"{"kind":"peer","node":-1}"#,
                "kind=peer needs a numeric \"node\"",
            ),
            (r#"{"kind":"entity"}"#, "kind=entity needs a hex \"entity\""),
            (
                r#"{"kind":"entity","entity":"zz"}"#,
                "owner_json entity is not hex",
            ),
            (
                r#"{"kind":"entity","entity":"abcd"}"#,
                "must be 32 hex-encoded bytes",
            ),
            (
                r#"{"kind":"relay"}"#,
                "kind \"relay\" is not one of local / peer / entity",
            ),
        ] {
            match owner_from_json(raw) {
                Err(A2aBoundaryError::Invalid(m)) => assert!(m.contains(needle), "{raw}: {m}"),
                other => panic!("{raw}: expected Invalid, got {other:?}"),
            }
        }
    }

    #[test]
    fn the_principal_vocabulary_is_exactly_three_words() {
        assert_eq!(
            parse_principal("session_peer").unwrap(),
            A2aPrincipal::SessionPeer
        );
        assert_eq!(
            parse_principal("same_org").unwrap(),
            A2aPrincipal::OrgAdmitted(OrgAccess::SameOrg)
        );
        assert_eq!(
            parse_principal("granted").unwrap(),
            A2aPrincipal::OrgAdmitted(OrgAccess::Granted)
        );
        for other in ["anyone", "", "SESSION_PEER", "org"] {
            assert!(
                matches!(parse_principal(other), Err(A2aBoundaryError::Invalid(m)) if m.contains("session_peer")),
                "{other:?}"
            );
        }
    }

    #[test]
    fn resolutions_parse_per_arm_and_refuse_the_rest() {
        assert!(matches!(
            parse_resolution(r#"{"resolution":"not_paid"}"#).unwrap(),
            AttemptResolution::NotPaid { reason } if reason == "operator established that no money moved"
        ));
        assert!(matches!(
            parse_resolution(r#"{"resolution":"closed","outcome":"refunded"}"#).unwrap(),
            AttemptResolution::Closed { outcome, evidence: Value::Null } if outcome == "refunded"
        ));
        for (raw, needle) in [
            ("[", "outcome_json is not JSON"),
            ("{}", "outcome_json needs a \"resolution\""),
            (
                r#"{"resolution":"paid"}"#,
                "resolution=paid needs the \"proof\"",
            ),
            (
                r#"{"resolution":"paid","proof":{}}"#,
                "proof is not a TaskPaymentProof",
            ),
            (
                r#"{"resolution":"closed"}"#,
                "resolution=closed needs an \"outcome\"",
            ),
            (
                r#"{"resolution":"refund"}"#,
                "is not one of paid / not_paid / closed",
            ),
        ] {
            match parse_resolution(raw) {
                Err(A2aBoundaryError::Invalid(m)) => assert!(m.contains(needle), "{raw}: {m}"),
                other => panic!("{raw}: expected Invalid, got {other:?}"),
            }
        }
    }

    #[test]
    fn a_generation_must_be_the_whole_object() {
        for (raw, needle) in [
            ("nope", "generation is not a JSON document"),
            ("7", "is not an attempt generation"),
            (r#"{"seq":1}"#, "is not an attempt generation"),
        ] {
            match parse_generation(raw) {
                Err(A2aBoundaryError::Invalid(m)) => assert!(m.contains(needle), "{raw}: {m}"),
                other => panic!("{raw}: expected Invalid, got {other:?}"),
            }
        }
    }

    #[test]
    fn a_malformed_prepared_document_is_invalid() {
        match parse_prepared(r#"{"provider_node":1}"#) {
            Err(A2aBoundaryError::Invalid(m)) => {
                assert!(m.contains("is not a PreparedTask document"), "{m}")
            }
            other => panic!("expected Invalid, got {other:?}"),
        }
    }
}
