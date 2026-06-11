//! Cross-binary wire-interop smoke for the packet AEAD.
//!
//! Two processes — typically built from DIFFERENT commits (e.g. the
//! pre-ring RustCrypto cipher vs the ring-backed cipher of
//! 5289be265) — handshake over loopback UDP and exchange reliable
//! events in both directions. If the AEAD implementations disagree
//! on a single wire byte, the session dies (heartbeat verify /
//! event decrypt fail) and the smoke times out instead of printing
//! `INTEROP_OK`.
//!
//! Usage (start the responder first):
//!
//!   cargo build --example interop_smoke
//!   target/debug/examples/interop_smoke responder 127.0.0.1:47801 127.0.0.1:47802
//!   target/debug/examples/interop_smoke initiator 127.0.0.1:47802 127.0.0.1:47801
//!
//! Both processes print `INTEROP_OK <role>` and exit 0 on success;
//! they exit 1 on timeout/failure. Key material is FIXED test-only
//! data (PSK 0x42*32, responder x25519 private 0x07*32) so the two
//! binaries need no out-of-band exchange — never reuse it outside
//! this smoke.

use std::net::SocketAddr;
use std::time::Duration;

use net::adapter::net::{NetAdapter, NetAdapterConfig, ReliabilityConfig, StaticKeypair};
use net::adapter::Adapter;
use net::event::{batch_process_nonce, Batch, InternalEvent};

const PSK: [u8; 32] = [0x42u8; 32];
const RESPONDER_PRIVATE: [u8; 32] = [0x07u8; 32];
const EVENT_COUNT: usize = 8;
const DEADLINE: Duration = Duration::from_secs(20);

/// Deterministic responder keypair: both binaries derive the same
/// x25519 public from the fixed private (RFC 7748), so the
/// initiator can pin it without an exchange step.
fn responder_keypair() -> StaticKeypair {
    let secret = x25519_dalek::StaticSecret::from(RESPONDER_PRIVATE);
    let public = x25519_dalek::PublicKey::from(&secret);
    StaticKeypair::from_keys(RESPONDER_PRIVATE, *public.as_bytes())
}

fn batch_of(count: usize, tag: &str) -> Batch {
    let events: Vec<InternalEvent> = (0..count)
        .map(|i| {
            let json = serde_json::json!({ "interop": tag, "i": i });
            InternalEvent::from_value(json, i as u64, 0)
        })
        .collect();
    Batch {
        shard_id: 0,
        events,
        sequence_start: 0,
        process_nonce: batch_process_nonce(),
    }
}

/// Poll shard 0 until `count` events arrived or the deadline passes.
async fn await_events(adapter: &NetAdapter, count: usize) -> usize {
    let start = std::time::Instant::now();
    let mut seen = 0usize;
    let mut cursor: Option<String> = None;
    while start.elapsed() < DEADLINE {
        match adapter.poll_shard(0, cursor.as_deref(), 100).await {
            Ok(result) => {
                seen += result.events.len();
                cursor = result.next_id;
                if seen >= count {
                    return seen;
                }
            }
            Err(e) => eprintln!("poll error: {e}"),
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    seen
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let usage = "usage: interop_smoke <responder|initiator> <bind_addr> <peer_addr>";
    let (role, bind, peer) = match (args.get(1), args.get(2), args.get(3)) {
        (Some(r), Some(b), Some(p)) => (r.as_str(), b, p),
        _ => {
            eprintln!("{usage}");
            std::process::exit(2);
        }
    };
    let bind_addr: SocketAddr = bind.parse().expect("bind addr");
    let peer_addr: SocketAddr = peer.parse().expect("peer addr");

    let mut config = match role {
        "responder" => NetAdapterConfig::responder(bind_addr, peer_addr, PSK, responder_keypair()),
        "initiator" => {
            NetAdapterConfig::initiator(bind_addr, peer_addr, PSK, responder_keypair().public)
        }
        _ => {
            eprintln!("{usage}");
            std::process::exit(2);
        }
    }
    .with_handshake(10, Duration::from_secs(2))
    .with_heartbeat_interval(Duration::from_millis(500))
    .with_session_timeout(Duration::from_secs(10));
    config.default_reliability = ReliabilityConfig::Light;

    let mut adapter = NetAdapter::new(config).expect("adapter construction");
    adapter.init().await.expect("handshake failed");
    eprintln!("[{role}] handshake complete");

    match role {
        "responder" => {
            // Receive the initiator's batch, then answer with our
            // own — proving seal+open in BOTH directions across the
            // two binaries.
            let got = await_events(&adapter, EVENT_COUNT).await;
            if got < EVENT_COUNT {
                eprintln!("[responder] FAIL: got {got}/{EVENT_COUNT} events");
                std::process::exit(1);
            }
            adapter
                .on_batch(std::sync::Arc::new(batch_of(EVENT_COUNT, "responder")))
                .await
                .expect("responder send failed");
            // Let retransmits/acks drain before tearing down.
            tokio::time::sleep(Duration::from_millis(750)).await;
            adapter.shutdown().await.ok();
            println!("INTEROP_OK responder");
        }
        _ => {
            // Initiator: send, then await the responder's answer.
            tokio::time::sleep(Duration::from_millis(250)).await;
            adapter
                .on_batch(std::sync::Arc::new(batch_of(EVENT_COUNT, "initiator")))
                .await
                .expect("initiator send failed");
            let got = await_events(&adapter, EVENT_COUNT).await;
            adapter.shutdown().await.ok();
            if got < EVENT_COUNT {
                eprintln!("[initiator] FAIL: got {got}/{EVENT_COUNT} events");
                std::process::exit(1);
            }
            println!("INTEROP_OK initiator");
        }
    }
}
