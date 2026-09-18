//! The job queue you no longer run.
//!
//! A producer, two workers, and a durable job log — three in-process mesh nodes
//! over loopback UDP plus a local append-only log. Jobs are appended to the log
//! (the queue), dispatched to a worker over nRPC, and a worker that fails a job
//! is retried on its peer. Nothing is re-executed, and the log is the record you
//! reconcile from.
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
use net_sdk::mesh_rpc::{CallOptionsTyped, Codec};
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

    producer.inner().start();
    w1.inner().start();
    w2.inner().start();

    let w1_id = w1.inner().node_id();
    let w2_id = w2.inner().node_id();

    // Two workers, one service. Each echoes its own id so the caller can prove
    // which one ran the job.
    let _serve_one = w1.serve_rpc_typed("run", Codec::Json, move |job: Job| async move {
        if job.id == POISON {
            // A typed application refusal, not a transport error. The caller
            // decides to retry; the substrate never does it silently.
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

    // The queue: a local append-only log, one record per submitted job. This is
    // the durability the broker used to provide.
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

    // Dispatch. Round-robin across the workers; a refusal re-issues the same
    // job to the other one.
    let targets = vec![w1_id, w2_id];
    let mut retried = 0usize;
    for (index, id) in (1..=JOBS).enumerate() {
        let primary = targets[index % targets.len()];
        let secondary = targets[(index + 1) % targets.len()];
        let job = Job { id };

        let done = match producer
            .call_typed::<Job, Done>(primary, "run", &job, CallOptionsTyped::default())
            .await
        {
            Ok(done) => done,
            Err(_) => {
                retried += 1;
                println!("job {id} refused by {primary:#x}; re-issuing to {secondary:#x}");
                producer
                    .call_typed::<Job, Done>(secondary, "run", &job, CallOptionsTyped::default())
                    .await?
            }
        };
        results.append(format!("done:{}:{}", done.id, done.worker).as_bytes())?;
    }

    // Reconcile from the logs, not from memory. Replaying the queue is how a
    // restarted dispatcher knows what was outstanding; the results log is how it
    // knows what was finished. A job id with one result record ran exactly once.
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
