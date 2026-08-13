//! AUTH — A2A task ownership over the real wire.
//!
//! A task id is a **name, not a bearer capability**: it is
//! client-generated, it travels through logs and polling loops as an
//! ordinary identifier, and a `TaskRecord` carries the complete prompt
//! and context refs. Learning one must not confer the right to read the
//! brief or stop the work.
//!
//! Submission stays open to every in-root peer — that is the design.
//! Inspection and cancellation are bound to the authenticated submitter.

#![cfg(all(feature = "net", feature = "cortex"))]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use net_sdk::a2a::{CancelToken, TaskBrief, TaskExecutor, TaskRegistry, TaskState};
use net_sdk::mesh::{Mesh, MeshBuilder};

const PSK: [u8; 32] = [0x5Au8; 32];

async fn mesh() -> Mesh {
    MeshBuilder::new("127.0.0.1:0", &PSK)
        .expect("builder")
        .build()
        .await
        .expect("build")
}

/// Handshake every caller to `executor`, then start all dispatch
/// loops.
///
/// `accept()` has to happen for every peer before `start()` — the
/// dispatch loop would otherwise race the responder handshake — so the
/// accepts are batched here rather than done per caller.
async fn connect_all(executor: &Mesh, callers: &[&Mesh]) {
    let addr = executor.inner().local_addr();
    let pubkey = *executor.inner().public_key();
    let nid_exec = executor.inner().node_id();
    for caller in callers {
        let nid_caller = caller.inner().node_id();
        let (accepted, connected) = tokio::join!(executor.inner().accept(nid_caller), async {
            tokio::time::sleep(Duration::from_millis(50)).await;
            caller.inner().connect(addr, &pubkey, nid_exec).await
        });
        accepted.expect("accept");
        connected.expect("connect");
    }
    for caller in callers {
        caller.inner().start();
    }
    executor.inner().start();
}

/// Runs until cancelled, so the RED has something live to try to stop.
struct Grinder {
    saw_cancel: Arc<AtomicBool>,
}

#[async_trait::async_trait]
impl TaskExecutor for Grinder {
    async fn run(&self, _brief: TaskBrief, cancel: CancelToken) -> Result<String, String> {
        cancel.cancelled().await;
        self.saw_cancel.store(true, Ordering::SeqCst);
        Err("cancelled".to_string())
    }
}

/// The three-peer RED. A submits to the executor; C learns the task id
/// (as it would from a log line or a shared dashboard) and tries to
/// read the brief and cancel the work.
///
/// Before ownership binding, both succeeded: status and cancel keyed on
/// the caller-supplied id alone and the registry stored no submitter.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_third_peer_cannot_read_or_cancel_another_submitters_task() {
    let executor_mesh = mesh().await;
    let alice = mesh().await;
    let carol = mesh().await;

    let saw_cancel = Arc::new(AtomicBool::new(false));
    let registry = TaskRegistry::new();
    let _handles = executor_mesh
        .serve_a2a(
            registry.clone(),
            Arc::new(Grinder {
                saw_cancel: Arc::clone(&saw_cancel),
            }),
        )
        .expect("serve a2a");

    connect_all(&executor_mesh, &[&alice, &carol]).await;
    let target = executor_mesh.inner().node_id();

    // Alice submits. The brief carries exactly the material that must
    // not leak: the prompt and the context refs.
    let brief = TaskBrief::new("summarize the incident postmortem")
        .with_context_refs(vec!["blob://private-postmortem".to_string()]);
    let ack = alice
        .submit_task(target, &brief)
        .await
        .expect("alice submits");
    assert!(ack.accepted, "submission is open to any in-root peer");
    let task_id = ack.task_id.clone();

    // Carol learns the id — a log line, a dashboard, a shared channel.
    // 1. She must not be able to read the brief.
    let leaked = carol
        .task_status(target, &task_id)
        .await
        .expect("status call itself succeeds");
    assert!(
        leaked.is_none(),
        "a third peer read another submitter's task record — prompt and context \
         refs included: {leaked:?}"
    );

    // 2. She must not be able to stop the work.
    let cancelled = carol
        .cancel_task(target, &task_id)
        .await
        .expect("cancel call itself succeeds");
    assert!(
        !cancelled,
        "a third peer cancelled another submitter's task"
    );
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        !saw_cancel.load(Ordering::SeqCst),
        "the executor observed a cancel it should never have been asked for"
    );

    // 3. Alice keeps full access — the point is ownership, not a
    //    blanket refusal. A gate that denied everyone would satisfy
    //    both assertions above and break A2A.
    let mine = alice
        .task_status(target, &task_id)
        .await
        .expect("alice status");
    let mine = mine.expect("the submitter can read her own task");
    assert_eq!(mine.brief.prompt, brief.prompt);

    let stopped = alice
        .cancel_task(target, &task_id)
        .await
        .expect("alice cancel");
    assert!(stopped, "the submitter could not cancel her own task");

    // And it actually stopped.
    for _ in 0..100 {
        if let Some(rec) = alice
            .task_status(target, &task_id)
            .await
            .expect("poll status")
        {
            if rec.state.is_terminal() {
                assert_eq!(rec.state, TaskState::Cancelled);
                return;
            }
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the owner's cancel never drove the task terminal");
}

/// Two submitters may use the same task id without interfering.
///
/// Ids are client-generated, so a collision is plausible — `"task-1"`
/// is not a stretch. Storing one owner per id would make the second
/// submitter's request fail, and would let a peer squat obvious ids to
/// deny others. Keying on the pair removes the interaction.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_submitters_may_use_the_same_task_id() {
    let executor_mesh = mesh().await;
    let alice = mesh().await;
    let carol = mesh().await;

    let registry = TaskRegistry::new();
    let _handles = executor_mesh
        .serve_a2a(
            registry.clone(),
            Arc::new(Grinder {
                saw_cancel: Arc::new(AtomicBool::new(false)),
            }),
        )
        .expect("serve a2a");

    connect_all(&executor_mesh, &[&alice, &carol]).await;
    let target = executor_mesh.inner().node_id();

    let mut alice_brief = TaskBrief::new("alice's work");
    alice_brief.task_id = "task-1".to_string();
    let mut carol_brief = TaskBrief::new("carol's work");
    carol_brief.task_id = "task-1".to_string();

    let a_ack = alice
        .submit_task(target, &alice_brief)
        .await
        .expect("alice submits");
    assert!(a_ack.accepted);
    let c_ack = carol
        .submit_task(target, &carol_brief)
        .await
        .expect("carol submits");
    assert!(
        c_ack.accepted,
        "a colliding task id from a different submitter was refused: {:?}",
        c_ack.reason
    );

    // Each sees only their own brief under that shared id.
    let a_rec = alice
        .task_status(target, "task-1")
        .await
        .expect("status")
        .expect("alice's record");
    assert_eq!(a_rec.brief.prompt, "alice's work");
    let c_rec = carol
        .task_status(target, "task-1")
        .await
        .expect("status")
        .expect("carol's record");
    assert_eq!(
        c_rec.brief.prompt, "carol's work",
        "the shared id resolved to the other submitter's task"
    );
}

/// Re-submitting an id with a *different* brief is refused rather than
/// silently answering with the existing task.
///
/// A silent hand-back is the dangerous shape: the submitter would poll
/// a task that is not the one they described and read its result as the
/// answer to this request.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reusing_an_id_for_different_work_is_refused() {
    let executor_mesh = mesh().await;
    let alice = mesh().await;

    let registry = TaskRegistry::new();
    let _handles = executor_mesh
        .serve_a2a(
            registry.clone(),
            Arc::new(Grinder {
                saw_cancel: Arc::new(AtomicBool::new(false)),
            }),
        )
        .expect("serve a2a");

    connect_all(&executor_mesh, &[&alice]).await;
    let target = executor_mesh.inner().node_id();

    let mut first = TaskBrief::new("the work I asked for");
    first.task_id = "shared-id".to_string();
    let ack = alice.submit_task(target, &first).await.expect("submit");
    assert!(ack.accepted);

    // Identical re-submit: idempotent, same id back (the nRPC
    // retransmit case).
    let again = alice.submit_task(target, &first).await.expect("resubmit");
    assert!(
        again.accepted,
        "an identical re-submit must stay idempotent"
    );
    assert_eq!(again.task_id, "shared-id");

    // Different brief, same id: refused.
    let mut second = TaskBrief::new("completely different work");
    second.task_id = "shared-id".to_string();
    let refused = alice.submit_task(target, &second).await.expect("submit");
    assert!(
        !refused.accepted,
        "an id reused for different work was accepted, so the submitter would \
         poll a task that is not the one they described"
    );
    assert!(
        refused
            .reason
            .as_deref()
            .unwrap_or_default()
            .contains("already names a different brief"),
        "the refusal should say what happened; got {:?}",
        refused.reason
    );

    // The original is untouched.
    let rec = alice
        .task_status(target, "shared-id")
        .await
        .expect("status")
        .expect("record");
    assert_eq!(rec.brief.prompt, "the work I asked for");
}
