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

use net::adapter::net::MeshNode;
use net_sdk::a2a::{
    A2aBounds, A2aOffer, CancelToken, PreparedTask, TaskBrief, TaskExecutor, TaskOwner,
    TaskRegistry,
};
use net_sdk::a2a_journal::{A2aAdmissionJournal, A2aJournalError, SharedAdmissionStore};
use net_sdk::mesh::Mesh;
use net_sdk::mesh_a2a::{
    A2aPrincipal, A2aServiceConfig, A2aServicePolicy, A2aServing, TaskPreflight,
};

use net_payments::engine::PaymentEngine;
use net_payments::flow::a2a::json::{self as boundary, A2aBoundaryError};
use net_payments::flow::a2a::{A2aCallerFlow, AttemptGeneration, AttemptResolution};
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
// The shared boundary
// ---------------------------------------------------------------------------

/// Render a [`A2aBoundaryError`] as the Python exception this surface has
/// always raised for it: a caller-shape refusal is `ValueError`, a
/// failure underneath is `RuntimeError`, and the admission journal keeps
/// its own mapping ([`journal_err`]) so `JournalOwnedElsewhere` stays
/// distinct.
///
/// The documents and their messages live in
/// `net_payments::flow::a2a::json`, shared with the Node binding
/// (`NODE_A2A_PAID_ADMISSION_PLAN.md` WS-A); this module keeps only what
/// is Python's own — the catalog dict parser, the executor and preflight
/// bridges, and this mapping.
fn boundary_err(e: A2aBoundaryError) -> PyErr {
    match e {
        A2aBoundaryError::Invalid(message) => PyValueError::new_err(message),
        A2aBoundaryError::Failed(message) => PyRuntimeError::new_err(message),
        A2aBoundaryError::Journal(e) => journal_err(e),
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

/// The principal vocabulary — one spelling for every binding, kept in
/// `net_payments::flow::a2a::json::parse_principal`.
fn parse_principal(principal: &str) -> PyResult<A2aPrincipal> {
    boundary::parse_principal(principal).map_err(boundary_err)
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
        let owner_json = boundary::owner_to_json(owner).to_string();
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
    py.detach(move || runtime.block_on(boundary::unresolved_json(&store)))
        .map_err(boundary_err)
}

/// Resolve one unresolved-financial admission to a terminal state; see
/// `net_payments::flow::a2a::json::resolve_admission` for how `generation`
/// selects the exact incarnation.
pub(crate) fn resolve_admission(
    py: Python<'_>,
    runtime: Arc<GuardedRuntime>,
    store: SharedAdmissionStore,
    owner_json: &str,
    task_id: String,
    state_json: &str,
    generation: Option<u64>,
) -> PyResult<()> {
    let (owner_json, state_json) = (owner_json.to_string(), state_json.to_string());
    py.detach(move || {
        runtime.block_on(boundary::resolve_admission(
            &store,
            &owner_json,
            task_id,
            &state_json,
            generation,
        ))
    })
    .map_err(boundary_err)
}

// ---------------------------------------------------------------------------
// Caller side: the paid-A2A flow
// ---------------------------------------------------------------------------

/// Build the caller's paid-A2A flow over the gateway's own payment flow.
pub(crate) fn build_flow(
    payments: Arc<CallerPaymentFlow>,
    mesh: Arc<Mesh>,
    purchase_path: &str,
    clock: Arc<dyn Clock>,
) -> Arc<A2aCallerFlow> {
    boundary::build_flow(payments, mesh, purchase_path, clock)
}

/// Decode the `prepared` document a caller hands back.
pub(crate) fn parse_prepared(prepared_json: &str) -> PyResult<PreparedTask> {
    boundary::parse_prepared(prepared_json).map_err(boundary_err)
}

/// `prepare_task`: validate + reserve + quote, moving no money.
pub(crate) async fn do_prepare(
    flow: &A2aCallerFlow,
    mesh: &Mesh,
    provider_node: u64,
    service: String,
    brief: TaskBrief,
) -> String {
    boundary::prepare_json(flow, mesh, provider_node, service, brief).await
}

/// `purchase_task`: consume the stored quote and pay for it.
pub(crate) async fn do_purchase(flow: &A2aCallerFlow, provider_node: u64, task_id: &str) -> String {
    boundary::purchase_json(flow, provider_node, task_id).await
}

/// `submit_task`: send the brief plus the **stored** proof.
pub(crate) async fn do_submit(flow: &A2aCallerFlow, provider_node: u64, task_id: &str) -> String {
    boundary::submit_json(flow, provider_node, task_id).await
}

/// Every stored attempt this gateway's caller identity owns, each labelled
/// `retained` by the class it was read from.
pub(crate) async fn do_attempts(flow: &A2aCallerFlow) -> PyResult<String> {
    boundary::attempts_json(flow).await.map_err(boundary_err)
}

/// Parse the operator's `outcome_json` into an [`AttemptResolution`].
pub(crate) fn parse_resolution(outcome_json: &str) -> PyResult<AttemptResolution> {
    boundary::parse_resolution(outcome_json).map_err(boundary_err)
}

/// Parse the operator's `generation_json` into an [`AttemptGeneration`].
pub(crate) fn parse_generation(generation_json: &str) -> PyResult<AttemptGeneration> {
    boundary::parse_generation(generation_json).map_err(boundary_err)
}

/// Resolve one attempt; `generation` routes to the archive-only exit.
pub(crate) async fn do_resolve_attempt(
    flow: &A2aCallerFlow,
    task_id: &str,
    provider_node: Option<u64>,
    generation: Option<AttemptGeneration>,
    resolution: AttemptResolution,
) -> PyResult<()> {
    boundary::resolve_attempt(flow, task_id, provider_node, generation, resolution)
        .await
        .map_err(boundary_err)
}
