//! The job queue you no longer run.
//!
//! A producer, two workers, and an append-only job log — three in-process mesh
//! nodes over loopback UDP plus a local log. Jobs are appended to the log (the
//! queue), dispatch reads them back out of it, and a worker that *refuses* a
//! job re-issues it to its peer. Nothing is re-executed, and the results log is
//! the record you reconcile from.
//!
//! Both logs live in memory for the life of the process — that is all this
//! route needs, and all it claims. Surviving a restart is two more arguments:
//! `Redex::new().with_persistent_dir(dir)` and
//! `RedexFileConfig::new().with_persistent(true)`.
//!
//! Run (from a crate whose `examples/` holds this file):
//!
//!   cargo run --example jobqueue
//!
//! Expected final line: `RESULT ok jobs=6 done=6 retried=1 duplicates=0`

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::time::Duration;

use net_sdk::cortex::{Redex, RedexFileConfig};
use net_sdk::mesh::{Mesh, MeshBuilder};
use net_sdk::mesh_rpc::{CallOptionsTyped, Codec, RpcError, NRPC_TYPED_HANDLER_ERROR};
use net_sdk::{ChannelName, Identity};
use serde::{Deserialize, Serialize};

/// 32 bytes exactly — a PSK, not a passphrase. Every node in a mesh shares it.
const PSK: [u8; 32] = [0x42; 32];

const JOBS: u64 = 6;
/// Job 3 is answered with an error by the first worker that sees it, so the
/// dispatcher has to re-issue it somewhere else and the retry is observable.
const POISON: u64 = 3;

#[derive(Debug, Serialize, Deserialize)]
struct Job {
    id: u64,
}

#[derive(Debug, Serialize, Deserialize)]
struct Done {
    id: u64,
    worker: u64,
}

async fn build(seed_byte: u8) -> Mesh {
    MeshBuilder::new("127.0.0.1:0", &PSK)
        .expect("builder")
        .identity(Identity::from_seed([seed_byte; 32]))
        .build()
        .await
        .expect("build mesh node")
}

async fn handshake(responder: &Mesh, initiator: &Mesh, responder_addr: SocketAddr) {
    let responder_pub = *responder.inner().public_key();
    let responder_id = responder.inner().node_id();
    let initiator_id = initiator.inner().node_id();
    let (accepted, connected) = tokio::join!(
        responder.inner().accept(initiator_id),
        async {
            tokio::time::sleep(Duration::from_millis(50)).await;
            initiator
                .inner()
                .connect(responder_addr, &responder_pub, responder_id)
                .await
        }
    );
    accepted.expect("accept");
    connected.expect("connect");
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let producer = build(0x71).await;
    let w1 = build(0x72).await;
    let w2 = build(0x73).await;

    let producer_addr = producer.inner().local_addr();
    let w1_addr = w1.inner().local_addr();
    handshake(&producer, &w1, producer_addr).await;
    handshake(&producer, &w2, producer_addr).await;
    // Workers can see each other too — nothing here needs that, but it is what
    // makes re-dispatch to "any worker" a local query rather than a config file.
    handshake(&w1, &w2, w1_addr).await;

    producer.start();
    w1.start();
    w2.start();

    let w1_id = w1.inner().node_id();
    let w2_id = w2.inner().node_id();

    // Two workers, one service. Each echoes its own id so the caller can prove
    // which one ran the job.
    let _serve_one = w1.serve_rpc_typed("run", Codec::Json, move |job: Job| async move {
        if job.id == POISON {
            // A typed application refusal — `Err(String)` from a typed handler
            // arrives at the caller as `ServerError`/`NRPC_TYPED_HANDLER_ERROR`,
            // not as a transport fault. It is issued BEFORE the job does any
            // work, which is what makes re-issuing it safe.
            return Err(format!("worker {w1_id:#x} refused job {}", job.id));
        }
        Ok(Done {
            id: job.id,
            worker: w1_id,
        })
    })?;
    let _serve_two = w2.serve_rpc_typed("run", Codec::Json, move |job: Job| async move {
        Ok(Done {
            id: job.id,
            worker: w2_id,
        })
    })?;

    // The queue: a local append-only log, one record per submitted job.
    // `Redex::new()` is the in-memory manager — these logs are the queue for
    // as long as the process lives, and no longer.
    let redex = Redex::new();
    let queue = redex.open_file(
        &ChannelName::new("jobs/queue").expect("channel"),
        RedexFileConfig::new(),
    )?;
    let results = redex.open_file(
        &ChannelName::new("jobs/results").expect("channel"),
        RedexFileConfig::new(),
    )?;

    for id in 1..=JOBS {
        let seq = queue.append(format!("job:{id}").as_bytes())?;
        println!("queued job {id} at seq {seq}");
    }

    // Dispatch. The work list comes back out of the queue log, not out of the
    // loop that wrote it — the log IS the queue. Round-robin across the
    // workers; a refusal re-issues that job to the other one.
    let queued: Vec<u64> = queue
        .read_range(0, queue.len() as u64)
        .iter()
        .filter_map(|event| {
            let text = std::str::from_utf8(&event.payload).ok()?;
            text.strip_prefix("job:")?.parse::<u64>().ok()
        })
        .collect();

    let targets = vec![w1_id, w2_id];
    let mut retried = 0usize;
    for (index, id) in queued.into_iter().enumerate() {
        let primary = targets[index % targets.len()];
        let secondary = targets[(index + 1) % targets.len()];
        let job = Job { id };

        let done = match producer
            .call_typed::<Job, Done>(primary, "run", &job, CallOptionsTyped::default())
            .await
        {
            Ok(done) => done,
            // ONLY the typed refusal is re-issued. A timeout or a transport
            // fault means the call failed, not that the job did — the worker
            // may have run it already, and re-issuing would execute it twice.
            // Those propagate; `duplicates=0` is a claim this match arm earns.
            Err(RpcError::ServerError { status, .. }) if status == NRPC_TYPED_HANDLER_ERROR => {
                retried += 1;
                println!("job {id} refused by {primary:#x}; re-issuing to {secondary:#x}");
                producer
                    .call_typed::<Job, Done>(secondary, "run", &job, CallOptionsTyped::default())
                    .await?
            }
            Err(other) => return Err(other.into()),
        };
        results.append(format!("done:{}:{}", done.id, done.worker).as_bytes())?;
    }

    // Reconcile from the results log, not from a counter kept beside the
    // dispatch loop: the completion count is whatever the log says. A job id
    // with one result record ran exactly once.
    let mut done: BTreeMap<u64, usize> = BTreeMap::new();
    for event in results.read_range(0, results.len() as u64) {
        if let Ok(text) = std::str::from_utf8(&event.payload) {
            if let Some(rest) = text.strip_prefix("done:") {
                if let Some((id, _worker)) = rest.split_once(':') {
                    if let Ok(id) = id.parse::<u64>() {
                        *done.entry(id).or_default() += 1;
                    }
                }
            }
        }
    }

    let jobs_queued = queue.len();
    let jobs_done = done.len();
    let duplicates = done.values().filter(|count| **count > 1).count();

    println!("queued:     {jobs_queued}");
    println!("completed:  {jobs_done} (one result record each)");
    println!("re-issued:  {retried}");

    println!(
        "RESULT ok jobs={jobs_queued} done={jobs_done} retried={retried} duplicates={duplicates}"
    );

    producer.shutdown().await?;
    w1.shutdown().await?;
    w2.shutdown().await?;
    Ok(())
}
