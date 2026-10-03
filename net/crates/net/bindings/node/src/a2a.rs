//! NAPI surface for agent-to-agent (A2A) task handoff
//! (`HERMES_INTEGRATION_PLAN_V2.md` Phase 3) — the Node twin of the Python
//! `a2a.rs`.
//!
//! A JS agent serves the A2A task lifecycle backed by an **async task
//! executor callback** (its own agent loop), and a JS requester hands off a
//! job, polls, and cancels it by node id. The whole protocol + registry +
//! cancellation lives in `net_sdk::{a2a, mesh_a2a}` (H2); this file marshals
//! through the proven TSFN→Promise bridge (`publish.rs` shape).
//!
//! **Cancellation (one-sided).** A `cancelTask` trips the Rust cancel token,
//! which wins the executor's `select` — the task's registry state flips to
//! `Cancelled` and the JS handler's eventual result is discarded. Unlike the
//! Python binding (whose coroutine is genuinely cancelled), a JS Promise
//! cannot be aborted from outside: the handler keeps running to completion
//! unless it cooperates. Handlers doing real work should check in via their
//! own abort plumbing; the wire-visible contract (state = `cancelled`, no
//! result served) holds regardless.
//!
//! **Deadline.** Each task's JS Promise must settle within
//! `ServeA2aOptions.handlerTimeoutMs` (default 1 hour; `0` disables) or the
//! task records a `Failed` terminal state — a wedged event loop or a
//! never-settling handler must not strand an accepted task in `Running`
//! forever.
//!
//! **H8.** Only task briefs (prompt + Datafort context refs) and result refs
//! cross — never keys.

#![cfg(feature = "a2a")]
// napi-derive registers these items via a generated `extern "C"` table the
// dead-code lint can't trace under the test profile.
#![allow(dead_code)]

use napi::bindgen_prelude::*;
use napi::threadsafe_function::{ThreadsafeFunction, ThreadsafeFunctionCallMode};
use napi_derive::napi;
use parking_lot::Mutex;
use std::sync::Arc;

use net_sdk::a2a::{CancelToken, PreparedTask, TaskBrief, TaskExecutor, TaskRegistry};
use net_sdk::a2a_payment::TaskPaymentProof;
use net_sdk::mesh::Mesh as SdkMesh;
use net_sdk::mesh_a2a::A2aFlowError;
use net_sdk::mesh_rpc::ServeHandle;
use serde_json::Value;

use crate::delegation::u64_arg;
use crate::enrollment::mesh_over;
use crate::NetMesh;

fn a2a_err(msg: impl std::fmt::Display) -> Error {
    Error::from_reason(format!("a2a: {msg}"))
}

/// The stable prefix of a refusal of the caller's own input — a document of
/// the wrong shape, a selector that names nothing, a brief the wire cannot
/// carry. Never a transport failure and never worth retrying unchanged.
/// `errors.ts` maps it to `A2aInvalidArgumentError`.
pub(crate) const ERR_INVALID_ARGUMENT: &str = "a2a:invalid_argument:";

/// The stable prefix of a provider's payment or admission refusal on the
/// raw paid submit. Then one space, the provider's `net.payment.failure@1`
/// schematic as compact JSON exactly as it was encoded (or `null`), a
/// newline, and the human message. The schematic is never re-encoded on the
/// JS side — its `extra` map is open, so a JS parse/stringify round trip
/// could round a number in it — and compact JSON has no raw newline, so the
/// first newline is the boundary. `errors.ts` splits it into
/// `PaymentRefusedError`.
pub(crate) const ERR_PAYMENT_REFUSED: &str = "a2a:payment_refused:";

pub(crate) fn invalid(msg: impl std::fmt::Display) -> Error {
    Error::from_reason(format!("{ERR_INVALID_ARGUMENT} {msg}"))
}

/// [`A2aFlowError`] from the raw paid submit onto the binding's prefixes.
fn paid_submit_err(e: A2aFlowError) -> Error {
    match e {
        A2aFlowError::PaymentRefused { message, schematic } => {
            let schematic = schematic
                .and_then(|s| serde_json::to_string(&*s).ok())
                .unwrap_or_else(|| "null".to_string());
            Error::from_reason(format!("{ERR_PAYMENT_REFUSED} {schematic}\n{message}"))
        }
        // Refused locally, before any packet: no retry and no fresh quote
        // can make an over-long brief or an unpresentable proof deliverable.
        local @ (A2aFlowError::ProofUndeliverable(_) | A2aFlowError::BriefTooLarge { .. }) => {
            invalid(format!("submitTaskPaid: {local}"))
        }
        other => a2a_err(format!("submitTaskPaid: {other}")),
    }
}

/// The value at an RFC 6901 pointer in a JSON document, read by `serde_json`
/// — which keeps u64 integers exact, unlike a JavaScript double.
fn pointed(json: &str, pointer: &str, verb: &str) -> Result<Value> {
    let doc: Value = serde_json::from_str(json)
        .map_err(|e| invalid(format!("{verb}: the document is not JSON: {e}")))?;
    doc.pointer(pointer).cloned().ok_or_else(|| {
        invalid(format!(
            "{verb}: the pointer {pointer:?} names nothing in the document"
        ))
    })
}

/// The sub-document at `pointer` (RFC 6901, e.g. `/prepared`, `/0/owner`) of
/// a paid-A2A JSON document, as a JSON string.
///
/// **The supported way to hand a nested document back.** Paid-A2A documents
/// carry u64 integers — a provider node id, an owner's node id, a recovery
/// generation, a quote expiry in nanoseconds — and `JSON.parse` turns every
/// number into a double, so a value above 2^53 is silently rounded and
/// `JSON.stringify` writes the rounded one back. A `prepared` document whose
/// `provider_node` was rounded names the wrong provider. This reads and
/// re-serializes in Rust, where those integers stay exact.
///
/// Rejects `a2a:invalid_argument:` when `json` is not JSON or the pointer
/// names nothing. `""` is the whole document.
#[napi(js_name = "a2aDocument")]
pub fn a2a_document(json: String, pointer: String) -> Result<String> {
    let value = pointed(&json, &pointer, "a2aDocument")?;
    serde_json::to_string(&value).map_err(|e| a2a_err(format!("a2aDocument: encode: {e}")))
}

/// The u64 integer at `pointer` in a paid-A2A JSON document, as a `bigint` —
/// exact, where `JSON.parse` would round anything above 2^53. Rejects
/// `a2a:invalid_argument:` when the pointer names nothing or names anything
/// but a non-negative integer.
#[napi(js_name = "a2aU64")]
pub fn a2a_u64(json: String, pointer: String) -> Result<BigInt> {
    match pointed(&json, &pointer, "a2aU64")?.as_u64() {
        Some(v) => Ok(BigInt::from(v)),
        None => Err(invalid(format!(
            "a2aU64: the value at {pointer:?} is not a non-negative integer that fits a u64"
        ))),
    }
}

impl NetMesh {
    /// The SDK `Mesh` every A2A requester verb calls through: a fresh one
    /// over the live node, carrying the organization identity
    /// `setA2aOrgCaller` installed — applied per call, so a later
    /// `setA2aOrgCaller(null)` takes effect on the next verb.
    pub(crate) fn a2a_requester(&self) -> Result<SdkMesh> {
        let node = self.node_arc_clone()?;
        let mesh = mesh_over(node, None);
        #[cfg(feature = "org")]
        mesh.set_a2a_org_caller(self.a2a_org_caller_slot().lock().client());
        Ok(mesh)
    }
}

/// Default budget for one task-executor call (JS returning the Promise +
/// the Promise settling), against a single deadline. A2A tasks are long
/// jobs by design — cooperative cancellation is the primary control — but
/// a wedged Node event loop or a never-settling Promise must not strand
/// an accepted task in `Running` forever (the registry would retain it
/// indefinitely and requesters would poll a task that can no longer end).
/// Past the deadline the task records a `Failed` terminal state. Genuinely
/// longer jobs override via `ServeA2aOptions.handlerTimeoutMs`.
const DEFAULT_TASK_HANDLER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60 * 60);

/// Optional knobs for [`NetMesh::serve_a2a`].
// `js_name` pinned like `serveA2a` / `A2aServeHandle`: napi's auto-camelCase
// would emit `ServeA2AOptions`.
#[napi(object, js_name = "ServeA2aOptions")]
pub struct ServeA2aOptions {
    /// Per-task budget in milliseconds for the JS executor to settle its
    /// Promise (the handler returning the Promise + that Promise
    /// resolving, one deadline across both). Default `3600000` (1 hour).
    /// Past it the task records a `Failed` terminal state. Pass `0` to
    /// disable the deadline entirely (Python-binding parity, where
    /// cancellation is the only control) — a wedged event loop then leaves
    /// the task `Running` until a requester cancels it.
    pub handler_timeout_ms: Option<u32>,
}

/// The brief handed to the JS task executor.
#[napi(object)]
pub struct TaskBriefJs {
    /// The registry-assigned task id (poll / cancel by this).
    pub task_id: String,
    /// What to do.
    pub prompt: String,
    /// Datafort refs carrying the task's context (the executor doesn't
    /// share the requester's memory).
    pub context_refs: Vec<String>,
    /// Routing / bookkeeping tags.
    pub tags: Vec<String>,
    /// The catalog service this task was admitted under. Set only on the
    /// configured path (`PaymentProvider.serveA2aConfigured`); absent on the
    /// free `serveA2a` path, so a handler written for that ignores it.
    pub service: Option<String>,
    /// The catalog revision this task was admitted under (configured path
    /// only, like `service`).
    pub revision: Option<String>,
}

/// The bridged JS task executor:
/// `(brief: TaskBriefJs) => Promise<string>` resolving to the result's
/// artifact ref.
pub(crate) type ExecutorTsfn =
    ThreadsafeFunction<TaskBriefJs, Promise<String>, TaskBriefJs, Status, false>;

/// `handlerTimeoutMs` to a deadline: absent is the default, `0` is the
/// explicit opt-out (cancellation is then the only control). Shared by the
/// free and the configured serving paths so they cannot disagree.
pub(crate) fn executor_timeout(handler_timeout_ms: Option<u32>) -> Option<std::time::Duration> {
    match handler_timeout_ms {
        Some(0) => None,
        Some(ms) => Some(std::time::Duration::from_millis(u64::from(ms))),
        None => Some(DEFAULT_TASK_HANDLER_TIMEOUT),
    }
}

/// A [`TaskExecutor`] backed by a JS **async** callback. A mesh-side cancel
/// trips the select below; the registry records `Cancelled` and the JS
/// handler's result (if it ever resolves) is discarded. `timeout` bounds
/// how long the JS side may take to settle (`None` = unbounded, the
/// explicit `handlerTimeoutMs: 0` opt-out).
pub(crate) struct NodeTaskExecutor {
    callback: ExecutorTsfn,
    timeout: Option<std::time::Duration>,
}

impl NodeTaskExecutor {
    pub(crate) fn new(callback: ExecutorTsfn, timeout: Option<std::time::Duration>) -> Self {
        Self { callback, timeout }
    }
}

#[async_trait::async_trait]
impl TaskExecutor for NodeTaskExecutor {
    async fn run(
        &self,
        brief: TaskBrief,
        cancel: CancelToken,
    ) -> std::result::Result<String, String> {
        let args = TaskBriefJs {
            task_id: brief.task_id.clone(),
            prompt: brief.prompt.clone(),
            context_refs: brief.context_refs.clone(),
            tags: brief.tags.clone(),
            service: brief.service.clone(),
            revision: brief.revision.clone(),
        };
        // Enqueue the JS call; the oneshot resolves with the handler's
        // returned Promise (or its synchronous throw).
        let (tx, rx) = tokio::sync::oneshot::channel::<napi::Result<Promise<String>>>();
        let status = self.callback.call_with_return_value(
            args,
            ThreadsafeFunctionCallMode::NonBlocking,
            move |ret, _env| {
                let _ = tx.send(ret);
                Ok(())
            },
        );
        if status != Status::Ok {
            return Err(format!("a2a executor: TSFN enqueue status {status:?}"));
        }

        let js_result = async move {
            let promise = match rx.await {
                Ok(Ok(p)) => p,
                Ok(Err(e)) => {
                    return Err(format!(
                        "a2a task handler threw before returning a Promise: {e}"
                    ))
                }
                Err(_) => return Err("a2a task handler callback channel disconnected".to_string()),
            };
            match promise.await {
                Ok(v) => Ok(v),
                Err(e) => Err(crate::js_promise::failure_reason(
                    "a2a task handler",
                    &e,
                    |e| format!("a2a task handler Promise rejected: {e}"),
                )),
            }
        };

        // One deadline across both JS stages (handler returning the
        // Promise + the Promise settling) — a timed-out task records a
        // `Failed` terminal state instead of sitting in `Running` forever.
        let bounded = async {
            match self.timeout {
                Some(t) => match tokio::time::timeout(t, js_result).await {
                    Ok(r) => r,
                    Err(_) => Err(format!(
                        "a2a task handler did not settle within {} ms",
                        t.as_millis()
                    )),
                },
                None => js_result.await,
            }
        };

        // `biased` polls the JS result first so an already-resolved result
        // beats a simultaneous cancel. On cancel the future drops — the JS
        // work itself cannot be aborted (see the module docs), but its
        // result is discarded and the registry records `Cancelled`.
        tokio::select! {
            biased;
            r = bounded => r,
            _ = cancel.cancelled() => Err("cancelled".to_string()),
        }
    }
}

/// What a serve call registered — held opaquely, because dropping it is
/// the whole contract (the Python binding's `Registered`).
pub(crate) enum Registered {
    /// The free path: one `ServeHandle` per service (task/status/cancel).
    Legacy(Vec<ServeHandle>),
    /// The configured path: five registrations plus the journal's
    /// exclusive-ownership handle, kept alive for exactly as long as the
    /// handlers that can still write.
    #[cfg(all(feature = "payments", feature = "publish"))]
    Configured(net_sdk::mesh_a2a::A2aServing),
}

impl Registered {
    /// Three on the free path, five on the configured one (plus prepare and
    /// describe).
    fn services(&self) -> usize {
        match self {
            Registered::Legacy(handles) => handles.len(),
            #[cfg(all(feature = "payments", feature = "publish"))]
            Registered::Configured(serving) => serving.handles.len(),
        }
    }
}

/// The registration a handle owns.
pub(crate) type SharedRegistration = Arc<Mutex<Option<(SdkMesh, Registered)>>>;

/// The `PaymentProvider`'s view of a registration it created: **weak**, so
/// the handle stays the only owner and dropping it (a `#[napi]` class is
/// GC-finalized) still unregisters the services, while `provider.close()` can
/// retire any registration that is still alive.
pub(crate) type WeakRegistration = std::sync::Weak<Mutex<Option<(SdkMesh, Registered)>>>;

/// Keeps the served A2A services alive (returned by `NetMesh.serveA2a` or
/// `PaymentProvider.serveA2aConfigured`). Dropping it or calling
/// [`stop`](Self::stop) unregisters them.
// `js_name` pinned: napi's auto-camelCase would emit `A2AServeHandle` /
// `serveA2A`; the plan + Python parity spell the surface `A2aServeHandle`
// / `serveA2a`.
#[napi(js_name = "A2aServeHandle")]
pub struct A2aServeHandle {
    // The `Mesh` holds the channel registry the services registered against,
    // and the registrations are whatever the serving path returned. A
    // `parking_lot::Mutex` because napi hands out `&self`; a `#[napi]` class
    // is GC-finalized, not scope-dropped, so `stop()` is the deterministic
    // release (the `close()` gotcha in `bindings.md`).
    inner: SharedRegistration,
}

impl A2aServeHandle {
    pub(crate) fn new(mesh: SdkMesh, registered: Registered) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Some((mesh, registered)))),
        }
    }

    /// The registration, for the provider that created it.
    pub(crate) fn shared(&self) -> SharedRegistration {
        Arc::clone(&self.inner)
    }
}

#[napi]
impl A2aServeHandle {
    /// Stop accepting A2A tasks: unregister the services and release this
    /// handle's references. Idempotent.
    ///
    /// On the configured path this **retires the registration; it does not
    /// release the admission journal** while work remains. A task already
    /// launched, and the terminal write that records its outcome, keep
    /// their hold on the journal until they finish — so a second provider
    /// on the same journal path is still refused until then. That is the
    /// guarantee that no two owners ever both believe they may launch paid
    /// work.
    #[napi]
    pub fn stop(&self) {
        let _ = self.inner.lock().take();
    }

    /// Whether the services are still registered.
    #[napi(getter)]
    pub fn serving(&self) -> bool {
        self.inner.lock().is_some()
    }

    /// How many nRPC services are registered: three on the free `serveA2a`
    /// path, five on the configured one (which also serves
    /// `net.a2a.prepare` and `net.a2a.describe`). `0` once stopped.
    #[napi(getter)]
    pub fn services(&self) -> u32 {
        self.inner
            .lock()
            .as_ref()
            .map_or(0, |(_, r)| r.services() as u32)
    }
}

#[napi]
impl NetMesh {
    /// **Executor side.** Serve the A2A task lifecycle on this node, backed
    /// by a JS **async** task executor
    /// `(brief: TaskBriefJs) => Promise<string>` returning the result's
    /// artifact ref, with a fresh task registry. Hold the resolved handle
    /// to keep accepting tasks; call `handle.stop()` before `shutdown()`.
    /// This node must be `start()`ed. (Requires the `a2a` feature.)
    ///
    /// `options.handlerTimeoutMs` bounds how long the executor may take to
    /// settle each task's Promise (default 1 hour; `0` disables) — past the
    /// deadline the task records a `Failed` terminal state instead of
    /// staying `Running` forever behind a wedged event loop.
    ///
    /// Sync setup (the `Function` is `!Send`, so the TSFN is built on the
    /// JS thread), then `spawn_future` for the registration — the SDK's
    /// `serve_rpc` spawns a response-drainer task, which needs the tokio
    /// runtime context only the future has (the `publish.rs` shape).
    #[napi(js_name = "serveA2a")]
    pub fn serve_a2a<'env>(
        &self,
        env: &'env Env,
        executor: Function<'_, TaskBriefJs, Promise<String>>,
        options: Option<ServeA2aOptions>,
    ) -> Result<PromiseRaw<'env, A2aServeHandle>> {
        let node = self.node_arc_clone()?;
        let tsfn: ExecutorTsfn = executor.build_threadsafe_function().build()?;
        let timeout = executor_timeout(options.and_then(|o| o.handler_timeout_ms));
        env.spawn_future(async move {
            let mesh = mesh_over(node, None);
            let registry = TaskRegistry::new();
            let executor: Arc<dyn TaskExecutor> = Arc::new(NodeTaskExecutor::new(tsfn, timeout));
            let handles = mesh
                .serve_a2a(registry, executor)
                .map_err(|e| a2a_err(format!("serveA2a failed: {e}")))?;
            Ok(A2aServeHandle::new(mesh, Registered::Legacy(handles)))
        })
    }

    /// **Requester side.** Hand `prompt` (+ optional Datafort `contextRefs`
    /// + routing `tags`) to the executor at `targetNodeId`; resolves the
    /// accepted task id. Rejects if the executor refused the brief. The
    /// node must already be connected to `targetNodeId`. (Requires the
    /// `a2a` feature.)
    ///
    /// `taskId` retains a caller-chosen id instead of the random one a
    /// brief mints (omit it for random). A retained id is what makes a
    /// submission idempotent on a provider that keeps durable admission
    /// records: the caller that lost a reply re-submits the *same* id and
    /// converges on the original admission instead of starting a second
    /// one.
    ///
    /// `service` + `revision` (both or neither) name a catalog entry on
    /// such a provider, and address that catalog's **free** entries only:
    /// this is the uncharged submit verb. Naming a *paid* entry is refused
    /// by the provider (payment status `0x8006`) before the executor runs,
    /// because a paid admission needs a reservation, a quote id and a signed
    /// binding: buy one through `CapabilityGateway.prepareTask` /
    /// `purchaseTask` / `submitTask`, or present one with `submitTaskPaid`.
    /// This verb does not consult `describeA2a` to refuse a paid entry
    /// client-side — pricing is the provider's, and it says so on the wire.
    /// A provider serving the legacy free path ignores both fields.
    #[napi]
    #[allow(clippy::too_many_arguments)]
    pub async fn submit_task(
        &self,
        target_node_id: BigInt,
        prompt: String,
        context_refs: Option<Vec<String>>,
        tags: Option<Vec<String>>,
        task_id: Option<String>,
        service: Option<String>,
        revision: Option<String>,
    ) -> Result<String> {
        let target = u64_arg("targetNodeId", target_node_id)?;
        let mesh = self.a2a_requester()?;
        let mut brief = TaskBrief::new(prompt)
            .with_context_refs(context_refs.unwrap_or_default())
            .with_tags(tags.unwrap_or_default());
        if let Some(task_id) = task_id {
            if task_id.is_empty() {
                return Err(a2a_err(
                    "taskId must be a non-empty string (omit it for a random id)",
                ));
            }
            brief = brief.with_task_id(task_id);
        }
        match (service, revision) {
            (Some(service), Some(revision)) => brief = brief.with_service(service, revision),
            (None, None) => {}
            // A catalog-driven provider resolves a brief by the pair, so one
            // without the other could never match an offer.
            _ => {
                return Err(a2a_err(
                    "service and revision must be given together — a \
                     catalog-driven provider resolves a brief by the pair",
                ))
            }
        }
        let ack = mesh
            .submit_task(target, &brief)
            .await
            .map_err(|e| a2a_err(format!("submitTask: {e}")))?;
        if !ack.accepted {
            return Err(a2a_err(format!(
                "executor rejected the task: {}",
                ack.reason.unwrap_or_else(|| "no reason given".to_string())
            )));
        }
        Ok(ack.task_id)
    }

    /// **Requester side.** The executor's status record for `taskId` as a
    /// JSON string (`{brief, state, updated_at}`), or `null` if the
    /// executor doesn't know it. (Requires the `a2a` feature.)
    #[napi]
    pub async fn task_status(
        &self,
        target_node_id: BigInt,
        task_id: String,
    ) -> Result<Option<String>> {
        let target = u64_arg("targetNodeId", target_node_id)?;
        let mesh = self.a2a_requester()?;
        let record = mesh
            .task_status(target, &task_id)
            .await
            .map_err(|e| a2a_err(format!("taskStatus: {e}")))?;
        match record {
            Some(rec) => Ok(Some(
                String::from_utf8(rec.encode())
                    .map_err(|e| a2a_err(format!("encode record: {e}")))?,
            )),
            None => Ok(None),
        }
    }

    /// **Requester side.** Cancel `taskId` on the executor; resolves
    /// whether it was in flight. The executor's select observes the token
    /// and the task state flips to `cancelled` — the JS handler's eventual
    /// result is discarded (see the module docs on one-sided
    /// cancellation). (Requires the `a2a` feature.)
    #[napi]
    pub async fn cancel_task(&self, target_node_id: BigInt, task_id: String) -> Result<bool> {
        let target = u64_arg("targetNodeId", target_node_id)?;
        let mesh = self.a2a_requester()?;
        mesh.cancel_task(target, &task_id)
            .await
            .map_err(|e| a2a_err(format!("cancelTask: {e}")))
    }

    /// **Requester side.** What `targetNodeId` serves, as a JSON array of
    /// `A2aOffer`s — one per catalog entry, with bounds, retention terms and,
    /// for a paid entry, its `net.pricing.terms@1`. Uncharged; the only
    /// sanctioned way to learn a price. A provider on the legacy free path
    /// (`serveA2a`) has no describe service and rejects. The bounds are u64:
    /// read them with `a2aU64` where exactness matters.
    #[napi(js_name = "describeA2a")]
    pub async fn describe_a2a(&self, target_node_id: BigInt) -> Result<String> {
        let target = u64_arg("targetNodeId", target_node_id)?;
        let mesh = self.a2a_requester()?;
        let offers = mesh
            .describe_a2a(target)
            .await
            .map_err(|e| a2a_err(format!("describeA2a: {e}")))?;
        serde_json::to_string(&offers).map_err(|e| a2a_err(format!("encode offers: {e}")))
    }

    /// **Requester side, raw.** Submit a prepared task with its payment
    /// proof: the brief from `preparedJson`, the quote id and binding
    /// signature from `proofJson`, to the prepared provider. Keeps no
    /// records — `CapabilityGateway.submitTask` is the durable verb. Safe to
    /// resend.
    ///
    /// Pass both documents exactly as `a2aDocument` extracted them (never
    /// through `JSON.parse` / `JSON.stringify`, which rounds the u64
    /// provider node). Resolves the accepted task id. A payment or admission
    /// refusal rejects with `a2a:payment_refused:` followed by
    /// `{"message", "schematic"}` (`classifyError` turns it into
    /// `PaymentRefusedError`); an over-long brief or an unpresentable proof
    /// rejects `a2a:invalid_argument:` before any packet.
    #[napi(js_name = "submitTaskPaid")]
    pub async fn submit_task_paid(
        &self,
        prepared_json: String,
        proof_json: String,
    ) -> Result<String> {
        let prepared: PreparedTask = serde_json::from_str(&prepared_json).map_err(|e| {
            invalid(format!(
                "preparedJson is not a PreparedTask document (pass \
                 a2aDocument(prepareEnvelope, '/prepared') verbatim): {e}"
            ))
        })?;
        let proof: TaskPaymentProof = serde_json::from_str(&proof_json).map_err(|e| {
            invalid(format!(
                "proofJson is not a TaskPaymentProof document (pass \
                 a2aDocument(purchaseEnvelope, '/proof') verbatim): {e}"
            ))
        })?;
        let mesh = self.a2a_requester()?;
        let ack = mesh
            .submit_task_paid(&prepared, &proof)
            .await
            .map_err(paid_submit_err)?;
        if !ack.accepted {
            return Err(a2a_err(format!(
                "executor rejected the task: {}",
                ack.reason.unwrap_or_else(|| "no reason given".to_string())
            )));
        }
        Ok(ack.task_id)
    }
}

// Its own `#[napi] impl` block, cfg'd as a whole: napi-derive registers every
// method of a block, so a per-method `#[cfg]` leaves a dangling registration
// in builds without `org`.
#[cfg(feature = "org")]
#[napi]
impl NetMesh {
    /// Install (or clear with `null`) the organization identity the A2A
    /// requester verbs on **this mesh** present to a PROTECTED provider — one
    /// serving its catalog under `principal: "same_org"` or `"granted"`.
    /// Applies to `describeA2a`, `submitTask`, `submitTaskPaid`,
    /// `taskStatus` and `cancelTask` from the next call on.
    ///
    /// A `CapabilityGateway` has its own slot (`gateway.setA2aOrgCaller`) for
    /// the prepare → purchase → submit lifecycle it composes; setting one does
    /// not set the other. Rejects with `org:credentials:closed` for a closed
    /// client.
    #[napi(js_name = "setA2aOrgCaller")]
    pub fn set_a2a_org_caller(&self, org_client: Option<&crate::org::OrgClient>) -> Result<()> {
        let installed = match org_client {
            Some(client) => Some(client.shared().ok_or_else(|| {
                Error::from_reason("org:credentials:closed: this OrgClient has been closed")
            })?),
            None => None,
        };
        self.a2a_org_caller_slot().lock().install(installed);
        Ok(())
    }
}
