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

use net_sdk::a2a::{CancelToken, TaskBrief, TaskExecutor, TaskRegistry};
use net_sdk::mesh::Mesh as SdkMesh;
use net_sdk::mesh_rpc::ServeHandle;

use crate::delegation::u64_arg;
use crate::enrollment::mesh_over;
use crate::NetMesh;

fn a2a_err(msg: impl std::fmt::Display) -> Error {
    Error::from_reason(format!("a2a: {msg}"))
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
}

/// The bridged JS task executor:
/// `(brief: TaskBriefJs) => Promise<string>` resolving to the result's
/// artifact ref.
type ExecutorTsfn = ThreadsafeFunction<TaskBriefJs, Promise<String>, TaskBriefJs, Status, false>;

/// A [`TaskExecutor`] backed by a JS **async** callback. A mesh-side cancel
/// trips the select below; the registry records `Cancelled` and the JS
/// handler's result (if it ever resolves) is discarded.
struct NodeTaskExecutor {
    callback: ExecutorTsfn,
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
                Err(e) => Err(format!("a2a task handler Promise rejected: {e}")),
            }
        };

        // `biased` polls the JS result first so an already-resolved result
        // beats a simultaneous cancel. On cancel the future drops — the JS
        // work itself cannot be aborted (see the module docs), but its
        // result is discarded and the registry records `Cancelled`.
        tokio::select! {
            biased;
            r = js_result => r,
            _ = cancel.cancelled() => Err("cancelled".to_string()),
        }
    }
}

/// Keeps the served A2A services alive (returned by `NetMesh.serveA2a`).
/// Dropping it or calling [`stop`](Self::stop) unregisters them.
// `js_name` pinned: napi's auto-camelCase would emit `A2AServeHandle` /
// `serveA2A`; the plan + Python parity spell the surface `A2aServeHandle`
// / `serveA2a`.
#[napi(js_name = "A2aServeHandle")]
pub struct A2aServeHandle {
    // The `Mesh` holds the channel registry the services registered against,
    // and each `ServeHandle` one dispatcher registration (task/status/
    // cancel). A `parking_lot::Mutex` because napi hands out `&self`; a
    // `#[napi]` class is GC-finalized, not scope-dropped, so `stop()` is the
    // deterministic release (the `close()` gotcha in `bindings.md`).
    inner: Mutex<Option<(SdkMesh, Vec<ServeHandle>)>>,
}

#[napi]
impl A2aServeHandle {
    /// Stop accepting A2A tasks (unregister the services). Idempotent.
    #[napi]
    pub fn stop(&self) {
        let _ = self.inner.lock().take();
    }

    /// Whether the services are still registered.
    #[napi(getter)]
    pub fn serving(&self) -> bool {
        self.inner.lock().is_some()
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
    /// Sync setup (the `Function` is `!Send`, so the TSFN is built on the
    /// JS thread), then `spawn_future` for the registration — the SDK's
    /// `serve_rpc` spawns a response-drainer task, which needs the tokio
    /// runtime context only the future has (the `publish.rs` shape).
    #[napi(js_name = "serveA2a")]
    pub fn serve_a2a<'env>(
        &self,
        env: &'env Env,
        executor: Function<'_, TaskBriefJs, Promise<String>>,
    ) -> Result<PromiseRaw<'env, A2aServeHandle>> {
        let node = self.node_arc_clone()?;
        let tsfn: ExecutorTsfn = executor.build_threadsafe_function().build()?;
        env.spawn_future(async move {
            let mesh = mesh_over(node, None);
            let registry = TaskRegistry::new();
            let executor: Arc<dyn TaskExecutor> = Arc::new(NodeTaskExecutor { callback: tsfn });
            let handles = mesh
                .serve_a2a(registry, executor)
                .map_err(|e| a2a_err(format!("serveA2a failed: {e}")))?;
            Ok(A2aServeHandle {
                inner: Mutex::new(Some((mesh, handles))),
            })
        })
    }

    /// **Requester side.** Hand `prompt` (+ optional Datafort `contextRefs`
    /// + routing `tags`) to the executor at `targetNodeId`; resolves the
    /// accepted task id. Rejects if the executor refused the brief. The
    /// node must already be connected to `targetNodeId`. (Requires the
    /// `a2a` feature.)
    #[napi]
    pub async fn submit_task(
        &self,
        target_node_id: BigInt,
        prompt: String,
        context_refs: Option<Vec<String>>,
        tags: Option<Vec<String>>,
    ) -> Result<String> {
        let target = u64_arg("targetNodeId", target_node_id)?;
        let mesh = mesh_over(self.node_arc_clone()?, None);
        let brief = TaskBrief::new(prompt)
            .with_context_refs(context_refs.unwrap_or_default())
            .with_tags(tags.unwrap_or_default());
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
        let mesh = mesh_over(self.node_arc_clone()?, None);
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
        let mesh = mesh_over(self.node_arc_clone()?, None);
        mesh.cancel_task(target, &task_id)
            .await
            .map_err(|e| a2a_err(format!("cancelTask: {e}")))
    }
}
