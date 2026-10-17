// SPDX-License-Identifier: MIT OR Apache-2.0
//! The Rust driver for the cross-language org matrix
//! (`tests/cross_lang_org/matrix/PROTOCOL.md`): one process plays the caller
//! or the provider of a cross-org Granted call, `unary` or
//! `server-streaming`, over a scenario from `gen_org_scenario`.
//!
//! ```text
//! cargo build -p net-mesh-sdk --features net,cortex --example org_matrix_peer
//! org_matrix_peer provider --manifest <dir> --shape unary --caller-node-id <id>
//! org_matrix_peer caller   --manifest <dir> --shape server-streaming
//! ```
//!
//! stdout carries only protocol lines; diagnostics go to stderr.

use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;
use std::time::{Duration, Instant};

use bytes::Bytes;
use futures::StreamExt;
use net_sdk::identity::Identity;
use net_sdk::org::{OrgAccess, OrgCaller, OrgCredentials, OrgSdkError};
use net_sdk::{Mesh, MeshBuilder};

const WATCHDOG: Duration = Duration::from_secs(120);
const DISCOVERY_BUDGET: Duration = Duration::from_secs(60);

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn hex_decode(s: &str) -> Result<Vec<u8>, ()> {
    if !s.len().is_multiple_of(2) {
        return Err(());
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2).ok_or(())?, 16).map_err(|_| ()))
        .collect()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Shape {
    Unary,
    ServerStreaming,
}

struct Args {
    role: String,
    manifest: PathBuf,
    shape: Shape,
    caller_node_id: Option<u64>,
}

fn usage(msg: &str) -> ! {
    eprintln!("org_matrix_peer: {msg}");
    eprintln!(
        "usage: org_matrix_peer (provider --manifest DIR --shape S --caller-node-id N | caller --manifest DIR --shape S)"
    );
    std::process::exit(2);
}

fn parse_args() -> Args {
    let mut it = std::env::args().skip(1);
    let role = it.next().unwrap_or_else(|| usage("missing role"));
    let (mut manifest, mut shape, mut caller_node_id) = (None, None, None);
    while let Some(flag) = it.next() {
        let value = it
            .next()
            .unwrap_or_else(|| usage(&format!("{flag} needs a value")));
        match flag.as_str() {
            "--manifest" => manifest = Some(PathBuf::from(value)),
            "--shape" => {
                shape = Some(match value.as_str() {
                    "unary" => Shape::Unary,
                    "server-streaming" => Shape::ServerStreaming,
                    other => usage(&format!("unknown shape {other}")),
                })
            }
            "--caller-node-id" => {
                caller_node_id = Some(value.parse().unwrap_or_else(|_| usage("bad node id")))
            }
            other => usage(&format!("unknown flag {other}")),
        }
    }
    let args = Args {
        role,
        manifest: manifest.unwrap_or_else(|| usage("missing --manifest")),
        shape: shape.unwrap_or_else(|| usage("missing --shape")),
        caller_node_id,
    };
    match (args.role.as_str(), args.caller_node_id) {
        ("provider", Some(_)) | ("caller", None) => args,
        ("provider", None) => usage("provider needs --caller-node-id"),
        ("caller", Some(_)) => usage("caller takes no --caller-node-id"),
        (other, _) => usage(&format!("unknown role {other}")),
    }
}

fn line(text: &str) {
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{text}");
    let _ = out.flush();
}

fn finish(ok: Result<String, String>) -> ! {
    match ok {
        Ok(tail) => {
            line(&format!("RESULT ok{tail}"));
            std::process::exit(0);
        }
        Err(detail) => {
            let detail = detail.replace('\n', " ");
            line(&format!("RESULT fail {detail}"));
            eprintln!("org_matrix_peer: RESULT fail {detail}");
            std::process::exit(1);
        }
    }
}

fn manifest(dir: &Path) -> serde_json::Value {
    let raw = std::fs::read(dir.join("manifest.json"))
        .unwrap_or_else(|e| finish(Err(format!("read manifest: {e}"))));
    serde_json::from_slice(&raw).unwrap_or_else(|e| finish(Err(format!("parse manifest: {e}"))))
}

fn field<'a>(m: &'a serde_json::Value, role: &str, key: &str) -> &'a str {
    m[role][key]
        .as_str()
        .unwrap_or_else(|| finish(Err(format!("manifest {role}.{key} missing"))))
}

fn seed(hex_seed: &str) -> [u8; 32] {
    hex_decode(hex_seed)
        .ok()
        .and_then(|b| b.try_into().ok())
        .unwrap_or_else(|| finish(Err("seed_hex is not 32 bytes".to_string())))
}

async fn build(m: &serde_json::Value, role: &str) -> Mesh {
    let psk: [u8; 32] = hex_decode(m["psk_hex"].as_str().unwrap_or_default())
        .ok()
        .and_then(|b| b.try_into().ok())
        .unwrap_or_else(|| finish(Err("psk_hex is not 32 bytes".to_string())));
    MeshBuilder::new("127.0.0.1:0", &psk)
        .unwrap_or_else(|e| finish(Err(format!("builder: {e}"))))
        .identity(Identity::from_seed(seed(field(m, role, "seed_hex"))))
        .heartbeat_ms(200)
        // Every driver uses 4 shards (the Python default): a shard-count
        // mismatch strands capability announcements on unpooled shards.
        .shards(4)
        .build()
        .await
        .unwrap_or_else(|e| finish(Err(format!("build: {e}"))))
}

fn json_eq(bytes: &[u8], want: serde_json::Value) -> bool {
    serde_json::from_slice::<serde_json::Value>(bytes).is_ok_and(|got| got == want)
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
    let args = parse_args();
    let m = manifest(&args.manifest);
    if args.role == "caller" {
        finish(run_caller(&args, &m).await)
    } else {
        finish(run_provider(&args, &m).await)
    }
}

async fn run_caller(args: &Args, m: &serde_json::Value) -> Result<String, String> {
    let mesh = build(m, "caller").await;
    line(&format!("ID {}", mesh.node_id()));
    let contact = tokio::task::spawn_blocking(|| {
        let mut buf = String::new();
        std::io::stdin().lock().read_line(&mut buf).map(|_| buf)
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| format!("stdin: {e}"))?;
    let parts: Vec<&str> = contact.split_whitespace().collect();
    let [tag, addr, key_hex, node_id] = parts.as_slice() else {
        return Err(format!("expected PROVIDER line, got {contact:?}"));
    };
    if *tag != "PROVIDER" {
        return Err(format!("expected PROVIDER line, got {contact:?}"));
    }
    let key: [u8; 32] = hex_decode(key_hex)
        .ok()
        .and_then(|b| b.try_into().ok())
        .ok_or("bad provider public key")?;
    let node_id: u64 = node_id.parse().map_err(|_| "bad provider node id")?;
    mesh.connect(addr, &key, node_id)
        .await
        .map_err(|e| format!("connect: {e}"))?;
    mesh.start().map_err(|e| format!("start: {e}"))?;
    mesh.install_org_authority(&args.manifest.join(field(m, "caller", "authority_dir")))
        .map_err(|e| format!("install authority: {e}"))?;
    let read = |key: &str| {
        std::fs::read(args.manifest.join(field(m, "caller", key)))
            .map_err(|e| format!("read caller.{key}: {e}"))
    };
    let credentials = OrgCredentials::from_parts(
        &read("membership_path")?,
        &read("dispatcher_path")?,
        &[read("grant_path")?],
        &[args.manifest.join(field(m, "caller", "grant_secret_path"))],
    )
    .map_err(|e| format!("credentials: {e}"))?;
    let org = mesh.org(credentials).map_err(|e| format!("bind: {e}"))?;
    let service = m["granted_service"]
        .as_str()
        .ok_or("granted_service missing")?;
    let request = Bytes::from_static(br#"{"n":7}"#);
    let deadline = Instant::now() + DISCOVERY_BUDGET;

    let outcome = loop {
        let attempt: Result<(), OrgSdkError> = match args.shape {
            Shape::Unary => match org.call_bytes(service, request.clone()).await {
                Ok(body) if json_eq(&body, serde_json::json!({"n": 8})) => Ok(()),
                Ok(body) => {
                    break Err(format!(
                        "unexpected response {:?}",
                        String::from_utf8_lossy(&body)
                    ))
                }
                Err(e) => Err(e),
            },
            Shape::ServerStreaming => {
                match org.call_streaming_bytes(service, request.clone()).await {
                    Ok(mut stream) => {
                        let mut items = Vec::new();
                        let mut failure = None;
                        while let Some(item) = stream.next().await {
                            match item {
                                Ok(bytes) => items.push(bytes),
                                Err(e) => {
                                    failure = Some(e);
                                    break;
                                }
                            }
                        }
                        if let Some(e) = failure {
                            break Err(format!("stream ended with an error: {e}"));
                        }
                        let want: Vec<_> = (0..3).map(|i| serde_json::json!({ "i": i })).collect();
                        let got: Vec<serde_json::Value> = items
                            .iter()
                            .filter_map(|b| serde_json::from_slice(b).ok())
                            .collect();
                        if got == want {
                            Ok(())
                        } else {
                            break Err(format!("stream items {got:?}, want {want:?}"));
                        }
                    }
                    Err(e) => Err(e),
                }
            }
        };
        match attempt {
            Ok(()) => break Ok(String::new()),
            // Discovery is local: nothing was sent, so waiting and asking
            // again is not an invocation retry.
            Err(OrgSdkError::Discovery(e)) if Instant::now() < deadline => {
                eprintln!("org_matrix_peer: discovery pending: {e}");
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            Err(e) => break Err(format!("call: {e}")),
        }
    };
    drop(org);
    let _ = mesh.shutdown().await;
    outcome
}

async fn run_provider(args: &Args, m: &serde_json::Value) -> Result<String, String> {
    let mesh = build(m, "provider").await;
    mesh.install_org_authority(&args.manifest.join(field(m, "provider", "authority_dir")))
        .map_err(|e| format!("install authority: {e}"))?;
    let caller_node_id = args.caller_node_id.ok_or("no caller node id")?;
    let node = mesh.node().clone();
    let accept = tokio::spawn(async move { node.accept(caller_node_id).await });
    line(&format!(
        "READY {} {} {}",
        mesh.local_addr(),
        hex_encode(mesh.public_key()),
        mesh.node_id()
    ));
    tokio::time::timeout(Duration::from_secs(30), accept)
        .await
        .map_err(|_| "accept did not complete within 30s")?
        .map_err(|e| format!("accept task: {e}"))?
        .map_err(|e| format!("accept: {e}"))?;
    mesh.start().map_err(|e| format!("start: {e}"))?;

    let service = m["granted_service"]
        .as_str()
        .ok_or("granted_service missing")?
        .to_string();
    let calls = Arc::new(AtomicUsize::new(0));
    let facts: Arc<Mutex<Option<OrgCaller>>> = Arc::new(Mutex::new(None));
    let request_ok = Arc::new(Mutex::new(true));
    let (fired_tx, fired_rx) = tokio::sync::watch::channel(false);
    let fired_tx = Arc::new(fired_tx);

    let record = {
        let (calls, facts, request_ok, fired_tx) = (
            calls.clone(),
            facts.clone(),
            request_ok.clone(),
            fired_tx.clone(),
        );
        move |caller: OrgCaller, body: &Bytes| {
            calls.fetch_add(1, Ordering::SeqCst);
            *facts.lock() = Some(caller);
            if !json_eq(body, serde_json::json!({"n": 7})) {
                *request_ok.lock() = false;
            }
            let _ = fired_tx.send(true);
        }
    };
    let handle = match args.shape {
        Shape::Unary => mesh.serve_org_bytes(&service, OrgAccess::Granted, move |caller, body| {
            record(caller, &body);
            async move { Ok(Bytes::from_static(br#"{"n":8}"#)) }
        }),
        Shape::ServerStreaming => mesh.serve_org_streaming_bytes(
            &service,
            OrgAccess::Granted,
            move |caller, body, sink| {
                record(caller, &body);
                async move {
                    for i in 0..3 {
                        sink.send(Bytes::from(format!("{{\"i\":{i}}}")));
                    }
                    Ok(())
                }
            },
        ),
    }
    .map_err(|e| format!("serve: {e}"))?;
    let grant = std::fs::read(args.manifest.join(field(m, "provider", "grant_path")))
        .map_err(|e| format!("read grant: {e}"))?;
    mesh.install_provider_grant_audience(
        &grant,
        &args
            .manifest
            .join(field(m, "provider", "grant_secret_path")),
    )
    .map_err(|e| format!("provider audience: {e}"))?;

    // Re-announce until the handler has run, so the caller discovers us.
    let announcer = {
        let node = mesh.node().clone();
        let mut fired = fired_rx.clone();
        tokio::spawn(async move {
            while !*fired.borrow() {
                let _ = node
                    .announce_capabilities(net_sdk::capabilities::CapabilitySet::new())
                    .await;
                let _ = tokio::time::timeout(Duration::from_secs(1), fired.changed()).await;
            }
        })
    };
    let mut fired = fired_rx;
    let ran = tokio::time::timeout(WATCHDOG, fired.wait_for(|f| *f)).await;
    announcer.abort();
    if ran.is_err() {
        line("RESULT fail callback-loss");
        std::process::exit(2);
    }

    let facts_ok = {
        let facts = facts.lock();
        let caller = facts.as_ref().ok_or("no facts recorded")?;
        let acting = hex_encode(caller.acting_org.as_bytes());
        let provider = hex_encode(caller.provider_org.as_bytes());
        if acting != field(m, "caller", "org_id_hex") {
            Err(format!("acting_org {acting}"))
        } else if provider != field(m, "provider", "org_id_hex") {
            Err(format!("provider_org {provider}"))
        } else if caller.is_same_org() {
            Err("a same-org call".to_string())
        } else if !*request_ok.lock() {
            Err("request body was not {\"n\":7}".to_string())
        } else {
            Ok(())
        }
    };
    match &facts_ok {
        Ok(()) => line("FACTS ok"),
        Err(detail) => line(&format!("FACTS fail {detail}")),
    }

    // Teardown waits for the orchestrator's DRAINED.
    let drained = tokio::task::spawn_blocking(|| {
        let mut buf = String::new();
        std::io::stdin().lock().read_line(&mut buf).map(|_| buf)
    });
    match tokio::time::timeout(WATCHDOG, drained).await {
        Ok(Ok(Ok(l))) if l.trim() == "DRAINED" => {}
        _ => {
            line("RESULT fail callback-loss");
            std::process::exit(2);
        }
    }
    drop(handle);
    let _ = mesh.shutdown().await;
    facts_ok?;
    let n = calls.load(Ordering::SeqCst);
    if n != 1 {
        return Err(format!("calls={n}, want 1"));
    }
    Ok(" calls=1".to_string())
}
