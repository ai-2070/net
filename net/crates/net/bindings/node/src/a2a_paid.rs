//! NAPI surface for **paid** agent-to-agent tasks — the provider half
//! (`docs/internal/plans/NODE_A2A_PAID_ADMISSION_PLAN.md` WS-B), the Node
//! twin of the Python binding's `a2a_paid.rs`.
//!
//! `PaymentProvider.serveA2aConfigured` serves a catalog-driven A2A
//! lifecycle — prepare / submit / status / cancel / describe — gated by
//! the provider's own `PaymentEngine` and journalled under
//! lifetime-exclusive ownership. Every decision is the Rust layer's:
//! `Mesh::serve_a2a_configured` validates the catalog and runs the
//! admission state machine, `EngineTaskAdmissionGate` redeems against the
//! engine, and the operator documents come from the shared
//! `net_payments::flow::a2a::json` boundary the Python binding uses too.
//! This file converts JS arguments, bridges two JS callbacks, and maps
//! errors onto the binding's stable prefixes.
//!
//! **Catalog numbers are `bigint`, checked to u64** (plan D2, review R4).
//! Every bound and duration is u64 in core and part of the offer
//! commitment, so a `number` would silently narrow it.
//!
//! **Preflight** (plan D3): `({ownerJson, offerJson, briefJson}) =>
//! Promise<string | null>`. `null` / `undefined` admits; a string refuses
//! with that reason verbatim; a throw, a rejection, a non-string result or
//! missing the [`PREFLIGHT_BUDGET`] all **refuse** — an application check
//! that did not answer admitted nothing. So does an **abandoned** Promise:
//! one that is pending and that nothing references can never settle, and V8
//! may collect it before the budget runs out. That is reported as such — "a
//! Promise that was dropped before it settled" — at once, rather than as a
//! rejection
//! (`crate::js_promise::is_abandoned`).
//!
//! **Stop is retirement, not release** (plan D6). Stopping the returned
//! handle unregisters the services; the journal's ownership ends only once
//! every launched task and terminal write has let go of it.
//!
//! **H8.** Briefs, offers, quotes and signatures cross; keys never do.

#![cfg(all(feature = "a2a", feature = "payments", feature = "publish"))]
// napi-derive registers these items via a generated `extern "C"` table the
// dead-code lint can't trace under the test profile.
#![allow(dead_code)]

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Weak};
use std::time::Duration;

use napi::bindgen_prelude::*;
use napi::threadsafe_function::{ThreadsafeFunction, ThreadsafeFunctionCallMode};
use napi_derive::napi;

use net::adapter::net::MeshNode;
use net_sdk::a2a::{A2aBounds, A2aOffer, TaskBrief, TaskExecutor, TaskOwner, TaskRegistry};
use net_sdk::a2a_journal::{
    A2aAdmissionJournal, A2aJournalError, AdmissionStore, SharedAdmissionStore,
};
use net_sdk::mesh_a2a::{A2aServiceConfig, A2aServicePolicy, TaskPreflight};

use net_payments::engine::PaymentEngine;
use net_payments::flow::a2a::json::{self as boundary, A2aBoundaryError};
use net_payments::flow::mesh::EngineTaskAdmissionGate;

use crate::a2a::{
    executor_timeout, invalid, A2aServeHandle, ExecutorTsfn, NodeTaskExecutor, Registered,
    TaskBriefJs,
};
use crate::enrollment::mesh_over;

/// The stable prefix of a journal-ownership refusal; `errors.ts` maps it to
/// `JournalOwnedElsewhereError`.
pub(crate) const ERR_JOURNAL_OWNED_ELSEWHERE: &str = "a2a:journal_owned_elsewhere:";

/// How long a JS preflight may take (dispatch + Promise settlement) before
/// it counts as a refusal. Below the caller's 30 s `A2A_CALL_TIMEOUT`, which
/// starts earlier, so a refusal can still reach the caller as a refusal
/// rather than a transport timeout (plan D3, review R3). Headroom, not a
/// guarantee: a slow network can still turn it into a caller timeout.
pub(crate) const PREFLIGHT_BUDGET: Duration = Duration::from_secs(5);

/// The admission journal's refusal, with `OwnedElsewhere` on its own prefix
/// — it is the one an operator acts on directly (stop the other holder, or
/// let its running tasks finish), and the one a second provider over one
/// path must never be able to mistake for anything else.
pub(crate) fn journal_err(e: A2aJournalError) -> Error {
    match e {
        A2aJournalError::OwnedElsewhere { .. } => Error::from_reason(format!(
            "{ERR_JOURNAL_OWNED_ELSEWHERE} {e} — another PaymentProvider in this \
             process or another process holds it, or tasks launched under a \
             stopped handle are still running; let them finish (or cancel them) \
             rather than deleting the `.owner` sidecar"
        )),
        other => Error::from_reason(format!("a2a: a2a admission journal: {other}")),
    }
}

/// [`A2aBoundaryError`] onto the binding's prefixes: the caller's input is
/// `a2a:invalid_argument:`, a failure underneath is plain `a2a:`.
pub(crate) fn boundary_err(e: A2aBoundaryError) -> Error {
    match e {
        A2aBoundaryError::Invalid(message) => invalid(message),
        A2aBoundaryError::Failed(message) => Error::from_reason(format!("a2a: {message}")),
        A2aBoundaryError::Journal(e) => journal_err(e),
    }
}

// ---------------------------------------------------------------------------
// The service catalog
// ---------------------------------------------------------------------------

/// The five admission bounds of one catalog entry. Every field is a checked
/// u64 (`bigint`): they are part of the offer commitment.
#[napi(object, js_name = "A2aBoundsJs")]
pub struct A2aBoundsJs {
    pub max_prompt_bytes: BigInt,
    pub max_context_refs: BigInt,
    pub max_tags: BigInt,
    pub max_tag_bytes: BigInt,
    pub max_in_flight: BigInt,
}

/// One catalog entry for `serveA2aConfigured`.
///
/// `pricingTerms` is the free/paid selector and nothing else: present (the
/// JSON `PaymentProvider.pricingTerms` returns) means paid, absent means
/// free. There is deliberately no "paid" flag a caller could set without
/// terms.
#[napi(object, js_name = "A2aServicePolicyJs")]
pub struct A2aServicePolicyJs {
    pub revision: String,
    pub pricing_terms: Option<String>,
    pub bounds: A2aBoundsJs,
    pub reservation_ttl_secs: BigInt,
    pub reservation_retention_secs: BigInt,
    pub retention_secs: BigInt,
    pub description: Option<String>,
}

/// Optional knobs for `serveA2aConfigured`.
#[napi(object, js_name = "ServeA2aConfiguredOptions")]
pub struct ServeA2aConfiguredOptions {
    /// `"session_peer"` (default), `"same_org"` or `"granted"`.
    pub principal: Option<String>,
    /// Per-task budget for the executor's Promise, exactly as on
    /// `serveA2a` (default 1 hour; `0` disables). A paid task past it ends
    /// `failed` — a terminal execution outcome, **never a refund**.
    pub handler_timeout_ms: Option<u32>,
}

fn catalog_u64(service: &str, field: &str, value: BigInt) -> Result<u64> {
    crate::common::bigint_u64(value)
        .map_err(|e| invalid(format!("services[{service:?}].{field}: {}", e.reason)))
}

fn parse_services(
    services: HashMap<String, A2aServicePolicyJs>,
) -> Result<BTreeMap<String, A2aServicePolicy>> {
    if services.is_empty() {
        return Err(invalid(
            "serveA2aConfigured needs at least one service — an empty catalog \
             refuses every brief",
        ));
    }
    let mut out = BTreeMap::new();
    for (service, entry) in services {
        let b = entry.bounds;
        let offer = A2aOffer {
            service_id: service.clone(),
            revision: entry.revision,
            description: entry.description,
            pricing_terms: entry.pricing_terms,
            bounds: A2aBounds {
                max_prompt_bytes: catalog_u64(
                    &service,
                    "bounds.maxPromptBytes",
                    b.max_prompt_bytes,
                )?,
                max_context_refs: catalog_u64(
                    &service,
                    "bounds.maxContextRefs",
                    b.max_context_refs,
                )?,
                max_tags: catalog_u64(&service, "bounds.maxTags", b.max_tags)?,
                max_tag_bytes: catalog_u64(&service, "bounds.maxTagBytes", b.max_tag_bytes)?,
                max_in_flight: catalog_u64(&service, "bounds.maxInFlight", b.max_in_flight)?,
            },
            reservation_ttl_secs: catalog_u64(
                &service,
                "reservationTtlSecs",
                entry.reservation_ttl_secs,
            )?,
            reservation_retention_secs: catalog_u64(
                &service,
                "reservationRetentionSecs",
                entry.reservation_retention_secs,
            )?,
            retention_secs: catalog_u64(&service, "retentionSecs", entry.retention_secs)?,
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

// ---------------------------------------------------------------------------
// The preflight bridge
// ---------------------------------------------------------------------------

/// What the JS preflight is called with — three complete JSON documents.
// `js_name` pinned: napi's auto-camelCase would emit `A2APreflightArgs`.
#[napi(object, js_name = "A2aPreflightArgs")]
pub struct A2aPreflightArgs {
    /// The owner, as an unresolved row spells it (`{"kind":"peer",...}`).
    /// A `peer` node id is a u64 — read it with `a2aU64`, not `JSON.parse`.
    pub owner_json: String,
    /// The `A2aOffer` the brief was resolved against.
    pub offer_json: String,
    /// The `TaskBrief`.
    pub brief_json: String,
}

pub(crate) type PreflightTsfn =
    ThreadsafeFunction<A2aPreflightArgs, Promise<Option<String>>, A2aPreflightArgs, Status, false>;

struct NodePreflight {
    callback: PreflightTsfn,
    budget: Duration,
}

#[async_trait::async_trait]
impl TaskPreflight for NodePreflight {
    async fn preflight(
        &self,
        owner: TaskOwner,
        offer: &A2aOffer,
        brief: &TaskBrief,
    ) -> std::result::Result<(), String> {
        let args = A2aPreflightArgs {
            owner_json: boundary::owner_to_json(owner).to_string(),
            offer_json: serde_json::to_string(offer)
                .map_err(|e| format!("preflight refused: encoding the offer failed: {e}"))?,
            brief_json: serde_json::to_string(brief)
                .map_err(|e| format!("preflight refused: encoding the brief failed: {e}"))?,
        };
        let (tx, rx) = tokio::sync::oneshot::channel::<napi::Result<Promise<Option<String>>>>();
        let status = self.callback.call_with_return_value(
            args,
            ThreadsafeFunctionCallMode::NonBlocking,
            move |ret, _env| {
                let _ = tx.send(ret);
                Ok(())
            },
        );
        if status != Status::Ok {
            return Err(format!("preflight refused: TSFN enqueue status {status:?}"));
        }
        let answer = async move {
            let promise = match rx.await {
                Ok(Ok(p)) => p,
                Ok(Err(e)) => return Err(format!("preflight refused: the preflight threw: {e}")),
                Err(_) => {
                    return Err("preflight refused: the preflight callback was dropped".to_string())
                }
            };
            promise.await.map_err(|e| {
                crate::js_promise::failure_reason("preflight refused: the preflight", &e, |e| {
                    format!(
                        "preflight refused: the preflight rejected or did not return null or a string: {e}"
                    )
                })
            })
        };
        // On expiry the future — and with it the oneshot — is dropped, so a
        // late answer from JS has nowhere to land and cannot revive the
        // refused attempt.
        match tokio::time::timeout(self.budget, answer).await {
            Err(_) => Err(format!(
                "preflight refused: the preflight did not answer within {} ms",
                self.budget.as_millis()
            )),
            Ok(Err(refusal)) => Err(refusal),
            Ok(Ok(None)) => Ok(()),
            Ok(Ok(Some(reason))) => Err(reason),
        }
    }
}

// ---------------------------------------------------------------------------
// Serving
// ---------------------------------------------------------------------------

/// Everything `serveA2aConfigured` needs that is built on the JS thread
/// (TSFNs are `!Send` until built) — the rest happens in the future.
pub(crate) struct ConfiguredServe {
    pub node: Arc<MeshNode>,
    pub engine: Arc<PaymentEngine>,
    pub executor: ExecutorTsfn,
    pub executor_timeout: Option<Duration>,
    pub preflight: Option<PreflightTsfn>,
    pub catalog: BTreeMap<String, A2aServicePolicy>,
    pub principal: net_sdk::mesh_a2a::A2aPrincipal,
    pub journal_path: String,
}

/// Parse and validate on the JS thread — a malformed catalog rejects before
/// any journal is opened.
pub(crate) fn prepare_serve(
    node: Arc<MeshNode>,
    engine: Arc<PaymentEngine>,
    executor: Function<'_, TaskBriefJs, Promise<String>>,
    services: HashMap<String, A2aServicePolicyJs>,
    journal_path: String,
    options: Option<ServeA2aConfiguredOptions>,
    preflight: Option<Function<'_, A2aPreflightArgs, Promise<Option<String>>>>,
) -> Result<ConfiguredServe> {
    let catalog = parse_services(services)?;
    let (principal, timeout_ms) = match options {
        Some(o) => (o.principal, o.handler_timeout_ms),
        None => (None, None),
    };
    let principal = boundary::parse_principal(principal.as_deref().unwrap_or("session_peer"))
        .map_err(boundary_err)?;
    Ok(ConfiguredServe {
        node,
        engine,
        executor: executor.build_threadsafe_function().build()?,
        executor_timeout: executor_timeout(timeout_ms),
        preflight: match preflight {
            Some(f) => Some(f.build_threadsafe_function().build()?),
            None => None,
        },
        catalog,
        principal,
        journal_path,
    })
}

/// The async half: open the journal (taking ownership and running the
/// recovery pass), then register the five services. Returns the handle and
/// the store the operator verbs read through.
pub(crate) async fn serve(spec: ConfiguredServe) -> Result<(A2aServeHandle, SharedAdmissionStore)> {
    let journal = A2aAdmissionJournal::open(spec.journal_path)
        .await
        .map_err(journal_err)?;
    let mut config = A2aServiceConfig::new(spec.catalog)
        .with_payment(Arc::new(EngineTaskAdmissionGate::new(spec.engine)))
        .with_principal(spec.principal)
        .with_journal(journal);
    if let Some(callback) = spec.preflight {
        config = config.with_preflight(Arc::new(NodePreflight {
            callback,
            budget: PREFLIGHT_BUDGET,
        }));
    }
    let mesh = mesh_over(spec.node, None);
    let executor: Arc<dyn TaskExecutor> =
        Arc::new(NodeTaskExecutor::new(spec.executor, spec.executor_timeout));
    let serving = mesh
        .serve_a2a_configured(TaskRegistry::new(), executor, config)
        .map_err(|e| invalid(format!("serveA2aConfigured refused to start: {e}")))?;
    let store = Arc::clone(&serving.store);
    Ok((
        A2aServeHandle::new(mesh, Registered::Configured(serving)),
        store,
    ))
}

// ---------------------------------------------------------------------------
// The operator queue
// ---------------------------------------------------------------------------

/// The live journal behind a provider's operator verbs, if any.
///
/// The provider holds it `Weak`: the serve handle and every task it launched
/// own the journal, and the operator verbs answer for exactly as long as one
/// of them still does (plan D6 — store lifetime, not registration lifetime,
/// so an operator can read the rows a running task is still writing).
pub(crate) fn live_store(
    slot: &parking_lot::Mutex<Option<Weak<dyn AdmissionStore>>>,
) -> Result<SharedAdmissionStore> {
    // A lifecycle state, not the caller's input: the same call succeeds once
    // `serveA2aConfigured` has opened a journal. So the plain `a2a:` family,
    // never `a2a:invalid_argument:` ("retrying unchanged cannot succeed").
    slot.lock().as_ref().and_then(Weak::upgrade).ok_or_else(|| {
        Error::from_reason(
            "a2a: no A2A admission journal is live — serveA2aConfigured(...) opens one, \
             and it stays live while its serve handle, or any task launched under \
             it, still holds it",
        )
    })
}

pub(crate) async fn unresolved(store: SharedAdmissionStore) -> Result<String> {
    boundary::unresolved_json(&store)
        .await
        .map_err(boundary_err)
}

pub(crate) async fn resolve(
    store: SharedAdmissionStore,
    owner_json: String,
    task_id: String,
    state_json: String,
    generation: Option<BigInt>,
) -> Result<()> {
    let generation = match generation {
        Some(g) => Some(
            crate::common::bigint_u64(g)
                .map_err(|e| invalid(format!("generation: {}", e.reason)))?,
        ),
        None => None,
    };
    boundary::resolve_admission(&store, &owner_json, task_id, &state_json, generation)
        .await
        .map_err(boundary_err)
}
