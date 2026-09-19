//! PyO3 surface for **paid** agent-to-agent tasks
//! (`docs/internal/plans/A2A_PAID_ADMISSION_PLAN.md` WS-E) — the marshaling
//! for the configured serving path and the caller's prepare → purchase →
//! submit flow.
//!
//! Doctrine #1 holds throughout: every decision is the Rust layer's. The
//! provider side hands `Mesh::serve_a2a_configured` a catalog,
//! `net-payments`' `EngineTaskAdmissionGate` and a durable
//! `A2aAdmissionJournal`; the caller side hands `net-payments`'
//! `A2aCallerFlow` a node id and reads back its typed verdicts. This file
//! parses config dicts, projects those typed results onto the structured
//! JSON the Python surface returns, and nothing else.
//!
//! **Every handle crossing the boundary is a complete JSON document.**
//! `prepared` is a `PreparedTask` (provider node, the byte-exact brief, the
//! offer hash and the reservation) and `proof` is a `TaskPaymentProof` —
//! nothing is resolved from a hash on a later call, so a Python caller that
//! crashed between paying and submitting reloads what it stored and
//! submits.
//!
//! **H8.** Briefs, offers, quotes and signatures cross; keys never do.

#![cfg(all(feature = "a2a", feature = "payments"))]

use std::collections::BTreeMap;
use std::sync::Arc;

use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::PyDict;
use serde_json::{json, Value};

use net::adapter::net::MeshNode;
use net_sdk::a2a::{
    A2aBounds, A2aOffer, CancelToken, PreparedTask, TaskBrief, TaskExecutor, TaskOwner,
    TaskRegistry, TaskState,
};
use net_sdk::a2a_journal::{now_secs, A2aAdmissionJournal, A2aJournalError, SharedAdmissionStore};
use net_sdk::mesh::Mesh;
use net_sdk::mesh_a2a::{
    A2aPrincipal, A2aServiceConfig, A2aServicePolicy, A2aServing, TaskPreflight,
};
use net_sdk::org::OrgAccess;

use net_payments::core::quote::PaymentQuote;
use net_payments::engine::PaymentEngine;
use net_payments::flow::a2a::{
    A2aCallerFlow, A2aPrepareError, A2aPurchase, A2aPurchaseStore, A2aSubmit, AttemptGeneration,
    AttemptResolution, MeshA2aChannel, PurchaseAttempt,
};
use net_payments::flow::mesh::EngineTaskAdmissionGate;
use net_payments::flow::{CallerPaymentFlow, Clock};

use crate::a2a::{mesh_over, PyA2aServeHandle, Registered};
use crate::runtime_guard::GuardedRuntime;

pyo3::create_exception!(
    _net,
    JournalOwnedElsewhere,
    pyo3::exceptions::PyException,
    "The A2A admission journal at this path is already owned by a live \
     holder — another `PaymentProvider` in this process, or another process \
     on this machine. Two writers over one set of admission records would \
     each believe they may launch paid work, so opening fails closed \
     instead. Ownership is held for the lifetime of the serve handle; stop \
     the other provider (or serve a different path) rather than deleting \
     the `.owner` sidecar."
);

// ---------------------------------------------------------------------------
// Owner marshaling
// ---------------------------------------------------------------------------

/// `TaskOwner` as the journal persists it: the tagged object an operator
/// reads out of an admission record, so `a2a_unresolved()`'s rows and
/// `a2a_resolve()`'s argument are the same shape.
///
/// The journal's own encoding is private to `net_sdk::a2a_journal`; this is
/// the boundary's parser for it, and the round-trip is pinned by
/// `tests/test_a2a_paid.py` (an unresolved row's `owner` is fed straight
/// back into `a2a_resolve`).
fn owner_to_json(owner: TaskOwner) -> Value {
    match owner {
        TaskOwner::Local => json!({ "kind": "local" }),
        TaskOwner::Peer(node) => json!({ "kind": "peer", "node": node }),
        TaskOwner::Entity(id) => json!({ "kind": "entity", "entity": hex::encode(id) }),
    }
}

fn owner_from_json(raw: &str) -> PyResult<TaskOwner> {
    let value: Value = serde_json::from_str(raw)
        .map_err(|e| PyValueError::new_err(format!("owner_json is not JSON: {e}")))?;
    let kind = value.get("kind").and_then(Value::as_str).ok_or_else(|| {
        PyValueError::new_err(
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
            .ok_or_else(|| PyValueError::new_err("owner_json kind=peer needs a numeric \"node\"")),
        "entity" => {
            let hex_id = value.get("entity").and_then(Value::as_str).ok_or_else(|| {
                PyValueError::new_err("owner_json kind=entity needs a hex \"entity\"")
            })?;
            let bytes = hex::decode(hex_id)
                .map_err(|e| PyValueError::new_err(format!("owner_json entity is not hex: {e}")))?;
            let id: [u8; 32] = bytes.try_into().map_err(|_| {
                PyValueError::new_err("owner_json entity must be 32 hex-encoded bytes")
            })?;
            Ok(TaskOwner::Entity(id))
        }
        other => Err(PyValueError::new_err(format!(
            "owner_json kind {other:?} is not one of local / peer / entity"
        ))),
    }
}

// ---------------------------------------------------------------------------
// Provider side: the service catalog
// ---------------------------------------------------------------------------

/// One present, non-`None` key of a dict; absent and `None` are the same
/// thing, so an explicit `pricing_terms: None` reads as "free".
fn item<'py>(entry: &Bound<'py, PyDict>, key: &str) -> PyResult<Option<Bound<'py, PyAny>>> {
    Ok(entry.get_item(key)?.filter(|v| !v.is_none()))
}

fn need_u64(entry: &Bound<'_, PyDict>, service: &str, key: &str) -> PyResult<u64> {
    match item(entry, key)? {
        Some(v) => v.extract::<u64>().map_err(|e| {
            PyValueError::new_err(format!(
                "services[{service:?}][{key:?}] must be a non-negative int: {e}"
            ))
        }),
        None => Err(PyValueError::new_err(format!(
            "services[{service:?}] is missing required key {key:?}"
        ))),
    }
}

fn need_str(entry: &Bound<'_, PyDict>, service: &str, key: &str) -> PyResult<String> {
    match item(entry, key)? {
        Some(v) => v.extract::<String>().map_err(|e| {
            PyValueError::new_err(format!("services[{service:?}][{key:?}] must be a str: {e}"))
        }),
        None => Err(PyValueError::new_err(format!(
            "services[{service:?}] is missing required key {key:?}"
        ))),
    }
}

fn opt_str(entry: &Bound<'_, PyDict>, service: &str, key: &str) -> PyResult<Option<String>> {
    match item(entry, key)? {
        Some(v) => v.extract::<String>().map(Some).map_err(|e| {
            PyValueError::new_err(format!(
                "services[{service:?}][{key:?}] must be a str or None: {e}"
            ))
        }),
        None => Ok(None),
    }
}

fn parse_bounds(entry: &Bound<'_, PyDict>, service: &str) -> PyResult<A2aBounds> {
    let bounds = match item(entry, "bounds")? {
        Some(v) => v.cast_into::<PyDict>().map_err(|_| {
            PyValueError::new_err(format!(
                "services[{service:?}][\"bounds\"] must be a dict of \
                 {{max_prompt_bytes, max_context_refs, max_tags, max_tag_bytes, \
                 max_in_flight}}"
            ))
        })?,
        None => {
            return Err(PyValueError::new_err(format!(
                "services[{service:?}] is missing required key \"bounds\""
            )))
        }
    };
    Ok(A2aBounds {
        max_prompt_bytes: need_u64(&bounds, service, "max_prompt_bytes")?,
        max_context_refs: need_u64(&bounds, service, "max_context_refs")?,
        max_tags: need_u64(&bounds, service, "max_tags")?,
        max_tag_bytes: need_u64(&bounds, service, "max_tag_bytes")?,
        max_in_flight: need_u64(&bounds, service, "max_in_flight")?,
    })
}

/// Parse `services: dict[str, dict]` into the catalog
/// `Mesh::serve_a2a_configured` validates.
///
/// `pricing_terms` is the free/paid selector and nothing else: present
/// means [`A2aServicePolicy::Paid`], absent (or `None`) means
/// [`A2aServicePolicy::Free`]. There is deliberately no "paid" flag a
/// caller could set without terms — an announced price with no terms, or
/// terms with no gate, is refused at serve time rather than served free.
fn parse_services(services: &Bound<'_, PyDict>) -> PyResult<BTreeMap<String, A2aServicePolicy>> {
    if services.is_empty() {
        return Err(PyValueError::new_err(
            "serve_a2a_configured needs at least one service — an empty catalog \
             refuses every brief",
        ));
    }
    let mut out = BTreeMap::new();
    for (key, value) in services.iter() {
        let service: String = key.extract().map_err(|e| {
            PyValueError::new_err(format!("services keys must be service-id strings: {e}"))
        })?;
        let entry = value.cast_into::<PyDict>().map_err(|_| {
            PyValueError::new_err(format!(
                "services[{service:?}] must be a dict of {{revision, pricing_terms, \
                 bounds, reservation_ttl_secs, reservation_retention_secs, \
                 retention_secs, description}}"
            ))
        })?;
        let offer = A2aOffer {
            service_id: service.clone(),
            revision: need_str(&entry, &service, "revision")?,
            description: opt_str(&entry, &service, "description")?,
            pricing_terms: opt_str(&entry, &service, "pricing_terms")?,
            bounds: parse_bounds(&entry, &service)?,
            reservation_ttl_secs: need_u64(&entry, &service, "reservation_ttl_secs")?,
            reservation_retention_secs: need_u64(&entry, &service, "reservation_retention_secs")?,
            retention_secs: need_u64(&entry, &service, "retention_secs")?,
        };
        let policy = if offer.pricing_terms.is_some() {
            A2aServicePolicy::Paid(offer)
        } else {
            A2aServicePolicy::Free(offer)
        };
        out.insert(service, policy);
    }
    Ok(out)
}

/// The principal vocabulary, spelled as the org serve surface already
/// spells it (`"same_org"` / `"granted"`).
fn parse_principal(principal: &str) -> PyResult<A2aPrincipal> {
    match principal {
        "session_peer" => Ok(A2aPrincipal::SessionPeer),
        "same_org" => Ok(A2aPrincipal::OrgAdmitted(OrgAccess::SameOrg)),
        "granted" => Ok(A2aPrincipal::OrgAdmitted(OrgAccess::Granted)),
        other => Err(PyValueError::new_err(format!(
            "principal {other:?} is not one of \"session_peer\" (the \
             AEAD-authenticated session peer; direct sessions only), \
             \"same_org\" or \"granted\" (the entity an organization admission \
             proof names, which additionally enforces payer == caller)"
        ))),
    }
}

// ---------------------------------------------------------------------------
// Provider side: the Python executor + preflight
// ---------------------------------------------------------------------------

/// The configured path's [`TaskExecutor`]: the same async Python callback
/// the free path takes, called with the catalog service and revision as
/// **keyword** arguments.
///
/// Keywords, not positions, so a handler written for the free
/// `serve_a2a` signature `(task_id, prompt, context_refs, tags)` is never
/// silently handed two more positionals.
struct ConfiguredExecutor {
    callback: Py<PyAny>,
}

#[async_trait::async_trait]
impl TaskExecutor for ConfiguredExecutor {
    async fn run(&self, brief: TaskBrief, cancel: CancelToken) -> Result<String, String> {
        // GIL only to build + submit the coroutine; await it off the GIL.
        let fut = Python::attach(|py| -> PyResult<_> {
            let kwargs = PyDict::new(py);
            kwargs.set_item("service", brief.service.clone())?;
            kwargs.set_item("revision", brief.revision.clone())?;
            let coro = self.callback.bind(py).call(
                (
                    brief.task_id.as_str(),
                    brief.prompt.as_str(),
                    brief.context_refs.clone(),
                    brief.tags.clone(),
                ),
                Some(&kwargs),
            )?;
            crate::async_bridge::dispatch_handler_coro(py, coro)
        })
        .map_err(|e| format!("a2a executor: calling the task handler failed: {e}"))?;

        // Same cancel discipline as the free path: `biased` polls the
        // handler's result first so an already-in result beats a
        // simultaneous cancel, and dropping `fut` cancels the coroutine.
        tokio::select! {
            biased;
            r = fut => match r {
                Ok(obj) => Python::attach(|py| {
                    obj.bind(py)
                        .extract::<String>()
                        .map_err(|e| format!("a2a task handler must return a str result ref: {e}"))
                }),
                Err(e) => Err(format!("a2a task handler raised: {e}")),
            },
            _ = cancel.cancelled() => Err("cancelled".to_string()),
        }
    }
}

/// The application preflight: an async Python callable
/// `(owner_json, offer_json, brief_json) -> None | str`.
///
/// `None` admits; a string refuses with that reason travelling to the
/// caller verbatim. A raise is *also* a refusal — the exception text
/// becomes the reason — because an application check that blew up has
/// admitted nothing, and failing open here would admit work the provider
/// never authorized.
struct PyPreflight {
    callback: Py<PyAny>,
}

#[async_trait::async_trait]
impl TaskPreflight for PyPreflight {
    async fn preflight(
        &self,
        owner: TaskOwner,
        offer: &A2aOffer,
        brief: &TaskBrief,
    ) -> Result<(), String> {
        let owner_json = owner_to_json(owner).to_string();
        let offer_json = serde_json::to_string(offer)
            .map_err(|e| format!("preflight refused: encoding the offer failed: {e}"))?;
        let brief_json = serde_json::to_string(brief)
            .map_err(|e| format!("preflight refused: encoding the brief failed: {e}"))?;
        let fut = Python::attach(|py| -> PyResult<_> {
            let coro = self
                .callback
                .bind(py)
                .call1((owner_json, offer_json, brief_json))?;
            crate::async_bridge::dispatch_handler_coro(py, coro)
        })
        .map_err(|e| format!("preflight refused: calling the preflight failed: {e}"))?;
        let obj = fut
            .await
            .map_err(|e| format!("preflight refused: the preflight raised: {e}"))?;
        Python::attach(|py| {
            let bound = obj.bind(py);
            if bound.is_none() {
                return Ok(());
            }
            match bound.extract::<String>() {
                Ok(reason) => Err(reason),
                Err(_) => Err("preflight refused: the preflight must return None \
                               (admit) or a str reason (refuse)"
                    .to_string()),
            }
        })
    }
}

// ---------------------------------------------------------------------------
// Provider side: serving
// ---------------------------------------------------------------------------

/// Serve the configured (catalog-driven) A2A lifecycle over `node`, gated
/// by `engine` and journalled at `journal_path`.
///
/// Returns the serve handle and the admission store the handlers write
/// through. The store is the operator's queue (`a2a_unresolved` /
/// `a2a_resolve`), and the configuration consumed the journal, so this is
/// the only place that handle can come from.
#[allow(clippy::too_many_arguments)]
pub(crate) fn serve_configured(
    py: Python<'_>,
    node: Arc<MeshNode>,
    runtime: Arc<GuardedRuntime>,
    engine: Arc<PaymentEngine>,
    callback: Py<PyAny>,
    services: &Bound<'_, PyDict>,
    journal_path: String,
    principal: &str,
    preflight: Option<Py<PyAny>>,
) -> PyResult<(PyA2aServeHandle, SharedAdmissionStore)> {
    let catalog = parse_services(services)?;
    let principal = parse_principal(principal)?;
    // Opening takes lifetime-exclusive ownership of the journal and runs
    // the recovery pass. Off the GIL: it touches the filesystem.
    let journal = {
        let runtime = runtime.clone();
        py.detach(move || runtime.block_on(A2aAdmissionJournal::open(journal_path)))
    }
    .map_err(journal_err)?;

    let gate = Arc::new(EngineTaskAdmissionGate::new(engine));
    let mut config = A2aServiceConfig::new(catalog)
        .with_payment(gate)
        .with_principal(principal)
        .with_journal(journal);
    if let Some(preflight) = preflight {
        config = config.with_preflight(Arc::new(PyPreflight {
            callback: preflight,
        }));
    }

    let mesh = mesh_over(node);
    let registry = TaskRegistry::new();
    let executor: Arc<dyn TaskExecutor> = Arc::new(ConfiguredExecutor { callback });
    // Registration spawns bridge tasks, so it needs a runtime context.
    let serving: A2aServing = {
        let _guard = runtime.enter();
        mesh.serve_a2a_configured(registry, executor, config)
    }
    .map_err(|e| PyValueError::new_err(format!("serve_a2a_configured refused to start: {e}")))?;
    let store = Arc::clone(&serving.store);
    Ok((
        PyA2aServeHandle::new(mesh, Registered::Configured(serving)),
        store,
    ))
}

/// `OwnedElsewhere` is its own exception: it is the one journal failure an
/// operator can act on directly (stop the other holder), and it is what a
/// second provider over one path must never be able to ignore.
fn journal_err(e: A2aJournalError) -> PyErr {
    match e {
        A2aJournalError::OwnedElsewhere { .. } => JournalOwnedElsewhere::new_err(e.to_string()),
        other => PyRuntimeError::new_err(format!("a2a admission journal: {other}")),
    }
}

/// The unresolved-financial queue as a JSON array of `AdmissionRecord`s.
pub(crate) fn unresolved_json(
    py: Python<'_>,
    runtime: Arc<GuardedRuntime>,
    store: SharedAdmissionStore,
) -> PyResult<String> {
    let records = py
        .detach(move || runtime.block_on(store.unresolved()))
        .map_err(journal_err)?;
    serde_json::to_string(&records)
        .map_err(|e| PyRuntimeError::new_err(format!("encode unresolved records: {e}")))
}

/// Resolve one unresolved-financial admission to a terminal state.
///
/// `generation` names the **exact** incarnation, as it appears on the
/// row in `a2a_unresolved()`. One `(owner, task id)` can carry two
/// charges — a redemption retained against a superseded admission and
/// the live replacement that took its place — and the store refuses a
/// key-only resolution of such a key rather than closing one of them at
/// random. Passing the generation off the row the operator is looking at
/// closes that row and nothing else.
pub(crate) fn resolve_admission(
    py: Python<'_>,
    runtime: Arc<GuardedRuntime>,
    store: SharedAdmissionStore,
    owner_json: &str,
    task_id: String,
    state_json: &str,
    generation: Option<u64>,
) -> PyResult<()> {
    let owner = owner_from_json(owner_json)?;
    let state: TaskState = serde_json::from_str(state_json).map_err(|e| {
        PyValueError::new_err(format!(
            "state_json is not a TaskState document (e.g. \
             {{\"state\":\"completed\",\"result_ref\":\"blob://x\"}} or \
             {{\"state\":\"failed\",\"error\":\"...\"}}): {e}"
        ))
    })?;
    let Some(generation) = generation else {
        return py
            .detach(move || runtime.block_on(store.resolve(owner, &task_id, state, now_secs())))
            .map_err(journal_err);
    };
    py.detach(move || {
        runtime.block_on(async move {
            // The identity comes from the store's own row, never from
            // Python: the admission id and the commitment are part of
            // what a resolution re-presents, and an operator supplies
            // the one field that distinguishes two incarnations.
            let queue = store.unresolved().await.map_err(journal_err)?;
            let found = queue
                .into_iter()
                .find(|r| r.owner == owner && r.task_id == task_id && r.generation == generation)
                .ok_or_else(|| {
                    PyValueError::new_err(format!(
                        "no unresolved admission of task {task_id:?} has generation \
                         {generation}; read the `generation` field off the row you mean in \
                         a2a_unresolved()"
                    ))
                })?;
            store
                .resolve_exact(&found.identity(), state, now_secs())
                .await
                .map_err(journal_err)
        })
    })
}

// ---------------------------------------------------------------------------
// Caller side: the paid-A2A flow
// ---------------------------------------------------------------------------

/// Build the caller's paid-A2A flow over the gateway's own payment flow.
///
/// One `CallerPaymentFlow` serves both the invoke gate and this: the same
/// caller identity, the same spend-policy store, the same signers — so a
/// budget spent on a paid tool is a budget spent for a paid task.
pub(crate) fn build_flow(
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

/// Decode the `prepared` document a caller hands back. A malformed one is
/// a caller-shape error and raises, exactly like a malformed capability id
/// — it is not an outcome of the purchase.
pub(crate) fn parse_prepared(prepared_json: &str) -> PyResult<PreparedTask> {
    serde_json::from_str(prepared_json).map_err(|e| {
        PyValueError::new_err(format!(
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
/// without it until the provider is reconfigured, so there is nothing
/// for a caller to wait for. Collapsing them told an operator to retry
/// forever against a service that does not exist.
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

/// `{"status": ..., "message": ..., "retryable": ...}` for an outcome
/// with no typed arm.
///
/// `retryable` is passed in rather than assumed: it is the field a
/// caller actually acts on, and two outcomes reported under one status
/// line do not have to share the answer.
fn status_json(status: &str, message: impl std::fmt::Display, retryable: bool) -> String {
    json!({ "status": status, "message": message.to_string(), "retryable": retryable }).to_string()
}

/// `prepare_task`: validate + reserve + quote, moving no money.
pub(crate) async fn do_prepare(
    flow: &A2aCallerFlow,
    mesh: &Mesh,
    provider_node: u64,
    service: String,
    brief: TaskBrief,
) -> String {
    let offer = match offer_for(mesh, provider_node, &service).await {
        Ok(offer) => offer,
        // Nothing was reserved and nothing was quoted either way; what
        // differs is whether a retry can ever change the answer. A
        // catalog that did not answer is `busy`. A catalog that
        // answered without this service is a dead end — the same
        // `rejected` the wire gives for a service the provider refuses
        // or does not price.
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
        // A local size refusal: no packet was sent, nothing was
        // reserved, and no retry or fresh quote can help. It used to
        // fold into `Transport` and render as `busy`/retryable, which
        // told an operator to keep re-sending a brief the wire can
        // never carry.
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
pub(crate) async fn do_purchase(flow: &A2aCallerFlow, provider_node: u64, task_id: &str) -> String {
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
pub(crate) async fn do_submit(flow: &A2aCallerFlow, provider_node: u64, task_id: &str) -> String {
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

/// Every stored attempt **this gateway's caller identity owns** — the
/// operator's queue.
///
/// The purchase store is a file keyed by `(caller, provider node, task
/// id)` and `A2aCallerFlow::attempts` returns every row in it, including
/// rows written by a *different* caller identity sharing the same path
/// (two gateways over one machine-shared store, or one operator rotating
/// a delegated identity). Showing those on this gateway's queue invites
/// an operator to resolve an attempt it cannot possibly have paid for, so
/// the boundary filters to the identity whose money is at stake.
///
/// Filtered by comparing each row's own key against the key **this flow**
/// would mint for that row's provider and task: the caller half is the
/// only field that can differ, and the comparison never has to name it.
///
/// Each row carries `"retained"`: a complete key can hold both the live
/// attempt and the retained evidence of a superseded incarnation, and
/// those two are closed by different arguments — the live one by the key
/// alone, a retained one by passing its `generation`, which routes to
/// the exit that writes only the archive. A queue whose rows cannot be
/// told apart is a queue an operator cannot act on.
///
/// The label is the **class of the map each row was read out of**, and
/// each row is read out of that map by the class-addressed verb that
/// closes it: `stored_attempt` for the live class, `retained_attempts`
/// for the archive. It is never recovered afterwards by comparing a row
/// against the archive listing.
///
/// Recovering it by value would be unsound twice over. The two listings
/// are two separate loads of one file — a store built for cross-process
/// siblings — so a write or a prune landing between them leaves an
/// archive row that no longer equals what the queue read, and that
/// charge is then labelled live: an operator's key-only call closes the
/// live attempt instead, and the archived charge stays open with
/// nothing left pointing at it. And equality cannot separate the
/// classes even within one load, because no field of a record says
/// which map holds it — the archive entry for an incarnation is seeded
/// from that incarnation's live disposition, so the two are built to
/// agree rather than to differ. One store read per key, each through
/// the verb that addresses exactly one class, is the price of a label
/// that cannot be wrong.
pub(crate) async fn do_attempts(flow: &A2aCallerFlow) -> PyResult<String> {
    let mut rows: Vec<Value> = Vec::new();
    for (provider_node, task_id) in my_keys(flow).await? {
        if let Some(live) = flow
            .stored_attempt(provider_node, &task_id)
            .await
            .map_err(|e| PyRuntimeError::new_err(format!("a2a purchase store: {e}")))?
        {
            rows.push(attempt_row(&live, false)?);
        }
    }
    let archive = flow
        .retained_attempts()
        .await
        .map_err(|e| PyRuntimeError::new_err(format!("a2a purchase store: {e}")))?;
    for attempt in &archive {
        // The identity filter `mine` applies, applied to the archive
        // too: it is one file as well, and a charge a different caller
        // identity paid for is not this gateway's to show or to close.
        if attempt.key == flow.key(attempt.key.provider_node, &attempt.key.task_id) {
            rows.push(attempt_row(attempt, true)?);
        }
    }
    serde_json::to_string(&rows)
        .map_err(|e| PyRuntimeError::new_err(format!("encode purchase attempts: {e}")))
}

/// One attempt as a queue row: its stored fields, plus the `retained`
/// label the class it was read from decided.
fn attempt_row(attempt: &PurchaseAttempt, retained: bool) -> PyResult<Value> {
    let mut row = serde_json::to_value(attempt)
        .map_err(|e| PyRuntimeError::new_err(format!("encode purchase attempt: {e}")))?;
    if let Value::Object(fields) = &mut row {
        fields.insert("retained".to_string(), Value::Bool(retained));
    }
    Ok(row)
}

/// The `(provider node, task id)` of every key this flow's caller
/// identity owns, deduplicated, in the order the store lists them.
///
/// A key is the only thing taken from the class-blind listing — never a
/// class, which is what the listing cannot answer.
async fn my_keys(flow: &A2aCallerFlow) -> PyResult<Vec<(u64, String)>> {
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
async fn mine(flow: &A2aCallerFlow) -> PyResult<Vec<PurchaseAttempt>> {
    let attempts = flow
        .attempts()
        .await
        .map_err(|e| PyRuntimeError::new_err(format!("a2a purchase store: {e}")))?;
    Ok(attempts
        .into_iter()
        .filter(|a| a.key == flow.key(a.key.provider_node, &a.key.task_id))
        .collect())
}

/// Parse the operator's `outcome_json` into an [`AttemptResolution`].
///
/// `resolution` tags the arm because `closed` carries an `outcome` field
/// of its own, and an internally-tagged `outcome` would collide with it.
pub(crate) fn parse_resolution(outcome_json: &str) -> PyResult<AttemptResolution> {
    let value: Value = serde_json::from_str(outcome_json)
        .map_err(|e| PyValueError::new_err(format!("outcome_json is not JSON: {e}")))?;
    let resolution = value
        .get("resolution")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            PyValueError::new_err(
                "outcome_json needs a \"resolution\": \
                 {\"resolution\":\"paid\",\"proof\":<proof>,\"billing\":{...}} | \
                 {\"resolution\":\"not_paid\",\"reason\":\"...\"} | \
                 {\"resolution\":\"closed\",\"outcome\":\"refunded\",\"evidence\":{...}}",
            )
        })?;
    match resolution {
        "paid" => {
            let proof = value.get("proof").cloned().ok_or_else(|| {
                PyValueError::new_err("resolution=paid needs the \"proof\" the payment produced")
            })?;
            let proof = serde_json::from_value(proof).map_err(|e| {
                PyValueError::new_err(format!(
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
                    PyValueError::new_err(
                        "resolution=closed needs an \"outcome\" naming what the \
                         operator established (e.g. \"refunded\", \"written_off\", \
                         \"executed_elsewhere\")",
                    )
                })?
                .to_string(),
            evidence: value.get("evidence").cloned().unwrap_or(Value::Null),
        }),
        other => Err(PyValueError::new_err(format!(
            "outcome_json resolution {other:?} is not one of paid / not_paid / closed"
        ))),
    }
}

/// Parse the operator's `generation_json` into an
/// [`AttemptGeneration`] — the `generation` object exactly as it appears
/// on an `a2a_attempts()` row.
///
/// Both halves are load-bearing and neither is derivable from the other:
/// `seq` restarts at 1 after a prune, and `incarnation` is unique per
/// creation. So this takes the whole object rather than a number, and a
/// partial one is refused rather than completed with a guess.
pub(crate) fn parse_generation(generation_json: &str) -> PyResult<AttemptGeneration> {
    let value: Value = serde_json::from_str(generation_json)
        .map_err(|e| PyValueError::new_err(format!("generation is not a JSON document: {e}")))?;
    serde_json::from_value(value).map_err(|e| {
        PyValueError::new_err(format!(
            "generation is not an attempt generation (the {{\"seq\": <int>, \
             \"incarnation\": \"<hex>\"}} object on an a2a_attempts() row): {e}"
        ))
    })
}

/// Resolve one attempt.
///
/// A purchase key is `(caller, provider node, task id)`. The caller half
/// is this gateway's own identity, so `provider_node` is the only part
/// an operator has to supply — and when it is supplied the key is
/// **complete** and the attempt is resolved directly, with no search.
///
/// `provider_node = None` is the convenience path: the id is resolved
/// against this caller's own rows. It cannot be used when the same id
/// names attempts on two **providers**, because guessing which purchase
/// to close would close the wrong one — and the refusal hands the
/// operator the exact `provider_node` values to choose from, all of
/// which are valid keys for *this* verb.
///
/// Rows sharing one complete key are **not** such a collision, and must
/// not be reported as one: a retained superseded incarnation sits beside
/// its live replacement under the same `(provider_node, task_id)`, and
/// listing that node twice tells an operator nothing. `generation`
/// selects between them — absent it closes the live attempt, and
/// supplied it closes exactly that retained incarnation through the
/// generation-scoped exit, which writes only the archive. A key whose
/// live attempt is gone and whose retained rows are the only ones left
/// is refused with their generations named, because the key-only verb
/// has nothing to close there.
pub(crate) async fn do_resolve_attempt(
    flow: &A2aCallerFlow,
    task_id: &str,
    provider_node: Option<u64>,
    generation: Option<AttemptGeneration>,
    resolution: AttemptResolution,
) -> PyResult<()> {
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
                [] => {
                    return Err(PyValueError::new_err(format!(
                        "no purchase attempt for task {task_id:?}"
                    )))
                }
                many => {
                    return Err(PyValueError::new_err(format!(
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
            .map_err(|e| PyRuntimeError::new_err(format!("resolve_superseded_attempt: {e}")));
    }
    if flow
        .stored_attempt(provider_node, task_id)
        .await
        .map_err(|e| PyRuntimeError::new_err(format!("a2a purchase store: {e}")))?
        .is_none()
    {
        let retained: Vec<String> = flow
            .retained_attempts()
            .await
            .map_err(|e| PyRuntimeError::new_err(format!("a2a purchase store: {e}")))?
            .iter()
            .filter(|a| a.key.provider_node == provider_node && a.key.task_id == task_id)
            .map(|a| a.generation.to_string())
            .collect();
        if !retained.is_empty() {
            return Err(PyValueError::new_err(format!(
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
        .map_err(|e| PyRuntimeError::new_err(format!("resolve_attempt: {e}")))
}
