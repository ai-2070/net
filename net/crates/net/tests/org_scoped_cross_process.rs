//! F-S4Vectors-1 witness: org-scoped (private) discovery must cross OS-PROCESS
//! boundaries.
//!
//! The S4Vectors lane (Stage 4 Wave 2) proved the gap this closes: public
//! discovery and sessions cross an OS-process boundary fine
//! (`discovered_nodes=1`) while the private plane stays empty
//! (`0 private candidate(s) considered`), so a protected call can never plan in
//! a real two-process deployment. Every pre-existing live cell runs its two
//! nodes in ONE process, so the whole estate was blind to it.
//!
//! Shape: this test binary re-executes ITSELF as a second OS process
//! (`std::env::current_exe()` + `--ignored --exact`, the repo's established
//! multi-process idiom from `tests/natsim.rs`). The CHILD plays the org-B
//! provider (adopted authority + provider grant audience + one
//! `serve_rpc_granted` service); the PARENT plays the org-A caller (adopted
//! authority + consumer grant audience) and drives the real protected call.
//!
//! Asserted, fail-closed, in this order:
//!
//! 1. the child's EMISSION actually carries the granted envelope
//!    (`RESULT ok calls=1 emitted=<n>` — the `SendEmission.scoped` cache site);
//! 2. the parent's private plane CONSIDERS the child's provider entity as a
//!    granted candidate (`granted_capability_providers` — the scoped ingest +
//!    consumer-grant lookup sites), with the intake counters printed on a red
//!    (`[inserted, updated, stale, rejected_public, at_capacity,
//!    too_many_declarations, verify_refused, race_refused]` — the
//!    `note_verify_refused` site named directly);
//! 3. the protected call is ADMITTED and the child's handler observes the exact
//!    five-field attribution (the call/serve attribution, verified on BOTH
//!    sides).
//!
//! Any mismatch, any lost callback, or a handler that never fires exits
//! non-zero on its own side — a silent drop must never read as success.

#![cfg(all(feature = "net", feature = "cortex"))]

use std::net::SocketAddr;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use net::adapter::net::behavior::capability::CapabilitySet;
use net::adapter::net::behavior::org::{OrgKeypair, OrgMembershipCert};
use net::adapter::net::behavior::org_admission::Admitted;
use net::adapter::net::behavior::org_authority::{load_grant_audience_secret, NodeAuthority};
use net::adapter::net::behavior::org_grant::{
    CapabilityAuthorityId, DispatcherScope, GrantRights, GrantTargetScope, OrgAudienceSecret,
    OrgCapabilityGrant, OrgDispatcherGrant,
};
use net::adapter::net::cortex::{
    RpcContext, RpcHandler, RpcHandlerError, RpcResponsePayload, RpcStatus,
};
use net::adapter::net::identity::EntityKeypair;
use net::adapter::net::mesh_rpc::{CallOptions, OrgProofIntent, RpcReply, ServeHandle};
use net::adapter::net::{MeshNode, MeshNodeConfig};

// NOTE (inherited discipline from `integration_nrpc_protected.rs`): an adopted
// authority's revocation `.lock` sidecar is deliberately LEFT BEHIND when this
// file's scratch directories are cleaned up in-place at process end only —
// `OrgRevocationStore` keys its process-global core registry by that sidecar's
// `(device, inode)`, and deleting the directory while a core is still
// registered lets a recycled inode join the wrong view. The parent removes its
// scenario dir only after the child process has fully exited.

const ENV_DIR: &str = "R4_XPROC_DIR";
const ENV_CALLER_NODE: &str = "R4_XPROC_CALLER_NODE";

const PSK: [u8; 32] = [0x5C; 32];
const SERVICE: &str = "customer.read";

/// The fixed identities both processes reconstruct from the SAME seeds — the
/// scenario's whole point is that the two sides agree without talking.
const PROVIDER_SEED: [u8; 32] = [0xB1; 32];
const CALLER_SEED: [u8; 32] = [0xA1; 32];
const ORG_A_SEED: [u8; 32] = [0xA0; 32];
const ORG_B_SEED: [u8; 32] = [0xB0; 32];

fn node_config() -> MeshNodeConfig {
    // THE PYTHON BINDING'S EXACT CONSTRUCTION (caller.py/provider.py `_mesh`):
    // `NetMesh(bind_addr, psk, identity_seed, heartbeat_interval_ms=200,
    // permissive_channels=True)` — i.e. `MeshNodeConfig` defaults everywhere
    // else. That includes the DEFAULT 10 s `min_announce_interval` and the
    // DEFAULT socket buffers — the two configuration deltas between every
    // in-process green recipe and the cross-process red ones. The witness must
    // run where the defect runs.
    let addr: SocketAddr = "127.0.0.1:0".parse().expect("addr");
    let mut cfg = MeshNodeConfig::new(addr, PSK).with_heartbeat_interval(Duration::from_millis(200));
    cfg.configured_identity = true;
    cfg
}

fn keypair(seed: [u8; 32]) -> EntityKeypair {
    EntityKeypair::from_bytes(seed)
}

fn cap(service: &str) -> CapabilityAuthorityId {
    CapabilityAuthorityId::for_tag(&format!("nrpc:{service}"))
}

fn encode_hex32(bytes: &[u8; 32]) -> String {
    let mut s = String::with_capacity(64);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn decode_hex32(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

/// The handler's exact five-field attribution check plus its run counter — the
/// "call/serve attributed" half of the witness, asserted on the CHILD side and
/// echoed to the parent on the `RESULT` line.
struct AttributingHandler {
    calls: Arc<AtomicUsize>,
    attribution_ok: Arc<AtomicBool>,
    expected: Admitted,
}

#[async_trait::async_trait]
impl RpcHandler for AttributingHandler {
    async fn call(&self, ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if ctx.org_admission.as_ref() == Some(&self.expected) {
            self.attribution_ok.store(true, Ordering::SeqCst);
        }
        Ok(RpcResponsePayload {
            status: RpcStatus::Ok,
            headers: vec![],
            body: bytes::Bytes::from_static(b"pong"),
        })
    }
}

/// The provider-child role. Runs ONLY as the spawned second OS process
/// (`--ignored --exact`), never in an ordinary `cargo test` — its skip gate is
/// the S4Vectors mixed-pair's own fail-closed idiom.
#[test]
#[ignore]
fn xproc_scoped_provider_child() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("runtime");
    rt.block_on(provider_child_main())
}

async fn provider_child_main() {
    fn fail(detail: String) -> ! {
        println!("RESULT fail {detail}");
        std::process::exit(1);
    }

    let dir = std::path::PathBuf::from(std::env::var(ENV_DIR).unwrap_or_default());
    let caller_node: u64 = std::env::var(ENV_CALLER_NODE)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    if dir.as_os_str().is_empty() || caller_node == 0 {
        fail("child missing env contract".into());
    }

    let provider = Arc::new(
        MeshNode::new(keypair(PROVIDER_SEED), node_config())
            .await
            .expect("provider MeshNode::new"),
    );

    // Adopted authority (org B) from the parent-minted directory — the same
    // open + install + emission-enable sequence every binding's
    // `install_org_authority` runs.
    let authority = match NodeAuthority::open(&dir.join("provider_authority"), provider.entity_id())
    {
        Ok(a) => a,
        Err(e) => fail(format!("provider authority open: {e}")),
    };
    if let Err(e) = provider.install_node_authority(Arc::new(authority)) {
        fail(format!("provider authority install: {e}"));
    }
    if let Err(e) = provider.set_owner_cert_emission(true) {
        fail(format!("emission enable: {e}"));
    }

    // The provider grant audience from the shared generated files.
    let grant_bytes = match std::fs::read(dir.join("grant.bin")) {
        Ok(b) => b,
        Err(e) => fail(format!("read grant.bin: {e}")),
    };
    let grant = match OrgCapabilityGrant::from_bytes(&grant_bytes) {
        Ok(g) => g,
        Err(e) => fail(format!("grant decode: {e}")),
    };
    let secret = match load_grant_audience_secret(&dir.join("grant.audience")) {
        Ok(s) => s,
        Err(e) => fail(format!("grant secret load: {e}")),
    };
    if let Err(e) = provider.install_provider_grant_audience(grant, secret) {
        fail(format!("provider grant install: {e}"));
    }

    // Arm the accept BEFORE announcing readiness (the node refuses an accept
    // once started and refuses a start while an accept is in flight).
    let accept_node = provider.clone();
    let accept = tokio::spawn(async move { accept_node.accept(caller_node).await });

    println!(
        "READY {} {} {}",
        provider.local_addr(),
        encode_hex32(provider.public_key()),
        provider.node_id()
    );

    if let Err(e) = accept.await.expect("accept task") {
        fail(format!("accept: {e}"));
    }
    provider.start();

    // Serve AFTER start — registration must see the running node (the exact
    // ordering every working live cell uses).
    let org_a = OrgKeypair::from_bytes(ORG_A_SEED);
    let org_b = OrgKeypair::from_bytes(ORG_B_SEED);
    let expected = Admitted {
        caller: keypair(CALLER_SEED).entity_id().clone(),
        acting_org: org_a.org_id(),
        provider_org: org_b.org_id(),
        provider: provider.entity_id().clone(),
        capability: cap(SERVICE),
    };
    let handler = Arc::new(AttributingHandler {
        calls: Arc::new(AtomicUsize::new(0)),
        attribution_ok: Arc::new(AtomicBool::new(false)),
        expected,
    });
    let calls = handler.calls.clone();
    let attribution_ok = handler.attribution_ok.clone();
    let _serve: ServeHandle =
        match provider.serve_rpc_granted(SERVICE, handler, Arc::new(|_| true)) {
            Ok(h) => h,
            Err(e) => fail(format!("granted serve: {e:?}")),
        };

    // Announce loop + emission instrumentation: the `SendEmission.scoped`
    // cache site, observed from the provider side and reported on the RESULT
    // line either way, so a red names its own mechanism.
    let deadline = Instant::now() + Duration::from_secs(90);
    let mut emitted = 0usize;
    while Instant::now() < deadline {
        provider.announce_capabilities(CapabilitySet::new()).await.ok();
        emitted = provider.announcement_scoped_for_send_for_test().len();
        if calls.load(Ordering::SeqCst) > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    if calls.load(Ordering::SeqCst) == 0 {
        println!(
            "RESULT fail callback-loss emitted={emitted} provider_grants={} relay_gate={}",
            provider.provider_grant_audiences_len_for_test(),
            provider.scoped_relay_gate_len_for_test(),
        );
        std::process::exit(2);
    }
    if !attribution_ok.load(Ordering::SeqCst) {
        println!("RESULT fail attribution emitted={emitted}");
        std::process::exit(1);
    }
    println!("RESULT ok calls=1 emitted={emitted}");
    std::process::exit(0);
}

/// The witness: two OS processes, scoped plane, candidate considered + the
/// call/serve attributed — fail-closed at every step.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn scoped_discovery_crosses_an_os_process_boundary() {
    let dir = std::env::temp_dir().join(format!(
        "net-xproc-scoped-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scenario dir");

    // ---- the shared cross-org chain (org A caller, org B provider) ----
    let org_a = OrgKeypair::from_bytes(ORG_A_SEED);
    let org_b = OrgKeypair::from_bytes(ORG_B_SEED);
    let provider_kp = keypair(PROVIDER_SEED);
    let caller_kp = keypair(CALLER_SEED);
    let provider_entity = provider_kp.entity_id().clone();
    let caller_entity = caller_kp.entity_id().clone();
    let capability = cap(SERVICE);

    // Provider authority dir for the CHILD to open (org B, provider entity).
    let provider_cert =
        OrgMembershipCert::try_issue(&org_b, provider_entity.clone(), 1, 3600).expect("p cert");
    NodeAuthority::adopt(
        &dir.join("provider_authority"),
        provider_cert,
        &provider_entity,
        0,
        None,
    )
    .expect("adopt provider authority");

    // The B→A DISCOVER|INVOKE grant + audience secret, shared through files —
    // the same bytes-and-paths contract the cross-language fixture pins.
    let (grant, secret) = OrgCapabilityGrant::try_issue(
        &org_b,
        org_a.org_id(),
        capability,
        GrantRights::INVOKE.union(GrantRights::DISCOVER),
        GrantTargetScope::AnyNodeOwnedBy(org_b.org_id()),
        3600,
    )
    .expect("issue grant");
    let secret: OrgAudienceSecret = secret.expect("DISCOVER mints a secret");
    std::fs::write(dir.join("grant.bin"), grant.to_bytes()).expect("write grant");
    {
        use std::io::Write;
        let mut f = std::fs::File::create(dir.join("grant.audience")).expect("create secret");
        f.write_all(&secret.encode_config()).expect("write secret");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                dir.join("grant.audience"),
                std::fs::Permissions::from_mode(0o600),
            )
            .expect("chmod secret");
        }
    }

    // ---- the caller node (parent process) ----
    let caller = Arc::new(
        MeshNode::new(keypair(CALLER_SEED), node_config())
            .await
            .expect("caller MeshNode::new"),
    );
    let caller_cert =
        OrgMembershipCert::try_issue(&org_a, caller_entity.clone(), 1, 3600).expect("c cert");
    let caller_authority =
        NodeAuthority::adopt(&dir.join("caller_authority"), caller_cert, &caller_entity, 0, None)
            .expect("adopt caller authority");
    caller
        .install_node_authority(Arc::new(caller_authority))
        .expect("install caller authority");
    caller
        .set_owner_cert_emission(true)
        .expect("enable caller emission");
    // The consumer grant audience — the caller side of the SAME grant+secret.
    let secret_copy = OrgAudienceSecret::decode_config(&secret.encode_config()).expect("copy");
    caller
        .install_consumer_grant_audience(grant.clone(), secret_copy)
        .expect("install consumer grant audience");

    // ---- spawn the provider as a SECOND OS PROCESS ----
    let exe = std::env::current_exe().expect("current_exe");
    let mut child = Command::new(exe)
        .args([
            "--ignored",
            "--exact",
            "xproc_scoped_provider_child",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(ENV_DIR, &dir)
        .env(ENV_CALLER_NODE, caller.node_id().to_string())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn provider child process");
    use std::io::BufRead;
    let stdout = child.stdout.take().expect("child stdout");
    let mut reader = std::io::BufReader::new(stdout);

    // READY handshake with the child. The spawned binary is a libtest harness,
    // so its own output precedes the child's protocol lines — and its
    // `test <name> ... ` progress line carries NO trailing newline, so the
    // child's first println CONCATENATES onto it. Scan for the READY marker
    // anywhere in a line and slice from there; fail closed on a terminal
    // RESULT before it.
    let ready_line = {
        let mut line = String::new();
        loop {
            line.clear();
            let n = reader.read_line(&mut line).expect("read child stdout");
            assert!(n > 0, "child exited before READY");
            let trimmed = line.trim();
            if let Some(idx) = trimmed.find("READY ") {
                break trimmed[idx..].to_string();
            }
            if let Some(idx) = trimmed.find("RESULT fail") {
                panic!("child failed before READY: {}", &trimmed[idx..]);
            }
            // Anything else is harness noise (headers, blank lines, the
            // `test ...` progress prefix).
        }
    };
    let fields: Vec<&str> = ready_line.split_whitespace().collect();
    assert!(
        fields.len() == 4 && fields[0] == "READY",
        "child READY line malformed: {ready_line:?}"
    );
    let provider_addr: SocketAddr = fields[1].parse().expect("provider addr");
    let provider_pub = decode_hex32(fields[2]).expect("provider pubkey");
    let provider_node: u64 = fields[3].parse().expect("provider node id");

    // Handshake: the child-side accept was armed before READY; both nodes start
    // only after the session lands (a node refuses an accept once started).
    caller
        .connect(provider_addr, &provider_pub, provider_node)
        .await
        .expect("connect to provider child");
    caller.start();

    // Entity pins need signed announcements from BOTH sides (protected RPC is
    // direct-session-only and the provider-side gate resolves the caller
    // through its own pin).
    let caller_id = caller.node_id();
    let pin_deadline = Instant::now() + Duration::from_secs(20);
    loop {
        caller
            .announce_capabilities(CapabilitySet::new())
            .await
            .ok();
        if caller.peer_entity_id(provider_node).is_some() {
            break;
        }
        assert!(
            Instant::now() < pin_deadline,
            "the caller pinned the provider's entity across the process boundary"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // ---- (1)+(2) the scoped plane: the provider's granted candidate must be
    // CONSIDERED on the caller — the F-S4Vectors-1 property. ----
    let consider_deadline = Instant::now() + Duration::from_secs(60);
    let considered = loop {
        caller
            .announce_capabilities(CapabilitySet::new())
            .await
            .ok();
        if !caller
            .granted_capability_providers(&grant.grant_id)
            .is_empty()
        {
            break true;
        }
        if Instant::now() >= consider_deadline {
            break false;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    if !considered {
        // Localize the drop across the three named sites — printed on every
        // red so the mechanism names itself.
        eprintln!(
            "xproc scoped discovery RED: candidate never considered; caller \
             ({}): consumer_grants={} relay_gate={} intake={:?}",
            caller_id,
            caller.consumer_grant_audiences_len_for_test(),
            caller.scoped_relay_gate_len_for_test(),
            caller.org_scoped_ingest_counts(),
        );
        let _ = child.kill();
        panic!(
            "F-S4Vectors-1: scoped discovery did not cross the OS-process boundary"
        );
    }
    let providers = caller.granted_capability_providers(&grant.grant_id);
    assert_eq!(providers.len(), 1, "exactly the one granted provider considered");
    assert_eq!(
        providers[0].provider, provider_entity,
        "the considered candidate is the child process's provider entity",
    );

    // ---- (3) the protected call is admitted with exact five-field
    // attribution — the call/serve half (verified again on the child side). ----
    let membership =
        OrgMembershipCert::try_issue(&org_a, caller_entity.clone(), 1, 3600).expect("membership");
    let dispatcher = OrgDispatcherGrant::try_issue(
        &org_a,
        caller_entity.clone(),
        DispatcherScope::Exact(capability),
        3600,
    )
    .expect("dispatcher");
    let intent = OrgProofIntent {
        caller: Arc::new(keypair(CALLER_SEED)),
        membership,
        dispatcher,
        capability_grant: Some(grant),
        acting_org: org_a.org_id(),
        provider_owner_org: org_b.org_id(),
        provider: provider_entity.clone(),
        capability: capability,
        proof_ttl_secs: 30,
    };
    let reply: RpcReply = caller
        .call(
            provider_node,
            SERVICE,
            bytes::Bytes::from_static(b"ping"),
            CallOptions {
                org_proof_intent: Some(intent),
                deadline: Some(Instant::now() + Duration::from_secs(10)),
                ..Default::default()
            },
        )
        .await
        .expect("the cross-process protected call is admitted");
    assert_eq!(reply.body.as_ref(), b"pong", "handler reply crosses back");

    // The child's own verdict — its handler-side attribution + emission count
    // (both sides pin; a silent drop must never read as success). Same harness
    // noise tolerance as the READY scan.
    let result_line = {
        let mut line = String::new();
        loop {
            line.clear();
            let n = reader.read_line(&mut line).expect("read child RESULT");
            assert!(n > 0, "child exited before RESULT");
            let trimmed = line.trim();
            if let Some(idx) = trimmed.find("RESULT ") {
                break trimmed[idx..].to_string();
            }
        }
    };
    assert!(
        result_line.trim().starts_with("RESULT ok calls=1 emitted="),
        "the provider child verified its own handler attribution + emission; got {result_line:?}"
    );
    let emitted: usize = result_line
        .trim()
        .rsplit("emitted=")
        .next()
        .expect("emitted field")
        .parse()
        .expect("emitted count");
    assert!(
        emitted >= 1,
        "the child's SendEmission.scoped carried the granted envelope (got {emitted})"
    );

    let status = child.wait().expect("child exit");
    assert!(status.success(), "provider child exited cleanly");
    let _ = std::fs::remove_dir_all(&dir);
}
