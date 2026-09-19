//! The event log you no longer run.
//!
//! One node, one local append-only log. Eight records are appended, replayed
//! from the start, then replayed again from a consumer checkpoint — and the
//! second replay is three records, not eight, because the offset is the
//! consumer's own bookkeeping and the log is not asked to remember it.
//!
//! Kafka needs a cluster for this. `RedexFile` is one file with a monotonic
//! sequence, and the consumer's position lives in the consumer.
//!
//! Run (from a crate whose `examples/` holds this file):
//!
//!   cargo run --example eventlog
//!
//! Expected final line: `RESULT ok records=8 replayed=8 resumed=3`

use net_sdk::cortex::{Redex, RedexFileConfig};
use net_sdk::ChannelName;

/// Records appended to the log.
const RECORDS: usize = 8;
/// The consumer's checkpoint: the next sequence it has *not* processed.
/// Records 0..=4 are done, so it resumes at 5.
const CHECKPOINT: u64 = 5;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let redex = Redex::new();
    let log = redex.open_file(
        &ChannelName::new("audit/events").expect("channel name"),
        RedexFileConfig::new(),
    )?;

    for index in 0..RECORDS {
        let seq = log.append(format!("event:{index}").as_bytes())?;
        println!("appended event {index} at seq {seq}");
    }

    // Full replay. Every record, in append order — the log does not need a
    // broker to hand them back.
    let all = log.read_range(0, log.len() as u64);

    // The consumer's checkpoint is application state: it holds the next
    // sequence the consumer has not processed, and the consumer reads from
    // there. `read_range` is half-open — `[start, end)` — so the checkpoint
    // is the start bound verbatim: no `+ 1`, and no record replayed twice.
    // Nothing is committed on the log's side, which is why a second consumer
    // with a different checkpoint costs the log nothing.
    let resumed = log.read_range(CHECKPOINT, log.len() as u64);

    // Replay is a read, not a transformation: the same range yields the same
    // sequence, so a fold over it is reproducible.
    let again = log.read_range(0, log.len() as u64);
    let stable = again
        .iter()
        .map(|e| e.entry.seq)
        .eq(all.iter().map(|e| e.entry.seq));

    println!("records in the log: {}", log.len());
    println!("replayed from 0:    {}", all.len());
    println!("resumed from {CHECKPOINT}:     {}", resumed.len());
    println!("replay is stable:   {stable}");

    println!(
        "RESULT ok records={} replayed={} resumed={}",
        log.len(),
        all.len(),
        resumed.len()
    );

    log.close()?;
    Ok(())
}
